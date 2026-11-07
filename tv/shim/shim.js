// Redline for TV page shim: what the browser extension's runtime gives the shared client, provided by the
// TV app's shell. The shell composes the document-start script as
//   (() => { const CFG = {...}; const CSS = {...}; const ytcClient = () => { <extension content.js> }; <this file> })();
// and installs it for https://www.youtube.com (tv/shell/src-tauri/src/script.rs).
//
// - where it runs: the extension's content_scripts matches/excludes (top frame only);
// - YTC_PLATFORM = 'tv' (the client's TV mode) and YTC_SHELL.call(cmd, json) → Promise<string>
//   (the shell's `ytc` command over Tauri's IPC: account, storage);
// - chrome.storage.local → the shell's persistent store; chrome.runtime → inert (dev reload and the voice
//   socket relay are off on TV), except that while the owner turned speed telemetry on (CFG.tel) the client's
//   reports (ytcPerf batches, ytcIncident reports) go to the shell, which delivers them to their debug center;
//   with it off they go nowhere, as in the extension's public build;
// - the content script's stylesheet (client.css, or native.css on ?yt-native=1 pages) as a
//   constructed sheet (YouTube's CSP never applies to it).
if (window.top === window && location.hostname === 'www.youtube.com' && !globalThis.YTC_PLATFORM
  && !/^\/(?:embed\/|live_chat|account|premium|paid_memberships|signin|logout|redirect|s\/|youtubei\/|api\/)/.test(location.pathname)) {
  const native = /[?&]yt-native=1(?:&|$)/.test(location.search);
  const call = (cmd, arg) => {
    const t = globalThis.__TAURI_INTERNALS__;
    if (!t || typeof t.invoke !== 'function') return Promise.reject(new Error('no shell'));
    return t.invoke('ytc', { cmd, json: typeof arg === 'string' ? arg : JSON.stringify(arg === undefined ? null : arg) });
  };
  Object.defineProperty(globalThis, 'YTC_PLATFORM', { value: 'tv' });
  Object.defineProperty(globalThis, 'YTC_SHELL', { value: Object.freeze({ call }) });
  const parsed = p => p.then(s => JSON.parse(s));
  const local = {
    get: (keys, cb) => { const p = parsed(call('store_get', keys === undefined ? null : keys)); if (cb) p.then(cb); return p; },
    set: (items, cb) => { const p = call('store_set', items).then(() => undefined); if (cb) p.then(cb); return p; },
    remove: (keys, cb) => { const p = call('store_remove', keys).then(() => undefined); if (cb) p.then(cb); return p; },
  };
  const manifest = Object.freeze({ manifest_version: 3, name: 'Redline', version: CFG.version, version_name: CFG.version_name, host_permissions: [] });
  const inert = { addListener() {}, removeListener() {} };
  const send = CFG.tel ? m => {
    if (m && typeof m === 'object') {
      if (typeof m.ytcPerf === 'string') call('tel_send', { perf: m.ytcPerf }).catch(() => {});
      else if (typeof m.ytcIncident === 'string') call('tel_send', { incident: m.ytcIncident }).catch(() => {});
    }
    return Promise.resolve();
  } : () => Promise.resolve();
  const runtime = {
    id: 'lumen-tv',
    sendMessage: send,
    connect: () => { throw new Error('not in the TV app'); },
    getManifest: () => manifest,
    getURL: p => p,
    onMessage: inert,
    onConnect: inert,
  };
  globalThis.chrome = Object.assign(globalThis.chrome || {}, { storage: { local }, runtime });
  try {
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(native ? CSS.native : CSS.client);
    document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  } catch (e) { console.error('[Redline] stylesheet', e); }
  // Text fields, remote first: the D-pad may land on a field without the on-screen keyboard popping up (it
  // would take the arrows); OK opens it (VirtualKeyboard API, policy "manual"). Once it was opened, Enter
  // (the keyboard's search key) submits as usual.
  const textual = el => !!el && (el.tagName === 'TEXTAREA' || (el.tagName === 'INPUT' && /^(?:text|search|email|url|tel|password|number)$/.test(el.type)));
  const kb = navigator.virtualKeyboard;
  addEventListener('focusin', e => { if (textual(e.target)) { e.target.setAttribute('virtualkeyboardpolicy', 'manual'); delete e.target.dataset.ytcKb; } }, true);
  // Leaving the field (a search went out, the remote moved on) puts the keyboard away.
  addEventListener('focusout', e => { if (textual(e.target) && kb && typeof kb.hide === 'function') kb.hide(); }, true);
  addEventListener('keydown', e => {
    const t = e.target;
    if (e.key !== 'Enter' || !textual(t) || t.dataset.ytcKb) return;
    t.dataset.ytcKb = '1';
    e.preventDefault(); e.stopImmediatePropagation();
    if (kb && typeof kb.show === 'function') kb.show(); else { t.removeAttribute('virtualkeyboardpolicy'); t.blur(); t.focus(); }
  }, true);
  ytcClient();
}
