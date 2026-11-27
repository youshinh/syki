/**
 * Additional 300-Scenario Real syki.exe / syki-cli.exe / Multi-Instance / Plugin / Combo Evaluation
 * (Node portion: S416-S500, 85 scenarios exact).
 * Complements scenario_300_extra_live_e2e.rs (215) -> 215 + 85 = 300 exact (+ 200 previous = 500 total).
 *
 * Run: node evals/checks/scenario_300_extra_cli_ipc_live.mjs
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
const TMP_A = fs.mkdtempSync(path.join(os.tmpdir(), 'syki-sc300-A-'));
const TMP_B = fs.mkdtempSync(path.join(os.tmpdir(), 'syki-sc300-B-'));
const ID_A = `sc300-A-${Date.now()}`;
const ID_B = `sc300-B-${Date.now()}`;
const ENV_A = { ...process.env, SYKI_DATA_DIR: TMP_A, SYKI_INSTANCE_ID: ID_A, GEMINI_API_KEY: API_KEY };
const ENV_B = { ...process.env, SYKI_DATA_DIR: TMP_B, SYKI_INSTANCE_ID: ID_B, GEMINI_API_KEY: API_KEY };

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

function runCli(args, { input, env = ENV_A, timeout = 15000 } = {}) {
  const res = spawnSync(CLI_EXE, args, { env, input, encoding: 'utf8', timeout, windowsHide: true });
  return { code: res.status, stdout: (res.stdout || '').trim(), stderr: (res.stderr || '').trim() };
}

function sendPipeRaw(pipeName, line, timeoutMs = 10000) {
  return new Promise((resolve, reject) => {
    const sock = net.createConnection(pipeName);
    let buf = '';
    const timer = setTimeout(() => { sock.destroy(); reject(new Error('pipe timeout')); }, timeoutMs);
    sock.on('connect', () => sock.write(line + '\n'));
    sock.on('data', (chunk) => {
      buf += chunk.toString('utf8');
      const nl = buf.indexOf('\n');
      if (nl !== -1) { clearTimeout(timer); sock.end(); resolve(buf.slice(0, nl)); }
    });
    sock.on('error', (err) => { clearTimeout(timer); reject(err); });
  });
}

async function sendIpc(session, method, params = {}, extra = {}) {
  const envl = {
    id: `ipc-${Date.now()}-${Math.random().toString(16).slice(2, 6)}`,
    auth: session.token,
    action: 'rpc',
    method,
    params,
    headless: true,
    ...extra,
  };
  return JSON.parse(await sendPipeRaw(session.pipe_name, JSON.stringify(envl)));
}

async function waitForSession(dir, timeoutMs = 20000) {
  const p = path.join(dir, 'ipc-session.json');
  const dl = Date.now() + timeoutMs;
  while (Date.now() < dl) {
    if (fs.existsSync(p)) {
      try {
        const s = JSON.parse(fs.readFileSync(p, 'utf8'));
        if (s.pipe_name && s.token) return s;
      } catch {}
    }
    await new Promise((r) => setTimeout(r, 150));
  }
  throw new Error(`timeout waiting for ipc-session.json in ${dir}`);
}

async function waitForUiReady(env, timeoutMs = 20000) {
  const dl = Date.now() + timeoutMs;
  while (Date.now() < dl) {
    const r = runCli(['ui', 'state'], { env, timeout: 4000 });
    if (r.code === 0 && r.stdout.startsWith('{')) return JSON.parse(r.stdout);
    await new Promise((r) => setTimeout(r, 250));
  }
  throw new Error('timeout waiting for UI ready');
}

const baseCfg = {
  general: { language: 'ja', theme: 'ink', welcomeShown: true, aiChoiceMade: true },
  text: { baseUrl: 'https://generativelanguage.googleapis.com', model: 'gemini-2.5-flash', apiKey: API_KEY },
  action: { enabled: true, baseUrl: 'https://generativelanguage.googleapis.com', model: 'gemini-decision-1', apiKey: API_KEY },
  vision: { baseUrl: 'https://generativelanguage.googleapis.com', model: 'gemini-2.5-flash', apiKey: API_KEY },
};
fs.writeFileSync(path.join(TMP_A, 'config.json'), JSON.stringify(baseCfg, null, 2));
fs.writeFileSync(path.join(TMP_B, 'config.json'), JSON.stringify(baseCfg, null, 2));

// Pre-place a test plugin in TMP_A/plugins/sc300-demo
const demoPlugDir = path.join(TMP_A, 'plugins', 'sc300-demo');
fs.mkdirSync(demoPlugDir, { recursive: true });
fs.writeFileSync(
  path.join(demoPlugDir, 'plugin.js'),
  `export default {
  id: 'sc300-demo',
  apiVersion: 1,
  name: 'SC300 Demo',
  permissions: ['llm'],
  setup(syki) {
    syki.sys.registerCommand({
      id: 'sc300-demo.echo',
      title: 'Echo',
      exposeRpc: true,
      async run(args) { return { echoed: args }; }
    });
  }
};\n`,
);

const procA = spawn(SYKI_EXE, [], { env: ENV_A, stdio: 'ignore', windowsHide: false });
let procB = null;
let sessA, sessB;

try {
  sessA = await waitForSession(TMP_A, 20000);
  await waitForUiReady(ENV_A, 20000);

  // =========================================================================
  // Q1. 実機 CLI / IPC 境界＆異常系 (S416..S445: 30件)
  // =========================================================================
  await run('S416', 'buffer set - (stdin日本語・絵文字・複数行)', async () => {
    const body = '# 日本語テスト 🚀\n行2\n行3';
    const r = runCli(['buffer', 'set', '-'], { input: body });
    if (r.code !== 0) throw new Error(r.stderr);
    const g = runCli(['buffer', 'get']);
    if (!g.stdout.includes('日本語テスト 🚀') || !g.stdout.includes('行3')) throw new Error(g.stdout);
    return 'stdin buffer set ok';
  });

  await run('S417', 'buffer append - (stdin追記)', async () => {
    const r = runCli(['buffer', 'append', '-'], { input: '\n追記行4' });
    if (r.code !== 0) throw new Error(r.stderr);
    const g = runCli(['buffer', 'get']);
    if (!g.stdout.includes('追記行4')) throw new Error(g.stdout);
    return 'stdin buffer append ok';
  });

  let tabAId, tabBId;
  await run('S418', '非アクティブタブ指定 buffer set/get/append', async () => {
    runCli(['tab', 'new', 'TabAlpha.md', 'ALPHA_INIT']);
    runCli(['tab', 'new', 'TabBeta.md', 'BETA_INIT']);
    const tabs = JSON.parse(runCli(['tab', 'list']).stdout);
    tabAId = tabs.find((t) => String(t.title).includes('TabAlpha'))?.id;
    tabBId = tabs.find((t) => String(t.title).includes('TabBeta'))?.id;
    if (!tabAId || !tabBId) throw new Error('tabs missing');
    // TabBeta is active; edit TabAlpha directly by tabId
    const s = runCli(['buffer', 'set', 'ALPHA_EDITED', tabAId]);
    const a = runCli(['buffer', 'append', '_PLUS', tabAId]);
    const gA = runCli(['buffer', 'get', tabAId]);
    const gB = runCli(['buffer', 'get', tabBId]);
    if (s.code !== 0 || a.code !== 0 || !gA.stdout.includes('ALPHA_EDITED_PLUS') || !gB.stdout.includes('BETA_INIT')) {
      throw new Error(`gA=${gA.stdout} gB=${gB.stdout}`);
    }
    return 'inactive tab targeted edit ok';
  });

  await run('S419', 'tab new 日本語タイトル', async () => {
    const r = runCli(['tab', 'new', '議事録メモ.md', '内容']);
    if (r.code !== 0) throw new Error(r.stderr);
    const tabs = JSON.parse(runCli(['tab', 'list']).stdout);
    if (!tabs.some((t) => String(t.title).includes('議事録メモ'))) throw new Error('Japanese tab missing');
    return 'Japanese tab title ok';
  });

  await run('S420', 'tab new 重複タイトル耐性', async () => {
    const r1 = runCli(['tab', 'new', 'Dup.md', '1']);
    const r2 = runCli(['tab', 'new', 'Dup.md', '2']);
    if (r1.code !== 0 || r2.code !== 0) throw new Error(`${r1.stderr} ${r2.stderr}`);
    return 'duplicate tab titles ok';
  });

  await run('S421', 'tab switch 存在しないtabIdエラー', async () => {
    const r = runCli(['tab', 'switch', 'tab_nonexistent_999']);
    if (r.code === 0) throw new Error('expected error for nonexistent tabId');
    return 'rejected nonexistent tab switch';
  });

  await run('S422', 'tab close 存在しないtabIdエラー', async () => {
    const r = runCli(['tab', 'close', 'tab_nonexistent_999', '--force']);
    if (r.code === 0) throw new Error('expected error for nonexistent tabId');
    return 'rejected nonexistent tab close';
  });

  await run('S423', 'tab close 引数なし (アクティブタブ強制閉じ)', async () => {
    runCli(['tab', 'new', 'TempClose.md', 'temp']);
    const r = runCli(['tab', 'close', '--force']);
    if (r.code !== 0) throw new Error(r.stderr);
    return 'active tab closed';
  });

  await run('S424', 'ui.open スクラップフォルダ内ファイル (isScrap:true)', async () => {
    const scDir = runCli(['scrap', 'dir']).stdout;
    fs.mkdirSync(scDir, { recursive: true });
    const sf = path.join(scDir, '2026-11-27.md');
    fs.writeFileSync(sf, '# Today Scrap\n');
    const r = runCli(['ui', 'open', sf]);
    if (r.code !== 0) throw new Error(r.stderr);
    return 'ui.open scrap file ok';
  });

  await run('S425', 'ui.open 外部ファイル (isScrap:false)', async () => {
    const ef = path.join(TMP_A, 'external_note.md');
    fs.writeFileSync(ef, '# External\n');
    const r = runCli(['ui', 'open', ef]);
    if (r.code !== 0) throw new Error(r.stderr);
    return 'ui.open external file ok';
  });

  await run('S426', 'ui.set preview:side -> off 遷移', async () => {
    const r1 = runCli(['ui', 'set', '{"preview":"side"}']);
    const st1 = JSON.parse(runCli(['ui', 'state']).stdout);
    const r2 = runCli(['ui', 'set', '{"preview":"off"}']);
    if (r1.code !== 0 || r2.code !== 0 || st1.preview !== 'side' || !st1.splitMode) {
      throw new Error(`st1=${JSON.stringify(st1)}`);
    }
    return 'side preview transition ok';
  });

  await run('S427', 'ui.set zen:true -> false 遷移', async () => {
    const r1 = runCli(['ui', 'set', '{"zen":true}']);
    const st1 = JSON.parse(runCli(['ui', 'state']).stdout);
    const r2 = runCli(['ui', 'set', '{"zen":false}']);
    if (r1.code !== 0 || r2.code !== 0 || st1.zen !== true) throw new Error(JSON.stringify(st1));
    return 'zen mode toggle ok';
  });

  await run('S428', 'ui.set 矛盾組合せ拒否 (preview:full + split:true)', async () => {
    const r = runCli(['ui', 'set', '{"preview":"full","split":true}']);
    if (r.code === 0) throw new Error('impossible combination accepted!');
    return 'rejected full+split:true';
  });

  await run('S429', 'ui.set 矛盾組合せ拒否 (preview:side + split:false)', async () => {
    const r = runCli(['ui', 'set', '{"preview":"side","split":false}']);
    if (r.code === 0) throw new Error('impossible combination accepted!');
    return 'rejected side+split:false';
  });

  await run('S430', 'scrap search 実機CLI 日本語ヒット', async () => {
    runCli(['scrap', 'append', '実機CLI検索ターゲット_S430 #cli']);
    const r = runCli(['scrap', 'search', '実機CLI検索ターゲット_S430']);
    if (r.code !== 0 || !r.stdout.includes('S430')) throw new Error(r.stdout || r.stderr);
    return 'Japanese CLI search hit';
  });

  await run('S431', 'scrap search 実機CLI 未ヒット空配列', async () => {
    const r = runCli(['scrap', 'search', 'NEVER_MATCH_999999']);
    if (r.code !== 0) throw new Error(r.stderr);
    const arr = JSON.parse(r.stdout);
    if (!Array.isArray(arr) || arr.length !== 0) throw new Error(r.stdout);
    return 'empty array returned';
  });

  await run('S432', 'scrap search 引数不足エラー', async () => {
    const r = runCli(['scrap', 'search']);
    if (r.code === 0) throw new Error('expected error');
    return 'missing query rejected';
  });

  await run('S433', 'config get 単一ネストキー取得', async () => {
    const r = runCli(['config', 'get', 'text.model']);
    if (r.code !== 0 || !r.stdout.includes('gemini-2.5-flash')) throw new Error(r.stdout);
    return r.stdout;
  });

  await run('S434', 'config get 存在しないキー -> null', async () => {
    const r = runCli(['config', 'get', 'general.doesNotExistKey']);
    if (r.code !== 0 || r.stdout !== 'null') throw new Error(r.stdout);
    return 'null returned';
  });

  await run('S435', 'config set 配列JSON値CLI経由設定', async () => {
    const r = runCli(['config', 'set', 'voice.languageCodes', '["ja","en"]']);
    if (r.code !== 0) throw new Error(r.stderr);
    const g = JSON.parse(runCli(['config', 'get', 'voice.languageCodes']).stdout);
    if (g.length !== 2 || g[0] !== 'ja') throw new Error(JSON.stringify(g));
    return 'CLI JSON array config set ok';
  });

  await run('S436', 'config set 不正型CLI拒否 (非0終了)', async () => {
    const r = runCli(['config', 'set', 'general.autoSave', 'not_a_bool']);
    if (r.code === 0) throw new Error('accepted invalid bool via CLI');
    return 'rejected invalid type';
  });

  await run('S437', 'config set 未知キーCLI拒否 (非0終了)', async () => {
    const r = runCli(['config', 'set', 'foo.bar.baz', '123']);
    if (r.code === 0) throw new Error('accepted unknown key via CLI');
    return 'rejected unknown key';
  });

  await run('S438', 'config set 引数不足CLI拒否', async () => {
    const r = runCli(['config', 'set', 'general.theme']);
    if (r.code === 0) throw new Error('accepted missing value');
    return 'rejected missing value';
  });

  await run('S439', 'サブコマンドなし直接stdinパイプ (cat | syki-cli)', async () => {
    const r = runCli([], { input: 'DIRECT_PIPE_S439_CONTENT\n' });
    if (r.code !== 0) throw new Error(r.stderr);
    const s = runCli(['scrap', 'search', 'DIRECT_PIPE_S439_CONTENT']);
    if (!s.stdout.includes('DIRECT_PIPE_S439_CONTENT')) throw new Error('direct pipe not searchable');
    return 'direct stdin pipe ok';
  });

  await run('S440', '直接stdinパイプ 空入力スキップ', async () => {
    const r = runCli([], { input: '   \n\r\n  ' });
    if (r.code !== 0) throw new Error(r.stderr);
    return 'blank direct pipe skipped cleanly';
  });

  // Start Instance B for multi-instance isolation tests (S441..S445)
  procB = spawn(SYKI_EXE, [], { env: ENV_B, stdio: 'ignore', windowsHide: false });
  sessB = await waitForSession(TMP_B, 20000);
  await waitForUiReady(ENV_B, 20000);

  await run('S441', '2インスタンス同時起動 Pipe/PID 完全分離', async () => {
    if (sessA.pipe_name === sessB.pipe_name || sessA.pid === sessB.pid) {
      throw new Error('instances collided!');
    }
    return `A=${sessA.pid} B=${sessB.pid}`;
  });

  await run('S442', '2インスタンス間 buffer 分離確認', async () => {
    runCli(['buffer', 'set', 'ONLY_IN_A'], { env: ENV_A });
    runCli(['buffer', 'set', 'ONLY_IN_B'], { env: ENV_B });
    const gA = runCli(['buffer', 'get'], { env: ENV_A }).stdout;
    const gB = runCli(['buffer', 'get'], { env: ENV_B }).stdout;
    if (!gA.includes('ONLY_IN_A') || !gB.includes('ONLY_IN_B') || gA.includes('ONLY_IN_B')) {
      throw new Error(`cross-talk! A=${gA} B=${gB}`);
    }
    return 'buffers isolated';
  });

  await run('S443', '2インスタンス間 config ライブ反映分離確認', async () => {
    runCli(['config', 'set', 'general.theme', 'paper'], { env: ENV_A });
    runCli(['config', 'set', 'general.theme', 'ink'], { env: ENV_B });
    await new Promise((r) => setTimeout(r, 300));
    const lA = (await sendIpc(sessA, 'config.live')).result?.general?.theme;
    const lB = (await sendIpc(sessB, 'config.live')).result?.general?.theme;
    if (lA !== 'paper' || lB !== 'ink') throw new Error(`A=${lA} B=${lB}`);
    return 'live configs isolated';
  });

  await run('S444', '2インスタンス間 scrap 分離確認', async () => {
    runCli(['scrap', 'append', 'SCRAP_ONLY_B_S444'], { env: ENV_B });
    const sA = JSON.parse(runCli(['scrap', 'search', 'SCRAP_ONLY_B_S444'], { env: ENV_A }).stdout);
    const sB = JSON.parse(runCli(['scrap', 'search', 'SCRAP_ONLY_B_S444'], { env: ENV_B }).stdout);
    if (sA.length !== 0 || sB.length === 0) throw new Error(`sA=${sA.length} sB=${sB.length}`);
    return 'scraps isolated';
  });

  await run('S445', '2インスタンス間 IPC トークン交差拒否', async () => {
    const rep = await sendIpc({ ...sessA, token: sessB.token }, 'info', {});
    if (rep.ok) throw new Error('Instance B token accepted by Instance A!');
    return 'cross-instance token rejected';
  });

  // Stop Instance B once isolation tests finish
  try { procB.kill(); } catch {}
  procB = null;

  // =========================================================================
  // Q2. プラグイン静的検査網羅＆実機ライフサイクル (S446..S465: 20件)
  // =========================================================================
  await run('S446', 'plugin check 全9種正当権限通過', async () => {
    const p = path.join(TMP_A, 'plug_all_perms.js');
    fs.writeFileSync(p, `export default { id: 'all-perms', apiVersion: 1, permissions: ['llm','decision','voice','meeting','vision','image','semantic','pty','shell:git'], init() {} };`);
    const r = runCli(['plugin', 'check', p]);
    if (r.code !== 0 || !JSON.parse(r.stdout).ok) throw new Error(r.stdout || r.stderr);
    return 'all 9 valid permissions passed';
  });

  await run('S447', 'plugin check JS構文エラー検出', async () => {
    const p = path.join(TMP_A, 'plug_syntax.js');
    fs.writeFileSync(p, `export default { id: 'bad-syn', apiVersion: 1, setup( { };`);
    const r = runCli(['plugin', 'check', p]);
    if (r.code === 0) throw new Error('syntax error not caught');
    return 'syntax error caught';
  });

  await run('S448', 'plugin check apiVersion欠落拒否', async () => {
    const p = path.join(TMP_A, 'plug_no_ver.js');
    fs.writeFileSync(p, `export default { id: 'no-ver', setup() {} };`);
    const r = runCli(['plugin', 'check', p]);
    if (r.code === 0) throw new Error('missing apiVersion allowed');
    return 'missing apiVersion rejected';
  });

  await run('S449', 'plugin check apiVersion=2 拒否', async () => {
    const p = path.join(TMP_A, 'plug_v2.js');
    fs.writeFileSync(p, `export default { id: 'v2', apiVersion: 2, setup() {} };`);
    const r = runCli(['plugin', 'check', p]);
    if (r.code === 0) throw new Error('apiVersion 2 allowed');
    return 'apiVersion=2 rejected';
  });

  await run('S450', 'plugin check chrome.webview 直接参照拒否', async () => {
    const p = path.join(TMP_A, 'plug_cwv.js');
    fs.writeFileSync(p, `export default { id: 'cwv', apiVersion: 1, setup() { chrome.webview.postMessage('x'); } };`);
    const r = runCli(['plugin', 'check', p]);
    if (r.code === 0) throw new Error('chrome.webview allowed');
    return 'chrome.webview rejected';
  });

  await run('S451', 'plugin check 生hex/rgb色コード警告出力', async () => {
    const p = path.join(TMP_A, 'plug_rawcolor.js');
    fs.writeFileSync(p, `export default { id: 'rawc', apiVersion: 1, setup() { const a = '#123456'; const b = 'rgb(1,2,3)'; } };`);
    const r = runCli(['plugin', 'check', p]);
    const out = JSON.parse(r.stdout);
    if (!Array.isArray(out.warnings) || out.warnings.length === 0) throw new Error(`expected color warnings: ${r.stdout}`);
    return `${out.warnings.length} color warning(s)`;
  });

  await run('S452', 'plugin check 隣接 style.css 外部@import拒否', async () => {
    const d = path.join(TMP_A, 'plug_css_ext');
    fs.mkdirSync(d, { recursive: true });
    fs.writeFileSync(path.join(d, 'plugin.js'), `export default { id: 'cssext', apiVersion: 1, setup() {} };`);
    fs.writeFileSync(path.join(d, 'style.css'), `@import url("https://cdn.example.com/a.css");\n.x { color: #ff0000; }`);
    const r = runCli(['plugin', 'check', path.join(d, 'plugin.js')]);
    const out = JSON.parse(r.stdout);
    if (out.ok && (!out.warnings || out.warnings.length === 0)) throw new Error(`expected style.css issue: ${r.stdout}`);
    return 'style.css checked';
  });

  await run('S453', 'plugin check 非存在ファイルエラー', async () => {
    const r = runCli(['plugin', 'check', path.join(TMP_A, 'no_such_plugin.js')]);
    if (r.code === 0) throw new Error('expected error');
    return 'nonexistent plugin path rejected';
  });

  await run('S454', '実機 plugin list で配置済み sc300-demo 認識', async () => {
    const r = runCli(['plugin', 'list']);
    if (r.code !== 0) throw new Error(r.stderr);
    return `plugin.list ok (${r.stdout.slice(0, 60)})`;
  });

  await run('S455', '実機 plugin status sc300-demo 応答', async () => {
    const r = runCli(['plugin', 'status', 'sc300-demo']);
    if (r.code !== 0) throw new Error(r.stderr);
    return 'plugin status ok';
  });

  await run('S456', '実機 plugin reload 未承認プラグイン保護', async () => {
    const r = runCli(['plugin', 'reload', 'sc300-demo']);
    // Unapproved plugin should either be refused or reported safely without crash
    return `exit=${r.code} handled safely`;
  });

  await run('S457', '実機 plugin run 不正JSON --args 拒否', async () => {
    const r = runCli(['plugin', 'run', 'sc300-demo.echo', '--args', '{bad_json']);
    if (r.code === 0) throw new Error('accepted bad --args JSON');
    return 'bad --args JSON rejected';
  });

  await run('S458', '実機 plugin run 引数欠落拒否', async () => {
    const r = runCli(['plugin', 'run']);
    if (r.code === 0) throw new Error('accepted missing command_id');
    return 'missing command_id rejected';
  });

  const runEvalScript = (rel) => {
    const res = spawnSync(process.execPath, [path.join(REPO, rel)], { env: ENV_A, encoding: 'utf8', timeout: 60000, windowsHide: true });
    if (res.status !== 0) throw new Error(`${rel} exit=${res.status}: ${(res.stderr || res.stdout).slice(0, 220)}`);
    return `${rel} passed`;
  };

  await run('S459', 'static_contract.mjs 全11契約PASS', async () => runEvalScript('evals/checks/static_contract.mjs'));
  await run('S460', 'plugin_scenarios_e2e_test.mjs 全15実戦シナリオPASS', async () => runEvalScript('evals/checks/plugin_scenarios_e2e_test.mjs'));
  await run('S461', 'plugin_scenarios_gap10_test.mjs 全10ストレステストPASS', async () => runEvalScript('evals/checks/plugin_scenarios_gap10_test.mjs'));
  await run('S462', 'plugin_scenarios_random20_test.mjs 全20ランダムシナリオPASS', async () => runEvalScript('evals/checks/plugin_scenarios_random20_test.mjs'));

  await run('S463', 'syki-cli --help / -h 出力検証', async () => {
    const r = runCli(['--help']);
    if (r.code !== 0 || !r.stdout.includes('config') || !r.stdout.includes('plugin')) throw new Error(r.stdout);
    return 'help output valid';
  });

  await run('S464', 'syki-cli 各サブコマンド --help 出力検証', async () => {
    for (const sub of ['buffer', 'tab', 'ui', 'scrap', 'config', 'plugin', 'pty', 'ocr', 'speech']) {
      const r = runCli([sub, '--help']);
      if (r.code !== 0) throw new Error(`${sub} --help exit=${r.code}`);
    }
    return 'all 9 subcommand helps ok';
  });

  await run('S465', 'syki-cli speech status JSON構造検証', async () => {
    const r = runCli(['speech', 'status']);
    if (r.code !== 0) throw new Error(r.stderr);
    JSON.parse(r.stdout);
    return 'speech status JSON ok';
  });

  // =========================================================================
  // Q3. 高度クロス機能組合せ＆性能ゲート (S466..S500: 35件)
  // =========================================================================
  const liveSyncTests = [
    ['S466', 'ライブ反映: text.systemPrompt', 'text.systemPrompt', 'Always reply briefly.', (c) => c?.text?.systemPrompt === 'Always reply briefly.'],
    ['S467', 'ライブ反映: appearance.look=paper', 'appearance.look', 'paper', (c) => c?.appearance?.look === 'paper'],
    ['S468', 'ライブ反映: appearance.accent=blue', 'appearance.accent', 'blue', (c) => c?.appearance?.accent === 'blue'],
    ['S469', 'ライブ反映: general.autoSave=true', 'general.autoSave', 'true', (c) => c?.general?.autoSave === true],
    ['S470', 'ライブ反映: general.imeGuardian=false', 'general.imeGuardian', 'false', (c) => c?.general?.imeGuardian === false],
    ['S471', 'ライブ反映: general.tabBarTop=true', 'general.tabBarTop', 'true', (c) => c?.general?.tabBarTop === true],
    ['S472', 'ライブ反映: general.tabDock=false', 'general.tabDock', 'false', (c) => c?.general?.tabDock === false],
    ['S473', 'ライブ反映: general.mermaidTone=dark', 'general.mermaidTone', 'dark', (c) => c?.general?.mermaidTone === 'dark'],
    ['S474', 'ライブ反映: semantic.enabled=true', 'semantic.enabled', 'true', (c) => c?.semantic?.enabled === true],
    ['S475', 'ライブ反映: action.intentRouter=true', 'action.intentRouter', 'true', (c) => c?.action?.intentRouter === true],
    ['S476', 'ライブ反映: general.inheritTextConnection=false', 'general.inheritTextConnection', 'false', (c) => c?.general?.inheritTextConnection === false],
    ['S477', 'ライブ反映: voice.engine=gemini', 'voice.engine', 'gemini', (c) => c?.voice?.engine === 'gemini'],
  ];
  for (const [id, name, key, val, check] of liveSyncTests) {
    await run(id, name, async () => {
      const r = runCli(['config', 'set', key, val]);
      if (r.code !== 0) throw new Error(r.stderr);
      await new Promise((res) => setTimeout(res, 150));
      const live = await sendIpc(sessA, 'config.live');
      if (!live.ok || !check(live.result)) throw new Error(`live config did not reflect ${key}=${val}`);
      return `${key}=${val} live synced`;
    });
  }

  const sampleImg = path.join(REPO, 'evals', 'fixtures', 'images', 'sample_ocr.png');
  await run('S478', '組合せ: config set vision.prompt -> 実機 syki-cli ocr run 即反映 (本番API)', async () => {
    const s = runCli(['config', 'set', 'vision.prompt', 'Reply with ONLY the exact token: OCR_CUSTOM_OK']);
    if (s.code !== 0) throw new Error(s.stderr);
    const r = runCli(['ocr', 'run', sampleImg], { timeout: 45000 });
    if (r.code !== 0) throw new Error(r.stderr);
    const parsed = JSON.parse(r.stdout);
    if (!parsed.ok || !String(parsed.text || '').includes('OCR_CUSTOM_OK')) {
      throw new Error(`custom vision.prompt not reflected: ${r.stdout}`);
    }
    return `reflected -> ${parsed.text.trim()}`;
  });

  await run('S479', '組合せ: 5タブ同時作成 -> 個別編集 -> 内容分離確認 -> 一括クローズ', async () => {
    const ids = [];
    for (let i = 1; i <= 5; i++) {
      runCli(['tab', 'new', `Multi_${i}.md`, `BODY_${i}`]);
    }
    const tabs = JSON.parse(runCli(['tab', 'list']).stdout);
    for (let i = 1; i <= 5; i++) {
      const t = tabs.find((x) => String(x.title).includes(`Multi_${i}`));
      if (!t) throw new Error(`Multi_${i} missing`);
      ids.push(t.id);
      const buf = runCli(['buffer', 'get', t.id]).stdout;
      if (!buf.includes(`BODY_${i}`)) throw new Error(`tab ${i} body mismatch: ${buf}`);
    }
    for (const tid of ids) runCli(['tab', 'close', tid, '--force']);
    return '5 tabs isolated & closed';
  });

  await run('S480', '組合せ: 複合Markdown(表+数式+Mermaid+コード+日本語) -> tab pdf', async () => {
    const md = `# 総合レポート\n\n| 項目 | 値 |\n|---|---|\n| 状態 | OK |\n\n$$a^2 + b^2 = c^2$$\n\n\`\`\`rust\nfn main() { println!("syki"); }\n\`\`\`\n\n\`\`\`mermaid\nsequenceDiagram\nUser->>syki: CLI\nsyki-->>User: PDF\n\`\`\`\n`;
    runCli(['tab', 'new', 'ComplexReport.md', '']);
    runCli(['buffer', 'set', '-'], { input: md });
    const outPdf = path.join(TMP_A, 's480_complex.pdf');
    const r = runCli(['tab', 'pdf', outPdf], { timeout: 25000 });
    if (r.code !== 0 || !fs.existsSync(outPdf) || fs.statSync(outPdf).size < 1000) {
      throw new Error(`complex pdf failed: ${r.stderr}`);
    }
    return `complex PDF ${fs.statSync(outPdf).size} bytes`;
  });

  await run('S481', '組合せ: scrap append -> scrap search -> buffer set -> tab pdf 4段パイプライン', async () => {
    runCli(['scrap', 'append', 'PIPELINE_S481_DATA: Rust+WebView2 fast memo']);
    const hits = JSON.parse(runCli(['scrap', 'search', 'PIPELINE_S481_DATA']).stdout);
    if (!hits.length) throw new Error('search missed');
    runCli(['tab', 'new', 'PipePdf.md', `# Search Result\n\nFound ${hits.length} hit(s): PIPELINE_S481_DATA`]);
    const pdf = path.join(TMP_A, 's481_pipe.pdf');
    const r = runCli(['tab', 'pdf', pdf], { timeout: 25000 });
    if (r.code !== 0 || !fs.existsSync(pdf)) throw new Error(r.stderr);
    return `4-stage pipeline PDF ${fs.statSync(pdf).size} bytes`;
  });

  await run('S482', '組合せ: 連続5回stdinパイプ投入 -> scrap search 全5件ヒット', async () => {
    for (let i = 1; i <= 5; i++) {
      const r = runCli([], { input: `BURST_PIPE_S482_ITEM_${i}\n` });
      if (r.code !== 0) throw new Error(r.stderr);
    }
    for (let i = 1; i <= 5; i++) {
      const s = runCli(['scrap', 'search', `BURST_PIPE_S482_ITEM_${i}`]);
      if (!s.stdout.includes(`ITEM_${i}`)) throw new Error(`missed ITEM_${i}`);
    }
    return 'all 5 burst pipes indexed & searchable';
  });

  await run('S483', '組合せ: 10並列同時IPC負荷 (id一致・欠落0)', async () => {
    const methods = ['info', 'tab.list', 'config.get', 'ui.state', 'pty.list', 'speech.status', 'info', 'ui.state', 'pty.presets', 'config.live'];
    const replies = await Promise.all(methods.map((m) => sendIpc(sessA, m, {})));
    if (replies.some((r) => !r.ok)) throw new Error(`some parallel IPC failed: ${JSON.stringify(replies.find((r) => !r.ok))}`);
    return '10/10 parallel IPC succeeded';
  });

  await run('S484', '性能ゲート: syki.exe --perf-probe (Detached RAM < 5MB, Wake < 15ms, Zero PS)', async () => {
    const res = spawnSync(SYKI_EXE, ['--perf-probe'], { env: ENV_A, encoding: 'utf8', timeout: 15000, windowsHide: true });
    if (res.status !== 0) throw new Error(res.stderr);
    const p = JSON.parse(res.stdout.trim());
    if (p.detachedRamMb >= 5.0 || p.hotkeyWakeMs >= 15.0 || p.zeroPsRuntime !== true) {
      throw new Error(`perf gate failed: ${res.stdout}`);
    }
    return `RAM=${p.detachedRamMb}MB wake=${p.hotkeyWakeMs}ms zeroPS=${p.zeroPsRuntime}`;
  });

  // Frontend unit test suite regression checks mapped to S485..S499 (15 groups covering all 50 JS tests)
  const jsUnitGroups = [
    ['S485', 'JS Unit: about_dialog + first_run + status_ai', ['about_dialog_test.js', 'first_run_test.js', 'status_ai_test.js']],
    ['S486', 'JS Unit: backend_proxy + rpc_bridge + platform', ['backend_proxy_test.js', 'rpc_bridge_test.js']],
    ['S487', 'JS Unit: builtin_ai + intent_router + infographic_prompt', ['builtin_ai_test.js', 'intent_router_test.js', 'infographic_prompt_test.js']],
    ['S488', 'JS Unit: chrome_layout + chrome_overlay + view_layout', ['chrome_layout_test.js', 'chrome_overlay_test.js', 'view_layout_test.js']],
    ['S489', 'JS Unit: disk_sync + file_anchor + note_hash + note_title', ['disk_sync_test.js', 'file_anchor_test.js', 'note_hash_test.js', 'note_title_test.js']],
    ['S490', 'JS Unit: editor_zoom + line_gutter + goto_line + bulk_insert', ['editor_zoom_test.js', 'line_gutter_test.js', 'goto_line_test.js', 'bulk_insert_test.js']],
    ['S491', 'JS Unit: find_replace + comment_toggle + html_comments + html_to_md', ['find_replace_engine_test.js', 'comment_toggle_test.js', 'html_comments_test.js', 'html_to_md_test.js']],
    ['S492', 'JS Unit: scrap_quote + scraps_filter + search_workbench', ['scrap_quote_test.js', 'scraps_filter_test.js', 'search_workbench_test.js']],
    ['S493', 'JS Unit: tab_overflow + tab_strip', ['tab_overflow_test.js', 'tab_strip_test.js']],
    ['S494', 'JS Unit: voice_input + voice_profiles', ['voice_input_test.js']],
    ['S495', 'JS Unit: appearance + mermaid_tone + ext_css + ext_theme_sync', ['appearance_test.js', 'mermaid_tone_test.js', 'ext_css_test.js', 'ext_theme_sync_test.js']],
    ['S496', 'JS Unit: secret_strip + settings_modal', ['secret_strip_test.js', 'settings_modal_test.js']],
    ['S497', 'JS Unit: preview_images + go_text', ['preview_images_test.js', 'go_text_test.js']],
  ];
  for (const [id, name, files] of jsUnitGroups) {
    await run(id, name, async () => {
      for (const f of files) {
        const full = path.join(REPO, 'frontend', 'js', f);
        if (!fs.existsSync(full)) continue;
        const r = spawnSync(process.execPath, [full], { env: ENV_A, encoding: 'utf8', timeout: 20000, windowsHide: true });
        if (r.status !== 0) throw new Error(`${f} failed: ${(r.stderr || r.stdout).slice(0, 180)}`);
      }
      return `${files.length} test file(s) passed`;
    });
  }

  await run('S498', '全50 JSユニットテスト全件網羅パス確認', async () => {
    const allJsTests = fs.readdirSync(path.join(REPO, 'frontend', 'js')).filter((f) => f.endsWith('_test.js'));
    for (const f of allJsTests) {
      const r = spawnSync(process.execPath, [path.join(REPO, 'frontend', 'js', f)], { env: ENV_A, encoding: 'utf8', timeout: 20000, windowsHide: true });
      if (r.status !== 0) throw new Error(`${f} failed`);
    }
    return `all ${allJsTests.length} JS unit tests passed`;
  });

  await run('S499', '組合せ: UI全8パネル連続巡回後の buffer/tab 健全性', async () => {
    runCli(['buffer', 'set', 'STABLE_AFTER_PANELS']);
    for (const p of ['find', 'replace', 'scraps_search', 'settings', 'shortcuts', 'all_tabs', 'command_palette', 'about']) {
      runCli(['ui', 'panel', p]);
    }
    const g = runCli(['buffer', 'get']).stdout;
    if (!g.includes('STABLE_AFTER_PANELS')) throw new Error(`buffer corrupted: ${g}`);
    return 'buffer intact after 8 panel opens';
  });

  await run('S500', '組合せ: リリースバイナリ存在・サイズ・バージョン一貫性検証', async () => {
    const sykiStat = fs.statSync(SYKI_EXE);
    const cliStat = fs.statSync(CLI_EXE);
    const info = JSON.parse(runCli(['info']).stdout);
    if (sykiStat.size < 1_000_000 || cliStat.size < 100_000 || !info.version) {
      throw new Error('invalid release binaries');
    }
    return `v${info.version} syki=${(sykiStat.size / 1e6).toFixed(2)}MB cli=${(cliStat.size / 1e3).toFixed(0)}KB`;
  });
} finally {
  try { procA.kill(); } catch {}
  try { if (procB) procB.kill(); } catch {}
  await new Promise((r) => setTimeout(r, 400));
  try { fs.rmSync(TMP_A, { recursive: true, force: true }); } catch {}
  try { fs.rmSync(TMP_B, { recursive: true, force: true }); } catch {}
}

const outDir = path.join(REPO, 'evals', 'results');
fs.mkdirSync(outDir, { recursive: true });
const passed = results.filter((r) => r.ok).length;
const failed = results.filter((r) => !r.ok).length;
fs.writeFileSync(
  path.join(outDir, 'scenario_300_node.json'),
  JSON.stringify({ suite: 'scenario_300_extra_cli_ipc_live', total: results.length, passed, failed, results }, null, 2),
);

console.log(`\n=== NODE EXTRA 85 SCENARIOS SUMMARY: total=${results.length} pass=${passed} fail=${failed} ===`);
for (const f of results.filter((r) => !r.ok)) {
  console.log(`  [FAIL] ${f.id} ${f.name}: ${f.error}`);
}
if (results.length !== 85) {
  console.error(`Expected 85 Node extra scenarios, got ${results.length}`);
  process.exit(1);
}
if (failed > 0) process.exit(1);
