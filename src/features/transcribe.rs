//! Transcripts on YouTube's pages: the card chip, the watch page's Transcript button, the channel import controls. The
//! feature's logic lives in the extension's second Wasm (`transcripts/`, background worker and extension pages); this
//! file only asks the worker over `chrome.runtime` and shows what it answers. It never holds the pairing token and
//! never talks to the helper.
//!
//! Gating: `enabled()` is false until `chrome.storage.local` `ytcTranscriptsV1` says the feature is set up, and always
//! on TV. While false no chip, button or slot is written and no message is sent. With the flag on: one batched status
//! message per page render, nothing while idle, and the worker pushes live stages while jobs run.

use crate::{bridge, json, parse::Video, render, tv, util::{esc_into, is_video_id, next_id}};
use std::cell::RefCell;
use std::collections::HashMap;

pub const KEY: &str = "ytcTranscriptsV1";
pub const PREFIX: &str = "tsc:";
const CHANNEL: &str = "ytc-tsc-ch";
const PANEL: &str = "ytc-tsc-chp";
const WATCH: &str = "ytc-tsc-w";
const TOAST: &str = "ytc-tsc-toast";
const SEND_MAX: usize = 200;
const CHANNEL_POLL_MS: u32 = 1500;
const MAX_VIDEOS: usize = 3000;
const REMOVED: &str = "Removed from your save folder. Kept excluded so it is not downloaded again.";
/// How long one message stays up.
const TOAST_MS: u32 = 7000;

const I_LINES: &str = "<svg viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path class=\"tsc-l\" d=\"M2.5 4.5h11M2.5 8h11M2.5 11.5h7\"/><path class=\"tsc-c\" d=\"m3.5 8.5 3 3 6-7\"/><path class=\"tsc-x\" d=\"M3.5 3.5a6.4 6.4 0 1 0 9 9 6.4 6.4 0 0 0-9-9Zm0 0 9 9\"/><path class=\"tsc-r\" d=\"M13 8a5 5 0 1 1-1.5-3.6M13 2.5v3h-3\"/></svg>";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Pending {
    Queueing,
    Cancelling,
    Revealing,
}

/// What this tab knows about one video.
#[derive(Default)]
struct Known {
    status: &'static str,
    saved: bool,
    percent: Option<u32>,
    version: u64,
    pending: Option<Pending>,
    failed: bool,
    asked: bool,
    shown: String,
}

enum Reply {
    Status(Vec<(String, u64)>),
    Start(String),
    Cancel(String),
    Reveal(String),
    Reconcile(String, Pending, String),
    Plain,
    Channel(String),
}

/// A channel page's import: the worker's state (opaque here) and panel.
#[derive(Default)]
struct Channel {
    url: String,
    shorts: bool,
    state: String,
    html: String,
    poll: bool,
    busy: bool,
    /// The panel was closed: the import goes on in the helper, but this tab stops watching until it is asked again.
    closed: bool,
}

#[derive(Default)]
struct T {
    on: bool,
    videos: HashMap<String, Known>,
    offline: bool,
    version: u64,
    scan: Option<u32>,
    replies: Vec<(u32, Reply)>,
    toast: Option<u32>,
    channels: Vec<Channel>,
    channel_timer: Option<u32>,
    in_flight: usize,
    again: bool,
}

thread_local! {
    static S: RefCell<T> = RefCell::new(T::default());
}
fn with<R>(f: impl FnOnce(&mut T) -> R) -> R {
    S.with(|s| f(&mut s.borrow_mut()))
}

/// The feature is set up, and this is not the TV app.
pub fn enabled() -> bool {
    !tv::on() && with(|t| t.on)
}

/// Reads the flag (once per document; a tab keeps listening so setting it up in another tab shows up here).
pub fn boot() {
    if tv::on() {
        return;
    }
    bridge::storage_get(KEY);
    bridge::storage_watch(KEY);
}

fn is_on(json_text: &str) -> bool {
    json::parse_str(json_text).as_ref().and_then(|v| v.get("on")).and_then(json::Value::as_bool).unwrap_or(false)
}

/// chrome.storage answered; true when the key was ours.
pub fn on_storage(key: &str, json_text: &str) -> bool {
    if key != KEY {
        return false;
    }
    set_on(is_on(json_text));
    true
}

/// The flag changed in another context.
pub fn on_storage_change(key: &str, json_text: &str) -> bool {
    on_storage(key, json_text)
}

