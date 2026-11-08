#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
mod service;
pub use service::{AppState, ServerConfig, router};
