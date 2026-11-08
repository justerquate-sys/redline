//! The library and reader page (`reader.html`): folders, selection, copying, moving, saving, job progress, and the
//! transcript itself, set for reading.

use crate::core::{self, REMOVED_MESSAGE};
use crate::html::{self, esc, icon};
use crate::host;
use crate::ui::{self, disable, notice, plural, put, send, set, show};
use js_sys::Promise;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{future_to_promise, spawn_local};

const ALL: &str = "__all__";
const CHOOSE: &str = "__choose__";

#[derive(Clone)]
struct Doc {
    content: String,
    sections: Vec<(f64, String)>,
}

#[derive(Default)]
struct S {
    jobs: Vec<Value>,
    folders: Vec<Value>,
    default_folder: Option<String>,
    filter: String,
    selected: HashSet<String>,
    errors: HashSet<String>,
    retrying: HashSet<String>,
    docs: HashMap<String, Doc>,
    removed_ids: HashSet<String>,
    removed_videos: HashSet<String>,
    current: Option<String>,
    video: Option<String>,
    attention_only: bool,
    attention: Vec<Value>,
    attention_loaded: bool,
    current_job: Option<Value>,
    load: u64,
    refreshing: bool,
    busy: bool,
    hover_select: bool,
}

thread_local! {
    static STATE: RefCell<S> = RefCell::new(S { filter: ALL.into(), ..S::default() });
}
fn with<R>(f: impl FnOnce(&mut S) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}

impl S {
    fn completed(&self, job: &Value) -> bool {
        job["status"] == "completed" && job["document_available"] == true && !self.removed_ids.contains(text(job, "id")) && !self.removed_videos.contains(text(job, "video_id"))
    }
    fn reconcile(&mut self, mut job: Value) -> Value {
        let (id, video) = (text(&job, "id").to_owned(), text(&job, "video_id").to_owned());
        if job["status"] == "removed" {
            self.removed_ids.insert(id.clone());
            if !video.is_empty() {
                self.removed_videos.insert(video.clone());
            }
        }
        if !self.removed_ids.contains(&id) && !self.removed_videos.contains(&video) {
            return job;
        }
        self.removed_ids.insert(id.clone());
        self.selected.remove(&id);
        self.errors.remove(&id);
        self.docs.remove(&id);
        job["status"] = json!("removed");
        job["document_available"] = json!(false);
        job
    }
    fn visible(&self) -> Vec<Value> {
        if !self.attention_only {
            return self.jobs.iter().filter(|j| self.filter == ALL || text(j, "folder_id") == self.filter).cloned().collect();
        }
        self.attention
            .iter()
            .filter(|i| !text(i, "id").is_empty())
            .map(|issue| {
                self.jobs.iter().find(|j| j["id"] == issue["id"]).cloned().unwrap_or_else(|| {
                    let status = match text(issue, "kind") {
                        "save_failed" | "unsaved" => "completed",
                        "persistence" => "failed",
                        k => k,
                    };
                    json!({"id":issue["id"],"video_id":issue["video_id"],"title":issue["title"],"status":status,"document_available":false,"error":issue["message"]})
                })
            })
            .collect()
    }
    fn selected_jobs(&self) -> Vec<Value> {
        self.visible().into_iter().filter(|j| self.selected.contains(text(j, "id")) && self.completed(j)).collect()
    }
    fn folder_name(&self, id: Option<&str>) -> String {
        match id.filter(|i| !i.is_empty()) {
            None => "Unfiled".into(),
            Some(id) => self.folders.iter().find(|f| text(f, "id") == id).map_or("Unknown folder".to_owned(), |f| text(f, "name").to_owned()),
        }
    }
    fn reader_url(&self, id: Option<&str>) -> String {
        let mut q = vec![];
        if self.attention_only {
            q.push("view=attention".to_owned());
        }
        if let Some(id) = id {
            q.push(format!("id={id}"));
        }
        if q.is_empty() { "reader.html".into() } else { format!("reader.html?{}", q.join("&")) }
    }
}

fn job_title(job: &Value) -> String {
    let t = text(job, "title");
    if !t.is_empty() {
        t.to_owned()
    } else if !text(job, "video_id").is_empty() {
        text(job, "video_id").to_owned()
    } else {
        "YouTube video".into()
    }
}

pub fn boot() {
    with(|s| {
        s.current = ui::query_param("id");
        s.video = ui::query_param("video").filter(|v| core::valid_video_id(v));
        s.attention_only = ui::query_param("view").as_deref() == Some("attention");
    });
    host::html("tr-root", &shell());
    let current = with(|s| s.current.clone());
    navigate(current, false);
    spawn_local(refresh());
}

