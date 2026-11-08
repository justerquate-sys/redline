//! The toolbar popup's Transcripts section, only for someone who set the feature up: the current batch with Stop all,
//! the folder cards (destination, counts, Browse files, Copy as a ZIP attachment, Folders, Show file) and the way to the
//! library. `src/popup.rs` in the client decides whether this runs at all.

use crate::core;
use crate::html::{self, esc, icon};
use crate::host;
use crate::ui::{self, disable, num, plural, put, send, set, show};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use wasm_bindgen_futures::spawn_local;

const ZIP_PREFIX: &str = "allZipExclusions:";

#[derive(Default, Clone)]
struct Stat {
    data: Option<Value>,
    error: Option<String>,
    checked_at: Option<f64>,
    pending: bool,
}
#[derive(Default, Clone)]
struct Feedback {
    state: String,
    message: String,
    attachment: Option<Value>,
    seq: u64,
}

#[derive(Default)]
struct P {
    snapshot: Option<Value>,
    unavailable: bool,
    stopping: bool,
    stats: HashMap<String, Stat>,
    generation: u64,
    context: String,
    stats_busy: bool,
    visible: Vec<String>,
    destination: Option<Value>,
    folders: Vec<Value>,
    default_folder: String,
    library_ready: bool,
    folder_busy: bool,
    folder_rev: u64,
    queue_rev: u64,
    reveal: Option<String>,
    seq: u64,
    browsing: HashSet<String>,
    browse: HashMap<String, Feedback>,
    copying: Option<String>,
    copies: HashMap<String, Feedback>,
    revealing: HashSet<String>,
    export: Option<Value>,
    export_error: String,
    export_busy: bool,
    select_busy: bool,
    select_rev: u64,
    excluded: BTreeSet<String>,
    unresolved: Vec<String>,
    zip_view: bool,
    in_flight: HashMap<&'static str, bool>,
}

