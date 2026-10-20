# 抽出と結果

[English](jev.en.md) · 日本語 · [README](../README.ja.md)

<a id="models"></a>

## モデル指定

既定は `jev-1.14.0`。CLIの `--model ID`、Rustの `ExtractOptions.model`、multipartの `options.model` で変更します。

Text-firstはバージョン指定IDと参照先が変わる `jev-latest`・`jev-preview` を使えます。Vision／Hybridは1.14.0以降のバージョン指定IDが必要で、古いID・エイリアスは送信前に拒否します。形式が正しくても提供元での利用は保証しません。

<a id="output"></a>

## 抽出結果

| 項目 | 内容 |
|---|---|
| `fields` | 項目ごとの値・状態・Evidence ID |
| `evidence` | 元の文字列、PDFのページ・座標、値との対応 |
| `usage` | 試行回数と、判明しているトークン使用量 |
| `jev_responses` | 各HTTP試行で受信した本文、ステータス、質問ID、待機時間の指定、通信エラー |

値が非nullでも無条件の成功ではなく、`status`・`reason_codes` を確認します。曖昧さ・検証エラー・別ページの失敗でも途中の値とEvidenceを保ちます。Hybrid配列は文字・画像の不一致や同点でも解決済みBox断片を保ち、解決できない回答から値は作りません。

`jev_responses[].body` は再試行・エラーを含む全試行の文字列・バイト列を保ち、`body_complete: false` は受信が不完全なことを示します。レポートの `model` は指定ID、元レスポンスは実際のモデルIDを保ちます。結果は `evaluation_only: true` で、confidenceは校正済みの正答確率ではありません。

<a id="evaluation"></a>

## confidenceの評価

`evaluation_profile` 省略時はconfidenceで足切りせず解決済みの値を返し、原文・座標・型・validatorは検証します。`--evaluation-profile synthetic` はローカル合成テスト用で、通常の実APIコマンドでは省略します。

profileなしではNoulは0.5以上がtrue、0.5未満がfalseです。Vision／Hybridは存在回答がfalseでもBoxを質問し、存在による省略はprofile明示時だけ行います。明示したprofileでも解決済みの原文値を削除せず判定結果を付けます。

Choice確率は受信したまま保ちます。確率合計は診断情報とし、同点・最大確率でない選択は `ambiguous` を付けます。Jevの選択を置き換えず、解決済みの値・Evidenceも削除しません。

<a id="multiple-source-runs"></a>

## 複数の原文断片

`text_in_box`・`selected_candidate`・`selected_candidates` で `"output_type": "string_array"` を指定すると、原文断片を別々の文字列で返します。

`text_in_box` はBoxと正の面積で重なるusableな原文断片をすべて、PDFの `item_index` 順に返します。見た目の読み順推測、重なり率の足切り、切り出し・連結、欠落補完はしません。断片全体なのでBox外の文字を含む場合もあり、Evidenceは元の文字列・座標を保ちます。`selected_candidate` は選択した文字列を1要素配列で返します。既存の `string` 契約は変わりません。

Text-firstの複数取得は [text-multi-form.json](../examples/text-multi-form.json) を使います。`selected_candidates` には `string_array`、動的な `text_spans` を持つ独立したText Choice質問ちょうど1つ、**ページごと**の `max_selections`（1〜254の明示指定）が必要です。Vision／Hybridは非対応です。全文か必要部分かは質問で指定します。

質問を変えず選択済みIDを除き、`none` または全候補取得まで繰り返します。原文順で返し、同文でも位置が違えば別要素です。各回は実行制限を共有します。候補を残して上限に達すると `ambiguous` / `selection_limit_reached` とし、エラー・予算切れでも取得済みの値とEvidenceを保ちます。

配列validatorは全ページを集めた後に適用します。正規化は要素ごとでEvidenceは不変です。`non_empty` は空配列・空文字・空白のみの要素を拒否し、`max_chars` は配列全体ではなく各要素のUnicodeスカラー数を数えます。不正な要素は検証エラーにし、黙って削除・切り詰めません。

原文を返す項目（`string` を含む）は `"unify_cjk_radicals": true`（既定false）を指定できます。正規化前に康熙部首・CJK部首補助の互換分解を持つ文字を統合漢字に直し、他の互換文字は変更しません。Evidenceは元の文字を保ちます。
