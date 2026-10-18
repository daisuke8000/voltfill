#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
fn main() {
    std::process::exit(i32::from(voltfill_server::cli::entry(
        std::env::args().skip(1).collect(),
        None,
    )));
}
