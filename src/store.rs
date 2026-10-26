//! Minimal SQLite persistence.
//!
//! Matrix already stores messages, identities, and room state; Rose owns
//! Telegram warnings. We keep only what the bot itself must remember:
//! activity counters, trust/protection flags, Matrix-native warnings,
//! and enforcement ids for duplicate suppression.

use anyhow::{Context, Result};
use chrono::Utc;

#[derive(Debug, Clone)]
pub struct Store {
    path: String,
}

#[derive(Debug, Clone)]
pub struct UserRow {
    pub user_id: String,
    pub activity_count: i64,
    pub distinct_days: i64,
    pub last_day: Option<String>,
    pub last_activity_ts: i64,
    pub trusted: bool,
    /// true = established / age-unknown → never auto-permaban.
    /// false = verified newcomer → permaban allowed on clear spam.
    pub protected: bool,
    pub verified_newcomer: bool,
    pub baseline_seen: bool,
    /// Warns issued by Mirsad itself, all platforms (Matrix-native warnings
    /// and Rose commands sent through the bridge). Drives repeat escalation.
    /// Counts OUR warns only — Rose's counters are never mirrored here.
    pub matrix_warns: i64,
}

impl Default for UserRow {
    /// Unknown users are protected by default (conservative: never auto-ban
    /// someone whose history we have not verified as a newcomer).
    fn default() -> Self {
        Self {
            user_id: String::new(),
            activity_count: 0,
            distinct_days: 0,
            last_day: None,
            last_activity_ts: 0,
            trusted: false,
            protected: true,
            verified_newcomer: false,
            baseline_seen: false,
            matrix_warns: 0,
        }
    }
}

