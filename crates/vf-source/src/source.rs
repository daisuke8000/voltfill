use crate::geometry::Rect;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    Usable,
    Unknown,
    Suspect,
}

/// Which way a fragment's text reads on the rendered (Display) page: its
/// baseline turned counter-clockwise from left-to-right by a right angle.
/// Serialized as `deg0`, `deg90`, `deg180`, `deg270`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Orientation {
    /// Left to right: ordinary horizontal text.
    #[default]
    Deg0,
    /// Bottom to top, glyph tops facing left.
    Deg90,
    /// Right to left, upside down.
    Deg180,
    /// Top to bottom, glyph tops facing right.
    Deg270,
}

impl Orientation {
    /// Whether the text reads left to right.
    #[must_use]
    pub fn is_upright(&self) -> bool {
        *self == Self::Deg0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceFragment {
    pub id: String,
    pub page: u32,
    pub item_index: usize,
    pub raw_text: String,
    /// Display coordinates, whatever the orientation.
    pub bbox: Rect,
    pub quality: Quality,
    /// Omitted from JSON for upright text, so upright fragments serialize as
    /// before orientation existed.
    #[serde(skip_serializing_if = "Orientation::is_upright")]
    pub orientation: Orientation,
}

#[derive(Debug, Clone, Serialize)]
pub struct PageDiagnostic {
    pub page: u32,
    pub text_available: bool,
    pub quality: Quality,
    pub reason_codes: Vec<String>,
}
