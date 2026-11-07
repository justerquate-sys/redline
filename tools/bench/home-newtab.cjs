// Live: how fast a NEW TAB (or window) shows Home, vs going from a video back to Home, vs YouTube's own new tab.
// Talks to the real youtube.com (signed out, seeded profile: tools/bench/profile.cjs); run by hand, rarely.
//
//   node tools/bench/home-newtab.cjs [--n=10] [--modes=ready,fresh,playing,video,yt]
//
//   ready  another tab is on Home (its ready Home response is in memory): open a new tab to youtube.com
//   fresh  another tab is on a watch page (watching discards the ready response): open a new tab
//   playing another tab plays a video (a new one from Home each time) for --play seconds (default 70: the ready
//          response refills at most once a minute): open a new tab
//   video  in one tab, a watch page → click the logo (Redline's video → Home)
//   yt     YouTube's own page in a new tab (a separate run without the extension)
//   --noready  no ready responses at all (localStorage ytc-test-noready): every Home takes the network path
//   --nokeep   Home's network path fetches YouTube's Home document instead of reading the tab's own (ytc-test-nokeep)
//   YTC_EXT  the extension folder (default extension/; a development build gives the boot marks)
//
// "Home painted" = the first frame whose Home grid shows video cards, measured in the page (performance.now():
// from the tab's navigation start; for `video` from the logo press). Printed per mode: p50/p90 and Redline's
// own breakdown (YouTube's document response start, the content script, the shell, where Home came from).
'use strict';
const { extensionDir } = require('../../tests/browser.cjs');
const { seeded, launch } = require('./profile.cjs');
const arg = (k, d) => { const a = process.argv.find(x => x.startsWith(`--${k}=`)); return a ? a.slice(k.length + 3) : d; };
const N = +arg('n', 10), MODES = arg('modes', 'ready,fresh,playing,video,yt').split(','), PLAY = +arg('play', 70);
const EXT = extensionDir();
const pct = (a, p) => { if (!a.length) return NaN; const s = [...a].sort((x, y) => x - y); return s[Math.min(s.length - 1, Math.floor(p * s.length))]; };
const fmt = a => a.length ? `p50 ${pct(a, .5).toFixed(0)} ms, p90 ${pct(a, .9).toFixed(0)} ms (n=${a.length})` : 'n=0';

// In every document: the first frame with Home video cards (Redline's grid or YouTube's own), and a pointerdown stamp.
const PROBE = `(() => {
  const H = window.__nt = { first: null, down: null };
  addEventListener('pointerdown', () => { H.down = performance.now(); H.first = null; }, true);
  const tick = t => {
    if (H.first === null && document.querySelector('#ytc-list .c[data-v] a.t[href^="/watch"], ytd-rich-item-renderer a#thumbnail[href*="/watch"], ytd-rich-item-renderer a[href*="/watch?v="]')) H.first = performance.now();
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
})();`;

async function newTab(ctx) {
  const p = await ctx.newPage();
  await p.goto('https://www.youtube.com/', { waitUntil: 'commit' });
  for (let k = 0; k < 300; k++) {
    await p.waitForTimeout(50);
    const r = await p.evaluate(() => {
      const H = window.__nt; if (!H || H.first === null) return null;
      const nav = performance.getEntriesByType('navigation')[0] || {};
      const d = document.documentElement.dataset;
      return { first: H.first, resp: nav.responseStart, script: +d.ytcScript || NaN, shell: +d.ytcShell || NaN, src: (d.ytcHomeSrc || '') + (d.ytcHomeDoc ? '/' + d.ytcHomeDoc : '') + (d.ytcHedge ? '/hedge:' + d.ytcHedge : '') };
    }).catch(() => null);
    if (r) return { p, r };
  }
  return { p, r: null };
}

