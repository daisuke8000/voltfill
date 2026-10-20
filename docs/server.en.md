# HTTP server

English · [日本語](server.md) · [README](../README.md)

For fixed-answer loopback testing without API access, see the [README quick start](../README.md#http-server).

<a id="remote"></a>

## Use Jev

Stop the mock server, set `VOLTFILL_JEV_ENDPOINT`, and inject Jev credentials by environment value or explicitly selected file. Start with `--remote`:

```sh
voltfill serve --remote --listen 127.0.0.1:3000
```

Upload PDF/schema to `/v1/extract` as multipart `pdf` and `schema`, with optional JSON `options`. Default: Text-first and `jev-1.14.0`; startup `--model` sets the default for omitted request models. For example, use `--form-string 'options={"strategy":"text_first","model":"jev-1.14.0"}'`. See [evaluation](jev.en.md#evaluation) and [output](jev.en.md#output). `/health` checks health.

<a id="images"></a>

## Vision and Hybrid

Enable a [renderer](configuration.en.md#rendering), then replace the Text-first server on the same address. A remote server requires startup `--allow-images`; otherwise image-based requests are rejected before API execution. With PDFium:

```sh
voltfill serve --remote --allow-images \
  --listen 127.0.0.1:3000 --pdfium-lib ABSOLUTE_PATH
```

Hayro: `--remote --allow-images --renderer hayro --listen 127.0.0.1:3000`. Startup selects the renderer for every upload; multipart options cannot change it. The server renders the uploaded PDF.

For requests, use `options.strategy: "vision_first"` with [vision-form.json](../examples/vision-form.json), or `hybrid` with [hybrid-form.json](../examples/hybrid-form.json). HTTP names are `text_first`, `vision_first`, `hybrid`.

<a id="authentication"></a>

## Authentication

`127.0.0.1` accepts only same-machine connections; authentication is optional there when `VOLTFILL_SERVER_API_KEY` is unset. To enable it, inject that variable before startup and give the client the same value. This example sends the Bearer header to curl through stdin:

```sh
printf 'Authorization: Bearer %s\n' "${VOLTFILL_SERVER_API_KEY}" |
  curl --fail-with-body --silent --show-error --header @- \
    --form 'pdf=@input.pdf;type=application/pdf' \
    --form 'schema=@schema.json;type=application/json' \
    http://127.0.0.1:3000/v1/extract
```

`VOLTFILL_SERVER_API_KEY_FILE` can supply an explicit credential file; [credential-file rules](configuration.en.md#environment) apply. Non-loopback binding, e.g. `--listen 0.0.0.0:3000`, requires Bearer authentication. Extraction requests with `Origin` require configured authentication even on loopback. Embedding applications must secure their router’s listener and transport.

For other machines, provide HTTPS through a TLS-terminating proxy or equivalent and use its HTTPS URL in curl. VOLTFILL itself serves HTTP; protect the proxy-to-VOLTFILL path with loopback or an equivalent boundary.

<a id="admission"></a>

## Upload and admission

Upload time is separate from extraction deadlines: `--upload-timeout-ms` / `VOLTFILL_UPLOAD_TIMEOUT_MS` accepts 1–3600000 ms, default 30000. [PDF resource limits](configuration.en.md#resources) apply. The server admits one extraction job at a time; `max_concurrency` controls Jev workers inside it.

Busy/cooldown returns `429 server_busy` with `Retry-After`; paused remote work returns `503 remote_paused_unknown_outcome`, without starting extraction. Rejected uploads normally drain within size/deadline limits; oversized declarations or too many draining rejections may close the connection before upload completion. `Expect: 100-continue` receives the response before uploading.
