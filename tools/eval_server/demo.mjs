// Demo data for headless-Edge functional & smoke evaluations under syki: deterministic, offline, no private content.
// buildBoot(query, title) returns the object the mock backend reads as window.__DOCSHOT_BOOT.

export const FIXED_CLOCK = { y: 2026, m: 8, d: 18, h: 10, mi: 24 }; // 2026-09-18 10:24 (month is 0-based)

const ROOT = 'C:\\Users\\demo\\Documents\\notes';
const SCRAP_DIR = '~/Documents/syki/scraps';
const P = (name) => ROOT + '\\' + name;

const MAIN_EN = [
  '# syki rollout',
  '',
  'Plan for shipping the lightweight Rust editor and CLI. Owner: Aya.',
  '',
  '## Schedule',
  '',
  '| Task            | Owner | Due   |',
  '|-----------------|-------|-------|',
  '| API design memo | Aya   | 09-24 |',
  '| Beta test       | Ken   | 10-01 |',
  '| Release notes   | Mio   | 10-08 |',
  '',
  '## API sketch',
  '',
  '```json',
  '{',
  '  "endpoint": "/upload-batch",',
  '  "rateLimit": 60',
  '}',
  '```',
  '',
  '## Checklist',
  '',
  '- [x] Draft the API design',
  '- [ ] Review rate limits at the weekly review',
  '- [ ] Prepare the release notes',
  '',
  '## Flow',
  '',
  '```mermaid',
  'graph LR',
  '  A[Editor] --> B[Rust Core]',
  '  B --> C[Note]',
  '```',
  '',
  '## Attachments',
  '',
  '[spec.pdf](./assets/spec.pdf)',
  '![diagram](./assets/diagram.png)',
  '',
  '## Open questions',
  '',
  '- Should the idle WebView2 detach delay default to 5 minutes?',
  '- Where should voice recordings be cached by default?',
  '- Who signs off the rate limit?',
  '',
  '## Next steps',
  '',
  '- Send the beta invite list to Ken',
  '- Book the review meeting',
  '',
].join('\n');

const MAIN_JA = [
  '# syki 展開計画',
  '',
  '軽量 Rust エディタと CLI を公開するための計画メモ。担当: 佐藤。',
  '',
  '## スケジュール',
  '',
  '| タスク | 担当 | 期限 |',
  '|--------|------|------|',
  '| API 設計メモ | 佐藤 | 09-24 |',
  '| ベータ試験 | 田中 | 10-01 |',
  '| リリースノート | 鈴木 | 10-08 |',
  '',
  '## API スケッチ',
  '',
  '```json',
  '{',
  '  "endpoint": "/upload-batch",',
  '  "rateLimit": 60',
  '}',
  '```',
  '',
  '## チェックリスト',
  '',
  '- [x] API 設計を下書きする',
  '- [ ] 週次レビューでレート制限を見直す',
  '- [ ] リリースノートを用意する',
  '',
  '## フロー',
  '',
  '```mermaid',
  'graph LR',
  '  A[エディタ] --> B[Rust コア]',
  '  B --> C[ノート]',
  '```',
  '',
  '## 添付',
  '',
  '[spec.pdf](./assets/spec.pdf)',
  '![diagram](./assets/diagram.png)',
  '',
  '## 未解決の質問',
  '',
  '- アイドル時の WebView2 デタッチ猶予は既定 5 分にするか',
  '- 音声キャッシュの保存先の既定値をどうするか',
  '- レート制限の承認者は誰か',
  '',
  '## 次のアクション',
  '',
  '- 田中さんにベータ招待リストを送る',
  '- レビュー会議を予約する',
  '',
].join('\n');

const MEETING_EN = [
  '# Meeting 2026-09-18',
  '',
  'Attendees: Aya, Ken, Mio',
  '',
  '## Decisions',
  '',
  '- Ship the beta on 10-01.',
  '- Keep the rate limit at 60 requests per minute.',
  '',
  '## Next steps',
  '',
  '- [ ] Ken: set up the beta test group',
  '- [ ] Mio: draft the release notes',
  '',
].join('\n');

const MEETING_JA = [
  '# 会議メモ 2026-09-18',
  '',
  '参加者: 佐藤、田中、鈴木',
  '',
  '## 決定事項',
  '',
  '- ベータ版は 10-01 に公開する。',
  '- レート制限は 1 分あたり 60 リクエストのままにする。',
  '',
  '## 次のアクション',
  '',
  '- [ ] 田中: ベータ試験のグループを用意する',
  '- [ ] 鈴木: リリースノートを下書きする',
  '',
].join('\n');

