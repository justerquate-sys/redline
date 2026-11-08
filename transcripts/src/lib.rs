//! Redline's Transcripts, for the background worker and the extension's own pages. See docs/architecture.md,
//! "Transcripts". The YouTube page's own Wasm (`src/features/transcribe.rs`) carries only its card and watch controls.

pub mod channel;
pub mod core;
pub mod host;
pub mod html;
pub mod icon;
pub mod plan;
mod popup;
mod reader;
mod settings;
pub mod ui;
pub mod worker;

/// The Redline mark for the extension pages (`browser/icons/redline.svg`).
pub fn ui_mark() -> String {
    format!("<svg class=\"tx-mark\" viewBox=\"0 0 32 24\" aria-hidden=\"true\"><path fill-rule=\"evenodd\" d=\"{}\"/></svg>", icon::MARK)
}

use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

#[wasm_bindgen]
pub fn worker_boot(origin: &str) {
    worker::boot(origin);
}

#[wasm_bindgen]
pub fn on_message(message: String, sender: String) -> Promise {
    future_to_promise(async move { Ok(JsValue::from_str(&worker::on_message(&message, &sender).await)) })
}

#[wasm_bindgen]
pub fn on_alarm() {
    spawn_local(worker::on_alarm());
}

#[wasm_bindgen]
pub fn on_startup() {
    spawn_local(worker::on_startup());
}

#[wasm_bindgen]
pub fn adopt_token(token: String) {
    spawn_local(async move { worker::adopt_legacy_token(&token).await });
}

/// An extension page's script is ready: `reader`, `settings` or `popup`.
#[wasm_bindgen]
pub fn page_boot(page: &str) {
    ui::set_page(page);
    match page {
        "reader" => reader::boot(),
        "settings" => settings::boot(),
        "popup" => popup::boot(),
        _ => {}
    }
}

/// A DOM event on an element with a `data-tx*` attribute (`kind`: click, input, change, submit, hover, key, timer,
/// visible, popstate, scroll).
#[wasm_bindgen]
pub fn on_event(kind: &str, name: &str, arg: &str, value: &str) {
    match ui::page() {
        ui::Page::Reader => reader::on_event(kind, name, arg, value),
        ui::Page::Settings => settings::on_event(kind, name, arg, value),
        ui::Page::Popup => popup::on_event(kind, name, arg, value),
        ui::Page::None => {}
    }
}
