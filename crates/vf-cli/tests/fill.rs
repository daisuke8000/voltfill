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

#[test]
fn a_refused_output_target_fails_before_credentials_are_read() {
    let temp = Temp::new();
    let (pdf, form) = inputs(&temp);
    let key_file = temp.file("key", b"synthetic_secret_canary\n");
    let link = temp.0.join("link.json");
    #[cfg(unix)]
    std::os::unix::fs::symlink(temp.0.join("elsewhere.json"), &link)
        .expect("create the output symlink");
    let link = path(&link);
    let originals: Vec<Vec<u8>> = [&pdf, &form, &key_file]
        .iter()
        .map(|p| std::fs::read(p).expect("read the synthetic input"))
        .collect();
    let key_env = [("VOLTFILL_JEV_API_KEY_FILE", key_file.as_str())];
    // Without the preflight these would fail later with api_key_required or
    // jev_endpoint_required, after reading the inputs and the key.
    type Env<'a> = &'a [(&'a str, &'a str)];
    let mut cases: Vec<(Vec<&str>, Env, &str)> = vec![
        (
            vec!["fill", &pdf, &form, "--remote", "-o", &form],
            &[],
            "input_overwrite_refused",
        ),
        (
            vec!["fill", &pdf, &form, "--remote", "-o", &key_file],
            &key_env,
            "input_overwrite_refused",
        ),
        // The legacy run/extract command shares the same preflight.
        (
            vec!["run", &pdf, &form, "--remote", "-o", &form],
            &[],
            "input_overwrite_refused",
        ),
    ];
    if cfg!(unix) {
        cases.push((
            vec!["fill", &pdf, &form, "--remote", "-o", &link],
            &[],
            "symlink_output_refused",
        ));
    }
    for (args, env, reason) in cases {
        let out = cli(&args, &temp, env);
        assert_eq!(out.status.code(), Some(5), "{args:?}");
        assert!(out.stdout.is_empty());
        assert_eq!(error(&out), reason, "{args:?}");
        assert!(!String::from_utf8_lossy(&out.stderr).contains("synthetic_secret_canary"));
    }
    for (p, original) in [&pdf, &form, &key_file].into_iter().zip(originals) {
        assert_eq!(std::fs::read(p).expect("read the input again"), original);
    }
    assert!(!temp.0.join("elsewhere.json").exists());
}

