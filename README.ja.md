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

## 基盤 crate

基盤 crate は抽出スキーマや Evidence を必須とせずに利用できます。サポート対象の公開 API は次のとおりです。

| crate | 公開 API |
|---|---|
| `pdf-probe` | `inspect`、`DocumentSnapshot`、`PdfLimits`、`PdfError`、`InspectionTimings`、原文の `SourceFragment` / `PageDiagnostic` / `Quality`、`geometry`、`metadata` |
| `pdf-render` | `hayro` または `pdfium` 有効時の `render`、`RenderDocument`（1 回開いて複数ページを描く。検査と画像は `render` と同じ）、`RenderConfig`、`RenderBackend`、`RenderedImage`、`RenderError` |
| `jev-client` | `build_request`、`parse_request`、`parse_response`、`RequestDraft`、`Request`、`Content`、`Question`、`Asset`、`Limits`、`Answer`、`Response`、`Error`、`PngImage`、`Endpoint`、`HttpTransport`、`Secret`、`Policy`、`ExecutionConfig`、`RecordingTransport`、`ResponseRecord`、`ResponseBody`。`transport` の trait・共有予算・`execute` / `execute_observed` / `execute_deferred`、`planning`、`multipart` |

`voltfill-core` は3つすべてに依存し、`pdf-render` は `pdf-probe` にも依存します。基盤から core への依存はなく、`jev-client` は PDF 系にも依存しません。Evidence、抽出フィールド・結果、全体の `Timings` / `UsageSummary`、field/page を付けた `JevResponse` は core が持ちます。PDF 調査は独立した `InspectionTimings` を返します。既存の core の利用パス（`pdf`・`geometry`・`render`・`jev`・`model`）は再公開と互換アダプターで維持し、field/page 付き observer API は `voltfill_core::jev::transport` に残します。

default feature はすべて空です。次の例には `pdf-render/hayro` を有効にします。PDFium なら `pdf-render/pdfium` を有効にし、呼び出し元が用意したライブラリの絶対パスを渡します。core は両 feature を転送し、バックエンドの選択と native library の扱いは従来どおりです。ページから作る PNG の各辺は Jev の280〜1568ピクセルに収める必要があります。この例は構築のみで、送信しません。

独立したプロジェクトからソース checkout を path 依存として指定できます。

```toml
[dependencies]
pdf-probe = { path = "/path/to/voltfill/crates/pdf-probe" }
pdf-render = { path = "/path/to/voltfill/crates/pdf-render", features = ["hayro"] }
jev-client = { path = "/path/to/voltfill/crates/jev-client" }
```

```rust
use std::collections::BTreeMap;
use jev_client::{Asset, Content, Limits, PngImage, Question, RequestDraft, build_request};

fn prepare(pdf: &[u8]) -> Result<jev_client::multipart::Body, Box<dyn std::error::Error>> {
    let doc = pdf_probe::inspect(pdf, pdf_probe::PdfLimits::default())?;
    let image = pdf_render::render(pdf, doc.pages[0].page, &pdf_render::RenderConfig::hayro())?;
    let png = PngImage::new("page".into(), image.png)?;
    let request = build_request(RequestDraft {
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

送信には、明示的な許可、呼び出し元が注入した `Secret`、検証済みの上限、`Clock` とともに `transport::execute` または `execute_observed` を使います。`execute_deferred` は、送信枠を確保してからページ画像を作り、作れなかったときは送らずに枠を返します。`RecordingTransport::new(&mut transport, |record| records.push(record))` で包むと、受信本文と機密情報を含まない通信結果を保持できます。sink は解析と observer の前に、エラーや不完全な本文を含む各 send で1回、完了順に呼ばれます。複数の呼び出し・worker で共有するときは sink でリクエストの文脈を付けます。認証情報や HTTP ヘッダーは渡しません。ログ target は互換用の `voltfill_core::jev` を維持し、ログは応答データの代わりにはしません。

[独立した consumer](tests/foundation-consumer/src/main.rs) は基盤3 crate と画像だけの合成 PDF を使用します。`python3 tests/run-foundation-consumer.py` で、workspace 外へコピーし、依存グラフと固定済み外部依存のバージョンを確認して、Hayro で無送信の検証を実行し、一時ビルド出力を削除します。native の検証には `--pdfium-library /absolute/path/to/library` を指定します。ライブラリは自動取得しません。OCR は対象外で、抽出レポートは従来どおり評価専用です。

## ドキュメント

[設定](docs/configuration.md) · [HTTPサーバー](docs/server.md) · [開発](docs/development.md) · [ベンチマーク](docs/benchmarks.md)

[アーキテクチャ・C4](https://daisuke8000.github.io/voltfill/) · [MITライセンス](LICENSE)
