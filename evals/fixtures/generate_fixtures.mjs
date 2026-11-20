// Deterministic generator for v1 compatibility acceptance fixtures under evals/fixtures/.
// Run from repo root: node evals/fixtures/generate_fixtures.mjs
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const SCRAPS_DIR = path.join(HERE, 'scraps');
const SEMINDEX_DIR = path.join(HERE, 'semindex');
const AUDIO_DIR = path.join(HERE, 'audio');
const IMAGES_DIR = path.join(HERE, 'images');

for (const dir of [SCRAPS_DIR, SEMINDEX_DIR, AUDIO_DIR, IMAGES_DIR]) {
  fs.mkdirSync(dir, { recursive: true });
}

function sha256Prefix16(text) {
  return crypto.createHash('sha256').update(text, 'utf8').digest('hex').slice(0, 32);
}

function writeUtf8(filePath, content) {
  fs.writeFileSync(filePath, content, 'utf8');
}

// 1. Legacy Go v1 config.json (both kept keys and pruned legacy keys)
const configV1 = {
  general: {
    language: 'ja',
    theme: 'olive',
    autoSave: true,
    pasteImageOcr: true,
    restoreSession: true,
    trayResident: true,
    splitViewOnStartup: false,
    imeGuardian: true,
    aiCorrection: true,
    scrapDir: '~/Documents/syki-sok/scraps',
  },
  text: {
    baseUrl: 'http://localhost:8080/v1',
    model: 'qwen3.8-flash-next',
    apiKey: '',
    systemPrompt: 'You are a concise Markdown assistant.',
  },
  autocomplete: {
    enabled: true,
    baseUrl: 'http://localhost:8080/v1',
    model: 'qwen3.8-flash-next',
    apiKey: '',
    delayMs: 450,
    maxTokens: 32,
  },
  vision: {
    baseUrl: 'https://generativelanguage.googleapis.com',
    model: 'gemini-flash-lite-latest',
    apiKey: 'FIXTURE-DUMMY-KEY-NOT-SECRET',
    prompt: 'Transcribe all visible text, tables, and diagrams accurately into Markdown.',
  },
  image: {
    baseUrl: 'https://generativelanguage.googleapis.com',
    model: 'gemini-3.1-flash-lite-image',
    apiKey: 'FIXTURE-DUMMY-KEY-NOT-SECRET',
    aspectRatio: '16:9',
    resolution: '1024',
  },
  semantic: {
    enabled: true,
    model: {
      baseUrl: 'http://localhost:11434',
      model: 'bge-m3',
    },
  },
  speech: {
    provider: 'whisper-local',
    model: 'haas-ja-v1.0-q8_0',
    language: 'ja',
  },
  voice: {
    model: 'haas-ja-v1.0-q8_0',
    apiStyle: 'auto',
    languageCodes: ['ja-JP'],
    mode: 'smart',
    silence_timeout_sec: 5,
    baseUrl: 'http://127.0.0.1:8178/v1',
    apiKey: '',
  },
  cli: {
    openResultInNewTab: true,
    openErrorInNewTab: true,
  },
  webView: {
    idleTrimMs: 30000,
    idleDetachMs: 300000,
  },
  shortcuts: {
    previewToSide: 'Ctrl+Alt+V',
    inlineRewrite: 'Ctrl+K',
  },
  // Pruned legacy keys from Go v1 (MUST parse without error and not break kept behavior)
  ocr: { onDevice: true },
  ollama: { autoStart: true, managedModel: 'gemma4:e2b' },
  gitSync: { enabled: true, debounceSeconds: 30, branch: 'main', remoteUrl: 'https://example.invalid/repo.git' },
  cfBridge: { enabled: false, tunnelUrl: '' },
  inbox: { enabled: true, folder: 'C:\\Users\\demo\\Inbox' },
  lessons: { autoPropose: true },
  mobileDrop: { idleTimeoutSeconds: 60 },
  slotAgent: { default_agent: 'claude-code' },
  stickyNotes: { enabled: true },
  scraps: {
    scrapDir: '~/Documents/syki-sok/scraps',
    gitSyncEnabled: true,
    gitSyncDebounceSeconds: 30,
    gitRemoteBranch: 'main',
    gitRemoteUrl: 'https://example.invalid/scraps.git',
    maxPipeSizeMB: 10,
  },
};
writeUtf8(path.join(HERE, 'config_v1_legacy.json'), JSON.stringify(configV1, null, 2) + '\n');

// 2. Scraps UTF-8 notes
const scrap17 = [
  '# 2026-09-17',
  '',
  '## 10:12 API 設計メモ #work #idea',
  'Batch upload endpoint design and Rust zero-copy search engine notes.',
  'Bamboo grows fast and tolerates coastal wind.',
  '',
  '<!-- syki:res id=res_fixture_1 -->',
  'AI summary: Keep batch size capped at 10 MiB and use atomic temp-rename writes.',
  '<!-- /syki:res -->',
  '',
  '## 15:40 リリース準備 #work',
  'Verify CP932 legacy notes and semindex vectors.bin compatibility.',
  '',
].join('\n');