const IDEAS_EN = [
  '# Ideas',
  '',
  '- Sub-5ms CLI cold start',
  '- Voice notes transcribed via Haas-ja local STT',
  '- A weekly digest of open tasks',
  '',
].join('\n');

const IDEAS_JA = [
  '# アイデア',
  '',
  '- 5ms 未満の CLI コールドスタート',
  '- Haas-ja ローカル文字起こしによる音声メモ',
  '- 未完了タスクの週次ダイジェスト',
  '',
].join('\n');

const WORKSPACE_EN = [
  { name: 'api-design-memo.md', title: 'API design memo', snippet: 'Draft the API design first. Rate limits, auth and the upload-batch endpoint.' },
  { name: 'weekly-review.md', title: 'Weekly review', snippet: 'Review rate limits, release notes and open risks every Friday.' },
  { name: 'reading-list.md', title: 'Reading list', snippet: 'Articles about local-first software and Markdown tooling.' },
  { name: 'release-checklist.md', title: 'Release checklist', snippet: 'Tag, build, smoke test, publish the notes.' },
  { name: 'meeting-2026-09-10.md', title: 'Meeting 2026-09-10', snippet: 'Kick-off. Scope of the Rust rewrite and owners.' },
];

const WORKSPACE_JA = [
  { name: 'api-design-memo.md', title: 'API 設計メモ', snippet: 'API 設計を下書きする。レート制限とバッチ送信の仕様を整理する。' },
  { name: 'weekly-review.md', title: '週次レビュー', snippet: '議事: 週次レビューでレート制限を見直す。リリースノートの担当を決める。' },
  { name: 'reading-list.md', title: '読書リスト', snippet: 'ローカルファーストのソフトウェアと Markdown 周辺ツールの記事。' },
  { name: 'release-checklist.md', title: 'リリースチェックリスト', snippet: 'タグ付け、ビルド、動作確認、ノート公開。' },
  { name: 'meeting-2026-09-10.md', title: '会議メモ 2026-09-10', snippet: 'キックオフ。Rust 刷新の範囲と担当。' },
];

const SCRAPS_EN = [
  { name: '2026-09-17.md', lines: ['# 2026-09-17', '', '10:12 API design: keep /upload-batch, add a size limit', '10:40 Ask Ken about the beta group', '15:05 Reading: local-first sync notes', ''] },
  { name: '2026-09-15.md', lines: ['# 2026-09-15', '', '09:30 Weekly review: rate limits stay at 60 per minute', '11:02 API keys are never stored in notes', '17:20 Draft the release notes tomorrow', ''] },
  { name: '2026-09-12.md', lines: ['# 2026-09-12', '', '13:15 Quick capture: native Win32 popup wakes in 1 ms', '14:48 API error format: plain text, one line', '16:30 Book list for the weekend', ''] },
];

const SCRAPS_JA = [
  { name: '2026-09-17.md', lines: ['# 2026-09-17', '', '10:12 API 設計: /upload-batch は残し、サイズ上限を追加する', '10:40 田中さんにベータ参加者の件を確認', '15:05 読書: ローカルファースト同期のメモ', ''] },
  { name: '2026-09-15.md', lines: ['# 2026-09-15', '', '09:30 週次レビュー: レート制限は 1 分あたり 60 のまま', '11:02 API キーはノートに保存しない', '17:20 リリースノートは明日下書きする', ''] },
  { name: '2026-09-12.md', lines: ['# 2026-09-12', '', '13:15 Quick Capture: ネイティブ Win32 ポップアップで 1ms 起動', '14:48 API のエラー形式: 1 行のプレーンテキスト', '16:30 週末の読書リスト', ''] },
];

