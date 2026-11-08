// Generic browser bridge for the Rust app (bundled into content.js before the
// wasm-bindgen glue). It knows nothing about YouTube: DOM writes, fetch,
// storage, history, timers and event forwarding only. `wasm_bindgen` is the
// glue's export object, resolved lazily at call time.
const YTC = (() => {
  const d = document, perf = performance;
  const byId = id => d.getElementById(id);
  const gfetch = globalThis.fetch.bind(globalThis);
  const ctl = new Map();
  // Media objects (video elements, MediaSources, SourceBuffers) by numeric handle.
  const mo = new Map();
  // Elements made outside the document for Rust to place later (`media_grab`), by id.
  const held = new Map();
  const sx = {}; // last sideways scroll position per scroller id (wheel_x)
  const evt = (h, type) => call('on_media', h, type);
  const path = (o, p) => p.split('.').reduce((x, k) => (x == null ? x : x[k]), o);
  // An extension's content script: after the extension is reloaded or updated, the page goes on running this
  // old script, and chrome.runtime.id is gone (its calls throw "Extension context invalidated").
  const extId = () => { try { return chrome.runtime.id; } catch { return undefined; } };
  const extAtBoot = !!extId();
  let policy = null;
  // Write HTML; falls back to a Trusted Types policy if the page enforces it here.
  const setHTML = (el, html) => {
    if (policy) { el.innerHTML = policy.createHTML(html); return; }
    try { el.innerHTML = html; } catch (e) {
      if (!globalThis.trustedTypes) throw e;
      policy = trustedTypes.createPolicy('lumen', { createHTML: s => s });
      el.innerHTML = policy.createHTML(html);
    }
  };
  // `late` (set by back_hook, the app shells): a focus change the client itself made while Rust runs reports its
  // focus/intent callbacks right after that call returns (the app takes one call at a time; they would be lost).
  let depth = 0, late = false;
  const call = (name, ...args) => {
    if (depth && late && (name === 'on_focus' || name === 'on_intent')) { queueMicrotask(() => call(name, ...args)); return; }
    depth++;
    try { return wasm_bindgen[name](...args); } catch (e) { console.error('[Redline]', name, e); } finally { depth--; }
  };
  let io = null, lastHover = null, lastCls = null, hoverEl = null, clicked = null, shownWatch = null, docHeld = null, keyHeld = false;
  const insert = (el, html) => {
    if (policy) el.insertAdjacentHTML('beforeend', policy.createHTML(html));
    else try { el.insertAdjacentHTML('beforeend', html); } catch { setHTML(el, el.innerHTML + html); }
  };
  const hover = a => {
    hoverEl = a;
    const h = a ? a.getAttribute('href') : '', c = a ? a.className : '';
    if (h !== lastHover || c !== lastCls) { lastHover = h; lastCls = c; call('on_hover', h, c); }
  };
  // Hover zones: the closest [data-hov] under the pointer; on_hov(name, data-arg, buttons held) on every change.
  let zoneEl;
  const zone = (t, held) => {
    const z = (t && t.closest && t.closest('[data-hov]')) || null;
    if (z !== zoneEl) { zoneEl = z; call('on_hov', z ? z.dataset.hov : '', z ? z.dataset.arg || '' : '', held); }
  };
  const editable = t => !!t && (t.isContentEditable || t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.tagName === 'SELECT');
  const listen = () => {
    // Errors in this content script (bridge/glue) become debug-center incidents.
    const report = (kind, reason) => { try { chrome.runtime.sendMessage({ ytcIncident: JSON.stringify({ kind, reason: String(reason).slice(0, 4000), at: Date.now(), path: location.pathname + location.search, ua: navigator.userAgent }) }).catch(() => {}); } catch {} };
    addEventListener('error', e => report('js-error', (e.error && e.error.stack) || e.message));
    addEventListener('unhandledrejection', e => report('js-rejection', (e.reason && e.reason.stack) || e.reason));
    d.addEventListener('click', e => {
      if (e.defaultPrevented || e.button !== 0) return;
      const el = e.target.closest && e.target.closest('a[href],[data-act]');
      if (!el) return;
      clicked = el; // click_rect: the source of a shared-element morph
      // 32: keyboard activation (Enter/Space on a focused control: click with detail 0 and no pointer position).
      // 64: and that key is being held (an auto-repeat activated it, not a press).
      const kbd = e.detail === 0 && !e.clientX && !e.clientY;
      const mods = (e.ctrlKey ? 1 : 0) | (e.metaKey ? 2 : 0) | (e.shiftKey ? 4 : 0) | (e.altKey ? 8 : 0) | (el.target === '_blank' ? 16 : 0) | (kbd ? 32 : 0) | (kbd && keyHeld ? 64 : 0);
      if (call('on_click', el.getAttribute('href') || '', el.dataset.act || '', el.dataset.arg || '', mods)) e.preventDefault();
    });
    d.addEventListener('submit', e => {
      const f = e.target, input = f.querySelector('input[name]');
      if (call('on_submit', f.id || '', input ? input.value : '') !== false) e.preventDefault();
      if (input) input.blur();
    });
    d.addEventListener('pointerover', e => { hover(e.target.closest && e.target.closest('a[href]')); zone(e.target, e.buttons !== 0); }, { passive: true });
    d.documentElement.addEventListener('mouseleave', () => { hover(null); zone(null, false); }, { passive: true });
    // After a held gesture ends, report the zone under the pointer again (held travel is not hover intent).
    d.addEventListener('pointerup', () => { if (zoneEl !== undefined) call('on_hov', zoneEl ? zoneEl.dataset.hov : '', zoneEl ? zoneEl.dataset.arg || '' : '', false); }, { passive: true });
    // [data-focus] elements report focus changes: on_focus(name, arg, focused).
    const focusEvt = on => e => { const el = e.target.closest && e.target.closest('[data-focus]'); if (el) call('on_focus', el.dataset.focus, el.dataset.arg || '', on); };
    d.addEventListener('focusin', focusEvt(true));
    d.addEventListener('focusout', focusEvt(false));
    // Pressing inside [data-keep-focus] does not move focus (dropdown rows keep the search box focused).
    d.addEventListener('mousedown', e => { if (e.target.closest && e.target.closest('[data-keep-focus]')) e.preventDefault(); });
    // Presses report the `data-pop` they are inside ('' = outside every popup).
    d.addEventListener('pointerdown', e => {
      const p = e.target.closest && e.target.closest('[data-pop]'); call('on_press', p ? p.dataset.pop : '');
      const a = e.button === 0 && e.target.closest && e.target.closest('a[href]'); if (a) call('on_intent', a.getAttribute('href'));
    }, { passive: true, capture: true });
    // Keyboard focus on a link is intent too (prefetch before Enter).
    // A microtask later: focus moved by Rust itself (the TV remote's spatial_focus) fires focusin inside
    // that Rust call, where a nested call would be dropped.
    // A held key (a remote's arrow held down) passes links by: only where it rests is intent, on the key's release.
    d.addEventListener('keydown', e => { keyHeld = e.repeat; }, { capture: true, passive: true });
    d.addEventListener('keyup', () => {
      if (!keyHeld) return;
      keyHeld = false;
      const a = d.activeElement && d.activeElement.closest && d.activeElement.closest('a[href]');
      if (a) call('on_intent', a.getAttribute('href'));
    }, { capture: true, passive: true });
    d.addEventListener('focusin', e => { const a = !keyHeld && e.target.closest && e.target.closest('a[href]'); if (a) { const h = a.getAttribute('href'); queueMicrotask(() => call('on_intent', h)); } });
    d.addEventListener('visibilitychange', () => { call(d.visibilityState === 'hidden' ? 'on_hide' : 'on_show'); });
    addEventListener('focus', () => call('on_window_focus'));
    addEventListener('blur', () => call('on_window_blur'));
    d.addEventListener('keydown', e => {
      if (e.altKey) return;
      // Ctrl/⌘ combinations outside text fields are offered as `Mod+<key>`; unhandled ones stay the browser's.
      if (e.ctrlKey || e.metaKey) { if (!editable(e.target) && call('on_key', 'Mod+' + e.key, false)) e.preventDefault(); return; }
      // Enter/Space stay native activation unless this control's region also accepts text keys.
      // An unhandled key still follows its native activation path.
      if ((e.key === 'Enter' || e.key === ' ') && e.target.closest && e.target.closest('button,a[href]') && !e.target.closest('[data-key-input]')) return;
      if (call('on_key', e.key === 'Tab' && e.shiftKey ? 'Shift+Tab' : e.key, editable(e.target))) e.preventDefault();
    });
    addEventListener('popstate', () => call('on_pop'));
    addEventListener('scrollend', () => call('on_scroll_end', scrollY), { passive: true });
    try { chrome.storage.session.onChanged.addListener(ch => { for (const k in ch) call('on_shared_change', k, typeof ch[k].newValue === 'string' ? ch[k].newValue : ''); }); } catch {}
    addEventListener('pagehide', () => call('on_unload'));
    d.addEventListener('fullscreenchange', () => evt(0, 'fullscreenchange'));
    d.addEventListener('dblclick', e => {
      const el = e.target.closest && e.target.closest('[data-dbl]');
      if (el && call('on_click', '', el.dataset.dbl, '', 0)) e.preventDefault();
    });
    // Drag on [data-drag]: on_drag(name, fraction 0..1 across the element, phase 0 down / 1 move / 2 up).
    d.addEventListener('pointerdown', e => {
      const el = e.button === 0 && e.target.closest && e.target.closest('[data-drag]');
      if (!el) return;
      e.preventDefault();
      // Fractions are across `data-drag-box` (an element id) when given, else the element itself.
      const name = el.dataset.drag, box = (el.dataset.dragBox && byId(el.dataset.dragBox)) || el;
      const f = (v, a, n) => n ? Math.min(1, Math.max(0, (v - a) / n)) : 0;
      const send = (ev, ph) => { const r = box.getBoundingClientRect(); call('on_drag', name, f(ev.clientX, r.left, r.width), f(ev.clientY, r.top, r.height), ph); };
      try { el.setPointerCapture(e.pointerId); } catch {}
      send(e, 0);
      const move = ev => send(ev, 1);
      const up = ev => { el.removeEventListener('pointermove', move); el.removeEventListener('pointerup', up); el.removeEventListener('pointercancel', up); send(ev, 2); };
      el.addEventListener('pointermove', move);
      el.addEventListener('pointerup', up);
      el.addEventListener('pointercancel', up);
    });
    // Pointer tracking on [data-track]: on_track(name, fraction 0..1 across it; -1 = left). The rect is
    // read once when the pointer enters (no layout read per move). Held drags keep reporting (capture).
    let trEl = null, trL = 0, trW = 0;
    const track = (el, x) => {
      if (el !== trEl) {
        if (trEl) call('on_track', trEl.dataset.track, -1);
        trEl = el;
        if (el) { const r = el.getBoundingClientRect(); trL = r.left; trW = r.width; }
      }
      if (el && trW) call('on_track', el.dataset.track, Math.min(1, Math.max(0, (x - trL) / trW)));
    };
    d.addEventListener('pointermove', e => track(e.target.closest ? e.target.closest('[data-track]') : null, e.clientX), { passive: true });
    d.documentElement.addEventListener('mouseleave', () => track(null, 0), { passive: true });
    d.addEventListener('input', e => { const n = e.target.dataset && e.target.dataset.input; if (n) call('on_input', n, String(e.target.value), e.inputType || ''); });
    let moved = 0;
    d.addEventListener('pointermove', () => { const t = perf.now(); if (t - moved > 150) { moved = t; call('on_move'); } }, { passive: true });
    io = new IntersectionObserver(es => {
      for (const x of es) if (x.isIntersecting) call('on_visible', x.target.id);
    }, { rootMargin: '1500px 0px' });
  };
  return {
    now: () => perf.now(),
    epoch: () => Date.now(),
    href: () => location.pathname + location.search + location.hash,
    cookie: () => d.cookie,
    // The key being handled is an auto-repeat of one held down.
    key_held: () => keyHeld,
    lang: () => { try { const t = localStorage.getItem('ytc-test-lang'); if (t) return t; } catch {} return navigator.language || 'en'; },
    doc(html) {
      const root = d.documentElement;
      for (const a of [...root.attributes]) if (!a.name.startsWith('data-ytc')) root.removeAttribute(a.name);
      root.lang = (navigator.language || 'en').split('-')[0];
      setHTML(root, html);
      try { history.scrollRestoration = 'manual'; } catch {}
      listen();
    },
    // The page's own document, still streaming in, kept out of the page (the loader does not stop it; `page_stop`
    // does). Every node the HTML parser inserts at the top of the document is detached at once, before anything of it
    // renders, and watched where it lies; a detached script never runs (the parser runs a script only while it is in
    // the document). `doc_hold(prefix, keep)`: once a <script> whose text starts with `prefix` is complete (the parser
    // has inserted something after it), on_doc_script(text): `<script>…</script>` for every earlier script whose text
    // contains `keep`, then that one; only the earlier ones when the document ended without it. Call after `doc` (its
    // nodes are ours).
    doc_hold(prefix, keep) {
      const root = d.documentElement, tops = [], checked = [], kept = [];
      let done = false;
      const complete = (s, i) => { if (i < tops.length - 1) return true; for (let n = s; n && n !== tops[i]; n = n.parentNode) if (n.nextSibling) return true; return false; };
      const scan = all => {
        for (let i = 0; i < tops.length && !done; i++) {
          const list = tops[i].getElementsByTagName('script');
          for (; checked[i] < list.length && !done; checked[i]++) {
            const sc = list[checked[i]];
            if (!all && !complete(sc, i)) break;
            const t = sc.textContent;
            if (t.startsWith(prefix)) { done = true; sub.disconnect(); root.dataset.ytcDocData = perf.now().toFixed(1); call('on_doc_script', kept.join('') + '<script>' + t + '</script>'); }
            else if (t.includes(keep)) kept.push('<script>' + t + '</script>');
          }
        }
      };
      const sub = new MutationObserver(() => scan(false));
      const top = new MutationObserver(rs => {
        for (const r of rs) for (const n of r.addedNodes) {
          if (n.parentNode !== root) continue;
          n.remove();
          if (n.nodeType !== 1 || done) continue;
          tops.push(n); checked.push(0);
          sub.observe(n, { childList: true, subtree: true });
        }
        scan(false);
      });
      top.observe(root, { childList: true });
      const end = () => { scan(true); top.disconnect(); sub.disconnect(); if (!done) { done = true; call('on_doc_script', kept.join('')); } docHeld = null; };
      docHeld = { top, sub, end };
      d.addEventListener('DOMContentLoaded', () => { if (docHeld) end(); }, { once: true });
    },
    // Stops the page's own document (its download and parsing); a `doc_hold` ends without answering.
    page_stop() { if (docHeld) { docHeld.top.disconnect(); docHeld.sub.disconnect(); docHeld = null; } try { window.stop(); } catch {} },
    html(id, html) { const el = byId(id); if (el) setHTML(el, html); },
    append(id, html) { const el = byId(id); if (el) insert(el, html); },
    // Appends into the closest `sel` ancestor of the element under the pointer.
    hover_insert(sel, html) { const el = hoverEl && hoverEl.isConnected && hoverEl.closest(sel); if (el) insert(el, html); },
    remove_all(sel) { for (const el of d.querySelectorAll(sel)) el.remove(); },
    append_all(sel, html) { for (const el of d.querySelectorAll(sel)) el.insertAdjacentHTML('beforeend', html); },
    class_all(sel, cls, on) { for (const el of d.querySelectorAll(sel)) el.classList.toggle(cls, !!on); },
    get_html: id => { const el = byId(id); return el ? el.innerHTML : ''; },
    attr(id, name, value) { const el = byId(id); if (el) el.setAttribute(name, value); },
    focus_id() { return d.activeElement?.id || ''; },
    toggle: (id, cls) => { const el = byId(id); return el ? el.classList.toggle(cls) : false; },
    value(id, v) { const el = byId(id); if (el) el.value = v; },
    // Focuses a control inside `id`: delta 0 = the selected (.on) one or the first; ±1 = next/previous (wrapping).
    focus_in(id, delta) {
      const el = byId(id); if (!el) return;
      const xs = [...el.querySelectorAll('button:not([disabled]),input')]; if (!xs.length) return;
      const i = xs.indexOf(d.activeElement);
      const x = delta === 0 || i < 0 ? (el.querySelector('button.on') || xs[0]) : xs[(i + delta + xs.length) % xs.length];
      x.focus();
    },
    // Roving focus over `sel` items inside `id` (cards): delta ±1 or ±1000 (one
    // row) from the focused item, or the first item in view when none is focused.
    focus_grid(id, sel, delta) {
      const el = byId(id); if (!el) return false;
      const xs = el.querySelectorAll(sel); if (!xs.length) return false;
      const cur = d.activeElement && d.activeElement.closest && d.activeElement.closest(sel);
      let i = cur ? [...xs].indexOf(cur) : -1;
      if (i < 0) { i = 0; for (let k = 0; k < xs.length; k++) if (xs[k].getBoundingClientRect().bottom > 64) { i = k; break; } }
      else {
        // ±1000 = one row: as many items as share the first item's top edge.
        if (Math.abs(delta) === 1000) { const t = xs[0].getBoundingClientRect().top; let n = 1; while (n < xs.length && Math.abs(xs[n].getBoundingClientRect().top - t) < 2) n++; delta = Math.sign(delta) * n; }
        i = Math.min(xs.length - 1, Math.max(0, i + delta));
      }
      xs[i].focus({ preventScroll: true }); xs[i].scrollIntoView({ block: 'nearest' });
      return true;
    },
    focus(id) { const el = byId(id); if (el) { el.focus(); if (el.select) el.select(); } },
    // hrefs of the `link` descendants of the `sel` elements inside `id` that intersect the viewport (document
    // order, at most `max`). Only the `sel` boxes are measured: measuring inside a content-visibility:auto box
    // lays that box out on its own, one forced layout per box.
    visible_hrefs(id, sel, link, max) {
      const el = byId(id); if (!el) return '';
      const out = [], h = innerHeight;
      for (const a of el.querySelectorAll(sel)) {
        const r = a.getBoundingClientRect();
        if (r.bottom > 0 && r.top < h && r.width) { const l = a.querySelector(link); if (l) { out.push(l.getAttribute('href') || ''); if (out.length >= max) break; } }
        else if (out.length && r.top >= h) break;
      }
      return out.join('\n');
    },
    visible: () => d.visibilityState === 'visible',
    // Scrolls the box `id` (position:relative) so its first `sel` child is centered; the window stays.
    // A sideways scroller: a plain mouse wheel (vertical) scrolls it horizontally,
    // and a re-rendered one returns to where it was scrolled (never reads layout
    // unless it was scrolled: a forced layout during a scroll restore moves the page).
    wheel_x(id) {
      const el = byId(id); if (!el || el.dataset.wx) return; el.dataset.wx = '1';
      if (sx[id]) el.scrollLeft = sx[id];
      el.addEventListener('scroll', () => { sx[id] = el.scrollLeft; }, { passive: true });
      el.addEventListener('wheel', e => { if (Math.abs(e.deltaY) > Math.abs(e.deltaX) && el.scrollWidth > el.clientWidth) { el.scrollLeft += e.deltaY; e.preventDefault(); } }, { passive: false });
    },
    // Scrolls the window so `id` is centered (instant: a scroll that answers a click never animates).
    into_view(id) { const el = byId(id); if (el) el.scrollIntoView({ block: 'center' }); },
    // Focuses the first `sel` inside `id` without scrolling.
    focus_sel(id, sel) { const el = byId(id), x = el && el.querySelector(sel); if (x) x.focus({ preventScroll: true }); },
    scroll_in(id, sel) { const el = byId(id), x = el && el.querySelector(sel); if (x) el.scrollTop = x.offsetTop - (el.clientHeight - x.offsetHeight) / 2; },
    // True when some element matches `sel` (a state check; the selector comes from Rust).
    any: sel => !!d.querySelector(sel),
    // ---- pages left to the site itself (native mode): generic page primitives. Selectors, attribute
    // names and every decision come from Rust; these only read, mark, click and forward events.
    // An attribute (null removes it) / a style property on <html>.
    root_attr(name, v) { const r = d.documentElement; if (v == null) r.removeAttribute(name); else if (r.getAttribute(name) !== v) r.setAttribute(name, v); },
    root_var(name, v) { d.documentElement.style.setProperty(name, v); },
    // on_native_dom() at most once per frame while the document changes (`attrs`: watched attribute names, comma-separated).
    page_watch(attrs) {
      let queued = false;
      const run = () => { queued = false; call('on_native_dom'); };
      new MutationObserver(() => { if (!queued) { queued = true; requestAnimationFrame(run); } })
        .observe(d.documentElement, { childList: true, subtree: true, attributes: true, attributeFilter: attrs.split(',') });
      run();
    },
    // Attribute `name` of the first `sel` match (undefined: no match or no attribute).
    page_attr(sel, name) { const el = d.querySelector(sel); const v = el && el.getAttribute(name); return v == null ? undefined : v; },
    // Whitespace-collapsed text of each `sel` match (at most 64, 160 chars each), joined by U+001E.
    page_texts(sel) { return [...d.querySelectorAll(sel)].slice(0, 64).map(el => (el.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 160)).join('\u001e'); },
    // Sets attribute `name` on the i-th `sel` match.
    page_mark(sel, i, name) { const el = d.querySelectorAll(sel)[i]; if (el && !el.hasAttribute(name)) el.setAttribute(name, ''); },
    // Clicks the first rendered `inner` match inside the i-th `sel` match ('' = that match itself).
    page_click(sel, i, inner) {
      const el = d.querySelectorAll(sel)[i]; if (!el) return false;
      const seen = x => (x.checkVisibility ? x.checkVisibility() : !!x.offsetParent) && !x.disabled && x.getAttribute('aria-disabled') !== 'true';
      const t = inner ? [...el.querySelectorAll(inner)].find(seen) : (seen(el) ? el : null);
      if (!t) return false; t.click(); return true;
    },
    // Inserts `html` into the first `parent` match unless it already has a `.cls` child. Pointer and
    // click events inside it stay there (the site never sees a press on it).
    page_mount(parent, cls, html) {
      const p = d.querySelector(parent); if (!p || p.querySelector(':scope > .' + cls)) return;
      insert(p, html);
      const w = p.querySelector(':scope > .' + cls);
      if (w) for (const t of ['pointerdown', 'mousedown', 'mouseup', 'click', 'dblclick', 'wheel', 'keydown']) w.addEventListener(t, e => e.stopPropagation());
    },
    // Sets the value of every `sel` input (not one being dragged right now).
    page_value(sel, v) { for (const el of d.querySelectorAll(sel)) if (el.value !== v && !el.matches(':active')) el.value = v; },
    // Capture-phase guards, decided in Rust:
    //  keydown → on_native_key(key, code, flags) true = swallow; wheel → on_native_wheel(dy, flags) 1 = stop, 2 = stop + no scroll;
    //  a press on `drag` inside `frame` → on_native_drag(fx, fy, ex, ey, phase 0/1/2) (pointer and element
    //  corner as fractions of the frame box; the site never sees the press, nor the click after it);
    //  input/change on [data-nin] → on_native_input(name, value, committed); a user's click on `intent` → on_native_intent().
    // Flags: 1 ctrl/meta/alt, 2 composing, 4 editable target, 8 inside `skip` (keys: `key_skip`), 16 inside
    // `scope` or on the page itself, 32 fullscreen around the target, 64 on `drag`.
    page_guards(scope, key_skip, wheel_skip, drag, frame, intent) {
      const flags = (e, skip) => {
        const t = e.target && e.target.nodeType === 1 ? e.target : d.documentElement, a = d.activeElement;
        const page = t === d.body || t === d.documentElement;
        return (e.ctrlKey || e.metaKey || e.altKey ? 1 : 0) | (e.isComposing ? 2 : 0) | (editable(t) || editable(a) ? 4 : 0)
          | (t.closest(skip) || (a && a !== d.body && a.closest && a.closest(skip)) ? 8 : 0) | (page || t.closest(scope) ? 16 : 0)
          | (d.fullscreenElement && d.fullscreenElement.contains(t) ? 32 : 0) | (t.closest(drag) ? 64 : 0);
      };
      addEventListener('keydown', e => {
        if (e.defaultPrevented) return;
        if (call('on_native_key', e.key || '', e.code || '', flags(e, key_skip))) { e.preventDefault(); e.stopImmediatePropagation(); }
      }, true);
      // The decision is taken by a passive window listener (page scrolling stays off the main thread);
      // only `scope` boxes, hooked when the pointer first enters them, can then cancel a scroll.
      let wheelEv = null, wheelR = 0;
      addEventListener('wheel', e => {
        wheelEv = e; wheelR = e.defaultPrevented ? 0 : call('on_native_wheel', e.deltaY, flags(e, wheel_skip)) || 0;
        if (wheelR === 1) e.stopImmediatePropagation();
      }, { capture: true, passive: true });
      const hooked = new WeakSet();
      addEventListener('pointerover', e => {
        const s = e.target.closest && e.target.closest(scope);
        if (!s || hooked.has(s)) return;
        hooked.add(s);
        s.addEventListener('wheel', w => { if (w === wheelEv && wheelR === 2) { w.preventDefault(); w.stopPropagation(); } }, { capture: true, passive: false });
      }, true);
      let held = null, quiet = 0;
      const at = (e, ph) => {
        const f = held.f.getBoundingClientRect(), b = held.el.getBoundingClientRect();
        if (!f.width || !f.height) return;
        call('on_native_drag', (e.clientX - f.left) / f.width, (e.clientY - f.top) / f.height, (b.left - f.left) / f.width, (b.top - f.top) / f.height, ph);
      };
      addEventListener('pointerdown', e => {
        const el = e.button === 0 && e.isPrimary && e.target.closest && e.target.closest(drag), f = el && el.closest(frame);
        if (!f) return;
        e.preventDefault(); e.stopImmediatePropagation();
        held = { el, f, id: e.pointerId }; at(e, 0);
      }, true);
      addEventListener('pointermove', e => { if (held && e.pointerId === held.id) { e.stopImmediatePropagation(); at(e, 1); } }, true);
      const up = e => { if (!held || e.pointerId !== held.id) return; at(e, 2); held = null; quiet = perf.now() + 400; };
      addEventListener('pointerup', up, true); addEventListener('pointercancel', up, true);
      addEventListener('mousedown', e => { if (e.target.closest && e.target.closest(drag) && e.target.closest(frame)) { e.preventDefault(); e.stopImmediatePropagation(); } }, true);
      addEventListener('click', e => {
        if (e.target.closest && e.target.closest(drag) && (held || perf.now() < quiet)) { e.preventDefault(); e.stopImmediatePropagation(); return; }
        if (intent && e.isTrusted && e.target.closest && e.target.closest(intent)) call('on_native_intent');
      }, true);
      for (const [t, c] of [['input', false], ['change', true]]) d.addEventListener(t, e => {
        const n = e.target && e.target.dataset && e.target.dataset.nin; if (n) call('on_native_input', n, String(e.target.value), c);
      }, true);
    },
    // ---- feed and chip motion (the motion law: the DOM is final before these run; they only animate
    // paint, never read layout outside a click, and nothing waits for them).
    // Entrance wave over the first `max` `sel` matches inside `id` (their `sub` parts when given):
    // opacity 0 → 1 rising `dy` px, `step` ms apart (at most 7 steps). No layout read; clicks work at once.
    stagger(id, sel, sub, max, step, dy, easing, ms) {
      const el = byId(id); if (!el) return;
      let i = 0;
      for (const c of el.querySelectorAll(sel)) {
        if (i >= max) break;
        const from = { opacity: 0, transform: dy ? `translateY(${dy}px)` : 'none' }, o = { duration: ms, delay: Math.min(i, 7) * step, easing, fill: 'backwards' };
        for (const t of sub ? c.querySelectorAll(sub) : [c]) t.animate([from, { opacity: 1, transform: 'none' }], o);
        i++;
      }
    },
    // A persistent element's new text arrives: opacity 0 → 1 from `dy` px below.
    nudge(id, dy, easing, ms) { const el = byId(id); if (el) el.animate([{ opacity: 0, transform: `translateY(${dy}px)` }, { opacity: 1, transform: 'none' }], { duration: ms, easing }); },
    // Selection pill ("duplicate, clip, slide"): a copy of `scope`'s items in their selected look (class
    // `cls` styles it) lies over them, clipped to one item's rounded box; the clip slides and morphs from
    // item `from` (or from where a running slide is painted now) to item `to`. Meanwhile the scope carries
    // `data-pill="1"` (CSS: the real selected item keeps its plain look, colors switch without fades), so
    // the text inverts exactly along the pill's edge. One layout read, at the click; the real items are
    // final and clickable throughout (the copy is inert).
    // An empty `to` only stops a running slide (a keyboard selection is instant).
    pill_slide(scope, from, to, cls, easing, ms) {
      const s = byId(scope), b = to ? byId(to) : null, prev = YTC._pills.get(scope);
      let f = null;
      if (prev && b && prev.a.playState === 'running') { const p = prev.a.effect.getComputedTiming().progress || 0; f = prev.f.map((v, i) => v + (prev.t[i] - v) * p); }
      if (prev) { YTC._pills.delete(scope); prev.a.cancel(); prev.g.remove(); }
      if (!s) return;
      if (!b) return YTC._pill_settle(s, null);
      if (!f) { const a = from && byId(from); if (!a || a === b) return YTC._pill_settle(s, b); f = YTC._pill_box(s, a); }
      const t = YTC._pill_box(s, b);
      if (!t[2] || !f[2] || f.every((v, i) => Math.abs(v - t[i]) < 0.5)) return YTC._pill_settle(s, b);
      YTC._pill_run(s, scope, b, f, t, getComputedStyle(b).borderTopLeftRadius, cls, easing, ms, 0);
    },
    // `scope` was re-rendered while its pill slides (a feed answered mid-slide): the slide goes on over
    // the new items from where it is. Nothing to do (no read) when no slide runs.
    pill_carry(scope) {
      const st = YTC._pills.get(scope), s = byId(scope);
      if (!st || !s || st.g.isConnected) return;
      YTC._pills.delete(scope);
      if (st.a.playState !== 'running') return;
      const time = st.a.currentTime, b = byId(st.to) || st.b; st.a.cancel();
      YTC._pill_run(s, scope, b, st.f, st.t, st.r, st.cls, st.easing, st.ms, time);
    },
    // Reflow FLIP for a list re-rendered by one change (an item removed, moved or hidden): `flip_mark`
    // records where the `sel` items in `scope` are painted (keyed by data-k, else their text) and keeps
    // them; after the swap `flip_play` slides each surviving item from its old place and a copy of each
    // vanished one shrinks out where it was. One read before the swap, one after; nothing waits.
    flip_mark(scope, sel) { const s = byId(scope); YTC._flips.set(scope + ' ' + sel, s ? [...s.querySelectorAll(sel)].map(el => ({ k: el.dataset.k || el.textContent, r: el.getBoundingClientRect(), el })) : []); },
    flip_play(scope, sel, easing, ms, out) {
      const key = scope + ' ' + sel, old = YTC._flips.get(key), s = byId(scope); YTC._flips.delete(key);
      if (!old || !s) return;
      const now = new Map([...s.querySelectorAll(sel)].map(el => [el.dataset.k || el.textContent, el]));
      for (const o of old) {
        const el = now.get(o.k);
        if (el) {
          const r = el.getBoundingClientRect(), dx = o.r.left - r.left, dy = o.r.top - r.top;
          if (r.width && (Math.abs(dx) > 0.5 || Math.abs(dy) > 0.5)) el.animate([{ transform: `translate(${dx}px,${dy}px)` }, { transform: 'none' }], { duration: ms, easing });
        } else if (o.r.width && o.r.bottom > 0 && o.r.top < innerHeight) {
          const g = YTC._inert_copy(o.el);
          g.setAttribute('aria-hidden', 'true'); g.inert = true; g.dataset.ghost = '1';
          g.style.cssText += `;position:fixed;z-index:16;left:${o.r.left}px;top:${o.r.top}px;width:${o.r.width}px;height:${o.r.height}px;margin:0;box-sizing:border-box;pointer-events:none`;
          s.append(g);
          g.animate([{ opacity: 1, transform: 'none' }, { opacity: 0, transform: matchMedia('(prefers-reduced-motion: reduce)').matches ? 'none' : 'scale(.85)' }], { duration: out, easing, fill: 'forwards' }).onfinish = () => g.remove();
        }
      }
    },
    // (pill/flip internals, not called from Rust)
    _pills: new Map(), _flips: new Map(),
    // [x, y, w, h] of `el` in `s`'s scrolled content box (where an absolute child of `s` paints).
    _pill_box(s, el) { const a = s.getBoundingClientRect(), r = el.getBoundingClientRect(); return [r.left - a.left + s.scrollLeft - s.clientLeft, r.top - a.top + s.scrollTop - s.clientTop, r.width, r.height]; },
    _pill_run(s, scope, b, f, t, r, cls, easing, ms, time) {
      const g = d.createElement('div');
      g.className = cls; g.setAttribute('aria-hidden', 'true'); g.inert = true; g.dataset.ghost = '1';
      const W = s.scrollWidth, H = s.scrollHeight;
      g.style.cssText = `position:absolute;left:0;top:0;width:${W}px;height:${H}px;margin:0;padding:0;pointer-events:none`;
      for (const c of s.children) {
        if (c.dataset.ghost) continue;
        const x = YTC._pill_box(s, c); if (!x[2]) continue;
        const k = YTC._inert_copy(c);
        k.style.cssText += `;position:absolute;left:${x[0]}px;top:${x[1]}px;width:${x[2]}px;height:${x[3]}px;margin:0;box-sizing:border-box`;
        g.append(k);
      }
      s.append(g);
      s.dataset.pill = '1';
      const clip = q => `inset(${q[1]}px ${W - q[0] - q[2]}px ${H - q[1] - q[3]}px ${q[0]}px round ${r})`;
      const a = g.animate([{ clipPath: clip(f) }, { clipPath: clip(t) }], { duration: ms, easing, fill: 'both' });
      if (time) a.currentTime = time;
      const st = { a, g, f, t, r, b, to: b.id, cls, easing, ms };
      YTC._pills.set(scope, st);
      a.onfinish = () => { g.remove(); if (YTC._pills.get(scope) === st) { YTC._pills.delete(scope); YTC._pill_settle(s, b); } };
    },
    // A look-alike copy of `el` without behaviour: no ids (an item's id becomes data-of), no data-act/arg/hov/focus.
    _inert_copy(el) {
      const k = el.cloneNode(true);
      for (const e of [k, ...k.querySelectorAll('*')]) {
        if (e.id) { e.dataset.of = e.id; e.removeAttribute('id'); }
        for (const n of ['data-act', 'data-arg', 'data-hov', 'data-focus', 'data-input', 'name', 'for']) e.removeAttribute(n);
      }
      return k;
    },
    // The slide ended: the real selected item takes over in the same frame, without a color fade.
    _pill_settle(s, b) {
      if (!s.dataset.pill) return;
      s.dataset.pill = '0';
      if (b && b.isConnected) getComputedStyle(b).backgroundColor; // style flush: the plain → selected switch happens with fades off
      delete s.dataset.pill;
    },
    // ---- motion (the motion law: everything is final before these run; they only animate paint)
    // Viewport rect [x, y, w, h] of `inner` in the closest `scope` ancestor of the element the last
    // click landed on; [] when there is none or it is off-screen. One layout read, before a swap.
    click_rect(scope, inner) {
      const s = clicked && clicked.isConnected && clicked.closest(scope), t = s && (s.matches(inner) ? s : s.querySelector(inner));
      if (!t) return new Float64Array(0);
      const r = t.getBoundingClientRect();
      return r.width && r.bottom > 0 && r.right > 0 && r.top < innerHeight && r.left < innerWidth ? new Float64Array([r.left, r.top, r.width, r.height]) : new Float64Array(0);
    },
    // Viewport rect [x, y, w, h] of `id` as painted now (a running morph included); [] if missing.
    rect(id) { const el = byId(id); if (!el) return new Float64Array(0); const r = el.getBoundingClientRect(); return new Float64Array([r.left, r.top, r.width, r.height]); },
    // FLIP: `id` is already at its final place; paint it from the rect (x, y, w, h) back to there with a
    // compositor-only transform. A running morph is replaced (the caller measured its on-screen rect).
    // The element carries `data-flip` while it moves (CSS lifts it; its class list may be rewritten meanwhile).
    flip_from(id, x, y, w, h, easing, ms) {
      const el = byId(id); if (!el || !w || !h) return;
      for (const a of el.getAnimations()) if (a.id === 'flip') a.cancel();
      const r = el.getBoundingClientRect(); if (!r.width || !r.height) return;
      const from = `translate(${x - r.left}px,${y - r.top}px) scale(${w / r.width},${h / r.height})`;
      if (from === 'translate(0px,0px) scale(1,1)') return;
      el.dataset.flip = '1';
      const a = el.animate([{ transformOrigin: '0 0', transform: from }, { transformOrigin: '0 0', transform: 'none' }], { duration: ms, easing });
      a.id = 'flip';
      a.onfinish = a.oncancel = () => { if (!el.getAnimations().some(b => b.id === 'flip' && b !== a)) delete el.dataset.flip; };
    },
    // Opacity 0 → 1 on `id` (content just written; never on content that was already visible).
    fade_in(id, easing, ms) { const el = byId(id); if (el) el.animate([{ opacity: 0 }, { opacity: 1 }], { duration: ms, easing }); },
    reduced_motion: () => matchMedia('(prefers-reduced-motion: reduce)').matches,
    matches: q => { try { return matchMedia(q).matches; } catch { return false; } },
    // Calls on_timer(id) once the next frame has rendered (styles of content written now are computed).
    frame(id) { requestAnimationFrame(() => setTimeout(() => call('on_timer', id), 0)); },
    title(t) { d.title = t; },
    push(url) { history.pushState(null, '', url); },
    history_back() { history.back(); },
    replace(url) { history.replaceState(null, '', url); },
    scroll_y: () => scrollY,
    scroll_to(y) { scrollTo(0, y); },
    fetch(id, method, url, body, headers) { YTC.fetch_opts(id, method, url, body, headers, '{}'); },
    // `opts` (JSON) is merged into the fetch init, e.g. {"credentials":"omit"} or {"keepalive":true}.
    // `progress: 1` in `opts` also reports on_fetch_progress(id, status, bytes so far): once when the
    // response headers arrive (0 bytes), then at most every 100 ms while the body streams in.
    // `chunks: 1` hands a successful (2xx) body over as it arrives instead: on_fetch_chunk(id, status, bytes)
    // per piece the network delivered, then on_fetch(id, status, <empty>) at its end.
    // `gzip: 1` sends the body gzip-compressed (CompressionStream, with Content-Encoding: gzip; plain where the
    // browser has no CompressionStream). A redirect
    // not followed (`redirect: 'manual'`) answers with status 302 and an empty body.
    fetch_opts(id, method, url, body, headers, opts) {
      const c = new AbortController();
      ctl.set(id, c);
      const o = JSON.parse(opts), prog = o.progress, chunks = o.chunks, gz = o.gzip; delete o.progress; delete o.chunks; delete o.gzip;
      const init = { method, headers: JSON.parse(headers), credentials: 'same-origin', signal: c.signal, ...o };
      const done = (status, b) => { ctl.delete(id); call('on_fetch', id, status, b); };
      const go = b => { if (b) init.body = b; return gfetch(url, init); };
      const zip = gz && body && globalThis.CompressionStream;
      if (zip) init.headers['Content-Encoding'] = 'gzip';
      (zip ? new Response(new Blob([body]).stream().pipeThrough(new CompressionStream('gzip'))).arrayBuffer().then(go) : go(body))
        .then(r => {
          if (r.type === 'opaqueredirect') return done(302, new Uint8Array(0));
          const stream = chunks && r.ok;
          // Test switch only (e2e): hand the body over in pieces of this many bytes (boxes split anywhere).
          let split = Infinity; if (stream) try { split = +localStorage.getItem('ytc-test-chunk') || Infinity; } catch {}
          if ((!prog && !stream) || !r.body) return r.arrayBuffer().then(b => done(r.status, new Uint8Array(b)));
          if (prog) call('on_fetch_progress', id, r.status, 0);
          const rd = r.body.getReader(), parts = [];
          let n = 0, last = perf.now();
          const next = () => rd.read().then(({ done: end, value }) => {
            if (!ctl.has(id)) return;
            if (end) {
              if (stream) return done(r.status, new Uint8Array(0));
              const b = new Uint8Array(n); let at = 0;
              for (const p of parts) { b.set(p, at); at += p.length; }
              return done(r.status, b);
            }
            n += value.length;
            if (stream) { for (let i = 0; i < value.length; i += split) call('on_fetch_chunk', id, r.status, value.subarray(i, i + split)); }
            else parts.push(value);
            if (!prog) return next();
            const t = perf.now();
            if (t - last >= 100) { last = t; call('on_fetch_progress', id, r.status, n); }
            return next();
          });
          return next();
        })
        .catch(e => { ctl.delete(id); if (!e || e.name !== 'AbortError') call('on_fetch', id, 0, new Uint8Array(0)); });
    },
    abort(id) { const c = ctl.get(id); if (c) { ctl.delete(id); c.abort(); } },
    storage_get(key) {
      const done = v => call('on_storage', key, JSON.stringify(v && v[key] !== undefined ? v[key] : null));
      try { chrome.storage.local.get(key).then(done, () => done(null)); } catch { queueMicrotask(() => done(null)); }
    },
    // Saves everything in chrome.storage.local (without the `skip` keys, a JSON list) as a file. The values stay here:
    // a secret among them never enters Wasm. on_store_result(id, ok) when the file is handed to the browser.
    storage_export(id, name, skip) {
      const done = ok => call('on_store_result', id, ok);
      try {
        chrome.storage.local.get(null).then(all => {
          for (const k of JSON.parse(skip)) delete all[k];
          const url = URL.createObjectURL(new Blob([JSON.stringify({ redlineSettings: 1, exportedAt: new Date().toISOString(), items: all }, null, 1)], { type: 'application/json' }));
          const a = d.createElement('a');
          a.href = url; a.download = name; d.body.append(a); a.click(); a.remove();
          setTimeout(() => URL.revokeObjectURL(url), 60000);
          done(true);
        }, () => done(false));
      } catch { queueMicrotask(() => done(false)); }
    },
    open_tab(path) { try { chrome.tabs.create({ url: chrome.runtime.getURL(path) }).catch(() => {}); } catch {} },
    storage_set(key, json) { try { chrome.storage.local.set({ [key]: JSON.parse(json) }); } catch {} },
    extension_page() { return /^(chrome|moz)-extension:$/.test(location.protocol); },
    storage_watch(key) {
      if (YTC._sw) return;
      try { chrome.storage.onChanged.addListener((ch, area) => { if (area === 'local' && ch[key]) call('on_storage_change', key, JSON.stringify(ch[key].newValue ?? null)); }); YTC._sw = true; } catch {}
    },
    storage_write(id, key, json) {
      try { chrome.storage.local.set({ [key]: JSON.parse(json) }).then(() => call('on_store_result', id, true), () => call('on_store_result', id, false)); }
      catch { queueMicrotask(() => call('on_store_result', id, false)); }
    },
    message_listen() {
      try { chrome.runtime.onMessage.addListener((message, sender, reply) => { if (typeof message === 'string' && sender.id === chrome.runtime.id) reply(call('on_extension_message', message)); }); } catch {}
    },
    active_message(id, message) {
      try { chrome.tabs.query({ active: true, currentWindow: true }).then(tabs => tabs[0]?.id == null ? '' : chrome.tabs.sendMessage(tabs[0].id, message)).then(value => call('on_message_result', id, typeof value === 'string' ? value : ''), () => call('on_message_result', id, '')); }
      catch { queueMicrotask(() => call('on_message_result', id, '')); }
    },
    timer(id, ms) { setTimeout(() => call('on_timer', id), ms); },
    idle(id, ms) { (globalThis.requestIdleCallback || (f => setTimeout(f, 1)))(() => call('on_timer', id), { timeout: ms }); },
    // Tab-lifetime storage (sessionStorage); never chrome.storage or localStorage.
    session_get(key) { try { return sessionStorage.getItem(key) || ''; } catch { return ''; } },
    session_set(key, v) { try { sessionStorage.setItem(key, v); return true; } catch { return false; } },
    session_del(key) { try { sessionStorage.removeItem(key); } catch {} },
    // The extension's memory-only storage shared by its tabs (chrome.storage.session: never on disk, gone when the
    // browser closes; the background worker opens it to content scripts). Values are strings; reads answer in
    // on_shared(key, value) ('' when missing); changes made by any tab arrive in on_shared_change(key, value).
    shared_ok() { try { return !!(chrome.storage && chrome.storage.session); } catch { return false; } },
    shared_get(key) {
      const done = v => call('on_shared', key, typeof v === 'string' ? v : '');
      try { chrome.storage.session.get(key).then(o => done(o && o[key]), () => done('')); } catch { done(''); }
    },
    shared_take(key) {
      const done = v => call('on_shared', key, typeof v === 'string' ? v : '');
      try { chrome.storage.session.get(key).then(o => { const v = o && o[key]; if (v !== undefined) chrome.storage.session.remove(key).catch(() => {}); done(v); }, () => done('')); } catch { done(''); }
    },
    shared_set(key, v) { try { chrome.storage.session.set({ [key]: v }).catch(() => {}); } catch {} },
    shared_del(key) { try { chrome.storage.session.remove(key).catch(() => {}); } catch {} },
    // Elements in view (an IntersectionObserver; the box comes with its entry, no layout read of ours): each `sel`
    // inside #list with data-<attr> calls on_shown(value, index, x, y, w, h) once when ≥ `ratio` of it is in the
    // viewport (index: its place among those elements, in document order, as first seen).
    shown_watch(list, sel, attr, ratio) {
      YTC.shown_stop();
      const seen = new WeakMap();
      let n = 0;
      const ob = new IntersectionObserver(es => {
        for (const x of es) {
          if (!x.isIntersecting || x.intersectionRatio < ratio) continue;
          ob.unobserve(x.target);
          const r = x.boundingClientRect;
          call('on_shown', x.target.getAttribute('data-' + attr) || '', seen.get(x.target) || 0, r.left, r.top, r.width, r.height);
        }
      }, { threshold: [ratio] });
      shownWatch = { ob, scan: () => { const l = byId(list); if (l) for (const el of l.querySelectorAll(sel)) if (!seen.has(el)) { seen.set(el, n++); ob.observe(el); } } };
      shownWatch.scan();
    },
    shown_scan() { if (shownWatch) shownWatch.scan(); },
    shown_stop() { if (shownWatch) { shownWatch.ob.disconnect(); shownWatch = null; } },
    nav_type() { try { return performance.getEntriesByType('navigation')[0].type; } catch { return 'navigate'; } },
    // When this document's response started arriving (performance.now ms; 0 when unknown).
    nav_response_start() { try { return performance.getEntriesByType('navigation')[0].responseStart || 0; } catch { return 0; } },
    width: () => innerWidth,
    // The window as a web app describes it to its server: size, pixel ratio, dark preference, Web Share.
    viewport: () => JSON.stringify({ w: innerWidth, h: innerHeight, dpr: devicePixelRatio || 1, dark: matchMedia('(prefers-color-scheme: dark)').matches, share: typeof navigator.share === 'function' }),
    observe(id) { const el = byId(id); if (el && io) { io.unobserve(el); io.observe(el); } },
    post(id, msg, origin) { const f = byId(id); if (f && f.contentWindow) f.contentWindow.postMessage(msg, origin); },
    mark(name) { d.documentElement.dataset[name] = perf.now().toFixed(1); },
    mark_val(name, v) { d.documentElement.dataset[name] = v; },
    go(url) { location.assign(url); },
    replace_doc(url) { location.replace(url); },
    ua: () => navigator.userAgent,
    ext_gone() { return extAtBoot && !extId(); },
    has_text(sel) { return [...d.querySelectorAll(sel)].some(e => !!(e.value ?? e.textContent ?? '').trim()); },
    local_flag(key) { try { return localStorage.getItem(key) === '1'; } catch { return false; } },
    local_flag_set(key, on) { try { if (on) localStorage.setItem(key, '1'); else localStorage.removeItem(key); } catch {} },
    // ---- app shell (the TV app, tv/). An app shell sets YTC_PLATFORM ('tv') and YTC_SHELL before this
    // script runs (tv/shim/shim.js); localStorage 'ytc:platform' is a development switch for testing in a browser,
    // and 'ytc:shell' (JSON {cmd: answer string, or null = the call fails}) stands in for the shell there; with
    // 'ytc:shell-log' set to '[]' every call made to the stand-in is appended to it ([cmd, json]).
    platform() { try { return String(globalThis.YTC_PLATFORM || localStorage.getItem('ytc:platform') || ''); } catch { return ''; } },
    // YTC_SHELL.call(cmd, json) → Promise<string>; answered by on_shell(seq, ok, json).
    shell(seq, cmd, json) {
      let s = globalThis.YTC_SHELL;
      if (!s) try {
        const t = JSON.parse(localStorage.getItem('ytc:shell') || 'null');
        if (t) s = { call: (c, j) => {
          const l = localStorage.getItem('ytc:shell-log');
          if (l) localStorage.setItem('ytc:shell-log', JSON.stringify([...JSON.parse(l), [c, j]].slice(-200)));
          return typeof t[c] === 'string' ? Promise.resolve(t[c]) : Promise.reject(new Error(String(t.error || 'no shell')));
        } };
      } catch {}
      if (!s || typeof s.call !== 'function') { setTimeout(() => call('on_shell', seq, false, '"no shell"'), 0); return; }
      Promise.resolve().then(() => s.call(cmd, json)).then(r => call('on_shell', seq, true, String(r == null ? 'null' : r)), e => call('on_shell', seq, false, JSON.stringify(String((e && e.message) || e))));
    },
    // The platform Back button: the shell calls YTC_BACK() → true = the page handled it.
    // on_back answers 0 = not handled (the shell goes back), 1 = handled, 2 = leave the app: YTC_EXIT tells a
    // shell that knows it (older shells read only the boolean and go back as before).
    // The same from any script world (a test, a shell that cannot reach this one): a `ytc-back` event on the
    // document, answered in <html data-ytc-back>. A `ytc-wake` event: the shell has news for the page (on_shell_wake).
    back_hook() {
      late = true;
      const back = () => { const r = call('on_back'); globalThis.YTC_EXIT = r === 2; return r; };
      globalThis.YTC_BACK = () => back() === 1;
      d.addEventListener('ytc-back', () => { d.documentElement.dataset.ytcBack = String(back()); });
      d.addEventListener('ytc-wake', () => call('on_shell_wake'));
    },
    // Spatial navigation (the decision is Rust's, tv::pick): rects of the visible `sel` elements (minus `skip`)
    // inside the innermost open `scope` element around the focus (else the last open one, else the document),
    // as [x, y, w, h, m]*n (m = 1 + the index of the first `|`-separated selector of `mark` it matches, 0 none)
    // followed by the focused element's index (-1: none). The list is kept for spatial_focus.
    // An element inside a `boxes` ancestor is measured (and checked for visibility) by that ancestor: an off-screen
    // content-visibility:auto card gives its own box without laying out its contents (one forced layout per card).
    spatial_rects(scope, sel, skip, mark, boxes) {
      const marks = mark ? mark.split('|') : [];
      const a = d.activeElement, rect = el => { const r = el.getBoundingClientRect(); return r.width > 0 && r.height > 0 && (!el.checkVisibility || el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) ? r : null; };
      let root = d;
      if (scope) {
        const open = [...d.querySelectorAll(scope)].filter(rect);
        root = open.filter(s => s.contains(a)).pop() || open.pop() || d;
      }
      const els = [], out = [];
      for (const el of root.querySelectorAll(sel)) {
        if ((skip && el.matches(skip)) || el.closest('[inert],[aria-hidden="true"],[hidden]')) continue;
        const r = rect((boxes && el.closest(boxes)) || el);
        if (!r) continue;
        els.push(el); out.push(r.left, r.top, r.width, r.height, marks.findIndex(m => el.matches(m)) + 1);
      }
      YTC._sp = els;
      out.push(els.indexOf(a));
      return new Float64Array(out);
    },
    // Focuses item `i` of the last spatial_rects list. y ≥ 0: the window scrolls to y first (instant), then only
    // the scrolling boxes around the item bring it into view; y < 0: it is scrolled into view the nearest way.
    spatial_focus(i, y) {
      const el = (YTC._sp || [])[i]; if (!el) return;
      if (y >= 0) scrollTo(0, y);
      el.focus({ preventScroll: true, focusVisible: true });
      el.scrollIntoView({ block: 'nearest', inline: 'nearest' });
    },
    // Scrolls the box `id` by `frac` of its own height (instant: it answers a key).
    scroll_box(id, frac) { const el = byId(id); if (el) el.scrollTop += el.clientHeight * frac; },
    // Focus memory: remembers the focused element under `name`; focus_load refocuses it (if it is still in the
    // document and shown) without scrolling and answers whether it did.
    focus_save(name) { (YTC._fm || (YTC._fm = {}))[name] = d.activeElement; },
    focus_load(name) {
      const el = (YTC._fm || {})[name];
      if (!el || !el.isConnected || !el.getClientRects().length) return false;
      el.focus({ preventScroll: true, focusVisible: true }); return d.activeElement === el;
    },
    session_clear() { try { sessionStorage.clear(); } catch {} },
    hidden: () => d.hidden,
    screen_px: () => new Float64Array([screen.width, screen.height, devicePixelRatio || 1]),
    playback_quality(h) { const v = mo.get(h); const q = v && v.getVideoPlaybackQuality ? v.getVideoPlaybackQuality() : null; return new Float64Array(q ? [q.droppedVideoFrames, q.totalVideoFrames] : []); },
    media_caps(json) {
      let list = []; try { list = JSON.parse(json); } catch {}
      const mc = navigator.mediaCapabilities;
      // Test switch only (e2e): fixed decodingInfo answers per itag.
      let fake = null; try { fake = JSON.parse(localStorage.getItem('ytc-test-caps') || 'null'); } catch {}
      if (fake) { Promise.resolve().then(() => call('on_caps', JSON.stringify(list.map(c => ({ i: c.i, ok: true, s: fake[c.i] ? fake[c.i].s : true, e: fake[c.i] ? fake[c.i].e : true }))))); return; }
      if (!mc || !list.length) { call('on_caps', '[]'); return; }
      // `tf` (an HDR format's transfer function: 'pq' / 'hlg') also asks for BT.2020 colour.
      Promise.all(list.map(c => mc.decodingInfo({ type: 'media-source', video: { contentType: c.mime, width: c.w, height: c.h, bitrate: c.b, framerate: c.f, ...(c.tf ? { transferFunction: c.tf, colorGamut: 'rec2020' } : {}) } })
        .then(r => ({ i: c.i, ok: r.supported, s: r.smooth, e: r.powerEfficient }), () => ({ i: c.i, ok: false }))))
        .then(r => call('on_caps', JSON.stringify(r)));
    },
    incident(json) { try { chrome.runtime.sendMessage({ ytcIncident: json }).catch(() => {}); } catch {} },
    // ---- speed telemetry (decided and shaped in Rust; these only observe, buffer and forward).
    perf_batch(json) { try { chrome.runtime.sendMessage({ ytcPerf: json }).catch(() => {}); } catch {} },
    // Browser entries buffered for the next batch (no per-entry bridge call): Long Animation Frames
    // (else long tasks), Event Timing ≥ 40 ms, layout shifts (summed per page), storyboard images.
    tel_start() {
      if (YTC._tel) return;
      const T = YTC._tel = { q: [], drop: 0, cls: 0, cost: 0, ts: 0 };
      const push = e => { if (T.q.length < 300) T.q.push(e); else T.drop++; };
      const path = u => { try { const x = new URL(u); return x.protocol === 'chrome-extension:' ? x.pathname.slice(1) : x.host + x.pathname; } catch { return ''; } };
      const tgt = el => { if (!el || el.nodeType !== 1) return ''; const a = el.closest && el.closest('[id],[data-act]'); return el.tagName.toLowerCase() + (a ? (a.id ? '#' + a.id : '') + (a.dataset.act ? '[' + a.dataset.act + ']' : '') : ''); };
      const po = (type, f, o) => { try { new PerformanceObserver(l => { const s = perf.now(); for (const e of l.getEntries()) f(e); T.cost += perf.now() - s; }).observe({ type, buffered: true, ...o }); return true; } catch { return false; } };
      const loaf = (PerformanceObserver.supportedEntryTypes || []).includes('long-animation-frame') && po('long-animation-frame', e => push({
        k: 'loaf', t: e.startTime, ms: e.duration, block: e.blockingDuration,
        render: e.renderStart ? e.startTime + e.duration - e.renderStart : 0,
        layout: e.styleAndLayoutStart ? e.startTime + e.duration - e.styleAndLayoutStart : 0,
        scripts: [...(e.scripts || [])].sort((a, b) => b.duration - a.duration).slice(0, 3).map(x => ({ ms: x.duration, type: x.invokerType, inv: String(x.invoker || '').split('?')[0].slice(0, 80), fn: (x.sourceFunctionName || '').slice(0, 60), src: path(x.sourceURL), forced: x.forcedStyleAndLayoutDuration })),
      }));
      if (!loaf) po('longtask', e => push({ k: 'longtask', t: e.startTime, ms: e.duration }));
      // One entry per interaction (click, key, tap: its longest event), as INP counts them.
      let lastId = 0, lastE = null;
      po('event', e => {
        if (e.duration < 40 || !e.interactionId) return;
        const x = { k: 'event', t: e.startTime, ms: e.duration, name: e.name, delay: e.processingStart - e.startTime, proc: e.processingEnd - e.processingStart, pres: e.startTime + e.duration - e.processingEnd, target: tgt(e.target) };
        if (e.interactionId === lastId && lastE) { if (x.ms > lastE.ms) Object.assign(lastE, x); return; }
        lastId = e.interactionId; lastE = x; push(x);
      }, { durationThreshold: 40 });
      po('layout-shift', e => { if (!e.hadRecentInput) T.cls += e.value; });
      po('resource', e => { if (e.name.includes('ytimg.com/sb/')) push({ k: 'res', t: e.startTime, ms: e.duration, bytes: e.transferSize, cached: e.transferSize === 0 }); });
      const stamp = e => { T.ts = e.timeStamp; T.tg = e.target; if (e.type !== 'submit') T.ty = e.type === 'click' && e.detail === 0 ? 'keyclick' : e.type; if (e.pointerType) T.pt = e.pointerType; };
      for (const t of ['pointerdown', 'click', 'keydown', 'submit']) addEventListener(t, stamp, { capture: true, passive: true });
      // The interaction log's target: tag, the closest id (the area) and data-act; never text.
      T.tgt = el => { if (!el || el.nodeType !== 1 || !el.closest) return ''; const a = el.closest('[id]'), c = el.closest('[data-act],a[href],button,input'); const k = c || el; return k.tagName.toLowerCase() + '#' + (a ? a.id : '') + '[' + ((c && c.dataset.act) || '') + ']'; };
    },
    tel_take() { const T = YTC._tel; if (!T) return '[]'; const q = T.q; T.q = []; if (T.drop) { q.push({ k: 'drop', n: T.drop }); T.drop = 0; } return JSON.stringify(q); },
    tel_cost() { const T = YTC._tel; if (!T) return 0; const c = T.cost; T.cost = 0; return c; },
    tel_cls() { const T = YTC._tel; if (!T) return -1; const v = T.cls; T.cls = 0; return v; },
    tel_frame(token) { requestAnimationFrame(() => setTimeout(() => call('on_tel_frame', token, perf.now()), 0)); },
    event_ts: () => (YTC._tel ? YTC._tel.ts : 0),
    tel_target() { const T = YTC._tel; return T && T.tg ? T.tgt(T.tg) + '|' + (T.pt || '') + '|' + (T.ty || '') : '||'; },
    nav_timing() {
      const n = perf.getEntriesByType('navigation')[0];
      if (!n) return '{}';
      const r = x => Math.round(x * 10) / 10;
      return JSON.stringify({ type: n.type, fetch: r(n.fetchStart), dns: r(n.domainLookupEnd - n.domainLookupStart), connect: r(n.connectEnd - n.connectStart), request: r(n.requestStart), response_start: r(n.responseStart), response_end: r(n.responseEnd), worker: r(n.workerStart), script: +d.documentElement.dataset.ytcScript || 0, wasm: +d.documentElement.dataset.ytcWasm || 0 });
    },
    // Native fallback page: forward link clicks (capture, before YouTube's app)
    // and YouTube's own in-page navigations to Rust.
    native_listen() {
      d.addEventListener('click', e => {
        if (e.defaultPrevented || e.button !== 0 || e.ctrlKey || e.metaKey || e.shiftKey || e.altKey) return;
        const a = e.target.closest && e.target.closest('a[href]');
        if (a && call('on_native_link', a.getAttribute('href'))) { e.preventDefault(); e.stopImmediatePropagation(); }
      }, true);
      addEventListener('yt-navigate-finish', () => call('on_native_nav', location.pathname + location.search));
      addEventListener('popstate', () => call('on_native_nav', location.pathname + location.search));
    },
    text(id, s) { const el = byId(id); if (el && el.textContent !== s) el.textContent = s; },
    css_var(id, name, v) { const el = byId(id); if (el) el.style.setProperty(name, v); },
    // Selects an input's text and copies it; false if the clipboard refused.
    copy(id) {
      const el = byId(id); if (!el) return false;
      el.focus(); el.select();
      try { if (d.execCommand('copy')) return true; } catch {}
      try { navigator.clipboard.writeText(el.value).catch(() => {}); return true; } catch { return false; }
    },
    blur() { const a = d.activeElement; if (a && a !== d.body && !editable(a) && a.blur) a.blur(); },
    // Warm a connection (DNS + TCP/QUIC + TLS) to an https origin, once per origin and page (anonymous, like our fetches).
    preconnect(origin) {
      if (!/^https:\/\/[a-z0-9.-]+$/.test(origin) || (this.pc || (this.pc = new Set())).has(origin)) return;
      this.pc.add(origin);
      const l = d.createElement('link'); l.rel = 'preconnect'; l.href = origin; l.crossOrigin = 'anonymous'; (d.head || d.documentElement).appendChild(l);
    },
    // A wheel over element `id` reports on_wheel_step(name, -1 up / 1 down), one call per notch (trackpad
    // deltas add up to a notch of 50 px; lines count 40 px); a handled one keeps the page from
    // scrolling. Ctrl/⌘ wheels (zoom) stay the browser's. Bound once per element.
    wheel(id, name) {
      const el = byId(id); if (!el || el.dataset.wh) return; el.dataset.wh = '1';
      let acc = 0;
      el.addEventListener('wheel', e => {
        if (e.ctrlKey || e.metaKey || !e.deltaY) return;
        acc += e.deltaY * (e.deltaMode === 1 ? 40 : e.deltaMode === 2 ? 400 : 1);
        if (Math.sign(acc) !== Math.sign(e.deltaY)) acc = e.deltaY;
        let handled = false;
        if (Math.abs(acc) < 50) handled = !!call('on_wheel_step', name, 0);
        while (Math.abs(acc) >= 50) { handled = !!call('on_wheel_step', name, Math.sign(acc)) || handled; acc -= Math.sign(acc) * Math.max(50, Math.min(Math.abs(acc), 100)); }
        if (handled) { e.preventDefault(); e.stopPropagation(); }
      }, { passive: false });
    },
    read_value(id) { const el = byId(id); return el && typeof el.value === 'string' ? el.value : ''; },
    // ---- voice primitives (2 Nov 2026): microphone capture, a WebSocket relayed by the background worker, a held
    // key, stored secrets the page never reads. Generic: Rust decides what to open, when, and what messages mean.
    input_value(id) { const el = byId(id); return el && 'value' in el ? String(el.value) : ''; },
    // A WebSocket opened by the background worker (a chrome.runtime port; the page's CSP never applies). The
    // worker appends the chrome.storage.local value of `secret` to `protocols` (JSON array), so it never enters
    // this page, and fetches `probe` with it if the handshake fails. on_sock(h, kind, data): open | msg (text) |
    // close (code) | fail (nokey, key, net, url, ext). Binary chunks queue in the worker until the socket opens.
    sock_open(h, url, protocols, secret, probe) {
      let port;
      try { port = chrome.runtime.connect({ name: 'ytc-sock' }); } catch { setTimeout(() => call('on_sock', h, 'fail', 'ext'), 0); return; }
      YTC._socks.set(h, port);
      port.onMessage.addListener(m => {
        if (!m || typeof m.t !== 'string') return;
        if ((m.t === 'close' || m.t === 'fail') && YTC._socks.get(h) === port) YTC._socks.delete(h);
        call('on_sock', h, m.t, String(m.d ?? ''));
      });
      port.onDisconnect.addListener(() => { if (YTC._socks.get(h) === port) { YTC._socks.delete(h); call('on_sock', h, 'fail', 'ext'); } });
      port.postMessage({ t: 'open', url, protocols: JSON.parse(protocols), secret, probe });
    },
    sock_text(h, s) { const p = YTC._socks.get(h); if (p) try { p.postMessage({ t: 'text', d: s }); } catch {} },
    sock_close(h) { const p = YTC._socks.get(h); if (p) { YTC._socks.delete(h); try { p.disconnect(); } catch {} } },
    // Microphone → 16 kHz mono 16-bit PCM in 32 ms chunks to socket `sock`. The level (0..1: RMS from −60 to −10 dBFS)
    // is written to the custom property `prop` of element `el` at most every 33 ms (CSS animates from it). Nothing is
    // played back (the processor's output is silent). on_mic(h, 'on' | 'error:<DOMException name>').
    mic_start(h, sock, el, prop) {
      const st = { stop: false, s: null, ac: null };
      YTC._mics.set(h, st);
      const md = navigator.mediaDevices;
      if (!md || !md.getUserMedia) { setTimeout(() => call('on_mic', h, 'error:NotFoundError'), 0); return; }
      md.getUserMedia({ audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true } }).then(s => {
        if (st.stop) { s.getTracks().forEach(t => t.stop()); return; }
        st.s = s;
        const ac = new AudioContext({ sampleRate: 16000, latencyHint: 'interactive' }); st.ac = ac;
        const src = ac.createMediaStreamSource(s), p = ac.createScriptProcessor(512, 1, 1), target = byId(el);
        let last = 0, lv = -1;
        p.onaudioprocess = e => {
          if (st.stop) return;
          const f = e.inputBuffer.getChannelData(0), n = f.length, pcm = new Int16Array(n);
          let sum = 0;
          for (let i = 0; i < n; i++) { const x = f[i] < -1 ? -1 : f[i] > 1 ? 1 : f[i]; sum += x * x; pcm[i] = x < 0 ? x * 0x8000 : x * 0x7fff; }
          YTC._sock_bin(sock, pcm);
          const t = perf.now();
          if (target && t - last >= 33) {
            last = t;
            const v = Math.round(Math.min(1, Math.max(0, (10 * Math.log10(sum / n + 1e-10) + 60) / 50)) * 50) / 50;
            if (v !== lv) { lv = v; target.style.setProperty(prop, v); }
          }
        };
        src.connect(p); p.connect(ac.destination);
        st.end = () => { try { src.disconnect(); p.disconnect(); } catch {} if (target) target.style.setProperty(prop, 0); };
        call('on_mic', h, 'on');
      }, e => { if (!st.stop) call('on_mic', h, 'error:' + ((e && e.name) || 'Error')); });
    },
    mic_stop(h) {
      const st = YTC._mics.get(h); if (!st) return;
      YTC._mics.delete(h); st.stop = true;
      if (st.end) st.end();
      if (st.s) st.s.getTracks().forEach(t => t.stop());
      if (st.ac) st.ac.close().catch(() => {});
    },
    // A bare key held alone (e.g. 'Alt'): on_hold(key, 1) when pressed without other modifiers, 0 on release,
    // 2 when another key or a pointer button joins it (a chord), 3 when the window loses focus. Never prevents defaults.
    hold_key(key) {
      if (YTC._held) return; YTC._held = key;
      let down = false;
      const end = c => { if (down) { down = false; call('on_hold', key, c); } };
      addEventListener('keydown', e => {
        if (e.key === key) { if (!down && !e.repeat && !e.ctrlKey && !e.metaKey && !e.shiftKey) { down = true; call('on_hold', key, 1); } }
        else end(2);
      }, true);
      addEventListener('keyup', e => { if (e.key === key) end(0); }, true);
      addEventListener('pointerdown', () => end(2), true);
      addEventListener('blur', () => end(3));
      d.addEventListener('visibilitychange', () => { if (d.hidden) end(3); });
    },
    // contextmenu (right click, Shift+F10, the menu key) on a [data-ctx] element → on_ctx(name, data-arg); handled
    // ones show no browser menu.
    ctx_menu() {
      if (YTC._ctx) return; YTC._ctx = true;
      d.addEventListener('contextmenu', e => { const el = e.target.closest && e.target.closest('[data-ctx]'); if (el && call('on_ctx', el.dataset.ctx, el.dataset.arg || '')) e.preventDefault(); });
    },
    // chrome.storage.local secrets: the page only learns whether one is set, on_secret(key, set, changed).
    secret_has(key) {
      const done = v => call('on_secret', key, typeof v === 'string' && v !== '', false);
      try { chrome.storage.local.get(key).then(o => done(o && o[key]), () => done('')); } catch { done(''); }
    },
    // Stores the trimmed value of input `id` under `key` (never handed to Wasm) and empties the input; an empty
    // input stores nothing (on_secret answers with the current state, changed = false).
    secret_save(id, key) {
      const el = byId(id), v = el ? String(el.value).trim() : '';
      if (el) el.value = '';
      if (!v) { YTC.secret_has(key); return; }
      try { chrome.storage.local.set({ [key]: v }).then(() => call('on_secret', key, true, true), () => YTC.secret_has(key)); } catch { YTC.secret_has(key); }
    },
    secret_del(key) { try { chrome.storage.local.remove(key).then(() => call('on_secret', key, false, true), () => YTC.secret_has(key)); } catch {} },
    // (voice internals) Open sockets and microphones by handle; a PCM chunk to a socket (base64: runtime ports carry JSON).
    _socks: new Map(), _mics: new Map(),
    _sock_bin(h, pcm) {
      const p = YTC._socks.get(h); if (!p) return;
      const u = new Uint8Array(pcm.buffer, pcm.byteOffset, pcm.byteLength);
      let s = ''; for (let i = 0; i < u.length; i += 4096) s += String.fromCharCode.apply(null, u.subarray(i, i + 4096));
      try { p.postMessage({ t: 'bin', d: btoa(s) }); } catch {}
    },
    // A popover grows out of its trigger: the trigger's box in the panel's coordinates, as custom
    // properties on the panel (client.css clips the panel's surface from it). Offsets, not rects:
    // a running transform does not count. One read at the open.
    morph_vars(btn, pop) {
      const b = byId(btn), p = byId(pop); if (!b || !p || b.offsetParent !== p.offsetParent) return;
      const w = b.offsetWidth, h = b.offsetHeight, r = getComputedStyle(b).borderTopRightRadius;
      const rad = Math.min(r.endsWith('%') ? parseFloat(r) / 100 * w : parseFloat(r) || 0, w / 2, h / 2);
      const s = p.style;
      s.setProperty('--bw', w + 'px'); s.setProperty('--bh', h + 'px'); s.setProperty('--br', rad + 'px');
      s.setProperty('--up', (p.offsetTop - b.offsetTop) + 'px');
      s.setProperty('--bx', (p.offsetLeft + p.offsetWidth - b.offsetLeft - w) + 'px');
    },
    // Images under `sel` still loading get `ld` (opacity 0); when one lands it shows at once
    // if it took < 50 ms (cache: no fade, never a flash), else `ldf` fades it in. Complete
    // images are untouched; no layout is read; nothing waits (an image paints when decoded
    // either way). One capture listener for every image (load/error do not bubble).
    fade_imgs(sel) {
      if (!this.fading) {
        this.fading = true;
        const done = e => {
          const im = e.target;
          if (!im.classList || !im.classList.contains('ld')) return;
          im.classList.remove('ld');
          if (e.type === 'load' && perf.now() - (im.ytcT || 0) > 50) im.classList.add('ldf');
        };
        d.addEventListener('load', done, true);
        d.addEventListener('error', done, true);
      }
      // A microtask: `srcset` images choose their source in one queued at insertion, so only
      // after it does `complete` tell a cached image (already showable) from a loading one.
      // Still before the frame is painted.
      const t = perf.now();
      queueMicrotask(() => { for (const im of d.querySelectorAll(sel)) if (!im.complete && !im.classList.contains('ld')) { im.classList.add('ld'); im.ytcT = t; } });
    },
    // Layout size (a running transform such as a morph does not count).
    size(id) { const el = byId(id); if (!el) return new Float64Array(0); return new Float64Array([el.offsetWidth, el.offsetHeight, devicePixelRatio || 1]); },
    fullscreen(id) { if (d.fullscreenElement) d.exitFullscreen().catch(() => {}); else { const el = byId(id); if (el && el.requestFullscreen) el.requestFullscreen().catch(() => {}); } },
    is_fullscreen: () => !!d.fullscreenElement,
    // ---- media primitives (no player logic: Rust decides everything)
    mse_ok(mime) { try { return MediaSource.isTypeSupported(mime); } catch { return false; } },
    media_video(h, id, events) {
      const el = byId(id); if (!el) return false;
      mo.set(h, el);
      for (const t of events.split(',')) el.addEventListener(t, () => evt(h, t));
      return true;
    },
    media_source(h, vh) {
      const v = mo.get(vh); if (!v) return;
      const ms = new MediaSource(), u = URL.createObjectURL(ms);
      mo.set(h, ms);
      // Test switch only (e2e): the event never arrives (the player must notice the open state itself).
      let lost = false; try { lost = localStorage.getItem('ytc-test-nosourceopen') === '1'; } catch {}
      if (!lost) ms.addEventListener('sourceopen', () => { URL.revokeObjectURL(u); evt(h, 'sourceopen'); }, { once: true });
      for (const t of ['sourceended', 'sourceclose']) ms.addEventListener(t, () => evt(h, t));
      v.src = u;
    },
    source_buffer(h, msh, mime) {
      try {
        let sb = mo.get(msh).addSourceBuffer(mime);
        // Test switch only (e2e): the next video SourceBuffer takes data but never gives a picture (its
        // init segment goes in, later appends go nowhere, and `buffered` says they arrived).
        let np = false; try { np = mime.startsWith('video/') && localStorage.getItem('ytc-test-nopicture') === '1'; if (np) localStorage.removeItem('ytc-test-nopicture'); } catch {}
        if (np) {
          const real = sb, on = {}, fire = () => { for (const f of on.updateend || []) f(); };
          let n = 0, busy = false, end = 0;
          const done = () => { busy = true; setTimeout(() => { busy = false; end = 60; fire(); }, 5); };
          real.addEventListener('updateend', fire);
          sb = { addEventListener(t, f) { (on[t] = on[t] || []).push(f); }, appendBuffer(b) { if (n++) done(); else real.appendBuffer(b); }, remove() { done(); }, abort() {}, changeType() {},
            get updating() { return busy || real.updating; }, get buffered() { return { length: end ? 1 : 0, start: () => 0, end: () => end }; } };
        }
        mo.set(h, sb);
        sb.addEventListener('updateend', () => evt(h, 'updateend'));
        sb.addEventListener('error', () => evt(h, 'error'));
        return true;
      } catch (e) { console.warn('[Redline] addSourceBuffer', mime, e); return false; }
    },
    // Returns '' on success, else the exception name (e.g. QuotaExceededError).
    sb_append(h, bytes) { try { mo.get(h).appendBuffer(bytes.slice()); return ''; } catch (e) { return (e && e.name) || 'Error'; } },
    // Drops a partly appended media segment (resetParserState), so the next append may start a new one.
    sb_abort(h) { try { mo.get(h).abort(); return true; } catch { return false; } },
    sb_remove(h, a, b) { try { mo.get(h).remove(a, b); return true; } catch { return false; } },
    sb_change_type(h, mime) { try { mo.get(h).changeType(mime); return true; } catch { return false; } },
    media_ranges(h) {
      const o = mo.get(h), out = [];
      try { const r = o && o.buffered; if (r) for (let i = 0; i < r.length; i++) out.push(r.start(i), r.end(i)); } catch {}
      return new Float64Array(out);
    },
    media_get(h, p) { const v = path(mo.get(h), p); return typeof v === 'boolean' ? +v : typeof v === 'number' ? v : NaN; },
    media_str(h, p) { const v = path(mo.get(h), p); return v == null ? '' : String(v); },
    media_set(h, p, v) { const o = mo.get(h); if (!o) return; try { o[p] = typeof o[p] === 'boolean' ? !!v : v; } catch {} },
    media_call(h, m) {
      const o = mo.get(h); if (!o) return;
      try { const r = o[m](); if (r && r.catch) r.catch(e => evt(h, m + ':' + ((e && e.name) || 'Error'))); }
      catch (e) { evt(h, m + ':' + ((e && e.name) || 'Error')); }
    },
    media_reset(h) { const v = mo.get(h); if (v && v.removeAttribute) { v.removeAttribute('src'); try { v.load(); } catch {} } },
    media_free(h) { mo.delete(h); },
    // A new <video> (muted, inline) as handle `h`, not in the document; each listed event calls on_media(h, type).
    media_video_new(h, events) {
      const el = d.createElement('video'); el.muted = true; el.playsInline = true; el.preload = 'auto';
      mo.set(h, el);
      for (const t of events.split(',')) el.addEventListener(t, () => evt(h, t));
    },
    // Draws the frame video `h` has now into a new <canvas id class> (its size, at most `max_w` px wide), held
    // outside the document until `held_put` places it; false when there is no frame to draw.
    media_grab(h, id, cls, max_w) {
      const v = mo.get(h); if (!v || v.readyState < 2 || !v.videoWidth || !v.videoHeight) return false;
      const k = Math.min(1, max_w / v.videoWidth), c = d.createElement('canvas');
      c.width = Math.max(1, Math.round(v.videoWidth * k)); c.height = Math.max(1, Math.round(v.videoHeight * k));
      c.id = id; c.className = cls; c.setAttribute('aria-hidden', 'true');
      try { const g = c.getContext('2d', { alpha: false }); g.imageSmoothingQuality = 'high'; g.drawImage(v, 0, 0, c.width, c.height); } catch { return false; }
      held.set(id, c);
      return true;
    },
    // Calls on_timer(id) once video `h` has presented a frame to the screen (requestVideoFrameCallback; else the next frame).
    media_frame(h, id) {
      const v = mo.get(h); if (!v) return;
      if (v.requestVideoFrameCallback) v.requestVideoFrameCallback(() => call('on_timer', id));
      else requestAnimationFrame(() => setTimeout(() => call('on_timer', id), 0));
    },
    // A held element (`media_grab`) goes right after element `after`; false when either is missing.
    held_put(id, after) { const el = held.get(id), a = byId(after); if (!el || !a) return false; a.after(el); return true; },
    // Forgets held element `id` and takes it out of the document.
    held_drop(id) { const el = held.get(id); held.delete(id); if (el) el.remove(); },
    // ---- wheel to choose (player Speed/Quality): a wheel over a `[data-wheel]` inside `id` never scrolls the
    // page (nor swipes history); it reports on_wheel(name, dy px, notches). `notches` counts a mouse wheel's clicks
    // (+ down): lines (Firefox), or Chrome's wheelDeltaY in whole 120s that is not a precise device's −3×deltaY;
    // 0 for trackpads and macOS mice (their pixels are accumulated in Rust). Ctrl+wheel (zoom) stays the browser's.
    wheel_take(id) {
      const el = byId(id); if (!el || el.dataset.wt) return; el.dataset.wt = '1';
      el.addEventListener('wheel', e => {
        if (e.ctrlKey) return;
        const z = e.target.closest && e.target.closest('[data-wheel]'); if (!z || !el.contains(z)) return;
        e.preventDefault();
        if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) return;
        const dy = e.deltaY * (e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1), w = e.wheelDeltaY || 0;
        const n = e.deltaMode === 1 ? Math.sign(dy) * Math.max(1, Math.round(Math.abs(e.deltaY) / 3)) : w && w % 120 === 0 && w !== -3 * e.deltaY ? -w / 120 : 0;
        call('on_wheel', z.dataset.wheel, dy, n);
      }, { passive: false });
    },
    // Scrolls the box `id` the least so its `sel` descendant is fully visible (instant; only that box moves).
    scroll_keep(id, sel) {
      const el = byId(id), x = el && el.querySelector(sel); if (!x || el.scrollHeight <= el.clientHeight) return;
      const a = el.getBoundingClientRect(), r = x.getBoundingClientRect();
      if (r.top < a.top) el.scrollTop += r.top - a.top - 4; else if (r.bottom > a.bottom) el.scrollTop += r.bottom - a.bottom + 4;
    },
    // Rubber band: `id` moves `dy` px and eases back over `ms` (a compositor transform; nothing waits for it).
    band(id, dy, ms) {
      const el = byId(id); if (!el) return;
      const out = 'cubic-bezier(.23,1,.32,1)';
      el.animate([{ transform: 'none', easing: out }, { transform: `translateY(${dy}px)`, offset: 0.25, easing: out }, { transform: 'none' }], { duration: ms, composite: 'add' });
    },
    // ---- player and watch-page motion (the motion law: everything is final before these run; they only paint)
    // Box morph for a persistent surface whose content changes size (the player's menu panel, the seek-bar
    // tooltip): `box_mark` records the box `id` paints now (a running morph's on-screen box included) right before
    // Rust writes the new content; `box_morph` then paints the surface (the element's ::before carries background,
    // border and radius) and the `content` element from that box to the new one with clip-paths on `easing` over
    // `ms` (a shrinking surface is extended to the old box through --mx first), and fades the content's children
    // in from `dx` px over `fade` ms. Final and clickable at once; one layout read at each end, both at the input
    // that caused the change. `ms` 0 only stops a running morph (keyboard: instant).
    box_mark(id) { const el = byId(id); if (el) YTC._bx.set(id, YTC._box_now(el)); },
    box_morph(id, content, easing, ms, dx, fade) {
      const el = byId(id), c = content ? byId(content) : null, old = YTC._bx.get(id); YTC._bx.delete(id);
      if (!el) return;
      const run = YTC._bxs.get(el); if (run) { YTC._bxs.delete(el); run.stop(); }
      if (c && fade) for (const k of c.children) k.animate([{ opacity: 0, transform: dx ? `translateX(${dx}px)` : 'none' }, { opacity: 1, transform: 'none' }], { duration: fade, easing });
      if (!old || !ms || !old[2] || !old[3]) return;
      const r = el.getBoundingClientRect(), W = r.width, H = r.height; if (!W || !H) return;
      const x0 = old[0] - r.left, y0 = old[1] - r.top, w0 = old[2], h0 = old[3];
      if (Math.abs(x0) < 0.5 && Math.abs(y0) < 0.5 && Math.abs(w0 - W) < 0.5 && Math.abs(h0 - H) < 0.5) return;
      // Extension of the surface beyond the new box (top, right, bottom, left) so it can still paint the old one.
      const e = [Math.max(0, -y0), Math.max(0, x0 + w0 - W), Math.max(0, y0 + h0 - H), Math.max(0, -x0)];
      el.style.setProperty('--mx', `${-e[0]}px ${-e[1]}px ${-e[2]}px ${-e[3]}px`);
      const rad = getComputedStyle(el, '::before').borderTopLeftRadius, PW = W + e[1] + e[3], PH = H + e[0] + e[2];
      const ins = (t, rt, b, l) => `inset(${t}px ${rt}px ${b}px ${l}px round ${rad})`;
      const a = el.animate([{ clipPath: ins(y0 + e[0], PW - x0 - e[3] - w0, PH - y0 - e[0] - h0, x0 + e[3]) }, { clipPath: ins(e[0], e[1], e[2], e[3]) }], { duration: ms, easing, fill: 'forwards', pseudoElement: '::before' });
      let b = null;
      if (c) { const q = c.getBoundingClientRect(), cx = old[0] - q.left, cy = old[1] - q.top; b = c.animate([{ clipPath: ins(cy, q.width - cx - w0, q.height - cy - h0, cx) }, { clipPath: ins(0, 0, 0, 0) }], { duration: ms, easing }); }
      const st = { e, stop() { a.cancel(); if (b) b.cancel(); el.style.removeProperty('--mx'); } };
      YTC._bxs.set(el, st);
      a.onfinish = () => { if (YTC._bxs.get(el) === st) { YTC._bxs.delete(el); st.stop(); } };
    },
    // Selection mark: one persistent `i.cls` inside the list `scope` (made on first use, aria-hidden, behind the
    // items) shows item `item`'s selected surface (`sub`: the part of it that has the box) with a clip-path that CSS
    // transitions, so with `slide` it springs from where it is painted to the new item (retargeting mid-way); else it
    // jumps. An empty or hidden item removes it. Runs in the next animation frame (that frame's own layout, never a
    // forced one); the scope carries `data-mk` while the mark is placed (CSS drops the item's own surface).
    mark_slide(scope, item, sub, cls, slide) {
      requestAnimationFrame(() => {
        const s = byId(scope); if (!s) return;
        let m = s.querySelector(':scope>i.' + cls);
        const it = item ? byId(item) : null, t = it && (sub ? it.querySelector(sub) : it);
        if (!t || !t.offsetHeight) { if (m) m.remove(); delete s.dataset.mk; return; }
        if (!m) { m = d.createElement('i'); m.className = cls; m.setAttribute('aria-hidden', 'true'); s.prepend(m); }
        const x = YTC._pill_box(s, t), H = Math.max(s.scrollHeight, x[1] + x[3]);
        m.style.height = H + 'px';
        const clip = `inset(${x[1]}px ${s.clientWidth - x[0] - x[2]}px ${H - x[1] - x[3]}px ${x[0]}px round ${getComputedStyle(t).borderTopLeftRadius})`;
        if (!slide || !s.dataset.mk) { m.style.transition = 'none'; m.style.clipPath = clip; getComputedStyle(m).clipPath; m.style.transition = ''; }
        else m.style.clipPath = clip;
        s.dataset.mk = '1';
      });
    },
    // Restarts the CSS animations of `id` and its descendants (a one-shot effect on a persistent element: a pulse).
    replay(id) { const el = byId(id); if (el) for (const a of el.getAnimations({ subtree: true })) { a.cancel(); a.play(); } },
    // (box morph internals) The viewport box `el` paints now: its own box, or a running morph's clipped surface.
    _bx: new Map(), _bxs: new Map(),
    _box_now(el) {
      const r = el.getBoundingClientRect(), st = YTC._bxs.get(el);
      if (!st) return [r.left, r.top, r.width, r.height];
      const m = /inset\(([^)]*?)(?: round [^)]*)?\)/.exec(getComputedStyle(el, '::before').clipPath || '');
      if (!m) return [r.left, r.top, r.width, r.height];
      const v = m[1].trim().split(/\s+/).map(parseFloat), [t, rt, b, l] = [v[0], v[1] ?? v[0], v[2] ?? v[0], v[3] ?? v[1] ?? v[0]], e = st.e;
      return [r.left - e[3] + l, r.top - e[0] + t, r.width + e[1] + e[3] - l - rt, r.height + e[0] + e[2] - t - b];
    },
  };
})();
