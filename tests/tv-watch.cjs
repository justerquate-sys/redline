/* TV watch flows run inside e2e.cjs's offline browser/fixture harness. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');

module.exports = async function tvWatchTests({ step, tvOpen, tvClose, tvGo, tvAudit, tvShot, back, TVSIZES, T_FIRST, T_NEW, T_PAGE2, T_REP }) {
  const openAction = async (tv, act) => {
    await tv.focus('#ytc-seek');
    // An idle overlay first wakes on the seek bar; the next Down reaches its buttons.
    for (let i = 0; i < 2; i++) {
      if (await tv.evaluate(() => !!document.activeElement.closest('.prow'))) break;
      await tv.keyboard.press('ArrowDown');
    }
    for (let i = 0; i < 12; i++) {
      if (await tv.evaluate(a => document.activeElement.dataset.act === a, act)) {
        await tv.keyboard.press('Enter');
        return;
      }
      await tv.keyboard.press('ArrowRight');
    }
    assert.fail(`remote could not reach ${act}`);
  };
  const drawer = async (tv, selector) => {
    const state = await tv.evaluate(s => {
      const el = document.querySelector(s), r = el.getBoundingClientRect();
      return { right: r.right, top: r.top, bottom: r.bottom, width: r.width, W: innerWidth, H: innerHeight, inside: el.contains(document.activeElement), modal: el.getAttribute('aria-modal') };
    }, selector);
    assert.ok(state.inside && state.modal === 'true' && state.right === state.W && state.top === 0 && state.bottom === state.H && state.width >= state.W * .3 && state.width <= state.W * .37, JSON.stringify(state));
  };

  await step('TV comments: explicitly opened by remote, no playback requests, replies/sort/paging, close and reopen preserve state, Back restores focus (960×540 and 1920×1080)', async () => {
    const tv = await tvOpen();
    const requested = [];
    tv.on('request', request => {
      if (!request.url().includes('/youtubei/v1/next')) return;
      try { const body = request.postDataJSON(); if (body.continuation) requested.push(body.continuation); } catch {}
    });
    try {
      for (const [width, height] of TVSIZES) {
        await tv.setViewportSize({ width, height });
        await tvGo(tv, 'signed', 'https://www.youtube.com/watch?v=-Bxc4gJluC0');
        await tv.waitForSelector('#ytc-cm .yc-h', { state: 'attached' });
        const before = requested.length;
        await tv.waitForTimeout(1700);
        assert.equal(requested.length, before, 'closed comments never compete with playback');
        await openAction(tv, 'tv-comments');
        await tv.waitForSelector('#ytc-comments.on #ytc-cm-list > .ycm');
        await drawer(tv, '#ytc-comments.on');
        assert.deepEqual(await tvAudit(tv, `comments ${width}`), []);
        await tvShot(tv, 'player-comments');
        assert.equal(requested.slice(before).filter(t => t === T_FIRST).length, 1);
        assert.equal(await tv.locator('#ytc-cm-list > .ycm').count(), 20);
        assert.equal(await tv.locator('#ytc-cm-list > .ycm > .ycb > .yct[tabindex="0"]').count(), 20);
        // The first Down from Back reaches sorting; Right reaches Newest.
        await tv.keyboard.press('ArrowDown');
        assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-csort')), 'sort reachable by remote');
        await tv.keyboard.press('ArrowRight');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-cs1');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.querySelector('#ytc-cm-list .yca')?.textContent === '@newest-e2e');
        assert.ok(requested.includes(T_NEW));
        assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-comments')), 'sort keeps drawer focus');
        assert.deepEqual(await tvAudit(tv, `comments sorted ${width}`), []);
        await tvShot(tv, 'player-comments-newest');
        // Back/reopen at the top does not fetch the first or next page again.
        const loaded = requested.length;
        await tv.evaluate(() => document.querySelector('#ytc-player [data-act=tv-comments]').click());
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-comments');
        await tv.keyboard.press('Enter');
        await tv.waitForTimeout(150);
        assert.equal(requested.length, loaded, 'reopening the loaded top does not fetch another page');
        // Existing reply actions work with OK and collapse without refetching.
        await tv.focus('#ytc-rb-0');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-rp-0 .ycm');
        assert.ok(requested.includes(T_REP));
        const replies = requested.filter(t => t === T_REP).length;
        await tv.keyboard.press('Enter');
        assert.equal(await tv.locator('#ytc-rp-0.hide').count(), 1);
        assert.equal(requested.filter(t => t === T_REP).length, replies);
        // A held Down walks the actual reader and reaches the continuation sentinel.
        await tv.focus('#ytc-cm-list > .ycm:first-child > .ycb > .yct');
        for (let i = 0; i < 130 && !(await tv.locator('#ytc-cm-more.yc-end').count()); i++) {
          await tv.keyboard.press('ArrowDown');
        }
        await tv.waitForSelector('#ytc-cm-more.yc-end', { state: 'attached', timeout: 5000 });
        assert.ok(requested.includes(T_PAGE2));
        assert.equal(await tv.locator('#ytc-cm-list > .ycm').count(), 40);
        assert.ok(await tv.evaluate(() => document.getElementById('ytc-tvcm-body').scrollTop > 0), 'reader scrolls with remote');
        assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-comments')), 'paging keeps focus in reader');
        await tv.keyboard.press('Escape');
        assert.equal(await tv.locator('#ytc-comments.on').count(), 0);
        assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-comments');
        assert.deepEqual(tv.errors, []);
      }
    } finally { await tvClose(tv); }
  });

  await step('TV More info: visible Back, actionable chapters/channel, Subscribe slot, focus stays in drawer; Back/Forward restores details and never leaks an overlay', async () => {
    const tv = await tvOpen();
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'signed', 'https://www.youtube.com/watch?v=descchaps01');
      await tv.waitForSelector('#ytc-info .wt');
      await openAction(tv, 'tv-info');
      await drawer(tv, '#ytc-tvinfo.on');
      assert.equal(await tv.evaluate(() => document.activeElement.className), 'tvd');
      assert.deepEqual(await tvAudit(tv, 'More info details'), []);
      await tvShot(tv, 'player-info-details');
      assert.equal(await tv.locator('#ytc-tvinfo [data-act=tv-info-close]').textContent(), 'Back to video');
      assert.equal(await tv.locator('#ytc-tvinfo .tvchap button').count(), 3);
      assert.equal(await tv.locator('#ytc-tvsub [data-act=a-sub]').count(), 1);
      const title = await tv.locator('#ytc-tvinfo h2').textContent();
      await tv.focus('#ytc-tvinfo .tvchap button[data-arg="12"]');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => Math.abs(document.getElementById('ytc-video').currentTime - 12) < 2, null, { timeout: 10000 });
      assert.equal(await tv.locator('#ytc-tvinfo.on').count(), 1, 'chapter seek keeps the reader open');
      await tv.focus('#ytc-tvinfo .tvia a');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav !== 'watch');
      assert.equal(await tv.locator('#ytc-tvinfo.on').count(), 0, 'channel navigation closes details');
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForSelector('#ytc-player .prow');
      await openAction(tv, 'tv-info');
      await tv.waitForSelector('#ytc-tvinfo.on');
      assert.equal(await tv.locator('#ytc-tvinfo h2').textContent(), title, 'restored watch rebuilds its details');
      await tv.focus('#ytc-tvinfo [data-act=tv-info-close]');
      await tv.keyboard.press('Enter');
      assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-info');
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV comments: failed first page offers remote Retry, empty and disabled comments show a readable notice with a working Back control', async () => {
    const tv = await tvOpen();
    let mode = 'error';
    const fixture = JSON.parse(zlib.gunzipSync(fs.readFileSync(path.join(__dirname, 'fixtures/next.json.gz'))));
    const disable = o => {
      if (!o || typeof o !== 'object') return;
      if (o.itemSectionRenderer?.sectionIdentifier === 'comment-item-section') {
        o.itemSectionRenderer.contents = [{ messageRenderer: { text: { simpleText: 'Comments are turned off' } } }];
      }
      for (const x of Object.values(o)) disable(x);
    };
    disable(fixture);
    await tv.route('**/youtubei/v1/next*', async route => {
      const body = route.request().postDataJSON();
      if (mode === 'disabled' && body.videoId) return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(fixture) });
      if (body.continuation === T_FIRST) {
        if (mode === 'error') return route.fulfill({ status: 500, contentType: 'application/json', body: '{"error":{"code":500,"message":"Backend Error"}}' });
        if (mode === 'empty') return route.fulfill({ status: 200, contentType: 'application/json', body: '{"onResponseReceivedEndpoints":[{"reloadContinuationItemsCommand":{"continuationItems":[{"messageRenderer":{"text":{"simpleText":"No comments yet"}}}]}}]}' });
      }
      return route.fallback();
    });
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=-Bxc4gJluC0');
      await tv.waitForSelector('#ytc-cm .yc-h', { state: 'attached' });
      await openAction(tv, 'tv-comments');
      await tv.waitForSelector('#ytc-comments [data-act=a-cretry]');
      assert.match(await tv.locator('#ytc-comments').textContent(), /Couldn't load comments/);
      mode = 'empty';
      await tv.focus('#ytc-comments [data-act=a-cretry]');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc-cm-list')?.textContent.includes('No comments yet'));
      assert.ok(await tv.evaluate(() => !!document.activeElement.closest('#ytc-comments')), 'retry replacing itself preserves focus');
      assert.deepEqual(await back(tv), { handled: true, exit: false });
      mode = 'disabled';
      await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=disabledtv1');
      await tv.waitForSelector('#ytc-cm .yc-note', { state: 'attached' });
      await openAction(tv, 'tv-comments');
      assert.match(await tv.locator('#ytc-comments').textContent(), /Comments are turned off/);
      await tv.keyboard.press('ArrowDown');
      await tv.keyboard.press('Enter');
      assert.equal(await tv.locator('#ytc-comments.on').count(), 0, 'Back is usable with no comment controls');
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV reply continuation: hiding Show more replies keeps a visible remote focus; failed replies can retry their own page', async () => {
    const tv = await tvOpen();
    const moreToken = 'TV_MORE_REPLIES';
    const replyPage = (text, more) => JSON.stringify({ onResponseReceivedEndpoints: [{ appendContinuationItemsAction: { continuationItems: [
      { commentRenderer: { commentId: text, authorText: { simpleText: '@reader' }, contentText: { runs: [{ text }] }, publishedTimeText: { simpleText: '1 day ago' } } },
      ...(more ? [{ continuationItemRenderer: { continuationEndpoint: { continuationCommand: { token: moreToken } } } }] : [])
    ] } }] });
    let fail = true, attempts = 0, release;
    let requested;
    const pending = new Promise(resolve => { requested = resolve; });
    await tv.route('**/youtubei/v1/next*', async route => {
      const body = route.request().postDataJSON();
      if (body.continuation === T_REP) return route.fulfill({ status: 200, contentType: 'application/json', body: replyPage('First reply', true) });
      if (body.continuation !== moreToken) return route.fallback();
      attempts++;
      if (fail) {
        await new Promise(resolve => { release = resolve; requested(); });
        return route.fulfill({ status: 500, contentType: 'application/json', body: '{"error":{"message":"Reply page unavailable"}}' });
      }
      return route.fulfill({ status: 200, contentType: 'application/json', body: replyPage('Second reply after retry', false) });
    });
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=-Bxc4gJluC0');
      await tv.waitForSelector('#ytc-cm .yc-h', { state: 'attached' });
      await openAction(tv, 'tv-comments');
      await tv.waitForSelector('#ytc-rb-0');
      await tv.focus('#ytc-rb-0');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-rp-0 > button.ycrb');
      await tv.focus('#ytc-rp-0 > button.ycrb');
      await tv.keyboard.press('Enter');
      await pending;
      const usableFocus = () => tv.evaluate(() => {
        const a = document.activeElement;
        return !!a.closest('#ytc-comments') && !a.closest('[hidden],.hide') && a.checkVisibility({ checkVisibilityCSS: true });
      });
      assert.ok(await usableFocus(), 'hiding the continuation action immediately leaves a visible focus');
      const previous = await tv.evaluate(() => document.activeElement.outerHTML);
      await tv.keyboard.press('ArrowDown');
      assert.ok(await usableFocus());
      assert.notEqual(await tv.evaluate(() => document.activeElement.outerHTML), previous, 'pending reply page does not deadlock arrows');
      release();
      await tv.waitForSelector('#ytc-rp-0 .yc-note [data-act=a-rep]');
      assert.match(await tv.locator('#ytc-rp-0').textContent(), /Couldn't load replies/);
      fail = false;
      await tv.focus('#ytc-rp-0 .yc-note [data-act=a-rep]');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc-rp-0').textContent.includes('Second reply after retry'));
      assert.equal(attempts, 2, 'Retry continues replies instead of refetching top-level comments');
      assert.ok(await usableFocus());
      assert.deepEqual(await back(tv), { handled: true, exit: false });
      assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-comments');
      assert.deepEqual(tv.errors, []);
    } finally { if (release) release(); await tvClose(tv); }
  });

  await step('TV long comment threads: remote reaches Replies, Down reads every expanded reply without jumping backward, Up returns to the parent action', async () => {
    const tv = await tvOpen();
    const replyToken = 'TV_LONG_THREAD';
    const comment = (id, text) => ({ commentRenderer: {
      commentId: id, authorText: { simpleText: '@reader' }, authorEndpoint: { browseEndpoint: { browseId: 'UCtvreader', canonicalBaseUrl: '/@tv-reader' } },
      contentText: { runs: id === 'parent' ? [{ text: 'At ' }, { text: '0:12', navigationEndpoint: { watchEndpoint: { videoId: '-Bxc4gJluC0', startTimeSeconds: 12 } } }, { text: '\n\n' + text }] : [{ text }] },
      publishedTimeText: { simpleText: '1 day ago' }
    } });
    const longText = prefix => Array.from({ length: 14 }, (_, i) => `${prefix} paragraph ${i + 1}: The materials and the steps are explained here so the complete text can be read using the remote.`).join('\n\n');
    const first = JSON.stringify({ onResponseReceivedEndpoints: [{ reloadContinuationItemsCommand: { continuationItems: [
      { commentThreadRenderer: { comment: comment('parent', longText('Parent')), replies: { commentRepliesRenderer: { contents: [{ continuationItemRenderer: { continuationEndpoint: { continuationCommand: { token: replyToken } } } }], viewReplies: { buttonRenderer: { text: { simpleText: '6 replies' } } } } } } },
      { commentThreadRenderer: { comment: comment('after-thread', 'The next top-level comment') } }
    ] } }] });
    const replies = JSON.stringify({ onResponseReceivedEndpoints: [{ appendContinuationItemsAction: { continuationItems: Array.from({ length: 6 }, (_, i) => comment(`reply-${i + 1}`, longText(`Reply ${i + 1}`))) } }] });
    await tv.route('**/youtubei/v1/next*', async route => {
      const token = route.request().postDataJSON().continuation;
      if (token === T_FIRST) return route.fulfill({ status: 200, contentType: 'application/json', body: first });
      if (token === replyToken) return route.fulfill({ status: 200, contentType: 'application/json', body: replies });
      return route.fallback();
    });
    const position = () => tv.evaluate(() => {
      const a = document.activeElement, body = document.getElementById('ytc-tvcm-body'), r = a.getBoundingClientRect();
      const text = a.matches('.yct') ? a.textContent : '';
      return { scroll: body.scrollTop, act: a.dataset.act || '', arg: a.dataset.arg || '', reader: a.matches('.yct'), author: a.matches('.yca'), text: text.slice(0, 32), reply: /^Reply (\d+)/.exec(text)?.[1] || '', after: text === 'The next top-level comment', top: r.top, bottom: r.bottom, inDrawer: !!a.closest('#ytc-comments') };
    });
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=-Bxc4gJluC0');
      await tv.waitForSelector('#ytc-cm .yc-h', { state: 'attached' });
      await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > .1);
      await tv.keyboard.press('Enter'); // OK on the initial seek stop pauses while reading.
      await openAction(tv, 'tv-comments');
      await tv.waitForSelector('#ytc-rb-0');
      // Only remote input from the drawer's initial Back focus: no DOM focus jumps.
      let state = await position(), authorReached = false, timestampReached = false;
      for (let i = 0; i < 100 && state.act !== 'a-rep'; i++) {
        authorReached ||= state.author;
        if (state.reader && !timestampReached) {
          await tv.keyboard.press('ArrowRight');
          assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-comments .yct a[data-act=seek][data-arg="12"]')), 'Right reaches the timestamp inside comment text');
          await tv.keyboard.press('Enter');
          await tv.waitForFunction(() => Math.abs(document.getElementById('ytc-video').currentTime - 12) < 2);
          await tv.keyboard.press('ArrowLeft');
          assert.ok((await position()).reader, 'Left returns from inline link to its reader');
          timestampReached = true;
        }
        await tv.keyboard.press('ArrowDown');
        state = await position();
      }
      assert.ok(authorReached, 'the author link is reachable before the text');
      assert.ok(timestampReached, 'the inline timestamp is reachable from the text');
      assert.equal(state.act, 'a-rep', `the long parent's own Replies action is remote reachable: ${JSON.stringify(state)}`);
      assert.equal(state.arg, '0');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.querySelectorAll('#ytc-rp-0 .yct').length === 6);
      const down = [await position()], seen = new Set();
      for (let i = 0; i < 200 && !down.at(-1).after; i++) {
        await tv.keyboard.press('ArrowDown');
        const next = await position(), last = down.at(-1);
        assert.ok(next.inDrawer && next.scroll >= last.scroll - 1, `Down moved backward: ${JSON.stringify({ last, next, tail: down.slice(-4) })}`);
        if (next.reply) seen.add(next.reply);
        down.push(next);
      }
      assert.ok(down.at(-1).after, `Down reaches the next thread instead of looping: ${JSON.stringify(down.slice(-6))}`);
      assert.deepEqual([...seen].sort(), ['1', '2', '3', '4', '5', '6'], 'every expanded reply is read along the way');
      const up = [await position()], reverse = new Set();
      for (let i = 0; i < 200 && up.at(-1).act !== 'a-rep'; i++) {
        await tv.keyboard.press('ArrowUp');
        const next = await position(), last = up.at(-1);
        assert.ok(next.inDrawer && next.scroll <= last.scroll + 1, `Up moved forward: ${JSON.stringify({ last, next, tail: up.slice(-4) })}`);
        if (next.reply) reverse.add(next.reply);
        up.push(next);
      }
      assert.equal(up.at(-1).act, 'a-rep', `Up returns to the parent action: ${JSON.stringify(up.slice(-6))}`);
      assert.deepEqual([...reverse].sort(), ['1', '2', '3', '4', '5', '6']);
      await tv.keyboard.press('Enter');
      assert.equal(await tv.locator('#ytc-rp-0.hide').count(), 1, 'the parent action still collapses its replies');
      assert.deepEqual(await back(tv), { handled: true, exit: false });
      assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-comments');
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV long descriptions: initial reader focus is visible, arrows read long text before leaving, duplicate opens preserve Back focus', async () => {
    const tv = await tvOpen();
    const fixture = JSON.parse(zlib.gunzipSync(fs.readFileSync(path.join(__dirname, 'fixtures/next.json.gz'))));
    fixture.currentVideoEndpoint.watchEndpoint.videoId = 'longdetail1';
    const replace = o => {
      if (!o || typeof o !== 'object') return;
      if (o.attributedDescription) {
        o.attributedDescription.content = '0:12\n\n' + Array.from({ length: 70 }, (_, i) => `Section ${i + 1}: A detailed explanation of the video and its materials.`).join('\n\n');
        o.attributedDescription.commandRuns = [{ startIndex: 0, length: 4, onTap: { innertubeCommand: { watchEndpoint: { videoId: 'longdetail1', startTimeSeconds: 12 } } } }];
      }
      if (o.videoPrimaryInfoRenderer?.title) o.videoPrimaryInfoRenderer.title = { simpleText: 'A longer video title explaining the complete process, the equipment, the results, and the next steps in this detailed workshop' };
      for (const x of Object.values(o)) replace(x);
    };
    replace(fixture);
    await tv.route('**/youtubei/v1/next*', async route => {
      if (route.request().postDataJSON().videoId) return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(fixture) });
      return route.fallback();
    });
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=longdetail1');
      await tv.waitForSelector('#ytc-info .wt');
      await openAction(tv, 'tv-info');
      const reader = () => tv.evaluate(() => {
        const d = document.querySelector('#ytc-tvinfo .tvd'), b = document.getElementById('ytc-tvinfo-body');
        const r = d.getBoundingClientRect(), s = b.getBoundingClientRect();
        return { focused: d === document.activeElement, top: r.top, bottom: r.bottom, bodyTop: s.top, bodyBottom: s.bottom, scroll: b.scrollTop };
      });
      const start = await reader();
      assert.ok(start.focused && start.top < start.bodyBottom && start.bottom > start.bodyTop, `initial reader intersects its viewport: ${JSON.stringify(start)}`);
      await tv.keyboard.press('ArrowRight');
      assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-tvinfo .tvd a[data-act=seek][data-arg="12"]')), 'Right reaches description timestamp links');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => Math.abs(document.getElementById('ytc-video').currentTime - 12) < 2);
      await tv.keyboard.press('ArrowLeft');
      assert.ok((await reader()).focused, 'Left returns from description link to the reader');
      for (let i = 0; i < 3; i++) await tv.keyboard.press('ArrowDown');
      const after = await reader();
      assert.ok(after.focused && after.scroll > start.scroll, 'Down reads long text without skipping the rest');
      await tv.keyboard.press('ArrowUp');
      const beforeDuplicate = await reader();
      assert.ok(beforeDuplicate.scroll < after.scroll, 'Up reads back through the same text');
      await tv.evaluate(() => document.querySelector('#ytc-player [data-act=tv-info]').click());
      assert.equal((await reader()).scroll, beforeDuplicate.scroll, 'duplicate open preserves the reading position');
      assert.deepEqual(await back(tv), { handled: true, exit: false });
      assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-info', 'duplicate opening did not replace the return target with hidden reader text');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-tvinfo.on');
      assert.deepEqual(await back(tv), { handled: true, exit: false });
      assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-info');
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });
};
