#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
pub mod cli;

mod service;
pub use service::{AppState, ServerConfig, router};
