<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

English · [日本語](README.ja.md)

A Rust library, CLI and Axum server for extracting structured values from PDFs. Define fields and questions in JSON; receive values, source Evidence and original received API response bodies. The PDF stays unchanged.

<a id="installation"></a>

## Installation

Requires Rust 1.99.1. From the repository root:

```sh
cargo install --path crates/voltfill-cli --locked
```

For Vision/Hybrid, [enable a renderer](docs/configuration.en.md#rendering).

<a id="basic-usage"></a>

## Basic usage

Inspect locally without API access:

```sh
voltfill inspect input.pdf
```

Use your PDF as `input.pdf` and a JSON field/question definition as `schema.json`. For remote extraction, set a trusted Jev HTTPS URL in `VOLTFILL_JEV_ENDPOINT` and inject the key through `VOLTFILL_JEV_API_KEY` or `VOLTFILL_JEV_API_KEY_FILE`:

```sh
voltfill run input.pdf schema.json --remote
```

Text-first is the default. Use `--strategy hybrid` for Hybrid and `-o result.json` for file output. CLI/server `--help` lists options; existing long-form flags and strategy names remain supported.

<a id="extraction-strategies"></a>

## Extraction strategies

| Strategy | Behavior | Schema example |
|---|---|---|
| `text` | Selects candidates from PDF source text | [Text](examples/text-form.json) |
| `vision` | Maps image Boxes to PDF source text | [Vision](examples/vision-form.json) |
| `hybrid` | Compares text selection and image Boxes; array output retains Box runs on disagreement | [Hybrid](examples/hybrid-form.json) |

Across CLI, Rust and HTTP, Text-first sends text, Vision sends page images, and Hybrid sends both to Jev. Vision/Hybrid render PNGs with optional PDFium or Hayro; default builds have no renderer and never switch automatically. See [renderer setup and limitations](docs/configuration.en.md#rendering).

The default model is `jev-1.14.0`. Vision/Hybrid require a versioned ID ≥ 1.14.0; `jev-latest` and `jev-preview` work only with Text-first. See [model selection](docs/jev.en.md#models).

<a id="http-server"></a>

## HTTP quick start

Start a fixed-answer mock on loopback without Jev access:

```sh
voltfill serve --mock examples/text-mock.json --listen 127.0.0.1:3000
```

From another terminal, upload your PDF and the example schema. Fixed answers demonstrate output format, not accuracy on arbitrary PDFs.

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/text-form.json;type=application/json'
```

See [HTTP server](docs/server.en.md) for real API access, images and authentication.

<a id="documentation"></a>

## Documentation

[Configuration](docs/configuration.en.md) · [Extraction and results](docs/jev.en.md) · [HTTP server](docs/server.en.md) · [Development](docs/development.en.md) · [Benchmarks](docs/benchmarks.en.md)

[Architecture and C4](https://daisuke8000.github.io/voltfill/) · [MIT license](LICENSE)
