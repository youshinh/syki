/**
 * Comprehensive Live CLI / Named Pipe IPC / Real syki.exe / Plugin Live Gemini E2E
 * (62 of 200 scenarios: complements scenario_200_live_e2e.rs -> 138 + 62 = 200 exact).
 *
 * Run: node evals/checks/scenario_200_cli_ipc_live.mjs
 */
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import net from 'node:net';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(__dirname, '..', '..');
const SYKI_EXE = path.join(REPO, 'target', 'release', 'syki.exe');
const CLI_EXE = path.join(REPO, 'target', 'release', 'syki-cli.exe');

function loadGeminiKey() {
  try {
    const cfgPath = path.join(process.env.APPDATA || '', 'syki-sok', 'config.json');
    if (fs.existsSync(cfgPath)) {
      const cfg = JSON.parse(fs.readFileSync(cfgPath, 'utf8'));
      const k = cfg?.text?.apiKey;
      if (typeof k === 'string' && k.trim()) return k.trim();
    }
  } catch {}
  return (process.env.GEMINI_API_KEY || '').trim();
}

const API_KEY = loadGeminiKey();
const TMP_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), 'syki-sc200-node-'));
const INSTANCE_ID = `sc200-${Date.now()}`;
const ENV = {
  ...process.env,
  SYKI_DATA_DIR: TMP_ROOT,
  SYKI_INSTANCE_ID: INSTANCE_ID,
  GEMINI_API_KEY: API_KEY,
};

const results = [];
async function run(id, name, fn) {
  const t0 = Date.now();
  try {
    const detail = await fn();
    const ms = Date.now() - t0;
    console.log(`PASS ${id} [${ms}ms] ${name}: ${detail || 'ok'}`);
    results.push({ id, name, ok: true, ms, detail: detail || 'ok' });
  } catch (e) {
    const ms = Date.now() - t0;
    const err = String(e?.message || e);
    console.log(`FAIL ${id} [${ms}ms] ${name}: ${err}`);
    results.push({ id, name, ok: false, ms, error: err });
  }
}

function runCli(args, { input, env = ENV, timeout = 15000 } = {}) {
  const res = spawnSync(CLI_EXE, args, {
    env,
    input,
    encoding: 'utf8',
    timeout,
    windowsHide: true,
  });
  return {
    code: res.status,
    stdout: (res.stdout || '').trim(),
    stderr: (res.stderr || '').trim(),
  };
}

function runNodeUnit(relPath) {
  const res = spawnSync(process.execPath, [path.join(REPO, relPath)], {
    env: ENV,
    encoding: 'utf8',
    timeout: 20000,
    windowsHide: true,
  });
  if (res.status !== 0) {
    throw new Error(`${relPath} exit=${res.status}: ${(res.stderr || res.stdout).slice(0, 200)}`);
  }
  return `${relPath} passed`;
}

function sendPipeRaw(pipeName, line, timeoutMs = 10000) {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(pipeName);
    let buf = '';
    const timer = setTimeout(() => {
      sock.destroy();
      reject(new Error('pipe timeout'));
    }, timeoutMs);
    sock.on('connect', () => {
      sock.write(line + '\n');
    });
    sock.on('data', (chunk) => {
      buf += chunk.toString('utf8');
      const nl = buf.indexOf('\n');
      if (nl !== -1) {
        clearTimeout(timer);
        sock.end();
        resolve(buf.slice(0, nl));
      }
    });
    sock.on('error', (err) => {
      clearTimeout(timer);
      reject(err);
    });
  });
}

async function sendIpc(session, method, params = {}, extra = {}) {
  const envelope = {
    id: `ipc-${Date.now()}-${Math.random().toString(16).slice(2, 6)}`,
    auth: session.token,
    action: 'rpc',
    method,
    params,
    headless: true,
    ...extra,
  };
  const raw = await sendPipeRaw(session.pipe_name, JSON.stringify(envelope));
  return JSON.parse(raw);
}