async function runLumen(mode) {
  const ctx = await launch(await seeded(arg('profile', 'main')), EXT);
  await ctx.addInitScript(PROBE);
  const base = ctx.pages()[0] || await ctx.newPage();
  const rows = [];
  // The base tab: Home (ready) or a watch page (fresh / video).
  await base.goto('https://www.youtube.com/results?search_query=lumen', { waitUntil: 'commit' });
  await base.evaluate(f => { for (const [k, on] of Object.entries(f)) on ? localStorage.setItem(k, '1') : localStorage.removeItem(k); },
    { 'ytc-test-noready': process.argv.includes('--noready'), 'ytc-test-nokeep': process.argv.includes('--nokeep') });
  await base.goto('https://www.youtube.com/', { waitUntil: 'commit' });
  await base.waitForSelector('#ytc-list .c[data-v]', { timeout: 30000 });
  await base.mouse.move(300, 300);
  if (mode !== 'ready') {
    await base.click('#ytc-list a.t[href^="/watch"]');
    await base.waitForSelector('#ytc-info h1.wt', { timeout: 20000 });
  }
  await base.waitForTimeout(5000);
  for (let i = 0; i < N; i++) {
    if (mode === 'video') {
      // watch page → logo
      if (!(await base.locator('#ytc-info h1.wt').count())) {
        await base.click('#ytc-list a.t[href^="/watch"]');
        await base.waitForSelector('#ytc-info h1.wt', { timeout: 20000 });
        await base.waitForTimeout(4000);
      }
      await base.click('#ytc-logo');
      let r = null;
      for (let k = 0; k < 300 && !r; k++) { await base.waitForTimeout(50); r = await base.evaluate(() => window.__nt.first !== null && window.__nt.down !== null ? { first: window.__nt.first - window.__nt.down, src: document.documentElement.dataset.ytcHomeSrc || '' } : null).catch(() => null); }
      rows.push(r);
      console.log(`${mode} #${i}: ${r ? r.first.toFixed(0) + ' ms ' + r.src : 'none'}`);
      await base.waitForTimeout(2000);
      continue;
    }
    if (mode === 'playing') {
      // A new video in the base tab (the related list, else Home's), playing for PLAY s.
      await base.bringToFront();
      // (Home's i-th card: opening it discards whatever Home fetched ahead, as watching does.)
      await base.click('#ytc-logo');
      await base.waitForSelector('#ytc-list .c[data-v]');
      await base.locator('#ytc-list .c[data-v] a.t[href^="/watch"]').nth(2 + i).click();
      await base.waitForSelector('#ytc-info h1.wt', { timeout: 20000 });
      await base.waitForTimeout(PLAY * 1000);
      const st = await base.evaluate(() => { const v = document.querySelector('video'); return v ? `${v.currentTime.toFixed(0)} s played` : 'no video'; });
      process.stdout.write(`  (${st}) `);
    }
    const { p, r } = await newTab(ctx);
    rows.push(r);
    console.log(`${mode} #${i}: ${r ? `${r.first.toFixed(0)} ms (document ${r.resp.toFixed(0)}, script ${r.script.toFixed(0)}, shell ${r.shell.toFixed(0)}) ${r.src}` : 'none'}`);
    // Let the new tab settle (its ready response refill), then close it.
    await p.waitForTimeout(mode === 'ready' ? 5000 : 1500);
    await p.close();
    if (mode === 'ready') { await base.bringToFront(); await base.mouse.move(310 + i, 300); await base.waitForTimeout(1000); }
  }
  await ctx.close();
  return rows.filter(Boolean);
}

async function runYt() {
  const ctx = await launch(await seeded(arg('profile', 'main')), null);
  await ctx.addInitScript(PROBE);
  const base = ctx.pages()[0] || await ctx.newPage();
  await base.goto('https://www.youtube.com/', { waitUntil: 'domcontentloaded' });
  await base.waitForTimeout(3000);
  const rows = [];
  for (let i = 0; i < N; i++) {
    const { p, r } = await newTab(ctx);
    rows.push(r);
    console.log(`yt #${i}: ${r ? `${r.first.toFixed(0)} ms (document ${r.resp.toFixed(0)})` : 'none'}`);
    await p.waitForTimeout(1500);
    await p.close();
  }
  await ctx.close();
  return rows.filter(Boolean);
}

(async () => {
  const out = {};
  for (const m of MODES) out[m] = m === 'yt' ? await runYt() : await runLumen(m);
  console.log(`\n== Home painted (${EXT})`);
  for (const m of MODES) {
    const rows = out[m];
    const srcs = {}; for (const r of rows) srcs[r.src || '-'] = (srcs[r.src || '-'] || 0) + 1;
    console.log(`${m.padEnd(6)} ${fmt(rows.map(r => r.first))}${rows[0] && rows[0].resp !== undefined ? ` · document response ${fmt(rows.map(r => r.resp))}` : ''}${m !== 'yt' ? ' · from ' + JSON.stringify(srcs) : ''}`);
    if (m !== 'yt' && m !== 'video') console.log(`       content script → shell ${fmt(rows.map(r => r.shell - r.script).filter(Number.isFinite))}; document → content script ${fmt(rows.map(r => r.script - r.resp).filter(Number.isFinite))}`);
  }
})().catch(e => { console.error(e); process.exit(1); });