/// Temporary output files left in `dir`.
fn temporaries(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir)
        .expect("read the test directory")
        .map(|entry| {
            entry
                .expect("read the directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with(".voltfill-"))
        .collect()
}

fn legacy_schema() -> String {
    path(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/legacy/text-form.json")
            .canonicalize()
            .expect("the legacy example schema exists"),
    )
}

#[test]
fn an_unwritable_output_target_fails_before_inputs_credentials_or_sends() {
    let temp = Temp::new();
    let (pdf, form) = inputs(&temp);
    let schema = legacy_schema();
    let key_file = temp.file("key", b"synthetic_secret_canary\n");
    // No endpoint is configured, so nothing could be sent; with the key file
    // set, a remote run that got past the preflight would stop later with
    // jev_endpoint_required (exit 2) after reading the inputs and the key.
    let env = [("VOLTFILL_JEV_API_KEY_FILE", key_file.as_str())];
    let existing = temp.0.join("existing-dir");
    std::fs::create_dir(&existing).expect("create the existing directory");
    let mut targets = vec![
        (path(&existing), "output_is_directory"),
        (
            path(&temp.0.join("missing").join("result.json")),
            "output_directory_not_found",
        ),
        (String::new(), "invalid_output_path"),
        // These would fail only at the final rename, after sending.
        (path(&temp.0.join("a".repeat(300))), "output_not_accessible"),
    ];
    #[cfg(unix)]
    {
        let looped = temp.0.join("loop");
        std::os::unix::fs::symlink(&looped, &looped).expect("create the symlink loop");
        targets.push((path(&looped.join("result.json")), "output_not_accessible"));
    }
    #[cfg(unix)]
    let unsearchable = {
        use std::os::unix::fs::PermissionsExt;
        let unsearchable = temp.0.join("unsearchable");
        std::fs::create_dir(&unsearchable).expect("create the unsearchable directory");
        std::fs::set_permissions(&unsearchable, std::fs::Permissions::from_mode(0o000))
            .expect("make the directory unsearchable");
        // A privileged user can still search it; skip that case then.
        if std::fs::symlink_metadata(unsearchable.join("probe"))
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        {
            targets.push((
                path(&unsearchable.join("result.json")),
                "output_not_accessible",
            ));
        }
        unsearchable
    };
    #[cfg(unix)]
    let read_only = {
        use std::os::unix::fs::PermissionsExt;
        let read_only = temp.0.join("read-only");
        std::fs::create_dir(&read_only).expect("create the read-only directory");
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o555))
            .expect("make the directory read-only");
        // A privileged user can still write there; skip that case then.
        let probe = read_only.join("probe");
        if std::fs::write(&probe, b"").is_ok() {
            std::fs::remove_file(&probe).ok();
        } else {
            targets.push((path(&read_only.join("result.json")), "output_not_writable"));
        }
        read_only
    };
    // Every command that writes a result shares the same preflight.
    let commands: [Vec<&str>; 4] = [
        vec!["fill", &pdf, &form, "--remote"],
        vec!["fill", &pdf, &form],
        vec!["inspect", &pdf],
        vec!["run", &pdf, &schema, "--remote"],
    ];
    for command in &commands {
        for (target, reason) in &targets {
            let mut args = command.clone();
            args.extend(["-o", target.as_str()]);
            let out = cli(&args, &temp, &env);
            assert_eq!(out.status.code(), Some(5), "{args:?}");
            assert!(out.stdout.is_empty(), "{args:?}");
            assert_eq!(error(&out), *reason, "{args:?}");
            assert!(!String::from_utf8_lossy(&out.stderr).contains("synthetic_secret_canary"));
        }
    }
    assert!(temporaries(&temp.0).is_empty());
    assert!(temporaries(&existing).is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(temporaries(&read_only).is_empty());
        std::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o755)).ok();
        std::fs::set_permissions(&unsearchable, std::fs::Permissions::from_mode(0o755)).ok();
    }
}

