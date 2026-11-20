// Unit tests for secret_strip.js: the copy of the settings kept in localStorage has no credentials.
const assert = require('assert');
const fs = require('fs');
const path = require('path');

global.window = global;
const SS = require('./secret_strip.js');

const read = (p) => fs.readFileSync(path.resolve(__dirname, p), 'utf-8').replace(/\r\n/g, '\n');

// ---- secret key classification ----------------------------------------------------------------------
(function testSecretKeyWords() {
  assert.ok(Array.isArray(SS.SECRET_WORDS) && SS.SECRET_WORDS.length > 0, 'SECRET_WORDS is populated');
  const names = SS.SECRET_WORDS.concat(SS.SECRET_WORDS.map((w) => w.toUpperCase()), SS.SECRET_WORDS.map((w) => 'my_' + w + '_x'),
    ['apiKey', 'API_KEY', 'botToken', 'OPENAI_API_KEY', 'passwordHash', 'Secret', 'x-api-key']);
  names.forEach((n) => assert.strictEqual(SS.isSecretKey(n), true, 'expected secret key: ' + n));
  ['authorization', 'model', 'baseUrl', 'monkey', 'keyword', 'key', '', null, undefined, 'tok', 'pass'].forEach((n) => {
    assert.strictEqual(SS.isSecretKey(n), false, 'expected non-secret key: ' + JSON.stringify(n));
  });
})();

// ---- stripSecrets ----------------------------------------------------------------------------------
const settings = () => ({
  general: { autoSave: true, theme: 'dark', language: 'ja' },
  text: { baseUrl: 'https://api.example.com/v1', model: 'm', apiKey: 'sk-TEXT-AAAA1111' },
  autocomplete: { apiKey: 'sk-AUTO-BBBB2222', maxTokens: 256, enabled: true },
  vision: { apiKey: 'sk-VISION-CCCC3333', model: 'vm' },
  voice: { apiKey: '', model: 'w' },
  cli: { apiKey: 'sk-CLI-DDDD4444' },
  action: { apiKey: 'sk-ACTION-EEEE5555' },
  image: { apiKey: 'sk-IMAGE-FFFF6666' },
  cfBridge: { enabled: true, token: 'SECRET-TOKEN', endpoint: 'https://my.workers.dev' },
  scraps: { scrapDir: '~/scraps', gitRemote: 'https://user:GITPASS-HHHH8888@github.com/me/notes.git' },
  agents: [{ name: 'a', env: { OPENAI_API_KEY: 'sk-ENV-IIII9999', MODE: 'fast' } }],
  secrets: { list: ['ARR-JJJJ0000', ''], count: 3, on: true },
  shortcuts: { newTab: 'Ctrl+N' }
});

(function testStripBlanksEverySecretAndKeepsTheRest() {
  const input = settings();
  const before = JSON.stringify(input);
  const out = SS.stripSecrets(input);
  assert.strictEqual(JSON.stringify(input), before, 'the input must not be modified');

  const text = JSON.stringify(out);
  ['AAAA1111', 'BBBB2222', 'CCCC3333', 'DDDD4444', 'EEEE5555', 'FFFF6666', 'GGGG7777', 'HHHH8888', 'IIII9999', 'JJJJ0000', 'GITPASS', 'sk-']
    .forEach((frag) => assert.ok(!text.includes(frag), 'a secret survived: ' + frag));

  ['text', 'autocomplete', 'vision', 'voice', 'cli', 'action', 'image'].forEach((k) => assert.strictEqual(out[k].apiKey, '', k + '.apiKey'));
  assert.strictEqual(out.cfBridge.token, '');
  assert.strictEqual(out.agents[0].env.OPENAI_API_KEY, '');
  assert.deepStrictEqual(out.secrets.list, ['', ''], 'strings below a secret-named key are all blanked, arrays included');

  // Everything else is exactly as it was.
  assert.strictEqual(out.autocomplete.maxTokens, 256, 'a number under a secret-sounding key is a setting');
  assert.strictEqual(out.secrets.count, 3);
  assert.strictEqual(out.secrets.on, true);
  assert.strictEqual(out.autocomplete.enabled, true);
  assert.deepStrictEqual(out.general, input.general);
  assert.deepStrictEqual(out.shortcuts, input.shortcuts);
  assert.strictEqual(out.text.baseUrl, 'https://api.example.com/v1');
  assert.strictEqual(out.text.model, 'm');
  assert.strictEqual(out.cfBridge.endpoint, 'https://my.workers.dev');
  assert.strictEqual(out.agents[0].env.MODE, 'fast');
  assert.strictEqual(out.scraps.gitRemote, 'https://github.com/me/notes.git', 'credentials inside a URL are removed');
  assert.strictEqual(out.scraps.scrapDir, '~/scraps');
})();

