# VOLTFILL agent instructions

Read README.md and the relevant source/tests before changing behavior. Keep work within the user's requested scope. Verify the actual workspace, toolchain and effective permissions; do not infer current execution results from fixtures.

## Invariants

These rules protect users, credentials and response data. Do not relax them without an explicit request.

- Do not read or print credentials, enumerate environment secrets, explore other projects, escalate privileges, push Git or send Jev requests without explicit authorization. Runtime code may use caller-injected credentials only for their documented purpose. Never log bodies, credentials or HTTP headers.
- Preserve original Evidence and every received Jev attempt body before parsing or observer failure, including retries, error paths and incomplete bodies. Recording sinks carry safe outcome metadata only. Logs do not replace response data.
- Validate inputs and bound requests, retries, concurrency, token/cost reservations and deadlines. Apply the configured limits across workers and retries.
- Keep all reports evaluation-only. Do not describe acceptance thresholds as calibrated probabilities or mock results as live accuracy/performance.
- Values come from the PDF's own text layer. Do not generate or transcribe value text with a model. OCR is currently outside scope: do not add OCR code or dependencies.
- Native rendering is optional and requires an explicit caller-supplied library path. Do not add automatic native downloads or ambient library searches.
- Track `ttf-parser` maintenance advisory RUSTSEC-2026-0192 through `pdf-inspector`. Adopt compatible official fixes after text, geometry and Evidence regression checks. Do not suppress the advisory or equate an unmaintained warning with a confirmed vulnerability. Prefer upstream fixes over a permanent fork. Any temporary patch must state its scope, downstream Cargo requirements and removal condition.

## Crate boundaries

- `voltfill-pdf` owns PDF inspection, source diagnostics and geometry. `voltfill-render` owns optional page rendering and may depend on `voltfill-pdf`. Neither requires an extraction schema or Evidence.
- `voltfill-jev` owns the Jev protocol, transport, bounded execution and received-body recording. It must not depend on PDF crates or `voltfill-core`.
- `voltfill-core` owns Evidence, extraction fields/results, aggregate timings/usage and logging. Foundation crates must never depend on core.
- `voltfill-cli` and `voltfill-server` are thin front ends over core.

## Conventions

- Workflow actions are locked in .github/workflows/actions.lock. After changing any `uses:`, regenerate it with `gh actions-lock` and never edit it by hand.
- Use the toolchain pinned in rust-toolchain.toml. Preserve the MSRV and Edition in Cargo.toml unless asked to change them.
- Use targeted tests for behavioral changes, then the workspace checks in README.md. Run default and renderer-feature test configurations sequentially because they share executable paths. Run `cargo clean` after final verification. Preserve toolchains, dependency downloads and caller-supplied native libraries.
- Documentation is README.md and the architecture/C4 page docs/architecture.html (published to GitHub Pages), in Japanese. Keep both accurate when documented behavior or crate structure changes, and mark unimplemented parts as planned. Focus it on usage and supported behavior. Exclude account-specific settings, local environment details, investigation reports and conversation history. State validation limits honestly.