impl Store {
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating storage dir for {path}"))?;
            }
        }
        let s = Self {
            path: path.to_string(),
        };
        s.migrate()?;
        Ok(s)
    }

    fn conn(&self) -> Result<rusqlite::Connection> {
        rusqlite::Connection::open(&self.path).with_context(|| format!("opening {}", self.path))
    }

    fn migrate(&self) -> Result<()> {
        let c = self.conn()?;
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS users(
               user_id TEXT PRIMARY KEY,
               activity_count INTEGER NOT NULL DEFAULT 0,
               distinct_days INTEGER NOT NULL DEFAULT 0,
               last_day TEXT,
               last_activity_ts INTEGER NOT NULL DEFAULT 0,
               trusted INTEGER NOT NULL DEFAULT 0,
               protected INTEGER NOT NULL DEFAULT 1,
               verified_newcomer INTEGER NOT NULL DEFAULT 0,
               baseline_seen INTEGER NOT NULL DEFAULT 0,
               matrix_warns INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS user_days(
               user_id TEXT NOT NULL,
               day TEXT NOT NULL,
               PRIMARY KEY(user_id, day)
             );
             CREATE TABLE IF NOT EXISTS enforcements(
               event_id TEXT PRIMARY KEY,
               room_id TEXT NOT NULL,
               user_id TEXT NOT NULL,
               action TEXT NOT NULL,
               ts INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS meta(
               key TEXT PRIMARY KEY,
               value TEXT NOT NULL
             );",
        )
        .context("migrating sqlite schema")?;
        Ok(())
    }

    pub fn get_user(&self, user_id: &str) -> Result<UserRow> {
        let c = self.conn()?;
        let mut stmt = c.prepare(
            "SELECT user_id, activity_count, distinct_days, last_day, last_activity_ts,
                    trusted, protected, verified_newcomer, baseline_seen, matrix_warns
             FROM users WHERE user_id = ?1",
        )?;
        let row = stmt
            .query_row([user_id], |r| {
                Ok(UserRow {
                    user_id: r.get(0)?,
                    activity_count: r.get(1)?,
                    distinct_days: r.get(2)?,
                    last_day: r.get(3)?,
                    last_activity_ts: r.get(4)?,
                    trusted: r.get::<_, i64>(5)? != 0,
                    protected: r.get::<_, i64>(6)? != 0,
                    verified_newcomer: r.get::<_, i64>(7)? != 0,
                    baseline_seen: r.get::<_, i64>(8)? != 0,
                    matrix_warns: r.get(9)?,
                })
            })
            .unwrap_or(UserRow {
                user_id: user_id.to_string(),
                ..Default::default()
            });
        Ok(row)
    }

    fn upsert(&self, u: &UserRow) -> Result<()> {
        let c = self.conn()?;
        c.execute(
            "INSERT INTO users(user_id, activity_count, distinct_days, last_day, last_activity_ts,
                              trusted, protected, verified_newcomer, baseline_seen, matrix_warns)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(user_id) DO UPDATE SET
               activity_count=excluded.activity_count, distinct_days=excluded.distinct_days,
               last_day=excluded.last_day, last_activity_ts=excluded.last_activity_ts,
               trusted=excluded.trusted, protected=excluded.protected,
               verified_newcomer=excluded.verified_newcomer, baseline_seen=excluded.baseline_seen,
               matrix_warns=excluded.matrix_warns",
            rusqlite::params![
                u.user_id,
                u.activity_count,
                u.distinct_days,
                u.last_day,
                u.last_activity_ts,
                i64::from(u.trusted),
                i64::from(u.protected),
                i64::from(u.verified_newcomer),
                i64::from(u.baseline_seen),
                u.matrix_warns,
            ],
        )?;
        Ok(())
    }

    /// Mark a user as seen at baseline (present when the bot first deployed)
    /// or as a verified newcomer (join observed after startup).
    pub fn mark_membership(
        &self,
        user_id: &str,
        verified_newcomer: bool,
        baseline: bool,
    ) -> Result<UserRow> {
        let mut u = self.get_user(user_id)?;
        if baseline {
            u.baseline_seen = true;
            u.protected = true;
            u.verified_newcomer = false;
        } else if verified_newcomer && !u.baseline_seen {
            // Only a join observed after startup flips protection off.
            // Unknown senders stay protected (conservative default).
            u.verified_newcomer = true;
            u.protected = false;
        }
        self.upsert(&u)?;
        Ok(u)
    }

    /// Record one qualifying legitimate message.
    /// Returns the updated row. Trust is earned progressively, never from age alone.
    pub fn record_legit_activity(
        &self,
        user_id: &str,
        now_ts: i64,
        min_messages: i64,
        min_days: i64,
    ) -> Result<UserRow> {
        let mut u = self.get_user(user_id)?;
        let day = chrono::DateTime::from_timestamp(now_ts, 0)
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let c = self.conn()?;
        let added: i64 = c.execute(
            "INSERT OR IGNORE INTO user_days(user_id, day) VALUES(?1, ?2)",
            rusqlite::params![user_id, day],
        )? as i64;
        drop(c);
        u.activity_count += 1;
        if added == 1 {
            u.distinct_days += 1;
            u.last_day = Some(day);
        }
        u.last_activity_ts = now_ts;
        if !u.trusted && u.activity_count >= min_messages && u.distinct_days >= min_days {
            u.trusted = true;
        }
        self.upsert(&u)?;
        Ok(u)
    }

    /// Whether routine scanning applies right now.
    /// Trusted + recently active users are skipped; everyone else is scanned.
    /// Protection (no auto-permaban) is independent and never expires.
    pub fn needs_scan(&self, user_id: &str, now_ts: i64, inactivity_days: i64) -> Result<bool> {
        let u = self.get_user(user_id)?;
        if !u.trusted {
            return Ok(true);
        }
        let inactive_secs = now_ts.saturating_sub(u.last_activity_ts);
        if inactive_secs > inactivity_days.saturating_mul(86_400) {
            return Ok(true); // trusted but stale → resume scanning; protection intact
        }
        Ok(false)
    }

    pub fn add_matrix_warn(&self, user_id: &str) -> Result<i64> {
        let mut u = self.get_user(user_id)?;
        u.matrix_warns += 1;
        let n = u.matrix_warns;
        self.upsert(&u)?;
        Ok(n)
    }

    /// Returns true if this (event_id) was already enforced (duplicate guard).
    /// Otherwise records it and returns false.
    pub fn check_and_mark_enforced(
        &self,
        event_id: &str,
        room_id: &str,
        user_id: &str,
        action: &str,
    ) -> Result<bool> {
        let c = self.conn()?;
        let exists: bool = c
            .query_row(
                "SELECT 1 FROM enforcements WHERE event_id = ?1",
                [event_id],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if exists {
            return Ok(true);
        }
        c.execute(
            "INSERT OR IGNORE INTO enforcements(event_id, room_id, user_id, action, ts)
             VALUES(?1,?2,?3,?4,?5)",
            rusqlite::params![event_id, room_id, user_id, action, Utc::now().timestamp()],
        )?;
        Ok(false)
    }

    pub fn already_enforced(&self, event_id: &str) -> Result<bool> {
        let c = self.conn()?;
        Ok(c.query_row(
            "SELECT 1 FROM enforcements WHERE event_id = ?1",
            [event_id],
            |_| Ok(true),
        )
        .unwrap_or(false))
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let c = self.conn()?;
        Ok(
            c.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
                r.get::<_, String>(0)
            })
            .ok(),
        )
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        let c = self.conn()?;
        c.execute(
            "INSERT INTO meta(key, value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store() -> (tempfile::TempDir, Store) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("t.db").to_string_lossy().to_string();
        let s = Store::open(&p).unwrap();
        (d, s)
    }

    #[test]
    fn new_users_are_protected_by_default() {
        let (_d, s) = tmp_store();
        let u = s.get_user("@new:example.org").unwrap();
        assert!(u.protected);
        assert!(!u.trusted);
    }

    #[test]
    fn verified_newcomer_loses_protection_baseline_keeps_it() {
        let (_d, s) = tmp_store();
        let u = s.mark_membership("@a:example.org", true, false).unwrap();
        assert!(!u.protected);
        assert!(u.verified_newcomer);
        let b = s.mark_membership("@b:example.org", false, true).unwrap();
        assert!(b.protected);
    }

    #[test]
    fn trust_earned_progressively_and_stale_resumes_scan() {
        let (_d, s) = tmp_store();
        let day1 = 1_700_000_000i64;
        // 3 messages across 3 UTC days — not trusted yet.
        s.record_legit_activity("@u:example.org", day1, 10, 3)
            .unwrap();
        s.record_legit_activity("@u:example.org", day1 + 86_400, 10, 3)
            .unwrap();
        s.record_legit_activity("@u:example.org", day1 + 2 * 86_400, 10, 3)
            .unwrap();
        // 3 messages only — not trusted yet
        let u = s.get_user("@u:example.org").unwrap();
        assert!(!u.trusted);
        // add 7 more across same days
        for _ in 0..7 {
            s.record_legit_activity("@u:example.org", day1 + 2 * 86_400, 10, 3)
                .unwrap();
        }
        let u = s.get_user("@u:example.org").unwrap();
        assert_eq!(u.distinct_days, 3);
        assert!(u.activity_count >= 10);
        assert!(u.trusted);
        // active trusted user: no scan
        assert!(!s
            .needs_scan("@u:example.org", day1 + 3 * 86_400, 90)
            .unwrap());
        // after 90d inactivity: scan resumes, protection intact
        assert!(s
            .needs_scan("@u:example.org", day1 + 100 * 86_400, 90)
            .unwrap());
        assert!(s.get_user("@u:example.org").unwrap().protected);
    }

    #[test]
    fn enforcement_guard_dedupes() {
        let (_d, s) = tmp_store();
        assert!(!s
            .check_and_mark_enforced("$e1", "!r:x", "@u:x", "ban")
            .unwrap());
        assert!(s
            .check_and_mark_enforced("$e1", "!r:x", "@u:x", "ban")
            .unwrap());
    }
}
