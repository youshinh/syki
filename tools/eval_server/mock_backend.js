// Mock backend for syki-lite headless-Edge smoke & acceptance evaluations.
// Injected by tools/eval_server/server.mjs before any frontend script runs.
// Includes ONLY kept syki-lite features + window/platform/UI helpers (13 pruned features omitted).
(function () {
  'use strict';
  var B = window.__DOCSHOT_BOOT;
  var D = window.__docshot = {
    boot: B,
    calls: [],
    ghost: '',
    workspaceScanned: false,
    llmReply: 'Demo reply.',
  };

  // ---- fixed clock ----
  var RealDate = Date;
  var base = new RealDate(B.clock.y, B.clock.m, B.clock.d, B.clock.h, B.clock.mi, 0).getTime();
  var t0 = RealDate.now();
  var frozen = null;
  function nowMs() { return frozen !== null ? frozen : base + (RealDate.now() - t0); }
  class FakeDate extends RealDate {
    constructor(...args) { if (args.length === 0) super(nowMs()); else super(...args); }
    static now() { return nowMs(); }
  }
  window.Date = FakeDate;
  D.freezeClock = function () { frozen = nowMs(); return frozen; };
  D.unfreezeClock = function () { if (frozen !== null) { t0 = RealDate.now(); base = frozen; frozen = null; } };

  // ---- block outbound network ----
  var realFetch = window.fetch.bind(window);
  window.fetch = function (input, init) {
    var raw = typeof input === 'string' ? input : (input && input.url) || '';
    var abs = new URL(raw, location.href);
    if (abs.origin !== location.origin) {
      D.calls.push({ fn: 'fetch(blocked)', args: [abs.origin] });
      return Promise.reject(new TypeError('eval_server: network blocked'));
    }
    return realFetch(input, init);
  };

  var FRESH = !!(B.query && B.query.fresh === '1');
  var NOSESSION = !!(B.query && B.query.nosession === '1');
  if (B.query && B.query.nomodel === '1' && B.config && B.config.text) {
    B.config.text.baseUrl = 'https://generativelanguage.googleapis.com';
    B.config.text.model = 'gemini-flash-lite-latest';
    B.config.text.apiKey = '';
  }
  if (B.query && B.query.semantic === '1' && B.config) {
    B.config.semantic = { enabled: true, model: { baseUrl: 'http://localhost:11434', model: 'bge-m3' } };
  }

  // ---- seed storage ----
  try {
    localStorage.clear();
    if (FRESH) {
      try { Object.defineProperty(navigator, 'language', { get: function () { return B.lang === 'ja' ? 'ja-JP' : 'en-US'; } }); } catch (e) { /* keep */ }
    } else {
      localStorage.setItem('md_notepad_config_v3', JSON.stringify(B.config));
    }
    if (!NOSESSION) {
      localStorage.setItem('syki_session_v1', JSON.stringify(B.session));
      localStorage.setItem('syki_workspace_folder', B.workspace.root);
      if (FRESH) localStorage.setItem('syki_calm_toolbar_v1', '1');
    }
    localStorage.setItem('syki_cli_history', JSON.stringify(B.cliHistory));
  } catch (e) { /* storage unavailable */ }

  var resolve = function (v) { return Promise.resolve(v); };
  var FILTERED = { searchScraps: 1, searchScrapsSemantic: 1 };
  var log = function (fn, args) {
    var entry = { fn: fn, args: Array.prototype.slice.call(args, 0, 3) };
    if (FILTERED[fn]) {
      entry.args = entry.args.slice(0, 2);
      entry.filter = args[2] == null ? null : JSON.parse(JSON.stringify(args[2]));
    }
    D.calls.push(entry);
  };

  function findNote(p) {
    for (var i = 0; i < B.noteFiles.length; i++) if (B.noteFiles[i].path === p) return B.noteFiles[i];
    return null;
  }

  // ---- search & filter (F02, F13) ----
  D.filter = { tags: {}, extra: [], options: null, reject: '', delayMs: 0 };
  D.filter.tags['2026-09-17.md'] = { file: ['work', 'idea'] };
  D.filter.tags['2026-09-15.md'] = { file: ['work', 'urgent'] };
  D.filter.tags['2026-09-12.md'] = { file: ['reading'] };
  (function () {
    for (var day = 18; day <= 28; day++) {
      var own = [];
      if (day % 2 === 0) own.push('work');
      if (day % 3 === 0) own.push('reading');
      if (day % 4 === 0) own.push('urgent');
      if (own.length) D.filter.tags['2026-09-' + day + '.md'] = { file: own };
    }
  })();
  function dayOfName(name) {
    var m = /^(\d{4})-(\d{2})-(\d{2})(?!\d)/.exec(String(name || ''));
    if (!m) return '';
    var dt = new RealDate(RealDate.UTC(+m[1], +m[2] - 1, +m[3]));
    return dt.getUTCFullYear() === +m[1] && dt.getUTCMonth() === +m[2] - 1 && dt.getUTCDate() === +m[3] ? m[1] + '-' + m[2] + '-' + m[3] : '';
  }
  function tagInfo(name) {
    if (D.filter.tags[name]) return D.filter.tags[name];
    for (var i = 0; i < D.filter.extra.length; i++) if (D.filter.extra[i].fileName === name) return D.filter.extra[i].tags || null;
    return null;
  }
  function tagsOfLine(name, line) {
    var info = tagInfo(name), out = info && info.file ? info.file.slice() : [];
    ((info && info.lines) || []).forEach(function (r) { if (line >= r.from && line <= r.to) out = out.concat(r.tags); });
    return out;
  }
  function badFilter(f) {
    if (!f) return '';
    if (f.tags != null && (!Array.isArray(f.tags) || f.tags.length > 8)) return 'filter: at most 8 tags';
    var re = /^\d{4}-\d{2}-\d{2}$/;
    if ((f.from != null && !re.test(f.from)) || (f.to != null && !re.test(f.to))) return 'filter: dates are written YYYY-MM-DD';
    return '';
  }
  function passes(name, line, f) {
    if (!f) return true;
    if (f.from || f.to) {
      var day = dayOfName(name);
      if (!day || (f.from && day < f.from) || (f.to && day > f.to)) return false;
    }
    var want = f.tags || [];
    if (want.length) {
      var have = tagsOfLine(name, line);
      for (var i = 0; i < want.length; i++) if (have.indexOf(want[i]) === -1) return false;
    }
    return true;
  }
  function pad2(n) { return (n < 10 ? '0' : '') + n; }
  function allNoteNames() {
    var seen = {}, names = [];
    var add = function (n) { if (!seen[n]) { seen[n] = true; names.push(n); } };
    B.scraps.forEach(function (f) { add(f.fileName); });
    D.filter.extra.forEach(function (f) { add(f.fileName); });
    for (var i = 0; i < D.semantic.total; i++) add('2026-09-' + pad2(28 - i) + '.md');
    return names;
  }
  function scrapFilterOptions() {
    if (D.filter.reject) return Promise.reject(new Error(D.filter.reject));
    var answer = D.filter.options;
    if (!answer) {
      var names = allNoteNames(), counts = {}, undated = 0;
      names.forEach(function (n) {
        if (!dayOfName(n)) undated++;
        var info = tagInfo(n), mine = {};
        ((info && info.file) || []).forEach(function (t) { mine[t] = 1; });
        ((info && info.lines) || []).forEach(function (r) { r.tags.forEach(function (t) { mine[t] = 1; }); });
        Object.keys(mine).forEach(function (t) { counts[t] = (counts[t] || 0) + 1; });
      });
      answer = {
        tags: Object.keys(counts).map(function (t) { return { tag: t, files: counts[t], entries: counts[t] }; })
          .sort(function (a, b) { return b.files - a.files || (a.tag < b.tag ? -1 : 1); }),
        files: names.length, undated: undated
      };
    }
    var copy = JSON.parse(JSON.stringify(answer));
    return D.filter.delayMs > 0 ? new Promise(function (res) { setTimeout(function () { res(copy); }, D.filter.delayMs); }) : resolve(copy);
  }

  function searchScraps(query, filter) {
    var q = String(query || '').toLowerCase();
    var out = [];
    B.scraps.concat(D.filter.extra.map(function (f) {
      return { filePath: 'C:\\Users\\demo\\Documents\\syki-lite\\scraps\\' + f.fileName, fileName: f.fileName, content: f.lines.join('\n') };
    })).forEach(function (f) {
      var lines = f.content.split('\n');
      var matches = [];
      lines.forEach(function (line, i) {
        if (q && line.toLowerCase().indexOf(q) !== -1 && passes(f.fileName, i + 1, filter)) {
          var parts = [];
          if (i > 0) parts.push(lines[i - 1]);
          parts.push(line);
          if (i + 1 < lines.length) parts.push(lines[i + 1]);
          matches.push({ lineNumber: i + 1, lineText: line, snippet: parts.join('\n') });
        }
      });
      if (matches.length) out.push({ filePath: f.filePath, fileName: f.fileName, matches: matches });
    });
    return out;
  }

  // ---- semantic search (F13) ----
  D.semantic = { total: 12, notes: [], semantic: true, pending: 0, leftOut: 0, reject: '' };
  function hitText(i) {
    var lines = D.semantic.lines;
    return lines && lines[i] ? lines[i] : 'Meaning hit ' + (i + 1) + ': bamboo grows fast and can be cut after about three years';
  }
  function semanticHit(i) {
    var name = '2026-09-' + pad2(28 - i) + '.md';
    return {
      filePath: 'C:\\Users\\demo\\Documents\\syki-lite\\scraps\\' + name, fileName: name,
      matches: [{
        lineNumber: 3, lineText: hitText(i),
        snippet: hitText(i),
        heading: '2026-09-' + pad2(28 - i) + ' 09:00', headingLine: 1, endLine: 5 + (i % 3),
        score: Number((0.9 - i * 0.02).toFixed(2)), source: 'semantic',
      }],
    };
  }
  function semanticHits(filter) {
    var hits = [];
    for (var i = 0; i < D.semantic.total; i++) {
      var hit = semanticHit(i);
      if (passes(hit.fileName, hit.matches[0].lineNumber, filter)) hits.push(hit);
    }
    return hits;
  }
  function searchScrapsSemantic(q, limit, filter) {
    if (D.semantic.reject) return Promise.reject(new Error(D.semantic.reject));
    var bad = badFilter(filter);
    if (bad) return Promise.reject(new Error(bad));
    var all = semanticHits(filter), n = Math.min(Number(limit) || 10, all.length);
    return resolve({ semantic: D.semantic.semantic, pending: D.semantic.pending, leftOut: D.semantic.leftOut, truncated: all.length > n, notes: D.semantic.notes.slice(), results: all.slice(0, n) });
  }

  // ---- print panel (F08) ----
  var BLANK_PDF = 'data:application/pdf;base64,' + btoa(['%PDF-1.1', '1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj', '2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj', '3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]>>endobj', 'trailer<</Root 1 0 R>>', '%%EOF', ''].join(String.fromCharCode(10)));
  D.print = { pages: 4, reject: '', pickPath: ['C:', 'Users', 'demo', 'Documents', 'note.pdf'].join(String.fromCharCode(92)), saveReject: '', seq: 0, saved: [], closed: 0 };
  function printPreview() {
    if (D.print.reject) return Promise.reject(new Error(D.print.reject));
    var n = ++D.print.seq, wait = (D.print.delays || []).length ? D.print.delays.shift() : 0;
    var answer = { url: BLANK_PDF + '#mock' + n, pages: D.print.pages, bytes: 1234 };
    return wait > 0 ? new Promise(function (res) { setTimeout(function () { res(answer); }, wait); }) : resolve(answer);
  }
  function printPickPdfPath() { return resolve(D.print.pickPath); }
  function printSavePdf(opts, path) {
    if (D.print.saveReject) return Promise.reject(new Error(D.print.saveReject));
    D.print.saved.push(path);
    (D.print.requests = D.print.requests || []).push(JSON.parse(JSON.stringify(opts || {})));
    var answer = { path: path, bytes: 1234, pages: D.print.pages };
    var wait = D.print.saveDelay || 0;
    return wait > 0 ? new Promise(function (res) { setTimeout(function () { res(answer); }, wait); }) : resolve(answer);
  }
  function printPreviewClose() { D.print.closed++; return resolve(null); }
  D.print.system = [];
  function printSystem(title) {
    D.print.system.push(title);
    if (D.print.systemReject) return Promise.reject(new Error(D.print.systemReject));
    var wait = D.print.systemDelay || 0;
    return wait > 0 ? new Promise(function (res) { setTimeout(function () { res(true); }, wait); }) : resolve(true);
  }

  // ---- semantic settings (F13) ----
  D.semanticIndex = {
    index: { exists: false, chunks: 0, files: 0, size_bytes: 0, updated: '', new_files: 53, changed_files: 0, removed_files: 0, rebuild_needed: false, corrupt: false },
    updateDelay: 0, updateReject: '', bigRun: false, statuses: 0, updates: [], held: null
  };
  function semanticDest(section) {
    var m = (section && section.model) || {}, base = String(m.baseUrl || '').trim();
    var host = base.replace(/^[a-z]+:\/\//i, '').replace(/\/.*$/, '').toLowerCase();
    return { host: host || '127.0.0.1:11434', local: !host || /^(localhost|127\.|\[::1\])/.test(host), model: String(m.model || '').trim() };
  }
  function semanticAllowed(section, d) {
    return d.local || !!(section && section.privacy && section.privacy.cloudConsent && section.privacy.cloudConsent[d.host]);
  }
  function semanticStatus(section) {
    D.semanticIndex.statuses++;
    var s = section || { enabled: true, model: { baseUrl: 'http://localhost:11434', model: 'bge-m3' } }, d = semanticDest(s);
    return resolve(Object.assign({
      enabled: s.enabled === true, model: d.model ? 'ollama:' + d.model : '', model_error: d.model ? '' : 'the embedding model is not set',
      destination: d.host, local: d.local, consent_given: semanticAllowed(s, d), scrap_dir: 'C:\\Users\\demo\\Documents\\syki-lite\\scraps',
      index_dir: 'C:\\Users\\demo\\AppData\\Roaming\\syki-lite\\index\\demo', files_in_folder: 53, index_model: d.model ? 'ollama:' + d.model : '', dim: 1024
    }, D.semanticIndex.index));
  }
  function semanticUpdate(section, rebuild, yes) {
    var s = section || {}, d = semanticDest(s), st = D.semanticIndex;
    st.updates.push({ rebuild: !!rebuild, yes: !!yes, enabled: s.enabled === true, host: d.host, model: d.model });
    if (s.enabled !== true) return Promise.reject(new Error('not_enabled'));
    if (!semanticAllowed(s, d)) return Promise.reject(new Error('consent_required: ' + d.host));
    if (st.updateReject) return Promise.reject(new Error(st.updateReject));
    if (st.bigRun && !d.local && !yes) return Promise.reject(new Error('confirm_required: 1500 chunk texts to ' + d.host));
    var before = st.index.new_files + st.index.changed_files;
    var finish = function () {
      st.index = Object.assign({}, st.index, { exists: true, chunks: 412, files: 53, size_bytes: 3355443, updated: '2026-09-18T10:24:00+09:00', new_files: 0, changed_files: 0, removed_files: 0, rebuild_needed: false, corrupt: false });
      return { files: 53, files_changed: before, files_removed: 0, chunks: 412, chunks_new: before * 8, chunks_reused: 0, embedded: before * 8, seconds: 1.4, rebuilt: !!rebuild, local: d.local, model: d.model };
    };
    if (typeof window.__semanticProgress === 'function') {
      window.__semanticProgress(0, 53, 0, 424);
      window.__semanticProgress(21, 53, 168, 424);
    }
    if (!(st.updateDelay > 0)) return resolve(finish());
    return new Promise(function (res, rej) {
      var timer = setTimeout(function () { st.held = null; res(finish()); }, st.updateDelay);
      st.held = { cancel: function () { clearTimeout(timer); st.held = null; rej(new Error('cancelled')); } };
    });
  }
  function cancelSemanticUpdate() {
    var h = D.semanticIndex.held;
    if (!h) return resolve(false);
    h.cancel();
    return resolve(true);
  }

  var impl = {
    getAppVersion: function () { return resolve(B.version); },
    getAppInfo: function () {
      return resolve({
        version: B.aboutVersion || B.version, commit: 'lite-dev', builtAt: '2026-09-18T09:00:00Z', os: 'windows', arch: 'amd64',
        executable: 'C:\\Program Files\\syki-lite\\syki.exe',
        configDir: 'C:\\Users\\demo\\AppData\\Roaming\\syki-lite', configFile: 'C:\\Users\\demo\\AppData\\Roaming\\syki-lite\\config.json',
        scrapDir: 'C:\\Users\\demo\\Documents\\syki-lite\\scraps', signing: 'unsigned'
      });
    },
    getPlatformCapabilities: function () { return resolve({ os: 'win32', nativeImeSwitch: true, tray: true, globalHotkey: true }); },
    getConfig: function () { return resolve(FRESH ? '' : JSON.stringify(B.config)); },
    saveConfig: function (json) { try { D.savedConfig = JSON.parse(json); } catch (e) { /* ignore */ } return resolve(null); },
    getSession: function () { return resolve(NOSESSION ? '' : JSON.stringify(B.session)); },
    saveSession: function () { return resolve(null); },
    getStartupFile: function () { return resolve(null); },
    scanFolderFiles: function () {
      return resolve(B.workspace.notes).then(function (v) { setTimeout(function () { D.workspaceScanned = true; }, 400); return v; });
    },
    readFileByPath: function (p) {
      var n = findNote(p);
      return resolve(n ? { path: n.path, title: n.title, content: n.content, encoding: 'UTF-8' } : { path: p, title: String(p).split(/[\\/]/).pop(), content: '', encoding: 'UTF-8' });
    },
    saveFile: function (p) { return resolve({ path: p, title: String(p).split(/[\\/]/).pop(), success: true }); },
    saveFileAs: function () { return resolve(null); },
    queryLLMAsync: function (reqId) {
      setTimeout(function () { if (window.__onLLMResult) window.__onLLMResult(reqId, D.llmReply, ''); }, 600);
      return resolve(null);
    },
    autocompleteAsync: function (reqId) {
      setTimeout(function () { if (window.__onAutocompleteResult) window.__onAutocompleteResult(reqId, D.ghost || '', ''); }, 40);
      return resolve(null);
    },
    detectLLMProvider: function () { return resolve('ollama'); },
    searchScraps: function (q, max, filter) {
      var bad = badFilter(filter);
      return bad ? Promise.reject(new Error(bad)) : resolve(searchScraps(q, filter));
    },
    searchScrapsSemantic: searchScrapsSemantic,
    scrapFilterOptions: scrapFilterOptions,
    printPreview: printPreview,
    printPickPdfPath: printPickPdfPath,
    printSavePdf: printSavePdf,
    printPreviewClose: printPreviewClose,
    printSystem: printSystem,
    semanticStatus: semanticStatus,
    semanticUpdate: semanticUpdate,
    cancelSemanticUpdate: cancelSemanticUpdate,
    saveAsset: function (dir, ext) { return resolve({ relPath: './assets/pasted-1.' + (ext || 'png'), fileUrl: '' }); },
    importAssetFile: function (dir, name) { return resolve({ relPath: './assets/' + name, fileUrl: '' }); },
    updateGlobalShortcut: function () { return resolve(true); },
  };

  // Remaining kept syki-lite backend methods (UI / Win32 / Shell / PTY / Speech / Audio / Vision / ImageGen)
  var names = (
    'openFile openFolder exportPlainTextAs ' +
    'runCommandFilter runCommandFilterAsync cancelCommandFilter ' +
    'queryVisionAsync generateImageAsync ' +
    'trimMemory closeWindow minimizeWindow toggleMaximize toggleFullscreen forceQuit openExternal setIMEMode ' +
    'watchActiveFile unwatchActiveFile openPath revealPath ' +
    'transcribeAudioAsync retryVoiceCacheAsync keepVoiceCache discardVoiceCache ' +
    'meetingRecordingSupported checkMeetingAudioAsync startMeetingRecording stopMeetingRecordingAsync abortMeetingRecording'
  ).split(' ');

  var backend = {};
  Object.keys(impl).forEach(function (k) {
    backend[k] = function () { log(k, arguments); return impl[k].apply(null, arguments); };
  });
  names.forEach(function (k) {
    if (!backend[k]) backend[k] = function () { log(k, arguments); return resolve(null); };
  });
  window.backend = backend;

  // ---- editor helpers ----
  D.editor = function () { return document.getElementById('editor'); };
  D.lineHeight = function () { return parseFloat(getComputedStyle(D.editor()).lineHeight) || 22; };
  D.charXY = function (index) {
    var ed = D.editor();
    var c = window.getCharPixelCoords(index, ed);
    var r = ed.getBoundingClientRect();
    return { x: r.left + c.left - ed.scrollLeft, y: r.top + c.top - ed.scrollTop };
  };
  D.find = function (str, from) { return D.editor().value.indexOf(str, from || 0); };
  D.lineStart = function (line) {
    var lines = D.editor().value.split('\n'), off = 0;
    for (var i = 0; i < line - 1 && i < lines.length; i++) off += lines[i].length + 1;
    return off;
  };
  D.lineEnd = function (line) { return D.lineStart(line) + (D.editor().value.split('\n')[line - 1] || '').length; };
  D.setCaret = function (start, end) {
    var ed = D.editor();
    ed.focus();
    ed.setSelectionRange(start, end === undefined ? start : end);
    ed.dispatchEvent(new KeyboardEvent('keyup', { key: 'ArrowRight', bubbles: true }));
  };
  D.isReady = function () {
    var tabs = document.querySelectorAll('#tabs-list .tab-item').length;
    var msg = document.getElementById('stat-message');
    return document.readyState === 'complete' && tabs >= 1 && (D.workspaceScanned || NOSESSION) && !(msg && msg.textContent.trim());
  };
})();
