use crate::pipeline::{Pipeline, PipelineRunner, Update};
use crate::process::cancelled;
use parking_lot::{Mutex, RwLock};
use scribe_common::{
    Error, Result, SharedSettings, atomic_json, canonical_video_url, export_filename, now,
    read_json, read_json_optional, render_document,
};
use scribe_library::{Library, REMOVED_MESSAGE, RemovalRegistry};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::{Notify, Semaphore},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

pub(crate) fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(crate) fn active(status: &str) -> bool {
    matches!(
        status,
        "queued" | "downloading" | "preparing" | "transcribing"
    )
}
pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
}
pub(crate) fn valid_video(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|v| v.is_ascii_alphanumeric() || v == b'_' || v == b'-')
}
pub(crate) fn merge(value: &mut Value, patch: Value) {
    if let (Some(value), Some(patch)) = (value.as_object_mut(), patch.as_object()) {
        for (k, v) in patch {
            value.insert(k.clone(), v.clone());
        }
    }
}
pub(crate) fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<Inner>,
}
pub(crate) struct ManagedTask {
    pub cancel: CancellationToken,
    pub done: CancellationToken,
}
struct PendingStatus {
    attempts: u32,
    next_attempt: Instant,
    revision: String,
    generation: u64,
}
pub(crate) const PERSISTENCE_WARNING: &str = "The transcript result is retained, but its local job status could not be saved. Scribe is retrying that status write; new transcription requests and helper updates are paused until storage is writable.";
pub(crate) struct State {
    pub jobs: BTreeMap<String, Value>,
    pub queue: VecDeque<String>,
    pub order: u64,
    pub batch: Option<Value>,
    pub previews: BTreeMap<String, Value>,
    pub folder_map: BTreeMap<String, String>,
    pub active_id: Option<String>,
    pub active_cancel: Option<CancellationToken>,
    pub active_queue_id: Option<String>,
    pub removal_uncertain: HashSet<String>,
    pub last_removal: Option<Instant>,
    pending_status: HashMap<String, PendingStatus>,
    pub pending_exports: VecDeque<String>,
}
pub(crate) struct Inner {
    pub settings: SharedSettings,
    pub library: Arc<Library>,
    pub removals: RemovalRegistry,
    pub data: PathBuf,
    pub jobs_root: PathBuf,
    pub work: PathBuf,
    pub state: RwLock<State>,
    pub pipeline: Arc<dyn PipelineRunner>,
    pub wake: Notify,
    pub progress: Notify,
    repair_wake: Notify,
    pub tasks: Mutex<Vec<JoinHandle<()>>>,
    pub producers: Mutex<HashMap<String, ManagedTask>>,
    pub scan_slots: Arc<Semaphore>,
    pub queue_slots: Arc<Semaphore>,
    pub stopping: AtomicBool,
    pub closing: AtomicBool,
    pub cancel_lock: tokio::sync::Mutex<()>,
    pub reconcile_lock: Mutex<()>,
    pub submission_lock: Mutex<()>,
    pub test_pipeline: bool,
}
impl Engine {
    pub async fn open(settings: SharedSettings, library: Arc<Library>) -> Result<Self> {
        Self::open_with_pipeline(settings, library, Arc::new(Pipeline), false).await
    }
    /// Injection point for offline integration tests; production always validates dependencies.
    pub async fn open_with_pipeline(
        settings: SharedSettings,
        library: Arc<Library>,
        pipeline: Arc<dyn PipelineRunner>,
        test_pipeline: bool,
    ) -> Result<Self> {
        let configured_data = settings.read().data_dir.clone();
        std::fs::create_dir_all(&configured_data)?;
        let data = configured_data.canonicalize()?;
        let jobs_root = data.join("jobs");
        let work = data.join("work");
        for directory in [&data, &jobs_root, &work] {
            match std::fs::symlink_metadata(directory) {
                Ok(info) if info.file_type().is_symlink() || !info.is_dir() => {
                    return Err(Error::internal(
                        "Private working directories must be regular directories, not symbolic links.",
                    ));
                }
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    return Err(error.into());
                }
                _ => {}
            }
            std::fs::create_dir_all(directory)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        // The server owns the exclusive helper lock before entering open().
        for entry in std::fs::read_dir(&work)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        let batch = read_json_optional(&data.join("current-batch.json"))
            .ok()
            .flatten()
            .filter(|v| valid_id(text(v, "id")));
        let folder_map = match read_json_optional(&data.join("channel-folders.json"))? {
            Some(value) => serde_json::from_value::<BTreeMap<String,String>>(value)
                .map_err(|_|Error::internal("Channel folder ownership could not be read. Library reconciliation was stopped to preserve existing folders."))?,
            None => BTreeMap::new(),
        };
        let removals = RemovalRegistry::open(&data)?;
        let engine = Self {
            inner: Arc::new(Inner {
                settings,
                library,
                removals,
                data: data.clone(),
                jobs_root,
                work,
                state: RwLock::new(State {
                    jobs: BTreeMap::new(),
                    queue: VecDeque::new(),
                    order: 0,
                    batch,
                    previews: BTreeMap::new(),
                    folder_map,
                    active_id: None,
                    active_cancel: None,
                    active_queue_id: None,
                    removal_uncertain: HashSet::new(),
                    last_removal: None,
                    pending_status: HashMap::new(),
                    pending_exports: VecDeque::new(),
                }),
                pipeline,
                wake: Notify::new(),
                progress: Notify::new(),
                repair_wake: Notify::new(),
                tasks: Mutex::new(vec![]),
                producers: Mutex::new(HashMap::new()),
                scan_slots: Arc::new(Semaphore::new(2)),
                queue_slots: Arc::new(Semaphore::new(1)),
                stopping: AtomicBool::new(false),
                closing: AtomicBool::new(false),
                cancel_lock: tokio::sync::Mutex::new(()),
                reconcile_lock: Mutex::new(()),
                submission_lock: Mutex::new(()),
                test_pipeline,
            }),
        };
        engine.recover_status_journal()?;
        engine.recover_jobs()?;
        engine.load_channels()?;
        engine.reconcile_library_folders()?;
        engine.reconcile_removals(true)?;
        for job in engine.jobs() {
            if text(&job, "status") == "completed"
                && job["export_pending"] == true
                && engine.inner.library.export_settings()["enabled"] == true
            {
                let _ = engine.export_one(text(&job, "id"), None);
            }
        }
        let worker = engine.clone();
        engine.inner.tasks.lock().push(tokio::spawn(async move {
            worker.worker().await;
        }));
        let repair = engine.clone();
        engine.inner.tasks.lock().push(tokio::spawn(async move {
            repair.repair_worker().await;
        }));
        engine.resume_channels()?;
        Ok(engine)
    }
    fn recover_jobs(&self) -> Result<()> {
        for entry in std::fs::read_dir(&self.inner.jobs_root)? {
            let Ok(entry) = entry else { continue };
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) {
                continue;
            }
            if self.inner.state.read().pending_status.contains_key(&id) {
                continue;
            }
            let Ok(mut job) = read_json(&entry.path().join("job.json")) else {
                continue;
            };
            if text(&job, "id") != id || !job.is_object() {
                continue;
            }
            let original = job.clone();
            let needs_partial_recovery = active(text(&job, "status"))
                && (text(&job, "status") != "queued" || job["cancel_requested"] == true);
            if needs_partial_recovery {
                if crate::pipeline::restore_partial_exports(&job, &entry.path()).is_err() {
                    job["recovery_warning"] = json!(
                        "Some partial previews could not be restored. Existing checkpoints were retained."
                    );
                }
                job["document_available"] =
                    json!(read_json(&entry.path().join("transcript.json")).is_ok());
            }
            for field in ["folder_id", "export_path", "export_error"] {
                if job.get(field).is_none() {
                    job[field] = Value::Null;
                }
            }
            let recovered_complete = text(&original, "status") == "transcribing"
                && job["cancel_requested"] != true
                && !self.inner.removals.contains(text(&job, "video_id"))
                && crate::pipeline::completed_checkpoint(&job, &entry.path()).unwrap_or(false);
            if recovered_complete {
                merge(
                    &mut job,
                    json!({"status":"completed","progress":100,"document_available":true,"error":Value::Null,
                    "completed_chunks":original["total_chunks"],"message":"Recovered the complete transcript. No new transcription was needed.",
                    "updated_at":now(),"export_pending":self.inner.library.export_settings()["enabled"]==true}),
                );
            }
            let pending = job["cancel_requested"] != true
                && (active(text(&job, "status"))
                    || (text(&job, "status") == "completed"
                        && job["export_pending"] == true
                        && self.inner.library.export_settings()["enabled"] == true));
            if pending {
                job["batch_id"] = json!(self.accept_batch(true)?);
            }
            if job["cancel_requested"] == true && active(text(&job, "status")) {
                merge(
                    &mut job,
                    json!({"status":"cancelled","error":Value::Null,"message":"Cancelled by you. Completed text is retained."}),
                );
            }
            if active(text(&job, "status")) && text(&job, "status") != "queued" {
                merge(
                    &mut job,
                    json!({"status":"failed","error":"The helper stopped before this video finished. Temporary audio was deleted; completed transcript chunks were retained.","message":"Interrupted. You can retry this video.","updated_at":now(),"document_available":entry.path().join("transcript.json").is_file()}),
                );
            }
            if (needs_partial_recovery || pending || job["cancel_requested"] == true)
                && job != original
            {
                atomic_json(&entry.path().join("job.json"), &job)?;
            }
            let mut state = self.inner.state.write();
            state.order = state.order.max(job["queue_order"].as_u64().unwrap_or(0));
            state.jobs.insert(id, job);
        }
        let mut queued: Vec<Value> = self
            .jobs()
            .into_iter()
            .filter(|j| text(j, "status") == "queued")
            .collect();
        queued.sort_by_key(|job| {
            (
                job["queue_order"].as_u64().is_some(),
                job["queue_order"].as_u64().unwrap_or(0),
                text(job, "created_at").to_owned(),
                text(job, "id").to_owned(),
            )
        });
        self.inner.state.write().queue = queued
            .iter()
            .map(|job| text(job, "id").to_owned())
            .collect();
        Ok(())
    }
    pub fn jobs(&self) -> Vec<Value> {
        let mut jobs: Vec<_> = self.inner.state.read().jobs.values().cloned().collect();
        jobs.sort_by(job_order);
        jobs
    }
    fn inventory_jobs(&self) -> Vec<Value> {
        // File matching consumes identity/status and tracked export evidence,
        // never the large provider timing or metadata payloads of each job.
        self.inner
            .state
            .read()
            .jobs
            .values()
            .filter(|job| matches!(text(job, "status"), "completed" | "removed"))
            .map(|job| {
                Value::Object(
                    [
                        "id",
                        "video_id",
                        "status",
                        "export_path",
                        "export_paths",
                        "export_evidence",
                    ]
                    .into_iter()
                    .filter_map(|key| job.get(key).map(|value| (key.to_owned(), value.clone())))
                    .collect(),
                )
            })
            .collect()
    }
    pub fn get_job(&self, id: &str) -> Result<Value> {
        self.inner
            .state
            .read()
            .jobs
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found("Transcript job not found."))
    }
    pub fn channel_folder_ids(&self) -> HashSet<String> {
        self.inner
            .state
            .read()
            .folder_map
            .values()
            .cloned()
            .collect()
    }
    pub fn has_active_work(&self) -> bool {
        let state = self.inner.state.read();
        state.active_id.is_some()
            || !state.pending_status.is_empty()
            || !state.pending_exports.is_empty()
            || state.jobs.values().any(|j| active(text(j, "status")))
            || Self::producers_active(&state)
    }
    pub(crate) fn producers_active(state: &State) -> bool {
        state.previews.values().any(|p| {
            (p["auto_queue"] == true && matches!(text(p, "status"), "waiting" | "scanning"))
                || text(p, "queue_state") == "queueing"
        })
    }
    pub(crate) fn accept_batch(&self, recovering: bool) -> Result<String> {
        let mut state = self.inner.state.write();
        let running = state.active_id.is_some()
            || !state.pending_status.is_empty()
            || !state.pending_exports.is_empty()
            || state.jobs.values().any(|j| active(text(j, "status")))
            || Self::producers_active(&state);
        if state.batch.is_none() || (!running && !recovering) {
            let batch = json!({"id":uuid::Uuid::new_v4().simple().to_string(),"created_at":now()});
            atomic_json(&self.inner.data.join("current-batch.json"), &batch)?;
            state.batch = Some(batch);
        }
        Ok(state
            .batch
            .as_ref()
            .map(|b| text(b, "id").to_owned())
            .unwrap_or_default())
    }
    pub(crate) fn update_job(&self, id: &str, patch: Value) -> Result<Value> {
        let mut state = self.inner.state.write();
        let mut job = state
            .jobs
            .get(id)
            .cloned()
            .ok_or_else(|| Error::not_found("Transcript job not found."))?;
        let old_status = text(&job, "status").to_owned();
        let old_generation = job["persistence_generation"].as_u64().unwrap_or(0);
        let cancel_requested = job["cancel_requested"] == true;
        merge(&mut job, patch);
        // Cancellation is monotonic for this attempt. A stale terminal patch
        // must never overwrite a newer user intent with null/false.
        if cancel_requested {
            job["cancel_requested"] = json!(true);
        }
        if self.inner.removals.contains(text(&job, "video_id")) {
            merge(
                &mut job,
                json!({"status":"removed","message":REMOVED_MESSAGE,"error":Value::Null,"export_error":Value::Null,"export_pending":false}),
            );
        } else if job["cancel_requested"] == true {
            merge(
                &mut job,
                json!({"status":"cancelled","error":Value::Null,"export_pending":false}),
            );
        }
        if text(&job, "status") == "completed"
            && old_status != "completed"
            && self.inner.library.export_settings()["enabled"] == true
        {
            job["export_pending"] = json!(true);
        }
        job["updated_at"] = json!(now());
        if let Some(object) = job.as_object_mut() {
            object.remove("persistence_pending");
            object.remove("persistence_warning");
        }
        if let Err(error) = atomic_json(&self.inner.jobs_root.join(id).join("job.json"), &job) {
            if matches!(
                text(&job, "status"),
                "completed" | "failed" | "cancelled" | "removed"
            ) {
                if text(&job, "status") == "completed"
                    && old_status != "completed"
                    && !crate::pipeline::completed_checkpoint(&job, &self.inner.jobs_root.join(id))
                        .unwrap_or(false)
                {
                    merge(
                        &mut job,
                        json!({"status":"failed","export_pending":false,"error":"The final transcript could not be verified after its local status write failed. Existing checkpoints were retained."}),
                    );
                }
                let pending = state
                    .pending_status
                    .entry(id.to_owned())
                    .or_insert(PendingStatus {
                        attempts: 0,
                        next_attempt: Instant::now() + Duration::from_millis(250),
                        revision: uuid::Uuid::new_v4().simple().to_string(),
                        generation: old_generation.saturating_add(1),
                    });
                job["persistence_revision"] = json!(pending.revision);
                job["persistence_generation"] = json!(pending.generation);
                job["persistence_pending"] = json!(true);
                job["persistence_warning"] = json!(PERSISTENCE_WARNING);
                state.jobs.insert(id.to_owned(), job.clone());
                let _ = self.persist_status_journal(&state);
                if job["cancel_requested"] == true
                    && state.active_id.as_deref() == Some(id)
                    && let Some(token) = &state.active_cancel
                {
                    token.cancel();
                }
                self.inner.repair_wake.notify_one();
                self.inner.progress.notify_waiters();
            }
            return Err(error);
        }
        let repaired = state.pending_status.remove(id).is_some();
        state.jobs.insert(id.to_owned(), job.clone());
        if repaired {
            let _ = self.persist_status_journal(&state);
        }
        if repaired
            && text(&job, "status") == "completed"
            && job["export_pending"] == true
            && self.inner.library.export_settings()["enabled"] == true
            && !state.pending_exports.iter().any(|queued| queued == id)
        {
            state.pending_exports.push_back(id.to_owned());
            self.inner.wake.notify_one();
        }
        self.inner.progress.notify_waiters();
        Ok(job)
    }
    pub(crate) fn ensure_status_writable(&self) -> Result<()> {
        if !self.inner.state.read().pending_status.is_empty() {
            return Err(Error::new(503, PERSISTENCE_WARNING));
        }
        Ok(())
    }
    fn persist_status_journal(&self, state: &State) -> Result<()> {
        let jobs: serde_json::Map<_, _> = state
            .pending_status
            .keys()
            .filter_map(|id| {
                let mut job = state.jobs.get(id)?.clone();
                if let Some(object) = job.as_object_mut() {
                    object.remove("persistence_pending");
                    object.remove("persistence_warning");
                }
                Some((id.clone(), job))
            })
            .collect();
        atomic_json(
            &self.inner.data.join("status-repairs.json"),
            &json!({"schema_version":1,"jobs":jobs}),
        )
    }
    fn recover_status_journal(&self) -> Result<()> {
        let Some(journal) = read_json_optional(&self.inner.data.join("status-repairs.json"))?
        else {
            return Ok(());
        };
        let invalid = || {
            Error::internal(
                "Pending local status repairs could not be read safely. The queue was not started.",
            )
        };
        let jobs = journal["jobs"].as_object().ok_or_else(invalid)?;
        if journal["schema_version"] != 1 {
            return Err(invalid());
        }
        if jobs.is_empty() {
            return Ok(());
        }
        for (id, saved) in jobs {
            if !valid_id(id)
                || text(saved, "id") != id
                || !valid_video(text(saved, "video_id"))
                || !valid_id(text(saved, "persistence_revision"))
                || saved["persistence_generation"]
                    .as_u64()
                    .is_none_or(|value| value == 0)
                || !canonical_video_url(text(saved, "url")).is_ok_and(|(video, url)| {
                    video == text(saved, "video_id") && url == text(saved, "url")
                })
                || !matches!(
                    text(saved, "status"),
                    "completed" | "failed" | "cancelled" | "removed"
                )
            {
                return Err(invalid());
            }
        }
        for (id, saved) in jobs {
            let path = self.inner.jobs_root.join(id).join("job.json");
            let current = match read_json_optional(&path) {
                Ok(value) => value,
                Err(_)
                    if std::fs::symlink_metadata(&path)
                        .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()) =>
                {
                    None
                }
                Err(_) => return Err(invalid()),
            };
            if let Some(current) = &current {
                if text(current, "id") != id
                    || current["video_id"] != saved["video_id"]
                    || current["url"] != saved["url"]
                {
                    return Err(invalid());
                }
                let current_generation = current["persistence_generation"].as_u64().unwrap_or(0);
                let saved_generation = saved["persistence_generation"]
                    .as_u64()
                    .ok_or_else(invalid)?;
                if current_generation >= saved_generation {
                    if matches!(
                        text(current, "status"),
                        "completed" | "failed" | "cancelled" | "removed"
                    ) && (current_generation > saved_generation
                        || current["persistence_revision"] == saved["persistence_revision"])
                    {
                        continue;
                    }
                    return Err(invalid());
                }
            }
            let mut job = saved.clone();
            if self.inner.removals.contains(text(&job, "video_id")) {
                merge(
                    &mut job,
                    json!({"status":"removed","message":REMOVED_MESSAGE,"error":Value::Null,"export_error":Value::Null,"export_pending":false}),
                );
            } else if job["cancel_requested"] == true
                || current
                    .as_ref()
                    .is_some_and(|job| job["cancel_requested"] == true)
            {
                merge(
                    &mut job,
                    json!({"status":"cancelled","cancel_requested":true,"error":Value::Null,"export_pending":false}),
                );
            } else if text(&job, "status") == "completed"
                && !crate::pipeline::completed_checkpoint(&job, &self.inner.jobs_root.join(id))
                    .unwrap_or(false)
            {
                merge(
                    &mut job,
                    json!({"status":"failed","export_pending":false,"error":"The saved final transcript could not be verified. Existing checkpoints were retained."}),
                );
            }
            if atomic_json(&path, &job).is_err() {
                let mut state = self.inner.state.write();
                state.order = state.order.max(job["queue_order"].as_u64().unwrap_or(0));
                state.pending_status.insert(
                    id.clone(),
                    PendingStatus {
                        attempts: 0,
                        next_attempt: Instant::now() + Duration::from_millis(250),
                        revision: text(&job, "persistence_revision").to_owned(),
                        generation: job["persistence_generation"].as_u64().ok_or_else(invalid)?,
                    },
                );
                job["persistence_pending"] = json!(true);
                job["persistence_warning"] = json!(PERSISTENCE_WARNING);
                state.jobs.insert(id.clone(), job);
            }
        }
        let state = self.inner.state.read();
        let _ = self.persist_status_journal(&state);
        Ok(())
    }
    fn update_reconciled_job(&self, id: &str, patch: Value) -> Result<Value> {
        if self.inner.state.read().pending_status.contains_key(id) {
            let job = self.get_job(id)?;
            // Read-side evidence/folder refresh must not turn every poll into
            // another failed write. Durable removal intent still takes effect
            // immediately; ordinary repair belongs to the backoff worker.
            if !self.inner.removals.contains(text(&job, "video_id"))
                || text(&job, "status") == "removed"
            {
                return Ok(job);
            }
        }
        match self.update_job(id, patch) {
            Err(_) if self.inner.state.read().pending_status.contains_key(id) => self.get_job(id),
            result => result,
        }
    }
    fn retry_status(&self, id: &str, schedule_export: bool) {
        let mut state = self.inner.state.write();
        if !state.pending_status.contains_key(id) {
            return;
        }
        let Some(mut job) = state.jobs.get(id).cloned() else {
            state.pending_status.remove(id);
            return;
        };
        if self.inner.removals.contains(text(&job, "video_id")) {
            merge(
                &mut job,
                json!({"status":"removed","message":REMOVED_MESSAGE,"error":Value::Null,"export_error":Value::Null,"export_pending":false}),
            );
        } else if job["cancel_requested"] == true {
            merge(
                &mut job,
                json!({"status":"cancelled","error":Value::Null,"export_pending":false}),
            );
        }
        if let Some(object) = job.as_object_mut() {
            object.remove("persistence_pending");
            object.remove("persistence_warning");
        }
        if atomic_json(&self.inner.jobs_root.join(id).join("job.json"), &job).is_err() {
            job["persistence_pending"] = json!(true);
            job["persistence_warning"] = json!(PERSISTENCE_WARNING);
            state.jobs.insert(id.to_owned(), job);
            if let Some(pending) = state.pending_status.get_mut(id) {
                pending.attempts = pending.attempts.saturating_add(1);
                let delay = 250u64
                    .saturating_mul(1u64 << pending.attempts.min(7))
                    .min(30_000);
                pending.next_attempt = Instant::now() + Duration::from_millis(delay);
            }
            let _ = self.persist_status_journal(&state);
            return;
        }
        state.pending_status.remove(id);
        state.jobs.insert(id.to_owned(), job.clone());
        let _ = self.persist_status_journal(&state);
        if schedule_export
            && text(&job, "status") == "completed"
            && job["export_pending"] == true
            && self.inner.library.export_settings()["enabled"] == true
            && !state.pending_exports.iter().any(|queued| queued == id)
        {
            state.pending_exports.push_back(id.to_owned());
        }
        self.inner.wake.notify_one();
        self.inner.progress.notify_waiters();
    }
    async fn repair_worker(&self) {
        loop {
            let notified = self.inner.repair_wake.notified();
            if self.inner.closing.load(Ordering::SeqCst) {
                break;
            }
            let next = self
                .inner
                .state
                .read()
                .pending_status
                .iter()
                .min_by_key(|(id, pending)| (pending.next_attempt, *id))
                .map(|(id, pending)| (id.clone(), pending.next_attempt));
            match next {
                Some((id, due)) if due <= Instant::now() => {
                    let engine = self.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || engine.retry_status(&id, true)).await;
                }
                Some((_, due)) => {
                    tokio::select! { _ = notified => {}, _ = tokio::time::sleep(due.saturating_duration_since(Instant::now())) => {} }
                }
                None => notified.await,
            }
        }
    }
    fn release_active(&self, id: &str, export_attempted: bool) {
        let mut state = self.inner.state.write();
        if state.active_id.as_deref() != Some(id) {
            return;
        }
        // A local repair may finish after the worker decided to skip autosave,
        // but before it releases the active slot. Hand off under the same lock.
        if !export_attempted
            && state.jobs.get(id).is_some_and(|job| {
                text(job, "status") == "completed"
                    && job["export_pending"] == true
                    && job["persistence_pending"] != true
            })
            && self.inner.library.export_settings()["enabled"] == true
            && !state.pending_exports.iter().any(|queued| queued == id)
        {
            state.pending_exports.push_back(id.to_owned());
        }
        state.active_id = None;
        state.active_cancel = None;
        self.inner.wake.notify_one();
        self.inner.progress.notify_waiters();
    }
    pub fn reconcile_removals(&self, force: bool) -> Result<()> {
        let _guard = self.inner.reconcile_lock.lock();
        if !force
            && self
                .inner
                .state
                .read()
                .last_removal
                .is_some_and(|last| last.elapsed() < Duration::from_secs(2))
        {
            return Ok(());
        }
        let jobs = self.jobs();
        let candidates: Vec<Value> = jobs
            .iter()
            .filter(|j| {
                text(j, "status") == "completed"
                    && !self.inner.removals.contains(text(j, "video_id"))
            })
            .cloned()
            .collect();
        let evidence =
            self.inner
                .library
                .scan_export_presence(&candidates, &self.inner.jobs_root, force)?;
        let mut absent: HashSet<String> = if evidence["complete"] == true {
            strings(&evidence["absent"]).into_iter().collect()
        } else {
            HashSet::new()
        };
        let retained: HashSet<String> = candidates
            .iter()
            .filter(|job| {
                has_exports(job)
                    && (!absent.contains(text(job, "id"))
                        || self
                            .inner
                            .library
                            .is_saved_anywhere(job, &self.inner.jobs_root))
            })
            .map(|j| text(j, "video_id").to_owned())
            .collect();
        absent.retain(|id| {
            !candidates
                .iter()
                .any(|j| text(j, "id") == id && retained.contains(text(j, "video_id")))
        });
        self.inner.removals.add(
            &candidates
                .iter()
                .filter(|j| absent.contains(text(j, "id")))
                .cloned()
                .collect::<Vec<_>>(),
        )?;
        {
            let mut state = self.inner.state.write();
            state.removal_uncertain = strings(&evidence["uncertain"]).into_iter().collect();
            state.last_removal = Some(Instant::now());
        }
        let records = self.inner.removals.records();
        for job in jobs {
            let id = text(&job, "id");
            if self.inner.removals.contains(text(&job, "video_id")) {
                {
                    let state = self.inner.state.read();
                    if state.active_id.as_deref() == Some(id)
                        && let Some(token) = &state.active_cancel
                    {
                        token.cancel();
                    }
                }
                if text(&job, "status") != "removed" || job["export_pending"] == true {
                    self.update_reconciled_job(id,json!({"removed_previous_status":job["status"],"removed_at":records[text(&job,"video_id")]["removed_at"]}))?;
                }
            }
        }
        if let Some(present) = evidence["present"].as_object() {
            for (id, path) in present {
                if let Ok(job) = self.get_job(id)
                    && text(&job, "status") == "completed"
                    && (job["export_path"] != *path || job["export_evidence"].is_null())
                {
                    let mut candidate = job.clone();
                    candidate["export_path"] = path.clone();
                    let remembered = self
                        .inner
                        .library
                        .export_evidence(&candidate, &self.inner.jobs_root);
                    self.update_reconciled_job(id,json!({"export_path":path,"export_evidence":remembered.unwrap_or(job["export_evidence"].clone())}))?;
                }
            }
        }
        Ok(())
    }
    pub(crate) fn existing_job(&self, video: &str) -> Option<Value> {
        // Thumbnail reads only need attempts for this video. Cloning/sorting
        // the entire library for every requested video makes batched reads quadratic.
        let mut jobs: Vec<_> = self
            .inner
            .state
            .read()
            .jobs
            .values()
            .filter(|job| text(job, "video_id") == video)
            .cloned()
            .collect();
        jobs.sort_by(job_order);
        self.existing_from_jobs(video, &jobs, None)
    }
    fn existing_from_jobs(
        &self,
        video: &str,
        jobs: &[Value],
        saved: Option<&HashMap<String, bool>>,
    ) -> Option<Value> {
        let matches: Vec<_> = jobs
            .iter()
            .filter(|job| text(job, "video_id") == video)
            .collect();
        if self.inner.removals.contains(video) {
            return matches.iter().find(|j|text(j,"status")=="removed").or(matches.first()).map(|j|(*j).clone()).or_else(||{
            let record=self.inner.removals.records();let record=&record[video];if !record.is_object(){return None;}Some(json!({"id":record["job_id"],"video_id":video,"url":format!("https://www.youtube.com/watch?v={video}"),"title":video,"status":"removed","progress":100,"message":REMOVED_MESSAGE,"error":Value::Null,"created_at":record["removed_at"],"updated_at":record["removed_at"],"metadata":{},"completed_chunks":0,"total_chunks":0,"document_available":false,"folder_id":Value::Null,"export_path":Value::Null,"export_error":Value::Null,"removed_at":record["removed_at"]}))
        });
        }
        let completed: Vec<_> = matches
            .iter()
            .filter(|j| {
                text(j, "status") == "completed"
                    && self
                        .inner
                        .jobs_root
                        .join(text(j, "id"))
                        .join("transcript.json")
                        .is_file()
            })
            .collect();
        completed
            .iter()
            .find(|j| {
                saved.map_or_else(
                    || {
                        self.inner
                            .library
                            .is_saved_anywhere(j, &self.inner.jobs_root)
                    },
                    |facts| facts.get(text(j, "id")).copied().unwrap_or(false),
                )
            })
            .or(completed.first())
            .map(|j| (***j).clone())
            .or_else(|| {
                matches
                    .into_iter()
                    .find(|j| active(text(j, "status")))
                    .cloned()
            })
    }
    fn require_ready(&self) -> Result<()> {
        if self.inner.test_pipeline {
            return Ok(());
        }
        let settings = self.inner.settings.read();
        if !settings.configured() {
            return Err(Error::new(
                503,
                "Add your Azure Speech resource and key in Settings first.",
            ));
        }
        let deps = settings.dependencies();
        if deps
            .as_object()
            .is_none_or(|d| d.values().any(|v| v != true))
        {
            return Err(Error::new(
                503,
                "The local audio dependencies are missing. Run setup again.",
            ));
        }
        Ok(())
    }
    pub fn submit(&self, url: &str, folder: Option<Option<String>>) -> Result<Value> {
        let _submission = self.inner.submission_lock.lock();
        self.submit_locked(url, folder, None)
    }
    pub(crate) fn submit_locked(
        &self,
        url: &str,
        folder: Option<Option<String>>,
        origin: Option<&str>,
    ) -> Result<Value> {
        if self.inner.stopping.load(Ordering::SeqCst) || self.inner.closing.load(Ordering::SeqCst) {
            return Err(Error::conflict(
                "The queue is stopping. Wait before adding videos.",
            ));
        }
        let (video, url) = canonical_video_url(url)?;
        let previously_exported = self
            .inner
            .state
            .read()
            .jobs
            .values()
            .any(|job| text(job, "video_id") == video && has_exports(job));
        self.reconcile_removals(previously_exported)?;
        if self.inner.removals.contains(&video) {
            return self
                .existing_job(&video)
                .ok_or_else(|| Error::conflict(REMOVED_MESSAGE));
        }
        self.require_ready()?;
        self.reconcile_library_folders()?;
        let selected = folder.clone().unwrap_or_else(|| {
            self.inner.library.snapshot()["default_folder_id"]
                .as_str()
                .map(String::from)
        });
        self.inner.library.validate_folder(selected.as_deref())?;
        if let Some(existing) = self.existing_job(&video) {
            let id = text(&existing, "id");
            if folder.is_some() && existing["folder_id"] != json!(selected) {
                self.move_jobs(&[id.to_owned()], selected.as_deref())?;
            } else if text(&existing, "status") == "completed"
                && self.inner.state.read().pending_status.is_empty()
                && self.inner.library.export_settings()["enabled"] == true
                && (self
                    .inner
                    .library
                    .is_saved(&existing, &self.inner.jobs_root)
                    || !self
                        .inner
                        .library
                        .is_saved_anywhere(&existing, &self.inner.jobs_root))
            {
                let _ = self.export_one(id, None);
            }
            return self.get_job(id);
        }
        self.ensure_status_writable()?;
        if self
            .jobs()
            .iter()
            .any(|j| text(j, "video_id") == video && has_exports(j))
        {
            return Err(Error::conflict(
                "This video was previously exported, but its saved transcript cannot be verified. It will not be downloaded or transcribed again automatically.",
            ));
        }
        let batch = self.accept_batch(false)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let timestamp = now();
        let mut state = self.inner.state.write();
        state.order += 1;
        let mut job = json!({"id":id,"url":url,"video_id":video,"batch_id":batch,"title":video,"status":"queued","progress":0,"message":"Queued for full-video transcription…","error":Value::Null,"created_at":timestamp,"queue_order":state.order,"updated_at":timestamp,"metadata":{},"completed_chunks":0,"total_chunks":0,"document_available":false,"folder_id":selected,"export_path":Value::Null,"export_error":Value::Null});
        if let Some(origin) = origin {
            job["channel_import_id"] = json!(origin);
            job["channel_import_ids"] = json!([origin]);
        }
        atomic_json(&self.inner.jobs_root.join(&id).join("job.json"), &job)?;
        state.jobs.insert(id.clone(), job.clone());
        state.queue.push_back(id);
        drop(state);
        self.inner.wake.notify_one();
        Ok(job)
    }
    pub(crate) fn export_one(&self, id: &str, format: Option<&str>) -> Result<bool> {
        if self.inner.state.read().pending_status.contains_key(id) {
            return Err(Error::new(503, PERSISTENCE_WARNING));
        }
        self.reconcile_library_folders()?;
        self.reconcile_removals(true)?;
        let job = self.get_job(id)?;
        if text(&job, "status") == "removed" || self.inner.removals.contains(text(&job, "video_id"))
        {
            return Ok(false);
        }
        if self.inner.state.read().removal_uncertain.contains(id) {
            self.update_job(id,json!({"export_error":"The previously saved transcript could not be located safely. It will not be recreated automatically.","export_pending":false}))?;
            return Ok(false);
        }
        match self
            .inner
            .library
            .export_job_detail(&job, &self.inner.jobs_root, format)
        {
            Ok(outcome) => {
                for other in self.jobs() {
                    let mut paths = strings(&other["export_paths"]);
                    let current = text(&other, "export_path");
                    let replaced = outcome
                        .replacements
                        .get(current)
                        .cloned()
                        .unwrap_or_else(|| current.to_owned());
                    let old = paths.clone();
                    for p in &mut paths {
                        if let Some(next) = outcome.replacements.get(p) {
                            *p = next.clone();
                        }
                    }
                    paths.sort();
                    paths.dedup();
                    if replaced != current || old != paths {
                        self.update_job(text(&other,"id"),json!({"export_path":if replaced.is_empty(){Value::Null}else{json!(replaced)},"export_paths":paths}))?;
                    }
                }
                let mut current = self.get_job(id)?;
                let mut paths = strings(&current["export_paths"]);
                if let Some(path) = current["export_path"].as_str() {
                    paths.push(path.into());
                }
                paths.push(outcome.path.clone());
                paths.sort();
                paths.dedup();
                current["export_path"] = json!(outcome.path);
                current["export_paths"] = json!(paths);
                let evidence = self
                    .inner
                    .library
                    .export_evidence(&current, &self.inner.jobs_root);
                self.update_job(id,json!({"export_path":current["export_path"],"export_paths":paths,"export_evidence":evidence,"export_error":Value::Null,"export_pending":false,"last_removed_duplicate_count":outcome.removed_duplicate_count}))?;
                Ok(true)
            }
            Err(error) => {
                if has_exports(&job) {
                    self.reconcile_removals(true)?;
                }
                self.update_job(
                    id,
                    json!({"export_error":error.message,"export_pending":false}),
                )?;
                Ok(false)
            }
        }
    }
    pub fn export_jobs(&self, ids: &[String], format: Option<&str>) -> Result<Value> {
        if format.is_some_and(|f| !["txt", "md", "json"].contains(&f)) {
            return Err(Error::bad_request(
                "Choose TXT, Markdown, or JSON for this export.",
            ));
        }
        self.reconcile_removals(true)?;
        let mut result = json!({"saved_count":0,"failed_count":0,"errors":[]});
        let mut removed = 0;
        let mut duplicates = 0;
        for id in ids {
            let job = self.get_job(id)?;
            if text(&job, "status") == "removed" {
                removed += 1;
                continue;
            }
            if self.export_one(id, format)? {
                result["saved_count"] = json!(result["saved_count"].as_u64().unwrap_or(0) + 1);
                duplicates += self.get_job(id)?["last_removed_duplicate_count"]
                    .as_u64()
                    .unwrap_or(0);
            } else {
                result["failed_count"] = json!(result["failed_count"].as_u64().unwrap_or(0) + 1);
                result["errors"]
                    .as_array_mut()
                    .ok_or_else(|| Error::internal("Export results could not be recorded."))?
                    .push(json!({"job_id":id,"error":self.get_job(id)?["export_error"]}));
            }
        }
        if removed > 0 {
            result["skipped_removed_count"] = json!(removed);
        }
        if duplicates > 0 {
            result["removed_duplicate_count"] = json!(duplicates);
        }
        Ok(result)
    }
    pub fn move_jobs(&self, ids: &[String], folder: Option<&str>) -> Result<Vec<Value>> {
        self.reconcile_removals(true)?;
        self.inner.library.validate_folder(folder)?;
        let mut result = vec![];
        for id in ids {
            let job = self.get_job(id)?;
            if job["folder_id"] != json!(folder) {
                let changed = self.update_job(id, json!({"folder_id":folder}))?;
                if text(&changed, "status") == "completed"
                    && self.inner.library.export_settings()["enabled"] == true
                {
                    let _ = self.export_one(id, None)?;
                }
            }
            result.push(self.get_job(id)?);
        }
        Ok(result)
    }
    async fn worker(&self) {
        loop {
            let notified = self.inner.wake.notified();
            if self.inner.closing.load(Ordering::SeqCst) {
                break;
            }
            let selected = {
                let _submission = self.inner.submission_lock.lock();
                let mut state = self.inner.state.write();
                let mut found = None;
                while let Some(id) = state.pending_exports.pop_front() {
                    if state.jobs.get(&id).is_some_and(|job| {
                        text(job, "status") == "completed"
                            && job["export_pending"] == true
                            && job["persistence_pending"] != true
                    }) {
                        state.active_id = Some(id.clone());
                        state.active_cancel = None;
                        found = Some((id, CancellationToken::new(), true));
                        break;
                    }
                }
                while found.is_none()
                    && let Some(id) = state.queue.pop_front()
                {
                    if state
                        .jobs
                        .get(&id)
                        .is_some_and(|j| text(j, "status") == "queued")
                    {
                        let token = CancellationToken::new();
                        state.active_id = Some(id.clone());
                        state.active_cancel = Some(token.clone());
                        found = Some((id, token, false));
                        break;
                    }
                }
                found
            };
            let Some((id, cancel, export_only)) = selected else {
                notified.await;
                continue;
            };
            if export_only {
                let engine = self.clone();
                let export_id = id.clone();
                let _ =
                    tokio::task::spawn_blocking(move || engine.export_one(&export_id, None)).await;
                self.release_active(&id, true);
                continue;
            }
            let mut export_attempted = false;
            let job = self.get_job(&id);
            if let Ok(job) = job {
                let engine = self.clone();
                let update_id = id.clone();
                let update: Update =
                    Arc::new(move |patch| engine.update_job(&update_id, patch).map(|_| ()));
                let settings = self.inner.settings.read().clone();
                let result = if self.inner.removals.contains(text(&job, "video_id")) {
                    Err(cancelled())
                } else {
                    self.inner
                        .pipeline
                        .run(
                            settings,
                            job,
                            update,
                            self.inner.jobs_root.join(&id),
                            self.inner.work.clone(),
                            cancel,
                        )
                        .await
                };
                if let Err(error) = result {
                    let latest = self.get_job(&id).unwrap_or(Value::Null);
                    let committed = matches!(text(&latest, "status"), "transcribing" | "completed")
                        && latest["cancel_requested"] != true
                        && !self.inner.removals.contains(text(&latest, "video_id"))
                        && crate::pipeline::completed_checkpoint(
                            &latest,
                            &self.inner.jobs_root.join(&id),
                        )
                        .unwrap_or(false);
                    if committed {
                        let _=self.update_job(&id,json!({"status":"completed","progress":100,"document_available":true,"error":Value::Null,
                            "completed_chunks":latest["total_chunks"],"message":"Complete transcript recovered after its final status update failed."}));
                    } else {
                        let status =
                            if error.status == 499 && !self.inner.closing.load(Ordering::SeqCst) {
                                "cancelled"
                            } else {
                                "failed"
                            };
                        let _=self.update_job(&id,json!({"status":status,"error":if status=="cancelled"{Value::Null}else{json!(if self.inner.closing.load(Ordering::SeqCst){"The helper stopped before this video finished. Temporary audio was deleted; completed chunks were retained.".to_owned()}else{error.message})},"message":"Processing stopped. Temporary audio deleted.","document_available":self.inner.jobs_root.join(&id).join("transcript.json").is_file()}));
                    }
                }
                if self.get_job(&id).is_ok_and(|j| {
                    text(&j, "status") == "completed" && j["persistence_pending"] != true
                }) && self.inner.library.export_settings()["enabled"] == true
                {
                    export_attempted = true;
                    let engine = self.clone();
                    let export_id = id.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || engine.export_one(&export_id, None))
                            .await;
                }
            }
            self.release_active(&id, export_attempted);
            tokio::task::yield_now().await;
        }
    }
    pub async fn shutdown(&self) -> Result<()> {
        let handles = {
            let _submission = self.inner.submission_lock.lock();
            self.inner.closing.store(true, Ordering::SeqCst);
            self.inner.wake.notify_waiters();
            self.inner.repair_wake.notify_waiters();
            if let Some(cancel) = self.inner.state.read().active_cancel.clone() {
                cancel.cancel();
            }
            for producer in self.inner.producers.lock().values() {
                producer.cancel.cancel();
            }
            std::mem::take(&mut *self.inner.tasks.lock())
        };
        for handle in handles {
            let _ = handle.await;
        }
        let pending: Vec<_> = self
            .inner
            .state
            .read()
            .pending_status
            .keys()
            .cloned()
            .collect();
        for id in pending {
            self.retry_status(&id, false);
        }
        self.ensure_status_writable()?;
        Ok(())
    }
    pub(crate) fn begin_cancel_ids(
        &self,
        ids: &HashSet<String>,
    ) -> Result<(usize, Option<String>)> {
        let targets: Vec<_> = self
            .jobs()
            .into_iter()
            .filter(|j| ids.contains(text(j, "id")) && active(text(j, "status")))
            .collect();
        // Persist all queued cancellations before the first await, so cleanup
        // cannot allow the worker to begin the next paid request.
        let mut persistence_error = None;
        for job in &targets {
            if let Err(error) = self.update_job(text(job,"id"),json!({"status":"cancelled","cancel_requested":true,"error":Value::Null,"message":"Cancelled. Completed text is retained; temporary audio is removed.","document_available":self.inner.jobs_root.join(text(job,"id")).join("transcript.json").is_file()})) {
                persistence_error = Some(error);
            }
        }
        let wait_id = {
            let state = self.inner.state.read();
            if state.active_id.as_ref().is_some_and(|id| ids.contains(id)) {
                if let Some(cancel) = &state.active_cancel {
                    cancel.cancel();
                }
                state.active_id.clone()
            } else {
                None
            }
        };
        if let Some(error) = persistence_error {
            return Err(error);
        }
        Ok((targets.len(), wait_id))
    }
    pub(crate) async fn wait_cancelled(&self, id: Option<String>) {
        if let Some(id) = id {
            loop {
                let notified = self.inner.progress.notified();
                if self.inner.state.read().active_id.as_deref() != Some(&id) {
                    break;
                }
                notified.await;
            }
        }
    }
    pub async fn request(&self, action: &str, input: Value) -> Result<Value> {
        match action {
            "cancelJob" => {
                let job = self.get_job(text(&input, "id"))?;
                if active(text(&job, "status")) {
                    self.cancel_video(text(&job, "video_id")).await?;
                }
                self.get_job(text(&input, "id"))
            }
            "cancelVideo" => self.cancel_video(text(&input, "video_id")).await,
            "cancelQueue" => self.cancel_all().await,
            "cancelChannelPreview" => self.cancel_channel(text(&input, "id")).await,
            "testConnection" => {
                let settings = self.inner.settings.read().clone();
                crate::pipeline::test_connection(&settings).await?;
                Ok(json!({"ok":true,"provider":"azure","model":scribe_common::MODEL}))
            }
            _ => {
                let engine = self.clone();
                let action = action.to_owned();
                tokio::task::spawn_blocking(move || engine.request_sync(&action, input))
                    .await
                    .map_err(|_| Error::internal("The local operation could not finish."))?
            }
        }
    }
    fn request_sync(&self, action: &str, input: Value) -> Result<Value> {
        let id = text(&input, "id");
        match action {
            "jobs" => {
                self.reconcile_removals(false)?;
                Ok(Value::Object(serde_json::Map::from_iter([(
                    "jobs".to_owned(),
                    Value::Array(self.jobs()),
                )])))
            }
            "getJob" => {
                self.reconcile_removals(false)?;
                self.get_job(id)
            }
            "submitJob" => self.submit(
                text(&input, "url"),
                input.get("folder_id").map(|v| v.as_str().map(String::from)),
            ),
            "queueStatus" => self.queue_status(Some(10)),
            "queueAttention" => {
                let result = self.queue_status(None)?;
                Ok(
                    json!({"attention":result["attention"],"attention_count":result["attention_count"]}),
                )
            }
            "videoStatuses" => self.video_statuses(&strings(&input["video_ids"])),
            "moveJobs" => Ok(
                json!({"jobs":self.move_jobs(&strings(&input["job_ids"]),input["folder_id"].as_str())?}),
            ),
            "exportJobs" => {
                let ids = if input.get("job_ids").is_some() {
                    strings(&input["job_ids"])
                } else {
                    self.jobs()
                        .into_iter()
                        .filter(|j| {
                            text(j, "status") == "completed"
                                && (input.get("folder_id").is_none()
                                    || j["folder_id"] == input["folder_id"])
                        })
                        .map(|j| text(&j, "id").to_owned())
                        .collect()
                };
                self.export_jobs(&ids, input["format"].as_str())
            }
            "startChannelPreview" => self.start_channel(&input, false),
            "startChannelImport" => {
                self.require_ready()?;
                self.start_channel(&input, true)
            }
            "getChannelPreview" => self.public_channel(id),
            "queueChannelPreview" => {
                self.require_ready()?;
                self.queue_channel(id)
            }
            "librarySnapshot" => self.library_snapshot(),
            "removeMissingFolder" => {
                self.inner
                    .library
                    .remove_missing_folder(text(&input, "folder_id"), &self.channel_folder_ids())?;
                self.reconcile_library_folders()?;
                self.library_snapshot()
            }
            "libraryStats" => {
                self.reconcile_removals(false)?;
                let mut result = self.inner.library.file_stats(
                    &self.inventory_jobs(),
                    &self.inner.jobs_root,
                    input["folder_id"].as_str(),
                )?;
                result["removed"] = json!(
                    self.inner
                        .removals
                        .records()
                        .as_object()
                        .map_or(0, |o| o.len())
                );
                Ok(result)
            }
            "folderDocuments" => self.inner.library.folder_documents(
                &self.jobs(),
                &self.inner.jobs_root,
                input["folder_id"].as_str(),
            ),
            "readDocument" => self.read_document(id, input["format"].as_str().unwrap_or("md")),
            "revealVideo" => self.reveal_video(text(&input, "video_id")),
            _ => Err(Error::not_found("Unsupported engine action.")),
        }
    }
    fn reconcile_library_folders(&self) -> Result<()> {
        self.inner
            .library
            .reconcile_folders(&self.channel_folder_ids(), &self.jobs())?;
        let snapshot = self.inner.library.snapshot();
        let known: HashSet<String> = snapshot["folders"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|folder| folder["id"].as_str().map(String::from))
            .collect();
        for job in self.jobs() {
            if job["folder_id"]
                .as_str()
                .is_some_and(|id| !known.contains(id))
            {
                self.update_reconciled_job(text(&job, "id"), json!({"folder_id":Value::Null}))?;
            }
        }
        Ok(())
    }
    fn library_snapshot(&self) -> Result<Value> {
        self.reconcile_library_folders()?;
        let ids = self.channel_folder_ids();
        let mut result = self.inner.library.snapshot();
        if let Some(folders) = result["folders"].as_array_mut() {
            for folder in folders {
                folder["kind"] = json!(if ids.contains(text(folder, "id")) {
                    "channel"
                } else {
                    "manual"
                });
            }
        }
        Ok(result)
    }
    fn read_document(&self, id: &str, format: &str) -> Result<Value> {
        self.reconcile_removals(true)?;
        if !["txt", "md", "json"].contains(&format) {
            return Err(Error::bad_request("Choose TXT, Markdown, or JSON."));
        }
        let job = self.get_job(id)?;
        if text(&job, "status") == "removed" {
            return Err(Error::conflict(
                "This transcript was removed from the save folder and is excluded from reuse.",
            ));
        }
        if !matches!(text(&job, "status"), "completed" | "failed" | "cancelled")
            || job["document_available"] != true
        {
            return Err(Error::conflict(
                "This transcript document is not ready yet.",
            ));
        }
        let document = read_json(&self.inner.jobs_root.join(id).join("transcript.json"))?;
        let content = render_document(&document, format)?;
        let mut filename = export_filename(text(&job, "title"), text(&job, "video_id"), format);
        if text(&job, "status") != "completed" {
            filename = format!("PARTIAL - {filename}");
        }
        let mut out = json!({"filename":filename,"content":content,"mime_type":match format{"txt"=>"text/plain;charset=utf-8","md"=>"text/markdown;charset=utf-8",_=>"application/json;charset=utf-8"}});
        // For the reader only: where each recognized section starts. The rendered documents stay exactly as they were.
        let sections = document_sections(&document);
        if !sections.is_empty() {
            out["sections"] = json!(sections);
        }
        Ok(out)
    }
    fn reveal_video(&self, video: &str) -> Result<Value> {
        if !valid_video(video) {
            return Err(Error::bad_request("Use a valid YouTube video ID."));
        }
        self.reconcile_removals(true)?;
        let job = self
            .existing_job(video)
            .ok_or_else(|| Error::not_found("No completed transcript exists for this video."))?;
        if text(&job, "status") == "removed" {
            return Err(Error::conflict(REMOVED_MESSAGE));
        }
        if text(&job, "status") != "completed" {
            return Err(Error::conflict("This transcript is not complete yet."));
        }
        let mut path = self
            .inner
            .library
            .verified_export_path(&job, &self.inner.jobs_root);
        if path.is_none() {
            if text(&self.inner.library.export_settings(), "directory").is_empty() {
                return Err(Error::conflict(
                    "Choose a destination folder before showing the saved transcript.",
                ));
            }
            if !self.export_one(text(&job, "id"), None)? {
                return Err(Error::conflict(
                    "The completed transcript could not be saved. Check the destination folder.",
                ));
            }
            path = self
                .inner
                .library
                .verified_export_path(&self.get_job(text(&job, "id"))?, &self.inner.jobs_root);
        }
        Ok(
            json!({"path":path.ok_or_else(||Error::conflict("The saved transcript could not be verified."))?}),
        )
    }
    pub fn video_statuses(&self, videos: &[String]) -> Result<Value> {
        if videos.len() > 200 || videos.iter().any(|v| !valid_video(v)) {
            return Err(Error::bad_request("Use up to200 valid YouTube video IDs."));
        }
        self.reconcile_removals(false)?;
        let requested: HashSet<&str> = videos.iter().map(String::as_str).collect();
        let mut jobs: Vec<_> = self
            .inner
            .state
            .read()
            .jobs
            .values()
            .filter(|job| requested.contains(text(job, "video_id")))
            .cloned()
            .collect();
        jobs.sort_by(job_order);
        let facts = self
            .inner
            .library
            .saved_job_facts(&jobs, &self.inner.jobs_root);
        let mut out = serde_json::Map::new();
        for video in videos {
            let job = self
                .existing_from_jobs(video, &jobs, Some(&facts))
                .or_else(|| {
                    jobs.iter()
                        .find(|j| {
                            text(j, "video_id") == video
                                && matches!(text(j, "status"), "failed" | "cancelled")
                        })
                        .cloned()
                });
            if let Some(job) = job {
                out.insert(video.clone(),json!({"status":job["status"],"saved":text(&job,"status")=="completed"&&facts.get(text(&job,"id")).copied().unwrap_or(false)}));
            }
        }
        Ok(json!({"statuses":out}))
    }
}
/// The start time and text of each section of a saved document: its segments when Azure gave them, else its chunks.
fn document_sections(document: &Value) -> Vec<Value> {
    let mut out = vec![];
    for chunk in document["chunks"].as_array().into_iter().flatten() {
        let segments = chunk["segments"].as_array().filter(|s| !s.is_empty());
        for part in segments.map_or_else(|| std::slice::from_ref(chunk), |s| s.as_slice()) {
            if let (Some(offset), Some(words)) = (part["offset_ms"].as_f64(), part["text"].as_str())
                && !words.trim().is_empty()
            {
                out.push(json!({"offset_ms":offset,"text":words}));
            }
        }
    }
    out
}
fn job_order(a: &Value, b: &Value) -> std::cmp::Ordering {
    text(b, "created_at")
        .cmp(text(a, "created_at"))
        .then_with(|| b["queue_order"].as_u64().cmp(&a["queue_order"].as_u64()))
}
fn has_exports(job: &Value) -> bool {
    job["export_path"].as_str().is_some_and(|v| !v.is_empty())
        || job["export_paths"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod repair_handoff_tests {
    use super::*;
    struct NoAudio;
    impl PipelineRunner for NoAudio {
        fn run(
            &self,
            _: scribe_common::Settings,
            _: Value,
            _: Update,
            _: PathBuf,
            _: PathBuf,
            _: CancellationToken,
        ) -> crate::pipeline::RunFuture {
            Box::pin(async { panic!("A status repair must not execute the audio pipeline") })
        }
    }
    async fn repaired_export_handoff(inline_succeeded: bool) {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let settings = Arc::new(RwLock::new(scribe_common::Settings {
            root: root.clone(),
            data_dir: root.join("data"),
            speech_key: "offline".into(),
            speech_endpoint: "https://offline.cognitiveservices.azure.com".into(),
            helper_token: "offline".into(),
            ffmpeg_location: String::new(),
            cookies_browser: String::new(),
            js_runtime: String::new(),
            chunk_seconds: 600,
            audio_speed: 1.25,
        }));
        let library = Arc::new(Library::open(root.join("data")).unwrap());
        let output = root.join("saved");
        std::fs::create_dir(&output).unwrap();
        library.set_directory(&output).unwrap();
        let engine = Engine::open_with_pipeline(settings, library.clone(), Arc::new(NoAudio), true)
            .await
            .unwrap();
        let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let mut job = json!({"id":id,"video_id":"v0000000001","url":"https://www.youtube.com/watch?v=v0000000001","title":"Offline handoff","status":"completed","document_available":true,"total_chunks":1,"metadata":{"audio_duration_seconds":1.,"duration_seconds":1.},"export_pending":true,"persistence_pending":true,"persistence_revision":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"});
        scribe_common::write_exports(&engine.inner.jobs_root.join(id), &scribe_common::build_document(&job, &[json!({"index":1,"offset_ms":0.,"duration_ms":1000.,"text":"Existing complete text only."})], true)).unwrap();
        {
            let mut state = engine.inner.state.write();
            state.jobs.insert(id.into(), job.clone());
            state.active_id = Some(id.into());
            state.pending_status.insert(
                id.into(),
                PendingStatus {
                    attempts: 0,
                    next_attempt: Instant::now() + Duration::from_secs(60),
                    revision: text(&job, "persistence_revision").into(),
                    generation: 1,
                },
            );
        }
        // The worker already counted an export attempt, but its initial guard
        // found newly pending metadata. Repair wins the lock before release.
        assert!(engine.get_job(id).unwrap()["persistence_pending"] == true);
        engine.retry_status(id, true);
        assert_eq!(engine.inner.state.read().pending_exports.len(), 1);
        if inline_succeeded {
            assert!(engine.export_one(id, None).unwrap());
        }
        engine.release_active(id, true);
        tokio::time::timeout(Duration::from_secs(3), async {
            while engine.has_active_work() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        job = engine.get_job(id).unwrap();
        assert!(
            library.is_saved_anywhere(&job, &engine.inner.jobs_root),
            "error={} pending={} path={}",
            job["export_error"],
            job["export_pending"],
            job["export_path"]
        );
        assert!(
            engine.inner.state.read().queue.is_empty(),
            "no transcription job was queued"
        );
        assert_eq!(std::fs::read_dir(&output).unwrap().count(), 1);
        engine.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn repair_after_an_already_counted_export_attempt_hands_off_existing_text() {
        repaired_export_handoff(false).await;
    }
    #[tokio::test]
    async fn repaired_export_queue_discards_a_successful_inline_save_without_duplicate_work() {
        repaired_export_handoff(true).await;
    }
}
