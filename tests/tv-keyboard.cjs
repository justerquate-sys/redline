// TV keyboard regressions. Run by e2e.cjs with its signed-out fixtures and remote helpers.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');

module.exports = async ({ step, tvOpen, tvClose, tvGo, tvAudit, tvShot, back, TVSIZES, tvWalk, incidents, telBatches }) => {
  await step('TV search keyboard: remote-only text entry, edit/symbol keys, Back/reopen, suggestions and physical typing work with the 0.3.0 keyboard shim', async () => {
    // Existing installed shells carry this listener even after a client bundle update. Install the
    // real listener, with no OS keyboard behind it, so tests cannot accidentally rely on an IME.
    const shim = fs.readFileSync(path.join(__dirname, '../tv/shim/shim.js'), 'utf8');
    const start = shim.indexOf('  const textual =');
    const end = shim.indexOf('  ytcClient();', start);
    assert.ok(start > 0 && end > start, 'the installed-shell keyboard shim is available');
    const keyboardShim = shim.slice(start, end);
    for (const [W, H] of TVSIZES) {
      const tv = await tvOpen();
      try {
        await tv.setViewportSize({ width: W, height: H });
        await tv.addInitScript(({ keyboardShim, available }) => {
          window.__tvImeCalls = { show: 0, hide: 0 };
          Object.defineProperty(navigator, 'virtualKeyboard', { configurable: true, value: available ? {
            show() { window.__tvImeCalls.show++; }, hide() { window.__tvImeCalls.hide++; },
          } : undefined });
          (0, eval)(`(() => { ${keyboardShim} })()`);
        }, { keyboardShim, available: W === 1920 });
        await tvGo(tv, 'code', 'https://www.youtube.com/');
        await tv.waitForSelector('#ytc-side .rl[data-nav=search]');
        await tv.focus('#ytc-side .rl[data-nav=search]');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.activeElement?.matches('#ytc-tvk .tvk-keys button'));
        const query = () => tv.inputValue('#ytc-q');
        const key = async (...keys) => { for (const k of keys) await tv.keyboard.press(k); };
        const focused = () => tv.evaluate(() => document.activeElement?.getAttribute('data-arg') || document.activeElement?.getAttribute('data-act'));
        assert.equal(await tv.getAttribute('#ytc-q', 'type'), 'hidden', 'the old shell never targets an editable field');
        assert.equal(await query(), '');
        assert.deepEqual(await tvAudit(tv, `keyboard ${W}`), []);
        await tvShot(tv, 'search-keyboard');

        // Only D-pad and OK from here through the submitted query: a → c → a → t.
        await key('ArrowRight', 'ArrowRight');
        assert.equal(await focused(), 'c');
        await key('Enter', 'ArrowLeft', 'ArrowLeft', 'Enter');
        await key('ArrowDown', 'ArrowDown', 'ArrowDown', 'ArrowRight');
        assert.equal(await focused(), 't');
        await key('Enter');
        assert.equal(await query(), 'cat');
        // Last alphabet row → Space → mode → Search, all reachable with the remote.
        await key('ArrowDown', 'ArrowDown');
        assert.equal(await focused(), ' ');
        await key('Enter');
        assert.equal(await query(), 'cat ');
        await key('ArrowRight', 'Enter');
        assert.equal(await query(), 'cat', 'Delete removes the last character');
        await key('ArrowDown');
        assert.equal(await focused(), 'tv-kb-search');
        await key('Enter');
        await tv.waitForFunction(() => location.pathname === '/results' && new URLSearchParams(location.search).get('search_query') === 'cat');
        await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'));
        assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '');
        assert.equal(await tv.getAttribute('#ytc-tvk', 'aria-hidden'), 'true');

        // The results query remains editable; Back dismisses the keyboard before the Search screen.
        await key('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvquery');
        assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '1');
        await key('Enter');
        assert.equal(await tv.getAttribute('#ytc-tvk', 'aria-hidden'), 'false');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '1');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvquery');
        assert.equal(await query(), 'cat');
        await key('Enter');
        assert.equal(await tv.getAttribute('#ytc-tvk', 'aria-hidden'), 'false', 'OK reopens it every time');

        const activate = async selector => { await tv.focus(selector); await tv.keyboard.press('Enter'); };
        await activate('#ytc-tvk [data-act=tv-kb-clear]');
        assert.equal(await query(), '');
        const before = tv.url();
        await activate('#ytc-tvk [data-act=tv-kb-search]');
        assert.equal(tv.url(), before, 'an empty search stays on the screen');
        await activate('#ytc-tvk [data-act=tv-kb-layout]');
        assert.equal(await tv.locator('#ytc-tvk .tvk-keys button').first().textContent(), '0');
        await activate('#ytc-tvk .tvk-keys button[data-arg="1"]');
        assert.equal(await query(), '1');
        await activate('#ytc-tvk [data-act=tv-kb-delete]');
        await activate('#ytc-tvk [data-act=tv-kb-layout]');
        assert.equal(await tv.locator('#ytc-tvk .tvk-keys button').first().textContent(), 'a');
        const walk = await tvWalk(tv, '#ytc-tvquery,#ytc-tvk button', 90);
        assert.deepEqual(walk.missing, [], `keyboard controls reachable ${W}`);
        assert.deepEqual(walk.dead, [], `keyboard has no traps ${W}`);

        // Tab and Shift+Tab wrap inside Search, including the visible suggestions, in both directions.
        await tv.focus('#ytc-tvquery');
        await key('Shift+Tab');
        assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-search') && document.activeElement.id !== 'ytc-tvquery'));
        await key('Tab');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvquery', 'forward wrap returns to query');
        await key('Tab');
        assert.equal(await focused(), 'a');
        await key('Shift+Tab');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvquery', 'backward traversal returns to query');

        // Physical text, including Space, goes through the same bounded Rust edits.
        await tv.focus('#ytc-tvk .tvk-keys button');
        await tv.keyboard.type('quiet rain');
        assert.equal(await query(), 'quiet rain', 'Space is text, not activation of the focused letter');
        await key('Backspace');
        assert.equal(await query(), 'quiet rai');
        await key('n');
        await tv.waitForSelector('#ytc-sug .sg:not(.hs)', { timeout: 5000 });
        // Moving to another control ends typing mode: Enter activates it instead of submitting.
        await tv.focus('#ytc-tvquery');
        await key('Tab', 'Enter');
        assert.equal(await query(), 'quiet raina', 'Tab then Enter activates the focused a key');
        assert.equal(tv.url(), before, 'Tab did not submit the query');
        await key('Backspace');
        await key('Enter');
        await tv.waitForFunction(() => location.pathname === '/results' && new URLSearchParams(location.search).get('search_query') === 'quiet rain');
        await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'));

        // Choosing a suggestion also closes both surfaces and returns focus to the results.
        await key('ArrowUp', 'Enter');
        await activate('#ytc-tvk [data-act=tv-kb-clear]');
        await back(tv);
        assert.equal(await query(), '', 'Back preserves an intentionally cleared query');
        assert.equal(await tv.locator('#ytc-tvquery-text').textContent(), 'Search videos');
        assert.ok((await tv.locator('#ytc-sug .sg.hs a').allTextContents()).some(t => t.trim() === 'cat'), 'empty query shows all recent searches, not matches for the route’s old query');
        await key('Enter');
        await tv.keyboard.type('rob');
        await tv.waitForSelector('#ytc-sug .sg:not(.hs)', { timeout: 5000 });
        await back(tv);
        await key('ArrowDown');
        assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-sug')), 'query ↓ reaches suggestions');
        await key('Enter');
        await tv.waitForFunction(() => document.getElementById('ytc')?.getAttribute('data-tvq') !== '1');
        await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'));
        assert.equal(await tv.evaluate(() => window.__tvImeCalls.show), 0, 'no call to an unavailable/no-op OS keyboard');
        assert.deepEqual(tv.errors, []);
      } finally { await tvClose(tv); }
    }
  });

  await step('TV search lifecycle: Search above watch/comments retains focus, Back returns to its source, history navigation closes the keyboard and results restore correctly', async () => {
    const tv = await tvOpen();
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/results?search_query=keyboard+lifecycle');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'));
      const results = tv.url();
      // The first recorded search result is a playlist. Pick a video explicitly so this lifecycle
      // check enters a watch page before asking for its comments drawer.
      await tv.locator('#ytc-list a.t[href^="/watch?"]').first().focus();
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => location.pathname === '/watch');
      await tv.waitForSelector('#ytc-cm .yc-h', { state: 'attached' });
      await tv.focus('#ytc-player [data-act=tv-comments]');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-comments.on');
      const origin = await tv.evaluate(() => document.activeElement.id || document.activeElement.dataset.act);
      await tv.keyboard.press('/');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-tvk .tvk-keys button'));
      assert.equal(await tv.locator('#ytc-top').isVisible(), true, 'Search is visible above the watch page');
      await tv.waitForSelector('#ytc-comments.on #ytc-cm-list > .ycm', { state: 'attached' });
      assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-search')), 'comment response never takes Search focus');
      await back(tv);
      assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvquery');
      await back(tv);
      assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '');
      assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-comments')), 'Back restores the comments control');
      assert.equal(await tv.evaluate(() => document.activeElement.id || document.activeElement.dataset.act), origin);

      await tv.keyboard.press('/');
      await tv.keyboard.type('unfinished');
      assert.equal(await tv.inputValue('#ytc-q'), 'unfinished');
      await tv.goBack({ waitUntil: 'commit' });
      // The URL is committed already. Redline stops YouTube's document, so an extra default
      // waitForURL would also wait for a full-page load that this same-document restore does not need.
      assert.equal(tv.url(), results, 'Back returns to the search result URL');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'));
      assert.equal(await tv.getAttribute('#ytc', 'data-tvq'), '', 'history navigation closes Search');
      assert.equal(await tv.getAttribute('#ytc', 'data-tvk'), '', 'history navigation closes keyboard');
      assert.equal(await tv.getAttribute('#ytc-tvk', 'aria-hidden'), 'true');
      assert.equal(await tv.locator('#ytc-tvquery-text').textContent(), 'keyboard lifecycle');
      assert.equal(await tv.locator('#ytc-comments.on').count(), 0);
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('dev build: TV search privacy: physical characters and remote keyboard paths never become telemetry; non-search navigation timings remain', async () => {
    const from = incidents.length;
    const tv = await tvOpen();
    const typed = 'qzx privacy sentinel';
    const submitted = typed + 'bc';
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/');
      await tv.focus('#ytc-side .rl[data-nav=search]');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-tvk .tvk-keys button'));
      const privateStart = await tv.evaluate(() => Date.now());
      await tv.keyboard.type(typed);
      // Remote entry into the same query: starting at a, Right+OK adds b, then Right+OK adds c.
      for (const key of ['ArrowRight', 'Enter', 'ArrowRight', 'Enter']) await tv.keyboard.press(key);
      assert.equal(await tv.inputValue('#ytc-q'), submitted);
      await tv.focus('#ytc-tvk [data-act=tv-kb-search]');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'));
      assert.equal(new URL(tv.url()).searchParams.get('search_query'), submitted);
      const privateEnd = await tv.evaluate(() => Date.now());

      // An ordinary remote move after Search remains measurable; this also proves telemetry is on.
      await tv.waitForTimeout(30);
      await tv.keyboard.press('ArrowRight');
      await tv.waitForTimeout(100);
      await tv.goto('https://www.youtube.com/results?search_query=privacy+flush', { waitUntil: 'commit' });
      let page;
      for (let i = 0; i < 50; i++) {
        page = telBatches(from).find(b => b.events.some(e => e.m === 'nav.search' && e.mode === 'push' && e.at >= privateStart && e.at <= privateEnd))?.page;
        if (page && telBatches(from).some(b => b.page === page && b.events.some(e => e.m === 'ui.act' && e.act === 'key.arrowright' && e.at > privateEnd))) break;
        await tv.waitForTimeout(100);
      }
      assert.ok(page, 'submitted search still produces a navigation timing in a delivered batch');
      const batches = telBatches(from).filter(b => b.page === page);
      const events = batches.flatMap(b => b.events);
      assert.ok(events.some(e => e.m === 'ui.act' && e.act === 'key.arrowright' && e.at > privateEnd), 'ordinary navigation keys still measured outside Search');
      const privateActs = events.filter(e => e.m === 'ui.act' && e.at >= privateStart && e.at <= privateEnd);
      assert.ok(!privateActs.some(e => /^(?:field\.)?(?:key|mod)\./.test(e.act) || /^tv\.kb\./.test(e.act)), 'no physical characters, directions, Enter sequence or keyboard clicks: ' + JSON.stringify(privateActs));
      const encoded = JSON.stringify(batches);
      for (const secret of [typed, submitted, 'qzx', 'search_query', 'privacy+sentinel', 'privacy%20sentinel']) {
        assert.ok(!encoded.includes(secret), 'no search text in telemetry: ' + secret);
      }
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });
};