(function testStripEdgeCases() {
  assert.strictEqual(SS.stripSecrets(null), null);
  assert.strictEqual(SS.stripSecrets(5), 5);
  assert.strictEqual(SS.stripSecrets('plain'), 'plain');
  assert.deepStrictEqual(SS.stripSecrets({}), {});
  assert.deepStrictEqual(SS.stripSecrets([]), []);
  assert.deepStrictEqual(SS.stripSecrets({ apiKey: null, token: false, password: 0 }), { apiKey: null, token: false, password: 0 },
    'only strings are credentials; null, booleans and numbers are left alone (as pkg/configpack does)');
  assert.deepStrictEqual(SS.stripSecrets({ a: undefined, f() {}, b: 1 }), { b: 1 }, 'what JSON.stringify drops is dropped');

  // A hostile __proto__ key from JSON must not become a prototype assignment.
  const evil = JSON.parse('{"__proto__": {"polluted": "yes"}, "ok": 1}');
  const cleaned = SS.stripSecrets(evil);
  assert.strictEqual(({}).polluted, undefined);
  assert.strictEqual(Object.getPrototypeOf(cleaned), Object.prototype);
  assert.strictEqual(cleaned.polluted, undefined);
  assert.strictEqual(cleaned.ok, 1);

  // Absurd nesting is cut off, not recursed into until the stack overflows.
  let deep = 'x';
  for (let i = 0; i < 5000; i++) deep = { a: deep };
  assert.doesNotThrow(() => SS.stripSecrets(deep));
  // An ancestor's name makes everything below it secret, however deep.
  assert.deepStrictEqual(SS.stripSecrets({ token: { a: { b: ['x', { c: 'y' }] } } }), { token: { a: { b: ['', { c: '' }] } } });
})();

(function testStripUserinfo() {
  const cases = {
    'https://user:pw@host/x': 'https://host/x',
    'https://tokenonly@host/x': 'https://host/x',
    'http://u:p@host:8080/x?y=1': 'http://host:8080/x?y=1',
    'HTTPS://u:p@HOST/x': 'HTTPS://HOST/x',
    'https://user:p@ss@host/x': 'https://host/x',
    'https://host/a@b': 'https://host/a@b',
    'https://host/x?q=a@b': 'https://host/x?q=a@b',
    'ssh://git@github.com/x.git': 'ssh://git@github.com/x.git',
    'git@github.com:me/x.git': 'git@github.com:me/x.git',
    'plain text': 'plain text',
    '': ''
  };
  Object.keys(cases).forEach((k) => assert.strictEqual(SS.stripUserinfo(k), cases[k], k));
})();

// ---- the value of a secret query parameter (https://host/v1?key=SECRET) ------------------------------------
// Golden corpus aligned with legacy v1 query-secret stripping semantics.
(function testQuerySecretsGoldenCorpus() {
  const rows = [
    ['https://host/x?key=SECRET', '', 'https://host/x?key=', true],
    ['https://host/x?key=SECRET', '<set>', 'https://host/x?key=<set>', true],
    ['https://host/x?a=1&api_key=SECRET&b=2', '', 'https://host/x?a=1&api_key=&b=2', true],
    ['https://host/x?token=S&Access_Token=T', '', 'https://host/x?token=&Access_Token=', true],
    ['https://host/x?sig=S#frag', '', 'https://host/x?sig=#frag', true],
    ['https://host/x?%6bey=SECRET', '', 'https://host/x?%6bey=', true],
    ['HTTPS://host/x?Key=SECRET', '', 'HTTPS://host/x?Key=', true],
    ['https://host/x?authorization=Bearer%20abc', '', 'https://host/x?authorization=', true],
    ['https://host/x?keyword=fine&monkey=1', '', 'https://host/x?keyword=fine&monkey=1', false],
    ['https://host/x?key=', '', 'https://host/x?key=', false],
    ['https://host/x?key=<set>', '<set>', 'https://host/x?key=<set>', false],
    ['https://host/x?empty=&key', '', 'https://host/x?empty=&key', false],
    ['https://host/x#key=SECRET', '', 'https://host/x#key=SECRET', false],
    ['https://host/x', '', 'https://host/x', false],
    ['file:///C:/x?key=SECRET', '', 'file:///C:/x?key=SECRET', false],
    ['plain ?key=SECRET', '', 'plain ?key=SECRET', false],
    ['', '', '', false],
  ];
  rows.forEach(([input, repl, want, changed]) => {
    const got = SS.redactQuerySecrets(input, repl);
    assert.strictEqual(got, want, 'redactQuerySecrets(' + JSON.stringify(input) + ', ' + JSON.stringify(repl) + ')');
    assert.strictEqual(got !== input, changed, 'changed flag of ' + JSON.stringify(input));
  });

  const blankIn = '{"text":{"baseUrl":"https://h.example/v1?key=K2","apiKey":"sk-x"},"scraps":{"gitRemoteUrl":"https://github.com/a/b.git?token=Q1"},"other":{"u":"https://u:pw@h.example/p?sig=S3&auth=A4&x=1#frag"}}';
  const blankWant = '{"text":{"baseUrl":"https://h.example/v1?key=","apiKey":""},"scraps":{"gitRemoteUrl":"https://github.com/a/b.git?token="},"other":{"u":"https://h.example/p?sig=&auth=&x=1#frag"}}';
  assert.strictEqual(JSON.stringify(SS.stripSecrets(JSON.parse(blankIn))), blankWant, 'query and userinfo secrets blanked consistently');
  const clean = '{"a":"https://h.example/v1?alt=json&keyword=fine&monkey=1","b":"https://h.example/v1?key=","c":"https://h.example/v1?empty=&key","d":"not a url ?key=SECRET","e":"ftp://h/x?key=SECRET"}';
  assert.strictEqual(JSON.stringify(SS.stripSecrets(JSON.parse(clean))), clean, 'harmless parameters, blank ones and non-URLs are left as they are');
})();

