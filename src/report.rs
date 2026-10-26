//! Public diagnostic replies for manual `/spam` reports.
//!
//! Every report triggers a FRESH classification and a public reply, including
//! when the message turns out legitimate. The reply must explain the model
//! outputs + the bot's rules without inventing internal model reasoning.

use crate::{decider::Classification, enforcement::EnforcementDecision};

#[allow(clippy::too_many_arguments)]
pub fn diagnostic_message(
    reported_body_snippet: &str,
    reported_event_id: &str,
    author: &str,
    cls: &Classification,
    spam_threshold: f64,
    language_score: Option<f64>,
    language_threshold: Option<f64>,
    trust_label: &str,
    protection_label: &str,
    history_label: &str,
    decision: &EnforcementDecision,
    duplicate: bool,
    // Enforcement failures (absent/empty = everything worked). Rendered so a
    // failed redact/ban can never again look like a completed action.
    outcome_failures: Option<&str>,
) -> String {
    let mut cats: Vec<(&String, &f64)> = cls.category_probs.iter().collect();
    cats.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    let cat_line = if cats.is_empty() {
        format!("Category: {}", cls.category)
    } else {
        let top: Vec<String> = cats
            .iter()
            .take(4)
            .map(|(k, v)| format!("{k} {:.1}%", **v * 100.0))
            .collect();
        format!("Category: {} ({})", cls.category, top.join(", "))
    };
    let dup = if duplicate {
        "\nNote: this message was already enforced; no duplicate punishment applied."
    } else {
        ""
    };
    let outcome = match outcome_failures {
        Some(f) if !f.is_empty() => {
            format!("\n⚠ Enforcement incomplete: {f}")
        }
        _ => String::new(),
    };
    let lang_line = match (language_score, language_threshold) {
        (Some(s), Some(t)) => {
            format!(
                "Language violation: {:.1}% (threshold {:.0}%)",
                s * 100.0,
                t * 100.0
            )
        }
        _ => "Language check: n/a (disabled)".to_string(),
    };
    format!(
        "Mirsad report — {reported_event_id} by {author}\n\
         Evaluated: {snippet}\n\
         Model: {model}\n\
         Spam: {spam:.1}% (threshold {spam_th:.0}%)\n\
         {lang_line}\n\
         {cat_line}\n\
         Trust: {trust_label} · Protection: {protection_label}\n\
         History: {history_label}\n\
         Result: {action}\n\
         {explanation}{dup}{outcome}",
        snippet = snippet(reported_body_snippet),
        model = cls.model,
        spam = cls.spam_score * 100.0,
        spam_th = spam_threshold * 100.0,
        action = action_label(&decision.action),
        explanation = decision.explanation,
    )
}

fn snippet(s: &str) -> String {
    const MAX: usize = 280;
    let t: String = s.chars().take(MAX).collect();
    let t = t.replace('\n', " ");
    if s.chars().count() > MAX {
        format!("{t}…")
    } else {
        t
    }
}

fn action_label(a: &crate::enforcement::Action) -> &'static str {
    match a {
        crate::enforcement::Action::None => "No action",
        crate::enforcement::Action::Delete => "Message deleted",
        crate::enforcement::Action::DeleteWarn => "Message deleted + warned",
        crate::enforcement::Action::DeleteBan => "Message deleted + user banned",
        crate::enforcement::Action::NotifyMods => "Flagged for moderator review",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enforcement::Action;
    use std::collections::HashMap;

    #[test]
    fn diagnostic_contains_scores_and_thresholds() {
        let cls = Classification {
            spam_score: 0.899,
            language_score: 0.1,
            category: "investment".to_string(),
            category_probs: HashMap::from([
                ("investment".to_string(), 0.7),
                ("legitimate".to_string(), 0.3),
            ]),
            model: "perplexity/pplx-decider-v1.2".to_string(),
            raw: serde_json::json!({}),
        };
        let d = EnforcementDecision {
            action: Action::None,
            explanation: "below threshold".to_string(),
            spam_hit: false,
            language_hit: false,
        };
        let msg = diagnostic_message(
            "hello world",
            "$ev1",
            "@u:x",
            &cls,
            0.9,
            Some(0.1),
            Some(0.9),
            "untrusted newcomer",
            "verified newcomer",
            "no prior room history",
            &d,
            false,
            None,
        );
        assert!(msg.contains("89.9%"));
        assert!(msg.contains("threshold 90%"));
        assert!(msg.contains("No action"));
        assert!(msg.contains("investment"));
    }

    #[test]
    fn diagnostic_marks_disabled_language_check() {
        let cls = Classification {
            spam_score: 0.99,
            language_score: 0.0,
            category: "none".to_string(),
            category_probs: HashMap::new(),
            model: "m".to_string(),
            raw: serde_json::json!({}),
        };
        let d = EnforcementDecision {
            action: Action::DeleteBan,
            explanation: "ban".to_string(),
            spam_hit: true,
            language_hit: false,
        };
        let msg = diagnostic_message(
            "x", "$e", "@u:x", &cls, 0.9, None, None, "t", "p", "h", &d, false, None,
        );
        assert!(msg.contains("n/a (disabled)"));
        assert!(!msg.contains("Language violation:"));
    }

    #[test]
    fn diagnostic_shows_enforcement_failures() {
        let cls = Classification {
            spam_score: 0.99,
            language_score: 0.0,
            category: "employment".to_string(),
            category_probs: HashMap::new(),
            model: "m".to_string(),
            raw: serde_json::json!({}),
        };
        let d = EnforcementDecision {
            action: Action::DeleteWarn,
            explanation: "would delete+warn".to_string(),
            spam_hit: true,
            language_hit: false,
        };
        let clean = diagnostic_message(
            "x",
            "$e",
            "@u:x",
            &cls,
            0.9,
            Some(0.0),
            Some(0.9),
            "t",
            "p",
            "h",
            &d,
            false,
            None,
        );
        assert!(!clean.contains("Enforcement incomplete"));
        let failed = diagnostic_message(
            "x",
            "$e",
            "@u:x",
            &cls,
            0.9,
            Some(0.0),
            Some(0.9),
            "t",
            "p",
            "h",
            &d,
            false,
            Some("message NOT deleted: 403"),
        );
        assert!(failed.contains("Enforcement incomplete"));
        assert!(failed.contains("NOT deleted"));
    }
}
