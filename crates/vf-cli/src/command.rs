//! Command parsing and the `inspect` and `fill` commands.
use crate::settings;
#[cfg(test)]
use serde_json::Value;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use vf_core::assist::{self, ExecutionConfig, Secret};

const HELP: &str = "VOLTFILL — fill a form from a PDF's own text (results are evaluation-only)

voltfill inspect INPUT.pdf
voltfill fill INPUT.pdf FORM.json [--remote [--images --renderer hayro|pdfium]]
voltfill serve [--listen 127.0.0.1:3000] (see voltfill serve --help)

fill matches form labels to the PDF text and AcroForm values offline and reads no
credentials. --deadline-ms N (1..600000, default: 30000) bounds layout and matching
from the start of fill; past a limit fill exits 4 before writing or sending.
--remote asks Jev to choose among the source candidates of ambiguous fields; with
--images it also sends page images and asks where each remaining value is. Values
always come from the PDF text layer. Remote runs need VOLTFILL_JEV_ENDPOINT (HTTPS)
and VOLTFILL_JEV_API_KEY or VOLTFILL_JEV_API_KEY_FILE, and accept --model ID
(default: jev-1.14.0), --max-attempts 1|2|3 (default: 3) and --execution-config FILE.
--images accepts --pdfium-lib PATH and --pixels-per-point N (default: 1.5) and takes
documents of up to 10 pages. Exit code 4 when every remote record failed (the full
result is still written).
--resource-config FILE lowers PDF/render ceilings.
--log-level off|error|warn|info|debug (default: off) writes JSON Lines to stderr, or
to stdout with --output FILE (-o FILE). Options also read VOLTFILL_LOG_LEVEL,
VOLTFILL_RESOURCE_CONFIG, VOLTFILL_MODEL, VOLTFILL_EXECUTION_CONFIG, VOLTFILL_RENDERER,
VOLTFILL_PIXELS_PER_POINT and VOLTFILL_PDFIUM_LIB_PATH; flags take precedence.
--output is checked before inputs, credentials or requests; a result that cannot
replace it (exit 5) is kept in a verified file named in output_kept, or written to
stderr as {\"error\":REASON,\"result\":RESULT}.
";

struct Args {
    command: String,
    values: BTreeMap<String, String>,
    flags: BTreeSet<String>,
    library_path: Option<PathBuf>,
    resources: settings::ResourceLimits,
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
const VALUE_OPTIONS: &[&str] = &[
    "--pdf",
    "--form",
    "--output",
    "--log-level",
    "--resource-config",
    "--deadline-ms",
    "--model",
    "--max-attempts",
    "--execution-config",
    "--renderer",
    "--pdfium-lib",
    "--pixels-per-point",
];
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, (u8, &'static str)> {
    let mut args = args.into_iter();
    let command = args
        .next()
        .filter(|c| c == "inspect" || c == "fill")
        .ok_or_else(input_error)?;
    let positional: &[&str] = if command == "fill" {
        &["--pdf", "--form"]
    } else {
        &["--pdf"]
    };
    let mut values = BTreeMap::new();
    let mut flags = BTreeSet::new();
    while let Some(arg) = args.next() {
        let arg = match arg.as_str() {
            "--remote" => "--allow-remote",
            "-o" => "--output",
            _ => &arg,
        }
        .to_owned();
        if arg == "--allow-remote" || arg == "--images" {
            if !flags.insert(arg) {
                return Err(input_error());
            }
        } else if VALUE_OPTIONS.contains(&arg.as_str()) {
            let value = args
                .next()
                .filter(|v| !v.starts_with("--"))
                .ok_or_else(input_error)?;
            if values.insert(arg, value).is_some() {
                return Err(input_error());
            }
        } else if !arg.starts_with('-') {
            let key = positional
                .iter()
                .find(|key| !values.contains_key(**key))
                .ok_or_else(input_error)?;
            values.insert((*key).into(), arg);
        } else {
            return Err(input_error());
        }
    }
    // Remote options need `fill --remote`, and rendering options `--images`,
    // so offline fill stays unchanged.
    let remote = command == "fill" && flags.contains("--allow-remote");
    let images = flags.contains("--images");
    let mut allowed = vec!["--pdf", "--output", "--log-level", "--resource-config"];
    if command == "fill" {
        allowed.extend(["--form", "--deadline-ms"]);
    }
    if remote {
        allowed.extend(["--model", "--max-attempts", "--execution-config"]);
    }
    if images {
        allowed.extend(["--renderer", "--pdfium-lib", "--pixels-per-point"]);
    }
    if (images && !remote)
        || (command == "inspect" && !flags.is_empty())
        || values.keys().any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(input_error());
    }
    let mut defaults = vec![
        ("--log-level", "VOLTFILL_LOG_LEVEL"),
        ("--resource-config", "VOLTFILL_RESOURCE_CONFIG"),
    ];
    if remote {
        defaults.extend([
            ("--model", "VOLTFILL_MODEL"),
            ("--execution-config", "VOLTFILL_EXECUTION_CONFIG"),
        ]);
    }
    if images {
        defaults.extend([
            ("--renderer", "VOLTFILL_RENDERER"),
            ("--pixels-per-point", "VOLTFILL_PIXELS_PER_POINT"),
        ]);
    }
    settings::defaults(&mut values, &defaults).map_err(|e| (2, e))?;
    let resources =
        settings::resources(values.get("--resource-config").map(Path::new)).map_err(|e| (2, e))?;
    let library_path = values.get("--pdfium-lib").map(PathBuf::from).or_else(|| {
        if cfg!(feature = "pdfium") && values.get("--renderer").is_none_or(|r| r != "hayro") {
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
        resources,
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
/// an execution or resource file, a native library or the API key file.
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
    if ["--pdf", "--form", "--execution-config", "--resource-config"]
        .into_iter()
        .filter_map(|key| args.values.get(key).map(Path::new))
        .chain(args.library_path.as_deref())
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
/// The renderer chosen by `--renderer`, or PDFium when only a library path
/// is given (`--pdfium-lib` / `VOLTFILL_PDFIUM_LIB_PATH`). No automatic
/// fallback or library search.
fn select_renderer(args: &Args) -> Result<&'static str, (u8, &'static str)> {
    let renderer = args.values.get("--renderer").map(String::as_str);
    let renderer = match renderer.or(args.library_path.as_ref().map(|_| "pdfium")) {
        Some(_) if renderer == Some("hayro") && args.values.contains_key("--pdfium-lib") => {
            return Err((2, "renderer_library_conflict"));
        }
        Some("hayro") if cfg!(feature = "hayro") => "hayro",
        Some("hayro") => return Err((2, "hayro_feature_disabled")),
        Some("pdfium") if !cfg!(feature = "pdfium") => return Err((2, "pdfium_feature_disabled")),
        Some("pdfium") if args.library_path.is_some() => "pdfium",
        None if args.values.contains_key("--pixels-per-point") => return Err(input_error()),
        Some("pdfium") | None => return Err((2, "rendering_required")),
        Some(_) => return Err((2, "invalid_renderer")),
    };
    Ok(renderer)
}
/// The validated render configuration for `fill --remote --images`. Without
/// a renderer feature no configuration can exist.
#[cfg(any(feature = "pdfium", feature = "hayro"))]
type FillRender = vf_core::images::RenderConfig;
#[cfg(not(any(feature = "pdfium", feature = "hayro")))]
type FillRender = std::convert::Infallible;
fn fill_render(args: &Args, model: &str) -> Result<FillRender, (u8, &'static str)> {
    if !vf_core::images::supports_images(model) {
        return Err((2, "model_does_not_support_images"));
    }
    let renderer = select_renderer(args)?;
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    {
        use vf_core::images::RenderConfig;
        let mut config = match (renderer, &args.library_path) {
            ("hayro", _) => RenderConfig::hayro(),
            (_, Some(library)) => RenderConfig::new(library),
            (_, None) => return Err((2, "rendering_required")),
        };
        config.pixels_per_point = match args.values.get("--pixels-per-point") {
            None => 1.5,
            Some(scale) => scale
                .parse::<f64>()
                .ok()
                .filter(|s| s.is_finite() && *s > 0.0)
                .ok_or((2, "invalid_render_scale"))?,
        };
        args.resources.apply_render(&mut config);
        config
            .validate()
            .map_err(|_| (2, "invalid_render_config"))?;
        Ok(config)
    }
    #[cfg(not(any(feature = "pdfium", feature = "hayro")))]
    {
        let _ = renderer;
        Err((2, "rendering_required"))
    }
}
/// Exit code and reason for an image-step refusal; every one comes before a
/// send. The two input refusals (page count, image size) exit 2 like other
/// unusable inputs, whether found right after the renderer opens or later.
#[cfg(any(feature = "pdfium", feature = "hayro"))]
fn image_error(error: vf_core::images::ImageError) -> (u8, &'static str) {
    use vf_core::images::ImageError;
    match error {
        ImageError::PageLimit => (2, "image_page_limit_exceeded"),
        ImageError::ImageSize => (2, "image_size_out_of_range"),
        ImageError::ModelWithoutImages => (2, "model_does_not_support_images"),
        ImageError::Fill(error) => (4, error.code()),
        _ => (4, "fill_assist_failed"),
    }
}
fn log_level(args: &Args) -> Result<log::LevelFilter, (u8, &'static str)> {
    crate::logging::level(args.values.get("--log-level").map(String::as_str))
        .ok_or_else(input_error)
}
/// The remote options of `fill --remote`, validated before any input or
/// credential is read.
fn assist_options(args: &Args) -> Result<assist::AssistOptions, (u8, &'static str)> {
    let mut execution = match args.values.get("--execution-config") {
        Some(path) => serde_json::from_slice::<ExecutionConfig>(&read(path, 64 * 1024)?)
            .map_err(|_| (2, "invalid_execution_config"))?,
        None => ExecutionConfig::default(),
    };
    if let Some(value) = args.values.get("--max-attempts") {
        execution.max_attempts = match value.as_str() {
            "1" => 1,
            "2" => 2,
            "3" => 3,
            _ => return Err(input_error()),
        };
    }
    let options = assist::AssistOptions {
        allow_remote: true,
        model: args
            .values
            .get("--model")
            .map_or(assist::MODEL, String::as_str)
            .into(),
        execution,
    };
    options.validate().map_err(|error| match error {
        assist::AssistError::Model => (2, "invalid_model"),
        _ => (2, "invalid_execution_config"),
    })?;
    Ok(options)
}
fn run_with<T: assist::Transport, C: assist::Clock>(
    arguments: impl IntoIterator<Item = String>,
    initialize_logger: impl FnOnce(log::LevelFilter, bool) -> Result<(), (u8, &'static str)>,
    load_secret: impl FnOnce() -> Result<Secret, (u8, &'static str)>,
    make_fill_remote: impl FnOnce() -> Result<(T, C), (u8, &'static str)>,
) -> Result<u8, Failure> {
    let args = parse_args(arguments)?;
    let level = log_level(&args)?;
    let options = if args.flag("--allow-remote") {
        Some(assist_options(&args)?)
    } else {
        None
    };
    initialize_logger(level, args.values.contains_key("--output"))?;
    // Configuration files have been read; inputs, credentials and requests
    // come only after the output target is checked and its temporary file
    // exists. Dropping the sink on an early error removes that file.
    let mut sink = Sink::prepare(&args)?;
    let result = if args.command == "inspect" {
        inspect(&args, &mut sink)
    } else {
        fill(&args, options, &mut sink, load_secret, make_fill_remote)
    };
    result.map_err(|(code, reason)| Failure {
        code,
        reason,
        fallback: sink.fallback(),
    })
}
fn inspect(args: &Args, sink: &mut Sink) -> Result<u8, (u8, &'static str)> {
    let bytes = read(args.value("--pdf")?, args.resources.pdf.max_bytes)?;
    let snapshot =
        vf_source::inspect(&bytes, args.resources.pdf).map_err(|_| (4, "pdf_inspection_failed"))?;
    output(sink, &snapshot)?;
    Ok(0)
}
fn fill<T: assist::Transport, C: assist::Clock>(
    args: &Args,
    options: Option<assist::AssistOptions>,
    sink: &mut Sink,
    load_secret: impl FnOnce() -> Result<Secret, (u8, &'static str)>,
    make_fill_remote: impl FnOnce() -> Result<(T, C), (u8, &'static str)>,
) -> Result<u8, (u8, &'static str)> {
    // Layout and matching stop at the deadline; inspection is bounded by
    // PdfLimits and is not interrupted, but its time counts.
    let deadline = Instant::now() + fill_deadline(args)?;
    let limits = vf_core::fill::FillLimits {
        deadline: Some(deadline),
        ..vf_core::fill::FillLimits::default()
    };
    let render = match &options {
        Some(options) if args.flag("--images") => Some(fill_render(args, &options.model)?),
        _ => None,
    };
    let form =
        vf_core::fill::parse_form(&read(args.value("--form")?, vf_core::fill::MAX_FORM_BYTES)?)
            .map_err(|_| (2, "invalid_form"))?;
    let bytes = read(args.value("--pdf")?, args.resources.pdf.max_bytes)?;
    // The renderer reads the page tree and page geometry before any
    // credential is read, so a document the image step must refuse (too
    // many pages, a page image outside the accepted size) stops here; the
    // backend loads when the first page is rendered. The image step's layout
    // analysis and selections share the offline step's deadline.
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    let image_options = vf_core::images::ImageOptions {
        limits,
        ..vf_core::images::ImageOptions::default()
    };
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    let mut renderer = match &render {
        Some(config) => {
            let renderer = vf_core::images::DocumentRenderer::open(&bytes, config)
                .map_err(|_| (4, "render_unavailable"))?;
            vf_core::images::check_pages(&renderer, &image_options).map_err(image_error)?;
            Some(renderer)
        }
        None => None,
    };
    let remote = match options {
        Some(options) => Some((options, load_secret()?, make_fill_remote()?)),
        None => None,
    };
    let (snapshot, acroform) = vf_source::inspect_with_acroform(&bytes, args.resources.pdf)
        .map_err(|_| (4, "pdf_inspection_failed"))?;
    let result = vf_core::acroform::fill_with_acroform(
        &snapshot.document_id,
        &snapshot.fragments,
        &acroform,
        &form,
        &limits,
    )
    .map_err(|error| (4, error.code()))?;
    let Some((options, secret, (mut transport, mut clock))) = remote else {
        output(sink, &result)?;
        return Ok(0);
    };
    log::debug!(target: "vf_cli", "{}", json!({"event":"fill_assist_config","model":options.model,"config":options.execution,"images":render.is_some()}));
    let assisted = match render {
        None => assist::assist(result, &form, &options, &secret, &mut transport, &mut clock)
            .map_err(|_| (4, "fill_assist_failed"))?,
        #[cfg(any(feature = "pdfium", feature = "hayro"))]
        Some(_) => {
            use vf_core::images::{self, ImageInputs};
            let renderer = renderer.as_mut().ok_or((4, "render_unavailable"))?;
            let inputs = ImageInputs {
                snapshot: &snapshot,
                renderer,
                options: &image_options,
            };
            images::assist_with_images(
                result,
                &form,
                &options,
                inputs,
                &secret,
                &mut transport,
                &mut clock,
            )
            .map_err(image_error)?
        }
        #[cfg(not(any(feature = "pdfium", feature = "hayro")))]
        Some(never) => match never {},
    };
    output(sink, &assisted)?;
    // The full result with every received body is written first; a run in
    // which every field of every step failed then exits 4.
    let mut outcomes = (assisted.assist.values().map(|a| a.outcome.is_failure()))
        .chain(
            assisted
                .image_assist
                .values()
                .map(|a| a.outcome.is_failure()),
        )
        .peekable();
    let all_failed = outcomes.peek().is_some() && outcomes.all(|failed| failed);
    log::info!(target: "vf_cli", "{}", json!({"event":"fill_assist_completed","questions":assisted.assist.len(),"image_fields":assisted.image_assist.len(),"attempts":assisted.usage.attempts,"responses":assisted.jev_responses.len(),"exit_code":if all_failed { 4 } else { 0 }}));
    if all_failed {
        return Err((4, "fill_assist_all_failed"));
    }
    Ok(0)
}
fn run(arguments: Vec<String>) -> Result<u8, Failure> {
    run_with(
        arguments,
        |level, output_file| {
            if level == log::LevelFilter::Off {
                return Ok(());
            }
            // With --output the result goes to the file and logs to stdout.
            let writer: Box<dyn Write + Send> = if output_file {
                Box::new(io::stdout())
            } else {
                Box::new(io::stderr())
            };
            crate::logging::JsonLogger::new("voltfill-cli", level, writer)
                .try_init()
                .map_err(|_| (4, "logging_setup_failed"))
        },
        || {
            settings::secret("VOLTFILL_JEV_API_KEY", "VOLTFILL_JEV_API_KEY_FILE")
                .map_err(|e| (2, e))?
                .ok_or((2, "api_key_required"))
        },
        || {
            let endpoint = settings::endpoint().map_err(|e| (2, e))?;
            let transport = assist::HttpTransport::try_new(endpoint)
                .map_err(|_| (4, "transport_setup_failed"))?;
            Ok((transport, assist::SystemClock::default()))
        },
    )
}
pub(crate) fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.first().is_some_and(|s| s == "serve") {
        std::process::exit(i32::from(crate::serve::entry(&arguments[1..])));
    }
    if arguments.is_empty() || arguments.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return;
    }
    let code = run(arguments).unwrap_or_else(|failure| {
        report_failure(
            &failure,
            log::max_level() != log::LevelFilter::Off,
            &mut io::stderr().lock(),
        );
        failure.code
    });
    std::process::exit(i32::from(code));
}

#[cfg(test)]
mod tests;
