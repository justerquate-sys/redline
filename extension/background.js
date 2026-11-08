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
chrome.runtime.onInstalled.addListener(() => { trAdopt(); trStartup(); });


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

// The thin host of the Transcripts Wasm (transcripts/): chrome.*, fetch, IndexedDB and the DOM as primitives, for the
// background worker and the extension's own pages (the reader, settings, the popup's Transcripts section). No decisions
// here. `wasm_bindgen` is the glue's export object, resolved at call time.
const TR = (() => {
  const on = (name, ...args) => { try { wasm_bindgen[name](...args); } catch (e) { console.error('[Redline transcripts]', name, e); } };
  const el = id => document.getElementById(id);
  // A key/value store in this extension's own origin. A content script runs in the page's origin, so it can never open
  // this database: the pairing token lives here, not in chrome.storage.local (which content scripts can read).
  let db = null;
  const vault = () => db || (db = new Promise((ok, no) => {
    const r = indexedDB.open('ytc-transcripts', 1);
    r.onupgradeneeded = () => r.result.createObjectStore('v');
    r.onsuccess = () => ok(r.result);
    r.onerror = () => no(r.error);
  }));
  const tx = (mode, f) => vault().then(d => new Promise((ok, no) => {
    const q = f(d.transaction('v', mode).objectStore('v'));
    q.onsuccess = () => ok(q.result);
    q.onerror = () => no(q.error);
  }));
  const timers = new Map();
  let lastHover = null, listening = false;
  return {
    http(url, method, headers, body, timeoutMs) {
      const c = new AbortController(), t = setTimeout(() => c.abort(), timeoutMs);
      return fetch(url, { method, headers: JSON.parse(headers), ...(body ? { body } : {}), signal: c.signal, cache: 'no-store', credentials: 'omit', redirect: 'error' })
        .then(r => r.text().then(text => JSON.stringify({ status: r.status, text })))
        .catch(e => JSON.stringify(c.signal.aborted || e.name === 'AbortError' ? { aborted: true } : { failed: true }))
        .finally(() => clearTimeout(t));
    },
    native(host, message) {
      return chrome.runtime.sendNativeMessage(host, JSON.parse(message)).then(v => JSON.stringify(v ?? null), e => JSON.stringify({ native_error: String((e && e.message) || '') }));
    },
    vault_get(key) { return tx('readonly', s => s.get(key)).then(v => (typeof v === 'string' ? v : ''), () => ''); },
    vault_set(key, value) { return tx('readwrite', s => s.put(value, key)).then(() => ''); },
    vault_del(key) { return tx('readwrite', s => s.delete(key)).then(() => ''); },
    local_get(key) { return chrome.storage.local.get(key).then(o => (o[key] === undefined ? '' : JSON.stringify(o[key]))); },
    local_set(key, json) { return chrome.storage.local.set({ [key]: JSON.parse(json) }).then(() => ''); },
    local_del(key) { return chrome.storage.local.remove(key).then(() => ''); },
    sleep(ms) { return new Promise(r => setTimeout(r, ms)); },
    epoch() { return Date.now(); },
    alarm_on(name, minutes) { chrome.alarms.get(name).then(a => { if (!a) chrome.alarms.create(name, { periodInMinutes: minutes }); }).catch(() => {}); },
    alarm_off(name) { chrome.alarms.clear(name).catch(() => {}); },
    icon(px16, px32) {
      const image = (px, size) => new ImageData(new Uint8ClampedArray(px), size, size);
      if (!px16.length) return chrome.action.setIcon({ path: chrome.runtime.getManifest().action.default_icon }).then(() => '');
      return chrome.action.setIcon({ imageData: { 16: image(px16, 16), 32: image(px32, 32) } }).then(() => '');
    },
    title(text) { return chrome.action.setTitle({ title: text || chrome.runtime.getManifest().action.default_title }).then(() => ''); },
    tabs_push(message) {
      chrome.tabs.query({ url: 'https://www.youtube.com/*' }).then(tabs => { for (const t of tabs) chrome.tabs.sendMessage(t.id, message).catch(() => {}); }).catch(() => {});
    },
    open_page(path) {
      const url = chrome.runtime.getURL(path), base = chrome.runtime.getURL(path.split('?')[0]);
      return chrome.runtime.getContexts({ contextTypes: ['TAB'] }).then(list => {
        const same = list.find(c => c.documentUrl === url) || list.find(c => (c.documentUrl || '').split('?')[0] === base);
        if (!same) return chrome.tabs.create({ url }).then(() => '');
        return chrome.tabs.update(same.tabId, { active: true, ...(same.documentUrl !== url ? { url } : {}) }).then(() => chrome.windows.update(same.windowId, { focused: true })).then(() => '');
      });
    },
    open_options() { return chrome.runtime.openOptionsPage().then(() => ''); },
    runtime_id() { return chrome.runtime.id; },
    runtime_base() { return chrome.runtime.getURL(''); },
    send(message) { return chrome.runtime.sendMessage(JSON.parse(message)).then(v => JSON.stringify(v ?? null), () => ''); },

    // ---- pages
    html(id, html) {
      const n = el(id);
      if (!n) return;
      const scroll = n.scrollTop, a = document.activeElement, keep = a && n.contains(a) ? [a.tagName, a.id, a.dataset.tx, a.dataset.arg] : null;
      n.innerHTML = html;
      if (keep) {
        for (const c of n.querySelectorAll('button,input,a,select')) if (c.tagName === keep[0] && c.id === keep[1] && c.dataset.tx === keep[2] && c.dataset.arg === keep[3]) { c.focus({ preventScroll: true }); break; }
      }
      n.scrollTop = scroll;
    },
    text(id, text) { const n = el(id); if (n && n.textContent !== text) n.textContent = text; },
    attr(id, name, value) {
      const n = el(id);
      if (!n) return;
      if (value === undefined || value === null) n.removeAttribute(name);
      else if (n.getAttribute(name) !== value) n.setAttribute(name, value);
    },
    body_attr(name, value) { document.body.setAttribute(name, value); },
    doc_title(text) { document.title = text; },
    set_value(id, value) { const n = el(id); if (n && n.value !== value) n.value = value; },
    set_checked(id, on) { const n = el(id); if (n) n.checked = on; },
    read_value(id) { const n = el(id); return n ? String(n.value) : ''; },
    read_checked(id) { const n = el(id); return !!(n && n.checked); },
    focus(selector) { const n = document.querySelector(selector); if (n) n.focus({ preventScroll: true }); },
    query() { return location.search; },
    hash() { return location.hash; },
    push_url(url) { history.pushState({}, '', url); },
    replace_url(url) { history.replaceState({}, '', url); },
    hidden() { return document.hidden; },
    clipboard(text) { return navigator.clipboard.writeText(text).then(() => ''); },
    clipboard_later(text) {
      return navigator.clipboard.write([new ClipboardItem({ 'text/plain': text.then(t => new Blob([t], { type: 'text/plain' })) })]).then(() => '');
    },
    local_time(iso) { const t = iso ? new Date(iso) : null; return t && !Number.isNaN(t.getTime()) ? t.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }) : ''; },
    number(n) { return Number(n).toLocaleString(); },
    manifest_version() { return chrome.runtime.getManifest().version; },
    in_box(list, selector, attr) {
      const box = el(list);
      if (!box) return '';
      const b = box.getBoundingClientRect();
      if (!b.width || !b.height) return '';
      const out = [];
      for (const c of box.querySelectorAll(selector)) { const r = c.getBoundingClientRect(); if (r.bottom > b.top && r.top < b.bottom) out.push(c.dataset[attr]); }
      return out.join('\u001e');
    },
    keep_in_box(id, selector) {
      const box = el(id), c = box && box.querySelector(selector);
      if (!c) return;
      const b = box.getBoundingClientRect(), r = c.getBoundingClientRect();
      if (r.bottom > b.bottom) box.scrollTop += r.bottom - b.bottom; else if (r.top < b.top) box.scrollTop += r.top - b.top;
    },
    later(name, ms) { clearTimeout(timers.get(name)); timers.set(name, setTimeout(() => on('on_event', 'timer', name, '', ''), ms)); },
    later_off(name) { clearTimeout(timers.get(name)); timers.delete(name); },
    // Delegated events: elements say what they mean with data-tx (click), data-tx-change, data-tx-input,
    // data-tx-submit, data-tx-hover and data-tx-scroll; data-arg carries the argument.
    listen() {
      if (listening) return;
      listening = true;
      const d = document;
      d.addEventListener('click', e => {
        const t = e.target.closest && e.target.closest('[data-tx]');
        if (!t || t.disabled || e.button || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
        e.preventDefault();
        on('on_event', 'click', t.dataset.tx, t.dataset.arg || '', '');
      });
      d.addEventListener('change', e => {
        const t = e.target;
        if (t.dataset && t.dataset.txChange) on('on_event', 'change', t.dataset.txChange, t.dataset.arg || '', t.type === 'checkbox' ? String(t.checked) : String(t.value));
      });
      d.addEventListener('input', e => {
        const t = e.target;
        if (t.dataset && t.dataset.txInput) on('on_event', 'input', t.dataset.txInput, t.dataset.arg || '', String(t.value));
      });
      d.addEventListener('submit', e => {
        const t = e.target;
        if (t.dataset && t.dataset.txSubmit) { e.preventDefault(); on('on_event', 'submit', t.dataset.txSubmit, '', ''); }
      });
      d.addEventListener('pointerover', e => {
        if (e.pointerType === 'touch') return;
        const t = e.target.closest && e.target.closest('[data-tx-hover]');
        if (t !== lastHover) { lastHover = t; if (t) on('on_event', 'hover', t.dataset.txHover, t.dataset.arg || '', ''); }
      });
      d.addEventListener('scroll', e => {
        const t = e.target;
        if (t.dataset && t.dataset.txScroll) on('on_event', 'scroll', t.dataset.txScroll, '', '');
      }, { passive: true, capture: true });
      d.addEventListener('keydown', e => { if (e.key === 'Escape') on('on_event', 'key', 'Escape', '', ''); });
      d.addEventListener('visibilitychange', () => on('on_event', 'visible', '', '', ''));
      addEventListener('popstate', () => on('on_event', 'popstate', '', '', ''));
    },
  };
})();

