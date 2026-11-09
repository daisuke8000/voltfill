use super::*;
use crate::support;
use std::cell::Cell;
use std::sync::{Arc, Mutex};
use vf_client::transport::{Clock, HttpRequest, HttpResponse, Transport, TransportError};

#[test]
fn logger_limits_levels_to_our_targets() {
    let logger =
        crate::logging::JsonLogger::new("voltfill-cli", log::LevelFilter::Info, io::sink());
    for (target, level, enabled) in [
        ("vf_cli", log::Level::Info, true),
        ("vf_client", log::Level::Error, true),
        ("vf_server", log::Level::Debug, false),
        ("reqwest::connect", log::Level::Error, false),
        ("vf_client_extra", log::Level::Error, false),
    ] {
        let metadata = log::Metadata::builder().target(target).level(level).build();
        assert_eq!(log::Log::enabled(&logger, &metadata), enabled);
    }
}

#[test]
fn would_block_is_retried_until_every_byte_is_written() {
    /// Accepts a few bytes per call and reports WouldBlock in between.
    struct Choppy(Vec<u8>, usize);
    impl Write for Choppy {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.1 += 1;
            if self.1.is_multiple_of(2) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = buf.len().min(3);
            self.0.extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut stream = Choppy(Vec::new(), 0);
    write_patiently(&mut stream, b"{\"answers\":[1,2,3]}").unwrap();
    assert_eq!(stream.0, b"{\"answers\":[1,2,3]}");
}

#[derive(Default, Clone)]
struct FakeClock(u64);
impl Clock for FakeClock {
    fn now_ms(&self) -> u64 {
        self.0
    }
    fn sleep_ms(&mut self, duration: u64) {
        self.0 += duration;
    }
}

/// Content type and body of each send.
type Sent = Arc<Mutex<Vec<(String, Vec<u8>)>>>;
/// Records each send and answers with `reply(number of sends so far, body)`.
struct Wire<F>(Sent, F);
impl<F: FnMut(usize, &[u8]) -> HttpResponse> Transport for Wire<F> {
    fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
        let body = request.body.bytes().to_vec();
        let count = {
            let mut sent = self.0.lock().expect("unpoisoned");
            sent.push((request.body.content_type.clone(), body.clone()));
            sent.len()
        };
        Ok((self.1)(count, &body))
    }
}
fn reply(status: u16, body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        status,
        body,
        body_error: None,
        retry_after_ms: None,
    }
}
fn unavailable() -> HttpResponse {
    reply(503, b"private_unavailable_canary".to_vec())
}
fn sends(sent: &Sent) -> usize {
    sent.lock().expect("unpoisoned").len()
}

/// A temporary directory with `input.pdf` and `form.json`, removed on drop.
struct Inputs {
    dir: PathBuf,
    pdf: String,
    form: String,
}
impl Drop for Inputs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn inputs(name: &str) -> Inputs {
    let dir = std::env::temp_dir().join(format!("voltfill-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("the temp dir is writable");
    let pdf = support::text_pdf(&["Invoice No: INV-7", "Subtotal: 100", "Total: 120"]);
    fs::write(dir.join("input.pdf"), pdf).expect("the PDF is writable");
    fs::write(dir.join("form.json"), br#"{"fields":[{"id":"no","label":"Invoice No"},{"id":"amount","label":"Amount","type":"amount","hints":["Subtotal","Total"]}]}"#)
        .expect("the form is writable");
    Inputs {
        pdf: arg(&dir.join("input.pdf")),
        form: arg(&dir.join("form.json")),
        dir,
    }
}
fn arg(p: &Path) -> String {
    p.to_str().expect("temp paths are UTF-8").to_owned()
}
/// Temporary output files left in `dir`.
fn temporaries(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .expect("the temp dir is readable")
        .map(|entry| entry.expect("the entry is readable").path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".voltfill-"))
        })
        .collect()
}
fn secret() -> Result<Secret, (u8, &'static str)> {
    Ok(Secret::try_new("synthetic_secret_canary".into()).expect("a valid bearer"))
}
/// `voltfill ARGS` with a synthetic key and `transport`.
fn run_args<T: Transport>(args: &[&str], transport: T) -> Result<u8, Failure> {
    let args = args.iter().map(|a| (*a).to_owned());
    run_with(
        args,
        |_, _| Ok(()),
        secret,
        || Ok((transport, FakeClock::default())),
    )
}
/// `fill --remote --max-attempts 2 -o OUTPUT` with every attempt answered 503,
/// running `before_first` on the first send.
fn fill_remote_with(
    inputs: &Inputs,
    output: &Path,
    sent: &Sent,
    before_first: impl FnOnce(),
) -> Result<u8, Failure> {
    let mut before_first = Some(before_first);
    let wire = Wire(sent.clone(), move |_: usize, _: &[u8]| {
        if let Some(f) = before_first.take() {
            f();
        }
        unavailable()
    });
    let output = arg(output);
    run_args(
        &[
            "fill",
            &inputs.pdf,
            &inputs.form,
            "--remote",
            "--max-attempts",
            "2",
            "-o",
            &output,
        ],
        wire,
    )
}
fn json_file(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("the result file exists"))
        .expect("the result is JSON")
}
/// Both scripted 503 bodies are in the result.
fn assert_every_body(result: &[u8]) {
    let result: Value = serde_json::from_slice(result).expect("the result is complete JSON");
    let responses = result["jev_responses"].as_array().expect("responses");
    assert_eq!(responses.len(), 2);
    assert!(
        responses
            .iter()
            .all(|r| r["http_status"] == 503 && r["body"] == "private_unavailable_canary")
    );
    assert_eq!(result["assist"]["amount"]["outcome"], "request_failed");
}

