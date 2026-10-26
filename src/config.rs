//! Configuration loading for Mirsad.
//!
//! Secrets (Matrix password / access token, OpenRouter API key) come from
//! environment variables. Everything else lives in `config.toml`.

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub matrix: MatrixConfig,
    #[serde(default)]
    pub decider: DeciderConfig,
    #[serde(default)]
    pub policy: PolicyConfig,
    #[serde(default)]
    pub trust: TrustConfig,
    #[serde(default)]
    pub exclusions: ExclusionsConfig,
    #[serde(default)]
    pub history: HistoryConfig,
    #[serde(default)]
    pub rose: RoseConfig,
    #[serde(default)]
    pub report: ReportConfig,
    #[serde(default)]
    pub storage: StorageConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MatrixConfig {
    /// e.g. https://matrix.aosus.org
    #[serde(default = "default_homeserver")]
    pub homeserver: String,
    /// Matrix user id, e.g. @mirsad:aosus.org
    #[serde(default)]
    pub user_id: String,
    #[serde(default = "default_device_name")]
    pub device_name: String,
    /// Stable login device id. MUST be fixed per deployment: a fresh random
    /// device on every start invalidates the persisted E2EE/crypto store and
    /// the bot fails to boot after any restart or redeploy.
    #[serde(default = "default_device_id")]
    pub device_id: String,
    /// Only these rooms are moderated. Empty = all joined rooms.
    #[serde(default)]
    pub monitored_rooms: Vec<String>,
    /// Optional private room for enforcement notifications / errors.
    #[serde(default)]
    pub mod_room: Option<String>,
    /// Auto-join invites.
    #[serde(default = "default_true")]
    pub autojoin: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeciderConfig {
    /// OpenRouter Decisions endpoint.
    #[serde(default = "default_decider_url")]
    pub base_url: String,
    /// OpenRouter model slug, e.g. perplexity/pplx-decider-v1.2
    #[serde(default = "default_model")]
    pub model: String,
    /// Timeout in seconds for a classification call.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Max message chars sent to the model.
    #[serde(default = "default_max_chars")]
    pub max_chars: usize,
    /// Optional HTTP referer / title for OpenRouter rankings.
    #[serde(default)]
    pub http_referer: Option<String>,
    #[serde(default = "default_app_title")]
    pub app_title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PolicyConfig {
    /// Path to the external decision-questions JSON file.
    #[serde(default = "default_questions_path")]
    pub questions_file: String,
    /// Question id holding the spam yes/no probability. Omit to disable
    /// spam enforcement entirely (the bot will warn at startup and idle).
    #[serde(default = "default_spam_q_opt")]
    pub spam_question: Option<String>,
    /// Question id holding the language-violation yes/no probability.
    /// Omit to disable language enforcement.
    #[serde(default)]
    pub language_question: Option<String>,
    /// Question id holding the spam category choice. Omit if the policy
    /// has no category question (category shows as unknown).
    #[serde(default = "default_cat_q_opt")]
    pub category_question: Option<String>,
    /// Spam score >= this triggers enforcement.
    #[serde(default = "default_spam_threshold")]
    pub spam_threshold: f64,
    /// Language score >= this triggers language enforcement.
    #[serde(default = "default_lang_threshold")]
    pub language_threshold: f64,
    /// If true, never redact/ban; only log + notify.
    #[serde(default)]
    pub dry_run: bool,
    /// Language action: "warn" (delete+warn) or "delete" or "ignore".
    #[serde(default = "default_lang_action")]
    pub language_action: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TrustConfig {
    #[serde(default = "default_min_messages")]
    pub min_messages: i64,
    #[serde(default = "default_min_days")]
    pub min_days: i64,
    /// Days of inactivity after which routine scanning resumes.
    #[serde(default = "default_inactivity_days")]
    pub inactivity_days: i64,
    /// Messages shorter than this never count toward trust.
    #[serde(default = "default_filler_min_chars")]
    pub filler_min_chars: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExclusionsConfig {
    /// User IDs never auto-scanned / auto-punished (admins, bots, bridges).
    /// `*` matches any substring, e.g. `@admin:*` or `@bot*:aosus.org`.
    #[serde(default)]
    pub user_ids: Vec<String>,
    /// Room staff (moderators/admins/owners by Matrix power level) are never
    /// auto-scanned or auto-punished, no matter which room they act in.
    #[serde(default = "default_true")]
    pub skip_moderators: bool,
    /// Minimum power level that counts as staff (50 = moderator by default).
    #[serde(default = "default_mod_power_level")]
    pub moderator_power_level: i64,
}

impl Default for ExclusionsConfig {
    fn default() -> Self {
        Self {
            user_ids: Vec::new(),
            skip_moderators: true,
            moderator_power_level: 50,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct HistoryConfig {
    /// Suspect-triggered sender history check (one server-filtered request).
    /// Resolves "unknown history" into newcomer/established before enforcing.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Trust fast-track: visible history already meeting the trust bar
    /// (min_messages across min_days) earns activity trust immediately,
    /// instead of waiting for prospective counting alone.
    #[serde(default = "default_true")]
    pub fast_track_trust: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            fast_track_trust: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoseConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Matrix id of the Rose bridge bot, if known.
    #[serde(default)]
    pub bot_user_id: Option<String>,
    /// Template for the warn command. Placeholders: {user} {reason}.
    #[serde(default = "default_rose_template")]
    pub warn_template: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReportConfig {
    /// Max manual /spam reports per reporter per 5 minutes.
    #[serde(default = "default_rate_limit")]
    pub max_reports_per_5min: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    #[serde(default = "default_sqlite_path")]
    pub sqlite_path: String,
    #[serde(default = "default_matrix_store")]
    pub matrix_store_path: String,
}

fn default_homeserver() -> String {
    "https://matrix.aosus.org".to_string()
}
fn default_device_name() -> String {
    "mirsad".to_string()
}
fn default_device_id() -> String {
    "mirsad-bot".to_string()
}
fn default_true() -> bool {
    true
}
fn default_mod_power_level() -> i64 {
    50
}
fn default_decider_url() -> String {
    "https://openrouter.ai/api/alpha/decisions".to_string()
}
fn default_model() -> String {
    "perplexity/pplx-decider-v1.2".to_string()
}
fn default_timeout_secs() -> u64 {
    30
}
fn default_max_chars() -> usize {
    4000
}
fn default_app_title() -> String {
    "mirsad".to_string()
}
fn default_questions_path() -> String {
    "config/decision-questions.json".to_string()
}
fn default_spam_q_opt() -> Option<String> {
    Some("spam".to_string())
}
fn default_cat_q_opt() -> Option<String> {
    Some("spam_category".to_string())
}
fn default_spam_threshold() -> f64 {
    0.9
}
fn default_lang_threshold() -> f64 {
    0.9
}
fn default_lang_action() -> String {
    "warn".to_string()
}
fn default_min_messages() -> i64 {
    10
}
fn default_min_days() -> i64 {
    3
}
fn default_inactivity_days() -> i64 {
    90
}
fn default_filler_min_chars() -> usize {
    5
}
fn default_rose_template() -> String {
    "/warn {user} {reason}".to_string()
}
fn default_rate_limit() -> usize {
    5
}
fn default_sqlite_path() -> String {
    "data/mirsad.db".to_string()
}
fn default_matrix_store() -> String {
    "data/matrix-store".to_string()
}

impl Default for MatrixConfig {
    fn default() -> Self {
        Self {
            homeserver: default_homeserver(),
            user_id: String::new(),
            device_name: default_device_name(),
            device_id: default_device_id(),
            monitored_rooms: vec![],
            mod_room: None,
            autojoin: true,
        }
    }
}
impl Default for DeciderConfig {
    fn default() -> Self {
        Self {
            base_url: default_decider_url(),
            model: default_model(),
            timeout_secs: default_timeout_secs(),
            max_chars: default_max_chars(),
            http_referer: None,
            app_title: default_app_title(),
        }
    }
}
impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            questions_file: default_questions_path(),
            spam_question: default_spam_q_opt(),
            language_question: None,
            category_question: default_cat_q_opt(),
            spam_threshold: default_spam_threshold(),
            language_threshold: default_lang_threshold(),
            dry_run: false,
            language_action: default_lang_action(),
        }
    }
}
impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            min_messages: default_min_messages(),
            min_days: default_min_days(),
            inactivity_days: default_inactivity_days(),
            filler_min_chars: default_filler_min_chars(),
        }
    }
}
impl Default for RoseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_user_id: None,
            warn_template: default_rose_template(),
        }
    }
}
impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            max_reports_per_5min: default_rate_limit(),
        }
    }
}
impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            sqlite_path: default_sqlite_path(),
            matrix_store_path: default_matrix_store(),
        }
    }
}