(function testTheLocalCopyKeepsNoQueryCredentials() {
  const cfg = {
    text: { baseUrl: 'https://h.example/v1?key=K2&alt=json', model: 'm' },
    scraps: { gitRemoteUrl: 'https://github.com/a/b.git?token=Q1' },
    other: { u: 'https://u:pw@h.example/p?sig=S3&auth=A4&x=1#frag' }
  };
  const out = SS.stripSecrets(cfg);
  assert.strictEqual(out.text.baseUrl, 'https://h.example/v1?key=&alt=json', 'the name, the order and the harmless parameters stay');
  assert.strictEqual(out.scraps.gitRemoteUrl, 'https://github.com/a/b.git?token=');
  assert.strictEqual(out.other.u, 'https://h.example/p?sig=&auth=&x=1#frag', 'userinfo and query secrets together');
  assert.strictEqual(cfg.text.baseUrl, 'https://h.example/v1?key=K2&alt=json', 'the input is not modified');

  const kept = {};
  SS.saveLocalCopy({ setItem: (k, v) => { kept[k] = String(v); }, removeItem: (k) => { delete kept[k]; } }, cfg);
  const stored = kept.md_notepad_config_v3;
  ['K2', 'Q1', 'S3', 'A4', 'pw@'].forEach((leak) => assert.ok(!stored.includes(leak), leak + ' reached the WebView profile'));
  assert.ok(stored.includes('alt=json'), 'but the rest of the address did');

  // an address is only touched when it is an http(s) URL with a value in a secret parameter
  ['not a url ?key=SECRET', 'ftp://h/x?key=SECRET', 'https://h/x?key=', 'https://h/x?keyword=fine&monkey=1', 'https://h/x#key=SECRET', '']
    .forEach((v) => assert.strictEqual(SS.stripURLSecrets(v), v, v));
  assert.strictEqual(SS.stripURLSecrets('https://h/x?%6bey=SECRET&Access_Token=T'), 'https://h/x?%6bey=&Access_Token=');
})();

// ---- saveLocalCopy ---------------------------------------------------------------------------------
class FakeStorage {
  constructor(initial) { this.map = Object.assign({}, initial); this.failSet = false; this.failRemove = false; }
  getItem(k) { return Object.prototype.hasOwnProperty.call(this.map, k) ? this.map[k] : null; }
  setItem(k, v) { if (this.failSet) throw new Error('QuotaExceededError'); this.map[k] = String(v); }
  removeItem(k) { if (this.failRemove) throw new Error('SecurityError'); delete this.map[k]; }
}

(function testSaveLocalCopy() {
  const old = JSON.stringify(settings()); // what an older build stored: the whole thing, keys included
  const s = new FakeStorage({ md_notepad_config_v3: old, syki_config_v1: old, other: 'kept' });
  SS.saveLocalCopy(s, settings());

  const stored = s.getItem('md_notepad_config_v3');
  assert.ok(stored && !/sk-|MTIz|GITPASS|ARR-/.test(stored), 'the old copy with keys is overwritten by a clean one');
  assert.deepStrictEqual(JSON.parse(stored), SS.stripSecrets(settings()));
  assert.strictEqual(s.getItem('syki_config_v1'), null, 'the legacy copy, which the loader prefers, is removed');
  assert.strictEqual(s.getItem('other'), 'kept', 'nothing else is touched');
  assert.strictEqual(SS.LOCAL_KEY, 'md_notepad_config_v3');
  assert.strictEqual(SS.LEGACY_KEY, 'syki_config_v1');

  // Saving again is the same (idempotent), and a config without secrets round-trips unchanged.
  SS.saveLocalCopy(s, settings());
  assert.strictEqual(s.getItem('md_notepad_config_v3'), stored);
  const plain = { general: { theme: 'dark' }, shortcuts: { a: 'b' } };
  SS.saveLocalCopy(s, plain);
  assert.deepStrictEqual(JSON.parse(s.getItem('md_notepad_config_v3')), plain);
})();