#[test]
fn fill_remote_sends_only_ambiguous_fields_and_keeps_every_body() {
    let inputs = inputs("fill-remote-unit");
    let form = inputs.dir.join("form.json");
    fs::write(&form, br#"{"fields":[{"id":"no","label":"Invoice No"},{"id":"amount","label":"Amount","type":"amount","hints":["Subtotal","Total"]},{"id":"missing","label":"Due"}]}"#)
        .expect("the form is writable");
    let output = inputs.dir.join("result.json");
    let sent = Sent::default();
    // The first attempt gets 503; the retry picks the `Total: 120` line.
    let wire = Wire(sent.clone(), |count: usize, body: &[u8]| {
        if count == 1 {
            return reply(503, b"private_retry_canary".to_vec());
        }
        let request: Value = serde_json::from_slice(body).expect("the fill request is JSON");
        let mut answers = serde_json::Map::new();
        for (id, question) in request["questions"].as_object().expect("questions") {
            let criteria = question["criteria"]
                .as_object()
                .expect("each question is a choice");
            let (selected, _) = criteria
                .iter()
                .find(|(_, text)| text.as_str().is_some_and(|t| t.ends_with("Total: 120")))
                .expect("Total: 120 is an option");
            let probabilities: serde_json::Map<String, Value> = criteria
                .keys()
                .map(|k| (k.clone(), json!(if k == selected { 1.0 } else { 0.0 })))
                .collect();
            answers.insert(id.clone(), json!({"type":"choice","choice":selected,"probabilities":probabilities,"confidence":0.8}));
        }
        let body = json!({"model":request["model"],"answers":answers,"usage":{"input_tokens":50,"output_tokens":2}});
        reply(200, serde_json::to_vec(&body).expect("serializes"))
    });
    let (form, output_arg) = (arg(&form), arg(&output));
    let args = [
        "fill",
        &inputs.pdf,
        &form,
        "--remote",
        "--max-attempts",
        "2",
        "-o",
        &output_arg,
    ];
    assert_eq!(run_args(&args, wire), Ok(0));
    let result = json_file(&output);
    let sent = sent.lock().expect("unpoisoned");
    assert_eq!(sent.len(), 2);
    for (_, body) in sent.iter() {
        let body = std::str::from_utf8(body).expect("requests are UTF-8");
        assert!(!body.contains("synthetic_secret_canary"));
        assert!(!body.contains("INV-7") && !body.contains("Due"));
        assert!(body.contains("Subtotal: 100") && body.contains("Total: 120"));
    }
    assert!(result["evaluation_only"].as_bool().expect("flag"));
    let amount = &result["fields"]["amount"];
    assert_eq!(amount["status"], "filled");
    assert_eq!(amount["method"], "jev_choice");
    assert_eq!(amount["raw"], "120");
    assert_eq!(amount["value"], 120);
    assert_eq!(amount["evidence"][0]["text"], "120");
    assert_eq!(result["fields"]["no"]["method"], "label_match");
    assert_eq!(result["fields"]["missing"]["status"], "not_found");
    assert_eq!(result["assist"]["amount"]["outcome"], "selected");
    assert!(result["assist"].get("no").is_none());
    assert_eq!(result["usage"]["attempts"], 2);
    let responses = result["jev_responses"].as_array().expect("responses");
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["http_status"], 503);
    assert_eq!(responses[0]["body"], "private_retry_canary");
    assert_eq!(responses[0]["field_ids"], json!(["amount"]));
    let body: Value = serde_json::from_str(responses[1]["body"].as_str().expect("kept as text"))
        .expect("original JSON");
    assert_eq!(body["answers"]["q0"]["confidence"], 0.8);
}

