//! Environment defaults for application launchers. Core APIs use explicit values.
use std::collections::BTreeMap;
use std::path::Path;
use voltfill_core::jev::http::Endpoint;
use voltfill_core::jev::transport::Secret;
use voltfill_core::resources::ResourceLimits;

pub const COMMON: &[(&str, &str)] = &[
    ("--log-level", "VOLTFILL_LOG_LEVEL"),
    ("--renderer", "VOLTFILL_RENDERER"),
    ("--pixels-per-point", "VOLTFILL_PIXELS_PER_POINT"),
    ("--model", "VOLTFILL_MODEL"),
];

/// Read only the named variable. Never include its value in errors.
pub fn environment(name: &str) -> Result<Option<String>, &'static str> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() && value.len() <= 4096 => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotUnicode(_)) => Err("invalid_environment_setting"),
        Err(std::env::VarError::NotPresent) => Ok(None),
    }
}
pub fn defaults(
    values: &mut BTreeMap<String, String>,
    settings: &[(&str, &str)],
) -> Result<(), &'static str> {
    for &(flag, variable) in settings {
        if !values.contains_key(flag)
            && let Some(value) = environment(variable)?
        {
            values.insert(flag.into(), value);
        }
    }
    Ok(())
}
pub fn endpoint() -> Result<Endpoint, &'static str> {
    let value = environment("VOLTFILL_JEV_ENDPOINT")?.ok_or("jev_endpoint_required")?;
    Endpoint::try_new(&value).map_err(|_| "invalid_jev_endpoint")
}

/// Runtime-only credential injection from a value or one explicitly named file.
pub fn secret(name: &str, file_name: &str) -> Result<Option<Secret>, &'static str> {
    let value = std::env::var_os(name);
    let file = std::env::var_os(file_name);
    let value = match (value, file) {
        (Some(_), Some(_)) => return Err("credential_sources_conflict"),
        (None, None) => return Ok(None),
        (Some(value), None) => value.into_string().map_err(|_| "invalid_credential")?,
        (None, Some(path)) => {
            if path.is_empty() {
                return Err("invalid_credential_file");
            }
            let bytes = read_file(std::path::Path::new(&path), 8192)
                .map_err(|_| "credential_read_failed")?;
            let mut value = String::from_utf8(bytes).map_err(|_| "invalid_credential")?;
            if value.ends_with('\n') {
                value.pop();
                if value.ends_with('\r') {
                    value.pop();
                }
            }
            value
        }
    };
    if value.len() > 8192 {
        return Err("invalid_credential");
    }
    Secret::try_new(value)
        .map(Some)
        .map_err(|_| "invalid_credential")
}
fn read_file(path: &std::path::Path, limit: usize) -> Result<Vec<u8>, &'static str> {
    use std::io::Read;

    if !std::fs::metadata(path)
        .map_err(|_| "input_read_failed")?
        .is_file()
    {
        return Err("input_read_failed");
    }
    let file = std::fs::File::open(path).map_err(|_| "input_read_failed")?;
    if !file.metadata().map_err(|_| "input_read_failed")?.is_file() {
        return Err("input_read_failed");
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input_read_failed")?;
    if bytes.len() > limit {
        return Err("input_too_large");
    }
    Ok(bytes)
}
/// Compatibility entry point for callers holding an optional owned path string.
/// New callers can pass any borrowed path to [`resources_from_path`].
pub fn resources(
    path: Option<&String>,
    configured: Option<ResourceLimits>,
) -> Result<ResourceLimits, &'static str> {
    resources_from_path(path.map(Path::new), configured)
}

/// Load an explicitly selected JSON file, otherwise validate configured or default limits.
/// File size is capped at 64 KiB; configured limits cannot exceed supported ceilings.
pub fn resources_from_path(
    path: Option<&Path>,
    configured: Option<ResourceLimits>,
) -> Result<ResourceLimits, &'static str> {
    let limits = if let Some(path) = path {
        let bytes = read_file(path, 64 * 1024)?;
        let value = voltfill_core::jev::json::parse(&bytes, voltfill_core::jev::Limits::default())
            .map_err(|_| "invalid_resource_limits")?;
        serde_json::from_value(value).map_err(|_| "invalid_resource_limits")?
    } else {
        configured.unwrap_or_default()
    };
    limits.validate()?;
    Ok(limits)
}
