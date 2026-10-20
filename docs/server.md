# HTTPサーバー

[English](server.en.md) · 日本語 · [README](../README.ja.md)

APIなしの固定回答・ループバックでの確認は [READMEのクイックスタート](../README.ja.md#http-server) を参照してください。

<a id="remote"></a>

## Jevの利用

モックを停止し、`VOLTFILL_JEV_ENDPOINT` と、環境変数の値か明示したファイルでJev認証情報を渡し、`--remote` で起動します。

```sh
voltfill serve --remote --listen 127.0.0.1:3000
```

`/v1/extract` にmultipartの `pdf`・`schema` と任意のJSON `options` を送ります。既定はText-first・`jev-1.14.0` で、モデル省略時の既定値は起動時の `--model` で変更できます。例：`--form-string 'options={"strategy":"text_first","model":"jev-1.14.0"}'`。[評価](jev.md#evaluation)・[抽出結果](jev.md#output)を参照してください。`/health` はヘルスチェックです。

<a id="images"></a>

## VisionとHybrid

[レンダラー](configuration.md#rendering)を有効にし、同じアドレスのText-firstサーバーと切り替えます。実APIサーバーは起動時の `--allow-images` が必須で、省略すると画像方式はAPI実行前に拒否します。PDFiumの例です。

```sh
voltfill serve --remote --allow-images \
  --listen 127.0.0.1:3000 --pdfium-lib ABSOLUTE_PATH
```

Hayroは `--remote --allow-images --renderer hayro --listen 127.0.0.1:3000` です。起動時のレンダラーを全アップロードに適用し、multipartのoptionsからは変更できません。PDF・スキーマを送るとサーバーでページを描画します。

リクエストは `options.strategy: "vision_first"` と [vision-form.json](../examples/vision-form.json)、または `hybrid` と [hybrid-form.json](../examples/hybrid-form.json) を使います。HTTPの方式名は `text_first`・`vision_first`・`hybrid` です。

<a id="authentication"></a>

## 認証

`127.0.0.1` は同じマシンからだけ接続でき、`VOLTFILL_SERVER_API_KEY` 未設定なら認証は任意です。有効にするには起動前に変数を渡し、クライアントにも同じ値を渡します。例ではBearerヘッダーを標準入力からcurlに渡します。

```sh
printf 'Authorization: Bearer %s\n' "${VOLTFILL_SERVER_API_KEY}" |
  curl --fail-with-body --silent --show-error --header @- \
    --form 'pdf=@input.pdf;type=application/pdf' \
    --form 'schema=@schema.json;type=application/json' \
    http://127.0.0.1:3000/v1/extract
```

`VOLTFILL_SERVER_API_KEY_FILE` で認証ファイルを明示でき、[認証ファイルの制限](configuration.md#environment)が適用されます。`--listen 0.0.0.0:3000` など非ループバックではBearer認証必須です。`Origin` 付き抽出もループバックを含め認証設定が必要です。組み込みアプリはルーターの待ち受け・通信経路を保護してください。

別マシンからはTLS終端プロキシなどでHTTPSを用意し、そのHTTPS URLをcurlに使います。VOLTFILL自体はHTTPなので、プロキシからの経路もループバックなどで保護します。

<a id="admission"></a>

## アップロードと受付制御

アップロード期限は抽出期限とは別で、`--upload-timeout-ms` / `VOLTFILL_UPLOAD_TIMEOUT_MS` は1〜3600000 ms、既定30000です。[PDF資源制限](configuration.md#resources)も適用します。同時に受け付ける抽出は1件で、`max_concurrency` はその内部のJevワーカー数です。

実行中・待機中は `Retry-After` 付き `429 server_busy`、送信一時停止中は `503 remote_paused_unknown_outcome` を返し、抽出を始めません。拒否時は通常サイズ・期限内で読み捨てますが、上限超過の宣言や読み捨て中の拒否が多い場合はアップロード完了前に切断し得ます。`Expect: 100-continue` はアップロード前に応答します。
