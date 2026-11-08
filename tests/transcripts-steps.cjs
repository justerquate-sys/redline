// The Transcripts feature, end to end, against the real extension in Chromium (tests/e2e.cjs runs these on its public
// build). The helper is a mock (tests/transcripts-mock.cjs), and one step runs the real helper built from helper/ on a
// temporary root and an ephemeral port with a loopback Speech server. Nothing reaches a real helper or Azure.
'use strict';
const fs = require('node:fs'), os = require('node:os'), path = require('node:path'), http = require('node:http');
const { spawn, spawnSync, execFileSync } = require('node:child_process');
const assert = require('node:assert/strict');
const mockHelper = require('./transcripts-mock.cjs');

const TOKEN = 'e2e-pairing-token-7f3a91c2d4b64e0f8a15';
const EXT_ID = 'lamdpdfcpkihifbpmcalmjdnbfegjjjh';

/** Called while the temporary copy of the public build is made: points its worker at the mock and records its calls.
    Only that copy changes; the public build has no such switch. */
async function prepare(load) {
  const mock = await mockHelper.start(TOKEN);
  const m = path.join(load, 'manifest.json'), manifest = JSON.parse(fs.readFileSync(m, 'utf8'));
  manifest.host_permissions.push('http://127.0.0.1:*/*');
  fs.writeFileSync(m, JSON.stringify(manifest, null, 2));
  const b = path.join(load, 'background.js');
  let src = fs.readFileSync(b, 'utf8');
  const line = "const TR_HELPER = 'http://127.0.0.1:8765';";
  if (!src.includes(line)) return null; // an older build, for the speed comparison
  src = src.replace(line, `let TR_HELPER = ${JSON.stringify(mock.origin)};`);
  fs.writeFileSync(b, `// ---- e2e only (prepended to a temporary copy of the public build).
self.__tr = { fetches: [], native: [], icons: [], nativeMode: 'ok', token: ${JSON.stringify(TOKEN)} };
{
  const f = self.fetch.bind(self);
  self.fetch = (u, o) => { const s = String(u); if (s.startsWith('http://127.0.0.1') && !s.includes('/incident')) self.__tr.fetches.push(s); return f(u, o); };
  chrome.runtime.sendNativeMessage = (host, message) => {
    self.__tr.native.push([host, message]);
    if (self.__tr.nativeMode === 'missing') return Promise.reject(new Error('Specified native messaging host not found.'));
    return Promise.resolve({ ok: true, url: TR_HELPER, pairingToken: self.__tr.token });
  };
  const icon = chrome.action.setIcon.bind(chrome.action);
  chrome.action.setIcon = o => { self.__tr.icons.push(o.imageData ? Object.keys(o.imageData) : 'manifest'); return icon(o); };
}
${src}`);
  return mock;
}

async function waitFor(fn, what, timeout = 8000) {
  const t = Date.now();
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() - t > timeout) throw new Error('timed out: ' + what);
    await new Promise(r => setTimeout(r, 50));
  }
}

