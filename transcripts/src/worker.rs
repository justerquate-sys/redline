//! The background worker's Transcripts service: pairing with the local helper, every request to it, tab ownership of
//! channel previews and the toolbar. Requests from YouTube tabs arrive here and only here; the pairing token never
//! leaves this worker and the extension's own pages.
//!
//! Gating: nothing in this file runs for someone who never set the feature up. The host loads this Wasm on the first
//! message, so a YouTube page of an unconfigured user causes no request and no native-messaging attempt; only opening
//! settings (an explicit act) probes.

use crate::{channel, core, host, icon, plan};
use js_sys::Promise;
use serde_json::{Value, json};
use std::cell::RefCell;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};

/// `chrome.storage.local`: the one flag a YouTube tab reads (`{"on":true}`). Absent means the feature is off.
pub const ENABLED_KEY: &str = "ytcTranscriptsV1";
const TOKEN_KEY: &str = "pairingToken";
const HOST_NAME: &str = "com.fulltranscript.helper";
const ALARM: &str = "ytc-transcripts-status";
const TOOLBAR_RECORD: &str = "toolbar";
const PREVIEWS_RECORD: &str = "previews";
const MAX_PREVIEWS: usize = 200;
/// Passive status reads may ask the native host to start a stopped helper at most this often.
const RECONNECT_EVERY_MS: f64 = 5.0 * 60.0 * 1000.0;
/// Read attempts while the helper does not answer and the last status had work in it (10 minutes at the alarm's pace).
const MAX_OFFLINE_READS: u32 = 20;

#[derive(Default)]
struct Worker {
    origin: String,
    pending_starts: Vec<(String, Promise)>,
    pending_cancels: Vec<(String, Promise)>,
    channel_starts: Vec<Promise>,
    queue_cancel: Option<Promise>,
    pairing: Option<Promise>,
    last_reconnect: f64,
    toolbar: Toolbar,
    live: Value,
    last_push: String,
}

#[derive(Default)]
struct Toolbar {
    initialized: bool,
    snapshot: Option<Value>,
    view: Value,
    life: Value,
    revision: u64,
    again: bool,
    pending: Option<Promise>,
    last_paint: String,
    painting: bool,
    repaint: bool,
    timer: u64,
    offline_reads: u32,
}

thread_local! {
    static W: RefCell<Worker> = RefCell::new(Worker { live: json!({}), ..Worker::default() });
}

fn with<R>(f: impl FnOnce(&mut Worker) -> R) -> R {
    W.with(|w| f(&mut w.borrow_mut()))
}

pub fn boot(origin: &str) {
    with(|w| w.origin = origin.to_owned());
}

#[derive(Debug)]
pub struct Failure {
    pub message: String,
    pub status: u16,
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Failure { message, status: 0 }
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Failure { message: message.to_owned(), status: 0 }
    }
}

#[derive(Clone)]
pub struct Options {
    method: String,
    body: Option<Value>,
    public: bool,
    timeout_ms: f64,
    can_reconnect: bool,
    read_only: bool,
    can_pair: bool,
}
impl Default for Options {
    fn default() -> Self {
        Options { method: "GET".into(), body: None, public: false, timeout_ms: 20000., can_reconnect: true, read_only: false, can_pair: true }
    }
}
fn post() -> Options {
    Options { method: "POST".into(), ..Options::default() }
}
fn passive() -> Options {
    Options { can_reconnect: false, can_pair: false, timeout_ms: 8000., ..Options::default() }
}

/// Pairing errors in words someone can act on (the helper's three ways of being unreachable look alike otherwise).
fn native_failure(reason: &str) -> String {
    let lower = reason.to_ascii_lowercase();
    if lower.contains("not found") {
        return "The browser cannot find the helper’s registration. If it was just installed or repaired, choose Check connection again. On a new computer, run Install Helper.command from the helper folder.".into();
    }
    if lower.contains("forbidden") || lower.contains("not allowed") {
        return "The browser blocked this extension’s connection to the helper. Reload the extension, then choose Check connection.".into();
    }
    "The browser could not start the helper connection. Choose Check connection to retry. The helper may be installed but its browser connection needs repair.".into()
}

fn valid_token(t: &str) -> bool {
    (16..=1024).contains(&t.len()) && !t.chars().any(char::is_whitespace)
}