fn set_on(on: bool) {
    let was = with(|t| std::mem::replace(&mut t.on, on));
    if tv::on() || was == on {
        return;
    }
    if on {
        // The first page may have painted before storage answered: dress what is there, without a re-render.
        bridge::decorate(".c[data-v]", ".th", ".tsc", "data-v", &chip_template());
        fill_watch();
        fill_channel();
        schedule_scan(1);
    } else {
        bridge::remove_all(".tsc");
        bridge::remove(WATCH);
        bridge::remove(CHANNEL);
        with(|t| t.videos.clear());
    }
}

// ------------------------------------------------------------ what a video shows

struct Look {
    /// The chip's state attribute (`""`: idle, shown only on hover and focus).
    s: &'static str,
    /// The chip's short text.
    chip: String,
    /// The watch button's text.
    label: String,
    /// Tooltip and accessible name.
    title: String,
    percent: Option<u32>,
}

fn active(status: &str) -> bool {
    matches!(status, "queued" | "downloading" | "preparing" | "transcribing")
}

/// The helper's status as this file's one spelling (`""`: unknown).
fn known_status(s: &str) -> &'static str {
    match s {
        "queued" => "queued",
        "downloading" => "downloading",
        "preparing" => "preparing",
        "transcribing" => "transcribing",
        "completed" => "completed",
        "failed" => "failed",
        "cancelled" => "cancelled",
        "removed" => "removed",
        _ => "",
    }
}

fn stage(status: &str) -> &'static str {
    match status {
        "queued" => "Queued",
        "downloading" => "Downloading",
        "preparing" => "Preparing",
        _ => "Transcribing",
    }
}

fn look(v: Option<&Known>, offline: bool) -> Look {
    let status = v.map_or("", |k| k.status);
    let saved = v.is_some_and(|k| k.saved);
    let simple = |s, chip: &str, label: &str, title: &str| Look { s, chip: chip.into(), label: label.into(), title: title.into(), percent: None };
    if status == "removed" {
        return simple("rm", "", "Removed", REMOVED);
    }
    match v.and_then(|k| k.pending) {
        Some(Pending::Queueing) => return simple("wait", "Adding", "Adding", "Adding this video to your queue. You can keep browsing"),
        Some(Pending::Cancelling) => return simple("wait", "Stopping", "Stopping", "Waiting for the helper to stop this video"),
        Some(Pending::Revealing) => return simple("wait", "Opening", "Opening", "Showing your transcript file"),
        None => {}
    }
    if v.is_some_and(|k| k.failed) {
        return simple("er", "", "Retry transcript", "This transcript did not start. Click to try again");
    }
    if active(status) {
        let word = stage(status);
        if offline {
            return Look { s: "un", chip: word.into(), label: "Status unavailable".into(), title: format!("Last known stage: {word}. The helper’s status is unavailable"), percent: None };
        }
        let percent = v.and_then(|k| k.percent).filter(|_| status != "queued");
        let (chip, label) = match (status, percent) {
            ("transcribing", Some(p)) => (format!("{p}%"), format!("Transcribing {p}%")),
            _ => (word.to_owned(), word.to_owned()),
        };
        return Look { s: "on", chip, title: format!("{label}. Click to cancel this video’s transcript"), label, percent };
    }
    match status {
        "completed" if saved => simple("ok", "", "Transcript saved", "Transcript saved. Show the file"),
        "completed" => simple("ok", "", "Transcribed", "Transcribed. Save the existing text and show the file"),
        "cancelled" => simple("", "", "Transcript", "Cancelled. Click to transcribe this video again"),
        "failed" => simple("er", "", "Retry transcript", "This transcript needs attention. Click to try again"),
        _ => simple("", "", "Transcript", "Full transcript"),
    }
}

fn look_of(id: &str) -> Look {
    with(|t| look(t.videos.get(id), t.offline))
}

// ------------------------------------------------------------ markup

fn state_attrs(out: &mut String, l: &Look) {
    if !l.s.is_empty() {
        out.push_str(" data-s=\"");
        out.push_str(l.s);
        out.push('"');
    }
    if let Some(p) = l.percent {
        out.push_str(&format!(" style=\"--p:{p}%\""));
    }
    out.push_str(" title=\"");
    esc_into(out, &l.title);
    out.push_str("\" aria-label=\"");
    esc_into(out, &l.title);
    out.push_str("\">");
    out.push_str(I_LINES);
}

/// The chip of a video card or playlist row, inside its thumbnail. Nothing at all while the feature is off.
pub fn card_chip(out: &mut String, v: &Video) {
    playlist_chip(out, &v.id);
}

pub fn playlist_chip(out: &mut String, id: &str) {
    if !enabled() || id.is_empty() {
        return;
    }
    let l = look_of(id);
    out.push_str("<button type=\"button\" class=\"tsc\" data-act=\"tsc\" data-arg=\"");
    esc_into(out, id);
    out.push('"');
    state_attrs(out, &l);
    out.push_str("<span class=\"tsc-t\">");
    esc_into(out, &l.chip);
    out.push_str("</span></button>");
    schedule_scan(120);
}

