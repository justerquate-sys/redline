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
  let lastHover = null, listening = false, pending = null;
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
    pages_push() { try { chrome.runtime.sendMessage({ tscChanged: true }).catch(() => {}); } catch {} },
    // A marker in the browser's memory-only storage: gone when the browser closes, kept while the worker sleeps.
    session_first() {
      return chrome.storage.session.get('ytc:tr-session').then(o => {
        if (o['ytc:tr-session']) return '';
        return chrome.storage.session.set({ 'ytc:tr-session': '1' }).then(() => 'first');
      }).catch(() => '');
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
    segment(id, name, style) { const n = el(id), c = n && n.querySelector(`[data-segment="${name}"]`); if (c && c.getAttribute('style') !== style) c.setAttribute('style', style); },
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
    // A settings file: its values stay in this closure and never enter Wasm (a Deepgram key is among them). file_peek
    // answers with key names and kinds only; file_commit writes the keys Rust chose.
    pick(id) { const n = el(id); if (n) { n.value = ''; n.click(); } },
    file_peek(id, maxBytes) {
      const f = el(id) && el(id).files[0];
      if (!f) return Promise.resolve('{"error":"none"}');
      if (f.size > maxBytes) return Promise.resolve('{"error":"big"}');
      return f.text().then(text => {
        const v = JSON.parse(text);
        if (!v || typeof v !== 'object' || Array.isArray(v)) return '{"error":"shape"}';
        const items = v.redlineSettings === 1 ? v.items : v;
        if (!items || typeof items !== 'object' || Array.isArray(items)) return '{"error":"shape"}';
        pending = items;
        const kind = x => (x === null ? 'null' : Array.isArray(x) ? 'array' : typeof x);
        return JSON.stringify({ items: Object.keys(items).map(k => ({ k, t: kind(items[k]) })) });
      }).catch(() => '{"error":"shape"}');
    },
    // For each [key, field]: 'absent' (not in the file), 'new', 'same' or 'replace'. Never the values.
    file_conflicts(list) {
      const wanted = JSON.parse(list), items = pending || {};
      return chrome.storage.local.get(wanted.map(w => w[0])).then(now => JSON.stringify(wanted.map(([k, f]) => {
        const pick = o => (f ? (o && typeof o === 'object' ? o[f] : undefined) : o);
        const inc = pick(items[k]), old = pick(now[k]);
        if (inc === undefined || inc === null || inc === '') return 'absent';
        if (old === undefined || old === null || old === '') return 'new';
        return JSON.stringify(inc) === JSON.stringify(old) ? 'same' : 'replace';
      })));
    },
    // Writes `keys` from the file; `hold` ([key, field]) keeps what is here now instead.
    file_commit(keys, hold) {
      const items = pending, out = {}, held = JSON.parse(hold);
      pending = null;
      for (const k of JSON.parse(keys)) if (items && Object.hasOwn(items, k)) out[k] = items[k];
      return chrome.storage.local.get(held.map(h => h[0])).then(now => {
        for (const [k, f] of held) {
          if (!f) delete out[k];
          else if (out[k] && typeof out[k] === 'object') { if (now[k] && now[k][f] !== undefined) out[k][f] = now[k][f]; else delete out[k][f]; }
        }
        return chrome.storage.local.set(out);
      }).then(() => '');
    },
    file_forget() { pending = null; },
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
      // The worker: something changed elsewhere (a video queued from a YouTube tab).
      try { chrome.runtime.onMessage.addListener(m => { if (m && m.tscChanged) on('on_event', 'changed', '', '', ''); }); } catch {}
      d.addEventListener('click', e => {
        const t = e.target.closest && e.target.closest('[data-tx]');
        if (!t || !e.isTrusted || t.disabled || e.button || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
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
