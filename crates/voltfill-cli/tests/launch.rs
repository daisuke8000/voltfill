//! Installed-command paths exercised without Jev or caller credentials.
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "voltfill-launch-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let p = self.0.join(name);
        fs::write(&p, text).unwrap();
        p
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}
fn cli(args: &[&str], cwd: &Temp) -> Output {
    Command::new(env!("CARGO_BIN_EXE_voltfill"))
        .args(args)
        .current_dir(&cwd.0)
        .env_clear()
        .output()
        .unwrap()
}
#[test]
fn unsafe_or_ambiguous_configs_fail_without_disclosing_contents_or_reading_keys() {
    let temp = Temp::new();
    for (ext, text) in [
        ("yaml", "run:\n  remote: true\n  remote: false\n"),
        ("yaml", "run: &a {pdf: sample.pdf}\nserve: *a\n"),
        ("yaml", "run: {remote: true}\n---\nrun: {remote: false}\n"),
        ("yaml", "run: {remote: yes}\n"),
        ("yaml", "run: !include secret-canary-private.yaml\n"),
        ("toml", "[run]\nremote = true\nremote = false\n"),
        ("toml", "[run]\nremote = true\napi_key = 'secret-canary'\n"),
        ("json", r#"{"run":{"remote":true,"remote":false}}"#),
    ] {
        let p = temp.file(&format!("bad.{ext}"), text);
        let out = cli(&["run", "--config", p.to_str().unwrap()], &temp);
        assert_eq!(out.status.code(), Some(2), "{text}");
        let err = String::from_utf8(out.stderr).unwrap();
        assert!(!err.contains("secret-canary"));
        assert!(!err.contains("api_key_required"));
    }
    let p = temp.file("oversized.yaml", &" ".repeat(65537));
    let out = cli(&["run", "-c", p.to_str().unwrap()], &temp);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("input_too_large"));
}

#[test]
fn explicit_empty_server_config_is_an_error_for_launch_and_legacy_files() {
    let temp = Temp::new();
    for (name, content) in [("empty.yaml", "serve: {}\n"), ("empty.json", "{}")] {
        let p = temp.file(name, content);
        let out = cli(&["serve", "--config", p.to_str().unwrap()], &temp);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("remote_permission_required"));
    }
}

#[test]
fn server_environment_and_credential_files_are_validated_before_listening() {
    let temp = Temp::new();
    let key = temp.file("synthetic-server-key", "synthetic-server-canary\r\n");
    let fixture = root().join("examples/text-mock.json");
    for (args, variables, error) in [
        (
            vec![],
            vec![("VOLTFILL_LISTEN", "invalid-private-canary")],
            "invalid_listen_address",
        ),
        (
            vec!["--listen", "0.0.0.0:0", "--model", "invalid"],
            vec![
                ("VOLTFILL_LISTEN", "invalid-private-canary"),
                ("VOLTFILL_SERVER_API_KEY_FILE", key.to_str().unwrap()),
            ],
            "invalid_model",
        ),
        (
            vec![],
            vec![
                ("VOLTFILL_SERVER_API_KEY_FILE", key.to_str().unwrap()),
                ("VOLTFILL_SERVER_API_KEY", "synthetic-server-canary"),
            ],
            "credential_sources_conflict",
        ),
        (
            vec![],
            vec![("VOLTFILL_UPLOAD_TIMEOUT_MS", "0")],
            "invalid_upload_timeout",
        ),
        (
            vec![],
            vec![("VOLTFILL_REUSE_DOCUMENT_TTL_MS", "0")],
            "invalid_reuse_document_ttl",
        ),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_voltfill"));
        command
            .args(["serve", "--mock", fixture.to_str().unwrap()])
            .args(args)
            .env_clear();
        for (name, value) in variables {
            command.env(name, value);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stderr).unwrap()["error"],
            error
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("canary"));
    }
}