fn shell() -> String {
    format!(
        r#"<div class="tx-app"><header class="tx-top"><a class="tx-brand" href="reader.html">{mark}<span>Transcripts</span></a><span id="connection" class="tx-state" role="status">Connecting</span><span class="tx-gap"></span><a class="tx-b quiet" href="settings.html#save-folder">Save folder</a><a class="tx-b quiet" href="settings.html">Settings</a></header>
<div class="tx-shell"><aside class="tx-lib" aria-label="Transcript library">
<div class="tx-row spread"><h2><span id="library-title">Library</span> <span id="library-count" class="tx-count"></span></h2><button type="button" id="refresh" class="tx-b icon quiet" data-tx="refresh" aria-label="Refresh library" title="Refresh library">{i_refresh}</button></div>
<button type="button" id="attention-filter" class="tx-link" data-tx="attention" aria-pressed="false">Needs attention</button>
<section class="tx-folders" aria-label="Transcript folders">
<div class="tx-field"><label for="folder-filter">Folder</label><select id="folder-filter" data-tx-change="filter"></select></div>
<form id="folder-form" class="tx-add" data-tx-submit="folder"><input id="folder-name" type="text" placeholder="New folder" aria-label="New folder name" maxlength="100" required><button type="submit" id="create-folder" class="tx-b icon" aria-label="Create folder" title="Create folder">{i_plus}</button></form>
<div class="tx-row spread wrap"><span id="default-folder-label" class="tx-hint"></span><button type="button" id="set-default-folder" class="tx-link" data-tx="set-default" disabled>Use for new videos</button></div>
<div class="tx-row"><button type="button" id="copy-folder" class="tx-b small" data-tx="copy-folder" disabled>Copy all</button><button type="button" id="save-folder" class="tx-b small" data-tx="save-folder" disabled>Save all</button></div>
</section>
<div class="tx-row spread wrap tx-sel-tools"><label class="tx-check"><input type="checkbox" id="hover-select" data-tx-change="hover">Hover select</label><span class="tx-row"><button type="button" id="select-all" class="tx-link" data-tx="select-all">Select all</button><button type="button" id="clear-selection" class="tx-link" data-tx="clear" disabled>Clear</button></span></div>
<p id="selection-hint" class="tx-hint" hidden>Hover to add. Hovering again keeps a video selected. Uncheck its box to remove it.</p>
<div id="library-notice" class="tx-notice" role="status" hidden></div>
<p id="library-empty" class="tx-empty">Loading your transcripts</p>
<div id="job-list" class="tx-jobs"></div>
<div id="selection-bar" class="tx-selbar" hidden><div id="selection-count" class="tx-hint" role="status"></div><button type="button" id="copy-selected" class="tx-b primary wide" data-tx="copy-selected">Copy selected</button><div class="tx-row"><button type="button" id="save-selected" class="tx-b small" data-tx="save-selected">Save selected</button><select id="move-folder" aria-label="Move selected transcripts to folder" data-tx-change="move-target"></select><button type="button" id="move-selected" class="tx-b small" data-tx="move" disabled>Move</button></div></div>
</aside>
<main class="tx-main">
<section id="attention-empty" class="tx-blank" hidden><h1 id="attention-empty-title">Checking issues</h1><p id="attention-empty-message"></p></section>
<section id="welcome" class="tx-blank"><h1>Your transcripts</h1><p>Queue videos while you browse YouTube. Their full text lands here, in folders, ready to read, copy or save.</p><form id="start-form" class="tx-add wide" data-tx-submit="start"><input id="start-url" aria-label="YouTube video URL" type="url" placeholder="Paste a YouTube video link" autocomplete="off" required><button type="submit" id="start-button" class="tx-b primary">Add to queue</button></form></section>
<article id="reader" class="tx-doc" hidden>
<button type="button" id="back" class="tx-b quiet small tx-back" data-tx="back">{i_back}Library</button>
<header class="tx-doc-head"><h1 id="video-title"></h1><div id="video-metadata" class="tx-meta"></div><div class="tx-row wrap"><span id="job-status" class="tx-chip"></span><a id="video-source" class="tx-link" target="_blank" rel="noopener noreferrer" hidden>Open video</a></div></header>
<section id="progress-panel" class="tx-progress" aria-label="Transcription progress" hidden><div class="tx-row spread"><h2 id="progress-title"></h2><span id="progress-percent" class="tx-count"></span></div><div id="progress-track" class="tx-bar" role="progressbar" aria-label="Overall transcription progress" aria-valuemin="0" aria-valuemax="100" aria-valuenow="0"><i id="progress-bar"></i></div><p id="progress-message" class="tx-hint"></p><p id="progress-chunks" class="tx-hint"></p><p id="youtube-access-help" class="tx-hint" hidden>YouTube may need your browser’s login. In <a href="settings.html">Settings</a>, turn on Use my Brave login, then try again.</p><div class="tx-row"><button type="button" id="cancel-job" class="tx-b small" data-tx="cancel-job">Cancel transcription</button><button type="button" id="retry-job" class="tx-b small" data-tx="retry-job" hidden>Try again</button></div><p id="progress-footer" class="tx-hint"></p></section>
<div id="document-tools" class="tx-tools" hidden><button type="button" id="copy-document" class="tx-b primary" data-tx="copy-document">Copy full transcript</button><button type="button" id="download-md" class="tx-b" data-tx="save-doc" data-arg="md">Save Markdown</button><button type="button" id="download-txt" class="tx-b quiet" data-tx="save-doc" data-arg="txt">Text</button><button type="button" id="download-json" class="tx-b quiet" data-tx="save-doc" data-arg="json">JSON</button><button type="button" id="show-file" class="tx-b quiet" data-tx="show-file">Show file</button><span id="word-count" class="tx-count"></span></div>
<p id="export-state" class="tx-hint" hidden></p>
<div id="reader-notice" class="tx-notice" role="status" hidden></div>
<div id="transcript" class="tx-text" aria-label="Full transcript" hidden></div>
</article>
</main></div></div><div id="toast" class="tx-notice tx-toast" role="status" hidden></div>"#,
        mark = crate::ui_mark(),
        i_refresh = icon(html::I_REFRESH),
        i_plus = icon(html::I_PLUS),
        i_back = icon(html::I_BACK),
    )
}

fn toast(message: &str, kind: &str) {
    notice("toast", message, &format!("tx-toast {kind}"));
    host::later("toast", 4500.);
}

fn render_folder_controls() {
    let (filter_html, move_html, label, is_default, all, busy, has_completed, filter_value) = with(|s| {
        if s.filter != ALL && !s.filter.is_empty() && !s.folders.iter().any(|f| text(f, "id") == s.filter) {
            s.filter = ALL.into();
            s.selected.clear();
            s.errors.clear();
        }
        let option = |v: &str, l: &str| format!("<option value=\"{}\">{}</option>", esc(v), esc(l));
        let named: String = s.folders.iter().map(|f| option(text(f, "id"), text(f, "name"))).collect();
        (
            format!("{}{}{named}", option(ALL, "All transcripts"), option("", "Unfiled")),
            format!("{}{}{named}", option(CHOOSE, "Move to"), option("", "Unfiled")),
            format!("New videos go to {}", s.folder_name(s.default_folder.as_deref())),
            s.filter != ALL && s.filter.as_str() == s.default_folder.as_deref().unwrap_or(""),
            s.filter == ALL,
            s.busy,
            s.visible().iter().any(|j| s.completed(j)),
            s.filter.clone(),
        )
    });
    let moved = host::read_value("move-folder");
    put("folder-filter", &filter_html);
    host::set_value("folder-filter", &filter_value);
    put("move-folder", &move_html);
    host::set_value("move-folder", if moved.is_empty() && !host::read_value("move-folder").is_empty() { CHOOSE } else { &moved });
    if host::read_value("move-folder") == "" && moved != "" {
        host::set_value("move-folder", CHOOSE);
    }
    host::text("default-folder-label", &label);
    disable("set-default-folder", busy || all || is_default);
    host::text("set-default-folder", if is_default { "Used for new videos" } else { "Use for new videos" });
    disable("copy-folder", busy || !has_completed);
    disable("save-folder", busy || !has_completed);
    host::text("copy-folder", if all { "Copy all" } else { "Copy folder" });
    host::text("save-folder", if all { "Save all" } else { "Save folder" });
}