/// Secrets resolved from the environment (never from the TOML file).
#[derive(Debug, Clone, Default)]
pub struct Secrets {
    pub matrix_password: Option<String>,
    pub matrix_access_token: Option<String>,
    pub openrouter_api_key: Option<String>,
}

impl Secrets {
    pub fn from_env() -> Self {
        Self {
            matrix_password: std::env::var("MATRIX_PASSWORD")
                .ok()
                .filter(|s| !s.is_empty()),
            matrix_access_token: std::env::var("MATRIX_ACCESS_TOKEN")
                .ok()
                .filter(|s| !s.is_empty()),
            openrouter_api_key: std::env::var("OPENROUTER_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
        }
    }
}

impl AppConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let builder = config::Config::builder()
            .add_source(config::File::from(path.to_path_buf()).required(true))
            .add_source(config::Environment::with_prefix("MIRSAD").separator("__"))
            .build()
            .with_context(|| format!("reading config {}", path.display()))?;
        let cfg: AppConfig = builder.try_deserialize().context("parsing config.toml")?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.policy.spam_threshold),
            "policy.spam_threshold must be in [0,1]"
        );
        anyhow::ensure!(
            (0.0..=1.0).contains(&self.policy.language_threshold),
            "policy.language_threshold must be in [0,1]"
        );
        anyhow::ensure!(
            ["warn", "delete", "ignore"].contains(&self.policy.language_action.as_str()),
            "policy.language_action must be warn|delete|ignore"
        );
        anyhow::ensure!(self.trust.min_messages >= 0, "trust.min_messages >= 0");
        anyhow::ensure!(self.trust.min_days >= 0, "trust.min_days >= 0");
        Ok(())
    }

    /// True when `user_id` matches the exclusion list (exact or `prefix*`).
    pub fn is_excluded(&self, user_id: &str) -> bool {
        is_excluded_user(&self.exclusions.user_ids, user_id)
    }

    pub fn room_monitored(&self, room_id: &str) -> bool {
        if self.matrix.monitored_rooms.is_empty() {
            return true;
        }
        self.matrix.monitored_rooms.iter().any(|r| r == room_id)
    }
}

