//! `voltfill fill` with and without `--remote`. No test configures both a
//! usable endpoint and a key, so no request can be sent.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "voltfill-fill-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).expect("create the isolated fill-test directory");
        Self(p)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).expect("write the synthetic input");
        path(&p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn path(p: &Path) -> String {
    p.to_str().expect("temp paths are UTF-8").to_owned()
}

/// A one-page PDF with one Helvetica text line per entry.
fn text_pdf(lines: &[&str]) -> Vec<u8> {
    let content: String = lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("BT /F1 12 Tf 72 {} Td ({line}) Tj ET\n", 700 - 40 * i))
        .collect();
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
    ];
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf += &format!("{} 0 obj\n{object}\nendobj\n", i + 1);
    }
    let xref = pdf.len();
    pdf += &format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        pdf += &format!("{offset:010} 00000 n \n");
    }
    pdf += &format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    pdf.into_bytes()
}

fn cli(args: &[&str], cwd: &Temp, env: &[(&str, &str)]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_voltfill"))
        .args(args)
        .current_dir(&cwd.0)
        .env_clear()
        .envs(env.iter().copied())
        .output()
        .expect("launch the built CLI")
}

fn inputs(temp: &Temp) -> (String, String) {
    let pdf = temp.file(
        "input.pdf",
        &text_pdf(&["Invoice No: INV-7", "Subtotal: 100", "Total: 120"]),
    );
    let form = temp.file(
        "form.json",
        br#"{"fields":[{"id":"no","label":"Invoice No"},{"id":"amount","label":"Amount","type":"amount","hints":["Subtotal","Total"]}]}"#,
    );
    (pdf, form)
}

fn error(out: &Output) -> String {
    let value: Value = serde_json::from_slice(&out.stderr).expect("CLI errors are JSON");
    value["error"].as_str().expect("error code").to_owned()
}

#[test]
fn offline_fill_reads_no_credentials_and_keeps_its_shape() {
    let temp = Temp::new();
    let (pdf, form) = inputs(&temp);
    let missing = path(&temp.0.join("missing-credential"));
    let clean = cli(&["fill", &pdf, &form], &temp, &[]);
    assert_eq!(clean.status.code(), Some(0));
    // Each of these would fail the run if fill read it without --remote.
    let hostile = cli(
        &["fill", &pdf, &form],
        &temp,
        &[
            ("VOLTFILL_JEV_API_KEY", "synthetic_secret_canary"),
            ("VOLTFILL_JEV_API_KEY_FILE", &missing),
            ("VOLTFILL_JEV_ENDPOINT", "http://127.0.0.1:9/"),
            ("VOLTFILL_MODEL", "not-a-model"),
            ("VOLTFILL_EXECUTION_CONFIG", &missing),
        ],
    );
    assert_eq!(hostile.status.code(), Some(0), "{:?}", hostile.stderr);
    assert!(hostile.stderr.is_empty());
    assert_eq!(hostile.stdout, clean.stdout);
    let result: Value = serde_json::from_slice(&clean.stdout).expect("the result is JSON");
    let keys: Vec<&String> = result
        .as_object()
        .expect("the result is an object")
        .keys()
        .collect();
    assert_eq!(keys, ["document_id", "evaluation_only", "fields"]);
    assert_eq!(result["fields"]["amount"]["status"], "ambiguous");
    assert_eq!(result["fields"]["no"]["raw"], "INV-7");
    assert!(!String::from_utf8_lossy(&hostile.stdout).contains("synthetic_secret_canary"));
}

#[test]
fn remote_fill_checks_its_settings_before_sending() {
    let temp = Temp::new();
    let (pdf, form) = inputs(&temp);
    let key_file = temp.file("key", b"synthetic_secret_canary\n");
    let key = ("VOLTFILL_JEV_API_KEY", "synthetic_secret_canary");
    for (args, env, reason) in [
        (vec!["--remote"], vec![], "api_key_required"),
        (vec!["--remote"], vec![key], "jev_endpoint_required"),
        (
            vec!["--remote"],
            vec![key, ("VOLTFILL_JEV_ENDPOINT", "http://127.0.0.1:9/")],
            "invalid_jev_endpoint",
        ),
        (
            vec!["--remote"],
            vec![key, ("VOLTFILL_JEV_API_KEY_FILE", key_file.as_str())],
            "credential_sources_conflict",
        ),
        (
            vec!["--remote", "--model", "gpt"],
            vec![key],
            "invalid_model",
        ),
        (
            vec!["--remote"],
            vec![key, ("VOLTFILL_MODEL", "gpt")],
            "invalid_model",
        ),
        (
            vec!["--remote", "--execution-config", form.as_str()],
            vec![key],
            "invalid_execution_config",
        ),
        (
            vec!["--remote", "--max-attempts", "4"],
            vec![key],
            "invalid_arguments",
        ),
    ] {
        let mut full = vec!["fill", pdf.as_str(), form.as_str()];
        full.extend(args.iter().copied());
        let out = cli(&full, &temp, &env);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
        assert_eq!(error(&out), reason, "{args:?}");
        assert!(!String::from_utf8_lossy(&out.stderr).contains("synthetic_secret_canary"));
    }
}
