// The README's screenshots (docs/screenshots/): the public build in Playwright's Chromium, signed out, with a
// fresh profile every run. Like tests/live-player.cjs it never loads a YouTube HTML page (Google rate-limits
// those): youtube.com documents are stubbed, while InnerTube, thumbnails, avatars, googlevideo media and
// SponsorBlock are real, so a run makes a few dozen requests. YouTube's own logging and the watch-history pings
// are answered locally; everything else is blocked. Requests ask for US English (`hl`/`gl`), so the pictures depend
// little on where they are taken (YouTube may still recommend a few local videos). Nothing is signed in: no account, avatar or notification can appear.
//
//   ./build.sh && node tools/screenshots/shoot.cjs            # all of them
//   node tools/screenshots/shoot.cjs watch search            # some of them
//
// Desktop pictures are 1440×900 CSS pixels at 2× (2880×1800; the watch page 1440×880), TV pictures 960×540 at 2× (1920×1080, the TV's own
// size), saved as WebP; mark.png is the README's logo; social-preview.png is 1280×640 for GitHub's social preview
// (Settings → General). Results change with YouTube's answers: look at every picture before committing it.
'use strict';
const { chromium } = require('playwright');
const fs = require('node:fs'), os = require('node:os'), path = require('node:path');
const { chromiumOptions, extensionDir } = require('../../tests/browser.cjs');

const ROOT = path.join(__dirname, '..', '..');
const EXT = extensionDir();
const OUT = path.resolve(process.env.YTC_SHOTS_OUT || path.join(ROOT, 'docs', 'screenshots'));
const VIDEO = 'aqz-KE-bpKQ'; // Big Buck Bunny (Blender Foundation, CC BY 3.0)
const QUERY = 'nature 4k';
// The mark's path, from its one source (the file's comment is dropped: it is not valid XML on its own).
const MARK = () => fs.readFileSync(path.join(ROOT, 'browser', 'icons', 'redline.svg'), 'utf8').replace(/<!--[\s\S]*?-->/g, '');
const MEDIA = new Set(['i.ytimg.com', 'yt3.ggpht.com', 'sponsor.ajay.app']);

// A browser with the extension and the network rules above. `tv`: the TV app's platform flag and a signed-out
// stand-in for its shell (what tv/shim/shim.js provides in the app).
async function launch({ width, height, tv = false }) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'redline-shots-'));
  const ctx = await chromium.launchPersistentContext(profile, {
    headless: true, ...chromiumOptions(), viewport: { width, height }, deviceScaleFactor: 2, colorScheme: 'dark', locale: 'en-US',
    args: [`--disable-extensions-except=${EXT}`, `--load-extension=${EXT}`, '--autoplay-policy=no-user-gesture-required', '--lang=en-US'],
  });
  await ctx.route('**/*', route => {
    const req = route.request(), url = new URL(req.url()), yt = url.hostname === 'www.youtube.com';
    if (url.protocol === 'chrome-extension:') return route.continue();
    if (yt && (url.pathname.startsWith('/api/stats/') || url.pathname.includes('log_event'))) return route.fulfill({ status: 204, body: '' });
    if (yt && url.pathname.startsWith('/youtubei/v1/') && req.method() === 'POST') {
      let body = req.postData();
      try { const j = JSON.parse(body); if (j.context && j.context.client) Object.assign(j.context.client, { hl: 'en', gl: 'US' }); body = JSON.stringify(j); } catch {}
      return route.continue({ postData: body });
    }
    if ((yt && ['/sw.js_data', '/api/timedtext', '/generate_204'].includes(url.pathname)) || url.hostname.endsWith('.googlevideo.com') || MEDIA.has(url.hostname)) return route.continue();
    if (yt && req.resourceType() === 'document') return route.fulfill({ status: 200, contentType: 'text/html', body: '<!DOCTYPE html><html><head></head><body></body></html>' });
    return route.abort();
  });
  const page = ctx.pages()[0];
  if (tv) await page.addInitScript(() => {
    localStorage.setItem('ytc:platform', 'tv');
    localStorage.setItem('ytc:shell', JSON.stringify({ auth_token: '{"token":null}', auth_start: null }));
  });
  for (let i = 0; i < 20; i++) { // the first load can come before the extension is ready
    await page.goto('https://www.youtube.com/hashtag/warmup', { waitUntil: 'commit' });
    await page.waitForTimeout(300);
    if (await page.locator('#ytc-top, html[data-ytc-tv]').count()) break;
  }
  await page.waitForTimeout(1500); // the stored config (visitor data) lands
  return { ctx, page };
}

