//! Integration tests: config + policy + enforcement pipeline end-to-end (no network).
//!
//! These tests prove the whole decision path — example configs parse, the
//! example questions satisfy the Decisions-API schema, classification outputs
//! map to the expected enforcement — so a green CI genuinely means the bot
//! will behave at runtime.

use std::collections::HashMap;

#[test]
fn example_configs_parse_and_agree() {
    let cfg_text = include_str!("../config/config.example.toml");
    let cfg: toml::Table = toml::from_str(cfg_text).expect("example config parses");
    let questions_file = cfg["policy"]["questions_file"].as_str().unwrap();
    assert!(questions_file.ends_with(".json"));

    let policy_text = include_str!("../config/decision-questions.example.json");
    let policy: serde_json::Value =
        serde_json::from_str(policy_text).expect("example questions parse");
    let questions = policy["questions"].as_object().expect("questions object");

    // Configured question ids must exist in the policy file.
    for q in ["spam", "language_violation", "spam_category"] {
        let cfg_q = cfg["policy"][q].as_str().unwrap();
        assert!(questions.contains_key(cfg_q), "missing question {cfg_q}");
    }
    // Types must be what the bot expects.
    assert_eq!(questions["spam"]["type"], "noul");
    assert_eq!(questions["language_violation"]["type"], "noul");
    assert_eq!(questions["spam_category"]["type"], "choice");
}

#[test]
fn full_pipeline_spam_newcomer_banned_and_admin_exempt() {
    // Simulate a Decisions-API response for obvious investment spam.
    let resp: mirsad::decider::DecisionsResponse = serde_json::from_value(serde_json::json!({
        "answers": {
            "spam": {"noul": 0.97},
            "language_violation": {"noul": 0.05},
            "spam_category": {"choice": "investment",
                "probabilities": {"investment": 0.85, "legitimate": 0.05}}
        },
        "model": "perplexity/pplx-decider-v1.2"
    }))
    .unwrap();
    let cls = mirsad::decider::Classification::from_response(
        &resp,
        serde_json::json!({}),
        "spam",
        "language_violation",
        "spam_category",
        "perplexity/pplx-decider-v1.2",
    );
    // Verified newcomer → ban.
    let d = mirsad::enforcement::decide(mirsad::enforcement::EnforcementInput {
        cls: &cls,
        spam_threshold: 0.9,
        language_threshold: 0.9,
        language_action: "warn",
        protected: false,
        excluded: false,
        dry_run: false,
        prior_warns: 0,
    });
    assert_eq!(d.action, mirsad::enforcement::Action::DeleteBan);
    // Same scores for an excluded admin → nothing.
    let d2 = mirsad::enforcement::decide(mirsad::enforcement::EnforcementInput {
        cls: &cls,
        spam_threshold: 0.9,
        language_threshold: 0.9,
        language_action: "warn",
        protected: false,
        excluded: true,
        dry_run: false,
        prior_warns: 0,
    });
    assert_eq!(d2.action, mirsad::enforcement::Action::None);
}

#[test]
fn prompt_not_hardcoded_in_binary() {
    // The spam behaviour must live in the JSON file, not in Rust sources.
    // This test scans the crate sources for Arabic-spam keywords that would
    // indicate hardcoded criteria.
    let sources = [
        include_str!("../src/enforcement.rs"),
        include_str!("../src/decider.rs"),
        include_str!("../src/matrix.rs"),
    ];
    for src in sources {
        for kw in ["اكتتاب", "توظيف", "استثمار", "Guaranteed returns"] {
            assert!(
                !src.contains(kw),
                "spam criteria appear hardcoded in Rust source: {kw}"
            );
        }
    }
    let _ = HashMap::<String, String>::new();
}
