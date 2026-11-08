// Background worker. Generic tooling only: no YouTube logic.
// Public build: the shared tab storage switch, the voice-search socket relay and the Transcripts service's loader below.
// Development build (./build.sh --dev; browser/build.cjs keeps the @dev blocks): also the
// auto-reload and the debug-center delivery.
'use strict';

// ---- Tab-shared memory (chrome.storage.session: never written to disk, cleared when the browser closes). Content
// scripts may use it only after this switch, which the browser keeps for its session; the worker sets it whenever it
// starts (the startup listener makes it start with the browser). The client keeps one thing there: the ready Home
// response (src/ready.rs; CONTRIBUTING "Privacy").
try { chrome.storage.session.setAccessLevel({ accessLevel: 'TRUSTED_AND_UNTRUSTED_CONTEXTS' }).catch(() => {}); } catch {}
chrome.runtime.onStartup.addListener(trStartup);

// ---- Transcripts (transcripts/, docs/architecture.md "Transcripts"). Its Wasm loads on the first message or alarm: an
// unconfigured browser never compiles it, never reaches the helper and never tries native messaging. The worker in Rust
// keeps the pairing token (IndexedDB in this origin); content scripts only send it messages.
// (Content scripts must keep reading chrome.storage.local: an older install under this extension id may have closed it.)
try { chrome.storage.local.setAccessLevel({ accessLevel: 'TRUSTED_AND_UNTRUSTED_CONTEXTS' }).catch(() => {}); } catch {}
const TR_HELPER = 'http://127.0.0.1:8765';
let trCore = null;
function tr() {
  if (trCore) return trCore;
  trCore = wasm_bindgen({ module_or_path: chrome.runtime.getURL('transcripts.wasm') }).then(() => { wasm_bindgen.worker_boot(TR_HELPER); return wasm_bindgen; });
  trAdopt();
  return trCore;
}
function trStartup() {
  chrome.storage.local.get('ytcTranscriptsV1').then(o => { if (o.ytcTranscriptsV1) tr().then(c => c.on_startup()); }).catch(() => {});
}
chrome.runtime.onMessage.addListener((m, sender, respond) => {
  if (!m || typeof m.action !== 'string') return false;
  tr()
    .then(c => c.on_message(JSON.stringify(m), JSON.stringify({ id: sender.id, url: sender.url, tab: sender.tab ? { id: sender.tab.id } : null, frameId: sender.frameId || 0 })))
    .then(text => respond(JSON.parse(text)), () => respond({ ok: false, error: 'Transcripts could not start. Reload the extension.' }));
  return true;
});
chrome.alarms.onAlarm.addListener(a => { if (a.name === 'ytc-transcripts-status') tr().then(c => c.on_alarm()).catch(() => {}); });
// A token an older install kept in chrome.storage.local moves out of content scripts' reach.
function trAdopt() {
  return chrome.storage.local.get('pairingToken').then(o => {
    if (o.pairingToken === undefined) return;
    chrome.storage.local.remove('pairingToken').catch(() => {});
    if (typeof o.pairingToken === 'string') return tr().then(c => c.adopt_token(o.pairingToken));
  }).catch(() => {});
}
// A leftover from the older install under this id: its alarm would wake this worker for nothing.
function trTidy() { return chrome.alarms.clear('full-transcript-status').catch(() => {}); }
chrome.runtime.onInstalled.addListener(() => { trAdopt(); trTidy(); });
// Every worker start, for someone who set it up: its memory of the toolbar slept with the last worker. Passive: this read
// never starts or pairs the helper.
trStartup();

