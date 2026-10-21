use crate::geometry::{ImageTransform, Rect};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Extracted,
    NotFound,
    Ambiguous,
    Unsupported,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    TextFirst,
    VisionFirst,
    Hybrid,
}

pub use pdf_probe::{Quality, SourceFragment};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceSpan {
    pub fragment_id: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub id: String,
    pub page: u32,
    pub spans: Vec<SourceSpan>,
    pub bbox: Rect,
}

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub id: String,
    pub field_id: String,
    pub page: u32,
    pub candidate_id: Option<String>,
    pub spans: Vec<SourceSpan>,
    pub raw_fragments: Vec<String>,
    pub display_bbox: Rect,
    pub bbox_precision: &'static str,
    pub question_id: String,
    pub strategy: Strategy,
    pub image_id: Option<String>,
    pub image_transform: Option<ImageTransform>,
}

#[derive(Debug, Serialize)]
pub struct FieldResult {
    pub field_id: String,
    pub status: Status,
    pub value: Option<Value>,
    pub value_origin: &'static str,
    pub evidence_ids: Vec<String>,
    pub reason_codes: Vec<String>,
}

impl FieldResult {
    pub(crate) fn pending(id: &str, status: Status, reason: &str) -> Self {
        debug_assert!(status != Status::Extracted);
        Self {
            field_id: id.into(),
            status,
            value: None,
            value_origin: "source_text",
            evidence_ids: Vec::new(),
            reason_codes: vec![reason.into()],
        }
    }
}

pub use pdf_probe::PageDiagnostic;

#[derive(Debug, Clone, Default, Serialize)]
pub struct Timings {
    pub metadata_ms: f64,
    pub positions_ms: f64,
    pub quality_ms: f64,
    pub render_ms: f64,
    pub candidates_ms: f64,
    pub evaluation_ms: f64,
    pub resolve_ms: f64,
    pub total_ms: f64,
    pub parser_load_calls: u32,
    pub render_calls: u32,
    pub requests: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct UsageSummary {
    pub known_input_tokens: u64,
    pub known_output_tokens: u64,
    pub unknown_usage_attempts: u32,
    pub attempts: u32,
    /// Saturated totals are lower bounds, not exact usage.
    pub totals_overflowed: bool,
}
impl UsageSummary {
    pub fn add(&mut self, other: &Self) -> bool {
        macro_rules! add {
            ($field:ident) => {
                match self.$field.checked_add(other.$field) {
                    Some(value) => self.$field = value,
                    None => {
                        self.$field = self.$field.saturating_add(other.$field);
                        self.totals_overflowed = true;
                    }
                }
            };
        }
        add!(known_input_tokens);
        add!(known_output_tokens);
        add!(unknown_usage_attempts);
        add!(attempts);
        self.totals_overflowed |= other.totals_overflowed;
        !self.totals_overflowed
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ResponseBody {
    Text(String),
    Bytes(Vec<u8>),
}

impl From<Vec<u8>> for ResponseBody {
    fn from(bytes: Vec<u8>) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Self::Text(text),
            Err(error) => Self::Bytes(error.into_bytes()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct JevResponse {
    pub field_id: String,
    pub page: u32,
    pub question_ids: Vec<String>,
    pub http_status: Option<u16>,
    pub retry_after_ms: Option<u64>,
    pub transport_error: Option<crate::jev::transport::TransportError>,
    /// Original response body, before parsing/validation. Non-UTF-8 uses bytes.
    pub body: Option<ResponseBody>,
    /// False for an interrupted/truncated body or an attempt with no received body.
    pub body_complete: bool,
}

#[derive(Debug, Serialize)]
pub struct ExtractionReport {
    pub run_id: String,
    pub configuration_id: String,
    pub evaluation_profile: Option<Value>,
    pub schema_version: u32,
    pub evaluation_only: bool,
    pub document_id: String,
    pub form_id: String,
    /// Requested model ID or alias; received bodies retain each resolved model ID.
    pub model: String,
    pub strategy: Strategy,
    pub fields: Vec<FieldResult>,
    pub evidence: Vec<Evidence>,
    pub pages: Vec<PageDiagnostic>,
    pub timings: Timings,
    pub usage: UsageSummary,
    pub evaluation_backend: String,
    pub jev_responses: Vec<JevResponse>,
}
