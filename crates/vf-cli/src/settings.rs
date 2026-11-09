//! Environment defaults, caller-injected credentials and resource limits.
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;
use vf_core::assist::{Endpoint, Secret};
use vf_source::PdfLimits;

/// Read only the named variable. Never include its value in errors.
pub fn environment(name: &str) -> Result<Option<String>, &'static str> {
    match std::env::var(name) {
        Ok(value) if !value.is_empty() && value.len() <= 4096 => Ok(Some(value)),
        Ok(_) | Err(std::env::VarError::NotUnicode(_)) => Err("invalid_environment_setting"),
        Err(std::env::VarError::NotPresent) => Ok(None),
    }
}
/// Fill each `(flag, variable)` not given on the command line from the environment.
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
    let value = match (std::env::var_os(name), std::env::var_os(file_name)) {
        (Some(_), Some(_)) => return Err("credential_sources_conflict"),
        (None, None) => return Ok(None),
        (Some(value), None) => value.into_string().map_err(|_| "invalid_credential")?,
        (None, Some(path)) if path.is_empty() => return Err("invalid_credential_file"),
        (None, Some(path)) => {
            let bytes = read_file(Path::new(&path), 8192).map_err(|_| "credential_read_failed")?;
            let value = String::from_utf8(bytes).map_err(|_| "invalid_credential")?;
            let value = value.strip_suffix('\n').unwrap_or(&value);
            value.strip_suffix('\r').unwrap_or(value).to_owned()
        }
    };
    if value.len() > 8192 {
        return Err("invalid_credential");
    }
    Secret::try_new(value)
        .map(Some)
        .map_err(|_| "invalid_credential")
}
/// Read a regular file of at most `limit` bytes.
pub fn read_file(path: &Path, limit: usize) -> Result<Vec<u8>, &'static str> {
    // Checked before opening, so a FIFO or device is never opened.
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
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

/// Deployment limits may lower the supported ceilings; zero is allowed only for
/// recovery pages. These ceilings do not sandbox PDF parsers.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceLimits {
    pub pdf: PdfLimits,
    pub max_render_pixels: u64,
    pub max_render_output_bytes: u64,
}
impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            pdf: PdfLimits::default(),
            max_render_pixels: 16_000_000,
            max_render_output_bytes: 64 * 1024 * 1024,
        }
    }
}
impl ResourceLimits {
    fn validate(&self) -> Result<(), &'static str> {
        let (c, pdf) = (Self::default(), &self.pdf);
        let valid = (1..=c.pdf.max_bytes).contains(&pdf.max_bytes)
            && (1..=c.pdf.max_pages).contains(&pdf.max_pages)
            && (1..=c.pdf.max_items_per_page).contains(&pdf.max_items_per_page)
            && pdf.max_recovery_pages <= c.pdf.max_recovery_pages
            && (1..=c.max_render_pixels).contains(&self.max_render_pixels)
            && (1..=c.max_render_output_bytes).contains(&self.max_render_output_bytes);
        if valid {
            Ok(())
        } else {
            Err("invalid_resource_limits")
        }
    }
    #[cfg(any(feature = "pdfium", feature = "hayro"))]
    pub fn apply_render(&self, render: &mut vf_core::images::RenderConfig) {
        render.max_input_bytes = render.max_input_bytes.min(self.pdf.max_bytes);
        render.max_pages = render.max_pages.min(self.pdf.max_pages);
        render.max_pixels = render.max_pixels.min(self.max_render_pixels);
        render.max_output_bytes = render.max_output_bytes.min(self.max_render_output_bytes);
    }
}
/// The limits in an explicitly selected JSON file (at most 64 KiB, duplicate keys
/// rejected), or the defaults.
pub fn resources(path: Option<&Path>) -> Result<ResourceLimits, &'static str> {
    let Some(path) = path else {
        return Ok(ResourceLimits::default());
    };
    let bytes = read_file(path, 64 * 1024)?;
    let value = vf_client::json::parse(&bytes, vf_client::Limits::default())
        .map_err(|_| "invalid_resource_limits")?;
    let limits: ResourceLimits =
        serde_json::from_value(value).map_err(|_| "invalid_resource_limits")?;
    limits.validate()?;
    Ok(limits)
}
