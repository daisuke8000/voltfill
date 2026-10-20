# 設定

[English](configuration.en.md) · 日本語 · [README](../README.ja.md)

<a id="launch-configuration"></a>

## 起動設定

```sh
voltfill run --config examples/voltfill.yaml
voltfill serve --config examples/voltfill.toml
```

YAML（`.yaml`／`.yml`）、TOML（`.toml`）、互換対応のJSONで設定項目は同じです。`-c` は `--config` の別名です。[YAML](../examples/voltfill.yaml)・[TOML](../examples/voltfill.toml)・[JSON](../examples/voltfill.json) の例は固定回答で、Jevに送信しません。同じディレクトリに `input.pdf` を置くか、CLIでパスを指定します。モックは出力形式の確認用で、任意の文書の精度は評価しません。

優先順位は **明示したCLI → 起動設定 → 環境変数 → 既定値**。`--config` 省略時は `VOLTFILL_CONFIG` を使います。ファイル内のパスは設定ディレクトリ基準、CLI・環境変数のパスは作業ディレクトリ基準です。選んだコマンドの `run` または `serve` だけを使います。

ファイルは明示指定、64 KiBまで。未知の項目・重複キー・不正な型はエラーで、YAMLのアンカー・エイリアス・マージ・includeは使えません。自動探索・環境変数展開・コマンド実行・`.env` 読み込みはしません。APIキーは設定に書かないでください。

実APIには `--remote` または起動設定の `remote: true` が必要で、環境変数だけでは有効にできません。`voltfill run -c FILE --dry-run` は無送信で入力を確認し、`serve` の起動だけでは送信しません。

<a id="environment"></a>

## 接続先と認証情報

文書・認証情報を渡す前に、HTTPSの接続先とDNS解決先を信頼できるか確認してください。TLS検証は有効で、リダイレクト、環境のプロキシ、HTTPクライアント独自の再試行は無効です。アップロードのoptionsでは接続先・認証情報・資源制限を変更できません。URL・認証情報はログと診断から除外します。

