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
