<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

English · [日本語](README.ja.md)

A Rust library, CLI and Axum server for extracting structured values from PDFs. Define fields and questions in JSON; receive values, source Evidence and original received API response bodies. The PDF stays unchanged.

> [!IMPORTANT]
> **Jev model selection:** The default is `jev-1.14.0`. Vision/Hybrid require a versioned model ID of 1.14.0 or later. `jev-latest` and `jev-preview` are accepted only for Text-first.

> [!NOTE]
> **Font parser maintenance:** `pdf-inspector` depends on `ttf-parser 0.25.1`, which has a [maintenance advisory](https://rustsec.org/advisories/RUSTSEC-2026-0192.html). This advisory does not report a discovered vulnerability. See [Dependency updates](#dependency-updates) for the update policy.

## Installation

Requires Rust 1.99.1; Edition 2024. One installed `voltfill` command runs extraction and the HTTP server.

```sh
cargo install --path crates/voltfill-cli --locked --features pdfium,hayro
```

Run this from the repository root. Omit `--features pdfium,hayro` when rendering is unnecessary. Cargo normally installs the executable into `$HOME/.cargo/bin`. Check that your shell can find it:

```sh
command -v voltfill
voltfill --help
```

If the command is not found after installation, add Cargo's bin directory to PATH:

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

For zsh, add that line to `~/.zshrc` to keep it in new terminals. If you customized Cargo's installation directory with `--root`, `CARGO_INSTALL_ROOT` or `CARGO_HOME`, use its `bin` directory instead.

## Basic usage

Inspect a PDF locally without sending it to the API:

```sh
voltfill inspect input.pdf
```

Use your own PDF as `input.pdf` and a field/question definition as `schema.json`. Text-first is the default extraction strategy.

For remote extraction, set `VOLTFILL_JEV_ENDPOINT` to your trusted Jev HTTPS URL and inject the key through `VOLTFILL_JEV_API_KEY` or `VOLTFILL_JEV_API_KEY_FILE`, then run:

```sh
voltfill run input.pdf schema.json --remote
```

By default, VOLTFILL returns resolved values without a confidence cutoff. Source, geometry, type and validator checks still apply. Confidence evaluation is enabled only by an explicit `evaluation_profile`; normally omit `--evaluation-profile synthetic` from real API commands, as it is a policy for local synthetic tests. Confidence is not a calibrated probability that the value is correct. Without a profile, Noul boolean conversion treats values >= 0.5 as true and values < 0.5 as false. Vision/Hybrid still ask the Box question when the presence answer is false; only an explicit profile enables presence-based skipping.

Use `--strategy hybrid` for Hybrid and `-o result.json` to write results to a file. CLI/server `--help` lists options. Existing long-form flags and strategy names remain supported.

## Launch configuration

Launch extraction or the server from a configuration file:

```sh
voltfill run --config examples/voltfill.yaml
voltfill serve --config examples/voltfill.toml
```

`run` uses the same extraction path as `extract`. `serve` starts the same HTTP server as the standalone `voltfill-server` binary, in process, without searching for or spawning another executable. Omit `--features` when rendering is unnecessary. The `extract` subcommand and standalone `voltfill-server` binary are also supported.

Launch files support YAML (`.yaml`/`.yml`) and TOML (`.toml`) as standard formats, plus JSON compatibility, with the same settings in each. The example [YAML](examples/voltfill.yaml), [TOML](examples/voltfill.toml) and [JSON](examples/voltfill.json) use fixed answers and send nothing to Jev. Provide `input.pdf` beside the configuration file, or override the PDF path on the command line. Fixed mock answers demonstrate the response format; they do not evaluate arbitrary documents. `-c` is an alias for `--config`.

```yaml
version: 1
run:
  pdf: input.pdf
  schema: hybrid-schema.json
  output: result.json
  remote: true
  strategy: hybrid
  model: jev-1.14.0
  renderer: pdfium
  pdfium_lib: /absolute/path/libpdfium.dylib
  pixels_per_point: 1.5
serve:
  remote: true
  allow_images: true
  renderer: pdfium
  pdfium_lib: /absolute/path/libpdfium.dylib
  pixels_per_point: 1.5
  listen: 127.0.0.1:3000
execution:
  max_concurrency: 2
  max_batch_questions: 10
  max_attempts: 3
```

Only the selected command's `run` or `serve` section is used. The shared `execution` section uses existing page/question batching, rate, concurrency, retry, token and budget controls. Precedence is **explicit CLI arguments → configuration file → environment defaults → built-in defaults**. `--execution-config FILE` replaces the complete `execution` section; extraction's `--max-attempts` and `--max-concurrency` flags apply afterward. Paths inside a file are relative to its directory; CLI and environment paths are relative to the current working directory. `VOLTFILL_CONFIG` selects the launch file when `--config` is omitted.

Files must be selected explicitly with a flag or a documented environment variable. There is no automatic discovery, inclusion, environment interpolation or command execution. Keep API keys out of files and inject them through the existing environment variables. `remote: true` selects real API access; use `voltfill run -c FILE --dry-run` to inspect extraction input without sending. Starting `serve` alone sends nothing to Jev. HTTP upload strategy and model remain request `options`; an omitted model uses the server startup default.

Files are limited to 64 KiB. Unknown fields, duplicate keys and invalid types are errors. YAML anchors, aliases, merge keys and includes are disabled; nesting and node counts are bounded. Legacy JSON files containing only execution limits remain supported with `--config` or `--execution-config`.

## Extraction strategies

| Strategy | Behavior | Schema example |
|---|---|---|
| `text` | Selects candidates from PDF source text | [Text](examples/text-form.json) |
| `vision` | Maps image Boxes to PDF source text | [Vision](examples/vision-form.json) |
| `hybrid` | Compares text selection and image Boxes; array output retains Box runs on disagreement | [Hybrid](examples/hybrid-form.json) |

The CLI, library and HTTP server all send text for Text-first, PDF page images for Vision, and text plus page images for Hybrid to Jev.

Vision/Hybrid render PDF pages as PNGs. Choose an optional renderer at build time and select it with `--renderer pdfium|hayro`. PDFium requires a caller-supplied library; Hayro renders in Rust without a native library. No renderer is enabled in the default build, and VOLTFILL never switches renderers automatically.

For PDFium, build with `--features pdfium` and supply an absolute library path:

```sh
cargo build --workspace --features pdfium --locked
./target/debug/voltfill extract input.pdf vision-schema.json --remote \
  --strategy vision --pdfium-lib ABSOLUTE_PATH
```

For the CLI and server, `VOLTFILL_PDFIUM_LIB_PATH` can be used instead of `--pdfium-lib`. Omitting `--renderer` preserves this PDFium path behavior; explicitly choosing `--renderer pdfium` also requires the path. CLI/server rendering defaults to 1.5 pixels per PDF point. Set `--pixels-per-point NUMBER` to override it. Rendered images must have each side between 280 and 1568 pixels; oversized images are rejected before sending rather than silently resized.

To use Hayro, enable its feature and select it explicitly:

```sh
cargo build --workspace --features hayro --locked
./target/debug/voltfill extract input.pdf vision-schema.json --remote \
  --strategy vision --renderer hayro
```

Both renderers can be compiled with `--features pdfium,hayro`. `--renderer hayro` ignores the PDFium environment variable and cannot be combined with `--pdfium-lib`. Hayro rejects unavailable font requests and public interpreter warnings instead of substituting system fonts. Upstream can also omit unsupported content without a public warning; VOLTFILL cannot detect every omission, so inspect rendered PDFs before relying on them. Rendering support can differ between backends; source text inspection and Evidence checks remain independent. Rust callers select `RenderConfig::new(path)` or `RenderConfig::hayro()`.

PDFium is supplied by the caller. VOLTFILL does not download it automatically or perform OCR.

## Returning multiple source runs

Use `"output_type": "string_array"` with `text_in_box`, `selected_candidate` or `selected_candidates`. For example, the following field settings return separate strings instead of joining source runs:

```json
{
  "output_type": "string_array",
  "normalization": "trim",
  "validators": [{"kind": "non_empty"}, {"kind": "max_chars", "count": 200}]
}
```

A field that returns source text, including a `string` field, can also set `"unify_cjk_radicals": true`. Characters in the Kangxi Radicals and CJK Radicals Supplement blocks that have a compatibility decomposition (for example U+2F08 for 人) are then mapped to the unified ideographs before `normalization` applies; other compatibility characters, such as full-width letters, keep their source form. The setting is false by default, and Evidence keeps the original text.

For `text_in_box`, each usable source run whose rectangle overlaps the Box with positive area becomes an array element, in PDF source `item_index` order. VOLTFILL does not infer visual reading order, impose an overlap percentage, crop the text, join runs or fill missing text. Because the entire run is returned, an element can include text outside the Box. Each element retains its original text and geometry in Evidence. `selected_candidate` returns the selected source string as a one-element array. The existing `string` contract remains unchanged.

For multiple Text-first matches, use [`examples/text-multi-form.json`](examples/text-multi-form.json): `"resolver": {"kind": "selected_candidates", "question_id": "pick", "max_selections": 8}`. This opt-in resolver requires `string_array`, exactly one independent Text Choice question with dynamic `text_spans`, and an explicit `max_selections` from 1 to 254 **per page**. It is unavailable in Vision/Hybrid. Specify the desired scope in the question: ask for the complete value or only the relevant part. VOLTFILL repeats that unchanged question, removing selected candidate IDs from subsequent choices until Jev selects `none` or all candidates are selected. Results follow source order; identical strings at different source positions remain separate elements. The original candidate limit applies before exclusions. Each round shares the normal global batching, request, retry, token, cost and deadline limits. Reaching the selection cap with candidates remaining gives `ambiguous` / `selection_limit_reached`; errors and budget exhaustion retain all values and Evidence already obtained.

Choice probabilities are preserved as received: an approximate total such as 0.99 is reported diagnostically rather than rejected, and VOLTFILL does not replace Jev's selection with a higher-probability candidate. Ties or a nonmaximum selection annotate the result as `ambiguous` while retaining the resolved value and Evidence. Multiple selection continues after such annotations until the API selects `none` or an execution limit stops it. Explicit confidence profiles also annotate resolved source values rather than deleting them. Structural/type/range checks and candidate membership still apply; an unresolvable answer retains its original response body without inventing a value. Probability totals are diagnostic for Score too; its legend, range and expected-score checks remain in place.

Array validators apply after collecting results across pages. Known array elements and Evidence are retained even when validation or another page fails; check `status` and `reason_codes` rather than treating a non-null value as success. Normalization applies separately to each element and leaves original Evidence unchanged. `non_empty` rejects an empty array or any empty/whitespace-only element. `max_chars` limits each element's Unicode scalar count, not the array's total length. Invalid elements cause validation failure; elements are never silently removed or truncated.

## Jev model selection

The default model is `jev-1.14.0`. Override it with CLI `--model ID`, Rust `ExtractOptions.model`, or the HTTP multipart `options` field's `model` property.

Text-first accepts versioned model IDs and the moving aliases `jev-latest` and `jev-preview`. Vision/Hybrid require a versioned ID of 1.14.0 or later; older versions and aliases are rejected before sending. A syntactically valid ID does not guarantee provider availability.

The report's `model` records the requested ID. Each original response retains the model ID returned by Jev for the model it actually used.

## HTTP server

### Try it without API access

After building, start a server that uses fixed answers. This example is for local testing on loopback without configured Bearer authentication.

```sh
voltfill serve --mock examples/text-mock.json --listen 127.0.0.1:3000
```

From another terminal, check health and request extraction:

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/health

curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/text-form.json;type=application/json' \
  --form-string 'options={"strategy":"text_first","evaluation_profile":{"min_confidence":0.99,"noul_true_min":0.99,"noul_false_max":0.01}}'
```

`evaluation_profile` explicitly enables confidence annotations and sets Noul decision thresholds for this synthetic example. These values do not guarantee accuracy on real data. Extraction returns [JSON containing values, Evidence and original responses](#what-voltfill-returns-to-its-caller).

### Use Jev

Stop the mock server, set `VOLTFILL_JEV_ENDPOINT`, inject the Jev credential through an environment value or explicit file, and start with `--remote`:

```sh
voltfill serve --remote --listen 127.0.0.1:3000
```

Send the PDF and schema to the same `/v1/extract` endpoint. `options` is optional and defaults to Text-first with `jev-1.14.0`. To override it, for example, use `--form-string 'options={"strategy":"text_first","model":"jev-1.13.0"}'`. Omit `evaluation_profile` to return resolved values without a confidence cutoff. Set it explicitly to enable caller-defined confidence annotations and Noul decision thresholds.

To accept Vision/Hybrid, [build with a renderer enabled](#extraction-strategies), then start with PDFium as follows. Replace the Text-first server rather than running both on the same address.

```sh
voltfill serve --remote --allow-images \
  --listen 127.0.0.1:3000 --pdfium-lib ABSOLUTE_PATH
```

For a Hayro build, start the server with `--remote --allow-images --renderer hayro --listen 127.0.0.1:3000`. The server startup option selects the renderer for all uploads; multipart options do not change it.

Example Vision request: upload the PDF and schema; the server renders the page images.

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/vision-form.json;type=application/json' \
  --form-string 'options={"strategy":"vision_first","model":"jev-1.14.0"}'
```

For Hybrid, change the schema to `examples/hybrid-form.json` and `options.strategy` to `hybrid`. HTTP strategy names are `text_first`, `vision_first` and `hybrid`.

### Server authentication

`127.0.0.1` accepts connections only from the same machine. Authentication is optional on this address when `VOLTFILL_SERVER_API_KEY` is not set.

To enable authentication, inject `VOLTFILL_SERVER_API_KEY` through the environment before starting the server. Supply the same value to the client. This example passes the authentication header to curl through standard input:

```sh
printf 'Authorization: Bearer %s\n' "${VOLTFILL_SERVER_API_KEY}" |
  curl --fail-with-body --silent --show-error --header @- \
    --form 'pdf=@input.pdf;type=application/pdf' \
    --form 'schema=@schema.json;type=application/json' \
    http://127.0.0.1:3000/v1/extract
```

For access from other machines, provide HTTPS through a TLS-terminating proxy or equivalent and change the curl URL to that HTTPS endpoint. VOLTFILL itself listens over HTTP, so protect the proxy-to-VOLTFILL connection through loopback or an equivalent boundary as well.

Bearer authentication must be configured when changing the VOLTFILL listener to a non-loopback address such as `--listen 0.0.0.0:3000`. Extraction requests with an `Origin` header require authentication even on `127.0.0.1`.

## Jev requests and responses

These examples use fictional data and illustrative candidate IDs, question IDs and image references. Jev wire payloads and [the data VOLTFILL returns to its caller](#what-voltfill-returns-to-its-caller) use different formats.

### Text-first: send text

Send source text and questions as `application/json`, without an image attachment. Independent questions can share one request.

```json
{
  "model": "jev-1.14.0",
  "state": {"source_fragments": ["請求番号", "ABC-123"]},
  "questions": {
    "pick": {
      "type": "choice",
      "instructions": "請求番号を選んでください",
      "criteria": {"c_1": "請求番号", "c_2": "ABC-123", "none": "該当なし"}
    },
    "present": {"type": "noul", "instructions": "請求番号の記載はありますか？"}
  }
}
```

Example Jev response: `choice` identifies the selected candidate, and `noul` answers the presence question.

```json
{
  "model": "jev-1.14.0",
  "answers": {
    "pick": {"type": "choice", "choice": "c_2", "probabilities": {"c_1": 0, "c_2": 1, "none": 0}, "confidence": 1},
    "present": {"type": "noul", "noul": 1}
  },
  "usage": {"input_tokens": 120, "output_tokens": 0}
}
```

### Vision-first: send a page image

Send `multipart/form-data`: the `request` part contains JSON, and the `image` part contains the rendered PNG. Image requests include `TypeSafe-Danger-Zone-Images: true`.

This example asks for the location after the presence check has returned.

```json
{
  "model": "jev-1.14.0",
  "state": {"image": "<image>"},
  "questions": {
    "location": {"type": "box", "instructions": "請求番号の値がある領域を指定してください"}
  },
  "assets": {"<image>": {"part": "image"}}
}
```

`<image>` references the attached PNG. No extracted text is sent. Jev returns a Box in the image; VOLTFILL maps that region to PDF source text.

```json
{
  "model": "jev-1.14.0",
  "answers": {
    "location": {"type": "box", "box": {"left": 0.12, "top": 0.64, "right": 0.29, "bottom": 0.72}, "confidence": 1}
  },
  "usage": {"input_tokens": 1500, "output_tokens": 0}
}
```

### Hybrid: use text and images

Combine text candidate selection with image location questions. Questions using both modalities include text and the image reference in the same JSON `state`, with a PNG attachment.

```json
{
  "model": "jev-1.14.0",
  "state": {"source_fragments": ["請求番号", "ABC-123"], "image": "<image>"},
  "questions": {
    "location": {"type": "box", "instructions": "請求番号の値がある領域を指定してください"}
  },
  "assets": {"<image>": {"part": "image"}}
}
```

The location question returns the Box response above; candidate selection returns a Choice response. VOLTFILL maps both to the source and checks agreement. With `string_array`, disagreement or a tie marks the result ambiguous while retaining the Box array and its Evidence; inspect status and reasons alongside values.

Different input modalities use separate batches. Dependent questions, such as those requiring a presence check, are sent after their prerequisites return. Independent batches run within configured concurrency limits.

### What VOLTFILL returns to its caller

| Field | Contents |
|---|---|
| `fields` | Per-field values, statuses and Evidence IDs |
| `evidence` | Original strings, PDF pages and coordinates, and value provenance |
| `usage` | Attempt counts and known token usage |
| `jev_responses` | Original body, status, question IDs, retry delay and transport error for every HTTP attempt |

`jev_responses[].body` contains the received string or byte array, including retry and error responses. `body_complete: false` indicates an incomplete body due to interruption, size limits or other receive failures.

Logging does not alter returned data. Confidence is not a guaranteed correctness probability; current extraction reports have `evaluation_only: true`.

## Benchmarks

The [live API benchmark report](docs/benchmarks.en.md) covers ten CLI/HTTP conditions and their measurement settings. Each condition used a recreated two-page, 20-field PDF once. All 200 values and Evidence were retained: 164 full matches, 36 partial matches and four fields with warnings. Text used the single-candidate schema; the new multiple-selection feature was not measured. Confidence is not correctness probability, and direct performance comparisons with the earlier PDF are unsupported.

## Environment variables

| Variable | Purpose |
|---|---|
| `VOLTFILL_JEV_ENDPOINT` | Required trusted HTTPS destination for remote mode; no built-in URL fallback |
| `VOLTFILL_JEV_API_KEY` / `VOLTFILL_JEV_API_KEY_FILE` | Jev credential value or explicit credential file; choose one |
| `VOLTFILL_SERVER_API_KEY` / `VOLTFILL_SERVER_API_KEY_FILE` | Server Bearer credential value or file; required for non-loopback binding |
| `VOLTFILL_CONFIG` | Launch file for `voltfill run`, `extract` or `serve`; `--config` overrides it |
| `VOLTFILL_EXECUTION_CONFIG` | JSON execution limits when no explicit execution file or launch `execution` section is selected |
| `VOLTFILL_RESOURCE_CONFIG` | JSON resource limits when no `--resource-config` or launch `resources` section is selected |
| `VOLTFILL_LISTEN` | Server listen address; default `127.0.0.1:3000` |
| `VOLTFILL_UPLOAD_TIMEOUT_MS` | Server upload deadline, 1–3600000 ms; default `30000` |
| `VOLTFILL_MODEL` | CLI/server default model; HTTP `options.model` overrides the server default |
| `VOLTFILL_STRATEGY` | Extraction CLI strategy; default `text` |
| `VOLTFILL_RENDERER` | Explicit `pdfium` or `hayro` renderer selection |
| `VOLTFILL_PDFIUM_LIB_PATH` | Absolute PDFium library path; ignored when Hayro is selected |
| `VOLTFILL_PIXELS_PER_POINT` | CLI/server rendering scale; default `1.5` |
| `VOLTFILL_LOG_LEVEL` | `off`, `error`, `warn`, `info` or `debug`; default `off` |

CLI and launch-file values take precedence over environment defaults. Environment settings are read only by application launchers; Rust callers inject `Endpoint`, `ExecutionConfig`, `ResourceLimits` and renderer settings explicitly. The standalone `voltfill-server` supports execution/resource JSON files and startup environment defaults; use `voltfill serve` for YAML/TOML launch files.

The endpoint must use HTTPS, be at most 2048 bytes, and contain no URL credentials, query, fragment, whitespace or backslash. The operator is responsible for trusting the destination and its DNS resolution: HTTPS alone does not make an arbitrary server safe to receive documents or credentials. TLS verification remains enabled; redirects, ambient proxies and automatic HTTP retries remain disabled. Uploaded request options cannot change the endpoint, credentials or resource policy. URLs and credentials are omitted from logs and diagnostics.

Credential files are explicitly selected regular files, limited to 8 KiB; one final LF or CRLF is accepted. Setting both a credential value and its `_FILE` variable is an error. No files are discovered automatically and no `.env` files are loaded. Help, inspect, validate, mock extraction and extraction dry-run need no Jev endpoint or credential. `--remote` (or launch `remote: true`) remains the explicit remote permission; environment defaults do not enable it.

A remote server must be started with `--allow-images` to accept image-based extraction; otherwise it rejects those requests before API execution.

HTTP extraction requests with an `Origin` header require configured server Bearer authentication, including on loopback. Embedding applications are responsible for securing the router's listener and transport.

## Resource limits

A launch file can contain a shared `resources` section. `--resource-config FILE` selects a JSON replacement for that section; `VOLTFILL_RESOURCE_CONFIG` provides a fallback. Values may lower the supported ceilings below; omitted fields keep their defaults. PDF limits apply to local reads, HTTP uploads, inspection, extraction and rendering. Render limits apply only when a renderer is selected.

```yaml
resources:
  pdf:
    max_bytes: 16777216
    max_pages: 100
    max_items_per_page: 10000
    max_recovery_pages: 8
  max_render_pixels: 16000000
  max_render_output_bytes: 67108864
```

All limits must be positive except `max_recovery_pages`, which may be zero. `serve.upload_timeout_ms` or `--upload-timeout-ms` controls upload time separately from extraction deadlines. `serve.model` or `--model` sets the default for requests that omit a model. The server admits one extraction job at a time so execution budgets and cooldown are not multiplied across jobs; `max_concurrency` controls Jev workers within that job. While a job runs, during the cooldown after it, or while remote work is paused, the server answers `429 server_busy` with `Retry-After` (or `503 remote_paused_unknown_outcome`) without starting an extraction; it first reads and discards the rest of the upload, within the upload size limit and upload deadline, so the client receives that response instead of a closed connection. A client sending `Expect: 100-continue` gets the response before uploading. An upload declared larger than the limit, or arriving while several rejected uploads are already being read, is answered without reading it, and the connection may close before the upload finishes. Parser-internal allocations remain outside these limits.

## Request control

Pass an optional JSON file with `--config FILE`. Specify only overrides; omitted fields use defaults. See [execution-config.json](examples/execution-config.json) for a complete example.

| Field | Meaning | Default |
|---|---|---|
| `max_concurrency` | Concurrent POST workers; at least 1. Up to this many pages are rendered and held at once, so memory grows with it | `2` |
| `max_batch_questions` | Questions per batch; 1–255 | `32` |
| `max_attempts` | Attempts per request, including the first; 1–3 | `3` |
| `max_requests` | Total POST attempts, including retries | `128` |
| `requests_per_second` | Request-rate limit | `80` |
| `input_tokens_per_second` | Input-token reservation rate limit | `100000` |
| `input_tokens_per_attempt` | Maximum estimated input tokens allowed per attempt; batches split to fit | `64000` |
| `context.max_request_tokens` | Estimated state plus all questions per request | `64000` |
| `context.max_question_tokens` | Estimated state plus any one question | `32000` |
| `context.image_tokens_per_image` | Planning allowance per occurrence of an image reference | `16000` |
| `context.text_tokens_per_100_bytes` | Text tokens counted per 100 serialized UTF-8 bytes, 25–100 | `100` |
| `total_deadline_ms` | Overall deadline, including waits and retries | `30000` |
| `attempt_timeout_ms` | Timeout for one HTTP attempt | `10000` |
| `initial_backoff_ms` | Initial retry delay | `250` |
| `backoff_multiplier` | Exponential retry-delay multiplier | `2` |
| `max_backoff_ms` | Backoff cap; provider `Retry-After` remains a minimum wait | `10000` |
| `jitter_ms` | Maximum random addition to retry delay | `250` |
| `max_cost_nano_usd` | Estimated cost budget in billionths of a USD | `200000000` ($0.20) |
| `input_token_price_nano_usd` | Estimated input-token unit price, 1–1000000000 nano-USD | `42` |
| `completion_gap_ms` | Token reservation/cooldown floor, 1000–60000 ms; larger `Retry-After` is retained | `2000` |
| `max_json_bytes` | Request/response JSON byte limit, 1–1048576 | `1048576` |
| `max_total_body_bytes` | Total outbound body bytes across attempts for one planned request, 1–33554432 | `33554432` |
| `organization.requests_per_second` | Upper bound for the configured request rate | `80` |
| `organization.input_tokens_per_second` | Upper bound for the configured token rate | `100000` |

Cost accounting estimates input tokens only, using the configured unit price for reservations and received usage. It is not a complete provider invoice or live price lookup; set it for the chosen model and endpoint.

Times use milliseconds (`1000` = one second). Context defaults follow the standard 64k/32k model limits. Rate defaults are application settings, not discovered organization allocations. Configure your allocation and lower rates as needed; higher organization allocations are also accepted. Increasing context settings does not enable experimental long context or guarantee provider support.

Before sending, VOLTFILL splits compatible questions by question count, serialized size and estimated input tokens, preserving page scope and dependencies. A single question with its page state that cannot fit is returned as `unsupported` with `input_token_limit_exceeded`; source text, candidates and images are never silently truncated.

Estimation uses `context.text_tokens_per_100_bytes` tokens per 100 serialized UTF-8 bytes (one token per byte by default) plus 256 framing tokens, and the configured image allowance for each image occurrence. This is a conservative text estimate, not a provider tokenizer or a guaranteed upper bound. Lower `context.text_tokens_per_100_bytes` only to a value above the tokens per 100 bytes measured from the model's reported usage for your own inputs; the ratio varies with script and content. A value below it underestimates, which also loosens the token-rate and cost reservations. The default image allowance is an application estimate, not a published image-token formula. Actual usage can exceed it. Each attempt reserves its own estimate; if received input usage exceeds that reservation, further admission stops while the original response remains available. Unknown usage retains the cost reservation. `request_planned` logs contain counts and estimates, never content.

Retries use the same limits. VOLTFILL does not manage traffic from other processes. The `organization` fields validate configured rates; they do not coordinate external clients.

CLI `--max-concurrency` and `--max-attempts` override the corresponding file values.

PDF input and page limits do not bound all upstream stream decompression or predictor allocations. Untrusted PDFs can exhaust process memory; VOLTFILL does not provide process isolation.

The total deadline is cooperative: it stops later scheduling and limits waits, retries and HTTP attempts; it cannot interrupt a synchronous PDF parser or renderer call.

## Logging

OTLP JSON logs are **disabled by default**. CLI/server `--log-level off|error|warn|info|debug` enables or disables them.

CLI logs use stderr, or stdout with `--output FILE`. Server logs use stdout.

External forwarding is managed by the caller. Response bodies, credentials and HTTP headers are excluded from logs. Logging does not change returned response data.

## Development

To build and inspect from the workspace, use the commands below. `cargo build` creates `target/debug/voltfill` without installing it into PATH.

```sh
cargo build --workspace --locked
./target/debug/voltfill inspect input.pdf
```

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Run feature tests with `cargo test --workspace --features hayro --locked` or `cargo test --workspace --features pdfium --locked`. These tests do not use PDF files or exercise native rendering. Run feature configurations sequentially because executable paths are shared. Generate Rust API documentation with `cargo doc -p voltfill-core --no-deps`.

Run `cargo clean` to remove local build artifacts after verification.

### Dependency updates

VOLTFILL tracks official font-parser dependency releases and checks source text, coordinates and Evidence before adopting relevant fixes.

[Architecture and C4](https://daisuke8000.github.io/voltfill/) · [MIT license](LICENSE)
