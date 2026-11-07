// Live speed benchmark: the built extension in Playwright's Chromium, signed out, against the real
// youtube.com. It reads the client's own speed telemetry (development build: the perf batches go to
// a collector this script runs) plus the document's navigation timing and boot marks, and prints
// p50/p90 per scenario. This is the only place where the network is used on purpose; the e2e suite
// (tests/e2e.cjs) stays offline.
//
//   ./build.sh --dev && node tools/bench/bench.cjs [--runs=8] [--ext=<extension folder>] [--headed]
//                                                  [--only=boot,reload,spa,search] [--json=<out.json>]
//                                                  [--ndjson=<file>: the batches as a debug-center incidents
//                                                   file, to look at a run in tools/debug-center]
//
// Signed out, YouTube's Home has no recommendations ("Your YouTube history is off"), so Home measures
// the generic boot (shell + hero) and the card grid is measured on channel video pages (the same
// cards, chunks and continuations as Home's feed). Scenarios (each run in its own fresh tab):
//   boot    cold boot of Home in a new tab, then a cold boot of a channel's Videos page
//   reload  reload of that channel page in the same tab (snapshot, then the fresh response)
//   spa     channel page → click the first card (watch: the tab's first watch open) → a related
//           card (a later open) → back
//   search  search typed in the box + Enter at once; the same with a 350 ms pause before Enter
//           (the typing-pause prefetch); then a cold boot of a results URL
// Also per scenario: InnerTube bytes on the wire (requests the pages made, by endpoint) and, on the
// channel boot, when the first row's thumbnails had loaded (resource timing, ms from navigation start).
//   latency (only with --only=latency) YouTube made slow: for each factor F in --slow (default
//           1,10,100) a share --slowp (default 0.2) of the next / search / browse requests answers
//           (F − 1) × 400 ms late (each request draws on its own, so a second copy usually escapes),
//           and with --uniform every request does; each factor runs with hedging on and off
//           (localStorage ytc-test-nohedge). Boots of a watch, a results and a channel URL.
// Nothing from the pages is stored: only numbers, metric names and route kinds are printed or saved.
'use strict';
const { chromium } = require('playwright');
const fs = require('node:fs'), os = require('node:os'), path = require('node:path'), http = require('node:http');

const arg = (k, d) => { const a = process.argv.find(x => x.startsWith(`--${k}=`)); return a ? a.slice(k.length + 3) : d; };
const RUNS = +arg('runs', 8);
const EXT = path.resolve(arg('ext', path.join(__dirname, '..', '..', 'extension')));
const ONLY = new Set(arg('only', 'boot,reload,spa,search').split(','));
const HEADED = process.argv.includes('--headed');
const OUT = arg('json', '');
const NDJSON = arg('ndjson', '');
const SLOW = arg('slow', '1,10,100').split(',').map(Number);
const SLOWP = +arg('slowp', 0.2);
const UNIFORM = process.argv.includes('--uniform');
// --cpu=4: Chromium's CPU throttling on every page (a slower machine); --loaf: print the long frames of watch pages.
const CPU = +arg('cpu', 1);
const LOAF = process.argv.includes('--loaf');
// Fixed, harmless search terms (the benchmark's own, never the user's).
const CHANNELS = ['/@veritasium/videos', '/@kurzgesagt/videos', '/@mkbhd/videos', '/@TED/videos', '/@NASA/videos', '/@3blue1brown/videos', '/@LinusTechTips/videos', '/@NileRed/videos'];
const TERMS = ['lofi hip hop', 'rust programming', 'cooking pasta', 'piano tutorial', 'space documentary', 'guitar lesson', 'chess openings', 'woodworking'];

const devBuild = JSON.parse(fs.readFileSync(path.join(EXT, 'manifest.json'), 'utf8')).host_permissions.some(p => p.startsWith('http://127.0.0.1:47321/'));
if (!devBuild) { console.error('bench: needs a development build (./build.sh --dev): it reads the client telemetry'); process.exit(2); }

const q = (a, p) => { const s = a.filter(Number.isFinite).sort((x, y) => x - y); return s.length ? s[Math.min(s.length - 1, Math.round((s.length - 1) * p))] : NaN; };
const f = v => Number.isFinite(v) ? v.toFixed(0).padStart(6) : '     -';

