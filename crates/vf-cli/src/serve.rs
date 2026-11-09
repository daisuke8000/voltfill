//! `voltfill serve`: argument parsing and startup. The server never sends to Jev.
use crate::settings;
use serde_json::json;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use vf_server::{AppState, ServerConfig};

const HELP: &str =
    "voltfill serve [--listen 127.0.0.1:3000] [--upload-timeout-ms 1..3600000] (default: 30000)
  [--log-level off|error|warn|info|debug] (default: off) [--resource-config FILE]
VOLTFILL_LISTEN, VOLTFILL_UPLOAD_TIMEOUT_MS, VOLTFILL_LOG_LEVEL and VOLTFILL_RESOURCE_CONFIG
provide defaults; flags take precedence.

POST /v1/fill: multipart parts pdf and form (the voltfill fill form JSON); matches form
labels to the PDF text offline and never sends to Jev. It applies voltfill fill's work
limits and a fixed 30 s deadline from the job start (PDF inspection included, not
interrupted); over a limit it answers an error, never a partial result. One job runs
at a time; another request is answered 429.
GET /health: health check.
VOLTFILL_SERVER_API_KEY or VOLTFILL_SERVER_API_KEY_FILE enables Bearer authentication and
is required for non-loopback binding. Logs are JSON Lines on stdout.
";

fn run(arguments: &[String]) -> Result<(), &'static str> {
    const OPTIONS: [(&str, &str); 4] = [
        ("--listen", "VOLTFILL_LISTEN"),
        ("--upload-timeout-ms", "VOLTFILL_UPLOAD_TIMEOUT_MS"),
        ("--log-level", "VOLTFILL_LOG_LEVEL"),
        ("--resource-config", "VOLTFILL_RESOURCE_CONFIG"),
    ];
    let mut values = BTreeMap::new();
    for pair in arguments.chunks(2) {
        let [flag, value] = pair else {
            return Err("invalid_arguments");
        };
        if !OPTIONS.iter().any(|(f, _)| f == flag)
            || value.starts_with("--")
            || values.insert(flag.clone(), value.clone()).is_some()
        {
            return Err("invalid_arguments");
        }
    }
    settings::defaults(&mut values, &OPTIONS)?;
    let get = |flag: &str| values.get(flag).map(String::as_str);
    let level = crate::logging::level(get("--log-level")).ok_or("invalid_log_level")?;
    if level != log::LevelFilter::Off {
        crate::logging::JsonLogger::new("voltfill-server", level, std::io::stdout())
            .try_init()
            .map_err(|_| "logging_setup_failed")?;
    }
    let resources = settings::resources(get("--resource-config").map(std::path::Path::new))?;
    let address: SocketAddr = get("--listen")
        .unwrap_or("127.0.0.1:3000")
        .parse()
        .map_err(|_| "invalid_listen_address")?;
    let mut config = ServerConfig::default();
    config.pdf = resources.pdf;
    config.api_key = settings::secret("VOLTFILL_SERVER_API_KEY", "VOLTFILL_SERVER_API_KEY_FILE")?;
    if let Some(timeout) = get("--upload-timeout-ms") {
        config.upload_timeout_ms = timeout.parse().map_err(|_| "invalid_upload_timeout")?;
    }
    if !address.ip().is_loopback() && config.api_key.is_none() {
        return Err("authentication_required_for_non_loopback");
    }
    let state = AppState::try_new(config)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| "runtime_setup_failed")?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|_| "listen_failed")?;
        log::info!(target:"vf_server","{}",json!({"event":"server_started","address":listener.local_addr().ok().map(|a|a.to_string())}));
        axum::serve(listener, vf_server::router(state))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|_| "server_failed")
    })
}

/// Launch the HTTP server for `voltfill serve`; the exit code.
#[must_use]
pub(crate) fn entry(arguments: &[String]) -> u8 {
    if arguments.iter().any(|s| s == "--help" || s == "-h") {
        println!("{HELP}");
        return 0;
    }
    let Err(code) = run(arguments) else {
        return 0;
    };
    if log::max_level() != log::LevelFilter::Off {
        log::error!(target:"vf_server","{}",json!({"event":code}));
    } else {
        eprintln!("{}", json!({"error":code}));
    }
    2
}
