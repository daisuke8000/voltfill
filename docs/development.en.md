# Development

English · [日本語](development.md) · [README](../README.md)

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

For renderer features, add `--features hayro` or `--features pdfium` to tests and run configurations sequentially. Native rendering needs separate verification. Use `cargo clean` afterward; automated checks are defined in [CI](../.github/workflows/ci.yml).

CI runs Rust formatting, workspace checks, Clippy and tests on pull requests and pushes to `main`. PDFium tests are compiled only; CI does not download a native library. Security checks also run daily and on manual dispatch: Cargo.lock advisory checks with `cargo-audit`, redacted committed-history secret scanning with Gitleaks, and workflow validation with [actionlint](https://github.com/rhysd/actionlint) and [zizmor](https://docs.zizmor.sh/usage/). The advisory check runs on every PR even when dependencies are unchanged. Zizmor runs offline, without GitHub API audits. Findings fail their job; maintenance warnings remain visible without being treated as confirmed vulnerabilities. Pages deployment requires all check jobs to pass. Dependabot proposes weekly Cargo and GitHub Actions updates.

`pdf-inspector` uses `ttf-parser 0.25.1`, covered by [maintenance advisory RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html), not a report of a discovered vulnerability. Check official fixes with source-text, geometry and Evidence regressions before adoption.
