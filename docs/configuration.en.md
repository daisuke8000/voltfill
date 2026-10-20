# Configuration

English · [日本語](configuration.md) · [README](../README.md)

<a id="launch-configuration"></a>

## Launch files

```sh
voltfill run --config examples/voltfill.yaml
voltfill serve --config examples/voltfill.toml
```

Use the [YAML](../examples/voltfill.yaml), [TOML](../examples/voltfill.toml) or [JSON](../examples/voltfill.json) examples. They use fixed answers without Jev access; place `input.pdf` beside the file or override it on the CLI.

Precedence: **CLI → selected file → environment → defaults**. `VOLTFILL_CONFIG` selects the file when `--config` is omitted. File paths are relative to the configuration directory; CLI/environment paths use the working directory.

For request limits, see [execution-config.json](../examples/execution-config.json); its values are examples, not defaults. `--execution-config FILE` replaces `execution`, and `--resource-config FILE` replaces `resources`, with JSON.

<a id="environment"></a>

## Remote access

Set a trusted HTTPS URL in `VOLTFILL_JEV_ENDPOINT` and inject `VOLTFILL_JEV_API_KEY` or `VOLTFILL_JEV_API_KEY_FILE`; setting both is an error. Keep keys out of configuration; `.env` files are not loaded.

Enable real API access explicitly with `--remote` or `remote: true`. Use `voltfill run -c FILE --dry-run` to check input without sending. Starting `serve` alone sends nothing.

<a id="rendering"></a>

## Rendering

Vision/Hybrid need a renderer enabled at build time and selected explicitly; there is no automatic fallback. For example:

```sh
cargo install --path crates/voltfill-cli --locked --features pdfium,hayro
voltfill run input.pdf examples/vision-form.json --remote --strategy vision --renderer hayro
```

Hayro needs no native library. For PDFium, select `--renderer pdfium` and supply an absolute library path through `--pdfium-lib` or `VOLTFILL_PDFIUM_LIB_PATH`. The library is caller-supplied and never downloaded automatically. OCR is unsupported.

CLI/server scale defaults to 1.5 pixels/PDF point (`--pixels-per-point`). Each image side must be 280–1568 pixels; images are not resized automatically. Hayro may omit unsupported content without a warning, so check rendered output.

Resource limits do not isolate PDF parsing or bound all memory; deadlines cannot interrupt synchronous parsing/rendering. Cost reservations estimate input only and do not guarantee a provider billing cap.

Logs default to off; enable with `--log-level`. CLI logs use stderr (stdout with `--output`); server logs use stdout. Bodies, credentials and HTTP headers are excluded.