pub fn is_excluded_user(patterns: &[String], user_id: &str) -> bool {
    patterns.iter().any(|p| glob_match(p, user_id))
}

/// `*` inside a pattern matches any (possibly empty) substring.
/// e.g. `@admin:*` matches any id starting with `@admin:`,
/// and `@bot*:aosus.org` matches `@bot123:aosus.org`.
fn glob_match(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == text;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = text;
    if !parts[0].is_empty() {
        match rest.strip_prefix(parts[0]) {
            Some(r) => rest = r,
            None => return false,
        }
    }
    let mut inner = rest;
    for part in &parts[1..parts.len() - 1] {
        if part.is_empty() {
            continue;
        }
        match inner.find(part) {
            Some(i) => inner = &inner[i + part.len()..],
            None => return false,
        }
    }
    let last = parts[parts.len() - 1];
    if last.is_empty() {
        return true;
    }
    if parts.len() == 2 {
        return rest.ends_with(last);
    }
    inner.ends_with(last)
}

/// Placeholder expansion for templates with `{user}` / `{reason}`.
pub fn expand_template(template: &str, vars: &HashMap<&str, &str>) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{k}}}"), v);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusion_exact_and_wildcard() {
        let patterns = vec![
            "@admin:aosus.org".to_string(),
            "@bot*:aosus.org".to_string(),
        ];
        assert!(is_excluded_user(&patterns, "@admin:aosus.org"));
        assert!(is_excluded_user(&patterns, "@bot123:aosus.org"));
        assert!(!is_excluded_user(&patterns, "@user:aosus.org"));
    }

    #[test]
    fn room_filter_empty_means_all() {
        let mut cfg = AppConfig {
            matrix: MatrixConfig::default(),
            decider: DeciderConfig::default(),
            policy: PolicyConfig::default(),
            trust: TrustConfig::default(),
            exclusions: ExclusionsConfig::default(),
            history: HistoryConfig::default(),
            rose: RoseConfig::default(),
            report: ReportConfig::default(),
            storage: StorageConfig::default(),
        };
        assert!(cfg.room_monitored("!anything:example.org"));
        cfg.matrix.monitored_rooms = vec!["!a:example.org".to_string()];
        assert!(cfg.room_monitored("!a:example.org"));
        assert!(!cfg.room_monitored("!b:example.org"));
    }

    #[test]
    fn thresholds_validated() {
        let mut cfg = AppConfig {
            matrix: MatrixConfig::default(),
            decider: DeciderConfig::default(),
            policy: PolicyConfig::default(),
            trust: TrustConfig::default(),
            exclusions: ExclusionsConfig::default(),
            history: HistoryConfig::default(),
            rose: RoseConfig::default(),
            report: ReportConfig::default(),
            storage: StorageConfig::default(),
        };
        assert!(cfg.validate().is_ok());
        cfg.policy.spam_threshold = 9.0;
        assert!(cfg.validate().is_err());
    }
}
