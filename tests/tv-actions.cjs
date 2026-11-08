/* TV: what the official TV app has and the raw one had not (the player row's new actions, the channel, the
   languages), walked with the remote's keys. Run by e2e.cjs with its offline fixtures. */
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const zlib = require('node:zlib');

module.exports = async ({ step, tvOpen, tvClose, tvGo, tvAudit, tvShot, back, TVSIZES, tvWalk, calls, log, ACTS, sw, MIDERR, midRelease, noHedge }) => {
  const VID = '-Bxc4gJluC0', CH = 'UCaZO-66UcqyVT9btiqdDryA';
  const nextFixture = () => JSON.parse(zlib.gunzipSync(fs.readFileSync(path.join(__dirname, 'fixtures/next.json.gz'))));
  const video = tv => tv.evaluate(() => { const v = document.getElementById('ytc-video'); return { t: v.currentTime, paused: v.paused }; });
  const playing = (tv, t = 0.3) => tv.waitForFunction(t => document.getElementById('ytc-video')?.currentTime > t, t, { timeout: 20000 });
  // (Paused the overlay stays: a step can look at it and take its screenshots at any pace.)
  const paused = async tv => { await playing(tv); await tv.evaluate(() => document.getElementById('ytc-video').pause()); await tv.waitForFunction(() => document.getElementById('ytc-video').paused); await tv.waitForTimeout(100); };
  const act = tv => tv.evaluate(() => { const a = document.activeElement; return a.id || a.dataset.act || a.className; });
  const isIn = (tv, sel) => tv.evaluate(s => !!document.activeElement.closest(s), sel);
  // From the seek bar (or the hidden overlay) into the button row, with the remote.
  const toRow = async tv => {
    for (let i = 0; i < 3 && !(await isIn(tv, '.prow')); i++) await tv.keyboard.press('ArrowDown');
    assert.ok(await isIn(tv, '.prow'), 'the row is reached');
  };
  const walkRow = async (tv, sel) => {
    await toRow(tv);
    const here = () => tv.evaluate(s => document.activeElement.matches(s), sel);
    for (let i = 0; i < 14 && !(await here()); i++) await tv.keyboard.press('ArrowLeft');
    for (let i = 0; i < 15 && !(await here()); i++) await tv.keyboard.press('ArrowRight');
    assert.ok(await here(), `the remote reaches ${sel}: on ${await act(tv)}`);
  };
  // The overlay stays up while a step looks at it (it hides 4 s after the last key).
  const label = tv => tv.evaluate(() => { const l = document.activeElement.querySelector('.tvpl'), r = l && l.getBoundingClientRect(); return l && { text: l.textContent, shown: l.checkVisibility(), l: r.left, r: r.right, b: r.bottom, W: innerWidth, H: innerHeight }; });
  const inSafe = l => l.l >= l.W * 0.025 - 1 && l.r <= l.W * 0.975 + 1 && l.b <= l.H * 0.965 + 1;
  // One row inside the safe area at 60 px buttons.
  const rowFits = tv => tv.evaluate(() => {
    const b = [...document.querySelectorAll('#ytc-player .prow :is(.pb,.wl)')].filter(e => e.checkVisibility()).map(e => e.getBoundingClientRect());
    const k = innerHeight / 1080, row = document.querySelector('#ytc-player .prow');
    return { n: b.length, tops: new Set(b.map(r => Math.round(r.top))).size, h: b.every(r => Math.abs(r.height - 60 * k) < 1), round: b.filter(r => Math.abs(r.width - 60 * k) < 1).length, right: Math.max(...b.map(r => r.right)), left: Math.min(...b.map(r => r.left)), W: innerWidth, over: row.scrollWidth > row.clientWidth + 1 };
  });
  const stored = async () => { const o = await sw.evaluate(async () => (await chrome.storage.local.get('ytcConfigV1')).ytcConfigV1); return typeof o === 'string' ? JSON.parse(o) : o || {}; };
  // Back to the defaults this file's steps change (the layout, the two languages).
  const forget = () => sw.evaluate(async () => {
    const o = (await chrome.storage.local.get('ytcConfigV1')).ytcConfigV1; if (!o) return;
    const j = typeof o === 'string' ? JSON.parse(o) : o;
    delete j.kb; if (j.p) { delete j.p.al; delete j.p.cl; }
    await chrome.storage.local.set({ ytcConfigV1: typeof o === 'string' ? JSON.stringify(j) : j });
  });
  const accountCall = (ep, n = -1) => { const c = calls(ep).at(n); assert.ok(c, `${ep} went out`); assert.equal(c.body.context.client.clientName, 'TVHTML5', `${ep} as the TV client`); assert.match(c.headers.authorization, /^Bearer ya29\./, `${ep} with the access token`); return c; };
  const note = tv => tv.evaluate(() => { const n = document.getElementById('ytc-tvactnote'), r = n.getBoundingClientRect(); return { text: n.textContent, shown: n.checkVisibility(), l: r.left, r: r.right, t: r.top, b: r.bottom }; });

  await step('TV channel from the player (960×540 and 1920×1080): the row\'s first action, "Go to channel", opens it; Back returns to the video at its position; More info\'s first ↑ reaches View channel', async () => {
    const tv = await tvOpen();
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        for (const mode of ['signed', 'code']) {
          await tvGo(tv, mode, `https://www.youtube.com/watch?v=${VID}`);
          await tv.waitForSelector('#ytc-tvch .tvch');
          await playing(tv);
          await tv.evaluate(() => { document.getElementById('ytc-video').currentTime = 15; });
          await tv.waitForFunction(() => document.getElementById('ytc-video').currentTime >= 15);
          await tv.waitForFunction(() => document.activeElement?.id === 'ytc-seek');
          // ↓ lands on Play as before; ← walks to Channel in the same row.
          await tv.keyboard.press('ArrowDown');
          await toRow(tv);
          assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'p-toggle', '↓ from the bar lands on play/pause');
          await walkRow(tv, '.tvch');
          const lab = await label(tv);
          assert.ok(lab.text === 'Go to channel' && lab.shown && inSafe(lab), `its label: ${JSON.stringify(lab)}`);
          assert.equal(await tv.evaluate(() => getComputedStyle(document.querySelector('.tvch .av')).borderRadius), '50%', 'the channel\'s picture');
          assert.deepEqual(await tvAudit(tv, `row channel ${mode} ${W}`), []);
          if (mode === 'signed') await tvShot(tv, 'player-row-channel');
          const t0 = (await video(tv)).t;
          await tv.keyboard.press('Enter');
          await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav !== 'watch' && document.activeElement?.matches('#ytc-list a.t'), null, { timeout: 8000 });
          assert.equal(await tv.locator('#ytc-player video').count(), 0, 'the player went');
          // One Back: the shell goes back (not the rail first).
          assert.deepEqual(await back(tv), { handled: false, exit: false });
          await tv.goBack({ waitUntil: 'commit' });
          await tv.waitForFunction(() => document.getElementById('ytc-video') && document.documentElement.dataset.ytcTv === '1', null, { timeout: 8000 });
          await tv.waitForFunction(t => document.getElementById('ytc-video').currentTime >= t - 1, t0, { timeout: 15000 });
          const t1 = (await video(tv)).t;
          assert.ok(t1 >= t0 - 1 && t1 < t0 + 8, `back at its position: ${t0} → ${t1}`);
          // More info: the description is focused; the first ↑ reaches View channel.
          await tv.waitForSelector('#ytc-tvch .tvch');
          await walkRow(tv, '[data-act=tv-info]');
          await tv.keyboard.press('Enter');
          await tv.waitForSelector('#ytc-tvinfo.on');
          assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-tvinfo .tvd')), 'the description has the focus');
          await tv.keyboard.press('ArrowUp');
          assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-tvinfo .tvia a')), `the first ↑ reaches View channel: ${await act(tv)}`);
          assert.equal((await tv.locator('#ytc-tvinfo .tvia a').textContent()), 'View channel');
          assert.deepEqual(await tvAudit(tv, `info channel ${mode} ${W}`), []);
          await tv.keyboard.press('Enter');
          await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav !== 'watch', null, { timeout: 8000 });
          assert.deepEqual(await back(tv), { handled: false, exit: false }, 'from More info too');
        }
      }
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV Subscribe (960×540 and 1920×1080): on the channel page at the header\'s right end and in the player row; one state on every surface; a second OK confirms unsubscribing; refused: back with a readable notice; signed out → Sign in; the account\'s own state shows', async () => {
    const tv = await tvOpen();
    let account = null;
    await tv.route(/\/youtubei\/v1\/(next|browse)/, async route => {
      const b = route.request().postDataJSON();
      if (account && b.context?.client?.clientName === 'TVHTML5' && (b.videoId || b.browseId?.startsWith('UC'))) return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(account) });
      return route.fallback();
    });
    const sub = id => tv.evaluate(id => { const b = document.getElementById(id); return b && { text: b.textContent, pressed: b.getAttribute('aria-pressed'), cls: b.className }; }, id);
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        // The channel page: the first card keeps the focus (OK never subscribes by accident); ↑ from the first row reaches it.
        await tvGo(tv, 'signed', `https://www.youtube.com/channel/${CH}`);
        await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'), null, { timeout: 8000 });
        await tv.waitForSelector('#ytc-sub-b');
        const place = await tv.evaluate(() => { const h = document.querySelector('.feed>.ch').getBoundingClientRect(), b = document.getElementById('ytc-sub-b').getBoundingClientRect(), c = document.querySelectorAll('#ytc-list .c')[4].getBoundingClientRect(), rem = parseFloat(getComputedStyle(document.documentElement).fontSize); return { inHeader: b.top >= h.top - 1 && b.bottom <= h.bottom + 1, right: Math.abs(b.right - c.right), above: b.bottom <= c.top, rem: b.height / rem }; });
        assert.ok(place.inHeader && place.right <= 1 && place.above && Math.abs(place.rem - 2.25) < 0.05, `at the header's right end, on the grid's right edge: ${JSON.stringify(place)}`);
        // ↑ from the first card: the channel's selected tab; once more: Subscribe.
        await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-ct-videos', '↑ from the first card');
        await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-sub-b', '↑ from the tab row');
        assert.deepEqual(await tvAudit(tv, `channel subscribe ${W}`), []);
        await tvShot(tv, 'channel-subscribe');
        // (The scrim leaves the first row's ring alone.)
        const w = await tvWalk(tv, '#ytc-sub-b', 12);
        assert.deepEqual([w.dead, w.missing], [[], []], `channel page walk ${W}`);
        await tv.focus('#ytc-sub-b');
        const n0 = calls('subscription/subscribe').length;
        await tv.keyboard.press('Enter');
        assert.equal((await sub('ytc-sub-b')).text, 'Subscribed');
        await tv.waitForTimeout(150);
        assert.equal(calls('subscription/subscribe').length, n0 + 1);
        assert.deepEqual(accountCall('subscription/subscribe').body.channelIds, [CH]);
        await tvShot(tv, 'channel-subscribed');
        // Unsubscribing asks first, as on the desktop.
        const u0 = calls('subscription/unsubscribe').length;
        await tv.keyboard.press('Enter');
        assert.equal((await sub('ytc-sub-b')).text, 'Unsubscribe?');
        assert.equal(calls('subscription/unsubscribe').length, u0, 'the first OK only asks');
        // (A press right on top of the question has not read it.)
        await tv.keyboard.press('Enter');
        assert.equal((await sub('ytc-sub-b')).text, 'Unsubscribe?', 'too soon to count');
        await tv.waitForTimeout(450);
        await tv.keyboard.press('Enter');
        assert.equal((await sub('ytc-sub-b')).text, 'Subscribe');
        await tv.waitForTimeout(150);
        assert.equal(calls('subscription/unsubscribe').length, u0 + 1);
        accountCall('subscription/unsubscribe');
        // Refused: the state comes back, and the reason is on screen.
        ACTS.fail.add('subscription/subscribe');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.getElementById('ytc-sub-b').textContent === 'Subscribe' && document.getElementById('ytc-tvactnote').textContent);
        ACTS.fail.clear();
        const refused = await note(tv);
        assert.ok(refused.shown && /Subscription not saved/.test(refused.text) && refused.r <= W * 0.975 + 1 && refused.t >= H * 0.035 - 1, `a readable notice: ${JSON.stringify(refused)}`);
        assert.deepEqual(await tvAudit(tv, `refused ${W}`), []);
        await tvShot(tv, 'channel-subscribe-refused');

        // Signed out: Sign in, as elsewhere.
        await tvGo(tv, 'code', `https://www.youtube.com/channel/${CH}`);
        await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'), null, { timeout: 8000 });
        await tv.waitForSelector('#ytc-chsub [data-act=tv-signin]');
        await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-ct-videos', '↑ from the first card: the selected tab');
        await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-signin');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Subscribe');
        assert.equal(await tv.evaluate(() => getComputedStyle(document.activeElement).backgroundColor), 'rgb(241, 241, 241)', 'solid white, focused too');
        assert.deepEqual(await tvAudit(tv, `channel signed out ${W}`), []);
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-tvsign.on');
        assert.deepEqual(await back(tv), { handled: true, exit: false });

        // The player row, signed in: Subscribe beside Channel; the channel page and More info say the same.
        await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
        await tv.waitForSelector('#ytc-tvsubr-b');
        await paused(tv);
        await walkRow(tv, '#ytc-tvsubr-b');
        assert.equal((await label(tv)).text, 'Subscribe');
        assert.deepEqual(await tvAudit(tv, `row subscribe ${W}`), []);
        await tvShot(tv, 'player-row-subscribe');
        await tv.keyboard.press('Enter');
        let row = await sub('ytc-tvsubr-b');
        assert.ok(row.text === 'Subscribed' && row.pressed === 'true' && /\bon\b/.test(row.cls), JSON.stringify(row));
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvsubr-b', 'the focus stays on it');
        await tv.waitForTimeout(150);
        accountCall('subscription/subscribe');
        await tvShot(tv, 'player-row-subscribed');
        await tv.keyboard.press('ArrowLeft');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav !== 'watch');
        await tv.waitForSelector('#ytc-sub-b');
        assert.equal((await sub('ytc-sub-b')).text, 'Subscribed', 'the channel page knows');
        // Unsubscribed there: the row and More info follow.
        await tv.focus('#ytc-sub-b');
        await tv.keyboard.press('Enter');
        await tv.waitForTimeout(450);
        await tv.keyboard.press('Enter');
        assert.equal((await sub('ytc-sub-b')).text, 'Subscribe');
        await tv.waitForTimeout(150);
        await tv.goBack({ waitUntil: 'commit' });
        await tv.waitForSelector('#ytc-tvsubr-b');
        row = await sub('ytc-tvsubr-b');
        assert.ok(row.text === 'Subscribe' && row.pressed === 'false', JSON.stringify(row));
        await tv.waitForFunction(() => document.activeElement?.closest('#ytc-player'));
        await paused(tv);
        await walkRow(tv, '[data-act=tv-info]');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-tvinfo.on');
        assert.equal(await tv.locator('#ytc-tvsub button').textContent(), 'Subscribe', 'More info knows');
        await back(tv);

        // Signed out: no Subscribe in the row; Channel stays.
        await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${VID}`);
        await tv.waitForSelector('#ytc-tvch .tvch');
        assert.equal(await tv.locator('#ytc-tvsubr-b').count(), 0);

        // What the account says (its own client's answer): subscribed and liked show, without a press.
        account = { contents: { singleColumnWatchNextResults: { results: { results: { contents: [{ itemSectionRenderer: { contents: [{ videoOwnerRenderer: { navigationEndpoint: { browseEndpoint: { browseId: CH } }, subscriptionButton: { subscribed: true } } }] } }] } } } }, likeStatus: 'LIKE' };
        await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
        await tv.waitForFunction(() => document.getElementById('ytc-tvsubr-b')?.textContent === 'Subscribed' && document.getElementById('ytc-tvlk')?.getAttribute('aria-pressed') === 'true', null, { timeout: 8000 });
        account = { header: { subscribeButtonRenderer: { channelId: CH, subscribed: true } } };
        await tvGo(tv, 'signed', `https://www.youtube.com/channel/${CH}`);
        await tv.waitForFunction(() => document.getElementById('ytc-sub-b')?.textContent === 'Subscribed', null, { timeout: 8000 });
        // A shape it cannot read, or another channel's state, changes nothing.
        account = { responseContext: {}, subscriptionStateEntity: { subscribed: true }, subscriptionButton: { subscribed: true }, x: { subscribeButtonRenderer: { channelId: 'UCother', subscribed: true } } };
        await tvGo(tv, 'signed', `https://www.youtube.com/channel/${CH}`);
        await tv.waitForSelector('#ytc-sub-b');
        await tv.waitForTimeout(400);
        assert.equal((await sub('ytc-sub-b')).text, 'Subscribe');
        account = null;
      }
      assert.deepEqual(tv.errors, []);
    } finally { ACTS.fail.clear(); await tvClose(tv); }
  });

  await step('TV Audio (960×540 and 1920×1080): its own action only with several tracks, names as YouTube gives them, the current one marked; Captions holds captions only; the chosen language plays on the next video, the default where it has none; an explicit choice stays; the failure carry-through keeps it', async () => {
    const tv = await tvOpen();
    // dubtv000001's Up next is dubtv000002, whose is dubtw000001 (no French), then back to a video with one track.
    const fixture = nextFixture();
    const nextOf = { dubtv000001: 'dubtv000002', dubtv000002: 'dubtw000001', dubtw000001: 'dubtv000003', dubtv000003: VID };
    await tv.route('**/youtubei/v1/next*', route => {
      const b = route.request().postDataJSON(), to = nextOf[b.videoId];
      if (!to) return route.fallback();
      const o = JSON.parse(JSON.stringify(fixture));
      o.currentVideoEndpoint.watchEndpoint.videoId = b.videoId;
      o.contents.twoColumnWatchNextResults.autoplay.autoplay.sets[0].autoplayVideo.watchEndpoint.videoId = to;
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(o) });
    });
    const langOf = async (v, n0) => { await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 20000 }); const a = log.gv.slice(n0).filter(g => g.v === v && g.lang); return a.length ? a.at(-1).lang : null; };
    const rows = () => tv.$$eval('#ytc-menu .mls button', a => a.map(e => [e.textContent, e.classList.contains('on')]));
    const afterNext = async to => { await tv.keyboard.press('MediaTrackNext'); await tv.waitForFunction(v => new URLSearchParams(location.search).get('v') === v, to, { timeout: 8000 }); };
    try {
      for (const [W, H] of TVSIZES) {
        await forget();
        await tv.setViewportSize({ width: W, height: H });
        let n0 = log.gv.length;
        await tvGo(tv, 'signed', 'https://www.youtube.com/watch?v=dubtv000001');
        await tv.waitForSelector('#ytc-aub.on');
        assert.equal(await langOf('dubtv000001', n0), 'ru', 'nothing chosen: the original');
        await tv.waitForSelector('#ytc-tvch .tvch');
        await tv.waitForFunction(() => document.activeElement?.id === 'ytc-seek');
        await paused(tv);
        await walkRow(tv, '#ytc-aub');
        const fit = await rowFits(tv);
        assert.ok(fit.n === 13 && fit.tops === 1 && fit.h && !fit.over && fit.right <= fit.W * 0.975 + 1 && fit.left >= fit.W * 0.025 - 1, `the whole row, one row in the safe area: ${JSON.stringify(fit)}`);
        const lab = await label(tv);
        assert.equal(lab.text, 'Audio');
        await tvShot(tv, 'player-row-audio');
        assert.deepEqual(await tvAudit(tv, `row audio ${W}`), []);
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-player.menu #ytc-menu');
        assert.equal(await tv.evaluate(() => getComputedStyle(document.querySelector('#ytc-menu .pmi'), '::before').content), '"Audio"');
        assert.deepEqual(await rows(), [['Russian (original)', true], ['English', false], ['French', false]], 'YouTube\'s names, the default first, the current one marked');
        assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-menu .mls button.on')), 'the current one has the focus');
        assert.deepEqual(await tvAudit(tv, `audio panel ${W}`), []);
        await tvShot(tv, 'player-audio');
        // Choose French, with the remote.
        for (let i = 0; i < 3 && await tv.evaluate(() => document.activeElement.textContent !== 'French'); i++) await tv.keyboard.press('ArrowDown');
        const n1 = log.gv.length;
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.querySelector('#ytc-menu .mls button.on')?.textContent === 'French');
        for (let i = 0; i < 80 && !log.gv.slice(n1).some(g => g.lang === 'fr'); i++) await tv.waitForTimeout(100);
        assert.ok(log.gv.slice(n1).some(g => g.lang === 'fr'), 'the French track is fetched');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        await tv.waitForSelector('#ytc-player.menu', { state: 'detached', timeout: 3000 });
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-aub', 'Back returns to Audio');
        // Remembered for the device (a language, not a video).
        await tv.waitForTimeout(500);
        assert.equal((await stored()).p.al, 'fr');
        // Captions: captions only, no tabs.
        await tv.keyboard.press('ArrowLeft');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-ccb');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-player.menu #ytc-menu');
        assert.equal(await tv.locator('#ytc-menu .mtab, #ytc-menu [data-arg^="au:"]').count(), 0, 'no audio under Captions');
        assert.equal(await tv.evaluate(() => getComputedStyle(document.querySelector('#ytc-menu .pmi'), '::before').content), '"Captions"');
        await back(tv);
        // The next video: French by itself.
        n0 = log.gv.length;
        await afterNext('dubtv000002');
        await tv.waitForSelector('#ytc-aub.on');
        assert.equal(await langOf('dubtv000002', n0), 'fr', 'the chosen language on the next video');
        await tv.waitForFunction(() => document.querySelector('#ytc-player'));
        await walkRow(tv, '#ytc-aub');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-player.menu #ytc-menu');
        assert.equal((await rows()).find(r => r[1])?.[0], 'French', 'marked as playing');
        await back(tv);
        // No French here: the default track, and the remembered language stays for later.
        n0 = log.gv.length;
        await afterNext('dubtw000001');
        await tv.waitForSelector('#ytc-aub.on');
        assert.equal(await langOf('dubtw000001', n0), 'ru', 'falls back to the original');
        await tv.waitForTimeout(400);
        assert.equal((await stored()).p.al, 'fr', 'a fallback is not a choice');
        // An explicit choice for a video stays that video's: English on this one, then Back to it after another.
        await walkRow(tv, '#ytc-aub');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-player.menu #ytc-menu');
        for (let i = 0; i < 3 && await tv.evaluate(() => document.activeElement.textContent !== 'English'); i++) await tv.keyboard.press('ArrowDown');
        const n2 = log.gv.length;
        await tv.keyboard.press('Enter');
        for (let i = 0; i < 80 && !log.gv.slice(n2).some(g => g.lang === 'en'); i++) await tv.waitForTimeout(100);
        assert.ok(log.gv.slice(n2).some(g => g.lang === 'en'));
        await back(tv);
        await tv.waitForTimeout(400);
        assert.equal((await stored()).p.al, 'en');
        // Chosen on the next video instead: this one still comes back in its own.
        n0 = log.gv.length;
        await afterNext('dubtv000003');
        await tv.waitForSelector('#ytc-aub.on');
        assert.equal(await langOf('dubtv000003', n0), 'en');
        await walkRow(tv, '#ytc-aub');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-player.menu #ytc-menu');
        for (let i = 0; i < 3 && await tv.evaluate(() => document.activeElement.textContent !== 'French'); i++) await tv.keyboard.press('ArrowUp');
        await tv.keyboard.press('Enter');
        await tv.waitForTimeout(400);
        await back(tv);
        assert.equal((await stored()).p.al, 'fr');
        n0 = log.gv.length;
        await tv.goBack({ waitUntil: 'commit' });
        await tv.waitForFunction(() => new URLSearchParams(location.search).get('v') === 'dubtw000001');
        await tv.waitForSelector('#ytc-aub.on');
        assert.equal(await langOf('dubtw000001', n0), 'en', 'never switched silently against an explicit choice');
        // One track: no Audio action; a reload keeps the language (the device's).
        await afterNext('dubtv000003');
        await tv.waitForSelector('#ytc-aub.on');
        await afterNext(VID);
        await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 20000 });
        assert.ok(!await tv.evaluate(() => document.getElementById('ytc-aub').checkVisibility()), 'one track: no Audio action');
        const row = await rowFits(tv);
        assert.equal(row.n, 12);
        n0 = log.gv.length;
        await tvGo(tv, 'signed', 'https://www.youtube.com/watch?v=dubtv000004');
        await tv.waitForSelector('#ytc-aub.on');
        assert.equal(await langOf('dubtv000004', n0), 'fr', 'after a restart too');
      }
      assert.deepEqual(tv.errors, []);
    } finally { await forget(); await tvClose(tv); }
  });

  await step('TV Audio, a long list (960×540 and 1920×1080): eighteen languages: every focused row stays inside the safe area, down to the last and back to the first with its title', async () => {
    const tv = await tvOpen();
    try {
      await forget();
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=dubmany0001');
        await tv.waitForSelector('#ytc-aub.on');
        await paused(tv);
        await walkRow(tv, '#ytc-aub');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-player.menu #ytc-menu');
        assert.equal(await tv.locator('#ytc-menu .mls button').count(), 18);
        const rows = await tv.locator('#ytc-menu .mls button').count();
        for (let i = 1; i < rows; i++) {
          await tv.keyboard.press('ArrowDown');
          assert.deepEqual(await tvAudit(tv, `long audio list, row ${i} at ${W}`), []);
        }
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Language tr');
        await tvShot(tv, 'player-audio-long');
        for (let i = 1; i < rows; i++) await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Language en');
        assert.equal(await tv.evaluate(() => document.querySelector('#ytc-menu .pmi').scrollTop), 0, 'the title shows again');
        assert.deepEqual(await tvAudit(tv, `long audio list, first row at ${W}`), []);
        await back(tv);
      }
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV Audio on failure: a media failure\'s new player keeps the chosen track (own::Carry, audio_sel)', () => noHedge(async () => {
    const tv = await tvOpen();
    try {
      await forget();
      await tv.setViewportSize({ width: 960, height: 540 });
      const v = 'dubmid00009';
      MIDERR.go = false;
      await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${v}`);
      await tv.waitForSelector('#ytc-aub.on');
      await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 20000 });
      await walkRow(tv, '#ytc-aub');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-player.menu #ytc-menu');
      for (let i = 0; i < 3 && await tv.evaluate(() => document.activeElement.textContent !== 'French'); i++) await tv.keyboard.press('ArrowDown');
      await tv.keyboard.press('Enter');
      for (let i = 0; i < 100 && !log.gv.some(g => g.v === v && g.lang === 'fr'); i++) await tv.waitForTimeout(50);
      assert.ok(log.gv.some(g => g.v === v && g.lang === 'fr'), 'French goes out');
      await back(tv);
      await tv.waitForTimeout(800);
      midRelease();
      for (let i = 0; i < 300 && !log.gv.some(g => g.v === v && g.gen === 3 && g.lang); i++) await tv.waitForTimeout(50);
      await tv.waitForTimeout(800);
      assert.deepEqual([...new Set(log.gv.filter(g => g.v === v && g.gen === 3 && g.lang).map(g => g.lang))], ['fr'], 'the new player keeps the chosen track');
      assert.ok(await tv.evaluate(() => document.getElementById('ytc-aub').classList.contains('on')), 'the new player\'s row has Audio');
      assert.deepEqual(tv.errors, []);
    } finally { MIDERR.go = false; await forget(); await tvClose(tv); }
  }, true));

  await step('TV Audio: choosing a video\'s original clears the remembered dub language (also OK on the track already playing): the next video plays its own original; this video keeps the choice', async () => {
    const tv = await tvOpen();
    const fixture = nextFixture();
    const nextOf = { dubtv000011: 'dubtv000012', dubtv000012: 'dubtv000013', dubtv000013: 'dubtv000014' };
    await tv.route('**/youtubei/v1/next*', route => {
      const b = route.request().postDataJSON(), to = nextOf[b.videoId];
      if (!to) return route.fallback();
      const o = JSON.parse(JSON.stringify(fixture));
      o.currentVideoEndpoint.watchEndpoint.videoId = b.videoId;
      o.contents.twoColumnWatchNextResults.autoplay.autoplay.sets[0].autoplayVideo.watchEndpoint.videoId = to;
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(o) });
    });
    const audioMenu = async () => { await walkRow(tv, '#ytc-aub'); await tv.keyboard.press('Enter'); await tv.waitForSelector('#ytc-player.menu #ytc-menu'); };
    const choose = async name => { await audioMenu(); for (let i = 0; i < 3 && await tv.evaluate(n => document.activeElement.textContent !== n, name); i++) await tv.keyboard.press('ArrowDown'); assert.equal(await tv.evaluate(() => document.activeElement.textContent), name); await tv.keyboard.press('Enter'); await tv.waitForTimeout(500); await back(tv); };
    const lang = async (v, n0) => { await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 20000 }); const a = log.gv.slice(n0).filter(g => g.v === v && g.lang); return a.at(-1)?.lang; };
    try {
      await forget();
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=dubtv000011');
      await tv.waitForSelector('#ytc-aub.on');
      await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 20000 });
      // OK on the track already playing, the original: nothing to remember.
      await audioMenu();
      assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Russian (original)');
      await tv.keyboard.press('Enter');
      await tv.waitForTimeout(500);
      await back(tv);
      assert.equal((await stored()).p?.al, undefined);
      // A dub is remembered; escaping it to the original clears that.
      await choose('French');
      assert.equal((await stored()).p.al, 'fr');
      let n0 = log.gv.length;
      await tv.keyboard.press('MediaTrackNext');
      await tv.waitForFunction(() => new URLSearchParams(location.search).get('v') === 'dubtv000012', null, { timeout: 8000 });
      await tv.waitForSelector('#ytc-aub.on');
      assert.equal(await lang('dubtv000012', n0), 'fr');
      await audioMenu();
      await tv.focus('#ytc-menu [data-arg="au:ru.3"]');
      await tv.keyboard.press('Enter');
      await tv.waitForTimeout(500);
      await back(tv);
      assert.equal((await stored()).p?.al, undefined, 'the original is no language preference');
      n0 = log.gv.length;
      await tv.keyboard.press('MediaTrackNext');
      await tv.waitForFunction(() => new URLSearchParams(location.search).get('v') === 'dubtv000013', null, { timeout: 8000 });
      await tv.waitForSelector('#ytc-aub.on');
      assert.equal(await lang('dubtv000013', n0), 'ru', 'the next video: its own original, no dub');
      // Back on the video where the original was chosen: it stays.
      n0 = log.gv.length;
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForFunction(() => new URLSearchParams(location.search).get('v') === 'dubtv000012');
      await tv.waitForSelector('#ytc-aub.on');
      assert.equal(await lang('dubtv000012', n0), 'ru');
      assert.deepEqual(tv.errors, []);
    } finally { await forget(); await tvClose(tv); }
  });

  await step('TV focus prefetch: a link resting under the focus prefetches the track the player will start on (the remembered language), once', async () => {
    const tv = await tvOpen();
    try {
      await forget();
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', 'https://www.youtube.com/watch?v=dubtv000021');
      await tv.waitForSelector('#ytc-aub.on');
      await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 20000 });
      await walkRow(tv, '#ytc-aub');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-player.menu #ytc-menu');
      for (let i = 0; i < 3 && await tv.evaluate(() => document.activeElement.textContent !== 'French'); i++) await tv.keyboard.press('ArrowDown');
      await tv.keyboard.press('Enter');
      await tv.waitForTimeout(600);
      assert.equal((await stored()).p.al, 'fr');
      await tvGo(tv, 'code', 'https://www.youtube.com/');
      await tv.waitForSelector('.tvpc .tvb');
      await tv.waitForTimeout(1500);
      const n0 = log.gv.length, v = 'dubtv000022';
      await tv.evaluate(v => { const a = document.createElement('a'); a.href = `/watch?v=${v}`; a.className = 't'; a.id = 'e2e-link'; a.textContent = 'x'; document.querySelector('#ytc-main').prepend(a); a.focus(); }, v);
      for (let i = 0; i < 60 && !log.gv.slice(n0).some(g => g.v === v && g.lang); i++) await tv.waitForTimeout(100);
      const pre = log.gv.slice(n0).filter(g => g.v === v && g.lang).map(g => g.lang);
      assert.ok(pre.length && pre.every(l => l === 'fr'), 'the prefetch: ' + pre);
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.5, null, { timeout: 20000 });
      const all = log.gv.slice(n0).filter(g => g.v === v && g.lang).map(g => g.lang);
      assert.ok(all.every(l => l === 'fr'), 'no other track fetched: ' + all);
      assert.deepEqual(tv.errors, []);
    } finally { await forget(); await tvClose(tv); }
  });

  await step('TV a held OK is one press: Subscribe, its confirmation, Like and Dislike, a playlist row, a link; only typing repeats', async () => {
    const tv = await tvOpen();
    const hold = async (n = 6) => { for (let i = 0; i < n; i++) { await tv.keyboard.down('Enter'); await tv.waitForTimeout(40); } await tv.keyboard.up('Enter'); await tv.waitForTimeout(200); };
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'signed', `https://www.youtube.com/channel/${CH}`);
      await tv.waitForSelector('#ytc-sub-b');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'), null, { timeout: 8000 });
      await tv.focus('#ytc-sub-b');
      const text = () => tv.textContent('#ytc-sub-b');
      const c = () => [calls('subscription/subscribe').length, calls('subscription/unsubscribe').length];
      let c0 = c();
      await hold();
      assert.equal(await text(), 'Subscribed', 'held: subscribes once');
      assert.deepEqual(c(), [c0[0] + 1, c0[1]]);
      await tv.waitForTimeout(3300);
      c0 = c();
      await hold();
      assert.equal(await text(), 'Unsubscribe?', 'held: asks, and goes no further');
      assert.deepEqual(c(), c0);
      // Three quick presses: the question, then two too soon to have read it.
      await tv.waitForTimeout(3300);
      assert.equal(await text(), 'Subscribed', 'the question times out');
      c0 = c();
      for (let i = 0; i < 3; i++) await tv.keyboard.press('Enter');
      await tv.waitForTimeout(150);
      assert.equal(await text(), 'Unsubscribe?');
      assert.deepEqual(c(), c0);
      // The player row.
      await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
      await tv.waitForSelector('#ytc-like', { state: 'attached' });
      await tv.waitForSelector('#ytc-tvch .tvch');
      await paused(tv);
      await walkRow(tv, '#ytc-tvlk');
      const l0 = calls('like/like').length, r0 = calls('like/removelike').length;
      await hold();
      assert.equal(await tv.getAttribute('#ytc-tvlk', 'aria-pressed'), 'true');
      assert.deepEqual([calls('like/like').length - l0, calls('like/removelike').length - r0], [1, 0], 'one rating');
      await tv.keyboard.press('ArrowRight');
      const d0 = calls('like/dislike').length;
      await hold();
      assert.equal(calls('like/dislike').length - d0, 1);
      // The Channel link: its page opens; the card focused there stays closed.
      await walkRow(tv, '.tvch');
      await hold(8);
      await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav !== 'watch', null, { timeout: 8000 });
      await tv.waitForTimeout(400);
      assert.notEqual(await tv.evaluate(() => location.pathname), '/watch', 'held: no further than its page');
      // Typing does repeat.
      await tvGo(tv, 'code', 'https://www.youtube.com/');
      await tv.waitForSelector('#ytc-side .rl[data-nav=search]');
      await tv.focus('#ytc-side .rl[data-nav=search]');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-tvk .tvk-keys button'));
      await hold(4);
      assert.equal((await tv.inputValue('#ytc-q')).length, 4, 'a held letter types');
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV a held OK on the seek bar is one play or pause, and the hold that opens a video from a card does not pause it', async () => {
    const tv = await tvOpen();
    const hold = async (n, ms = 40) => { for (let i = 0; i < n; i++) { await tv.keyboard.down('Enter'); await tv.waitForTimeout(ms); } await tv.keyboard.up('Enter'); await tv.waitForTimeout(200); };
    const count = () => tv.evaluate(() => { window.__toggles = []; if (!window.__togglesOn) { window.__togglesOn = true; for (const t of ['play', 'pause']) document.addEventListener(t, () => window.__toggles.push(t), true); } });
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${VID}`);
      await playing(tv, 0.5);
      await tv.waitForFunction(() => document.activeElement?.id === 'ytc-seek');
      await tv.keyboard.press('ArrowUp'); // (the overlay is awake: the next OK is play or pause, not its wake-up)
      await count();
      await hold(11);
      assert.deepEqual(await tv.evaluate(() => window.__toggles), ['pause'], 'one pause in eleven key events');
      assert.equal((await video(tv)).paused, true);
      await count();
      await hold(11);
      assert.deepEqual(await tv.evaluate(() => window.__toggles), ['play'], 'one play');
      assert.equal((await video(tv)).paused, false);
      // Separate presses still toggle.
      await count();
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc-video').paused);
      await tv.waitForTimeout(100);
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => window.__toggles.length >= 2);
      assert.deepEqual(await tv.evaluate(() => window.__toggles), ['pause', 'play']);

      // A card held for a second and a half: the video opens and plays.
      await tvGo(tv, 'code', 'https://www.youtube.com/results?search_query=held+ok');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'));
      await tv.locator('#ytc-list a.t[href^="/watch?"]').first().focus();
      await count();
      await hold(30, 50);
      await tv.waitForFunction(() => location.pathname === '/watch');
      await playing(tv, 0.3);
      await tv.waitForTimeout(300);
      const t = await tv.evaluate(() => window.__toggles);
      assert.ok(!t.includes('pause'), 'not paused by the hold: ' + t);
      assert.equal((await video(tv)).paused, false);
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV how a video was left for its channel is forgotten once it is opened anew: Back to it later returns where it was last, playing', async () => {
    const tv = await tvOpen();
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${VID}`);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv, 1);
      await tv.evaluate(() => { document.getElementById('ytc-video').currentTime = 8; });
      await tv.waitForFunction(() => document.getElementById('ytc-video').currentTime >= 8);
      await tv.waitForFunction(() => document.activeElement?.id === 'ytc-seek');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc-video').paused);
      await walkRow(tv, '.tvch');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav !== 'watch' && document.activeElement?.matches('#ytc-list a.t'), null, { timeout: 8000 });
      // Home by the rail, the same video from a card.
      await tv.focus('#ytc-side .rl[data-nav=home]');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav === 'home', null, { timeout: 8000 });
      await tv.waitForTimeout(500);
      await tv.evaluate(v => { const a = document.createElement('a'); a.href = `/watch?v=${v}`; a.className = 't'; a.textContent = 'x'; document.querySelector('#ytc-main').prepend(a); a.focus(); }, VID);
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc-video') && location.pathname === '/watch', null, { timeout: 8000 });
      await tv.waitForFunction(() => { const v = document.getElementById('ytc-video'); return v.readyState >= 2 && v.currentTime > 0; }, null, { timeout: 15000 });
      await tv.waitForTimeout(300);
      await tv.evaluate(() => { const v = document.getElementById('ytc-video'); v.currentTime = 24.5; if (v.paused) v.play(); });
      await tv.waitForFunction(() => { const v = document.getElementById('ytc-video'); return v.currentTime >= 24.5 && !v.paused; }, null, { timeout: 8000 });
      await tv.waitForSelector('#ytc-tvch .tvch');
      await tv.keyboard.press('MediaTrackNext');
      await tv.waitForFunction(v => new URLSearchParams(location.search).get('v') !== v, VID, { timeout: 8000 });
      await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 15000 });
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForFunction(v => new URLSearchParams(location.search).get('v') === v, VID, { timeout: 8000 });
      await tv.waitForFunction(() => { const v = document.getElementById('ytc-video'); return v?.readyState >= 2 && v.currentTime > 0; }, null, { timeout: 15000 });
      await tv.waitForTimeout(800);
      const v = await video(tv);
      assert.ok(!v.paused && v.t >= 24, `where it was last, playing, not an old 8 s paused: ${JSON.stringify(v)}`);
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV Back from a channel page: one step only on the entry opened from a video; the same channel reached another way goes to the rail first, and the video comes back as it was left: paused stays paused, a video opened with t resumes where it was', async () => {
    const tv = await tvOpen();
    const toChannel = async () => {
      await walkRow(tv, '.tvch');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav !== 'watch' && document.activeElement?.matches('#ytc-list a.t'), null, { timeout: 8000 });
    };
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${VID}`);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv, 1);
      await tv.waitForFunction(() => document.activeElement?.id === 'ytc-seek');
      const href = await tv.evaluate(() => document.querySelector('#ytc-tvch .tvch').getAttribute('href'));
      await toChannel();
      assert.deepEqual(await back(tv), { handled: false, exit: false }, 'opened from the video: Back leaves it');
      // A tab of that channel: as far from the video, so Back walks the tabs and then returns to it.
      await tv.focus('#ytc-ct-streams');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => location.pathname.endsWith('/streams') && document.querySelector('#ytc-ct-streams[aria-current=page]') && document.querySelector('#ytc-list .c:not(.sk)'), null, { timeout: 8000 });
      await tv.focus('#ytc-list a.t');
      assert.deepEqual(await back(tv), { handled: false, exit: false }, 'a tab of a channel opened from the video: Back steps back');
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForFunction(h => location.pathname === h && document.querySelector('#ytc-list .c:not(.sk)'), href, { timeout: 8000 });
      await tv.focus('#ytc-list a.t');
      assert.deepEqual(await back(tv), { handled: false, exit: false }, 'and that entry still leads to the video');
      // Home by the rail, then a link to the same channel: another history entry.
      await tv.focus('#ytc-side .rl[data-nav=home]');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav === 'home', null, { timeout: 8000 });
      await tv.waitForTimeout(800);
      await tv.evaluate(h => { const a = document.createElement('a'); a.href = h; a.className = 't'; a.textContent = 'x'; document.querySelector('#ytc-main').prepend(a); a.focus(); }, href);
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(h => location.pathname === h && document.getElementById('ytc')?.dataset.nav !== 'home', href, { timeout: 8000 });
      await tv.waitForTimeout(800);
      assert.deepEqual(await back(tv), { handled: true, exit: false }, 'reached from Home: the rail first');
      assert.ok(await isIn(tv, '#ytc-side'));
      // Back through the history to the entry opened from the video: one step again.
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForFunction(() => document.getElementById('ytc')?.dataset.nav === 'home', null, { timeout: 8000 });
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForFunction(h => location.pathname === h, href, { timeout: 8000 });
      await tv.waitForSelector('#ytc-list a.t');
      await tv.focus('#ytc-list a.t');
      assert.deepEqual(await back(tv), { handled: false, exit: false }, 'that entry keeps its rule');

      // Paused stays paused, at its place.
      await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${VID}`);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv, 1);
      await tv.evaluate(() => { document.getElementById('ytc-video').currentTime = 15; });
      await tv.waitForFunction(() => document.getElementById('ytc-video').currentTime >= 15);
      await tv.waitForFunction(() => document.activeElement?.id === 'ytc-seek');
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => document.getElementById('ytc-video').paused);
      const t0 = (await video(tv)).t;
      await toChannel();
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForFunction(t => document.getElementById('ytc-video')?.readyState >= 2 && Math.abs(document.getElementById('ytc-video').currentTime - t) < 1, t0, { timeout: 15000 });
      await tv.waitForTimeout(1500);
      const v1 = await video(tv);
      assert.ok(v1.paused && Math.abs(v1.t - t0) < 1, `paused at ${t0}: ${JSON.stringify(v1)}`);
      // Playing stays playing.
      await tv.keyboard.press('Enter');
      await tv.waitForFunction(() => !document.getElementById('ytc-video').paused);
      await toChannel();
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForFunction(() => { const v = document.getElementById('ytc-video'); return v && !v.paused && v.currentTime > 1; }, null, { timeout: 15000 });

      // Opened with t: where it was left, not the URL's t.
      await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${VID}&t=5`);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv, 5);
      await tv.evaluate(() => { document.getElementById('ytc-video').currentTime = 22; });
      await tv.waitForFunction(() => document.getElementById('ytc-video').currentTime >= 22);
      await tv.waitForFunction(() => document.activeElement?.id === 'ytc-seek');
      await toChannel();
      await tv.goBack({ waitUntil: 'commit' });
      await tv.waitForFunction(() => document.getElementById('ytc-video')?.readyState >= 2, null, { timeout: 15000 });
      const t2 = (await video(tv)).t;
      assert.ok(t2 >= 21, `left at 22+, resumed at ${t2}`);
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV Previous in a playlist: the playlist\'s previous item, with the list kept; on the first item this video from the start', async () => {
    const tv = await tvOpen();
    const pl = JSON.parse(zlib.gunzipSync(fs.readFileSync(path.join(__dirname, 'fixtures/next_list.json.gz')))).contents.twoColumnWatchNextResults.playlist.playlist;
    const items = pl.contents.map(c => c.playlistPanelVideoRenderer.videoId), id = pl.playlistId;
    const on = v => tv.waitForFunction(([v, id]) => { const q = new URLSearchParams(location.search); return q.get('v') === v && q.get('list') === id && document.getElementById('ytc-video'); }, [v, id], { timeout: 10000 });
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${items[2]}&list=${id}`);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv);
      await tv.keyboard.press('MediaTrackPrevious');
      await on(items[1]);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv);
      await tv.keyboard.press('MediaTrackNext');
      await on(items[2]);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv);
      await tv.keyboard.press('MediaTrackPrevious');
      await on(items[1]);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv);
      await tv.keyboard.press('MediaTrackPrevious');
      await on(items[0]);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv, 3);
      // The first item: nothing before it in this session (the way here was walked back): it restarts.
      await tv.keyboard.press('MediaTrackPrevious');
      await tv.waitForFunction(() => document.getElementById('ytc-video').currentTime < 2, null, { timeout: 3000 });
      assert.equal(new URL(tv.url()).searchParams.get('v'), items[0]);
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });

  await step('TV Save, the odd moments: pressed before the page\'s data came (no request without a video, it fills itself); an answer for the last video still out when the panel opens; forty playlists stay inside the safe area', async () => {
    const tv = await tvOpen();
    const fixture = nextFixture();
    let holdNext = false, release = null, holdList = null;
    const held = [];
    await tv.route('**/youtubei/v1/next*', async route => {
      const b = route.request().postDataJSON();
      if (holdNext && b.videoId === VID && b.context?.client?.clientName !== 'TVHTML5') await new Promise(r => { release = r; });
      return route.fallback();
    });
    await tv.route('**/youtubei/v1/playlist/get_add_to_playlist*', async route => {
      if (holdList && route.request().postDataJSON().videoIds[0] === holdList) await new Promise(r => held.push(r));
      return route.fallback();
    });
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      // Pressed at once: Loading, no request for no video; it fills when the page's data is in.
      holdNext = true;
      const g0 = calls('playlist/get_add_to_playlist').length;
      await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
      await tv.waitForSelector('#ytc-tvsv');
      await tv.focus('#ytc-tvsv');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-tvsave.on');
      assert.match(await tv.textContent('#ytc-tvsave-body'), /Loading your playlists/);
      await tv.waitForTimeout(300);
      assert.equal(calls('playlist/get_add_to_playlist').slice(g0).filter(c => !c.body.videoIds[0]).length, 0, 'no request without a video');
      holdNext = false;
      if (release) release();
      await tv.waitForSelector('#ytc-tvsave .ya-opt', { timeout: 8000 });
      assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-tvsave .ya-opt')), 'the first playlist takes the focus');
      assert.ok(calls('playlist/get_add_to_playlist').slice(g0).every(c => c.body.videoIds[0] === VID));
      await back(tv);

      // A's quiet check still out, the panel opens on B: B's list comes.
      const auto = fixture.contents.twoColumnWatchNextResults.autoplay.autoplay.sets[0].autoplayVideo.watchEndpoint.videoId;
      holdList = 'dubtw000031';
      await tvGo(tv, 'signed', 'https://www.youtube.com/watch?v=dubtw000031');
      await tv.waitForSelector('#ytc-tvch .tvch');
      for (let i = 0; i < 60 && !held.length; i++) await tv.waitForTimeout(100);
      assert.equal(held.length, 1, 'the quiet check for the first video is out');
      await tv.keyboard.press('MediaTrackNext');
      await tv.waitForFunction(a => new URLSearchParams(location.search).get('v') === a, auto, { timeout: 8000 });
      await tv.waitForSelector('#ytc-tvch .tvch');
      await paused(tv);
      await walkRow(tv, '#ytc-tvsv');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-tvsave.on');
      assert.match(await tv.textContent('#ytc-tvsave-body'), /Loading/);
      held.splice(0).forEach(r => r());
      await tv.waitForSelector('#ytc-tvsave .ya-opt', { timeout: 8000 });
      assert.equal(calls('playlist/get_add_to_playlist').at(-1).body.videoIds[0], auto);
      await back(tv);
      holdList = null;
      assert.deepEqual(tv.errors, []);
    } finally { if (release) release(); held.splice(0).forEach(r => r()); await tvClose(tv); }
    // Forty playlists.
    const tv2 = await tvOpen();
    const pls = Array.from({ length: 40 }, (_, i) => ({ playlistAddToOptionRenderer: { playlistId: i ? `PLx${i}` : 'WL', title: { simpleText: i ? `Playlist number ${i}` : 'Watch later' }, privacy: 'PRIVATE', containsSelectedVideos: 'NONE' } }));
    await tv2.route('**/youtubei/v1/playlist/get_add_to_playlist*', route => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ contents: [{ addToPlaylistRenderer: { playlists: pls } }] }) }));
    try {
      for (const [W, H] of TVSIZES) {
        await tv2.setViewportSize({ width: W, height: H });
        await tvGo(tv2, 'signed', `https://www.youtube.com/watch?v=${VID}`);
        await tv2.waitForSelector('#ytc-tvch .tvch');
        await paused(tv2);
        await walkRow(tv2, '#ytc-tvsv');
        await tv2.keyboard.press('Enter');
        await tv2.waitForSelector('#ytc-tvsave .ya-opt');
        for (const key of ['ArrowDown', 'ArrowUp']) {
          for (let i = 0; i < 42; i++) {
            await tv2.keyboard.press(key);
            if (i % 7 === 0 || i > 38) assert.deepEqual(await tvAudit(tv2, `forty playlists ${key} ${i} at ${W}`), []);
          }
        }
        assert.equal(await tv2.evaluate(() => document.activeElement.dataset.act), 'tv-save-close', 'up to Back to video');
        await back(tv2);
      }
      assert.deepEqual(tv2.errors, []);
    } finally { await tvClose(tv2); }
  });

  await step('TV the refusal notice stays clear of an open panel: beside Comments and a menu, never over their headers; hidden over Search and in More info (its own line)', async () => {
    const tv = await tvOpen();
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      const refuse = async () => {
        await walkRow(tv, '#ytc-tvlk');
        ACTS.fail.add('like/like'); ACTS.delay = 500;
        await tv.keyboard.press('Enter');
      };
      const settle = async () => {
        await tv.waitForFunction(() => document.getElementById('ytc-tvactnote').textContent, null, { timeout: 5000 });
        ACTS.fail.clear(); ACTS.delay = 0;
      };
      await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
      await tv.waitForSelector('#ytc-like', { state: 'attached' });
      await tv.waitForSelector('#ytc-tvch .tvch');
      await paused(tv);
      await refuse();
      await tv.focus('[data-act=tv-comments]');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-comments.on');
      await settle();
      const c = await tv.evaluate(() => { const n = document.getElementById('ytc-tvactnote'), r = n.getBoundingClientRect(); return { shown: n.checkVisibility(), r: r.right, panel: document.getElementById('ytc-comments').getBoundingClientRect().left }; });
      assert.ok(c.shown && c.r <= c.panel, `beside Comments: ${JSON.stringify(c)}`);
      assert.deepEqual(await tvAudit(tv, 'notice beside Comments'), []);
      await back(tv);
      await tv.waitForFunction(() => !document.getElementById('ytc-tvactnote').textContent, null, { timeout: 9000 });
      await refuse();
      await tv.focus('#ytc-speed');
      await tv.keyboard.press('Enter');
      await tv.waitForSelector('#ytc-player.menu #ytc-menu');
      await settle();
      const m = await tv.evaluate(() => { const n = document.getElementById('ytc-tvactnote'), r = n.getBoundingClientRect(); return { shown: n.checkVisibility(), r: r.right, panel: document.getElementById('ytc-menu').getBoundingClientRect().left }; });
      assert.ok(m.shown && m.r <= m.panel, `beside a menu: ${JSON.stringify(m)}`);
      await back(tv);
      await tv.waitForFunction(() => !document.getElementById('ytc-tvactnote').textContent, null, { timeout: 9000 });
      await refuse();
      await tv.keyboard.press('/');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-tvk .tvk-keys button'));
      await settle();
      assert.ok(!(await tv.evaluate(() => document.getElementById('ytc-tvactnote').checkVisibility())), 'not over Search');
      assert.deepEqual(tv.errors, []);
    } finally { ACTS.fail.clear(); ACTS.delay = 0; await tvClose(tv); }
  });

  await step('TV caption auto-translate (960×540 and 1920×1080): Captions lists a short Auto-translate section led by the device\'s language, Russian, English; the choice is fetched with tlang and remembered for the next videos', async () => {
    const tv = await tvOpen();
    try {
      for (const [W, H] of TVSIZES) {
        await forget();
        await tv.setViewportSize({ width: W, height: H });
        await tv.goto('https://www.youtube.com/', { waitUntil: 'commit' });
        await tv.evaluate(() => localStorage.setItem('ytc-test-lang', 'de-DE'));
        await tvGo(tv, 'signed', 'https://www.youtube.com/watch?v=ccjapanese1');
        await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 20000 });
        await tv.waitForFunction(() => document.activeElement?.id === 'ytc-seek');
        await tv.waitForTimeout(400);
        await walkRow(tv, '#ytc-ccb');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-player.menu #ytc-menu');
        const list = await tv.evaluate(() => [...document.querySelectorAll('#ytc-menu .mls > :is(button,p)')].map(e => e.textContent));
        assert.deepEqual(list, ['Japanese (auto-generated)', 'Japanese', 'Auto-translate', 'German', 'Russian', 'English'], 'the device\'s language first');
        assert.deepEqual(await tvAudit(tv, `captions translate ${W}`), []);
        await tvShot(tv, 'player-captions-translate');
        // To Russian with the remote: the row under German.
        await tv.keyboard.press('ArrowDown');
        for (let i = 0; i < 8 && await tv.evaluate(() => document.activeElement.textContent !== 'Russian'); i++) await tv.keyboard.press('ArrowDown');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Russian');
        const n0 = log.timedtext.length;
        await tv.keyboard.press('Enter');
        for (let i = 0; i < 50 && !log.timedtext.slice(n0).length; i++) await tv.waitForTimeout(100);
        const url = log.timedtext.slice(n0).at(-1) || '';
        assert.ok(/[?&]lang=ja\b/.test(url) && /[?&]tlang=ru\b/.test(url) && /fmt=json3/.test(url), url);
        assert.ok(await tv.evaluate(() => document.querySelector('#ytc-menu .mls button.on')?.textContent === 'Russian'), 'marked');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-ccb');
        await tv.waitForTimeout(500);
        assert.equal((await stored()).p.cl, 'ru');
        // The next video starts in it.
        const n1 = log.timedtext.length;
        await tvGo(tv, 'signed', 'https://www.youtube.com/watch?v=ccjapanese2');
        for (let i = 0; i < 80 && !log.timedtext.slice(n1).length; i++) await tv.waitForTimeout(100);
        assert.ok(/[?&]tlang=ru\b/.test(log.timedtext.slice(n1).at(-1) || ''), 'remembered: ' + log.timedtext.slice(n1).at(-1));
        // A track of the video's own ends it (the fixed preference again).
        await tv.waitForFunction(() => document.getElementById('ytc-video')?.currentTime > 0.2, null, { timeout: 20000 });
        await walkRow(tv, '#ytc-ccb');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-player.menu #ytc-menu');
        await tv.focus('#ytc-menu [data-arg="cc:1"]');
        await tv.keyboard.press('Enter');
        await tv.waitForTimeout(500);
        assert.equal((await stored()).p.cl, undefined);
        await back(tv);
      }
      assert.deepEqual(tv.errors, []);
    } finally { await tv.evaluate(() => localStorage.removeItem('ytc-test-lang')).catch(() => {}); await forget(); await tvClose(tv); }
  });

  await step('TV Like and Dislike (960×540 and 1920×1080): mutual, signed in only, with the TV token; refused: back with a readable notice', async () => {
    const tv = await tvOpen();
    const pressed = () => tv.evaluate(() => [document.getElementById('ytc-tvlk').getAttribute('aria-pressed'), document.getElementById('ytc-tvdl').getAttribute('aria-pressed')]);
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
        await tv.waitForSelector('#ytc-like', { state: 'attached' });
        await paused(tv);
        await walkRow(tv, '#ytc-tvlk');
        assert.deepEqual(await pressed(), ['false', 'false']);
        const l0 = calls('like/like').length, d0 = calls('like/dislike').length, r0 = calls('like/removelike').length;
        await tv.keyboard.press('Enter');
        assert.deepEqual(await pressed(), ['true', 'false']);
        // A clear state: solid, focused or not, and the label says so.
        const look = id => tv.evaluate(id => { const b = document.getElementById(id), s = getComputedStyle(b); return [s.backgroundColor, s.color, b.querySelector('.tvpl').textContent]; }, id);
        assert.deepEqual(await look('ytc-tvlk'), ['rgb(241, 241, 241)', 'rgb(15, 15, 15)', 'Liked']);
        await tv.waitForTimeout(150);
        assert.equal(calls('like/like').length, l0 + 1);
        accountCall('like/like');
        assert.equal(calls('like/like').at(-1).body.target.videoId, VID);
        await tv.keyboard.press('ArrowRight');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvdl');
        assert.equal((await label(tv)).text, 'Dislike');
        assert.deepEqual(await look('ytc-tvlk'), ['rgb(241, 241, 241)', 'rgb(15, 15, 15)', 'Liked'], 'not focused: as solid');
        assert.deepEqual(await tvAudit(tv, `row dislike ${W}`), []);
        await tvShot(tv, 'player-row-dislike');
        await tv.keyboard.press('Enter');
        assert.deepEqual(await pressed(), ['false', 'true'], 'a dislike ends the like');
        assert.deepEqual(await look('ytc-tvdl'), ['rgb(241, 241, 241)', 'rgb(15, 15, 15)', 'Disliked']);
        assert.equal((await look('ytc-tvlk'))[2], 'Like');
        await tv.waitForTimeout(150);
        assert.equal(calls('like/dislike').length, d0 + 1);
        accountCall('like/dislike');
        await tvShot(tv, 'player-row-disliked');
        await tv.keyboard.press('Enter');
        assert.deepEqual(await pressed(), ['false', 'false'], 'OK again takes it back');
        await tv.waitForTimeout(150);
        assert.equal(calls('like/removelike').length, r0 + 1);
        accountCall('like/removelike');
        // Refused: rolled back, the reason over the picture.
        ACTS.fail.add('like/dislike');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.getElementById('ytc-tvdl').getAttribute('aria-pressed') === 'false' && document.getElementById('ytc-tvactnote').textContent);
        ACTS.fail.clear();
        const refused = await note(tv);
        assert.ok(refused.shown && /Rating not saved/.test(refused.text) && refused.r <= W * 0.975 + 1, JSON.stringify(refused));
        assert.deepEqual(await tvAudit(tv, `rating refused ${W}`), []);
        await tvShot(tv, 'player-rating-refused');
        // Signed out: neither is in the row.
        await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${VID}`);
        await tv.waitForSelector('#ytc-like', { state: 'attached' });
        await playing(tv);
        assert.ok(await tv.evaluate(() => !document.getElementById('ytc-tvlk').checkVisibility() && !document.getElementById('ytc-tvdl').checkVisibility() && !document.getElementById('ytc-tvsv').checkVisibility()), 'signed in only');
      }
      assert.deepEqual(tv.errors, []);
    } finally { ACTS.fail.clear(); await tvClose(tv); }
  });

  await step('TV Save to playlist (960×540 and 1920×1080): a panel of the account\'s playlists from the row, OK toggles a row with the TV token, refused: back with the reason in the panel; Back returns to Save', async () => {
    const tv = await tvOpen();
    const row = id => tv.evaluate(id => { const r = document.getElementById(id); return r && { on: r.getAttribute('aria-checked'), cls: r.className, text: r.textContent }; }, id);
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
        await paused(tv);
        await tv.waitForSelector('#ytc-tvch .tvch');
        await walkRow(tv, '#ytc-tvsv');
        assert.equal((await label(tv)).text, 'Save');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-tvsave.on');
        await tv.waitForSelector('#ytc-tvsave .ya-opt');
        const box = await tv.evaluate(() => { const r = document.getElementById('ytc-tvsave').getBoundingClientRect(); return { r: r.right, t: r.top, b: r.bottom, w: r.width, W: innerWidth, H: innerHeight, modal: document.getElementById('ytc-tvsave').getAttribute('aria-modal') }; });
        assert.ok(box.r === box.W && box.t === 0 && box.b === box.H && box.w <= box.W * .37 && box.modal === 'true', JSON.stringify(box));
        assert.equal(await tv.locator('#ytc-tvsave form').count(), 0, 'no form the remote cannot fill');
        assert.ok(await tv.evaluate(() => document.activeElement.matches('#ytc-tvsave .ya-opt')), 'the first playlist has the focus');
        assert.ok(await tv.evaluate(() => { const d = document.querySelector('#ytc-player .pctl'); return getComputedStyle(d).opacity === '0'; }), 'the overlay rests under the panel');
        assert.deepEqual(await tvAudit(tv, `save panel ${W}`), []);
        await tvShot(tv, 'player-save');
        assert.equal((await row('ytc-pl-0')).on, 'true', 'Watch later: already in it');
        assert.equal((await row('ytc-pl-1')).on, 'false');
        const w = await tvWalk(tv, '#ytc-tvsave button', 10);
        assert.deepEqual([w.dead, w.missing], [[], []], `save panel walk ${W}`);
        // ↓ to the second row, OK adds it.
        await tv.focus('#ytc-pl-0');
        await tv.keyboard.press('ArrowDown');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-pl-1');
        const n0 = calls('browse/edit_playlist').length;
        await tv.keyboard.press('Enter');
        assert.equal((await row('ytc-pl-1')).on, 'true');
        await tv.waitForTimeout(150);
        assert.equal(calls('browse/edit_playlist').length, n0 + 1);
        const c = accountCall('browse/edit_playlist');
        assert.deepEqual([c.body.playlistId, c.body.actions], ['PLe2e', [{ addedVideoId: VID, action: 'ACTION_ADD_VIDEO' }]]);
        await tvShot(tv, 'player-save-checked');
        // Refused: unchecked again, the reason in the panel.
        ACTS.fail.add('browse/edit_playlist');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.getElementById('ytc-save-m').textContent);
        ACTS.fail.clear();
        assert.equal((await row('ytc-pl-1')).on, 'true', 'rolled back to what YouTube confirmed');
        assert.match(await tv.textContent('#ytc-save-m'), /Removing not saved/);
        assert.ok(await tv.evaluate(() => document.getElementById('ytc-save-m').checkVisibility()), 'readable');
        assert.deepEqual(await tvAudit(tv, `save refused ${W}`), []);
        await tvShot(tv, 'player-save-refused');
        assert.deepEqual(await back(tv), { handled: true, exit: false });
        assert.equal(await tv.locator('#ytc-tvsave.on').count(), 0);
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvsv', 'Back returns to Save');
        assert.ok(await tv.evaluate(() => document.getElementById('ytc-tvsv').classList.contains('on')), 'the action shows it is saved');
        // Opened again, nothing is asked twice.
        const g0 = calls('playlist/get_add_to_playlist').length;
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-tvsave .ya-opt');
        assert.equal(calls('playlist/get_add_to_playlist').length, g0);
        await back(tv);
        // Refused at the start: the reason, Back to video usable.
        ACTS.fail.add('playlist/get_add_to_playlist');
        await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
        await paused(tv);
        await walkRow(tv, '#ytc-tvsv');
        await tv.keyboard.press('Enter');
        await tv.waitForSelector('#ytc-tvsave.on');
        await tv.waitForFunction(() => /HTTP 500/.test(document.getElementById('ytc-tvsave-body').textContent), null, { timeout: 5000 });
        ACTS.fail.clear();
        assert.deepEqual(await tvAudit(tv, `save failed ${W}`), []);
        await tv.focus('#ytc-tvsave [data-act=tv-save-close]');
        await tv.keyboard.press('Enter');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvsv');
      }
      assert.deepEqual(tv.errors, []);
    } finally { ACTS.fail.clear(); await tvClose(tv); }
  });

  await step('TV Next and Previous on the remote: Next opens Up next\'s first, Previous the video before it in this session, or restarts when there is none; the row keeps working after them', async () => {
    const tv = await tvOpen();
    const fixture = nextFixture();
    const auto = fixture.contents.twoColumnWatchNextResults.autoplay.autoplay.sets[0].autoplayVideo.watchEndpoint.videoId;
    const on = v => tv.waitForFunction(v => new URLSearchParams(location.search).get('v') === v && document.getElementById('ytc-video'), v, { timeout: 10000 });
    try {
      await tv.setViewportSize({ width: 960, height: 540 });
      await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${VID}`);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv, 3);
      // No video before it: Previous restarts it.
      await tv.keyboard.press('MediaTrackPrevious');
      await tv.waitForFunction(() => document.getElementById('ytc-video').currentTime < 2, null, { timeout: 3000 });
      assert.equal(new URL(tv.url()).searchParams.get('v'), VID, 'the same video');
      // Over the search screen they do nothing: what is being typed stays.
      await tv.keyboard.press('/');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-tvk .tvk-keys button'));
      const typing = tv.url();
      await tv.keyboard.press('MediaTrackNext');
      await tv.keyboard.press('MediaTrackPrevious');
      await tv.waitForTimeout(400);
      assert.equal(tv.url(), typing);
      assert.equal(await tv.getAttribute('#ytc', 'data-tvk'), '1', 'the keyboard stays up');
      await back(tv); await back(tv);
      // Next: Up next's first.
      const depth = await tv.evaluate(() => history.length);
      await tv.keyboard.press('MediaTrackNext');
      await on(auto);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv);
      // Previous: back to it, a step back through the history: as if Next was never pressed.
      await tv.keyboard.press('MediaTrackPrevious');
      await on(VID);
      assert.equal(await tv.evaluate(() => history.length), depth + 1, 'no new entry');
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv);
      // And none before that: it restarts again.
      await tv.evaluate(() => { document.getElementById('ytc-video').currentTime = 10; });
      await tv.waitForFunction(() => document.getElementById('ytc-video').currentTime >= 10);
      await tv.keyboard.press('MediaTrackPrevious');
      await tv.waitForFunction(() => document.getElementById('ytc-video').currentTime < 5, null, { timeout: 3000 });
      assert.equal(new URL(tv.url()).searchParams.get('v'), VID);
      await tv.waitForFunction(() => document.activeElement?.closest('#ytc-player'));
      await walkRow(tv, '[data-act=tv-info]');
      // A page that is not a video ignores them.
      await tvGo(tv, 'code', 'https://www.youtube.com/results?search_query=keys');
      await tv.waitForFunction(() => document.activeElement?.matches('#ytc-list a.t'));
      const u = tv.url();
      await tv.keyboard.press('MediaTrackNext');
      await tv.keyboard.press('MediaTrackPrevious');
      assert.equal(tv.url(), u);
      assert.deepEqual(tv.errors, []);
    } finally { await tvClose(tv); }
  });
};
