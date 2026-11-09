<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

PDFのテキスト層（入力欄つきPDFではAcroFormの値も）から、フォーム項目に対応する原文の値を根拠（Evidence）付きで返すRust製のCLI／HTTPサーバーです。値を生成・OCRせず、PDFも変更しません。構成は[アーキテクチャ](https://daisuke8000.github.io/voltfill/)を参照してください。

## インストール

Rust 1.99.1で、リポジトリのルートから実行します。`--images` を使う場合は `--features hayro`（または `pdfium`）を付けます。

```sh
cargo install --path crates/vf-cli --locked
```

## 使い方

```sh
voltfill inspect input.pdf                                   # テキストと座標を確認
voltfill fill input.pdf examples/forms/invoice.json -o result.json
```

フォーム（[例](examples/forms/invoice.json)）の各項目は `id`・`label`・`type`（`text` / `date` / `amount` / `number` / `table`）・`hints`（別名）を持ちます。`table` は `columns` に列を並べ、明細行を `rows` で返します。

照合は「誤って埋めるより空欄」の厳格な方針です。ラベルが正規化のうえで完全に一致し、区切り記号（`ラベル：値`）で結び付き、値が1行に1つで、型の書式に合う場合だけ埋めます。それ以外は確定できない理由（`hold`）付きの候補を返します。入力欄つきPDFでは、名前が一致し表示とも矛盾しないAcroFormの値でも埋めます。

| `status` | 意味 |
|---|---|
| `filled` | `raw`（原文）・`value`（正規化値）・`evidence`（ページ・原文片・座標）を返す |
| `ambiguous` | 候補が決まらない。`candidates` に `hold` を付けて返す |
| `not_found` | 候補がない |
| `no_text` | 使えるテキスト層がない（スキャンなど） |
| `invalid` | 値が型の書式に合わない |

主な `hold`: `partial_label`・`one_sided_qualifier`（ラベルの一部・片方だけの補足）、`geometric_only`・`no_delimiter`（位置か空白だけの結び付き）、`multi_line`・`split_value`・`several_values`（値が1つでない）、表では `irregular_table`・`untyped_table`・`continues_on_next_page` など。

### Jevの利用（任意）

```sh
voltfill fill input.pdf form.json --remote                              # 候補から選ばせる
voltfill fill input.pdf form.json --remote --images --renderer hayro    # ページ画像で値の位置を探す
```

`--remote` は `ambiguous` の項目の候補をJevに選ばせ、`--images` は残った項目の値の位置をページ画像で答えさせ、その位置の原文で埋めます（スキャンのページは位置 `located` だけを返します）。Jevは文字列を返さず、値は常にPDFの原文から取ります。接続先は `VOLTFILL_JEV_ENDPOINT`（HTTPS）、キーは `VOLTFILL_JEV_API_KEY` か `VOLTFILL_JEV_API_KEY_FILE` で渡します。結果には全試行の受信本文（`jev_responses`）が入ります。模擬応答でのみ試験しており、実APIでの精度は未測定です。

PDFiumは `--features pdfium` と `--pdfium-lib`（絶対パス）で利用側が用意します。自動取得はしません。

### 終了コード

| コード | 意味 |
|---|---|
| 0 | 結果を書いた（未確定の項目は `status` で確認） |
| 2 | 引数・フォーム・入力の誤り。何も送らない |
| 4 | PDF解析失敗・処理量の上限・期限（`--deadline-ms`、既定30秒）。`--remote` で全項目が失敗した場合は結果を書いてから4 |
| 5 | 出力先に書けない。受信本文は一時ファイルか標準エラーに残す |

## HTTPサーバー

```sh
voltfill serve --remote --listen 127.0.0.1:3000
curl -sS http://127.0.0.1:3000/v1/fill -F 'pdf=@input.pdf' -F 'form=@examples/forms/invoice.json'
```

`POST /v1/fill` はオフラインの `voltfill fill` と同じ結果を返し、Jevには送りません。`GET /health` はヘルスチェックです。同時実行は1件で、実行中は `429` を返します。`VOLTFILL_SERVER_API_KEY`（または `_FILE`）を設定するとBearer認証が有効になり、ループバック以外で待ち受けるには必須です。

## 開発

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p vf-core --test eval -- --nocapture   # 合成PDFでの評価（評価専用）
```

`pdf-inspector` が依存する `ttf-parser` の保守警告[RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)は抑止せず追跡します。

[MITライセンス](LICENSE)
