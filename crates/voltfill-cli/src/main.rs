#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
mod command;
mod config;

fn main() {
    command::main();
}