const scrap15 = [
  '# 2026-09-15',
  '',
  '## 09:30 セキュリティ監査メモ #urgent #work',
  'IPC session token uses constant-time comparison and 0600 / user-only ACL.',
  'Never leak session tokens inside JSON-RPC error strings.',
  '',
].join('\n');

const scrap17Path = path.join(SCRAPS_DIR, '2026-09-17.md');
const scrap15Path = path.join(SCRAPS_DIR, '2026-09-15.md');
writeUtf8(scrap17Path, scrap17);
writeUtf8(scrap15Path, scrap15);

// 3. CP932 / Shift_JIS Markdown fixture (exact bytes for "# Shift_JIS テスト\r\n日本語メモの読み込み互換性確認。\r\n")
const shiftJisBytes = Buffer.from([
  35, 32, 83, 104, 105, 102, 116, 95, 74, 73, 83, 32, 131, 101, 131, 88, 131, 103, 13, 10,
  147, 250, 150, 123, 140, 234, 131, 129, 131, 130, 130, 204, 147, 199, 130, 221, 141, 158,
  130, 221, 140, 221, 138, 183, 144, 171, 138, 109, 148, 70, 129, 66, 13, 10,
]);
fs.writeFileSync(path.join(SCRAPS_DIR, 'shift_jis_note.md'), shiftJisBytes);

// 4. semindex Go v1 binary-compatible fixtures (pkg/semindex/store.go + chunk.go)
const chunk0Text = 'Batch upload endpoint design and Rust zero-copy search engine notes.';
const chunk1Text = 'Verify CP932 legacy notes and semindex vectors.bin compatibility.';
const chunk2Text = 'IPC session token uses constant-time comparison and never leaks tokens.';

const chunkRows = [
  {
    rel: '2026-09-17.md',
    entry: 0,
    line: 4,
    end: 5,
    date: '2026-09-17',
    heading: '10:12 API 設計メモ #work #idea',
    kind: 'note',
    text: chunk0Text,
    hash: sha256Prefix16(chunk0Text),
    v: 0,
  },
  {
    rel: '2026-09-17.md',
    entry: 1,
    line: 12,
    end: 12,
    date: '2026-09-17',
    heading: '15:40 リリース準備 #work',
    kind: 'note',
    text: chunk1Text,
    hash: sha256Prefix16(chunk1Text),
    v: 1,
  },
  {
    rel: '2026-09-15.md',
    entry: 0,
    line: 4,
    end: 5,
    date: '2026-09-15',
    heading: '09:30 セキュリティ監査メモ #urgent #work',
    kind: 'note',
    text: chunk2Text,
    hash: sha256Prefix16(chunk2Text),
    v: 2,
  },
];

const chunksJsonl = chunkRows.map((c) => JSON.stringify(c)).join('\n') + '\n';
const chunksPath = path.join(SEMINDEX_DIR, 'chunks.jsonl');
writeUtf8(chunksPath, chunksJsonl);
const chunksBytes = fs.statSync(chunksPath).size;

const dim = 4;
const vecs = [
  [1.0, 0.0, 0.0, 0.0],
  [0.0, 1.0, 0.0, 0.0],
  [Math.SQRT1_2, Math.SQRT1_2, 0.0, 0.0],
];
const vecBuf = Buffer.alloc(16 + vecs.length * dim * 4);
vecBuf.write('MDSV', 0, 4, 'ascii');
vecBuf.writeUInt32LE(1, 4);      // storeVersion = 1
vecBuf.writeUInt32LE(dim, 8);    // dim = 4
vecBuf.writeUInt32LE(0, 12);     // reserved = 0
for (let r = 0; r < vecs.length; r++) {
  for (let c = 0; c < dim; c++) {
    vecBuf.writeFloatLE(vecs[r][c], 16 + (r * dim + c) * 4);
  }
}
fs.writeFileSync(path.join(SEMINDEX_DIR, 'vectors.bin'), vecBuf);

