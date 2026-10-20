# Development

English · [日本語](development.md) · [README](../README.md)

<a id="checks"></a>

## Build and verify

```sh
cargo build --workspace --locked
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Feature tests: `cargo test --workspace --features hayro --locked` or `cargo test --workspace --features pdfium --locked`. These tests use no PDF files and do not exercise native rendering. Run configurations sequentially because executable paths are shared. Generate Rust API docs with `cargo doc -p voltfill-core --no-deps`; remove local build artifacts after verification with `cargo clean`.

Automated checks are defined in [CI](../.github/workflows/ci.yml).

<a id="dependency-updates"></a>

## Dependency updates

`pdf-inspector` depends on `ttf-parser 0.25.1`, covered by [maintenance advisory RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html). It does not report a discovered vulnerability. Track official font-parser releases and check source text, coordinates and Evidence before adopting relevant fixes.
