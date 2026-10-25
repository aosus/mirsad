//! Suspect-triggered sender history check.
//!
//! When a message scores above the spam threshold but the author's membership
//! history is unknown, we ask the homeserver for that sender's own recent
//! events in the room (server-side sender filter — one cheap request, no room
//! scan). No prior events at all → treat as a verified newcomer (ban-eligible).
//! Any prior activity → keep them protected (established or compromised).
//!
//! Limits, stated honestly: homeservers only return history they actually
//! retain and the bot is allowed to see. Federation gaps, retention purges,
//! and rooms the bot just joined can all hide real history. An empty answer
//! therefore means "no visible history", and the admin accepted that this is
//! enough to treat the sender as new.

use anyhow::{Context, Result};
use matrix_sdk::{
    room::MessagesOptions,
    ruma::{OwnedUserId, UInt},
    Room,
};

/// How many of the sender's own events to pull (newest first).
const PAGE_LIMIT: u8 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryVerdict {
    /// The sender has visible prior events in this room → keep protected.
    Established,
    /// No visible prior events at all → treat as a newcomer.
    Newcomer,
}

#[derive(Debug, Clone, Default)]
pub struct HistoryInfo {
    pub verdict: Option<HistoryVerdict>,
    /// Number of prior events found (bounded by PAGE_LIMIT).
    pub prior_events: usize,
    /// Age of the newest prior event, if any.
    pub newest_age_days: Option<i64>,
}

/// Pure verdict helper, unit-tested: ANY prior activity keeps protection.
/// Only a complete absence of history lifts it.
pub fn decide_verdict(prior_events: usize) -> HistoryVerdict {
    if prior_events > 0 {
        HistoryVerdict::Established
    } else {
        HistoryVerdict::Newcomer
    }
}

pub async fn check_sender_history(room: &Room, sender: &str) -> Result<HistoryInfo> {
    let user_id: OwnedUserId = sender
        .try_into()
        .map_err(|e| anyhow::anyhow!("bad user id: {e}"))?;
    let mut opts = MessagesOptions::backward();
    opts.limit = UInt::from(PAGE_LIMIT);
    opts.filter.senders = Some(vec![user_id]);
    let page = room
        .messages(opts)
        .await
        .context("fetching sender history")?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut prior = 0usize;
    let mut newest_ts: Option<i64> = None;
    for ev in &page.chunk {
        let v = serde_json::to_value(ev.kind.raw()).context("serializing event")?;
        let ts = v
            .get("origin_server_ts")
            .and_then(|t| t.as_i64())
            .unwrap_or(0);
        prior += 1;
        newest_ts = Some(newest_ts.map_or(ts, |m: i64| m.max(ts)));
    }
    let newest_age_days = newest_ts.map(|ts| (now_ms - ts).max(0) / 86_400_000);
    Ok(HistoryInfo {
        verdict: Some(decide_verdict(prior)),
        prior_events: prior,
        newest_age_days,
    })
}

/// One-line history summary for public diagnostics.
pub fn history_label(info: &HistoryInfo) -> String {
    match info.verdict {
        None => "not checked".to_string(),
        Some(HistoryVerdict::Newcomer) => {
            "no prior messages in room history → treated as newcomer".to_string()
        }
        Some(HistoryVerdict::Established) => match (info.prior_events, info.newest_age_days) {
            (n, Some(age)) => format!("{n} prior message(s), newest {age}d ago → protected"),
            (n, None) => format!("{n} prior message(s) → protected"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_history_keeps_protection() {
        assert_eq!(decide_verdict(0), HistoryVerdict::Newcomer);
        assert_eq!(decide_verdict(1), HistoryVerdict::Established);
        assert_eq!(decide_verdict(50), HistoryVerdict::Established);
    }

    #[test]
    fn labels_describe_outcome() {
        let h = HistoryInfo {
            verdict: Some(HistoryVerdict::Newcomer),
            prior_events: 0,
            newest_age_days: None,
        };
        assert!(history_label(&h).contains("newcomer"));
        let h2 = HistoryInfo {
            verdict: Some(HistoryVerdict::Established),
            prior_events: 5,
            newest_age_days: Some(12),
        };
        assert!(history_label(&h2).contains("12d"));
    }
}