#[test]
fn every_command_refuses_the_output_target_before_credentials_or_sends() {
    let inputs = inputs("output-preflight");
    let (dir, pdf, form) = (
        &inputs.dir,
        PathBuf::from(&inputs.pdf),
        PathBuf::from(&inputs.form),
    );
    let config = dir.join("execution.json");
    fs::write(&config, b"{}").expect("the config is writable");
    let existing = dir.join("existing-dir");
    fs::create_dir(&existing).expect("the directory is creatable");
    // Which commands a case applies to: every command, `fill` only (inspect
    // has no form), or `fill --remote --execution-config`.
    let (all, fill, with_config) = (0, 1, 2);
    let mut cases = vec![
        (form.clone(), fill, "input_overwrite_refused"),
        (pdf.clone(), all, "input_overwrite_refused"),
        (config.clone(), with_config, "input_overwrite_refused"),
        (existing.clone(), all, "output_is_directory"),
        (
            dir.join("missing").join("result.json"),
            all,
            "output_directory_not_found",
        ),
        (form.join("result.json"), all, "output_directory_not_found"),
        (PathBuf::new(), all, "invalid_output_path"),
        (
            PathBuf::from(format!("{}/", arg(&dir.join("new")))),
            all,
            "invalid_output_path",
        ),
        (dir.join(".."), all, "invalid_output_path"),
        // No file system accepts a 300-byte name; the rename would fail only
        // after sending.
        (dir.join("a".repeat(300)), all, "output_not_accessible"),
    ];
    #[cfg(unix)]
    let (unsearchable, read_only) = {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let link = dir.join("link.json");
        symlink(dir.join("elsewhere.json"), &link).expect("the symlink is creatable");
        cases.push((link, all, "symlink_output_refused"));
        cases.push((PathBuf::from("/dev/null"), all, "output_not_regular_file"));
        let looped = dir.join("loop");
        symlink(&looped, &looped).expect("the loop is creatable");
        cases.push((looped.join("result.json"), all, "output_not_accessible"));
        let unsearchable = dir.join("unsearchable");
        fs::create_dir(&unsearchable).expect("creatable");
        fs::set_permissions(&unsearchable, fs::Permissions::from_mode(0o000)).expect("settable");
        // A privileged user can still search or write; skip those cases then.
        if fs::symlink_metadata(unsearchable.join("probe"))
            .is_err_and(|e| e.kind() == io::ErrorKind::PermissionDenied)
        {
            cases.push((
                unsearchable.join("result.json"),
                all,
                "output_not_accessible",
            ));
        }
        let read_only = dir.join("read-only");
        fs::create_dir(&read_only).expect("creatable");
        fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555)).expect("settable");
        let probe = read_only.join("probe");
        if fs::write(&probe, b"").is_ok() {
            fs::remove_file(&probe).ok();
        } else {
            cases.push((read_only.join("result.json"), all, "output_not_writable"));
        }
        (unsearchable, read_only)
    };
    let originals: Vec<Vec<u8>> = [&form, &pdf, &config]
        .iter()
        .map(|p| fs::read(p).expect("readable"))
        .collect();
    let (pdf, form, config) = (arg(&pdf), arg(&form), arg(&config));
    for (target, applies, reason) in cases {
        let target = arg(&target);
        let (pdf, form, config) = (pdf.as_str(), form.as_str(), config.as_str());
        let mut commands = vec![vec!["fill", pdf, form, "--remote"]];
        if applies == with_config {
            commands[0].extend(["--execution-config", config]);
        } else {
            commands.push(vec!["fill", pdf, form]);
        }
        if applies == all {
            commands.push(vec!["inspect", pdf]);
        }
        for mut command in commands {
            command.extend(["-o", target.as_str()]);
            let sent = Sent::default();
            let (secret_loads, transports) = (Cell::new(0), Cell::new(0));
            let result = run_with(
                command.iter().map(|a| (*a).to_owned()),
                |_, _| Ok(()),
                || {
                    secret_loads.set(secret_loads.get() + 1);
                    secret()
                },
                || {
                    transports.set(transports.get() + 1);
                    Ok((
                        Wire(sent.clone(), |_: usize, _: &[u8]| unavailable()),
                        FakeClock::default(),
                    ))
                },
            );
            let expected = Failure {
                code: 5,
                reason,
                fallback: None,
            };
            assert_eq!(result, Err(expected), "{command:?}");
            assert_eq!(
                (sends(&sent), secret_loads.get(), transports.get()),
                (0, 0, 0),
                "{command:?}"
            );
        }
    }
    for (path, original) in [&form, &pdf, &config].into_iter().zip(originals) {
        assert_eq!(fs::read(path).expect("readable"), original);
    }
    assert!(!dir.join("elsewhere.json").exists());
    assert!(fs::read_dir(&existing).expect("readable").next().is_none());
    assert!(temporaries(dir).is_empty());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(temporaries(&read_only).is_empty());
        fs::set_permissions(&read_only, fs::Permissions::from_mode(0o755)).ok();
        fs::set_permissions(&unsearchable, fs::Permissions::from_mode(0o755)).ok();
    }
}