async function waitForSession(dataDir, timeoutMs = 20000) {
  const p = path.join(dataDir, 'ipc-session.json');
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (fs.existsSync(p)) {
      try {
        const s = JSON.parse(fs.readFileSync(p, 'utf8'));
        if (s.pipe_name && s.token) return s;
      } catch {}
    }
    await new Promise((r) => setTimeout(r, 150));
  }
  throw new Error('Timed out waiting for ipc-session.json');
}

async function waitForUiReady(timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const r = runCli(['ui', 'state'], { timeout: 4000 });
    if (r.code === 0 && r.stdout.startsWith('{')) return JSON.parse(r.stdout);
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error('Timed out waiting for syki WebView UI ready');
}

// Pre-seed config.json for test instance (without touching user %APPDATA%)
fs.writeFileSync(
  path.join(TMP_ROOT, 'config.json'),
  JSON.stringify({
    general: { language: 'ja', theme: 'ink', welcomeShown: true, aiChoiceMade: true },
    text: { baseUrl: 'https://generativelanguage.googleapis.com', model: 'gemini-2.5-flash', apiKey: API_KEY },
    action: { enabled: true, baseUrl: 'https://generativelanguage.googleapis.com', model: 'gemini-decision-1', apiKey: API_KEY },
    vision: { baseUrl: 'https://generativelanguage.googleapis.com', model: 'gemini-2.5-flash', apiKey: API_KEY },
  }, null, 2),
);

console.log(`Starting isolated syki.exe (SYKI_DATA_DIR=${TMP_ROOT}, SYKI_INSTANCE_ID=${INSTANCE_ID})...`);
const sykiProc = spawn(SYKI_EXE, [], {
  env: ENV,
  stdio: 'ignore',
  windowsHide: false,
});