(function testSaveLocalCopyFailures() {
  // The write fails (quota): an older copy with keys in it must not be left behind.
  const s = new FakeStorage({ md_notepad_config_v3: JSON.stringify(settings()) });
  s.failSet = true;
  assert.doesNotThrow(() => SS.saveLocalCopy(s, settings()));
  assert.strictEqual(s.getItem('md_notepad_config_v3'), null);

  // Storage that throws on everything (blocked site data): never propagates.
  const dead = new FakeStorage({});
  dead.failSet = true;
  dead.failRemove = true;
  assert.doesNotThrow(() => SS.saveLocalCopy(dead, settings()));
  assert.doesNotThrow(() => SS.saveLocalCopy(null, settings()));
  assert.doesNotThrow(() => SS.saveLocalCopy(undefined, settings()));
})();

// ---- what the page does with the copy: the real values come from config.json ------------------------
(function testLoaderOrderRestoresTheKeysFromTheFile() {
  // The same merge order as initApp: loadLocalConfigSync (the local copy), then syncBackendConfig (the
  // file, authoritative). Both use Object.assign per section.
  const config = { text: { apiKey: '', model: 'default' }, cfBridge: { token: '' } };
  const s = new FakeStorage({});
  SS.saveLocalCopy(s, { text: { apiKey: 'sk-REAL', model: 'from-local' }, cfBridge: { token: 'BOT-REAL' } });
  const local = JSON.parse(s.getItem('md_notepad_config_v3'));
  Object.assign(config.text, local.text);
  Object.assign(config.cfBridge, local.cfBridge);
  assert.strictEqual(config.text.apiKey, '', 'from the local copy alone there is no key');
  assert.strictEqual(config.text.model, 'from-local', 'but the other settings are there for the first paint');

  const file = { text: { apiKey: 'sk-REAL', model: 'from-local' }, cfBridge: { token: 'BOT-REAL' } };
  Object.assign(config.text, file.text);
  Object.assign(config.cfBridge, file.cfBridge);
  assert.strictEqual(config.text.apiKey, 'sk-REAL');
  assert.strictEqual(config.cfBridge.token, 'BOT-REAL');
})();

// ---- wiring: index.html and app.js ------------------------------------------------------------------
(function testWiring() {
  const html = read('../index.html');
  const app = read('./app.js');

  const at = (f) => html.indexOf('js/' + f + '?v=');
  assert.ok(at('secret_strip.js') > 0, 'index.html loads secret_strip.js');
  assert.ok(at('secret_strip.js') < at('app.js'), 'before app.js');
  assert.ok(/<script src="js\/secret_strip\.js\?v=\d+\.\d+\.\d+"><\/script>/.test(html), 'with the usual ?v= pattern');

  // savePersistentConfig: the local copy goes through SecretStrip, the backend gets the whole config.
  const start = app.indexOf('async function savePersistentConfig()');
  assert.ok(start > 0, 'savePersistentConfig not found');
  const body = app.slice(start, app.indexOf('\n  }\n', start));
  assert.ok(body.includes('window.SecretStrip.saveLocalCopy(localStorage, config)'), 'the local copy is written by SecretStrip');
  assert.ok(!/localStorage\.setItem\(/.test(body), 'savePersistentConfig writes nothing to localStorage by hand');
  assert.ok(body.includes('window.backend.saveConfig(JSON.stringify(config))'), 'the backend still receives the real, complete config');
  assert.ok(!/SecretStrip\.stripSecrets\(config\)[^;]*saveConfig|saveConfig\([^)]*stripSecrets/.test(body), 'and never the stripped one (that would erase the keys in config.json)');

  // No other place in app.js may put the settings object into localStorage.
  assert.ok(!/localStorage\.setItem\([^)]*JSON\.stringify\(config\)/.test(app), 'no other write of the whole config to localStorage');
  assert.ok(!app.includes("setItem('md_notepad_config_v3'"), 'the settings key is only written by SecretStrip');

  // Startup order: the (keyless) local copy first, the file after it.
  assert.ok(app.indexOf('loadLocalConfigSync();\n\n    // 1. Instant Synchronous First Paint') > 0);
  assert.ok(app.indexOf('function initApp()') < app.indexOf('await syncBackendConfig();'));
})();

console.log('secret_strip tests passed');
