//! Rose integration for bridged Telegram members.
//!
//! Rose remains authoritative for Telegram warning history. The bot only
//! *issues* a warn command through the Matrix↔Telegram bridge and never
//! mirrors Rose counters into SQLite.
//!
//! Bridge behaviour (mentions, bot-to-bot commands) must be verified in a
//! staging room: see README "Rose verification". The command shape is fully
//! configurable via `rose.warn_template` so admins can adapt without rebuilding.

use std::collections::HashMap;

use crate::config::expand_template;

/// `@telegram_123456:aosus.org` → `Some("123456")`.
pub fn telegram_id_of(matrix_id: &str) -> Option<String> {
    let local = matrix_id.strip_prefix("@telegram_")?;
    let id = local.split(':').next()?;
    if id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty() {
        Some(id.to_string())
    } else {
        None
    }
}

pub fn is_bridged_telegram(matrix_id: &str) -> bool {
    telegram_id_of(matrix_id).is_some()
}

/// Build the Rose warn command text to send as a room message.
/// Template placeholders: `{user}` (Telegram id or Matrix id), `{reason}`.
pub fn build_warn_command(template: &str, matrix_user_id: &str, reason: &str) -> String {
    let user = telegram_id_of(matrix_user_id).unwrap_or_else(|| matrix_user_id.to_string());
    let mut vars = HashMap::new();
    let user_s = user.as_str();
    vars.insert("user", user_s);
    vars.insert("reason", reason);
    expand_template(template, &vars)
}

/// Reason string attached to bans / warn commands.
pub fn spam_reason(category: &str, spam_score: f64) -> String {
    format!("spam ({category}, {:.0}%) — automated removal by Mirsad", spam_score * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_telegram_id() {
        assert_eq!(
            telegram_id_of("@telegram_123456:aosus.org"),
            Some("123456".to_string())
        );
        assert_eq!(telegram_id_of("@user:aosus.org"), None);
        assert_eq!(telegram_id_of("@telegram_:aosus.org"), None);
    }

    #[test]
    fn warn_template_expands() {
        let cmd = build_warn_command("/warn {user} {reason}", "@telegram_42:aosus.org", "spam");
        assert_eq!(cmd, "/warn 42 spam");
        let cmd2 = build_warn_command("/warn {user} {reason}", "@native:aosus.org", "spam");
        assert_eq!(cmd2, "/warn @native:aosus.org spam");
    }
}
