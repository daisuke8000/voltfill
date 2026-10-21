//! Optional PDF rendering, independent of extraction schemas and Evidence.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#[cfg(any(feature = "pdfium", feature = "hayro"))]
mod render;
#[cfg(any(feature = "pdfium", feature = "hayro"))]
pub use render::*;
