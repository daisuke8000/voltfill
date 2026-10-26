use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root must exist relative to the CLI manifest")
}

fn cli(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_voltfill"))
        .args(args)
        .current_dir(root())
        .env_clear()
        .output()
        .expect("Cargo-built CLI must launch")
}

fn assert_error_response(bytes: &[u8], reason: &str) {
    assert_eq!(
        serde_json::from_slice::<Value>(bytes).expect("CLI errors must be JSON"),
        serde_json::json!({"error": reason})
    );
}

#[test]
fn concurrency_flag_accepts_values_above_the_default() {
    let out = cli(&[
        "extract",
        "--pdf",
        "missing-private-path",
        "--allow-remote",
        "--max-concurrency",
        "64",
    ]);
    let error = serde_json::from_slice::<Value>(&out.stderr)
        .expect("validation failure must return a JSON error")["error"]
        .as_str()
        .expect("error code must be a string")
        .to_string();
    assert!(
        !["invalid_arguments", "invalid_execution_config"].contains(&error.as_str()),
        "{error}"
    );
}

#[test]
fn retry_and_logging_help_and_validation_precede_input_or_credentials() {
    let out = cli(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let help = String::from_utf8(out.stdout).expect("CLI help must be UTF-8");
    for phrase in [
        "--max-attempts",
        "--max-concurrency",
        "--execution-config",
        "default: 3",
        "--log-level",
        "default: off",
        "stderr",
        "stdout",
    ] {
        assert!(help.contains(phrase), "{phrase}");
    }
    for (flag, invalid) in [
        ("--max-attempts", "0"),
        ("--max-attempts", "4"),
        ("--max-attempts", "-1"),
        ("--max-attempts", "01"),
        ("--max-attempts", "unbounded"),
        ("--max-concurrency", "0"),
        ("--max-concurrency", "02"),
        ("--log-level", "trace"),
        ("--log-level", "DEBUG"),
    ] {
        let out = cli(&[
            "extract",
            "--pdf",
            "missing-private-path",
            "--allow-remote",
            flag,
            invalid,
        ]);
        assert_eq!(out.status.code(), Some(2));
        assert_error_response(&out.stderr, "invalid_arguments");
        assert!(out.stdout.is_empty());
    }
    for flags in [
        ["--max-attempts", "2", "--max-attempts", "3"],
        ["--log-level", "info", "--log-level", "debug"],
    ] {
        let mut args = vec!["extract"];
        args.extend(flags);
        let out = cli(&args);
        assert_error_response(&out.stderr, "invalid_arguments");
    }
}
