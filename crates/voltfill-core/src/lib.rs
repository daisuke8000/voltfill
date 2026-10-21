//! Best-effort PDF form extraction with explicit provenance and abstention.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
pub mod candidates;
pub mod engine;
pub mod evaluate;
pub use pdf_probe::geometry;
pub mod jev;
pub mod logging;
pub mod model;
pub mod pdf;
#[cfg(any(feature = "pdfium", feature = "hayro"))]
pub use pdf_render as render;
pub mod resources;
pub mod schema;
pub mod validate;