fn update_selection() {
    let (count, loading, failed, any, busy, selected_any, has_completed) = with(|s| {
        let ordered = s.selected_jobs();
        let loading = ordered.iter().filter(|j| !s.docs.contains_key(text(j, "id"))).count();
        let failed = ordered.iter().filter(|j| s.errors.contains(text(j, "id"))).count();
        (ordered.len(), loading, failed, !ordered.is_empty(), s.busy, !s.selected.is_empty(), s.visible().iter().any(|j| s.completed(j)))
    });
    let label = if count == 0 {
        String::new()
    } else {
        format!("{} selected{}", plural(count as f64, "transcript"), if failed > 0 { format!(", {failed} could not load") } else if loading > 0 { format!(", preparing {loading}") } else { ", ready to copy".into() })
    };
    show("selection-bar", any);
    host::text("selection-count", &label);
    disable("copy-selected", !any || (loading > 0 && failed == 0));
    host::text("copy-selected", &if loading > 0 && failed > 0 { "Retry loading selected".to_owned() } else if loading > 0 { "Preparing full transcripts".to_owned() } else if any { format!("Copy selected ({count})") } else { "Copy selected".to_owned() });
    disable("clear-selection", !selected_any);
    disable("select-all", !has_completed);
    disable("save-selected", busy || !any);
    disable("move-selected", busy || !any || host::read_value("move-folder") == CHOOSE);
}

fn row_state(job: &Value, issue: Option<&Value>) -> String {
    let status = text(job, "status");
    if status == "removed" {
        return format!("<span class=\"tx-chip removed\" title=\"{}\">Removed</span>", esc(REMOVED_MESSAGE));
    }
    if let Some(issue) = issue {
        let label = match text(issue, "kind") {
            "failed" => "Failed",
            "cancelled" => "Cancelled",
            "save_failed" => "Save failed",
            "unsaved" => "Not saved",
            "missing" => "Unavailable",
            _ => "Needs attention",
        };
        return format!("<span class=\"tx-chip error\" title=\"{}\">{label}</span>", esc(text(issue, "message")));
    }
    match status {
        "queued" => "<span class=\"tx-chip active\">Queued</span>".into(),
        "downloading" | "preparing" | "transcribing" => {
            let p = job["progress"].as_f64().unwrap_or(0.).clamp(0., 100.);
            format!("<span class=\"tx-chip active\" style=\"--p:{p:.0}%\">{p:.0}%</span>")
        }
        "failed" => "<span class=\"tx-chip error\">Failed</span>".into(),
        "cancelled" => "<span class=\"tx-chip\">Cancelled</span>".into(),
        _ => String::new(),
    }
}

fn render_library() {
    let (html_rows, count, empty, title, attention_only, attention_loaded, current) = with(|s| {
        let removed: Vec<_> = s.jobs.iter().filter(|j| j["status"] == "removed").cloned().collect();
        for j in removed {
            s.reconcile(j);
        }
        let jobs = std::mem::take(&mut s.jobs);
        s.jobs = jobs.into_iter().map(|j| s.reconcile(j)).collect();
        let visible = s.visible();
        let valid: HashSet<String> = visible.iter().filter(|j| s.completed(j)).map(|j| text(j, "id").to_owned()).collect();
        s.selected.retain(|id| valid.contains(id));
        s.errors.retain(|id| valid.contains(id));
        let mut rows = String::with_capacity(visible.len() * 300);
        for job in &visible {
            let id = text(job, "id");
            let title = job_title(job);
            let issue = if s.attention_only { s.attention.iter().find(|i| i["id"] == job["id"]) } else { None };
            let meta = match issue {
                Some(i) => text(i, "message").to_owned(),
                None => {
                    let mut parts = vec![];
                    let when = host::local_time(text(job, "created_at"));
                    if !when.is_empty() {
                        parts.push(when);
                    }
                    if s.filter == ALL && !text(job, "folder_id").is_empty() {
                        parts.push(s.folder_name(job["folder_id"].as_str()));
                    }
                    parts.join(" · ")
                }
            };
            let url = s.reader_url(Some(id));
            let selected = s.selected.contains(id);
            rows.push_str(&format!(
                "<article class=\"tx-job{active}\" id=\"job-{eid}\" data-selected=\"{selected}\" data-job-status=\"{status}\" data-tx-hover=\"job\" data-arg=\"{eid}\"><input type=\"checkbox\" class=\"select-job\" id=\"sel-{eid}\" data-tx-change=\"select\" data-arg=\"{eid}\" aria-label=\"Select full transcript: {etitle}\"{checked}{disabled}><div><a class=\"tx-job-t\" href=\"{url}\" data-tx=\"open\" data-arg=\"{eid}\">{etitle}</a><div class=\"tx-job-m\">{state}<span class=\"tx-job-meta\">{meta}</span></div></div></article>",
                active = if s.current.as_deref() == Some(id) { " on" } else { "" },
                eid = esc(id),
                status = esc(text(job, "status")),
                etitle = esc(&title),
                checked = if selected { " checked" } else { "" },
                disabled = if s.completed(job) { "" } else { " disabled" },
                state = row_state(job, issue),
                meta = esc(&meta),
            ));
        }
        let empty = if s.attention_only {
            if s.attention_loaded { "Nothing needs attention" } else { "Loading issues" }
        } else if s.filter == ALL {
            "Your transcripts appear here. Queue videos from YouTube or paste a link"
        } else {
            "This folder is empty. Move transcripts here, or use it for new videos"
        };
        (rows, visible.len(), if visible.is_empty() { empty } else { "" }, if s.attention_only { "Needs attention" } else { "Library" }, s.attention_only, s.attention_loaded, s.current.clone())
    });
    render_folder_controls();
    host::body_attr("data-view", if attention_only { "attention" } else { "library" });
    host::body_attr("data-open", if current.is_some() { "true" } else { "false" });
    host::text("library-title", title);
    set("attention-filter", "aria-pressed", if attention_only { "true" } else { "false" });
    host::text("attention-filter", if attention_only { "All transcripts" } else { "Needs attention" });
    show("attention-empty", attention_only && current.is_none());
    host::text("attention-empty-title", if attention_loaded { "All clear" } else { "Checking issues" });
    host::text("attention-empty-message", if attention_loaded { "No videos need attention. Your saved transcripts are in the library." } else { "Loading the videos that need your attention." });
    host::text("library-count", &if count > 0 { host::number(count as f64) } else { String::new() });
    show("library-empty", count == 0);
    host::text("library-empty", empty);
    put("job-list", &html_rows);
    update_selection();
}