/* @dev-start */
// ---- Auto-update (development builds). Open YouTube tabs ping this worker. When the files on
// disk change (browser/build.cjs rewrites build-id.txt on every dev build) the extension reloads
// itself; each open tab then notices its old script was invalidated and refreshes into the new
// build, waiting while a video plays (see browser/build.cjs).
const read = () => fetch(chrome.runtime.getURL('build-id.txt'), { cache: 'no-store' }).then(r => r.text()).then(t => t.trim()).catch(() => '');
// The build the browser loaded (baked into the manifest at build time). Reading
// build-id.txt at worker start would miss updates made while the worker slept.
const loadedId = (/\(([0-9a-f]{16})\)$/.exec(chrome.runtime.getManifest().version_name || '') || [])[1] || '';
const loaded = Promise.resolve(loadedId);

// ---- Debug center (development builds). The Rust app reports incidents (player fallbacks,
// InnerTube errors, panics, boot failures) and speed telemetry batches (`kind: "perf"`,
// schema 2) as JSON. Incidents are kept here (last 100), telemetry batches in their own queue
// (last 30, removed once delivered, so they never push an incident out), and both are delivered
// to the local collector (tools/debug-center, 127.0.0.1:47321), which appends them to its
// incidents file. chrome.storage.local `ytcCollectorV1` may name another collector URL (the e2e
// test points it at its own). The public build has none of this: it keeps and sends nothing.
const KEY = 'ytcIncidentsV1', MAX = 100, PERF_KEY = 'ytcPerfQueueV1', PERF_MAX = 30, URL_KEY = 'ytcCollectorV1', COLLECTOR = 'http://127.0.0.1:47321/incident';
const manifest = chrome.runtime.getManifest();
// Every report carries the build it came from (the debug center groups by it).
const BUILD = manifest.version_name || manifest.version;
const stampBuild = json => json.startsWith('{') && !json.startsWith('{"build":') ? '{"build":' + JSON.stringify(BUILD) + ',' + json.slice(1) : json;
// Storage writes run one at a time (two reports arriving together must both be kept).
let chain = Promise.resolve();
let flushing = false, again = false;
async function flush() {
  if (flushing) { again = true; return; }
  flushing = true; again = false;
  try {
    const got = await chrome.storage.local.get([KEY, PERF_KEY, URL_KEY]);
    const url = got[URL_KEY] || COLLECTOR, sent = new Set();
    outer: for (const k of [KEY, PERF_KEY]) for (const item of got[k] || []) {
      if (item.sent) continue;
      try {
        const r = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: item.json });
        if (!r.ok) break outer;
        sent.add(item.json);
      } catch { break outer; }
    }
    if (!sent.size) return;
    // Re-read in the write queue: reports stored meanwhile must stay.
    await (chain = chain.then(async () => {
      const now = await chrome.storage.local.get([KEY, PERF_KEY]);
      await chrome.storage.local.set({
        [KEY]: (now[KEY] || []).map(i => sent.has(i.json) ? { ...i, sent: true } : i),
        [PERF_KEY]: (now[PERF_KEY] || []).filter(i => !sent.has(i.json)),
      });
    }).catch(() => {}));
  } finally { flushing = false; if (again) flush(); }
}
function store(json, key, max) {
  chain = chain.then(async () => {
    const list = (await chrome.storage.local.get(key))[key] || [];
    list.push({ json: stampBuild(json.slice(0, 400000)), sent: false });
    await chrome.storage.local.set({ [key]: list.slice(-max) });
    flush();
  }).catch(() => {});
}

let lastFlush = 0;
chrome.runtime.onMessage.addListener((message, sender) => {
  if (message && typeof message.ytcIncident === 'string') { store(message.ytcIncident, KEY, MAX); return; }
  if (message && typeof message.ytcPerf === 'string') { store(message.ytcPerf, PERF_KEY, PERF_MAX); return; }
  if (message !== 'ytc-dev-check' || !sender.tab) return;
  Promise.all([loaded, read()]).then(([was, now]) => { if (was && now && was !== now) chrome.runtime.reload(); });
  if (Date.now() - lastFlush > 60000) { lastFlush = Date.now(); flush(); }
});
/* @dev-end */