async function steps({ step, ctx, sw, mock, incidents, HOME, CT_SKY }) {
  HOME.rich = true;
  const shots = process.env.YTC_TR_SHOTS;
  const ext = p => `chrome-extension://${EXT_ID}/${p}`;
  const worker = (f, arg) => sw().evaluate(f, arg);
  const native = () => worker(() => self.__tr.native.length);
  const fetches = () => worker(() => self.__tr.fetches.length);
  const open = async (url, width = 1400, height = 900) => {
    const p = await ctx.newPage();
    await p.setViewportSize({ width, height });
    await p.addInitScript(() => { try { localStorage.setItem('ytc-test-nowatchdog', '1'); } catch {} });
    p.errors = [];
    p.on('pageerror', e => p.errors.push(String(e)));
    p.on('console', m => { if (m.type() === 'error' && !/ERR_BLOCKED_BY_CLIENT|Failed to load resource|ERR_CONNECTION/.test(m.text())) p.errors.push(m.text()); });
    await p.goto(url, { waitUntil: 'commit' });
    return p;
  };
  const home = async (w, h) => { const p = await open('https://www.youtube.com/', w, h); await p.waitForSelector('#ytc-list .c[data-v] .t'); return p; };
  const enable = async () => {
    mock.state.configured = true; mock.state.hasKey = true; mock.state.endpoint = 'https://resource.cognitiveservices.azure.com';
    await worker(t => chrome.storage.local.set({ ytcTranscriptsV1: { on: true } }));
  };
  const disable = () => worker(() => chrome.storage.local.remove('ytcTranscriptsV1'));
  const shot = async (p, name) => { if (shots) { fs.mkdirSync(shots, { recursive: true }); await p.screenshot({ path: path.join(shots, name + '.png') }); } };
  const box = (p, sel) => p.evaluate(s => { const n = document.querySelector(s); if (!n) return null; const r = n.getBoundingClientRect(); return { x: r.x, y: r.y, w: r.width, h: r.height, r: r.right, b: r.bottom }; }, sel);
  const overlap = (a, b) => a.x < b.r && b.x < a.r && a.y < b.b && b.y < a.b;
  const chipOf = id => `.tsc[data-arg="${id}"]`;

  // ---- someone who never set it up: nothing at all.
  await step('transcripts off: Home, watch and channel pages have no controls, no extra card DOM, no request and no native attempt', async () => {
    await disable();
    // (An earlier step in this browser session, members-only and Transcripts, may have loaded it: what counts is this.)
    await worker(() => { trCore = null; });
    const [f0, n0, m0] = [await fetches(), await native(), mock.log.length];
    const p = await home();
    await p.waitForTimeout(500);
    assert.equal(await p.locator('.tsc').count(), 0, 'no chip');
    const card = await p.locator('#ytc-list .c[data-v]').first().evaluate(c => c.innerHTML);
    assert.ok(!/tsc|transcript/i.test(card), 'no extra DOM on a card');
    const id = await p.locator('#ytc-list .c[data-v]').first().getAttribute('data-v');
    await p.goto(`https://www.youtube.com/watch?v=${id}`, { waitUntil: 'commit' });
    await p.waitForSelector('#ytc-acts .ya-slot', { timeout: 10000 });
    assert.equal(await p.locator('.tsc-w').count(), 0, 'no watch button');
    await p.goto('https://www.youtube.com/@monkeydeveloper/videos', { waitUntil: 'commit' });
    await p.waitForSelector('.ch h1');
    assert.equal(await p.locator('.tsc-ch').count(), 0, 'no channel controls');
    await p.waitForTimeout(300);
    assert.equal(await fetches(), f0, 'no request to 127.0.0.1');
    assert.equal(await native(), n0, 'no native-messaging attempt');
    assert.equal(mock.log.length, m0, 'the helper saw nothing');
    assert.equal(await worker(() => trCore === null), true, 'nothing on these pages asked the worker to load the second Wasm');
    assert.equal(await p.evaluate(() => [...performance.getEntriesByType('resource')].filter(e => e.name.includes('127.0.0.1')).length), 0);
    assert.deepEqual(await worker(() => chrome.alarms.getAll().then(a => a.length)), 0, 'no alarm');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  await step('transcripts off: the popup shows one quiet row; it leads to settings; nothing else, no badge', async () => {
    const [f0, n0] = [await fetches(), await native()];
    const p = await open(ext('popup.html'), 420, 600);
    await p.waitForSelector('#popup-tr-setup');
    assert.equal((await p.locator('#popup-tr').innerText()).replace(/\s+/g, ' ').trim(), 'Transcripts Set up');
    assert.equal(await p.locator('script[src="transcripts-core.js"]').count(), 0, 'the second Wasm is not loaded');
    assert.equal(await worker(() => chrome.action.getBadgeText({})), '', 'no badge');
    assert.equal(await worker(() => chrome.action.getTitle({})), 'Redline preferences');
    await p.waitForTimeout(200);
    assert.equal(await fetches(), f0);
    assert.equal(await native(), n0);
    await shot(p, 'popup-setup');
    const opened = ctx.waitForEvent('page');
    await p.click('#popup-tr-setup');
    const s = await opened;
    await s.waitForURL(u => u.href.endsWith('/settings.html'));
    await s.close();
    await p.close();
  });

  // ---- settings: the one place that probes.
  await step('transcripts settings: a missing helper gets a clear message; the flag stays off', async () => {
    await worker(() => { self.__tr.nativeMode = 'missing'; });
    await worker(() => TR.vault_del('pairingToken'));
    const n0 = await native();
    const p = await open(ext('settings.html'));
    await p.waitForFunction(() => document.querySelector('#helper-status')?.textContent === 'Helper unavailable');
    assert.ok((await native()) > n0, 'opening settings is what probes');
    assert.match(await p.locator('#helper-notice').textContent(), /cannot find the helper/);
    assert.match(await p.locator('#helper-description').textContent(), /Install Helper\.command/);
    assert.equal(await worker(() => chrome.storage.local.get('ytcTranscriptsV1').then(o => o.ytcTranscriptsV1 ?? null)), null);
    await shot(p, 'settings-helper-missing');
    await p.close();
    await worker(() => { self.__tr.nativeMode = 'ok'; });
  });

  await step('transcripts settings: pairing, Foundry URL, the key never in browser storage, a blank key keeps the saved one, test connection, tools, Brave login, save folder, manual pairing', async () => {
    mock.reset();
    const p = await open(ext('settings.html'));
    await p.waitForFunction(() => document.querySelector('#helper-status')?.textContent === 'Helper connected');
    assert.match(await p.locator('#health-details').innerText(), /Video downloader\s+Ready/);
    assert.match(await p.locator('#health-details').innerText(), /Audio processing\s+Ready/);
    assert.equal(await p.locator('#azure-status').textContent(), 'Add your Azure connection');
    assert.equal(await worker(() => chrome.storage.local.get('ytcTranscriptsV1').then(o => o.ytcTranscriptsV1 ?? null)), null, 'paired but no Azure: still off');
    await p.fill('#azure-endpoint', 'https://myres.services.ai.azure.com/api/projects/myproj');
    await p.fill('#azure-key', 'e2e-azure-key-value');
    await p.click('#save-azure');
    await p.waitForFunction(() => document.querySelector('#azure-status')?.textContent === 'Configuration saved');
    assert.equal(await p.inputValue('#azure-endpoint'), 'https://myres.cognitiveservices.azure.com', 'a Foundry project URL converts');
    assert.equal(await p.inputValue('#azure-key'), '', 'the key leaves the field');
    assert.equal(mock.state.saveBodies[0].api_key, 'e2e-azure-key-value');
    assert.match(await p.getAttribute('#azure-key', 'placeholder'), /leave blank/);
    // Enabled now, and the key is nowhere in the browser.
    await waitFor(() => worker(() => chrome.storage.local.get('ytcTranscriptsV1').then(o => o.ytcTranscriptsV1?.on === true)), 'the enabled flag');
    const stored = JSON.stringify(await worker(() => chrome.storage.local.get(null)));
    assert.ok(!stored.includes('e2e-azure-key-value'), 'no Azure key in storage');
    assert.ok(!stored.includes(TOKEN), 'no pairing token in chrome.storage.local');
    // A blank key keeps the saved one.
    await p.fill('#azure-endpoint', 'https://myres.cognitiveservices.azure.com');
    await p.check('#use-brave-login');
    assert.equal(await p.isDisabled('#test-azure'), true, 'unsaved changes hold the test back');
    await p.click('#save-azure');
    await p.waitForFunction(() => document.querySelector('#notice')?.textContent.includes('saved'));
    assert.ok(!('api_key' in mock.state.saveBodies[1]), 'a blank key is not sent');
    assert.equal(mock.state.saveBodies[1].use_brave_login, true);
    assert.equal(mock.state.hasKey, true);
    // Test connection.
    await p.click('#test-azure');
    await p.waitForFunction(() => document.querySelector('#azure-status')?.textContent === 'Azure connection verified');
    assert.equal(mock.state.tested, 1);
    // Save folder: the system picker through its polling route, then the automatic-saving choices.
    assert.equal(await p.locator('#export-directory').textContent(), '/synthetic/Transcripts');
    await p.click('#choose-directory');
    await p.waitForFunction(() => document.querySelector('#export-directory')?.textContent === '/synthetic/Chosen', null, { timeout: 8000 });
    await p.waitForSelector('#choose-directory:not([disabled])');
    await p.uncheck('#auto-export');
    await p.selectOption('#export-format', 'txt');
    await p.click('#save-export');
    await p.waitForFunction(() => document.querySelector('#export-status')?.textContent === 'Automatic saving off');
    assert.equal(mock.state.export.format, 'txt');
    // Manual pairing, then forgetting it; the flag does not depend on it.
    await p.evaluate(() => { document.querySelector('#pairing').open = true; });
    await p.fill('#pairing-token', 'short');
    await p.click('#save-token');
    await p.waitForFunction(() => document.querySelector('#pair-notice')?.textContent.includes('complete'));
    await p.fill('#pairing-token', TOKEN);
    await p.click('#save-token');
    await p.waitForFunction(() => document.querySelector('#pair-notice')?.textContent.includes('verified'));
    assert.equal(await worker(() => TR.vault_get('pairingToken')), TOKEN);
    await p.click('#forget-token');
    await p.waitForFunction(() => document.querySelector('#pair-notice')?.textContent.includes('removed'));
    assert.equal(await worker(() => TR.vault_get('pairingToken')), '');
    await p.click('#auto-pair');
    await p.waitForFunction(() => document.querySelector('#pair-notice')?.textContent.includes('complete'));
    assert.equal(await worker(() => TR.vault_get('pairingToken')), TOKEN, 'automatic pairing again');
    await shot(p, 'settings-1400');
    await p.setViewportSize({ width: 480, height: 900 });
    await shot(p, 'settings-480');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  // ---- cards.
  await step('transcripts cards: a chip on hover or keyboard focus at the thumbnail\'s top right, aligned, overlapping nothing, never in the way; one status request per page, nothing while idle', async () => {
    mock.reset();
    await enable();
    const p = await home();
    await p.waitForSelector('#ytc-list .c[data-v] .tsc');
    const card = '#ytc-list .c[data-v]:has(.th b):not(:has(.lv))';
    const id = await p.locator(card).first().getAttribute('data-v');
    const chip = `${card} .tsc`;
    // Every card has one, painted right away; hidden at rest.
    assert.equal(await p.locator('#ytc-list .c[data-v]:not(:has(.tsc))').count(), await p.locator('#ytc-list .c[data-v]:has(.chc)').count());
    assert.equal(await p.locator(chip).first().evaluate(n => getComputedStyle(n).opacity), '0');
    assert.equal(await p.locator(chip).first().evaluate(n => getComputedStyle(n).pointerEvents), 'none', 'it cannot block the video while hidden');
    assert.equal(await p.locator(chip).first().getAttribute('aria-label'), 'Full transcript');
    assert.equal(await p.locator(chip).first().getAttribute('title'), 'Full transcript');
    // Hover: visible, in the corner, 6 px in, the duration badge's height and surface.
    await p.locator(card).first().locator('a.th').hover();
    await p.waitForTimeout(250);
    assert.equal(await p.locator(chip).first().evaluate(n => getComputedStyle(n).opacity), '1');
    const th = await p.locator(card).first().locator('a.th').boundingBox();
    const c = await p.locator(chip).first().boundingBox();
    assert.ok(Math.abs(th.x + th.width - (c.x + c.width) - 6) <= 1.5, `right inset ${th.x + th.width - c.x - c.width}`);
    assert.ok(Math.abs(c.y - th.y - 6) <= 1.5, `top inset ${c.y - th.y}`);
    const d = await p.locator(card).first().locator('a.th > b').first().boundingBox();
    assert.ok(Math.abs(c.height - d.height) <= 1, `chip ${c.height} vs duration ${d.height}`);
    assert.ok(!(c.x < d.x + d.width && d.x < c.x + c.width && c.y < d.y + d.height && d.y < c.y + c.height), 'no overlap with the duration');
    const same = await p.locator(chip).first().evaluate(n => { const b = n.parentElement.querySelector('b'); const s = getComputedStyle(n), t = getComputedStyle(b); return s.backgroundColor === t.backgroundColor && s.borderRadius === t.borderRadius; });
    assert.ok(same, 'wears the duration badge\'s surface');
    const icon = await p.locator(chip).first().locator('svg').boundingBox();
    assert.ok(Math.abs(icon.x + icon.width / 2 - (c.x + c.width / 2)) <= 1 && Math.abs(icon.y + icon.height / 2 - (c.y + c.height / 2)) <= 1, 'the icon is centred');
    await shot(p, 'card-hover-1400');
    await p.mouse.move(2, 2);
    // Keyboard focus shows it too.
    await p.locator(chip).first().focus();
    await p.keyboard.press('Shift+Tab');
    let tabs = 0;
    while (tabs < 30 && !(await p.locator(chip).first().evaluate(n => document.activeElement === n))) { await p.keyboard.press('Tab'); tabs++; }
    const focused = await p.locator(chip).first().evaluate(n => ({ active: document.activeElement === n, visible: n.matches(':focus-visible'), opacity: getComputedStyle(n).opacity, fading: n.getAnimations().filter(x => x instanceof CSSTransition).length }));
    assert.deepEqual(focused, { active: true, visible: true, opacity: '1', fading: 0 }, `Tab reaches the chip and it shows at once (${tabs} tabs)`);
    await p.evaluate(() => document.activeElement.blur());
    // Exactly one batched request for the page, covering every card; then silence (no polling while idle).
    await waitFor(() => mock.log.some(l => l.path === '/videos/status'), 'the status batch');
    await p.waitForTimeout(1500);
    const statuses = mock.log.filter(l => l.path === '/videos/status');
    const n = await p.locator('#ytc-list .tsc').count();
    // One batch per render: Home's preload appends its cards in chunks, and each chunk asks only for what is new.
    const asked = statuses.flatMap(s => s.body.video_ids);
    assert.ok(statuses.length <= Math.ceil(n / 24) + 1, `${statuses.length} requests for ${n} cards`);
    assert.equal(new Set(asked).size, asked.length, 'no video asked about twice');
    assert.ok(new Set(asked).size >= Math.min(n, 200), 'every card is covered');
    const before = mock.log.length;
    await p.waitForTimeout(6000);
    assert.equal(mock.log.length, before, 'no polling while idle');
    assert.equal(await worker(() => chrome.alarms.getAll().then(a => a.length)), 0, 'no alarm while idle');
    assert.equal(await p.locator('#ytc-tsc-toast').count(), 0);
    // A click on the card still opens the video.
    await p.locator(card).first().locator('a.t').click();
    await p.waitForFunction(v => location.search.includes(v), id);
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  await step('transcripts: setting it up while a page is open dresses that page without a reload; turning it off undresses it; a page restored from memory shows the latest state', async () => {
    mock.reset();
    await disable();
    const p = await home();
    await p.waitForTimeout(300);
    assert.equal(await p.locator('.tsc').count(), 0);
    await enable();
    await p.waitForSelector('#ytc-list .c[data-v] .tsc', { state: 'attached', timeout: 5000 });
    const cards = await p.locator('#ytc-list .c[data-v]').count();
    assert.equal(await p.locator('#ytc-list .c[data-v] .tsc').count(), cards, 'every card');
    await waitFor(() => mock.log.some(l => l.path === '/videos/status'), 'one status batch');
    const id = await p.locator('#ytc-list .c[data-v]').first().getAttribute('data-v');
    // Away and back: the job finished meanwhile; the restored page is not stale.
    await p.evaluate(() => { document.querySelector('a[href="/feed/history"]').click(); });
    await p.waitForFunction(() => location.pathname === '/feed/history');
    mock.state.jobs.push(mockHelper.job('job-z', id, 'completed'));
    await p.goBack();
    await p.waitForFunction(s => document.querySelector(s)?.dataset.s === 'ok', chipOf(id), { timeout: 8000 });
    await disable();
    await p.waitForFunction(() => !document.querySelector('.tsc'), null, { timeout: 5000 });
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  await step('transcripts cards: card layout is the same with the feature on and off (no shift)', async () => {
    const measure = async () => {
      const p = await home();
      await p.waitForTimeout(400);
      const r = await p.evaluate(() => [...document.querySelectorAll('#ytc-list .c[data-v]')].slice(0, 12).map(c => { const a = c.getBoundingClientRect(), b = c.querySelector('.th').getBoundingClientRect(); return [a.x, a.y, a.width, a.height, b.x, b.y, b.width, b.height].map(v => Math.round(v * 2) / 2); }));
      await p.close();
      return r;
    };
    await enable();
    const on = await measure();
    await disable();
    const off = await measure();
    assert.deepEqual(on, off);
  });

  await step('transcripts cards: click to queue, live stages with real progress across tabs, click to cancel, Saved reveals the file, Transcribed saves without a new request, Removed, retry after a failure', async () => {
    mock.reset();
    await enable();
    const a = await home(), b = await home();
    await a.waitForSelector('#ytc-list .tsc');
    await b.waitForSelector('#ytc-list .tsc');
    const id = await a.locator('#ytc-list .c[data-v]:has(.tsc)').first().getAttribute('data-v');
    const chip = chipOf(id);
    await a.locator(`#ytc-list .c[data-v="${id}"] a.th`).hover();
    const url = a.url();
    await a.click(chip);
    await a.waitForFunction(s => document.querySelector(s)?.dataset.s === 'on', chip);
    assert.equal(a.url(), url, 'the click did not open the video');
    assert.equal(mock.log.filter(l => l.path === '/jobs' && l.method === 'POST').length, 1);
    assert.equal(mock.log.find(l => l.path === '/jobs' && l.method === 'POST').body.url, `https://www.youtube.com/watch?v=${id}`);
    assert.match(await a.locator('#ytc-tsc-toast').textContent(), /queue/);
    assert.equal(await a.locator(chip).first().innerText(), 'Queued');
    // Live across tabs, by message, with a real percent.
    const j = mock.state.jobs[0];
    for (const [status, progress, text] of [['downloading', 3, 'Downloading'], ['preparing', 18, 'Preparing'], ['transcribing', 42, '42%']]) {
      j.status = status; j.progress = progress;
      for (const p of [a, b]) await p.waitForFunction(([s, t]) => document.querySelector(s)?.innerText === t, [chip, text], { timeout: 9000 });
    }
    assert.equal(await a.locator(chip).first().evaluate(n => n.style.getPropertyValue('--p')), '42%');
    assert.match(await a.locator(chip).first().getAttribute('title'), /Transcribing 42%.*cancel/);
    await a.mouse.move(2, 2);
    await a.waitForTimeout(200);
    assert.equal(await a.locator(chip).first().evaluate(n => getComputedStyle(n).opacity), '1', 'state shows without hover');
    await shot(a, 'card-active-1400');
    const c1 = await a.locator(chip).first().boundingBox();
    const th1 = await a.locator(`#ytc-list .c[data-v="${id}"] a.th`).boundingBox();
    assert.ok(Math.abs(th1.x + th1.width - (c1.x + c1.width) - 6) <= 1.5, 'a wider chip keeps its corner');
    // The toolbar: progress painted, a useful title; the alarm wakes the worker only now.
    assert.match(await worker(() => chrome.action.getTitle({})), /^Transcripts: 1 transcribing/);
    assert.ok((await worker(() => self.__tr.icons.at(-1))).includes('32'), 'the icon is painted from Rust');
    assert.equal(await worker(() => chrome.alarms.getAll().then(x => x.length)), 1, 'an alarm only while work is active');
    // Click an active video: cancel.
    await a.click(chip);
    await a.waitForFunction(s => !document.querySelector(s)?.dataset.s, chip, { timeout: 9000 });
    assert.equal(mock.log.filter(l => l.path === `/videos/${id}/cancel`).length, 1);
    await b.waitForFunction(s => !document.querySelector(s)?.dataset.s, chip, { timeout: 12000 });
    assert.match(await a.locator(chip).first().getAttribute('title'), /Cancelled.*again/);
    await waitFor(() => worker(() => chrome.alarms.getAll().then(x => x.length === 0)), 'the alarm stops when work ends', 12000);
    // Again, to the end: saved; a quiet check; one six-second success.
    await a.locator(`#ytc-list .c[data-v="${id}"] a.th`).hover();
    await a.click(chip);
    await a.waitForFunction(s => document.querySelector(s)?.dataset.s === 'on', chip);
    const j2 = mock.state.jobs.find(x => x.video_id === id && x.status === 'queued');
    assert.ok(j2, 'a cancelled video starts again');
    mock.state.jobs = [j2];
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => t.startsWith('Transcripts:'))), 'an active title');
    j2.status = 'completed'; j2.progress = 100; j2.document_available = true;
    await a.waitForFunction(s => document.querySelector(s)?.dataset.s === 'ok', chip, { timeout: 12000 });
    await a.mouse.move(2, 2);
    await a.waitForTimeout(200);
    assert.equal(await a.locator(chip).first().evaluate(n => getComputedStyle(n).opacity), '1');
    await shot(a, 'card-saved-1400');
    assert.match(await a.locator(chip).first().getAttribute('title'), /Transcript saved/);
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => t.includes('Transcripts saved'))), 'the six-second success', 9000);
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => t === 'Redline preferences')), 'the plain mark returns', 9000);
    assert.equal(await worker(() => self.__tr.icons.at(-1)), 'manifest');
    // Saved reveals the file, no Azure, no new job.
    const jobsBefore = mock.state.jobs.length;
    await a.click(chip);
    await waitFor(() => mock.state.reveal.includes(id), 'the file shown');
    assert.equal(mock.state.jobs.length, jobsBefore);
    await a.waitForFunction(s => document.querySelector(s)?.dataset.s === 'ok', chip);
    assert.match(await a.locator('#ytc-tsc-toast').textContent(), /shown/);
    // Transcribed (finished, not saved): the same click saves the existing text; no new job.
    j2.saved = false;
    await a.reload({ waitUntil: 'commit' });
    await a.waitForSelector(chip);
    await a.waitForFunction(s => /Transcribed/.test(document.querySelector(s)?.title || ''), chip);
    await a.click(chip);
    await a.waitForFunction(s => /Transcript saved/.test(document.querySelector(s)?.title || ''), chip);
    assert.equal(j2.saved, true);
    assert.equal(mock.state.jobs.length, jobsBefore, 'no new transcription');
    // Removed: stays excluded; a click tells why and does nothing else.
    j2.status = 'removed'; j2.document_available = false;
    await a.reload({ waitUntil: 'commit' });
    await a.waitForFunction(s => document.querySelector(s)?.dataset.s === 'rm', chip);
    await a.mouse.move(2, 2);
    await shot(a, 'card-removed-1400');
    const posts = mock.log.filter(l => l.path === '/jobs' && l.method === 'POST').length;
    await a.click(chip);
    await a.waitForFunction(() => /Removed from your save folder/.test(document.querySelector('#ytc-tsc-toast')?.textContent || ''));
    assert.equal(mock.log.filter(l => l.path === '/jobs' && l.method === 'POST').length, posts);
    // A start the helper refuses: the reason, and a retry control; no automatic resubmission.
    mock.state.jobs = [];
    mock.state.failNextStart = true;
    await a.reload({ waitUntil: 'commit' });
    await a.waitForSelector(chip);
    await a.waitForFunction(s => !document.querySelector(s)?.dataset.s, chip);
    await a.locator(`#ytc-list .c[data-v="${id}"] a.th`).hover();
    const n = mock.log.filter(l => l.path === '/jobs' && l.method === 'POST').length;
    await a.click(chip);
    await a.waitForFunction(s => document.querySelector(s)?.dataset.s === 'er', chip);
    assert.match(await a.locator('#ytc-tsc-toast').textContent(), /Azure is not configured/);
    assert.equal(mock.log.filter(l => l.path === '/jobs' && l.method === 'POST').length, n + 1, 'never resubmitted by itself');
    assert.deepEqual(a.errors, []);
    await a.close(); await b.close();
  });

  await step('transcripts watch page: a Transcript button in the action row like Share; it shows state and opens the reader when saved; related and playlist rows have chips; fullscreen shows none', async () => {
    mock.reset();
    await enable();
    const h = await home();
    await h.waitForSelector('#ytc-list .tsc');
    const id = await h.locator('#ytc-list .c[data-v]:has(.tsc)').first().getAttribute('data-v');
    await h.close();
    const p = await open(`https://www.youtube.com/watch?v=${id}`);
    await p.waitForSelector('.tsc-w');
    assert.equal(await p.locator('#ytc-acts .tsc-w').innerText(), 'Transcript');
    const share = await p.locator('#ytc-share-b').boundingBox(), t = await p.locator('.tsc-w').boundingBox();
    assert.equal(t.height, share.height, 'the same height as Share');
    assert.ok(Math.abs(t.y - share.y) <= 1, 'on Share\'s row');
    const css = n => p.locator(n).evaluate(e => { const s = getComputedStyle(e); return [s.backgroundColor, s.borderRadius, s.borderWidth, s.fontSize, s.fontWeight].join(); });
    assert.equal(await css('.tsc-w'), await css('#ytc-share-b'), 'no special border, same style');
    await p.waitForSelector('#ytc-list .c .tsc');
    await p.locator('#ytc-list .c[data-v]:has(.tsc)').first().locator('a.th').hover();
    await p.waitForTimeout(200);
    await shot(p, 'watch-1400');
    // Related cards: chip inside the small thumbnail.
    const rel = await p.locator('#ytc-list .c[data-v]:has(.tsc)').first().locator('.tsc').boundingBox();
    const relTh = await p.locator('#ytc-list .c[data-v]:has(.tsc)').first().locator('a.th').boundingBox();
    assert.ok(rel.x >= relTh.x && rel.x + rel.width <= relTh.x + relTh.width + 0.5 && rel.y >= relTh.y, 'inside its thumbnail');
    // Queue from the button; it follows the stages in words.
    await p.click('.tsc-w');
    await p.waitForFunction(() => document.querySelector('.tsc-w')?.innerText === 'Queued');
    const j = mock.state.jobs[0];
    j.status = 'transcribing'; j.progress = 55;
    await p.waitForFunction(() => document.querySelector('.tsc-w')?.innerText === 'Transcribing 55%', null, { timeout: 9000 });
    await shot(p, 'watch-active-1400');
    // Saved: it opens the reader for this video.
    j.status = 'completed'; j.progress = 100; j.document_available = true;
    await p.waitForFunction(() => document.querySelector('.tsc-w')?.innerText === 'Transcript saved', null, { timeout: 12000 });
    await shot(p, 'watch-saved-1400');
    const opened = ctx.waitForEvent('page');
    await p.click('.tsc-w');
    const r = await opened;
    await r.waitForSelector('#transcript p');
    assert.match(r.url(), new RegExp(`reader\\.html\\?id=${j.id}$`));
    await r.close();
    // Fullscreen shows only the player: no control can sit over it.
    await p.bringToFront();
    const fs_ = await p.evaluate(async () => { try { await document.querySelector('#ytc-player').requestFullscreen(); } catch { return null; } return !!document.fullscreenElement; });
    if (fs_) {
      assert.equal(await p.evaluate(() => document.fullscreenElement.querySelectorAll('.tsc,.tsc-w').length), 0);
      await p.evaluate(() => document.exitFullscreen());
    }
    // A playlist's rows.
    await p.goto(`https://www.youtube.com/watch?v=${id}&list=PLabc`, { waitUntil: 'commit' });
    await p.waitForSelector('#ytc-pl .pli');
    const rows = await p.locator('#ytc-pl .pli').count();
    assert.equal(await p.locator('#ytc-pl .pli .tsc').count(), rows, 'every playlist row');
    await p.locator('#ytc-pl .pli').nth(1).hover();
    await p.waitForTimeout(200);
    const pt = await p.locator('#ytc-pl .pli').nth(1).locator('.plth').boundingBox(), pc = await p.locator('#ytc-pl .pli').nth(1).locator('.tsc').boundingBox();
    assert.ok(pc.x >= pt.x && pc.x + pc.width <= pt.x + pt.width + 0.5, 'inside its thumbnail');
    const before = p.url();
    await p.locator('#ytc-pl .pli').nth(1).locator('.tsc').click();
    await waitFor(() => mock.state.jobs.length === 2, 'a playlist row queues its own video');
    assert.equal(p.url(), before, 'the row did not navigate');
    await shot(p, 'playlist-1400');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  await step('transcripts 480 wide: chips, the watch button and the toast fit without horizontal scroll', async () => {
    mock.reset();
    await enable();
    const p = await home(480, 900);
    await p.waitForSelector('#ytc-list .tsc');
    const id = await p.locator('#ytc-list .c[data-v]:has(.tsc)').first().getAttribute('data-v');
    await p.locator(`#ytc-list .c[data-v="${id}"] a.th`).hover();
    await p.waitForTimeout(200);
    await shot(p, 'card-hover-480');
    await p.click(chipOf(id));
    await p.waitForFunction(s => document.querySelector(s)?.dataset.s === 'on', chipOf(id));
    mock.state.jobs[0].status = 'transcribing'; mock.state.jobs[0].progress = 42;
    await p.waitForFunction(s => document.querySelector(s)?.innerText === '42%', chipOf(id), { timeout: 9000 });
    await shot(p, 'card-active-480');
    assert.ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'no sideways scroll');
    await p.goto(`https://www.youtube.com/watch?v=${id}`, { waitUntil: 'commit' });
    await p.waitForSelector('.tsc-w');
    await p.waitForTimeout(300);
    await shot(p, 'watch-480');
    const wide = await p.evaluate(() => ({ over: document.documentElement.scrollWidth - innerWidth, who: [...document.querySelectorAll('#ytc *')].filter(n => n.getBoundingClientRect().right > innerWidth + 1 && getComputedStyle(n).position !== 'fixed').slice(0, 6).map(n => n.tagName + '#' + n.id + '.' + n.className) }));
    assert.ok(wide.over <= 0 || !wide.who.some(w => /tsc/.test(w)), 'no sideways scroll caused by the transcript controls: ' + JSON.stringify(wide));
    await p.close();
  });

  await step('transcripts: the helper stopping mid-session is quiet (no toasts, no errors on cards), and recovers', async () => {
    mock.reset();
    await enable();
    const j = mockHelper.job('job-live', 'zzzzzzzzzzz', 'transcribing', { progress: 30 });
    mock.state.jobs.push(j);
    const p = await home();
    await p.waitForSelector('#ytc-list .tsc');
    const id = await p.locator('#ytc-list .c[data-v]:has(.tsc)').first().getAttribute('data-v');
    j.video_id = id;
    await p.reload({ waitUntil: 'commit' });
    await p.waitForFunction(s => document.querySelector(s)?.dataset.s === 'on', chipOf(id), { timeout: 12000 });
    await p.waitForFunction(s => document.querySelector(s)?.innerText === '30%', chipOf(id), { timeout: 12000 });
    mock.down(true);
    await p.waitForFunction(s => document.querySelector(s)?.dataset.s === 'un', chipOf(id), { timeout: 45000 });
    assert.match(await p.locator(chipOf(id)).first().getAttribute('title'), /unavailable/);
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => /Helper unavailable/.test(t))), 'the toolbar says so');
    assert.equal(await p.locator('#ytc-tsc-toast').count(), 0, 'no message by itself');
    const n0 = await native();
    await p.reload({ waitUntil: 'commit' });
    await p.waitForSelector('#ytc-list .tsc');
    await p.waitForTimeout(1500);
    assert.equal(await p.locator('#ytc-tsc-toast').count(), 0, 'a page load stays quiet');
    assert.ok((await native()) - n0 <= 1, 'a page load asks the native host at most once in a while');
    assert.deepEqual(p.errors, []);
    await shot(p, 'helper-stopped-1400');
    mock.down(false);
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => t.startsWith('Transcripts: 1 transcribing'))), 'recovers by itself', 45000);
    await p.waitForFunction(s => document.querySelector(s)?.dataset.s === 'on', chipOf(id), { timeout: 15000 });
    j.status = 'completed'; j.document_available = true; j.progress = 100;
    await p.waitForFunction(s => document.querySelector(s)?.dataset.s === 'ok', chipOf(id), { timeout: 15000 });
    await p.close();
  });

  await step('transcripts: the pairing token never reaches a YouTube page: not its storage, DOM, replies, incidents or debug overlay', async () => {
    mock.reset();
    await enable();
    assert.equal(await worker(() => TR.vault_get('pairingToken')), TOKEN);
    const before = incidents.length;
    const p = await home();
    await p.waitForSelector('#ytc-list .tsc');
    const seen = await p.evaluate(async () => {
      const out = [document.documentElement.outerHTML, JSON.stringify({ ...localStorage }), JSON.stringify({ ...sessionStorage }), String(document.cookie)];
      const rq = indexedDB.databases ? await indexedDB.databases() : [];
      out.push(JSON.stringify(rq));
      return out.join('\n');
    });
    assert.ok(!seen.includes(TOKEN), 'not in the page');
    assert.ok(!seen.includes('ytc-transcripts'), 'the vault is not this origin\'s');
    // The isolated world (a content script can read chrome.storage.local) has nothing either: checked from the worker,
    // whose view of that area is the same.
    const area = JSON.stringify(await worker(() => chrome.storage.local.get(null)));
    assert.ok(!area.includes(TOKEN) && !area.includes('pairingToken'), 'not in chrome.storage.local');
    const session = JSON.stringify(await worker(() => chrome.storage.session.get(null)));
    assert.ok(!session.includes(TOKEN), 'not in chrome.storage.session');
    // A status reply carries only status and saved, never paths or titles.
    const id = await p.locator('#ytc-list .c[data-v]:has(.tsc)').first().getAttribute('data-v');
    mock.state.jobs.push(mockHelper.job('job-p', id, 'completed'));
    const reply = await worker(v => wasm_bindgen.on_message(JSON.stringify({ action: 'getVideoStatuses', videoIds: [v] }), JSON.stringify({ id: chrome.runtime.id, url: 'https://www.youtube.com/', tab: { id: 1 }, frameId: 0 })), id);
    assert.ok(!reply.includes('must-not-reach') && !reply.includes(TOKEN), reply);
    // What a YouTube tab may ask is a short list.
    for (const action of ['getAzureSettings', 'saveAzureSettings', 'getSettings', 'saveSettings', 'forgetSettings', 'connectHelper', 'testAzureConnection', 'listJobs', 'getDocument', 'cancelQueue']) {
      const r = JSON.parse(await worker(a => wasm_bindgen.on_message(JSON.stringify({ action: a }), JSON.stringify({ id: chrome.runtime.id, url: 'https://www.youtube.com/', tab: { id: 1 }, frameId: 0 })), action));
      assert.equal(r.ok, false, action);
      assert.equal(r.error, 'This request is not allowed.', action);
    }
    for (const url of ['https://evil.example/', 'http://www.youtube.com/', 'https://www.youtube.com.evil.example/']) {
      const r = JSON.parse(await worker(u => wasm_bindgen.on_message(JSON.stringify({ action: 'getVideoStatuses', videoIds: [] }), JSON.stringify({ id: chrome.runtime.id, url: u, tab: { id: 1 }, frameId: 0 })), url));
      assert.equal(r.ok, false, url);
    }
    await p.keyboard.press('Backquote');
    const overlay = await p.locator('#ytc-debug').textContent().catch(() => '');
    assert.ok(!overlay.includes(TOKEN), 'not in the debug overlay');
    assert.ok(!JSON.stringify(incidents.slice(before)).includes(TOKEN), 'not in an incident');
    await p.close();
  });

  await step('transcripts: a token an older install left in chrome.storage.local moves into the vault', async () => {
    await worker(() => TR.vault_del('pairingToken'));
    await worker(t => chrome.storage.local.set({ pairingToken: t }), TOKEN);
    await worker(() => trAdopt());
    await waitFor(async () => (await worker(() => TR.vault_get('pairingToken'))) === TOKEN, 'adopted');
    assert.equal(await worker(() => chrome.storage.local.get('pairingToken').then(o => o.pairingToken ?? null)), null, 'gone from content scripts\' reach');
    const src = fs.readFileSync(path.join(__dirname, '..', 'extension', 'background.js'), 'utf8');
    assert.ok(src.includes("setAccessLevel({ accessLevel: 'TRUSTED_AND_UNTRUSTED_CONTEXTS' })"), 'storage stays readable for content scripts');
  });

  await step('transcripts settings: Import all settings brings an exported file back, without a pairing token or the switch; a wrong file changes nothing; values never pass through Wasm', async () => {
    mock.reset();
    await disable();
    const seed = { ytcConfigV1: { p: { sbo: false } }, ytcVoiceKeyV1: 'e2e-import-deepgram', yourTubeHistoryV1: { version: 1, entries: [['q', 1]] }, shortsInFeeds: true, yourTubeShowMembersV1: false };
    const keep = await worker(() => chrome.storage.local.get(null));
    await worker(s => chrome.storage.local.set(s), seed);
    // Even if these sat in storage, the export leaves them out.
    await worker(t => chrome.storage.local.set({ pairingToken: t, ytcTranscriptsV1: { on: true } }), TOKEN);
    const s = await open(ext('settings.html'));
    const tabbed = ctx.waitForEvent('page');
    await s.click('#export-all');
    const tab = await tabbed;
    await tab.waitForSelector('#popup-export-save');
    await shot(tab, 'export-page-1400');
    const saving = tab.waitForEvent('download');
    await tab.click('#popup-export-save');
    const file = await saving, filePath = await file.path();
    const saved = JSON.parse(fs.readFileSync(filePath, 'utf8'));
    await tab.close();
    assert.equal(saved.items.ytcVoiceKeyV1, 'e2e-import-deepgram');
    assert.ok(!('pairingToken' in saved.items) && !('ytcTranscriptsV1' in saved.items), 'never in the file');
    assert.ok(!fs.readFileSync(filePath, 'utf8').includes(TOKEN));
    // A fresh install: nothing there. Import.
    await worker(() => chrome.storage.local.clear());
    await s.setInputFiles('#import-file', filePath);
    await s.waitForFunction(() => /Imported \d+ settings/.test(document.querySelector('#import-notice').textContent));
    assert.match(await s.locator('#import-notice').textContent(), /Reload your YouTube tabs/);
    const after = await worker(() => chrome.storage.local.get(null));
    for (const k of Object.keys(seed)) assert.deepEqual(after[k], seed[k], k);
    assert.ok(!('pairingToken' in after) && !('ytcTranscriptsV1' in after));
    // A console dump works too; what is not ours stays out, even with a token in it.
    const dump = path.join(os.tmpdir(), `ytc-dump-${Date.now()}.json`);
    fs.writeFileSync(dump, JSON.stringify({ shortsInFeeds: false, pairingToken: TOKEN, ytcTranscriptsV1: { on: true }, ytcCollectorV1: 'https://evil.example/', other: 1 }));
    await s.setInputFiles('#import-file', dump);
    await s.waitForFunction(() => /Imported 1 setting\. 4 other entries left out/.test(document.querySelector('#import-notice').textContent));
    const after2 = await worker(() => chrome.storage.local.get(null));
    assert.equal(after2.shortsInFeeds, false);
    assert.ok(!('pairingToken' in after2) && !('ytcTranscriptsV1' in after2) && !('ytcCollectorV1' in after2) && !('other' in after2));
    // Not settings at all: nothing changes.
    for (const [text, why] of [['not json', /not a Redline settings file/], ['[1,2]', /not a Redline settings file/], ['{"other":1}', /holds no Redline settings/]]) {
      const wrong = `${dump}.${Buffer.from(text).toString('hex').slice(0, 8)}.json`;
      fs.writeFileSync(wrong, text);
      await s.setInputFiles('#import-file', wrong);
      await s.waitForFunction(w => new RegExp(w).test(document.querySelector('#import-notice').textContent), why.source, { timeout: 8000 }).catch(async e => { throw new Error(`${text}: ${await s.locator('#import-notice').textContent()} | ${s.errors} | ${e.message}`); });
    }
    assert.deepEqual(await worker(() => chrome.storage.local.get(null)), after2);
    await s.bringToFront();
    await s.locator('#settings-file').scrollIntoViewIfNeeded();
    await shot(s, 'settings-file-1400');
    for (const f of fs.readdirSync(os.tmpdir())) if (f.startsWith(path.basename(dump))) fs.rmSync(path.join(os.tmpdir(), f), { force: true });
    assert.deepEqual(s.errors, []);
    await s.close();
    await worker(k => chrome.storage.local.clear().then(() => chrome.storage.local.set(k)), keep);
  });

  // ---- the reviewer's adversarial cases, kept.
  const stopWorkers = async () => {
    const pg = await ctx.newPage();
    // (A page that asks the worker nothing: anything else would wake it again right away.)
    await pg.goto(ext('popup.html?file=export'), { waitUntil: 'commit' }).catch(() => {});
    const cdp = await ctx.newCDPSession(pg);
    await cdp.send('ServiceWorker.enable');
    await cdp.send('ServiceWorker.stopAllWorkers');
    await new Promise(r => setTimeout(r, 400));
    await pg.close();
  };
  const paid = () => mock.log.filter(l => (l.path === '/jobs' && l.method === 'POST') || l.path === '/channels/import').length;

  await step('transcripts: a page script cannot spend money: planted buttons, real chips, a synthetic double click or key press send nothing; a real click and real Enter still work', async () => {
    mock.reset(); await enable();
    const p = await home();
    await p.waitForSelector('#ytc-list .tsc', { state: 'attached' });
    const id = await p.locator('#ytc-list .c[data-v]:has(.tsc)').first().getAttribute('data-v');
    const planted = await p.evaluate(v => {
      let trusted = null;
      addEventListener('click', e => { trusted = e.isTrusted; }, { capture: true, once: true });
      const b = document.createElement('button'); b.dataset.act = 'tsc'; b.dataset.arg = 'PLANTEDvid1'; document.body.append(b); b.click(); b.remove();
      // The real chip of a real card, clicked by a script.
      document.querySelector(`.tsc[data-arg="${v}"]`).click();
      document.querySelector(`.tsc[data-arg="${v}"]`).dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, detail: 1, clientX: 5, clientY: 5 }));
      // Through the other route into on_click, and a key that claims to activate a focused chip.
      const d = document.createElement('div'); d.dataset.dbl = 'tsc'; document.body.append(d); d.dispatchEvent(new MouseEvent('dblclick', { bubbles: true })); d.remove();
      const chip = document.querySelector(`.tsc[data-arg="${v}"]`); chip.focus();
      for (const type of ['keydown', 'keypress', 'keyup']) chip.dispatchEvent(new KeyboardEvent(type, { key: 'Enter', code: 'Enter', bubbles: true, cancelable: true }));
      chip.blur();
      return trusted;
    }, id);
    assert.equal(planted, false, 'the browser itself says it is not trusted');
    await p.goto('https://www.youtube.com/@monkeydeveloper/videos', { waitUntil: 'commit' });
    await p.waitForSelector('.tsc-ch');
    await p.evaluate(() => {
      for (const [act, arg] of [['tsc-import', 'all'], ['tsc-import', 'year'], ['tsc-chretry', ''], ['tsc-chcancel', ''], ['tsc-shorts', '']]) { const b = document.createElement('button'); b.dataset.act = act; b.dataset.arg = arg; document.body.append(b); b.click(); b.remove(); }
      document.querySelector('[data-act="tsc-import"]').click();
      const d = document.createElement('div'); d.dataset.dbl = 'tsc-chretry'; document.body.append(d); d.dispatchEvent(new MouseEvent('dblclick', { bubbles: true })); d.remove();
    });
    await p.waitForTimeout(900);
    assert.equal(paid(), 0, 'nothing reached the helper: ' + JSON.stringify(mock.log.filter(l => l.method === 'POST' && l.path !== '/videos/status')));
    assert.equal(await p.locator('#ytc-tsc-shorts').getAttribute('aria-checked'), 'false');
    // A person can still do all of it.
    await p.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await p.waitForSelector(chipOf(id), { state: 'attached' });
    await p.locator(`#ytc-list .c[data-v="${id}"] a.th`).hover();
    await p.focus(chipOf(id));
    await p.keyboard.press('Enter');
    await waitFor(() => paid() === 1, 'a real Enter is trusted');
    assert.equal(mock.log.find(l => l.path === '/jobs' && l.method === 'POST').body.url, `https://www.youtube.com/watch?v=${id}`);
    await p.close();
  });

  await step('transcripts: someone who never set it up: a failed Pair automatically leaves the toolbar alone', async () => { try {
    mock.reset(); await disable();
    await worker(() => chrome.alarms.clearAll());
    await worker(() => { self.__tr.nativeMode = 'missing'; });
    await worker(() => TR.vault_del('pairingToken'));
    await worker(() => Promise.all([chrome.action.setTitle({ title: 'Redline preferences' }), chrome.action.setIcon({ path: chrome.runtime.getManifest().action.default_icon })]));
    await worker(() => { self.__tr.icons.length = 0; });
    const p = await open(ext('settings.html'));
    await p.waitForFunction(() => document.querySelector('#helper-status')?.textContent === 'Helper unavailable');
    assert.equal(await p.locator('#switch').isVisible(), false, 'no switch for someone with nothing to turn off');
    await p.evaluate(() => { document.querySelector('#pairing').open = true; });
    await p.click('#auto-pair');
    await p.waitForFunction(() => /cannot find/.test(document.querySelector('#pair-notice')?.textContent || ''));
    await p.waitForTimeout(800);
    assert.equal(await worker(() => chrome.action.getTitle({})), 'Redline preferences');
    assert.deepEqual(await worker(() => self.__tr.icons), [], 'nothing painted');
    assert.equal(await worker(() => chrome.alarms.getAll().then(a => a.length)), 0);
    await p.close();
  } finally { await worker(() => { self.__tr.nativeMode = 'ok'; }); } });

  await step('transcripts: a helper removed for good clears the flag; a stopped one keeps it; Turn off holds until turned on again', async () => { try {
    const flag = () => worker(() => chrome.storage.local.get('ytcTranscriptsV1').then(o => o.ytcTranscriptsV1 ?? null));
    mock.reset(); await enable();
    await worker(t => TR.vault_set('pairingToken', t), TOKEN);
    // A stop: the port is down but the browser still finds the helper. The feature stays; its chips stay.
    mock.down(true);
    let s = await open(ext('settings.html'));
    await s.waitForFunction(() => document.querySelector('#helper-status')?.textContent === 'Helper unavailable', null, { timeout: 25000 });
    await s.waitForTimeout(500);
    assert.deepEqual(await flag(), { on: true }, 'a transient stop keeps it');
    await s.close();
    // Removed: the port is down and the registration is gone. The chips leave an open page without a reload.
    const p = await home();
    await p.waitForSelector('#ytc-list .tsc', { state: 'attached', timeout: 8000 });
    await worker(() => { self.__tr.nativeMode = 'missing'; });
    s = await open(ext('settings.html'));
    await s.waitForFunction(() => document.querySelector('#helper-status')?.textContent === 'Helper unavailable', null, { timeout: 25000 });
    await waitFor(async () => (await flag()) === null, 'the flag is cleared', 8000);
    await p.waitForFunction(() => !document.querySelector('.tsc'), null, { timeout: 8000 });
    assert.equal(await worker(() => chrome.action.getTitle({})), 'Redline preferences');
    await s.close();
    // It comes back by itself the next time settings finds a configured helper.
    mock.down(false);
    await worker(() => { self.__tr.nativeMode = 'ok'; });
    s = await open(ext('settings.html'));
    await waitFor(async () => (await flag())?.on === true, 'on again', 10000);
    await p.waitForSelector('#ytc-list .tsc', { state: 'attached', timeout: 8000 });
    // The switch, for a helper that is fine: off everywhere at once, no request after, and settings does not undo it.
    await s.waitForSelector('#turn-off:not([hidden])');
    await s.locator('#switch').scrollIntoViewIfNeeded();
    await shot(s, 'settings-switch-1400');
    await s.click('#turn-off');
    await p.waitForFunction(() => !document.querySelector('.tsc'), null, { timeout: 8000 });
    assert.equal(await flag(), null);
    await s.waitForSelector('#turn-on:not([hidden])');
    await s.reload({ waitUntil: 'commit' });
    await s.waitForFunction(() => document.querySelector('#helper-status')?.textContent === 'Helper connected', null, { timeout: 10000 });
    assert.equal(await flag(), null, 'reopening settings does not turn it back on');
    await p.waitForTimeout(300);
    const before = mock.log.length;
    await p.reload({ waitUntil: 'commit' });
    await p.waitForSelector('#ytc-list .c[data-v] .t');
    await p.waitForTimeout(600);
    assert.equal(mock.log.length, before, 'no request from YouTube while off');
    await s.waitForSelector('#turn-on:not([hidden])');
    await s.click('#turn-on');
    await waitFor(async () => (await flag())?.on === true, 'on again by asking', 8000);
    await p.waitForSelector('#ytc-list .tsc', { state: 'attached', timeout: 8000 });
    await s.close(); await p.close();
  } finally { mock.down(false); await worker(() => { self.__tr.nativeMode = 'ok'; }); } });

  await step('transcripts: one Set up click with an installed helper completes pairing: an open YouTube tab gets its controls, the popup its section; YouTube alone stays quiet', async () => {
    await disable();
    await worker(() => TR.vault_del('pairingToken'));
    await new Promise(r => setTimeout(r, 300));
    mock.reset();
    mock.state.configured = true; mock.state.hasKey = true; mock.state.endpoint = 'https://resource.cognitiveservices.azure.com';
    const n0 = await native();
    const yt = await home();
    await yt.waitForTimeout(600);
    assert.equal(await yt.locator('.tsc').count(), 0);
    assert.equal(await native(), n0, 'opening YouTube stays quiet');
    assert.equal(mock.log.length, 0);
    const pop = await open(ext('popup.html'), 420, 600);
    await pop.waitForSelector('#popup-tr-setup');
    assert.equal(await native(), n0, 'opening the popup stays quiet');
    const opened = ctx.waitForEvent('page');
    await pop.click('#popup-tr-setup');
    const s = await opened;
    await s.waitForFunction(() => document.querySelector('#helper-status')?.textContent === 'Helper connected', null, { timeout: 10000 });
    await yt.waitForSelector('#ytc-list .tsc', { state: 'attached', timeout: 8000 });
    assert.equal(await worker(() => TR.vault_get('pairingToken')), TOKEN);
    await s.close(); await pop.close();
    const again = await open(ext('popup.html'), 420, 600);
    await again.waitForSelector('#popup-tr .ptr-card');
    await again.close(); await yt.close();
  });

  await step('transcripts: the worker sleeping keeps the native rule (at most once in 5 minutes across wakes); a grey icon recovers; a batch that finished with the browser closed never replays its success', async () => { try {
    mock.reset(); await enable();
    await worker(() => TR.vault_del('lastReconnect'));
    await worker(() => TR.vault_del('pairingToken'));
    mock.down(true);
    let total = 0;
    for (let i = 0; i < 3; i++) {
      await stopWorkers();
      const p = await home();
      await p.waitForSelector('#ytc-list .tsc', { state: 'attached', timeout: 8000 });
      await p.waitForTimeout(1500);
      total += await worker(() => self.__tr.native.length);
      await p.close();
    }
    assert.ok(total <= 1, `native attempts across three page loads and worker restarts: ${total}`);
    // Grey at start; the helper returns; the worker sleeps; the next page corrects it, with no click.
    await worker(t => TR.vault_set('pairingToken', t), TOKEN);
    await worker(() => tr().then(c => c.on_startup()));
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => /unavailable/.test(t))), 'grey', 12000);
    await stopWorkers();
    mock.down(false);
    const q = await home();
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => t === 'Redline preferences')), 'the toolbar recovers', 15000);
    await q.close();
    // Work seen active, the browser closes (the session marker goes with it), it finishes meanwhile.
    const j = mockHelper.job('job-closed', 'closedjob01', 'transcribing', { progress: 50 });
    mock.state.jobs = [j];
    await worker(() => tr().then(c => c.on_startup()));
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => t.startsWith('Transcripts: 1 transcribing'))), 'active', 12000);
    await worker(() => chrome.storage.session.remove('ytc:tr-session'));
    await stopWorkers();
    j.status = 'completed'; j.progress = 100; j.document_available = true;
    // A page wakes the new worker, whose first read finds the batch finished.
    const woke = await home();
    const titles = [];
    for (let i = 0; i < 20; i++) { titles.push(await worker(() => chrome.action.getTitle({}))); await new Promise(r => setTimeout(r, 150)); }
    await woke.close();
    assert.ok(!titles.some(t => /Transcripts saved/.test(t)), 'no success replay: ' + [...new Set(titles)]);
    assert.equal(titles.at(-1), 'Redline preferences', 'the read did happen, and found it finished');
    // A sleep inside a session keeps what it saw: a real success still shows.
    mock.state.jobs = [mockHelper.job('job-seen', 'seenjob0001', 'transcribing', { progress: 50 })];
    mock.state.batch = 'batch-2';
    await worker(() => tr().then(c => c.on_startup()));
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => t.startsWith('Transcripts: 1 transcribing'))), 'active again', 12000);
    await stopWorkers();
    Object.assign(mock.state.jobs[0], { status: 'completed', progress: 100, document_available: true });
    const woke2 = await home();
    await waitFor(() => worker(() => chrome.action.getTitle({}).then(t => t.includes('Transcripts saved'))), 'a seen success still shows after a sleep', 12000);
    await woke2.close();
  } finally { mock.down(false); } });

  await step('transcripts: a leftover alarm from an older install is cleared', async () => {
    await worker(() => chrome.alarms.create('full-transcript-status', { periodInMinutes: 1 }));
    await worker(() => trTidy());
    assert.equal(await worker(() => chrome.alarms.get('full-transcript-status').then(a => a ?? null)), null);
  });

  await step('transcripts settings: importing asks before replacing a Deepgram key or visitor id and names them; junk in a file does not break the client', async () => {
    mock.reset(); await disable();
    const keep = await worker(() => chrome.storage.local.get(null));
    await worker(() => chrome.storage.local.remove('shortsInFeeds'));
    await worker(() => chrome.storage.local.set({ ytcVoiceKeyV1: 'mine-deepgram', ytcConfigV1: { vd: 'MyVisitorData', v: '2.1', p: { sbo: true } } }));
    const file = path.join(os.tmpdir(), `ytc-crafted-${Date.now()}.json`);
    const write = (o, n) => { const f = `${file}.${n}.json`; fs.writeFileSync(f, JSON.stringify(o)); return f; };
    const crafted = { redlineSettings: 1, items: { ytcVoiceKeyV1: 'attacker-deepgram', ytcConfigV1: { vd: 'AttackerVisitor', v: '2.2', p: { sbo: false } }, shortsInFeeds: true } };
    const s = await open(ext('settings.html'));
    const stored = () => worker(() => chrome.storage.local.get(['ytcVoiceKeyV1', 'ytcConfigV1', 'shortsInFeeds']));
    await s.setInputFiles('#import-file', write(crafted, 1));
    await s.waitForSelector('#import-confirm:not([hidden])');
    assert.match(await s.locator('#import-notice').textContent(), /replace your Deepgram key and your YouTube visitor id/);
    assert.equal((await stored()).ytcVoiceKeyV1, 'mine-deepgram', 'nothing is written while it asks');
    await s.locator('#settings-file').scrollIntoViewIfNeeded();
    await shot(s, 'settings-import-confirm-1400');
    await s.click('#import-cancel');
    await s.waitForFunction(() => document.querySelector('#import-notice').textContent === 'Nothing changed.');
    assert.equal((await stored()).shortsInFeeds, undefined);
    // Import the rest: everything but what says who you are.
    await s.setInputFiles('#import-file', write(crafted, 2));
    await s.waitForSelector('#import-confirm:not([hidden])');
    await s.click('#import-rest');
    await s.waitForFunction(() => /Your Deepgram key and YouTube visitor id stayed as it was/.test(document.querySelector('#import-notice').textContent));
    let now = await stored();
    assert.equal(now.ytcVoiceKeyV1, 'mine-deepgram');
    assert.equal(now.ytcConfigV1.vd, 'MyVisitorData');
    assert.equal(now.ytcConfigV1.v, '2.2', 'the rest of that record came');
    assert.equal(now.shortsInFeeds, true);
    // Replace, on purpose.
    await s.setInputFiles('#import-file', write(crafted, 3));
    await s.waitForSelector('#import-confirm:not([hidden])');
    await s.click('#import-replace');
    await s.waitForFunction(() => /including a Deepgram key and a YouTube visitor id/.test(document.querySelector('#import-notice').textContent));
    now = await stored();
    assert.equal(now.ytcVoiceKeyV1, 'attacker-deepgram');
    // Nothing to replace: no question, but the notice names what came.
    await worker(() => chrome.storage.local.remove(['ytcVoiceKeyV1', 'ytcConfigV1']));
    await s.setInputFiles('#import-file', write(crafted, 4));
    await s.waitForFunction(() => /Imported 3 settings, including a Deepgram key and a YouTube visitor id/.test(document.querySelector('#import-notice').textContent));
    // Junk in importable keys: Home still renders, nothing thrown.
    await worker(() => chrome.storage.local.clear());
    await s.setInputFiles('#import-file', write({ redlineSettings: 1, items: { ytcConfigV1: { p: 'x', v: 12, visitor: '<img src=x onerror=alert(1)>', vd: '<img src=x>', au: -5, sts: 'x' }, ytcVoiceKeyV1: '<b>', yourTubeHistoryV1: { entries: 'x' }, yourTubeProgressV1: [1, 2], yourTubeBlockedWebsitesV1: {}, yourTubeRecommendationTopicsV1: [{}], yourTubeNativeTopicsV1: [null], yourTubeMinimumHomeDurationV1: 'x', yourTubeHomeOrderV1: '<i>', shortsInFeeds: true } }, 5));
    await s.waitForFunction(() => /Imported/.test(document.querySelector('#import-notice').textContent));
    await s.close();
    const p = await open('https://www.youtube.com/');
    await p.waitForSelector('#ytc-list .c[data-v] .t', { timeout: 15000 });
    assert.deepEqual(p.errors, []);
    await p.close();
    await worker(k => chrome.storage.local.clear().then(() => chrome.storage.local.set(k)), keep);
    for (const f of fs.readdirSync(os.tmpdir())) if (f.startsWith(path.basename(file))) fs.rmSync(path.join(os.tmpdir(), f), { force: true });
  });

  await step('transcripts reader: Clear unchecks with no poll in between; a job queued elsewhere shows at once; the installed helper\'s old wording is replaced; a spoken "Transcript:" stays in the text; the source header shows', async () => {
    mock.reset(); await enable();
    mock.state.jobs.push(
      mockHelper.job('job-a', 'aaaaaaaaaaa', 'completed', { title: 'One', text: 'First words.\nTranscript:\nStill speaking.', created_at: '2026-11-01T10:00:00Z' }),
      mockHelper.job('job-b', 'bbbbbbbbbbb', 'completed', { title: 'Two', created_at: '2026-11-01T09:00:00Z' }),
      mockHelper.job('job-c', 'ccccccccccc', 'failed', { title: 'Three', error: 'The transcript result is retained, but its local job status could not be saved. Scribe is retrying that status write; new transcriptions wait.', created_at: '2026-11-01T08:00:00Z' }));
    const p = await open(ext('reader.html'));
    await p.waitForSelector('.tx-job');
    await p.check('#sel-job-a');
    await p.waitForFunction(() => /ready to copy/.test(document.querySelector('#selection-count').textContent));
    await p.click('#clear-selection');
    await p.waitForTimeout(200);
    assert.equal(await p.locator('#sel-job-a').isChecked(), false);
    assert.equal(await p.locator('#selection-bar').isVisible(), false);
    assert.equal(await p.locator('#job-job-a').getAttribute('data-selected'), 'false');
    // The document: its saved header above the text; a spoken marker is the text.
    await p.click('.tx-job[data-arg="job-a"] .tx-job-t');
    await p.waitForSelector('#transcript p');
    assert.match(await p.locator('#doc-source').innerText(), /Channel: Synthetic channel/);
    assert.match(await p.locator('#doc-source').innerText(), /Source: https:\/\/www\.youtube\.com\/watch\?v=aaaaaaaaaaa/);
    assert.match(await p.locator('#transcript').innerText(), /First words\.[\s\S]*Transcript:[\s\S]*Still speaking\./);
    // Old wording from an installed helper never shows.
    await p.click('.tx-job[data-arg="job-c"] .tx-job-t');
    await p.waitForFunction(() => /retrying that status write/.test(document.querySelector('#progress-message').textContent));
    assert.match(await p.locator('#progress-message').textContent(), /The helper is retrying/);
    assert.ok(!(await p.content()).includes('Scribe'), 'not anywhere on the page');
    // Queued from a YouTube tab: the library follows at once, not at its slow idle poll.
    const n = mock.log.filter(l => l.path === '/jobs' && l.method === 'GET').length;
    await p.waitForTimeout(300);
    const r = JSON.parse(await worker(() => wasm_bindgen.on_message(JSON.stringify({ action: 'startJob', url: 'https://www.youtube.com/watch?v=dddddddddd1' }), JSON.stringify({ id: chrome.runtime.id, url: 'https://www.youtube.com/', tab: { id: 1 }, frameId: 0 }))));
    assert.equal(r.ok, true);
    await p.waitForSelector('.tx-job[data-job-status="queued"]', { timeout: 2500 });
    assert.ok(mock.log.filter(l => l.path === '/jobs' && l.method === 'GET').length > n);
    await p.close();
  });

  await step('transcripts channel panel: close sticks and stops polling; a chip click does not bring it back; Escape closes; a double click on Retry sends one import', async () => {
    mock.reset(); await enable();
    const p = await open('https://www.youtube.com/@monkeydeveloper/videos');
    await p.waitForSelector('.tsc-ch');
    await p.click('[data-act="tsc-import"][data-arg="year"]');
    await p.waitForFunction(() => /Finding videos/.test(document.querySelector('#ytc-tsc-cht')?.textContent || ''), null, { timeout: 8000 });
    assert.equal(await p.locator('#ytc-tsc-chb').getAttribute('data-state'), 'discovering');
    await p.click('[data-act="tsc-chclose"]');
    assert.equal(await p.locator('#ytc-tsc-chp').isHidden(), true);
    await p.waitForTimeout(400);
    const polls = () => mock.log.filter(l => l.path.startsWith('/channels/previews/')).length;
    const p0 = polls();
    await p.waitForTimeout(3200);
    assert.equal(polls(), p0, 'no polling after close');
    await p.waitForSelector('#ytc-list .c[data-v] .tsc', { state: 'attached' });
    const id = await p.locator('#ytc-list .c[data-v]:has(.tsc)').first().getAttribute('data-v');
    await p.locator(`#ytc-list .c[data-v="${id}"] a.th`).hover();
    await p.click(chipOf(id));
    await p.waitForTimeout(800);
    assert.equal(await p.locator('#ytc-tsc-chp').isVisible(), false, 'a chip click does not reopen it');
    // Asking again opens it; Escape closes it.
    await p.click('[data-act="tsc-import"][data-arg="year"]');
    await p.waitForFunction(() => !document.querySelector('#ytc-tsc-chp').hidden, null, { timeout: 8000 });
    assert.equal(mock.log.filter(l => l.path === '/channels/import').length, 1, 'watches the running import rather than starting another');
    await p.mouse.move(2, 2);
    await p.evaluate(() => document.activeElement && document.activeElement.blur());
    await p.keyboard.press('Escape');
    await p.waitForFunction(() => document.querySelector('#ytc-tsc-chp').hidden, null, { timeout: 3000 });
    // A failed import offers Retry; two quick clicks are one import.
    mock.state.previewFail = true;
    await p.goto('https://www.youtube.com/@othercreator/videos', { waitUntil: 'commit' }).catch(() => {});
    await p.waitForSelector('.tsc-ch', { timeout: 15000 });
    const n0 = mock.log.filter(l => l.path === '/channels/import').length;
    await p.click('[data-act="tsc-import"][data-arg="all"]');
    await p.waitForSelector('#ytc-tsc-chr', { timeout: 8000 });
    await waitFor(() => mock.log.filter(l => l.path === '/channels/import').length === n0 + 1, 'first try');
    mock.state.previewFail = false;
    mock.log.length = 0;
    await p.locator('#ytc-tsc-chr').dblclick();
    await p.waitForTimeout(1200);
    assert.equal(mock.log.filter(l => l.path === '/channels/import').length, 1, 'a double click is one import');
    await p.close();
  });

  await step('transcripts look: one control height per row, one left edge, one row height, two font families, the list leads', async () => {
    mock.reset(); await enable();
    mock.state.jobs.push(
      mockHelper.job('job-a', 'aaaaaaaaaaa', 'completed', { title: 'A title long enough to wrap onto a second line if the row were allowed to grow at all, which it is not', created_at: '2026-11-01T10:00:00Z' }),
      mockHelper.job('job-b', 'bbbbbbbbbbb', 'transcribing', { progress: 64, created_at: '2026-11-01T09:00:00Z' }),
      mockHelper.job('job-c', 'ccccccccccc', 'completed', { title: 'Short', created_at: '2026-11-01T08:00:00Z' }));
    const p = await open(ext('reader.html'));
    await p.waitForSelector('.tx-job');
    const h = sel => p.locator(sel).evaluate(n => n.getBoundingClientRect().height);
    const x = sel => p.locator(sel).evaluate(n => Math.round(n.getBoundingClientRect().x));
    assert.equal(await h('#start-url'), await h('#start-button'));
    assert.deepEqual([...new Set(await p.locator('.tx-job').evaluateAll(a => a.map(n => n.getBoundingClientRect().height)))], [60]);
    assert.equal(await x('#hover-select'), await x('#sel-job-a'));
    assert.equal(await x('#folder-filter'), await x('#sel-job-a'));
    const empty = await x('#welcome h1');
    await p.click('.tx-job[data-arg="job-c"] .tx-job-t');
    await p.waitForSelector('#transcript p');
    assert.equal(await x('#video-title'), empty, 'the edge does not jump when opening one');
    assert.equal(await p.locator('#folder-name').isVisible(), false);
    await p.click('#folder-tools summary');
    assert.equal(await h('#folder-name'), await h('#create-folder'), 'New folder and +');
    await p.click('#folder-tools summary');
    await p.check('#sel-job-c');
    await p.waitForSelector('#selection-bar:not([hidden])');
    assert.deepEqual([...new Set([await h('#save-selected'), await h('#move-folder'), await h('#move-selected')])], [32]);
    await p.uncheck('#sel-job-c');
    mock.state.jobs[2].sections = [{ offset_ms: 0, text: 'Words.' }];
    await p.reload({ waitUntil: 'commit' });
    await p.waitForSelector('.tx-job');
    await p.click('.tx-job[data-arg="job-c"] .tx-job-t');
    await p.waitForSelector('#transcript p.ts');
    const families = await p.evaluate(() => [...new Set([...document.querySelectorAll('#tr *')].filter(n => n.childNodes.length && n.getClientRects().length).map(n => getComputedStyle(n).fontFamily))]);
    assert.ok(families.length <= 2, 'font families: ' + families.join(' | '));
    assert.ok(!families.some(f => /mono/i.test(f)));
    assert.equal(await p.locator('#transcript .tx-t').evaluate(n => getComputedStyle(n).fontVariantNumeric), 'tabular-nums');
    await shot(p, 'reader-1400');
    await p.setViewportSize({ width: 480, height: 900 });
    await p.click('#back');
    await p.waitForSelector('.tx-job:visible');
    const y = await p.locator('.tx-job').first().evaluate(n => n.getBoundingClientRect().y);
    assert.ok(y < 260, `the list starts at y=${y}`);
    await shot(p, 'reader-library-480');
    await p.close();
    mock.state.jobs = [];
    const pop = await open(ext('popup.html'), 400, 700);
    await pop.waitForSelector('[data-tx="zip-open"]');
    await pop.click('[data-tx="zip-open"]');
    await pop.waitForSelector('#zip-choices input');
    await pop.waitForTimeout(500);
    assert.equal(await pop.locator('#zip-choices input').first().evaluate(n => Math.round(n.getBoundingClientRect().x)), await pop.locator('#zip-title').evaluate(n => Math.round(n.getBoundingClientRect().x)));
    await pop.close();
  });

  await step('transcripts popup: the sponsor row never flashes the browser\'s default button colour as the popup opens', async () => {
    await disable();
    const p = await ctx.newPage();
    await p.addInitScript(() => {
      window.__bg = [];
      const tick = () => { const b = document.querySelector('#popup-sponsor'); if (b) { const c = getComputedStyle(b).backgroundColor; if (!window.__bg.includes(c)) window.__bg.push(c); } if (performance.now() < 800) requestAnimationFrame(tick); };
      requestAnimationFrame(tick);
    });
    for (let i = 0; i < 3; i++) {
      await p.goto(ext('popup.html'), { waitUntil: 'commit' });
      await p.waitForSelector('#ytc-popup[data-ready=true]');
      await p.waitForTimeout(850);
      const seen = await p.evaluate(() => window.__bg);
      assert.ok(seen.length > 0, 'the row was watched');
      assert.deepEqual(seen.filter(c => c !== 'rgba(0, 0, 0, 0)'), [], `colours seen: ${seen}`);
    }
    await p.close();
  });

  // ---- the popup.
  await step('transcripts popup: batch status, Stop all, folder cards (destination, All and named, create with Enter or +, counts, Browse files, Copy as ZIP, Folders, Show file), channel folders kept out', async () => {
    mock.reset();
    await enable();
    mock.state.jobs.push(mockHelper.job('job-1', 'aaaaaaaaaaa', 'completed', { folder_id: 'f-motion' }), mockHelper.job('job-2', 'bbbbbbbbbbb', 'transcribing', { progress: 40 }), mockHelper.job('job-3', 'ccccccccccc', 'queued'));
    const p = await open(ext('popup.html'), 400, 760);
    await p.waitForSelector('#popup-tr .ptr-card');
    assert.equal(await p.locator('#queue-stage').textContent(), '1 transcribing');
    assert.equal(await p.locator('#queue-summary').textContent(), '1 / 3');
    assert.equal(await p.locator('#queue-headline').textContent(), '2', 'N left');
    assert.equal(await p.locator('#queue-headline-label').textContent(), 'left');
    assert.equal(await p.locator('#connection-label').textContent(), 'Connected');
    assert.equal(await p.locator('#queue-progress').getAttribute('data-state'), 'working');
    assert.equal(await p.locator('#queue-progress [data-segment="saved"]').evaluate(n => n.style.width.startsWith('33')), true);
    assert.equal(await p.locator('#queue-counts').textContent(), '1 queued');
    await p.waitForFunction(() => document.querySelector('.ptr-card[data-folder-id=""] .ptr-count')?.textContent === '1');
    assert.deepEqual(await p.locator('.ptr-choice b').allTextContents(), ['All', 'Motion'], 'channel-created folders stay out of the chooser');
    assert.equal(await p.locator('.ptr-card[data-folder-id=""]').getAttribute('data-selected'), 'true');
    assert.equal(await p.locator('script[src="transcripts-core.js"]').count(), 1);
    await shot(p, 'popup-active');
    // Select a destination in one click; it does not change by actions on a card.
    await p.click('.ptr-card[data-folder-id="f-motion"] .ptr-choice');
    await p.waitForFunction(() => document.querySelector('.ptr-card[data-folder-id="f-motion"]')?.dataset.selected === 'true');
    assert.equal(mock.state.defaultFolder, 'f-motion');
    // Create with Enter, then with +; each selects itself.
    await p.fill('#folder-name', 'Talks');
    await p.press('#folder-name', 'Enter');
    await p.waitForFunction(() => document.querySelector('.ptr-card[data-folder-id="f-talks"]')?.dataset.selected === 'true');
    await p.waitForFunction(() => document.querySelector('#folder-name').value === '');
    await p.waitForSelector('#folder-name:not([disabled])');
    await p.fill('#folder-name', 'Notes');
    await p.click('#create-folder');
    await p.waitForFunction(() => document.querySelector('.ptr-card[data-folder-id="f-notes"]')?.dataset.selected === 'true');
    // Browse files: this folder, no selection change.
    await p.click('.ptr-card[data-folder-id="f-motion"] [data-tx="browse"]');
    await waitFor(() => mock.state.revealDirs.includes('f-motion'), 'browse');
    assert.equal(mock.state.defaultFolder, 'f-notes');
    // Copy as ZIP: a file attachment, confirmed; Show file appears.
    await p.click('.ptr-card[data-folder-id="f-motion"] .folder-copy');
    await waitFor(() => mock.state.copies.length === 1, 'copied');
    await p.waitForFunction(() => /ZIP copied.*Paste to attach/.test(document.querySelector('.ptr-card[data-folder-id="f-motion"] .ptr-note')?.textContent || ''));
    await p.click('.ptr-card[data-folder-id="f-motion"] [data-tx="show-zip"]');
    await waitFor(() => mock.state.reveal.includes('b'.repeat(32)), 'Show file');
    // Folders: include/exclude for All, remembered per save root; the choice travels with the All ZIP.
    await p.click('[data-tx="zip-open"]');
    await p.waitForSelector('#zip-view:not([hidden]) #zip-choices input');
    const names = await p.locator('#zip-choices span').allTextContents();
    assert.ok(names.includes('Motion'));
    await p.locator('#zip-choices input[data-zip-folder="Motion"]').uncheck();
    await waitFor(async () => JSON.stringify(await worker(() => chrome.storage.local.get('allZipExclusions:' + 'a'.repeat(64)))).includes('Motion'), 'the choice is kept');
    await shot(p, 'popup-folders');
    await p.click('#zip-done');
    await p.waitForSelector('#ptr-main:not([hidden])');
    assert.equal(await p.locator('[data-tx="zip-open"]').getAttribute('data-filtered'), 'true');
    await p.click('.ptr-card[data-folder-id=""] .folder-copy');
    await waitFor(() => mock.state.copies.length === 2, 'All copied');
    assert.deepEqual(mock.state.attachments['b'.repeat(32)].excluded_folders, ['Motion']);
    assert.equal(mock.state.attachments['b'.repeat(32)].root_token, 'a'.repeat(64));
    // Stop all.
    await p.click('#queue-stop');
    await p.waitForFunction(() => document.querySelector('#queue-stage')?.textContent !== '1 transcribing', null, { timeout: 8000 });
    assert.equal(mock.state.jobs.filter(j => j.status === 'cancelled').length, 2);
    assert.equal(mock.state.jobs.filter(j => j.status === 'completed').length, 1, 'finished transcripts stay');
    // Library and settings.
    const opened = ctx.waitForEvent('page');
    await p.click('#library');
    const r = await opened;
    await r.waitForURL(u => u.href.includes('reader.html'));
    await r.close();
    await shot(p, 'popup-idle');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  await step('transcripts popup: an attention issue opens its details; a stopped helper offers Retry', async () => {
    mock.reset();
    await enable();
    mock.state.jobs.push(mockHelper.job('job-f', 'fffffffffff', 'failed', { error: 'YouTube said no.', title: 'A failed one' }));
    const p = await open(ext('popup.html'), 400, 700);
    await p.waitForSelector('.ptr-error');
    assert.match(await p.locator('.ptr-error').innerText(), /A failed one/);
    await shot(p, 'popup-attention');
    const opened = ctx.waitForEvent('page');
    await p.click('.ptr-error');
    const r = await opened;
    await r.waitForSelector('#video-title');
    await r.waitForFunction(() => document.querySelector('#progress-message')?.textContent === 'YouTube said no.');
    assert.match(r.url(), /view=attention/);
    await shot(r, 'reader-attention-1400');
    // The library has one tab: asking again focuses it, even for another view of it.
    const before = ctx.pages().length;
    await p.click('#library');
    await waitFor(() => r.url().endsWith('/reader.html'), 'the same tab shows the library', 8000);
    assert.equal(ctx.pages().length, before, 'no second library tab');
    await r.close();
    mock.down(true);
    await p.waitForFunction(() => !document.querySelector('#queue-retry').hidden, null, { timeout: 8000 });
    assert.equal(await p.locator('#queue-stage').textContent(), 'Last confirmed counts');
    assert.equal(await p.locator('#connection-label').textContent(), 'Offline');
    assert.equal(await p.locator('#queue-headline-label').textContent(), 'connection lost');
    await shot(p, 'popup-offline');
    mock.down(false);
    await p.click('#queue-retry');
    await p.waitForFunction(() => document.querySelector('#queue-retry').hidden, null, { timeout: 8000 });
    await p.close();
  });

  // ---- the reader.
  await step('transcripts reader: folder filter, checkbox and hover selection, copy full text, duplicates once, move, export TXT/Markdown/JSON, Show file, cancel, retry, removed, paste a link', async () => {
    mock.reset();
    await enable();
    const long = Array.from({ length: 12 }, (_, i) => `Paragraph ${i + 1}. A transcript is mostly plain sentences in a row, so its measure and leading decide whether anyone can read it for an hour. `.repeat(3)).join('\n\n');
    mock.state.jobs.push(
      mockHelper.job('job-a', 'aaaaaaaaaaa', 'completed', { title: 'First <b>talk</b> & friends', folder_id: 'f-motion', text: long, created_at: '2026-11-01T10:00:00Z' }),
      mockHelper.job('job-b', 'bbbbbbbbbbb', 'completed', { title: 'Second talk', text: 'Same words.', created_at: '2026-11-01T09:00:00Z' }),
      mockHelper.job('job-c', 'ccccccccccc', 'completed', { title: 'Second talk', text: 'Same words.', url: 'https://www.youtube.com/watch?v=bbbbbbbbbbb', created_at: '2026-11-01T08:00:00Z' }),
      mockHelper.job('job-d', 'ddddddddddd', 'transcribing', { progress: 64, total_chunks: 8, completed_chunks: 5, created_at: '2026-11-01T07:00:00Z', message: 'Transcribing chunk 6 of 8 with MAI-Transcribe-2' }),
      mockHelper.job('job-e', 'eeeeeeeeeee', 'removed', { title: 'Gone', created_at: '2026-11-01T06:00:00Z' }),
      mockHelper.job('job-f', 'fffffffffff', 'failed', { title: 'Needs sign in', error: 'Sign in to confirm you are not a bot', created_at: '2026-11-01T05:00:00Z' }),
    );
    const p = await open(ext('reader.html'));
    await p.context().grantPermissions(['clipboard-read', 'clipboard-write']);
    await p.waitForSelector('.tx-job');
    assert.equal(await p.locator('.tx-job').count(), 6);
    assert.equal(await p.locator('.tx-job-t').first().textContent(), 'First <b>talk</b> & friends', 'hostile titles are text');
    assert.equal(await p.locator('#connection').textContent(), 'Helper connected');
    await shot(p, 'reader-library-1400');
    // Open: the transcript set for reading; the header stays out of the body but in what is copied.
    await p.click('.tx-job[data-arg="job-a"] .tx-job-t');
    await p.waitForSelector('#transcript p');
    assert.equal(await p.locator('#transcript p').count(), 12);
    const measure = await p.locator('#transcript p').first().evaluate(n => { const s = getComputedStyle(n); return { w: n.getBoundingClientRect().width, size: parseFloat(s.fontSize), line: parseFloat(s.lineHeight) }; });
    assert.ok(measure.w / measure.size <= 40 && measure.w / measure.size >= 28, `a comfortable measure: ${measure.w}px at ${measure.size}px`);
    assert.ok(measure.line / measure.size >= 1.5, 'open leading');
    assert.equal(await p.locator('#video-title').textContent(), 'First <b>talk</b> & friends');
    assert.match(await p.locator('#video-metadata').innerText(), /Synthetic channel/);
    assert.match(await p.locator('#video-metadata').innerText(), /1:02:05/);
    assert.match(await p.locator('#video-metadata').innerText(), /Published 2026-08-28/);
    assert.equal(await p.locator('#video-source').getAttribute('href'), 'https://www.youtube.com/watch?v=aaaaaaaaaaa');
    assert.match(p.url(), /reader\.html\?id=job-a$/);
    await p.bringToFront();
    await shot(p, 'reader-1400');
    await p.click('#copy-document');
    await p.waitForFunction(() => /copied/i.test(document.querySelector('#toast')?.textContent || ''));
    const copied = await p.evaluate(() => navigator.clipboard.readText());
    assert.ok(copied.startsWith('# First <b>talk</b> & friends') && copied.includes('Paragraph 12.'), 'the whole document');
    // Timestamps beside the sections, when the helper knows them.
    mock.state.jobs[1].sections = [{ offset_ms: 0, text: 'Same words.' }, { offset_ms: 3725000, text: 'Later on.' }];
    await p.click('.tx-job[data-arg="job-b"] .tx-job-t');
    await p.waitForSelector('#transcript p.ts');
    assert.deepEqual(await p.locator('#transcript .tx-t').allTextContents(), ['0:00', '1:02:05']);
    assert.equal(await p.locator('#transcript .tx-t').nth(1).getAttribute('href'), 'https://www.youtube.com/watch?v=bbbbbbbbbbb&t=3725s');
    await shot(p, 'reader-timestamps-1400');
    // Export: each format, through the helper's writer.
    for (const [id, format] of [['#download-md', 'md'], ['#download-txt', 'txt'], ['#download-json', 'json']]) {
      await p.click(id);
      await waitFor(() => mock.state.exports.some(e => e.format === format && e.job_ids?.[0] === 'job-b'), 'export ' + format);
    }
    await p.click('#show-file');
    await waitFor(() => mock.state.reveal.includes('bbbbbbbbbbb'), 'Show file');
    // Selection: checkbox, then hover; exact duplicates once.
    await p.check('#sel-job-b');
    await p.waitForFunction(() => /1 transcript selected, ready to copy/.test(document.querySelector('#selection-count').textContent));
    await p.check('#hover-select');
    await p.locator('.tx-job[data-arg="job-c"]').hover();
    await p.waitForFunction(() => /2 transcripts selected/.test(document.querySelector('#selection-count').textContent));
    await p.locator('.tx-job[data-arg="job-d"]').hover();
    assert.equal(await p.locator('#sel-job-d').isChecked(), false, 'an unfinished job cannot be selected');
    await p.waitForFunction(() => /ready to copy/.test(document.querySelector('#selection-count').textContent));
    await shot(p, 'reader-selection-1400');
    await p.click('#copy-selected');
    await p.waitForFunction(() => /Copied 1 full transcript\. Exact duplicates were included only once/.test(document.querySelector('#toast')?.textContent || ''));
    assert.equal((await p.evaluate(() => navigator.clipboard.readText())).split('---').length, 1);
    // Move selected.
    await p.selectOption('#move-folder', 'f-motion');
    await p.click('#move-selected');
    await p.waitForFunction(() => /Moved 2 transcripts to Motion/.test(document.querySelector('#toast')?.textContent || ''));
    assert.equal(mock.state.jobs.find(j => j.id === 'job-c').folder_id, 'f-motion');
    // Folder filter; Copy folder; Save folder; use for new videos; create.
    await p.uncheck('#hover-select');
    await p.selectOption('#folder-filter', '');
    await p.waitForFunction(() => document.querySelectorAll('.tx-job').length === 3);
    await p.selectOption('#folder-filter', 'f-motion');
    await p.waitForFunction(() => document.querySelectorAll('.tx-job').length === 3);
    await p.click('#folder-tools summary');
    assert.equal(await p.locator('#copy-folder').textContent(), 'Copy folder');
    await p.click('#copy-folder');
    await p.waitForFunction(() => /Copied 2 full transcripts/.test(document.querySelector('#toast')?.textContent || ''), null, { timeout: 8000 });
    const folderText = await p.evaluate(() => navigator.clipboard.readText());
    assert.equal(folderText.split('\n\n---\n\n').length, 2, 'whole documents, duplicates once');
    await p.click('#save-folder');
    await waitFor(() => mock.state.exports.some(e => e.folder_id === 'f-motion'), 'Save folder');
    await p.waitForSelector('#set-default-folder:not([disabled])');
    await p.click('#set-default-folder');
    await waitFor(() => mock.state.defaultFolder === 'f-motion', 'use for new videos');
    await p.fill('#tr-root #folder-name', 'Fresh');
    await p.press('#tr-root #folder-name', 'Enter');
    await p.waitForFunction(() => document.querySelector('#folder-filter').value === 'f-fresh');
    assert.match(await p.locator('#library-empty').textContent(), /This folder is empty/);
    await p.selectOption('#folder-filter', '__all__');
    // Active job: progress, chunks, cancel.
    await p.click('.tx-job[data-arg="job-d"] .tx-job-t');
    await p.waitForFunction(() => document.querySelector('#progress-percent')?.textContent === '64%');
    assert.match(await p.locator('#progress-chunks').textContent(), /5 of 8/);
    assert.equal(await p.locator('.tx-job[data-arg="job-d"] .tx-chip').textContent(), '64%');
    await shot(p, 'reader-progress-1400');
    await p.click('#cancel-job');
    await waitFor(() => mock.state.jobs.find(j => j.id === 'job-d').status === 'cancelled', 'cancelled');
    await p.waitForSelector('#retry-job:not([hidden])');
    await p.click('#retry-job');
    await waitFor(() => mock.state.jobs.some(j => j.video_id === 'ddddddddddd' && j.status === 'queued'), 'retried');
    // Failed with a sign-in message: the Brave login hint.
    await p.click('.tx-job[data-arg="job-f"] .tx-job-t');
    await p.waitForSelector('#youtube-access-help:not([hidden])');
    // Removed: excluded, explained, no document.
    await p.click('.tx-job[data-arg="job-e"] .tx-job-t');
    await p.waitForFunction(() => /Removed from your save folder/.test(document.querySelector('#reader-notice').textContent));
    assert.equal(await p.locator('#document-tools').isVisible(), false);
    assert.equal(await p.locator('#sel-job-e').isDisabled(), true);
    await shot(p, 'reader-removed-1400');
    // Attention view.
    await p.click('#attention-filter');
    await p.waitForFunction(() => document.querySelector('#library-title').textContent === 'Needs attention');
    await p.waitForFunction(() => document.querySelectorAll('.tx-job').length >= 1, null, { timeout: 8000 });
    assert.match(p.url(), /view=attention/);
    await p.click('#attention-filter');
    await p.waitForFunction(() => document.querySelector('#library-title').textContent === 'Library');
    // The welcome: paste a link.
    await p.waitForSelector('#welcome:not([hidden])');
    await shot(p, 'reader-welcome-1400');
    await p.fill('#start-url', 'https://example.com/watch?v=aaaaaaaaaaa');
    await p.click('#start-button');
    await p.waitForFunction(() => /YouTube video/.test(document.querySelector('#toast')?.textContent || ''));
    await p.fill('#start-url', 'https://youtu.be/ggggggggggg?t=9');
    await p.click('#start-button');
    await waitFor(() => mock.state.jobs.some(j => j.video_id === 'ggggggggggg'), 'queued from a link');
    assert.equal(mock.log.filter(l => l.path === '/jobs' && l.method === 'POST').pop().body.url, 'https://www.youtube.com/watch?v=ggggggggggg', 'time and share parts dropped');
    // 480 wide: one column.
    await p.click('.tx-job[data-arg="job-a"] .tx-job-t');
    await p.waitForSelector('#transcript p');
    await p.setViewportSize({ width: 480, height: 900 });
    await p.waitForTimeout(150);
    await shot(p, 'reader-480');
    assert.ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'no sideways scroll');
    assert.equal(await p.locator('.tx-lib').isVisible(), false);
    await p.click('#back');
    await p.waitForSelector('.tx-lib:visible');
    await shot(p, 'reader-library-480');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  // ---- a channel's tabs (real answers, tests/fixtures/channel_tabs.json): lists set aside and brought back.
  const ctUrl = `https://www.youtube.com/channel/${CT_SKY}`;
  const land = p => p.waitForFunction(() => document.querySelectorAll('#ytc-list .c').length && !document.querySelector('#ytc-list .c.sk'));
  const openCt = async (url = ctUrl) => { const p = await open(url); await p.waitForSelector('#ytc-cht'); await land(p); return p; };
  // In the click's own task: nothing that arrives later (the next status answer) can make this pass.
  const pressAndRead = (p, sel, read, arg) => p.evaluate(({ sel, read, arg }) => {
    document.querySelector(sel).dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, view: window }));
    return (0, eval)(read)(arg);
  }, { sel, read: read.toString(), arg });

  await step('transcripts channel tabs: off, walking the tabs shows no control and asks the helper nothing', async () => {
    await disable();
    const before = await fetches(), nativeBefore = await native();
    const p = await openCt();
    for (const id of await p.$$eval('#ytc-cht [id^="ytc-ct-"]', a => a.map(e => e.id))) {
      await p.click('#' + id);
      await land(p);
      assert.equal(await p.locator('.tsc,.tsc-ch').count(), 0, id);
    }
    await p.waitForTimeout(300);
    assert.equal(await fetches(), before);
    assert.equal(await native(), nativeBefore);
    await p.close();
  });

  await step('transcripts channel tabs: every tab\'s video cards carry a chip in the thumbnail\'s corner, clear of the badge; a playlist card has none; each list is asked about once', async () => {
    mock.reset();
    await enable();
    const p = await openCt();
    const seen = {};
    for (const id of await p.$$eval('#ytc-cht [id^="ytc-ct-"]', a => a.map(e => e.id))) {
      await p.click('#' + id);
      await land(p);
      const counts = await p.evaluate(() => ({ cards: document.querySelectorAll('#ytc-list .c').length, videos: document.querySelectorAll('#ytc-list .c[data-v]').length, bare: document.querySelectorAll('#ytc-list .c[data-v]:not(.chc):not(:has(.tsc))').length, stray: document.querySelectorAll('#ytc-list .c:not([data-v]) .tsc').length }));
      seen[id] = counts;
      assert.equal(counts.bare, 0, `${id}: a video card without a chip`);
      assert.equal(counts.stray, 0, `${id}: a chip on a card that is not a video`);
      if (counts.videos) {
        const n = await p.evaluate(() => { const c = document.querySelector('#ytc-list .c[data-v]:has(.tsc)'); c.dataset.trShown = '1'; return c.dataset.v; });
        const card = `#ytc-list .c[data-v="${n}"]`;
        await p.locator(`${card} a.th`).hover();
        await p.waitForFunction(s => getComputedStyle(document.querySelector(s)).opacity === '1', `${card} .tsc`);
        const g = await p.evaluate(s => { const c = document.querySelector(s), r = e => e.getBoundingClientRect(), t = r(c.querySelector('a.th')), k = r(c.querySelector('.tsc')), b = c.querySelector('.th b'); const q = b && r(b); return { right: t.right - k.right, top: k.top - t.top, hit: !q || k.right <= q.left || q.right <= k.left || k.bottom <= q.top || q.bottom <= k.top }; }, card);
        assert.ok(Math.abs(g.right - 6) <= 1.5 && Math.abs(g.top - 6) <= 1.5, `${id}: corner ${JSON.stringify(g)}`);
        assert.ok(g.hit, `${id}: chip clear of the badge`);
        await shot(p, `channel-tab-${id.replace('ytc-ct-', '')}-chip`);
        await p.mouse.move(2, 2);
      }
    }
    for (const t of ['ytc-ct-videos', 'ytc-ct-shorts', 'ytc-ct-streams']) assert.ok(seen[t].videos >= 1, `${t} has video cards: ${JSON.stringify(seen)}`);
    if (seen['ytc-ct-playlists']) assert.equal(seen['ytc-ct-playlists'].videos, 0, 'a playlist is not one video');
    await waitFor(() => mock.log.some(l => l.path === '/videos/status'), 'the status batch');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  await step('transcripts channel tabs: a job\'s live state reaches a list set aside, in the task it comes back (tab and sort); turned off or on while away, the list follows', async () => {
    mock.reset();
    await enable();
    const p = await openCt(), witness = await openCt();
    const id = await p.$eval('#ytc-list .c[data-v]:has(.tsc)', c => c.dataset.v);
    const chip = chipOf(id);
    await witness.waitForSelector(chip);
    await p.bringToFront();
    await p.locator(`#ytc-list .c[data-v="${id}"] a.th`).hover();
    await p.click(chip);
    await p.waitForFunction(s => document.querySelector(s)?.dataset.s === 'on', chip);
    await p.mouse.move(2, 2);
    const j = mock.state.jobs[0];
    // A tab: live while its list is away. (The witness tab shows when the message has gone out.)
    await p.click('#ytc-ct-shorts');
    await land(p);
    j.status = 'transcribing'; j.progress = 42;
    await witness.waitForFunction(s => document.querySelector(s)?.innerText === '42%', chip, { timeout: 9000 });
    const read = s => { const n = document.querySelector(s); return n && [n.dataset.s, n.textContent, n.style.getPropertyValue('--p')]; };
    assert.deepEqual(await pressAndRead(p, '#ytc-ct-videos', read, chip), ['on', '42%', '42%'], 'the percent is there on the frame the list is back');
    // Finished while away.
    await p.click('#ytc-ct-streams');
    await land(p);
    const asked = () => mock.log.filter(l => l.path === '/videos/status' && l.body.video_ids.includes(id)).length, asked0 = asked();
    j.status = 'completed'; j.progress = 100; j.saved = true;
    await witness.waitForFunction(s => document.querySelector(s)?.dataset.s === 'ok', chip, { timeout: 9000 });
    // Both tabs ask what became of it (this one has no such card on the page).
    await waitFor(() => asked() >= asked0 + 2, 'the tab with the list away asks too', 9000);
    await p.waitForTimeout(200);
    assert.equal((await pressAndRead(p, '#ytc-ct-videos', read, chip))[0], 'ok', 'Saved, on the frame the list is back');
    // A sort's list comes back without a navigation.
    await p.click('#ytc-cs-1');
    await p.waitForFunction(() => !document.querySelector('#ytc-chs .fx-pg') && document.querySelector('#ytc-cs-1')?.getAttribute('aria-pressed') === 'true');
    await land(p);
    j.status = 'removed'; j.saved = false;
    await witness.bringToFront();
    await p.bringToFront();
    const statusesBefore = mock.log.filter(l => l.path === '/videos/status').length;
    await p.click('#ytc-cs-0');
    await p.waitForFunction(s => document.querySelector(s)?.dataset.s === 'rm', chip, { timeout: 4000 });
    assert.ok(mock.log.filter(l => l.path === '/videos/status').length > statusesBefore, 'a sort\'s list is asked about again');
    await witness.close();
    // Turned off while a list is away: it comes back with no control at all.
    await p.click('#ytc-ct-shorts');
    await land(p);
    await disable();
    await p.waitForFunction(() => !document.querySelector('.tsc,.tsc-ch'));
    assert.equal(await pressAndRead(p, '#ytc-ct-videos', () => document.querySelectorAll('.tsc').length), 0, 'no chip in a list that left with them');
    // Turned on while a list is away: it comes back dressed. (Shorts left with no chips.)
    await enable();
    await p.waitForSelector('#ytc-list .c[data-v] .tsc');
    assert.equal(await pressAndRead(p, '#ytc-ct-shorts', () => document.querySelectorAll('#ytc-list .c[data-v]:not(:has(.tsc))').length), 0, 'every card of a list that left without them');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  await step('transcripts channel tabs: Import stays above the tab row on every tab, on the channel\'s own URL; its panel survives a tab change; the row still sticks; turning it on dresses the header', async () => {
    mock.reset();
    await disable();
    const p = await openCt(`${ctUrl}/shorts`);
    await enable();
    await p.waitForSelector('.ch .tsc-ch');
    const above = sel => p.evaluate(s => { const a = document.querySelector(s).getBoundingClientRect(), r = document.querySelector('.chb').getBoundingClientRect(); return a.bottom <= r.top + 0.5; }, sel);
    assert.ok(await above('.tsc-ch'), 'the import row is in the header, above the tabs');
    await shot(p, 'channel-tabs-import-1400');
    await p.click('[data-act="tsc-import"][data-arg="year"]');
    await waitFor(() => mock.log.some(l => l.path === '/channels/import'), 'the import started');
    assert.equal(mock.log.find(l => l.path === '/channels/import').body.url, ctUrl, 'the channel, not its tab');
    await p.waitForFunction(() => { const n = document.querySelector('#ytc-tsc-chp'); return n && !n.hidden; });
    assert.ok(await above('#ytc-tsc-chp'), 'its panel opens above the tabs, pushing them down');
    const panel = await p.evaluateHandle(() => document.querySelector('#ytc-tsc-chp'));
    await p.click('#ytc-ct-videos');
    await land(p);
    assert.ok(await panel.evaluate(n => n.isConnected && !n.hidden), 'the same panel, still open, on another tab');
    assert.ok(await above('#ytc-tsc-chp'));
    await shot(p, 'channel-tabs-panel-1400');
    assert.equal(mock.log.filter(l => l.path === '/channels/import').length, 1, 'a tab change starts nothing');
    await p.evaluate(() => scrollTo(0, 900));
    await p.waitForFunction(() => scrollY > 300);
    const stuck = await p.$eval('.chb', e => e.getBoundingClientRect().top);
    assert.ok(Math.abs(stuck - 64) <= 1, `the tab row under the top bar: ${stuck}`);
    await p.evaluate(() => scrollTo(0, 0));
    await p.setViewportSize({ width: 480, height: 800 });
    await p.waitForTimeout(100);
    assert.ok(await above('#ytc-tsc-chp'), '480 px');
    assert.ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'no sideways scroll at 480 px');
    await shot(p, 'channel-tabs-panel-480');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  // ---- channel import.
  await step('transcripts channel page: All videos or Past year, Include Shorts, live panel, Cancel discovery, close keeps it running', async () => {
    mock.reset();
    await enable();
    const p = await open('https://www.youtube.com/@monkeydeveloper/videos');
    await p.waitForSelector('.tsc-ch');
    assert.equal(await p.locator('.tsc-ch [data-act="tsc-import"]').count(), 2);
    assert.equal(await p.locator('#ytc-tsc-shorts').getAttribute('aria-checked'), 'false', 'Shorts excluded by default');
    assert.match(await p.locator('.tsc-chnote').textContent(), /every video it finds.*Azure resource bills each one/, 'said before anything starts');
    await shot(p, 'channel-1400');
    await p.click('[data-act="tsc-import"][data-arg="year"]');
    await p.waitForFunction(() => document.querySelector('#ytc-tsc-chp') && !document.querySelector('#ytc-tsc-chp').hidden);
    await waitFor(() => mock.log.some(l => l.path === '/channels/import'), 'the import started');
    const first = mock.log.find(l => l.path === '/channels/import').body;
    assert.deepEqual(first, { url: 'https://www.youtube.com/@monkeydeveloper', scope: 'year', include_shorts: false });
    await p.waitForFunction(() => /Finding videos: 4 checked, 2 found/.test(document.querySelector('#ytc-tsc-cht')?.textContent || ''), null, { timeout: 8000 });
    assert.match(await p.locator('#ytc-tsc-chq').textContent(), /2 queued so far\. Your Azure resource bills each one/);
    assert.deepEqual(await p.locator('#ytc-tsc-tot span').allTextContents(), ['Queued', 'Processing', 'Saved', 'Failed']);
    assert.equal(await p.locator('#ytc-tsc-tot [data-count="queued"]').textContent(), '2');
    assert.equal(await p.locator('#ytc-tsc-chp .tsc-chl').getAttribute('href'), 'https://www.youtube.com/@monkeydeveloper');
    assert.match(await p.locator('#ytc-tsc-chs').textContent(), /Past year, Shorts excluded/);
    assert.match(await p.locator('#ytc-tsc-chc').textContent(), /0 saved, 0 failed, 2 remaining/);
    await p.waitForTimeout(350);
    assert.equal(await p.locator('#ytc-tsc-chp').evaluate(n => getComputedStyle(n).opacity), '1', 'the panel settles');
    await shot(p, 'channel-active-1400');
    // Cancel discovery: it stops finding; queued videos go on.
    await p.click('#ytc-tsc-chk');
    await p.waitForFunction(() => /Stopped/.test(document.querySelector('#ytc-tsc-cht').textContent), null, { timeout: 8000 });
    assert.equal(mock.state.previews.pv1.status, 'cancelled');
    // Include Shorts for the next one, All videos.
    await p.click('#ytc-tsc-shorts');
    assert.equal(await p.locator('#ytc-tsc-shorts').getAttribute('aria-checked'), 'true');
    await p.click('[data-act="tsc-import"][data-arg="all"]');
    await waitFor(() => mock.log.filter(l => l.path === '/channels/import').length === 2, 'the second import');
    assert.deepEqual(mock.log.filter(l => l.path === '/channels/import')[1].body, { url: 'https://www.youtube.com/@monkeydeveloper', scope: 'all', include_shorts: true });
    await p.waitForFunction(() => /All videos, Shorts included/.test(document.querySelector('#ytc-tsc-chs').textContent), null, { timeout: 8000 });
    // Finish: all saved.
    const v = mock.state.previews.pv2;
    v.status = 'ready'; v.progress = { total: 2, saved: 2, queued: 0, processing: 0, saving: 0, discovery_complete: true, finished: true };
    await p.waitForFunction(() => /All transcripts saved/.test(document.querySelector('#ytc-tsc-cht').textContent), null, { timeout: 8000 });
    assert.equal(await p.locator('#ytc-tsc-chb [data-segment="saved"]').evaluate(n => n.style.width), '100%');
    await p.click('[data-act="tsc-chclose"]');
    assert.equal(await p.locator('#ytc-tsc-chp').isVisible(), false);
    // Another tab's preview id is refused: previews belong to the tab that started them.
    const r = JSON.parse(await worker(() => wasm_bindgen.on_message(JSON.stringify({ action: 'getChannelPreview', id: 'pv2' }), JSON.stringify({ id: chrome.runtime.id, url: 'https://www.youtube.com/', tab: { id: 987654 }, frameId: 0 }))));
    assert.equal(r.ok, false);
    assert.match(r.error, /belongs to another page/);
    await p.setViewportSize({ width: 480, height: 900 });
    await p.waitForTimeout(150);
    await shot(p, 'channel-480');
    assert.ok(await p.evaluate(() => document.documentElement.scrollWidth <= innerWidth), 'no sideways scroll');
    assert.deepEqual(p.errors, []);
    await p.close();
  });

  // ---- TV.
  await step('transcripts TV mode: never, even with the flag on: no chips, no button, no message, no request', async () => {
    mock.reset();
    await enable();
    const n0 = await native();
    const tv = await ctx.newPage();
    await tv.setViewportSize({ width: 1280, height: 720 });
    await tv.addInitScript(() => { try { localStorage.setItem('ytc:platform', 'tv'); localStorage.setItem('ytc-test-noready', '1'); } catch {} });
    await tv.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await tv.waitForSelector('#ytc-list .c[data-v] .t', { timeout: 20000 });
    await tv.waitForTimeout(500);
    assert.equal(await tv.locator('.tsc,.tsc-w,.tsc-ch').count(), 0);
    assert.deepEqual(mock.log.filter(l => l.path === '/videos/status' || l.method === 'POST'), [], 'nothing asked from the TV page (the worker\'s own toolbar read is not the page)');
    assert.equal(await native(), n0);
    await tv.evaluate(() => { for (const k of ['ytc:platform', 'ytc-test-noready']) localStorage.removeItem(k); }).catch(() => {});
    await tv.close();
  });

  // ---- the real helper.
  await step('transcripts with the REAL helper from helper/: temp root, ephemeral port, blank Azure, then a whole job with a loopback Speech server', async () => {
    // A test build (the feature loopback-speech is never in a release).
    const helperDir = path.join(__dirname, '..', 'helper');
    execFileSync('cargo', ['build', '--locked', '-q', '--features', 'loopback-speech', '-p', 'scribe-helper', '--target-dir', 'target/e2e'], { cwd: helperDir, stdio: 'inherit' });
    const bin = path.join(helperDir, 'target', 'e2e', 'debug', 'scribe-helper');
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'ytc-real-helper-'));
    const speech = await realSpeech();
    let helper = null;
    const token = 'real-helper-e2e-token-0123456789abcdef';
    const port = await freePort(), origin = `http://127.0.0.1:${port}`;
    const ffmpeg = process.env.SCRIBE_TEST_FFMPEG_DIR || '';
    const start = async env => {
      fs.writeFileSync(path.join(root, '.env'), env, { mode: 0o600 });
      const logFile = fs.openSync(path.join(root, 'stdout.log'), 'a');
      helper = spawn(bin, ['--root', root, 'serve', '--port', String(port)], { stdio: ['ignore', logFile, logFile], env: Object.fromEntries(Object.entries(process.env).filter(([k]) => !/^(AZURE_|HELPER_TOKEN|DATA_DIR)/.test(k))) });
      await waitFor(async () => { try { return (await fetch(origin + '/health')).ok; } catch { return false; } }, 'the real helper', 20000);
    };
    const stop = () => new Promise(r => { if (!helper) return r(); helper.once('exit', r); helper.kill('SIGTERM'); helper = null; setTimeout(r, 6000); });
    try {
      fs.mkdirSync(path.join(root, 'extension'));
      fs.copyFileSync(path.join(__dirname, '..', 'extension', 'manifest.json'), path.join(root, 'extension', 'manifest.json'));
      await start(`AZURE_SPEECH_ENDPOINT=\nAZURE_SPEECH_KEY=\nHELPER_TOKEN='${token}'\n`);
      // The native host, framed as a browser calls it, from a Redline-shaped root. SCRIBE_NATIVE_NO_START (a test-build
      // feature; never in a release) keeps this from probing or kickstarting the service installed on this machine.
      const frame = () => { const b = Buffer.from('{"action":"getConnection"}'); const h = Buffer.alloc(4); h.writeUInt32LE(b.length); return Buffer.concat([h, b]); };
      const host = origin_ => { const r = spawnSync(bin, ['--root', root, 'native-host', origin_], { input: frame(), env: { ...process.env, SCRIBE_NATIVE_NO_START: '1' }, timeout: 15000 }); return JSON.parse(r.stdout.subarray(4).toString('utf8')); };
      const answered = host(`chrome-extension://${EXT_ID}/`);
      assert.deepEqual([answered.ok, answered.pairingToken, answered.url], [true, token, 'http://127.0.0.1:8765'], 'this extension pairs');
      const refused = host('chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/');
      assert.equal(refused.ok, false, 'another extension does not');
      assert.equal(refused.pairingToken, undefined);
      await worker(([o, t]) => { TR_HELPER = o; wasm_bindgen.worker_boot(o); self.__tr.token = t; return TR.vault_del('pairingToken'); }, [origin, token]);
      await disable();
      // Settings pairs with it and reads the blank configuration: the feature stays off.
      const p = await open(ext('settings.html'));
      await p.waitForFunction(() => document.querySelector('#helper-status')?.textContent === 'Helper connected', null, { timeout: 15000 });
      await p.waitForFunction(() => document.querySelector('#azure-status')?.textContent === 'Add your Azure connection');
      assert.equal(await worker(() => chrome.storage.local.get('ytcTranscriptsV1').then(o => o.ytcTranscriptsV1 ?? null)), null);
      await p.fill('#azure-endpoint', 'https://myresource.services.ai.azure.com/api/projects/proj1');
      await p.fill('#azure-key', 'not-a-real-key');
      await p.click('#save-azure');
      await p.waitForFunction(() => document.querySelector('#azure-status')?.textContent === 'Configuration saved', null, { timeout: 15000 });
      assert.equal(await p.inputValue('#azure-endpoint'), 'https://myresource.cognitiveservices.azure.com', 'the real helper converts a Foundry URL');
      assert.ok(fs.readFileSync(path.join(root, '.env'), 'utf8').includes('not-a-real-key'), 'the key lives in the helper\'s .env');
      assert.equal(fs.statSync(path.join(root, '.env')).mode & 0o777, 0o600, 'owner-only');
      await waitFor(() => worker(() => chrome.storage.local.get('ytcTranscriptsV1').then(o => o.ytcTranscriptsV1?.on === true)), 'enabled');
      assert.ok(!JSON.stringify(await worker(() => chrome.storage.local.get(null))).includes('not-a-real-key'));
      await p.close();
      // Library, folders, default folder and counts against the real routes.
      const pop = await open(ext('popup.html'), 400, 700);
      await pop.waitForSelector('.ptr-card');
      await pop.fill('#folder-name', 'Real folder');
      await pop.press('#folder-name', 'Enter');
      await pop.waitForFunction(() => [...document.querySelectorAll('.ptr-choice b')].some(b => b.textContent === 'Real folder'), null, { timeout: 10000 });
      await pop.waitForFunction(() => document.querySelector('.ptr-card[data-selected="true"] .ptr-choice b')?.textContent === 'Real folder', null, { timeout: 10000 });
      await pop.close();
      // Real shapes through the worker.
      const asTab = { url: 'https://www.youtube.com/', tab: { id: 1 }, frameId: 0 };
      const statuses = JSON.parse(await worker(s => wasm_bindgen.on_message(JSON.stringify({ action: 'getVideoStatuses', videoIds: ['aaaaaaaaaaa'] }), JSON.stringify({ ...s, id: chrome.runtime.id })), asTab));
      assert.equal(statuses.ok, true);
      assert.deepEqual(statuses.data.statuses, {});
      const queue = JSON.parse(await worker(() => wasm_bindgen.on_message(JSON.stringify({ action: 'getQueueStatus' }), JSON.stringify({ id: chrome.runtime.id, url: chrome.runtime.getURL('popup.html') }))));
      assert.equal(queue.ok, true, JSON.stringify(queue));
      assert.equal(typeof queue.data.total, 'number');
      // The helper itself refuses a caller without the token and any web origin.
      assert.equal((await fetch(origin + '/library')).status, 401);
      assert.equal((await fetch(origin + '/library', { headers: { origin: 'https://evil.example', authorization: `Bearer ${token}` } })).status, 403);
      if (!ffmpeg) {
        console.log('     (SCRIBE_TEST_FFMPEG_DIR not set: the real helper\'s whole-job part was skipped)');
      } else {
        // A whole job, offline: a downloader stand-in, real FFmpeg, the loopback Speech server, a real save folder.
        await stop();
        const video = 'realjob0001';
        fs.mkdirSync(path.join(root, '.venv', 'bin'), { recursive: true });
        fs.mkdirSync(path.join(root, 'metadata'), { recursive: true });
        fs.writeFileSync(path.join(root, '.venv', 'bin', 'yt-dlp'), `#!/bin/sh
set -eu
base="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
mode=stream; previous=; info=
for arg in "$@"; do
  if [ "$previous" = --load-info-json ]; then info=$arg; fi
  case "$arg" in --dump-single-json) mode=metadata;; --load-info-json) mode=download;; esac
  previous=$arg; last=$arg
done
if [ "$mode" = download ]; then cp "$base/source.wav" "$PWD/source.wav"; printf '%s\\n' "$PWD/source.wav"
elif [ "$mode" = metadata ]; then id=\${last##*=}; cat "$base/metadata/$id.json"; fi
`, { mode: 0o700 });
        const frames = 64000, wav = Buffer.alloc(44 + frames * 2);
        wav.write('RIFF', 0); wav.writeUInt32LE(36 + frames * 2, 4); wav.write('WAVEfmt ', 8); wav.writeUInt32LE(16, 16); wav.writeUInt16LE(1, 20); wav.writeUInt16LE(1, 22); wav.writeUInt32LE(16000, 24); wav.writeUInt32LE(32000, 28); wav.writeUInt16LE(2, 32); wav.writeUInt16LE(16, 34); wav.write('data', 36); wav.writeUInt32LE(frames * 2, 40);
        for (let i = 0; i < frames; i++) wav.writeInt16LE(Math.round(Math.sin(i * 440 * 2 * Math.PI / 16000) * 12000), 44 + i * 2);
        fs.writeFileSync(path.join(root, 'source.wav'), wav);
        fs.writeFileSync(path.join(root, 'metadata', `${video}.json`), JSON.stringify({ id: video, title: 'A real job', channel: 'Offline fixture', release_date: '20261022', duration: 4, description: 'Creator link https://example.com/?ref=1' }));
        await start(`AZURE_SPEECH_ENDPOINT=${speech.url}\nAZURE_SPEECH_KEY=not-a-real-key\nHELPER_TOKEN='${token}'\nFFMPEG_LOCATION=${ffmpeg}\nYTDLP_JS_RUNTIME=node:${process.execPath}\n`);
        const saveDir = path.join(root, 'saved'); fs.mkdirSync(saveDir);
        const dataFile = path.join(root, '.data', 'library.json');
        void dataFile;
        const started = JSON.parse(await worker(([v, s]) => wasm_bindgen.on_message(JSON.stringify({ action: 'startJob', url: `https://www.youtube.com/watch?v=${v}` }), JSON.stringify({ ...s, id: chrome.runtime.id })), [video, asTab]));
        assert.equal(started.ok, true, JSON.stringify(started));
        assert.ok(['queued', 'downloading', 'preparing', 'transcribing'].includes(started.data.status));
        const job = started.data.id;
        const done = await waitFor(async () => {
          const r = await (await fetch(`${origin}/jobs/${job}`, { headers: { authorization: `Bearer ${token}`, host: `127.0.0.1:${port}` } })).json();
          if (r.status === 'failed') throw new Error('the job failed: ' + r.error);
          return r.status === 'completed' ? r : null;
        }, 'the job to finish', 60000);
        assert.ok(speech.requests.length >= 1, 'Speech was asked, on loopback');
        assert.ok(speech.requests.every(r => r.key === 'not-a-real-key'), 'with the key from .env');
        assert.equal(done.document_available, true);
        const r = await open(ext(`reader.html?id=${job}`));
        await r.waitForSelector('#transcript p.ts', { timeout: 15000 });
        assert.match(await r.locator('#transcript').innerText(), /A real helper wrote these words/);
        assert.equal(await r.locator('#transcript .tx-t').first().textContent(), '0:00', 'timestamps from the real document');
        assert.equal(await r.locator('#video-title').textContent(), 'A real job');
        await r.close();
        // The status a card gets, from the real helper.
        const after = JSON.parse(await worker(([v, s]) => wasm_bindgen.on_message(JSON.stringify({ action: 'getVideoStatuses', videoIds: [v] }), JSON.stringify({ ...s, id: chrome.runtime.id })), [video, asTab]));
        assert.equal(after.data.statuses[video].status, 'completed');
      }
    } finally {
      await stop();
      speech.close();
      fs.rmSync(root, { recursive: true, force: true });
      await worker(o => { TR_HELPER = o; wasm_bindgen.worker_boot(o); self.__tr.token = 'x'; return TR.vault_del('pairingToken'); }, mock.origin);
      await worker(t => { self.__tr.token = t; }, TOKEN);
    }
  });

  await step('transcripts: the build keeps the fixed id, the key in the manifest and the palette in step', async () => {
    const m = JSON.parse(fs.readFileSync(path.join(__dirname, '..', 'extension', 'manifest.json'), 'utf8'));
    assert.equal(await worker(() => chrome.runtime.id), EXT_ID);
    assert.ok(m.key && m.permissions.includes('nativeMessaging') && m.permissions.includes('alarms'));
    assert.ok(m.host_permissions.includes('http://127.0.0.1:8765/*'));
    assert.equal(m.options_ui.page, 'settings.html');
    assert.equal(m.name, 'Redline');
    await disable();
    mock.reset();
    await worker(() => { self.__tr.icons.length = 0; });
  });
}

function freePort() {
  return new Promise(r => { const s = http.createServer(); s.listen(0, '127.0.0.1', () => { const { port } = s.address(); s.close(() => r(port)); }); });
}

/** The Speech endpoint the helper's pipeline posts to, on loopback, answering in Azure's shape for the audio it got. */
async function realSpeech() {
  const requests = [];
  const server = http.createServer((req, res) => {
    const chunks = [];
    req.on('data', c => chunks.push(c));
    req.on('end', () => {
      const body = Buffer.concat(chunks), at = body.indexOf('RIFF');
      const ms = at >= 0 ? body.readUInt32LE(at + 40) / 32 : 1000;
      requests.push({ url: req.url, key: req.headers['ocp-apim-subscription-key'], bytes: body.length });
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ durationMilliseconds: ms, combinedPhrases: [{ text: 'A real helper wrote these words.' }], phrases: [{ text: 'A real helper wrote these words.', offsetMilliseconds: 0, durationMilliseconds: ms }] }));
    });
  });
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  return { requests, url: `http://127.0.0.1:${server.address().port}`, close: () => { server.closeAllConnections(); server.close(); } };
}

