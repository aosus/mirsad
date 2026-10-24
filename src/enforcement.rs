//! Enforcement policy: the app decides the punishment, the model decides guilt.
//!
//! Inputs: model scores (spam / language / category), configured thresholds,
//! trust + protection state, exclusion list, dry-run flag.
//! Spam behaviour itself lives in the prompt file — this module only compares
//! numbers against cutoffs.

use crate::decider::Classification;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing to do.
    None,
    /// Delete the message only.
    Delete,
    /// Delete + warn (Rose for Telegram, local counter for Matrix-native).
    DeleteWarn,
    /// Delete + permanent ban (newcomers, clear spam only).
    DeleteBan,
    /// Flag for moderator review (protected members, repeated issues).
    NotifyMods,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Action::None => "none",
            Action::Delete => "delete",
            Action::DeleteWarn => "delete+warn",
            Action::DeleteBan => "delete+ban",
            Action::NotifyMods => "notify-mods",
        }
    }

    pub fn destructive(&self) -> bool {
        matches!(
            self,
            Action::Delete | Action::DeleteWarn | Action::DeleteBan
        )
    }
}

#[derive(Debug, Clone)]
pub struct EnforcementInput<'a> {
    pub cls: &'a Classification,
    pub spam_threshold: f64,
    pub language_threshold: f64,
    /// "warn" | "delete" | "ignore"
    pub language_action: &'a str,
    /// Verified newcomer (not protected) vs established/unknown (protected).
    pub protected: bool,
    /// Admins / excluded users: never punished.
    pub excluded: bool,
    /// Observation mode: never redact/ban.
    pub dry_run: bool,
    /// Matrix-native prior warnings (for escalation messaging).
    pub prior_warns: i64,
}

#[derive(Debug, Clone)]
pub struct EnforcementDecision {
    pub action: Action,
    /// Human-readable reason, reused in public diagnostics.
    pub explanation: String,
    pub spam_hit: bool,
    pub language_hit: bool,
}

