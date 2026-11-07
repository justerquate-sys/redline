//! Redline: a fast, ad-free client for YouTube, running at www.youtube.com.
//! Rust owns routing, InnerTube requests and parsing, state and HTML.

pub mod actions;
pub mod appearance;
mod popup;
pub mod api;
mod app;
mod bridge;
mod native;
pub mod config;
pub mod description;
pub mod hedge;
pub mod impress;
pub mod homedoc;
pub mod json;
pub mod features;
pub mod parse;
pub mod motion;
pub mod perf;
pub mod player;
pub mod qr;
pub mod render;
pub mod route;
pub mod ready;
pub mod snap;
pub mod telemetry;
pub mod tv;
pub mod util;

use wasm_bindgen::prelude::*;

/// Rust panics become incident reports before the module aborts.
fn panic_reports() {
    std::panic::set_hook(Box::new(|info| {
        let report = serde_json::json!({
            "kind": "rust-panic",
            "reason": info.to_string(),
            "at": bridge::epoch(),
            "client": env!("CARGO_PKG_VERSION"),
            "path": bridge::href(),
        });
        bridge::incident(&report.to_string());
    }));
}

#[wasm_bindgen]
pub fn boot() {
    panic_reports();
    if bridge::extension_page() { popup::boot(); } else { app::boot(); }
}

/// Native fallback page: watch for leaving the fallback video.
#[wasm_bindgen]
pub fn boot_native() {
    panic_reports();
    native::boot(&bridge::href());
}

#[wasm_bindgen]
pub fn on_native_link(href: &str) -> bool {
    native::on_link(href)
}

#[wasm_bindgen]
pub fn on_native_nav(href: &str) {
    native::on_nav(href);
}

/// Native page: the DOM changed (at most once per frame).
#[wasm_bindgen]
pub fn on_native_dom() {
    native::on_dom();
}

/// Native page keydown (capture): true swallows it.
#[wasm_bindgen]
pub fn on_native_key(key: &str, code: &str, flags: u32) -> bool {
    native::clean::on_key(key, code, flags)
}

/// Native page wheel (capture): 1 stops it, 2 also keeps the page from scrolling.
#[wasm_bindgen]
pub fn on_native_wheel(dy: f64, flags: u32) -> u32 {
    native::clean::on_wheel(dy, flags)
}

/// Native page caption drag: pointer and caption corner as fractions of the caption frame.
#[wasm_bindgen]
pub fn on_native_drag(fx: f64, fy: f64, ex: f64, ey: f64, phase: u32) {
    native::clean::on_drag(fx, fy, ex, ey, phase);
}

#[wasm_bindgen]
pub fn on_native_input(name: &str, value: &str, committed: bool) {
    native::clean::on_input(name, value, committed);
}

/// Native page: a click on the CC button (a deliberate toggle).
#[wasm_bindgen]
pub fn on_native_intent() {
    native::clean::on_intent();
}

/// An app shell call answered (`bridge::shell`).
#[wasm_bindgen]
pub fn on_shell(seq: u32, ok: bool, json: &str) {
    app::with(|a| a.on_shell(seq, ok, json));
}

/// The app shell's Back button: 0 = not handled (the shell goes back), 1 = handled by the client,
/// 2 = leave the app.
#[wasm_bindgen]
pub fn on_back() -> u32 {
    app::with_ret(|a| a.on_back())
}

#[wasm_bindgen]
pub fn on_fetch(id: u32, status: u32, body: &[u8]) {
    app::with(|a| a.on_fetch(id, status, body));
}

/// A streaming fetch (`progress` option): headers arrived (`bytes` 0), then body progress.
#[wasm_bindgen]
pub fn on_fetch_progress(id: u32, status: u32, bytes: f64) {
    app::with(|a| a.on_fetch_progress(id, status, bytes));
}

