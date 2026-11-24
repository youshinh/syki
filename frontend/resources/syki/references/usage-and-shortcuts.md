# syki 使い方・全機能・ショートカット完全リファレンス

## 1. syki の特徴とアーキテクチャ
- **Windows ネイティブ・超高速**: Rust + Win32 + WebView2（単一メインウィンドウ＋分割ペイン＋Win32 ネイティブ Quick Capture）。
- **省メモリ 3 状態待機（`CanvasHost`）**:
  - `Attached`（通常表示）→ `SuspendedLowMem`（最小化時低メモリ）→ `Detached`（トレイ待機時 WebView2 破棄で **RAM 3〜5 MB**）。
  - ホットキーまたは `syki-cli` / JSON-RPC 呼び出しで即座に再アタッチ（`< 15ms` 復帰）。
  - プラグインや LLM の非同期ジョブ実行中は `ActiveTaskGuard` が自動でデタッチを抑止し、処理の途中破棄を防ぐ。
- **データ互換**: 既存の `scraps/*.md`（既定は `~/Documents/syki-sok/scraps`、`SYKI_DATA_DIR` 指定時は `<SYKI_DATA_DIR>/scraps`）、`config.json`、`semindex/`、`ipc-session.json` と完全互換。

---

## 2. 全 16 保持機能一覧（`F01`〜`F23`）

| ID | 機能 | 主な使い方・ポイント |
|---|---|---|
| `F01` | **Markdown エディタ I/O** | タブ・分割ペイン・セッション自動復元・原子保存・Shift_JIS 自動判定・外部変更ホットリロード（`notify`） |
| `F02` | **日次スクラップ＆並列全文検索** | `scraps/YYYY-MM-DD.md` へ高速追記。タグ・日付フィルタ付き並列全文検索 |
| `F04` | **`syki-cli.exe` / Named Pipe JSON-RPC / `cat \| syki`** | 10 サブコマンド（~20–30 ms 起動 ※Windows プロセス起動・AV スキャン込み）・標準入力パイプ投入・単一インスタンス IPC 制御 |
| `F05` | **シェルパイプ (`Ctrl+E`)** | 選択テキストを任意 CLI コマンドの stdin に通して stdout で置換 |
| `F06` | **ライブプレビュー** | `markdown-it` + `KaTeX` 数式 + `Mermaid` 図解のリアルタイム描画 |
| `F07` | **日本語 IME Guardian** | `imm32` 連携による IME 状態保護・誤変換再入力（`ime_retype`）・IME 変換中のショートカット誤爆自動抑止 |
| `F08` | **印刷プレビュー＆CDP PDF 出力** | `Page.printToPDF` による高品質 PDF 書き出し。`user.css` の `@media print` がそのまま効く |
| `F10a` | **Win32 ネイティブ Quick Capture** | WebView2 不使用（~1ms 起動）。トレイ待機（`Detached`）中でも瞬時にテキスト・音声・スクショをスクラップへ保存 |
| `F10b` | **Win32 画面領域キャプチャ (`imgcap`)** | 画面の一部を切り出して即ノート貼付／AI OCR 入力 |
| `F11` | **Cursor AI（内蔵 LLM）** | `Ctrl+L`（質問）・`Ctrl+K`（インライン書き換え＋`ghost_diff` 差分承認）・`Alt+C`・`Tab` オートコンプリート。送信前に `secret_strip` で API キー等を自動マスク |
| `F12` | **画像生成＆インフォグラフィック** | OpenAI 互換 / Gemini 画像生成モデルで図解・画像をローカルアセット保存＆自動挿入 |
| `F13` | **ローカル意味検索 (`semindex`)** | `memmap2` + SIMD コサイン類似度検索で過去スクラップを高速 RAG 検索 |
| `F18` | **組み込み PTY ターミナル** | `portable-pty` (ConPTY) + `xterm.js` + Git Worktree 作成ヘルパー |
| `F21` | **ローカル音声入力 (STT) & WAV ドロップ文字起こし** | `speech.cpp` + `Haas-ja` デフォルト（または OpenAI 互換 / Gemini STT）。純 Rust WAV 変換。WAV ファイルのウィンドウドロップでも自動文字起こし＆ノート追記 |
| `F22` | **会議録音** | WASAPI ループバック（相手音声）＋マイクのデュアル録音＆ミックス文字起こし |
| `F23` | **画像貼付＆AI OCR** | 貼付／ドロップ画像を Vision LLM で OCR・構造化テキスト化（`syki-cli ocr run <image>` / `ocr <image>` 対応） |

