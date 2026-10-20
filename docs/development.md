# 開発

[English](development.en.md) · 日本語 · [README](../README.ja.md)

<a id="checks"></a>

## ビルドと検証

```sh
cargo build --workspace --locked
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

featureテストは `cargo test --workspace --features hayro --locked` または `cargo test --workspace --features pdfium --locked` です。PDFファイルやネイティブ描画はテストしません。実行ファイルのパスを共有するため構成ごとに順番に実行します。Rust APIドキュメントは `cargo doc -p voltfill-core --no-deps`、検証後のローカル成果物削除は `cargo clean` です。

自動検査は [CI](../.github/workflows/ci.yml) を参照してください。

<a id="dependency-updates"></a>

## 依存ライブラリの更新

`pdf-inspector` の依存先 `ttf-parser 0.25.1` には[保守状況の警告 RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)があります。脆弱性の発見を知らせるものではありません。正式なフォント解析ライブラリのリリースを追い、修正の採用前に原文・座標・Evidenceを確認します。