fn navigate(id: Option<String>, update_history: bool) {
    let url = with(|s| {
        s.current = id.clone();
        s.current_job = None;
        s.load += 1;
        s.reader_url(id.as_deref())
    });
    if update_history {
        host::push_url(&url);
    }
    let attention = with(|s| s.attention_only);
    show("welcome", !attention && id.is_none());
    show("reader", id.is_some());
    show("transcript", false);
    put("transcript", "");
    show("document-tools", false);
    show("progress-panel", false);
    show("export-state", false);
    host::text("video-title", "Loading video");
    put("video-metadata", "");
    show("video-source", false);
    host::text("job-status", "");
    notice("reader-notice", "", "");
    render_library();
    let Some(id) = id else {
        host::doc_title("Transcripts");
        return;
    };
    let job = with(|s| s.jobs.iter().find(|j| text(j, "id") == id).cloned());
    match job {
        Some(j) => render_job(j),
        None => spawn_local(async move {
            let r = send("getJob", json!({"id":id})).await;
            if with(|s| s.current.as_deref() == Some(id.as_str())) {
                match r {
                    Ok(found) => render_job(found),
                    Err(e) => notice("reader-notice", &e, "error"),
                }
            }
        }),
    }
}

fn render_job(job: Value) {
    let id = text(&job, "id").to_owned();
    if with(|s| s.current.as_deref() != Some(id.as_str())) {
        return;
    }
    let (job, same_document, complete, attention, issue, retrying) = with(|s| {
        let job = s.reconcile(job);
        let complete = s.completed(&job);
        let same = s.current_job.as_ref().is_some_and(|c| text(c, "id") == id && c["status"] == "completed") && complete && s.docs.contains_key(&id);
        s.current_job = Some(job.clone());
        (job, same, complete, s.attention_only, s.attention.iter().find(|i| text(i, "id") == id).cloned(), s.retrying.contains(&id))
    });
    let status = text(&job, "status");
    show("welcome", false);
    show("reader", true);
    let title = text(&job, "title");
    host::text("video-title", if title.is_empty() { "Preparing your YouTube video" } else { title });
    host::doc_title(if title.is_empty() { "Transcript" } else { title });
    host::text("job-status", core::status_label(status));
    set("job-status", "class", match status {
        "completed" => "tx-chip ready",
        "removed" => "tx-chip removed",
        "failed" => "tx-chip error",
        "cancelled" => "tx-chip",
        _ => "tx-chip active",
    });
    let metadata = &job["metadata"];
    let channel = metadata["channel"].as_str().or(metadata["uploader"].as_str()).unwrap_or("");
    let duration = core::format_duration(if metadata["duration_seconds"].is_null() { &metadata["duration"] } else { &metadata["duration_seconds"] });
    let published = core::format_published(if metadata["published_date"].is_null() { &metadata["upload_date"] } else { &metadata["published_date"] });
    let mut meta = String::new();
    for part in [channel.to_owned(), duration, format!("Published {published}")] {
        if !part.is_empty() {
            meta.push_str(&format!("<span>{}</span>", esc(&part)));
        }
    }
    put("video-metadata", &meta);
    match core::normalize_video(text(&job, "url")) {
        Ok((_, url)) => {
            set("video-source", "href", &url);
            show("video-source", true);
        }
        Err(_) => show("video-source", false),
    }
    let terminal = core::terminal(status);
    show("progress-panel", !complete && status != "removed");
    show("cancel-job", !terminal);
    show("retry-job", matches!(status, "failed" | "cancelled"));
    disable("retry-job", job["persistence_pending"] == true || retrying);
    host::text("progress-title", core::status_label(status));
    let progress = job["progress"].as_f64().unwrap_or(0.).clamp(0., 100.).round();
    host::text("progress-percent", &format!("{progress:.0}%"));
    set("progress-bar", "style", &format!("transform:scaleX({})", progress / 100.));
    set("progress-track", "aria-valuenow", &format!("{progress:.0}"));
    let message = job["error"].as_str().or(job["message"].as_str()).unwrap_or("The helper is processing the entire recording.");
    host::text("progress-message", message);
    let lower = message.to_ascii_lowercase();
    show("youtube-access-help", status == "failed" && ["sign in", "sign-in", "signin", "login", "cookies", "not a bot"].iter().any(|w| lower.contains(w)));
    let total = job["total_chunks"].as_f64().unwrap_or(0.);
    host::text("progress-chunks", &if total > 0. { format!("{} of {} audio sections transcribed", job["completed_chunks"].as_f64().unwrap_or(0.), total) } else { String::new() });
    host::text("progress-footer", if terminal { "Temporary audio is cleaned up automatically." } else { "You can close this tab. Transcription continues in the background while this computer is awake." });
    let export_error = job["export_error"].as_str();
    let export_path = job["export_path"].as_str();
    show("export-state", export_error.is_some() || export_path.is_some());
    host::text("export-state", &match (export_error, export_path) {
        (Some(e), _) => format!("Could not save to your destination: {e}"),
        (_, Some(p)) => format!("Saved to {p}"),
        _ => String::new(),
    });
    if attention && status != "removed" {
        let show_it = issue.as_ref().is_some_and(|i| status == "completed" || text(i, "kind") == "persistence");
        notice("reader-notice", if show_it { issue.as_ref().map_or("", |i| text(i, "message")) } else { "" }, "error");
        if issue.is_some() && status == "completed" && export_error.is_none() {
            show("export-state", false);
        }
    }
    if !complete {
        with(|s| s.load += 1);
        put("transcript", "");
        show("transcript", false);
        show("document-tools", false);
        host::text("word-count", "");
        if status == "removed" {
            show("export-state", false);
            notice("reader-notice", REMOVED_MESSAGE, "removed");
            render_library();
        }
    } else if !same_document {
        spawn_local(load_document(id));
    }
}

