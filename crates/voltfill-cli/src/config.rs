//! Explicit launch files; no filesystem discovery, interpolation or credentials.
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    io::Read,
    path::{Path, PathBuf},
};
use voltfill_core::{jev::transport::ExecutionConfig, resources::ResourceLimits};
use voltfill_server::cli::settings;

pub struct Invocation {
    pub arguments: Vec<String>,
    pub execution: Option<ExecutionConfig>,
    pub resources: Option<ResourceLimits>,
    pub source_path: Option<PathBuf>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchConfig {
    #[serde(default = "version")]
    version: u32,
    run: Option<Run>,
    serve: Option<Serve>,
    execution: Option<ExecutionConfig>,
    resources: Option<ResourceLimits>,
}
fn version() -> u32 {
    1
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    pdf: Option<String>,
    schema: Option<String>,
    output: Option<String>,
    mock: Option<String>,
    #[serde(default)]
    remote: bool,
    strategy: Option<String>,
    model: Option<String>,
    renderer: Option<String>,
    pdfium_lib: Option<String>,
    pixels_per_point: Option<f64>,
    evaluation_profile: Option<String>,
    log_level: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Serve {
    mock: Option<String>,
    #[serde(default)]
    remote: bool,
    #[serde(default)]
    allow_images: bool,
    listen: Option<String>,
    model: Option<String>,
    upload_timeout_ms: Option<u64>,
    #[serde(default)]
    reuse_document: bool,
    reuse_document_ttl_ms: Option<u64>,
    renderer: Option<String>,
    pdfium_lib: Option<String>,
    pixels_per_point: Option<f64>,
    log_level: Option<String>,
}
fn canonical(flag: &str) -> &str {
    match flag {
        "-c" => "--config",
        "--allow-remote" => "--remote",
        "--allow-preview-images" => "--allow-images",
        "-o" => "--output",
        _ => flag,
    }
}
/// Resolve paths in the file relative to that file, leaving CLI paths untouched.
fn path(base: &Path, value: String) -> Result<String, &'static str> {
    let p = Path::new(&value);
    if value.is_empty() {
        return Err("invalid_launch_config");
    }
    base.join(p)
        .into_os_string()
        .into_string()
        .map_err(|_| "invalid_launch_config")
}
fn push(args: &mut Vec<String>, explicit: &BTreeSet<String>, flag: &str, value: Option<String>) {
    if let Some(value) = value
        && !explicit.contains(flag)
    {
        args.extend([flag.into(), value]);
    }
}
fn flag(args: &mut Vec<String>, explicit: &BTreeSet<String>, name: &str, enabled: bool) {
    if enabled && !explicit.contains(name) {
        args.push(name.into());
    }
}
pub fn expand(arguments: impl IntoIterator<Item = String>) -> Result<Invocation, &'static str> {
    let mut args: Vec<_> = arguments
        .into_iter()
        .map(|a| if a == "-c" { "--config".into() } else { a })
        .collect();
    if args
        .first()
        .is_some_and(|a| ["run", "extract", "serve"].contains(&a.as_str()))
        && !args.iter().any(|a| a == "--config")
        && let Some(path) = settings::environment("VOLTFILL_CONFIG")?
    {
        args.extend(["--config".into(), path]);
    }
    let positions: Vec<_> = args
        .iter()
        .enumerate()
        .filter(|(_, a)| *a == "--config")
        .map(|(i, _)| i)
        .collect();
    let Some(&position) = positions.first() else {
        return Ok(Invocation {
            arguments: args,
            execution: None,
            resources: None,
            source_path: None,
        });
    };
    if positions.len() != 1 {
        return Err("invalid_arguments");
    }
    let file = args
        .get(position + 1)
        .filter(|p| !p.starts_with('-'))
        .ok_or("invalid_arguments")?;
    let command = args.first().cloned().ok_or("invalid_arguments")?;
    if !["run", "extract", "serve"].contains(&command.as_str()) {
        return Err("invalid_arguments");
    }
    let absolute = std::env::current_dir()
        .map_err(|_| "input_read_failed")?
        .join(file);
    let input = std::fs::File::open(&absolute).map_err(|_| "input_read_failed")?;
    let mut bytes = Vec::new();
    input
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input_read_failed")?;
    if bytes.len() > 64 * 1024 {
        return Err("input_too_large");
    }
    let value: Value = if absolute
        .extension()
        .is_some_and(|ext| ext == "yaml" || ext == "yml")
    {
        let mut budget = serde_saphyr::Budget::default();
        budget.max_depth = 32;
        budget.max_nodes = 4096;
        budget.max_events = 16384;
        budget.max_total_scalar_bytes = 64 * 1024;
        budget.max_documents = 1;
        budget.max_anchors = 0;
        budget.max_aliases = 0;
        let mut options = serde_saphyr::Options::default();
        options.budget = Some(budget);
        options.duplicate_keys = serde_saphyr::DuplicateKeyPolicy::Error;
        options.merge_keys = serde_saphyr::MergeKeyPolicy::Error;
        options.strict_booleans = true;
        options.reject_unsupported_tags = true;
        options.with_snippet = false;
        serde_saphyr::from_str_with_options(
            std::str::from_utf8(&bytes).map_err(|_| "invalid_launch_config")?,
            options,
        )
        .map_err(|_| "invalid_launch_config")?
    } else if absolute.extension().is_some_and(|ext| ext == "toml") {
        toml::from_str(std::str::from_utf8(&bytes).map_err(|_| "invalid_launch_config")?)
            .map_err(|_| "invalid_launch_config")?
    } else {
        // Use the existing duplicate-key/depth checks for JSON configuration too.
        voltfill_core::jev::json::parse(&bytes, voltfill_core::jev::Limits::default())
            .map_err(|_| "invalid_launch_config")?
    };
    let is_launch = value.as_object().is_some_and(|v| {
        ["run", "serve", "execution", "resources", "version"]
            .iter()
            .any(|k| v.contains_key(*k))
    });
    let base = absolute.parent().ok_or("invalid_launch_config")?;
    let execution;
    let resources;
    if is_launch {
        let config: LaunchConfig =
            serde_json::from_value(value).map_err(|_| "invalid_launch_config")?;
        if config.version != 1 {
            return Err("unsupported_launch_config_version");
        }
        execution = config.execution;
        resources = config.resources;
        if let Some(r) = &resources {
            r.validate()?;
        }
        // First determine explicit options, including positional input paths.
        let mut explicit = BTreeSet::new();
        let mut scan = args.iter().skip(1).enumerate().peekable();
        while let Some((i, a)) = scan.next() {
            if i + 1 == position {
                let _ = scan.next();
                continue;
            }
            if a.starts_with('-') {
                let key = canonical(a);
                explicit.insert(key.to_owned());
                if ![
                    "--remote",
                    "--allow-images",
                    "--reuse-document",
                    "--dry-run",
                    "--exclusive-org",
                ]
                .contains(&key)
                {
                    let _ = scan.next();
                }
            } else {
                let keys = if command == "serve" {
                    &[][..]
                } else {
                    &["--pdf", "--schema"][..]
                };
                if let Some(k) = keys.iter().find(|k| !explicit.contains(**k)) {
                    explicit.insert((*k).into());
                }
            }
        }
        args.drain(position..=position + 1);
        if command == "serve" {
            let c = config.serve.ok_or("serve_config_required")?;
            if !c.remote
                && c.mock.is_none()
                && !explicit.contains("--remote")
                && !explicit.contains("--mock")
            {
                return Err("remote_permission_required");
            }
            if c.remote && c.mock.is_some() {
                return Err("mock_remote_conflict");
            }
            if !explicit.contains("--remote") {
                push(
                    &mut args,
                    &explicit,
                    "--mock",
                    c.mock.map(|v| path(base, v)).transpose()?,
                );
            }
            flag(
                &mut args,
                &explicit,
                "--remote",
                c.remote && !explicit.contains("--mock"),
            );
            flag(&mut args, &explicit, "--allow-images", c.allow_images);
            flag(&mut args, &explicit, "--reuse-document", c.reuse_document);
            push(&mut args, &explicit, "--listen", c.listen);
            push(&mut args, &explicit, "--model", c.model);
            push(
                &mut args,
                &explicit,
                "--upload-timeout-ms",
                c.upload_timeout_ms.map(|v| v.to_string()),
            );
            push(
                &mut args,
                &explicit,
                "--reuse-document-ttl-ms",
                c.reuse_document_ttl_ms.map(|v| v.to_string()),
            );
            push(&mut args, &explicit, "--renderer", c.renderer);
            push(
                &mut args,
                &explicit,
                "--pdfium-lib",
                c.pdfium_lib.map(|v| path(base, v)).transpose()?,
            );
            push(
                &mut args,
                &explicit,
                "--pixels-per-point",
                c.pixels_per_point.map(|v| v.to_string()),
            );
            push(&mut args, &explicit, "--log-level", c.log_level);
        } else {
            let c = config.run.ok_or("run_config_required")?;
            if c.remote && c.mock.is_some() {
                return Err("mock_remote_conflict");
            }
            for (key, value) in [
                ("--pdf", c.pdf),
                ("--schema", c.schema),
                ("--output", c.output),
            ] {
                push(
                    &mut args,
                    &explicit,
                    key,
                    value.map(|v| path(base, v)).transpose()?,
                );
            }
            if !explicit.contains("--remote") {
                push(
                    &mut args,
                    &explicit,
                    "--mock",
                    c.mock.map(|v| path(base, v)).transpose()?,
                );
            }
            flag(
                &mut args,
                &explicit,
                "--remote",
                c.remote && !explicit.contains("--mock"),
            );
            for (key, value) in [
                ("--strategy", c.strategy),
                ("--model", c.model),
                ("--renderer", c.renderer),
                ("--evaluation-profile", c.evaluation_profile),
                ("--log-level", c.log_level),
            ] {
                push(&mut args, &explicit, key, value);
            }
            push(
                &mut args,
                &explicit,
                "--pdfium-lib",
                c.pdfium_lib.map(|v| path(base, v)).transpose()?,
            );
            push(
                &mut args,
                &explicit,
                "--pixels-per-point",
                c.pixels_per_point.map(|v| v.to_string()),
            );
        }
    } else {
        resources = None;
        execution = Some(serde_json::from_value(value).map_err(|_| "invalid_execution_config")?);
        args.drain(position..=position + 1);
    }
    if command == "serve" && args.len() == 1 {
        return Err("remote_permission_required");
    }
    if let Some(e) = &execution {
        e.validate().map_err(|_| "invalid_execution_config")?;
    }
    Ok(Invocation {
        arguments: args,
        execution,
        resources,
        source_path: Some(absolute),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_reuse_keys_expand_to_the_server_flags() {
        let path = std::env::temp_dir().join(format!("voltfill-reuse-{}.yaml", std::process::id()));
        std::fs::write(
            &path,
            "serve:\n  mock: mock.json\n  reuse_document: true\n  reuse_document_ttl_ms: 1234\n",
        )
        .unwrap();
        let invocation = expand([
            "serve".into(),
            "--config".into(),
            path.to_str().unwrap().into(),
        ]);
        std::fs::remove_file(&path).unwrap();
        let arguments = invocation.unwrap().arguments;
        assert!(arguments.contains(&"--reuse-document".to_owned()));
        assert!(
            arguments
                .windows(2)
                .any(|w| w == ["--reuse-document-ttl-ms", "1234"])
        );
    }
}
