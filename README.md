<div align="center">

<img src="app.png" width="96" height="96" alt="syki" />

<h1>syki</h1>

<p><strong>Instant thought membrane.</strong></p>
<p>Featherweight Windows-native scratchpad &amp; AI workflow kernel in Rust + Win32 + WebView2.</p>

<p>
  <a href="https://github.com/youshinh/syki/releases"><img src="https://img.shields.io/github/v/release/youshinh/syki?style=flat-square&amp;color=111111" alt="Release" /></a>
  <img src="https://img.shields.io/badge/platform-Windows-111111?style=flat-square" alt="Platform: Windows" />
  <img src="https://img.shields.io/badge/rust-2021-111111?style=flat-square" alt="Rust 2021" />
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-111111?style=flat-square" alt="License: MIT" /></a>
</p>

<p>
  <code>&lt; 5 ms</code> CLI ·
  <code>&lt; 15 ms</code> wake ·
  <code>3–5 MB</code> idle RAM
</p>

<p>
  <strong>English</strong> · <a href="README.ja.md">日本語</a>
</p>

</div>

---

## Core Concept

- **Membrane, not a vault.** Plain Markdown (`scraps/YYYY-MM-DD.md`), zero lock-in.
- **Dormant until summoned.** `Attached` → `Suspended` → `Detached` (3–5 MB tray footprint).
- **Fast core, lazy skin.** Pure Rust kernel (`syki-core`); customize via `user.css` and `plugins/`.

## Capabilities

| # | Capability | Scope |
|---|---|---|
| **01** | **Instant Capture** | ~1 ms Win32 Quick Capture, screen grab, local STT / WASAPI meeting audio, Vision OCR |
| **02** | **Markdown Studio** | Split tabs, Japanese IME Guardian, KaTeX / Mermaid preview, CDP PDF, ConPTY terminal |
| **03** | **Hybrid Search** | Parallel full-text + `mmap` SIMD semantic search across plain-text Scraps |
| **04** | **Local-First AI** | Inline rewrite, autocomplete, Vision OCR, image gen — local OpenAI-compatible or Gemini |
| **05** | **CLI & Plugins** | `syki-cli` stdin pipes &amp; Named Pipe JSON-RPC, `user.css`, sandboxed `window.syki` plugins |

---

## Quick Start (3 Steps · AI Agent First)

1. **Download & Launch** — Unzip `syki-*-windows-x64.zip` from [Releases](https://github.com/youshinh/syki/releases) and run `syki.exe` *(add folder to `PATH` for `syki-cli`)*.
2. **Load the Skill** — Register the bundled `skills/syki-customize/SKILL.md` *(source: [`SKILL.md`](frontend/resources/syki-customize/SKILL.md))* in your AI agent.
3. **Ask & Delegate** — Ask your agent how to use `syki` (shortcuts, features, troubleshooting), or let it configure `config.json`, style `user.css`, build plugins, and drive `syki-cli`.

## License & Trademarks

Released under the [MIT License](LICENSE).

<sub>
Windows and WebView2 are trademarks of Microsoft Corporation. Gemini is a trademark of Google LLC. OpenAI is a trademark of OpenAI, Inc. All other trademarks belong to their respective owners; this project is independent and unaffiliated.
</sub>
