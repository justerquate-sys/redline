use crate::{
    jobs::{Engine, ManagedTask, active, merge, strings, text, valid_id, valid_video},
    process::{self, cancelled},
};
use chrono::{Datelike, NaiveDate, Utc};
use parking_lot::Mutex;
use scribe_common::{Error, Result, Settings, atomic_json, now, publication_date, read_json};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::{Arc, atomic::Ordering},
};
use tokio_util::sync::CancellationToken;
use unicode_normalization::UnicodeNormalization;

const INTERRUPTED: &[&str] = &[
    "The helper stopped while scanning this channel. No batch was queued; scan it again.",
    "The helper stopped while scanning this channel. Scan it again.",
    "The helper stopped before the channel scan finished.",
    "The helper stopped before this channel scan finished.",
];

#[derive(Clone, Copy, Default)]
struct VerifiedJob {
    document_present: bool,
    saved: bool,
}

/// Fresh facts for one response only. Global counts, batch counts, and duplicate
/// attempts share verification rather than rereading the same text repeatedly.
/// Nothing survives into the next request, so edits/deletions remain visible.
struct ProgressVerification<'a> {
    engine: &'a Engine,
    completed_by_video: HashMap<&'a str, Vec<&'a Value>>,
    checked: HashMap<String, VerifiedJob>,
    saved_facts: HashMap<String, bool>,
}
impl<'a> ProgressVerification<'a> {
    fn new(engine: &'a Engine, jobs: &'a [Value]) -> Self {
        let mut completed_by_video: HashMap<&str, Vec<&Value>> = HashMap::new();
        for job in jobs {
            if text(job, "status") == "completed" {
                completed_by_video
                    .entry(text(job, "video_id"))
                    .or_default()
                    .push(job);
            }
        }
        Self {
            engine,
            completed_by_video,
            checked: HashMap::new(),
            saved_facts: engine
                .inner
                .library
                .saved_job_facts(jobs, &engine.inner.jobs_root),
        }
    }
    fn verify(&mut self, job: &Value) -> VerifiedJob {
        let id = text(job, "id");
        if let Some(result) = self.checked.get(id) {
            return *result;
        }
        let saved = self.saved_facts.get(id).copied().unwrap_or(false);
        let document_present = text(job, "status") == "completed"
            && (saved
                || self
                    .engine
                    .inner
                    .jobs_root
                    .join(id)
                    .join("transcript.json")
                    .is_file());
        let result = VerifiedJob {
            document_present,
            saved: document_present && saved,
        };
        self.checked.insert(id.to_owned(), result);
        result
    }
    fn saved(&mut self, job: &Value) -> bool {
        if self.verify(job).saved {
            return true;
        }
        let video = text(job, "video_id");
        if self.engine.inner.removals.contains(video) {
            return false;
        }
        // A channel/batch can retain an older unsaved attempt while another
        // completed attempt of the same video already has a verified export.
        let candidates = self
            .completed_by_video
            .get(video)
            .cloned()
            .unwrap_or_default();
        candidates.into_iter().any(|other| self.verify(other).saved)
    }
}
fn channel_id(value: &str) -> bool {
    value.len() == 24
        && value.starts_with("UC")
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn decode_path(raw: &str) -> Result<String> {
    let mut bytes = vec![];
    let raw = raw.as_bytes();
    let mut n = 0;
    while n < raw.len() {
        if raw[n] == b'%' {
            if n + 2 >= raw.len() {
                return Err(channel_url_error());
            }
            let hex = std::str::from_utf8(&raw[n + 1..n + 3]).map_err(|_| channel_url_error())?;
            bytes.push(u8::from_str_radix(hex, 16).map_err(|_| channel_url_error())?);
            n += 3;
        } else {
            bytes.push(raw[n]);
            n += 1;
        }
    }
    String::from_utf8(bytes).map_err(|_| channel_url_error())
}
fn channel_url_error() -> Error {
    Error::bad_request(
        "Use a YouTube channel URL such as youtube.com/@handle or youtube.com/channel/UC…, not a video or playlist URL.",
    )
}
static CHANNEL_NAME: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^[\p{L}\p{M}\p{N}_.·-]+$").expect("constant channel pattern")
});
pub fn canonical_channel_url(value: &str) -> Result<String> {
    if value.len() > 4096 {
        return Err(channel_url_error());
    }
    let parsed = url::Url::parse(value.trim()).map_err(|_| channel_url_error())?;
    if !["https", "http"].contains(&parsed.scheme())
        || !["youtube.com", "www.youtube.com", "m.youtube.com"]
            .contains(&parsed.host_str().unwrap_or(""))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some_and(|p| p != 80 && p != 443)
        || parsed
            .query_pairs()
            .any(|(key, _)| key == "v" || key == "list")
    {
        return Err(channel_url_error());
    }
    let path = decode_path(parsed.path())?;
    let mut parts: Vec<&str> = path.trim_end_matches('/').split('/').skip(1).collect();
    if parts
        .last()
        .is_some_and(|v| ["videos", "shorts", "streams"].contains(v))
    {
        parts.pop();
    }
    let name = match parts.as_slice() {
        [handle] if handle.starts_with('@') => &handle[1..],
        [kind, name] if ["channel", "c", "user"].contains(kind) => {
            if *kind == "channel" && !channel_id(name) {
                return Err(channel_url_error());
            }
            name
        }
        _ => return Err(channel_url_error()),
    };
    if name.is_empty()
        || name.chars().count() > 128
        || matches!(name, "." | "..")
        || !CHANNEL_NAME.is_match(name)
    {
        return Err(channel_url_error());
    }
    let mut canonical =
        url::Url::parse("https://www.youtube.com/").map_err(|_| channel_url_error())?;
    canonical.set_path(&format!("/{}", parts.join("/")));
    Ok(canonical.to_string())
}
pub fn calendar_year_cutoff(date: NaiveDate) -> NaiveDate {
    date.with_year(date.year() - 1).unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(date.year() - 1, 2, 28).expect("February28 is valid")
    })
}
pub fn video_date(value: &Value) -> Option<NaiveDate> {
    ["release_date", "upload_date"]
        .iter()
        .find_map(|key| NaiveDate::parse_from_str(&publication_date(&value[*key]), "%Y-%m-%d").ok())
}
pub fn classify_video(
    info: &Value,
    scope: &str,
    cutoff: Option<NaiveDate>,
    as_of: NaiveDate,
) -> Result<&'static str> {
    if !["year", "all"].contains(&scope) {
        return Err(Error::bad_request(
            "Choose all videos or videos from the past year.",
        ));
    }
    if info["is_live"] == true
        || ["is_live", "is_upcoming", "post_live"].contains(&text(info, "live_status"))
        || info
            .get("availability")
            .is_some_and(|v| !v.is_null() && v != "public")
    {
        return Ok("unavailable");
    }
    let published = video_date(info);
    if published.is_some_and(|v| v > as_of) {
        return Ok("unavailable");
    }
    if scope == "year" {
        let cutoff = cutoff.ok_or_else(|| {
            Error::bad_request("The channel selection is missing its date cutoff.")
        })?;
        match published {
            None => return Ok("unknown"),
            Some(date) if date < cutoff => return Ok("old"),
            _ => {}
        }
    }
    Ok("eligible")
}
fn compact(info: &Value) -> Value {
    let mut value = json!({});
    for key in [
        "id",
        "title",
        "channel_id",
        "upload_date",
        "release_date",
        "availability",
        "live_status",
        "is_live",
        "duration",
    ] {
        value[key] = info[key].clone();
    }
    value
}
#[derive(Default)]
struct Cursor {
    seen: HashSet<String>,
    old_streak: usize,
}
impl Engine {
    pub(crate) fn update_preview(&self, id: &str, patch: Value) -> Result<Value> {
        let mut state = self.inner.state.write();
        let mut preview = state
            .previews
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found("Channel preview not found."))?;
        merge(&mut preview, patch);
        if preview["user_cancelled"] == true {
            merge(
                &mut preview,
                json!({"status":"cancelled","error":Value::Null,"queue_error":Value::Null}),
            );
            if matches!(
                text(&preview, "queue_state"),
                "queueing" | "queued" | "failed"
            ) {
                preview["queue_state"] = json!("cancelled");
            }
        }
        preview["updated_at"] = json!(now());
        atomic_json(
            &self
                .inner
                .data
                .join("channel-previews")
                .join(format!("{id}.json")),
            &preview,
        )?;
        state.previews.insert(id.into(), preview.clone());
        Ok(preview)
    }
    pub(crate) fn preview(&self, id: &str) -> Result<Value> {
        self.inner
            .state
            .read()
            .previews
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found("Channel preview not found. Scan the channel again."))
    }
    pub(crate) fn load_channels(&self) -> Result<()> {
        let root = self.inner.data.join("channel-previews");
        std::fs::create_dir_all(&root)?;
        for file in std::fs::read_dir(&root)? {
            let Ok(file) = file else { continue };
            if !file.file_type().is_ok_and(|t| t.is_file()) {
                continue;
            }
            let id = file
                .path()
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_owned();
            if !valid_id(&id) {
                continue;
            }
            let Ok(mut preview) = read_json(&file.path()) else {
                continue;
            };
            if !preview.is_object() || text(&preview, "id") != id {
                continue;
            }
            preview["include_shorts"] = json!(preview["include_shorts"] == true);
            if preview["user_cancelled"] == true {
                preview["status"] = json!("cancelled");
                if text(&preview, "queue_state") == "queueing" {
                    preview["queue_state"] = json!("cancelled");
                }
            } else if text(&preview, "queue_state") == "queueing" {
                preview["queue_state"] = json!("failed");
                preview["queue_error"] = json!(
                    "The helper stopped while adding videos. Already saved jobs are retained; retry to add the remaining videos."
                );
                atomic_json(&file.path(), &preview)?;
            }
            self.inner.state.write().previews.insert(id, preview);
        }
        Ok(())
    }
    pub(crate) fn resume_channels(&self) -> Result<()> {
        let previews: Vec<_> = self.inner.state.read().previews.values().cloned().collect();
        for preview in previews {
            let interrupted = matches!(text(&preview, "status"), "waiting" | "scanning")
                || (text(&preview, "status") == "failed"
                    && INTERRUPTED.contains(&text(&preview, "error")));
            if preview["user_cancelled"] == true
                || !interrupted
                || !preview["queue_state"].is_null()
            {
                continue;
            }
            let id = text(&preview, "id");
            let valid = canonical_channel_url(text(&preview, "channel_url")).is_ok()
                && ["year", "all"].contains(&text(&preview, "scope"))
                && NaiveDate::parse_from_str(text(&preview, "as_of_date"), "%Y-%m-%d").is_ok_and(
                    |date| {
                        text(&preview, "scope") != "year"
                            || text(&preview, "cutoff_date")
                                == calendar_year_cutoff(date).to_string()
                    },
                );
            if !valid {
                self.update_preview(id,json!({"status":"failed","error":"The saved channel scan could not be resumed safely. Scan the channel again."}))?;
                continue;
            }
            let mut imports = preview["import_job_ids"]
                .as_object()
                .cloned()
                .unwrap_or_default();
            for job in self.jobs() {
                let mut origins = strings(&job["channel_import_ids"]);
                origins.push(text(&job, "channel_import_id").into());
                origins.push(text(&job["metadata"], "channel_import_id").into());
                if origins.iter().any(|origin| origin == id) {
                    imports
                        .entry(text(&job, "video_id").to_owned())
                        .or_insert(job["id"].clone());
                }
            }
            let mut patch = json!({"status":"waiting","selection_version":2,"channel_name":"","channel_id":Value::Null,"import_job_ids":imports,"found_count":0,"processed_count":0,"eligible_count":0,"existing_count":0,"queued_count":0,"skipped_old":0,"skipped_unknown":0,"skipped_unavailable":0,"estimated_duration_seconds":0,"unknown_duration_count":0,"error":Value::Null,"warnings":[],"videos":[],"discovery_complete":false,"queue_result":Value::Null,"message":"Resuming interrupted channel discovery. Waiting for a scan slot…"});
            if preview["auto_queue"] == true {
                patch["batch_id"] = json!(self.accept_batch(true)?);
            }
            self.update_preview(id, patch)?;
            self.schedule_scan(id)?;
        }
        Ok(())
    }
    pub(crate) fn start_channel(&self, input: &Value, auto_queue: bool) -> Result<Value> {
        let _submission = self.inner.submission_lock.lock();
        self.ensure_status_writable()?;
        if self.inner.stopping.load(Ordering::SeqCst) || self.inner.closing.load(Ordering::SeqCst) {
            return Err(Error::conflict(
                "The queue is stopping. Wait before adding a channel.",
            ));
        }
        let url = canonical_channel_url(text(input, "url"))?;
        let scope = text(input, "scope");
        if !["year", "all"].contains(&scope) {
            return Err(Error::bad_request(
                "Choose all videos or videos from the past year.",
            ));
        }
        if input.get("include_shorts").is_some_and(|v| !v.is_boolean()) {
            return Err(Error::bad_request(
                "Choose whether to include Shorts using a boolean option.",
            ));
        }
        let shorts = input["include_shorts"] == true;
        if auto_queue {
            self.reconcile_removals(true)?;
        }
        let previews: Vec<_> = self.inner.state.read().previews.values().cloned().collect();
        for preview in &previews {
            if preview["channel_url"] == url
                && preview["scope"] == scope
                && (preview["include_shorts"] == true) == shorts
                && (preview["auto_queue"] == true) == auto_queue
            {
                let live = matches!(text(preview, "status"), "waiting" | "scanning");
                let unfinished = auto_queue
                    && text(preview, "status") == "ready"
                    && !self.channel_progress(preview)["finished"]
                        .as_bool()
                        .unwrap_or(false);
                if live || unfinished {
                    return Ok(
                        json!({"id":preview["id"],"status":preview["status"],"auto_queue":auto_queue}),
                    );
                }
            }
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let date = Utc::now().date_naive();
        let mut preview = json!({"id":id,"status":"waiting","channel_name":"","channel_url":url,"channel_id":Value::Null,"scope":scope,"include_shorts":shorts,"selection_version":2,"cutoff_date":if scope=="year"{json!(calendar_year_cutoff(date).to_string())}else{Value::Null},"as_of_date":date.to_string(),"found_count":0,"processed_count":0,"eligible_count":0,"existing_count":0,"skipped_old":0,"skipped_unknown":0,"skipped_unavailable":0,"estimated_duration_seconds":0,"unknown_duration_count":0,"error":Value::Null,"warnings":[],"message":"Waiting for a channel scan slot. Up to two channels can be scanned at once…","created_at":now(),"updated_at":now(),"videos":[],"folder_id":Value::Null,"queue_state":Value::Null,"queue_result":Value::Null,"queue_error":Value::Null,"queue_processed_count":0,"auto_queue":auto_queue,"queued_count":0,"import_job_ids":{},"discovery_complete":false,"current_tab":Value::Null});
        if auto_queue {
            preview["batch_id"] = json!(self.accept_batch(false)?);
        }
        atomic_json(
            &self
                .inner
                .data
                .join("channel-previews")
                .join(format!("{id}.json")),
            &preview,
        )?;
        self.inner
            .state
            .write()
            .previews
            .insert(id.clone(), preview);
        self.schedule_scan(&id)?;
        Ok(json!({"id":id,"status":"waiting","auto_queue":auto_queue}))
    }
    fn schedule_scan(&self, id: &str) -> Result<()> {
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        self.inner.producers.lock().insert(
            id.into(),
            ManagedTask {
                cancel: cancel.clone(),
                done: done.clone(),
            },
        );
        let engine = self.clone();
        let id = id.to_owned();
        let settings = self.inner.settings.read().clone();
        let handle = tokio::spawn(async move {
            let result=async {let permit=tokio::select!{biased;_=cancel.cancelled()=>return Err(cancelled()),permit=engine.inner.scan_slots.clone().acquire_owned()=>permit.map_err(|_|cancelled())?};engine.update_preview(&id,json!({"status":"scanning","message":"Discovering public channel uploads…"}))?;let result=engine.scan_channel(&id,&settings,&cancel).await;drop(permit);result}.await;
            if let Err(error) = result {
                let closing = engine.inner.closing.load(Ordering::SeqCst);
                let _=engine.update_preview(&id,json!({"status":if error.status==499&&!closing{"cancelled"}else{"failed"},"error":if error.status==499&&!closing{Value::Null}else{json!(if closing{"The helper stopped before this channel scan finished.".to_owned()}else{error.message})},"message":"Channel discovery stopped. Already queued videos remain in your library."}));
            }
            done.cancel();
        });
        self.inner.tasks.lock().push(handle);
        Ok(())
    }
    async fn scan_channel(
        &self,
        id: &str,
        settings: &Settings,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let preview = self.preview(id)?;
        let auto = preview["auto_queue"] == true;
        let year = text(&preview, "scope") == "year";
        let temporary = tempfile::Builder::new()
            .prefix("channel-import-")
            .tempdir_in(&self.inner.work)?;
        let directory = temporary.path().to_owned();
        let cursor = Arc::new(Mutex::new(Cursor::default()));
        let tabs = if preview["include_shorts"] == true {
            vec!["videos", "shorts", "streams"]
        } else {
            vec!["videos", "streams"]
        };
        let mut warnings = vec![json!(
            "Only public uploads listed by YouTube are included. Already queued videos remain in the library if discovery is stopped."
        )];
        if auto && year {
            warnings.push(json!("Past year follows YouTube's newest-first tab order and stops each tab after two consecutive uploads with verified older dates; unknown dates are excluded."));
        }
        for tab in tabs {
            if cancel.is_cancelled() {
                return Err(cancelled());
            }
            cursor.lock().old_streak = 0;
            self.update_preview(id,json!({"current_tab":tab,"warnings":warnings,"message":if auto{format!("Discovering {tab}; eligible videos are queued immediately…")}else{format!("Checking publication dates in {tab}…")}}))?;
            let engine = self.clone();
            let preview_id = id.to_owned();
            let config = settings.clone();
            let token = cancel.clone();
            let folder = directory.clone();
            let tab_name = tab.to_owned();
            let position = cursor.clone();
            let result = process::stream_tab(
                settings,
                &format!("{}/{tab}", text(&preview, "channel_url")),
                cancel,
                move |entry| {
                    let engine = engine.clone();
                    let id = preview_id.clone();
                    let settings = config.clone();
                    let cancel = token.clone();
                    let directory = folder.clone();
                    let tab = tab_name.clone();
                    let cursor = position.clone();
                    Box::pin(async move {
                        engine
                            .scan_entry(&id, &settings, &cancel, &directory, &tab, entry, cursor)
                            .await
                    })
                },
            )
            .await;
            if let Err(error) = result {
                if error.status == 424 {
                    continue;
                }
                return Err(error);
            }
        }
        let preview = self.preview(id)?;
        if preview["skipped_unknown"].as_u64().unwrap_or(0) > 0 {
            warnings.push(json!(format!(
                "{} videos were excluded because their exact publication date was unavailable.",
                preview["skipped_unknown"]
            )));
        }
        let result = if preview["queue_result"].is_object() {
            preview["queue_result"].clone()
        } else {
            json!({"folder_id":preview["folder_id"],"queued_count":0,"existing_count":0,"job_ids":[]})
        };
        let mut patch = json!({"status":"ready","discovery_complete":true,"current_tab":Value::Null,"warnings":warnings,"message":if auto{"Discovery finished. Videos are in the transcription queue."}else{"Channel preview ready. No videos have been queued."}});
        if auto {
            patch["queue_state"] = json!("queued");
            patch["queue_result"] = result;
        }
        self.update_preview(id, patch)?;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)] // Explicit independent scan resources; never shared across channel tasks.
    async fn scan_entry(
        &self,
        id: &str,
        settings: &Settings,
        cancel: &CancellationToken,
        directory: &Path,
        tab: &str,
        entry: Value,
        cursor: Arc<Mutex<Cursor>>,
    ) -> Result<bool> {
        if cancel.is_cancelled() || self.inner.stopping.load(Ordering::SeqCst) {
            return Err(cancelled());
        }
        let video = text(&entry, "id").to_owned();
        if !valid_video(&video) {
            return Err(Error::bad_request(
                "YouTube returned an invalid channel entry; discovery stopped.",
            ));
        }
        if !cursor.lock().seen.insert(video.clone()) {
            return Ok(true);
        }
        let mut preview = self.preview(id)?;
        let channel = entry["playlist_channel_id"]
            .as_str()
            .or(entry["channel_id"].as_str())
            .or(entry["playlist_id"].as_str())
            .unwrap_or("");
        if !channel_id(channel) {
            return Err(Error::bad_request(
                "YouTube did not identify this channel reliably; discovery stopped.",
            ));
        }
        let requested =
            url::Url::parse(text(&preview, "channel_url")).map_err(|_| channel_url_error())?;
        if (!preview["channel_id"].is_null() && preview["channel_id"] != channel)
            || (requested.path().starts_with("/channel/")
                && requested.path().trim_start_matches("/channel/") != channel)
            || entry["channel_id"]
                .as_str()
                .is_some_and(|value| value != channel)
        {
            return Err(Error::bad_request(
                "YouTube returned a different channel; discovery stopped.",
            ));
        }
        let channel_name = entry["playlist_channel"]
            .as_str()
            .or(entry["channel"].as_str())
            .or(entry["uploader"].as_str())
            .or(entry["playlist_title"].as_str())
            .unwrap_or(channel);
        let found = preview["found_count"].as_u64().unwrap_or(0) + 1;
        merge(
            &mut preview,
            json!({"channel_id":channel,"channel_name":channel_name,"found_count":found}),
        );
        self.update_preview(id,json!({"channel_id":channel,"channel_name":channel_name,"found_count":preview["found_count"]}))?;
        let auto = preview["auto_queue"] == true;
        let year = text(&preview, "scope") == "year";
        let mut info = entry.clone();
        merge(
            &mut info,
            json!({"channel_id":channel,"release_date":Value::Null,"upload_date":Value::Null}),
        );
        let unavailable = entry["is_live"] == true
            || ["is_live", "is_upcoming", "post_live"].contains(&text(&entry, "live_status"))
            || entry
                .get("availability")
                .is_some_and(|v| !v.is_null() && v != "public")
            || ["[Private video]", "[Deleted video]"].contains(&text(&entry, "title"));
        let mut reason = if unavailable {
            "unavailable"
        } else {
            "eligible"
        };
        if reason == "eligible" && (!auto || year) {
            match process::metadata(
                settings,
                &format!("https://www.youtube.com/watch?v={video}"),
                directory,
                cancel,
            )
            .await
            {
                Ok(full) => {
                    if text(&full, "id") != video || text(&full, "channel_id") != channel {
                        return Err(Error::bad_request(
                            "A listed video's channel identity could not be verified; discovery stopped.",
                        ));
                    }
                    info = full;
                    let as_of = NaiveDate::parse_from_str(text(&preview, "as_of_date"), "%Y-%m-%d")
                        .map_err(|_| Error::bad_request("Invalid channel selection date."))?;
                    let cutoff =
                        NaiveDate::parse_from_str(text(&preview, "cutoff_date"), "%Y-%m-%d").ok();
                    reason = classify_video(&info, text(&preview, "scope"), cutoff, as_of)?;
                }
                Err(error) if error.status == 410 => reason = "unavailable",
                Err(error) => return Err(error),
            }
        }
        if reason == "eligible" && auto {
            let engine = self.clone();
            let owned_id = id.to_owned();
            let owned_info = info.clone();
            tokio::task::spawn_blocking(move || engine.add_import_video(&owned_id, &owned_info))
                .await
                .map_err(|_| Error::internal("The channel video could not be queued."))??;
        }
        preview = self.preview(id)?;
        let mut patch = json!({"processed_count":preview["processed_count"].as_u64().unwrap_or(0)+1,"message":format!("Checking {tab}: {} new videos queued, {} already in your library…",preview["queued_count"],preview["existing_count"])});
        if reason == "eligible" {
            let mut videos = preview["videos"].as_array().cloned().unwrap_or_default();
            videos.push(compact(&info));
            patch["videos"] = json!(videos);
            patch["eligible_count"] = json!(preview["eligible_count"].as_u64().unwrap_or(0) + 1);
            if let Some(duration) = info["duration"]
                .as_f64()
                .filter(|v| v.is_finite() && *v >= 0.)
            {
                patch["estimated_duration_seconds"] =
                    json!(preview["estimated_duration_seconds"].as_f64().unwrap_or(0.) + duration);
            } else {
                patch["unknown_duration_count"] =
                    json!(preview["unknown_duration_count"].as_u64().unwrap_or(0) + 1);
            }
        } else {
            let key = format!("skipped_{reason}");
            patch[&key] = json!(preview[&key].as_u64().unwrap_or(0) + 1);
        }
        self.update_preview(id, patch)?;
        let mut position = cursor.lock();
        position.old_streak = if reason == "old" {
            position.old_streak + 1
        } else {
            0
        };
        Ok(!(auto && year && position.old_streak >= 2))
    }
    fn channel_folder(&self, id: &str) -> Result<String> {
        let preview = self.preview(id)?;
        let channel = text(&preview, "channel_id");
        if !channel_id(channel) {
            return Err(Error::bad_request(
                "The channel identity could not be verified.",
            ));
        }
        if let Some(folder) = self.inner.state.read().folder_map.get(channel).cloned()
            && self.inner.library.validate_folder(Some(&folder)).is_ok()
        {
            return Ok(folder);
        }
        let mut name: String = text(&preview, "channel_name")
            .nfc()
            .map(|c| {
                if c.is_control() || "/\\:*?\"<>|".contains(c) {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        name = name
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .trim_matches('.')
            .to_owned();
        if name.is_empty() {
            name = "YouTube channel".into();
        }
        while name.chars().count() > 70 || name.len() > 130 {
            name.pop();
        }
        let snapshot = self.inner.library.snapshot();
        if snapshot["folders"].as_array().is_some_and(|folders| {
            folders
                .iter()
                .any(|f| text(f, "name").to_lowercase() == name.to_lowercase())
        }) {
            while name.chars().count() > 40 || name.len() > 100 {
                name.pop();
            }
            name = format!("{name} — {channel}");
        }
        let folder = self.inner.library.create_folder(&name)?;
        let folder_id = text(&folder, "id").to_owned();
        let mut state = self.inner.state.write();
        let mut mapping = state.folder_map.clone();
        mapping.insert(channel.into(), folder_id.clone());
        atomic_json(
            &self.inner.data.join("channel-folders.json"),
            &json!(mapping),
        )?;
        state.folder_map = mapping;
        Ok(folder_id)
    }
    fn add_import_video(&self, id: &str, info: &Value) -> Result<()> {
        let _submission = self.inner.submission_lock.lock();
        if self.inner.stopping.load(Ordering::SeqCst) {
            return Err(cancelled());
        }
        let preview = self.preview(id)?;
        if preview["user_cancelled"] == true {
            return Err(cancelled());
        }
        self.reconcile_removals(false)?;
        let folder = if let Some(folder) = preview["folder_id"].as_str() {
            folder.to_owned()
        } else {
            let folder = self.channel_folder(id)?;
            self.update_preview(id, json!({"folder_id":folder}))?;
            folder
        };
        let video = text(info, "id");
        let prior = preview["import_job_ids"][video]
            .as_str()
            .and_then(|id| self.get_job(id).ok())
            .filter(|j| text(j, "video_id") == video);
        let existing = if self.inner.removals.contains(video) {
            self.existing_job(video)
        } else {
            prior.or_else(|| self.existing_job(video))
        };
        let reused = existing.is_some();
        let job = match existing {
            Some(job) => job,
            None => self.submit_locked(
                &format!("https://www.youtube.com/watch?v={video}"),
                Some(Some(folder.clone())),
                Some(id),
            )?,
        };
        let job_id = text(&job, "id");
        let mut origins = strings(&job["channel_import_ids"]);
        if !origins.iter().any(|origin| origin == id) {
            origins.push(id.into());
        }
        let mut patch = json!({"channel_import_ids":origins});
        if !reused {
            patch["title"] = info["title"]
                .as_str()
                .map(|v| json!(v))
                .unwrap_or(json!(video));
            patch["channel_import_id"] = json!(id);
            patch["metadata"] = json!({"channel":preview["channel_name"],"published_date":video_date(info).map(|d|d.to_string()).unwrap_or_default(),"duration_seconds":info["duration"],"channel_import_id":id});
        }
        // Origin is persisted even for reused/failed jobs before preview map,
        // so a crash cannot silently create a second paid attempt on resume.
        if self.get_job(job_id).is_ok() {
            self.update_job(job_id, patch)?;
        }
        if reused && text(&job, "status") != "removed" {
            self.move_jobs(&[job_id.into()], Some(&folder))?;
        }
        let preview = self.preview(id)?;
        let queued = preview["queued_count"].as_u64().unwrap_or(0) + u64::from(!reused);
        let existed = preview["existing_count"].as_u64().unwrap_or(0) + u64::from(reused);
        let mut imports = preview["import_job_ids"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        imports.insert(video.into(), json!(job_id));
        let mut ids = strings(&preview["queue_result"]["job_ids"]);
        if !ids.iter().any(|v| v == job_id) {
            ids.push(job_id.into());
        }
        self.update_preview(id,json!({"folder_id":folder,"queued_count":queued,"existing_count":existed,"import_job_ids":imports,"queue_processed_count":ids.len(),"queue_result":{"folder_id":folder,"queued_count":queued,"existing_count":existed,"job_ids":ids}}))?;
        Ok(())
    }
    pub(crate) fn queue_channel(&self, id: &str) -> Result<Value> {
        let _submission = self.inner.submission_lock.lock();
        if self.inner.stopping.load(Ordering::SeqCst) || self.inner.closing.load(Ordering::SeqCst) {
            return Err(Error::conflict("The queue is stopping."));
        }
        let preview = self.preview(id)?;
        if text(&preview, "queue_state") == "queued" && preview["queue_result"].is_object() {
            return Ok(preview["queue_result"].clone());
        }
        if self
            .inner
            .producers
            .lock()
            .get(id)
            .is_some_and(|task| !task.done.is_cancelled())
        {
            return Ok(json!({"pending":true}));
        }
        if text(&preview, "status") != "ready" || preview["selection_version"] != 2 {
            return Err(Error::conflict(
                "Only a finished, verified channel preview can be queued.",
            ));
        }
        self.ensure_status_writable()?;
        let as_of =
            NaiveDate::parse_from_str(text(&preview, "as_of_date"), "%Y-%m-%d").map_err(|_| {
                Error::bad_request("Scan this channel again to verify the date selection.")
            })?;
        let cutoff = NaiveDate::parse_from_str(text(&preview, "cutoff_date"), "%Y-%m-%d").ok();
        if text(&preview, "scope") == "year" && cutoff != Some(calendar_year_cutoff(as_of)) {
            return Err(Error::bad_request(
                "Scan this channel again to verify the date selection.",
            ));
        }
        let videos = preview["videos"]
            .as_array()
            .ok_or_else(|| Error::bad_request("Scan this channel again to verify its videos."))?;
        for info in videos {
            if !valid_video(text(info, "id"))
                || info["channel_id"] != preview["channel_id"]
                || classify_video(info, text(&preview, "scope"), cutoff, as_of)? != "eligible"
            {
                return Err(Error::bad_request(
                    "This preview's selection could not be verified. Scan the channel again.",
                ));
            }
        }
        let batch = self.accept_batch(false)?;
        self.update_preview(id,json!({"batch_id":batch,"queue_state":"queueing","queue_error":Value::Null,"queue_processed_count":0,"message":"Waiting to add channel videos to the transcription queue…"}))?;
        let cancel = CancellationToken::new();
        let done = CancellationToken::new();
        self.inner.producers.lock().insert(
            id.into(),
            ManagedTask {
                cancel: cancel.clone(),
                done: done.clone(),
            },
        );
        let engine = self.clone();
        let id = id.to_owned();
        let handle = tokio::spawn(async move {
            let result=async{let permit=tokio::select!{biased;_=cancel.cancelled()=>return Err(cancelled()),permit=engine.inner.queue_slots.clone().acquire_owned()=>permit.map_err(|_|cancelled())?};engine.inner.state.write().active_queue_id=Some(id.clone());let preview=engine.preview(&id)?;let videos=preview["videos"].as_array().cloned().unwrap_or_default();for info in videos{if cancel.is_cancelled(){return Err(cancelled());}let own=engine.clone();let own_id=id.clone();tokio::task::spawn_blocking(move||own.add_import_video(&own_id,&info)).await.map_err(|_|Error::internal("The channel video could not be queued."))??;tokio::task::yield_now().await;}engine.update_preview(&id,json!({"queue_state":"queued","queue_error":Value::Null,"message":"Channel videos were added to the transcription library."}))?;drop(permit);Ok::<(),Error>(())}.await;
            if let Err(error) = result {
                let _=engine.update_preview(&id,json!({"queue_state":if error.status==499&&!engine.inner.closing.load(Ordering::SeqCst){"cancelled"}else{"failed"},"queue_error":if error.status==499&&!engine.inner.closing.load(Ordering::SeqCst){Value::Null}else{json!(error.message)}}));
            }
            {
                let mut state = engine.inner.state.write();
                if state.active_queue_id.as_deref() == Some(&id) {
                    state.active_queue_id = None;
                }
            }
            done.cancel();
        });
        self.inner.tasks.lock().push(handle);
        Ok(json!({"pending":true}))
    }
    pub(crate) fn public_channel(&self, id: &str) -> Result<Value> {
        self.reconcile_removals(false)?;
        let preview = self.preview(id)?;
        let mut out = json!({});
        for key in [
            "id",
            "status",
            "channel_name",
            "channel_url",
            "channel_id",
            "scope",
            "include_shorts",
            "cutoff_date",
            "as_of_date",
            "found_count",
            "processed_count",
            "eligible_count",
            "existing_count",
            "skipped_old",
            "skipped_unknown",
            "skipped_unavailable",
            "estimated_duration_seconds",
            "unknown_duration_count",
            "error",
            "warnings",
            "message",
            "created_at",
            "updated_at",
            "folder_id",
            "queue_state",
            "queue_result",
            "queue_error",
            "queue_processed_count",
            "auto_queue",
            "queued_count",
            "discovery_complete",
            "current_tab",
        ] {
            out[key] = preview[key].clone();
        }
        out["progress"] = self.channel_progress(&preview);
        let state = self.inner.state.read();
        let waiting: Vec<_> = state
            .previews
            .values()
            .filter(|p| text(p, "status") == "waiting")
            .collect();
        out["waiting_position"] = json!(
            waiting
                .iter()
                .position(|p| text(p, "id") == id)
                .map(|n| n + 1)
        );
        let waiting: Vec<_> = state
            .previews
            .values()
            .filter(|p| {
                text(p, "queue_state") == "queueing"
                    && state.active_queue_id.as_deref() != Some(text(p, "id"))
            })
            .collect();
        out["queue_position"] = json!(
            waiting
                .iter()
                .position(|p| text(p, "id") == id)
                .map(|n| n + 1)
        );
        Ok(out)
    }
    fn channel_progress(&self, preview: &Value) -> Value {
        let mut ids = strings(&preview["queue_result"]["job_ids"]);
        ids.sort();
        ids.dedup();
        let jobs: Vec<_> = ids.iter().map(|id| self.get_job(id).ok()).collect();
        let videos: HashSet<_> = jobs
            .iter()
            .flatten()
            .map(|job| text(job, "video_id"))
            .collect();
        let related: Vec<_> = self
            .inner
            .state
            .read()
            .jobs
            .values()
            .filter(|job| {
                text(job, "status") == "completed" && videos.contains(text(job, "video_id"))
            })
            .cloned()
            .collect();
        let mut verification = ProgressVerification::new(self, &related);
        self.job_progress(
            &jobs.iter().map(Option::as_ref).collect::<Vec<_>>(),
            text(preview, "status") == "ready" && preview["discovery_complete"] == true,
            &mut verification,
        )
    }
    fn job_progress(
        &self,
        jobs: &[Option<&Value>],
        discovery_complete: bool,
        verification: &mut ProgressVerification<'_>,
    ) -> Value {
        let mut out = json!({"total":jobs.len(),"queued":0,"processing":0,"completed":0,"saved":0,"failed":0,"cancelled":0,"save_failed":0,"missing":0,"unsaved":0,"saving":0,"removed":0});
        let state = self.inner.state.read();
        let active_id = state.active_id.clone();
        let pending_exports = state.pending_exports.clone();
        drop(state);
        let persistence_pending = jobs
            .iter()
            .flatten()
            .filter(|job| job["persistence_pending"] == true)
            .count();
        for job in jobs {
            let Some(job) = job else {
                inc(&mut out, "missing");
                continue;
            };
            match text(job, "status") {
                "queued" => inc(&mut out, "queued"),
                "downloading" | "preparing" | "transcribing" => inc(&mut out, "processing"),
                "completed" => {
                    inc(&mut out, "completed");
                    let saved = verification.saved(job);
                    let saving = !saved
                        && job["export_pending"] == true
                        && (active_id.as_deref() == Some(text(job, "id"))
                            || pending_exports.iter().any(|id| id == text(job, "id")));
                    if saved {
                        inc(&mut out, "saved");
                    }
                    if saving {
                        inc(&mut out, "saving");
                    }
                    if !saved
                        && !saving
                        && job["export_error"].as_str().is_some_and(|e| !e.is_empty())
                    {
                        inc(&mut out, "save_failed");
                    }
                }
                status @ ("failed" | "cancelled" | "removed") => inc(&mut out, status),
                _ => inc(&mut out, "missing"),
            }
        }
        out["unsaved"] = json!(
            count(&out, "completed")
                .saturating_sub(count(&out, "saved") + count(&out, "save_failed"))
        );
        out["discovery_complete"] = json!(discovery_complete);
        out["finished"] = json!(
            discovery_complete
                && count(&out, "queued") + count(&out, "processing") + count(&out, "saving") == 0
                && persistence_pending == 0
        );
        if persistence_pending > 0 {
            out["persistence_pending"] = json!(persistence_pending);
        }
        out
    }
    fn queue_snapshot(
        &self,
        jobs: &[Value],
        previews: &[Value],
        limit: Option<usize>,
        include_batches: bool,
        verification: &mut ProgressVerification<'_>,
    ) -> Value {
        type RankedJob<'a> = ((u8, String, u64, String), &'a Value);
        let mut selected: BTreeMap<String, RankedJob<'_>> = BTreeMap::new();
        let mut actual_active = false;
        for job in jobs {
            let status = text(job, "status");
            actual_active |= active(status);
            let priority = if status == "removed" {
                5
            } else if status == "completed" && verification.verify(job).document_present {
                if verification.verify(job).saved { 4 } else { 3 }
            } else if active(status) {
                2
            } else if matches!(status, "failed" | "cancelled") {
                1
            } else {
                0
            };
            let id = text(job, "id");
            let video = text(job, "video_id");
            let key = if video.is_empty() {
                format!("job:{id}")
            } else {
                video.into()
            };
            let rank = (
                priority,
                text(job, "created_at").to_owned(),
                job["queue_order"].as_u64().unwrap_or(0),
                id.to_owned(),
            );
            if selected.get(&key).is_none_or(|(old, _)| rank > *old) {
                selected.insert(key, (rank, job));
            }
        }
        let state = self.inner.state.read();
        let active_queue = state.active_queue_id.clone();
        let active_id = state.active_id.clone();
        let pending_exports = state.pending_exports.clone();
        drop(state);
        let discovering = previews
            .iter()
            .filter(|p| {
                (p["auto_queue"] == true && text(p, "status") == "scanning")
                    || (include_batches
                        && text(p, "queue_state") == "queueing"
                        && active_queue.as_deref() == Some(text(p, "id")))
            })
            .count();
        let waiting = previews
            .iter()
            .filter(|p| {
                (p["auto_queue"] == true && text(p, "status") == "waiting")
                    || (include_batches
                        && text(p, "queue_state") == "queueing"
                        && active_queue.as_deref() != Some(text(p, "id")))
            })
            .count();
        let mut out = self.job_progress(
            &selected
                .values()
                .map(|(rank, job)| if rank.0 == 0 { None } else { Some(*job) })
                .collect::<Vec<_>>(),
            discovering + waiting == 0,
            verification,
        );
        let saving = jobs.iter().any(|j| {
            (active_id.as_deref() == Some(text(j, "id"))
                || pending_exports.iter().any(|id| id == text(j, "id")))
                && text(j, "status") == "completed"
                && j["export_pending"] == true
        });
        out["finished"] = json!(out["finished"] == true && !actual_active && !saving);
        let mut entries: Vec<_> = selected.values().collect();
        entries.sort_by(|a, b| text(b.1, "updated_at").cmp(text(a.1, "updated_at")));
        let mut attention = vec![];
        for (rank, job) in entries {
            let kind = if rank.0 == 0 {
                Some((
                    "missing",
                    "The saved transcript record could not be verified.".to_owned(),
                ))
            } else if text(job, "status") == "failed" {
                Some((
                    "failed",
                    job["error"]
                        .as_str()
                        .unwrap_or(
                            "Transcription failed. Open this item in the library for details.",
                        )
                        .to_owned(),
                ))
            } else if text(job, "status") == "completed" && rank.0 == 3 {
                if job["export_pending"] == true
                    && (active_id.as_deref() == Some(text(job, "id"))
                        || pending_exports.iter().any(|id| id == text(job, "id")))
                {
                    None
                } else if let Some(error) = job["export_error"].as_str().filter(|v| !v.is_empty()) {
                    Some(("save_failed", error.into()))
                } else {
                    Some(("unsaved","The transcript is complete, but no unchanged saved file could be verified. Open it in the library to review its save status.".into()))
                }
            } else {
                None
            };
            if let Some((kind, message)) = kind {
                attention.push(json!({"id":job["id"],"video_id":job["video_id"],"title":text(job,"title").chars().take(300).collect::<String>(),"kind":kind,"message":message.chars().take(800).collect::<String>()}));
            }
        }
        let pending: Vec<_> = jobs
            .iter()
            .filter(|job| job["persistence_pending"] == true)
            .collect();
        if !pending.is_empty() {
            out["persistence_pending"] = json!(pending.len());
            out["finished"] = json!(false);
            for job in pending {
                attention.retain(|item| item["id"] != job["id"]);
                attention.insert(0, json!({"id":job["id"],"video_id":job["video_id"],"title":text(job,"title").chars().take(300).collect::<String>(),"kind":"persistence","message":super::jobs::PERSISTENCE_WARNING}));
            }
        }
        out["attention_count"] = json!(attention.len());
        if let Some(limit) = limit {
            attention.truncate(limit);
        }
        merge(
            &mut out,
            json!({"scope":"all","discovering_channels":discovering,"waiting_channels":waiting,"attention":attention}),
        );
        out
    }
    pub fn queue_status(&self, limit: Option<usize>) -> Result<Value> {
        self.reconcile_removals(false)?;
        let jobs = self.jobs();
        let state = self.inner.state.read();
        let previews: Vec<_> = state.previews.values().cloned().collect();
        let batch = state.batch.clone();
        drop(state);
        let mut verification = ProgressVerification::new(self, &jobs);
        let mut out = self.queue_snapshot(&jobs, &previews, limit, false, &mut verification);
        let id = batch.as_ref().map(|v| text(v, "id")).unwrap_or("");
        let current_jobs: Vec<_> = jobs
            .iter()
            .filter(|j| !id.is_empty() && text(j, "batch_id") == id)
            .cloned()
            .collect();
        let current_previews: Vec<_> = previews
            .iter()
            .filter(|p| !id.is_empty() && text(p, "batch_id") == id)
            .cloned()
            .collect();
        let mut progress = self.queue_snapshot(
            &current_jobs,
            &current_previews,
            Some(10),
            true,
            &mut verification,
        );
        merge(
            &mut progress,
            json!({"scope":"batch","id":batch.as_ref().map(|b|b["id"].clone()),"created_at":batch.as_ref().map(|b|b["created_at"].clone())}),
        );
        out["batch_progress"] = progress;
        Ok(out)
    }
    pub async fn cancel_video(&self, video: &str) -> Result<Value> {
        if !valid_video(video) {
            return Err(Error::bad_request("Use a valid YouTube video ID."));
        }
        let wait_id = {
            let _submission = self.inner.submission_lock.lock();
            let targets: Vec<_> = self
                .jobs()
                .into_iter()
                .filter(|j| text(j, "video_id") == video && active(text(j, "status")))
                .collect();
            let fallback = targets
                .first()
                .cloned()
                .or_else(|| self.existing_job(video));
            if let Some(job) = fallback {
                let previews: Vec<_> = self.inner.state.read().previews.values().cloned().collect();
                for preview in previews {
                    if (preview["auto_queue"] == true
                        && matches!(text(&preview, "status"), "waiting" | "scanning"))
                        || text(&preview, "queue_state") == "queueing"
                    {
                        let mut imports = preview["import_job_ids"]
                            .as_object()
                            .cloned()
                            .unwrap_or_default();
                        imports.insert(video.into(), job["id"].clone());
                        self.update_preview(
                            text(&preview, "id"),
                            json!({"import_job_ids":imports}),
                        )?;
                    }
                }
            }
            let ids = targets.iter().map(|j| text(j, "id").to_owned()).collect();
            self.begin_cancel_ids(&ids)?.1
        };
        self.wait_cancelled(wait_id).await;
        let engine = self.clone();
        let video = video.to_owned();
        tokio::task::spawn_blocking(move || {
            let statuses = engine.video_statuses(std::slice::from_ref(&video))?;
            Ok(statuses["statuses"][&video]
                .as_object()
                .map(|_| statuses["statuses"][&video].clone())
                .unwrap_or(json!({"status":"unknown","saved":false})))
        })
        .await
        .map_err(|_| Error::internal("Cancellation finished but status could not be read."))?
    }
    pub(crate) async fn cancel_channel(&self, id: &str) -> Result<Value> {
        let preview = self.preview(id)?;
        if text(&preview, "queue_state") == "queueing" {
            return Err(Error::conflict(
                "This batch is already being queued. Manage its videos in the library or use Stop all.",
            ));
        }
        if matches!(text(&preview, "status"), "waiting" | "scanning" | "ready")
            && text(&preview, "queue_state") != "queued"
        {
            self.update_preview(id,json!({"user_cancelled":true,"status":"cancelled","error":Value::Null,"message":"Discovery cancelled. Already queued videos remain in your library."}))?;
            let done = {
                let producers = self.inner.producers.lock();
                producers.get(id).map(|task| {
                    task.cancel.cancel();
                    task.done.clone()
                })
            };
            if let Some(done) = done {
                done.cancelled().await;
            }
        }
        self.public_channel(id)
    }
    pub async fn cancel_all(&self) -> Result<Value> {
        let _guard = self.inner.cancel_lock.lock().await;
        self.inner.stopping.store(true, Ordering::SeqCst);
        let result=async {
            let (scans,batches,done,cancelled,wait_id) = {
                let _submission=self.inner.submission_lock.lock();
                let previews:Vec<_>=self.inner.state.read().previews.values().cloned().collect();let mut scans=0;let mut batches=0;let mut done=vec![];
                for preview in previews {
                    let scan=matches!(text(&preview,"status"),"waiting"|"scanning");let batch=text(&preview,"queue_state")=="queueing";
                    if scan||batch { scans+=usize::from(scan);batches+=usize::from(batch);let id=text(&preview,"id");
                        self.update_preview(id,json!({"user_cancelled":true,"status":"cancelled","queue_state":if batch{json!("cancelled")}else{preview["queue_state"].clone()},"error":Value::Null,"queue_error":Value::Null,"message":"Stopped by you. Already saved transcripts are retained."}))?;
                        if let Some(task)=self.inner.producers.lock().get(id){task.cancel.cancel();done.push(task.done.clone());}
                    }
                }
                let ids=self.jobs().iter().filter(|j|active(text(j,"status"))).map(|j|text(j,"id").to_owned()).collect();
                let (cancelled,wait_id)=self.begin_cancel_ids(&ids)?;
                (scans,batches,done,cancelled,wait_id)
            };
            self.wait_cancelled(wait_id).await;for done in done {done.cancelled().await;}
            let engine=self.clone();let status=tokio::task::spawn_blocking(move||engine.queue_status(Some(10))).await.map_err(|_|Error::internal("The stopped queue status could not be read."))??;
            Ok(json!({"cancelled_jobs":cancelled,"cancelled_channels":scans,"cancelled_batches":batches,"status":status}))
        }.await;
        self.inner.stopping.store(false, Ordering::SeqCst);
        result
    }
}
fn count(v: &Value, key: &str) -> u64 {
    v[key].as_u64().unwrap_or(0)
}
fn inc(v: &mut Value, key: &str) {
    v[key] = json!(count(v, key) + 1);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[test]
    fn strict_channel_urls() {
        assert_eq!(
            canonical_channel_url("https://youtube.com/@creator/videos?view=0").unwrap(),
            "https://www.youtube.com/@creator"
        );
        for url in [
            "https://youtube.com/watch?v=abcdefghijk",
            "https://youtube.com/playlist?list=PL",
            "https://youtube.com.evil/@foo",
            "file:///channel/foo",
            "https://youtube.com/@foo%2f..%2fsecret",
        ] {
            assert!(canonical_channel_url(url).is_err());
        }
    }
    #[test]
    fn year_is_calendar_inclusive_and_rejects_unknown_future_live() {
        let as_of = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        let cutoff = calendar_year_cutoff(as_of);
        assert_eq!(cutoff.to_string(), "2023-02-28");
        assert_eq!(
            classify_video(
                &json!({"upload_date":"20230228"}),
                "year",
                Some(cutoff),
                as_of
            )
            .unwrap(),
            "eligible"
        );
        assert_eq!(
            classify_video(
                &json!({"upload_date":"20230227"}),
                "year",
                Some(cutoff),
                as_of
            )
            .unwrap(),
            "old"
        );
        assert_eq!(
            classify_video(&json!({}), "year", Some(cutoff), as_of).unwrap(),
            "unknown"
        );
        assert_eq!(
            classify_video(
                &json!({"release_date":"20240301","upload_date":"20230228"}),
                "all",
                None,
                as_of
            )
            .unwrap(),
            "unavailable"
        );
    }
}
