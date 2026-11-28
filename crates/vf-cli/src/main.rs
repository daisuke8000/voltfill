#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
mod command;
mod logging;
mod serve;
mod settings;
#[cfg(test)]
#[path = "../tests/support/mod.rs"]
mod support;

fn main() {
    command::main();
}
