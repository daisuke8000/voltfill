# Development

English · [日本語](development.md) · [README](../README.md)

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

For renderer features, add `--features hayro` or `--features pdfium` to tests and run configurations sequentially. Native rendering needs separate verification. Use `cargo clean` afterward; automated checks are defined in [CI](../.github/workflows/ci.yml).

`pdf-inspector` uses `ttf-parser 0.25.1`, covered by [maintenance advisory RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html), not a report of a discovered vulnerability. Check official fixes with source-text, geometry and Evidence regressions before adoption.