// ---- Socket relay (voice search). A content script asks for a WebSocket over a
// chrome.runtime port: {t:'open', url, protocols, secret, probe}, then {t:'bin', d: base64} audio chunks and
// {t:'text', d} messages (queued until the socket opens). The stored credential named by `secret` is read here and
// appended to the subprotocols, so it never enters the web page; it may only go to its own host (SECRETS), over
// wss/https. When the handshake fails, `probe` is fetched with the credential to tell a rejected key (401/403) from
// a network failure. Back to the page: {t:'open'}, {t:'msg', d: text}, {t:'close', d: code}, {t:'fail', d: reason}.
// Nothing here is logged or reported. ytcSockTestV1 (e2e only): an http://127.0.0.1:port origin that stands in for
// the host, so the test suite never reaches the real service.
const SECRETS = { ytcVoiceKeyV1: { host: 'api.deepgram.com', scheme: 'Token' } }, SOCK_TEST = 'ytcSockTestV1';
const unb64 = s => { const b = atob(s), u = new Uint8Array(b.length); for (let i = 0; i < b.length; i++) u[i] = b.charCodeAt(i); return u; };
chrome.runtime.onConnect.addListener(port => {
  if (port.name !== 'ytc-sock' || !port.sender || !port.sender.tab) return;
  let ws = null, q = [], gone = false, asked = false;
  const post = (t, d) => { if (!gone) try { port.postMessage({ t, d }); } catch {} };
  const end = () => { if (!gone) { gone = true; q = []; try { port.disconnect(); } catch {} } };
  const fail = why => { post('fail', why); end(); };
  const send = x => { if (gone) return; if (ws && ws.readyState === 1) ws.send(x); else if ((!ws || ws.readyState === 0) && q.length < 2000) q.push(x); };
  port.onDisconnect.addListener(() => { gone = true; q = []; if (ws && ws.readyState < 2) ws.close(1000); });
  port.onMessage.addListener(async m => {
    if (!m || gone) return;
    if (m.t === 'bin') return send(unb64(String(m.d)));
    if (m.t === 'text') return send(String(m.d));
    if (m.t !== 'open' || asked) return;
    asked = true;
    const rule = Object.hasOwn(SECRETS, m.secret) ? SECRETS[m.secret] : null;
    let url, probe;
    try { url = new URL(m.url); probe = m.probe ? new URL(m.probe) : null; } catch { return fail('url'); }
    if (!rule || url.protocol !== 'wss:' || url.hostname !== rule.host || (probe && (probe.protocol !== 'https:' || probe.hostname !== rule.host))) return fail('url');
    const got = await chrome.storage.local.get([m.secret, SOCK_TEST]);
    const key = typeof got[m.secret] === 'string' ? got[m.secret] : '';
    if (gone) return;
    if (!key) return fail('nokey');
    if (typeof got[SOCK_TEST] === 'string') {
      const t = new URL(got[SOCK_TEST]);
      if (t.hostname === '127.0.0.1') { url.protocol = 'ws:'; url.host = t.host; if (probe) { probe.protocol = 'http:'; probe.host = t.host; } }
    }
    const protocols = Array.isArray(m.protocols) ? m.protocols.map(String) : [];
    let opened = false;
    try { ws = new WebSocket(url.href, [...protocols, key]); } catch { return fail('key'); } // not a valid subprotocol token
    ws.binaryType = 'arraybuffer';
    ws.onopen = () => { if (gone) { ws.close(1000); return; } opened = true; for (const x of q) ws.send(x); q = []; post('open', ''); };
    ws.onmessage = e => { if (typeof e.data === 'string') post('msg', e.data); };
    ws.onclose = async e => {
      if (opened) { post('close', String(e.code)); end(); return; }
      if (gone) return;
      if (!probe) return fail('net');
      try {
        const r = await fetch(probe.href, { headers: { authorization: rule.scheme + ' ' + key }, credentials: 'omit', cache: 'no-store' });
        fail(r.status === 401 || r.status === 403 ? 'key' : 'net');
      } catch { fail('net'); }
    };
  });
});
