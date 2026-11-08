<p align="center">
  <img src="docs/assets/voltfill.png" alt="VOLTFILL" width="640">
</p>

# VOLTFILL

PDFのテキスト層から、指定された項目に対応する原文の値を根拠（Evidence）付きで返すRust製CLI／HTTPサーバーです。PDFは変更しません。構成と実装状況は[アーキテクチャ・C4](https://daisuke8000.github.io/voltfill/)を参照してください。

> 機能の範囲を絞り込み中です。Vision／Hybrid方式とスキーマ形式は今後変更されます。

## インストール

Rust 1.99.1で、リポジトリのルートから実行します。

```sh
cargo install --path crates/vf-cli --locked
```

## 使い方

Jevに送信せず、PDFのテキストと座標を確認します。

```sh
voltfill inspect input.pdf
```

<a id="fill"></a>

### フォーム入力（fill）

フォーム項目の一覧（[例](examples/forms/invoice.json)）を渡すと、PDFの原文から各項目の値を探して返します。`--remote` を付けない限りJevには送信しません。

```sh
voltfill fill input.pdf examples/forms/invoice.json -o result.json
```

項目は `id`・`label`・`type`（`text` / `date` / `amount` / `number`、既定 `text`）・`hints`（ラベルの別名）です。ラベルの表記ゆれ（全角半角・空白・括弧）を吸収して照合し、型に合う値だけを採用します。

結果は項目ごとに `status` を持ちます。誤って埋めるより空欄を優先し、決めきれない項目は値を入れず候補を返します。

| status | 意味 |
|---|---|
| `filled` | 値が1つに決まった。`raw`（原文）・`value`（日付はISO形式、金額・数値は数値）・`evidence` を返す |
| `ambiguous` | 異なる値の候補が並んだ。`candidates` を返す |
| `not_found` | 対応するラベルが見つからない |
| `no_text` | 使えるテキスト層がない（スキャンPDF・文字化け） |
| `invalid` | ラベルは見つかったが値が型に合わない。`raw` を返す |

`evidence` はページ・原文片のIDとバイト範囲・座標（PDFポイント、左上原点）です。1つの片を「ラベル：値」で分けた場合の座標は文字幅からの推定で、`precision: "estimated"` と示します。表・AcroForm・区切り記号のない「ラベル 値」は未対応です。結果は評価専用です。

日付は西暦のほか、和暦（令和・平成・昭和・大正、「元年」）と略記 `R8.11.8` / `H31/4/30` / `S55.3.1` をISO形式に変換します。元号の範囲外（例: 平成31年5月1日、令和元年4月30日）や、西暦と和暦の併記（例: `2026年(令和8年)11月8日`）・明治は変換せず `invalid` です。

`--remote` を付けると、`ambiguous` の項目だけをJevに送り、その項目の候補から選ばせます。接続先とキーは `run --remote` と同じく必要です（[実APIの利用](#environment)）。`--remote` なしでは認証情報を読まず、結果も変わりません。

```sh
voltfill fill input.pdf examples/forms/invoice.json --remote -o result.json
```

- 送るのは項目のラベル・型・別名と、候補ごとのページ・原文のラベルと値だけです。選択肢は候補の順に `c0`・`c1`…と `none` です。ページ画像・前後の行・ほかの項目は送りません。候補の文は選択対象の文書データで指示ではないと、要求に明記します。
- 選ばれた候補が確率の単独最大で、確率の合計が1のときだけ `filled`（`method: "jev_choice"`）にし、`raw`・`value`・`evidence` はその候補の原文から取ります。`none` が単独最大なら `not_found`、同率・不正な回答（確率の合計が1でないものを含む）・送信失敗では `ambiguous` のままです。送った項目は `candidates` を残します。
- 結果には `model`・`assist`（項目ごとの質問ID・`outcome`・選ばれたキー）・`usage`・`jev_responses` が加わります。`jev_responses` は再試行・エラー・不完全な本文を含む全試行の受信本文です。
- `outcome` は、回答を得た場合が `selected`・`none_selected`・`not_unique_maximum` です。失敗には `answer_invalid`・`request_failed`・`budget_exhausted`（上限に達したか予算が停止していて、そのバッチかその再試行を送らなかった）・`usage_exceeded_budget`・`deadline_exceeded`・`request_too_large`・`cancelled` があります。`usage_exceeded_budget` は、受信して解析できた回答の使用量が予約か費用上限を超えた場合です。その回答は使わず（本文は残します）、以降のバッチも送りません。`cancelled` はライブラリの呼び出し側が中止させた場合で、CLIからは発生しません。
- `--model`・`--max-attempts`・`--execution-config FILE` が使えます。バッチは1件ずつ送ります。送信回数（`max_requests`）・費用・本文量・期限の上限は全バッチと再試行の合計に適用します。試行回数（`--max-attempts`）はバッチごとで、各試行も送信回数に数えます。トークンの上限は1試行ごと（`input_tokens_per_attempt`）と1秒あたり（`input_tokens_per_second`）です。
- 送った項目がすべて失敗で終わった場合も、全受信本文を含む結果を書き出してから終了コード4（`fill_assist_all_failed`）を返します。書き出せないと事前に分かる出力先は送信前に止めます（[出力先](#output)）。項目が `ambiguous`・`not_found` のまま残っても終了コードは0です（送った項目がすべて `not_unique_maximum` の場合も含みます。`--remote` なしの `fill` と同じく、各項目の `status` で確認します）。
- 模擬応答でのみ試験しており、実APIでの精度は未測定です。confidence・確率は校正済みの正答確率ではありません。

接続先とキーを設定し（[実APIの利用](#environment)）、PDFと抽出定義を渡します。

```sh
voltfill run input.pdf examples/legacy/text-form.json --remote -o result.json
```

既定はText方式です。全オプションは `voltfill --help` を参照してください。

| 方式 | 動作 | スキーマ例 |
|---|---|---|
| `text` | 原文の候補から選択 | [Text](examples/legacy/text-form.json) |
| `vision` | 画像のBoxを原文に対応付け | [Vision](examples/legacy/vision-form.json) |
| `hybrid` | 文字候補とBoxを照合 | [Hybrid](examples/legacy/hybrid-form.json) |

既定モデルは `jev-1.14.0`（`--model ID` で変更）。Vision／Hybridは1.14.0以降のバージョン指定IDと[レンダラー](#rendering)が必要です。

<a id="results"></a>

## 抽出結果

結果は `fields`・`evidence`・`usage`・`jev_responses` です。部分失敗でも値を保持するため、`status`・`reason_codes` を確認してください。再試行・エラーの受信本文も残し、未受信・不完全なら `body_complete: false` です。

原文・座標・型・validatorを検証し、既定ではconfidenceで足切りしません。結果は評価専用（`evaluation_only: true`）で、confidenceは校正済みの正答確率ではありません。

<a id="output"></a>

## 出力先（`-o` / `--output`）

`-o` を付けたコマンド（`fill`・`inspect`・`run`／`extract`）は、設定ファイル（`--config`・`--execution-config`・`--resource-config`）を読んだ後、PDF・フォーム・スキーマ・模擬応答・APIキーを読む前で送信前に、出力先を確かめて同じディレクトリに一時ファイル（`.voltfill-<PID>-<番号>.tmp`）を作ります。次の場合は終了コード5で止まり、何も送りません。

| エラー | 出力先 |
|---|---|
| `invalid_output_path` | 空、または `/`・`.`・`..` で終わるパス |
| `symlink_output_refused` | シンボリックリンク |
| `output_is_directory`・`output_not_regular_file` | 既存のディレクトリ、通常ファイル以外 |
| `input_overwrite_refused` | 入力・設定ファイル・ネイティブライブラリ・`VOLTFILL_JEV_API_KEY_FILE` と同じファイル（キーファイルはパスだけを比べ、中身は読みません） |
| `output_directory_not_found`・`output_not_writable` | ディレクトリがない、書き込めない |
| `output_not_accessible` | 状態を確かめられない（名前が長すぎる、シンボリックリンクが循環している、ディレクトリを検索する権限がないなど） |

結果は一時ファイルに書いてから出力先へ名前を変えて置き換えます。置き換えはリンクをたどらないため、実行中に出力先へ置かれたシンボリックリンクはそれ自体が置き換わり、リンク先は変わりません。結果を書き始める前に止まった場合（送信前のエラーなど）、一時ファイルは削除します。

事前の確認ですべての失敗が分かるわけではありません。変更不可フラグ（`chflags uchg` など）の付いた出力先や、スティッキービット付きディレクトリにあるほかのユーザーの出力先は、送信後の置き換えで `output_rename_failed` になります。結果を書き込めない場合（`output_write_failed`。ディスク不足など）と置き換えられない場合（`output_rename_failed`）は、どちらも終了コード5で、受信本文を含む結果を次の順に残します。

1. 置き換えに失敗し、一時ファイルが結果全体を持ったまま残っていれば、それを残します。
2. そうでなければ（書き込みの失敗、実行中に一時ファイルや出力先のディレクトリが消えた場合など）、出力先のディレクトリ、次に作業ディレクトリに新しい一時ファイルを作り、結果全体を書き直します。
3. どのファイルにも書けなければ、標準エラーに `{"error":"<エラー>","result":<結果>}` という1つのJSONで書きます。結果のバイト列は変えません。`-o` 指定時のログは標準出力なので、標準エラーには混ざりません。

1・2では、存在して結果全体の長さを持つと確かめたファイルだけを、エラーのJSON（ログが有効ならエラーのログ）の `output_kept` に示します。3では `output_kept` を付けず、ログが有効ならエラーのログに `result_on_stderr`（標準エラーへの書き込みが成功したか）を付けます。標準出力には切り替えません。標準エラーを `/dev/null` に向けている場合、3の結果は残りません。書き込みに失敗した一時ファイル（空または途中まで）は受信本文を含みうるため削除せず、`output_kept` にも示さずに残します。シグナル（`SIGTERM`・Ctrl-C など）で強制終了した場合も、権限0600の `.voltfill-<PID>-<番号>.tmp`（空、書き込み中なら途中まで）が残ることがあります。

## 設定

```sh
voltfill run --config examples/legacy/voltfill.yaml
```

[YAML例](examples/legacy/voltfill.yaml)は固定回答で無送信です。TOML・JSONも使えます。優先順位は **CLI → 指定ファイル → 環境変数 → 既定値**。`--config` 省略時は `VOLTFILL_CONFIG` を使います。相対パスの基準は、ファイル内なら設定ディレクトリ、それ以外は作業ディレクトリです。

送信制限の[設定例](examples/legacy/execution-config.json)は既定値一覧ではありません。`--execution-config FILE` / `--resource-config FILE` は `execution` / `resources` 全体をJSONで置き換えます。資源制限はPDF解析を隔離せず、費用予約も入力の推定で請求上限ではありません。

ログは既定オフ（`--log-level`）で、1行1件のJSONを出力します。CLIは標準エラー、`--output` 指定時とサーバーは標準出力です。本文・認証情報・HTTPヘッダーは出しません。

<a id="environment"></a>

### 実APIの利用

接続先は `VOLTFILL_JEV_ENDPOINT`（信頼できるHTTPS）、キーは `VOLTFILL_JEV_API_KEY` / `VOLTFILL_JEV_API_KEY_FILE` の一方だけで指定します。設定ファイルにキーは書けず、`.env` も読みません。送信には `--remote` / `remote: true` が必須です。

<a id="rendering"></a>

### 描画

Vision／Hybridはレンダラーのビルドと選択が必要です。自動切り替え・自動取得・OCRは行いません。

```sh
cargo install --path crates/vf-cli --locked --features hayro
voltfill run input.pdf examples/legacy/vision-form.json --remote --strategy vision --renderer hayro
```

Hayroはネイティブライブラリ不要です。PDFiumは利用側で用意し、`--features pdfium`、`--renderer pdfium` と `--pdfium-lib` / `VOLTFILL_PDFIUM_LIB_PATH` の絶対パスで指定します。描画倍率は既定1.5ピクセル/PDF単位（`--pixels-per-point`）です。

<a id="http-server"></a>

## HTTPサーバー

```sh
voltfill serve --mock examples/legacy/text-mock.json --listen 127.0.0.1:3000
```

`POST /v1/extract` はmultipartの `pdf`・`schema` と任意のJSON `options`（`strategy`・`model`）、`GET /health` はヘルスチェックです。モックは形式確認用で、精度評価には使えません。実APIでは `--remote` で起動し、Vision／Hybridは `--allow-images` とレンダラーを指定します。

```sh
curl --fail-with-body --silent --show-error http://127.0.0.1:3000/v1/extract \
  --form 'pdf=@input.pdf;type=application/pdf' \
  --form 'schema=@examples/legacy/text-form.json;type=application/json'
```

認証は `VOLTFILL_SERVER_API_KEY` / `VOLTFILL_SERVER_API_KEY_FILE` の一方を設定し、`Authorization: Bearer <key>` を送ります。未設定のループバックでは任意、非ループバック待ち受け・`Origin` 付き抽出では必須です。VOLTFILLはHTTPのため、別マシンからはTLSプロキシを使います。

抽出は同時1件で、実行・待機中は `Retry-After` 付き `429 server_busy`、送信停止中は `503 remote_paused_unknown_outcome` です。`--reuse-document`（既定オフ）は直前1件の文書をTTL（`--reuse-document-ttl-ms`、既定60000 ms）までメモリに保持して再利用します。内容が要求後も残るため単一テナント向けです。

## 開発

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

描画テストは `--features hayro` / `--features pdfium` を付けて構成ごとに順次実行し、検証後に `cargo clean` を実行します。[CI](.github/workflows/ci.yml)は上記に加え、次の検査を実行します。PDFiumはコンパイルのみで、ライブラリは取得しません。

| 検査 | 対象 | 失敗条件 |
|---|---|---|
| cargo-audit | `Cargo.lock`（RustSec） | 脆弱性・unsound |
| OSV-Scanner | `Cargo.lock`（OSV） | 脆弱性・unsound。unmaintainedなどの情報系は警告表示のみ |
| cargo-deny | ライセンス・重複／ワイルドカード依存・取得元（[deny.toml](deny.toml)） | 許可外ライセンス、ワイルドカード依存、crates.io以外の取得元 |
| Gitleaks | Git履歴 | 秘密情報の検出 |
| actionlint・zizmor | ワークフロー | 構文・セキュリティの問題 |
| gh actions-lock | [actions.lock](.github/workflows/actions.lock) | 間接的に使われるものも含め、全アクションがロックしたコミットに解決できない |

ワークフローの `uses:` はタグで書き、実行するコミットは `actions.lock` で固定します。`uses:` を変えたら `gh actions-lock`（拡張 `github/gh-actions-lock`）で更新します。Dependabotの更新PRはロックも一緒に更新します。

`pdf-inspector` の `ttf-parser 0.25.1` に[RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)の保守警告があります。脆弱性の発見を示すものではないため、各検査で抑止せず警告として表示します。正式修正は原文・座標・Evidenceの回帰検証後に採用します。

<a id="evaluation"></a>

### フォーム入力の評価セット

フォーム入力（fill）を合成PDFで評価します。フォントファイルなしで日本語（BMP外の文字を含む）のテキスト層を持つPDFを生成し（Type0・Identity-H、ToUnicode付き、グリフ非埋め込み）、ケースごとに項目の期待値（原文と正規化値）または「空欄のまま」と照合します。ネットワークは使いません。

```sh
cargo test -p vf-core --test eval -- --nocapture
```

| 指標 | 意味 |
|---|---|
| `correct` | 期待どおりの原文・正規化値で埋めた |
| `wrong` | 値を期待した項目を、記録にない別の値で埋めた |
| `unexpected` | 空欄を期待した項目を、記録にない値で埋めた |
| `known` | 項目に記録した既知の誤入力（`known_wrong`）と同じ原文・正規化値で埋めた |
| `empty` | 値を期待したが埋めなかった |
| `abstain` | 空欄を期待し、空欄のままだった |

ケースごとの表に、対応済み・未対応のケース別の小計と合計を添えます。既知の誤入力も誤入力の件数（`wrong fills`）に含めます。

次のいずれかでテストは失敗します。

- 値を期待した項目を別の値で埋めた、または空欄を期待した項目を埋めた。項目の `known_wrong`（`{"status": "filled", "raw": …, "value": …, "reason": …}`）と原文・正規化値が完全に一致する出力だけは除く
- `known_wrong` を記録した項目の出力が記録と変わった。直った場合も別の誤った値になった場合も失敗にし、ケースの更新を求める
- 対応済みのケースで、期待した値が空欄になった、または空欄の状態（`not_found`・`ambiguous`・`invalid`）が期待と違う
- 埋めた値と `invalid` の原文のEvidenceが、原文片の正確な切り出しでない、期待した値の場合に期待したランを指さない、生成時に配置した位置と0.05pt以内で一致しない、または `precision` が片全体なら `run`・一部なら `estimated` でない
- 埋めていない項目が正規化値を持つ、または `invalid` 以外の項目が原文やEvidenceを持つ

失敗にしないのは、`known_wrong` と完全に一致する出力（`KNOWN` として理由とともに表示）と、未対応のケース（区切り記号のない「ラベル 値」・和暦・明細表・1つのランに2組の「ラベル：値」など）で空欄のままの項目だけです。未対応のケースは、正しく埋まっていた項目（`currently_correct`）が空欄に戻ったとき、また全項目が合格したときに注記（`NOTE`）を出します。ケースは[tests/eval/cases](crates/vf-core/tests/eval/cases)のJSONです。

合成した評価専用のデータで、実文書での精度や性能を示すものではありません。推定座標が正解位置と一致するのは、合成フォントの文字幅が推定の前提（全角1em・半角0.5em）と同じためです。描画時はグリフを代替するため、見た目の再現は対象外です。

[アーキテクチャ・C4](https://daisuke8000.github.io/voltfill/)（[ソース](docs/architecture.html)） · [MITライセンス](LICENSE)