function appearanceFor(query) {
  const look = query.look === 'paper' || query.look === 'ink' ? query.look : '';
  const accent = String(query.accent || '');
  if (!look && !accent) return null;
  const out = {};
  if (look) out.look = look;
  if (/^#[0-9a-fA-F]{6}$/.test(accent)) { out.accent = 'custom'; out.accentCustom = accent.toLowerCase(); }
  else if (['olive', 'blue', 'forest', 'charcoal', 'vermilion'].includes(accent)) out.accent = accent;
  return out;
}

export function buildBoot(query, title) {
  const lang = query.lang === 'ja' ? 'ja' : 'en';
  const ja = lang === 'ja';
  const main = ja ? MAIN_JA : MAIN_EN;

  const tabs = [
    { id: 'tab_demo_1', title: 'project-notes.md', path: P('project-notes.md'), content: main, isDirty: false, encoding: 'UTF-8', cursorPos: 0 },
    { id: 'tab_demo_2', title: 'meeting-2026-09-18.md', path: P('meeting-2026-09-18.md'), content: ja ? MEETING_JA : MEETING_EN, isDirty: false, encoding: 'UTF-8', cursorPos: 0 },
    { id: 'tab_demo_3', title: 'ideas.md', path: P('ideas.md'), content: ja ? IDEAS_JA : IDEAS_EN, isDirty: false, encoding: 'UTF-8', cursorPos: 0 },
  ];

  const session = {
    activeTabId: 'tab_demo_1',
    tabCounter: 4,
    isSplitMode: false,
    secondaryTabId: null,
    secondaryViewMode: 'editor',
    activePane: 'primary',
    isPreviewMode: false,
    tabs,
  };

  const config = {
    text: { baseUrl: 'http://localhost:11434', model: 'gemma4:latest', apiKey: '', systemPrompt: 'You are a helpful assistant. Provide concise, accurate markdown responses.' },
    autocomplete: { enabled: true, baseUrl: 'http://localhost:11434', model: 'gemma4:latest', apiKey: '', delayMs: 500, maxTokens: 30 },
    vision: { baseUrl: 'https://generativelanguage.googleapis.com', model: 'gemini-flash-lite-latest', apiKey: 'DEMO-KEY-NOT-REAL-0000', prompt: 'Transcribe the content of this image (text, diagrams, tables, code, etc.) into structured, faithful Markdown format.' },
    voice: { model: 'haas-ja-v1.0-q8_0', apiStyle: 'auto', languageCodes: ja ? ['ja-JP'] : [], mode: 'smart', customVocabulary: [], silence_timeout_sec: 5, prompt: ja ? 'この音声を正確に文字起こししてください。前置きや解説は不要です。句読点を含む自然な日本語テキストのみを出力してください。' : 'Transcribe this audio accurately. No preamble or commentary; output only the natural text with punctuation.', baseUrl: '', apiKey: '' },
    speech: { provider: 'whisper-local', model: 'haas-ja-v1.0-q8_0', language: ja ? 'ja' : 'en' },
    cli: { model: '', baseUrl: '', apiKey: '', systemPrompt: '', openResultInNewTab: true, openErrorInNewTab: true },
    image: { model: 'gemini-3.1-flash-lite-image', aspectRatio: '16:9', resolution: '1024', apiKey: '' },
    general: {
      language: lang,
      theme: 'olive',
      autoSave: true,
      pasteImageOcr: true,
      restoreSession: true,
      trayResident: true,
      splitViewOnStartup: false,
      imeGuardian: ja,
      aiCorrection: true,
      cursorAura: false,
      toolbarLayout: { order: [], hidden: [] },
      contextMenuLayout: { order: [], hidden: [] },
    },
    scraps: { scrapDir: SCRAP_DIR, maxPipeSizeMB: 10 },
    shortcuts: {},
  };
  const appearance = appearanceFor(query);
  if (appearance) config.appearance = appearance;

  const wsSrc = ja ? WORKSPACE_JA : WORKSPACE_EN;
  const notes = wsSrc.map((n) => ({ path: P(n.name), relPath: n.name, title: n.title, snippet: n.snippet }));

  const scrapSrc = ja ? SCRAPS_JA : SCRAPS_EN;
  const scraps = scrapSrc.map((s) => ({ filePath: SCRAP_DIR.replace('~', 'C:\\Users\\demo') + '\\' + s.name, fileName: s.name, content: s.lines.join('\n') }));

  return {
    title,
    lang,
    clock: FIXED_CLOCK,
    config,
    session,
    workspace: { root: ROOT, notes },
    scraps,
    cliHistory: ['sort -u', 'jq .'],
    noteFiles: tabs.map((t) => ({ path: t.path, title: t.title, content: t.content })),
    version: '2.0.0-lite',
    query,
  };
}
