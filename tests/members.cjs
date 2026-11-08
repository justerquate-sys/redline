/* Members-only videos: the card's label, the switch, next-video selection, learning from a click and the
   verdict panels. Run inside e2e.cjs's offline harness; the badge shapes are the real signed-out ones (8 Nov 2026,
   tests/fixtures/members.json.gz is the recording itself). */
const assert = require('node:assert/strict');
const path = require('node:path');

// YouTube's words (VISIONOS, hl=en, 8 Nov 2026).
const LOCKED_REASON = "This video is available to this channel's members on level: Супер-Звезды (or any higher level). Join this YouTube channel from your computer or mobile app.";
const LABEL = 'Members only';
const CLICK = 'mbrclick001', WATCH = 'mbrwatch001', WATCH2 = 'mbrwatch002', LATE = 'mbrlate0001';
const UUMO = 'UUMO1cyGje-DRza7ei9M_oYHNA';
// Real shapes.
const rowBadge = (text = 'Members only') => ({ badges: [{ badgeViewModel: { badgeText: text, badgeStyle: 'BADGE_MEMBERS_ONLY', iconName: 'SPONSORSHIP_STAR' } }] });
const vrBadge = { metadataBadgeRenderer: { icon: { iconType: 'SPONSORSHIP_STAR' }, style: 'BADGE_STYLE_TYPE_MEMBERS_ONLY', label: 'Members only' } };

const all = (o, key, out = []) => {
  if (o && typeof o === 'object') {
    if (o[key]) out.push(o[key]);
    for (const v of Object.values(o)) all(v, key, out);
  }
  return out;
};
const videoLockups = o => all(o, 'lockupViewModel').filter(l => l.contentType === 'LOCKUP_CONTENT_TYPE_VIDEO');
const setDuration = (l, text) => { const b = l.contentImage.thumbnailViewModel.overlays.find(o => o.thumbnailBottomOverlayViewModel); if (b) b.thumbnailBottomOverlayViewModel.badges[0].thumbnailBadgeViewModel.text = text; };
const lock = (l, text) => { l.metadata.lockupMetadataViewModel.metadata.contentMetadataViewModel.metadataRows.push(rowBadge(text)); };