thread_local! {
    static STATE: RefCell<P> = RefCell::new(P::default());
}
fn with<R>(f: impl FnOnce(&mut P) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

pub fn boot() {
    host::html("popup-tr", &shell());
    refresh();
}

fn shell() -> String {
    format!(
        r##"<div class="ptr-head"><b>Transcripts</b><span class="ptr-tools"><button type="button" id="settings" class="ptr-i" data-tx="settings" aria-label="Transcripts settings" title="Settings">{i_settings}</button><button type="button" id="library" class="ptr-link" data-tx="library" title="Read, copy and save your transcripts">Library</button></span></div>
<div id="ptr-main"><section id="queue-card" class="ptr-queue" aria-label="Current batch"><div class="ptr-line"><span id="queue-stage" role="status">Connecting</span><span id="queue-summary" class="ptr-num"></span></div><div id="queue-progress" class="tx-bar" role="progressbar" aria-label="Saved transcripts in the current batch" aria-valuemin="0"><i id="queue-fill"></i></div><div class="ptr-line"><span id="queue-counts" class="ptr-sub"></span><button type="button" id="queue-stop" class="ptr-link" data-tx="stop" title="Stop channel discovery and all unfinished transcriptions. Saved files stay" hidden>Stop all</button></div><div id="queue-attention" class="ptr-line" hidden><span id="queue-issues" class="ptr-err"></span><button type="button" id="queue-view" class="ptr-link" data-tx="view-issues" hidden></button></div><div id="queue-errors" class="ptr-errors" tabindex="0" aria-label="Videos needing attention" hidden></div><button type="button" id="queue-retry" class="ptr-link" data-tx="retry" hidden>Retry connection</button></section>
<section id="folder-picker" aria-label="Save folder"><p id="destination" class="ptr-sub" hidden></p><div id="folder-choices" class="ptr-folders" role="group" aria-label="Choose a folder" tabindex="0" data-tx-scroll="folders"></div><span id="copy-status" class="ptr-sr" role="status" aria-live="polite"></span><form id="folder-create" class="ptr-new" data-tx-submit="create"><input id="folder-name" type="text" maxlength="80" placeholder="New folder" aria-label="New folder name" required disabled><button type="submit" id="create-folder" class="ptr-i" aria-label="Create folder" title="Create folder" disabled>{i_plus}</button></form></section>
<div id="notice" class="tx-notice" role="status" hidden></div></div>
<section id="zip-view" aria-labelledby="zip-title" hidden><div class="ptr-line"><b id="zip-title">Folders in All</b><span id="zip-summary" class="ptr-num" role="status"></span></div><p class="ptr-sub">Files directly in the save folder are always included.</p><div class="ptr-line ptr-gap"><span><button type="button" id="zip-all" class="ptr-link" data-tx="zip-all">All</button> <button type="button" id="zip-none" class="ptr-link" data-tx="zip-none">None</button> <button type="button" id="zip-retry" class="ptr-link" data-tx="zip-retry" hidden>Retry</button></span><button type="button" id="zip-done" class="ptr-link" data-tx="zip-done">Done</button></div><div id="zip-choices" class="ptr-folders" role="group" aria-label="Folders included in All" tabindex="0"></div><p id="zip-empty" class="ptr-sub" hidden>No subfolders</p><div id="zip-notice" class="tx-notice" role="status" hidden></div></section>"##,
        i_settings = icon(html::I_SETTINGS),
        i_plus = icon(html::I_PLUS),
    )
}

fn notice(message: &str) {
    ui::notice("notice", message, "error");
}

fn view() -> Value {
    with(|p| core::queue_state(p.snapshot.as_ref().unwrap_or(&Value::Null), p.unavailable))
}

fn root_folder_issue() -> bool {
    with(|p| p.stats.get("").is_some_and(|e| e.error.is_some() || (e.data.as_ref().is_some_and(|d| d["destination_selected"] == true && d["destination_available"] == false))))
}

fn render_attention(state: &Value) {
    let (folder_unavailable, stats_unavailable, issues) = with(|p| {
        let root = p.stats.get("");
        let issues: Vec<Value> = p.snapshot.as_ref().and_then(|s| s["attention"].as_array()).map(|a| a.iter().filter(|i| !text(i, "id").is_empty() && !matches!(text(i, "kind"), "cancelled" | "removed")).take(10).cloned().collect()).unwrap_or_default();
        (root.and_then(|r| r.data.as_ref()).is_some_and(|d| d["destination_selected"] == true && d["destination_available"] == false), root.is_some_and(|r| r.error.is_some()), issues)
    });
    let n = num(&state["issues"]);
    let available: &[Value] = if n > 0. { &issues } else { &[] };
    let folder = folder_unavailable || stats_unavailable;
    show("queue-attention", folder || n > available.len() as f64);
    // The stage line already says how many need attention; this line is for the folder's own troubles.
    host::text("queue-issues", if folder_unavailable { "Save folder unavailable" } else if stats_unavailable { "File count unavailable" } else { "" });
    let more = n > available.len() as f64;
    show("queue-view", folder || more);
    host::text("queue-view", if more { "View all" } else { "Settings" });
    show("queue-errors", !available.is_empty());
    let mut h = String::new();
    for issue in available {
        let (title, message) = (text(issue, "title"), text(issue, "message"));
        let title = if title.is_empty() { "Untitled video" } else { title };
        let message = if message.is_empty() { "Open this video to review the issue." } else { message };
        h.push_str(&format!("<button type=\"button\" class=\"ptr-error\" data-tx=\"issue\" data-arg=\"{id}\" title=\"{m}\" aria-label=\"Open details for {t}: {m}\"><b>{t}</b><small>{m}</small></button>", id = esc(text(issue, "id")), t = esc(title), m = esc(message)));
    }
    put("queue-errors", &h);
}

fn render() {
    let state = view();
    let g = |k: &str| num(&state[k]);
    let kind = state["kind"].as_str().unwrap_or("loading").to_owned();
    let (has_snapshot, unavailable, stopping, until) = with(|p| (p.snapshot.is_some(), p.unavailable, p.stopping, p.snapshot.as_ref().map_or(0., |s| num(&s["completion_until"]))));
    let active = g("processing") + g("queued") + g("saving") + g("discovering") + g("waiting") > 0.;
    let files_layout = state["settled"] == true && !stopping;
    let success = kind == "complete" && until > host::epoch();
    if success {
        host::later("completion", (until - host::epoch()).min(core::COMPLETION_MS));
    }
    host::body_attr("data-tr-state", &kind);
    let stage = if unavailable {
        if has_snapshot { "Last confirmed counts" } else { "Helper unavailable" }.to_owned()
    } else if !has_snapshot {
        "Connecting".to_owned()
    } else if g("discovering") > 0. {
        "Finding videos".to_owned()
    } else if g("processing") > 0. {
        format!("{} transcribing", host::number(g("processing")))
    } else if g("saving") > 0. {
        format!("{} saving", host::number(g("saving")))
    } else if g("waiting") > 0. {
        format!("{} waiting", plural(g("waiting"), "channel"))
    } else if g("queued") > 0. {
        "Waiting to transcribe".to_owned()
    } else {
        match kind.as_str() {
            "complete" if success => "All transcripts saved".to_owned(),
            "complete" | "settled" => "Finished".to_owned(),
            "idle" => "Ready for your next video".to_owned(),
            "attention" => need_attention(g("issues")),
            _ => "Saved so far".to_owned(),
        }
    };
    host::text("queue-stage", &stage);
    host::text("queue-summary", &if has_snapshot { format!("{} / {}", host::number(g("saved")), host::number(g("total"))) } else { String::new() });
    set("queue-summary", "title", &format!("{} of {} videos saved in the current batch", host::number(g("saved")), host::number(g("total"))));
    let bar_state = if unavailable { "unavailable" } else if !has_snapshot { "loading" } else if g("discovering") > 0. && g("total") == 0. { "discovering" } else if kind == "attention" { "attention" } else { "working" };
    set("queue-progress", "data-state", bar_state);
    set("queue-progress", "aria-busy", if !unavailable && active { "true" } else { "false" });
    let ratio = if g("total") > 0. { g("saved") / g("total") } else { 0. };
    set("queue-fill", "style", &format!("transform:scaleX({ratio})"));
    let final_denominator = has_snapshot && !unavailable && state["discoveryUnknown"] != true && state["discoveryComplete"] == true && g("total") > 0.;
    host::attr("queue-progress", "aria-valuenow", final_denominator.then(|| g("saved").to_string()));
    host::attr("queue-progress", "aria-valuemax", final_denominator.then(|| g("total").to_string()));
    set("queue-progress", "aria-valuetext", &format!("{stage}. {} of {} saved. {} failed, {} remaining.", g("saved"), g("total"), g("failed"), g("remaining")));
    host::text("queue-counts", &if g("queued") > 0. { format!("{} queued", host::number(g("queued"))) } else if g("discovering") > 0. { "Totals growing".to_owned() } else { String::new() });
    let stoppable = g("queued") + g("processing") + g("discovering") + g("waiting") > 0.;
    show("queue-stop", stoppable || stopping);
    disable("queue-stop", stopping || unavailable);
    set("queue-stop", "aria-busy", if stopping { "true" } else { "false" });
    host::text("queue-stop", if stopping { "Stopping" } else { "Stop all" });
    show("queue-retry", unavailable);
    render_attention(&state);
    let issues = g("issues") > 0.;
    show("queue-card", !(files_layout && !issues && !root_folder_issue() && !stopping));
    render_folders();
}

fn need_attention(n: f64) -> String {
    format!("{} {} attention", host::number(n), if n == 1. { "needs" } else { "need" })
}

fn count_presentation(p: &P, id: &str) -> (&'static str, String, String) {
    let entry = p.stats.get(id);
    let data = entry.and_then(|e| e.data.as_ref());
    if p.destination.as_ref().is_none_or(|d| text(d, "directory").is_empty()) {
        return ("unavailable", "–".into(), "Choose a save folder in Settings to count files".into());
    }
    if entry.is_some_and(|e| e.error.is_some()) || data.is_some_and(|d| d["destination_available"] == false || d["available"] == false || d["availability"]["saved_here"] == false) {
        return ("unavailable", "–".into(), entry.and_then(|e| e.error.clone()).unwrap_or("This folder is unavailable. Check the save folder in Settings".into()));
    }
    if let Some(d) = data {
        if d["inventory_complete"] == false || d["details_available"] == false || d["source"] == "verified_videos" {
            return ("legacy", "–".into(), "The installed helper reports videos, not physical file counts. Update it to count this folder".into());
        }
        if d["scan_complete"] == false {
            return ("partial", "–".into(), "The folder scan is incomplete. A complete file count is not available yet".into());
        }
        if d["availability"]["saved_here"] == true && d["scan_complete"] == true && d["saved_here"].as_u64().is_some() {
            let n = num(&d["saved_here"]);
            let files = format!("{} saved", plural(n, "transcript file"));
            let pending = entry.is_some_and(|e| e.pending);
            return (if pending { "refreshing" } else { "ready" }, host::number(n), if pending { format!("Last confirmed: {files}. Refreshing") } else { files });
        }
        if !entry.is_some_and(|e| e.pending) {
            return ("unavailable", "–".into(), "The helper did not provide a complete file count for this folder".into());
        }
    }
    ("loading", "–".into(), "Checking saved transcript files".into())
}

fn zip_label(p: &P) -> (String, String, bool) {
    let names: Vec<&str> = p.export.as_ref().and_then(|e| e["folders"].as_array()).map(|a| a.iter().map(|f| text(f, "name")).collect()).unwrap_or_default();
    let selected = names.iter().filter(|n| !p.excluded.contains(**n)).count();
    let ready = p.export.is_some() && p.export_error.is_empty();
    if !p.unresolved.is_empty() {
        return ("Folders: review".into(), "Folders changed. Review your ZIP selection before copying All".into(), true);
    }
    if ready {
        return (format!("Folders {selected}/{}", names.len()), format!("{selected} of {} subfolders included in the All ZIP. Files in the save folder are always included", names.len()), selected < names.len());
    }
    ("Folders".into(), if p.export_error.is_empty() { "Choose the folders included in the All ZIP".to_owned() } else { p.export_error.clone() }, false)
}

fn render_folders() {
    let (h, dest_text, dest_title, input_off, load_export, scroll) = with(|p| {
        let directory = p.destination.as_ref().map_or("", |d| text(d, "directory")).to_owned();
        let dest = if !p.library_ready { "Folders unavailable" } else if directory.is_empty() { "Choose a save folder in Settings" } else if p.destination.as_ref().is_some_and(|d| d["enabled"] == true) { "" } else { "Automatic saving is off" };
        let root = directory.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("Transcripts").to_owned();
        let mut choices: Vec<(String, String)> = vec![(String::new(), "All".into())];
        choices.extend(p.folders.iter().filter(|f| text(f, "kind") != "channel").map(|f| (text(f, "id").to_owned(), text(f, "name").to_owned())));
        let mut out = String::new();
        for (id, name) in &choices {
            let selected = *id == p.default_folder;
            let (cstate, ctext, ctitle) = count_presentation(p, id);
            let copy = p.copies.get(id);
            let browse = p.browse.get(id);
            let feedback = match (browse, copy) {
                (Some(b), Some(c)) => {
                    if b.seq > c.seq {
                        Some(b)
                    } else {
                        Some(c)
                    }
                }
                (Some(b), None) => Some(b),
                (None, Some(c)) => Some(c),
                _ => None,
            };
            let cstatus = copy.map_or("idle", |c| c.state.as_str());
            let copy_label = if id.is_empty() { "Copy all folders as a ZIP file".to_owned() } else { format!("Copy {name} as a ZIP file") };
            let copy_title = copy.filter(|c| !c.message.is_empty()).map_or(copy_label.clone(), |c| c.message.clone());
            let copy_icon = match cstatus {
                "success" => icon(html::I_CHECK),
                "error" => icon(html::I_ALERT),
                "loading" => "<span class=\"ptr-dots\" aria-hidden=\"true\"></span>".to_owned(),
                _ => icon(html::I_COPY),
            };
            let off = !p.library_ready || directory.is_empty() || p.folder_busy;
            let browse_off = off || p.browsing.contains(id) || p.stats.get(id).and_then(|s| s.data.as_ref()).is_some_and(|d| d["destination_available"] == false);
            let path_title = if directory.is_empty() { if id.is_empty() { root.clone() } else { name.clone() } } else if id.is_empty() { directory.trim_end_matches('/').to_owned() } else { format!("{}/{name}", directory.trim_end_matches('/')) };
            out.push_str(&format!(
                "<div class=\"ptr-card\" data-folder-id=\"{eid}\" data-selected=\"{selected}\"><button type=\"button\" class=\"ptr-choice\" data-tx=\"choose\" data-arg=\"{eid}\" aria-pressed=\"{selected}\" title=\"{etitle}\"{chooser_off}>{i}<b>{ename}</b><span class=\"ptr-count\" data-count-state=\"{cstate}\" title=\"{ctitle}\" role=\"img\" aria-label=\"{ctitle}\" aria-busy=\"{cbusy}\">{ctext}</span></button><div class=\"ptr-actions\"><button type=\"button\" class=\"ptr-link\" data-tx=\"browse\" data-arg=\"{eid}\" aria-label=\"Browse files in {ename}\" aria-busy=\"{bbusy}\"{browse_off}>Browse files</button>",
                eid = esc(id),
                etitle = esc(&path_title),
                ename = esc(name),
                i = icon(if id.is_empty() { html::I_FOLDERS } else { html::I_FOLDER }),
                chooser_off = if !p.library_ready || p.folder_busy { " disabled" } else { "" },
                ctitle = esc(&ctitle),
                cbusy = matches!(cstate, "loading" | "refreshing"),
                ctext = esc(&ctext),
                bbusy = p.browsing.contains(id),
                browse_off = if browse_off { " disabled" } else { "" },
            ));
            if let Some(c) = copy.filter(|c| c.attachment.is_some()) {
                let _ = c;
                out.push_str(&format!("<button type=\"button\" class=\"ptr-link\" data-tx=\"show-zip\" data-arg=\"{eid}\" title=\"Show the prepared attachment to drag it into a chat\" aria-label=\"Show prepared attachment for {ename}\"{dis}>{label}</button>", eid = esc(id), ename = esc(name), dis = if p.copying.is_some() || p.revealing.contains(id) { " disabled" } else { "" }, label = if p.revealing.contains(id) { "Opening" } else { "Show file" }));
            }
            if id.is_empty() {
                let (label, title, filtered) = zip_label(p);
                let dis = p.copying.is_some() || p.select_busy || directory.is_empty();
                out.push_str(&format!("<button type=\"button\" class=\"ptr-link\" data-tx=\"zip-open\" data-filtered=\"{filtered}\" title=\"{t}\" aria-label=\"{t}\"{d}>{l}</button>", t = esc(&title), d = if dis { " disabled" } else { "" }, l = esc(&label)));
            }
            out.push_str(&format!(
                "<button type=\"button\" class=\"ptr-i folder-copy\" data-tx=\"copy\" data-arg=\"{eid}\" data-copy-state=\"{cstatus}\" title=\"{ct}\" aria-label=\"{ct}\" aria-busy=\"{busy}\"{dis}>{copy_icon}</button></div>",
                eid = esc(id),
                ct = esc(&copy_title),
                busy = cstatus == "loading",
                dis = if off || p.select_busy || p.copying.is_some() { " disabled" } else { "" },
            ));
            if let Some(f) = feedback.filter(|f| !f.message.is_empty()) {
                out.push_str(&format!("<p class=\"ptr-note\" data-copy-state=\"{}\" title=\"{m}\">{m}</p>", esc(&f.state), m = esc(&f.message)));
            }
            out.push_str("</div>");
        }
        let scroll = p.reveal.take();
        (out, dest, directory, !p.library_ready || p.folder_busy, p.export.is_none() && !p.export_busy && p.export_error.is_empty() && p.destination.as_ref().is_some_and(|d| !text(d, "directory").is_empty()), scroll)
    });
    host::text("destination", dest_text);
    set("destination", "title", &dest_title);
    show("destination", !dest_text.is_empty());
    set("folder-choices", "aria-busy", if input_off { "true" } else { "false" });
    disable("create-folder", input_off);
    disable("folder-name", input_off);
    put("folder-choices", &h);
    if let Some(id) = scroll {
        host::keep_in_box("folder-choices", &format!(".ptr-card[data-folder-id=\"{}\"]", esc(&id)));
    }
    update_visible();
    render_zip();
    if load_export {
        spawn_local(async {
            let _ = load_export_folders().await;
        });
    }
}

fn render_zip() {
    let (open, names, excluded, selected, busy, ready, err, unresolved, loading, select_busy) = with(|p| {
        let names: Vec<String> = p.export.as_ref().and_then(|e| e["folders"].as_array()).map(|a| a.iter().map(|f| text(f, "name").to_owned()).collect()).unwrap_or_default();
        let selected = names.iter().filter(|n| !p.excluded.contains(*n)).count();
        (p.zip_view, names, p.excluded.clone(), selected, p.export_busy || p.select_busy || p.copying.is_some(), p.export.is_some() && p.export_error.is_empty(), p.export_error.clone(), !p.unresolved.is_empty(), p.export_busy, p.select_busy)
    });
    if !open {
        return;
    }
    host::text("zip-summary", &if loading { String::new() } else if !err.is_empty() { "Unavailable".to_owned() } else { format!("{selected}/{}", names.len()) });
    disable("zip-all", busy || !ready || selected == names.len());
    disable("zip-none", busy || !ready || selected == 0);
    disable("zip-done", select_busy || (unresolved && (loading || !err.is_empty())));
    show("zip-retry", !err.is_empty());
    disable("zip-retry", busy);
    let message = if !err.is_empty() { err } else if unresolved { "Folders changed. Review your selection, then choose Done before copying All.".to_owned() } else { String::new() };
    ui::notice("zip-notice", &message, "error");
    show("zip-empty", ready && names.is_empty());
    let mut h = String::new();
    for (i, name) in names.iter().enumerate() {
        h.push_str(&format!("<label class=\"tx-check\" title=\"{e}\"><input type=\"checkbox\" id=\"zip-{i}\" data-zip-folder=\"{e}\" data-tx-change=\"zip-folder\" data-arg=\"{e}\"{c}{d}><span>{e}</span></label>", e = esc(name), c = if excluded.contains(name) { "" } else { " checked" }, d = if busy || !ready { " disabled" } else { "" }));
    }
    put("zip-choices", &h);
}

fn update_visible() {
    let list = host::in_box("folder-choices", ".ptr-card", "folderId");
    let shown = !list.is_empty() || with(|p| !p.zip_view);
    with(|p| {
        p.visible = if shown && !p.zip_view {
            let mut v = vec![String::new()];
            for id in list.split('\u{1e}').filter(|s| !s.is_empty()) {
                if !v.iter().any(|x| x == id) {
                    v.push(id.to_owned());
                }
            }
            v
        } else {
            vec![]
        };
    });
    plan_stats();
}

fn stats_interval() -> f64 {
    if matches!(view()["kind"].as_str(), Some("processing" | "queued")) { 2000. } else { 10000. }
}

fn plan_stats() {
    host::later_off("stats");
    let can = with(|p| !p.stats_busy && p.library_ready && p.destination.as_ref().is_some_and(|d| !text(d, "directory").is_empty()) && !p.visible.is_empty());
    if !can || host::hidden() {
        return;
    }
    let interval = stats_interval();
    let now = host::epoch();
    let delay = with(|p| p.visible.iter().map(|id| p.stats.get(id).and_then(|s| s.checked_at).map_or(0., |t| (t + interval - now).max(0.))).fold(f64::INFINITY, f64::min));
    host::later("stats", delay);
}

async fn refresh_stats() {
    if with(|p| p.stats_busy || !p.library_ready || p.destination.as_ref().is_none_or(|d| text(d, "directory").is_empty())) || host::hidden() {
        return;
    }
    with(|p| p.stats_busy = true);
    let interval = stats_interval();
    let now = host::epoch();
    let (generation, due) = with(|p| {
        let mut due: Vec<(f64, String)> = p.visible.iter().filter_map(|id| {
            let at = p.stats.get(id).and_then(|s| s.checked_at);
            (at.is_none() || now - at.unwrap_or(0.) >= interval).then(|| (at.unwrap_or(0.), id.clone()))
        }).collect();
        due.sort_by(|a, b| a.0.total_cmp(&b.0));
        (p.generation, due.into_iter().take(3).map(|d| d.1).collect::<Vec<_>>())
    });
    // The helper serializes filesystem scans: one at a time, never for hidden cards or after the root changed.
    for id in due {
        if host::hidden() || with(|p| p.generation != generation) {
            break;
        }
        if !with(|p| p.visible.contains(&id)) {
            continue;
        }
        with(|p| p.stats.entry(id.clone()).or_default().pending = true);
        render_folders();
        let folder = if id.is_empty() { Value::Null } else { json!(id) };
        let r = send("getLibraryStats", json!({"folderId":folder})).await;
        if with(|p| p.generation != generation) {
            break;
        }
        let stat = match r {
            Ok(d) if d.is_object() => Stat { data: Some(d), checked_at: Some(host::epoch()), ..Stat::default() },
            Ok(_) => Stat { error: Some("File count unavailable. Try again shortly.".into()), checked_at: Some(host::epoch()), ..Stat::default() },
            Err(e) => Stat { error: Some(e), checked_at: Some(host::epoch()), ..Stat::default() },
        };
        with(|p| p.stats.insert(id.clone(), stat));
        render();
    }
    with(|p| p.stats_busy = false);
    plan_stats();
}

fn update_destination(value: Value) {
    let context = json!([value["directory"].as_str().unwrap_or(""), value["enabled"], value["format"]]).to_string();
    let same = with(|p| {
        p.destination = Some(value);
        if context == p.context {
            return true;
        }
        p.context = context;
        p.generation += 1;
        p.stats.clear();
        p.copies.clear();
        p.browse.clear();
        p.export = None;
        p.export_error.clear();
        p.export_busy = false;
        p.excluded.clear();
        p.unresolved.clear();
        p.select_rev += 1;
        false
    });
    if !same {
        render_zip();
    }
}

fn invalidate_all_attachment() {
    with(|p| {
        p.select_rev += 1;
        p.copies.remove("");
        p.browse.remove("");
    });
    host::text("copy-status", "");
}

async fn load_export_folders() -> Result<Value, String> {
    let directory = with(|p| p.destination.as_ref().map(|d| text(d, "directory").to_owned())).unwrap_or_default();
    if directory.is_empty() {
        return Err("Choose a save folder in Settings first.".into());
    }
    let generation = with(|p| {
        p.export_busy = true;
        p.export_error.clear();
        p.generation
    });
    render_zip();
    let result = async {
        let data = send("getExportFolders", json!({})).await?;
        if with(|p| p.generation != generation) {
            return Err("The save folder changed. Try again.".to_owned());
        }
        let token = text(&data, "root_token");
        let valid = text(&data, "directory") == directory && token.len() == 64 && token.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) && data["folders"].as_array().is_some_and(|a| a.iter().all(|f| !text(f, "name").is_empty()));
        if !valid {
            return Err("Could not verify the ZIP folders. Refresh and try again.".to_owned());
        }
        let raw = host::local_read(&format!("{ZIP_PREFIX}{token}")).await;
        let stored: Vec<String> = if raw.is_empty() {
            vec![]
        } else {
            let v: Value = serde_json::from_str(&raw).map_err(|_| "Could not read your saved ZIP selection.".to_owned())?;
            v.as_array().filter(|a| a.iter().all(Value::is_string)).ok_or("Could not read your saved ZIP selection.")?.iter().filter_map(|n| n.as_str().map(str::to_owned)).collect()
        };
        if with(|p| p.generation != generation) {
            return Err("The save folder changed. Try again.".to_owned());
        }
        let existing: HashSet<&str> = data["folders"].as_array().into_iter().flatten().map(|f| text(f, "name")).collect();
        let next: BTreeSet<String> = stored.iter().filter(|n| existing.contains(n.as_str())).cloned().collect();
        let mut missing: Vec<String> = stored.iter().filter(|n| !existing.contains(n.as_str())).cloned().collect();
        missing.sort();
        missing.dedup();
        let differs = with(|p| p.export.as_ref().is_none_or(|e| e["root_token"] != data["root_token"]) || next != p.excluded || missing != p.unresolved);
        if differs {
            invalidate_all_attachment();
        }
        with(|p| {
            p.export = Some(data.clone());
            p.excluded = next;
            // A missing excluded name may have been renamed: its contents stay out until the person reviews the list.
            p.unresolved = missing;
        });
        Ok(data)
    }
    .await;
    with(|p| {
        if let Err(e) = &result
            && p.generation == generation
        {
            p.export_error = e.clone();
        }
        p.export_busy = false;
    });
    render_folders();
    result
}