/** YTC_TR_SPEED=1 (optionally YTC_TR_ON=1, YTC_EXT=<older build>): time to first card on the fixture Home, and what a
    hover costs: input to the next frame. Prints medians; compare the three runs. */
async function speed({ ctx, sw, mock, EXT }) {
  if (process.env.YTC_TR_ON) {
    mock.state.configured = true; mock.state.hasKey = true; mock.state.endpoint = 'https://r.cognitiveservices.azure.com';
    await sw.evaluate(() => chrome.storage.local.set({ ytcTranscriptsV1: { on: true } }));
  }
  const median = a => [...a].sort((x, y) => x - y)[Math.floor(a.length / 2)];
  const first = [], hover = [], wasm = [];
  const warm = await ctx.newPage();
  await warm.goto('https://www.youtube.com/', { waitUntil: 'commit' });
  await warm.waitForSelector('#ytc-list .c[data-v] .t');
  await warm.close();
  for (let i = 0; i < 15; i++) {
    const p = await ctx.newPage();
    await p.setViewportSize({ width: 1400, height: 900 });
    await p.addInitScript(() => {
      try { localStorage.setItem('ytc-test-noready', '1'); localStorage.setItem('ytc-test-nowatchdog', '1'); } catch {}
      new MutationObserver((_, o) => { if (document.querySelector('#ytc-list .c[data-v] .t')) { o.disconnect(); requestAnimationFrame(() => { window.__firstCard = performance.now(); }); } }).observe(document, { childList: true, subtree: true });
    });
    await p.goto('https://www.youtube.com/', { waitUntil: 'commit' });
    await p.waitForFunction(() => window.__firstCard);
    first.push(await p.evaluate(() => window.__firstCard));
    wasm.push(await p.evaluate(() => +document.documentElement.dataset.ytcWasm));
    await p.waitForTimeout(700);
    await p.evaluate(() => {
      window.__hover = [];
      addEventListener('pointerover', e => { if (e.target.closest && e.target.closest('.th')) { const t = performance.now(); requestAnimationFrame(() => requestAnimationFrame(() => window.__hover.push(performance.now() - t))); } }, { capture: true });
    });
    const links = await p.locator('#ytc-list .c[data-v] a.th').all();
    for (const l of links.slice(0, 8)) { await l.hover(); await p.waitForTimeout(60); }
    await p.mouse.move(2, 2);
    hover.push(median(await p.evaluate(() => window.__hover)));
    await p.close();
  }
  console.log(`speed ${EXT} ${process.env.YTC_TR_ON ? '(transcripts on)' : '(transcripts off)'}: first card ${median(first).toFixed(0)} ms, Wasm ready ${median(wasm).toFixed(0)} ms, hover to second frame ${median(hover).toFixed(1)} ms (15 runs, median)`);
}

module.exports = { prepare, steps, speed };
