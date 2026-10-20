<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

[English](README.md) · 日本語

PDFから構造化データを抽出するRustライブラリ、CLI、Axumサーバーです。項目と質問をJSONで定義すると、抽出した値、根拠となる原文（Evidence）、受信したAPIレスポンスの本文を返します。PDFへの書き込みは行いません。

<a id="installation"></a>

## インストール

Rust 1.99.1が必要です。リポジトリのルートで実行します。

```sh
cargo install --path crates/voltfill-cli --locked
```

Vision／Hybridには[レンダラーを有効にします](docs/configuration.md#rendering)。

<a id="basic-usage"></a>

## 基本の使い方

APIに送信せず確認します。

```sh
voltfill inspect input.pdf
```

`input.pdf` は手元のPDF、`schema.json` は項目と質問を定義したJSONです。実APIを使うには、信頼できるJevのHTTPS URLを `VOLTFILL_JEV_ENDPOINT` に設定し、`VOLTFILL_JEV_API_KEY` または `VOLTFILL_JEV_API_KEY_FILE` でキーを渡します。

```sh
voltfill run input.pdf schema.json --remote
```

既定はText-firstです。Hybridは `--strategy hybrid`、ファイル出力は `-o result.json` を指定します。オプションはCLI・サーバーの `--help` で確認でき、従来の長いフラグ名・方式名も使えます。

<a id="extraction-strategies"></a>

## 抽出方式

| 方式 | 動作 | スキーマ例 |
|---|---|---|
| `text` | PDF原文からの候補選択 | [Text](examples/text-form.json) |
| `vision` | 画像内の領域（Box）とPDF原文の対応付け | [Vision](examples/vision-form.json) |
| `hybrid` | 文字候補とBoxの照合。配列出力では不一致でもBoxの原文断片を保持 | [Hybrid](examples/hybrid-form.json) |

CLI・Rust・HTTPのいずれも、Text-firstは文字、Visionはページ画像、Hybridは両方をJevへ送ります。Vision／HybridのPNG描画には任意のPDFiumまたはHayroを使います。既定ビルドはレンダラーを含まず、自動切り替えもしません。[描画の設定と制限](docs/configuration.md#rendering)を参照してください。

既定モデルは `jev-1.14.0` です。Vision／Hybridは1.14.0以降のバージョン指定IDが必要で、`jev-latest`・`jev-preview` はText-first専用です。[モデル指定](docs/jev.md#models)を参照してください。

<a id="http-server"></a>

## HTTPサーバーのクイックスタート

Jevに送信しない固定回答モックをループバックで起動します。

```sh
voltfill serve --mock examples/text-mock.json --listen 127.0.0.1:3000
```

別のターミナルから手元のPDFとスキーマ例を送ります。固定回答は出力形式の確認用で、任意のPDFの精度は評価しません。

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/text-form.json;type=application/json'
```

実API・画像・認証は[HTTPサーバー](docs/server.md)を参照してください。

<a id="documentation"></a>

## ドキュメント

[設定](docs/configuration.md) · [抽出と結果](docs/jev.md) · [HTTPサーバー](docs/server.md) · [開発](docs/development.md) · [ベンチマーク](docs/benchmarks.md)

[アーキテクチャ・C4](https://daisuke8000.github.io/voltfill/) · [MITライセンス](LICENSE)