async fn get_document(id: &str) -> Result<Doc, String> {
    if with(|s| s.removed_ids.contains(id)) {
        return Err(REMOVED_MESSAGE.into());
    }
    if let Some(d) = with(|s| s.docs.get(id).cloned()) {
        return Ok(d);
    }
    let r = send("getDocument", json!({"id":id,"format":"md"})).await?;
    if with(|s| s.removed_ids.contains(id)) {
        return Err(REMOVED_MESSAGE.into());
    }
    let Some(content) = r["content"].as_str() else {
        return Err("The helper returned an unreadable transcript document.".into());
    };
    let sections = r["sections"].as_array().map(|a| a.iter().filter_map(|s| Some((s["offset_ms"].as_f64()?, s["text"].as_str()?.to_owned()))).collect()).unwrap_or_default();
    let doc = Doc { content: content.to_owned(), sections };
    with(|s| s.docs.insert(id.to_owned(), doc.clone()));
    Ok(doc)
}

/// The moment of this page: a measure that reads like a book, with a timestamp beside each section the helper knows
/// the start of. The saved document itself stays untouched; this is only how it is set.
fn transcript_html(doc: &Doc, video: &str) -> String {
    let mut out = String::with_capacity(doc.content.len() + 1024);
    if !doc.sections.is_empty() {
        for (ms, text) in &doc.sections {
            let t = core::clock(ms / 1000.);
            let secs = (ms / 1000.).floor();
            out.push_str("<p class=\"ts\">");
            if core::valid_video_id(video) {
                out.push_str(&format!("<a class=\"tx-t\" href=\"https://www.youtube.com/watch?v={video}&amp;t={secs:.0}s\" target=\"_blank\" rel=\"noopener noreferrer\">{t}</a>"));
            } else {
                out.push_str(&format!("<span class=\"tx-t\">{t}</span>"));
            }
            out.push_str(&esc(text));
            out.push_str("</p>");
        }
        return out;
    }
    for p in core::paragraphs(core::document_body(&doc.content)) {
        out.push_str("<p>");
        out.push_str(&esc(p));
        out.push_str("</p>");
    }
    out
}

async fn load_document(id: String) {
    let sequence = with(|s| {
        s.load += 1;
        s.load
    });
    let r = get_document(&id).await;
    let still = with(|s| s.current.as_deref() == Some(id.as_str()) && s.load == sequence && s.current_job.as_ref().is_some_and(|j| s.completed(j)));
    if !still {
        return;
    }
    match r {
        Ok(doc) => {
            let video = with(|s| s.current_job.as_ref().map(|j| text(j, "video_id").to_owned()).unwrap_or_default());
            put("transcript", &transcript_html(&doc, &video));
            show("transcript", true);
            show("document-tools", true);
            host::text("word-count", &plural(core::word_count(core::document_body(&doc.content)) as f64, "word"));
            let issue = with(|s| if s.attention_only { s.attention.iter().find(|i| text(i, "id") == id).map(|i| text(i, "message").to_owned()) } else { None });
            notice("reader-notice", &issue.unwrap_or_default(), "error");
            update_selection();
        }
        Err(e) => notice("reader-notice", &e, "error"),
    }
}

