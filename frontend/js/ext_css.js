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

  function createLink(doc, href) {
    const link = doc.createElement('link');
    link.id = LINK_ID;
    link.rel = 'stylesheet';
    if (typeof link.setAttribute === 'function') {
      link.setAttribute('id', LINK_ID);
      link.setAttribute('rel', 'stylesheet');
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