fn chip_template() -> String {
    format!("<button type=\"button\" class=\"tsc\" data-act=\"tsc\" data-arg=\"{{}}\" title=\"Full transcript\" aria-label=\"Full transcript\">{I_LINES}<span class=\"tsc-t\"></span></button>")
}

fn current_video() -> Option<String> {
    let href = bridge::href();
    let (path, q) = href.split_once('?')?;
    (path == "/watch").then_some(())?;
    q.split('&').find_map(|p| p.strip_prefix("v=")).map(|v| v.split('#').next().unwrap_or(v).to_owned()).filter(|v| is_video_id(v))
}

fn watch_markup(id: &str) -> String {
    let l = look_of(id);
    let mut s = format!("<button type=\"button\" id=\"{WATCH}\" class=\"ya-b tsc-w\" data-act=\"tsc-w\" data-arg=\"{id}\"");
    state_attrs(&mut s, &l);
    s.push_str("<span>");
    esc_into(&mut s, &l.label);
    s.push_str("</span></button>");
    s
}

/// The watch page's Transcript button, in the action row after Share and Save.
pub fn watch_button(out: &mut String) {
    if !enabled() {
        return;
    }
    if let Some(id) = current_video() {
        out.push_str(&watch_markup(&id));
        schedule_scan(120);
    }
}

fn fill_watch() {
    if bridge::any(".ya-slot") && !bridge::any(&format!("#{WATCH}"))
        && let Some(id) = current_video()
    {
        bridge::append(render::ACTIONS, &watch_markup(&id));
    }
}

// ------------------------------------------------------------ status

/// A new page is on screen: its cards are asked about once more, in one batch.
pub fn page_changed() {
    if !enabled() {
        return;
    }
    with(|t| {
        if t.videos.len() > MAX_VIDEOS {
            t.videos.retain(|_, v| active(v.status) || v.pending.is_some() || v.failed);
        }
        for v in t.videos.values_mut() {
            v.asked = false;
            // A page restored from memory may show an older state: the next answer paints it again.
            v.shown.clear();
        }
    });
    schedule_scan(120);
}

fn schedule_scan(delay: u32) {
    let id = with(|t| {
        if t.scan.is_some() {
            return None;
        }
        let id = next_id();
        t.scan = Some(id);
        Some(id)
    });
    if let Some(id) = id {
        bridge::idle(id, delay);
    }
}

fn worker(reply: Reply, message: &str) {
    let id = next_id();
    with(|t| t.replies.push((id, reply)));
    bridge::worker_send(id, message);
}

fn video_message(action: &str, id: &str) -> String {
    format!("{{\"action\":\"{action}\",\"videoId\":\"{id}\"}}")
}

/// Asks about every video on the page not asked about yet; at most one batch in flight.
fn scan() {
    if !enabled() {
        return;
    }
    if with(|t| t.in_flight > 0) {
        with(|t| t.again = true);
        return;
    }
    let listed = bridge::attr_list(".tsc[data-arg],.tsc-w[data-arg]", "data-arg");
    let ids = with(|t| {
        let mut ids: Vec<String> = vec![];
        for id in listed.split('\n').filter(|s| is_video_id(s)) {
            let v = t.videos.entry(id.to_owned()).or_default();
            if !v.asked {
                v.asked = true;
                ids.push(id.to_owned());
            }
        }
        ids
    });
    if ids.is_empty() {
        return;
    }
    let chunks: Vec<&[String]> = ids.chunks(SEND_MAX).collect();
    with(|t| t.in_flight = chunks.len());
    for chunk in chunks {
        let versions: Vec<(String, u64)> = with(|t| chunk.iter().map(|id| (id.clone(), t.videos.get(id).map_or(0, |k| k.version))).collect());
        worker(Reply::Status(versions), &format!("{{\"action\":\"getVideoStatuses\",\"videoIds\":[\"{}\"]}}", chunk.join("\",\"")));
    }
}

/// Shows a video's state on every chip and button of it (nothing when it did not change).
fn paint(id: &str) {
    let l = look_of(id);
    let key = format!("{}|{}|{}|{}", l.s, l.chip, l.label, l.title);
    let same = with(|t| {
        let v = t.videos.entry(id.to_owned()).or_default();
        let same = v.shown == key;
        v.shown = key;
        same
    });
    if same {
        return;
    }
    let sel = format!("[data-act^=tsc][data-arg=\"{id}\"]");
    bridge::attr_all(&sel, "data-s", (!l.s.is_empty()).then(|| l.s.to_owned()));
    bridge::attr_all(&sel, "title", Some(l.title.clone()));
    bridge::attr_all(&sel, "aria-label", Some(l.title));
    bridge::attr_all(&sel, "style", l.percent.map(|p| format!("--p:{p}%")));
    bridge::text_all(&format!(".tsc[data-arg=\"{id}\"] .tsc-t"), &l.chip);
    bridge::text_all(&format!(".tsc-w[data-arg=\"{id}\"] span"), &l.label);
}

