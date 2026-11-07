//! Imports from the generic JavaScript bridge (`browser/bridge.js`). The bridge
//! knows nothing about YouTube: it only performs DOM writes, fetches, storage,
//! history, timers and event forwarding on Rust's behalf.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    /// `localStorage.getItem(key) === '1'` (test switches only).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn local_flag(key: &str) -> bool;
    /// `localStorage.setItem(key, '1')` when `on`, else `removeItem(key)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn local_flag_set(key: &str, on: bool);
    /// `document.hidden`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn hidden() -> bool;
    /// `navigator.userAgent`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn ua() -> String;
    /// `[screen.width, screen.height, devicePixelRatio]`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn screen_px() -> Vec<f64>;
    /// `[droppedVideoFrames, totalVideoFrames]` of the media element `h`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn playback_quality(h: u32) -> Vec<f64>;
    /// `navigator.mediaCapabilities.decodingInfo` for each config; results in `on_caps(json)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_caps(json: &str);
    /// Hand an incident report (JSON) to the background worker's debug center.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn incident(json: &str);
    /// Hand a telemetry batch (JSON, `kind: "perf"`, schema 2) to the background worker.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn perf_batch(json: &str);
    /// Starts the browser observers (Long Animation Frames or long tasks, Event Timing ≥ 40 ms,
    /// layout shifts, storyboard resource timings) and input timestamps. Once per document.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn tel_start();
    /// The observers' buffered entries (JSON array, emptied).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn tel_take() -> String;
    /// Time spent in the observers' callbacks since the last call (ms).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn tel_cost() -> f64;
    /// Cumulative layout shift since the last call (and resets it).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn tel_cls() -> f64;
    /// Calls `on_tel_frame(token, performance.now())` once the next frame has rendered.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn tel_frame(token: u32);
    /// `timeStamp` of the last click/keydown/pointerdown/submit (performance.now ms; 0 = none).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn event_ts() -> f64;
    /// The last input event's target as `tag#closest-id[data-act]|pointerType|event type` (no text).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn tel_target() -> String;
    /// The document's navigation timing (JSON: `type`, `fetch`, `dns`, `connect`, `request`,
    /// `response_start`, `response_end`, and the loader's marks `script` / `wasm`, performance.now ms; `{}` when
    /// missing).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn nav_timing() -> String;
    /// `location.replace(url)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn replace_doc(url: &str);
    /// Native fallback page: forwards link clicks and YouTube navigations.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn native_listen();
    /// `performance.now()` (ms since navigation start).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn now() -> f64;
    /// `Date.now()`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn epoch() -> f64;
    /// `location.pathname + search + hash`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn href() -> String;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn cookie() -> String;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn lang() -> String;
    /// Replaces the whole document content and installs delegated listeners.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn doc(html: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn html(id: &str, html: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn wheel_x(id: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn append(id: &str, html: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn get_html(id: &str) -> String;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn attr(id: &str, name: &str, value: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn toggle(id: &str, class: &str) -> bool;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn value(id: &str, v: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn focus(id: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn focus_id() -> String;
    /// Focuses a control inside `id`: 0 = the `.on` one (else the first), ±1 = next/previous.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn focus_in(id: &str, delta: i32);
    /// Roving focus over `sel` inside `id`: ±1, ±1000 (= one row), or the first in view.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn focus_grid(id: &str, sel: &str, delta: i32) -> bool;
    /// hrefs (newline-separated) of the `sel` elements in `id` that intersect the viewport, at most `max`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn visible_hrefs(id: &str, sel: &str, link: &str, max: u32) -> String;
    /// `document.visibilityState === 'visible'`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn visible() -> bool;
    /// Scrolls the window (instantly) so `id` is centered.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn into_view(id: &str);
    /// Focuses the first `sel` inside `id` without scrolling.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn focus_sel(id: &str, sel: &str);
    /// Scrolls the box `id` (not the window) so its first `sel` child is centered.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn scroll_in(id: &str, sel: &str);
    // ---- app shell (the TV app, `tv/`); see bridge.js.
    /// The platform an app shell declared (`"tv"`), else "".
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn platform() -> String;
    /// A call into the app shell; answered by `on_shell(seq, ok, json)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shell(seq: u32, cmd: &str, json: &str);
    /// The shell's Back button calls `on_back()` from now on.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn back_hook();
    /// Spatial navigation: `[x, y, w, h, m]*n, focused index (-1 none)` of the visible `sel` elements
    /// (minus `skip`) inside the innermost open `scope` around the focus (else the last open one,
    /// else the document); `m` = 1 + the index of the first `|`-separated selector of `mark` it matches (0: none). The list is kept for `spatial_focus`.
    /// An element inside a `boxes` ancestor is measured (and checked for visibility) by that ancestor's box: a
    /// `content-visibility:auto` card off screen gives its own box without laying out its contents (measuring the
    /// link inside it would force that, one layout per card and key).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn spatial_rects(scope: &str, sel: &str, skip: &str, mark: &str, boxes: &str) -> Vec<f64>;
    /// Focuses item `i` of the last `spatial_rects` list: `y` ≥ 0 scrolls the window there first
    /// (then only inner scrollers bring it into view), `y` < 0 scrolls it into view the nearest way.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn spatial_focus(i: u32, y: f64);
    /// Scrolls the box `id` by `frac` of its height (instant).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn scroll_box(id: &str, frac: f64);
    /// Remembers the focused element under `name`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn focus_save(name: &str);
    /// Refocuses the element saved under `name` (still in the document and shown): true if it did.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn focus_load(name: &str) -> bool;
    /// Empties this tab's sessionStorage.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn session_clear();
    /// True when some element in the document matches `sel`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn any(sel: &str) -> bool;
    // ---- pages left to the site (native mode); see bridge.js.
    /// Sets (`None`: removes) an attribute on `<html>`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn root_attr(name: &str, value: Option<String>);
    /// Sets a style property (a CSS variable) on `<html>`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn root_var(name: &str, value: &str);
    /// `on_native_dom()` at most once per frame while the document changes (and once now).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_watch(attrs: &str);
    /// Attribute `name` of the first `sel` match.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_attr(sel: &str, name: &str) -> Option<String>;
    /// Collapsed text of each `sel` match, joined by U+001E.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_texts(sel: &str) -> String;
    /// Sets the empty attribute `name` on the i-th `sel` match.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_mark(sel: &str, i: u32, name: &str);
    /// Clicks the first rendered, enabled `inner` inside the i-th `sel` match ("" = that match).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_click(sel: &str, i: u32, inner: &str) -> bool;
    /// Inserts `html` into the first `parent` match unless it has a `.cls` child; events stay inside it.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_mount(parent: &str, cls: &str, html: &str);
    /// Sets the value of every `sel` input not being dragged.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_value(sel: &str, value: &str);
    /// Capture-phase key, wheel, drag, input and click forwarding (`on_native_*`).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_guards(scope: &str, key_skip: &str, wheel_skip: &str, drag: &str, frame: &str, intent: &str);
    /// Entrance wave: the first `max` `sel` matches in `id` (their `sub` parts if given) fade in
    /// rising `dy` px, `step` ms apart (≤ 7 steps). No layout read.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn stagger(id: &str, sel: &str, sub: &str, max: u32, step: u32, dy: f64, easing: &str, ms: u32);
    /// The new text of persistent `id` fades in from `dy` px below.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn nudge(id: &str, dy: f64, easing: &str, ms: u32);
    /// Selection pill: a copy of `scope`'s items in their selected look (`cls`), clipped to one
    /// item, slides and morphs from item `from` (or a running slide's place) to item `to`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn pill_slide(scope: &str, from: &str, to: &str, cls: &str, easing: &str, ms: u32);
    /// `scope` was re-rendered: a running pill slide continues over the new items.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn pill_carry(scope: &str);
    /// Reflow FLIP: remember where the `sel` items in `scope` are painted (before a re-render)…
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn flip_mark(scope: &str, sel: &str);
    /// …then slide survivors from there and shrink copies of vanished items out (`out` ms).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn flip_play(scope: &str, sel: &str, easing: &str, ms: u32, out: u32);
    /// `[x, y, w, h]` (viewport) of `inner` in the closest `scope` ancestor of the element the
    /// last click landed on; empty when there is none or it is off-screen. Read before a swap.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn click_rect(scope: &str, inner: &str) -> Vec<f64>;
    /// `[x, y, w, h]` (viewport) of `id` as painted now, a running morph included.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn rect(id: &str) -> Vec<f64>;
    /// FLIP: paints `id` (already final) from the viewport rect `x, y, w, h` to its own place with
    /// a compositor-only transform (`easing`, `ms`); `data-flip` while it moves.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn flip_from(id: &str, x: f64, y: f64, w: f64, h: f64, easing: &str, ms: u32);
    /// Opacity 0 → 1 on `id`, for content just written.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn fade_in(id: &str, easing: &str, ms: u32);
    /// `prefers-reduced-motion: reduce`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn reduced_motion() -> bool;
    /// `matchMedia(query).matches`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn matches(query: &str) -> bool;
    /// Calls `on_timer(id)` once the next frame has rendered.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn frame(id: u32);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn title(t: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn push(url: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn replace(url: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn scroll_y() -> f64;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn scroll_to(y: f64);
    /// Response arrives in `on_fetch(id, status, bytes)`; status 0 = network error.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn fetch(id: u32, method: &str, url: &str, body: &str, headers: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn abort(id: u32);
    /// Result arrives in `on_storage(key, json)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn storage_get(key: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn storage_set(key: &str, json: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn extension_page() -> bool;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn storage_watch(key: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn storage_write(id: u32, key: &str, json: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn message_listen();
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn active_message(id: u32, message: &str);
    /// Calls `on_timer(id)` after `ms`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn timer(id: u32, ms: u32);
    /// Calls `on_timer(id)` when the main thread is idle (at most `ms` later).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn idle(id: u32, ms: u32);
    /// sessionStorage (tab lifetime). `""` when missing.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn session_get(key: &str) -> String;
    /// False when the quota is exceeded or storage is unavailable.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn session_set(key: &str, value: &str) -> bool;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn session_del(key: &str);
    /// Keeps the page's own document streaming with its nodes held out of the page; once a script starting with
    /// `prefix` is complete, `on_doc_script(text)` (the earlier scripts containing `keep`, then it; without it when
    /// the document ended without one).
    /// Call after `doc`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn doc_hold(prefix: &str, keep: &str);
    /// `window.stop()`: the page's own document stops (and a `doc_hold` with it, without an answer).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn page_stop();
    /// The extension's memory-only storage shared by its tabs (`chrome.storage.session`) is there.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shared_ok() -> bool;
    /// `chrome.storage.session` get: the value (a string, `""` when missing) arrives in `on_shared(key, value)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shared_get(key: &str);
    /// Get and remove (the value arrives in `on_shared`).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shared_take(key: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shared_set(key: &str, value: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shared_del(key: &str);
    /// Reports elements in view: every `sel` element inside `#list` (now and when `shown_scan` runs) with the
    /// attribute `data-<attr>` calls `on_shown(value, index, x, y, w, h)` once it is ≥ `ratio` in the viewport (an
    /// IntersectionObserver: no layout read of ours). `shown_stop` ends it.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shown_watch(list: &str, sel: &str, attr: &str, ratio: f64);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shown_scan();
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn shown_stop();
    /// When this document's response started arriving (performance.now ms; 0 when unknown).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn nav_response_start() -> f64;
    /// Navigation timing type: `navigate`, `reload` or `back_forward`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn nav_type() -> String;
    /// `innerWidth` (CSS px).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn width() -> f64;
    /// `{w, h, dpr, dark, share}` of this window (JSON).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn viewport() -> String;
    /// Appends HTML into the closest `sel` ancestor of the hovered link.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn hover_insert(sel: &str, html: &str);
    /// Removes every element matching `sel`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn remove_all(sel: &str);
    /// Adds (`on`) or removes a class on every element matching `sel`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn class_all(sel: &str, cls: &str, on: bool);
    /// (Re)observes an element; `on_visible(id)` fires when it nears the viewport.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn observe(id: &str);
    /// `iframe#id.contentWindow.postMessage(msg, origin)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn post(id: &str, msg: &str, origin: &str);
    /// `document.documentElement.dataset[name] = performance.now()` (for tests).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn mark(name: &str);
    /// `document.documentElement.dataset[name] = value` (for tests).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn mark_val(name: &str, value: &str);
    /// `fetch` with extra init options (JSON merged into the init object).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn fetch_opts(id: u32, method: &str, url: &str, body: &str, headers: &str, opts: &str);
    /// `location.assign(url)` (a real document navigation).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn go(url: &str);
    /// Sets `textContent` (only when it changed).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn text(id: &str, s: &str);
    /// `el.style.setProperty(name, value)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn css_var(id: &str, name: &str, value: &str);
    /// Selects the text of input `id` and copies it to the clipboard.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn copy(id: &str) -> bool;
    /// Blurs the focused element unless it is a text field.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn blur();
    /// Warm a connection to an https origin (once per origin and page).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn preconnect(origin: &str);
    /// Wheel notches over element `id` → `on_wheel_step(name, dir)` (−1 up, 1 down, 0 = a partial notch).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn wheel(id: &str, name: &str);
    /// The `value` of a form control (textarea, input) by id ("" when missing).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn read_value(id: &str) -> String;
    // ---- voice primitives (features/voice.rs, app/voiceglue.rs)
    /// The current `value` of input `id` ('' if none).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn input_value(id: &str) -> String;
    /// A WebSocket through the background worker: the stored `secret` is appended to `protocols` (JSON) there
    /// and never enters the page; `probe` is fetched with it when the handshake fails. `on_sock(h, kind, data)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn sock_open(h: u32, url: &str, protocols: &str, secret: &str, probe: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn sock_text(h: u32, s: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn sock_close(h: u32);
    /// Microphone → 16 kHz 16-bit PCM chunks to socket `sock`; the level (0..1) to custom property `prop` of `el`
    /// (≤ 30×/s). `on_mic(h, 'on' | 'error:<name>')`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn mic_start(h: u32, sock: u32, el: &str, prop: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn mic_stop(h: u32);
    /// `on_hold(key, code)` for a bare key held alone (1 down, 0 up, 2 chord, 3 focus lost).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn hold_key(key: &str);
    /// contextmenu on `[data-ctx]` → `on_ctx(name, arg)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn ctx_menu();
    /// `on_secret(key, set, changed)`: whether chrome.storage.local holds a value for `key` (never the value).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn secret_has(key: &str);
    /// Stores input `id`'s trimmed value under `key` without handing it to Wasm, and empties the input.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn secret_save(id: &str, key: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn secret_del(key: &str);
    /// Popover morph source: writes trigger `btn`'s box relative to panel `pop`
    /// as custom properties on the panel (`--bw`/`--bh` size, `--br` corner
    /// radius, `--up` how far above the panel's top it starts, `--bx` its gap to
    /// the panel's right edge). Layout offsets only (running transforms do not
    /// count); read once at the open, never per frame.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn morph_vars(btn: &str, pop: &str);
    /// Images matching `sel` that are still loading fade in when they land
    /// (classes `ld`/`ldf` in client.css); complete (cached) ones are left
    /// alone and ones landing within 50 ms show at once. Reads no layout.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn fade_imgs(sel: &str);
    /// `[width, height, devicePixelRatio]` of an element (empty if missing).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn size(id: &str) -> Vec<f64>;
    /// Toggles fullscreen for an element (exits if anything is fullscreen).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn fullscreen(id: &str);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn is_fullscreen() -> bool;
    /// `MediaSource.isTypeSupported(mime)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn mse_ok(mime: &str) -> bool;
    /// Binds `<video id>` to handle `h`; each listed event calls `on_media(h, type)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_video(h: u32, id: &str, events: &str) -> bool;
    /// A new `<video>` (muted, inline) as handle `h`, outside the document; each listed event calls `on_media(h, type)`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_video_new(h: u32, events: &str);
    /// Draws video `h`'s current frame into a new `<canvas id class>` (its size, at most `max_w` px wide),
    /// held outside the document until `held_put`; false when there is no frame.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_grab(h: u32, id: &str, cls: &str, max_w: f64) -> bool;
    /// `on_timer(id)` once video `h` has presented a frame (requestVideoFrameCallback).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_frame(h: u32, id: u32);
    /// Places held element `id` right after element `after`; false when either is missing.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn held_put(id: &str, after: &str) -> bool;
    /// Forgets held element `id` and removes it from the document.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn held_drop(id: &str);
    /// New MediaSource `h` attached to video handle `vh`; `sourceopen` (once), `sourceended` and `sourceclose` are reported.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_source(h: u32, vh: u32);
    /// `addSourceBuffer(mime)` as handle `h` (`updateend`/`error` reported).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn source_buffer(h: u32, ms: u32, mime: &str) -> bool;
    /// `appendBuffer(copy of bytes)`; '' or the exception name.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn sb_append(h: u32, bytes: &[u8]) -> String;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn sb_remove(h: u32, start: f64, end: f64) -> bool;
    /// `abort()`: drops a partly appended media segment (resetParserState).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn sb_abort(h: u32) -> bool;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn sb_change_type(h: u32, mime: &str) -> bool;
    /// `buffered` as flat `[start0, end0, start1, end1, …]`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_ranges(h: u32) -> Vec<f64>;
    /// Numeric/boolean property (dotted path, e.g. `error.code`); NaN otherwise.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_get(h: u32, prop: &str) -> f64;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_str(h: u32, prop: &str) -> String;
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_set(h: u32, prop: &str, v: f64);
    /// Calls a no-argument method; a rejected promise/exception reports `method:ErrorName`.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_call(h: u32, method: &str);
    /// Detaches the video's source (`removeAttribute('src')` + `load()`).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_reset(h: u32);
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn media_free(h: u32);
    /// Wheel to choose: wheel events over a `[data-wheel]` inside element `id` never scroll the page; they
    /// report `on_wheel(name, dy px, notches)` (`notches` = mouse-wheel clicks, + down; 0 = a precise device).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn wheel_take(id: &str);
    /// Scrolls box `id` (only it, instantly) the least so its `sel` descendant is fully in view.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn scroll_keep(id: &str, sel: &str);
    /// A rubber-band nudge: `id` moves `dy` px and springs back over `ms` (compositor transform, nothing waits).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn band(id: &str, dy: f64, ms: u32);
    /// Box morph, step 1: remember the box `id` paints now (before its content changes size).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn box_mark(id: &str);
    /// Box morph, step 2 (after the write): its surface (`::before`) and `content` clip from the remembered
    /// box to the new one (`easing`, `ms`; 0 = just stop a running morph); `content`'s children fade in
    /// from `dx` px over `fade` ms.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn box_morph(id: &str, content: &str, easing: &str, ms: u32, dx: f64, fade: u32);
    /// Selection mark: `i.cls` in list `scope` shows item `item` (its `sub` part) selected; `slide` springs
    /// it there from where it is (CSS transition), else it jumps; '' removes it. Next animation frame.
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn mark_slide(scope: &str, item: &str, sub: &str, cls: &str, slide: bool);
    /// Restarts the CSS animations of `id` and its descendants (one-shot effects on persistent elements).
    #[wasm_bindgen(js_namespace = YTC)]
    pub fn replay(id: &str);
}
