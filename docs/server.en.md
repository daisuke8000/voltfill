# HTTP server

English · [日本語](server.md) · [README](../README.md)

Configure the [endpoint and credentials](configuration.en.md#environment), stop the mock server, then start remote extraction:

```sh
voltfill serve --remote --listen 127.0.0.1:3000
```

`POST /v1/extract` accepts multipart `pdf`, `schema` and optional JSON `options`. `GET /health` checks health. See the [upload example](../README.md#http-server) and [results](../README.md#results).

HTTP strategy names are `text_first`, `vision_first`, `hybrid`. For example, append `--form-string 'options={"strategy":"hybrid"}'` to curl with the Hybrid schema. `options.model` overrides the startup `--model` default.

Vision/Hybrid also need startup `--allow-images` and a [renderer](configuration.en.md#rendering), such as `--renderer hayro`. The server renders the PDF; uploads cannot change renderer, endpoint, credentials or resource policy.

<a id="authentication"></a>

## Authentication

Set `VOLTFILL_SERVER_API_KEY` or `VOLTFILL_SERVER_API_KEY_FILE` (not both); send `Authorization: Bearer <key>` from clients. Authentication is optional on loopback when unconfigured, and required for non-loopback binding or extraction with `Origin`.

VOLTFILL serves HTTP. For other machines, provide HTTPS via a TLS proxy and protect its connection to VOLTFILL with loopback or an equivalent boundary. Embedding applications must secure their listener and transport.

One extraction job runs at a time. Busy/cooldown returns `429 server_busy` with `Retry-After`; paused remote work returns `503 remote_paused_unknown_outcome`. Change the upload deadline with `--upload-timeout-ms`.

## Reusing the previous document

Off by default. With `--reuse-document` (launch file: `serve.reuse_document: true`), the server keeps the last PDF's text, positions and rendered page images in memory and reuses them for the next request whose PDF bytes and every inspection and render setting are identical. The copy is dropped once `--reuse-document-ttl-ms` (1 to 3600000, default 60000; `serve.reuse_document_ttl_ms`, `VOLTFILL_REUSE_DOCUMENT_TTL_MS`) has passed since the last request that used it finished, even when no request arrives. Only the flag or launch-file key enables it; the environment variable sets just the TTL. Cached images are capped at `max_concurrency` (from the execution config) x the render `max_output_bytes` bytes of PNG in total, held in addition to the images of the window being evaluated. The text and position snapshot is held too, copied once more while a request misses the cache; it has no byte cap of its own (the PDF limits bound input bytes, pages and items, not the retained text size) and is not counted in the image cap. While enabled, document content stays in server memory after a request ends, and the response speed and report timings (including the parse and render counts) reveal a recent identical upload, so use it on single-tenant servers only.
