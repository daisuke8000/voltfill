<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

[English](README.md) · 日本語

PDFから構造化データを抽出するRustライブラリ、CLI、Axumサーバーです。項目と質問をJSONで定義すると、抽出した値、根拠となる原文（Evidence）、受信したAPIレスポンスの本文を返します。PDFへの書き込みは行いません。

## インストール

Rust 1.99.1が必要です。リポジトリのルートで実行します。

```sh
cargo install --path crates/voltfill-cli --locked
```

Vision／Hybridには[レンダラーを有効にします](docs/configuration.md#rendering)。

## 基本の使い方

Jevに送信せず確認します。

```sh
voltfill inspect input.pdf
```

実APIを使うには、信頼できるJevのHTTPS URLを `VOLTFILL_JEV_ENDPOINT` に設定し、`VOLTFILL_JEV_API_KEY` または `VOLTFILL_JEV_API_KEY_FILE` でキーを渡します。`input.pdf` は手元のPDF、`schema.json` は項目と質問を定義したJSONです。

```sh
voltfill run input.pdf schema.json --remote -o result.json
```

既定はText-first、Hybridは `--strategy hybrid` です。オプションは `voltfill --help` を参照してください。

## 抽出方式

| 方式 | 動作 | スキーマ |
|---|---|---|
| `text` | 原文の候補から選択 | [Text](examples/text-form.json) / [複数取得](examples/text-multi-form.json) |
| `vision` | 画像のBoxを原文に対応付け | [Vision](examples/vision-form.json) |
| `hybrid` | 文字候補とBoxを照合 | [Hybrid](examples/hybrid-form.json) |

既定モデルは `jev-1.14.0`、変更は `--model ID` です。Vision／Hybridは1.14.0以降のバージョン指定IDが必要です。

<a id="results"></a>

## 抽出結果

抽出レポートは `fields`・`evidence`・`usage`・`jev_responses` を含みます。部分失敗や曖昧な結果でも値を含む場合があるため、`status`・`reason_codes` を確認してください。再試行・エラーを含む各試行の受信本文を保ち、`body_complete: false` は未受信または不完全な本文を示します。

既定ではconfidenceで足切りせず解決済みの値を返し、原文・座標・型・validatorを検証します。結果は `evaluation_only: true` で、confidenceは校正済みの正答確率ではありません。

<a id="http-server"></a>

## HTTPサーバーのクイックスタート

Jevに送信しない固定回答モックを起動します。

```sh
voltfill serve --mock examples/text-mock.json --listen 127.0.0.1:3000
```

別のターミナルから手元のPDFとスキーマ例を送ります。モックは出力形式の確認用で、抽出精度は評価しません。

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/text-form.json;type=application/json'
```

## ドキュメント

[設定](docs/configuration.md) · [HTTPサーバー](docs/server.md) · [開発](docs/development.md) · [ベンチマーク](docs/benchmarks.md)

[アーキテクチャ・C4](https://daisuke8000.github.io/voltfill/) · [MITライセンス](LICENSE)