fn set_known(id: &str, status: &str, saved: bool, percent: Option<u32>, local: bool) {
    with(|t| {
        let next = if local {
            t.version += 1;
            Some(t.version)
        } else {
            None
        };
        let v = t.videos.entry(id.to_owned()).or_default();
        // A removal is final for this page: an older answer cannot bring a video back.
        if v.status == "removed" && status != "removed" {
            return;
        }
        v.status = known_status(status);
        v.saved = saved;
        v.percent = percent;
        if let Some(n) = next {
            v.version = n;
        }
    });
}

fn path<'a, 'b>(v: Option<&'a json::Value<'b>>, keys: &[&str]) -> Option<&'a json::Value<'b>> {
    keys.iter().fold(v, |at, k| at.and_then(|x| x.get(*k)))
}

fn on_status(batch: Vec<(String, u64)>, reply: Option<&json::Value<'_>>) {
    let ok = path(reply, &["ok"]).and_then(json::Value::as_bool) == Some(true);
    if ok {
        with(|t| t.offline = false);
        // Most videos have no transcript: an idle answer for a chip already idle touches nothing.
        let dressed = bridge::attr_list("[data-act^=tsc][data-s]", "data-arg");
        for (id, version) in &batch {
            // A later click's result is never erased by an older lookup.
            let skip = with(|t| t.videos.get(id).is_some_and(|k| k.pending.is_some() || k.version != *version));
            if skip {
                continue;
            }
            let found = path(reply, &["data", "statuses", id]);
            let status = path(found, &["status"]).and_then(json::Value::as_str).unwrap_or("");
            if matches!(status, "completed" | "removed") || active(status) {
                with(|t| {
                    if let Some(v) = t.videos.get_mut(id) {
                        v.failed = false;
                    }
                });
            }
            let percent = path(reply, &["data", "live", id, "p"]).and_then(json::Value::as_f64).map(|p| p as u32);
            set_known(id, status, path(found, &["saved"]).and_then(json::Value::as_bool) == Some(true), percent, false);
            if !look_of(id).s.is_empty() || dressed.split('\n').any(|d| d == id) || status == "cancelled" {
                paint(id);
            }
        }
    } else {
        // Quiet: the cards keep what they show, and the next page render asks again.
        with(|t| {
            t.offline = batch.iter().any(|(id, _)| t.videos.get(id).is_some_and(|k| active(k.status)));
            for (id, _) in &batch {
                if let Some(v) = t.videos.get_mut(id) {
                    v.asked = false;
                }
            }
        });
        for (id, _) in &batch {
            paint(id);
        }
    }
    let again = with(|t| {
        t.in_flight = t.in_flight.saturating_sub(1);
        t.in_flight == 0 && std::mem::replace(&mut t.again, false)
    });
    if again {
        schedule_scan(120);
    }
}

/// A live message from the worker: the unfinished jobs' stages, or that the helper stopped answering.
pub fn on_live(text: &str) {
    if !enabled() {
        return;
    }
    let parsed = json::parse_str(text);
    let root = parsed.as_ref();
    let offline = path(root, &["offline"]).and_then(json::Value::as_bool) == Some(true);
    let live = path(root, &["live"]);
    let (changed, gone) = with(|t| {
        let changed = std::mem::replace(&mut t.offline, offline) != offline;
        let mut gone: Vec<String> = vec![];
        for (id, v) in &mut t.videos {
            if active(v.status) && v.pending.is_none() && path(live, &[id]).is_none() {
                v.asked = false;
                gone.push(id.clone());
            }
        }
        (changed, gone)
    });
    if let Some(entries) = live.and_then(json::Value::as_object) {
        for (id, job) in entries.iter() {
            let id: &str = id.as_ref();
            if !is_video_id(id) || with(|t| t.videos.get(id).is_some_and(|k| k.pending.is_some())) {
                continue;
            }
            let status = job.get("s").and_then(json::Value::as_str).unwrap_or("");
            set_known(id, status, false, job.get("p").and_then(json::Value::as_f64).map(|p| p as u32), false);
            paint(id);
        }
    }
    if !offline && !gone.is_empty() {
        // A job left the list: ask what became of it (saved, failed, cancelled).
        schedule_scan(1);
    }
    if changed {
        let ids: Vec<String> = with(|t| t.videos.iter().filter(|(_, k)| active(k.status)).map(|(id, _)| id.clone()).collect());
        ids.iter().for_each(|id| paint(id));
    }
}

