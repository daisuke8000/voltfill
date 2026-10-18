use serde_json::{Value, json};

#[test]
fn server_otlp_is_opt_in_and_disabled_errors_remain_available() {
    for level in [None, Some("off"), Some("info")] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_voltfill-server"));
        command
            .env_clear()
            .args(["--mock", "missing-fixture-for-logging-test.json"]);
        if let Some(level) = level {
            command.args(["--log-level", level]);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        if level == Some("info") {
            assert!(output.stderr.is_empty());
            let logs: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(logs["resourceLogs"].is_array());
            assert!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .contains("input_read_failed")
            );
        } else {
            assert!(output.stdout.is_empty());
            assert_eq!(
                serde_json::from_slice::<Value>(&output.stderr).unwrap(),
                json!({"error":"input_read_failed"})
            );
        }
    }
}

#[test]
fn remote_alias_does_not_require_an_exclusive_organization_flag() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_voltfill-server"))
        .env_clear()
        .args(["--remote", "--allow-images"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stderr).unwrap(),
        json!({"error":"api_key_required"})
    );
}