#[test]
fn a_run_that_stops_before_sending_removes_its_temporary_output() {
    let inputs = inputs("fill-remote-early-exit");
    let output = inputs.dir.join("result.json");
    let output_arg = arg(&output);
    let args = [
        "fill",
        inputs.pdf.as_str(),
        inputs.form.as_str(),
        "--remote",
        "-o",
        output_arg.as_str(),
    ]
    .map(str::to_owned);
    for reason in ["api_key_required", "jev_endpoint_required"] {
        let result = run_with::<Wire<fn(usize, &[u8]) -> HttpResponse>, FakeClock>(
            args.clone(),
            |_, _| Ok(()),
            || {
                if reason == "api_key_required" {
                    Err((2, reason))
                } else {
                    secret()
                }
            },
            || Err((2, "jev_endpoint_required")),
        );
        assert_eq!(result, Err((2, reason).into()));
        assert!(
            temporaries(&inputs.dir).is_empty() && !output.exists(),
            "{reason}"
        );
    }
    // A limit error writes no result and sends nothing; unlike
    // `fill_assist_all_failed`, the exit code 4 comes before any output.
    let sent = Sent::default();
    let result = run_with(
        [
            "fill",
            inputs.pdf.as_str(),
            inputs.form.as_str(),
            "--remote",
            "--deadline-ms",
            "1",
            "-o",
            output_arg.as_str(),
        ]
        .map(str::to_owned),
        |_, _| Ok(()),
        secret,
        || {
            // Inputs are read; the deadline passes before layout starts.
            std::thread::sleep(Duration::from_millis(20));
            Ok((
                Wire(sent.clone(), |_: usize, _: &[u8]| unavailable()),
                FakeClock::default(),
            ))
        },
    );
    assert_eq!(result, Err((4, "fill_deadline_exceeded").into()));
    assert_eq!(sends(&sent), 0);
    assert!(temporaries(&inputs.dir).is_empty() && !output.exists());
}

#[test]
fn fill_remote_writes_every_body_then_exits_4_when_every_sent_field_fails() {
    let inputs = inputs("fill-remote-all-failed");
    let output = inputs.dir.join("result.json");
    let sent = Sent::default();
    assert_eq!(
        fill_remote_with(&inputs, &output, &sent, || {}),
        Err((4, "fill_assist_all_failed").into())
    );
    assert_eq!(sends(&sent), 2);
    assert!(temporaries(&inputs.dir).is_empty());
    let written = json_file(&output);
    assert_every_body(&fs::read(&output).expect("written"));
    assert_eq!(written["fields"]["amount"]["status"], "ambiguous");
    assert_eq!(written["fields"]["no"]["status"], "filled");
}