async fn pair_once(origin: String) -> Result<String, String> {
    let answer = host::native_call(HOST_NAME, r#"{"action":"getConnection"}"#).await;
    let v: Value = serde_json::from_str(&answer).unwrap_or(Value::Null);
    if let Some(reason) = v["native_error"].as_str() {
        return Err(native_failure(reason));
    }
    // A connection may only ever name the fixed loopback address: nothing is sent to anywhere else.
    if v["ok"] != true || v["url"].as_str() != Some(origin.as_str()) {
        return Err("The local helper could not connect. Run Install Helper.command once, then try again.".into());
    }
    let token = v["pairingToken"].as_str().filter(|t| valid_token(t)).ok_or("The local helper returned an invalid pairing token. Run Install Helper.command again.")?;
    host::vault_write(TOKEN_KEY, token).await;
    Ok(token.to_owned())
}

/// Pairs through the installed native host; concurrent callers share one attempt.
async fn pair() -> Result<String, String> {
    let shared = with(|w| {
        if let Some(p) = &w.pairing {
            return p.clone();
        }
        let origin = w.origin.clone();
        let p = future_to_promise(async move {
            let r = pair_once(origin).await;
            with(|w| w.pairing = None);
            r.map(|t| JsValue::from_str(&t)).map_err(|e| JsValue::from_str(&e))
        });
        w.pairing = Some(p.clone());
        p
    });
    match JsFuture::from(shared).await {
        Ok(v) => Ok(v.as_string().unwrap_or_default()),
        Err(e) => Err(e.as_string().unwrap_or_default()),
    }
}

fn unreachable_message() -> Failure {
    "The local helper is not responding. Open Settings and check the connection. If it is not installed, run Install Helper.command once.".into()
}

/// One request to the helper. Read-only requests retry once through the native host; a POST is never repeated after an
/// ambiguous network failure, because it could be billable.
pub async fn helper(path: &str, o: Options) -> Result<Value, Failure> {
    let origin = with(|w| w.origin.clone());
    let mut can_reconnect = o.can_reconnect;
    loop {
        let mut token = String::new();
        if !o.public {
            token = host::vault_read(TOKEN_KEY).await;
            if token.is_empty() {
                if !o.can_pair {
                    return Err("The helper is not connected yet.".into());
                }
                token = pair().await?;
            }
        }
        let mut headers = json!({"Accept":"application/json"});
        if o.body.is_some() {
            headers["Content-Type"] = json!("application/json");
        }
        if !o.public {
            headers["Authorization"] = json!(format!("Bearer {token}"));
        }
        let body = o.body.as_ref().map(Value::to_string).unwrap_or_default();
        let raw = host::http_call(&format!("{origin}{path}"), &o.method, &headers.to_string(), &body, o.timeout_ms).await;
        let r: Value = serde_json::from_str(&raw).unwrap_or(json!({"failed":true}));
        if r["aborted"] == true {
            return Err("The local helper did not respond in time. Try again shortly. Existing transcription jobs continue in the background.".into());
        }
        if r["status"].is_null() {
            if can_reconnect && (o.method == "GET" || o.read_only) {
                can_reconnect = false;
                pair().await?;
                continue;
            }
            return Err(unreachable_message());
        }
        let status = r["status"].as_u64().unwrap_or(0) as u16;
        let Ok(data) = serde_json::from_str::<Value>(r["text"].as_str().unwrap_or("")) else {
            return Err("The helper returned an unexpected response. Check that the correct helper is running on port 8765.".into());
        };
        if (200..300).contains(&status) {
            return Ok(data);
        }
        if status == 401 && can_reconnect && !o.public {
            can_reconnect = false;
            pair().await?;
            continue;
        }
        if status == 401 || status == 403 {
            return Err(Failure { message: "The local helper rejected this browser’s connection. Run Install Helper.command again, or use Manual pairing in Settings.".into(), status });
        }
        if status == 404 {
            let message = if path.starts_with("/settings") || path == "/videos/status" { "The installed helper needs an update. Run Install Helper.command again, then reload Settings." } else { "This transcript was not found in the helper. Open your library or start the video again." };
            return Err(Failure { message: message.into(), status });
        }
        let message = data["detail"].as_str().or(data["error"].as_str()).unwrap_or("The helper could not complete this request.");
        return Err(Failure { message: message.into(), status });
    }
}

async fn queue_status(o: Options) -> Result<Value, Failure> {
    match helper("/queue/status", o.clone()).await {
        Ok(v) => return Ok(v),
        Err(e) if e.status != 404 => return Err(e),
        Err(_) => {}
    }
    // An older helper may be transcribing a batch and cannot be restarted safely: its read-only routes still give
    // verified counts per video.
    let response = helper("/jobs", o.clone()).await?;
    let jobs = response["jobs"].as_array().ok_or("Queue status is unavailable.")?.clone();
    let mut ids: Vec<String> = vec![];
    for job in &jobs {
        if let Some(id) = job["video_id"].as_str().filter(|i| core::valid_video_id(i))
            && !ids.iter().any(|x| x == id)
        {
            ids.push(id.to_owned());
        }
    }
    let mut statuses = serde_json::Map::new();
    for chunk in ids.chunks(200) {
        let r = video_statuses(chunk.to_vec(), o.clone()).await?;
        if let Some(m) = r["statuses"].as_object() {
            statuses.extend(m.clone());
        }
    }
    let mut result = json!({"scope":"all","total":ids.len(),"queued":0,"processing":0,"completed":0,"saved":0,"removed":0,"failed":0,"cancelled":0,"save_failed":0,"missing":0,"unsaved":0,"saving":0,"discovering_channels":null,"waiting_channels":null,"discovery_unknown":true,"discovery_complete":false,"finished":false});
    let bump = |r: &mut Value, k: &str| r[k] = json!(r[k].as_u64().unwrap_or(0) + 1);
    for id in &ids {
        let status = statuses.get(id);
        match status.and_then(|s| s["status"].as_str()) {
            Some("queued") => bump(&mut result, "queued"),
            Some("downloading" | "preparing" | "transcribing") => bump(&mut result, "processing"),
            Some("completed") => {
                bump(&mut result, "completed");
                if status.is_some_and(|s| s["saved"] == true) {
                    bump(&mut result, "saved");
                } else if jobs.iter().any(|j| j["video_id"] == json!(id) && j["status"] == "completed" && !j["export_error"].is_null()) {
                    bump(&mut result, "save_failed");
                } else {
                    bump(&mut result, "unsaved");
                }
            }
            Some(k @ ("failed" | "cancelled" | "removed")) => bump(&mut result, k),
            _ => bump(&mut result, "missing"),
        }
    }
    Ok(result)
}

/// Only the two fields a YouTube tab needs, only for the ids it asked about: no titles, no paths, no history.
async fn video_statuses(ids: Vec<String>, o: Options) -> Result<Value, Failure> {
    if ids.is_empty() {
        return Ok(json!({"statuses":{}}));
    }
    let data = helper("/videos/status", Options { method: "POST".into(), body: Some(json!({"video_ids":ids})), read_only: true, ..o }).await?;
    let mut out = serde_json::Map::new();
    for id in &ids {
        let entry = &data["statuses"][id];
        if let Some(s) = entry["status"].as_str().filter(|s| matches!(*s, "completed" | "queued" | "downloading" | "preparing" | "transcribing" | "failed" | "cancelled" | "removed")) {
            out.insert(id.clone(), json!({"status":s,"saved":s == "completed" && entry["saved"] == true}));
        }
    }
    Ok(json!({"statuses":out}))
}

async fn await_promise(p: Promise) -> Result<Value, Failure> {
    match JsFuture::from(p).await {
        Ok(v) => Ok(serde_json::from_str(&v.as_string().unwrap_or_default()).unwrap_or(Value::Null)),
        Err(e) => {
            let raw = e.as_string().unwrap_or_default();
            let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
            Err(Failure { message: v["m"].as_str().unwrap_or("Something went wrong. Try again.").to_owned(), status: v["s"].as_u64().unwrap_or(0) as u16 })
        }
    }
}
fn to_js(r: Result<Value, Failure>) -> Result<JsValue, JsValue> {
    match r {
        Ok(v) => Ok(JsValue::from_str(&v.to_string())),
        Err(e) => Err(JsValue::from_str(&json!({"m":e.message,"s":e.status}).to_string())),
    }
}

async fn start_job(url: &str, folder: Option<Value>) -> Result<Value, Failure> {
    if with(|w| w.queue_cancel.is_some()) {
        return Err("The queue is stopping. Try again after it stops.".into());
    }
    let (id, url) = core::normalize_video(url)?;
    if with(|w| w.pending_cancels.iter().any(|(i, _)| *i == id)) {
        return Err("This video is stopping. Try again after it stops.".into());
    }
    if let Some(p) = with(|w| w.pending_starts.iter().find(|(i, _)| *i == id).map(|(_, p)| p.clone())) {
        return await_promise(p).await;
    }
    let mut body = json!({"url":url});
    if let Some(f) = folder {
        body["folder_id"] = f;
    }
    let key = id.clone();
    let p = future_to_promise(async move {
        let r = helper("/jobs", Options { body: Some(body), ..post() }).await;
        with(|w| w.pending_starts.retain(|(i, _)| *i != key));
        to_js(r)
    });
    with(|w| w.pending_starts.push((id, p.clone())));
    await_promise(p).await
}

async fn cancel_video(id: String) -> Result<Value, Failure> {
    if !core::valid_video_id(&id) {
        return Err("Invalid YouTube video identifier.".into());
    }
    if let Some(p) = with(|w| w.pending_cancels.iter().find(|(i, _)| *i == id).map(|(_, p)| p.clone())) {
        return await_promise(p).await;
    }
    let key = id.clone();
    let start = with(|w| w.pending_starts.iter().find(|(i, _)| *i == id).map(|(_, p)| p.clone()));
    let path = format!("/videos/{id}/cancel");
    let p = future_to_promise(async move {
        if let Some(s) = start {
            let _ = JsFuture::from(s).await;
        }
        let r = async {
            let result = match helper(&path, Options { can_reconnect: false, timeout_ms: 60000., ..post() }).await {
                Ok(r) => r,
                Err(e) if e.status == 404 => return Err("Update the helper to cancel videos directly from YouTube.".into()),
                Err(e) => return Err(e),
            };
            let status = &result["status"];
            let known = status.is_null() || matches!(status.as_str(), Some("completed" | "cancelled" | "removed" | "failed" | "unknown"));
            if !known {
                return Err("The helper has not confirmed this video stopped. Check its status before trying again.".into());
            }
            Ok(json!({"status":status,"saved":status == "completed" && result["saved"] == true}))
        }
        .await;
        with(|w| w.pending_cancels.retain(|(i, _)| *i != key));
        to_js(r)
    });
    with(|w| w.pending_cancels.push((id, p.clone())));
    await_promise(p).await
}

async fn cancel_queue() -> Result<Value, Failure> {
    if let Some(p) = with(|w| w.queue_cancel.clone()) {
        return await_promise(p).await;
    }
    let waits = with(|w| w.pending_starts.iter().map(|(_, p)| p.clone()).chain(w.channel_starts.iter().cloned()).collect::<Vec<_>>());
    let p = future_to_promise(async move {
        for wait in waits {
            let _ = JsFuture::from(wait).await;
        }
        let r = match helper("/queue/cancel", Options { can_reconnect: false, timeout_ms: 60000., ..post() }).await {
            Err(e) if e.status == 404 => Err("Update the helper to stop the whole queue. Completed transcripts will stay saved.".into()),
            other => other,
        };
        with(|w| w.queue_cancel = None);
        to_js(r)
    });
    with(|w| w.queue_cancel = Some(p.clone()));
    await_promise(p).await
}

fn preview_path(id: &Value) -> Result<String, Failure> {
    let s = id.as_str().filter(|s| !s.is_empty() && s.len() <= 100 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')).ok_or("Invalid channel preview.")?;
    Ok(format!("/channels/previews/{s}"))
}
fn preview_owner(id: &str, sender: &Value) -> String {
    format!("{id}:{}:{}", sender["tab"]["id"].as_i64().unwrap_or(-1), sender["frameId"].as_i64().unwrap_or(0))
}
async fn preview_list() -> Vec<Value> {
    serde_json::from_str::<Value>(&host::vault_read(PREVIEWS_RECORD).await).ok().and_then(|v| v.as_array().cloned()).unwrap_or_default()
}
async fn authorize_preview(id: &Value, sender: &Value, trusted: bool) -> Result<String, Failure> {
    let path = preview_path(id)?;
    if trusted {
        return Ok(path);
    }
    let key = preview_owner(id.as_str().unwrap_or(""), sender);
    if !preview_list().await.iter().any(|v| v.as_str() == Some(key.as_str())) {
        return Err("This channel preview belongs to another page. Start a preview from this channel page.".into());
    }
    Ok(path)
}

async fn channel_request(path: String, body: Option<Value>) -> Result<Value, Failure> {
    if with(|w| w.queue_cancel.is_some()) {
        return Err("The queue is stopping. Try again after it stops.".into());
    }
    let p = future_to_promise(async move { to_js(helper(&path, Options { body, ..post() }).await) });
    with(|w| w.channel_starts.push(p.clone()));
    let r = await_promise(p.clone()).await;
    with(|w| w.channel_starts.retain(|x| !js_sys::Object::is(x, &p)));
    r
}

/// The channel page's panel: runs one step of an import and answers with the panel's markup, whether to ask again, and
/// the state for the tab to hold. The tab echoes the state back; ownership of the preview is still checked here.
async fn channel_panel(message: &Value, sender: &Value, trusted: bool) -> Result<Value, Failure> {
    let op = message["op"].as_str().unwrap_or("");
    let echoed: Value = serde_json::from_str(message["state"].as_str().unwrap_or("")).unwrap_or(Value::Null);
    let start = async |state: &mut Value| {
        let url = state["url"].as_str().unwrap_or("").to_owned();
        let body = json!({"url":url,"scope":state["scope"],"include_shorts":state["shorts"] == true});
        match channel_request("/channels/import".into(), Some(body)).await.and_then(|p| preview_path(&p["id"]).map(|_| p)) {
            Ok(preview) => {
                if !trusted {
                    let mut list = preview_list().await;
                    list.push(json!(preview_owner(preview["id"].as_str().unwrap_or(""), sender)));
                    let first = list.len().saturating_sub(MAX_PREVIEWS);
                    host::vault_write(PREVIEWS_RECORD, &Value::Array(list[first..].to_vec()).to_string()).await;
                }
                state["id"] = preview["id"].clone();
                state["preview"] = preview;
                state["error"] = json!("");
            }
            Err(e) => state["error"] = json!(e.message),
        }
    };
    let poll = async |state: &mut Value| match authorize_preview(&state["id"], sender, trusted).await {
        Ok(path) => match helper(&path, Options::default()).await {
            Ok(preview) => {
                state["preview"] = preview;
                state["error"] = json!("");
            }
            Err(e) => state["error"] = json!(e.message),
        },
        Err(e) => state["error"] = json!(e.message),
    };
    let was_saved = channel::all_saved(&echoed["preview"]);
    let mut state = echoed.clone();
    match op {
        "start" => {
            state = channel::start_state(message["url"].as_str().unwrap_or(""), message["scope"].as_str().unwrap_or(""), message["shorts"] == true)?;
            // A running import of the same channel and choices is watched, not started twice.
            let same = echoed["url"] == state["url"] && echoed["scope"] == state["scope"] && echoed["shorts"] == state["shorts"];
            if same && echoed["id"].is_string() && (channel::needs_polling(&echoed["preview"]) || echoed["error"] != "") {
                state = echoed.clone();
                poll(&mut state).await;
            } else {
                start(&mut state).await;
            }
        }
        "poll" => poll(&mut state).await,
        "cancel" => {
            let path = authorize_preview(&state["id"], sender, trusted).await?;
            match helper(&format!("{path}/cancel"), post()).await {
                Ok(preview) => state["preview"] = preview,
                Err(e) => state["error"] = json!(e.message),
            }
        }
        _ => {
            // Retry: the status of a known import, or the import again.
            if state["id"].is_string() && (state["error"] != "" || state["preview"]["status"] == "ready" || channel::needs_polling(&state["preview"])) {
                poll(&mut state).await;
            } else {
                state = channel::start_state(state["url"].as_str().unwrap_or(""), state["scope"].as_str().unwrap_or(""), state["shorts"] == true)?;
                start(&mut state).await;
            }
        }
    }
    let saved_now = channel::all_saved(&state["preview"]);
    let toast = if saved_now && !was_saved { format!("{} channel transcripts saved.", channel::counts(&state["preview"]["progress"]).saved) } else { String::new() };
    let again = state["error"] == "" && channel::needs_polling(&state["preview"]);
    Ok(json!({"html":channel::panel(&state),"poll":again,"state":state.to_string(),"toast":toast}))
}

/// Keeps the flag YouTube tabs read in step with what the helper says. Called whenever settings are read or saved.
async fn set_enabled(on: bool) {
    let was = !host::local_read(ENABLED_KEY).await.is_empty();
    if on && !was {
        host::local_write(ENABLED_KEY, r#"{"on":true}"#).await;
        let _ = refresh_toolbar().await;
    } else if !on && was {
        host::local_remove(ENABLED_KEY).await;
        with(|w| {
            w.toolbar.view = Value::Null;
            w.toolbar.timer += 1;
        });
        host::alarm_off(ALARM);
        host::set_icon(&[], &[]).await;
        host::set_title("").await;
    }
}

async fn settings_read(o: Options) -> Result<Value, Failure> {
    let settings = helper("/settings", o).await?;
    set_enabled(settings["configured"] == true && settings["has_api_key"] == true).await;
    Ok(settings)
}

async fn route(message: &Value, sender: &Value, trusted: bool) -> Result<Value, Failure> {
    let action = message["action"].as_str().unwrap_or("");
    if let Some(p) = plan::plan(message, trusted)? {
        let path = p["path"].as_str().unwrap_or("/").to_owned();
        let o = Options {
            method: p["method"].as_str().unwrap_or("GET").to_owned(),
            body: p.get("body").cloned(),
            public: p["publicRequest"] == true,
            timeout_ms: p["timeoutMs"].as_f64().unwrap_or(20000.),
            can_reconnect: p["canReconnect"] != false,
            can_pair: p["canPair"] != false,
            ..Options::default()
        };
        let not_found = p["notFoundMessage"].as_str().map(str::to_owned);
        if action == "getAzureSettings" {
            return settings_read(o).await;
        }
        let result = helper(&path, o).await;
        if action == "saveAzureSettings" && let Ok(saved) = &result {
            set_enabled(saved["configured"] == true && saved["has_api_key"] == true).await;
        }
        return match result {
            Err(e) if e.status == 404 && not_found.is_some() => Err(Failure { message: not_found.unwrap_or_default(), status: 404 }),
            other => other,
        };
    }
    match action {
        "startJob" => {
            let folder = (trusted && message.get("folderId").is_some()).then(|| message["folderId"].clone());
            start_job(message["url"].as_str().unwrap_or(""), folder).await
        }
        "cancelVideo" => cancel_video(message["videoId"].as_str().unwrap_or("").to_owned()).await,
        "cancelQueue" => cancel_queue().await,
        "getVideoStatuses" => {
            let ids: Vec<String> = message["videoIds"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).fold(vec![], |mut a, id| {
                if !a.contains(&id) {
                    a.push(id);
                }
                a
            });
            // A page load asks at most once in a while to start a stopped helper; the card stays quiet either way.
            let now = host::epoch();
            let may_start = with(|w| {
                let due = now - w.last_reconnect > RECONNECT_EVERY_MS;
                if due {
                    w.last_reconnect = now;
                }
                due
            });
            let o = Options { can_reconnect: may_start, can_pair: may_start, timeout_ms: 8000., ..Options::default() };
            let mut r = video_statuses(ids, o).await?;
            let active = r["statuses"].as_object().is_some_and(|m| m.values().any(|s| matches!(s["status"].as_str(), Some("queued" | "downloading" | "preparing" | "transcribing"))));
            r["live"] = with(|w| w.live.clone());
            // Work the toolbar has not seen, or a helper that answers again: it catches up.
            let (watching, was_offline) = with(|w| (matches!(w.toolbar.view["kind"].as_str(), Some("processing" | "queued")), w.toolbar.view["kind"] == "offline"));
            if (active && !watching) || was_offline {
                spawn_local(async {
                    let _ = refresh_toolbar().await;
                });
            }
            Ok(r)
        }
        "revealVideoFile" => {
            let id = message["videoId"].as_str().unwrap_or("");
            let r = helper(&format!("/videos/{id}/reveal"), Options { can_reconnect: false, ..post() }).await?;
            Ok(json!({"revealed":r["revealed"] == true}))
        }
        "previewChannel" | "startChannelImport" => {
            let url = core::normalize_channel(message["url"].as_str().unwrap_or(""))?;
            let path = if action == "startChannelImport" { "/channels/import" } else { "/channels/preview" };
            let preview = channel_request(path.into(), Some(json!({"url":url,"scope":message["scope"],"include_shorts":message["include_shorts"] == true}))).await?;
            preview_path(&preview["id"])?;
            if !trusted {
                let mut list = preview_list().await;
                list.push(json!(preview_owner(preview["id"].as_str().unwrap_or(""), sender)));
                let start = list.len().saturating_sub(MAX_PREVIEWS);
                host::vault_write(PREVIEWS_RECORD, &Value::Array(list[start..].to_vec()).to_string()).await;
            }
            Ok(preview)
        }
        "channelPanel" => channel_panel(message, sender, trusted).await,
        "getChannelPreview" => helper(&authorize_preview(&message["id"], sender, trusted).await?, Options::default()).await,
        "queueChannelPreview" => {
            let path = authorize_preview(&message["id"], sender, trusted).await?;
            channel_request(format!("{path}/queue"), None).await
        }
        "cancelChannelPreview" => helper(&format!("{}/cancel", authorize_preview(&message["id"], sender, trusted).await?), post()).await,
        "getQueueStatus" => {
            let expected = with(|w| w.toolbar.revision);
            let result = queue_status(Options::default()).await?;
            let accepted = accept(Some(result), false, Some(expected)).await;
            if !accepted || with(|w| w.toolbar.revision) != expected + 1 {
                refresh_toolbar().await?;
            }
            let (snapshot, offline, until) = with(|w| (w.toolbar.snapshot.clone(), w.toolbar.view["kind"] == "offline", w.toolbar.life["completionUntil"].as_f64().unwrap_or(0.)));
            let Some(mut snapshot) = snapshot.filter(|_| !offline) else {
                return Err("Queue status is unavailable. Try again shortly.".into());
            };
            snapshot["completion_until"] = json!(if until > host::epoch() { until } else { 0. });
            Ok(snapshot)
        }
        "getLibraryStats" => {
            let folder = message.get("folderId").filter(|f| !f.is_null()).and_then(|f| f.as_str());
            let path = folder.map_or("/library/stats".to_owned(), |f| format!("/library/stats?folder_id={f}"));
            match helper(&path, Options::default()).await {
                Err(e) if e.status == 404 => {}
                other => return other,
            }
            if folder.is_some() {
                return Err("The helper needs an update to count files in this folder.".into());
            }
            let q = queue_status(Options::default()).await?;
            Ok(json!({"saved_here":q["saved"],"saved_videos":q["saved"],"unlocated":null,"moved_elsewhere":null,"in_trash":null,"availability":{"saved_here":true,"unlocated":false,"moved_elsewhere":false,"in_trash":false},"scan_complete":false,"inventory_complete":false,"details_available":false,"source":"verified_videos"}))
        }
        "openReader" => {
            let mut q = vec![];
            if let Some(view) = message["view"].as_str() {
                q.push(format!("view={view}"));
            }
            if let Some(id) = message["id"].as_str().filter(|i| !i.is_empty()) {
                q.push(format!("id={id}"));
            }
            let path = if q.is_empty() { "reader.html".to_owned() } else { format!("reader.html?{}", q.join("&")) };
            host::open(&path).await?;
            Ok(Value::Null)
        }
        "openReaderVideo" => {
            host::open(&format!("reader.html?video={}", message["videoId"].as_str().unwrap_or(""))).await?;
            Ok(Value::Null)
        }
        "openSettings" => {
            host::options().await?;
            Ok(Value::Null)
        }
        "openExportPage" => {
            host::open("popup.html?file=export").await?;
            Ok(Value::Null)
        }
        "connectHelper" => {
            pair().await?;
            let _ = settings_read(Options::default()).await;
            Ok(json!({"paired":true}))
        }
        "getSettings" => Ok(json!({"paired":!host::vault_read(TOKEN_KEY).await.is_empty()})),
        "saveSettings" => {
            let token = message["pairingToken"].as_str().unwrap_or("").trim();
            host::vault_write(TOKEN_KEY, token).await;
            Ok(json!({"paired":true}))
        }
        "forgetSettings" => {
            host::vault_remove(TOKEN_KEY).await;
            Ok(json!({"paired":false}))
        }
        _ => Err("Unknown extension request.".into()),
    }
}

/// A message from a tab or an extension page: `{"ok":true,"data":…}` or `{"ok":false,"error":…}`.
pub async fn on_message(message: &str, sender: &str) -> String {
    let message: Value = serde_json::from_str(message).unwrap_or(Value::Null);
    let sender: Value = serde_json::from_str(sender).unwrap_or(Value::Null);
    let trusted = match plan::sender_trust(&message, &sender, &host::runtime_id(), &host::runtime_base()) {
        Ok(t) => t,
        Err(e) => return json!({"ok":false,"error":e}).to_string(),
    };
    let result = route(&message, &sender, trusted).await;
    let refreshes = matches!(message["action"].as_str(), Some("startJob" | "startChannelImport" | "queueChannelPreview" | "cancelChannelPreview" | "cancelJob" | "cancelVideo" | "cancelQueue" | "exportJobs" | "connectHelper" | "saveSettings"));
    if refreshes {
        spawn_local(async {
            let _ = refresh_invalidate().await;
        });
    }
    match result {
        Ok(data) => json!({"ok":true,"data":data}).to_string(),
        Err(e) => json!({"ok":false,"error":if e.message.is_empty() { "Unexpected error.".to_owned() } else { e.message }}).to_string(),
    }
}

// ---------------------------------------------------------------- the toolbar

async fn init_toolbar() {
    if with(|w| w.toolbar.initialized) {
        return;
    }
    let stored: Value = serde_json::from_str(&host::vault_read(TOOLBAR_RECORD).await).unwrap_or(Value::Null);
    let life = core::lifecycle_restore(&stored, host::epoch());
    with(|w| {
        if !w.toolbar.initialized {
            w.toolbar.life = life;
            w.toolbar.initialized = true;
        }
    });
}

/// Writes the lifecycle, then the icon and title. One at a time, newest state last: an old response finishing late can
/// never restore an obsolete icon.
async fn commit() {
    if with(|w| {
        if w.toolbar.painting {
            w.toolbar.repaint = true;
            true
        } else {
            w.toolbar.painting = true;
            false
        }
    }) {
        return;
    }
    loop {
        with(|w| w.toolbar.repaint = false);
        let (view, life) = with(|w| (w.toolbar.view.clone(), w.toolbar.life.clone()));
        host::vault_write(TOOLBAR_RECORD, &life.to_string()).await;
        let check = view["kind"] == "complete" && life["completionUntil"].as_f64().unwrap_or(0.) > host::epoch();
        let title = core::toolbar_title(&view, check);
        let key = format!("{}|{}|{}|{}|{}|{}|{}", view["batchId"], view["kind"], view["saved"], view["total"], view["issues"], check, title);
        if with(|w| w.toolbar.last_paint != key) {
            let (a, b) = icon::render(&view, check);
            let painted = host::set_icon(&a, &b).await && host::set_title(&title).await;
            if painted {
                with(|w| w.toolbar.last_paint = key);
            }
        }
        if !with(|w| w.toolbar.repaint) {
            break;
        }
    }
    with(|w| w.toolbar.painting = false);
}

fn schedule() {
    let (token, kind, until, offline_again) = with(|w| {
        w.toolbar.timer += 1;
        let last_had_work = w.toolbar.snapshot.as_ref().is_some_and(|s| matches!(core::queue_state(s, false)["kind"].as_str(), Some("processing" | "queued")));
        (w.toolbar.timer, w.toolbar.view["kind"].as_str().unwrap_or("").to_owned(), w.toolbar.life["completionUntil"].as_f64().unwrap_or(0.), last_had_work && w.toolbar.offline_reads < MAX_OFFLINE_READS)
    });
    let active = matches!(kind.as_str(), "processing" | "queued");
    if active {
        host::alarm_on(ALARM, 0.5);
        spawn_local(async move {
            host::wait(5000.).await;
            if with(|w| w.toolbar.timer == token) {
                let _ = refresh_toolbar().await;
            }
        });
    } else if kind == "offline" && offline_again {
        host::alarm_on(ALARM, 0.5);
    } else {
        // Idle: nothing wakes this worker until someone asks.
        host::alarm_off(ALARM);
    }
    let now = host::epoch();
    if until > now {
        spawn_local(async move {
            host::wait(until - now).await;
            let stale = with(|w| {
                if w.toolbar.timer == token {
                    w.toolbar.life["completionUntil"] = json!(0);
                    false
                } else {
                    true
                }
            });
            if !stale {
                commit().await;
            }
        });
    }
}

/// Accepts a status (`None` with `unavailable`: the helper did not answer). False when a newer state won meanwhile.
async fn accept(value: Option<Value>, unavailable: bool, expected: Option<u64>) -> bool {
    init_toolbar().await;
    let current = with(|w| {
        if expected.is_some_and(|e| e != w.toolbar.revision) {
            return false;
        }
        w.toolbar.revision += 1;
        if !unavailable {
            w.toolbar.snapshot = value;
            w.toolbar.offline_reads = 0;
        } else {
            w.toolbar.offline_reads += 1;
        }
        let snapshot = w.toolbar.snapshot.clone().unwrap_or(Value::Null);
        w.toolbar.view = core::queue_state(&snapshot, unavailable);
        w.toolbar.life = core::lifecycle(&w.toolbar.view, &w.toolbar.life, host::epoch());
        true
    });
    if !current {
        return false;
    }
    commit().await;
    schedule();
    push_live().await;
    true
}

/// While work is active the tabs get each job's stage and real percent, by message rather than by polling; when it
/// ends they get one last message and everything goes quiet.
async fn push_live() {
    let (kind, had) = with(|w| (w.toolbar.view["kind"].as_str().unwrap_or("").to_owned(), w.live.as_object().is_some_and(|m| !m.is_empty())));
    let offline = kind == "offline";
    let live = if matches!(kind.as_str(), "processing" | "queued") {
        match helper("/jobs", passive()).await {
            Ok(r) => core::live_jobs(&r["jobs"]),
            Err(_) => with(|w| w.live.clone()),
        }
    } else if offline {
        with(|w| w.live.clone())
    } else {
        json!({})
    };
    let message = json!({"live":live,"offline":offline}).to_string();
    let changed = with(|w| {
        w.live = live.clone();
        let changed = w.last_push != message;
        if changed {
            w.last_push = message.clone();
        }
        changed
    });
    // A tab has nothing to learn from "still nothing" (the first read of an idle queue).
    if changed && (had || offline || live.as_object().is_some_and(|m| !m.is_empty())) {
        host::tabs_push(&format!("tsc:{message}"));
    }
}

async fn read_once() {
    let expected = with(|w| w.toolbar.revision);
    match queue_status(passive()).await {
        Ok(r) if r["total"].as_f64().is_some_and(|t| t >= 0.) => {
            if with(|w| w.toolbar.revision) == expected {
                accept(Some(r), false, Some(expected)).await;
            }
        }
        _ => {
            if with(|w| w.toolbar.revision) == expected {
                accept(None, true, Some(expected)).await;
            }
        }
    }
}

async fn refresh_invalidate() -> Result<(), Failure> {
    with(|w| {
        w.toolbar.revision += 1;
        w.toolbar.again = true;
    });
    refresh_toolbar().await
}

/// Reads the queue once; concurrent callers share the read and an invalidation queues one fresh read behind it, so a
/// slow poll cannot hide a newly accepted batch.
async fn refresh_toolbar() -> Result<(), Failure> {
    init_toolbar().await;
    let shared = with(|w| {
        if let Some(p) = &w.toolbar.pending {
            return p.clone();
        }
        let p = future_to_promise(async move {
            loop {
                with(|w| w.toolbar.again = false);
                read_once().await;
                if !with(|w| w.toolbar.again) {
                    break;
                }
            }
            with(|w| w.toolbar.pending = None);
            Ok(JsValue::UNDEFINED)
        });
        w.toolbar.pending = Some(p.clone());
        p
    });
    let _ = JsFuture::from(shared).await;
    Ok(())
}

/// The periodic wake while work is active.
pub async fn on_alarm() {
    let _ = refresh_toolbar().await;
}

/// The browser started and the feature is on: one read, in case work resumed. Never pairs or starts the helper.
pub async fn on_startup() {
    let _ = refresh_toolbar().await;
}

/// A token an older install left in `chrome.storage.local`, where content scripts could read it, moves into the vault.
pub async fn adopt_legacy_token(token: &str) {
    if valid_token(token) && host::vault_read(TOKEN_KEY).await.is_empty() {
        host::vault_write(TOKEN_KEY, token).await;
    }
}
