// Scraps Search and Meaning Search workbench module (Editor Canvas Ring 1).
// Encapsulates modal state, debounce/staleness guards, tag filters, and semantic search.
(function (global) {
  'use strict';

  function create(deps) {
    const t = deps.t;
    const getConfig = deps.getConfig;
    const escapeHtml = deps.escapeHtml;
    const showMessage = deps.showMessage;
    const oneLineFailure = deps.oneLineFailure;
    const backendErrorCode = deps.backendErrorCode;
    const getActiveEditor = deps.getActiveEditor;
    const getActiveTab = deps.getActiveTab;
    const getSearchSeed = deps.getSearchSeed;
    const insertTextWithUndo = deps.insertTextWithUndo;
    const onEditorInput = deps.onEditorInput;
    const findTabByPath = deps.findTabByPath;
    const createTab = deps.createTab;
    const selectTab = deps.selectTab;
    const gotoLineNumber = deps.gotoLineNumber;
    const flashEditorLine = deps.flashEditorLine;
    const dom = deps.dom || {};
    const isMac = typeof deps.isMac === 'function'
      ? !!deps.isMac()
      : (deps.isMac !== undefined
        ? !!deps.isMac
        : ((typeof window !== 'undefined' && window.SykiPlatform)
          ? !!window.SykiPlatform.isMac
          : (typeof navigator !== 'undefined' && /Mac|iPhone|iPod|iPad/i.test(navigator.platform || navigator.userAgent))));
    const scrapsSearchModal = dom.scrapsSearchModal || null;
    const scrapsSearchInput = dom.scrapsSearchInput || null;
    const scrapsSearchResults = dom.scrapsSearchResults || null;
    const btnSearchScraps = dom.btnSearchScraps || null;

    // --- Feature 4: Ultra-fast Scraps In-Memory Search Modal (Ctrl+Shift+F) ---
    let scrapsSearchDebounceTimer = null;
    let scrapsSearchFlattened = [];
    let scrapsSearchSelectedIndex = 0;
    // The editor that had the caret when the search opened: Tab quotes the chosen line into it.
    let scrapsSearchTarget = null;
    // Numbers the searches, so a slow answer to an earlier query never replaces a newer one.
    let scrapsSearchSeq = 0;
    // True from an edit of the query until the answer for it is shown: the list on screen answers an older query, so Enter / Tab
    // must not open one of its lines (the query box says something else).
    let scrapsSearchStale = false;
    // Meaning search: inert until Settings > Semantic search is on.
    const SCRAPS_MODE_KEY = 'syki_scraps_search_mode'; // sessionStorage: the choice lasts for this session only
    const SCRAPS_MORE_LIMIT = 30;       // "Show more" asks for this many notes (the first ask is 10)
    let scrapsSemanticWired = false;
    let scrapsSearchMode = 'exact';     // 'exact' | 'meaning'
    let scrapsSearchModeLoaded = false;
    let scrapsSearchLimit = 10;         // how many notes the meaning search asks for: 10, then SCRAPS_MORE_LIMIT
    let scrapsStatusShown = false;
    // The filter row (a period and tags that narrow every search; frontend/js/scraps_filter.js). Nothing of it is built until it is opened.
    const scrapsFilterBox = typeof document !== 'undefined' ? document.getElementById('scraps-filter') : null;
    let scrapsFilterState = null;       // { period, tags }: made when the row first shows, kept while the app is open (not saved)
    let scrapsFilterWired = false;
    let scrapsFilterAreaOpen = false;
    let scrapsFilterLoad = { status: 'idle', options: null, message: '' }; // the tag list of this opening of the panel
    let scrapsFilterLoadSeq = 0;
    let scrapsSearchFilterSent = null;  // the filter (object, or null) the list on screen was searched with

    function runScrapsSearch(q) {
      if (scrapsMeaningMode()) { runScrapsSemanticSearch(q); return; }
      const seq = ++scrapsSearchSeq;
      if (!(window.backend && window.backend.searchScraps)) return;
      Promise.resolve(window.backend.searchScraps(q, 100, scrapsFilterForSearch())).then((results) => {
        if (seq === scrapsSearchSeq) renderScrapsSearchResults(results || []);
      }).catch((err) => {
        console.error('searchScraps failed:', err);
        if (seq === scrapsSearchSeq) scrapsSearchStale = false;
      });
    }

    // The query or the filter was changed: the list on screen answers something else until the answer for the new one is shown. It is asked for
    // once the changing has paused (the meaning search embeds the text first, so it waits a little longer).
    function queueScrapsSearch(q) {
      scrapsSearchStale = true;
      if (scrapsSemanticWired) scrapsSearchTyped();
      clearTimeout(scrapsSearchDebounceTimer);
      scrapsSearchDebounceTimer = setTimeout(() => runScrapsSearch(q), scrapsMeaningMode() ? 400 : 150);
    }

    // Also the toolbar button's click handler, so it takes no argument.
    function openScrapsSearchModal() {
      if (!scrapsSearchModal) return;
      // Before focus moves into the search box: the selection, or the word before the caret, is the first query.
      const seed = getSearchSeed(true);
      scrapsSearchTarget = getActiveEditor();
      scrapsSearchModal.classList.remove('hidden');
      scrapsSearchSelectedIndex = 0;
      scrapsSearchFlattened = [];
      clearTimeout(scrapsSearchDebounceTimer);
      scrapsSearchSeq++;
      openScrapsSearchExtras();
      openScrapsFilterRow();
      if (scrapsSearchInput) {
        scrapsSearchInput.value = seed;
        setTimeout(() => {
          scrapsSearchInput.focus();
          scrapsSearchInput.select();
        }, 40);
      }
      renderScrapsSearchResults([]);
      if (seed) runScrapsSearch(seed);
    }

    // Tab in the search: put the chosen line into the note at the caret (replacing a selection, like a paste,
    // and undoable with Ctrl+Z) instead of opening its file.
    function quoteScrapLine(item) {
      const text = window.ScrapQuote.quoteText(item && item.match);
      if (!text) return;
      const editor = scrapsSearchTarget;
      // The preview covering the editor: there is no visible caret to insert at.
      if (!editor || editor.getClientRects().length === 0) {
        showMessage(t('scrapsSearchNoEditor'), 3000);
        return;
      }
      closeScrapsSearchModal();
      insertTextWithUndo(text, editor);
      const tab = getActiveTab();
      if (tab) onEditorInput(editor, tab, true);
    }

    function closeScrapsSearchModal() {
      if (!scrapsSearchModal) return;
      if (scrapsSemanticWired) closeScrapsSearchExtras();
      scrapsSearchModal.classList.add('hidden');
      const editor = getActiveEditor();
      if (editor) editor.focus();
    }

    if (scrapsSearchModal) {
      scrapsSearchModal.addEventListener('click', (e) => {
        if (e.target === scrapsSearchModal) {
          closeScrapsSearchModal();
        }
      });
    }

    if (btnSearchScraps) {
      btnSearchScraps.onclick = openScrapsSearchModal;
    }

    if (scrapsSearchInput) {
      scrapsSearchInput.addEventListener('input', () => {
        const q = scrapsSearchInput.value.trim();
        clearTimeout(scrapsSearchDebounceTimer);
        if (!q) {
          scrapsSearchFlattened = [];
          if (scrapsMeaningMode()) scrapsSearchSeq++; // a slow meaning answer to the text that was just erased must not bring its list back
          if (scrapsSemanticWired) scrapsSearchTyped();
          renderScrapsSearchResults([]);
          return;
        }
        queueScrapsSearch(q);
      });

      scrapsSearchInput.addEventListener('keydown', (e) => {
        // While an IME composition is open, Enter / Tab / arrows / Esc belong to the IME (confirming a
        // conversion must neither jump to a result nor insert one).
        if (e.isComposing || e.keyCode === 229) return;
        if (e.key === 'Escape') {
          e.preventDefault();
          e.stopPropagation(); // one Esc closes one panel (see the ask bar)
          closeScrapsSearchModal();
        } else if (e.key === 'ArrowDown') {
          e.preventDefault();
          if (scrapsSearchFlattened.length > 0) {
            scrapsSearchSelectedIndex = (scrapsSearchSelectedIndex + 1) % scrapsSearchFlattened.length;
            updateScrapsSearchSelection();
          }
        } else if (e.key === 'ArrowUp') {
          e.preventDefault();
          if (scrapsSearchFlattened.length > 0) {
            scrapsSearchSelectedIndex = (scrapsSearchSelectedIndex - 1 + scrapsSearchFlattened.length) % scrapsSearchFlattened.length;
            updateScrapsSearchSelection();
          }
        } else if (e.key === 'Enter' && !e.shiftKey) {
          e.preventDefault();
          if (!scrapsSearchStale && scrapsSearchFlattened.length > 0 && scrapsSearchFlattened[scrapsSearchSelectedIndex]) {
            const item = scrapsSearchFlattened[scrapsSearchSelectedIndex];
            jumpToScrap(item.filePath, item.fileName, item.match.lineNumber);
            closeScrapsSearchModal();
          }
        } else if ((e.key === 'Tab' && !e.shiftKey) || (e.key === 'Enter' && e.shiftKey)) {
          // Tab (or Shift+Enter): quote the chosen line into the note. Shift+Tab keeps its usual meaning.
          // stopPropagation: focus moves to the editor below, and the key must not then reach the
          // document-level key handlers as if it had been pressed there (Shift+Enter would add a line break).
          e.preventDefault();
          e.stopPropagation();
          if (!scrapsSearchStale && scrapsSearchFlattened.length > 0 && scrapsSearchFlattened[scrapsSearchSelectedIndex]) {
            quoteScrapLine(scrapsSearchFlattened[scrapsSearchSelectedIndex]);
          }
        }
      });
    }

    // view (the meaning search only): { notes: [the backend's sentences], truncated, limit }. The same list is drawn; the notes go above it as
    // quiet lines, a match shows the lines it spans, and "Show more" ends it when the backend had more. A score is never shown.
    function renderScrapsSearchResults(results, view) {
      scrapsSearchFlattened = [];
      scrapsSearchSelectedIndex = 0;
      scrapsSearchStale = false;

      results.forEach(res => {
        if (res.matches) {
          res.matches.forEach(m => {
            const normMatch = Object.assign({}, m, {
              lineNumber: m.lineNumber != null ? m.lineNumber : (m.line != null ? m.line : 1),
              lineText: m.lineText != null ? m.lineText : (m.text != null ? m.text : '')
            });
            scrapsSearchFlattened.push({
              filePath: res.filePath,
              fileName: res.fileName,
              match: normMatch
            });
          });
        }
      });
      if (!scrapsSearchResults) return;
      // Meaning search: the counts the backend sends as numbers are said here, in the UI language; its own notes (English: what has no
      // count, such as why the model could not answer) follow them.
      const noteTexts = [];
      if (view) {
        if (view.semantic === false) noteTexts.push(t('scrapsSearchWordsFallback'));
        if (view.pending > 0) noteTexts.push(t('scrapsSearchPending', { n: view.pending }));
        if (view.leftOut > 0) noteTexts.push(t('scrapsSearchLeftOut', { n: view.leftOut }));
        (view.notes || []).forEach((n) => noteTexts.push(n));
      }
      const notesHtml = noteTexts.map((n) => `<div class="scraps-search-note">${escapeHtml(n)}</div>`).join('');

      if (scrapsSearchFlattened.length === 0) {
        const q = scrapsSearchInput ? scrapsSearchInput.value.trim() : '';
        // With a filter chosen the list is empty because of it: say so (the key is the filter module's decision)
        const emptyKey = window.ScrapsFilter ? window.ScrapsFilter.emptyMessageKey(scrapsSearchFilterSent, !!q) : q ? 'scrapsSearchNoResults' : 'scrapsSearchEmpty';
        scrapsSearchResults.innerHTML = `${notesHtml}<div class="scraps-search-empty">${escapeHtml(t(emptyKey))}</div>`;
        return;
      }

      scrapsSearchResults.innerHTML = scrapsSearchFlattened.map((item, idx) => {
        const isSelected = idx === 0 ? 'active' : '';
        const previewText = item.match.snippet || item.match.lineText;
        const endLine = item.match.endLine;
        const lines = view && endLine > item.match.lineNumber ? `${item.match.lineNumber}–${endLine}` : String(item.match.lineNumber);
        const heading = view && item.match.heading ? ` title="${escapeHtml(item.match.heading)}"` : '';
        return `
          <div class="scraps-match-item ${isSelected}" data-idx="${idx}"${heading}>
            <div class="scraps-match-header">
              <span class="scraps-match-file">${escapeHtml(item.fileName)}</span>
              <span class="scraps-match-line">Ln ${lines}</span>
            </div>
            <div class="scraps-match-snippet">${escapeHtml(previewText)}</div>
          </div>
        `;
      }).join('');

      if (view) {
        // Meaning search: its own notes (cut-off count, a switch to words, files not indexed yet) instead of the word search's label
        if (notesHtml) scrapsSearchResults.insertAdjacentHTML('afterbegin', notesHtml);
        if (view.truncated && view.limit < SCRAPS_MORE_LIMIT) {
          scrapsSearchResults.insertAdjacentHTML('beforeend',
            `<div class="scraps-search-more"><button type="button" id="btn-scraps-more" class="btn-secondary">${escapeHtml(t('scrapsSearchMore'))}</button></div>`);
          const more = document.getElementById('btn-scraps-more');
          if (more) more.onclick = showMoreScrapsMeaning;
        }
      }

      // No line holds the whole text: the backend listed the notes that hold its words, best first (a match then carries a score).
      // Say so, so that the list is not read as "lines that contain what I typed".
      if (!view && scrapsSearchFlattened.some((item) => item.match && item.match.score > 0)) {
        const everyOnePartial = scrapsSearchFlattened.every((item) => item.match && item.match.partial);
        scrapsSearchResults.insertAdjacentHTML('afterbegin',
          `<div class="scraps-search-note">${escapeHtml(t(everyOnePartial ? 'scrapsSearchPartialNote' : 'scrapsSearchRankedNote'))}</div>`);
      }

      scrapsSearchResults.querySelectorAll('.scraps-match-item').forEach(el => {
        el.onclick = () => {
          const idx = parseInt(el.getAttribute('data-idx'), 10);
          if (!isNaN(idx) && scrapsSearchFlattened[idx]) {
            const item = scrapsSearchFlattened[idx];
            jumpToScrap(item.filePath, item.fileName, item.match.lineNumber);
            closeScrapsSearchModal();
          }
        };
      });
    }

    function updateScrapsSearchSelection() {
      if (!scrapsSearchResults) return;
      const items = scrapsSearchResults.querySelectorAll('.scraps-match-item');
      items.forEach((el, idx) => {
        const isActive = idx === scrapsSearchSelectedIndex;
        el.classList.toggle('active', isActive);
        if (isActive) {
          el.scrollIntoView({ block: 'nearest' });
        }
      });
    }

    // ---- The filter row (docs/design/tag-filter-2026-10.md section 5) ----------------------------------------------------------------
    // A period and tags that narrow the exact search, the meaning search and the Deep search alike. The row is there when the backend can list
    // the tags (an older one has no scrapFilterOptions: no row, every search goes as it always did). The area under it is built and the tags
    // are asked for when the person opens it, at most once per opening of the panel; a person who never opens it pays one click handler.

    function scrapsFilterSupported() {
      return !!(window.ScrapsFilter && scrapsFilterBox && window.backend && typeof window.backend.scrapFilterOptions === 'function');
    }

    // The filter of the search that is starting: an object, or null when nothing is chosen. Remembered as what the list on screen is for, so
    // that Deep search and the empty-list sentence speak of the same filter (the period is worked out now, from the clock).
    function scrapsFilterForSearch() {
      scrapsSearchFilterSent = scrapsFilterState ? window.ScrapsFilter.toRequest(scrapsFilterState, new Date()) : null;
      return scrapsSearchFilterSent;
    }

    // Each time the panel opens: the row shows (or not), the area starts closed, and the tags are asked for again if it is opened.
    function openScrapsFilterRow() {
      scrapsSearchFilterSent = null;
      scrapsFilterLoadSeq++; // an answer to an earlier opening is not wanted
      scrapsFilterLoad = { status: 'idle', options: null, message: '' };
      scrapsFilterAreaOpen = false;
      if (!scrapsFilterSupported()) {
        scrapsFilterState = null;
        if (scrapsFilterBox) scrapsFilterBox.classList.add('hidden');
        return;
      }
      if (!scrapsFilterState) scrapsFilterState = window.ScrapsFilter.create();
      if (!scrapsFilterWired) {
        scrapsFilterWired = true;
        scrapsFilterBox.addEventListener('click', onScrapsFilterClick);
      }
      scrapsFilterBox.classList.remove('hidden');
      paintScrapsFilter();
    }

    // The button (its count, lit while something is chosen, open or closed) and the area, brought in step with the state.
    function paintScrapsFilter() {
      const F = window.ScrapsFilter;
      const label = F.countLabel(scrapsFilterState);
      const btn = document.getElementById('btn-scraps-filter');
      if (btn) {
        btn.classList.toggle('on', label !== '');
        btn.setAttribute('aria-expanded', scrapsFilterAreaOpen ? 'true' : 'false');
      }
      const count = document.getElementById('scraps-filter-count');
      if (count) count.textContent = label;
      const area = document.getElementById('scraps-filter-area');
      if (!area) return;
      area.classList.toggle('hidden', !scrapsFilterAreaOpen);
      if (scrapsFilterAreaOpen) {
        area.innerHTML = F.areaHtml({ state: scrapsFilterState, status: scrapsFilterLoad.status, options: scrapsFilterLoad.options, message: scrapsFilterLoad.message }, t);
      }
    }

    function toggleScrapsFilterArea() {
      scrapsFilterAreaOpen = !scrapsFilterAreaOpen;
      if (scrapsFilterAreaOpen && scrapsFilterLoad.status === 'idle') loadScrapsFilterOptions();
      else paintScrapsFilter();
    }

    // {tags: [{tag, files, entries}], files, undated} from the backend, once per opening of the panel (a failure stays until the next one).
    function loadScrapsFilterOptions() {
      const seq = ++scrapsFilterLoadSeq;
      scrapsFilterLoad = { status: 'loading', options: null, message: '' };
      paintScrapsFilter();
      let call;
      try {
        call = Promise.resolve(window.backend.scrapFilterOptions());
      } catch (err) {
        call = Promise.reject(err);
      }
      call.then((raw) => {
        if (seq !== scrapsFilterLoadSeq) return;
        let answer = raw;
        if (typeof answer === 'string') {
          try { answer = JSON.parse(answer); } catch (e) { answer = null; }
        }
        scrapsFilterLoad = { status: 'ready', options: window.ScrapsFilter.normalizeOptions(answer), message: '' };
        paintScrapsFilter();
      }, (err) => {
        if (seq !== scrapsFilterLoadSeq) return;
        scrapsFilterLoad = { status: 'failed', options: null, message: oneLineFailure(err, false) };
        paintScrapsFilter();
      });
    }

    // One handler for the whole row and area. The buttons are out of the Tab order (Tab in the box quotes the line) and take no keys, so after
    // any click the caret goes back to the box, where Tab, Enter and Esc mean what they always did.
    function onScrapsFilterClick(e) {
      const el = e.target && e.target.closest ? e.target.closest('[data-filter-toggle],[data-filter-period],[data-filter-tag],[data-filter-clear]') : null;
      if (scrapsSearchInput) scrapsSearchInput.focus();
      if (!el || el.disabled) return;
      if (el.hasAttribute('data-filter-toggle')) {
        toggleScrapsFilterArea();
        return;
      }
      const F = window.ScrapsFilter;
      const before = scrapsFilterState;
      if (el.hasAttribute('data-filter-period')) scrapsFilterState = F.setPeriod(before, el.getAttribute('data-filter-period'));
      else if (el.hasAttribute('data-filter-tag')) scrapsFilterState = F.toggleTag(before, el.getAttribute('data-filter-tag'));
      else if (F.isActive(before)) scrapsFilterState = F.clear();
      if (scrapsFilterState !== before) onScrapsFilterChanged();
    }

    // The filter changed: search again, by the same road as typing.
    function onScrapsFilterChanged() {
      paintScrapsFilter();
      const q = scrapsSearchInput ? scrapsSearchInput.value.trim() : '';
      if (!q) return;
      scrapsSearchSeq++; // an answer still on its way was searched with the old filter
      queueScrapsSearch(q);
    }

    // ---- Meaning search ("Exact | Meaning" in the notes search) -------------------------------------------------------------------

    function scrapsSemanticEnabled() {
      return !!(getConfig().semantic && getConfig().semantic.enabled === true);
    }

    // True while the panel searches by meaning. For a person who never turned the feature on this is one comparison.
    function scrapsMeaningMode() {
      return scrapsSearchMode === 'meaning' && scrapsSemanticEnabled();
    }

    // Called each time the panel opens. With Semantic search off it reads one setting and shows nothing.
    function openScrapsSearchExtras() {
      scrapsSearchLimit = 10;
      if (!scrapsSemanticEnabled()) {
        if (scrapsSemanticWired) { // it was on and has been turned off: the plain panel again
          scrapsSearchMode = 'exact';
          paintScrapsSearchMode();
        }
        return;
      }
      wireScrapsSemanticUi();
      if (!scrapsSearchModeLoaded) {
        scrapsSearchModeLoaded = true;
        try {
          if (sessionStorage.getItem(SCRAPS_MODE_KEY) === 'meaning') scrapsSearchMode = 'meaning';
        } catch (e) { /* no storage: the panel starts in Exact, as it always does */ }
      }
      setScrapsSearchStatus('');
      paintScrapsSearchMode();
    }

    function closeScrapsSearchExtras() {
      setScrapsSearchStatus('');
    }

    function wireScrapsSemanticUi() {
      if (scrapsSemanticWired) return;
      scrapsSemanticWired = true;
      const on = (id, fn) => {
        const el = document.getElementById(id);
        if (el) el.addEventListener('click', fn);
      };
      on('scraps-mode-exact', () => setScrapsSearchMode('exact'));
      on('scraps-mode-meaning', () => setScrapsSearchMode('meaning'));
      // Esc on one of the panel's buttons closes the panel, as it does in the box
      if (scrapsSearchModal) {
        scrapsSearchModal.addEventListener('keydown', (e) => {
          if (e.key !== 'Escape' || e.isComposing) return;
          e.preventDefault();
          e.stopPropagation();
          closeScrapsSearchModal();
        });
      }
    }

    // Shows or hides the switch, and brings the label, hint and placeholder in step with the mode.
    function paintScrapsSearchMode() {
      const bar = document.getElementById('scraps-search-mode');
      if (!bar) return;
      const enabled = scrapsSemanticEnabled();
      const meaning = enabled && scrapsSearchMode === 'meaning';
      const mod = isMac ? 'Cmd' : 'Ctrl';
      bar.classList.toggle('hidden', !enabled);
      bar.setAttribute('aria-label', t('scrapsModeLabel'));
      [['scraps-mode-exact', !meaning], ['scraps-mode-meaning', meaning]].forEach(([id, active]) => {
        const btn = document.getElementById(id);
        if (!btn) return;
        btn.classList.toggle('active', active);
        btn.setAttribute('aria-pressed', active ? 'true' : 'false');
      });
      if (scrapsSearchInput) scrapsSearchInput.placeholder = t(meaning ? 'scrapsSearchPlaceholderMeaning' : 'scrapsSearchPlaceholder');
      const hint = document.getElementById('scraps-search-hint');
      if (hint) hint.textContent = meaning ? t('scrapsSearchHintMeaning', { mod: mod }) : t('scrapsSearchHint');
    }

    function setScrapsSearchMode(mode) {
      const next = mode === 'meaning' ? 'meaning' : 'exact';
      if (scrapsSearchInput) scrapsSearchInput.focus();
      if (next === scrapsSearchMode) return;
      scrapsSearchMode = next;
      try {
        sessionStorage.setItem(SCRAPS_MODE_KEY, next);
      } catch (e) { /* the choice is just not remembered */ }
      scrapsSearchLimit = 10;
      clearTimeout(scrapsSearchDebounceTimer);
      scrapsSearchSeq++; // an answer still on its way belongs to the other kind of search
      setScrapsSearchStatus('');
      paintScrapsSearchMode();
      scrapsSearchFlattened = [];
      scrapsSearchSelectedIndex = 0;
      const q = scrapsSearchInput ? scrapsSearchInput.value.trim() : '';
      if (!q) {
        renderScrapsSearchResults([]);
        return;
      }
      if (scrapsSearchResults) scrapsSearchResults.innerHTML = ''; // the other kind's list (and its "Show more") must not stay
      scrapsSearchStale = true;
      runScrapsSearch(q);
    }

    function scrapsSearchTyped() {
      scrapsSearchLimit = 10;
      setScrapsSearchStatus('');
    }

    // The quiet line under the header: a failure of Deep search, or the way to set the AI model up (action: { label, run }). '' clears it.
    function setScrapsSearchStatus(text, action) {
      if (!text && !scrapsStatusShown) return;
      const box = document.getElementById('scraps-search-status');
      if (!box) return;
      const label = document.getElementById('scraps-search-status-text');
      const btn = document.getElementById('btn-scraps-status-action');
      scrapsStatusShown = !!text;
      if (label) label.textContent = text || '';
      box.classList.toggle('hidden', !text);
      if (btn) {
        btn.classList.toggle('hidden', !(text && action));
        btn.textContent = text && action ? action.label : '';
        btn.onclick = text && action ? action.run : null;
      }
    }

    function runScrapsSemanticSearch(q) {
      const seq = ++scrapsSearchSeq;
      const limit = scrapsSearchLimit;
      if (!(window.backend && window.backend.searchScrapsSemantic)) {
        showScrapsMeaningFailure(t('scrapsSearchMeaningUnavailable'));
        return;
      }
      // Embedding the text takes a moment: with no list on screen yet, say what is going on (an older list stays, flagged stale)
      if (scrapsSearchResults && scrapsSearchFlattened.length === 0) {
        scrapsSearchResults.innerHTML = `<div class="scraps-search-empty">${escapeHtml(t('scrapsSearchBusy'))}</div>`;
      }
      let call;
      try {
        call = Promise.resolve(window.backend.searchScrapsSemantic(q, limit, scrapsFilterForSearch()));
      } catch (err) {
        call = Promise.reject(err);
      }
      call.then((raw) => {
        if (seq !== scrapsSearchSeq) return;
        let answer = raw;
        if (typeof answer === 'string') {
          try { answer = JSON.parse(answer); } catch (e) { answer = null; }
        }
        if (!answer || typeof answer !== 'object') answer = {};
        renderScrapsSearchResults(Array.isArray(answer.results) ? answer.results : [], {
          notes: (Array.isArray(answer.notes) ? answer.notes : []).filter((n) => typeof n === 'string' && n.trim() !== ''),
          truncated: answer.truncated === true,
          semantic: answer.semantic !== false,
          pending: Number(answer.pending) || 0,
          leftOut: Number(answer.leftOut) || 0,
          limit: limit
        });
      }).catch((err) => {
        if (seq !== scrapsSearchSeq) return;
        // "superseded": the backend dropped this search for a newer one, whose answer is the one to show. No sentence for that.
        if (backendErrorCode(err) === 'superseded') return;
        showScrapsMeaningFailure(t('scrapsSearchMeaningFailed', { message: oneLineFailure(err, false) }));
      });
    }

    // The meaning search could not answer (the feature is off, a cloud host is not allowed, no model ...): the message on one line in
    // place of the list. The switch stays, so the person can go back to Exact.
    function showScrapsMeaningFailure(text) {
      scrapsSearchFlattened = [];
      scrapsSearchSelectedIndex = 0;
      scrapsSearchStale = false;
      if (scrapsSearchResults) {
        scrapsSearchResults.innerHTML = `<div class="scraps-search-empty scraps-search-error" role="alert">${escapeHtml(text)}</div>`;
      }
    }

    function showMoreScrapsMeaning() {
      const q = scrapsSearchInput ? scrapsSearchInput.value.trim() : '';
      if (!q || !scrapsMeaningMode()) return;
      scrapsSearchLimit = SCRAPS_MORE_LIMIT;
      scrapsSearchStale = true; // the list on screen is the shorter one until the longer one arrives
      if (scrapsSearchInput) scrapsSearchInput.focus();
      runScrapsSemanticSearch(q);
    }

    global.__onSemanticIdleProgress = function (p) {
      if (!p || !scrapsSearchModal || scrapsSearchModal.classList.contains('hidden')) return;
      if (!scrapsMeaningMode()) return;
      const statusTextEl = document.getElementById('scraps-search-status-text');
      if (p.running) {
        setScrapsSearchStatus(t('semanticIdleProgress', { files: String(p.filesDone || 0), filesTotal: String(p.filesTotal || 0) }));
      } else if (statusTextEl && statusTextEl.textContent && statusTextEl.textContent.indexOf('/') !== -1) {
        setScrapsSearchStatus('');
      }
    };

    async function jumpToScrap(filePath, fileName, lineNumber) {
      // The tab that shows THIS file (same path), never another note that only has the same name: the date-named scrap exists in every folder.
      let targetTab = findTabByPath(filePath);
      if (!targetTab) {
        let content = '';
        if (window.backend && window.backend.readFileByPath) {
          try {
            const res = await window.backend.readFileByPath(filePath);
            if (res) content = res.content;
          } catch (e) {
            console.warn('Failed to read scrap file:', e);
          }
        }
        targetTab = createTab(fileName, content, filePath);
        targetTab.isAutoTitle = false;
        targetTab.isScrap = true;
      }
      selectTab(targetTab.id);

      setTimeout(() => {
        gotoLineNumber(lineNumber);
        flashEditorLine(lineNumber);
      }, 60);
    }

    function openForRpc(opts, failFn) {
      const query = opts && typeof opts.query === 'string' ? opts.query.trim() : '';
      const mode = opts && (opts.mode === 'exact' || opts.mode === 'meaning') ? opts.mode : '';
      if (mode === 'meaning' && !scrapsSemanticEnabled()) {
        if (typeof failFn === 'function') {
          failFn('invalid_params', 'the meaning search is off: turn it on in Settings > AI Models > Semantic search first');
        }
        return null;
      }
      openScrapsSearchModal();
      if (!query && !mode) return null;
      if (mode && mode !== scrapsSearchMode) {
        if (scrapsSearchInput) scrapsSearchInput.value = query; // setScrapsSearchMode runs the search for what the box holds
        setScrapsSearchMode(mode);
        if (query) return mode;
      }
      if (query && scrapsSearchInput) {
        scrapsSearchInput.value = query;
        scrapsSearchStale = true;
        runScrapsSearch(query);
      }
      return scrapsMeaningMode() ? 'meaning' : 'exact';
    }

    return {
      open: openScrapsSearchModal,
      close: closeScrapsSearchModal,
      setMode: setScrapsSearchMode,
      openForRpc: openForRpc,
      jumpToScrap: jumpToScrap
    };
  }

  const api = { create: create };
  global.SykiSearchWorkbench = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
