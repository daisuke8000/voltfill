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
| `text` | Selects candidates from source text | [Text](examples/text-form.json) / [Multiple matches](examples/text-multi-form.json) |
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

## Documentation

[Configuration](docs/configuration.en.md) · [HTTP server](docs/server.en.md) · [Development](docs/development.en.md) · [Benchmarks](docs/benchmarks.en.md)

[Architecture and C4](https://daisuke8000.github.io/voltfill/) · [MIT license](LICENSE)
