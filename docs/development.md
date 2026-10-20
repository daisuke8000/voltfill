# 開発

[English](development.en.md) · 日本語 · [README](../README.ja.md)

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

描画featureのテストには `--features hayro` または `--features pdfium` を付け、構成ごとに順番に実行します。ネイティブ描画の動作確認は別途必要です。検証後は `cargo clean` を実行します。自動検査は [CI](../.github/workflows/ci.yml) を参照してください。

`pdf-inspector` が使う `ttf-parser 0.25.1` には[保守状況の警告 RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)があり、脆弱性の発見を知らせるものではありません。正式な修正は原文・座標・Evidenceの退行を検証して採用します。