#[cfg(unix)]
#[test]
fn a_symlink_planted_at_the_target_mid_run_is_replaced_not_followed() {
    let inputs = inputs("fill-remote-planted-link");
    let output = inputs.dir.join("result.json");
    let victim = inputs.dir.join("victim.txt");
    fs::write(&victim, b"unchanged").expect("writable");
    let sent = Sent::default();
    let result = fill_remote_with(&inputs, &output, &sent, || {
        std::os::unix::fs::symlink(&victim, &output).expect("the symlink can be planted");
    });
    // Both attempts failed, so the run exits 4 after writing the result.
    assert_eq!(result, Err((4, "fill_assist_all_failed").into()));
    assert_eq!(sends(&sent), 2);
    assert!(
        fs::symlink_metadata(&output)
            .expect("the target exists")
            .is_file()
    );
    assert_eq!(fs::read(&victim).expect("readable"), b"unchanged");
    assert_eq!(
        json_file(&output)["jev_responses"].as_array().map(Vec::len),
        Some(2)
    );
    assert!(temporaries(&inputs.dir).is_empty());
}

/// The kept file reported by `failure`, checked to hold the whole result.
fn kept_file(failure: &Failure) -> (PathBuf, Vec<u8>) {
    let Some(Fallback::File(kept)) = &failure.fallback else {
        panic!("a kept file is reported: {:?}", failure.fallback.is_some());
    };
    let bytes = fs::read(kept).expect("the reported file exists");
    assert_every_body(&bytes);
    assert_eq!(
        fs::metadata(kept).expect("exists").len(),
        bytes.len() as u64
    );
    (kept.clone(), bytes)
}
fn stderr_bytes(failure: &Failure) -> &[u8] {
    let Some(Fallback::Stderr(bytes)) = &failure.fallback else {
        panic!("no file can hold the result, so it goes to stderr");
    };
    assert_every_body(bytes);
    bytes
}
/// Runs `fill --remote` with `faults` on the next file writes and `current`
/// in place of the working directory, resetting both after.
fn fill_remote_seamed(
    faults: &[seam::WriteFault],
    current: Option<PathBuf>,
    inputs: &Inputs,
    output: &Path,
    before_first: impl FnOnce(),
) -> Failure {
    seam::set(faults, current);
    let sent = Sent::default();
    let result = fill_remote_with(inputs, output, &sent, before_first);
    seam::set(&[], None);
    assert_eq!(sends(&sent), 2);
    let failure = result.expect_err("the result cannot replace the target");
    assert_eq!(failure.code, 5);
    assert!(!output.exists());
    failure
}

#[test]
fn a_result_that_cannot_replace_the_target_is_kept_in_a_verified_file() {
    let inputs = inputs("fill-remote-rename-failed");
    let dir = &inputs.dir;
    // The target passes the preflight and becomes a directory mid-run: the
    // temporary file is kept where it is.
    let output = dir.join("result.json");
    let sent = Sent::default();
    let result = fill_remote_with(&inputs, &output, &sent, || {
        fs::create_dir(&output).expect("the target can become a directory");
    });
    assert_eq!(sends(&sent), 2);
    let failure = result.expect_err("the rename over a directory fails");
    assert_eq!((failure.code, failure.reason), (5, "output_rename_failed"));
    let (kept, _) = kept_file(&failure);
    assert_eq!(kept.parent(), output.parent());
    assert_eq!(temporaries(dir), std::slice::from_ref(&kept));
    fs::remove_file(kept).expect("removable");
    // The temporary file is deleted mid-run: the result is rewritten.
    let output = dir.join("result-2.json");
    let failure = fill_remote_seamed(&[], None, &inputs, &output, || {
        temporaries(dir)
            .into_iter()
            .for_each(|t| fs::remove_file(t).expect("deletable"));
    });
    assert_eq!(failure.reason, "output_rename_failed");
    let (kept, _) = kept_file(&failure);
    assert_eq!(kept.parent(), Some(dir.as_path()));
    assert_eq!(temporaries(dir), [kept]);
}

#[test]
fn a_removed_output_directory_falls_back_to_the_working_directory_then_stderr() {
    let inputs = inputs("fill-remote-directory-removed");
    let current = inputs.dir.join("current");
    fs::create_dir(&current).expect("creatable");
    for working in [current.clone(), inputs.dir.join("current-missing")] {
        let out = inputs.dir.join("out");
        fs::create_dir(&out).expect("creatable");
        let failure = fill_remote_seamed(
            &[],
            Some(working.clone()),
            &inputs,
            &out.join("result.json"),
            || {
                fs::remove_dir_all(&out).expect("the output directory can be removed");
            },
        );
        assert_eq!(failure.reason, "output_rename_failed", "{working:?}");
        if working == current {
            let (kept, _) = kept_file(&failure);
            assert_eq!(kept.parent(), Some(current.as_path()));
            fs::remove_file(kept).expect("removable");
        } else {
            stderr_bytes(&failure);
        }
    }
}

