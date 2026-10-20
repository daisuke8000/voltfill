//! Shared HTTP server command launcher.
pub mod settings;
use crate::{AppState, ServerConfig};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    net::SocketAddr,
};
use voltfill_core::jev::transport::ExecutionConfig;

const HELP: &str = "voltfill serve --config voltfill.yaml (or voltfill.toml / voltfill.json)\nvoltfill serve --mock FIXTURE.json | --remote\nvoltfill-server remains supported.\n  [--listen 127.0.0.1:3000] [--config CONFIG.json]\n  [--allow-images] [--renderer pdfium|hayro] (no automatic fallback)\n  [--pdfium-lib ABSOLUTE_PATH or VOLTFILL_PDFIUM_LIB_PATH] (selects PDFium when renderer omitted)\n  [--pixels-per-point NUMBER] (default: 1.5)\n  [--log-level off|error|warn|info|debug] (default: off)\n  [--model ID] [--upload-timeout-ms 1..3600000] (default: 30000)\n  [--reuse-document] keeps the last PDF's text and page images in memory for reuse by an identical next request (default: off)\n  [--reuse-document-ttl-ms 1..3600000] drops that copy after this idle time (default: 60000)\n  [--resource-config FILE] lowers PDF/render resource ceilings.\nPrecedence: CLI > selected launch file > environment > defaults.\nVOLTFILL_LISTEN, VOLTFILL_UPLOAD_TIMEOUT_MS, VOLTFILL_REUSE_DOCUMENT_TTL_MS, VOLTFILL_MODEL, VOLTFILL_RENDERER,\nVOLTFILL_PIXELS_PER_POINT and VOLTFILL_LOG_LEVEL provide startup defaults.\nVOLTFILL_EXECUTION_CONFIG and VOLTFILL_RESOURCE_CONFIG select JSON files when\nno corresponding file/section was selected. VOLTFILL_CONFIG selects a launch\nfile for voltfill serve; standalone voltfill-server accepts execution JSON.\n\nPOST /v1/extract: multipart fields pdf, schema, optional options JSON.\noptions.model overrides the startup model (default jev-1.14.0); image strategies require a pinned version >= 1.14.0.\noptions.evaluation_profile optionally enables confidence filtering; omitted by default.\nGET /health: health check. Remote mode requires VOLTFILL_JEV_ENDPOINT (trusted HTTPS)\nand VOLTFILL_JEV_API_KEY or VOLTFILL_JEV_API_KEY_FILE.\nVOLTFILL_SERVER_API_KEY or VOLTFILL_SERVER_API_KEY_FILE enables Bearer authentication and is required\nfor non-loopback binding. Logs are JSONL; responses contain originals.\n";

