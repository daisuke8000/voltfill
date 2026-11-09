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

- `vf-source` owns PDF inspection, source diagnostics, geometry and layout hypotheses (lines and label/value pairs). `vf-render` owns optional page rendering and may depend on `vf-source`. Neither requires a form, an extraction schema or Evidence.
- `vf-client` owns the Jev protocol, transport, bounded execution and received-body recording. It must not depend on `vf-source`, `vf-render`, `vf-core` or `vf-legacy`.
- `vf-core` owns the form contract, label/type matching, value normalization, fill results and their Evidence. It is the product core and must not depend on `vf-legacy`.
- `vf-legacy` owns the legacy schema-based Text/Vision/Hybrid extraction, its models, Jev adapter and resource limits. It is kept only for the Text-versus-Vision comparison and will be removed as a whole; do not add features to it.
- `vf-server` is a library of HTTP routes. `vf-cli` builds the only binary, `voltfill`, and owns settings loading, logging setup and `serve`.
- Foundation crates (`vf-source`, `vf-render`, `vf-client`) never depend on `vf-core`, `vf-legacy`, `vf-server` or `vf-cli`. `vf-server` never depends on `vf-cli`.

## Conventions

- Workflow actions are locked in .github/workflows/actions.lock. After changing any `uses:`, regenerate it with `gh actions-lock` and never edit it by hand.
- Use the toolchain pinned in rust-toolchain.toml. Preserve the MSRV and Edition in Cargo.toml unless asked to change them.
- Use targeted tests for behavioral changes, then the workspace checks in README.md. Run default and renderer-feature test configurations sequentially because they share executable paths. Run `cargo clean` after final verification. Preserve toolchains, dependency downloads and caller-supplied native libraries.
- Documentation is README.md and the architecture/C4 page docs/architecture.html (published to GitHub Pages), in Japanese. Keep both accurate when documented behavior or crate structure changes, and mark unimplemented parts as planned. Keep both minimal: usage and supported behavior only; detailed rules live in code and tests, not in the docs. Exclude account-specific settings, local environment details, investigation reports and conversation history. State validation limits honestly.
