// Live benchmark: Home reload → first paint of a fresh feed (Redline) vs YouTube's own page.
// Talks to the REAL youtube.com (signed out); run it by hand and sparingly, never in CI.
//
//   node tools/bench/home-reload.cjs [--n=10] [--wait=4000] [--delay=MS] [--slow=X] [--yt] [--parity]
//
//   --n        reloads per mode (default 10)
//   --wait     ms between a paint and the next reload (default 4000: time to read the page)
//   --delay    adds MS to the first Home browse request of every reload (Playwright route delay)
//   --slow     multiplies: --slow=10 delays the first Home browse request by 9× its median (measured live)
//   --yt       also YouTube's own page (a separate profile without the extension), same reload loop
//   --parity   same profile: YouTube's own Home (?yt-native=1) right after each Redline reload, and the
//              overlap of the two feeds (YouTube's response, not Redline's filtered cards)
//   --noready  no ready responses (localStorage ytc-test-noready): every reload takes the network path
//   --nohedge  no hedge request (localStorage ytc-test-nohedge)
//   --nodoc    Home from the browse API instead of YouTube's Home document (localStorage ytc-test-nodoc)
//   --noimp    no "shown" signals (localStorage ytc-test-noimp)
//   --nokeep   the network path fetches YouTube's Home document instead of reading the tab's own (ytc-test-nokeep)
//   --cap      give up on a reload after this many ms (default 30000)
//   --profile  the seeded profile's name (default main)
//   YTC_EXT    the extension folder (default extension/)
//
// "fresh paint" = the first frame whose Home cards all come from a browse response no earlier load
// showed (the response is matched by video ids, captured from the network). Redline's own breakdown
// (src, age, hedge) comes from its development-build marks on <html>.
'use strict';
const fs = require('node:fs'), os = require('node:os'), path = require('node:path');
const { extensionDir } = require('../../tests/browser.cjs');
const { seeded, launch, homeIds } = require('./profile.cjs');
// The Home data inside YouTube's Home document (what Redline reads since 0.12.0, src/homedoc.rs).
const docData = html => { const m = /var ytInitialData = (\{.*?\});<\/script>/s.exec(html); try { return m ? JSON.parse(m[1]) : {}; } catch { return {}; } };

const arg = (k, d) => { const a = process.argv.find(x => x.startsWith(`--${k}=`)); return a ? a.slice(k.length + 3) : d; };
const flag = k => process.argv.includes(`--${k}`);
const N = +arg('n', 10), WAIT = +arg('wait', 4000), DELAY = +arg('delay', 0), SLOW = +arg('slow', 0), CAP = +arg('cap', 30000);
const EXT = extensionDir();

// In the page's main world: every frame, the ids of the Home cards on screen (changes only).
const PROBE = `(() => {
  const H = window.__bench = { frames: [], t0: performance.timeOrigin };
  let last = '';
  const ids = () => {
    const a = document.querySelectorAll('#ytc-list .c:not(.sk) a.t[href^="/watch"], ytd-rich-item-renderer a[href*="/watch?v="]');
    const out = [];
    for (const x of a) { const m = /[?&]v=([\\w-]{11})/.exec(x.getAttribute('href')); if (m && !out.includes(m[1])) out.push(m[1]); if (out.length >= 12) break; }
    return out;
  };
  const tick = t => {
    const v = ids(), k = v.join(',');
    if (k !== last) { last = k; if (v.length) H.frames.push({ t, ids: v }); }
    if (t < 30000) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
})();`;

const pct = (a, p) => { if (!a.length) return NaN; const s = [...a].sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.floor(p * s.length))]; };
const fmt = a => a.length ? `p50 ${pct(a, .5).toFixed(0)} ms, p90 ${pct(a, .9).toFixed(0)} ms (n=${a.length})` : 'n=0';
const overlap = (a, b) => { if (!a.length || !b.length) return NaN; const B = new Set(b); return a.filter(x => B.has(x)).length / Math.min(a.length, b.length); };

// One seeded signed-out profile (tools/bench/profile.cjs) for every mode, one mode at a time.
async function context(withExt) {
  const ctx = await launch(await seeded(arg('profile', 'main')), withExt ? EXT : null);
  await ctx.addInitScript(PROBE);
  return { ctx };
}

