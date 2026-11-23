// Unit tests for ext_css.js: 0-fetch when absent, safeMode skip, initial insertion at <head> tail, and hot-reload.
const assert = require('assert');

global.window = global;
const ExtCss = require('./ext_css.js');

function makeFakeDoc(initialLinks = []) {
  const headChildren = initialLinks.map((href) => ({
    tagName: 'LINK',
    rel: 'stylesheet',
    href,
    attrs: { rel: 'stylesheet', href },
    getAttribute(k) { return this.attrs[k]; },
    setAttribute(k, v) { this.attrs[k] = v; this[k] = v; },
  }));
  let createCount = 0;
  let appendCount = 0;

  const doc = {
    head: {
      children: headChildren,
      appendChild(el) {
        appendCount += 1;
        headChildren.push(el);
        return el;
      },
    },
    createElement(tag) {
      createCount += 1;
      const el = {
        tagName: tag.toUpperCase(),
        attrs: {},
        getAttribute(k) { return this.attrs[k]; },
        setAttribute(k, v) { this.attrs[k] = v; this[k] = v; },
      };
      return el;
    },
    getElementById(id) {
      return headChildren.find((el) => el.id === id) || null;
    },
    stats() {
      return { createCount, appendCount };
    },
  };
  return doc;
}

(function testZeroFetchWhenNoUserCss() {
  for (const preload of [undefined, null, {}, { ext: {} }, { ext: { hasUserCss: false, safeMode: false } }]) {
    const doc = makeFakeDoc(['css/tokens.css', 'css/style.css', 'css/print.css']);
    const res = ExtCss.initUserCss(doc, preload);
    assert.strictEqual(res, null);
    assert.strictEqual(doc.stats().createCount, 0, 'must not create <link> when hasUserCss is falsy');
    assert.strictEqual(doc.stats().appendCount, 0, 'must not touch <head> when hasUserCss is falsy');
    assert.strictEqual(doc.head.children.length, 3);
  }
})();

(function testSafeModeSkipsInjection() {
  const doc = makeFakeDoc(['css/tokens.css', 'css/style.css', 'css/print.css']);
  const res = ExtCss.initUserCss(doc, { ext: { hasUserCss: true, safeMode: true } });
  assert.strictEqual(res, null);
  assert.strictEqual(doc.stats().createCount, 0, 'safeMode must skip creating <link>');
  assert.strictEqual(doc.stats().appendCount, 0, 'safeMode must skip appending <link>');
  assert.strictEqual(doc.head.children.length, 3);
})();

(function testInitialInsertionAtHeadTail() {
  const doc = makeFakeDoc(['css/tokens.css', 'css/style.css', 'css/print.css']);
  const link = ExtCss.initUserCss(doc, { ext: { hasUserCss: true, safeMode: false } });
  assert.ok(link, 'should return inserted <link>');
  assert.strictEqual(link.id, 'syki-user-css');
  assert.strictEqual(link.rel, 'stylesheet');
  assert.strictEqual(link.href, '/__ext/r/1/user.css');
  assert.strictEqual(doc.head.children.length, 4);
  assert.strictEqual(doc.head.children[3], link, 'must be appended after tokens.css/style.css/print.css');

  // Calling initUserCss again is idempotent.
  const second = ExtCss.initUserCss(doc, { ext: { hasUserCss: true, safeMode: false } });
  assert.strictEqual(second, link);
  assert.strictEqual(doc.head.children.length, 4);
})();

(function testHotReloadUpdatesHrefInPlace() {
  const doc = makeFakeDoc(['css/tokens.css', 'css/style.css', 'css/print.css']);
  const link = ExtCss.initUserCss(doc, { ext: { hasUserCss: true, safeMode: false } });
  assert.strictEqual(link.href, '/__ext/r/1/user.css');

  const reloaded = ExtCss.reloadUserCss(doc, 2);
  assert.strictEqual(reloaded, link, 'hot reload must reuse existing <link>');
  assert.strictEqual(reloaded.href, '/__ext/r/2/user.css');
  assert.strictEqual(reloaded.getAttribute('href'), '/__ext/r/2/user.css');
  assert.strictEqual(doc.head.children.length, 4, 'must not duplicate <link>');

  const reloaded3 = ExtCss.reloadUserCss(doc, 42);
  assert.strictEqual(reloaded3.href, '/__ext/r/42/user.css');
})();

console.log('ext_css_test: all passed');
