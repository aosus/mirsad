//! Manual vision probe — ignored by default, never runs in CI.
//!
//! Requires env: OPENROUTER_API_KEY, MIRSAD_TEST_IMAGE (png/jpg path).
//! Optional: MIRSAD_TEST_MODEL (default perplexity/pplx-decider-v1.2),
//! MIRSAD_TEST_POLICY (default config/decision-questions.json).
//!
//! Run: OPENROUTER_API_KEY=sk-... MIRSAD_TEST_IMAGE=/tmp/shot.png \
//!   cargo test --test vision_probe -- --ignored --nocapture

#[tokio::test]
#[ignore = "needs live OpenRouter key + sample image; run manually"]
async fn vision_probe_flags_photo_spam() {
    let key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    if key.is_empty() {
        println!("SKIP: OPENROUTER_API_KEY not set");
        return;
    }
    let img_path = std::env::var("MIRSAD_TEST_IMAGE").unwrap_or_default();
    if img_path.is_empty() {
        println!("SKIP: MIRSAD_TEST_IMAGE not set");
        return;
    }
    let model = std::env::var("MIRSAD_TEST_MODEL")
        .unwrap_or_else(|_| "perplexity/pplx-decider-v1.2".to_string());
    let policy_path = std::env::var("MIRSAD_TEST_POLICY")
        .unwrap_or_else(|_| "config/decision-questions.json".to_string());

    let bytes = std::fs::read(&img_path).expect("read test image");
    println!("image bytes: {}", bytes.len());
    let policy_text = std::fs::read_to_string(&policy_path).expect("read policy");
    let policy: serde_json::Value = serde_json::from_str(&policy_text).expect("parse policy");
    let questions = policy["questions"].clone();
    assert!(questions.is_object(), "policy must have a questions object");

    let client = mirsad::decider::DeciderClient::new(
        "https://openrouter.ai/api/alpha/decisions".to_string(),
        model.clone(),
        key,
        "mirsad-probe".to_string(),
        None,
        90,
    )
    .expect("decider client");

    // Mimic a real m.image event body (usually just a filename).
    let filename = std::path::Path::new(&img_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("image.jpg");
    let img = mirsad::decider::ImageAttachment {
        bytes,
        mime: mirsad::decider::guess_mime(filename),
    };
    let state = mirsad::decider::message_state_with_image(
        "image.jpg",
        "@probe:aosus.org",
        "!probe:aosus.org",
        4000,
        &img,
    );

    let resp = client
        .classify(state, questions)
        .await
        .expect("decisions call");
    let cls = mirsad::decider::Classification::from_response(
        &resp,
        serde_json::json!({}),
        "spam",
        "",
        "spam_category",
        &model,
    );
    println!(
        "RESULT model={} spam={:.1}% category={} probs={:?}",
        cls.model,
        cls.spam_score * 100.0,
        cls.category,
        cls.category_probs
    );
    println!(
        "ENFORCE at 0.90 threshold: {}",
        if cls.spam_score >= 0.9 {
            "BAN (>= threshold)"
        } else {
            "no action (< threshold)"
        }
    );
}