/// A streaming fetch (`chunks` option): the next piece of a successful body as it arrives.
#[wasm_bindgen]
pub fn on_fetch_chunk(id: u32, status: u32, bytes: &[u8]) {
    app::with(|a| a.on_fetch_chunk(id, status, bytes));
}

#[wasm_bindgen]
pub fn on_storage(key: &str, json: &str) {
    if popup::active() { return popup::on_storage(key, json); }
    if telemetry::on_storage(key, json) {
        return;
    }
    if native::active() {
        return native::clean::on_storage(key, json);
    }
    app::with(|a| a.on_storage(key, json));
}

/// A telemetry frame span's frame has rendered (`bridge::tel_frame`).
#[wasm_bindgen]
pub fn on_tel_frame(token: u32, t: f64) {
    telemetry::on_frame(token, t);
}

#[wasm_bindgen]
pub fn on_timer(id: u32) {
    if popup::active() { return popup::on_timer(id); }
    if native::active() {
        return native::clean::on_timer(id);
    }
    // A timer is no input; a handler that acts on a hover dwell names it (`telemetry::act_label`).
    let tel = telemetry::enabled();
    if tel {
        telemetry::act_begin(String::new(), "dwell".into(), "hover", app::route_now(), true);
    }
    app::with(|a| a.on_timer(id));
    if tel {
        telemetry::act_end(true);
    }
}

#[wasm_bindgen]
pub fn on_storage_change(key: &str, json: &str) {
    if popup::active() { return popup::on_storage_change(key, json); }
    if native::active() { return; }
    app::with(|a| a.on_storage_change(key, json));
}

#[wasm_bindgen]
pub fn on_store_result(id: u32, ok: bool) { if popup::active() { popup::on_store_result(id, ok); } }

#[wasm_bindgen]
pub fn on_message_result(id: u32, json: &str) { if popup::active() { popup::on_message_result(id, json); } }

#[wasm_bindgen]
pub fn on_extension_message(message: &str) -> String { app::with_ret(|a| a.toolbar_message(message)) }

/// Returns true when the click was handled (the bridge then prevents default).
#[wasm_bindgen]
pub fn on_click(href: &str, act: &str, arg: &str, mods: u32) -> bool {
    if popup::active() { return popup::on_click(act, arg, mods & 32 != 0); }
    let tel = telemetry::enabled() && !act.starts_with("tv-");
    if tel {
        let (tg, pt) = telemetry::target();
        let to = (!href.is_empty()).then(|| route::parse(href).map(|r| app::route_kind(&r)).unwrap_or("other"));
        let (name, kind) = telemetry::click_action(act, arg, &tg, to);
        let input = if mods & 32 != 0 { "keyboard" } else if pt.starts_with("touch") { "touch" } else { "pointer" };
        telemetry::act_begin(name, kind, input, app::route_now(), false);
        if let Some(to) = to {
            telemetry::act_field("to", to.into());
        }
        if let Some(v) = href.strip_prefix("/watch?v=").map(|v| &v[..v.len().min(11)]).filter(|v| util::is_video_id(v)) {
            telemetry::act_field("vid", v.into());
        }
    }
    let r = app::with_ret(|a| a.on_click(href, act, arg, mods));
    if tel {
        telemetry::act_end(r);
    }
    r
}

/// Pointer press or keyboard focus on a link: prefetch it now.
#[wasm_bindgen]
pub fn on_intent(href: &str) {
    app::with(|a| a.on_intent(href));
}

#[wasm_bindgen]
pub fn on_submit(form: &str, value: &str) -> bool {
    let tel = telemetry::enabled();
    if tel {
        let (_, pt) = telemetry::target();
        let name = if form.starts_with("ytc-search") { "search.submit".to_string() } else { format!("submit.{}", telemetry::area(form)) };
        telemetry::act_begin(name, "form".into(), if pt.ends_with("|click") { "pointer" } else { "keyboard" }, app::route_now(), false);
    }
    let r = app::with_ret(|a| a.on_submit(form, value));
    if tel {
        telemetry::act_end(r);
    }
    r
}

