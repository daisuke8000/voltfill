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
use vf_core::assist;
use vf_legacy::engine::{self, EvaluationProfile, ExtractOptions};
use vf_legacy::evaluate::{Evaluator, FixtureEvaluator, RemoteEvaluator};
use vf_legacy::jev::transport::{ExecutionConfig, Secret};
use vf_legacy::model::{JevResponse, Strategy};
use vf_legacy::{pdf, schema};

const HELP: &str = "VOLTFILL — PDF extraction\n\nvoltfill run --config voltfill.yaml (or voltfill.toml / voltfill.json)\nvoltfill serve --config voltfill.yaml\nvoltfill serve --help\nvoltfill run INPUT.pdf SCHEMA.json --remote\nvoltfill extract INPUT.pdf SCHEMA.json --remote\nvoltfill extract INPUT.pdf SCHEMA.json --mock ANSWERS.json\nvoltfill inspect INPUT.pdf\nvoltfill fill INPUT.pdf FORM.json (offline: match form labels to PDF text)\nvoltfill fill INPUT.pdf FORM.json --remote (Jev chooses among ambiguous fields' source candidates)\nvoltfill validate SCHEMA.json\n\n--strategy text|vision|hybrid (default: text)\n--model ID (default: jev-1.14.0); images require a pinned version >= 1.14.0.\n--renderer pdfium|hayro selects an enabled renderer; no automatic fallback.\n--pdfium-lib PATH or VOLTFILL_PDFIUM_LIB_PATH selects PDFium when --renderer is omitted\n--pixels-per-point NUMBER sets render scale (default: 1.5).\n--config FILE (or -c FILE, default VOLTFILL_CONFIG) loads YAML/TOML launch settings or legacy JSON execution limits.\nPrecedence: CLI > selected file > environment > defaults.\n--resource-config FILE (or VOLTFILL_RESOURCE_CONFIG) lowers PDF/render ceilings.\nVOLTFILL_EXECUTION_CONFIG selects execution JSON when no execution section/file is supplied.\nVOLTFILL_MODEL, VOLTFILL_STRATEGY, VOLTFILL_RENDERER, VOLTFILL_PIXELS_PER_POINT\nand VOLTFILL_LOG_LEVEL provide startup defaults.\n--max-concurrency N (default: 2, at least 1); rate limits may reduce concurrency.\n--max-attempts 1|2|3 includes the first attempt (default: 3).\n--log-level off|error|warn|info|debug (default: off) enables JSON Lines logs.\nLogs use stderr with JSON on stdout; with --output FILE (or -o FILE),\nlogs use stdout and results go to the file. Debug logs cover VOLTFILL only.\n--dry-run inspects the input without sending requests.\n--evaluation-profile synthetic enables confidence filtering for local synthetic examples.\nBy default, resolved values are returned without a confidence cutoff.\n\nRemote mode requires VOLTFILL_JEV_ENDPOINT (trusted HTTPS, no URL fallback) and\nVOLTFILL_JEV_API_KEY or VOLTFILL_JEV_API_KEY_FILE (one explicit credential file). Vision/Hybrid select image input;\nno additional image permission flag is required. Reports retain original\nresponse bodies regardless of log level and remain evaluation-only.\nLong-form --pdf, --schema, --allow-remote and --execution-config are also supported.\n\nfill --remote sends only fields left ambiguous, as choices among their own source\ncandidates (label and value text, no images); values still come from the PDF.\nIt accepts --model, --max-attempts and --execution-config FILE (not --config);\nbatches are sent one at a time. If every sent field fails, the full result is\nstill written and the exit code is 4. Without --remote, fill reads no credentials.\n";