// One mode's reload loop. Returns per-load rows.
async function run(label, withExt, opts = {}) {
  // Measured browse times (for --slow) carry over from a warm-up of this mode.
  const { ctx } = await context(withExt);
  const page = ctx.pages()[0] || await ctx.newPage();
  const responses = []; // {at, ids, doc}
  let delayNext = 0, firstOfDoc = true, browseMs = [];
  if (withExt) {
    await ctx.route('**/youtubei/v1/browse*', async route => {
      const b = route.request().postDataJSON() || {};
      if (b.browseId === 'FEwhat_to_watch' && delayNext && firstOfDoc) {
        firstOfDoc = false;
        await new Promise(r => setTimeout(r, delayNext));
      }
      return route.continue();
    });
  }
  page.on('requestfinished', async req => {
    try {
      const u = new URL(req.url());
      // (The tab's own document is Home's answer too since 0.12.0 when nothing is ready.)
      const doc = u.pathname === '/' && (req.resourceType() === 'fetch' || req.resourceType() === 'document');
      if (u.pathname !== '/youtubei/v1/browse' && !doc) return;
      const b = doc ? { browseId: 'FEwhat_to_watch' } : req.postDataJSON() || {};
      if (b.browseId !== 'FEwhat_to_watch') return;
      const t = req.timing();
      const r = await req.response();
      const text = (await r.body()).toString('utf8');
      const ids = homeIds(doc ? docData(text) : JSON.parse(text));
      if (!ids.length) return; // a continuation, or an empty Home
      browseMs.push(t.responseEnd);
      // net: request start → first byte → last byte (ms), for the streaming question.
      responses.push({ at: Date.now(), ids, host: u.host, net: { ttfb: Math.round(t.responseStart), end: Math.round(t.responseEnd) } });
    } catch {}
  });
  const rows = [], shown = new Set();
  const url = withExt ? 'https://www.youtube.com/' : 'https://www.youtube.com/';
  await page.goto(url, { waitUntil: 'commit' });
  if (withExt) await page.evaluate(f => { for (const [k, on] of Object.entries(f)) on ? localStorage.setItem(k, '1') : localStorage.removeItem(k); }, { 'ytc-test-noready': flag('noready'), 'ytc-test-nohedge': flag('nohedge'), 'ytc-test-nodoc': flag('nodoc'), 'ytc-test-noimp': flag('noimp'), 'ytc-test-nokeep': flag('nokeep') }).catch(() => {});
  await page.waitForTimeout(6000);
  for (let i = 0; i <= N; i++) {
    // Everything this document showed counts as shown.
    const prev = await page.evaluate(() => window.__bench && window.__bench.frames).catch(() => []);
    for (const f of prev || []) for (const id of f.ids) shown.add(id);
    if (i === N) break;
    if (SLOW && browseMs.length >= 3) delayNext = Math.round(pct(browseMs, .5) * (SLOW - 1));
    else delayNext = DELAY;
    firstOfDoc = true;
    const start = Date.now();
    try { await page.reload({ waitUntil: 'commit' }); } catch (e) { console.log(`${label} #${i}: reload failed (${e.message.split('\n')[0]}), again`); await page.waitForTimeout(3000); i--; continue; }
    // Wait for a fresh frame (or 30 s).
    let row = null;
    for (let k = 0; k < CAP / 100 && !row; k++) {
      await page.waitForTimeout(100);
      const H = await page.evaluate(() => window.__bench).catch(() => null);
      if (!H) continue;
      const first = H.frames[0];
      if (!withExt && first) {
        // YouTube's own page renders its server's response (ytInitialData) once: the first frame is fresh.
        const ids = homeIds(await page.evaluate(() => window.ytInitialData).catch(() => ({})));
        row = { i, first: first.t, fresh: first.t, via: 'network', ids, delay: 0 };
        break;
      }
      for (const f of H.frames) {
        // The response a frame shows: the newest one whose ids contain the frame's cards in order
        // (Redline's filters only drop cards). Fresh = that response was never shown by an earlier load.
        const inOrder = r => { let k = 0; for (const id of r.ids) if (id === f.ids[k]) k++; return k === f.ids.length; };
        const resp = [...responses].reverse().find(inOrder);
        if (resp && !resp.used) {
          const marks = await page.evaluate(() => ({ ...document.documentElement.dataset })).catch(() => ({}));
          row = { i, first: first.t, fresh: f.t, stale_first: first !== f,
            via: resp.at < start ? 'prefetch' : 'network', host: resp.host, src: marks.ytcHomeSrc || '', age: +marks.ytcHomeAge || 0,
            hedge: marks.ytcHedge || '', ids: resp.ids, delay: delayNext, net: resp.net };
          resp.used = true;
          break;
        }
      }
      if (!row && Date.now() - start > CAP) row = { i, first: first ? first.t : NaN, fresh: NaN, via: 'none', delay: delayNext };
    }
    rows.push(row);
    console.log(`${label} #${i}: first ${row.first?.toFixed(0)} ms, fresh ${row.fresh?.toFixed?.(0)} ms, ${row.via}${row.src ? ' src=' + row.src : ''}${row.age ? ' age=' + row.age : ''}${row.hedge ? ' hedge=' + row.hedge : ''}${row.delay ? ' delay=' + row.delay : ''}${row.stale_first ? ' (old cards first)' : ''}`);
    if (opts.parity) {
      // YouTube's own Home in the same profile (same visitor), right now.
      const t = Date.now();
      await page.goto('https://www.youtube.com/?yt-native=1', { waitUntil: 'domcontentloaded' }).catch(() => {});
      await page.waitForFunction(() => window.ytInitialData, null, { timeout: 20000 }).catch(() => {});
      const yt = homeIds(await page.evaluate(() => window.ytInitialData).catch(() => ({})));
      row.yt = yt;
      row.parity = overlap(row.ids || [], yt);
      console.log(`  parity: Redline response ∩ YouTube's own page = ${(row.parity * 100).toFixed(0)} % (top 12: ${(overlap((row.ids || []).slice(0, 12), yt.slice(0, 12)) * 100).toFixed(0)} %) in ${Date.now() - t} ms`);
      // (YouTube's page may still be navigating itself: retry once.)
      await page.goto('https://www.youtube.com/', { waitUntil: 'commit' }).catch(() => page.waitForTimeout(1500).then(() => page.goto('https://www.youtube.com/', { waitUntil: 'commit' })));
      await page.waitForTimeout(3000);
    }
    await page.waitForTimeout(WAIT);
  }
  await ctx.close();
  return { rows, browseMs };
}