const firstFrame = page => page.waitForFunction(() => document.documentElement.dataset.ytcFirstFrame || document.querySelector('#ytc-player.failed'), null, { timeout: 30000 });
// A paused picture at `t` seconds, its frame decoded.
async function still(page, t) {
  await firstFrame(page);
  if (await page.locator('#ytc-player.failed').count()) throw new Error('the player failed: ' + await page.locator('#ytc-player').innerText());
  await page.waitForTimeout(1000);
  await page.evaluate(t => { const v = document.getElementById('ytc-video'); v.pause(); v.currentTime = t; }, t);
  await page.waitForFunction(() => { const v = document.getElementById('ytc-video'); return !v.seeking && v.readyState >= 2; });
  await page.waitForTimeout(1500);
}

// PNG bytes → WebP (Chromium's encoder; no image tools needed).
async function webp(page, png, quality = 0.86) {
  const data = await page.evaluate(async ([b64, q]) => {
    const img = new Image(); img.src = 'data:image/png;base64,' + b64; await img.decode();
    const c = document.createElement('canvas'); c.width = img.naturalWidth; c.height = img.naturalHeight;
    c.getContext('2d').drawImage(img, 0, 0);
    return c.toDataURL('image/webp', q).split(',')[1];
  }, [png.toString('base64'), quality]);
  return Buffer.from(data, 'base64');
}
async function save(page, name, png, quality) {
  const file = path.join(OUT, name + '.webp');
  fs.writeFileSync(file, await webp(page, png, quality));
  console.log(`${path.relative(ROOT, file)}  ${Math.round(fs.statSync(file).size / 1024)} KB`);
}