async fn refresh() {
    if with(|s| std::mem::replace(&mut s.refreshing, true)) {
        return;
    }
    disable("refresh", true);
    let wanted_attention = with(|s| s.attention_only);
    let jobs = send("listJobs", json!({})).await;
    let library = send("getLibrary", json!({})).await;
    let issues = if wanted_attention { Some(send("getQueueIssues", json!({})).await) } else { None };
    let mut active = false;
    match (jobs, library) {
        (Ok(data), Ok(library)) => {
            let issues_ok = match issues {
                Some(Ok(v)) if v["attention"].is_array() => {
                    let list: Vec<Value> = v["attention"].as_array().cloned().unwrap_or_default().into_iter().filter(|i| !matches!(text(i, "kind"), "cancelled" | "removed")).collect();
                    with(|s| {
                        if s.attention_only {
                            s.attention = list;
                            s.attention_loaded = true;
                        }
                    });
                    Ok(())
                }
                Some(Ok(_)) => Err("Issue details are unavailable. Update the helper and try again.".to_owned()),
                Some(Err(e)) => Err(e),
                None => Ok(()),
            };
            let mut list = data["jobs"].as_array().cloned().unwrap_or_default();
            list.sort_by(|a, b| text(b, "created_at").cmp(text(a, "created_at")).then_with(|| text(a, "id").cmp(text(b, "id"))));
            active = list.iter().any(|j| !core::terminal(text(j, "status")));
            with(|s| {
                s.jobs = list;
                s.folders = library["folders"].as_array().cloned().unwrap_or_default();
                s.default_folder = library["default_folder_id"].as_str().map(str::to_owned);
                // Opened from a video's Transcript button: its newest finished job, else its newest job.
                if let Some(video) = s.video.take() {
                    let of_video: Vec<&Value> = s.jobs.iter().filter(|j| text(j, "video_id") == video).collect();
                    s.current = of_video.iter().find(|j| j["status"] == "completed").or(of_video.first()).map(|j| text(j, "id").to_owned());
                    let url = s.reader_url(s.current.as_deref());
                    host::replace_url(&url);
                }
            });
            host::text("connection", "Helper connected");
            set("connection", "class", "tx-state ready");
            notice("library-notice", "", "");
            with(|s| {
                if s.attention_only && s.current.is_none() {
                    s.current = s.attention.iter().find(|i| !text(i, "id").is_empty()).map(|i| text(i, "id").to_owned());
                    if s.current.is_some() {
                        host::replace_url(&s.reader_url(s.current.as_deref()));
                    }
                }
            });
            render_library();
            let current = with(|s| s.current.clone());
            if let Some(id) = current {
                match with(|s| s.jobs.iter().find(|j| text(j, "id") == id).cloned()) {
                    Some(j) => render_job(j),
                    None => match send("getJob", json!({"id":id})).await {
                        Ok(j) => render_job(j),
                        Err(e) => notice("reader-notice", &e, "error"),
                    },
                }
            }
            if let Err(e) = issues_ok {
                notice("library-notice", &e, "error");
            }
        }
        (a, b) => {
            let e = a.err().or(b.err()).unwrap_or_default();
            host::text("connection", "Check helper");
            set("connection", "class", "tx-state error");
            if with(|s| s.visible().is_empty()) {
                show("library-empty", true);
                host::text("library-empty", "Open Settings to check the helper connection and load saved transcripts");
            }
            notice("library-notice", &e, "error");
            if with(|s| s.attention_only && s.current.is_none()) {
                host::text("attention-empty-title", "Could not load issues");
                host::text("attention-empty-message", &e);
            }
        }
    }
    with(|s| s.refreshing = false);
    disable("refresh", false);
    let again = wanted_attention != with(|s| s.attention_only);
    // Active jobs are watched closely; a finished library only needs an occasional look.
    host::later("poll", if again { 0. } else if host::hidden() { 30000. } else if active { 2500. } else { 15000. });
}

fn select_job(id: &str, on: bool) {
    // Only finished transcripts can be selected, however the row was reached.
    if !with(|s| s.jobs.iter().find(|j| text(j, "id") == id).is_some_and(|j| s.completed(j))) {
        return;
    }
    let fetch = with(|s| {
        s.errors.remove(id);
        if on {
            s.selected.insert(id.to_owned())
        } else {
            s.selected.remove(id);
            false
        }
    });
    set(&format!("job-{id}"), "data-selected", if on { "true" } else { "false" });
    host::set_checked(&format!("sel-{id}"), on);
    if fetch {
        prefetch(id.to_owned());
    }
    update_selection();
}

fn prefetch(id: String) {
    spawn_local(async move {
        let r = get_document(&id).await;
        with(|s| {
            if let Err(_e) = &r
                && s.selected.contains(&id)
            {
                s.errors.insert(id.clone());
            }
        });
        update_selection();
    });
}

fn clear_selection() {
    with(|s| {
        s.selected.clear();
        s.errors.clear();
    });
}

fn copy_message(unique: usize, total: usize, tail: &str) -> String {
    format!("Copied {}.{}", plural(unique as f64, "full transcript"), if unique < total { " Exact duplicates were included only once.".to_owned() } else { tail.to_owned() })
}

fn copy_selected() {
    let (ordered, missing) = with(|s| {
        let ordered = s.selected_jobs();
        let missing: Vec<String> = ordered.iter().filter(|j| !s.docs.contains_key(text(j, "id"))).map(|j| text(j, "id").to_owned()).collect();
        (ordered, missing)
    });
    if ordered.is_empty() {
        return;
    }
    if !missing.is_empty() {
        missing.into_iter().for_each(prefetch);
        return;
    }
    let contents: Vec<String> = with(|s| ordered.iter().filter_map(|j| s.docs.get(text(j, "id")).map(|d| d.content.clone())).collect());
    let unique = core::unique_documents(&contents);
    let text = core::combine_documents(&unique);
    let total = ordered.len();
    spawn_local(async move {
        if host::copy_text(&text).await {
            toast(&copy_message(unique.len(), total, " Paste into your AI chat or document."), "success");
        } else {
            toast("Clipboard access was blocked. Keep this tab focused and try again, or save Markdown.", "error");
        }
    });
}

fn copy_folder() {
    let ordered: Vec<String> = with(|s| s.visible().into_iter().filter(|j| s.completed(j)).map(|j| text(&j, "id").to_owned()).collect());
    if ordered.is_empty() {
        return;
    }
    with(|s| s.busy = true);
    render_folder_controls();
    let total = ordered.len();
    let counted = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = counted.clone();
    // The pending text goes to the clipboard inside the click, so the browser keeps the user's activation while the
    // documents load.
    let text: Promise = future_to_promise(async move {
        let mut contents = vec![];
        for id in &ordered {
            contents.push(get_document(id).await.map_err(|e| JsValue::from_str(&e))?.content);
        }
        let unique = core::unique_documents(&contents);
        counter.set(unique.len());
        Ok(JsValue::from_str(&core::combine_documents(&unique)))
    });
    spawn_local(async move {
        if host::copy_later(&text).await {
            toast(&copy_message(counted.get(), total, ""), "success");
        } else {
            toast("Could not copy this folder. Try again with this tab focused.", "error");
        }
        with(|s| s.busy = false);
        render_folder_controls();
        update_selection();
    });
}