async fn save_zip_selection(next: BTreeSet<String>, acknowledge: bool) -> bool {
    let Some((token, generation, before, unresolved)) = with(|p| {
        if p.select_busy || p.export_busy || p.copying.is_some() {
            return None;
        }
        p.export.as_ref().map(|e| (text(e, "root_token").to_owned(), p.generation, p.excluded.clone(), p.unresolved.clone()))
    }) else {
        return false;
    };
    with(|p| {
        p.select_busy = true;
        p.excluded = next.clone();
    });
    invalidate_all_attachment();
    render_folders();
    let mut names: BTreeSet<String> = next;
    if !acknowledge {
        names.extend(unresolved);
    }
    let ok = host::local_write(&format!("{ZIP_PREFIX}{token}"), &json!(names).to_string()).await;
    let current = with(|p| p.generation == generation && p.export.as_ref().is_some_and(|e| text(e, "root_token") == token));
    let saved = ok && current;
    with(|p| {
        if saved && acknowledge {
            p.unresolved.clear();
        }
        if !ok && current {
            p.excluded = before;
            p.export_error = "Could not save your selection. Try again.".into();
        }
        p.select_busy = false;
    });
    render_folders();
    saved
}

fn open_zip() {
    if with(|p| p.copying.is_some() || p.select_busy || p.destination.as_ref().is_none_or(|d| text(d, "directory").is_empty())) {
        return;
    }
    with(|p| p.zip_view = true);
    show("ptr-main", false);
    show("zip-view", true);
    render_zip();
    host::focus("#zip-done");
    update_visible();
    spawn_local(async {
        let _ = load_export_folders().await;
    });
}