const SHOTS = {
  // The README's mark: browser/icons/redline.svg as a PNG at 2× of 96×72.
  async mark() {
    const browser = await chromium.launch({ headless: true, ...chromiumOptions() });
    try {
      const page = await browser.newPage({ viewport: { width: 96, height: 72 }, deviceScaleFactor: 2 });
      await page.setContent(`<!doctype html><style>html,body{margin:0;background:transparent}svg{display:block;width:96px;height:72px}</style>${MARK()}`);
      const file = path.join(OUT, 'mark.png');
      await page.screenshot({ path: file, omitBackground: true });
      console.log(`${path.relative(ROOT, file)}  ${Math.round(fs.statSync(file).size / 1024)} KB`);
    } finally { await browser.close(); }
  },
  // The watch page: our player paused on a frame, the pointer on the seek bar (storyboard preview).
  async watch() {
    const { ctx, page } = await launch({ width: 1440, height: 900 });
    try {
      await page.goto(`https://www.youtube.com/watch?v=${VIDEO}`, { waitUntil: 'commit' });
      await still(page, 100);
      const b = await page.locator('#ytc-seek').boundingBox();
      await page.mouse.move(b.x + b.width * 0.3, b.y + b.height / 2, { steps: 4 });
      await page.mouse.move(b.x + b.width * 0.36, b.y + b.height / 2, { steps: 4 });
      await page.waitForTimeout(1200);
      await save(page, 'watch', await page.screenshot({ clip: { x: 0, y: 0, width: 1440, height: 880 } }));
      // The quality menu (it opens on hover), cropped to the player's corner.
      await page.mouse.move(700, 300);
      await page.hover('#ytc-qual');
      await page.waitForSelector('#ytc-menu', { state: 'visible' });
      await page.waitForTimeout(600);
      const p = await page.locator('#ytc-player').boundingBox();
      await save(page, 'player-quality', await page.screenshot({ clip: { x: p.x + p.width - 560, y: p.y + p.height - 440, width: 560, height: 440 } }));
    } finally { await ctx.close(); }
  },
  // Search results (the grid layout of Home, Subscriptions and channels too).
  async search() {
    const { ctx, page } = await launch({ width: 1440, height: 900 });
    try {
      await page.goto('https://www.youtube.com/results?search_query=' + encodeURIComponent(QUERY), { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c:not(.sk)');
      await page.mouse.move(1430, 890);
      await page.waitForFunction(() => [...document.querySelectorAll('#ytc-list .c img')].slice(0, 12).every(i => i.complete && i.naturalWidth));
      await page.waitForTimeout(1000);
      await save(page, 'search', await page.screenshot(), 0.6); // (a page of photographs: a smaller quality keeps it light)
    } finally { await ctx.close(); }
  },
  // The browser toolbar's Redline popup (SponsorSkip), categories open.
  async popup() {
    const { ctx, page } = await launch({ width: 360, height: 600 });
    try {
      let [sw] = ctx.serviceWorkers();
      if (!sw) sw = await ctx.waitForEvent('serviceworker');
      const id = new URL(sw.url()).host;
      await page.goto(`chrome-extension://${id}/popup.html`);
      await page.waitForSelector('#ytc-popup details');
      await page.evaluate(() => { for (const d of document.querySelectorAll('#ytc-popup details')) d.open = true; });
      await page.waitForTimeout(500);
      const h = await page.evaluate(() => Math.ceil(document.getElementById('ytc-popup').getBoundingClientRect().height));
      await save(page, 'popup', await page.screenshot({ clip: { x: 0, y: 0, width: 360, height: h } }), 0.92);
    } finally { await ctx.close(); }
  },
  // Redline for TV: search results under the focus frame, and the player with its Quality menu (opened with the remote).
  async tv() {
    const { ctx, page } = await launch({ width: 960, height: 540, tv: true });
    try {
      await page.goto('https://www.youtube.com/results?search_query=' + encodeURIComponent(QUERY), { waitUntil: 'commit' });
      await page.waitForSelector('#ytc-list .c:not(.sk)');
      await page.waitForFunction(() => [...document.querySelectorAll('#ytc-list .c img')].slice(0, 8).every(i => i.complete && i.naturalWidth));
      await page.waitForTimeout(1500);
      await save(page, 'tv-search', await page.screenshot());
      await page.goto(`https://www.youtube.com/watch?v=${VIDEO}`, { waitUntil: 'commit' });
      await still(page, 140);
      await page.keyboard.press('ArrowDown'); // any key shows the controls, focus on Play / pause
      await page.waitForTimeout(500);
      for (let i = 0; i < 6 && !(await page.evaluate(() => document.activeElement.id === 'ytc-qual')); i++) { await page.keyboard.press('ArrowRight'); await page.waitForTimeout(300); }
      await page.keyboard.press('Enter');
      await page.waitForTimeout(1500);
      await save(page, 'tv-player', await page.screenshot());
    } finally { await ctx.close(); }
  },
  // GitHub's social preview (1280×640): the mark, the name, the tagline and the watch picture.
  async social() {
    const browser = await chromium.launch({ headless: true, ...chromiumOptions() });
    try {
      const page = await browser.newPage({ viewport: { width: 1280, height: 640 }, deviceScaleFactor: 1 });
      const mark = MARK();
      const shot = fs.readFileSync(path.join(OUT, 'watch.webp')).toString('base64');
      await page.setContent(`<!doctype html><html><head><style>
        html,body{margin:0;width:1280px;height:640px;overflow:hidden;background:#0f0f0f;color:#f1f1f1;font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,sans-serif}
        body{position:relative;background:radial-gradient(900px 520px at 100% 100%,rgba(255,0,51,.30),transparent 72%),#0f0f0f}
        .text{position:absolute;left:84px;top:0;bottom:0;width:470px;display:flex;flex-direction:column;justify-content:center}
        .mark svg{display:block;width:96px;height:72px}
        h1{margin:28px 0 0;font-size:88px;line-height:1;font-weight:700;letter-spacing:-2px}
        p{margin:20px 0 0;font-size:32px;line-height:1.25;color:#cfcfcf;font-weight:500}
        small{display:block;margin-top:34px;font-size:21px;color:#9a9a9a;font-weight:500}
        .shot{position:absolute;left:600px;top:96px;width:900px;border-radius:18px;box-shadow:0 0 0 1px rgba(255,255,255,.08),0 30px 80px rgba(0,0,0,.6)}
      </style></head><body><div class="text"><div class="mark">${mark}</div><h1>Redline</h1>
        <p>A fast, ad-free client for YouTube</p><small>Chrome · Brave · Edge · Google TV</small></div>
        <img class="shot" src="data:image/webp;base64,${shot}"></body></html>`);
      await page.locator('img').evaluate(i => i.decode());
      const file = path.join(OUT, 'social-preview.png');
      await page.screenshot({ path: file });
      console.log(`${path.relative(ROOT, file)}  ${Math.round(fs.statSync(file).size / 1024)} KB`);
    } finally { await browser.close(); }
  },
};

(async () => {
  const names = process.argv.slice(2);
  for (const n of names) if (!SHOTS[n]) throw new Error(`unknown shot ${n} (${Object.keys(SHOTS).join(', ')})`);
  fs.mkdirSync(OUT, { recursive: true });
  for (const n of names.length ? names : Object.keys(SHOTS)) await SHOTS[n]();
})().catch(e => { console.error(e); process.exit(1); });