function summary(label, { rows, browseMs }) {
  const ok = rows.filter(r => r && Number.isFinite(r.fresh));
  const pre = ok.filter(r => r.via === 'prefetch'), net = ok.filter(r => r.via === 'network');
  console.log(`\n== ${label}`);
  console.log(`reload → fresh feed first paint: ${fmt(ok.map(r => r.fresh))}`);
  console.log(`  via prefetch: ${fmt(pre.map(r => r.fresh))}`);
  console.log(`  via network:  ${fmt(net.map(r => r.fresh))}`);
  console.log(`reload → first cards (any): ${fmt(ok.map(r => r.first))}; old cards painted first: ${ok.filter(r => r.stale_first).length}/${ok.length}`);
  console.log(`browse request (response end): ${fmt(browseMs)}`);
  const nets = rows.filter(r => r && r.net);
  if (nets.length) console.log(`browse body transfer after the first byte: ${fmt(nets.map(r => r.net.end - r.net.ttfb))}`);
  const hedged = ok.filter(r => r.hedge);
  if (hedged.length) console.log(`hedge fired: ${hedged.length}/${ok.length} (${hedged.map(r => r.hedge).join(', ')})`);
  // Consecutive responses: share of the first 12 ids that differ (YouTube's own variety, not a goal).
  const d = [];
  for (let i = 1; i < ok.length; i++) if (ok[i].ids && ok[i - 1].ids) d.push(1 - overlap(ok[i].ids.slice(0, 12), ok[i - 1].ids.slice(0, 12)));
  if (d.length) console.log(`consecutive loads: ${(d.reduce((a, b) => a + b, 0) / d.length * 100).toFixed(0)} % of the top 12 differ (YouTube's own variation)`);
  const par = rows.filter(r => r && Number.isFinite(r.parity));
  if (par.length) console.log(`parity with YouTube's own page at the same moment: mean ${(par.reduce((a, r) => a + r.parity, 0) / par.length * 100).toFixed(0)} % of ids shared`);
}

// Parity of Redline's Home with YouTube's own (YouTube's answers, before Redline's filters): overlap of
// the top 24 between loads of each, and how often YouTube's sticky picks (in ≥ 75 % of its own loads)
// appear in Redline's.
function parity(l, y) {
  const L = l.rows.filter(r => r && r.ids && r.ids.length).map(r => r.ids.slice(0, 24));
  const Y = y.rows.filter(r => r && r.ids && r.ids.length).map(r => r.ids.slice(0, 24));
  const mean = pairs => pairs.length ? (pairs.reduce((a, b) => a + b, 0) / pairs.length * 100).toFixed(0) + ' %' : 'n/a';
  const within = A => { const p = []; for (let i = 0; i < A.length; i++) for (let j = i + 1; j < A.length; j++) p.push(overlap(A[i], A[j])); return p; };
  const cross = []; for (const a of L) for (const b of Y) cross.push(overlap(a, b));
  const count = A => { const m = new Map(); for (const ids of A) for (const id of ids) m.set(id, (m.get(id) || 0) + 1); return m; };
  const cy = count(Y), cl = count(L);
  const sticky = [...cy].filter(([, n]) => n >= 0.75 * Y.length).map(([id]) => id);
  console.log(`\n== Parity (top 24 of YouTube's answer; ${L.length} Redline loads, ${Y.length} of YouTube's own)`);
  console.log(`  YouTube × YouTube ${mean(within(Y))} shared · Redline × Redline ${mean(within(L))} · Redline × YouTube ${mean(cross)}`);
  console.log(`  sticky picks (in ≥ 75 % of YouTube's own loads): ${sticky.length}; in Redline's loads: ${sticky.map(id => Math.round((cl.get(id) || 0) / L.length * 100) + ' %').join(', ') || '-'}`);
}

(async () => {
  const out = {};
  out.lumen = await run('lumen', true, { parity: flag('parity') });
  if (flag('yt')) out.yt = await run('youtube', false);
  summary(`Redline (${EXT})${DELAY ? ` +${DELAY} ms on the first browse` : ''}${SLOW ? ` first browse ${SLOW}× slower` : ''}`, out.lumen);
  if (out.yt) {
    summary("YouTube's own page (no extension)", out.yt);
    parity(out.lumen, out.yt);
  }
  if (process.env.BENCH_JSON) fs.writeFileSync(process.env.BENCH_JSON, JSON.stringify(out, null, 1));
})().catch(e => { console.error(e); process.exit(1); });