module.exports = async function membersTests(h) {
  const { step, page, ctx, sw, tvOpen, tvClose, tvGo, tvAudit, tvShot, back, TVSIZES, HOOK, json, fixture, homeFirst, nextStr, searchStr, VER, NEXT, nativeOk, incidents, problems, readyDel, log } = h;
  const shots = process.env.YTC_SHOTS;
  const shotPage = async (name, clip) => { if (shots) { await page.waitForTimeout(120); await page.screenshot({ path: path.join(shots, `${name}.png`), ...(clip ? { clip } : {}) }); } };

  // ---- The fixtures: Home-shaped cards (a labelled one, one titled "Members only" without a label, one Russian
  // label, one that only the player's verdict will reveal), Search's videoRenderers, a related column whose
  // first cards (the autoplay target among them) are locked.
  const feed = (() => {
    const j = JSON.parse(homeFirst), vs = videoLockups(j);
    lock(vs[0]); setDuration(vs[0], '1:01:47');
    vs[1].metadata.lockupMetadataViewModel.title.content = 'Members only: what the members asked';
    lock(vs[2], 'Только для спонсоров');
    const was = vs[3].contentId;
    return { str: JSON.stringify(j).split(was).join(CLICK), locked: vs[0].contentId, russian: vs[2].contentId, titled: vs[1].contentId, count: vs.length };
  })();
  const searched = (() => {
    // (The results themselves: a shelf's cards may be recommendations, which Search drops.)
    const j = JSON.parse(searchStr), vs = j.contents.twoColumnSearchResultsRenderer.primaryContents.sectionListRenderer.contents.flatMap(s => s.itemSectionRenderer?.contents || []).map(x => x.videoRenderer).filter(v => v?.lengthText && v.navigationEndpoint?.commandMetadata?.webCommandMetadata?.url?.startsWith('/watch'));
    vs[0].badges = [vrBadge]; vs[0].lengthText = { simpleText: '1:01:47' };
    vs[1].title = { runs: [{ text: 'Members only: what the members asked' }] };
    vs[2].badges = [{ metadataBadgeRenderer: { icon: { iconType: 'SPONSORSHIP_STAR' }, style: 'BADGE_STYLE_TYPE_MEMBERS_ONLY', label: 'Только для спонсоров' } }];
    return { str: JSON.stringify(j), locked: vs[0].videoId, russian: vs[2].videoId, titled: vs[1].videoId };
  })();
  const nextWith = (v, target, locks) => {
    const j = JSON.parse(nextStr), rel = videoLockups(j.contents.twoColumnWatchNextResults.secondaryResults);
    rel.slice(0, locks).forEach(l => { lock(l); setDuration(l, '1:01:47'); });
    const auto = j.contents.twoColumnWatchNextResults.autoplay.autoplay;
    auto.sets[0].autoplayVideo.watchEndpoint.videoId = target || rel[0].contentId;
    j.currentVideoEndpoint = { watchEndpoint: { videoId: v } };
    return { str: JSON.stringify(j), rel, target: target || rel[0].contentId, open: rel[locks] };
  };
  const related = nextWith(WATCH, null, 2);
  const unlabelled = nextWith(WATCH2, CLICK, 0);
  const recording = JSON.parse(fixture('members.json').toString('utf8'));
  // The channel's "Members-only videos" playlist on a watch page: its real panel rows carry no badge.
  const listed = (() => { const j = json('next_list.json'); j.contents.twoColumnWatchNextResults.playlist.playlist.playlistId = UUMO; return JSON.stringify(j); })();
  // The TV account's shelves: the recorded tiles with the real badge line on the first two.
  const tvHome = (() => {
    const j = json('tv_shelves.json'), tiles = all(j, 'tileRenderer').filter(t => t.contentType === 'TILE_CONTENT_TYPE_VIDEO');
    const badge = { lineItemRenderer: { badge: { metadataBadgeRenderer: { icon: { iconType: 'SPONSORSHIP_STAR' }, style: 'BADGE_STYLE_TYPE_MEMBERS_ONLY', label: 'Members only' } } } };
    tiles.slice(0, 2).forEach(t => { const lines = t.metadata.tileMetadataRenderer.lines; lines[lines.length - 1].lineRenderer.items.unshift(badge); });
    return JSON.stringify(j);
  })();

  const verdicts = {
    [CLICK]: { status: 'UNPLAYABLE', reason: LOCKED_REASON },
    [LATE]: { status: 'UNPLAYABLE', reason: LOCKED_REASON },
    mbrjoinonly: { status: 'UNPLAYABLE', reason: 'Join this channel to get access to members-only content like this video, and other exclusive perks.' },
    mbrremoved1: { status: 'ERROR', reason: 'This video has been removed by the uploader' },
    mbrprivate1: { status: 'LOGIN_REQUIRED', reason: 'This video is private' },
    mbrwrong001: { status: 'ERROR', reason: 'Something went wrong' },
  };
  const hook = () => {
    HOOK.vision = v => (verdicts[v] ? { playabilityStatus: verdicts[v] } : null);
    HOOK.api = (ep, body) => {
      const tvc = body.context?.client?.clientName === 'TVHTML5';
      if (ep === 'browse' && !body.continuation) {
        if (tvc && body.browseId === 'FEwhat_to_watch') return tvHome;
        if (['FEwhat_to_watch', 'FEsubscriptions', 'FEhistory', 'UCmembers00000000000000'].includes(body.browseId)) return feed.str;
        if (body.browseId === 'VL' + UUMO) return JSON.stringify(recording.web_playlist);
      }
      if (ep === 'search' && body.query === 'membersq' && !body.continuation) return searched.str;
      if (ep === 'next' && !body.continuation) {
        if (body.playlistId === UUMO) return listed;
        if (body.videoId === WATCH) return related.str;
        if (body.videoId === WATCH2) return unlabelled.str;
        if (body.videoId === CLICK || body.videoId === LATE || verdicts[body.videoId]) return nextWith(body.videoId, null, 1).str;
      }
      return null;
    };
  };
  const unhook = () => { HOOK.api = null; HOOK.vision = null; };
  const withHook = fn => async () => {
    hook();
    await page.evaluate(() => { localStorage.setItem('ytc-test-noready', '1'); sessionStorage.clear(); }).catch(() => {});
    await readyDel(true);
    // (Subscriptions and History need an account.)
    await ctx.addCookies([{ name: 'SAPISID', value: 'e2e-members', domain: '.youtube.com', path: '/', secure: true }]);
    try { await fn(); } finally { await ctx.clearCookies({ name: 'SAPISID' }); unhook(); await page.evaluate(() => { localStorage.removeItem('ytc-test-noready'); sessionStorage.clear(); }).catch(() => {}); await sw.evaluate(() => chrome.storage.local.remove('yourTubeShowMembersV1')); }
  };
  const open = async url => {
    await page.goto('about:blank');
    await page.goto(`https://www.youtube.com${url}`, { waitUntil: 'commit' });
  };
  const cardBadge = id => page.locator(`#ytc-list .c[data-v="${id}"] .th b.mo`);
  const hasCard = id => page.locator(`#ytc-list .c[data-v="${id}"]`).count();
  const waitCard = id => page.waitForSelector(`#ytc-list .c[data-v="${id}"]`, { timeout: 15000 });
  // Every label: on the duration's row, the same inset from its edge, and clear of it (+ the size at this width).
  const labelGeometry = () => page.evaluate(() => [...document.querySelectorAll('.th b.mo')].filter(m => m.getClientRects().length).map(m => {
    const th = m.parentElement, t = th.getBoundingClientRect(), a = m.getBoundingClientRect(), dur = [...th.querySelectorAll('b')].find(b => b !== m), d = dur?.getBoundingClientRect();
    const cs = getComputedStyle(m);
    return { w: t.width, left: a.left - t.left, bottomInset: t.bottom - a.bottom, top: a.top, h: a.height, font: parseFloat(cs.fontSize), gap: d ? d.left - a.right : null, dBottomInset: d ? t.bottom - d.bottom : null, dRight: d ? t.right - d.right : null, dH: d?.height, inside: a.left >= t.left && a.right <= t.right && a.top >= t.top && a.bottom <= t.bottom };
  }));
  const checkLabels = (geo, what, minFont = 11.5) => {
    assert.ok(geo.length > 0, `${what}: labels shown`);
    for (const g of geo) {
      assert.ok(g.inside, `${what}: inside the thumbnail ${JSON.stringify(g)}`);
      assert.ok(g.font >= minFont, `${what}: readable ${JSON.stringify(g)}`);
      if (g.gap !== null) {
        assert.ok(g.gap >= 2, `${what}: clear of the duration ${JSON.stringify(g)}`);
        assert.ok(Math.abs(g.bottomInset - g.dBottomInset) <= 0.5, `${what}: on the duration's row ${JSON.stringify(g)}`);
        assert.ok(Math.abs(g.left - g.dRight) <= 0.5, `${what}: the same inset as the duration ${JSON.stringify(g)}`);
      }
    }
  };

  await step('members-only: a visible label on the card on Home, Subscriptions, Search, the related column, a channel, a playlist and History, in both shapes; a title that merely says "members" gets none', withHook(async () => {
    const surfaces = [
      ['Home', '/', feed.locked, feed.russian, feed.titled],
      ['Subscriptions', '/feed/subscriptions', feed.locked, feed.russian, feed.titled],
      ['Search (videoRenderer)', '/results?search_query=membersq', searched.locked, searched.russian, searched.titled],
      ['channel', '/channel/UCmembers00000000000000', feed.locked, feed.russian, feed.titled],
      ['History', '/feed/history', feed.locked, feed.russian, feed.titled],
      ['playlist (the real recording)', `/playlist?list=${UUMO}`, 'J3M2L2pLhxA', null, null],
    ];
    for (const [name, url, locked, russian, titled] of surfaces) {
      await open(url);
      await waitCard(locked).catch(e => { throw new Error(`${name}: ${e.message}`); });
      assert.equal(await cardBadge(locked).textContent(), LABEL, name);
      if (russian) assert.equal(await cardBadge(russian).textContent(), LABEL, `${name}: a localized badge reads the same`);
      if (titled) {
        assert.equal(await cardBadge(titled).count(), 0, `${name}: the title alone is no label`);
        assert.match(await page.textContent(`#ytc-list .c[data-v="${titled}"] .t`), /Members only/);
      }
      // The duration stays what `.th b` finds first, and the meta line carries no badge text.
      if (titled) assert.equal(await page.textContent(`#ytc-list .c[data-v="${locked}"] .th b`), '1:01:47');
      if (name === 'Home') {
        for (const w of [1400, 480]) {
          await page.setViewportSize({ width: w, height: 900 });
          await page.waitForTimeout(150);
          checkLabels(await labelGeometry(), `Home ${w}`);
          await shotPage(`members-home-${w}`);
        }
        await page.setViewportSize({ width: 1400, height: 900 });
      }
      if (name.startsWith('Search')) {
        for (const w of [1400, 480]) {
          await page.setViewportSize({ width: w, height: 900 });
          await page.waitForTimeout(150);
          checkLabels(await labelGeometry(), `Search ${w}`);
          await shotPage(`members-search-${w}`);
        }
        await page.setViewportSize({ width: 1400, height: 900 });
      }
    }
    // The related column, at the width where its thumbnails are the smallest.
    await open(`/watch?v=${WATCH}`);
    await waitCard(related.rel[0].contentId);
    assert.equal(await cardBadge(related.rel[0].contentId).textContent(), LABEL);
    assert.equal(await cardBadge(related.rel[2].contentId).count(), 0);
    for (const w of [1400, 1100, 980, 480]) {
      await page.setViewportSize({ width: w, height: 900 });
      await page.waitForTimeout(150);
      const geo = await labelGeometry();
      checkLabels(geo, `related ${w}`);
      if (w === 1400) assert.ok(geo[0].w <= 215, `the small thumbnail is measured: ${JSON.stringify(geo[0])}`);
      await shotPage(`members-related-${w}`);
    }
    await page.setViewportSize({ width: 1400, height: 900 });
    // A row of the "Members-only videos" playlist's panel says so (its rows carry no badge).
    await open(`/watch?v=${related.open.contentId}&list=${UUMO}`);
    await page.waitForSelector('#ytc-pll .pli', { timeout: 15000 });
    assert.match(await page.textContent('#ytc-pll .pli .plb'), /· Members only$/);
    await shotPage('members-playlist-panel');
  }));

  await step('members-only: the switch is on by default, off hides at once with no request on Home, Subscriptions, Search and next to videos, a channel and a playlist stay as YouTube sends them; on brings them back; remembered', withHook(async () => {
    await open('/');
    await waitCard(feed.locked);
    await page.hover('#ytc-hfx .fx-fb');
    assert.equal(await page.getAttribute('#ytc-hfx [data-act=fx-members]', 'aria-checked'), 'true', 'default on');
    assert.equal(await page.textContent('#ytc-hfx [data-act=fx-members]'), 'Members-only videos');
    // Inside the panel, on one line with its neighbours where there's room.
    for (const w of [1400, 480]) {
      await page.setViewportSize({ width: w, height: 900 });
      await page.waitForTimeout(350);
      await page.hover('#ytc-hfx .fx-fb').catch(() => {});
      await page.waitForTimeout(250);
      const sw2 = await page.evaluate(() => { const p = document.querySelector('#ytc-hfx .fx-p').getBoundingClientRect(); const xs = [...document.querySelectorAll('#ytc-hfx .fx-sw')].map(e => e.getBoundingClientRect()); return { inside: p.left >= 0 && p.right <= innerWidth && xs.every(x => x.left >= p.left - .5 && x.right <= p.right + .5), rows: new Set(xs.map(x => Math.round(x.top))).size }; });
      assert.ok(sw2.inside, `the switches stay inside the panel at ${w}: ${JSON.stringify(sw2)}`);
      if (w === 1400) assert.equal(sw2.rows, 1, 'on one line');
      await shotPage(`members-filters-${w}`);
    }
    await page.setViewportSize({ width: 1400, height: 900 });
    await page.mouse.move(5, 600);
    await page.hover('#ytc-hfx .fx-fb');
    const n = log.api.length;
    await page.click('#ytc-hfx [data-act=fx-members]');
    await page.waitForFunction(id => !document.querySelector(`#ytc-list .c[data-v="${id}"]`), feed.locked);
    assert.equal(await hasCard(feed.russian), 0);
    assert.equal(await hasCard(feed.titled), 1, 'only labelled cards go');
    assert.deepEqual(log.api.slice(n).filter(a => a.ep === 'browse' || a.ep === 'search').map(a => a.ep), [], 'no feed request for the switch');
    await page.mouse.move(5, 600);
    // Stored once it settles; the next pages go by it.
    for (let i = 0; i < 50; i++) { if ((await sw.evaluate(() => chrome.storage.local.get('yourTubeShowMembersV1')))?.yourTubeShowMembersV1 === false) break; await page.waitForTimeout(100); }
    assert.equal((await sw.evaluate(() => chrome.storage.local.get('yourTubeShowMembersV1'))).yourTubeShowMembersV1, false, 'remembered');
    for (const [url, gone, stays] of [['/feed/subscriptions', feed.locked, feed.titled], ['/results?search_query=membersq', searched.locked, searched.titled], [`/watch?v=${WATCH}`, related.rel[0].contentId, related.rel[2].contentId]]) {
      await open(url);
      await waitCard(stays);
      assert.equal(await hasCard(gone), 0, url);
    }
    for (const [url, stays] of [['/channel/UCmembers00000000000000', feed.locked], [`/playlist?list=${UUMO}`, 'J3M2L2pLhxA']]) {
      await open(url);
      await waitCard(stays);
      assert.equal(await cardBadge(stays).count(), 1, `${url}: kept and labelled`);
    }
    // On again.
    await open('/');
    await waitCard(feed.titled);
    assert.equal(await hasCard(feed.locked), 0, 'remembered across pages');
    await page.hover('#ytc-hfx .fx-fb');
    assert.equal(await page.getAttribute('#ytc-hfx [data-act=fx-members]', 'aria-checked'), 'false');
    await page.click('#ytc-hfx [data-act=fx-members]');
    await waitCard(feed.locked);
    await page.mouse.move(5, 600);
  }));

  await step('members-only: autoplay and its countdown skip a locked target, whatever the switch says; a viewer can still click the card', withHook(async () => {
    await open(`/watch?v=${WATCH}&t=35`);
    await page.waitForSelector('#ytc-up .upc', { timeout: 25000 });
    const openTitle = related.open.metadata.lockupMetadataViewModel.title.content;
    assert.match(await page.textContent('#ytc-up .upc'), new RegExp(openTitle.replace(/[.*+?^${}()|[\]\\]/g, '\\$&').slice(0, 20)), 'the countdown names the first open card');
    await page.click('#ytc-up-now');
    await page.waitForFunction(v => new URL(location.href).searchParams.get('v') === v, related.open.contentId);
    // The card itself still opens: the verdict decides, not the label.
    verdicts[related.rel[1].contentId] = { status: 'UNPLAYABLE', reason: LOCKED_REASON };
    try {
      await open(`/watch?v=${WATCH}`);
      await waitCard(related.rel[1].contentId);
      const n = log.api.filter(a => a.ep === 'player' && a.body.videoId === related.rel[1].contentId).length;
      await page.click(`#ytc-list .c[data-v="${related.rel[1].contentId}"] a.t`);
      await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
      assert.ok(log.api.filter(a => a.ep === 'player' && a.body.videoId === related.rel[1].contentId).length > n, 'the click asks the player');
    } finally { delete verdicts[related.rel[1].contentId]; }
  }));

  await step('members-only: a verdict on a card without the signal: labelled from then on, by the switch, never next, through a reload of the tab; a video that plays after all is forgotten', withHook(async () => {
    await open('/');
    await waitCard(CLICK);
    assert.equal(await cardBadge(CLICK).count(), 0, 'nothing says so yet');
    await page.click(`#ytc-list .c[data-v="${CLICK}"] a.t`);
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
    assert.equal(await page.evaluate(() => sessionStorage.getItem('ytc:members')), CLICK, 'this tab only');
    assert.equal((await sw.evaluate(() => chrome.storage.local.get(null)))['ytc:members'], undefined, 'never in the extension\'s storage');
    await page.goBack({ waitUntil: 'commit' });
    await waitCard(CLICK);
    assert.equal(await cardBadge(CLICK).textContent(), LABEL, 'labelled when back on Home');
    // The switch takes it too.
    await page.hover('#ytc-hfx .fx-fb');
    await page.click('#ytc-hfx [data-act=fx-members]');
    await page.waitForFunction(id => !document.querySelector(`#ytc-list .c[data-v="${id}"]`), CLICK);
    await page.click('#ytc-hfx [data-act=fx-members]');
    await waitCard(CLICK);
    await page.mouse.move(5, 600);
    // Never next: YouTube's target here is that video.
    await open(`/watch?v=${WATCH2}&t=35`);
    await page.waitForSelector('#ytc-up .upc', { timeout: 25000 });
    await page.click('#ytc-up-now');
    await page.waitForFunction(v => new URL(location.href).searchParams.get('v') === v, unlabelled.rel[0].contentId);
    // A reload of the tab keeps it.
    await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await waitCard(CLICK);
    assert.equal(await cardBadge(CLICK).count(), 1, 'kept for the tab');
    // It plays after all: forgotten.
    const was = verdicts[CLICK];
    delete verdicts[CLICK];
    try {
      await page.goto(`https://www.youtube.com/watch?v=${CLICK}`, { waitUntil: 'commit' });
      await page.waitForFunction(() => document.documentElement.dataset.ytcFirstFrame, null, { timeout: 15000 });
      await page.waitForFunction(() => !(sessionStorage.getItem('ytc:members') || '').length, null, { timeout: 3000 });
    } finally { verdicts[CLICK] = was; }
  }));

  const panelBox = () => page.evaluate(() => { const b = document.getElementById('ytc-player').getBoundingClientRect(), p = document.querySelector('#ytc-player .pfail').getBoundingClientRect(); return { h: p.height, w: p.width, H: b.height, W: b.width, dx: (p.left + p.right) / 2 - (b.left + b.right) / 2, dy: (p.top + p.bottom) / 2 - (b.top + b.bottom) / 2, inside: p.top >= b.top && p.bottom <= b.bottom && p.left >= b.left && p.right <= b.right }; });
  const panelButtons = () => page.locator('#ytc-player .pfbt').allTextContents();
  const panelText = () => page.evaluate(() => document.querySelector('#ytc-player .pfail').innerText);

  await step('members-only: the panel says who it is for in plain words, no Try again, no internal prefix, Next video focused and first; compact at every width; the raw reason for developers only', withHook(async () => {
    const inc0 = incidents.length;
    await open(`/watch?v=${CLICK}`);
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
    await page.waitForFunction(() => document.activeElement?.textContent === 'Next video', null, { timeout: 8000 });
    assert.equal(await page.textContent('.pfm'), 'Members-only video');
    assert.match(await page.textContent('#ytc-pfl'), /^This video is for members of .+ at the Супер-Звезды level or higher\.$/);
    assert.deepEqual(await panelButtons(), ['Next video', 'Open on YouTube']);
    assert.equal(await page.locator('#ytc-pfretry').count(), 0);
    const text = await panelText();
    assert.ok(!/VISIONOS|UNPLAYABLE|Join this/.test(text), text);
    assert.equal(await page.textContent('#ytc-player .pfail>.pfv'), VER, 'a quiet version');
    assert.match(await page.getAttribute('.pfail', 'data-r'), /^VISIONOS UNPLAYABLE: This video is available/, 'the raw reason stays in the page for developers');
    for (const w of [1400, 2560, 980, 480]) {
      await page.setViewportSize({ width: w, height: w === 2560 ? 1440 : 900 });
      await page.waitForTimeout(250);
      const g = await panelBox();
      assert.ok(g.inside && Math.abs(g.dx) <= 1 && Math.abs(g.dy) <= 1, `${w}: centred inside the box ${JSON.stringify(g)}`);
      assert.ok(g.h <= 240 && g.h < g.H, `${w}: a compact card, not a tall panel ${JSON.stringify(g)}`);
      const b = await page.locator('#ytc-player').boundingBox();
      await shotPage(`members-panel-${w}`, b);
    }
    await page.setViewportSize({ width: 1400, height: 900 });
    await page.waitForTimeout(100);
    await shotPage('members-panel-page');
    // Developers: the overlay has the whole reason; no incident (expected, not a bug).
    await page.keyboard.press('Backquote');
    await page.waitForSelector('#ytc-debug.on');
    assert.ok((await page.locator('#ytc-debug').textContent()).includes('VISIONOS UNPLAYABLE: This video is available'), 'the debug overlay');
    await page.keyboard.press('Backquote');
    assert.deepEqual(problems(inc0), []);
    // Next video goes on, to a card that isn't locked.
    const target = nextWith(CLICK, null, 1);
    await page.focus('#ytc-player .pfbt');
    await page.keyboard.press('Enter');
    await page.waitForFunction(v => new URL(location.href).searchParams.get('v') === v, target.open.contentId);
    // And Back returns to the panel.
    await page.goBack({ waitUntil: 'commit' });
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
    // Open on YouTube is still there for a member: YouTube's page knows the account.
    nativeOk.push(`https://www.youtube.com/watch?v=${CLICK}&yt-native=1`);
    await page.locator('#ytc-player .pfbt', { hasText: 'Open on YouTube' }).click();
    await page.waitForURL(/yt-native=1/, { timeout: 8000 });
  }));

  await step('members-only: `next` answering after the verdict brings the channel\'s name and Next video to the panel that is showing; a sentence with no level never invites to join', withHook(async () => {
    NEXT.latency = 1200;
    try {
      await open(`/watch?v=${LATE}`);
      await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
      if (!(await page.locator('#ytc-player .pfbt', { hasText: 'Next video' }).count())) {
        assert.deepEqual(await panelButtons(), ['Open on YouTube'], 'alone until `next` answers');
        assert.match(await page.textContent('#ytc-pfl'), /members of this channel at the/);
        assert.equal(await page.evaluate(() => document.activeElement?.textContent), 'Open on YouTube');
      }
      await page.waitForFunction(() => document.querySelector('#ytc-player .pfbt')?.textContent === 'Next video', null, { timeout: 8000 });
      assert.equal(await page.evaluate(() => document.activeElement?.textContent), 'Next video', 'the focus moves to it');
      assert.ok(!/this channel/.test(await page.textContent('#ytc-pfl')), await page.textContent('#ytc-pfl'));
    } finally { NEXT.latency = 0; }
    await open('/watch?v=mbrjoinonly');
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
    await page.waitForFunction(() => document.querySelector('#ytc-player .pfbt')?.textContent === 'Next video', null, { timeout: 8000 });
    assert.match(await page.textContent('#ytc-pfl'), /^This video is for members of .+\.$/);
    assert.ok(!/Join/.test(await panelText()));
  }));

  await step('members-only: other verdicts have no Try again and no prefix; what can change keeps Try again, the technical line and the version', withHook(async () => {
    const f = async (v, m, line, buttons) => {
      await open(`/watch?v=${v}`);
      await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
      await page.waitForFunction(() => document.querySelectorAll('#ytc-player .pfbt').length === 2, null, { timeout: 8000 });
      assert.equal(await page.textContent('.pfm'), m, v);
      assert.equal(await page.locator('#ytc-pfl').count() ? await page.textContent('#ytc-pfl') : '', line, v);
      assert.deepEqual(await panelButtons(), buttons, v);
      assert.ok(!/VISIONOS|Try again/.test(await panelText()), await panelText());
      assert.equal(await page.textContent('#ytc-player .pfail>.pfv'), VER, v);
      assert.equal(await page.evaluate(() => document.activeElement?.textContent), buttons[0], `${v}: the primary action is focused`);
      assert.equal(await page.getAttribute('#ytc-player .pfbt.on', 'data-act'), buttons[0] === 'Next video' ? 'pf-next' : 'pf-native');
    };
    await f('mbrremoved1', 'This video has been removed by the uploader', '', ['Next video', 'Open on YouTube']);
    await f('liveupcomin', 'Premieres in 2 hours', '', ['Next video', 'Open on YouTube']);
    await f('unplayable1', "YouTube won't play this video here", 'Forced failure (e2e)', ['Next video', 'Open on YouTube']);
    await f('agerestrict', 'Age-restricted — sign-in needed', 'Sign in to confirm your age', ['Open on YouTube', 'Next video']);
    await f('mbrprivate1', 'Sign-in needed to watch this video', 'This video is private', ['Open on YouTube', 'Next video']);
    await shotPage('verdict-signin');
    // YouTube's "something went wrong" can change: the old panel.
    await open('/watch?v=mbrwrong001');
    await page.waitForSelector('#ytc-player.failed .pfail', { timeout: 15000 });
    assert.deepEqual(await panelButtons(), ['Try again', 'Open on YouTube']);
    assert.equal(await page.textContent('.pfr'), `VISIONOS ERROR: Something went wrong · ${VER}`);
    await shotPage('retriable-panel');
  }));

  await step('members-only on TV (960×540 and 1920×1080): the panel inside the safe area with Next video focused, ←/→ between its actions, OK goes on, Back as ever; the label on the 5-column grid and the Up next row', async () => {
    hook();
    const tv = await tvOpen();
    try {
      for (const [W, H] of TVSIZES) {
        await tv.setViewportSize({ width: W, height: H });
        // The panel.
        await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${CLICK}`);
        await tv.waitForSelector('#ytc-player .pfail', { timeout: 15000 });
        await tv.waitForFunction(() => document.activeElement?.textContent === 'Next video', null, { timeout: 8000 });
        await tv.waitForTimeout(400); // (the panel fades in)
        assert.deepEqual(await tvAudit(tv, `members panel ${W}`), []);
        await tvShot(tv, 'members-panel');
        assert.equal(await tv.textContent('.pfm'), 'Members-only video');
        assert.equal(await tv.locator('#ytc-pfretry').count(), 0);
        assert.ok(!/VISIONOS|Try again/.test(await tv.evaluate(() => document.querySelector('.pfail').innerText)));
        await tv.keyboard.press('ArrowRight');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Open on YouTube');
        await tv.keyboard.press('ArrowLeft');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Next video');
        await tv.keyboard.press('ArrowDown');
        assert.equal(await tv.evaluate(() => document.activeElement.textContent), 'Next video', '↓ stays on the panel');
        const box = await tv.evaluate(() => { const r = document.querySelector('.pfail').getBoundingClientRect(); return { h: r.height, top: r.top, bottom: r.bottom }; });
        assert.ok(box.top >= 0 && box.bottom <= H && box.h <= H * 0.6, `compact on TV: ${JSON.stringify(box)}`);
        // OK goes on; Back returns to the panel (the TV's history), and leaves it as before.
        const open2 = nextWith(CLICK, null, 1).open.contentId;
        await tv.keyboard.press('Enter');
        await tv.waitForFunction(v => new URL(location.href).searchParams.get('v') === v, open2, { timeout: 8000 });
        // The Up next row: the label on a locked card, inside the small card, clear of the duration.
        await tvGo(tv, 'code', `https://www.youtube.com/watch?v=${WATCH}`);
        await tv.waitForSelector('.wr #ytc-list .c .th b.mo', { state: 'attached', timeout: 15000 });
        await tv.waitForFunction(() => document.documentElement.dataset.ytcFirstFrame || document.querySelector('#ytc-seek'), null, { timeout: 15000 });
        await tv.focus('#ytc-seek').catch(() => {});
        await tv.keyboard.press('ArrowDown');
        await tv.keyboard.press('ArrowDown');
        await tv.keyboard.press('ArrowDown');
        await tv.waitForTimeout(300);
        await tvShot(tv, 'members-upnext');
        const row = await tv.evaluate(() => [...document.querySelectorAll('.wr #ytc-list .c')].slice(0, 2).map(c => { const th = c.querySelector('.th').getBoundingClientRect(), m = c.querySelector('.th b.mo').getBoundingClientRect(), d = [...c.querySelectorAll('.th b')].find(b => !b.matches('.mo')).getBoundingClientRect(); return { gap: d.left - m.right, inset: m.left - th.left, right: th.right - d.right, bottom: Math.abs(m.bottom - d.bottom), font: parseFloat(getComputedStyle(c.querySelector('.th b.mo')).fontSize) }; }));
        for (const r of row) assert.ok(r.gap >= 2 && Math.abs(r.inset - r.right) <= 0.5 && r.bottom <= 0.5, `Up next ${W}: ${JSON.stringify(r)}`);
        // The grid: the account's tiles, five per row.
        await tvGo(tv, 'signed', 'https://www.youtube.com/');
        await tv.waitForSelector('#ytc-list .c .th b.mo', { timeout: 15000 });
        await tv.waitForTimeout(300);
        assert.deepEqual(await tvAudit(tv, `members grid ${W}`), []);
        await tvShot(tv, 'members-home');
        const grid = await tv.evaluate(() => { const m = document.querySelector('#ytc-list .c .th b.mo'), c = m.closest('.c'), th = c.querySelector('.th').getBoundingClientRect(), a = m.getBoundingClientRect(), d = [...c.querySelectorAll('.th b')].find(b => b !== m)?.getBoundingClientRect(); return { w: c.getBoundingClientRect().width, gap: d ? d.left - a.right : 9, left: a.left - th.left, right: d ? th.right - d.right : a.left - th.left }; });
        assert.ok(grid.gap >= 2 && Math.abs(grid.left - grid.right) <= 0.5, `TV grid ${W}: ${JSON.stringify(grid)}`);
      }
      assert.deepEqual(tv.errors, []);
    } finally { unhook(); await tvClose(tv); }
  });
};
