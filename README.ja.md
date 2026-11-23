# syki

<p align="center">
  <a href="https://github.com/youshinh/syki/releases/latest"><img src="https://img.shields.io/github/v/release/youshinh/syki?style=flat-square" alt="Latest Release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/youshinh/syki?style=flat-square" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/platform-Windows-blue?style=flat-square" alt="Platform: Windows">
  <img src="https://img.shields.io/badge/privacy-local--first-brightgreen?style=flat-square" alt="Local-First Privacy">
</p>

超軽量・即起動 Markdown スクラッチパッド。 &nbsp;[English](README.md) · **日本語**

<p align="center">
  <img src="img/poster_icon.svg" width="470" alt="syki">
  <img src="img/poster_spec.svg" width="470" alt="syki primitives">
</p>

---

### コアコンセプト

- **保管庫ではなく「膜」。** プレーン `.md`、ロックインゼロ。
- **呼ぶまで眠る。** 待機 3–5 MB、復帰 `< 15 ms`。
- **高速な核、遅延ロードの皮。** Rust コア、`user.css` ＆ `plugins/` で拡張。

---

### 主な機能

| # | 機能 | 内容 |
| :--- | :--- | :--- |
| **01** | **即時キャプチャ** | ~1 ms ポップアップ、画面切り抜き、ローカル STT / WASAPI、Vision OCR |
| **02** | **Markdown スタジオ** | 分割タブ、IME Guardian、KaTeX / Mermaid、PDF、内蔵 PTY |
| **03** | **ハイブリッド検索** | 全文検索 ＋ `mmap` SIMD 意味検索 |
| **04** | **ローカル優先 AI** | インライン書き換え、補完、Vision OCR、画像生成 |
| **05** | **CLI ＆ プラグイン** | `syki-cli` パイプ ＆ RPC、`user.css`、`window.syki` プラグイン |

---

### クイックスタート

1. **ダウンロード** — [Releases](https://github.com/youshinh/syki/releases/latest) の `syki-*-windows-x64.zip` を解凍し `syki.exe` を起動。
2. **スキル登録** — 同梱 `skills/syki-customize/SKILL.md` を AI エージェントへ追加。
3. **聞く＆拡張** — 使い方・設定・カスタマイズはすべてエージェントに指示。

---

### ライセンス

[MIT](LICENSE)
