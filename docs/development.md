# 開発

[English](development.en.md) · 日本語 · [README](../README.ja.md)

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

描画featureのテストには `--features hayro` または `--features pdfium` を付け、構成ごとに順番に実行します。ネイティブ描画の動作確認は別途必要です。検証後は `cargo clean` を実行します。自動検査は [CI](../.github/workflows/ci.yml) を参照してください。

CI は PR と `main` への push で Rust のフォーマット、workspace のコンパイル検査、Clippy、テストを実行します。PDFium のテストはコンパイルのみで、ネイティブライブラリは取得しません。セキュリティ検査は毎日と手動起動でも実行します。`cargo-audit` による Cargo.lock のアドバイザリ検査、Gitleaks による検出値を伏せたコミット履歴のシークレット検査、[actionlint](https://github.com/rhysd/actionlint) と [zizmor](https://docs.zizmor.sh/usage/) によるワークフロー検査が対象です。依存関係に変更がない PR でもアドバイザリ検査を実行します。zizmor はオフラインで動作し、GitHub API を使う検査は行いません。検出結果はジョブの失敗とし、保守状況の警告は確定した脆弱性とは区別して表示します。Pages の公開には全検査ジョブの成功が必要です。Dependabot は Cargo と GitHub Actions の更新を毎週提案します。

`pdf-inspector` が使う `ttf-parser 0.25.1` には[保守状況の警告 RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)があり、脆弱性の発見を知らせるものではありません。正式な修正は原文・座標・Evidenceの退行を検証して採用します。