#[test]
fn a_failed_write_is_rewritten_to_a_verified_file_or_handed_to_stderr() {
    use seam::WriteFault::{Fail, Short};
    let inputs = inputs("fill-remote-write-failed");
    let (dir, current) = (&inputs.dir, inputs.dir.join("current"));
    fs::create_dir(&current).expect("creatable");
    let output = dir.join("result.json");
    // A short write that reports success is caught only by verification.
    for (faults, kept_in) in [
        (&[Fail][..], Some(dir)),
        (&[Fail, Short], Some(&current)),
        (&[Fail, Fail, Fail], None),
        (&[Fail, Short, Short], None),
    ] {
        let failure = fill_remote_seamed(faults, Some(current.clone()), &inputs, &output, || {});
        assert_eq!(failure.reason, "output_write_failed");
        match kept_in {
            Some(directory) => {
                assert_eq!(kept_file(&failure).0.parent(), Some(directory.as_path()))
            }
            None => _ = stderr_bytes(&failure),
        }
        // Incomplete files are left in place but never reported.
        for temporary in temporaries(dir).into_iter().chain(temporaries(&current)) {
            fs::remove_file(temporary).expect("removable");
        }
    }
}

#[test]
fn a_failure_names_a_kept_file_or_writes_the_unsaved_result_to_stderr() {
    let failure = |fallback| Failure {
        code: 5,
        reason: "output_rename_failed",
        fallback: Some(fallback),
    };
    let mut stderr = Vec::new();
    report_failure(
        &failure(Fallback::File(PathBuf::from("out/.voltfill-1-0.tmp"))),
        false,
        &mut stderr,
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&stderr).expect("one JSON error"),
        json!({"error":"output_rename_failed","output_kept":"out/.voltfill-1-0.tmp"})
    );
    // The result's bytes are written unchanged inside one JSON object,
    // whether or not the error is also logged.
    let result = b"{\n  \"z\": 1,\n  \"jev_responses\": [{\"body\": \"private_canary\"}]\n}\n";
    for logging in [false, true] {
        let mut stderr = Vec::new();
        report_failure(
            &failure(Fallback::Stderr(result.to_vec())),
            logging,
            &mut stderr,
        );
        assert!(
            stderr
                .windows(result.len())
                .any(|window| window == result.as_slice())
        );
        let report: Value = serde_json::from_slice(&stderr).expect("one JSON object");
        assert_eq!(report["error"], "output_rename_failed");
        assert_eq!(
            report["result"],
            serde_json::from_slice::<Value>(result).expect("JSON")
        );
        assert_eq!(report.as_object().expect("an object").len(), 2);
    }
    // A closed stderr does not panic.
    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    report_failure(
        &failure(Fallback::Stderr(result.to_vec())),
        false,
        &mut Closed,
    );
    report_failure(&failure(Fallback::File(PathBuf::new())), false, &mut Closed);
}

/// Answers every image question: present, at a box near the top left.
#[cfg(feature = "hayro")]
fn image_reply(body: &[u8]) -> HttpResponse {
    let text = String::from_utf8_lossy(body).into_owned();
    let start = text.find("\r\n\r\n").expect("a part header") + 4;
    let end = start
        + text[start..]
            .find("\r\n--")
            .expect("the request part ends at a boundary");
    let request: Value = serde_json::from_str(&text[start..end]).expect("the request part is JSON");
    let mut answers = serde_json::Map::new();
    for (id, question) in request["questions"].as_object().expect("questions") {
        let answer = if question["type"] == "noul" {
            json!({"type":"noul","noul":0.9})
        } else {
            json!({"type":"box","box":{"left":0.1,"top":0.1,"right":0.3,"bottom":0.15},"confidence":0.7})
        };
        answers.insert(id.clone(), answer);
    }
    let body = json!({"model":request["model"],"answers":answers,"usage":{"input_tokens":16100,"output_tokens":4}});
    reply(200, serde_json::to_vec(&body).expect("serializes"))
}
/// `fill PDF FORM --remote --images --renderer hayro EXTRA -o OUTPUT`.
#[cfg(feature = "hayro")]
fn fill_images<T: Transport>(
    inputs: &Inputs,
    pdf: &[u8],
    form: &[u8],
    extra: &[&str],
    wire: T,
) -> (Result<u8, Failure>, Value) {
    let (pdf_path, form_path, output) = (
        inputs.dir.join("images.pdf"),
        inputs.dir.join("images.json"),
        inputs.dir.join("result.json"),
    );
    fs::write(&pdf_path, pdf).expect("writable");
    fs::write(&form_path, form).expect("writable");
    let (pdf_path, form_path, output_arg) = (arg(&pdf_path), arg(&form_path), arg(&output));
    let mut args = vec![
        "fill",
        &pdf_path,
        &form_path,
        "--remote",
        "--images",
        "--renderer",
        "hayro",
        "-o",
        &output_arg,
    ];
    args.extend(extra);
    (run_args(&args, wire), json_file(&output))
}

