# HTTPサーバー

[English](server.en.md) · 日本語 · [README](../README.ja.md)

[接続先・認証情報](configuration.md#environment)を設定し、モックを停止して実API用に起動します。

```sh
voltfill serve --remote --listen 127.0.0.1:3000
```

`POST /v1/extract` にmultipartの `pdf`・`schema` と任意のJSON `options` を送ります。`GET /health` はヘルスチェックです。[送信例](../README.ja.md#http-server)・[抽出結果](../README.ja.md#results)を参照してください。

HTTPの方式名は `text_first`・`vision_first`・`hybrid` です。例：Hybridスキーマを使い、curlに `--form-string 'options={"strategy":"hybrid"}'` を追加します。`options.model` は起動時の `--model` の既定値を上書きします。

Vision／Hybridは起動時の `--allow-images` と、`--renderer hayro` などの[レンダラー](configuration.md#rendering)も必要です。サーバーがPDFを描画し、アップロードからレンダラー・接続先・認証情報・資源制限は変更できません。

<a id="authentication"></a>

## 認証

`VOLTFILL_SERVER_API_KEY` または `VOLTFILL_SERVER_API_KEY_FILE` を設定し（同時指定不可）、クライアントから `Authorization: Bearer <key>` を送ります。未設定のループバックでは認証は任意で、非ループバックの待ち受け・`Origin` 付き抽出では必須です。

VOLTFILL自体はHTTPです。別マシンからはTLSプロキシでHTTPSを用意し、VOLTFILLまでの経路もループバックなどで保護します。組み込みアプリは待ち受け・通信経路を保護してください。

抽出は同時に1件です。実行中・待機中は `Retry-After` 付き `429 server_busy`、送信一時停止中は `503 remote_paused_unknown_outcome` を返します。アップロード期限は `--upload-timeout-ms` で変更できます。