#[test]
fn a_run_that_stops_before_sending_leaves_no_temporary_output() {
    let temp = Temp::new();
    let (pdf, form) = inputs(&temp);
    let bad_form = temp.file("bad-form.json", br#"{"fields":[]}"#);
    let key_file = temp.file("key", b"synthetic_secret_canary\n");
    let key_env = [("VOLTFILL_JEV_API_KEY_FILE", key_file.as_str())];
    let result = temp.0.join("result.json");
    type Env<'a> = &'a [(&'a str, &'a str)];
    let cases: [(Vec<&str>, Env, &str); 3] = [
        (vec!["fill", &pdf, &bad_form], &[], "invalid_form"),
        (
            vec!["fill", &pdf, &form, "--remote"],
            &[],
            "api_key_required",
        ),
        (
            vec!["fill", &pdf, &form, "--remote"],
            &key_env,
            "jev_endpoint_required",
        ),
    ];
    for (mut args, env, reason) in cases {
        // A relative target is created next to the working directory.
        args.extend(["-o", "result.json"]);
        let out = cli(&args, &temp, env);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(error(&out), reason, "{args:?}");
        assert!(temporaries(&temp.0).is_empty(), "{args:?}");
        assert!(!result.exists(), "{args:?}");
    }
    let out = cli(&["fill", &pdf, &form, "-o", "result.json"], &temp, &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    let written: Value =
        serde_json::from_slice(&std::fs::read(&result).expect("the result is written"))
            .expect("the result is JSON");
    assert_eq!(written["fields"]["no"]["raw"], "INV-7");
    assert!(temporaries(&temp.0).is_empty());
}

#[test]
fn the_api_key_file_is_never_an_output_target() {
    let temp = Temp::new();
    let (pdf, form) = inputs(&temp);
    let schema = legacy_schema();
    let key_file = temp.file("key", b"synthetic_secret_canary\n");
    let env = [("VOLTFILL_JEV_API_KEY_FILE", key_file.as_str())];
    // Neither offline fill, inspect nor a dry run reads the key, but none of
    // them may replace it either.
    let commands: [Vec<&str>; 3] = [
        vec!["fill", &pdf, &form],
        vec!["inspect", &pdf],
        vec!["run", &pdf, &schema, "--remote", "--dry-run"],
    ];
    for command in &commands {
        let mut refused = command.clone();
        refused.extend(["-o", key_file.as_str()]);
        let out = cli(&refused, &temp, &env);
        assert_eq!(out.status.code(), Some(5), "{refused:?}");
        assert!(out.stdout.is_empty(), "{refused:?}");
        assert_eq!(error(&out), "input_overwrite_refused", "{refused:?}");
        assert!(!String::from_utf8_lossy(&out.stderr).contains("synthetic_secret_canary"));
        // The same command succeeds with another target.
        let mut allowed = command.clone();
        allowed.extend(["-o", "other.json"]);
        let out = cli(&allowed, &temp, &env);
        assert_eq!(out.status.code(), Some(0), "{allowed:?} {:?}", out.stderr);
    }
    assert_eq!(
        std::fs::read(&key_file).expect("read the key file again"),
        b"synthetic_secret_canary\n"
    );
    assert!(temporaries(&temp.0).is_empty());
}

/// Keeps a file immutable while it lives (`chflags uchg` on macOS, `chattr +i`
/// on Linux), where this user may set the flag.
struct Immutable(PathBuf);
impl Immutable {
    fn set(path: &Path) -> Option<Self> {
        let (tool, flag) = if cfg!(target_os = "macos") {
            ("chflags", "uchg")
        } else if cfg!(target_os = "linux") {
            ("chattr", "+i")
        } else {
            return None;
        };
        let status = Command::new(tool)
            .arg(flag)
            .arg(path)
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        status.success().then(|| Self(path.to_owned()))
    }
}
impl Drop for Immutable {
    fn drop(&mut self) {
        let (tool, flag) = if cfg!(target_os = "macos") {
            ("chflags", "nouchg")
        } else {
            ("chattr", "-i")
        };
        let _ = Command::new(tool).arg(flag).arg(&self.0).status();
    }
}

#[test]
fn a_result_that_cannot_replace_an_immutable_target_is_kept_and_its_path_reported() {
    let temp = Temp::new();
    let (pdf, form) = inputs(&temp);
    // The target is a regular file in a writable directory, so the preflight
    // passes, but it cannot be replaced: the final rename fails after the
    // write. Dropped before `temp`, so the directory can be removed.
    let target = temp.0.join("result.json");
    std::fs::write(&target, b"previous").expect("write the existing target");
    let Some(_immutable) = Immutable::set(&target) else {
        // chattr +i needs privileges on Linux.
        eprintln!("skipped: this user cannot make a file immutable here");
        return;
    };
    let target = path(&target);
    for logged in [false, true] {
        let mut args = vec!["fill", pdf.as_str(), form.as_str(), "-o", target.as_str()];
        if logged {
            args.extend(["--log-level", "error"]);
        }
        let out = cli(&args, &temp, &[]);
        assert_eq!(out.status.code(), Some(5), "{logged}");
        // With -o, logs go to stdout; otherwise the error is JSON on stderr.
        let report: Value = if logged {
            assert!(out.stderr.is_empty());
            serde_json::from_slice(&out.stdout).expect("the error log is one JSON line")
        } else {
            assert!(out.stdout.is_empty());
            serde_json::from_slice(&out.stderr).expect("CLI errors are JSON")
        };
        let reason = if logged {
            &report["event"]
        } else {
            &report["error"]
        };
        assert_eq!(reason, "output_rename_failed", "{logged}");
        assert!(report.get("result_on_stderr").is_none());
        let kept = Path::new(report["output_kept"].as_str().expect("the kept path"));
        assert_eq!(kept.parent(), Some(temp.0.as_path()));
        let bytes = std::fs::read(kept).expect("the temporary file remains");
        let written: Value =
            serde_json::from_slice(&bytes).expect("the kept result is complete JSON");
        assert_eq!(written["fields"]["no"]["raw"], "INV-7");
        // The same command without -o prints the same result.
        let stdout = cli(&["fill", &pdf, &form], &temp, &[]).stdout;
        assert_eq!(bytes, stdout);
        std::fs::remove_file(kept).expect("remove the kept file");
    }
    assert_eq!(
        std::fs::read(&target).expect("read the target"),
        b"previous"
    );
    assert!(temporaries(&temp.0).is_empty());
}
