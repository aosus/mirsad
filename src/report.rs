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
    language_threshold: f64,
    trust_label: &str,
    protection_label: &str,
    decision: &EnforcementDecision,
    duplicate: bool,
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
    format!(
        "Mirsad report — {reported_event_id} by {author}\n\
         Evaluated: {snippet}\n\
         Model: {model}\n\
         Spam: {spam:.1}% (threshold {spam_th:.0}%)\n\
         Language violation: {lang:.1}% (threshold {lang_th:.0}%)\n\
         {cat_line}\n\
         Trust: {trust_label} · Protection: {protection_label}\n\
         Result: {action}\n\
         {explanation}{dup}",
        snippet = snippet(reported_body_snippet),
        model = cls.model,
        spam = cls.spam_score * 100.0,
        spam_th = spam_threshold * 100.0,
        lang = cls.language_score * 100.0,
        lang_th = language_threshold * 100.0,
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

/// Reply sent when someone reports one of the bot's own messages (a diagnostic
/// report, warning, etc.). We never classify our own output.
pub fn own_message_response() -> &'static str {
    "That's one of my own messages (a report, warning or notice) — I don't analyze my own output. If the original suspicious message is still there, reply to *it* with /spam and I'll check it."
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enforcement::Action;
    use std::collections::HashMap;

    #[test]
    fn own_message_response_points_back_to_original() {
        let msg = own_message_response();
        assert!(msg.contains("/spam"));
        // Must not contain anything that looks like a verdict on the message.
        assert!(!msg.contains("Spam:"));
    }

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
            0.9,
            "untrusted newcomer",
            "verified newcomer",
            &d,
            false,
        );
        assert!(msg.contains("89.9%"));
        assert!(msg.contains("threshold 90%"));
        assert!(msg.contains("No action"));
        assert!(msg.contains("investment"));
    }
}
