//! The built `voltfill` binary. No test configures both a usable endpoint and
//! a key, so no request can be sent.
mod support;

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("voltfill-cli-{}-{n}", std::process::id()));
        std::fs::create_dir(&p).expect("create the isolated test directory");
        Self(p)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> String {
        let p = self.0.join(name);
        std::fs::write(&p, bytes).expect("write the synthetic input");
        p.to_str().expect("temp paths are UTF-8").to_owned()
    }
    /// `input.pdf` and `form.json`: `no` fills offline, `amount` stays ambiguous.
    fn inputs(&self) -> (String, String) {
        let pdf = support::text_pdf(&["Invoice No: INV-7", "Subtotal: 100", "Total: 120"]);
        let form = br#"{"fields":[{"id":"no","label":"Invoice No"},{"id":"amount","label":"Amount","type":"amount","hints":["Subtotal","Total"]}]}"#;
        (self.file("input.pdf", &pdf), self.file("form.json", form))
    }
    /// Temporary output files left here.
    fn temporaries(&self) -> Vec<String> {
        std::fs::read_dir(&self.0)
            .expect("read the test directory")
            .map(|entry| {
                entry
                    .expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|name| name.starts_with(".voltfill-"))
            .collect()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
fn error(out: &Output) -> String {
    let value: Value = serde_json::from_slice(&out.stderr).expect("CLI errors are JSON");
    value["error"].as_str().expect("error code").to_owned()
}

#[test]
fn help_lists_the_fill_options() {
    let out = cli(&["--help"], &Temp::new(), &[]);
    assert_eq!(out.status.code(), Some(0));
    let help = String::from_utf8(out.stdout).expect("CLI help is UTF-8");
    for phrase in [
        "--max-attempts",
        "--execution-config",
        "default: 3",
        "--log-level",
        "default: off",
        "stderr",
        "stdout",
    ] {
        assert!(help.contains(phrase), "{phrase}");
    }
}

/// Each case is `voltfill ARGS` (split on spaces; `PDF`, `FORM`, `BAD` and
/// `KEYFILE` name files in the test directory) exiting 2 with `{"error":REASON}`
/// on stderr, before any send and without disclosing the key.
#[test]
fn arguments_and_settings_are_checked_before_inputs_credentials_or_sends() {
    let temp = Temp::new();
    let (pdf, form) = temp.inputs();
    let bad = temp.file("bad-form.json", br#"{"fields":[]}"#);
    let key_file = temp.file("key", b"synthetic_secret_canary\n");
    let key = ("VOLTFILL_JEV_API_KEY", "synthetic_secret_canary");
    let pick = |enabled: bool, on, off| if enabled { on } else { off };
    // With rendering configured, an image run stops at the missing key.
    let hayro = pick(
        cfg!(feature = "hayro"),
        "api_key_required",
        "hayro_feature_disabled",
    );
    let scale = pick(
        cfg!(feature = "hayro"),
        "invalid_render_scale",
        "hayro_feature_disabled",
    );
    let pdfium = pick(
        cfg!(feature = "pdfium"),
        "rendering_required",
        "pdfium_feature_disabled",
    );
    let images = "fill PDF FORM --remote --images";
    type Env = Vec<(&'static str, &'static str)>;
    let mut cases: Vec<(String, Env, &str)> = Vec::new();
    let mut case = |args: &str, env: &[(&'static str, &'static str)], reason: &'static str| {
        cases.push((args.into(), env.to_vec(), reason))
    };
    // The form is read and validated before the PDF and any credential.
    case("fill missing.pdf BAD", &[], "invalid_form");
    case(
        "fill missing.pdf missing-form.json",
        &[],
        "input_read_failed",
    );
    case("fill missing.pdf BAD --remote", &[], "invalid_form");
    // Options of removed commands, remote options without --remote, and
    // options of other commands.
    for args in [
        "fill missing.pdf BAD --remote --dry-run",
        "fill missing.pdf BAD --remote --mock BAD",
        "fill missing.pdf BAD --remote --max-concurrency 2",
        "fill missing.pdf BAD --remote --config BAD",
        "fill missing.pdf BAD --model jev-1.14.0",
        "fill missing.pdf BAD --max-attempts 1",
        "fill missing.pdf BAD --execution-config BAD",
        "inspect missing.pdf --remote",
        "inspect missing.pdf --deadline-ms 100",
        "validate BAD --remote",
        "extract missing.pdf BAD --remote",
        "fill PDF FORM --remote --max-attempts 2 --max-attempts 3",
        "fill PDF FORM --remote --log-level info --log-level debug",
        // Image options need --remote, and rendering options need --images.
        "fill PDF FORM --images",
        "fill PDF FORM --remote --renderer hayro",
        "fill PDF FORM --remote --pixels-per-point 1.5",
        "fill PDF FORM --remote --pdfium-lib /nonexistent/libpdfium",
    ] {
        case(args, &[], "invalid_arguments");
    }
    for invalid in ["0", "4", "-1", "01", "unbounded"] {
        case(
            &format!("fill missing.pdf missing-form.json --remote --max-attempts {invalid}"),
            &[],
            "invalid_arguments",
        );
    }
    for invalid in ["trace", "DEBUG"] {
        case(
            &format!("fill missing.pdf missing-form.json --remote --log-level {invalid}"),
            &[],
            "invalid_arguments",
        );
    }
    // The deadline is 1 to 600000 ms, with or without --remote or --images; a
    // valid one reaches the input read.
    for mode in ["", " --remote", " --remote --images"] {
        for value in ["0", "600001", "-1", "1.5", "soon"] {
            case(
                &format!("fill missing.pdf missing-form.json{mode} --deadline-ms {value}"),
                &[],
                "invalid_deadline",
            );
        }
    }
    for mode in ["", " --remote"] {
        for value in ["1", "600000"] {
            case(
                &format!("fill missing.pdf missing-form.json{mode} --deadline-ms {value}"),
                &[],
                "input_read_failed",
            );
        }
    }
    // Remote settings.
    case("fill PDF FORM --remote", &[], "api_key_required");
    case("fill PDF FORM --remote", &[key], "jev_endpoint_required");
    case(
        "fill PDF FORM --remote",
        &[key, ("VOLTFILL_JEV_ENDPOINT", "http://127.0.0.1:9/")],
        "invalid_jev_endpoint",
    );
    case(
        "fill PDF FORM --remote",
        &[key, ("VOLTFILL_JEV_API_KEY_FILE", "KEYFILE")],
        "credential_sources_conflict",
    );
    case(
        "fill PDF FORM --remote --model gpt",
        &[key],
        "invalid_model",
    );
    case(
        "fill PDF FORM --remote",
        &[key, ("VOLTFILL_MODEL", "gpt")],
        "invalid_model",
    );
    case(
        "fill PDF FORM --remote --execution-config FORM",
        &[key],
        "invalid_execution_config",
    );
    case(
        "fill PDF FORM --remote --max-attempts 4",
        &[key],
        "invalid_arguments",
    );
    // No renderer is chosen automatically.
    case(images, &[], "rendering_required");
    case(
        &format!("{images} --renderer skia"),
        &[],
        "invalid_renderer",
    );
    case(
        &format!("{images} --renderer hayro --pdfium-lib /nonexistent/libpdfium"),
        &[],
        "renderer_library_conflict",
    );
    case(
        &format!("{images} --renderer hayro --model jev-latest"),
        &[],
        "model_does_not_support_images",
    );
    case(&format!("{images} --renderer hayro"), &[], hayro);
    case(images, &[("VOLTFILL_RENDERER", "hayro")], hayro);
    case(
        &format!("{images} --renderer hayro --pixels-per-point 0"),
        &[],
        scale,
    );
    case(&format!("{images} --renderer pdfium"), &[], pdfium);
    for (args, env, reason) in cases {
        let path = |token: &str| match token {
            "PDF" => pdf.clone(),
            "FORM" => form.clone(),
            "BAD" => bad.clone(),
            "KEYFILE" => key_file.clone(),
            _ => token.to_owned(),
        };
        let argv: Vec<String> = args.split(' ').map(path).collect();
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        let env: Vec<(&str, String)> = env
            .iter()
            .map(|(name, value)| (*name, path(value)))
            .collect();
        let env: Vec<(&str, &str)> = env
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let out = cli(&argv, &temp, &env);
        assert_eq!(out.status.code(), Some(2), "{args}");
        assert!(out.stdout.is_empty(), "{args}");
        let stderr: Value = serde_json::from_slice(&out.stderr).expect("CLI errors are JSON");
        assert_eq!(stderr, json!({"error": reason}), "{args}");
    }
}

#[test]
fn offline_fill_reads_no_credentials_and_keeps_its_shape() {
    let temp = Temp::new();
    let (pdf, form) = temp.inputs();
    let missing = temp.0.join("missing-credential");
    let missing = missing.to_str().expect("UTF-8");
    let clean = cli(&["fill", &pdf, &form], &temp, &[]);
    assert_eq!(clean.status.code(), Some(0));
    // Each of these would fail the run if fill read it without --remote.
    let hostile = cli(
        &["fill", &pdf, &form],
        &temp,
        &[
            ("VOLTFILL_JEV_API_KEY", "synthetic_secret_canary"),
            ("VOLTFILL_JEV_API_KEY_FILE", missing),
            ("VOLTFILL_JEV_ENDPOINT", "http://127.0.0.1:9/"),
            ("VOLTFILL_MODEL", "not-a-model"),
            ("VOLTFILL_EXECUTION_CONFIG", missing),
            ("VOLTFILL_RENDERER", "not-a-renderer"),
            ("VOLTFILL_PIXELS_PER_POINT", "not-a-scale"),
            ("VOLTFILL_PDFIUM_LIB_PATH", missing),
        ],
    );
    assert_eq!(hostile.status.code(), Some(0), "{:?}", hostile.stderr);
    assert!(hostile.stderr.is_empty());
    assert_eq!(hostile.stdout, clean.stdout);
    let result: Value = serde_json::from_slice(&clean.stdout).expect("the result is JSON");
    let keys: Vec<&String> = result.as_object().expect("an object").keys().collect();
    assert_eq!(keys, ["document_id", "evaluation_only", "fields"]);
    assert_eq!(result["fields"]["amount"]["status"], "ambiguous");
    assert_eq!(result["fields"]["no"]["raw"], "INV-7");
}

/// Page count and page image size are known once the renderer has read the
/// page tree, so both refusals come before credentials (none are set here:
/// reading them would end in `api_key_required`) and exit 2.
#[cfg(feature = "hayro")]
#[test]
fn image_fill_refuses_page_count_and_image_size_before_credentials() {
    let temp = Temp::new();
    let form = temp.file(
        "form.json",
        br#"{"fields":[{"id":"ref","label":"Reference"}]}"#,
    );
    let order = "BT /F1 12 Tf 72 700 Td (Order AB12) Tj ET\n";
    let (letter, a3) = ("[0 0 612 792]", "[0 0 842 1191]");
    for (pages, media, scale, reason) in [
        // 11 Letter pages; the image step takes at most 10.
        (11, letter, "1.5", "image_page_limit_exceeded"),
        // One A3 page at 1.5 pixels per point is 1263 x 1787 pixels.
        (1, a3, "1.5", "image_size_out_of_range"),
        // One Letter page at 2 pixels per point is 1224 x 1584 pixels.
        (1, letter, "2", "image_size_out_of_range"),
        // A Letter page at 1.5 is accepted; the run stops at the missing key.
        (1, letter, "1.5", "api_key_required"),
    ] {
        let pdf = temp.file("input.pdf", &support::pages_pdf(pages, media, order));
        let args = [
            "fill",
            pdf.as_str(),
            form.as_str(),
            "--remote",
            "--images",
            "--renderer",
            "hayro",
            "--pixels-per-point",
            scale,
        ];
        let out = cli(&args, &temp, &[]);
        assert_eq!(out.status.code(), Some(2), "{reason}");
        assert!(out.stdout.is_empty());
        assert_eq!(error(&out), reason);
    }
}

#[test]
fn a_run_that_stops_before_sending_leaves_no_temporary_output() {
    let temp = Temp::new();
    let (pdf, form) = temp.inputs();
    let bad_form = temp.file("bad-form.json", br#"{"fields":[]}"#);
    let key_file = temp.file("key", b"synthetic_secret_canary\n");
    let key_env = [("VOLTFILL_JEV_API_KEY_FILE", key_file.as_str())];
    let result = temp.0.join("result.json");
    let (pdf, form, bad_form) = (pdf.as_str(), form.as_str(), bad_form.as_str());
    for (args, env, reason) in [
        (["fill", pdf, bad_form, ""], &[][..], "invalid_form"),
        (["fill", pdf, form, "--remote"], &[], "api_key_required"),
        (
            ["fill", pdf, form, "--remote"],
            &key_env,
            "jev_endpoint_required",
        ),
    ] {
        // A relative target is created next to the working directory.
        let mut args: Vec<&str> = args.into_iter().filter(|a| !a.is_empty()).collect();
        args.extend(["-o", "result.json"]);
        let out = cli(&args, &temp, env);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(error(&out), reason, "{args:?}");
        assert!(
            temp.temporaries().is_empty() && !result.exists(),
            "{args:?}"
        );
    }
    let out = cli(&["fill", pdf, form, "-o", "result.json"], &temp, &[]);
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    let written: Value =
        serde_json::from_slice(&std::fs::read(&result).expect("written")).expect("JSON");
    assert_eq!(written["fields"]["no"]["raw"], "INV-7");
    assert!(temp.temporaries().is_empty());
}

#[test]
fn the_api_key_file_is_never_an_output_target() {
    let temp = Temp::new();
    let (pdf, form) = temp.inputs();
    let key_file = temp.file("key", b"synthetic_secret_canary\n");
    let env = [("VOLTFILL_JEV_API_KEY_FILE", key_file.as_str())];
    // Only the key file's path is compared; offline fill and inspect never read
    // it, and --remote refuses the target before reading it.
    let (pdf, form) = (pdf.as_str(), form.as_str());
    for (command, readable) in [
        (vec!["fill", pdf, form], true),
        (vec!["inspect", pdf], true),
        (vec!["fill", pdf, form, "--remote"], false),
    ] {
        let mut refused = command.clone();
        refused.extend(["-o", key_file.as_str()]);
        let out = cli(&refused, &temp, &env);
        assert_eq!(out.status.code(), Some(5), "{refused:?}");
        assert!(out.stdout.is_empty(), "{refused:?}");
        assert_eq!(error(&out), "input_overwrite_refused", "{refused:?}");
        assert!(!String::from_utf8_lossy(&out.stderr).contains("synthetic_secret_canary"));
        if readable {
            // The same command succeeds with another target.
            let mut allowed = command.clone();
            allowed.extend(["-o", "other.json"]);
            let out = cli(&allowed, &temp, &env);
            assert_eq!(out.status.code(), Some(0), "{allowed:?} {:?}", out.stderr);
        }
    }
    assert_eq!(
        std::fs::read(&key_file).expect("readable"),
        b"synthetic_secret_canary\n"
    );
    assert!(temp.temporaries().is_empty());
}

/// Keeps a file immutable while it lives (`chflags uchg` on macOS, `chattr +i`
/// on Linux), where this user may set the flag.
struct Immutable(PathBuf);
fn chflags(path: &Path, on: bool) -> bool {
    let (tool, flag) = match (cfg!(target_os = "macos"), on) {
        (true, true) => ("chflags", "uchg"),
        (true, false) => ("chflags", "nouchg"),
        (false, true) => ("chattr", "+i"),
        (false, false) => ("chattr", "-i"),
    };
    let status = Command::new(tool)
        .arg(flag)
        .arg(path)
        .stderr(std::process::Stdio::null())
        .status();
    status.is_ok_and(|s| s.success())
}
impl Drop for Immutable {
    fn drop(&mut self) {
        chflags(&self.0, false);
    }
}

#[test]
fn a_result_that_cannot_replace_an_immutable_target_is_kept_and_its_path_reported() {
    let temp = Temp::new();
    let (pdf, form) = temp.inputs();
    // The target is a regular file in a writable directory, so the preflight
    // passes, but it cannot be replaced: the final rename fails after the
    // write. Dropped before `temp`, so the directory can be removed.
    let target = temp.file("result.json", b"previous");
    if !(cfg!(any(target_os = "macos", target_os = "linux")) && chflags(Path::new(&target), true)) {
        // chattr +i needs privileges on Linux.
        eprintln!("skipped: this user cannot make a file immutable here");
        return;
    }
    let _immutable = Immutable(PathBuf::from(&target));
    for logged in [false, true] {
        let mut args = vec!["fill", pdf.as_str(), form.as_str(), "-o", target.as_str()];
        if logged {
            args.extend(["--log-level", "error"]);
        }
        let out = cli(&args, &temp, &[]);
        assert_eq!(out.status.code(), Some(5), "{logged}");
        // With -o, logs go to stdout; otherwise the error is JSON on stderr.
        let (report, quiet) = if logged {
            (&out.stdout, &out.stderr)
        } else {
            (&out.stderr, &out.stdout)
        };
        assert!(quiet.is_empty());
        let report: Value = serde_json::from_slice(report).expect("one JSON line");
        assert_eq!(
            report[if logged { "event" } else { "error" }],
            "output_rename_failed",
            "{logged}"
        );
        assert!(report.get("result_on_stderr").is_none());
        let kept = Path::new(report["output_kept"].as_str().expect("the kept path"));
        assert_eq!(kept.parent(), Some(temp.0.as_path()));
        let bytes = std::fs::read(kept).expect("the temporary file remains");
        let written: Value =
            serde_json::from_slice(&bytes).expect("the kept result is complete JSON");
        assert_eq!(written["fields"]["no"]["raw"], "INV-7");
        // The same command without -o prints the same result.
        assert_eq!(bytes, cli(&["fill", &pdf, &form], &temp, &[]).stdout);
        std::fs::remove_file(kept).expect("remove the kept file");
    }
    assert_eq!(std::fs::read(&target).expect("readable"), b"previous");
    assert!(temp.temporaries().is_empty());
}