fn save_exports(scope: Value) {
    with(|s| s.busy = true);
    render_folder_controls();
    update_selection();
    spawn_local(async move {
        match send("exportJobs", scope).await {
            Ok(r) => {
                let (saved, failed) = (ui::num(&r["saved_count"]), ui::num(&r["failed_count"]));
                let (removed, excluded) = (ui::num(&r["removed_duplicate_count"]), ui::num(&r["skipped_removed_count"]));
                let detail = r["errors"].as_array().and_then(|a| a.iter().find_map(|e| e["error"].as_str())).unwrap_or("");
                let mut m = format!("{} saved to your chosen folder.", plural(saved, "document"));
                if removed > 0. {
                    m.push_str(&format!(" {} removed.", plural(removed, "exact duplicate file")));
                }
                if excluded > 0. {
                    m.push_str(&format!(" {} kept excluded.", plural(excluded, "removed transcript")));
                }
                if failed > 0. {
                    m.push_str(&format!(" {} could not be saved. {detail}", host::number(failed)));
                }
                toast(m.trim_end(), if failed > 0. { "error" } else if excluded > 0. && saved == 0. { "removed" } else { "success" });
                refresh().await;
            }
            Err(e) => toast(&format!("{e} Choose or check your save folder in Settings."), "error"),
        }
        with(|s| s.busy = false);
        render_folder_controls();
        update_selection();
    });
}

pub fn on_event(kind: &str, name: &str, arg: &str, value: &str) {
    match (kind, name) {
        ("timer", "poll") => spawn_local(refresh()),
        ("timer", "toast") => show("toast", false),
        ("visible", _) if !host::hidden() => spawn_local(refresh()),
        ("popstate", _) => {
            with(|s| s.attention_only = ui::query_param("view").as_deref() == Some("attention"));
            navigate(ui::query_param("id"), false);
            spawn_local(refresh());
        }
        ("click", "refresh") => spawn_local(refresh()),
        ("click", "open") => navigate(Some(arg.to_owned()), true),
        ("click", "back") => navigate(None, true),
        ("click", "attention") => {
            with(|s| {
                s.attention_only = !s.attention_only;
                if !s.attention_only {
                    s.filter = ALL.into();
                }
                s.attention_loaded = false;
                s.attention.clear();
            });
            clear_selection();
            navigate(None, true);
            spawn_local(refresh());
        }
        ("change", "filter") => {
            with(|s| s.filter = value.to_owned());
            clear_selection();
            render_library();
        }
        ("change", "move-target") => update_selection(),
        ("change", "hover") => {
            let on = host::read_checked("hover-select");
            with(|s| s.hover_select = on);
            show("selection-hint", on);
        }
        ("change", "select") => select_job(arg, host::read_checked(&format!("sel-{arg}"))),
        ("hover", "job") => {
            if with(|s| s.hover_select) {
                select_job(arg, true);
            }
        }
        ("click", "select-all") => {
            let ids: Vec<String> = with(|s| s.visible().into_iter().filter(|j| s.completed(j)).map(|j| text(&j, "id").to_owned()).collect());
            for id in ids {
                select_job(&id, true);
            }
        }
        ("click", "clear") => {
            clear_selection();
            render_library();
        }
        ("click", "copy-selected") => copy_selected(),
        ("click", "copy-folder") => copy_folder(),
        ("click", "save-selected") => {
            let ids: Vec<String> = with(|s| s.selected_jobs().iter().map(|j| text(j, "id").to_owned()).collect());
            if !ids.is_empty() {
                save_exports(json!({"jobIds":ids}));
            }
        }
        ("click", "save-folder") => {
            let scope = with(|s| {
                if !s.visible().iter().any(|j| s.completed(j)) {
                    None
                } else if s.filter == ALL {
                    Some(json!({}))
                } else {
                    Some(json!({"folderId":if s.filter.is_empty() { Value::Null } else { json!(s.filter) }}))
                }
            });
            if let Some(scope) = scope {
                save_exports(scope);
            }
        }
        ("click", "move") => {
            let target = host::read_value("move-folder");
            let ids: Vec<String> = with(|s| s.selected_jobs().iter().map(|j| text(j, "id").to_owned()).collect());
            if ids.is_empty() || target == CHOOSE {
                return;
            }
            with(|s| s.busy = true);
            update_selection();
            spawn_local(async move {
                let folder = if target.is_empty() { Value::Null } else { json!(target) };
                match send("moveJobs", json!({"jobIds":ids,"folderId":folder})).await {
                    Ok(r) => {
                        let moved: HashMap<String, Value> = r["jobs"].as_array().cloned().unwrap_or_default().into_iter().map(|j| (text(&j, "id").to_owned(), j)).collect();
                        let name = with(|s| {
                            for j in &mut s.jobs {
                                if let Some(n) = moved.get(text(j, "id")) {
                                    *j = n.clone();
                                }
                            }
                            s.folder_name(Some(&target))
                        });
                        clear_selection();
                        toast(&format!("Moved {} to {name}.", plural(ids.len() as f64, "transcript")), "success");
                    }
                    Err(e) => toast(&e, "error"),
                }
                with(|s| s.busy = false);
                render_library();
            });
        }
        ("click", "set-default") => {
            let folder = with(|s| (s.filter != ALL).then(|| s.filter.clone()));
            let Some(folder) = folder else { return };
            with(|s| s.busy = true);
            render_folder_controls();
            spawn_local(async move {
                match send("setDefaultFolder", json!({"folderId":if folder.is_empty() { Value::Null } else { json!(folder) }})).await {
                    Ok(library) => {
                        let name = with(|s| {
                            s.folders = library["folders"].as_array().cloned().unwrap_or_default();
                            s.default_folder = library["default_folder_id"].as_str().map(str::to_owned);
                            s.folder_name(s.default_folder.clone().as_deref())
                        });
                        toast(&format!("New videos will go to {name}."), "success");
                    }
                    Err(e) => toast(&e, "error"),
                }
                with(|s| s.busy = false);
                render_library();
            });
        }
        ("submit", "folder") => {
            let name = host::read_value("folder-name");
            disable("create-folder", true);
            spawn_local(async move {
                match send("createFolder", json!({"name":name})).await {
                    Ok(folder) => {
                        host::set_value("folder-name", "");
                        with(|s| {
                            s.filter = text(&folder, "id").to_owned();
                            s.folders.push(folder.clone());
                        });
                        clear_selection();
                        render_library();
                        toast(&format!("Created {}. Use it for new videos to file them here.", text(&folder, "name")), "success");
                    }
                    Err(e) => toast(&e, "error"),
                }
                disable("create-folder", false);
            });
        }
        ("submit", "start") => start_from_form(),
        ("click", "copy-document") => {
            let content = with(|s| s.current.as_ref().and_then(|id| s.docs.get(id)).map(|d| d.content.clone()));
            if let Some(content) = content {
                spawn_local(async move {
                    if host::copy_text(&content).await {
                        toast("Full transcript copied. Ready to paste.", "success");
                    } else {
                        toast("Clipboard access was blocked. Keep this tab focused and try again, or save Markdown.", "error");
                    }
                });
            }
        }
        ("click", "save-doc") => save_document(arg.to_owned()),
        ("click", "show-file") => show_file(),
        ("click", "cancel-job") => {
            let id = with(|s| s.current_job.as_ref().filter(|j| !core::terminal(text(j, "status"))).map(|j| text(j, "id").to_owned()));
            let Some(id) = id else { return };
            disable("cancel-job", true);
            spawn_local(async move {
                match send("cancelJob", json!({"id":id})).await {
                    Ok(j) => {
                        render_job(j);
                        refresh().await;
                    }
                    Err(e) => notice("reader-notice", &e, "error"),
                }
                disable("cancel-job", false);
            });
        }
        ("click", "retry-job") => retry_job(),
        _ => {}
    }
}