---

## 3. 主要ショートカット ＆ 起動オプション

| 操作 | ショートカット / オプション |
|---|---|
| AI 質問バー | `Ctrl+L` |
| AI インライン書き換え（`ghost_diff`） | `Ctrl+K` |
| シェルコマンドフィルタ | `Ctrl+E` |
| AI アクション / 補完 | `Alt+C` / `Tab` |
| 検索・置換 / 行ジャンプ | `Ctrl+F` / `Ctrl+H` / `Ctrl+G` |
| セーフモード起動（`user.css`・全プラグイン無効化） | `syki.exe --safe-mode` |
| データフォルダ切替（別環境・検証用） | 環境変数 `SYKI_DATA_DIR=<dir>` |
| 複数インスタンス分離（Named Pipe 衝突回避） | 環境変数 `SYKI_INSTANCE_ID=<name>` |

---

## 4. リセット・復旧手順（レベル別）

> **重要**: ユーザーのメモ本体は `<data_dir>/scraps/`（または設定した `scrapDir`）に保存されています。リセット作業時に **`scraps/` フォルダは絶対に削除しないでください**。

| 目的 | 具体的な手順 | 影響範囲 |
|---|---|---|
| **① 一時的に全拡張を無効化して起動（非破壊）** | `syki.exe --safe-mode` で起動 | `user.css` と全プラグインをスキップ（ファイルは残る） |
| **② カスタム CSS だけリセット** | 有効側（`<exe隣>` または `<data_dir>`）の `user.css` と `<data_dir>/qc-theme-cache.json` を削除／リネーム | 見た目・Quick Capture 色がデフォルトへ戻る |
| **③ 特定プラグインだけ無効化／削除** | 対象 `plugins/<id>/` フォルダを削除（または拡張ハブで無効化）。起動クラッシュしたプラグインは **サーキットブレーカー** が次回自動で `isolated` 隔離する | 該当プラグインのみ除去 |
| **④ プラグイン信頼・コマンド許可リストを初期化** | `<data_dir>/trust-registry.json` を削除して再起動 | SHA-256 承認・開発モード・`exec.shell` Allowlist が初期状態に戻る |
| **⑤ アプリ設定（LLM・ショートカット等）を初期化** | `<data_dir>/config.json` をリネームまたは削除して再起動 | 設定がデフォルト値で再生成される |
| **⑥ 開いているタブ状態を初期化** | `<data_dir>/session.json` を削除して再起動 | タブセッションのみクリア（保存済みノートは残る） |
| **⑦ 意味検索インデックスを再構築** | `<data_dir>/semindex/` を削除して再起動（または設定からインデックス更新） | 意味検索ベクトルをクリーン再生成 |
| **⑧ WebView2 キャッシュを初期化** | `syki.exe` 終了後に `<data_dir>/webview2/` を削除して再起動 | ブラウザキャッシュ・ローカルストレージのみ初期化 |

### よくあるトラブルと即効チェック
- **編集した `user.css` が反映されない**: `<exe隣>/user.css`（Portable）が存在すると `%APPDATA%\syki-sok\user.css` より優先される（単一ソース原則）。`syki-cli info` で実効 `dataDir` を確認すること。
- **プラグインを変更しても `plugin.reload` が拒否される**: 未承認の新規プラグインは `plugin.reload` で自動有効化できない。初回のみ拡張ハブで承認し、開発モードを有効化すること。