pub fn decide(inp: EnforcementInput<'_>) -> EnforcementDecision {
    let spam_hit = inp.cls.spam_score >= inp.spam_threshold;
    let language_hit = inp.cls.language_score >= inp.language_threshold;

    if inp.excluded {
        return EnforcementDecision {
            action: Action::None,
            explanation: "Author is on the exclusion list (e.g. admin); no enforcement applied."
                .to_string(),
            spam_hit,
            language_hit,
        };
    }

    // Spam path first: commercial solicitation outranks language policy.
    if spam_hit {
        if inp.dry_run {
            return EnforcementDecision {
                action: Action::NotifyMods,
                explanation: format!(
                    "Dry-run: spam score {:.1}% ≥ threshold {:.0}%, but destructive actions are disabled.",
                    inp.cls.spam_score * 100.0,
                    inp.spam_threshold * 100.0
                ),
                spam_hit,
                language_hit,
            };
        }
        if !inp.protected {
            return EnforcementDecision {
                action: Action::DeleteBan,
                explanation: format!(
                    "Clear spam from a verified newcomer (spam {:.1}% ≥ {:.0}%); deleting and banning.",
                    inp.cls.spam_score * 100.0,
                    inp.spam_threshold * 100.0
                ),
                spam_hit,
                language_hit,
            };
        }
        // Protected member: compromised account or model mistake — never auto-ban.
        if inp.prior_warns >= 2 {
            return EnforcementDecision {
                action: Action::NotifyMods,
                explanation: format!(
                    "Protected member with {} prior warnings posted spam-like content ({:.1}%); deleting and escalating to moderators instead of banning.",
                    inp.prior_warns,
                    inp.cls.spam_score * 100.0
                ),
                spam_hit,
                language_hit,
            };
        }
        return EnforcementDecision {
            action: Action::DeleteWarn,
            explanation: format!(
                "Protected member posted spam-like content (spam {:.1}% ≥ {:.0}%); deleting and warning, no ban.",
                inp.cls.spam_score * 100.0,
                inp.spam_threshold * 100.0
            ),
            spam_hit,
            language_hit,
        };
    }

    // Language path: independent of spam, never a ban.
    if language_hit {
        match inp.language_action {
            "ignore" => {
                return EnforcementDecision {
                    action: Action::None,
                    explanation: "Non-Arabic message, but language policy is set to ignore."
                        .to_string(),
                    spam_hit,
                    language_hit,
                }
            }
            "delete" => {
                if inp.dry_run {
                    return EnforcementDecision {
                        action: Action::NotifyMods,
                        explanation: "Dry-run: language violation would be deleted.".to_string(),
                        spam_hit,
                        language_hit,
                    };
                }
                return EnforcementDecision {
                    action: Action::Delete,
                    explanation: format!(
                        "Language-policy violation ({:.1}% ≥ {:.0}%); deleting without spam punishment.",
                        inp.cls.language_score * 100.0,
                        inp.language_threshold * 100.0
                    ),
                    spam_hit,
                    language_hit,
                };
            }
            _ => {
                // "warn"
                if inp.dry_run {
                    return EnforcementDecision {
                        action: Action::NotifyMods,
                        explanation: "Dry-run: language violation would be warned.".to_string(),
                        spam_hit,
                        language_hit,
                    };
                }
                return EnforcementDecision {
                    action: Action::DeleteWarn,
                    explanation: format!(
                        "Language-policy violation ({:.1}% ≥ {:.0}%); deleting and warning (not treated as financial scam).",
                        inp.cls.language_score * 100.0,
                        inp.language_threshold * 100.0
                    ),
                    spam_hit,
                    language_hit,
                };
            }
        }
    }

    EnforcementDecision {
        action: Action::None,
        explanation: format!(
            "Scores below thresholds (spam {:.1}% < {:.0}%, language {:.1}% < {:.0}%); no action.",
            inp.cls.spam_score * 100.0,
            inp.spam_threshold * 100.0,
            inp.cls.language_score * 100.0,
            inp.language_threshold * 100.0
        ),
        spam_hit,
        language_hit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cls(spam: f64, lang: f64) -> Classification {
        Classification {
            spam_score: spam,
            language_score: lang,
            category: "investment".to_string(),
            category_probs: HashMap::new(),
            model: "m".to_string(),
            raw: serde_json::json!({}),
        }
    }

    fn base(c: &Classification) -> EnforcementInput<'_> {
        EnforcementInput {
            cls: c,
            spam_threshold: 0.9,
            language_threshold: 0.9,
            language_action: "warn",
            protected: false,
            excluded: false,
            dry_run: false,
            prior_warns: 0,
        }
    }

    #[test]
    fn newcomer_clear_spam_banned() {
        let c = cls(0.99, 0.1);
        let d = decide(base(&c));
        assert_eq!(d.action, Action::DeleteBan);
    }

    #[test]
    fn ambiguous_spam_not_banned() {
        let c = cls(0.899, 0.1);
        let d = decide(base(&c));
        assert_eq!(d.action, Action::None);
    }

    #[test]
    fn protected_member_never_banned() {
        let c = cls(0.99, 0.1);
        let mut i = base(&c);
        i.protected = true;
        assert_eq!(decide(i).action, Action::DeleteWarn);
    }

    #[test]
    fn protected_repeat_offender_escalates_without_ban() {
        let c = cls(0.99, 0.1);
        let mut i = base(&c);
        i.protected = true;
        i.prior_warns = 3;
        assert_eq!(decide(i).action, Action::NotifyMods);
    }

    #[test]
    fn excluded_users_never_punished() {
        let c = cls(0.99, 0.99);
        let mut i = base(&c);
        i.excluded = true;
        assert_eq!(decide(i).action, Action::None);
    }

    #[test]
    fn dry_run_never_destructive() {
        let c = cls(0.99, 0.1);
        let mut i = base(&c);
        i.dry_run = true;
        assert!(!decide(i).action.destructive());
    }

    #[test]
    fn language_violation_not_a_ban() {
        let c = cls(0.1, 0.95);
        let mut i = base(&c);
        i.protected = false;
        let d = decide(i);
        assert_ne!(d.action, Action::DeleteBan);
        assert_eq!(d.action, Action::DeleteWarn);
    }
}