#[wasm_bindgen]
pub fn on_hover(href: &str, class: &str) {
    app::with(|a| a.on_hover(href, class));
}

/// `input` on an element with `data-input` (`kind` = `inputType`).
#[wasm_bindgen]
pub fn on_input(name: &str, value: &str, kind: &str) {
    app::with(|a| a.on_input(name, value, kind));
}

/// Tab hidden: write the tab-lifetime snapshot (pagehide → `on_unload`).
/// Focus entered/left a `[data-focus]` element.
#[wasm_bindgen]
pub fn on_focus(name: &str, arg: &str, on: bool) {
    app::with(|a| a.on_focus(name, arg, on));
}

#[wasm_bindgen]
pub fn on_hide() {
    app::with(|a| a.on_hide());
}

/// The tab became visible again.
#[wasm_bindgen]
pub fn on_show() {
    app::with(|a| a.on_show());
}

#[wasm_bindgen]
pub fn on_key(key: &str, editable: bool) -> bool {
    // TV Search edits text while buttons have focus, so `editable` is false. Neither physical
    // letters nor the remote's directions/OK sequence may become a keystroke trace. Navigation
    // and request spans still measure the submitted search without recording its text.
    let name = if telemetry::enabled() && !(tv::on() && bridge::any("#ytc[data-tvq='1']")) {
        telemetry::key_action(key, editable)
    } else {
        None
    };
    let tel = name.is_some();
    if let Some(n) = name {
        telemetry::act_begin(n, if editable { "field" } else { "key" }.into(), "keyboard", app::route_now(), false);
    }
    let r = app::with_ret(|a| a.on_key(key, editable));
    if tel {
        telemetry::act_end(r);
    }
    r
}

#[wasm_bindgen]
pub fn on_pop() {
    let tel = telemetry::enabled();
    if tel {
        telemetry::act_begin("history.pop".into(), "history".into(), "history", app::route_now(), false);
    }
    app::with(|a| a.on_pop());
    if tel {
        telemetry::act_end(true);
    }
}

/// `MediaCapabilities.decodingInfo` results for the player.
#[wasm_bindgen]
pub fn on_caps(json: &str) {
    app::with(|a| a.on_caps(json));
}

#[wasm_bindgen]
pub fn on_media(h: u32, ev: &str) {
    app::with(|a| a.on_media(h, ev));
}

/// Pointer over a `data-track` element: fraction across it (−1 = it left).
#[wasm_bindgen]
pub fn on_track(name: &str, fx: f64) {
    app::with(|a| a.on_track(name, fx));
}

/// Wheel over a `[data-wheel]` zone (`bridge::wheel_take`): vertical pixels and mouse-wheel clicks (+ = down).
#[wasm_bindgen]
pub fn on_wheel(name: &str, dy: f64, notches: i32) {
    let tel = telemetry::wheel_begin(name);
    app::with(|a| a.on_wheel(name, dy, notches));
    if tel {
        telemetry::act_end(true);
    }
}

/// A wheel notch over an element bound with `bridge::wheel` (−1 up, 1 down, 0 partial); true = handled.
#[wasm_bindgen]
pub fn on_wheel_step(name: &str, dir: i32) -> bool {
    let tel = telemetry::wheel_begin(name);
    let r = app::with_ret(|a| a.on_wheel_step(name, dir));
    if tel {
        telemetry::act_end(r);
    }
    r
}

/// Pointer drag on `[data-drag]`: fractions across its box, phase 0 down / 1 move / 2 up.
#[wasm_bindgen]
pub fn on_drag(name: &str, fx: f64, fy: f64, phase: u32) {
    let tel = phase == 2 && telemetry::enabled();
    if tel {
        telemetry::act_begin(format!("drag.{}", telemetry::area(name)), "drag".into(), "pointer", app::route_now(), false);
    }
    app::with(|a| a.on_drag(name, fx, fy, phase));
    if tel {
        telemetry::act_end(true);
    }
}