fn close_zip(acknowledge: bool) {
    spawn_local(async move {
        if with(|p| p.select_busy) {
            return;
        }
        let (needs, excluded) = with(|p| (!p.unresolved.is_empty() && acknowledge, p.excluded.clone()));
        if needs && !save_zip_selection(excluded, true).await {
            return;
        }
        with(|p| p.zip_view = false);
        show("zip-view", false);
        show("ptr-main", true);
        host::focus("[data-tx=\"zip-open\"]");
        update_visible();
    });
}

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn copy_folder(id: String, name: String) {
    let go = with(|p| {
        if p.copying.is_some() || p.folder_busy || p.select_busy || !p.library_ready || p.destination.as_ref().is_none_or(|d| text(d, "directory").is_empty()) {
            return None;
        }
        p.copying = Some(id.clone());
        p.seq += 1;
        p.copies.insert(id.clone(), Feedback { state: "loading".into(), message: "Preparing ZIP".into(), attachment: None, seq: p.seq });
        Some(p.generation)
    });
    let Some(generation) = go else { return };
    host::text("copy-status", &format!("Preparing a ZIP of {name}"));
    render_folders();
    spawn_local(async move {
        let mut attachment: Option<Value> = None;
        let mut selection_rev = None;
        let result = async {
            let mut selection = json!({});
            if id.is_empty() {
                let source = load_export_folders().await?;
                if with(|p| p.generation != generation) {
                    return Ok(None);
                }
                if with(|p| !p.unresolved.is_empty()) {
                    return Err("Folders changed. Open Folders, review your selection, then choose Done before copying All.".to_owned());
                }
                selection["rootToken"] = source["root_token"].clone();
                selection["excludedFolders"] = with(|p| json!(p.excluded.iter().collect::<Vec<_>>()));
                selection_rev = Some(with(|p| p.select_rev));
            }
            selection["folderId"] = if id.is_empty() { Value::Null } else { json!(id) };
            let wanted = selection.clone();
            let prepared = send("prepareFolderAttachment", selection).await?;
            if with(|p| p.generation != generation) {
                return Ok(None);
            }
            if id.is_empty() {
                let mut got: Vec<&str> = prepared["excluded_folders"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                let mut want: Vec<&str> = wanted["excludedFolders"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                got.sort_unstable();
                want.sort_unstable();
                if selection_rev != Some(with(|p| p.select_rev)) || prepared["root_token"] != wanted["rootToken"] || !prepared["excluded_folders"].is_array() || got != want {
                    return Err("The ZIP folder selection changed. Copy again to use the current selection.".to_owned());
                }
            }
            let filename = text(&prepared, "filename");
            if !is_hex(text(&prepared, "id"), 32) || filename.is_empty() || prepared["count"].as_u64().is_none() || prepared["bytes"].as_u64().is_none_or(|b| b < 1) {
                return Err("Could not prepare this folder's attachment. Try again.".to_owned());
            }
            if !filename.ends_with(".zip") {
                return Err("Update the helper to copy folders as ZIP files.".to_owned());
            }
            attachment = Some(prepared.clone());
            with(|p| {
                p.seq += 1;
                let seq = p.seq;
                p.copies.insert(id.clone(), Feedback { state: "loading".into(), message: "Copying ZIP".into(), attachment: Some(prepared.clone()), seq });
            });
            render_folders();
            // A file reference on the clipboard, never the text: a large folder must not flood a chat's text box.
            let copied = send("copyFolderAttachment", json!({"id":prepared["id"]})).await?;
            if copied["copied"] != true || copied["filename"] != prepared["filename"] || copied["count"] != prepared["count"] {
                return Err("File copy was not confirmed. Use Show file to attach it.".to_owned());
            }
            Ok(Some(prepared))
        }
        .await;
        let current = with(|p| p.generation == generation);
        match result {
            Ok(Some(prepared)) if current && (!id.is_empty() || selection_rev == Some(with(|p| p.select_rev))) => {
                let count = num(&prepared["count"]);
                let message = format!("ZIP copied, {}", plural(count, "file"));
                let seq = with(|p| {
                    p.seq += 1;
                    let seq = p.seq;
                    p.copies.insert(id.clone(), Feedback { state: "success".into(), message: message.clone(), attachment: Some(prepared.clone()), seq });
                    seq
                });
                host::text("copy-status", &format!("{name}: {} copied as a ZIP file. Paste to attach, or use Show file.", text(&prepared, "filename")));
                let key = id.clone();
                spawn_local(async move {
                    host::wait(4500.).await;
                    with(|p| {
                        if let Some(c) = p.copies.get_mut(&key).filter(|c| c.seq == seq) {
                            c.state = "idle".into();
                            c.message.clear();
                        }
                    });
                    render_folders();
                });
            }
            Ok(_) => {}
            Err(e) => {
                if current {
                    if id.is_empty() && selection_rev.is_some() && selection_rev != Some(with(|p| p.select_rev)) {
                        attachment = None;
                    }
                    with(|p| {
                        p.seq += 1;
                        let seq = p.seq;
                        p.copies.insert(id.clone(), Feedback { state: "error".into(), message: e.clone(), attachment, seq });
                    });
                    host::text("copy-status", &format!("{name}: {e}"));
                }
            }
        }
        with(|p| p.copying = None);
        render_folders();
    });
}

fn reveal_attachment(id: String, name: String) {
    let status = with(|p| p.copies.get(&id).filter(|c| c.attachment.is_some() && p.copying.is_none() && !p.revealing.contains(&id)).cloned());
    let Some(status) = status else { return };
    let generation = with(|p| {
        p.revealing.insert(id.clone());
        p.generation
    });
    render_folders();
    spawn_local(async move {
        let attachment_id = status.attachment.as_ref().map(|a| a["id"].clone()).unwrap_or(Value::Null);
        let r = send("revealFolderAttachment", json!({"id":attachment_id})).await.and_then(|v| if v["revealed"] == true { Ok(()) } else { Err("Could not show the attachment. Try again.".to_owned()) });
        if with(|p| p.generation == generation) {
            match r {
                Ok(()) => host::text("copy-status", &format!("{name}: attachment shown. Drag the file into your chat.")),
                Err(e) => {
                    with(|p| {
                        p.seq += 1;
                        let seq = p.seq;
                        p.copies.insert(id.clone(), Feedback { state: "error".into(), message: e, attachment: status.attachment.clone(), seq });
                    });
                    host::text("copy-status", &format!("{name}: could not show the attachment."));
                }
            }
        }
        with(|p| p.revealing.remove(&id));
        render_folders();
    });
}

fn browse_folder(id: String, name: String) {
    let go = with(|p| {
        if p.browsing.contains(&id) || p.folder_busy || !p.library_ready || p.destination.as_ref().is_none_or(|d| text(d, "directory").is_empty()) {
            return None;
        }
        p.browsing.insert(id.clone());
        p.seq += 1;
        p.browse.insert(id.clone(), Feedback { state: "loading".into(), message: "Opening folder".into(), attachment: None, seq: p.seq });
        Some(p.generation)
    });
    let Some(generation) = go else { return };
    render_folders();
    spawn_local(async move {
        let folder = if id.is_empty() { Value::Null } else { json!(id) };
        let r = send("revealExportDirectory", json!({"folderId":folder})).await.and_then(|v| if v["revealed"] == true { Ok(()) } else { Err("This folder could not be opened. Check Settings.".to_owned()) });
        if with(|p| p.generation == generation) {
            match r {
                Ok(()) => {
                    let seq = with(|p| {
                        p.seq += 1;
                        let seq = p.seq;
                        p.browse.insert(id.clone(), Feedback { state: "success".into(), message: "Opened".into(), attachment: None, seq });
                        seq
                    });
                    host::text("copy-status", &format!("{name}: opened."));
                    let key = id.clone();
                    spawn_local(async move {
                        host::wait(2500.).await;
                        with(|p| {
                            if p.browse.get(&key).is_some_and(|b| b.seq == seq) {
                                p.browse.remove(&key);
                            }
                        });
                        render_folders();
                    });
                }
                Err(e) => {
                    with(|p| {
                        p.seq += 1;
                        let seq = p.seq;
                        p.browse.insert(id.clone(), Feedback { state: "error".into(), message: e.clone(), attachment: None, seq });
                    });
                    host::text("copy-status", &format!("{name}: {e}"));
                }
            }
        }
        with(|p| p.browsing.remove(&id));
        render_folders();
    });
}

fn apply_library(library: &Value) {
    with(|p| {
        p.folders = library["folders"].as_array().cloned().unwrap_or_default();
        p.default_folder = text(library, "default_folder_id").to_owned();
        p.library_ready = true;
    });
}

fn resource(key: &'static str, action: &'static str, apply: impl Fn(Result<Value, String>, u64) + 'static) {
    let busy = with(|p| {
        let busy = p.in_flight.get(key).copied().unwrap_or(false);
        p.in_flight.insert(key, true);
        busy
    });
    if busy {
        return;
    }
    let (queue_rev, folder_rev) = with(|p| (p.queue_rev, p.folder_rev));
    spawn_local(async move {
        let r = send(action, json!({})).await;
        with(|p| p.in_flight.insert(key, false));
        if key == "queue" && with(|p| p.queue_rev) != queue_rev {
            return;
        }
        apply(r, folder_rev);
        render();
    });
}

/// Independent reads paint as each is ready: a slow folder scan cannot hold back the queue's feedback.
fn refresh() {
    host::later_off("refresh");
    if host::hidden() {
        return;
    }
    resource("queue", "getQueueStatus", |r, _| match r {
        Ok(v) if v["total"].as_f64().is_some() => with(|p| {
            p.snapshot = Some(v);
            p.unavailable = false;
        }),
        _ => with(|p| p.unavailable = true),
    });
    resource("settings", "getExportSettings", |r, _| {
        if let Ok(v) = r {
            update_destination(v);
        }
    });
    resource("library", "getLibrary", |r, rev| {
        if let Ok(v) = r
            && with(|p| p.folder_rev) == rev
        {
            apply_library(&v);
        }
    });
    host::later("refresh", 2000.);
}

fn folder_name(id: &str) -> String {
    if id.is_empty() {
        return "All".into();
    }
    with(|p| p.folders.iter().find(|f| text(f, "id") == id).map_or_else(String::new, |f| text(f, "name").to_owned()))
}

pub fn on_event(kind: &str, name: &str, arg: &str, value: &str) {
    match (kind, name) {
        ("timer", "refresh") => refresh(),
        ("timer", "stats") => spawn_local(refresh_stats()),
        ("timer", "completion") => render(),
        ("visible", _) => {
            if host::hidden() {
                host::later_off("refresh");
                host::later_off("stats");
            } else {
                refresh();
                plan_stats();
            }
        }
        ("scroll", "folders") => update_visible(),
        ("key", "Escape") if with(|p| p.zip_view) => close_zip(false),
        ("click", "settings") => spawn_local(async {
            if let Err(e) = send("openSettings", json!({})).await {
                notice(&e);
            }
        }),
        ("click", "library") => spawn_local(async {
            if let Err(e) = send("openReader", json!({})).await {
                notice(&e);
            }
        }),
        ("click", "view-issues") => {
            let to_reader = host_text_is_view_all();
            spawn_local(async move {
                let r = if to_reader { send("openReader", json!({"view":"attention"})).await } else { send("openSettings", json!({})).await };
                if let Err(e) = r {
                    notice(&e);
                }
            });
        }
        ("click", "issue") => {
            let id = arg.to_owned();
            spawn_local(async move {
                if let Err(e) = send("openReader", json!({"id":id,"view":"attention"})).await {
                    notice(&e);
                }
            });
        }
        ("click", "retry") => refresh(),
        ("click", "choose") => choose_folder(arg.to_owned()),
        ("click", "browse") => browse_folder(arg.to_owned(), folder_name(arg)),
        ("click", "copy") => copy_folder(arg.to_owned(), folder_name(arg)),
        ("click", "show-zip") => reveal_attachment(arg.to_owned(), folder_name(arg)),
        ("click", "zip-open") => open_zip(),
        ("click", "zip-done") => close_zip(true),
        ("click", "zip-all") => spawn_local(async {
            save_zip_selection(BTreeSet::new(), false).await;
        }),
        ("click", "zip-none") => {
            let all: BTreeSet<String> = with(|p| p.export.as_ref().and_then(|e| e["folders"].as_array()).map(|a| a.iter().map(|f| text(f, "name").to_owned()).collect()).unwrap_or_default());
            spawn_local(async move {
                save_zip_selection(all, false).await;
            });
        }
        ("click", "zip-retry") => spawn_local(async {
            let _ = load_export_folders().await;
        }),
        ("change", "zip-folder") => {
            let on = value == "true";
            let mut next = with(|p| p.excluded.clone());
            if on {
                next.remove(arg);
            } else {
                next.insert(arg.to_owned());
            }
            spawn_local(async move {
                save_zip_selection(next, false).await;
            });
        }
        ("click", "stop") => stop_all(),
        ("submit", "create") => create_folder(),
        _ => {}
    }
}

fn host_text_is_view_all() -> bool {
    with(|p| {
        let state = core::queue_state(p.snapshot.as_ref().unwrap_or(&Value::Null), p.unavailable);
        let listed = p.snapshot.as_ref().and_then(|s| s["attention"].as_array()).map_or(0, |a| a.iter().filter(|i| !text(i, "id").is_empty() && !matches!(text(i, "kind"), "cancelled" | "removed")).take(10).count());
        num(&state["issues"]) > listed as f64
    })
}

fn choose_folder(id: String) {
    if with(|p| p.folder_busy || !p.library_ready || p.default_folder == id) {
        return;
    }
    with(|p| {
        p.folder_busy = true;
        p.folder_rev += 1;
    });
    notice("");
    render_folders();
    spawn_local(async move {
        let folder = if id.is_empty() { Value::Null } else { json!(id) };
        match send("setDefaultFolder", json!({"folderId":folder})).await {
            Ok(library) => {
                with(|p| p.folder_rev += 1);
                apply_library(&library);
            }
            Err(e) => notice(&e),
        }
        with(|p| p.folder_busy = false);
        render();
        refresh();
    });
}

fn create_folder() {
    let name = host::read_value("folder-name");
    let name = name.trim().to_owned();
    if name.is_empty() || with(|p| p.folder_busy || !p.library_ready) {
        return;
    }
    with(|p| {
        p.folder_busy = true;
        p.folder_rev += 1;
    });
    notice("");
    render_folders();
    spawn_local(async move {
        let r = async {
            let mut folder = send("createFolder", json!({"name":name})).await?;
            folder["kind"] = json!("manual");
            let id = folder["id"].clone();
            with(|p| {
                p.folder_rev += 1;
                p.folders.push(folder);
            });
            render_folders();
            let library = send("setDefaultFolder", json!({"folderId":id})).await?;
            with(|p| {
                p.folder_rev += 1;
                p.reveal = id.as_str().map(str::to_owned);
            });
            apply_library(&library);
            host::set_value("folder-name", "");
            Ok::<_, String>(())
        }
        .await;
        if let Err(e) = r {
            notice(&e);
        }
        with(|p| p.folder_busy = false);
        render();
        refresh();
    });
}

fn stop_all() {
    if with(|p| std::mem::replace(&mut p.stopping, true)) {
        return;
    }
    with(|p| p.queue_rev += 1);
    notice("");
    render();
    spawn_local(async move {
        match send("cancelQueue", json!({})).await {
            Ok(r) if r["status"]["total"].as_f64().is_some() => {
                let mut snapshot = r["status"].clone();
                snapshot["completion_until"] = json!(0);
                with(|p| {
                    p.queue_rev += 1;
                    p.snapshot = Some(snapshot);
                    p.unavailable = false;
                });
                host::text("copy-status", "Queue stopped. Finished transcripts are saved.");
            }
            Ok(_) => notice("Could not confirm the queue stopped. Check the connection and try again."),
            Err(e) => notice(&e),
        }
        with(|p| p.stopping = false);
        render();
        refresh();
    });
}
