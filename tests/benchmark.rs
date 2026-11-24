use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use mirsad::benchmark::Benchmark;
use serde_json::{json, Value};

#[test]
fn private_dataset_round_trips_through_local_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("private");
    let config = tmp.path().join("config.toml");
    let output = tmp.path().join("export.jsonl");
    let room = "!test:example.org";
    let target = "$target";
    fs::write(
        &config,
        format!("[benchmark]\ndirectory = {:?}\nrooms = [{room:?}]\n", dir),
    )
    .unwrap();
    assert!(Benchmark::open(&dir, false).is_err());
    let db = Benchmark::open(&dir, true).unwrap();
    for id in ["$report1", "$report2", "$report1"] {
        db.record(
            "manual_report",
            room,
            Some(id),
            json!({"target_event_id": target, "report_event_id": id}),
        )
        .unwrap();
    }
    for model in ["first", "second"] {
        db.record(
            "analysis",
            room,
            None,
            json!({"target_event_id": target, "requested_model": model,
            "text_state": {"message": "test\nmessage"}}),
        )
        .unwrap();
    }
    db.record(
        "external_ban",
        room,
        Some("$ban"),
        json!({"subject_user_id": "@subject:example.org"}),
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_mirsad"))
            .arg("--config")
            .arg(&config)
            .args(args)
            .output()
            .unwrap()
    };
    assert!(!run(&[
        "benchmark-feedback",
        "--room",
        "!other:example.org",
        "--event",
        target,
        "--label",
        "spam"
    ])
    .status
    .success());
    let feedback = run(&[
        "benchmark-feedback",
        "--room",
        room,
        "--event",
        target,
        "--label",
        "false-positive",
    ]);
    assert!(
        feedback.status.success(),
        "{}",
        String::from_utf8_lossy(&feedback.stderr)
    );
    let export = run(&["benchmark-export", output.to_str().unwrap()]);
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert!(!run(&["benchmark-export", output.to_str().unwrap()])
        .status
        .success());
    let rows: Vec<Value> = fs::read_to_string(&output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0]["data"]["report_event_id"], "$report1");
    assert_eq!(rows[1]["data"]["report_event_id"], "$report2");
    assert_eq!(rows[2]["data"]["requested_model"], "first");
    assert_eq!(rows[3]["data"]["requested_model"], "second");
    assert!(rows[4]["data"].get("target_event_id").is_none());
    assert_eq!(rows[5]["kind"], "moderator_feedback");
    assert_eq!(rows[5]["data"]["label"], "false_positive");
    for path in [&dir, &dir.join("benchmark.sqlite3"), &output] {
        assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o077, 0);
    }
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Benchmark::open(&dir, false).is_err());
}
