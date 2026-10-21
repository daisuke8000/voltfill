use crate::geometry::Rect;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Usable,
    Unknown,
    Suspect,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceFragment {
    pub id: String,
    pub page: u32,
    pub item_index: usize,
    pub raw_text: String,
    pub bbox: Rect,
    pub quality: Quality,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageDiagnostic {
    pub page: u32,
    pub text_available: bool,
    pub quality: Quality,
    pub reason_codes: Vec<String>,
}
