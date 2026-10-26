# 設定

[English](configuration.en.md) · 日本語 · [README](../README.ja.md)

<a id="launch-configuration"></a>

## 起動設定

```sh
voltfill run --config examples/voltfill.yaml
voltfill serve --config examples/voltfill.yaml
```

[YAML例](../examples/voltfill.yaml)は固定回答で無送信です。同じディレクトリに `input.pdf` を置くか、CLIで指定します。TOML・JSONも使えます。

優先順位は **CLI → 指定ファイル → 環境変数 → 既定値**。`--config` 省略時は `VOLTFILL_CONFIG` を使います。相対パスの基準は、ファイル内なら設定ディレクトリ、それ以外は作業ディレクトリです。

`completion_gap_ms` は1秒〜上限、`max_completion_gap_ms` は最大600秒です。`completion_gap_backoff` は1〜16（既定2、1は固定）。送信制限の[設定例](../examples/execution-config.json)は既定値一覧ではありません。`--execution-config FILE` / `--resource-config FILE` は `execution` / `resources` 全体をJSONで置き換えます。

<a id="environment"></a>

## 実APIの利用

接続先は `VOLTFILL_JEV_ENDPOINT`（信頼できるHTTPS）、キーは `VOLTFILL_JEV_API_KEY` / `VOLTFILL_JEV_API_KEY_FILE` の一方だけで指定します。設定ファイルにキーは書けず、`.env` も読みません。

送信には `--remote` / `remote: true` が必須です。`voltfill run -c FILE --dry-run` と `serve` の起動だけでは送信しません。

<a id="rendering"></a>

## 描画

Vision／Hybridはレンダラーのビルドと選択が必要です。自動切り替えはありません。

```sh
cargo install --path crates/voltfill-cli --locked --features pdfium,hayro
voltfill run input.pdf examples/vision-form.json --remote --strategy vision --renderer hayro
```

Hayroはネイティブライブラリ不要。PDFiumは利用側で用意し、`--renderer pdfium` と `--pdfium-lib` / `VOLTFILL_PDFIUM_LIB_PATH` の絶対パスで指定します。自動取得・OCRは行いません。

描画倍率は既定1.5ピクセル/PDF単位（`--pixels-per-point`）、各辺280〜1568ピクセルで自動変形しません。Hayroは無警告で非対応内容が欠ける場合があり、描画確認が必要です。

資源制限はPDF解析を隔離せず、全メモリ確保を抑えられません。同期解析・描画は期限で強制中断できません。費用予約も入力の推定で、請求上限ではありません。

ログは既定オフ（`--log-level`）。CLIは標準エラー、`--output` 指定時とサーバーは標準出力です。本文・認証情報・HTTPヘッダーは出しません。