// ------------------------------------------------------------ actions

fn toast(message: &str) {
    if !bridge::any(&format!("#{TOAST}")) {
        bridge::append("ytc", &format!("<div id=\"{TOAST}\" class=\"tsc-toast\" role=\"status\"></div>"));
    }
    bridge::text(TOAST, message);
    let id = next_id();
    with(|t| t.toast = Some(id));
    bridge::timer(id, TOAST_MS);
}

/// A click a page script made, not a person (the bridge's bit): these acts spend the person's money, so it does nothing.
const UNTRUSTED: u32 = 128;

/// A click on a chip (`tsc`), the watch button (`tsc-w`) or the channel controls (`tsc-…`).
pub fn on_click(act: &str, arg: &str, mods: u32) -> bool {
    if !enabled() {
        return false;
    }
    if mods & UNTRUSTED != 0 {
        return true;
    }
    match act {
        "tsc" | "tsc-w" => video_click(arg, act == "tsc-w"),
        _ => channel_click(act, arg),
    }
    true
}

fn video_click(id: &str, watch_button: bool) {
    if !is_video_id(id) {
        return;
    }
    let (pending, status, saved, percent, offline) = with(|t| {
        let v = t.videos.get(id);
        (v.is_some_and(|k| k.pending.is_some()), v.map_or("", |k| k.status), v.is_some_and(|k| k.saved), v.and_then(|k| k.percent), t.offline)
    });
    if pending {
        return;
    }
    if status == "removed" {
        return toast(REMOVED);
    }
    if active(status) && offline {
        toast("Checking the helper for this video’s latest status");
        with(|t| {
            t.videos.entry(id.to_owned()).or_default().asked = false;
        });
        return schedule_scan(1);
    }
    // The watch page's button opens a saved transcript to read it; a card reveals the file.
    if watch_button && status == "completed" && saved {
        return worker(Reply::Plain, &video_message("openReaderVideo", id));
    }
    let kind = if active(status) {
        Pending::Cancelling
    } else if status == "completed" {
        Pending::Revealing
    } else {
        Pending::Queueing
    };
    with(|t| {
        let v = t.videos.entry(id.to_owned()).or_default();
        v.pending = Some(kind);
        v.failed = false;
        t.offline = false;
    });
    set_known(id, status, saved, percent, true);
    paint(id);
    let owned = id.to_owned();
    match kind {
        Pending::Cancelling => worker(Reply::Cancel(owned), &video_message("cancelVideo", id)),
        Pending::Revealing => worker(Reply::Reveal(owned), &video_message("revealVideoFile", id)),
        Pending::Queueing => worker(Reply::Start(owned), &format!("{{\"action\":\"startJob\",\"url\":\"https://www.youtube.com/watch?v={id}\"}}")),
    }
}

fn gone_message(error: &str) -> String {
    if bridge::ext_gone() {
        "The extension was updated. Reload YouTube and try again.".into()
    } else if error.is_empty() {
        "The transcript request could not finish. Try again.".into()
    } else {
        error.to_owned()
    }
}

fn settle(id: &str) {
    with(|t| {
        if let Some(v) = t.videos.get_mut(id) {
            v.pending = None;
        }
    });
    paint(id);
}

/// A paid or file-writing action failed: reconcile with a read; it is never retried here.
fn action_failed(id: String, kind: Pending, error: String) {
    let message = format!("{{\"action\":\"getVideoStatuses\",\"videoIds\":[\"{id}\"]}}");
    worker(Reply::Reconcile(id, kind, error), &message);
}

