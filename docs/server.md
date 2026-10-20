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

## 直前の文書の再利用

既定はオフです。`--reuse-document`（起動ファイルでは `serve.reuse_document: true`）を付けると、直前の1件のPDFの本文・位置情報と描画済みページ画像をメモリに保持し、PDFのバイト列と解析・描画の設定がすべて同じ次のリクエストで再利用します。最後にその文書を使ったリクエストが終了してから `--reuse-document-ttl-ms`（1〜3600000、既定60000。`serve.reuse_document_ttl_ms`、`VOLTFILL_REUSE_DOCUMENT_TTL_MS`）が過ぎると、リクエストがなくても破棄します。有効にできるのはフラグ／起動ファイルのキーだけで、環境変数はTTLのみを設定します。保持する画像はPNGの合計で最大 `max_concurrency`（実行設定の値）× 描画の `max_output_bytes` バイトで、評価中のウィンドウの画像とは別に保持されます。本文・位置情報のスナップショットも保持し、キャッシュにない場合は追加で1回コピーされます。これには独自のバイト上限がなく（PDF上限は入力バイト数・ページ数・項目数を制限するだけで、保持するテキストの大きさは制限しません）、画像の上限にも含まれません。有効時は、文書の内容がリクエスト終了後もサーバーのメモリに残り、直近の同一アップロードがあると応答の速さやレポートのタイミング（解析・描画の回数を含む）から分かるため、単一テナントのサーバー向けです。