fn start_from_form() {
    let raw = host::read_value("start-url");
    disable("start-button", true);
    let video = core::normalize_video(&raw);
    spawn_local(async move {
        let result = async {
            let (id, url) = video?;
            if with(|s| s.removed_videos.contains(&id)) {
                host::set_value("start-url", "");
                toast(REMOVED_MESSAGE, "removed");
                return Ok(());
            }
            let mut message = json!({"url":url});
            let filter = with(|s| s.filter.clone());
            if filter != ALL {
                message["folderId"] = if filter.is_empty() { Value::Null } else { json!(filter) };
            }
            let job = send("startJob", message).await?;
            let removed = job["status"] == "removed";
            if removed {
                with(|s| {
                    s.reconcile(job.clone());
                });
            }
            host::set_value("start-url", "");
            toast(if removed { REMOVED_MESSAGE } else { "Added to your queue. You can add another video while it transcribes." }, if removed { "removed" } else { "success" });
            refresh().await;
            Ok::<(), String>(())
        }
        .await;
        if let Err(e) = result {
            toast(&e, "error");
        }
        disable("start-button", false);
    });
}

fn save_document(format: String) {
    let id = with(|s| s.current_job.as_ref().filter(|j| s.completed(j)).map(|j| text(j, "id").to_owned()));
    let Some(id) = id else { return };
    let button = format!("download-{format}");
    disable(&button, true);
    spawn_local(async move {
        match send("exportJobs", json!({"jobIds":[id],"format":format})).await {
            Ok(r) if ui::num(&r["skipped_removed_count"]) > 0. => {
                refresh().await;
                toast(REMOVED_MESSAGE, "removed");
            }
            Ok(r) if ui::num(&r["failed_count"]) > 0. || ui::num(&r["saved_count"]) != 1. => {
                let e = r["errors"][0]["error"].as_str().unwrap_or("The transcript could not be saved.");
                toast(&format!("{e} Check your save folder in Settings."), "error");
            }
            Ok(_) => {
                toast(&format!("{} saved to your chosen folder.", match format.as_str() {
                    "md" => "Markdown",
                    "txt" => "Text",
                    _ => "JSON",
                }), "success");
                refresh().await;
            }
            Err(e) => toast(&format!("{e} Check your save folder in Settings."), "error"),
        }
        disable(&button, false);
    });
}

fn show_file() {
    let video = with(|s| s.current_job.as_ref().map(|j| text(j, "video_id").to_owned()));
    let Some(video) = video else { return };
    disable("show-file", true);
    spawn_local(async move {
        match send("revealVideoFile", json!({"videoId":video})).await {
            Ok(r) if r["revealed"] == true => toast("Transcript file shown.", "success"),
            Ok(_) => toast("The transcript file could not be shown. Check your save folder in Settings.", "error"),
            Err(e) => toast(&e, "error"),
        }
        disable("show-file", false);
    });
}

fn retry_job() {
    let source = with(|s| {
        s.current_job.clone().filter(|j| matches!(text(j, "status"), "failed" | "cancelled") && !s.removed_ids.contains(text(j, "id")) && j["persistence_pending"] != true && !s.retrying.contains(text(j, "id")))
    });
    let Some(source) = source else { return };
    let id = text(&source, "id").to_owned();
    with(|s| s.retrying.insert(id.clone()));
    disable("retry-job", true);
    spawn_local(async move {
        let folder = if text(&source, "folder_id").is_empty() { Value::Null } else { source["folder_id"].clone() };
        match send("startJob", json!({"url":source["url"],"folderId":folder})).await {
            Ok(job) => {
                if with(|s| s.current.as_deref() == Some(id.as_str())) {
                    navigate(Some(text(&job, "id").to_owned()), true);
                }
                refresh().await;
            }
            Err(e) => {
                if with(|s| s.current.as_deref() == Some(id.as_str())) {
                    notice("reader-notice", &e, "error");
                }
            }
        }
        with(|s| s.retrying.remove(&id));
        let off = with(|s| s.current_job.as_ref().is_some_and(|j| j["persistence_pending"] == true) || s.current.as_ref().is_some_and(|c| s.retrying.contains(c)));
        disable("retry-job", off);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transcript_is_escaped_and_paragraphed() {
        let doc = Doc { content: "# T\n\nPublished: Unknown\n\n<b>one</b>\n\ntwo\n".into(), sections: vec![] };
        assert_eq!(transcript_html(&doc, "abcdefghijk"), "<p>&lt;b&gt;one&lt;/b&gt;</p><p>two</p>");
    }
    #[test]
    fn timestamps_link_to_the_moment() {
        let doc = Doc { content: String::new(), sections: vec![(61000., "<x>".into())] };
        let h = transcript_html(&doc, "abcdefghijk");
        assert!(h.contains("t=61s") && h.contains(">1:01</a>") && h.contains("&lt;x&gt;"));
        let h = transcript_html(&doc, "bad\"id");
        assert!(!h.contains("href"));
    }
}
