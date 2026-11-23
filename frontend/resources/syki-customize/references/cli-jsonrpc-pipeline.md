# syki-cli・Named Pipe JSON-RPC・パイプライン統合リファレンス

外部の AI Agent・スクリプト・MCP サーバー・AutoHotkey・Stream Deck から `syki-lite` をリアルタイムに読み書き・制御するための公式インターフェース仕様です。

---

## 1. `syki-cli.exe` 全 10 サブコマンド一覧（起動 `< 5ms`）

| コマンド | 用途・代表例 |
|---|---|
| `syki-cli info` | 稼働インスタンスの `dataDir` / `instanceId` / IPC 状態を JSON 出力 |
| `syki-cli buffer ...` | アクティブエディタのテキスト取得・置換・挿入 |
| `syki-cli tab ...` | タブ一覧・新規作成・切替・PDF 出力（`syki-cli tab pdf`） |
| `syki-cli ui ...` | ウィンドウ表示・前面化・UI 制御 |
| `syki-cli scrap ...` | スクラップ検索・追記 |
| `syki-cli ocr <image>` | Vision LLM による画像 AI OCR 実行 |
| `syki-cli speech ...` | ローカル STT 音声文字起こし |
| `syki-cli pty ...` | 組み込み ConPTY ターミナル制御 |
| `syki-cli config ...` | `config.json` の参照・更新 |
| `syki-cli plugin ...` | プラグインの静的検査・一覧・状態確認・リロード・コマンド実行 |
| `<cmd> \| syki-cli` | 標準出力を日次スクラップへ直接パイプ投入（最大 10MB） |

### `syki-cli plugin` サブコマンド詳細
```powershell
# 1. 静的契約検証（構文・apiVersion・権限・禁止パターン・生色コード警告を JSON 出力）
syki-cli plugin check path\to\plugin.js

# 2. 導入済みプラグイン一覧・状態（ロード時間 ms・直近エラー・開発モード）
syki-cli plugin list
syki-cli plugin status [plugin_id]

# 3. 開発モード有効化済みプラグインの実機ホットリロード
syki-cli plugin reload <plugin_id>

# 4. exposeRpc: true で公開された自作コマンドの実行（既定で headless: true 非表示実行）
syki-cli plugin run <command_id> --args '{"query":"syki architecture"}'
syki-cli plugin run <command_id> --show   # ウィンドウを前面表示して実行したい場合
```

---

## 2. Named Pipe JSON-RPC 直接呼び出しプロトコル

`syki.exe` 起動中（トレイ常駐 `Detached` 中も含む）、以下の Windows Named Pipe が待機しています：
- **Pipe 名**: `\\.\pipe\syki-lite-<instance_id>`（既定は `ipc-session.json` に記載）
- **認証情報ファイル**: `<data_dir>/ipc-session.json`（`{ "pid", "pipe_name", "token" }`）
- **フレーミング**: 1 行 1 JSON（末尾 `\n`、最大 12MB）

### リクエスト（`IpcEnvelope`）
```json
{
  "id": "req-1",
  "auth": "<ipc-session.json の token>",
  "action": "rpc",
  "method": "plugin.run",
  "params": { "commandId": "local-rag.ask", "args": { "query": "設計方針" } },
  "headless": true
}
```
### レスポンス（`IpcReply`）
```json
{ "id": "req-1", "ok": true, "result": { "answer": "...", "sources": ["docs/SPEC.md"] } }
```

### プラグイン側で外部 JSON-RPC に機能を公開する方法
```js
syki.sys.registerCommand({
  id: 'my-rag.query',
  title: 'ローカルRAG検索',
  exposeRpc: true, // ← これを明示したコマンドのみ外部 JSON-RPC から呼べる
  async run(args) {
    const hits = await syki.knowledge.searchSemantic(args.query, { limit: 5 });
    const answer = await syki.exec.llm('過去メモを踏まえて回答して', {
      input: JSON.stringify(hits),
    });
    return { answer, sources: hits.map((h) => h.path) };
  },
});
```
- `Detached`（3–5MB 待機中）に `headless: true` で呼ばれた場合、ウィンドウを表示せず（フォーカスを奪わず）裏で Reattach → 実行 → 結果返却 → 自動 `Detached` 復帰します。

---

## 3. シェルパイプライン ＆ Policy Decision セキュリティ

### プラグインから安全に外部 CLI を呼ぶ（`exec.shell`）
- コマンド文字列ではなく **プログラム名＋引数配列** で渡す（シェル経由しないためコマンドインジェクション不可）：
  ```js
  const res = await syki.exec.shell('git', ['diff', '--staged'], { signal });
  ```
- Rust 側 `PolicyDecisionEngine` が `allow` / `deny` / `ask` を判定：
  - 未承認プログラム → `POLICY_ASK` エラー（ユーザーが承認すると `trust-registry.json` の Allowlist に登録）
  - 危険シェルインタプリタ（`cmd.exe` 等）→ デフォルト `POLICY_DENIED`
  - すべての判定は最大 128 件の監査ログに記録（`getPolicyAuditLog` で確認可能）。

### 外部パイプ受信フック（`cat log.txt | syki-cli`）
```js
syki.sys.on('pipe:in', async ({ text, source }) => {
  const d = await syki.flow.decide({
    input: text,
    choices: [
      { id: 'normal', description: '通常ログ' },
      { id: 'critical', description: '致命的エラー・異常終了' },
    ],
    rule: (s) => (s.includes('FATAL') ? 'critical' : null),
    fallback: 'normal',
  });
  if (d.choiceId === 'critical') {
    syki.ui.toast('致命的ログを検出しました', 'error');
    return { handled: true, transformedText: `## [ALERT]\n${text}` };
  }
});
```
