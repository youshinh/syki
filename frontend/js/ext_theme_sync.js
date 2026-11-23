// Lightweight theme snapshot sync for Win32 Quick Capture (qc-theme-cache.json).
// Pure extraction from CSS custom properties + change-detected sync gated by preload.ext.hasUserCss.
(function (global) {
  'use strict';

  const TOKEN_MAP = [
    ['bg', '--bg-modal'],
    ['field', '--bg-field'],
    ['border', '--border-color'],
    ['btn', '--bg-btn'],
    ['btnSave', '--accent-color'],
    ['text', '--text-main'],
    ['textOnSave', '--text-on-accent'],
    ['hint', '--text-muted'],
  ];

  function toHexByte(n) {
    const clamped = Math.max(0, Math.min(255, Math.round(n)));
    return clamped.toString(16).padStart(2, '0');
  }

  /**
   * Normalize a CSS color string (#rgb, #rrggbb, #rrggbbaa, rgb(), rgba()) to lowercase #rrggbb.
   * Returns null if the value cannot be normalized.
   */
  function normalizeCssColorToHex(raw) {
    if (typeof raw !== 'string') return null;
    const s = raw.trim().toLowerCase();
    if (!s) return null;

    // #rgb
    const shortHex = /^#([0-9a-f])([0-9a-f])([0-9a-f])$/.exec(s);
    if (shortHex) {
      return '#' + shortHex[1] + shortHex[1] + shortHex[2] + shortHex[2] + shortHex[3] + shortHex[3];
    }

    // #rrggbb or #rrggbbaa
    const fullHex = /^#([0-9a-f]{6})(?:[0-9a-f]{2})?$/.exec(s);
    if (fullHex) {
      return '#' + fullHex[1];
    }

    // rgb(r, g, b) / rgba(r, g, b, a) / rgb(r g b / a)
    const rgbMatch = /^rgba?\(\s*([+-]?\d+(?:\.\d+)?)\s*(?:,|\s)\s*([+-]?\d+(?:\.\d+)?)\s*(?:,|\s)\s*([+-]?\d+(?:\.\d+)?)(?:\s*(?:,|\/)\s*[+-]?\d*\.?\d+%?)?\s*\)$/.exec(s);
    if (rgbMatch) {
      const r = Number(rgbMatch[1]);
      const g = Number(rgbMatch[2]);
      const b = Number(rgbMatch[3]);
      if (Number.isFinite(r) && Number.isFinite(g) && Number.isFinite(b)) {
        return '#' + toHexByte(r) + toHexByte(g) + toHexByte(b);
      }
    }

    return null;
  }

  /**
   * Pure function: extract a lightweight #rrggbb JSON snapshot for Win32 Quick Capture
   * from a CSSStyleDeclaration-like object (supporting getPropertyValue).
   */
  function extractQcThemeSnapshot(computedStyle, isDark) {
    const snapshot = { isDark: Boolean(isDark) };
    if (!computedStyle || typeof computedStyle.getPropertyValue !== 'function') {
      return snapshot;
    }
    for (let i = 0; i < TOKEN_MAP.length; i++) {
      const key = TOKEN_MAP[i][0];
      const cssVar = TOKEN_MAP[i][1];
      const normalized = normalizeCssColorToHex(computedStyle.getPropertyValue(cssVar));
      if (normalized) {
        snapshot[key] = normalized;
      }
    }
    return snapshot;
  }

  const lastSnapshotByWin = new WeakMap();

  function detectIsDark(win) {
    const doc = win && win.document;
    const body = doc && doc.body;
    if (body && body.classList && typeof body.classList.contains === 'function') {
      return !body.classList.contains('look-paper');
    }
    const root = doc && doc.documentElement;
    if (root && root.classList && typeof root.classList.contains === 'function') {
      return !root.classList.contains('look-paper');
    }
    return true;
  }

  /**
   * Sync Quick Capture theme snapshot only when preload.ext.hasUserCss is true (and not safeMode)
   * and the extracted snapshot has changed since the last sync for this window.
   */
  function syncQcThemeIfNeeded(win, saveFn) {
    if (!win || typeof saveFn !== 'function') {
      return false;
    }
    const preload = win.__sykiPreload;
    if (!preload || !preload.ext || !preload.ext.hasUserCss || preload.ext.safeMode) {
      return false;
    }
    const doc = win.document;
    const target = (doc && (doc.body || doc.documentElement)) || null;
    if (!target || typeof win.getComputedStyle !== 'function') {
      return false;
    }
    const style = win.getComputedStyle(target);
    const snapshot = extractQcThemeSnapshot(style, detectIsDark(win));
    const serialized = JSON.stringify(snapshot);
    if (lastSnapshotByWin.get(win) === serialized) {
      return false;
    }
    lastSnapshotByWin.set(win, serialized);
    saveFn(snapshot);
    return true;
  }

  const api = {
    normalizeCssColorToHex,
    extractQcThemeSnapshot,
    syncQcThemeIfNeeded,
  };

  global.ExtThemeSync = api;
  global.SykiExtThemeSync = api;

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = api;
  }
})(typeof window !== 'undefined' ? window : globalThis);