fn read(path: &str, limit: usize) -> Result<Vec<u8>, &'static str> {
    let file = std::fs::File::open(path).map_err(|_| "input_read_failed")?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input_read_failed")?;
    if bytes.len() > limit {
        return Err("input_too_large");
    }
    Ok(bytes)
}
fn configure_rendering(
    config: &mut ServerConfig,
    values: &BTreeMap<String, String>,
    library: Option<std::path::PathBuf>,
) -> Result<(), &'static str> {
    let _ = &config;
    let renderer = values.get("--renderer").map(String::as_str);
    if renderer.is_some_and(|value| !["pdfium", "hayro"].contains(&value)) {
        return Err("invalid_renderer");
    }
    if renderer == Some("hayro") && values.contains_key("--pdfium-lib") {
        return Err("renderer_library_conflict");
    }
    let renderer = renderer.or(library.as_ref().map(|_| "pdfium"));
    let Some(renderer) = renderer else {
        return if values.contains_key("--pixels-per-point") {
            Err(if cfg!(any(feature = "pdfium", feature = "hayro")) {
                "rendering_required"
            } else {
                "pdfium_feature_disabled"
            })
        } else {
            Ok(())
        };
    };
    if renderer == "hayro" && !cfg!(feature = "hayro") {
        return Err("hayro_feature_disabled");
    }
    if renderer == "pdfium" {
        if !cfg!(feature = "pdfium") {
            return Err("pdfium_feature_disabled");
        }
        if library.is_none() {
            return Err("rendering_required");
        }
    }
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    {
        let mut render = if renderer == "hayro" {
            voltfill_core::render::RenderConfig::hayro()
        } else {
            voltfill_core::render::RenderConfig::new(library.ok_or("rendering_required")?)
        };
        render.pixels_per_point = values
            .get("--pixels-per-point")
            .map(|value| value.parse::<f64>())
            .transpose()
            .map_err(|_| "invalid_render_scale")?
            .unwrap_or(1.5);
        if !render.pixels_per_point.is_finite() || render.pixels_per_point <= 0.0 {
            return Err("invalid_render_scale");
        }
        config.render = Some(render);
    }
    Ok(())
}
fn run(
    arguments: Vec<String>,
    execution: Option<ExecutionConfig>,
    resources: Option<voltfill_core::resources::ResourceLimits>,
) -> Result<(), &'static str> {
    let mut args = arguments.into_iter();
    let (mut values, mut flags) = (BTreeMap::new(), BTreeSet::new());
    while let Some(arg) = args.next() {
        let arg = match arg.as_str() {
            "--remote" => "--allow-remote",
            "--allow-images" => "--allow-preview-images",
            "--config" => "--execution-config",
            _ => &arg,
        }
        .to_owned();
        if [
            "--allow-remote",
            "--exclusive-org",
            "--allow-preview-images",
            "--reuse-document",
        ]
        .contains(&arg.as_str())
        {
            if !flags.insert(arg) {
                return Err("invalid_arguments");
            }
        } else if [
            "--mock",
            "--listen",
            "--execution-config",
            "--renderer",
            "--pdfium-lib",
            "--pixels-per-point",
            "--log-level",
            "--model",
            "--upload-timeout-ms",
            "--reuse-document-ttl-ms",
            "--resource-config",
        ]
        .contains(&arg.as_str())
        {
            let value = args
                .next()
                .filter(|s| !s.starts_with("--"))
                .ok_or("invalid_arguments")?;
            if values.insert(arg, value).is_some() {
                return Err("invalid_arguments");
            }
        } else {
            return Err("invalid_arguments");
        }
    }
    settings::defaults(&mut values, settings::COMMON)?;
    settings::defaults(
        &mut values,
        &[
            ("--listen", "VOLTFILL_LISTEN"),
            ("--upload-timeout-ms", "VOLTFILL_UPLOAD_TIMEOUT_MS"),
            ("--reuse-document-ttl-ms", "VOLTFILL_REUSE_DOCUMENT_TTL_MS"),
        ],
    )?;
    if execution.is_none() {
        settings::defaults(
            &mut values,
            &[("--execution-config", "VOLTFILL_EXECUTION_CONFIG")],
        )?;
    }
    if resources.is_none() {
        settings::defaults(
            &mut values,
            &[("--resource-config", "VOLTFILL_RESOURCE_CONFIG")],
        )?;
    }
    let resource_limits = settings::resources(values.get("--resource-config"), resources)?;
    let level = match values
        .get("--log-level")
        .map(String::as_str)
        .unwrap_or("off")
    {
        "off" => log::LevelFilter::Off,
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "info" => log::LevelFilter::Info,
        "debug" => log::LevelFilter::Debug,
        _ => return Err("invalid_log_level"),
    };
    if level != log::LevelFilter::Off {
        voltfill_core::logging::TelemetryLogger::new("voltfill-server", level, std::io::stdout())
            .try_init()
            .map_err(|_| "logging_setup_failed")?;
    }
    let address: SocketAddr = values
        .get("--listen")
        .map(String::as_str)
        .unwrap_or("127.0.0.1:3000")
        .parse()
        .map_err(|_| "invalid_listen_address")?;
    let mut config = if let Some(path) = values.get("--mock") {
        if flags.contains("--allow-remote")
            || flags.contains("--exclusive-org")
            || flags.contains("--allow-preview-images")
        {
            return Err("mock_remote_conflict");
        }
        ServerConfig::fixture(read(path, 1024 * 1024)?)?
    } else {
        if !flags.contains("--allow-remote") {
            return Err("remote_permission_required");
        }
        let key = settings::secret("VOLTFILL_JEV_API_KEY", "VOLTFILL_JEV_API_KEY_FILE")?
            .ok_or("api_key_required")?;
        ServerConfig::remote(settings::endpoint()?, key)
    };
    config.allow_preview_images = flags.contains("--allow-preview-images");
    config.api_key = settings::secret("VOLTFILL_SERVER_API_KEY", "VOLTFILL_SERVER_API_KEY_FILE")?;
    config.resources = resource_limits;
    if let Some(model) = values.get("--model") {
        config.model = model.clone();
    }
    if let Some(timeout) = values.get("--upload-timeout-ms") {
        config.upload_timeout_ms = timeout.parse().map_err(|_| "invalid_upload_timeout")?;
    }
    config.reuse_document = flags.contains("--reuse-document");
    if let Some(ttl) = values.get("--reuse-document-ttl-ms") {
        config.reuse_document_ttl_ms = ttl.parse().map_err(|_| "invalid_reuse_document_ttl")?;
    }
    if !address.ip().is_loopback() && !config.requires_authentication() {
        return Err("authentication_required_for_non_loopback");
    }
    if let Some(execution) = execution {
        config.execution = execution;
    }
    if let Some(path) = values.get("--execution-config") {
        config.execution = serde_json::from_slice::<ExecutionConfig>(&read(path, 64 * 1024)?)
            .map_err(|_| "invalid_execution_config")?;
    }
    let library = values
        .get("--pdfium-lib")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            if cfg!(feature = "pdfium")
                && values
                    .get("--renderer")
                    .is_none_or(|value| value != "hayro")
            {
                std::env::var_os("VOLTFILL_PDFIUM_LIB_PATH").map(Into::into)
            } else {
                None
            }
        });
    configure_rendering(&mut config, &values, library)?;
    let state = AppState::new(config)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| "runtime_setup_failed")?;
    let result=runtime.block_on(async {
        let listener=tokio::net::TcpListener::bind(address).await.map_err(|_|"listen_failed")?;
        log::info!(target:"voltfill_server","{}",json!({"event":"server_started","address":listener.local_addr().ok().map(|a|a.to_string())}));
        axum::serve(listener,crate::router(state.clone())).with_graceful_shutdown(async {let _=tokio::signal::ctrl_c().await;}).await.map_err(|_|"server_failed")
    });
    drop(state);
    result
}
/// Launch the same HTTP server used by `voltfill serve` and the compatibility binary.
/// Credentials are read only by the remote backend at runtime.
pub fn entry(arguments: Vec<String>, execution: Option<ExecutionConfig>) -> u8 {
    entry_with_resources(arguments, execution, None)
}

