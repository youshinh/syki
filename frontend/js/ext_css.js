// Zero-overhead user.css loader and hot-reloader for syki.
// When preload.ext.hasUserCss is falsy or safeMode is active, initUserCss performs 0 DOM writes and 0 fetches.
(function (global) {
  'use strict';

  const LINK_ID = 'syki-user-css';

  function userCssHref(rev) {
    const token = (rev !== undefined && rev !== null && String(rev).trim() !== '')
      ? encodeURIComponent(String(rev))
      : '1';
    return '/__ext/r/' + token + '/user.css';
  }

  function setHref(link, href) {
    if (typeof link.setAttribute === 'function') {
      link.setAttribute('href', href);
    }
    link.href = href;
  }

  function mirrorRootVarsToBody(doc, link) {
    try {
      if (!doc || !doc.head || !link || !link.sheet || !link.sheet.cssRules) return;
      const vars = [];
      const rules = link.sheet.cssRules;
      for (let i = 0; i < rules.length; i++) {
        const rule = rules[i];
        if (!rule || !rule.selectorText || !rule.style) continue;
        if (!/(?:^|,)\s*:root(?:\s|,|$)/i.test(rule.selectorText)) continue;
        for (let j = 0; j < rule.style.length; j++) {
          const prop = rule.style[j];
          if (prop && prop.startsWith('--')) {
            const val = rule.style.getPropertyValue(prop);
            if (val) vars.push(prop + ':' + val + ';');
          }
        }
      }
      const styleId = LINK_ID + '-vars';
      let styleEl = typeof doc.getElementById === 'function' ? doc.getElementById(styleId) : null;
      if (vars.length === 0) {
        if (styleEl && styleEl.parentNode) styleEl.parentNode.removeChild(styleEl);
        return;
      }
      if (!styleEl) {
        styleEl = doc.createElement('style');
        styleEl.id = styleId;
        doc.head.appendChild(styleEl);
      }
      styleEl.textContent = 'body{' + vars.join('') + '}';
    } catch (_) {}
  }

  function createLink(doc, href) {
    const link = doc.createElement('link');
    link.id = LINK_ID;
    link.rel = 'stylesheet';
    if (typeof link.setAttribute === 'function') {
      link.setAttribute('id', LINK_ID);
      link.setAttribute('rel', 'stylesheet');
    }
    if (typeof link.addEventListener === 'function') {
      link.addEventListener('load', () => mirrorRootVarsToBody(doc, link));
    }
    setHref(link, href);
    return link;
  }

  function initUserCss(doc, preload) {
    if (!preload || !preload.ext || !preload.ext.hasUserCss || preload.ext.safeMode) {
      return null;
    }
    if (!doc || !doc.head) {
      return null;
    }
    const existing = typeof doc.getElementById === 'function' ? doc.getElementById(LINK_ID) : null;
    if (existing) {
      return existing;
    }
    const link = createLink(doc, userCssHref(1));
    doc.head.appendChild(link);
    return link;
  }

  function reloadUserCss(doc, rev) {
    if (!doc) {
      return null;
    }
    const href = userCssHref(rev);
    const existing = typeof doc.getElementById === 'function' ? doc.getElementById(LINK_ID) : null;
    if (existing) {
      setHref(existing, href);
      return existing;
    }
    if (!doc.head) {
      return null;
    }
    const link = createLink(doc, href);
    doc.head.appendChild(link);
    return link;
  }

  global.ExtCss = {
    LINK_ID,
    initUserCss,
    reloadUserCss,
  };

  if (typeof module !== 'undefined' && module.exports) {
    module.exports = global.ExtCss;
  }
})(typeof window !== 'undefined' ? window : globalThis);
