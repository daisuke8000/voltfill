//! Command parsing, local inspection, and explicitly enabled extraction.
use crate::config;
use crate::settings;
#[cfg(test)]
use serde_json::Value;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vf_core::assist;
use vf_legacy::engine::{self, EvaluationProfile, ExtractOptions};
use vf_legacy::evaluate::{Evaluator, FixtureEvaluator, RemoteEvaluator};
use vf_legacy::jev::transport::{ExecutionConfig, Secret};
use vf_legacy::model::{JevResponse, Strategy};
use vf_legacy::{pdf, schema};

const HELP: &str = "VOLTFILL — PDF extraction\n\nvoltfill run --config voltfill.yaml (or voltfill.toml / voltfill.json)\nvoltfill serve --config voltfill.yaml\nvoltfill serve --help\nvoltfill run INPUT.pdf SCHEMA.json --remote\nvoltfill extract INPUT.pdf SCHEMA.json --remote\nvoltfill extract INPUT.pdf SCHEMA.json --mock ANSWERS.json\nvoltfill inspect INPUT.pdf\nvoltfill fill INPUT.pdf FORM.json (offline: match form labels to PDF text)\nvoltfill fill INPUT.pdf FORM.json --remote (Jev chooses among ambiguous fields' source candidates)\n--deadline-ms N bounds fill's layout and matching from the start of fill (default: 30000,\n1 to 600000); PDF inspection is bounded by its own limits and is not interrupted.\nPast a fill limit, fill exits 4 (layout_limit_exceeded, fill_budget_exceeded or\nfill_deadline_exceeded) before writing a result or sending a request.\nvoltfill validate SCHEMA.json\n\n--strategy text|vision|hybrid (default: text)\n--model ID (default: jev-1.14.0); images require a pinned version >= 1.14.0.\n--renderer pdfium|hayro selects an enabled renderer; no automatic fallback.\n--pdfium-lib PATH or VOLTFILL_PDFIUM_LIB_PATH selects PDFium when --renderer is omitted\n--pixels-per-point NUMBER sets render scale (default: 1.5).\n--config FILE (or -c FILE, default VOLTFILL_CONFIG) loads YAML/TOML launch settings or legacy JSON execution limits.\nPrecedence: CLI > selected file > environment > defaults.\n--resource-config FILE (or VOLTFILL_RESOURCE_CONFIG) lowers PDF/render ceilings.\nVOLTFILL_EXECUTION_CONFIG selects execution JSON when no execution section/file is supplied.\nVOLTFILL_MODEL, VOLTFILL_STRATEGY, VOLTFILL_RENDERER, VOLTFILL_PIXELS_PER_POINT\nand VOLTFILL_LOG_LEVEL provide startup defaults.\n--max-concurrency N (default: 2, at least 1); rate limits may reduce concurrency.\n--max-attempts 1|2|3 includes the first attempt (default: 3).\n--log-level off|error|warn|info|debug (default: off) enables JSON Lines logs.\nLogs use stderr with JSON on stdout; with --output FILE (or -o FILE),\nlogs use stdout and results go to the file. Debug logs cover VOLTFILL only.\n--dry-run inspects the input without sending requests.\n--evaluation-profile synthetic enables confidence filtering for local synthetic examples.\nBy default, resolved values are returned without a confidence cutoff.\n\nRemote mode requires VOLTFILL_JEV_ENDPOINT (trusted HTTPS, no URL fallback) and\nVOLTFILL_JEV_API_KEY or VOLTFILL_JEV_API_KEY_FILE (one explicit credential file). Vision/Hybrid select image input;\nno additional image permission flag is required. Reports retain original\nresponse bodies regardless of log level and remain evaluation-only.\nLong-form --pdf, --schema, --allow-remote and --execution-config are also supported.\n\nfill --remote sends only fields left ambiguous, as choices among their own source\ncandidates (label and value text, no images); values still come from the PDF.\nIt accepts --model, --max-attempts and --execution-config FILE (not --config);\nbatches are sent one at a time. --deadline-ms bounds only the offline step;\nrequests are bounded by the execution config (total_deadline_ms). If every sent field fails, the full result is\nstill written and the exit code is 4 (fill_assist_all_failed). Fields left ambiguous or not_found (also\nwhen every sent field ends in not_unique_maximum) exit 0: check each status.\nWithout --remote, fill reads no credentials.\n\n--output FILE is checked and its temporary file created in FILE's directory after\nconfiguration files are read and before inputs, credentials or requests. If the\nresult cannot replace FILE (exit code 5), a file verified to hold all of it, in\nFILE's directory or else the working directory, is named in output_kept; if none\ncan hold it, stderr gets {\"error\":REASON,\"result\":RESULT}.\n";

struct Args {
    command: String,
    values: BTreeMap<String, String>,
    flags: BTreeSet<String>,
    library_path: Option<PathBuf>,
    execution: Option<ExecutionConfig>,
    resources: vf_legacy::resources::ResourceLimits,
    launch_path: Option<PathBuf>,
}
/// Default wall-clock deadline for `voltfill fill` layout and matching, counted
/// from the start of the `fill` command (inputs and inspection included).
const FILL_DEADLINE_MS: u64 = 30_000;
/// Largest accepted `--deadline-ms`.
const MAX_FILL_DEADLINE_MS: u64 = 600_000;

/// The `fill` deadline: `--deadline-ms` or [`FILL_DEADLINE_MS`].
fn fill_deadline(args: &Args) -> Result<Duration, (u8, &'static str)> {
    let ms = match args.values.get("--deadline-ms") {
        None => FILL_DEADLINE_MS,
        Some(value) => value
            .parse::<u64>()
            .ok()
            .filter(|ms| (1..=MAX_FILL_DEADLINE_MS).contains(ms))
            .ok_or((2, "invalid_deadline"))?,
    };
    Ok(Duration::from_millis(ms))
}

