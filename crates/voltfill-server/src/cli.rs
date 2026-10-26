//! Shared HTTP server command launcher.
mod launcher;
pub mod settings;

pub use launcher::{entry, entry_with_resources};
