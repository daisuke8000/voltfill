//! Native-free PDF snapshots. Usable means no observed issue, never verified
//! glyph fidelity, semantic ToUnicode correctness, or absence of overpainting.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
pub mod acroform;
pub mod budget;
pub mod geometry;
pub mod layout;
pub mod metadata;
pub mod select;
mod source;
pub use source::{PageDiagnostic, Quality, SourceFragment};

mod inspection;
pub use inspection::{DocumentSnapshot, InspectionTimings, PdfError, PdfLimits, inspect};
