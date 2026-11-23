# config.json 設定 ＆ user.css / レスポンシブ / 印刷カスタム完全ガイド

## 1. `config.json` の場所と主要設定手順

- **配置場所**: `%APPDATA%\syki-sok\config.json`（環境変数 `SYKI_DATA_DIR` 指定時はその配下）
- CLI での確認・設定: `syki-cli config get` / `syki-cli info`

### 設定例（Gemini 通常 LLM ＋ `gemini-decision-1` 判定専用モデル ＋ ローカル意味検索）
```json
{
  "llm": {
    "provider": "gemini",
    "model": "gemini-3.5-flash-lite",
    "baseUrl": "https://generativelanguage.googleapis.com/v1beta/openai",
    "apiKey": ""
  },
  "action": {
    "provider": "gemini",
    "model": "gemini-decision-1",
    "apiKey": ""
  },
  "scraps": {
    "scrapDir": "",
    "maxPipeSizeMB": 10
  },
  "semantic": {
    "enabled": true
  }
}
```
> **セキュリティ推奨**: `apiKey` は `config.json` に平文で書かず、空欄のまま環境変数（`GEMINI_API_KEY` / `OPENAI_API_KEY` / `ANTHROPIC_API_KEY`）で渡すことを推奨します。

### プロバイダ別の設定パターン
- **Google Gemini Decisions API（超高速・出力トークン 0）**:
  - `action.model`: `"gemini-decision-1"`（自動的に `POST https://generativelanguage.googleapis.com/v1beta/decisions` + `x-goog-api-key` で呼び出される）。
  - `flow.decide(...)` のデフォルトエンジンとして使用され、1回 ~360ms・出力トークン 0 で分類・ルーティング・合否判定を実行。
- **その他 Decision モデル / フォールバック**:
  - `jev-latest`（TypeSafe）/ `gpt-6-luna`（OpenAI Decisions）/ ローカル Ollama（`http://127.0.0.1:11434`, 例: `qwen2.5:7b`）にも対応。
  - `action` が空欄なら `llm` の設定へ自動フォールバックし、両方未設定でも起動エラーにはならず `flow.decide` は `rule` → `fallback` で安全動作する。

---

## 2. `user.css` カスタム完全ガイド

- **配置場所**: `<syki.exe の隣>/user.css`（最優先） または `%APPDATA%\syki-sok\user.css`
- **0ms デフォルト起動**: `user.css` が存在しない環境では fetch 0 回・DOM 操作 0 回。存在する環境のみ初期描画時に即適用（FOUC ゼロ）され、保存時にホットリロード可能。
- **外部読み込み制限**: 厳格な CSP により `@import url("https://...")` 等の外部通信は 403 遮断される。ローカルインストール済みフォント名指定のみ使用すること。

### 公式 CSS フック契約（`data-syki-part` / `data-syki-action`）
- **領域属性 (`data-syki-part`)**:
  `toolbar`, `tabs`, `tab-dock`, `statusbar`, `editor`, `gutter`, `preview`, `terminal`, `side-panel`, `inline-bar`, `find-bar`, `running-tasks`, `settings`
- **ボタン属性**:
  `data-syki-action="<id>"`, `data-priority="primary" | "secondary"`, ラベル `[data-syki-label]`
- **保護領域**: セキュリティ承認ダイアログと緊急リセット UI は closed Shadow DOM / ネイティブダイアログで隔離されており、`user.css` で隠せない。

### 主要セマンティック変数（`tokens.css`）
- **背景**: `--bg-main`, `--bg-header`, `--bg-editor`, `--bg-modal`, `--bg-field`, `--bg-input`, `--bg-btn`, `--bg-btn-hover`, `--bg-preview`, `--bg-statusbar`
- **文字**: `--text-main`, `--text-muted`, `--text-active`, `--text-editor`, `--text-on-accent`, `--text-ok`, `--text-warn-orange`, `--text-error`
- **アクセント・装飾**: `--accent-color`, `--accent-hover`, `--accent-label`, `--border-color`, `--radius-sm`, `--radius-md`, `--radius-lg`, `--radius-pill`

### Win32 Quick Capture への自動色同期
`user.css` で `--bg-modal`, `--bg-field`, `--border-color`, `--bg-btn`, `--accent-color`, `--text-main`, `--text-on-accent`, `--text-muted` を変更すると、`ext_theme_sync.js` が解決済み `#rrggbb` を `<data_dir>/qc-theme-cache.json` へ自動保存し、Win32 ネイティブ Quick Capture にも `<0.1ms` で反映される。

---

## 3. 実用 `user.css` スニペット集

### A. ウィンドウ幅に応じた「ミニ付箋モード」＆「ワイド3カラム」
```css
/* 幅 560px 未満: 画面端に寄せたミニメモモード */
@media (max-width: 560px) {
  [data-syki-part="statusbar"],
  [data-syki-part="tabs"],
  [data-syki-part="toolbar"] [data-priority="secondary"],
  [data-syki-part="toolbar"] [data-syki-label] {
    display: none;
  }
}

/* 設定モーダルを広く2カラム風に */
[data-syki-part="settings"] .modal-content {
  max-width: 920px;
}
```

### B. 印刷・CDP PDF 出力専用スタイル（`@media print`）
```css
@media print {
  @page { size: A4; margin: 18mm; }
  [data-syki-part="toolbar"],
  [data-syki-part="tabs"],
  [data-syki-part="statusbar"],
  [data-syki-part="terminal"] {
    display: none !important;
  }
  [data-syki-part="preview"] h2 {
    break-before: page; /* 見出しごとに改ページして資料化 */
  }
}
```
