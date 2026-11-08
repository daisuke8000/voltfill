#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
mod command;
mod config;
mod logging;
mod serve;
mod settings;

fn main() {
    command::main();
}
