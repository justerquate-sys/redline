//! The thin JavaScript host (`browser/transcripts-host.js`): `chrome.*`, `fetch`, IndexedDB and the DOM as primitives.
//! Every decision stays in Rust. Promises resolve to strings.

use js_sys::Promise;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen]
extern "C" {
    // ---- worker
    /// `fetch` with a deadline over the whole body: `{"status","text"}`, `{"aborted":true}` or `{"failed":true}`.
    #[wasm_bindgen(js_namespace = TR)]
    fn http(url: &str, method: &str, headers: &str, body: &str, timeout_ms: f64) -> Promise;
    /// `chrome.runtime.sendNativeMessage`: the JSON answer, or `{"native_error":"reason"}`.
    #[wasm_bindgen(js_namespace = TR)]
    fn native(host: &str, message: &str) -> Promise;
    /// IndexedDB in the extension's own origin (never visible to content scripts).
    #[wasm_bindgen(js_namespace = TR)]
    fn vault_get(key: &str) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    fn vault_set(key: &str, value: &str) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    fn vault_del(key: &str) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    fn local_get(key: &str) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    fn local_set(key: &str, json: &str) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    fn local_del(key: &str) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    fn sleep(ms: f64) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn epoch() -> f64;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn alarm_on(name: &str, minutes: f64);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn alarm_off(name: &str);
    /// The toolbar icon from RGBA pixels (16 and 32 px); with empty pixels the manifest's icons.
    #[wasm_bindgen(js_namespace = TR)]
    fn icon(px16: &[u8], px32: &[u8]) -> Promise;
    /// The toolbar title; `""` restores the manifest's title.
    #[wasm_bindgen(js_namespace = TR)]
    fn title(text: &str) -> Promise;
    /// A string message to every YouTube tab.
    #[wasm_bindgen(js_namespace = TR)]
    pub fn tabs_push(message: &str);
    /// Focuses an open tab of the page at `path` or opens one; `exact` also replaces its URL.
    #[wasm_bindgen(js_namespace = TR)]
    fn open_page(path: &str) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    fn open_options() -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn runtime_id() -> String;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn runtime_base() -> String;

    // ---- pages
    /// `chrome.runtime.sendMessage`: `{"ok","data"}`/`{"ok":false,"error"}` as JSON.
    #[wasm_bindgen(js_namespace = TR)]
    fn send(message: &str) -> Promise;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn html(id: &str, html: &str);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn text(id: &str, text: &str);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn attr(id: &str, name: &str, value: Option<String>);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn body_attr(name: &str, value: &str);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn doc_title(text: &str);
    /// Sets `.value` / `.checked` of a form control.
    #[wasm_bindgen(js_namespace = TR)]
    pub fn set_value(id: &str, value: &str);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn set_checked(id: &str, on: bool);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn read_value(id: &str) -> String;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn read_checked(id: &str) -> bool;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn focus(selector: &str);
    /// `location.search`.
    #[wasm_bindgen(js_namespace = TR)]
    pub fn query() -> String;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn hash() -> String;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn push_url(url: &str);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn replace_url(url: &str);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn hidden() -> bool;
    /// Copies text; must be called inside the click for a reliable write.
    #[wasm_bindgen(js_namespace = TR)]
    fn clipboard(text: &str) -> Promise;
    /// Copies a text that arrives later: the pending item is handed over inside the click.
    #[wasm_bindgen(js_namespace = TR)]
    fn clipboard_later(text: &Promise) -> Promise;
    /// Local date and time of an ISO string.
    #[wasm_bindgen(js_namespace = TR)]
    pub fn local_time(iso: &str) -> String;
    /// `n.toLocaleString()`.
    #[wasm_bindgen(js_namespace = TR)]
    pub fn number(n: f64) -> String;
    #[wasm_bindgen(js_namespace = TR)]
    pub fn manifest_version() -> String;
    /// The ids of `.folder-card` children of `list` that are in its box (U+001E separated); `""` when not shown.
    #[wasm_bindgen(js_namespace = TR)]
    pub fn in_box(list: &str, selector: &str, attr: &str) -> String;
    /// Scrolls `id`'s `selector` child into its box.
    #[wasm_bindgen(js_namespace = TR)]
    pub fn keep_in_box(id: &str, selector: &str);
    /// Calls back `on_event("timer", name, "", "")` after `ms` (a later call with the same name replaces it).
    #[wasm_bindgen(js_namespace = TR)]
    pub fn later(name: &str, ms: f64);
    #[wasm_bindgen(js_namespace = TR)]
    pub fn later_off(name: &str);
}

async fn string(p: Promise) -> Option<String> {
    JsFuture::from(p).await.ok().and_then(|v| v.as_string())
}

pub async fn http_call(url: &str, method: &str, headers: &str, body: &str, timeout_ms: f64) -> String {
    string(http(url, method, headers, body, timeout_ms)).await.unwrap_or_else(|| r#"{"failed":true}"#.into())
}
pub async fn native_call(host: &str, message: &str) -> String {
    string(native(host, message)).await.unwrap_or_else(|| r#"{"native_error":""}"#.into())
}
pub async fn vault_read(key: &str) -> String {
    string(vault_get(key)).await.unwrap_or_default()
}
pub async fn vault_write(key: &str, value: &str) {
    let _ = JsFuture::from(vault_set(key, value)).await;
}
pub async fn vault_remove(key: &str) {
    let _ = JsFuture::from(vault_del(key)).await;
}
pub async fn local_read(key: &str) -> String {
    string(local_get(key)).await.unwrap_or_default()
}
pub async fn local_write(key: &str, json: &str) -> bool {
    JsFuture::from(local_set(key, json)).await.is_ok()
}
pub async fn local_remove(key: &str) {
    let _ = JsFuture::from(local_del(key)).await;
}
pub async fn wait(ms: f64) {
    let _ = JsFuture::from(sleep(ms)).await;
}
pub async fn set_icon(px16: &[u8], px32: &[u8]) -> bool {
    JsFuture::from(icon(px16, px32)).await.is_ok()
}
pub async fn set_title(text: &str) -> bool {
    JsFuture::from(title(text)).await.is_ok()
}
pub async fn open(path: &str) -> Result<(), String> {
    JsFuture::from(open_page(path)).await.map(|_| ()).map_err(|_| "The page could not open. Try again.".to_owned())
}
pub async fn options() -> Result<(), String> {
    JsFuture::from(open_options()).await.map(|_| ()).map_err(|_| "Settings could not open. Try again.".to_owned())
}
pub async fn send_message(message: &str) -> String {
    string(send(message)).await.unwrap_or_default()
}
pub async fn copy_text(text: &str) -> bool {
    JsFuture::from(clipboard(text)).await.is_ok()
}
pub async fn copy_later(text: &Promise) -> bool {
    JsFuture::from(clipboard_later(text)).await.is_ok()
}
