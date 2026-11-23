# syki 実運用レシピ集（テスト検証済み 12 パターン）

以下はすべて `syki` の結合 E2E テストおよび実機 Gemini API（`gemini-decision-1` + `gemini-3.5-flash-lite`）で動作確認済みの設計パターンです。AI Agent はユーザー要望に最も近いレシピをベースにプラグインを生成してください。

---

## レシピ 1: 音声入力 → Decision 図種別選定 → Mermaid → オフスクリーン PNG 貼付
- **参照サンプル**: `examples/plugins/voice-to-diagram/plugin.js`
- **権限**: `['voice', 'decision', 'llm']`
- **流れ**: `input.voice({ signal })` → `ui.review(text)` → `flow.decide({ choices: ['flowchart','sequence','er','mindmap'], fallback: 'flowchart' })`（`gemini-decision-1` 出力トークン 0）→ `exec.llm` → `exec.render.mermaidToPng(code, { theme: syki.ui.getTheme(), signal })` → `knowledge.assets.save` → `editor.insert`

## レシピ 2: 設定連動・翻訳ミニバー（`ghost_diff` 差分承認）
- **参照サンプル**: `examples/plugins/translate-bar/plugin.js`
- **権限**: `['llm']`
- **流れ**: `ui.registerSettings([...])` → `sys.on('settings:change', ...)` で `ActionHandle.update({ label })` を即時更新 → `exec.llm` → `ui.diffConfirm(before, after)`（アクティブエディタなら既存 `ghost_diff` 表示）→ `editor.replaceSelection`

## レシピ 3: ローカル意味検索 RAG サイドパネル（Shadow DOM 隔離）
- **参照サンプル**: `examples/plugins/local-rag-panel/plugin.js` + `style.css`
- **権限**: `['semantic', 'llm']`
- **流れ**: `ui.mountPanel('side-panel', { render })` → `knowledge.searchSemantic(query, { limit: 5 })` → `exec.llm`（`stripSecrets: true` 既定）→ 出典リンク付きでパネル表示＆1クリック挿入

## レシピ 4: テスト合格まで自動修正する自己修復ループ
- **権限**: `['llm', 'shell:pytest']`
- **流れ**: `flow.loop({ init, maxTurns: 3, step, until })` 内で `exec.shell('pytest', ['-q'], { signal })` → 失敗時は `stderr` を `exec.llm` に渡してパッチ生成 → `exitCode === 0` で収束し `editor.replaceSelection`

## レシピ 5: 保存前シークレット混入スキャナー
- **権限**: `[]`（ローカル完結）
- **流れ**: `sys.on('file:before-save', ({ text }) => ...)` で API キー・トークンを検出したら `{ cancel: true, reason }` を返して保存を安全ブロック。同時に `sys.stripSecrets(text)` でマスク済みテキストを `sys.clipboard.writeText` へコピー可能

## レシピ 6: `Detached` 中の Quick Capture 保留キュー自動トリアージ
- **権限**: `['decision']`
- **流れ**: `sys.on('quick-capture:saved', async ({ text }) => ...)` で `flow.decide`（`task` / `idea` / `bug` / `memo`）を実行し、`knowledge.notes.append('todo.md', ...)` 等へ自動振り分け

## レシピ 7: PDF ドロップ横取り → 要約ノート自動生成
- **権限**: `['llm', 'shell:pdftotext']`
- **流れ**: `sys.on('file:drop', async ({ paths }) => ...)` で `.pdf` を `{ handled: true }` で横取り → `exec.shell('pdftotext', [paths[0], '-'])` → `exec.llm` → `knowledge.notes.create({ title, content, folder: 'summaries' })`

## レシピ 8: 複数タブ横断比較マトリクス生成
- **権限**: `['llm']`
- **流れ**: `editor.listTabs()` + `editor.getTabText(id)` で全タブを収集 → `exec.llm('共通点・対立点を比較表にして')` → `editor.openTab({ title: 'Comparison.md', content })`

## レシピ 9: 3 観点並列コードレビュー（`flow.parallel`）
- **権限**: `['llm']`
- **流れ**: `flow.parallel([exec.llm('Security'), exec.llm('Performance'), exec.llm('Readability')])` を `reqId` デマルチプレクサで同時実行し、統合結果を新規タブへ出力

## レシピ 10: 過去 ADR との矛盾チェック（意味検索 × Decision）
- **権限**: `['semantic', 'decision']`
- **流れ**: `knowledge.searchSemantic(editor.getSelection())` → `flow.decide({ choices: ['consistent', 'conflict'], fallback: 'consistent' })` → `conflict` なら `ui.toast(..., 'warn')`

## レシピ 11: ポモドーロタイマー（`Detached` 状態退避対応）
- **権限**: `[]`
- **流れ**: `ui.registerAction({ slot: 'statusbar', badge: '25:00' })` を `ActionHandle.update` で毎分更新。`sys.on('host:before-detach', () => sys.storage.set(...))`（500ms 以内に完了）で残り時間を退避し、再アタッチ時に復元

## レシピ 12: 外部 Agent / Stream Deck 連携コマンド（JSON-RPC 公開）
- **権限**: `['semantic', 'llm']`
- **流れ**: `sys.registerCommand({ id: 'agent.brief', title: '文脈取得', exposeRpc: true, run })` を登録。外部 Agent から `syki-cli plugin run agent.brief --args '{"topic":"..."}'` でヘッドレス呼び出しし、JSON 戻り値を直接受け取る