/// The worker answered a message.
pub fn on_reply(id: u32, text: &str) {
    let Some(reply) = with(|t| t.replies.iter().position(|(i, _)| *i == id).map(|at| t.replies.swap_remove(at).1)) else { return };
    let parsed = json::parse_str(text);
    let root = parsed.as_ref();
    let ok = path(root, &["ok"]).and_then(json::Value::as_bool) == Some(true);
    let error = path(root, &["error"]).and_then(json::Value::as_str).unwrap_or("").to_owned();
    let data = path(root, &["data"]);
    let status_of = || path(data, &["status"]).and_then(json::Value::as_str).unwrap_or("");
    match reply {
        Reply::Status(batch) => on_status(batch, root),
        Reply::Plain => {
            if !ok {
                toast(&gone_message(&error));
            }
        }
        Reply::Start(video) => {
            if !ok {
                return action_failed(video, Pending::Queueing, error);
            }
            let status = if known_status(status_of()).is_empty() { "queued" } else { status_of() };
            set_known(&video, status, false, None, true);
            toast(match status {
                "removed" => REMOVED,
                "completed" => "Already transcribed. Click its check mark to show the file.",
                _ => "Added to your transcript queue. Keep browsing; the library has it later.",
            });
            settle(&video);
            ask_again(&video);
        }
        Reply::Cancel(video) => {
            if !ok {
                return action_failed(video, Pending::Cancelling, error);
            }
            let status = status_of();
            set_known(&video, status, status == "completed" && path(data, &["saved"]).and_then(json::Value::as_bool) == Some(true), None, true);
            toast(match status {
                "removed" => REMOVED,
                "completed" => "This transcript already finished. Click its check mark to show the file.",
                _ => "Transcript cancelled.",
            });
            settle(&video);
        }
        Reply::Reveal(video) => {
            if !ok || path(data, &["revealed"]).and_then(json::Value::as_bool) != Some(true) {
                return action_failed(video, Pending::Revealing, if error.is_empty() { "The transcript file could not be shown. Check your save folder in Settings.".into() } else { error });
            }
            set_known(&video, "completed", true, None, true);
            toast("Transcript file shown.");
            settle(&video);
        }
        Reply::Reconcile(video, kind, original) => {
            let found = path(data, &["statuses", &video]).filter(|_| ok);
            let current = path(found, &["status"]).and_then(json::Value::as_str).unwrap_or("");
            let saved = path(found, &["saved"]).and_then(json::Value::as_bool) == Some(true);
            if current == "removed" {
                with(|t| {
                    if let Some(v) = t.videos.get_mut(&video) {
                        v.failed = false;
                    }
                });
                set_known(&video, "removed", false, None, true);
                toast(REMOVED);
                return settle(&video);
            }
            if kind == Pending::Cancelling && !known_status(current).is_empty() {
                set_known(&video, current, saved, None, true);
                if matches!(current, "cancelled" | "completed") {
                    return settle(&video);
                }
            }
            if kind == Pending::Queueing {
                with(|t| t.videos.entry(video.clone()).or_default().failed = true);
            }
            toast(&gone_message(&original));
            settle(&video);
        }
        Reply::Channel(url) => channel_reply(&url, ok, data, &error),
    }
}

fn ask_again(id: &str) {
    with(|t| t.videos.entry(id.to_owned()).or_default().asked = false);
    schedule_scan(1);
}

/// A timer fired; true when it was ours.
pub fn on_timer(id: u32) -> bool {
    let (scan_due, toast_due, channel_due) = with(|t| {
        let scan_due = t.scan == Some(id);
        if scan_due {
            t.scan = None;
        }
        let toast_due = t.toast == Some(id);
        if toast_due {
            t.toast = None;
        }
        let channel_due = t.channel_timer == Some(id);
        if channel_due {
            t.channel_timer = None;
        }
        (scan_due, toast_due, channel_due)
    });
    if scan_due {
        scan();
        render_channel_here();
    }
    if toast_due {
        bridge::remove(TOAST);
    }
    if channel_due {
        channel_poll();
    }
    scan_due || toast_due || channel_due
}

// ------------------------------------------------------------ channel import

/// The channel of the current page: `https://www.youtube.com/@name`, `/channel/id`, `/c/name`, `/user/name`.
fn channel_of(path: &str) -> Option<String> {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut parts = path.split('/').skip(1);
    let first = parts.next()?;
    if first.starts_with('@') && first.len() > 1 {
        return Some(format!("https://www.youtube.com/{first}"));
    }
    if matches!(first, "channel" | "c" | "user") {
        let second = parts.next().filter(|s| !s.is_empty())?;
        return Some(format!("https://www.youtube.com/{first}/{second}"));
    }
    None
}

fn channel_url() -> Option<String> {
    channel_of(&bridge::href())
}

fn channel_markup(url: &str) -> String {
    let shorts = with(|t| t.channels.iter().any(|c| c.url == url && c.shorts));
    format!(
        "<div id=\"{CHANNEL}\" class=\"tsc-ch\"><div class=\"tsc-chr\" role=\"group\" aria-label=\"Import channel transcripts\"><span class=\"m\">Transcribe channel</span><button type=\"button\" class=\"ya-b tsc-b\" data-act=\"tsc-import\" data-arg=\"all\" title=\"Queue all of this channel’s videos\">All videos</button><button type=\"button\" class=\"ya-b tsc-b\" data-act=\"tsc-import\" data-arg=\"year\" title=\"Queue this channel’s videos from the past year\">Past year</button><button type=\"button\" id=\"ytc-tsc-shorts\" class=\"tsc-sw\" data-act=\"tsc-shorts\" role=\"switch\" aria-checked=\"{shorts}\" title=\"Include Shorts in the next channel import\"><i aria-hidden=\"true\"></i>Include Shorts</button></div><p class=\"m tsc-chnote\">Queues every video it finds, counted below as it goes. Your Azure resource bills each one.</p><section id=\"{PANEL}\" class=\"tsc-chp\" aria-label=\"Channel import progress\" hidden></section></div>"
    )
}

