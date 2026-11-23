# syki

<p align="center">
  <a href="https://github.com/youshinh/syki/releases/latest"><img src="https://img.shields.io/github/v/release/youshinh/syki?style=flat-square" alt="Latest Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/youshinh/syki?style=flat-square" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-Windows-blue?style=flat-square" alt="Platform: Windows">
  <img src="https://img.shields.io/badge/privacy-local--first-brightgreen?style=flat-square" alt="Local-First Privacy">
</p>

思考を即座に受け止める、Rust 製 Windows ネイティブ・超軽量 Markdown スクラッチパッド＆AI ワークフロー基盤。 &nbsp;[English](README.md) · **日本語**

<p align="center">
  <img src="img/poster_icon.svg" width="470" alt="syki">
  <img src="img/poster_spec.svg" width="470" alt="syki primitives">
</p>

---

### コアコンセプト

- **保管庫ではなく「膜」。** プレーン Markdown（`scraps/YYYY-MM-DD.md`）、ロックインゼロ。
- **呼ぶまで眠る。** `Attached` → `Suspended` → `Detached`（待機 RAM 3–5 MB、復帰 `< 15 ms`）。
- **高速な核、遅延ロードの皮。** 純 Rust コア（`syki-core`）。`user.css` と `plugins/` で自在に拡張。

---

### 主な機能（5 選）

| # | 機能 | 内容 |
| :--- | :--- | :--- |
| **01** | **即時キャプチャ** | ~1 ms Win32 Quick Capture、画面切り抜き、ローカル STT / WASAPI 会議録音、Vision OCR |
| **02** | **Markdown スタジオ** | 分割タブ、日本語 IME Guardian、KaTeX / Mermaid プレビュー、CDP PDF、内蔵 ConPTY 端末 |
| **03** | **ハイブリッド検索** | プレーンテキスト Scrap 横断の並列全文検索 ＋ `mmap` SIMD 意味検索 |
| **04** | **ローカル優先 AI** | インライン書き換え、補完、Vision OCR、画像生成 — ローカル OpenAI 互換または Gemini |
| **05** | **CLI ＆ プラグイン** | `syki-cli` 標準入力パイプ ＆ Named Pipe JSON-RPC、`user.css`、サンドボックス `window.syki` プラグイン |

---

### クイックスタート（3 ステップ・AI エージェント前提）

1. **ダウンロード＆起動** — [Releases](https://github.com/youshinh/syki/releases/latest) から `syki-*-windows-x64.zip` を解凍して `syki.exe` を起動*（`syki-cli` 用にフォルダを `PATH` へ追加）*。
2. **スキルを読み込む** — 同梱の `skills/syki-customize/SKILL.md`*（原本: [`SKILL.md`](frontend/resources/syki-customize/SKILL.md)）* を AI エージェントへ登録。
3. **聞く＆任せる** — 使い方・ショートカット・トラブルシュートをエージェントに質問し、`config.json` 設定・`user.css`・プラグイン作成・`syki-cli` 操作まで委譲。

---

### ライセンス ＆ 商標

[MIT License](LICENSE) のもとで公開されています。

<sub>
Windows および WebView2 は Microsoft Corporation の商標です。Gemini は Google LLC の商標です。OpenAI は OpenAI, Inc. の商標です。その他すべての商標は各所有者に帰属します。本プロジェクトはこれら各社とは独立しており、提携・承認されたものではありません。
</sub>