実APIの接続先に既定URLはなく、`VOLTFILL_JEV_ENDPOINT` で指定します。キーは `VOLTFILL_JEV_API_KEY` または `VOLTFILL_JEV_API_KEY_FILE` で渡します。認証ファイルはパスを明示します。値と対応する `_FILE` の同時指定はエラーです。[サーバー認証情報](server.md#authentication)にも同じ制限を適用します。

<a id="request-control"></a>

## 実行制限

共通の `execution` は送信・再試行・予算を制御します。`--execution-config FILE` で全体をJSONに置き換え、従来の実行制限だけのJSONは `--config` でも使えます。`VOLTFILL_EXECUTION_CONFIG` は明示した実行設定ファイルも起動設定の `execution` もない場合に使います。抽出用の `--max-concurrency`・`--max-attempts` はその後に適用します。

| 項目 | 意味 | 既定値 |
|---|---|---|
| `max_concurrency` | 同時にPOSTを送信するワーカー数。1以上。この数までのページを同時に描画して持つため、上げるとメモリも増える | `2` |
| `max_batch_questions` | 1バッチにまとめる質問数。1〜255 | `32` |
| `max_attempts` | 初回を含むリクエストごとの最大試行回数。1〜3 | `3` |
| `max_requests` | リトライを含む処理全体の最大POST試行回数 | `128` |
| `requests_per_second` | 1秒あたりのリクエスト数の上限 | `80` |
| `input_tokens_per_second` | 1秒あたりに予約する入力トークン数の上限 | `100000` |
| `total_deadline_ms` | 待機・リトライを含む処理全体の期限 | `30000` |
| `attempt_timeout_ms` | 1回のHTTP通信のタイムアウト | `10000` |
| `max_cost_nano_usd` | 推定費用の上限。単位は10億分の1米ドル | `200000000`（$0.20） |
| `input_token_price_nano_usd` | 入力1トークンの推定単価。1〜1000000000 nano-USD | `42` |
| `completion_gap_ms` | トークン予約・待機時間の下限。1000〜60000 ms。より大きい `Retry-After` は保持 | `2000` |

全項目は[実行設定例](../examples/execution-config.json)を参照してください。記載値は設定例で、既定値一覧ではありません。省略項目は既定値を保ちます。再試行も同じ制限を共有し、他プロセスの送信量とは調整しません。

トークン・費用の予約は入力だけの推定で、提供元の請求上限を保証しません。単価はモデル・接続先に合わせてください。`context.text_tokens_per_100_bytes` を下げる場合も、自分の受信usageから測った100バイトあたりのトークン数より大きくします。usageが予約量を超えると後続の送信受付を止め、不明なら費用予約を維持します。全体期限で同期的なPDF解析・描画は強制中断できません。

<a id="resources"></a>

## 資源制限

共通の `resources` で以下の既定値・上限を指定します。値は小さくでき、省略項目は既定値を保ちます。`--resource-config FILE` は全体をJSONで置き換え、`VOLTFILL_RESOURCE_CONFIG` はフォールバックです。PDF制限はローカル読み込み・アップロード・検査・抽出・描画に、描画制限はレンダラー選択時に適用します。

```yaml
resources:
  pdf:
    max_bytes: 16777216
    max_pages: 100
    max_items_per_page: 10000
    max_recovery_pages: 8
  max_render_pixels: 16000000
  max_render_output_bytes: 67108864
```

値は正の数とし、`max_recovery_pages` だけは0を許容します。解析内部のメモリ確保はすべて制限できず、PDFサイズ・ページ数では上流のストリーム展開・予測復元の割り当てを抑え切れません。信頼できないPDFでメモリを使い切る可能性があり、プロセス隔離は提供しません。

<a id="rendering"></a>

## 描画

Vision／HybridはページPNGを描画します。ビルド時にレンダラーを有効にし、`--renderer pdfium|hayro` で選びます。既定ビルドはどちらも含まず、自動切り替えはしません。PDFiumは利用側がネイティブライブラリを用意し、HayroはネイティブライブラリなしでRust描画します。

PDFiumは `--features pdfium` でビルドし、ライブラリの絶対パスを渡します。

```sh
cargo build --workspace --features pdfium --locked
./target/debug/voltfill extract input.pdf vision-schema.json --remote \
  --strategy vision --pdfium-lib ABSOLUTE_PATH
```

CLI・サーバーは `--pdfium-lib` の代わりに `VOLTFILL_PDFIUM_LIB_PATH` を使えます。`--renderer` 省略時はこのパスでPDFiumを選び、`--renderer pdfium` 明示時もパスが必要です。倍率は既定で1 PDF単位あたり1.5ピクセル、変更は `--pixels-per-point NUMBER` です。画像の各辺は280〜1568ピクセルで、範囲外は自動変形せず送信前に拒否します。

Hayroはfeatureと選択を明示します。

```sh
cargo build --workspace --features hayro --locked
./target/debug/voltfill extract input.pdf vision-schema.json --remote \
  --strategy vision --renderer hayro
```

両方のビルドは `--features pdfium,hayro` です。`--renderer hayro` はPDFium用環境変数を無視し、`--pdfium-lib` との併用を拒否します。Hayroは利用不可のフォントや公開されたインタープリター警告を拒否し、システムフォントでは置き換えません。上流が警告なしで非対応内容を省略する場合もあり、すべて検出できないため利用前に描画を確認してください。対応内容は描画エンジンで異なり、原文解析・Evidence検証は別に行います。

PDFiumの自動ダウンロードとOCRは行いません。

<a id="logging"></a>

## ログ

OTLP JSONログは既定で無効、切り替えは `--log-level off|error|warn|info|debug` です。CLIは標準エラー（`--output FILE` 指定時は標準出力）、サーバーは標準出力に出します。転送は利用側で管理します。本文・認証情報・HTTPヘッダーは含めず、返却データにも影響しません。