/// The channel page's controls, under its header.
pub fn channel_slot(out: &mut String) {
    if !enabled() {
        return;
    }
    if let Some(url) = channel_url() {
        out.push_str(&channel_markup(&url));
        schedule_scan(120);
    }
}

fn fill_channel() {
    if bridge::any(".ch") && !bridge::any(&format!("#{CHANNEL}")) {
        if let Some(url) = channel_url() {
            bridge::decorate(".feed", ".ch", ".tsc-ch", "class", &channel_markup(&url));
        }
    }
}

fn channel_of_page<R>(url: &str, f: impl FnOnce(&mut Channel) -> R) -> R {
    with(|t| {
        let at = t.channels.iter().position(|c| c.url == url).unwrap_or_else(|| {
            t.channels.push(Channel { url: url.to_owned(), ..Channel::default() });
            t.channels.len() - 1
        });
        f(&mut t.channels[at])
    })
}

fn channel_send(url: &str, op: &str, extra: &str) {
    let state = channel_of_page(url, |c| {
        c.busy = true;
        serde_json::to_string(&c.state).unwrap_or_default()
    });
    worker(Reply::Channel(url.to_owned()), &format!("{{\"action\":\"channelPanel\",\"op\":\"{op}\",\"state\":{state}{extra}}}"));
}

fn channel_click(act: &str, arg: &str) {
    let Some(url) = channel_url() else { return };
    match act {
        "tsc-shorts" => {
            let on = channel_of_page(&url, |c| {
                c.shorts = !c.shorts;
                c.shorts
            });
            bridge::attr("ytc-tsc-shorts", "aria-checked", if on { "true" } else { "false" });
        }
        "tsc-chclose" => close_panel(&url),
        "tsc-import" if matches!(arg, "all" | "year") => {
            if channel_of_page(&url, |c| c.busy) {
                return;
            }
            let shorts = channel_of_page(&url, |c| {
                c.closed = false;
                c.shorts
            });
            bridge::attr_remove(PANEL, "hidden");
            if !bridge::any(&format!("#{PANEL} .tsc-cht")) {
                bridge::html(PANEL, "<p class=\"tsc-cht\" role=\"status\">Starting import</p>");
            }
            channel_send(&url, "start", &format!(",\"url\":\"{url}\",\"scope\":\"{arg}\",\"shorts\":{shorts}"));
        }
        // One request at a time: a double click on Retry must not start a second import.
        "tsc-chretry" if !channel_of_page(&url, |c| c.busy) => channel_send(&url, "retry", ""),
        "tsc-chcancel" if !channel_of_page(&url, |c| c.busy) => channel_send(&url, "cancel", ""),
        _ => {}
    }
}

fn close_panel(url: &str) {
    channel_of_page(url, |c| {
        c.closed = true;
        c.poll = false;
    });
    bridge::attr(PANEL, "hidden", "");
}

/// Escape closes an open channel panel; true when it did.
pub fn on_escape() -> bool {
    if !enabled() || !bridge::any(&format!("#{PANEL}:not([hidden])")) {
        return false;
    }
    let Some(url) = channel_url() else { return false };
    close_panel(&url);
    true
}

fn channel_reply(url: &str, ok: bool, data: Option<&json::Value<'_>>, error: &str) {
    let text = |k: &str| data.and_then(|d| d.get(k)).and_then(json::Value::as_str).unwrap_or("").to_owned();
    let again = data.and_then(|d| d.get("poll")).and_then(json::Value::as_bool).unwrap_or(false);
    let (html, toast_text) = (text("html"), text("toast"));
    channel_of_page(url, |c| {
        c.busy = false;
        if ok {
            c.state = text("state");
            c.html = html.clone();
            c.poll = again && !c.closed;
        } else {
            c.poll = false;
        }
    });
    if !ok {
        toast(&gone_message(error));
        return;
    }
    if channel_url().as_deref() == Some(url) && !channel_of_page(url, |c| c.closed) {
        show_panel(url);
        if !toast_text.is_empty() {
            toast(&toast_text);
            with(|t| t.videos.values_mut().for_each(|v| v.asked = false));
            schedule_scan(1);
        }
        if again {
            schedule_channel_poll();
        }
    }
}

