# Configuration

English · [日本語](configuration.md) · [README](../README.md)

<a id="launch-configuration"></a>

## Launch configuration

```sh
voltfill run --config examples/voltfill.yaml
voltfill serve --config examples/voltfill.toml
```

YAML (`.yaml`/`.yml`), TOML (`.toml`) and compatible JSON share the same settings. `-c` aliases `--config`. The [YAML](../examples/voltfill.yaml), [TOML](../examples/voltfill.toml) and [JSON](../examples/voltfill.json) examples use fixed answers without Jev access. Put `input.pdf` beside the file or override its path on the CLI. Mock answers demonstrate output format, not accuracy on arbitrary documents.

Precedence: **explicit CLI → launch file → environment → defaults**. Without `--config`, `VOLTFILL_CONFIG` selects the file. Paths in a file are relative to its directory; CLI/environment paths use the working directory. Only the selected command’s `run` or `serve` section applies.

Files must be explicitly selected, ≤ 64 KiB. Unknown fields, duplicate keys and invalid types fail; YAML anchors, aliases, merges and includes are disabled. No discovery, environment interpolation, command execution or `.env` loading occurs. Keep API keys out of configuration.

Real API access requires `--remote` or launch `remote: true`; environment defaults cannot enable it. `voltfill run -c FILE --dry-run` checks input without sending. Starting `serve` alone sends nothing.

<a id="environment"></a>

## Endpoint and credentials

Use a trusted HTTPS endpoint and DNS resolution before sending documents/credentials. TLS verification is enabled; redirects, ambient proxies and automatic HTTP-client retries are disabled. Upload options cannot change endpoint, credentials or resource policy. URLs/credentials are excluded from logs and diagnostics.

Remote mode has no default URL: set `VOLTFILL_JEV_ENDPOINT`. Inject `VOLTFILL_JEV_API_KEY` or `VOLTFILL_JEV_API_KEY_FILE`. Select credential files explicitly. A value plus its `_FILE` variable is an error. The same file rules apply to [server credentials](server.en.md#authentication).

<a id="request-control"></a>

## Execution limits

Shared `execution` controls requests, retries and budgets. `--execution-config FILE` replaces the whole section with JSON; legacy execution-only JSON also works with `--config`. `VOLTFILL_EXECUTION_CONFIG` is used only without an explicit execution file or launch `execution` section. Extraction’s `--max-concurrency` and `--max-attempts` apply afterward.

| Field | Meaning | Default |
|---|---|---|
| `max_concurrency` | Concurrent POST workers; at least 1. Up to this many pages are rendered and held at once, so memory grows with it | `2` |
| `max_batch_questions` | Questions per batch; 1–255 | `32` |
| `max_attempts` | Attempts per request, including the first; 1–3 | `3` |
| `max_requests` | Total POST attempts, including retries | `128` |
| `requests_per_second` | Request-rate limit | `80` |
| `input_tokens_per_second` | Input-token reservation rate limit | `100000` |
| `total_deadline_ms` | Overall deadline, including waits and retries | `30000` |
| `attempt_timeout_ms` | Timeout for one HTTP attempt | `10000` |
| `max_cost_nano_usd` | Estimated cost budget in billionths of a USD | `200000000` ($0.20) |
| `input_token_price_nano_usd` | Estimated input-token unit price, 1–1000000000 nano-USD | `42` |
| `completion_gap_ms` | Token reservation/cooldown floor, 1000–60000 ms; larger `Retry-After` is retained | `2000` |

See [all execution settings](../examples/execution-config.json) for a complete example; its values are examples, not a list of defaults. Omitted fields keep defaults. Retries share these limits; traffic from other processes is not coordinated.

Token/cost reservations estimate input only and do not guarantee a provider billing cap. Set the unit price for your model/endpoint. If lowering `context.text_tokens_per_100_bytes`, keep it above tokens per 100 bytes measured from your own reported usage. Usage above a reservation stops further admission; unknown usage retains the cost reservation. The total deadline cannot interrupt synchronous PDF parsing/rendering.

<a id="resources"></a>

## Resource limits

Shared `resources` sets the defaults/ceilings below; values can only lower them and omitted fields keep defaults. `--resource-config FILE` replaces the whole section with JSON; `VOLTFILL_RESOURCE_CONFIG` is the fallback. PDF limits apply to local reads, uploads, inspection, extraction and rendering; render limits apply only with a renderer.

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

Limits must be positive; only `max_recovery_pages` allows zero. Parser-internal allocations are not fully bounded: PDF byte/page limits cannot bound all upstream stream decompression or predictor allocations. Untrusted PDFs can exhaust memory; VOLTFILL provides no process isolation.

<a id="rendering"></a>

## Rendering

Vision/Hybrid render page PNGs. Enable a renderer at build time and select `--renderer pdfium|hayro`. Default builds include neither renderer; no automatic fallback occurs. PDFium requires a caller-supplied native library; Hayro renders in Rust without one.

PDFium: build with `--features pdfium` and provide an absolute library path.

```sh
cargo build --workspace --features pdfium --locked
./target/debug/voltfill extract input.pdf vision-schema.json --remote \
  --strategy vision --pdfium-lib ABSOLUTE_PATH
```

CLI/server accept `VOLTFILL_PDFIUM_LIB_PATH` instead of `--pdfium-lib`. With no `--renderer`, that path selects PDFium; explicit `--renderer pdfium` still needs the path. Rendering defaults to 1.5 pixels/PDF point; override with `--pixels-per-point NUMBER`. Each image side must be 280–1568 pixels; out-of-range images are rejected before sending without automatic resizing.

Hayro requires its feature and explicit selection:

```sh
cargo build --workspace --features hayro --locked
./target/debug/voltfill extract input.pdf vision-schema.json --remote \
  --strategy vision --renderer hayro
```

Build both with `--features pdfium,hayro`. `--renderer hayro` ignores the PDFium environment variable and rejects combination with `--pdfium-lib`. Hayro rejects unavailable fonts and public interpreter warnings instead of substituting system fonts. Upstream may omit unsupported content without warning; not every omission is detectable, so inspect rendering before relying on it. Backends can support different content; source inspection and Evidence checks remain separate.

VOLTFILL neither downloads PDFium automatically nor performs OCR.

<a id="logging"></a>

## Logging

OTLP JSON logs default to off; use `--log-level off|error|warn|info|debug`. CLI logs use stderr, or stdout with `--output FILE`; server logs use stdout. Forwarding is caller-managed. Logs exclude bodies, credentials and HTTP headers, and never change returned response data.