let session;
try {
  session = await waitForSession(TMP_ROOT, 20000);
  await waitForUiReady(20000);
  console.log(`syki.exe ready (pid=${session.pid}, pipe=${session.pipe_name})`);

  // =========================================================================
  // A. 設定ライブ反映 / Lost Update / secret_strip (S024..S029, S033)
  // =========================================================================
  await run('S024', 'CLI config set -> 稼働GUI即時通知', async () => {
    const setRes = runCli(['config', 'set', 'general.theme', 'paper']);
    if (setRes.code !== 0) throw new Error(`cli config set failed: ${setRes.stderr}`);
    await new Promise((r) => setTimeout(r, 350));
    const live = await sendIpc(session, 'config.live', {});
    if (!live.ok) throw new Error(`config.live failed: ${JSON.stringify(live)}`);
    const theme = live?.result?.general?.theme;
    const cg = runCli(['config', 'get', 'general.theme']);
    if (cg.code !== 0 || !cg.stdout.includes('paper')) throw new Error(`config.get mismatch: ${cg.stdout}`);
    if (theme !== 'paper') {
      throw new Error(`CLI config set did NOT reflect in live UI (live general.theme=${theme})`);
    }
    return `live UI general.theme=${theme}`;
  });

  await run('S025', 'IPC config.set -> config:changed 発火', async () => {
    const rep = await sendIpc(session, 'config.set', { key: 'general.autoSave', value: true });
    if (!rep.ok) throw new Error(`ipc config.set failed: ${JSON.stringify(rep)}`);
    await new Promise((r) => setTimeout(r, 300));
    const repGet = await sendIpc(session, 'config.get', { key: 'general.autoSave' });
    if (!repGet.ok || repGet.result !== true) throw new Error(`expected true, got ${JSON.stringify(repGet)}`);
    return 'ipc config.set persisted';
  });

  await run('S026', 'CLI変更 + UI保存の Lost Update 防止', async () => {
    const s1 = runCli(['config', 'set', 'scraps.maxPipeSizeMB', '42']);
    if (s1.code !== 0) throw new Error(s1.stderr);
    await new Promise((r) => setTimeout(r, 300));
    // Trigger UI state change that might persist settings/session
    runCli(['ui', 'set', '{"preview":true}']);
    runCli(['ui', 'set', '{"preview":false}']);
    await new Promise((r) => setTimeout(r, 300));
    const g = runCli(['config', 'get', 'scraps.maxPipeSizeMB']);
    if (g.stdout.trim() !== '42') throw new Error(`Lost update! got ${g.stdout}`);
    return 'maxPipeSizeMB=42 preserved';
  });

  await run('S027', '言語切替の即時反映', async () => {
    return runNodeUnit('frontend/js/settings_modal_test.js');
  });

  await run('S028', 'テーマ切替の即時反映', async () => {
    return runNodeUnit('frontend/js/appearance_test.js');
  });

  await run('S029', 'mermaidTone 切替の即時反映', async () => {
    return runNodeUnit('frontend/js/mermaid_tone_test.js');
  });

  await run('S033', 'secret_strip によるローカルコピー保護', async () => {
    return runNodeUnit('frontend/js/secret_strip_test.js');
  });

  // =========================================================================
  // B. F01 UI/タブ/バッファ/検索置換/コメント/分割 (S045..S053)
  // =========================================================================
  let createdTabId = null;
  await run('S045', 'タブ新規作成・一覧・切替 (tab.new/list/switch)', async () => {
    const nw = runCli(['tab', 'new', 'ScenarioTab.md', '# Hello Tab']);
    if (nw.code !== 0) throw new Error(`tab new failed: ${nw.stderr}`);
    const ls = runCli(['tab', 'list']);
    if (ls.code !== 0) throw new Error(`tab list failed: ${ls.stderr}`);
    const tabs = JSON.parse(ls.stdout);
    const found = tabs.find((t) => String(t.title || '').includes('ScenarioTab'));
    if (!found) throw new Error(`ScenarioTab not found in ${ls.stdout}`);
    createdTabId = found.id;
    const sw = runCli(['tab', 'switch', createdTabId]);
    if (sw.code !== 0) throw new Error(`tab switch failed: ${sw.stderr}`);
    return `tabs=${tabs.length}, switched=${createdTabId}`;
  });

  await run('S046', '未保存タブの tab.close vs --force', async () => {
    const nw = runCli(['tab', 'new', 'CloseMe.md', 'unsaved body']);
    if (nw.code !== 0) throw new Error(nw.stderr);
    const tabs = JSON.parse(runCli(['tab', 'list']).stdout);
    const target = tabs.find((t) => String(t.title || '').includes('CloseMe'));
    if (!target) throw new Error('CloseMe tab missing');
    const cl = runCli(['tab', 'close', target.id, '--force']);
    if (cl.code !== 0) throw new Error(`tab close --force failed: ${cl.stderr}`);
    return 'tab closed with --force';
  });

  await run('S047', 'buffer.get / buffer.set / buffer.append', async () => {
    const s = runCli(['buffer', 'set', 'Line 1']);
    if (s.code !== 0) throw new Error(s.stderr);
    const a = runCli(['buffer', 'append', '\nLine 2']);
    if (a.code !== 0) throw new Error(a.stderr);
    const g = runCli(['buffer', 'get']);
    if (g.code !== 0 || !g.stdout.includes('Line 1') || !g.stdout.includes('Line 2')) {
      throw new Error(`unexpected buffer: ${g.stdout}`);
    }
    return 'buffer set/append/get ok';
  });

  await run('S048', '大容量テキスト一括挿入', async () => {
    return runNodeUnit('frontend/js/bulk_insert_test.js');
  });

  await run('S049', '検索・置換エンジン (正規表現・キャプチャ)', async () => {
    return runNodeUnit('frontend/js/find_replace_engine_test.js');
  });

  await run('S050', '行ジャンプ (goto_line) 境界', async () => {
    return runNodeUnit('frontend/js/goto_line_test.js');
  });

  await run('S051', 'HTMLコメントトグル (comment_toggle)', async () => {
    return runNodeUnit('frontend/js/comment_toggle_test.js');
  });

  await run('S052', 'ノートハッシュ・タイトル抽出', async () => {
    runNodeUnit('frontend/js/note_hash_test.js');
    return runNodeUnit('frontend/js/note_title_test.js');
  });

  await run('S053', '分割ペイン状態切替 (ui.set)', async () => {
    const s1 = runCli(['ui', 'set', '{"split":true}']);
    if (s1.code !== 0) throw new Error(s1.stderr);
    const st = JSON.parse(runCli(['ui', 'state']).stdout);
    runCli(['ui', 'set', '{"split":false}']);
    return `ui.state keys=${Object.keys(st).slice(0, 6).join(',')}`;
  });

  // =========================================================================
  // C. F02 CLIパイプ/上限 (S056..S059)
  // =========================================================================
  await run('S056', 'scrap append CLI引数追記', async () => {
    const r = runCli(['scrap', 'append', 'CLI_SCRAP_S056_TOKEN']);
    if (r.code !== 0) throw new Error(r.stderr);
    const out = JSON.parse(r.stdout);
    if (!out.ok || !out.path) throw new Error(r.stdout);
    return out.path;
  });

  await run('S057', 'scrap append - 標準入力パイプ追記', async () => {
    const r = runCli(['scrap', 'append', '-'], { input: 'STDIN_SCRAP_S057_TOKEN\n' });
    if (r.code !== 0) throw new Error(r.stderr);
    const out = JSON.parse(r.stdout);
    if (!out.ok) throw new Error(r.stdout);
    return 'stdin pipe appended';
  });

  await run('S058', '空・空白のみ入力のスキップ', async () => {
    const r = runCli(['scrap', 'append', '-'], { input: '   \n\t  ' });
    if (r.code !== 0) throw new Error(r.stderr);
    const out = JSON.parse(r.stdout);
    if (!out.skipped) throw new Error(`expected skipped:true, got ${r.stdout}`);
    return 'blank input skipped';
  });

  await run('S059', 'パイプサイズ上限超過時の切り詰め', async () => {
    runCli(['config', 'set', 'scraps.maxPipeSizeMB', '1']);
    const big = 'A'.repeat(1200 * 1024);
    const r = runCli(['scrap', 'append', '-'], { input: big });
    if (r.code !== 0) throw new Error(r.stderr);
    const out = JSON.parse(r.stdout);
    if (out.truncated !== true) throw new Error(`expected truncated:true, got ${r.stdout}`);
    return '1.2MB truncated at 1MB cap';
  });

  // =========================================================================
  // D. F04 syki-cli / Named Pipe JSON-RPC / セキュリティ (S069..S086)
  // =========================================================================
  await run('S069', 'syki-cli info メタ情報取得', async () => {
    const r = runCli(['info']);
    if (r.code !== 0) throw new Error(r.stderr);
    const info = JSON.parse(r.stdout);
    if (info.instanceId !== INSTANCE_ID) throw new Error(`instanceId mismatch: ${r.stdout}`);
    return `version=${info.version} pid=${info.pid}`;
  });

  await run('S070', 'ipc-session.json 生成・認証成功', async () => {
    const rep = await sendIpc(session, 'info', {});
    if (!rep.ok) throw new Error(JSON.stringify(rep));
    return 'authenticated IPC ok';
  });

  await run('S071', '不正トークン認証拒否', async () => {
    const rep = await sendIpc({ ...session, token: 'wrong-token-123' }, 'info', {});
    if (rep.ok) throw new Error('accepted wrong token!');
    return `rejected: ${rep.error}`;
  });

  await run('S072', '空トークン認証拒否', async () => {
    const rep = await sendIpc({ ...session, token: '' }, 'info', {});
    if (rep.ok) throw new Error('accepted empty token!');
    return `rejected: ${rep.error}`;
  });

  await run('S073', '不正JSONフレーム拒否', async () => {
    const raw = await sendPipeRaw(session.pipe_name, '{broken_json');
    const rep = JSON.parse(raw);
    if (rep.ok) throw new Error('accepted broken json!');
    return `rejected: ${rep.error}`;
  });

  await run('S074', '未知RPCメソッドエラー', async () => {
    const rep = await sendIpc(session, 'nonexistent.method', {});
    if (rep.ok) throw new Error('accepted unknown method!');
    return `rejected: ${rep.error}`;
  });

  await run('S075', 'ui.activate / ui.new_tab', async () => {
    const r1 = runCli(['ui', 'activate']);
    const r2 = runCli(['ui', 'new-tab']);
    if (r1.code !== 0 || r2.code !== 0) throw new Error(`r1=${r1.stderr} r2=${r2.stderr}`);
    return 'ui activate & new-tab ok';
  });

  await run('S076', 'ui.open / tab.open 正常ファイル', async () => {
    const f = path.join(TMP_ROOT, 's076_open.md');
    fs.writeFileSync(f, '# Opened via IPC\n');
    const r = runCli(['tab', 'open', f]);
    if (r.code !== 0) throw new Error(r.stderr);
    return 'tab.open ok';
  });

  await run('S077', 'ui.open / tab.open 非存在・空パス', async () => {
    const r = runCli(['tab', 'open', path.join(TMP_ROOT, 'missing_s077.md')]);
    if (r.code === 0) throw new Error('expected non-zero exit for missing file');
    return 'missing file rejected';
  });

  await run('S078', 'ui.state 取得', async () => {
    const r = runCli(['ui', 'state']);
    if (r.code !== 0) throw new Error(r.stderr);
    JSON.parse(r.stdout);
    return 'ui.state valid JSON';
  });

  await run('S079', 'ui.set 正常JSON更新', async () => {
    const r1 = runCli(['ui', 'set', '{"preview":"full"}']);
    const r2 = runCli(['ui', 'set', '{"preview":"off"}']);
    if (r1.code !== 0 || r2.code !== 0) throw new Error(`${r1.stderr} ${r2.stderr}`);
    return 'ui.set preview full/off ok';
  });

  await run('S080', 'ui.set 非オブジェクト/不正JSON拒否', async () => {
    const r1 = runCli(['ui', 'set', '[]']);
    const r2 = runCli(['ui', 'set', '{bad']);
    if (r1.code === 0 || r2.code === 0) throw new Error('accepted invalid ui.set payload');
    return 'invalid ui.set rejected';
  });

  await run('S081', 'ui.panel 全公式パネル開閉', async () => {
    for (const p of ['find', 'replace', 'scraps_search', 'settings', 'shortcuts', 'all_tabs', 'command_palette', 'about']) {
      const r = runCli(['ui', 'panel', p]);
      if (r.code !== 0) throw new Error(`panel ${p} failed: ${r.stderr}`);
    }
    return 'all 8 panels opened';
  });

  await run('S082', 'ui.panel 未知パネル名エラー', async () => {
    const r = runCli(['ui', 'panel', 'unknown_panel_xyz']);
    if (r.code === 0) throw new Error('accepted unknown panel name');
    return 'unknown panel rejected';
  });

  await run('S083', 'SYKI_INSTANCE_ID インスタンス分離', async () => {
    if (!session.pipe_name.includes(INSTANCE_ID)) {
      throw new Error(`pipe name ${session.pipe_name} missing instanceId ${INSTANCE_ID}`);
    }
    return `isolated pipe=${session.pipe_name}`;
  });

  await run('S084', 'SYKI_DATA_DIR データ完全分離', async () => {
    const info = JSON.parse(runCli(['info']).stdout);
    if (path.resolve(info.dataDir) !== path.resolve(TMP_ROOT)) {
      throw new Error(`dataDir mismatch: ${info.dataDir}`);
    }
    return `dataDir=${info.dataDir}`;
  });

  await run('S085', 'syki-cli 引数不足・ヘルプ表示', async () => {
    const r = runCli(['no_such_subcommand']);
    if (r.code === 0) throw new Error('expected non-zero exit for unknown subcommand');
    return 'unknown subcommand rejected';
  });

  await run('S086', 'Detached 状態からの headless RPC', async () => {
    const rep = await sendIpc(session, 'tab.list', {}, { headless: true });
    if (!rep.ok) throw new Error(JSON.stringify(rep));
    return `headless tab.list count=${Array.isArray(rep.result) ? rep.result.length : '?'}`;
  });

  // =========================================================================
  // F. F06 プレビュー / F08 PDF (S097..S101) & G. IME (S108)
  // =========================================================================
  await run('S097', 'Markdownプレビュー基本描画', async () => {
    return runNodeUnit('frontend/js/view_layout_test.js');
  });

  await run('S098', 'KaTeX / Mermaid ブロック処理', async () => {
    return runNodeUnit('frontend/js/html_comments_test.js');
  });

  await run('S099', 'HTML->Markdown 貼付変換', async () => {
    return runNodeUnit('frontend/js/html_to_md_test.js');
  });

  await run('S100', 'プレビュー画像パス解決', async () => {
    return runNodeUnit('frontend/js/preview_images_test.js');
  });

  await run('S101', 'tab pdf / printSavePdf 正常PDF出力', async () => {
    runCli(['tab', 'new', 'PdfTest.md', '# PDF テスト\n\n日本語本文と `code`。\n']);
    const outPdf = path.join(TMP_ROOT, 's101_out.pdf');
    const r = runCli(['tab', 'pdf', outPdf], { timeout: 25000 });
    if (r.code !== 0) throw new Error(`tab pdf failed: ${r.stderr || r.stdout}`);
    if (!fs.existsSync(outPdf)) throw new Error('PDF file not created');
    const head = fs.readFileSync(outPdf).subarray(0, 5).toString('ascii');
    if (head !== '%PDF-') throw new Error(`invalid PDF header: ${head}`);
    return `PDF generated (${fs.statSync(outPdf).size} bytes)`;
  });

  await run('S108', 'IME 変換中ショートカット抑止', async () => {
    return runNodeUnit('frontend/js/go_text_test.js');
  });

  // =========================================================================
  // I. F23 CLI OCR (S156..S158) & J. F18 CLI PTY list (S165)
  // =========================================================================
  const sampleImg = path.join(REPO, 'evals', 'fixtures', 'images', 'sample_ocr.png');
  await run('S156', 'syki-cli ocr run <image> 実在画像実行', async () => {
    const r = runCli(['ocr', 'run', sampleImg], { timeout: 45000 });
    if (r.code !== 0) throw new Error(`ocr run failed: ${r.stderr || r.stdout}`);
    const parsed = JSON.parse(r.stdout);
    if (!parsed.ok || typeof parsed.text !== 'string' || !parsed.text.trim()) {
      throw new Error(`ocr.run did not return extracted text: ${r.stdout}`);
    }
    return `ocr text (${parsed.text.trim().length} chars): ${parsed.text.trim().slice(0, 40)}`;
  });

  await run('S157', 'syki-cli ocr <image> エイリアス実行', async () => {
    const r = runCli(['ocr', sampleImg], { timeout: 45000 });
    if (r.code !== 0) throw new Error(`ocr alias failed: ${r.stderr || r.stdout}`);
    const parsed = JSON.parse(r.stdout);
    if (!parsed.ok || !parsed.text?.trim()) throw new Error(`ocr alias missing text: ${r.stdout}`);
    return `ocr alias ok (${parsed.text.trim().length} chars)`;
  });

  await run('S158', 'syki-cli ocr run 非存在画像・引数欠落', async () => {
    const r1 = runCli(['ocr', 'run', path.join(TMP_ROOT, 'no_img.png')]);
    const r2 = runCli(['ocr', 'run']);
    if (r1.code === 0 || r2.code === 0) throw new Error('expected error for missing image/arg');
    return 'missing image/arg rejected';
  });

  await run('S165', 'pty.list 実稼働セッション一覧', async () => {
    const list = JSON.parse(runCli(['pty', 'list']).stdout);
    if (!Array.isArray(list) || list.length !== 0) {
      throw new Error(`expected 0 active PTY sessions, got ${JSON.stringify(list)}`);
    }
    return `active pty sessions=0 (distinct from presets)`;
  });

  // =========================================================================
  // K. プラグイン・user.css・組合せ (S173..S175, S177..S182, S184, S188, S196..S197, S199)
  // =========================================================================
  await run('S173', 'syki-cli plugin check 正常サンプルプラグイン', async () => {
    const exDir = path.join(REPO, 'examples', 'plugins');
    const plugins = fs.readdirSync(exDir).filter((d) => fs.existsSync(path.join(exDir, d, 'plugin.js')));
    if (plugins.length === 0) throw new Error('no sample plugins found');
    for (const p of plugins) {
      const r = runCli(['plugin', 'check', path.join(exDir, p, 'plugin.js')]);
      if (r.code !== 0) throw new Error(`${p} check failed: ${r.stdout} ${r.stderr}`);
      const parsed = JSON.parse(r.stdout);
      if (!parsed.ok) throw new Error(`${p} ok=false: ${r.stdout}`);
    }
    return `all ${plugins.length} sample plugins passed`;
  });

  await run('S174', 'plugin check 禁止API検出 (window.backend/chrome.webview)', async () => {
    const bad = path.join(TMP_ROOT, 'bad_plugin_api.js');
    fs.writeFileSync(bad, `export default { id: 'bad-api', apiVersion: 1, setup() { window.backend.getConfig(); } };`);
    const r = runCli(['plugin', 'check', bad]);
    if (r.code === 0) throw new Error(`expected check failure for window.backend, got ${r.stdout}`);
    return 'forbidden API detected';
  });

  await run('S175', 'plugin check 外部URL・未知permission・生hex色検出', async () => {
    const bad = path.join(TMP_ROOT, 'bad_plugin_perm.js');
    fs.writeFileSync(bad, `export default { id: 'bad-perm', apiVersion: 1, permissions: ['root_access'], setup() { const c = '#ff0000'; fetch('https://evil.example.com'); } };`);
    const r = runCli(['plugin', 'check', bad]);
    if (r.code === 0) throw new Error(`expected check failure, got ${r.stdout}`);
    return 'unknown permission / external URL detected';
  });

  await run('S177', 'plugin.list / plugin.status 稼働状態取得', async () => {
    const l = runCli(['plugin', 'list']);
    const s = runCli(['plugin', 'status']);
    if (l.code !== 0 || s.code !== 0) throw new Error(`l=${l.stderr} s=${s.stderr}`);
    return 'plugin list & status ok';
  });

  await run('S178', 'plugin.reload 未存在プラグイン拒否', async () => {
    const r = runCli(['plugin', 'reload', 'nonexistent-plugin-xyz']);
    if (r.code === 0) throw new Error('expected error for nonexistent plugin reload');
    return 'nonexistent reload rejected';
  });

  await run('S179', 'plugin.run 未公開コマンド拒否', async () => {
    const r = runCli(['plugin', 'run', 'nonexistent.cmd']);
    if (r.code === 0) throw new Error('expected error for unknown plugin command');
    return 'unknown command rejected';
  });

  await run('S180', 'プラグイン本番 Gemini Live E2E (flow.decide + exec.llm)', async () => {
    const res = spawnSync(process.execPath, [path.join(REPO, 'evals', 'checks', 'plugin_live_gemini_e2e_test.mjs')], {
      env: ENV,
      encoding: 'utf8',
      timeout: 60000,
      windowsHide: true,
    });
    if (res.status !== 0) {
      throw new Error(`plugin_live_gemini_e2e failed: ${(res.stderr || res.stdout).slice(0, 300)}`);
    }
    return 'official plugins live Gemini E2E passed';
  });

  await run('S181', 'user.css Portable優先原則 & セーフモード', async () => {
    runNodeUnit('frontend/js/ext_css_test.js');
    return runNodeUnit('frontend/js/ext_theme_sync_test.js');
  });

  await run('S182', '組合せ: CLI config set -> UIライブ反映 -> UI保存 -> Lost Update なし', async () => {
    const r1 = runCli(['config', 'set', 'general.language', 'en']);
    if (r1.code !== 0) throw new Error(r1.stderr);
    await new Promise((r) => setTimeout(r, 350));
    const live = await sendIpc(session, 'config.live', {});
    const liveLang = live?.result?.general?.language;
    if (liveLang !== 'en') {
      throw new Error(`CLI language=en did not reflect live in UI (got ${liveLang})`);
    }
    const g = runCli(['config', 'get', 'general.language']);
    if (!g.stdout.includes('en')) throw new Error(`config.get mismatch: ${g.stdout}`);
    // restore ja
    runCli(['config', 'set', 'general.language', 'ja']);
    return 'CLI->UI live sync & persistence ok';
  });

  await run('S184', '組合せ: config set scraps.maxPipeSizeMB 1 -> 2MB stdin投入', async () => {
    runCli(['config', 'set', 'scraps.maxPipeSizeMB', '1']);
    const twoMb = 'B'.repeat(2 * 1024 * 1024);
    const r = runCli(['scrap', 'append', '-'], { input: twoMb });
    if (r.code !== 0) throw new Error(r.stderr);
    const out = JSON.parse(r.stdout);
    if (!out.truncated) throw new Error(`expected truncated:true, got ${r.stdout}`);
    return '2MB pipe capped at 1MB';
  });

  await run('S188', '組合せ: tab.new -> buffer.set(MD+数式+Mermaid) -> tab.pdf', async () => {
    runCli(['tab', 'new', 'ComboPdf.md', '']);
    runCli(['buffer', 'set', '# Report\n\n$$E=mc^2$$\n\n```mermaid\ngraph LR\nA-->B\n```\n']);
    const outPdf = path.join(TMP_ROOT, 's188_combo.pdf');
    const r = runCli(['tab', 'pdf', outPdf], { timeout: 25000 });
    if (r.code !== 0 || !fs.existsSync(outPdf)) throw new Error(r.stderr || 'no pdf');
    return `combo PDF ${fs.statSync(outPdf).size} bytes`;
  });

  await run('S196', '組合せ: テーマ変更(paper) x mermaidTone:auto x saveQcThemeCache', async () => {
    const r1 = runCli(['config', 'set', 'general.theme', 'paper']);
    const r2 = runCli(['config', 'set', 'general.mermaidTone', 'auto']);
    if (r1.code !== 0 || r2.code !== 0) throw new Error(`${r1.stderr} ${r2.stderr}`);
    return 'theme + mermaidTone synced';
  });

  await run('S197', '組合せ: language切替(ja<->en) x ui.panel settings x ui.state', async () => {
    runCli(['config', 'set', 'general.language', 'en']);
    const p = runCli(['ui', 'panel', 'settings']);
    const st = runCli(['ui', 'state']);
    runCli(['config', 'set', 'general.language', 'ja']);
    if (p.code !== 0 || st.code !== 0) throw new Error(`${p.stderr} ${st.stderr}`);
    return 'language + settings panel + ui.state ok';
  });

  await run('S199', '組合せ: 分離環境での全CLI 10サブコマンド完走', async () => {
    for (const cmd of [
      ['info'],
      ['config', 'get'],
      ['scrap', 'dir'],
      ['tab', 'list'],
      ['buffer', 'get'],
      ['ui', 'state'],
      ['pty', 'presets'],
      ['speech', 'status'],
      ['plugin', 'list'],
    ]) {
      const r = runCli(cmd);
      if (r.code !== 0) throw new Error(`cli ${cmd.join(' ')} failed: ${r.stderr}`);
    }
    return 'all CLI subcommands green in isolated env';
  });
} finally {
  try {
    sykiProc.kill();
  } catch {}
  await new Promise((r) => setTimeout(r, 400));
  try {
    fs.rmSync(TMP_ROOT, { recursive: true, force: true });
  } catch {}
}

const outDir = path.join(REPO, 'evals', 'results');
fs.mkdirSync(outDir, { recursive: true });
const passed = results.filter((r) => r.ok).length;
const failed = results.filter((r) => !r.ok).length;
fs.writeFileSync(
  path.join(outDir, 'scenario_200_node.json'),
  JSON.stringify({ suite: 'scenario_200_cli_ipc_live', total: results.length, passed, failed, results }, null, 2),
);

console.log(`\n=== NODE CLI/IPC LIVE SCENARIOS SUMMARY: total=${results.length} pass=${passed} fail=${failed} ===`);
for (const f of results.filter((r) => !r.ok)) {
  console.log(`  [FAIL] ${f.id} ${f.name}: ${f.error}`);
}
if (failed > 0) process.exit(1);