#[cfg(feature = "hayro")]
#[test]
fn fill_remote_images_locates_a_scanned_page_and_keeps_every_body() {
    let inputs = inputs("fill-images-unit");
    let sent = Sent::default();
    let wire = Wire(sent.clone(), |count: usize, body: &[u8]| match count {
        1 => reply(503, b"private_image_retry_canary".to_vec()),
        _ => image_reply(body),
    });
    let form = br#"{"fields":[{"id":"no","label":"Invoice No"}]}"#;
    let (code, result) = fill_images(&inputs, &support::scanned_pdf(), form, &[], wire);
    assert_eq!(code, Ok(0));
    let sent = sent.lock().expect("unpoisoned");
    assert_eq!(sent.len(), 2);
    for (content_type, body) in sent.iter() {
        assert!(content_type.starts_with("multipart/form-data"));
        // The rendered page itself is attached; the key never is.
        assert!(body.windows(8).any(|w| w == b"\x89PNG\r\n\x1a\n"));
        assert!(!body.windows(23).any(|w| w == b"synthetic_secret_canary"));
    }
    assert!(result["evaluation_only"].as_bool().expect("flag"));
    let field = &result["fields"]["no"];
    assert_eq!(field["status"], "no_text");
    assert!(field.get("raw").is_none() && field.get("method").is_none());
    assert_eq!(field["evidence"], json!([]));
    assert_eq!(field["located"]["source"], "model");
    assert_eq!(field["located"]["page"], 1);
    assert_eq!(field["located"]["normalized"]["left"], 0.1);
    let record = &result["image_assist"]["no"];
    assert_eq!(record["outcome"], "located");
    assert_eq!(record["page"], 1);
    assert_eq!(record["pages"][0]["presence"], 0.9);
    assert_eq!(result["assist"], json!({}));
    assert_eq!(result["usage"]["attempts"], 2);
    let responses = result["jev_responses"].as_array().expect("responses");
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["http_status"], 503);
    assert_eq!(responses[0]["body"], "private_image_retry_canary");
    for response in responses {
        assert_eq!(response["page"], 1);
        assert_eq!(response["field_ids"], json!(["no"]));
        assert_eq!(response["question_ids"], json!(["i0_present", "i0_box"]));
    }
    let body: Value = serde_json::from_str(responses[1]["body"].as_str().expect("kept as text"))
        .expect("original JSON");
    assert_eq!(body["answers"]["i0_box"]["confidence"], 0.7);
}

#[cfg(feature = "hayro")]
#[test]
fn the_fill_deadline_bounds_the_image_steps_selections() {
    let inputs = inputs("fill-images-deadline");
    // `Reference` has no candidate offline, so only the image step asks; the
    // answer arrives after the deadline.
    let wire = Wire(Sent::default(), |_: usize, body: &[u8]| {
        std::thread::sleep(Duration::from_millis(3500));
        image_reply(body)
    });
    let form = br#"{"fields":[{"id":"ref","label":"Reference"}]}"#;
    let (code, written) = fill_images(
        &inputs,
        &support::renderable_text_pdf(),
        form,
        &["--deadline-ms", "3000"],
        wire,
    );
    // The page is not judged, and the full result is still written.
    assert_eq!(code, Err((4, "fill_assist_all_failed").into()));
    assert_eq!(
        written["image_assist"]["ref"]["outcome"],
        "deadline_exceeded"
    );
    assert_eq!(written["fields"]["ref"]["status"], "not_found");
    assert_eq!(written["jev_responses"].as_array().map(Vec::len), Some(1));
}
