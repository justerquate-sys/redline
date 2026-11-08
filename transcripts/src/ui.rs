//! What the reader, settings and the popup section share: the page switch, sending messages to the worker, toast-less
//! inline notices, and writing HTML only when it changed.

use crate::host;
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

#[derive(Clone, Copy, PartialEq)]
pub enum Page {
    None,
    Reader,
    Settings,
    Popup,
}

thread_local! {
    static PAGE: Cell<Page> = const { Cell::new(Page::None) };
    static WRITTEN: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

pub fn set_page(name: &str) {
    PAGE.with(|p| {
        p.set(match name {
            "reader" => Page::Reader,
            "settings" => Page::Settings,
            "popup" => Page::Popup,
            _ => Page::None,
        })
    });
}
pub fn page() -> Page {
    PAGE.with(Cell::get)
}

/// Writes `html` into `id` unless it already holds exactly that: a poll that changed nothing leaves hover, focus and
/// scroll alone.
pub fn put(id: &str, html: &str) {
    let same = WRITTEN.with(|w| w.borrow().get(id).is_some_and(|h| h == html));
    if !same {
        WRITTEN.with(|w| w.borrow_mut().insert(id.to_owned(), html.to_owned()));
        host::html(id, html);
    }
}
/// A parent was rewritten: forget what its children held.
pub fn forget_all() {
    WRITTEN.with(|w| w.borrow_mut().clear());
}

pub fn show(id: &str, shown: bool) {
    host::attr(id, "hidden", (!shown).then(String::new));
}
pub fn disable(id: &str, off: bool) {
    host::attr(id, "disabled", off.then(String::new));
}
pub fn set(id: &str, name: &str, value: &str) {
    host::attr(id, name, Some(value.to_owned()));
}

/// `notice`: an inline message; `kind`: `error`, `success`, `info` or `removed`. Empty hides it.
pub fn notice(id: &str, message: &str, kind: &str) {
    host::text(id, message);
    set(id, "class", &format!("tx-notice {kind}"));
    show(id, !message.is_empty());
}

/// A message to the worker: its `data`, or the error text to show.
pub async fn send(action: &str, data: Value) -> Result<Value, String> {
    let mut message = if data.is_object() { data } else { json!({}) };
    message["action"] = json!(action);
    let raw = host::send_message(&message.to_string()).await;
    let reply: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    if reply.is_null() {
        return Err("The extension connection was interrupted. Reload this page and try again.".into());
    }
    if reply["ok"] != true {
        return Err(reply["error"].as_str().unwrap_or("Something went wrong. Try again.").to_owned());
    }
    Ok(reply["data"].clone())
}

/// The value of a query parameter of this page's URL.
pub fn query_param(name: &str) -> Option<String> {
    let q = host::query();
    q.trim_start_matches('?').split('&').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        (k == name).then(|| v.to_owned())
    })
}

/// A plural: `plural(2, "file")` is `2 files`, with the page's number format.
pub fn plural(n: f64, word: &str) -> String {
    format!("{} {word}{}", host::number(n), if n == 1. { "" } else { "s" })
}

pub fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.)
}
