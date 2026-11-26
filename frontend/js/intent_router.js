(function (root, factory) {
  if (typeof module === 'object' && module.exports) {
    module.exports = factory();
  } else {
    root.IntentRouter = factory();
  }
}(typeof globalThis !== 'undefined' ? globalThis : this, function () {
  'use strict';

  const HISTORY_STORAGE_KEY = 'syki_intent_history_v1';
  const HISTORY_MAX = 16;

  const INTENT_CHOICES = [
    { id: 'ask_ai', description: 'Answer a question, explain, brainstorm, or generate general text in the note.' },
    { id: 'edit_note', description: 'Rewrite, proofread, summarize, translate, or reformat the selected/current text in place.' },
    { id: 'diagram', description: 'Create a Mermaid diagram (flowchart, sequence, ER, architecture, mindmap) from the request or note.' },
    { id: 'search_semantic', description: 'Search past notes/scraps by meaning, topic, or vague memory (natural language).' },
    { id: 'search_grep', description: 'Search past notes/scraps by exact keyword, identifier, symbol, code, or regex.' },
    { id: 'send_agent', description: 'Delegate a coding, debugging, shell command, or file task to the right-side Agent/Terminal pane.' },
    { id: 'run_cli', description: 'Generate and run a shell/CLI pipeline command over the selected text or local environment (e.g. ping IPs, curl URLs, sort/dedupe/aggregate CSV/JSON/logs).' },
    { id: 'infographic', description: 'Generate an infographic / visual slide image from the selected text or Mermaid diagram.' }
  ];

  const VALID_ROUTES = new Set(INTENT_CHOICES.map((c) => c.id));

  const SYSTEM_HINT = [
    'Route the user request from the AI/voice bar to exactly one action ID.',
    'Use [UI State] and [Recent History] (especially user corrections) as strong signals:',
    '- If hasSelection=true and the user asks to shorten, fix, translate, format, summarize into meeting minutes, or rewrite, prefer edit_note.',
    '- If the user asks to find/look up past notes by topic or memory, prefer search_semantic; for exact symbols/code/regex, prefer search_grep.',
    '- If agentRunning=true or the task asks to run commands, fix tests, or edit repository files, prefer send_agent.',
    '- If the user asks for a diagram/chart/flow/ER/mindmap, prefer diagram.',
    '- If the user asks to run a local check/command/script over text or OS (e.g. ping, curl, HTTP status, CSV/JSON/log filtering or aggregation), prefer run_cli.',
    '- If the user asks to generate an infographic or visual slide image (not a Mermaid text diagram), prefer infographic.',
    '- Otherwise prefer ask_ai.'
  ].join('\n');

  function extractSearchQuery(rawInput) {
    const trimmed = String(rawInput || '').trim();
    if (!trimmed) return '';
    let q = trimmed
      .replace(/^(?:search\s+for|find\s+notes?\s+(?:about|on)|grep\s+for)\s+/i, '')
      .replace(/^(?:grep|rg|search|検索)\s+/i, '')
      .replace(/(?:のメモ|のノート)?(?:を)?(?:意味検索|セマンティック検索|全文検索|grep検索|検索)(?:して|する)?[。．.!！?？]*$/i, '')
      .replace(/(?:のメモ|のノート)?(?:を)?(?:探して|さがして|調べて|しらべて)[。．.!！?？]*$/i, '')
      .trim();
    return q || trimmed;
  }

  function matchFastRule(rawInput, _uiState) {
    const s = String(rawInput || '').trim();
    if (!s) return null;
    if (s.startsWith('/') || s.startsWith('@')) return null;

    if (/^(?:grep|rg)\s+\S+/i.test(s)) {
      return { route: 'search_grep', by: 'rule', query: extractSearchQuery(s) };
    }
    if (/(?:意味検索|セマンティック検索|semantic\s+search)/i.test(s)) {
      return { route: 'search_semantic', by: 'rule', query: extractSearchQuery(s) };
    }
    if (/(?:grep|完全一致).*(?:検索|探して|調べて|search|find)/i.test(s)) {
      return { route: 'search_grep', by: 'rule', query: extractSearchQuery(s) };
    }
    if (/(?:インフォグラフィック|図解画像|スライド風.*画像|画像にして|画像化して|infographic)/i.test(s)) {
      return { route: 'infographic', by: 'rule', query: '' };
    }
    if (/(?:mermaid|フローチャート|シーケンス図|構成図|er図|マインドマップ|図にして|図解して|draw\s+a\s+diagram)/i.test(s)) {
      return { route: 'diagram', by: 'rule', query: '' };
    }
    if (/(?:エージェントに|エージェントで|agentに|ターミナルで|シェルで実行|右ペインで)/i.test(s)) {
      return { route: 'send_agent', by: 'rule', query: '' };
    }
    if (/(?:\bping\b|疎通確認|疎通状態|\bcurl\b|httpステータス|ポート確認|重複除去|重複行|集計して|ワンライナー|パイプ処理|コマンド生成)/i.test(s)) {
      return { route: 'run_cli', by: 'rule', query: '' };
    }
    if (_uiState && _uiState.hasSelection && /(?:議事録|要約|箇条書き|校正|添削|整形|清書|リライト|書き直|書き換|置き換|置換|翻訳|英訳|和訳|まとめて|整えて|直して|簡潔に|丁寧に|summarize|rewrite|proofread|translate|format|minutes)/i.test(s)) {
      return { route: 'edit_note', by: 'rule', query: '' };
    }
    return null;
  }

  function getStorage(storage) {
    if (storage && typeof storage.getItem === 'function' && typeof storage.setItem === 'function') {
      return storage;
    }
    try {
      if (typeof localStorage !== 'undefined' && localStorage) return localStorage;
    } catch (_) { /* ignore */ }
    return null;
  }

  function loadHistory(storage) {
    const st = getStorage(storage);
    if (!st) return [];
    try {
      const raw = st.getItem(HISTORY_STORAGE_KEY);
      if (!raw) return [];
      const parsed = JSON.parse(raw);
      return Array.isArray(parsed) ? parsed.slice(-HISTORY_MAX) : [];
    } catch (_) {
      return [];
    }
  }

  function saveHistory(list, storage) {
    const st = getStorage(storage);
    if (!st) return;
    try {
      st.setItem(HISTORY_STORAGE_KEY, JSON.stringify(list.slice(-HISTORY_MAX)));
    } catch (_) { /* ignore quota */ }
  }

  function recordIntent(entry, storage) {
    const list = loadHistory(storage);
    const id = (entry && entry.id) || ('ih_' + Date.now().toString(36) + '_' + Math.random().toString(36).slice(2, 6));
    const item = {
      id,
      text: String((entry && entry.text) || '').trim().slice(0, 60),
      route: String((entry && entry.route) || 'ask_ai'),
      by: String((entry && entry.by) || 'decision'),
      ts: (entry && entry.ts) || Date.now()
    };
    if (entry && entry.correctedTo) item.correctedTo = String(entry.correctedTo);
    list.push(item);
    saveHistory(list.slice(-HISTORY_MAX), storage);
    return id;
  }

  function recordCorrection(id, correctedTo, storage) {
    if (!id || !VALID_ROUTES.has(correctedTo)) return false;
    const list = loadHistory(storage);
    let updated = false;
    for (let i = list.length - 1; i >= 0; i--) {
      if (list[i] && list[i].id === id) {
        list[i].correctedTo = correctedTo;
        updated = true;
        break;
      }
    }
    if (updated) saveHistory(list, storage);
    return updated;
  }

  function formatHistoryHint(history, maxItems) {
    const limit = typeof maxItems === 'number' && maxItems > 0 ? maxItems : 5;
    if (!Array.isArray(history) || history.length === 0) return '';
    const corrected = history.filter((h) => h && h.correctedTo).slice(-Math.min(3, limit));
    const recent = history.filter((h) => h && !h.correctedTo).slice(-Math.max(0, limit - corrected.length));
    const picked = corrected.concat(recent);
    return picked.map((h) => {
      const label = h.correctedTo ? `${h.correctedTo} (corrected from ${h.route})` : h.route;
      return `- "${String(h.text || '').replace(/\s+/g, ' ')}" -> ${label}`;
    }).join('\n');
  }

  function buildDecisionPayload(rawInput, uiState, history, stripSecrets) {
    const st = uiState || {};
    const sanitize = typeof stripSecrets === 'function' ? stripSecrets : (s) => String(s || '');
    const selPreview = st.selectionPreview ? sanitize(String(st.selectionPreview).replace(/\s+/g, ' ').slice(0, 60)) : '';
    const stateLine = [
      `source=${st.source || 'bar_text'}`,
      `hasSelection=${!!st.hasSelection}(${Number(st.selectionChars || 0)}ch)`,
      selPreview ? `selectionPreview="${selPreview}"` : '',
      st.activeFile ? `activeFile="${String(st.activeFile).slice(0, 40)}"` : '',
      `hasContent=${!!st.hasEditorContent}`,
      `rightPane=${st.rightPane || 'closed'}`,
      `agentRunning=${!!st.agentRunning}`
    ].filter(Boolean).join(', ');

    const histText = formatHistoryHint(history, 5);
    const parts = [`[UI State] ${stateLine}`];
    if (histText) {
      parts.push(`[Recent History]\n${sanitize(histText)}`);
    }
    parts.push(`[User Input]\n${sanitize(String(rawInput || '').trim())}`);
    return parts.join('\n\n');
  }

  async function routeIntent(opts) {
    const o = opts || {};
    const raw = String(o.input || '').trim();
    if (!o.enabled || !raw || raw.startsWith('/') || raw.startsWith('@')) {
      return { route: 'ask_ai', by: 'bypass', query: '' };
    }

    const fast = matchFastRule(raw, o.uiState);
    if (fast) {
      const historyId = recordIntent({ text: raw, route: fast.route, by: 'rule' }, o.storage);
      return { route: fast.route, by: 'rule', query: fast.query || '', historyId };
    }

    if (typeof o.queryDecision !== 'function') {
      return { route: 'ask_ai', by: 'fallback', query: '' };
    }

    const history = loadHistory(o.storage);
    const payload = buildDecisionPayload(raw, o.uiState, history, o.stripSecrets);
    let res = null;
    try {
      res = await o.queryDecision({
        input: payload,
        choices: INTENT_CHOICES,
        fallback: 'ask_ai',
        prompt: SYSTEM_HINT,
        timeoutMs: Number(o.timeoutMs) > 0 ? Number(o.timeoutMs) : 2500
      });
    } catch (_) {
      return { route: 'ask_ai', by: 'fallback', query: '' };
    }

    const chosen = res && typeof res.choiceId === 'string' && VALID_ROUTES.has(res.choiceId)
      ? res.choiceId
      : 'ask_ai';
    const by = (res && res.by) || 'decision';
    const historyId = recordIntent({ text: raw, route: chosen, by }, o.storage);
    const query = (chosen === 'search_semantic' || chosen === 'search_grep')
      ? extractSearchQuery(raw)
      : '';

    return {
      route: chosen,
      by,
      query,
      historyId,
      reason: res && res.reason
    };
  }

  return {
    HISTORY_STORAGE_KEY,
    INTENT_CHOICES,
    SYSTEM_HINT,
    extractSearchQuery,
    matchFastRule,
    loadHistory,
    recordIntent,
    recordCorrection,
    formatHistoryHint,
    buildDecisionPayload,
    routeIntent
  };
}));
