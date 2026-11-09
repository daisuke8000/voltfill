use serde_json::{Value, json};
use std::process::{Command, Output};

fn serve(args: &[&str], env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_voltfill"))
        .env_clear()
        .envs(env.iter().copied())
        .arg("serve")
        .args(args)
        .output()
        .expect("Cargo-built server must launch")
}

#[test]
fn server_logs_are_opt_in_and_disabled_errors_remain_available() {
    for level in [None, Some("off"), Some("info")] {
        let mut args = vec![
            "--resource-config",
            "missing-resource-config-for-logging-test.json",
        ];
        if let Some(level) = level {
            args.extend(["--log-level", level]);
        }
        let output = serve(&args, &[]);
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
                serde_json::from_slice::<Value>(&output.stderr).expect("a JSON error"),
                json!({"error":"input_read_failed"})
            );
        }
    }
}

#[test]
fn server_arguments_environment_and_credential_files_are_validated_before_listening() {
    let dir = std::env::temp_dir().join(format!("voltfill-serve-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create the test directory");
    let key = dir.join("synthetic-server-key");
    std::fs::write(&key, "synthetic-server-canary\r\n").expect("write the synthetic key");
    let key = key.to_str().expect("UTF-8 path");
    let key_file = ("VOLTFILL_SERVER_API_KEY_FILE", key);
    for (args, env, error) in [
        (vec!["--remote"], vec![], "invalid_arguments"),
        (vec!["--listen"], vec![], "invalid_arguments"),
        (
            vec!["--listen", "a", "--listen", "b"],
            vec![],
            "invalid_arguments",
        ),
        (
            vec![],
            vec![("VOLTFILL_LISTEN", "invalid-private-canary")],
            "invalid_listen_address",
        ),
        (
            vec!["--listen", "0.0.0.0:0"],
            vec![],
            "authentication_required_for_non_loopback",
        ),
        (vec!["--log-level", "trace"], vec![], "invalid_log_level"),
        (
            vec!["--listen", "0.0.0.0:0", "--upload-timeout-ms", "0"],
            vec![("VOLTFILL_LISTEN", "invalid-private-canary"), key_file],
            "invalid_upload_timeout",
        ),
        (
            vec![],
            vec![
                key_file,
                ("VOLTFILL_SERVER_API_KEY", "synthetic-server-canary"),
            ],
            "credential_sources_conflict",
        ),
        (
            vec![],
            vec![("VOLTFILL_UPLOAD_TIMEOUT_MS", "0")],
            "invalid_upload_timeout",
        ),
    ] {
        let output = serve(&args, &env);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).expect("a JSON error")["error"],
            error,
            "{args:?}"
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("canary"));
    }
    std::fs::remove_dir_all(&dir).ok();
}