const manifest = {
  version: 1,
  embedder: 'fixture-4d',
  dim: 4,
  chunker_version: 4,
  max_chars: 150,
  header: false,
  include_ai: false,
  metas: chunkRows.length,
  metas_bytes: chunksBytes,
  vecs: vecs.length,
  files: {
    '2026-09-17.md': {
      size: Buffer.byteLength(scrap17, 'utf8'),
      mod_ns: 1789632000000000000,
      metas: [0, 1],
    },
    '2026-09-15.md': {
      size: Buffer.byteLength(scrap15, 'utf8'),
      mod_ns: 1789459200000000000,
      metas: [2],
    },
  },
  updated: '2026-09-18T10:24:00Z',
};
writeUtf8(path.join(SEMINDEX_DIR, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');

// 5. ipc-auth golden vectors (pkg/ipc/auth.go + rpc_types.go)
const ipcVectors = {
  session_example: {
    pid: 4242,
    port: 49152,
    token: 'fixture-secret-token-0123456789abcdef',
    started_at: '2026-09-18T10:24:00Z',
  },
  error_codes: {
    unauthorized: -32000,
    conflict: -32001,
    not_found: -32002,
    no_selection: -32003,
  },
  read_only_methods: ['buffer.get', 'buffer.get_selection', 'tab.list'],
  protected_methods: ['buffer.set', 'tab.open', 'ui.set', 'pty.exec', 'ocr.run', 'speech.transcribe'],
  cases: [
    { name: 'read_no_auth_allowed', method: 'buffer.get', auth: '', expect_ok: true },
    { name: 'read_sel_no_auth_allowed', method: 'buffer.get_selection', auth: '', expect_ok: true },
    { name: 'tab_list_no_auth_allowed', method: 'tab.list', auth: '', expect_ok: true },
    { name: 'read_wrong_auth_denied', method: 'buffer.get', auth: 'wrong-token', expect_ok: false, expect_code: -32000, must_not_leak: true },
    { name: 'write_no_auth_denied', method: 'buffer.set', auth: '', expect_ok: false, expect_code: -32000, must_not_leak: true },
    { name: 'write_wrong_auth_denied', method: 'buffer.set', auth: 'wrong-token', expect_ok: false, expect_code: -32000, must_not_leak: true },
    { name: 'write_valid_auth_allowed', method: 'buffer.set', auth: 'fixture-secret-token-0123456789abcdef', expect_ok: true },
    { name: 'ui_set_valid_auth_allowed', method: 'ui.set', auth: 'fixture-secret-token-0123456789abcdef', expect_ok: true },
    { name: 'pty_exec_no_auth_denied', method: 'pty.exec', auth: '', expect_ok: false, expect_code: -32000, must_not_leak: true },
    { name: 'ocr_run_no_auth_denied', method: 'ocr.run', auth: '', expect_ok: false, expect_code: -32000, must_not_leak: true },
    {
      name: 'legacy_pipe_no_auth_allowed',
      legacy_message: {
        action: 'pipe',
        content: 'echo hello',
        command: 'echo',
        cwd: 'C:\\Users\\demo',
        timestamp: '2026-09-18T10:24:00Z',
      },
      expect_ok: true,
    },
  ],
};
writeUtf8(path.join(HERE, 'ipc_auth_vectors.json'), JSON.stringify(ipcVectors, null, 2) + '\n');

// 6. Audio fixture: 44.1 kHz 16-bit PCM stereo 0.5s 440 Hz sine wave WAV
const sampleRate = 44100;
const channels = 2;
const bitsPerSample = 16;
const frames = Math.floor(sampleRate * 0.5); // 22050 frames
const blockAlign = channels * (bitsPerSample / 8);
const dataBytes = frames * blockAlign;
const wav = Buffer.alloc(44 + dataBytes);
wav.write('RIFF', 0, 4, 'ascii');
wav.writeUInt32LE(36 + dataBytes, 4);
wav.write('WAVE', 8, 4, 'ascii');
wav.write('fmt ', 12, 4, 'ascii');
wav.writeUInt32LE(16, 16); // PCM fmt chunk size
wav.writeUInt16LE(1, 20);  // PCM format = 1
wav.writeUInt16LE(channels, 22);
wav.writeUInt32LE(sampleRate, 24);
wav.writeUInt32LE(sampleRate * blockAlign, 28);
wav.writeUInt16LE(blockAlign, 32);
wav.writeUInt16LE(bitsPerSample, 34);
wav.write('data', 36, 4, 'ascii');
wav.writeUInt32LE(dataBytes, 40);
const freq = 440;
for (let i = 0; i < frames; i++) {
  const s = Math.round(Math.sin((2 * Math.PI * freq * i) / sampleRate) * 16000);
  const off = 44 + i * 4;
  wav.writeInt16LE(s, off);
  wav.writeInt16LE(s, off + 2);
}
fs.writeFileSync(path.join(AUDIO_DIR, 'tone_44100_stereo.wav'), wav);

// 7. Image fixture: 16x16 valid PNG
const TINY_PNG_B64 = 'iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAJUlEQVR4nGMIzdaHow+f38MRLnGGQaiBGEXI4oNRwyAM1pEYDwAFsOCQg89cjwAAAABJRU5ErkJggg==';
fs.writeFileSync(path.join(IMAGES_DIR, 'sample_ocr.png'), Buffer.from(TINY_PNG_B64, 'base64'));

console.log('Generated compatibility acceptance fixtures in evals/fixtures/');
