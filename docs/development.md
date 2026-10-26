# 開発

[English](development.en.md) · 日本語 · [README](../README.ja.md)

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

描画テストは `--features hayro` / `--features pdfium` を付け、構成ごとに順次実行します。ネイティブ動作は別途確認し、検証後に `cargo clean` を実行します。

[CI](../.github/workflows/ci.yml)はPR・mainへのpushで上記を実行します。PDFiumはコンパイルのみで、ライブラリは取得しません。

セキュリティ検査は毎日・手動でも実行します。対象はcargo-audit（依存変更のないPRも含む）、Gitleaks（履歴・検出値を伏せる）、[actionlint](https://github.com/rhysd/actionlint)、[zizmor](https://docs.zizmor.sh/usage/)です。zizmorはオフラインで、GitHub API検査は行いません。検出時はジョブを失敗させ、保守警告と確定した脆弱性は区別します。Pages公開には全検査成功が必要です。DependabotはCargo・Actions更新を毎週提案します。

`pdf-inspector` の `ttf-parser 0.25.1` に[RUSTSEC-2026-0192](https://rustsec.org/advisories/RUSTSEC-2026-0192.html)の保守警告があります。脆弱性の発見を示すものではなく、正式修正は原文・座標・Evidenceの回帰検証後に採用します。