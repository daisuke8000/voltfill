# HTTPサーバー

[English](server.en.md) · 日本語 · [README](../README.ja.md)

[接続先・認証情報](configuration.md#environment)を設定し、モックを停止して実API用に起動します。

```sh
voltfill serve --remote --listen 127.0.0.1:3000
```

`POST /v1/extract` はmultipartの `pdf`・`schema` と任意のJSON `options`、`GET /health` はヘルスチェックです。[送信例](../README.ja.md#http-server)・[結果](../README.ja.md#results)も参照してください。

方式は `text_first` / `vision_first` / `hybrid`。Hybridスキーマならcurlに `--form-string 'options={"strategy":"hybrid"}'` を追加します。`options.model` は起動時の `--model` を上書きします。

Vision／Hybridは起動時に `--allow-images` と[レンダラー](configuration.md#rendering)を指定します（例：`--renderer hayro`）。描画はサーバー側で行い、アップロードからレンダラー・接続先・認証・資源制限は変更できません。

<a id="authentication"></a>

## 認証

`VOLTFILL_SERVER_API_KEY` / `VOLTFILL_SERVER_API_KEY_FILE` の一方を設定し、`Authorization: Bearer <key>` を送ります。認証は未設定のループバックでは任意、非ループバック待ち受け・`Origin` 付き抽出では必須です。

VOLTFILLはHTTPです。別マシンからはTLSプロキシを使い、VOLTFILLまでの経路も保護します。組み込み時も待ち受け・経路の保護が必要です。

抽出は同時1件。実行・待機中は `Retry-After` 付き `429 server_busy`、送信停止中は `503 remote_paused_unknown_outcome` です。アップロード期限は `--upload-timeout-ms` で指定します。

## 直前の文書の再利用

既定オフの `--reuse-document` / `serve.reuse_document: true` は、直前1件の本文・位置・描画画像をメモリに保持します。PDFバイト列と解析・描画設定が同じ次の要求で再利用します。

最後の利用終了からTTL経過後、要求がなくても破棄します。TTLは `--reuse-document-ttl-ms` / `serve.reuse_document_ttl_ms` / `VOLTFILL_REUSE_DOCUMENT_TTL_MS` で1〜3600000 ms（既定60000）を指定します。環境変数だけでは有効化できません。

PNGは合計 `max_concurrency × max_output_bytes` バイトまでで、評価中の画像とは別に保持します。本文・位置も保持し、キャッシュ不在時は1回追加コピーします。テキストの保持量に独自の上限はなく、画像上限にも含まれません。PDF上限は入力バイト・ページ・項目数だけを制限します。

内容が要求終了後も残り、応答速度や解析・描画回数から同一アップロードの存在が分かるため、単一テナント向けです。