/// Writes the panel (only when it changed, keeping the keyboard's place).
fn show_panel(url: &str) {
    let html = with(|t| t.channels.iter().find(|c| c.url == url).map(|c| c.html.clone())).unwrap_or_default();
    if html.is_empty() || !bridge::any(&format!("#{PANEL}")) {
        return;
    }
    if bridge::get_html(PANEL) != html {
        bridge::focus_save("tsc");
        bridge::html(PANEL, &html);
        bridge::focus_load("tsc");
    }
}

fn schedule_channel_poll() {
    if with(|t| t.channel_timer.is_some()) {
        return;
    }
    let id = next_id();
    with(|t| t.channel_timer = Some(id));
    bridge::timer(id, CHANNEL_POLL_MS);
}

fn channel_poll() {
    let Some(url) = channel_url() else { return };
    if channel_of_page(&url, |c| c.poll && !c.busy) {
        channel_send(&url, "poll", "");
    }
}

/// A page with this channel's slot is on screen again: its panel and its polling come back.
fn render_channel_here() {
    let Some(url) = channel_url() else { return };
    let (has, poll, closed) = with(|t| t.channels.iter().find(|c| c.url == url).map_or((false, false, false), |c| (!c.html.is_empty(), c.poll, c.closed)));
    if closed {
        return;
    }
    if has && poll && bridge::any(&format!("#{PANEL}[hidden]")) {
        bridge::attr_remove(PANEL, "hidden");
    }
    if has {
        show_panel(&url);
    }
    if poll {
        schedule_channel_poll();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(status: &'static str, saved: bool, percent: Option<u32>) -> Known {
        Known { status, saved, percent, ..Known::default() }
    }
    #[test]
    fn an_idle_video_is_only_a_hover_control() {
        let l = look(None, false);
        assert_eq!((l.s, l.chip.as_str(), l.title.as_str()), ("", "", "Full transcript"));
    }
    #[test]
    fn live_stages_show_real_progress() {
        for (status, chip) in [("queued", "Queued"), ("downloading", "Downloading"), ("preparing", "Preparing")] {
            let l = look(Some(&k(status, false, Some(3))), false);
            assert_eq!((l.s, l.chip.as_str()), ("on", chip));
            assert!(l.title.contains("cancel"));
        }
        let l = look(Some(&k("transcribing", false, Some(42))), false);
        assert_eq!((l.chip.as_str(), l.label.as_str(), l.percent), ("42%", "Transcribing 42%", Some(42)));
        let l = look(Some(&k("transcribing", false, None)), false);
        assert_eq!(l.chip, "Transcribing");
    }
    #[test]
    fn a_stopped_helper_is_quiet() {
        let l = look(Some(&k("transcribing", false, Some(42))), true);
        assert_eq!((l.s, l.label.as_str(), l.percent), ("un", "Status unavailable", None));
        assert!(!l.title.contains("cancel"));
    }
    #[test]
    fn saved_transcribed_removed_and_failed() {
        assert_eq!(look(Some(&k("completed", true, None)), false).label, "Transcript saved");
        let t = look(Some(&k("completed", false, None)), false);
        assert_eq!(t.label, "Transcribed");
        assert!(t.title.contains("Save the existing text"));
        assert_eq!(look(Some(&k("removed", false, None)), false).s, "rm");
        let mut pending = k("removed", false, None);
        pending.pending = Some(Pending::Queueing);
        assert_eq!(look(Some(&pending), false).s, "rm");
        let failed = Known { failed: true, ..Known::default() };
        assert_eq!(look(Some(&failed), false).label, "Retry transcript");
        assert!(look(Some(&k("cancelled", false, None)), false).title.contains("again"));
    }
    #[test]
    fn channel_pages_only() {
        assert_eq!(channel_of("/@name/videos?x=1").as_deref(), Some("https://www.youtube.com/@name"));
        assert_eq!(channel_of("/channel/UC1").as_deref(), Some("https://www.youtube.com/channel/UC1"));
        assert_eq!(channel_of("/c/x/about").as_deref(), Some("https://www.youtube.com/c/x"));
        for p in ["/", "/watch?v=abcdefghijk", "/@", "/channel/", "/feed/history"] {
            assert_eq!(channel_of(p), None, "{p}");
        }
    }
    #[test]
    fn the_flag_reads_strictly() {
        assert!(is_on(r#"{"on":true}"#));
        for j in ["null", "", "{}", r#"{"on":1}"#, "true"] {
            assert!(!is_on(j), "{j}");
        }
    }
}