(async () => {
  // Collector: perf batches (and any incident) from the extension's background worker.
  const batches = [];
  const srv = http.createServer((req, res) => {
    let b = ''; req.on('data', c => b += c); req.on('end', () => {
      res.writeHead(req.method === 'OPTIONS' ? 204 : 200, { 'access-control-allow-origin': '*', 'access-control-allow-headers': 'content-type', 'access-control-allow-private-network': 'true' });
      if (req.method === 'POST') { try { const r = JSON.parse(b); if (r.kind === 'perf') batches.push(r); if (NDJSON) fs.appendFileSync(NDJSON, JSON.stringify({ received: Date.now(), report: r }) + '\n'); } catch {} }
      res.end('ok');
    });
  }).listen(0, '127.0.0.1');
  await new Promise(r => srv.once('listening', r));
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'lumen-bench-'));
  const ctx = await chromium.launchPersistentContext(profile, {
    headless: !HEADED, channel: 'chromium', viewport: { width: 1400, height: 900 },
    args: [`--disable-extensions-except=${EXT}`, `--load-extension=${EXT}`, '--mute-audio'],
  });
  // EU consent interstitial: decline-all consent cookie (signed out stays signed out).
  await ctx.addCookies([{ name: 'SOCS', value: 'CAESEwgDEgk0ODE3Nzk3MjQaAmVuIAEaBgiA_LyaBg', domain: '.youtube.com', path: '/', secure: true, sameSite: 'Lax' }]);
  if (CPU > 1) ctx.on('page', async p => { try { const c = await ctx.newCDPSession(p); await c.send('Emulation.setCPUThrottlingRate', { rate: CPU }); } catch {} });
  const sw = ctx.serviceWorkers()[0] || await ctx.waitForEvent('serviceworker');
  await sw.evaluate(port => chrome.storage.local.set({ ytcCollectorV1: `http://127.0.0.1:${port}/incident` }), srv.address().port);
  const rows = []; // {scenario, run, k: value}
  const add = (scenario, run, o) => rows.push({ scenario, run, ...o });
  // InnerTube bytes on the wire per scenario (the encoded response bodies the pages received).
  let scen = 'warmup';
  const wire = {}; // scen → ep → {n, bytes}
  const searchWin = []; // when each typed search ran (to tell the paused ones apart)
  ctx.on('requestfinished', async r => {
    const m = /\/youtubei\/v1\/([\w/]+)/.exec(r.url());
    if (!m) return;
    const sc = scen;
    try { const z = await r.sizes(); const w = ((wire[sc] ||= {})[m[1]] ||= { n: 0, bytes: 0 }); w.n++; w.bytes += z.responseBodySize; } catch {}
  });
  // The first row's thumbnails: when the last of their image requests finished, ms after `goto` (the
  // page's own resource timing does not see them: the client replaces YouTube's document).
  const imgDone = new Map();
  ctx.on('requestfinished', async r => {
    if (r.resourceType() !== 'image') return;
    imgDone.set(r.url(), Date.now());
    const sc = scen;
    if (/^https:\/\/i\.ytimg\.com\/(vi|vi_webp)\//.test(r.url())) try { const z = await r.sizes(); const w = ((wire[sc] ||= {})['thumbnails'] ||= { n: 0, bytes: 0 }); w.n++; w.bytes += z.responseBodySize; } catch {}
  });
  const firstRowImgs = async (p, t0) => {
    const urls = await p.evaluate(() => {
      const cards = [...document.querySelectorAll('#ytc-list .c')];
      if (!cards.length) return [];
      const top = cards[0].getBoundingClientRect().top;
      return cards.filter(c => Math.abs(c.getBoundingClientRect().top - top) < 4).map(c => c.querySelector('img')?.currentSrc || '');
    });
    const ends = urls.map(u => imgDone.get(u));
    return urls.length && ends.every(Number.isFinite) ? Math.max(...ends) - t0 : NaN;
  };

  // The page's own timings: navigation timing and the client's boot marks (performance.now ms).
  const docMarks = p => p.evaluate(() => {
    const n = performance.getEntriesByType('navigation')[0] || {}, d = document.documentElement.dataset;
    const num = k => d[k] ? +d[k] : NaN;
    return { response_start: n.responseStart, script: num('ytcScript'), wasm: num('ytcWasm'), shell: num('ytcShell'), ready: num('ytcReady'), fresh: num('ytcFresh'), repaint: num('ytcRepaint') };
  });
  const cards = p => p.waitForSelector('#ytc-list .c', { timeout: 20000 });
  // Leaving the document (about:blank) makes the client send its batch.
  const flushed = async p => { const n = batches.length; await p.goto('about:blank', { waitUntil: 'commit' }); for (let i = 0; i < 40 && batches.length === n; i++) await new Promise(r => setTimeout(r, 100)); };
  const pageIds = p => p.evaluate(() => [...document.querySelectorAll('#ytc-list .c[data-v]')].slice(0, 24).map(e => e.dataset.v));

  // Warm-up (extension registered, connection pool and DNS like a running browser).
  { const p = await ctx.newPage(); await p.goto('https://www.youtube.com/', { waitUntil: 'commit' }); await cards(p).catch(() => {}); await flushed(p); await p.close(); }
  const ytc = u => 'https://www.youtube.com' + u;
  const hero = p => p.waitForSelector('#ytc-list .c, #ytc-main .hero', { timeout: 20000 });
  // ---- latency: injected slowness, hedging on / off
  const lat = [];
  if (ONLY.has('latency')) {
    let seed = 7;
    const rand = () => { seed = (seed * 1103515245 + 12345) % 2147483648; return seed / 2147483648; };
    let F = 1;
    await ctx.route(/\/youtubei\/v1\/(next|search|browse)\?/, async route => {
      const slow = F > 1 && (UNIFORM || rand() < SLOWP);
      if (slow) await new Promise(r => setTimeout(r, (F - 1) * 400));
      try { await route.continue(); } catch {}
    });
    const p0 = await ctx.newPage();
    await p0.goto(ytc('/results?search_query=' + encodeURIComponent('nature documentary')), { waitUntil: 'commit' });
    await cards(p0);
    const vids = await p0.evaluate(() => [...document.querySelectorAll('#ytc-list .c[data-v]')].map(e => e.dataset.v).slice(0, 12));
    await p0.close();
    for (const f of SLOW) for (const hedge of [true, false]) {
      F = f;
      const from = batches.length;
      for (let run = 0; run < RUNS; run++) {
        const p = await ctx.newPage();
        await p.goto(ytc('/results?search_query=x'), { waitUntil: 'commit' });
        await p.evaluate(h => h ? localStorage.removeItem('ytc-test-nohedge') : localStorage.setItem('ytc-test-nohedge', '1'), hedge);
        const urls = [`/watch?v=${vids[run % vids.length]}`, '/results?search_query=' + encodeURIComponent(TERMS[run % TERMS.length] + ' ' + f), CHANNELS[run % CHANNELS.length]];
        for (const u of urls) {
          await p.goto(ytc(u), { waitUntil: 'commit' });
          await p.waitForFunction(() => document.documentElement.dataset.ytcReady, null, { timeout: 90000 }).catch(() => {});
          await p.waitForTimeout(300);
        }
        await flushed(p);
        await p.close();
        process.stdout.write(`latency ×${f} hedge ${hedge ? 'on' : 'off'} ${run + 1}/${RUNS}\r`);
      }
      await new Promise(r => setTimeout(r, 800));
      const ev = batches.slice(from).filter(b => Array.isArray(b.events)).flatMap(b => b.events);
      const navs = ev.filter(e => e.m.startsWith('nav.') && e.mode === 'boot' && ['watch', 'search', 'channel'].includes(e.route));
      const reqs = ev.filter(e => /^req\.(next|search|browse)$/.test(e.m) && e.nav);
      lat.push({ f, hedge, n: navs.length, p50: q(navs.map(e => e.v), 0.5), p90: q(navs.map(e => e.v), 0.9), max: Math.max(...navs.map(e => e.v)), fired: reqs.filter(e => e.hedge_fired).length, won: reqs.filter(e => e.hedge_won).length, reqs: reqs.length });
    }
    await ctx.unroute(/\/youtubei\/v1\/(next|search|browse)\?/);
  }
  for (let run = 0; run < (ONLY.has('latency') ? 0 : RUNS); run++) {
    const ch = CHANNELS[run % CHANNELS.length];
    if (ONLY.has('boot') || ONLY.has('reload') || ONLY.has('spa')) {
      scen = 'boot';
      const p = await ctx.newPage();
      await p.goto(ytc('/'), { waitUntil: 'commit' });
      await hero(p);
      await p.waitForTimeout(1200);
      add('boot-home', run, await docMarks(p));
      const t0 = Date.now();
      await p.goto(ytc(ch), { waitUntil: 'commit' });
      await cards(p);
      await p.waitForTimeout(2500); // idle preloads and their inserts (long frames)
      add('boot-channel', run, { ...(await docMarks(p)), img_row: await firstRowImgs(p, t0) });
      if (ONLY.has('reload')) {
        scen = 'reload';
        const before = await pageIds(p);
        await p.reload({ waitUntil: 'commit' });
        await cards(p);
        await p.waitForTimeout(2500); // the fresh response replaces the snapshot
        const after = await pageIds(p);
        add('reload-channel', run, { ...(await docMarks(p)), fresh_share: after.length ? after.filter(id => !before.includes(id)).length / after.length : NaN });
      }
      if (ONLY.has('spa')) {
        scen = 'spa';
        await p.mouse.wheel(0, 2400); // scroll inserts from the preloaded buffer
        await p.waitForTimeout(800);
        await p.evaluate(() => scrollTo(0, 0));
        await p.waitForTimeout(400);
        const a = p.locator('#ytc-list .c a.th[href^="/watch"]').first();
        await a.hover(); await p.waitForTimeout(150);
        await a.click();
        await p.waitForSelector('#ytc-player', { timeout: 20000 });
        await p.waitForTimeout(2500);
        // A later watch open in the same tab: a related card.
        const r = p.locator('#ytc-list .c a.th[href^="/watch"]').nth(2);
        await r.hover(); await p.waitForTimeout(150);
        await r.click();
        await p.waitForSelector('#ytc-player', { timeout: 20000 });
        await p.waitForTimeout(2000);
        await p.goBack({ waitUntil: 'commit' });
        await cards(p);
        await p.waitForTimeout(800);
      }
      await flushed(p);
      await p.close();
    }
    if (ONLY.has('search')) {
      scen = 'search';
      const p = await ctx.newPage();
      for (const pause of [0, 350]) {
        await p.goto(ytc(ch), { waitUntil: 'commit' });
        await cards(p);
        await p.waitForTimeout(1200);
        await p.click('#ytc-q');
        await p.keyboard.type(TERMS[(run + (pause ? 5 : 0)) % TERMS.length], { delay: 15 });
        if (pause) await p.waitForTimeout(pause);
        const t0 = Date.now();
        await p.keyboard.press('Enter');
        await p.waitForURL(/results/, { waitUntil: 'commit' });
        await cards(p);
        searchWin.push({ pause, t0, t1: Date.now() + 50 });
        await p.waitForTimeout(1200);
        await p.evaluate(x => document.documentElement.dataset.benchPause = x, String(pause));
        await flushed(p);
      }
      await p.goto(ytc('/results?search_query=' + encodeURIComponent(TERMS[(run + 3) % TERMS.length])), { waitUntil: 'commit' });
      await cards(p);
      await p.waitForTimeout(1200);
      add('boot-search', run, await docMarks(p));
      await flushed(p);
      await p.close();
    }
    process.stdout.write(`run ${run + 1}/${RUNS}\r`);
  }
  await new Promise(r => setTimeout(r, 1500));
  await ctx.close();
  srv.close();
  fs.rmSync(profile, { recursive: true, force: true });

  // ---- the client's own events
  const ev = batches.filter(b => Array.isArray(b.events)).flatMap(b => b.events.map(e => ({ ...e, page: b.page, overhead: b.overhead_ms })));
  // The first watch open of each document (page) vs later ones; searches by order (the second one paused before Enter).
  const seenWatch = new Set();
  for (const e of ev) {
    if (e.m === 'nav.watch' && e.mode === 'push') { e.order = seenWatch.has(e.page) ? 'later' : 'first'; seenWatch.add(e.page); }
  }
  const series = {};
  const put = (k, v) => { if (Number.isFinite(v)) (series[k] = series[k] || []).push(v); };
  for (const e of ev) {
    const m = e.m;
    if (m.startsWith('nav.') && m !== 'nav.fresh') {
      const key = `${m} ${e.mode}${e.mode === 'boot' ? '/' + e.src : ''}`;
      put(key, e.v); put(key + ' · handler', e.handler_ms); put(key + ' · insert', e.insert_ms);
      if (e.wait_ms != null) put(key + ' · wait', e.wait_ms);
      if (e.parse_ms != null) put(key + ' · parse', e.parse_ms);
      if (e.render_ms != null) put(key + ' · render', e.render_ms);
      if (e.order) { put(`${m} push ${e.order}`, e.v); put(`${m} push ${e.order} · insert`, e.insert_ms); }
      if (m === 'nav.search' && e.mode === 'push') {
        put(`nav.search push src=${e.src}`, e.v);
        const w = searchWin.find(w => e.at >= w.t0 - 20 && e.at <= w.t1);
        if (w) put(`nav.search push · Enter→results, ${w.pause} ms pause`, e.v);
      }
      if (e.boot) for (const [k, v] of Object.entries(e.boot)) put(`${key} · boot.${k}`, v);
    } else if (m === 'nav.fresh') put(`nav.fresh ${e.route}`, e.v);
    else if (m.startsWith('req.')) { put(`${m} (${e.via})`, e.v); if (e.bytes) put(`${m} (${e.via}) · decoded KB`, e.bytes / 1024); }
    else if (m === 'lag.loaf') { put(`lag.loaf ${e.route}`, e.v); put(`lag.loaf ${e.route} · layout`, e.layout); }
    else if (m === 'ui.act') { put(`ui.act ${e.act}`, e.v); }
    else put(m, e.v);
  }
  for (const r of rows) {
    for (const k of ['response_start', 'script', 'wasm', 'shell', 'ready', 'fresh', 'repaint']) put(`doc ${r.scenario} · ${k}`, r[k]);
    if (r.fresh_share != null) put(`reload-channel · share of new video ids`, r.fresh_share * 100);
    if (r.img_row != null) put(`doc ${r.scenario} · first-row images loaded (from goto)`, r.img_row);
  }
  const counts = {};
  for (const e of ev) if (e.m === 'lag.loaf') counts['loaf/' + e.route] = (counts['loaf/' + e.route] || 0) + 1;
  const keys = Object.keys(series).sort();
  console.log(`\nRedline live benchmark: ${EXT}, ${RUNS} runs${CPU > 1 ? `, CPU ${CPU}× slower` : ''}, ${batches.length} batches, ${ev.length} events`);
  console.log('metric'.padEnd(58) + '     n    p50    p90    max');
  for (const k of keys) {
    const s = series[k];
    console.log(k.padEnd(58) + String(s.length).padStart(6) + f(q(s, 0.5)) + f(q(s, 0.9)) + f(Math.max(...s)));
  }
  console.log('long frames per scenario page kind: ' + JSON.stringify(counts));
  if (LOAF) for (const e of ev.filter(e => e.m === 'lag.loaf' && e.route === 'watch')) console.log('loaf watch ' + JSON.stringify({ v: e.v, block: e.block, render: e.render, layout: e.layout, scripts: e.scripts }));
  const overhead = batches.filter(b => Array.isArray(b.events)).map(b => b.overhead_ms);
  console.log(`telemetry overhead per batch: p50 ${q(overhead, 0.5)} ms, max ${Math.max(...overhead)} ms`);
  if (lat.length) {
    console.log(`\nInjected slowness (${UNIFORM ? 'every request' : `${SLOWP * 100}% of next/search/browse requests`} × factor, +(F−1)×400 ms): boot navigations watch + search + channel`);
    console.log('factor  hedge      n    p50    p90    max  hedges fired/won of reqs');
    for (const r of lat) console.log(`${('×' + r.f).padEnd(7)} ${(r.hedge ? 'on' : 'off').padEnd(5)}${String(r.n).padStart(6)}${f(r.p50)}${f(r.p90)}${f(r.max)}  ${r.fired}/${r.won} of ${r.reqs}`);
  }
  console.log('\nInnerTube and video thumbnails on the wire per run (KB, encoded bodies; requests per run):');
  for (const [sc, eps] of Object.entries(wire)) {
    if (sc === 'warmup') continue;
    const tot = Object.values(eps).reduce((a, w) => a + w.bytes, 0);
    console.log(`  ${sc.padEnd(8)} total ${(tot / 1024 / RUNS).toFixed(0).padStart(5)}  ` + Object.entries(eps).map(([ep, w]) => `${ep} ${(w.bytes / 1024 / RUNS).toFixed(0)} (${(w.n / RUNS).toFixed(1)})`).join(', '));
  }
  if (OUT) fs.writeFileSync(OUT, JSON.stringify({ ext: EXT, runs: RUNS, series, counts, latency: lat, wire }, null, 1));
})().catch(e => { console.error(e); process.exit(1); });
