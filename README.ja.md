<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

[English](README.md) · 日本語

PDFから構造化データを抽出するRustライブラリ、CLI、Axumサーバーです。項目と質問をJSONで定義すると、抽出した値、根拠となる原文（Evidence）、受信したAPIレスポンスの本文を返します。PDFへの書き込みは行いません。

> [!IMPORTANT]
> **Jevのモデル指定**：既定は `jev-1.14.0` です。Vision／Hybridでは、1.14.0以降のバージョンを指定したIDが必要です。`jev-latest`・`jev-preview` はText-firstでのみ使えます。

> [!NOTE]
> **フォント解析ライブラリの保守状況**：`pdf-inspector` の依存先である `ttf-parser 0.25.1` には、[保守状況に関する警告](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)があります。この警告は、脆弱性の発見を知らせるものではありません。対応方針は[依存ライブラリの更新](#依存ライブラリの更新)を参照してください。

## インストール

必要環境：Rust 1.99.1、Edition 2024。CLIとHTTPサーバーは、1つの `voltfill` コマンドで実行できます。

```sh
cargo install --path crates/voltfill-cli --locked --features pdfium,hayro
```

リポジトリのルートで実行してください。レンダラーが不要なら `--features pdfium,hayro` は省略できます。通常、実行ファイルは `$HOME/.cargo/bin` にインストールされます。シェルから見つかることを確認します。

```sh
command -v voltfill
voltfill --help
```

インストール後も見つからない場合は、CargoのbinディレクトリをPATHに追加します。

```sh
export PATH="$HOME/.cargo/bin:$PATH"
```

zshでは同じ行を `~/.zshrc` に追加すると、新しいターミナルでも有効になります。`--root`・`CARGO_INSTALL_ROOT`・`CARGO_HOME` でインストール先を変更している場合は、その配下の `bin` を指定してください。

## 基本の使い方

まず、APIに送信せずPDFを確認します。

```sh
voltfill inspect input.pdf
```

`input.pdf` は手元のPDF、`schema.json` は項目と質問を定義したJSONです。既定の抽出方式はText-firstです。

実APIを使う場合は、信頼できるJevのHTTPS URLを `VOLTFILL_JEV_ENDPOINT` に指定し、`VOLTFILL_JEV_API_KEY` または `VOLTFILL_JEV_API_KEY_FILE` でAPIキーを渡します。

```sh
voltfill run input.pdf schema.json --remote
```

通常はconfidenceで値を除外せず、解決できた値を返します。原文・座標・型・validatorの検証は引き続き適用します。confidenceは値の正しさを保証する校正済み確率ではありません。

confidenceの評価は `evaluation_profile` を明示した場合だけ有効です。実APIでの通常の抽出にはローカル合成テスト用の `--evaluation-profile synthetic` を指定しません。

profile省略時のNoulの真偽値変換は、0.5以上がtrue、0.5未満がfalseです。Vision／Hybridでは存在回答がfalseでもBox質問を省略しません。存在回答による省略はprofileを明示した場合だけ行います。

Hybridで抽出する場合は `--strategy hybrid`、結果をファイルに保存する場合は `-o result.json` を指定します。オプション一覧はCLI・サーバーの `--help` で確認できます。従来の長いフラグ名・方式名も使用できます。

## 起動設定

設定ファイルを使う場合の起動例です。

```sh
voltfill run --config examples/voltfill.yaml
voltfill serve --config examples/voltfill.toml
```

`run` は `extract` と同じ抽出処理です。`serve` は単独実行用の `voltfill-server` と同じHTTPサーバーをプロセス内で起動します。別の実行ファイルを探したり、外部コマンドとして起動したりしません。レンダラー不要ならインストール時の `--features` を省略できます。`extract` サブコマンドと単独実行用の `voltfill-server` も使用できます。

起動設定はYAML（`.yaml`／`.yml`）とTOML（`.toml`）を標準対応とし、JSONも受け付けます。3形式で設定項目は同じです。例の [YAML](examples/voltfill.yaml)・[TOML](examples/voltfill.toml)・[JSON](examples/voltfill.json) は固定回答を使い、Jevに送信しません。`-c` は `--config` の短縮形です。

設定ファイルと同じディレクトリに`input.pdf`を用意するか、CLIで入力パスを指定してください。モックの固定回答は出力形式を確認するためのもので、任意のPDFの精度は評価できません。

```yaml
version: 1
run:
  pdf: input.pdf
  schema: hybrid-schema.json
  output: result.json
  remote: true
  strategy: hybrid
  model: jev-1.14.0
  renderer: pdfium
  pdfium_lib: /absolute/path/libpdfium.dylib
  pixels_per_point: 1.5
serve:
  remote: true
  allow_images: true
  renderer: pdfium
  pdfium_lib: /absolute/path/libpdfium.dylib
  pixels_per_point: 1.5
  listen: 127.0.0.1:3000
execution:
  max_concurrency: 2
  max_batch_questions: 10
  max_attempts: 3
```

`run` と `serve` は指定したコマンドの設定だけを使います。`execution` はページ・質問バッチ・送信頻度・並列数・再試行・トークン量・予算の既存の実行設定です。設定の優先順位は **明示したCLI引数 → 設定ファイル → 環境変数 → 既定値**。`--execution-config FILE` は `execution` セクション全体を上書きし、CLIの `--max-attempts`／`--max-concurrency` はその後に適用します（抽出用）。ファイル内のパスは設定ファイルのディレクトリ基準、CLI引数と環境変数のパスは実行時のディレクトリ基準です。`--config` を省略すると `VOLTFILL_CONFIG` のファイルを使います。

設定ファイルはフラグまたは対応する環境変数で明示したものだけを読み込みます。自動探索・別ファイルの取り込み・環境変数の展開・設定からのコマンド実行は行いません。APIキーは設定に書かず、従来どおり実行環境から渡します。`remote: true` は実APIの利用指定なので、送信せず入力を確認する場合は `voltfill run -c FILE --dry-run` を使ってください。`serve` の起動だけではJevに送信しません。HTTPアップロードの方式とモデルは、引き続きリクエストの `options` で指定します。モデルを省略したリクエストには起動時の既定モデルを使います。

ファイルは64 KiBまで。未知の項目・重複キー・型の不一致はエラーにします。YAMLのアンカー・エイリアス・マージ・includeは受け付けず、入れ子とノード数も制限します。従来の実行制限だけのJSON設定も、引き続き `--config` または `--execution-config` で使えます。

## 抽出方式

| 方式 | 動作 | スキーマ例 |
|---|---|---|
| `text` | PDF原文からの候補選択 | [Text](examples/text-form.json) |
| `vision` | 画像内の領域（Box）とPDF原文の対応付け | [Vision](examples/vision-form.json) |
| `hybrid` | 文字候補とBoxの照合。配列出力では不一致でもBoxの原文断片を保持 | [Hybrid](examples/hybrid-form.json) |

CLI・ライブラリ・HTTPサーバーのいずれでも、Text-firstは文字情報、VisionはPDFページの画像、Hybridは文字情報とページ画像をJevへ送ります。

Vision／Hybridでは、PDFからページ画像をPNG形式で描画します。ビルド時に任意のレンダラーを有効にし、`--renderer pdfium|hayro` で選択できます。PDFiumのライブラリは利用側で用意します。HayroはネイティブライブラリなしでRustによる描画を行います。既定のビルドはレンダラーを含まず、別のレンダラーへの自動切り替えも行いません。

PDFiumを使う場合は、`--features pdfium` を付けてビルドし、ライブラリの絶対パスを指定してください。

```sh
cargo build --workspace --features pdfium --locked
./target/debug/voltfill extract input.pdf vision-schema.json --remote \
  --strategy vision --pdfium-lib ABSOLUTE_PATH
```

CLI・サーバーでは、`--pdfium-lib` の代わりに環境変数 `VOLTFILL_PDFIUM_LIB_PATH` でも指定できます。`--renderer` を省略した場合は、従来どおりこのパス指定でPDFiumを使います。`--renderer pdfium` を明示した場合もパスが必要です。

CLI・サーバーの描画倍率は既定で1 PDF単位あたり1.5ピクセルです。`--pixels-per-point 数値` で変更できます。画像の各辺は280〜1568ピクセルが必要で、範囲外なら自動変形せず送信前に拒否します。

Hayroを使う場合は、対応するfeatureとレンダラーを明示します。

```sh
cargo build --workspace --features hayro --locked
./target/debug/voltfill extract input.pdf vision-schema.json --remote \
  --strategy vision --renderer hayro
```

`--features pdfium,hayro` で両方をビルドできます。`--renderer hayro` はPDFium用の環境変数を参照せず、`--pdfium-lib` との同時指定は拒否します。Hayroは利用できないフォントを必要とするページや、公開の警告通知を受けたページを拒否し、システムフォントで置き換えません。上流実装が警告通知なしで描画を省略する場合もあり、すべての欠落は検出できません。利用するPDFの描画結果を確認してください。描画できる内容はレンダラーによって異なりますが、原文の解析とEvidenceの検証は別に行います。Rust APIでは `RenderConfig::new(path)` または `RenderConfig::hayro()` を指定します。

PDFiumは利用側で用意します。自動ダウンロードやOCRは行いません。

## 複数の原文断片

`text_in_box`・`selected_candidate`・`selected_candidates` で `"output_type": "string_array"` を指定できます。たとえば次のフィールド設定では、複数の原文断片を結合せず、別々の文字列として返します。

```json
{
  "output_type": "string_array",
  "normalization": "trim",
  "validators": [{"kind": "non_empty"}, {"kind": "max_chars", "count": 200}]
}
```

`text_in_box` は、矩形がBoxと正の面積で重なるusableな原文断片をすべて配列要素にし、PDF原文の `item_index` 順で返します。見た目の読み順の推測、重なり率の足切り、文字列の切り出し、原文断片の連結、欠けた文字の補完は行いません。原文断片全体を返すため、Box外の文字も要素に含まれる場合があります。各要素の元の文字列と座標はEvidenceに保持します。

`selected_candidate` は選択した原文文字列を1要素の配列で返します。既存の `string` 型の契約は変わりません。

Text-firstで質問に合う原文を複数取得する場合は、[`examples/text-multi-form.json`](examples/text-multi-form.json) の `"resolver": {"kind": "selected_candidates", "question_id": "pick", "max_selections": 8}` を使います。`string_array`、依存関係のないText Choice質問1つ、動的な `text_spans`、**ページごと**の取得上限 `max_selections`（1〜254の明示指定）が必要です。Vision／Hybridでは使えません。全文を求めるか、一部に絞るかは質問で指定します。

質問文を変えず、選択済みIDを選択肢から除いて、Jevが `none` を選ぶか全候補を取得するまで繰り返します。配列は原文の順で返し、同文でも位置が違えば別要素として保持します。

候補数上限は除外前の全候補に適用します。各回は通常のバッチ・リクエスト・再試行・トークン・費用・期限の全体制限を共有します。未選択候補を残して取得上限に達した場合は `ambiguous` / `selection_limit_reached` とし、エラーや予算切れでも取得済みの値とEvidenceを保持します。

Choiceの確率値は受信したまま保持します。合計0.99などは診断情報として示し、拒否や勝手な正規化はしません。Jevの選択を最大確率の候補に置き換えません。同点や最大確率でない選択は `ambiguous` と理由で示し、解決できた値とEvidenceを保持します。複数取得はその場合も継続し、APIが `none` を選ぶか実行上限に達するまで取得します。

confidenceのprofileを明示した場合も、解決済みの原文値を削除せず判定結果を付けます。型・範囲・候補の存在は検証し、解決できない回答は値を作らず元レスポンスを保持します。Scoreも確率合計は診断情報としますが、legend・範囲・期待scoreの検証は維持します。

配列の検証ルールは全ページの結果を集めた後に適用します。検証エラーや別ページの失敗があっても、取得済みの配列とEvidenceを保持します。成功の判定には値の有無に加えて `status` と `reason_codes` を確認してください。

正規化は要素ごとに適用し、元のEvidenceは変更しません。`non_empty` は空配列と空文字・空白だけの要素を拒否します。`max_chars` は配列全体ではなく、各要素のUnicodeスカラー値の数に適用します。不正な要素があれば検証エラーにし、要素を黙って削除したり切り詰めたりしません。

## Jevのモデル指定

既定のモデルは `jev-1.14.0` です。CLIでは `--model ID`、Rustでは `ExtractOptions.model`、HTTPではmultipartの `options` 内の `model` で変更できます。

Text-firstでは、バージョンを指定したモデルIDのほか、参照先が変わるエイリアスの `jev-latest`・`jev-preview` も使えます。Vision／Hybridでは、1.14.0以降のバージョンを指定したIDが必要です。古いバージョンやエイリアスを指定すると、送信前にエラーを返します。IDの形式が正しくても、提供元で利用できるとは限りません。

レポートの `model` には指定したIDを記録します。Jevが実際に使用したモデルのIDは、返却する各レスポンスにそのまま保持します。

## HTTPサーバー

### API通信なしの確認

固定回答を使うサーバーの起動例です。Bearer認証を設定しない、ループバックでの動作確認に使います。

```sh
voltfill serve --mock examples/text-mock.json --listen 127.0.0.1:3000
```

別のターミナルから、ヘルスチェックと抽出を実行します。

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/health

curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/text-form.json;type=application/json' \
  --form-string 'options={"strategy":"text_first","evaluation_profile":{"min_confidence":0.99,"noul_true_min":0.99,"noul_false_max":0.01}}'
```

`evaluation_profile` は、この合成例でconfidenceによる評価注記を明示的に有効にし、Noulの判定閾値を指定します。実データに対する精度を保証する値ではありません。抽出結果は、[値・Evidence・元レスポンスを含むJSON](#voltfillから呼び出し元への返却)で返ります。

### Jevの利用

モックのサーバーを停止してから、`VOLTFILL_JEV_ENDPOINT` を指定し、環境変数の値または明示したファイルでJevの認証情報を渡して `--remote` で起動します。

```sh
voltfill serve --remote --listen 127.0.0.1:3000
```

同じ `/v1/extract` にPDFとスキーマを送ります。`options` は省略でき、既定はText-first・`jev-1.14.0` です。変更する場合は、たとえば `--form-string 'options={"strategy":"text_first","model":"jev-1.13.0"}'` を指定します。`evaluation_profile` を省略すると、confidenceによる足切りを行わず、解決できた値を返します。明示的に指定すると、利用側のconfidenceの評価注記とNoulの判定閾値を適用できます。

Vision／Hybridを受け付ける場合は、[レンダラーを有効にしてビルド](#抽出方式)します。PDFiumを使う場合は次のように起動します。Text-first用のサーバーとは切り替えて起動してください。

```sh
voltfill serve --remote --allow-images \
  --listen 127.0.0.1:3000 --pdfium-lib ABSOLUTE_PATH
```

Hayroを有効にしたビルドでは、`--remote --allow-images --renderer hayro --listen 127.0.0.1:3000` で起動します。レンダラーはサーバー起動時に指定し、すべてのアップロードに適用します。multipartのoptionsからは変更できません。

Visionのリクエスト例です。送るのはPDFとスキーマで、ページ画像はサーバー側で描画します。

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/vision-form.json;type=application/json' \
  --form-string 'options={"strategy":"vision_first","model":"jev-1.14.0"}'
```

Hybridではスキーマを `examples/hybrid-form.json`、`options.strategy` を `hybrid` に変更します。HTTPの方式名は `text_first`・`vision_first`・`hybrid` です。

### サーバーの認証

同じマシンからのみ接続できる `127.0.0.1` では、`VOLTFILL_SERVER_API_KEY` を設定しなければ認証なしで利用できます。

認証を有効にするには、サーバーの起動前に `VOLTFILL_SERVER_API_KEY` を環境変数で渡します。クライアント側にも同じ値を渡します。次の例では、認証ヘッダーを標準入力からcurlに渡します。

```sh
printf 'Authorization: Bearer %s\n' "${VOLTFILL_SERVER_API_KEY}" |
  curl --fail-with-body --silent --show-error --header @- \
    --form 'pdf=@input.pdf;type=application/pdf' \
    --form 'schema=@schema.json;type=application/json' \
    http://127.0.0.1:3000/v1/extract
```

別のマシンから利用する場合は、TLS終端プロキシなどでHTTPSを用意し、curlの接続先をそのHTTPS URLに変更します。VOLTFILL自体はHTTPで待ち受けるため、プロキシからVOLTFILLへの経路もループバックなどで保護してください。

VOLTFILLの待ち受け先を `--listen 0.0.0.0:3000` などの外部アドレスに変更する場合は、Bearer認証の設定が必須です。`Origin` ヘッダー付きの抽出リクエストには、接続先が `127.0.0.1` でも認証が必要です。

## Jevへの送信と応答

以下は架空データを使った送信・応答の例です。候補ID・質問ID・画像参照名も例示用です。Jevとの通信で使うJSONと、[VOLTFILLが呼び出し元に返すデータ](#voltfillから呼び出し元への返却)は形式が異なります。

### Text-first：文字情報を送る

`application/json` で原文と質問を送ります。画像は添付しません。独立した複数の質問を1リクエストにまとめられます。

```json
{
  "model": "jev-1.14.0",
  "state": {"source_fragments": ["請求番号", "ABC-123"]},
  "questions": {
    "pick": {
      "type": "choice",
      "instructions": "請求番号を選んでください",
      "criteria": {"c_1": "請求番号", "c_2": "ABC-123", "none": "該当なし"}
    },
    "present": {"type": "noul", "instructions": "請求番号の記載はありますか？"}
  }
}
```

次はJevの応答例です。`choice` は選択された候補のID、`noul` は存在確認への回答を表します。

```json
{
  "model": "jev-1.14.0",
  "answers": {
    "pick": {"type": "choice", "choice": "c_2", "probabilities": {"c_1": 0, "c_2": 1, "none": 0}, "confidence": 1},
    "present": {"type": "noul", "noul": 1}
  },
  "usage": {"input_tokens": 120, "output_tokens": 0}
}
```

### Vision-first：ページ画像を送る

`multipart/form-data` の `request` パートにJSONを、`image` パートにPDFから描画したPNGを入れます。画像を送るリクエストには `TypeSafe-Danger-Zone-Images: true` ヘッダーを付けます。

次の例は、存在確認への回答を受け取った後に、対象の位置を尋ねるリクエストです。

```json
{
  "model": "jev-1.14.0",
  "state": {"image": "<image>"},
  "questions": {
    "location": {"type": "box", "instructions": "請求番号の値がある領域を指定してください"}
  },
  "assets": {"<image>": {"part": "image"}}
}
```

`<image>` は添付したPNGを参照します。PDFから抽出したテキストは送りません。Jevが画像内の位置をBoxで返すと、VOLTFILLはその領域をPDFの原文に対応付けます。

```json
{
  "model": "jev-1.14.0",
  "answers": {
    "location": {"type": "box", "box": {"left": 0.12, "top": 0.64, "right": 0.29, "bottom": 0.72}, "confidence": 1}
  },
  "usage": {"input_tokens": 1500, "output_tokens": 0}
}
```

### Hybrid：文字情報と画像を使う

文字情報から候補を選ぶ質問と、画像内の位置を求める質問を組み合わせます。文字と画像の両方を使う質問では、同じJSONの `state` に文字情報と画像への参照を入れ、PNGを添付します。

```json
{
  "model": "jev-1.14.0",
  "state": {"source_fragments": ["請求番号", "ABC-123"], "image": "<image>"},
  "questions": {
    "location": {"type": "box", "instructions": "請求番号の値がある領域を指定してください"}
  },
  "assets": {"<image>": {"part": "image"}}
}
```

位置を尋ねる質問には、上の例と同じBox形式で応答が返ります。文字情報から候補を選ぶ質問にはChoice形式の応答を使います。VOLTFILLは両方を原文に対応付け、一致しているか確認します。不一致や同点でも解決済みのBoxの値とEvidenceを保持し、statusをambiguousにします。値と合わせてstatusと理由を確認してください。

入力形式が異なる質問は別のバッチにまとめます。存在確認など、先行する質問への回答が必要な質問は、その回答を受け取ってから送信します。独立したバッチは設定した並列数の範囲で実行します。

### VOLTFILLから呼び出し元への返却

| 項目 | 内容 |
|---|---|
| `fields` | 項目ごとの値・状態・Evidence ID |
| `evidence` | 元の文字列、PDFのページ・座標、値との対応 |
| `usage` | 試行回数と、判明しているトークン使用量 |
| `jev_responses` | 各HTTP試行で受信した本文、ステータス、質問ID、待機時間の指定、通信エラー |

`jev_responses[].body` には、受信した本文を文字列またはバイト配列で保持します。再試行中の応答やエラー応答も返します。`body_complete: false` は、受信の中断や上限超過などで本文を完全には取得できなかったことを表します。

ログの設定で返却データは変わりません。confidenceは抽出値が正しい確率を保証するものではなく、現在の抽出結果には `evaluation_only: true` が付きます。

## ベンチマーク

[実APIベンチマーク](docs/benchmarks.md)に、CLI・HTTP別の10条件と測定の前提をまとめています。再作成PDF・2ページ20項目を各条件1回測定し、全200値とEvidenceを保持しました。全文一致164、一部一致36、警告4項目。Textは単一候補スキーマで、新しい複数取得は未測定です。confidenceは正答確率を示さず、旧PDFとの直接の性能比較もできません。

## 環境変数

| 変数 | 用途 |
|---|---|
| `VOLTFILL_JEV_ENDPOINT` | リモート接続先のHTTPS URL。必須で、既定URLはない |
| `VOLTFILL_JEV_API_KEY` / `VOLTFILL_JEV_API_KEY_FILE` | Jev認証情報の値、または明示したファイル。どちらか一方を指定 |
| `VOLTFILL_SERVER_API_KEY` / `VOLTFILL_SERVER_API_KEY_FILE` | サーバーのBearer認証情報の値、またはファイル。非ループバック待ち受けでは必須 |
| `VOLTFILL_CONFIG` | `voltfill run`・`extract`・`serve` の起動設定。`--config` が優先 |
| `VOLTFILL_EXECUTION_CONFIG` | 実行制限JSON。明示した実行設定ファイルや起動設定の `execution` がない場合に使う |
| `VOLTFILL_RESOURCE_CONFIG` | 資源制限JSON。`--resource-config` や起動設定の `resources` がない場合に使う |
| `VOLTFILL_LISTEN` | サーバーの待ち受け先。既定は `127.0.0.1:3000` |
| `VOLTFILL_UPLOAD_TIMEOUT_MS` | アップロード期限。1〜3600000 ms、既定は `30000` |
| `VOLTFILL_MODEL` | CLI・サーバーの既定モデル。HTTPの `options.model` がサーバーの既定値より優先 |
| `VOLTFILL_STRATEGY` | 抽出CLIの方式。既定は `text` |
| `VOLTFILL_RENDERER` | `pdfium` または `hayro` の明示選択 |
| `VOLTFILL_PDFIUM_LIB_PATH` | PDFiumライブラリの絶対パス。Hayro選択時は参照しない |
| `VOLTFILL_PIXELS_PER_POINT` | CLI・サーバーの描画倍率。既定は `1.5` |
| `VOLTFILL_LOG_LEVEL` | `off`・`error`・`warn`・`info`・`debug`。既定は `off` |

CLI引数と起動設定の値は環境変数より優先します。環境変数はCLI・サーバーの起動処理だけで読み、Rust APIでは `Endpoint`・`ExecutionConfig`・`ResourceLimits` と描画設定を明示的に渡します。単独の `voltfill-server` は実行制限・資源制限のJSONと起動用環境変数に対応します。YAML／TOMLの起動設定は `voltfill serve` を使ってください。

接続先はHTTPS、2048バイト以内で、URL内の認証情報・クエリー・フラグメント・空白・バックスラッシュを禁止します。接続先とDNSの解決先を信頼できるかは運用側で確認してください。HTTPSであるだけでは、文書や認証情報を渡してよい相手とは限りません。TLS検証は有効、リダイレクト・環境変数のプロキシ・HTTPクライアント独自の再試行は無効です。アップロードされたリクエストから接続先・認証情報・資源制限を変更することはできません。URLと認証情報はログや診断に含めません。

認証ファイルは明示した通常ファイルだけを読み、8 KiBまでとします。末尾のLFまたはCRLFを1つ許容します。認証情報の値と対応する `_FILE` の同時指定はエラーです。ファイルの自動探索や `.env` の自動読み込みは行いません。help・inspect・validate・モック抽出・抽出のdry-runにはJev接続先や認証情報は不要です。実API利用には `--remote` または起動設定の `remote: true` が必要で、環境変数だけでは有効になりません。

Jevに接続するHTTPサーバーで画像を使った抽出を受け付けるには、起動時に `--allow-images` を指定します。指定しない場合は、Jevへの送信前に該当リクエストを拒否します。

`Origin` ヘッダーを持つHTTP抽出リクエストには、ループバックでもサーバーのBearer認証設定が必要です。ルーターを別のアプリケーションに組み込む場合、待ち受け先と通信経路の保護は組み込み側で行います。

## 資源制限

起動設定の共通 `resources` セクションで指定します。`--resource-config FILE` はこのセクション全体をJSONで置き換え、`VOLTFILL_RESOURCE_CONFIG` はその指定がない場合に使います。以下は既定値と変更可能な上限です。値を小さくすると制限を厳しくできます。PDFの制限はファイル読み込み・HTTPアップロード・検査・抽出・描画に適用し、描画の制限はレンダラーを選択した場合に使います。

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

値は正の数とし、`max_recovery_pages` だけは0を指定できます。`serve.upload_timeout_ms` または `--upload-timeout-ms` は抽出期限とは別にアップロード時間を制限します。`serve.model` または `--model` はモデル省略時の既定値を指定します。HTTPサーバーは実行予算と待機時間を複数ジョブで重複消費しないよう、同時に1件だけ抽出を受け付けます。`max_concurrency` はその1件の内部で動くJevワーカー数です。抽出の実行中、その後の待機時間中、または送信の一時停止中は、抽出を始めずに `Retry-After` 付きの `429 server_busy`（一時停止中は `503 remote_paused_unknown_outcome`）を返します。このとき、アップロードの残りを、アップロードの上限サイズと期限の範囲で読み捨ててから応答するので、呼び出し側には接続の切断ではなくこの応答が届きます。`Expect: 100-continue` を付けた要求には、アップロードの前に応答します。上限を超える大きさを宣言した要求と、断る要求の読み捨てがすでに複数進んでいるときの要求は、読み捨てずに応答するので、アップロードの途中で接続が閉じる場合があります。依存ライブラリ内部のメモリ確保は、これらの制限ですべて抑えられるわけではありません。

## 送信制御

送信制御は、JSONファイルを `--config FILE` で指定して設定します。変更したい項目だけを書けば、省略した項目には既定値が使われます。全項目の記載例は [execution-config.json](examples/execution-config.json) を参照してください。

| 項目 | 意味 | 既定値 |
|---|---|---|
| `max_concurrency` | 同時にPOSTを送信するワーカー数。1〜4 | `2` |
| `max_batch_questions` | 1バッチにまとめる質問数。1〜255 | `32` |
| `max_attempts` | 初回を含むリクエストごとの最大試行回数。1〜3 | `3` |
| `max_requests` | リトライを含む処理全体の最大POST試行回数 | `128` |
| `requests_per_second` | 1秒あたりのリクエスト数の上限 | `80` |
| `input_tokens_per_second` | 1秒あたりに予約する入力トークン数の上限 | `100000` |
| `input_tokens_per_attempt` | 1試行で許容する推定入力トークン数。収まるようにバッチを分割 | `64000` |
| `context.max_request_tokens` | stateと全質問を合わせた推定トークン数の上限 | `64000` |
| `context.max_question_tokens` | stateと各質問を合わせた推定トークン数の上限 | `32000` |
| `context.image_tokens_per_image` | 画像参照1回あたりのトークン推定量 | `16000` |
| `total_deadline_ms` | 待機・リトライを含む処理全体の期限 | `30000` |
| `attempt_timeout_ms` | 1回のHTTP通信のタイムアウト | `10000` |
| `initial_backoff_ms` | 最初のリトライまでの待ち時間 | `250` |
| `backoff_multiplier` | リトライ待ち時間を指数的に増やす倍率 | `2` |
| `max_backoff_ms` | バックオフの待ち時間の上限。ただし `Retry-After` の指定時間より早くは再送しない | `10000` |
| `jitter_ms` | 再送タイミングを分散するために加えるランダムな待ち時間の上限 | `250` |
| `max_cost_nano_usd` | 推定費用の上限。単位は10億分の1米ドル | `200000000`（$0.20） |
| `input_token_price_nano_usd` | 入力1トークンの推定単価。1〜1000000000 nano-USD | `42` |
| `completion_gap_ms` | トークン予約・待機時間の下限。2000〜60000 ms。より大きい `Retry-After` は保持 | `2000` |
| `max_json_bytes` | リクエスト・レスポンスJSONのバイト上限。1〜1048576 | `1048576` |
| `max_total_body_bytes` | 1つの計画済みリクエストの全試行で送る本文バイト合計。1〜33554432 | `33554432` |
| `organization.requests_per_second` | `requests_per_second` に設定できる値の上限 | `80` |
| `organization.input_tokens_per_second` | `input_tokens_per_second` に設定できる値の上限 | `100000` |

費用制御は入力トークンだけを推定し、予約と受信した使用量の精算に同じ設定単価を使います。請求総額の算出や現在価格の取得は行いません。モデルと接続先に合わせて単価を設定してください。

時間の単位はミリ秒です（`1000` ＝1秒）。コンテキストの既定値はモデルの通常上限64k／32kに合わせています。送信頻度の既定値はアプリケーションの設定であり、利用組織から取得した割当ではありません。実際の割当に合わせて設定してください。既定値より大きい組織割当も設定できます。コンテキストの設定を増やしても、実験的な長文入力が有効になったり、プロバイダーの対応が保証されたりするわけではありません。

送信前に、互換な質問を質問数・JSONサイズ・推定入力トークン数で分割します。ページの探索範囲と質問の依存関係は保ちます。質問を1件にしてもページのstateと合わせて収まらない場合は、`input_token_limit_exceeded` を理由とする `unsupported` を返します。原文・候補・画像を黙って切り捨てることはありません。

推定には、JSONのUTF-8バイト数と付加分256トークン、画像参照の出現回数に応じた設定値を使います。文字情報を多めに見積もる方式で、公式のトークナイザーや厳密な上限ではありません。画像の既定推定量もアプリケーションの設定であり、公式の算出式ではありません。実usageが推定を超える可能性があります。各試行は自分の推定量を予約し、受信した入力usageが予約量を超えたら、元レスポンスを残して後続の送信受付を止めます。usage不明時は費用予約を維持します。`request_planned` ログには件数と推定値だけを出し、本文は出しません。

再試行にも同じ制限を適用します。他のプロセスの送信量は、VOLTFILLでは管理しません。`organization` は設定値の上限を検証するための項目で、外部クライアントとの送信量の調整は行いません。

CLIの `--max-concurrency` と `--max-attempts` を指定すると、設定ファイルの対応する値を上書きします。

PDFの入力サイズやページ数の上限では、依存ライブラリ内部のストリーム展開や予測復元のメモリ割り当てをすべて制限できません。信頼できないPDFでメモリを使い切る可能性があり、VOLTFILL自体はプロセス隔離を提供しません。

処理全体の期限を過ぎると、新たな処理の開始を止めます。この期限は待機・再試行・HTTP通信を制限しますが、実行中の同期的なPDF解析や描画は強制中断できません。

## ログ

OTLP JSONログは**既定で無効**です。CLI・サーバーの `--log-level off|error|warn|info|debug` で切り替えます。

| 出力元 | ログの出力先 |
|---|---|
| CLI | 標準エラー |
| CLI（`--output FILE` 指定時） | 標準出力 |
| サーバー | 標準出力 |

外部サービスへの転送は利用側で管理します。レスポンスの本文、認証情報、HTTPヘッダーはログに含めません。ログの設定によって返却するレスポンスが変わることもありません。

## 開発

ワークスペースをビルドして確認する場合は、次のコマンドを使います。`cargo build` はPATHへインストールせず、`target/debug/voltfill` を生成します。

```sh
cargo build --workspace --locked
./target/debug/voltfill inspect input.pdf
```

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

featureごとのテストは `cargo test --workspace --features hayro --locked` または `cargo test --workspace --features pdfium --locked` で実行します。PDFファイルを使うテストとネイティブ描画のテストは含みません。実行ファイルのパスを共有するため、feature構成ごとのテストは順番に実行してください。Rust APIドキュメントは `cargo doc -p voltfill-core --no-deps` で生成できます。

[CI](.github/workflows/ci.yml)はLinuxとmacOSでfmt・check・Clippy・既定featureのテストを実行します。Hayroのcheck・Clippy・テスト、両レンダラーを含む構成のcheck、PDFiumのcheck・Clippy・テストのコンパイルも行います。PDFiumのfeature検査ではネイティブライブラリをロードしません。

依存関係はpush・pull request・手動実行と毎日の定期実行でRustSecのアドバイザリーと照合します。既知の脆弱性や健全性の問題が見つかると検査は失敗します。アプリケーション固有の問題には、別途コードレビューとテストが必要です。

CIではコミット履歴への秘密情報の混入も検査し、出力時は値を伏せます。DependabotはCargo依存とGitHub Actionsの更新を毎週提案します。自動マージは設定していません。

CIはJevへの送信、PDFiumのダウンロード、ビルドキャッシュの永続化を行いません。ローカルでの検証後は `cargo clean` でビルド成果物を削除できます。

### 依存ライブラリの更新

VOLTFILLではフォント解析に使う依存ライブラリの正式リリースを確認し、修正を取り込む前に原文・座標・Evidenceを確認します。

[アーキテクチャ・C4](docs/architecture.html) · [MITライセンス](LICENSE)