fn input_error() -> (u8, &'static str) {
    (2, "invalid_arguments")
}
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, (u8, &'static str)> {
    let invocation = config::expand(args).map_err(|reason| (2, reason))?;
    let mut args = invocation.arguments.into_iter();
    let command = args.next().ok_or_else(input_error)?;
    let command = if command == "run" {
        "extract".into()
    } else {
        command
    };
    if !["validate", "inspect", "extract", "fill"].contains(&command.as_str()) {
        return Err(input_error());
    }
    let mut values = BTreeMap::new();
    let mut flags = BTreeSet::new();
    while let Some(arg) = args.next() {
        let arg = match arg.as_str() {
            "--remote" => "--allow-remote",
            "--config" => "--execution-config",
            "-o" => "--output",
            _ => &arg,
        }
        .to_owned();
        if ["--allow-remote", "--allow-preview-images", "--dry-run"].contains(&arg.as_str()) {
            if !flags.insert(arg) {
                return Err(input_error());
            }
        } else if [
            "--schema",
            "--form",
            "--pdf",
            "--output",
            "--mock",
            "--strategy",
            "--model",
            "--evaluation-profile",
            "--renderer",
            "--pdfium-lib",
            "--pixels-per-point",
            "--max-attempts",
            "--max-concurrency",
            "--execution-config",
            "--resource-config",
            "--log-level",
            "--deadline-ms",
        ]
        .contains(&arg.as_str())
        {
            let value = args
                .next()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(input_error)?;
            if values.insert(arg, value).is_some() {
                return Err(input_error());
            }
        } else if !arg.starts_with('-') {
            let keys: &[&str] = match command.as_str() {
                "extract" => &["--pdf", "--schema"],
                "inspect" => &["--pdf"],
                "fill" => &["--pdf", "--form"],
                _ => &["--schema"],
            };
            let key = keys
                .iter()
                .find(|key| !values.contains_key(**key))
                .ok_or_else(input_error)?;
            values.insert((*key).into(), arg);
        } else {
            return Err(input_error());
        }
    }
    // Only `fill` accepts `--remote` among these commands; its remote options
    // are rejected without it so offline fill stays unchanged.
    let remote_fill = command == "fill" && flags.contains("--allow-remote");
    let allowed: Option<(&[&str], &[&str])> = match command.as_str() {
        "validate" => Some((&["--schema", "--log-level"], &[])),
        "inspect" => Some((
            &["--pdf", "--output", "--log-level", "--resource-config"],
            &[],
        )),
        "fill" if remote_fill => Some((
            &[
                "--pdf",
                "--form",
                "--output",
                "--log-level",
                "--resource-config",
                "--model",
                "--max-attempts",
                "--execution-config",
                "--deadline-ms",
            ],
            &["--allow-remote"],
        )),
        "fill" => Some((
            &[
                "--pdf",
                "--form",
                "--output",
                "--log-level",
                "--resource-config",
                "--deadline-ms",
            ],
            &[],
        )),
        _ => None,
    };
    if allowed.is_some_and(|(keys, allowed_flags)| {
        flags
            .iter()
            .any(|flag| !allowed_flags.contains(&flag.as_str()))
            || values.keys().any(|key| !keys.contains(&key.as_str()))
    }) {
        return Err(input_error());
    }
    settings::defaults(&mut values, &[("--log-level", "VOLTFILL_LOG_LEVEL")])
        .map_err(|e| (2, e))?;
    if remote_fill {
        settings::defaults(
            &mut values,
            &[
                ("--model", "VOLTFILL_MODEL"),
                ("--execution-config", "VOLTFILL_EXECUTION_CONFIG"),
            ],
        )
        .map_err(|e| (2, e))?;
    }
    if command == "extract" {
        settings::defaults(&mut values, settings::COMMON).map_err(|e| (2, e))?;
        settings::defaults(&mut values, &[("--strategy", "VOLTFILL_STRATEGY")])
            .map_err(|e| (2, e))?;
        if invocation.execution.is_none() {
            settings::defaults(
                &mut values,
                &[("--execution-config", "VOLTFILL_EXECUTION_CONFIG")],
            )
            .map_err(|e| (2, e))?;
        }
    }
    if command != "validate" && invocation.resources.is_none() {
        settings::defaults(
            &mut values,
            &[("--resource-config", "VOLTFILL_RESOURCE_CONFIG")],
        )
        .map_err(|e| (2, e))?;
    }
    let resources = settings::resources_from_path(
        values.get("--resource-config").map(std::path::Path::new),
        invocation.resources,
    )
    .map_err(|e| (2, e))?;
    let library_path = values.get("--pdfium-lib").map(PathBuf::from).or_else(|| {
        if cfg!(feature = "pdfium")
            && values
                .get("--renderer")
                .is_none_or(|value| value != "hayro")
        {
            std::env::var_os("VOLTFILL_PDFIUM_LIB_PATH").map(PathBuf::from)
        } else {
            None
        }
    });
    Ok(Args {
        command,
        values,
        flags,
        library_path,
        execution: invocation.execution,
        resources,
        launch_path: invocation.source_path,
    })
}
impl Args {
    fn value(&self, key: &str) -> Result<&str, (u8, &'static str)> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(input_error)
    }
    fn flag(&self, key: &str) -> bool {
        self.flags.contains(key)
    }
}
fn read(path: &str, limit: usize) -> Result<Vec<u8>, (u8, &'static str)> {
    let file = File::open(path).map_err(|_| (2, "input_read_failed"))?;
    if file.metadata().map_err(|_| (2, "input_read_failed"))?.len() > limit as u64 {
        return Err((2, "input_too_large"));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| (2, "input_read_failed"))?;
    if bytes.len() > limit {
        return Err((2, "input_too_large"));
    }
    Ok(bytes)
}
/// Refuse an output path that cannot name a file, an existing symlink,
/// directory or other non-regular file, and one that would overwrite an input,
/// a launch, execution or resource file, a native library or the API key file.
/// The key file is compared by path for every command; it is never read here.
fn check_output_target(args: &Args, path: &str) -> Result<(), (u8, &'static str)> {
    // `Path::file_name` normalizes `dir/` and `dir/.`, which rename cannot
    // replace, so the last component is taken from the raw argument.
    let name = path.rsplit(std::path::is_separator).next().unwrap_or("");
    if matches!(name, "" | "." | "..") {
        return Err((5, "invalid_output_path"));
    }
    let target = Path::new(path);
    match fs::symlink_metadata(target) {
        Ok(m) if m.file_type().is_symlink() => return Err((5, "symlink_output_refused")),
        Ok(m) if m.is_dir() => return Err((5, "output_is_directory")),
        Ok(m) if !m.is_file() => return Err((5, "output_not_regular_file")),
        Ok(_) => {}
        // A missing target is created; a missing directory fails when the
        // temporary file is created next to it.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) if error.kind() == io::ErrorKind::NotADirectory => {
            return Err((5, "output_directory_not_found"));
        }
        // A name that is too long, a symlink loop or a directory that cannot
        // be searched would only fail at the final rename, after sending.
        Err(_) => return Err((5, "output_not_accessible")),
    }
    let canonical = target.canonicalize().ok();
    if [
        "--pdf",
        "--schema",
        "--form",
        "--mock",
        "--execution-config",
        "--resource-config",
    ]
    .into_iter()
    .filter_map(|key| args.values.get(key).map(Path::new))
    .chain(args.library_path.as_deref())
    .chain(args.launch_path.as_deref())
    .any(|input| canonical.is_some() && input.canonicalize().ok() == canonical)
    {
        return Err((5, "input_overwrite_refused"));
    }
    // Only the key file's path is compared, for every command; its contents
    // are not read here.
    if std::env::var_os("VOLTFILL_JEV_API_KEY_FILE").is_some_and(|path| {
        canonical.is_some() && Path::new(&path).canonicalize().ok() == canonical
    }) {
        return Err((5, "input_overwrite_refused"));
    }
    Ok(())
}
/// Where the result is written: stdout, or with `--output` a temporary file
/// created next to the target before any input, credential or request.
enum Sink {
    Stdout,
    File(PendingOutput),
}
impl Sink {
    /// Check the `--output` target and create its temporary file, so a target
    /// that cannot be written fails before anything is sent.
    fn prepare(args: &Args) -> Result<Self, (u8, &'static str)> {
        let Some(path) = args.values.get("--output") else {
            return Ok(Self::Stdout);
        };
        check_output_target(args, path)?;
        PendingOutput::create(Path::new(path)).map(Self::File)
    }
    /// Where the result went when it could not be moved to the target.
    fn fallback(&mut self) -> Option<Fallback> {
        let Self::File(pending) = self else {
            return None;
        };
        match &mut pending.stage {
            Stage::Kept(path) => Some(Fallback::File(path.clone())),
            Stage::Unsaved(bytes) => Some(Fallback::Stderr(std::mem::take(bytes))),
            Stage::Open(_) | Stage::Moved => None,
        }
    }
}
/// A temporary file in the target's directory, held open until the result is
/// written into it and renamed over the target.
///
/// The target is checked once, in [`Sink::prepare`]. Rename replaces the
/// directory entry without following it, so a symlink or hard link planted at
/// the target during the run is itself replaced and its referent is never
/// written; a second check before the write would add no safety and could
/// only discard received bodies.
///
/// If the result cannot reach the target, it is still in memory: it is kept
/// in a file only after that file is verified to hold all of it, and
/// otherwise handed to `main` for stderr.
struct PendingOutput {
    target: PathBuf,
    directory: PathBuf,
    temporary: PathBuf,
    stage: Stage,
}
enum Stage {
    /// Nothing written yet; dropping removes the temporary file.
    Open(File),
    /// Renamed over the target.
    Moved,
    /// Not at the target, but this file was verified to hold the whole
    /// result. Files created by a failed write are never removed, because
    /// they may hold received bodies.
    Kept(PathBuf),
    /// No file could be verified to hold the result; these are its bytes.
    Unsaved(Vec<u8>),
}
/// Where a result that could not replace the `--output` target went.
#[derive(Debug, PartialEq, Eq)]
enum Fallback {
    /// A file verified to exist with the whole result.
    File(PathBuf),
    /// No file could hold it; `main` writes these bytes to stderr.
    Stderr(Vec<u8>),
}
/// Create `.voltfill-<pid>-<n>.tmp` (exclusive, mode 0600) in `directory`.
fn create_temporary(directory: &Path) -> io::Result<(PathBuf, File)> {
    let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
    for n in 0..100 {
        let path = directory.join(format!(".voltfill-{}-{n}.tmp", std::process::id()));
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        match open.open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => last = error,
            Err(error) => return Err(error),
        }
    }
    Err(last)
}
/// Write all of `bytes` and flush them to the device.
fn write_synced(file: &mut File, bytes: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    if let Some(fault) = seam::next_write_fault() {
        file.write_all(&bytes[..bytes.len() / 2])?;
        return match fault {
            seam::WriteFault::Fail => Err(io::Error::other("injected write failure")),
            seam::WriteFault::Short => file.sync_all(),
        };
    }
    file.write_all(bytes)?;
    file.sync_all()
}
/// Whether `path` still names `file` (same device and inode on unix), the
/// file is still linked, and it holds exactly `len` bytes.
fn holds(path: &Path, file: &File, len: usize) -> bool {
    let (Ok(named), Ok(open)) = (fs::symlink_metadata(path), file.metadata()) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if open.nlink() == 0 || (named.dev(), named.ino()) != (open.dev(), open.ino()) {
            return false;
        }
    }
    named.is_file() && named.len() == len as u64 && open.len() == len as u64
}
/// The working directory, the last place a result file is tried.
fn current_directory() -> PathBuf {
    #[cfg(test)]
    if let Some(directory) = seam::current_directory() {
        return directory;
    }
    PathBuf::from(".")
}
impl PendingOutput {
    fn create(target: &Path) -> Result<Self, (u8, &'static str)> {
        let directory = target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        match create_temporary(directory) {
            Ok((temporary, file)) => Ok(Self {
                target: target.to_owned(),
                directory: directory.to_owned(),
                temporary,
                stage: Stage::Open(file),
            }),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Err((5, "output_directory_not_found"))
            }
            Err(_) => Err((5, "output_not_writable")),
        }
    }
    /// Write the result and rename it over the target. On failure the stage
    /// records a verified file holding the result, or the result itself.
    fn commit(&mut self, bytes: Vec<u8>) -> Result<(), (u8, &'static str)> {
        let mut file = match std::mem::replace(&mut self.stage, Stage::Moved) {
            Stage::Open(file) => file,
            other => {
                self.stage = other;
                return Err((5, "output_write_failed"));
            }
        };
        // The file stays open across the rename, so its identity can be
        // checked against the name both before and after a failed rename.
        let reason = if write_synced(&mut file, &bytes).is_err() {
            "output_write_failed"
        } else if !holds(&self.temporary, &file, bytes.len()) {
            // Renaming a name that no longer holds the result could move
            // another file onto the target.
            "output_rename_failed"
        } else if fs::rename(&self.temporary, &self.target).is_ok() {
            return Ok(());
        } else if holds(&self.temporary, &file, bytes.len()) {
            self.stage = Stage::Kept(self.temporary.clone());
            return Err((5, "output_rename_failed"));
        } else {
            "output_rename_failed"
        };
        self.stage = self.rescue(bytes);
        Err((5, reason))
    }
    /// Write the whole result to a new file in the target's directory, then
    /// in the working directory; keep the first one verified to hold it.
    fn rescue(&self, bytes: Vec<u8>) -> Stage {
        let current = current_directory();
        // Compare resolved paths so an absolute target in the working
        // directory is not tried twice.
        let same = match (
            fs::canonicalize(&current),
            fs::canonicalize(&self.directory),
        ) {
            (Ok(a), Ok(b)) => a == b,
            _ => current == self.directory,
        };
        let directories = if same {
            vec![current]
        } else {
            vec![self.directory.clone(), current]
        };
        for directory in directories {
            let Ok((path, mut file)) = create_temporary(&directory) else {
                continue;
            };
            if write_synced(&mut file, &bytes).is_ok() && holds(&path, &file, bytes.len()) {
                return Stage::Kept(path);
            }
        }
        Stage::Unsaved(bytes)
    }
}
impl Drop for PendingOutput {
    fn drop(&mut self) {
        if matches!(self.stage, Stage::Open(_)) {
            let _ = fs::remove_file(&self.temporary);
        }
    }
}
/// Test-only faults for the output writer.
#[cfg(test)]
mod seam {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::path::PathBuf;