/// The hover zone (`[data-hov]` name + `data-arg`, '' = none) changed; `held` = a button is down.
#[wasm_bindgen]
pub fn on_hov(zone: &str, arg: &str, held: bool) {
    let tel = telemetry::enabled();
    if tel {
        telemetry::act_begin(String::new(), format!("hover@{}", telemetry::area(zone)), "hover", app::route_now(), true);
    }
    app::with(|a| a.on_hov(zone, arg, held));
    if tel {
        telemetry::act_end(true);
    }
}

/// A pointer press, with the `data-pop` it is inside ("" = outside).
#[wasm_bindgen]
pub fn on_press(pop: &str) {
    app::with(|a| a.on_press(pop));
}

#[wasm_bindgen]
pub fn on_move() {
    app::with(|a| a.on_move());
}

#[wasm_bindgen]
pub fn on_unload() {
    app::with(|a| a.on_unload());
}

#[wasm_bindgen]
pub fn on_visible(id: &str) {
    app::with(|a| a.on_visible(id));
}

/// The page's own document gave the script `bridge::doc_hold` waits for ("" when it ended without it).
#[wasm_bindgen]
pub fn on_doc_script(text: &str) {
    app::with(|a| a.on_doc_script(text));
}

/// `bridge::shared_get` / `shared_take` answered (`value` "" when missing).
#[wasm_bindgen]
pub fn on_shared(key: &str, value: &str) {
    app::with(|a| a.on_shared(key, value));
}

/// Another context changed the shared store (`value` "" when removed).
#[wasm_bindgen]
pub fn on_shared_change(key: &str, value: &str) {
    app::with(|a| a.on_shared_change(key, value));
}

/// An element watched by `bridge::shown_watch` came into view (`i`: its place among the watched elements in
/// document order; its box in the viewport, CSS px).
#[wasm_bindgen]
pub fn on_shown(key: &str, i: u32, x: f64, y: f64, w: f64, h: f64) {
    app::with(|a| a.on_shown(key, i, x, y, w, h));
}

/// The page came to rest after a scroll (`scrollend`).
#[wasm_bindgen]
pub fn on_scroll_end(y: f64) {
    app::with(|a| a.on_scroll_end(y));
}

/// Voice search: socket relay events (`open`, `msg`, `close`, `fail`).
#[wasm_bindgen]
pub fn on_sock(h: u32, kind: &str, data: &str) {
    app::with(|a| a.on_sock(h, kind, data));
}

/// Voice search: microphone state (`on`, `error:<name>`).
#[wasm_bindgen]
pub fn on_mic(h: u32, state: &str) {
    app::with(|a| a.on_mic(h, state));
}

/// A held bare key (`bridge::hold_key`): 1 down, 0 up, 2 chord, 3 focus lost.
#[wasm_bindgen]
pub fn on_hold(key: &str, code: u32) {
    app::with(|a| a.on_hold(key, code));
}

/// Whether a stored secret is set (`bridge::secret_*`); `changed` = it was just saved or removed.
#[wasm_bindgen]
pub fn on_secret(key: &str, set: bool, changed: bool) {
    app::with(|a| a.on_secret(key, set, changed));
}

/// contextmenu on a `[data-ctx]` element; true = handled (no browser menu).
#[wasm_bindgen]
pub fn on_ctx(name: &str, arg: &str) -> bool {
    let tel = telemetry::enabled();
    if tel {
        telemetry::act_begin(format!("ctx.{}", telemetry::area(name)), "contextmenu".into(), "pointer", app::route_now(), false);
    }
    let r = app::with_ret(|a| a.on_ctx(name, arg));
    if tel {
        telemetry::act_end(r);
    }
    r
}
