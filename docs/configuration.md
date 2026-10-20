# 設定

[English](configuration.en.md) · 日本語 · [README](../README.ja.md)

<a id="launch-configuration"></a>

## 起動設定

```sh
voltfill run --config examples/voltfill.yaml
voltfill serve --config examples/voltfill.toml
```

[YAML](../examples/voltfill.yaml)・[TOML](../examples/voltfill.toml)・[JSON](../examples/voltfill.json) の例は固定回答で、Jevに送信しません。同じディレクトリに `input.pdf` を置くか、CLIで入力を指定します。

優先順位は **CLI → 指定ファイル → 環境変数 → 既定値**。`--config` 省略時は `VOLTFILL_CONFIG` を使います。ファイル内のパスは設定ディレクトリ基準、CLI・環境変数のパスは作業ディレクトリ基準です。

送信制限は [execution-config.json](../examples/execution-config.json) を参照してください。記載値は設定例で、既定値一覧ではありません。`--execution-config FILE` は `execution`、`--resource-config FILE` は `resources` 全体をJSONで置き換えます。

<a id="environment"></a>

## 実APIの利用

信頼できるHTTPS URLを `VOLTFILL_JEV_ENDPOINT` に指定し、`VOLTFILL_JEV_API_KEY` または `VOLTFILL_JEV_API_KEY_FILE` でキーを渡します。同時指定はエラーです。キーは設定に書かず、`.env` は読み込みません。

実APIには `--remote` または `remote: true` を明示します。`voltfill run -c FILE --dry-run` は無送信で入力を確認し、`serve` の起動だけでは送信しません。

<a id="rendering"></a>

## 描画

Vision／Hybridはビルド時にレンダラーを有効にし、明示的に選びます。自動切り替えはありません。

```sh
cargo install --path crates/voltfill-cli --locked --features pdfium,hayro
voltfill run input.pdf examples/vision-form.json --remote --strategy vision --renderer hayro
```

Hayroはネイティブライブラリ不要です。PDFiumは利用側が用意し、`--renderer pdfium` で選んで、`--pdfium-lib` または `VOLTFILL_PDFIUM_LIB_PATH` で絶対パスを渡します。自動ダウンロード・OCRは行いません。

CLI・サーバーの描画倍率は既定1.5ピクセル/PDF単位（`--pixels-per-point`）です。画像の各辺は280〜1568ピクセルで、自動変形しません。Hayroでは非対応の内容が警告なしで欠ける場合もあるため、描画結果を確認してください。

資源制限はPDF解析を隔離せず、すべてのメモリ確保は抑えられません。期限で同期解析・描画は強制中断できません。費用予約は入力だけの推定で、提供元の請求上限を保証しません。

ログは既定で無効、切り替えは `--log-level` です。CLIは標準エラー（`--output` 指定時は標準出力）、サーバーは標準出力に出し、本文・認証情報・HTTPヘッダーは含めません。