    /// Both write half of the bytes; `Fail` then errors like a full disk and
    /// `Short` reports success, so only verification can catch it.
    #[derive(Clone, Copy)]
    pub(super) enum WriteFault {
        Fail,
        Short,
    }
    thread_local! {
        static WRITE_FAULTS: RefCell<VecDeque<WriteFault>> = const { RefCell::new(VecDeque::new()) };
        static CURRENT_DIRECTORY: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    }
    /// Apply `faults` to the next file writes on this thread, in order, and
    /// use `directory` in place of the working directory.
    pub(super) fn set(faults: &[WriteFault], directory: Option<PathBuf>) {
        WRITE_FAULTS.with(|f| *f.borrow_mut() = faults.iter().copied().collect());
        CURRENT_DIRECTORY.with(|d| *d.borrow_mut() = directory);
    }
    pub(super) fn next_write_fault() -> Option<WriteFault> {
        WRITE_FAULTS.with(|f| f.borrow_mut().pop_front())
    }
    pub(super) fn current_directory() -> Option<PathBuf> {
        CURRENT_DIRECTORY.with(|d| d.borrow().clone())
    }
}
fn output(sink: &mut Sink, value: &impl serde::Serialize) -> Result<(), (u8, &'static str)> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|_| (5, "output_serialization_failed"))?;
    bytes.push(b'\n');
    match sink {
        Sink::File(pending) => pending.commit(bytes),
        Sink::Stdout => io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|_| (5, "output_write_failed")),
    }
}
/// Write all of `bytes`, waiting briefly whenever a non-blocking stream is
/// full, so the last-resort copy of a result is not cut short. Gives up after
/// about a minute without progress.
fn write_patiently(stream: &mut impl Write, mut bytes: &[u8]) -> io::Result<()> {
    let mut idle = std::time::Duration::ZERO;
    let step = std::time::Duration::from_millis(5);
    while !bytes.is_empty() {
        match stream.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => {
                bytes = &bytes[n..];
                idle = std::time::Duration::ZERO;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    && idle < std::time::Duration::from_secs(60) =>
            {
                std::thread::sleep(step);
                idle += step;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
/// A failed run. `fallback` says where the result went when it could not be
/// moved to the `--output` target.
#[derive(Debug, PartialEq, Eq)]
struct Failure {
    code: u8,
    reason: &'static str,
    fallback: Option<Fallback>,
}
impl From<(u8, &'static str)> for Failure {
    fn from((code, reason): (u8, &'static str)) -> Self {
        Self {
            code,
            reason,
            fallback: None,
        }
    }
}
/// Report a failed run: a JSON error on stderr, or an error log event when
/// logging is on. A kept file is named in `output_kept`. A result no file
/// could hold is written to stderr as one JSON object, `{"error":REASON,
/// "result":RESULT}`, with the result's bytes unchanged; with logging on, the
/// log event then says whether that write succeeded (`result_on_stderr`).
fn report_failure(failure: &Failure, logging: bool, stderr: &mut impl Write) {
    let mut report = json!({});
    match &failure.fallback {
        Some(Fallback::File(path)) => {
            report["output_kept"] = json!(path.to_string_lossy());
        }
        Some(Fallback::Stderr(bytes)) => {
            let head = format!("{{\"error\":{},\"result\":", json!(failure.reason));
            let written = write_patiently(stderr, head.as_bytes())
                .and_then(|()| write_patiently(stderr, bytes))
                .and_then(|()| write_patiently(stderr, b"}\n"))
                .and_then(|()| stderr.flush());
            report["result_on_stderr"] = json!(written.is_ok());
        }
        None => {}
    }
    if logging {
        report["event"] = json!(failure.reason);
        log::error!(target: "vf_cli", "{report}");
    } else if !matches!(failure.fallback, Some(Fallback::Stderr(_))) {
        report["error"] = json!(failure.reason);
        // A closed stderr leaves only the exit code.
        let _ = writeln!(stderr, "{report}");
    }
}
#[derive(serde::Serialize)]
struct ExtractionError<'response> {
    error: &'static str,
    jev_responses: &'response [JevResponse],
}
fn execute<E: Evaluator>(
    sink: &mut Sink,
    pdf: &[u8],
    form: &schema::FormDefinition,
    options: &ExtractOptions,
    mut evaluator: E,
) -> Result<u8, (u8, &'static str)> {
    log::info!(target: "vf_cli", "{}", json!({"event":"extraction_started"}));
    let report = match engine::extract(pdf, form, options, &mut evaluator) {
        Ok(report) => report,
        Err(error) => {
            if !error.jev_responses().is_empty() {
                output(
                    sink,
                    &ExtractionError {
                        error: "extraction_failed",
                        jev_responses: error.jev_responses(),
                    },
                )?;
            }
            return Err((4, "extraction_failed"));
        }
    };
    let code = if report
        .fields
        .iter()
        .all(|f| f.status == vf_legacy::model::Status::Extracted)
    {
        0
    } else {
        3
    };
    output(sink, &report)?;
    log::info!(target: "vf_cli", "{}", json!({"event":"extraction_completed","fields":form.fields.len(),"exit_code":code}));
    Ok(code)
}
fn configure_rendering(
    options: &mut ExtractOptions,
    args: &Args,
) -> Result<(), (u8, &'static str)> {
    let _ = &options;
    let renderer = args.values.get("--renderer").map(String::as_str);
    if renderer.is_some_and(|value| !["pdfium", "hayro"].contains(&value)) {
        return Err((2, "invalid_renderer"));
    }
    if renderer == Some("hayro") && args.values.contains_key("--pdfium-lib") {
        return Err((2, "renderer_library_conflict"));
    }
    let renderer = renderer.or(args.library_path.as_ref().map(|_| "pdfium"));
    let Some(renderer) = renderer else {
        return if args.values.contains_key("--pixels-per-point") {
            Err(input_error())
        } else {
            Ok(())
        };
    };
    if renderer == "hayro" && !cfg!(feature = "hayro") {
        return Err((2, "hayro_feature_disabled"));
    }
    if renderer == "pdfium" {
        if !cfg!(feature = "pdfium") {
            return Err((2, "pdfium_feature_disabled"));
        }
        if args.library_path.is_none() {
            return Err((2, "rendering_required"));
        }
    }
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    {
        let mut config = if renderer == "hayro" {
            vf_legacy::render::RenderConfig::hayro()
        } else {
            vf_legacy::render::RenderConfig::new(
                args.library_path
                    .as_ref()
                    .ok_or((2, "rendering_required"))?,
            )
        };
        config.pixels_per_point = args
            .values
            .get("--pixels-per-point")
            .map(|scale| scale.parse::<f64>())
            .transpose()
            .map_err(|_| (2, "invalid_render_scale"))?
            .unwrap_or(1.5);
        if !config.pixels_per_point.is_finite() || config.pixels_per_point <= 0.0 {
            return Err((2, "invalid_render_scale"));
        }
        args.resources.apply_render(&mut config);
        options.render_config = Some(config);
    }
    Ok(())
}
fn log_level(args: &Args) -> Result<log::LevelFilter, (u8, &'static str)> {
    match args
        .values
        .get("--log-level")
        .map(String::as_str)
        .unwrap_or("off")
    {
        "off" => Ok(log::LevelFilter::Off),
        "error" => Ok(log::LevelFilter::Error),
        "warn" => Ok(log::LevelFilter::Warn),
        "info" => Ok(log::LevelFilter::Info),
        "debug" => Ok(log::LevelFilter::Debug),
        _ => Err(input_error()),
    }
}
fn max_attempts(args: &Args) -> Result<u32, (u8, &'static str)> {
    match args
        .values
        .get("--max-attempts")
        .map(String::as_str)
        .unwrap_or("3")
    {
        "1" => Ok(1),
        "2" => Ok(2),
        "3" => Ok(3),
        _ => Err(input_error()),
    }
}
fn execution_config(args: &Args) -> Result<ExecutionConfig, (u8, &'static str)> {
    let mut config = match args.values.get("--execution-config") {
        Some(path) => serde_json::from_slice::<ExecutionConfig>(&read(path, 64 * 1024)?)
            .map_err(|_| (2, "invalid_execution_config"))?,
        None => args.execution.clone().unwrap_or_default(),
    };
    if args.values.contains_key("--max-attempts") {
        config.max_attempts = max_attempts(args)?;
    }
    if let Some(value) = args.values.get("--max-concurrency") {
        config.max_concurrency = match value.parse::<usize>() {
            Ok(n) if n >= 1 && n.to_string() == *value => n,
            _ => return Err(input_error()),
        };
    }
    config
        .validate()
        .map_err(|_| (2, "invalid_execution_config"))?;
    Ok(config)
}
fn logger(level: log::LevelFilter, output_file: bool) -> crate::logging::JsonLogger {
    let writer: Box<dyn Write + Send> = if output_file {
        Box::new(io::stdout())
    } else {
        Box::new(io::stderr())
    };
    crate::logging::JsonLogger::new("voltfill-cli", level, writer)
}
fn run_with<E: Evaluator, T: assist::Transport, C: assist::Clock>(
    arguments: impl IntoIterator<Item = String>,
    initialize_logger: impl FnOnce(log::LevelFilter, bool) -> Result<(), (u8, &'static str)>,
    load_secret: impl FnOnce() -> Result<Secret, (u8, &'static str)>,
    make_remote: impl FnOnce(&ExecutionConfig, Secret, bool) -> Result<E, (u8, &'static str)>,
    make_fill_remote: impl FnOnce() -> Result<(T, C), (u8, &'static str)>,
) -> Result<u8, Failure> {
    let args = parse_args(arguments)?;
    let level = log_level(&args)?;
    let execution = execution_config(&args)?;
    initialize_logger(level, args.values.contains_key("--output"))?;
    // Configuration files have been read; inputs, credentials and requests
    // come only after the output target is checked and its temporary file
    // exists. Dropping the sink on an early error removes that file.
    let mut sink = Sink::prepare(&args)?;
    run_command(
        &args,
        &execution,
        &mut sink,
        load_secret,
        make_remote,
        make_fill_remote,
    )
    .map_err(|(code, reason)| Failure {
        code,
        reason,
        fallback: sink.fallback(),
    })
}
fn run_command<E: Evaluator, T: assist::Transport, C: assist::Clock>(
    args: &Args,
    execution: &ExecutionConfig,
    sink: &mut Sink,
    load_secret: impl FnOnce() -> Result<Secret, (u8, &'static str)>,
    make_remote: impl FnOnce(&ExecutionConfig, Secret, bool) -> Result<E, (u8, &'static str)>,
    make_fill_remote: impl FnOnce() -> Result<(T, C), (u8, &'static str)>,
) -> Result<u8, (u8, &'static str)> {
    if args.command == "validate" {
        let form = schema::parse(&read(args.value("--schema")?, schema::MAX_SCHEMA_BYTES)?)
            .map_err(|_| (2, "invalid_schema"))?;
        output(
            sink,
            &json!({"valid":true,"schema_version":form.schema_version,"field_count":form.fields.len()}),
        )?;
        return Ok(0);
    }
    if args.command == "fill" {
        // Layout and matching stop at the deadline; inspection is bounded by
        // PdfLimits and is not interrupted, but its time counts.
        let deadline = Instant::now() + fill_deadline(args)?;
        // Remote options are validated before any input or credential is read.
        let options = if args.flag("--allow-remote") {
            let options = assist::AssistOptions {
                allow_remote: true,
                model: args
                    .values
                    .get("--model")
                    .map_or(assist::MODEL, String::as_str)
                    .into(),
                execution: execution.clone(),
            };
            options.validate().map_err(|error| match error {
                assist::AssistError::Model => (2, "invalid_model"),
                _ => (2, "invalid_execution_config"),
            })?;
            Some(options)
        } else {
            None
        };
        let form =
            vf_core::fill::parse_form(&read(args.value("--form")?, vf_core::fill::MAX_FORM_BYTES)?)
                .map_err(|_| (2, "invalid_form"))?;
        let bytes = read(args.value("--pdf")?, args.resources.pdf.max_bytes)?;
        let remote = match options {
            Some(options) => Some((options, load_secret()?, make_fill_remote()?)),
            None => None,
        };
        let snapshot = vf_source::inspect(&bytes, args.resources.pdf)
            .map_err(|_| (4, "pdf_inspection_failed"))?;
        let result = vf_core::fill::fill_with_limits(
            &snapshot.document_id,
            &snapshot.fragments,
            &form,
            &vf_core::fill::FillLimits {
                deadline: Some(deadline),
                ..vf_core::fill::FillLimits::default()
            },
        )
        .map_err(|error| (4, error.code()))?;
        let Some((options, secret, (mut transport, mut clock))) = remote else {
            output(sink, &result)?;
            return Ok(0);
        };
        log::debug!(target: "vf_cli", "{}", json!({"event":"fill_assist_config","model":options.model,"config":options.execution}));
        let assisted = assist::assist(result, &form, &options, &secret, &mut transport, &mut clock)
            .map_err(|_| (4, "fill_assist_failed"))?;
        output(sink, &assisted)?;
        // The full result with every received body is written first; a run in
        // which every sent field failed then exits like a failed extraction.
        let all_failed =
            !assisted.assist.is_empty() && assisted.assist.values().all(|a| a.outcome.is_failure());
        log::info!(target: "vf_cli", "{}", json!({"event":"fill_assist_completed","questions":assisted.assist.len(),"attempts":assisted.usage.attempts,"responses":assisted.jev_responses.len(),"exit_code":if all_failed { 4 } else { 0 }}));
        if all_failed {
            return Err((4, "fill_assist_all_failed"));
        }
        return Ok(0);
    }
    let bytes = read(args.value("--pdf")?, args.resources.pdf.max_bytes)?;
    if args.command == "inspect" {
        let snapshot =
            pdf::inspect(&bytes, args.resources.pdf).map_err(|_| (4, "pdf_inspection_failed"))?;
        output(sink, &snapshot)?;
        return Ok(0);
    }
    let form = schema::parse(&read(args.value("--schema")?, schema::MAX_SCHEMA_BYTES)?)
        .map_err(|_| (2, "invalid_schema"))?;
    let strategy = match args
        .values
        .get("--strategy")
        .map(String::as_str)
        .unwrap_or("text-first")
    {
        "text" | "text-first" => Strategy::TextFirst,
        "vision" | "vision-first" => Strategy::VisionFirst,
        "hybrid" => Strategy::Hybrid,
        _ => return Err(input_error()),
    };
    form.validate_strategy(strategy)
        .map_err(|_| (2, "strategy_schema_mismatch"))?;
    let model = args
        .values
        .get("--model")
        .map(String::as_str)
        .unwrap_or(vf_legacy::jev::MODEL);
    vf_legacy::jev::validate_model(model).map_err(|_| (2, "invalid_model"))?;
    if strategy != Strategy::TextFirst && !vf_legacy::jev::supports_images(model) {
        return Err((2, "model_does_not_support_images"));
    }
    let evaluation_profile = match args.values.get("--evaluation-profile").map(String::as_str) {
        None => None,
        Some("synthetic") => Some(EvaluationProfile::synthetic()),
        _ => return Err((2, "unknown_evaluation_profile")),
    };
    let mut options = ExtractOptions {
        model: model.into(),
        pdf_limits: args.resources.pdf,
        strategy,
        evaluation_profile,
        ..ExtractOptions::default()
    }
    .with_execution_config(execution)
    .map_err(|_| (2, "invalid_execution_config"))?;
    configure_rendering(&mut options, args)?;
    let image_input = form
        .fields
        .iter()
        .flat_map(|f| &f.questions)
        .any(|q| q.input.modality != schema::Modality::Text);
    if image_input && !args.values.contains_key("--mock") {
        #[cfg(not(any(feature = "pdfium", feature = "hayro")))]
        return Err((2, "pdfium_feature_disabled"));
        #[cfg(any(feature = "pdfium", feature = "hayro"))]
        options
            .render_config
            .as_ref()
            .ok_or((2, "rendering_required"))?
            .validate()
            .map_err(|_| (2, "invalid_render_config"))?;
    }
    log::debug!(target: "vf_cli", "{}", json!({"event":"extraction_config","config":execution}));
    if args.flag("--dry-run") {
        let snapshot =
            pdf::inspect(&bytes, options.pdf_limits).map_err(|_| (4, "pdf_inspection_failed"))?;
        let document_pages: BTreeSet<_> = snapshot.pages.iter().map(|p| p.page).collect();
        let mut planned_pages = BTreeSet::new();
        for field in &form.fields {
            if field.pages.is_empty() {
                planned_pages.extend(&document_pages);
            } else {
                planned_pages.extend(field.pages.iter().copied());
            }
        }
        let fragment_count = snapshot
            .fragments
            .iter()
            .filter(|fragment| planned_pages.contains(&fragment.page))
            .count();
        output(
            sink,
            &json!({"dry_run":true,"document_id":snapshot.document_id,"pdf_bytes":bytes.len(),"pages":planned_pages,"fragment_count":fragment_count,"field_count":form.fields.len(),"model":model,"strategy":strategy,"remote_sent":false}),
        )?;
        return Ok(0);
    }
    if let Some(mock) = args.values.get("--mock") {
        if args.flag("--allow-remote") {
            return Err((2, "mock_remote_conflict"));
        }
        let evaluator = FixtureEvaluator::from_bytes(&read(mock, 1024 * 1024)?)
            .map_err(|_| (2, "invalid_mock_fixture"))?;
        execute(sink, &bytes, &form, &options, evaluator)
    } else {
        if !args.flag("--allow-remote") {
            return Err((2, "remote_permission_required"));
        }
        let secret = load_secret()?;
        let evaluator = make_remote(execution, secret, image_input)?;
        execute(sink, &bytes, &form, &options, evaluator)
    }
}
fn run(arguments: Vec<String>) -> Result<u8, Failure> {
    run_with(
        arguments,
        |level, output_file| {
            if level == log::LevelFilter::Off {
                return Ok(());
            }
            logger(level, output_file)
                .try_init()
                .map_err(|_| (4, "logging_setup_failed"))?;
            Ok(())
        },
        || {
            settings::secret("VOLTFILL_JEV_API_KEY", "VOLTFILL_JEV_API_KEY_FILE")
                .map_err(|e| (2, e))?
                .ok_or((2, "api_key_required"))
        },
        |config, secret, image_input| {
            let evaluator = RemoteEvaluator::try_new(
                settings::endpoint().map_err(|e| (2, e))?,
                secret,
                true,
                image_input,
            )
            .map_err(|_| (4, "transport_setup_failed"))?;
            evaluator
                .with_execution_config(config)
                .map_err(|_| (2, "invalid_execution_config"))
        },
        || {
            let transport =
                assist::HttpTransport::try_new(settings::endpoint().map_err(|e| (2, e))?)
                    .map_err(|_| (4, "transport_setup_failed"))?;
            Ok((transport, assist::SystemClock::default()))
        },
    )
}
pub(crate) fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().is_some_and(|s| s == "serve") {
        let code = if arguments.iter().any(|s| s == "--help" || s == "-h") {
            crate::serve::entry(vec!["--help".into()], None)
        } else {
            match config::expand(arguments) {
                Ok(invocation) => crate::serve::entry_with_resources(
                    invocation.arguments.into_iter().skip(1).collect(),
                    invocation.execution,
                    invocation.resources,
                ),
                Err(reason) => {
                    eprintln!("{}", json!({"error":reason}));
                    2
                }
            }
        };
        std::process::exit(i32::from(code));
    }
    if arguments.is_empty() || arguments.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return;
    }
    let code = match run(arguments) {
        Ok(code) => code,
        Err(failure) => {
            report_failure(
                &failure,
                log::max_level() != log::LevelFilter::Off,
                &mut io::stderr().lock(),
            );
            failure.code
        }
    };
    std::process::exit(i32::from(code));
}

#[cfg(test)]
mod write_patiently_tests {
    use super::write_patiently;
    use std::io::{self, Write};

    /// Accepts a few bytes per call and reports WouldBlock in between.
    struct Choppy {
        out: Vec<u8>,
        calls: usize,
    }
    impl Write for Choppy {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.calls.is_multiple_of(2) {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            let n = buf.len().min(3);
            self.out.extend_from_slice(&buf[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn would_block_is_retried_until_every_byte_is_written() {
        let mut stream = Choppy {
            out: Vec::new(),
            calls: 0,
        };
        write_patiently(&mut stream, b"{\"answers\":[1,2,3]}").unwrap();
        assert_eq!(stream.out, b"{\"answers\":[1,2,3]}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use vf_legacy::jev::transport::{
        AttemptBudget, Clock, HttpRequest, HttpResponse, NoopObserver, Transport, TransportError,
    };

    #[test]
    fn logger_limits_levels_to_our_targets() {
        let logger = logger(log::LevelFilter::Info, false);
        for (target, level, enabled) in [
            ("vf_cli", log::Level::Info, true),
            ("vf_client", log::Level::Error, true),
            ("vf_server", log::Level::Debug, false),
            ("reqwest::connect", log::Level::Error, false),
            ("vf_client_extra", log::Level::Error, false),
        ] {
            assert_eq!(
                log::Log::enabled(
                    &logger,
                    &log::Metadata::builder().target(target).level(level).build()
                ),
                enabled
            );
        }
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
    #[derive(Clone)]
    struct RetryThenSuccess(Arc<AtomicU32>, f64);
    impl Transport for RetryThenSuccess {
        fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            let attempt = self.0.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                return Ok(HttpResponse {
                    body_error: None,
                    status: 503,
                    body: b"private_response_canary".to_vec(),
                    retry_after_ms: None,
                });
            }
            let request: Value = serde_json::from_slice(request.body.bytes())
                .expect("the scripted transport must receive a JSON request");
            let mut answers = serde_json::Map::new();
            for (id, question) in request["questions"]
                .as_object()
                .expect("the scripted request must have a questions object")
            {
                let criteria = question["criteria"]
                    .as_object()
                    .expect("each candidate question must have a criteria object");
                let selected = criteria
                    .iter()
                    .find(|(_, text)| *text == "ALPHA42")
                    .expect("the source text ALPHA42 must appear among the candidates")
                    .0;
                let probabilities: serde_json::Map<String, Value> = criteria
                    .keys()
                    .map(|key| (key.clone(), json!(if key == selected { 1.0 } else { 0.0 })))
                    .collect();
                answers.insert(id.clone(), json!({"type":"choice","choice":selected,"probabilities":probabilities,"confidence":self.1}));
            }
            Ok(HttpResponse { body_error: None, status: 200, body: serde_json::to_vec(&json!({
                "model":vf_legacy::jev::MODEL,"answers":answers,"usage":{"input_tokens":10,"output_tokens":1}
            })).expect("the scripted response must serialize as JSON"), retry_after_ms: None })
        }
    }

    #[test]
    fn remote_originals_are_level_independent_and_logs_are_separate_json_events() {
        struct Capture {
            bytes: Arc<std::sync::Mutex<Vec<u8>>>,
            owner: std::thread::ThreadId,
        }
        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if std::thread::current().id() == self.owner {
                    self.bytes
                        .lock()
                        .expect("the test capture writer must not have panicked")
                        .extend_from_slice(bytes);
                }
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
        let writer = Capture {
            bytes: captured.clone(),
            owner: std::thread::current().id(),
        };
        let built =
            crate::logging::JsonLogger::new("voltfill-cli", log::LevelFilter::Debug, writer);
        log::set_boxed_logger(Box::new(built))
            .expect("this test binary must install the log capture once");
        let request=vf_legacy::jev::parse_request(br#"{"model":"jev-1.14.0","state":"private_source_canary","questions":{"q":{"type":"choice","instructions":"pick","criteria":{"a":"ALPHA42","b":"other"}}}}"#,vf_legacy::jev::Limits::default()).expect("the synthetic choice request satisfies the wire contract");
        let mut baseline = None;
        for level in [
            log::LevelFilter::Off,
            log::LevelFilter::Warn,
            log::LevelFilter::Debug,
        ] {
            log::set_max_level(level);
            captured
                .lock()
                .expect("the test capture writer must not have panicked")
                .clear();
            let mut remote = RemoteEvaluator::with_components(
                Secret::try_new("synthetic_secret_canary".into())
                    .expect("the synthetic bearer contains only permitted characters"),
                true,
                false,
                RetryThenSuccess(Arc::new(AtomicU32::new(0)), 0.87),
                FakeClock::default(),
                AttemptBudget::default(),
                NoopObserver,
            )
            .expect("the scripted retry evaluator must initialize with the default budget")
            .with_max_attempts(2)
            .expect("the evaluator must allow two attempts");
            remote
                .evaluate(
                    &request,
                    &[],
                    &vf_legacy::evaluate::EvaluationContext {
                        field_id: "field",
                        page: 1,
                        remaining_requests: 2,
                        remaining_ms: 30_000,
                    },
                )
                .expect("the scripted success must resolve after one retry");
            let originals = serde_json::to_value(remote.take_responses())
                .expect("recorded attempts must serialize as JSON");
            assert_eq!(
                originals
                    .as_array()
                    .expect("recorded attempts must serialize as an array")
                    .len(),
                2
            );
            assert_eq!(originals[0]["body"], "private_response_canary");
            let body: Value = serde_json::from_str(
                originals[1]["body"]
                    .as_str()
                    .expect("the successful original body must be retained as text"),
            )
            .expect("the successful original body must retain its JSON content");
            assert_eq!(body["answers"]["q"]["confidence"], 0.87);
            if let Some(previous) = &baseline {
                assert_eq!(&originals, previous);
            } else {
                baseline = Some(originals);
            }
            let bytes = captured
                .lock()
                .expect("the test capture writer must not have panicked")
                .clone();
            let logs = std::str::from_utf8(&bytes).expect("log output must be UTF-8");
            for canary in [
                "private_response_canary",
                "private_source_canary",
                "synthetic_secret_canary",
                "ALPHA42",
                "confidence",
            ] {
                assert!(!logs.contains(canary));
            }
            let events: Vec<Value> = logs
                .lines()
                .map(|line| {
                    serde_json::from_str(line).expect("each log line must be a JSON object")
                })
                .collect();
            assert_eq!(events.is_empty(), level <= log::LevelFilter::Error);
            for event in &events {
                assert!(event["event"].is_string());
                assert!(event["level"].is_string());
                assert!(event["timestamp_ms"].is_u64());
            }
            if level >= log::LevelFilter::Info {
                let reserved = events
                    .iter()
                    .find(|e| e["event"] == "attempt_reserved")
                    .expect("info logging must include attempt reservation metadata");
                assert_eq!(reserved["question_count"], 1);
                assert!(reserved["session_attempt"].is_u64());
                let completed = events
                    .iter()
                    .find(|e| e["event"] == "response_complete")
                    .expect("info logging must include response completion metadata");
                assert!(completed["elapsed_ms"].is_u64());
            }
        }
        log::set_max_level(log::LevelFilter::Debug);
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

    #[derive(Clone)]
    struct FillWire(Arc<std::sync::Mutex<Vec<Vec<u8>>>>);
    impl Transport for FillWire {
        fn send(&mut self, request: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            let mut sent = self.0.lock().expect("the request log must not be poisoned");
            sent.push(request.body.bytes().to_vec());
            if sent.len() == 1 {
                return Ok(HttpResponse {
                    status: 503,
                    body: b"private_retry_canary".to_vec(),
                    body_error: None,
                    retry_after_ms: None,
                });
            }
            let request: Value =
                serde_json::from_slice(request.body.bytes()).expect("the fill request is JSON");
            let mut answers = serde_json::Map::new();
            for (id, question) in request["questions"]
                .as_object()
                .expect("the fill request has questions")
            {
                let criteria = question["criteria"]
                    .as_object()
                    .expect("each fill question is a choice");
                let selected = criteria
                    .iter()
                    .find(|(_, text)| text.as_str().is_some_and(|t| t.ends_with("Total: 120")))
                    .expect("the source line Total: 120 is an option")
                    .0;
                let probabilities: serde_json::Map<String, Value> = criteria
                    .keys()
                    .map(|key| (key.clone(), json!(if key == selected { 1.0 } else { 0.0 })))
                    .collect();
                answers.insert(
                    id.clone(),
                    json!({"type":"choice","choice":selected,"probabilities":probabilities,"confidence":0.8}),
                );
            }
            Ok(HttpResponse {
                status: 200,
                body: serde_json::to_vec(&json!({"model":request["model"],"answers":answers,"usage":{"input_tokens":50,"output_tokens":2}}))
                    .expect("the scripted response serializes"),
                body_error: None,
                retry_after_ms: None,
            })
        }
    }

    #[test]
    fn fill_remote_sends_only_ambiguous_fields_and_keeps_every_body() {
        let dir =
            std::env::temp_dir().join(format!("voltfill-fill-remote-unit-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("the temp dir must be writable");
        let pdf = dir.join("input.pdf");
        fs::write(
            &pdf,
            text_pdf(&["Invoice No: INV-7", "Subtotal: 100", "Total: 120"]),
        )
        .expect("the PDF must be writable");
        let form = dir.join("form.json");
        fs::write(&form, br#"{"fields":[{"id":"no","label":"Invoice No"},{"id":"amount","label":"Amount","type":"amount","hints":["Subtotal","Total"]},{"id":"missing","label":"Due"}]}"#)
            .expect("the form must be writable");
        let output = dir.join("result.json");
        let path = |p: &Path| p.to_str().expect("temp paths are UTF-8").to_owned();
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let code = run_with::<FixtureEvaluator, _, _>(
            [
                "fill".into(),
                path(&pdf),
                path(&form),
                "--remote".into(),
                "--max-attempts".into(),
                "2".into(),
                "-o".into(),
                path(&output),
            ],
            |_, _| Ok(()),
            || {
                Ok(Secret::try_new("synthetic_secret_canary".into())
                    .expect("the synthetic bearer is valid"))
            },
            |_, _, _| Err((9, "legacy_evaluator_unexpected")),
            || Ok((FillWire(sent.clone()), FakeClock::default())),
        )
        .expect("fill --remote with a scripted transport succeeds");
        assert_eq!(code, 0);
        let result: Value =
            serde_json::from_slice(&fs::read(&output).expect("the result file exists"))
                .expect("the result is JSON");
        fs::remove_dir_all(&dir).ok();

        let sent = sent.lock().expect("the request log must not be poisoned");
        assert_eq!(sent.len(), 2);
        for body in sent.iter() {
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
        let body: Value = serde_json::from_str(
            responses[1]["body"]
                .as_str()
                .expect("the success body is kept as text"),
        )
        .expect("the success body is the original JSON");
        assert_eq!(body["answers"]["q0"]["confidence"], 0.8);
    }

    /// Counts sends and answers every attempt with 503.
    #[derive(Clone)]
    struct Unavailable(Arc<AtomicU32>);
    impl Transport for Unavailable {
        fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse {
                status: 503,
                body: b"private_unavailable_canary".to_vec(),
                body_error: None,
                retry_after_ms: None,
            })
        }
    }

    fn fill_inputs(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("voltfill-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("the temp dir must be writable");
        let pdf = dir.join("input.pdf");
        fs::write(
            &pdf,
            text_pdf(&["Invoice No: INV-7", "Subtotal: 100", "Total: 120"]),
        )
        .expect("the PDF must be writable");
        let form = dir.join("form.json");
        fs::write(&form, br#"{"fields":[{"id":"no","label":"Invoice No"},{"id":"amount","label":"Amount","type":"amount","hints":["Subtotal","Total"]}]}"#)
            .expect("the form must be writable");
        (dir, pdf, form)
    }

    fn arg(p: &Path) -> String {
        p.to_str().expect("temp paths are UTF-8").to_owned()
    }

    /// Temporary output files left in `dir`.
    fn temporaries(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .expect("the temp dir is readable")
            .map(|entry| entry.expect("the entry is readable").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".voltfill-"))
            })
            .collect()
    }

    /// Answers every attempt with 503 like [`Unavailable`], after running
    /// `before_first` when the first request is sent.
    struct Meddling<F: FnOnce()>(Arc<AtomicU32>, Option<F>);
    impl<F: FnOnce()> Transport for Meddling<F> {
        fn send(&mut self, _: HttpRequest<'_>) -> Result<HttpResponse, TransportError> {
            if let Some(before_first) = self.1.take() {
                before_first();
            }
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(HttpResponse {
                status: 503,
                body: b"private_unavailable_canary".to_vec(),
                body_error: None,
                retry_after_ms: None,
            })
        }
    }

    /// `fill --remote --max-attempts 2 -o output` with a transport that runs
    /// `before_first` on the first send.
    fn fill_remote_with(
        pdf: &Path,
        form: &Path,
        output: &Path,
        sends: &Arc<AtomicU32>,
        before_first: impl FnOnce(),
    ) -> Result<u8, Failure> {
        run_with::<FixtureEvaluator, _, _>(
            [
                "fill".into(),
                arg(pdf),
                arg(form),
                "--remote".into(),
                "--max-attempts".into(),
                "2".into(),
                "-o".into(),
                arg(output),
            ],
            |_, _| Ok(()),
            || {
                Ok(Secret::try_new("synthetic_secret_canary".into())
                    .expect("the synthetic bearer is valid"))
            },
            |_, _, _| Err((9, "legacy_evaluator_unexpected")),
            || {
                Ok((
                    Meddling(sends.clone(), Some(before_first)),
                    FakeClock::default(),
                ))
            },
        )
    }

    #[test]
    fn fill_remote_refuses_the_output_target_before_credentials_or_sends() {
        let (dir, pdf, form) = fill_inputs("fill-remote-preflight");
        let config = dir.join("execution.json");
        fs::write(&config, b"{}").expect("the config must be writable");
        let link = dir.join("link.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("elsewhere.json"), &link)
            .expect("the symlink must be creatable");
        let existing = dir.join("existing-dir");
        fs::create_dir(&existing).expect("the directory must be creatable");
        let mut cases = vec![
            (form.clone(), Vec::new(), "input_overwrite_refused"),
            (pdf.clone(), Vec::new(), "input_overwrite_refused"),
            (
                config.clone(),
                vec!["--execution-config".to_owned(), arg(&config)],
                "input_overwrite_refused",
            ),
            (existing.clone(), Vec::new(), "output_is_directory"),
            (
                dir.join("missing").join("result.json"),
                Vec::new(),
                "output_directory_not_found",
            ),
            (
                form.join("result.json"),
                Vec::new(),
                "output_directory_not_found",
            ),
            (PathBuf::new(), Vec::new(), "invalid_output_path"),
            (
                PathBuf::from(format!("{}/", arg(&dir.join("new")))),
                Vec::new(),
                "invalid_output_path",
            ),
            (dir.join(".."), Vec::new(), "invalid_output_path"),
            // No file system accepts a 300-byte name; the rename would fail
            // only after sending.
            (
                dir.join("a".repeat(300)),
                Vec::new(),
                "output_not_accessible",
            ),
        ];
        if cfg!(unix) {
            cases.push((link.clone(), Vec::new(), "symlink_output_refused"));
            cases.push((
                PathBuf::from("/dev/null"),
                Vec::new(),
                "output_not_regular_file",
            ));
        }
        #[cfg(unix)]
        {
            let looped = dir.join("loop");
            std::os::unix::fs::symlink(&looped, &looped).expect("the loop can be created");
            cases.push((
                looped.join("result.json"),
                Vec::new(),
                "output_not_accessible",
            ));
        }
        #[cfg(unix)]
        let unsearchable = {
            use std::os::unix::fs::PermissionsExt;
            let unsearchable = dir.join("unsearchable");
            fs::create_dir(&unsearchable).expect("the directory must be creatable");
            fs::set_permissions(&unsearchable, fs::Permissions::from_mode(0o000))
                .expect("the directory mode must be settable");
            // A privileged user can still search it; skip that case then.
            if fs::symlink_metadata(unsearchable.join("probe"))
                .is_err_and(|e| e.kind() == io::ErrorKind::PermissionDenied)
            {
                cases.push((
                    unsearchable.join("result.json"),
                    Vec::new(),
                    "output_not_accessible",
                ));
            }
            unsearchable
        };
        #[cfg(unix)]
        let read_only = {
            use std::os::unix::fs::PermissionsExt;
            let read_only = dir.join("read-only");
            fs::create_dir(&read_only).expect("the directory must be creatable");
            fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555))
                .expect("the directory mode must be settable");
            // A privileged user can still write there; skip that case then.
            let probe = read_only.join("probe");
            if fs::write(&probe, b"").is_ok() {
                fs::remove_file(&probe).ok();
            } else {
                cases.push((
                    read_only.join("result.json"),
                    Vec::new(),
                    "output_not_writable",
                ));
            }
            read_only
        };
        let originals: Vec<Vec<u8>> = [&form, &pdf, &config]
            .iter()
            .map(|p| fs::read(p).expect("the input is readable"))
            .collect();
        for (target, extra, reason) in cases {
            let sends = Arc::new(AtomicU32::new(0));
            let secret_loads = std::cell::Cell::new(0);
            let transports = std::cell::Cell::new(0);
            let mut arguments = vec![
                "fill".to_owned(),
                arg(&pdf),
                arg(&form),
                "--remote".into(),
                "-o".into(),
                arg(&target),
            ];
            arguments.extend(extra);
            let result = run_with::<FixtureEvaluator, _, _>(
                arguments,
                |_, _| Ok(()),
                || {
                    secret_loads.set(secret_loads.get() + 1);
                    Ok(Secret::try_new("synthetic_secret_canary".into())
                        .expect("the synthetic bearer is valid"))
                },
                |_, _, _| Err((9, "legacy_evaluator_unexpected")),
                || {
                    transports.set(transports.get() + 1);
                    Ok((Unavailable(sends.clone()), FakeClock::default()))
                },
            );
            assert_eq!(
                result,
                Err(Failure {
                    code: 5,
                    reason,
                    fallback: None
                }),
                "{target:?}"
            );
            assert_eq!(sends.load(Ordering::SeqCst), 0, "{target:?}");
            assert_eq!(secret_loads.get(), 0, "{target:?}");
            assert_eq!(transports.get(), 0, "{target:?}");
        }
        for (path, original) in [&form, &pdf, &config].into_iter().zip(originals) {
            assert_eq!(fs::read(path).expect("the input is readable"), original);
        }
        assert!(!dir.join("elsewhere.json").exists());
        assert!(fs::read_dir(&existing).expect("readable").next().is_none());
        assert!(temporaries(&dir).is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(temporaries(&read_only).is_empty());
            fs::set_permissions(&read_only, fs::Permissions::from_mode(0o755)).ok();
            fs::set_permissions(&unsearchable, fs::Permissions::from_mode(0o755)).ok();
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_run_that_stops_before_sending_removes_its_temporary_output() {
        let (dir, pdf, form) = fill_inputs("fill-remote-early-exit");
        let output = dir.join("result.json");
        for failing in ["secret", "transport"] {
            let result = run_with::<FixtureEvaluator, Unavailable, FakeClock>(
                [
                    "fill".into(),
                    arg(&pdf),
                    arg(&form),
                    "--remote".into(),
                    "-o".into(),
                    arg(&output),
                ],
                |_, _| Ok(()),
                || {
                    if failing == "secret" {
                        Err((2, "api_key_required"))
                    } else {
                        Ok(Secret::try_new("synthetic_secret_canary".into())
                            .expect("the synthetic bearer is valid"))
                    }
                },
                |_, _, _| Err((9, "legacy_evaluator_unexpected")),
                || Err((2, "jev_endpoint_required")),
            );
            let reason = if failing == "secret" {
                "api_key_required"
            } else {
                "jev_endpoint_required"
            };
            assert_eq!(result, Err((2, reason).into()), "{failing}");
            assert!(temporaries(&dir).is_empty(), "{failing}");
            assert!(!output.exists(), "{failing}");
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_rename_keeps_the_temporary_output_with_every_body() {
        let (dir, pdf, form) = fill_inputs("fill-remote-rename-failed");
        let output = dir.join("result.json");
        let sends = Arc::new(AtomicU32::new(0));
        // The target passes the preflight and becomes a directory mid-run.
        let result = fill_remote_with(&pdf, &form, &output, &sends, || {
            fs::create_dir(&output).expect("the target can become a directory");
        });
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        let failure = result.expect_err("the rename over a directory fails");
        assert_eq!((failure.code, failure.reason), (5, "output_rename_failed"));
        let Some(Fallback::File(kept)) = failure.fallback else {
            panic!("the temporary file is reported");
        };
        assert_eq!(kept.parent(), output.parent());
        assert_eq!(temporaries(&dir), std::slice::from_ref(&kept));
        let written: Value =
            serde_json::from_slice(&fs::read(&kept).expect("the temporary file remains"))
                .expect("the kept result is complete JSON");
        fs::remove_dir_all(&dir).ok();
        let responses = written["jev_responses"].as_array().expect("responses");
        assert_eq!(responses.len(), 2);
        assert!(
            responses
                .iter()
                .all(|r| r["http_status"] == 503 && r["body"] == "private_unavailable_canary")
        );
        assert_eq!(written["assist"]["amount"]["outcome"], "request_failed");
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

    /// The kept file reported by `failure`, checked to hold the whole result.
    fn kept_file(failure: &Failure) -> (PathBuf, Vec<u8>) {
        let Some(Fallback::File(kept)) = &failure.fallback else {
            panic!("a kept file is reported: {:?}", failure.fallback.is_some());
        };
        let bytes = fs::read(kept).expect("the reported file exists");
        assert_every_body(&bytes);
        (kept.clone(), bytes)
    }

    /// Runs `fill --remote` with `faults` on the next file writes and
    /// `current` in place of the working directory, resetting both after.
    fn fill_remote_seamed(
        faults: &[seam::WriteFault],
        current: Option<PathBuf>,
        pdf: &Path,
        form: &Path,
        output: &Path,
        before_first: impl FnOnce(),
    ) -> Failure {
        seam::set(faults, current);
        let sends = Arc::new(AtomicU32::new(0));
        let result = fill_remote_with(pdf, form, output, &sends, before_first);
        seam::set(&[], None);
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        let failure = result.expect_err("the result cannot replace the target");
        assert_eq!(failure.code, 5);
        assert!(!output.exists());
        failure
    }

    #[test]
    fn a_temporary_output_deleted_mid_run_is_rewritten_to_a_verified_file() {
        let (dir, pdf, form) = fill_inputs("fill-remote-temporary-deleted");
        let output = dir.join("result.json");
        let failure = fill_remote_seamed(&[], None, &pdf, &form, &output, || {
            for temporary in temporaries(&dir) {
                fs::remove_file(temporary).expect("the temporary file can be deleted");
            }
        });
        assert_eq!(failure.reason, "output_rename_failed");
        let (kept, _) = kept_file(&failure);
        assert_eq!(kept.parent(), Some(dir.as_path()));
        assert_eq!(temporaries(&dir), [kept]);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_removed_output_directory_falls_back_to_the_working_directory_then_stderr() {
        let (dir, pdf, form) = fill_inputs("fill-remote-directory-removed");
        let current = dir.join("current");
        fs::create_dir(&current).expect("the stand-in working directory is creatable");
        for working in [current.clone(), dir.join("current-missing")] {
            let out = dir.join("out");
            fs::create_dir(&out).expect("the output directory is creatable");
            let output = out.join("result.json");
            let failure =
                fill_remote_seamed(&[], Some(working.clone()), &pdf, &form, &output, || {
                    fs::remove_dir_all(&out).expect("the output directory can be removed");
                });
            assert_eq!(failure.reason, "output_rename_failed", "{working:?}");
            if working == current {
                let (kept, _) = kept_file(&failure);
                assert_eq!(kept.parent(), Some(current.as_path()));
                fs::remove_file(kept).expect("the kept file can be removed");
            } else {
                let Some(Fallback::Stderr(bytes)) = &failure.fallback else {
                    panic!("no file can hold the result, so it goes to stderr");
                };
                assert_every_body(bytes);
            }
        }
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_failed_write_is_rewritten_to_a_verified_file_or_handed_to_stderr() {
        use seam::WriteFault::{Fail, Short};
        let (dir, pdf, form) = fill_inputs("fill-remote-write-failed");
        let current = dir.join("current");
        fs::create_dir(&current).expect("the stand-in working directory is creatable");
        let output = dir.join("result.json");
        // A short write that reports success is caught only by verification.
        for (faults, kept_in) in [
            (&[Fail][..], Some(&dir)),
            (&[Fail, Short], Some(&current)),
            (&[Fail, Fail, Fail], None),
            (&[Fail, Short, Short], None),
        ] {
            let failure =
                fill_remote_seamed(faults, Some(current.clone()), &pdf, &form, &output, || {});
            assert_eq!(failure.reason, "output_write_failed");
            match kept_in {
                Some(directory) => {
                    let (kept, bytes) = kept_file(&failure);
                    assert_eq!(kept.parent(), Some(directory.as_path()));
                    assert_eq!(
                        fs::metadata(&kept).expect("the kept file exists").len(),
                        bytes.len() as u64
                    );
                }
                None => {
                    let Some(Fallback::Stderr(bytes)) = &failure.fallback else {
                        panic!("no file holds the result, so it goes to stderr");
                    };
                    assert_every_body(bytes);
                }
            }
            // Incomplete files are left in place but never reported.
            for temporary in temporaries(&dir).into_iter().chain(temporaries(&current)) {
                fs::remove_file(temporary).expect("the temporary file can be removed");
            }
        }
        fs::remove_dir_all(&dir).ok();
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
                serde_json::from_slice::<Value>(result).expect("the result is JSON")
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

    #[cfg(unix)]
    #[test]
    fn a_symlink_planted_at_the_target_mid_run_is_replaced_not_followed() {
        let (dir, pdf, form) = fill_inputs("fill-remote-planted-link");
        let output = dir.join("result.json");
        let victim = dir.join("victim.txt");
        fs::write(&victim, b"unchanged").expect("the victim must be writable");
        let sends = Arc::new(AtomicU32::new(0));
        let result = fill_remote_with(&pdf, &form, &output, &sends, || {
            std::os::unix::fs::symlink(&victim, &output).expect("the symlink can be planted");
        });
        // Both attempts failed, so the run exits 4 after writing the result.
        assert_eq!(result, Err((4, "fill_assist_all_failed").into()));
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        assert!(
            fs::symlink_metadata(&output)
                .expect("the target exists")
                .is_file()
        );
        assert_eq!(fs::read(&victim).expect("readable"), b"unchanged");
        let written: Value =
            serde_json::from_slice(&fs::read(&output).expect("the result is written"))
                .expect("the result is JSON");
        assert!(temporaries(&dir).is_empty());
        fs::remove_dir_all(&dir).ok();
        assert_eq!(
            written["jev_responses"]
                .as_array()
                .expect("responses")
                .len(),
            2
        );
    }

    #[test]
    fn fill_remote_writes_every_body_then_exits_4_when_every_sent_field_fails() {
        let (dir, pdf, form) = fill_inputs("fill-remote-all-failed");
        let output = dir.join("result.json");
        let sends = Arc::new(AtomicU32::new(0));
        let result = run_with::<FixtureEvaluator, _, _>(
            [
                "fill".into(),
                arg(&pdf),
                arg(&form),
                "--remote".into(),
                "--max-attempts".into(),
                "2".into(),
                "-o".into(),
                arg(&output),
            ],
            |_, _| Ok(()),
            || {
                Ok(Secret::try_new("synthetic_secret_canary".into())
                    .expect("the synthetic bearer is valid"))
            },
            |_, _, _| Err((9, "legacy_evaluator_unexpected")),
            || Ok((Unavailable(sends.clone()), FakeClock::default())),
        );
        assert_eq!(result, Err((4, "fill_assist_all_failed").into()));
        assert_eq!(sends.load(Ordering::SeqCst), 2);
        assert!(temporaries(&dir).is_empty());
        let written: Value =
            serde_json::from_slice(&fs::read(&output).expect("the result is still written"))
                .expect("the result is JSON");
        fs::remove_dir_all(&dir).ok();
        assert_eq!(written["assist"]["amount"]["outcome"], "request_failed");
        assert_eq!(written["fields"]["amount"]["status"], "ambiguous");
        assert_eq!(written["fields"]["no"]["status"], "filled");
        let responses = written["jev_responses"].as_array().expect("responses");
        assert_eq!(responses.len(), 2);
        assert!(
            responses
                .iter()
                .all(|r| r["http_status"] == 503 && r["body"] == "private_unavailable_canary")
        );
    }
}
