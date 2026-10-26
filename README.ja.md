<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

[English](README.md) · 日本語

PDFから値・原文の根拠（Evidence）・API応答を返すRustライブラリ／CLI／Axumサーバーです。質問はJSONで指定し、PDFは変更しません。

## インストール

Rust 1.99.1で、リポジトリのルートから実行します。

```sh
cargo install --path crates/voltfill-cli --locked
```

Vision／Hybridには[レンダラーを有効にします](docs/configuration.md#rendering)。

## 基本の使い方

Jevに送信せず確認します。

```sh
voltfill inspect input.pdf
```

[接続先・キー](docs/configuration.md#environment)を設定し、PDFと抽出定義を渡します。

```sh
voltfill run input.pdf schema.json --remote -o result.json
```

既定はText-first。Hybridは `--strategy hybrid`、全オプションは `voltfill --help` を参照してください。

## 抽出方式

| 方式 | 動作 | スキーマ |
|---|---|---|
| `text` | 原文の候補から選択 | [Text](examples/text-form.json) |
| `vision` | 画像のBoxを原文に対応付け | [Vision](examples/vision-form.json) |
| `hybrid` | 文字候補とBoxを照合 | [Hybrid](examples/hybrid-form.json) |

既定モデルは `jev-1.14.0`、変更は `--model ID` です。Vision／Hybridは1.14.0以降のバージョン指定IDが必要です。

<a id="results"></a>

## 抽出結果

結果は `fields`・`evidence`・`usage`・`jev_responses` です。部分失敗でも値を保持するため、`status`・`reason_codes` を確認してください。再試行・エラーの受信本文も残し、未受信・不完全なら `body_complete: false` です。

原文・座標・型・validatorを検証し、既定ではconfidenceで足切りしません。結果は評価専用（`evaluation_only: true`）で、confidenceは校正済みの正答確率ではありません。

<a id="http-server"></a>

## HTTPサーバーのクイックスタート

Jevに送信しない固定回答モックを起動します。

```sh
voltfill serve --mock examples/text-mock.json --listen 127.0.0.1:3000
```

別ターミナルからPDFとスキーマを送ります。モックは形式確認用で、精度評価には使えません。

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/text-form.json;type=application/json'
```

## 基盤 crate

基盤 crate は抽出スキーマ・Evidenceなしで使えます。

| crate | 公開 API |
|---|---|
| `pdf-probe` | `inspect`、`DocumentSnapshot`、`PdfLimits`、`PdfError`、`InspectionTimings`、原文の `SourceFragment` / `PageDiagnostic` / `Quality`、`geometry`、`metadata` |
| `pdf-render` | `hayro` または `pdfium` 有効時の `render`、`RenderDocument`（1 回開いて複数ページを描く。検査と画像は `render` と同じ）、`RenderConfig`、`RenderBackend`、`RenderedImage`、`RenderError` |
| `jev-client` | `build_request`、`parse_request`、`parse_response`、`RequestDraft`、`Request`、`Content`、`Question`、`Asset`、`Limits`、`Answer`、`Response`、`Error`、`PngImage`、`Endpoint`、`HttpTransport`、`Secret`、`Policy`、`ExecutionConfig`、`RecordingTransport`、`ResponseRecord`、`ResponseBody`。`transport` の trait・共有予算・`execute` / `execute_observed` / `execute_deferred`、`planning`、`multipart` |
| `otel-log` | `TelemetryLogger`（呼び出し側が指定した `log` の target だけを、ローカルの OTLP JSON ログか、渡された `SdkLoggerProvider` に出す）、`opentelemetry_sdk` |

基盤はcoreに依存せず、coreが抽出結果・Evidence・全体計測を持ちます。既存APIは維持します。依存関係と契約は[構成図](docs/architecture.html)を参照してください。

描画は既定オフです。次はHayroでリクエストを構築する無送信例です。PDFiumの指定と画像制限は[描画設定](docs/configuration.md#rendering)を参照してください。

外部プロジェクトではpath依存を指定します。

```toml
[dependencies]
pdf-probe = { path = "/path/to/voltfill/crates/pdf-probe" }
pdf-render = { path = "/path/to/voltfill/crates/pdf-render", features = ["hayro"] }
jev-client = { path = "/path/to/voltfill/crates/jev-client" }
```

```rust
use std::collections::BTreeMap;
use jev_client::{Asset, Content, Limits, PngImage, Question, RequestDraft};

fn prepare(pdf: &[u8]) -> Result<jev_client::multipart::Body, Box<dyn std::error::Error>> {
    let doc = pdf_probe::inspect(pdf, pdf_probe::PdfLimits::default())?;
    let image = pdf_render::render(pdf, doc.pages[0].page, &pdf_render::RenderConfig::hayro())?;
    let png = PngImage::try_new("page".into(), image.png)?;
    let request = jev_client::build_request(RequestDraft {
        model: "jev-1.14.0".into(),
        state: Content::Text("Inspect <page>".into()),
        questions: BTreeMap::from([("visible".into(), Question::Noul {
            instructions: Content::Text("Is a red image visible?".into()), criteria: None,
        })]),
        assets: BTreeMap::from([("<page>".into(), Asset { part: "page".into() })]),
    }, Limits::default())?;
    Ok(jev_client::multipart::encode(&request, &[png], "example-boundary")?)
}
```

送信には許可・`Secret`・上限・`Clock` を指定します。`execute` / `execute_observed` を使い、`execute_deferred` なら枠の予約後に画像を準備し、失敗なら枠を返します。

全試行の原本文を解析前に残すには `RecordingTransport` を使います。ログは応答の代わりになりません。詳しくは[実行シーケンス](docs/architecture.html#sequence)を参照してください。

## ドキュメント

CI・依存監査・シークレット／ワークフロー検査は[開発ガイド](docs/development.md)を参照してください。

[設定](docs/configuration.md) · [HTTPサーバー](docs/server.md) · [開発](docs/development.md) · [ベンチマーク](docs/benchmarks.md)

[アーキテクチャ・C4](https://daisuke8000.github.io/voltfill/) · [MITライセンス](LICENSE)