struct Args {
    command: String,
    values: BTreeMap<String, String>,
    flags: BTreeSet<String>,
    library_path: Option<PathBuf>,
    execution: Option<ExecutionConfig>,
    resources: vf_legacy::resources::ResourceLimits,
    launch_path: Option<PathBuf>,
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
/// Refuse a symlink output or one that would overwrite an input, a launch,
/// execution or resource file, a native library or the API key file.
/// `run_with` checks this before reading any input or credential and before
/// any request, so a refused target never discards received bodies;
/// [`output`] checks again in case the target changed during the run.
fn check_output_target(args: &Args) -> Result<(), (u8, &'static str)> {
    let Some(path) = args.values.get("--output") else {
        return Ok(());
    };
    let target = Path::new(path);
    if fs::symlink_metadata(target).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err((5, "symlink_output_refused"));
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
    // Only the key file's path is compared; its contents are not read here.
    if args.flag("--allow-remote")
        && !args.flag("--dry-run")
        && std::env::var_os("VOLTFILL_JEV_API_KEY_FILE").is_some_and(|path| {
            canonical.is_some() && Path::new(&path).canonicalize().ok() == canonical
        })
    {
        return Err((5, "input_overwrite_refused"));
    }
    Ok(())
}
fn output(args: &Args, value: &impl serde::Serialize) -> Result<(), (u8, &'static str)> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|_| (5, "output_serialization_failed"))?;
    bytes.push(b'\n');
    if let Some(path) = args.values.get("--output") {
        check_output_target(args)?;
        write_atomic(Path::new(path), &bytes).map_err(|_| (5, "output_write_failed"))?;
    } else {
        io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|_| (5, "output_write_failed"))?;
    }
    Ok(())
}
fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = None;
    for n in 0..100 {
        let candidate = parent.join(format!(".voltfill-{}-{n}.tmp", std::process::id()));
        let mut open = OpenOptions::new();
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        match open.open(&candidate) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    let (temporary, mut file) = temporary.ok_or_else(|| {
        io::Error::new(io::ErrorKind::AlreadyExists, "temporary output unavailable")
    })?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
#[derive(serde::Serialize)]
struct ExtractionError<'response> {
    error: &'static str,
    jev_responses: &'response [JevResponse],
}
fn execute<E: Evaluator>(
    args: &Args,
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
                    args,
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
    output(args, &report)?;
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
) -> Result<u8, (u8, &'static str)> {
    let args = parse_args(arguments)?;
    let level = log_level(&args)?;
    let execution = execution_config(&args)?;
    initialize_logger(level, args.values.contains_key("--output"))?;
    check_output_target(&args)?;
    if args.command == "validate" {
        let form = schema::parse(&read(args.value("--schema")?, schema::MAX_SCHEMA_BYTES)?)
            .map_err(|_| (2, "invalid_schema"))?;
        output(
            &args,
            &json!({"valid":true,"schema_version":form.schema_version,"field_count":form.fields.len()}),
        )?;
        return Ok(0);
    }
    if args.command == "fill" {
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
        let result = vf_core::fill::fill(&snapshot.document_id, &snapshot.fragments, &form);
        let Some((options, secret, (mut transport, mut clock))) = remote else {
            output(&args, &result)?;
            return Ok(0);
        };
        log::debug!(target: "vf_cli", "{}", json!({"event":"fill_assist_config","model":options.model,"config":options.execution}));
        let assisted = assist::assist(result, &form, &options, &secret, &mut transport, &mut clock)
            .map_err(|_| (4, "fill_assist_failed"))?;
        output(&args, &assisted)?;
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
        output(&args, &snapshot)?;
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
    .with_execution_config(&execution)
    .map_err(|_| (2, "invalid_execution_config"))?;
    configure_rendering(&mut options, &args)?;
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
            &args,
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
        execute(&args, &bytes, &form, &options, evaluator)
    } else {
        if !args.flag("--allow-remote") {
            return Err((2, "remote_permission_required"));
        }
        let secret = load_secret()?;
        let evaluator = make_remote(&execution, secret, image_input)?;
        execute(&args, &bytes, &form, &options, evaluator)
    }
}
fn run(arguments: Vec<String>) -> Result<u8, (u8, &'static str)> {
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
        Err((code, reason)) => {
            if log::max_level() != log::LevelFilter::Off {
                log::error!(target: "vf_cli", "{}", json!({"event":reason}));
            } else {
                eprintln!("{}", json!({"error":reason}));
            }
            code
        }
    };
    std::process::exit(i32::from(code));
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

    #[test]
    fn fill_remote_refuses_the_output_target_before_credentials_or_sends() {
        let (dir, pdf, form) = fill_inputs("fill-remote-preflight");
        let config = dir.join("execution.json");
        fs::write(&config, b"{}").expect("the config must be writable");
        let link = dir.join("link.json");
        #[cfg(unix)]
        std::os::unix::fs::symlink(dir.join("elsewhere.json"), &link)
            .expect("the symlink must be creatable");
        let mut cases = vec![
            (form.clone(), Vec::new(), "input_overwrite_refused"),
            (pdf.clone(), Vec::new(), "input_overwrite_refused"),
            (
                config.clone(),
                vec!["--execution-config".to_owned(), arg(&config)],
                "input_overwrite_refused",
            ),
        ];
        if cfg!(unix) {
            cases.push((link.clone(), Vec::new(), "symlink_output_refused"));
        }
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
            assert_eq!(result, Err((5, reason)), "{target:?}");
            assert_eq!(sends.load(Ordering::SeqCst), 0, "{target:?}");
            assert_eq!(secret_loads.get(), 0, "{target:?}");
            assert_eq!(transports.get(), 0, "{target:?}");
        }
        for (path, original) in [&form, &pdf, &config].into_iter().zip(originals) {
            assert_eq!(fs::read(path).expect("the input is readable"), original);
        }
        assert!(!dir.join("elsewhere.json").exists());
        fs::remove_dir_all(&dir).ok();
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
        assert_eq!(result, Err((4, "fill_assist_all_failed")));
        assert_eq!(sends.load(Ordering::SeqCst), 2);
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
