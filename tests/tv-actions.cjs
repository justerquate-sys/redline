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
    for (let i = 0; i < 3 && !(await here()); i++) await tv.keyboard.press('ArrowLeft');
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
  const note = tv => tv.evaluate(() => { const n = document.getElementById('ytc-tvnote'), r = n.getBoundingClientRect(); return { text: n.textContent, shown: n.checkVisibility(), l: r.left, r: r.right, t: r.top, b: r.bottom }; });

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

  await step('TV Subscribe (960×540 and 1920×1080): on the channel page under the header and in the player row; one state on every surface; a second OK confirms unsubscribing; refused: back with a readable notice; signed out → Sign in; the account\'s own state shows', async () => {
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
        const place = await tv.evaluate(() => { const h = document.querySelector('.feed>.ch').getBoundingClientRect(), b = document.getElementById('ytc-sub-b').getBoundingClientRect(), c = document.querySelector('#ytc-list .c').getBoundingClientRect(); return { under: b.top >= h.bottom, over: b.bottom <= c.top, x: Math.abs(b.left - c.left) }; });
        assert.ok(place.under && place.over && place.x <= 1, `under the header, over the first card: ${JSON.stringify(place)}`);
        await tv.keyboard.press('ArrowUp');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-sub-b', '↑ from the first card');
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
        await tv.keyboard.press('Enter');
        assert.equal((await sub('ytc-sub-b')).text, 'Subscribe');
        await tv.waitForTimeout(150);
        assert.equal(calls('subscription/unsubscribe').length, u0 + 1);
        accountCall('subscription/unsubscribe');
        // Refused: the state comes back, and the reason is on screen.
        ACTS.fail.add('subscription/subscribe');
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(() => document.getElementById('ytc-sub-b').textContent === 'Subscribe' && document.getElementById('ytc-tvnote').textContent);
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
        assert.equal(await tv.evaluate(() => document.activeElement.dataset.act), 'tv-signin');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Subscribe');
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
        await tv.keyboard.press('Enter'); await tv.keyboard.press('Enter');
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
        account = { contents: { singleColumnWatchNextResults: { results: { results: { contents: [{ itemSectionRenderer: { contents: [{ videoOwnerRenderer: { subscriptionButton: { subscribed: true } } }] } }] } } } }, likeStatus: 'LIKE' };
        await tvGo(tv, 'signed', `https://www.youtube.com/watch?v=${VID}`);
        await tv.waitForFunction(() => document.getElementById('ytc-tvsubr-b')?.textContent === 'Subscribed' && document.getElementById('ytc-tvlk')?.getAttribute('aria-pressed') === 'true', null, { timeout: 8000 });
        await tvGo(tv, 'signed', `https://www.youtube.com/channel/${CH}`);
        await tv.waitForFunction(() => document.getElementById('ytc-sub-b')?.textContent === 'Subscribed', null, { timeout: 8000 });
        // A shape it cannot read changes nothing.
        account = { responseContext: {} };
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
        await tv.waitForTimeout(150);
        assert.equal(calls('like/like').length, l0 + 1);
        accountCall('like/like');
        assert.equal(calls('like/like').at(-1).body.target.videoId, VID);
        await tv.keyboard.press('ArrowRight');
        assert.equal(await tv.evaluate(() => document.activeElement.id), 'ytc-tvdl');
        assert.equal((await label(tv)).text, 'Dislike');
        assert.deepEqual(await tvAudit(tv, `row dislike ${W}`), []);
        await tvShot(tv, 'player-row-dislike');
        await tv.keyboard.press('Enter');
        assert.deepEqual(await pressed(), ['false', 'true'], 'a dislike ends the like');
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
        await tv.waitForFunction(() => document.getElementById('ytc-tvdl').getAttribute('aria-pressed') === 'false' && document.getElementById('ytc-tvnote').textContent);
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
      // Next: Up next's first.
      await tv.keyboard.press('MediaTrackNext');
      await on(auto);
      await tv.waitForSelector('#ytc-tvch .tvch');
      await playing(tv);
      // Previous: back to it.
      await tv.keyboard.press('MediaTrackPrevious');
      await on(VID);
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
