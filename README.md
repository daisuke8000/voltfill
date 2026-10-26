<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

English · [日本語](README.ja.md)

A Rust library, CLI and Axum server for extracting structured values from PDFs. Define fields and questions in JSON; receive values, source Evidence and original received API response bodies. The PDF stays unchanged.

## Installation

Requires Rust 1.99.1. From the repository root:

```sh
cargo install --path crates/voltfill-cli --locked
```

For Vision/Hybrid, [enable a renderer](docs/configuration.en.md#rendering).

## Basic usage

Inspect locally without sending to Jev:

```sh
voltfill inspect input.pdf
```

For remote extraction, set your trusted Jev HTTPS URL in `VOLTFILL_JEV_ENDPOINT` and inject the key through `VOLTFILL_JEV_API_KEY` or `VOLTFILL_JEV_API_KEY_FILE`. Use your PDF as `input.pdf` and a JSON field/question definition as `schema.json`:

```sh
voltfill run input.pdf schema.json --remote -o result.json
```

Text-first is the default; use `--strategy hybrid` for Hybrid. See `voltfill --help` for options.

## Extraction strategies

| Strategy | Behavior | Schema |
|---|---|---|
| `text` | Selects candidates from source text | [Text](examples/text-form.json) |
| `vision` | Maps image Boxes to source text | [Vision](examples/vision-form.json) |
| `hybrid` | Compares text candidates and Boxes | [Hybrid](examples/hybrid-form.json) |

Default model: `jev-1.14.0`; override with `--model ID`. Vision/Hybrid require a versioned ID ≥ 1.14.0.

<a id="results"></a>

## Results

Extraction reports include `fields`, `evidence`, `usage` and `jev_responses`. Partial or ambiguous results may contain values: check `status` and `reason_codes`. Received bodies from every attempt, including retries/errors, are retained; `body_complete: false` indicates no body or an incomplete body.

By default, resolved values have no confidence cutoff; source, geometry, type and validators are checked. Reports have `evaluation_only: true`; confidence is not a calibrated correctness probability.

<a id="http-server"></a>

## HTTP quick start

Start a fixed-answer mock without Jev access:

```sh
voltfill serve --mock examples/text-mock.json --listen 127.0.0.1:3000
```

From another terminal, upload your PDF and the example schema. The mock demonstrates output format, not extraction accuracy.

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/text-form.json;type=application/json'
```

## Foundation crates

The foundation crates work without an extraction schema or Evidence. Their supported public APIs are:

| Crate | Public API |
|---|---|
| `pdf-probe` | `inspect`, `DocumentSnapshot`, `PdfLimits`, `PdfError`, `InspectionTimings`, source `SourceFragment` / `PageDiagnostic` / `Quality`, `geometry` and `metadata` |
| `pdf-render` | With `hayro` or `pdfium`: `render`, `RenderDocument` (open once, render several pages with the same checks and images), `RenderConfig`, `RenderBackend`, `RenderedImage`, `RenderError` |
| `jev-client` | `build_request`, `parse_request`, `parse_response`, `RequestDraft`, `Request`, `Content`, `Question`, `Asset`, `Limits`, `Answer`, `Response`, `Error`, `PngImage`, `Endpoint`, `HttpTransport`, `Secret`, `Policy`, `ExecutionConfig`, `RecordingTransport`, `ResponseRecord`, `ResponseBody`; `transport` traits, shared budgets and `execute` / `execute_observed` / `execute_deferred`; `planning` and `multipart` |
| `otel-log` | `TelemetryLogger` (local OTLP JSON logs for the `log` targets the caller names, or a supplied `SdkLoggerProvider`), `opentelemetry_sdk` |

`voltfill-core` depends on all four; `pdf-render` also depends on `pdf-probe`. No foundation crate depends on core, and `jev-client` has no PDF dependency. Core owns Evidence, extraction fields/results, aggregate `Timings` / `UsageSummary` and field/page-associated `JevResponse`. PDF inspection returns its own `InspectionTimings`. Existing core paths (`pdf`, `geometry`, `render`, `jev`, `model`, `logging`) remain supported through reexports and compatibility adapters; field/page-scoped observer APIs remain in `voltfill_core::jev::transport`.

Within one extraction, rendering opens a document only when a page needs an uncached image and reuses it across pages. Candidate lookups for a single page use a page index while preserving source order. Rendering budgets, geometry checks and Evidence remain unchanged.

All default feature sets remain empty. Enable `pdf-render/hayro` for this example, or `pdf-render/pdfium` with an explicit caller-supplied library path. Core forwards both features; backend selection and native-library policy are unchanged. `PngImage::try_new` checks PNG sides against `multipart::IMAGE_SIDE_PX` (280–1568 pixels). Library callers can use `PngImage::with_sides` with the range accepted by their model; this does not change the CLI/server image limits or confirm server acceptance. This prepares a request without sending it:

Use path dependencies on a source checkout from an independent project:

```toml
[dependencies]
pdf-probe = { path = "/path/to/voltfill/crates/pdf-probe" }
pdf-render = { path = "/path/to/voltfill/crates/pdf-render", features = ["hayro"] }
jev-client = { path = "/path/to/voltfill/crates/jev-client" }
```

```rust
use std::collections::BTreeMap;
use jev_client::{Asset, Content, Limits, PngImage, Question, RequestDraft};

fn prepare(pdf: &[u8]) -> Result<jev_client::multipart::Body, Box<dyn std::error::Error>> {
    let doc = pdf_probe::inspect(pdf, pdf_probe::PdfLimits::default())?;
    let image = pdf_render::render(pdf, doc.pages[0].page, &pdf_render::RenderConfig::hayro())?;
    let png = PngImage::try_new("page".into(), image.png)?;
    let request = jev_client::build_request(RequestDraft {
        model: "jev-1.14.0".into(),
        state: Content::Text("Inspect <page>".into()),
        questions: BTreeMap::from([("visible".into(), Question::Noul {
            instructions: Content::Text("Is a red image visible?".into()), criteria: None,
        })]),
        assets: BTreeMap::from([("<page>".into(), Asset { part: "page".into() })]),
    }, Limits::default())?;
    Ok(jev_client::multipart::encode(&request, &[png], "example-boundary")?)
}
```

For transmission, use `transport::execute` or `execute_observed` (or `execute_deferred`, which makes the page images only once an admitted slot is held and returns the slot unsent if they cannot be made) with explicit permission, a caller-injected `Secret`, validated limits and a `Clock`. Wrap the transport with `RecordingTransport::new(&mut transport, |record| records.push(record))` to retain received bodies and safe outcome metadata. The sink runs once per send, in completion order, before parsing and observer callbacks, including errors and incomplete bodies. Attach request context in the sink when sharing it across calls/workers. It receives no authentication data or HTTP headers. Logs retain the compatible `voltfill_core::jev` target and do not replace response data.

Each attempt reserves estimated input tokens before sending. After completion, the per-second input-token limit holds reported usage for the completion gap, or the full reservation when usage is unavailable. The gap starts at 2 seconds by default. Jev HTTP 429 responses multiply it by `completion_gap_backoff` (default 2, range 1–16; 1 keeps it fixed), capped by `max_completion_gap_ms` (default 60 seconds). Responses within the previous backoff window increase it only once. `completion_gap_ms` must be at least 1 second and no greater than the cap; the cap may be raised to 600 seconds. Cost accounting, stop-on-over-reservation checks and deadlines still apply.

## Documentation

See [Development](docs/development.en.md) for CI checks, dependency audits, secret scanning and workflow security checks.

[Configuration](docs/configuration.en.md) · [HTTP server](docs/server.en.md) · [Development](docs/development.en.md) · [Benchmarks](docs/benchmarks.en.md)

[Architecture and C4](https://daisuke8000.github.io/voltfill/) · [MIT license](LICENSE)