/// Launch with resources already decoded from a selected launch file.
pub fn entry_with_resources(
    arguments: Vec<String>,
    execution: Option<ExecutionConfig>,
    resources: Option<voltfill_core::resources::ResourceLimits>,
) -> u8 {
    if arguments.is_empty() || arguments.iter().any(|s| s == "--help" || s == "-h") {
        println!("{HELP}");
        return 0;
    }
    if let Err(code) = run(arguments, execution, resources) {
        if log::max_level() != log::LevelFilter::Off {
            log::error!(target:"voltfill_server","{}",json!({"event":code}));
        } else {
            eprintln!("{}", json!({"error":code}));
        }
        return 2;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ServerConfig {
        ServerConfig::fixture(include_bytes!("../../../examples/text-mock.json").to_vec()).unwrap()
    }
    fn values(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect()
    }

    #[test]
    fn renderer_selection_rejects_conflicts_and_missing_backends() {
        assert_eq!(
            configure_rendering(&mut fixture(), &values(&[("--renderer", "unknown")]), None),
            Err("invalid_renderer")
        );
        assert_eq!(
            configure_rendering(
                &mut fixture(),
                &values(&[("--renderer", "hayro"), ("--pdfium-lib", "/unused")]),
                Some("/unused".into())
            ),
            Err("renderer_library_conflict")
        );
        assert_eq!(
            configure_rendering(&mut fixture(), &values(&[("--renderer", "pdfium")]), None),
            Err(if cfg!(feature = "pdfium") {
                "rendering_required"
            } else {
                "pdfium_feature_disabled"
            })
        );
        let mut config = fixture();
        let result = configure_rendering(
            &mut config,
            &values(&[("--renderer", "hayro"), ("--pixels-per-point", "1.25")]),
            None,
        );
        if cfg!(feature = "hayro") {
            assert!(result.is_ok());
        } else {
            assert_eq!(result, Err("hayro_feature_disabled"));
        }
        #[cfg(feature = "hayro")]
        {
            let render = config.render.unwrap();
            assert_eq!(render.renderer_name(), "hayro");
            assert_eq!(render.pixels_per_point, 1.25);
            assert_eq!(
                configure_rendering(
                    &mut fixture(),
                    &values(&[("--renderer", "hayro"), ("--pixels-per-point", "NaN")]),
                    None
                ),
                Err("invalid_render_scale")
            );
        }
    }

    #[test]
    fn omitted_renderer_preserves_explicit_library_behavior() {
        let mut config = fixture();
        assert!(configure_rendering(&mut config, &BTreeMap::new(), None).is_ok());
        #[cfg(any(feature = "pdfium", feature = "hayro"))]
        assert!(config.render.is_none());
        let result = configure_rendering(
            &mut config,
            &BTreeMap::new(),
            Some("/synthetic-library".into()),
        );
        if cfg!(feature = "pdfium") {
            assert!(result.is_ok());
        } else {
            assert_eq!(result, Err("pdfium_feature_disabled"));
        }
        #[cfg(feature = "pdfium")]
        {
            let render = config.render.unwrap();
            assert_eq!(render.renderer_name(), "pdfium");
            assert_eq!(render.pixels_per_point, 1.5);
        }
    }
}