let wasm_bindgen = (function(exports) {
    let script_src;
    if (typeof document !== 'undefined' && document.currentScript !== null) {
        script_src = new URL(document.currentScript.src, location.href).toString();
    }

    /**
     * @param {string} token
     */
    function adopt_token(token) {
        const ptr0 = passStringToWasm0(token, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.adopt_token(ptr0, len0);
    }
    exports.adopt_token = adopt_token;

    function on_alarm() {
        wasm.on_alarm();
    }
    exports.on_alarm = on_alarm;

    /**
     * A DOM event on an element with a `data-tx*` attribute (`kind`: click, input, change, submit, hover, key, timer,
     * visible, popstate, scroll).
     * @param {string} kind
     * @param {string} name
     * @param {string} arg
     * @param {string} value
     */
    function on_event(kind, name, arg, value) {
        let ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3;
        try {
            ptr0 = passStringToWasm0(kind, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len0 = WASM_VECTOR_LEN;
            ptr1 = passStringToWasm0(name, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len1 = WASM_VECTOR_LEN;
            ptr2 = passStringToWasm0(arg, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len2 = WASM_VECTOR_LEN;
            ptr3 = passStringToWasm0(value, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len3 = WASM_VECTOR_LEN;
        } catch (e) {
            if (ptr3) { wasm.__wbindgen_free(ptr3, len3 * 1, 1); }
            if (ptr2) { wasm.__wbindgen_free(ptr2, len2 * 1, 1); }
            if (ptr1) { wasm.__wbindgen_free(ptr1, len1 * 1, 1); }
            if (ptr0) { wasm.__wbindgen_free(ptr0, len0 * 1, 1); }
            throw e;
        }
        wasm.on_event(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
    }
    exports.on_event = on_event;

    /**
     * @param {string} message
     * @param {string} sender
     * @returns {Promise<any>}
     */
    function on_message(message, sender) {
        let ptr0, len0, ptr1, len1;
        try {
            ptr0 = passStringToWasm0(message, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len0 = WASM_VECTOR_LEN;
            ptr1 = passStringToWasm0(sender, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            len1 = WASM_VECTOR_LEN;
        } catch (e) {
            if (ptr1) { wasm.__wbindgen_free(ptr1, len1 * 1, 1); }
            if (ptr0) { wasm.__wbindgen_free(ptr0, len0 * 1, 1); }
            throw e;
        }
        const ret = wasm.on_message(ptr0, len0, ptr1, len1);
        return ret;
    }
    exports.on_message = on_message;

    function on_startup() {
        wasm.on_startup();
    }
    exports.on_startup = on_startup;

    /**
     * An extension page's script is ready: `reader`, `settings` or `popup`.
     * @param {string} page
     */
    function page_boot(page) {
        const ptr0 = passStringToWasm0(page, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.page_boot(ptr0, len0);
    }
    exports.page_boot = page_boot;

    /**
     * @param {string} origin
     */
    function worker_boot(origin) {
        const ptr0 = passStringToWasm0(origin, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        wasm.worker_boot(ptr0, len0);
    }
    exports.worker_boot = worker_boot;
    function __wbg_get_imports() {
        const import0 = {
            __proto__: null,
            __wbg___wbindgen_is_function_741ff2bb612836f5: function(arg0) {
                const ret = typeof(arg0) === 'function';
                return ret;
            },
            __wbg___wbindgen_is_undefined_be5b34249ff1fc15: function(arg0) {
                const ret = arg0 === undefined;
                return ret;
            },
            __wbg___wbindgen_string_get_7ce8eee7fd327545: function(arg0, arg1) {
                const obj = arg1;
                const ret = typeof(obj) === 'string' ? obj : undefined;
                var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                var len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg___wbindgen_throw_4c88d8bf264fd984: function(arg0, arg1) {
                throw new Error(getStringFromWasm0(arg0, arg1));
            },
            __wbg__wbg_cb_unref_09736879a3f67559: function(arg0) {
                arg0._wbg_cb_unref();
            },
            __wbg_alarm_off_9f74cb51f5d6452b: function(arg0, arg1) {
                TR.alarm_off(getStringFromWasm0(arg0, arg1));
            },
            __wbg_alarm_on_cdd76db068abd0b7: function(arg0, arg1, arg2) {
                TR.alarm_on(getStringFromWasm0(arg0, arg1), arg2);
            },
            __wbg_attr_a9a6dc6573050497: function(arg0, arg1, arg2, arg3, arg4, arg5) {
                let v0;
                if (arg4 !== 0) {
                    v0 = getStringFromWasm0(arg4, arg5);
                    wasm.__wbindgen_free(arg4, arg5 * 1, 1);
                }
                TR.attr(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3), v0);
            },
            __wbg_body_attr_8075b82e1249b8df: function(arg0, arg1, arg2, arg3) {
                TR.body_attr(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
            },
            __wbg_call_a410eb4b59ed5965: function() { return handleError(function (arg0, arg1, arg2) {
                const ret = arg0.call(arg1, arg2);
                return ret;
            }, arguments); },
            __wbg_clipboard_e7db4231e58f9ecb: function(arg0, arg1) {
                const ret = TR.clipboard(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_clipboard_later_5b3c0e7c5dcfb34e: function(arg0) {
                const ret = TR.clipboard_later(arg0);
                return ret;
            },
            __wbg_doc_title_ebaa6fd1440b3cfe: function(arg0, arg1) {
                TR.doc_title(getStringFromWasm0(arg0, arg1));
            },
            __wbg_epoch_64e465725f4d7871: function() {
                const ret = TR.epoch();
                return ret;
            },
            __wbg_focus_a8ffe09661ce71ef: function(arg0, arg1) {
                TR.focus(getStringFromWasm0(arg0, arg1));
            },
            __wbg_hidden_97019787cf587076: function() {
                const ret = TR.hidden();
                return ret;
            },
            __wbg_html_76cff3f93dfa20d0: function(arg0, arg1, arg2, arg3) {
                TR.html(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
            },
            __wbg_http_1f502b1d3b64dc89: function(arg0, arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8) {
                const ret = TR.http(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3), getStringFromWasm0(arg4, arg5), getStringFromWasm0(arg6, arg7), arg8);
                return ret;
            },
            __wbg_icon_8385f27c533f5928: function(arg0, arg1, arg2, arg3) {
                const ret = TR.icon(getArrayU8FromWasm0(arg0, arg1), getArrayU8FromWasm0(arg2, arg3));
                return ret;
            },
            __wbg_in_box_25a675727a13dc1c: function(arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
                const ret = TR.in_box(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4), getStringFromWasm0(arg5, arg6));
                const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg_is_2248ff2b575d60c7: function(arg0, arg1) {
                const ret = Object.is(arg0, arg1);
                return ret;
            },
            __wbg_keep_in_box_c1e12c8a9555055c: function(arg0, arg1, arg2, arg3) {
                TR.keep_in_box(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
            },
            __wbg_later_0010804983891955: function(arg0, arg1, arg2) {
                TR.later(getStringFromWasm0(arg0, arg1), arg2);
            },
            __wbg_later_off_6588a7669910ea7e: function(arg0, arg1) {
                TR.later_off(getStringFromWasm0(arg0, arg1));
            },
            __wbg_local_del_9a86968f0c44f941: function(arg0, arg1) {
                const ret = TR.local_del(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_local_get_1dd1c3974a346ad8: function(arg0, arg1) {
                const ret = TR.local_get(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_local_set_a5b264c7f9840eaa: function(arg0, arg1, arg2, arg3) {
                const ret = TR.local_set(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
                return ret;
            },
            __wbg_local_time_757b85a2875c3db5: function(arg0, arg1, arg2) {
                const ret = TR.local_time(getStringFromWasm0(arg1, arg2));
                const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg_manifest_version_969624fedea9677d: function(arg0) {
                const ret = TR.manifest_version();
                const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg_native_75b72c0f35eee279: function(arg0, arg1, arg2, arg3) {
                const ret = TR.native(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
                return ret;
            },
            __wbg_new_typed_e09da9dae6ee5935: function(arg0, arg1) {
                try {
                    var state0 = {a: arg0, b: arg1};
                    var cb0 = (arg0, arg1) => {
                        const a = state0.a;
                        state0.a = 0;
                        try {
                            return wasm_bindgen_3d0bd8a515793b3f___convert__closures_____invoke___js_sys_366463b654380fd1___Function_fn_wasm_bindgen_3d0bd8a515793b3f___JsValue_____wasm_bindgen_3d0bd8a515793b3f___sys__Undefined___js_sys_366463b654380fd1___Function_fn_wasm_bindgen_3d0bd8a515793b3f___JsValue_____wasm_bindgen_3d0bd8a515793b3f___sys__Undefined_______true_(a, state0.b, arg0, arg1);
                        } finally {
                            state0.a = a;
                        }
                    };
                    const ret = new Promise(cb0);
                    return ret;
                } finally {
                    state0.a = 0;
                }
            },
            __wbg_number_e7452c5c889a49f3: function(arg0, arg1) {
                const ret = TR.number(arg1);
                const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg_open_options_4b9cd3851586da90: function() {
                const ret = TR.open_options();
                return ret;
            },
            __wbg_open_page_602ae88a9b823a2e: function(arg0, arg1) {
                const ret = TR.open_page(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_push_url_311f1768aa8e9e6e: function(arg0, arg1) {
                TR.push_url(getStringFromWasm0(arg0, arg1));
            },
            __wbg_query_12d804b8ff20ada4: function(arg0) {
                const ret = TR.query();
                const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg_queueMicrotask_1e8503a8a690f0a3: function(arg0) {
                const ret = arg0.queueMicrotask;
                return ret;
            },
            __wbg_queueMicrotask_4a1a4d8dd332373b: function(arg0) {
                queueMicrotask(arg0);
            },
            __wbg_read_checked_c92f78b5aaef4c60: function(arg0, arg1) {
                const ret = TR.read_checked(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_read_value_3819dc93a05b6024: function(arg0, arg1, arg2) {
                const ret = TR.read_value(getStringFromWasm0(arg1, arg2));
                const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg_replace_url_564893292df7c9b0: function(arg0, arg1) {
                TR.replace_url(getStringFromWasm0(arg0, arg1));
            },
            __wbg_resolve_dbf78ab4942478b0: function(arg0) {
                const ret = Promise.resolve(arg0);
                return ret;
            },
            __wbg_runtime_base_7f243f8eb250b13c: function(arg0) {
                const ret = TR.runtime_base();
                const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg_runtime_id_ea983941472f1d58: function(arg0) {
                const ret = TR.runtime_id();
                const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
                const len1 = WASM_VECTOR_LEN;
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 1, len1, true);
                getDataViewMemory0().setInt32((arg0 >>> 0) + 4 * 0, ptr1, true);
            },
            __wbg_send_07b9440bb3a17952: function(arg0, arg1) {
                const ret = TR.send(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_set_checked_dee93ee2cd65ba8b: function(arg0, arg1, arg2) {
                TR.set_checked(getStringFromWasm0(arg0, arg1), arg2 !== 0);
            },
            __wbg_set_value_ad37522f73a273a4: function(arg0, arg1, arg2, arg3) {
                TR.set_value(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
            },
            __wbg_sleep_34ee04141a14e28d: function(arg0) {
                const ret = TR.sleep(arg0);
                return ret;
            },
            __wbg_static_accessor_GLOBAL_THIS_003ee833af9699fc: function() {
                const ret = typeof globalThis === 'undefined' ? undefined : globalThis;
                return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
            },
            __wbg_static_accessor_GLOBAL_e4293fb22cbe77da: function() {
                const ret = typeof global === 'undefined' ? undefined : global;
                return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
            },
            __wbg_static_accessor_SELF_53bb20457401453d: function() {
                const ret = typeof self === 'undefined' ? undefined : self;
                return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
            },
            __wbg_static_accessor_WINDOW_b5e0810bfecb7feb: function() {
                const ret = typeof window === 'undefined' ? undefined : window;
                return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
            },
            __wbg_tabs_push_1124d98f68b494cb: function(arg0, arg1) {
                TR.tabs_push(getStringFromWasm0(arg0, arg1));
            },
            __wbg_text_5893926c1efff007: function(arg0, arg1, arg2, arg3) {
                TR.text(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
            },
            __wbg_then_656f253a5dd7d8ea: function(arg0, arg1, arg2) {
                const ret = arg0.then(arg1, arg2);
                return ret;
            },
            __wbg_then_e3f7f2d365708be8: function(arg0, arg1) {
                const ret = arg0.then(arg1);
                return ret;
            },
            __wbg_title_e873975073d62e69: function(arg0, arg1) {
                const ret = TR.title(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_vault_del_2e3dfbd50101aaae: function(arg0, arg1) {
                const ret = TR.vault_del(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_vault_get_84b109c360b0653f: function(arg0, arg1) {
                const ret = TR.vault_get(getStringFromWasm0(arg0, arg1));
                return ret;
            },
            __wbg_vault_set_a8996700aa36c74d: function(arg0, arg1, arg2, arg3) {
                const ret = TR.vault_set(getStringFromWasm0(arg0, arg1), getStringFromWasm0(arg2, arg3));
                return ret;
            },
            __wbindgen_generic_0000000000000001: function(arg0, arg1) {
                // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 197, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
                const ret = makeMutClosure(arg0, arg1, wasm_bindgen_3d0bd8a515793b3f___convert__closures_____invoke___wasm_bindgen_3d0bd8a515793b3f___JsValue__core_37ee0ce0c4cd57d5___result__Result_____wasm_bindgen_3d0bd8a515793b3f___JsError___true_);
                return ret;
            },
            __wbindgen_generic_0000000000000002: function(arg0, arg1) {
                // Cast intrinsic for `Ref(String) -> Externref`.
                const ret = getStringFromWasm0(arg0, arg1);
                return ret;
            },
            __wbindgen_init_externref_table: function() {
                const table = wasm.__wbindgen_externrefs;
                table.set(0, undefined);
                table.set(1024, undefined);
                table.set(1025, null);
                table.set(1026, true);
                table.set(1027, false);
            },
        };
        return {
            __proto__: null,
            "./transcripts_bg.js": import0,
        };
    }

    function wasm_bindgen_3d0bd8a515793b3f___convert__closures_____invoke___wasm_bindgen_3d0bd8a515793b3f___JsValue__core_37ee0ce0c4cd57d5___result__Result_____wasm_bindgen_3d0bd8a515793b3f___JsError___true_(arg0, arg1, arg2) {
        const ret = wasm.wasm_bindgen_3d0bd8a515793b3f___convert__closures_____invoke___wasm_bindgen_3d0bd8a515793b3f___JsValue__core_37ee0ce0c4cd57d5___result__Result_____wasm_bindgen_3d0bd8a515793b3f___JsError___true_(arg0, arg1, arg2);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }

    function wasm_bindgen_3d0bd8a515793b3f___convert__closures_____invoke___js_sys_366463b654380fd1___Function_fn_wasm_bindgen_3d0bd8a515793b3f___JsValue_____wasm_bindgen_3d0bd8a515793b3f___sys__Undefined___js_sys_366463b654380fd1___Function_fn_wasm_bindgen_3d0bd8a515793b3f___JsValue_____wasm_bindgen_3d0bd8a515793b3f___sys__Undefined_______true_(arg0, arg1, arg2, arg3) {
        wasm.wasm_bindgen_3d0bd8a515793b3f___convert__closures_____invoke___js_sys_366463b654380fd1___Function_fn_wasm_bindgen_3d0bd8a515793b3f___JsValue_____wasm_bindgen_3d0bd8a515793b3f___sys__Undefined___js_sys_366463b654380fd1___Function_fn_wasm_bindgen_3d0bd8a515793b3f___JsValue_____wasm_bindgen_3d0bd8a515793b3f___sys__Undefined_______true_(arg0, arg1, arg2, arg3);
    }

    function addToExternrefTable0(obj) {
        const idx = wasm.__externref_table_alloc();
        wasm.__wbindgen_externrefs.set(idx, obj);
        return idx;
    }

    const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
        ? { register: () => {}, unregister: () => {} }
        : new FinalizationRegistry(state => wasm.__wbindgen_destroy_closure(state.a, state.b));

    function getArrayU8FromWasm0(ptr, len) {
        ptr = ptr >>> 0;
        return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
    }

    let cachedDataViewMemory0 = null;
    function getDataViewMemory0() {
        if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
            cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
        }
        return cachedDataViewMemory0;
    }

    function getStringFromWasm0(ptr, len) {
        ptr = ptr >>> 0;
        return decodeText(getUint8ArrayMemory0().subarray(ptr, ptr + len));
    }

    let cachedUint8ArrayMemory0 = null;
    function getUint8ArrayMemory0() {
        if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
            cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
        }
        return cachedUint8ArrayMemory0;
    }

    function handleError(f, args) {
        try {
            return f.apply(this, args);
        } catch (e) {
            const idx = addToExternrefTable0(e);
            wasm.__wbindgen_exn_store(idx);
        }
    }

    function isLikeNone(x) {
        return x === undefined || x === null;
    }

    function makeMutClosure(arg0, arg1, f) {
        const state = { a: arg0, b: arg1, cnt: 1 };
        const real = (...args) => {

            // First up with a closure we increment the internal reference
            // count. This ensures that the Rust closure environment won't
            // be deallocated while we're invoking it.
            state.cnt++;
            const a = state.a;
            state.a = 0;
            try {
                return f(a, state.b, ...args);
            } finally {
                state.a = a;
                real._wbg_cb_unref();
            }
        };
        real._wbg_cb_unref = () => {
            if (--state.cnt === 0) {
                wasm.__wbindgen_destroy_closure(state.a, state.b);
                state.a = 0;
                CLOSURE_DTORS.unregister(state);
            }
        };
        CLOSURE_DTORS.register(real, state, state);
        return real;
    }

    function passStringToWasm0(arg, malloc, realloc) {
        if (realloc === undefined) {
            const buf = cachedTextEncoder.encode(arg);
            const ptr = malloc(buf.length, 1) >>> 0;
            getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
            WASM_VECTOR_LEN = buf.length;
            return ptr;
        }

        let len = arg.length;
        let ptr = malloc(len, 1) >>> 0;

        const mem = getUint8ArrayMemory0();

        let offset = 0;

        for (; offset < len; offset++) {
            const code = arg.charCodeAt(offset);
            if (code > 0x7F) break;
            mem[ptr + offset] = code;
        }
        if (offset !== len) {
            if (offset !== 0) {
                arg = arg.slice(offset);
            }
            ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
            const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
            const ret = cachedTextEncoder.encodeInto(arg, view);

            offset += ret.written;
            ptr = realloc(ptr, len, offset, 1) >>> 0;
        }

        WASM_VECTOR_LEN = offset;
        return ptr;
    }

    function takeFromExternrefTable0(idx) {
        const value = wasm.__wbindgen_externrefs.get(idx);
        wasm.__externref_table_dealloc(idx);
        return value;
    }

    let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true });
    cachedTextDecoder.decode();
    function decodeText(bytes) {
        return cachedTextDecoder.decode(bytes);
    }

    const cachedTextEncoder = new TextEncoder();

    if (!('encodeInto' in cachedTextEncoder)) {
        cachedTextEncoder.encodeInto = function (arg, view) {
            const buf = cachedTextEncoder.encode(arg);
            view.set(buf);
            return {
                read: arg.length,
                written: buf.length
            };
        };
    }

    let WASM_VECTOR_LEN = 0;

    let wasmModule, wasmInstance, wasm;
    function __wbg_finalize_init(instance, module) {
        wasmInstance = instance;
        wasm = instance.exports;
        wasmModule = module;
        cachedDataViewMemory0 = null;
        cachedUint8ArrayMemory0 = null;
        wasm.__wbindgen_start();
        return wasm;
    }

    async function __wbg_load(module, imports) {
        if (typeof Response === 'function' && module instanceof Response) {
            if (!module.ok) {
                throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
            }

            if (typeof WebAssembly.instantiateStreaming === 'function') {
                try {
                    return await WebAssembly.instantiateStreaming(module, imports);
                } catch (e) {
                    const validResponse = expectedResponseType(module.type);

                    if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                        console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                    } else { throw e; }
                }
            }

            const bytes = await module.arrayBuffer();
            return await WebAssembly.instantiate(bytes, imports);
        } else {
            const instance = await WebAssembly.instantiate(module, imports);

            if (instance instanceof WebAssembly.Instance) {
                return { instance, module };
            } else {
                return instance;
            }
        }

        function expectedResponseType(type) {
            switch (type) {
                case 'basic': case 'cors': case 'default': return true;
            }
            return false;
        }
    }

    function initSync(module) {
        if (wasm !== undefined) return wasm;


        if (module !== undefined) {
            if (Object.getPrototypeOf(module) === Object.prototype) {
                ({module} = module)
            } else {
                console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
            }
        }

        const imports = __wbg_get_imports();
        if (!(module instanceof WebAssembly.Module)) {
            module = new WebAssembly.Module(module);
        }
        const instance = new WebAssembly.Instance(module, imports);
        return __wbg_finalize_init(instance, module);
    }

    let __wbg_init_promise;
    async function __wbg_init(module_or_path) {
        if (wasm !== undefined) return wasm;
        if (__wbg_init_promise === undefined) {
            __wbg_init_promise = __wbg_init_once(module_or_path);
            __wbg_init_promise.catch(() => { __wbg_init_promise = undefined; });
        }
        return __wbg_init_promise;
    }

    async function __wbg_init_once(module_or_path) {

        if (module_or_path !== undefined) {
            if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
                ({module_or_path} = module_or_path)
            } else {
                console.warn('using deprecated parameters for the initialization function; pass a single object instead')
            }
        }

        if (module_or_path === undefined && script_src !== undefined) {
            module_or_path = script_src.replace(/\.js$/, "_bg.wasm");
        }
        const imports = __wbg_get_imports();

        if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
            module_or_path = fetch(module_or_path);
        }

        const { instance, module } = await __wbg_load(await module_or_path, imports);

        return __wbg_finalize_init(instance, module);
    }

    return Object.assign(__wbg_init, { initSync }, exports);
})({ __proto__: null });

