// Live: a realistic session with Redline (signed-out seeded profile, tools/bench/profile.cjs), and how
// each Home paint was served (ready response / network / logo prefetch) plus the Home requests it cost.
//   node tools/bench/home-session.cjs [--idle=15] (minutes)
// Steps: open Home · read 5 s · open a video, watch 20 s · logo → Home · read 5 s · reload · idle
// (no input) · reload · read 5 s · reload. Talks to the real youtube.com: run by hand, rarely.
'use strict';
const { extensionDir } = require('../../tests/browser.cjs');
const { seeded, launch } = require('./profile.cjs');
const arg = (k, d) => { const a = process.argv.find(x => x.startsWith(`--${k}=`)); return a ? a.slice(k.length + 3) : d; };
const IDLE = +arg('idle', 15);

(async () => {
  const ctx = await launch(await seeded(arg('profile', 'main')), extensionDir());
  const page = ctx.pages()[0] || await ctx.newPage();
  const home = [];
  page.on('request', r => {
    const u = r.url();
    if (!u.includes('/youtubei/v1/browse')) return;
    try { const b = JSON.parse(r.postData() || '{}'); if (b.browseId === 'FEwhat_to_watch') home.push(Date.now()); } catch {}
  });
  const paints = [];
  const painted = async (what, t0) => {
    await page.waitForFunction(() => document.querySelector('#ytc-list .c[data-v]') && document.documentElement.dataset.ytcHomeSrc, null, { timeout: 30000 });
    const m = await page.evaluate(() => ({ src: document.documentElement.dataset.ytcHomeSrc, age: +document.documentElement.dataset.ytcHomeAge || 0 }));
    paints.push({ what, ...m, ms: Date.now() - t0, reqs: home.length });
    console.log(`${what}: ${m.src}${m.src === 'ready' ? ` (age ${(m.age / 1000).toFixed(0)} s)` : ''} · ${Date.now() - t0} ms · Home requests so far ${home.length}`);
  };
  const reload = async what => { await page.evaluate(() => { delete document.documentElement.dataset.ytcHomeSrc; }); const t = Date.now(); await page.reload({ waitUntil: 'commit' }); await painted(what, t); };
  // A clean start: no measurement switches left from other benchmark runs in this profile.
  await page.goto('https://www.youtube.com/results?search_query=lumen', { waitUntil: 'commit' });
  await page.evaluate(() => { localStorage.removeItem('ytc-test-noready'); localStorage.removeItem('ytc-test-nohedge'); });
  let t = Date.now();
  await page.goto('https://www.youtube.com/', { waitUntil: 'commit' });
  await painted('open Home', t);
  await page.waitForTimeout(5000);
  await page.click('#ytc-list a.t[href^="/watch"]');
  await page.waitForSelector('#ytc-info h1.wt');
  await page.waitForTimeout(20000);
  await page.evaluate(() => { delete document.documentElement.dataset.ytcHomeSrc; });
  t = Date.now();
  await page.click('#ytc-logo');
  await painted('logo → Home after watching', t);
  await page.waitForTimeout(5000);
  await reload('reload');
  console.log(`idle ${IDLE} min…`);
  await page.waitForTimeout(IDLE * 60000);
  await reload(`reload after ${IDLE} min idle`);
  await page.waitForTimeout(5000);
  await reload('reload');
  await page.waitForTimeout(3000);
  const ready = paints.filter(p => p.src === 'ready').length;
  console.log(`\nHome paints ${paints.length}: from the ready response ${ready} (${Math.round(ready / paints.length * 100)} %), ${paints.map(p => p.src).join(', ')}`);
  console.log(`Home requests (first pages) ${home.length} for ${paints.length} Home paints: ${(home.length / paints.length).toFixed(2)} per paint (YouTube's own page: 1)`);
  await ctx.close();
})().catch(e => { console.error(e); process.exit(1); });
