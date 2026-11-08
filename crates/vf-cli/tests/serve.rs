use serde_json::{Value, json};

#[test]
fn server_logs_are_opt_in_and_disabled_errors_remain_available() {
    for level in [None, Some("off"), Some("info")] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_voltfill"));
        command
            .env_clear()
            .args(["serve", "--mock", "missing-fixture-for-logging-test.json"]);
        if let Some(level) = level {
            command.args(["--log-level", level]);
        }
        let output = command.output().expect("Cargo-built server must launch");
        assert_eq!(output.status.code(), Some(2));
        if level == Some("info") {
            assert!(output.stderr.is_empty());
            let logs = String::from_utf8(output.stdout).expect("log output must be UTF-8");
            let events: Vec<Value> = logs
                .lines()
                .map(|line| {
                    serde_json::from_str(line).expect("each log line must be a JSON object")
                })
                .collect();
            assert!(!events.is_empty());
            assert!(events.iter().all(|e| e["service"] == "voltfill-server"));
            assert!(logs.contains("input_read_failed"));
        } else {
            assert!(output.stdout.is_empty());
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stderr)
                    .expect("disabled logging must preserve the JSON error response"),
                json!({"error":"input_read_failed"})
            );
        }
    }
}

#[test]
fn remote_alias_does_not_require_an_exclusive_organization_flag() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_voltfill"))
        .env_clear()
        .args(["serve", "--remote", "--allow-images"])
        .output()
        .expect("Cargo-built server must launch");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr)
            .expect("disabled logging must preserve the JSON error response"),
        json!({"error":"api_key_required"})
    );
}
