# VOLTFILL agent instructions

Read README.md and the relevant source/tests before changing behavior. Keep work within the user's requested scope. Verify the actual workspace, toolchain and effective permissions; do not infer current execution results from fixtures.

- Use the development toolchain pinned in rust-toolchain.toml. Preserve the MSRV and Edition declared in Cargo.toml unless explicitly asked to change them.
- Keep schema, PDF/source geometry, evaluation, transport, resolution and output contracts distinct. Preserve original Evidence and every received Jev attempt body, including error paths; logs do not replace response data.
- Keep all reports evaluation-only. Do not describe acceptance thresholds as calibrated probabilities or mock results as live accuracy/performance.
- Validate inputs and bound requests, retries, concurrency, token/cost reservations and deadlines. Apply the configured limits across workers and retries.
- Do not read or print credentials, enumerate environment secrets, explore other projects, escalate privileges, push Git or send Jev requests without explicit authorization. Runtime code may use caller-injected credentials only for its documented purpose. Never log bodies, credentials or HTTP headers.
- Native rendering is optional and requires an explicit caller-supplied library path. Do not add automatic native downloads or ambient library searches.
- Use targeted tests for behavioral changes, then required workspace checks. Typical checks: `cargo fmt --all -- --check`, `cargo check --workspace --all-targets --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`. Exercise feature-specific changes with the corresponding feature and explicitly supplied runtime dependencies.
- After final verification, remove disposable build output with `cargo clean`. Preserve toolchains, dependency downloads and caller-supplied native libraries. Run default and PDFium test configurations sequentially because they share executable paths.
- Keep the Japanese and English READMEs and architecture HTML pages consistent and accurate when their documented behavior changes. State validation limits honestly; do not invent successful checks or measurements.
- Keep public documentation focused on usage, supported behavior and architecture. Exclude account-specific settings, local environment details, investigation reports and conversation history. Use system fonts without external font assets.
- OCR is outside scope. Do not add OCR code or dependencies.
- Track `ttf-parser` maintenance advisory RUSTSEC-2026-0192 through `pdf-inspector`. Review official releases and adopt compatible fixes after text, geometry and Evidence regression checks. Do not suppress the advisory or equate an unmaintained warning with a confirmed vulnerability. Prefer upstream fixes over a permanent fork; any temporary patch must state its scope, downstream Cargo requirements and removal condition, and be removed when an equivalent official release is adopted.
