#![allow(clippy::unwrap_used)]
use parking_lot::RwLock;
use scribe_common::{Settings, SharedSettings, atomic_json, build_document, write_exports};
use scribe_engine::{
    Engine,
    pipeline::{PipelineRunner, RunFuture, Update},
};
use scribe_library::Library;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

struct FakePipeline {
    started: mpsc::UnboundedSender<String>,
    release: Arc<Semaphore>,
}
impl PipelineRunner for FakePipeline {
    fn run(
        &self,
        _settings: Settings,
        job: Value,
        update: Update,
        result: PathBuf,
        work: PathBuf,
        cancel: CancellationToken,
    ) -> RunFuture {
        let started = self.started.clone();
        let release = self.release.clone();
        Box::pin(async move {
            let temp = tempfile::Builder::new()
                .prefix("offline-audio-")
                .tempdir_in(work)?;
            std::fs::write(temp.path().join("audio.wav"), b"synthetic temporary media")?;
            update(json!({"status":"transcribing","metadata":{"channel":"Offline fixture"}}))?;
            let _ = started.send(job["video_id"].as_str().unwrap().to_owned());
            tokio::select! {biased;_=cancel.cancelled()=>return Err(scribe_engine::process::cancelled()),permit=release.acquire()=>{permit.map_err(|_|scribe_engine::process::cancelled())?.forget();}}
            write_exports(
                &result,
                &build_document(&job, &[json!({"text":"Every offline fixture word."})], true),
            )?;
            temp.close()?;
            update(json!({"status":"completed","progress":100,"document_available":true}))?;
            Ok(())
        })
    }
}
fn settings(temp: &tempfile::TempDir) -> SharedSettings {
    Arc::new(RwLock::new(Settings {
        root: temp.path().into(),
        data_dir: temp.path().join("data"),
        speech_key: "test-only".into(),
        speech_endpoint: "https://test.cognitiveservices.azure.com".into(),
        helper_token: "test-token".into(),
        ffmpeg_location: String::new(),
        cookies_browser: String::new(),
        js_runtime: String::new(),
        chunk_seconds: 600,
        audio_speed: 1.25,
    }))
}
async fn setup(
    config: SharedSettings,
) -> (Engine, mpsc::UnboundedReceiver<String>, Arc<Semaphore>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let library = Arc::new(Library::open(&config.read().data_dir).unwrap());
    let engine = Engine::open_with_pipeline(
        config,
        library,
        Arc::new(FakePipeline {
            started: tx,
            release: release.clone(),
        }),
        true,
    )
    .await
    .unwrap();
    (engine, rx, release)
}
async fn started(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn settle(engine: &Engine) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !engine.has_active_work() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn url(n: usize) -> String {
    format!("https://www.youtube.com/watch?v=v{n:010}")
}
fn seed(config: &SharedSettings, n: usize, status: &str, extra: Value) -> Value {
    let id = format!("{n:032x}");
    let mut job = json!({"id":id,"video_id":format!("v{n:010}"),"url":url(n),"title":format!("Fixture {n}"),"status":status,"created_at":"2026-10-22T12:00:00Z","updated_at":"2026-10-22T12:00:00Z","queue_order":n,"metadata":{},"document_available":status=="completed","folder_id":Value::Null,"legacy_field":{"preserve":"yes"}});
    for (k, v) in extra.as_object().unwrap() {
        job[k] = v.clone();
    }
    let path = config.read().data_dir.join("jobs").join(&id);
    atomic_json(&path.join("job.json"), &job).unwrap();
    if status == "completed" {
        write_exports(
            &path,
            &build_document(&job, &[json!({"text":"Historical complete text"})], true),
        )
        .unwrap();
    }
    job
}

#[tokio::test]
async fn cancelled_queue_is_durable_before_active_cleanup_and_never_runs_next() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let (engine, mut rx, _) = setup(config.clone()).await;
    let one = engine.submit(&url(1), None).unwrap();
    let two = engine.submit(&url(2), None).unwrap();
    let three = engine.submit(&url(3), None).unwrap();
    assert_eq!(started(&mut rx).await, "v0000000001");
    let stopped = engine.cancel_all().await.unwrap();
    assert_eq!(stopped["cancelled_jobs"], 3);
    assert_eq!(stopped["status"]["batch_progress"]["cancelled"], 3);
    assert_eq!(stopped["status"]["attention_count"], 0);
    assert!(!engine.has_active_work());
    assert!(rx.try_recv().is_err());
    for job in [one, two, three] {
        let id = job["id"].as_str().unwrap();
        assert_eq!(engine.get_job(id).unwrap()["status"], "cancelled");
        let disk = scribe_common::read_json(
            &config
                .read()
                .data_dir
                .join("jobs")
                .join(id)
                .join("job.json"),
        )
        .unwrap();
        assert_eq!(disk["cancel_requested"], true);
    }
    assert_eq!(
        std::fs::read_dir(config.read().data_dir.join("work"))
            .unwrap()
            .count(),
        0
    );
    engine.shutdown().await.unwrap();
    let (restored, mut rx, _) = setup(config).await;
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err());
    assert_eq!(
        restored.queue_status(Some(10)).unwrap()["batch_progress"]["cancelled"],
        3
    );
    restored.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_video_submits_are_one_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let (engine, mut rx, _) = setup(settings(&temp)).await;
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..20 {
        let engine = engine.clone();
        tasks.spawn(async move {
            engine.submit(&url(1), None).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        });
    }
    let mut ids = std::collections::HashSet::new();
    while let Some(result) = tasks.join_next().await {
        ids.insert(result.unwrap());
    }
    assert_eq!(ids.len(), 1);
    assert_eq!(engine.jobs().len(), 1);
    assert_eq!(started(&mut rx).await, "v0000000001");
    assert!(rx.try_recv().is_err());
    engine.cancel_all().await.unwrap();
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_preserves_fifo_queued_work_but_never_retries_interrupted_audio() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    seed(&config, 99, "completed", json!({}));
    let interrupted = seed(&config, 1, "transcribing", json!({}));
    seed(&config, 3, "queued", json!({}));
    seed(&config, 2, "queued", json!({}));
    let stale = config.read().data_dir.join("work/crash");
    std::fs::create_dir_all(&stale).unwrap();
    std::fs::write(stale.join("audio.mp3"), b"leftover").unwrap();
    let (engine, mut rx, release) = setup(config.clone()).await;
    assert!(!stale.exists());
    assert_eq!(
        engine.get_job(interrupted["id"].as_str().unwrap()).unwrap()["status"],
        "failed"
    );
    let progress = engine.queue_status(Some(10)).unwrap();
    assert_eq!(progress["total"], 4);
    assert_eq!(progress["batch_progress"]["total"], 3);
    assert_eq!(progress["batch_progress"]["completed"], 0);
    assert_eq!(started(&mut rx).await, "v0000000002");
    release.add_permits(1);
    assert_eq!(started(&mut rx).await, "v0000000003");
    release.add_permits(1);
    settle(&engine).await;
    assert!(rx.try_recv().is_err());
    assert_eq!(
        engine.get_job(&format!("{:032x}", 2)).unwrap()["legacy_field"]["preserve"],
        "yes"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn current_batch_keeps_finished_members_resets_after_settling_and_excludes_history() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let historical = seed(&config, 99, "completed", json!({}));
    let (engine, mut rx, release) = setup(config).await;
    let one = engine.submit(&url(1), None).unwrap();
    let two = engine.submit(&url(2), None).unwrap();
    let batch = one["batch_id"].clone();
    assert_eq!(two["batch_id"], batch);
    started(&mut rx).await;
    release.add_permits(1);
    started(&mut rx).await;
    let three = engine.submit(&url(3), None).unwrap();
    assert_eq!(three["batch_id"], batch);
    assert_eq!(
        engine.submit(&url(99), None).unwrap()["id"],
        historical["id"]
    );
    let status = engine.queue_status(Some(10)).unwrap();
    assert_eq!(status["total"], 4);
    assert_eq!(status["batch_progress"]["total"], 3);
    assert_eq!(status["batch_progress"]["completed"], 1);
    release.add_permits(2);
    started(&mut rx).await;
    settle(&engine).await;
    assert_eq!(
        engine.queue_status(Some(10)).unwrap()["batch_progress"]["total"],
        3
    );
    let four = engine.submit(&url(4), None).unwrap();
    assert_ne!(four["batch_id"], batch);
    assert_eq!(
        engine.queue_status(Some(10)).unwrap()["batch_progress"]["total"],
        1
    );
    engine.cancel_all().await.unwrap();
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn removed_history_remains_blocked_and_documents_keep_unknown_fields() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let job = seed(&config, 1, "completed", json!({}));
    let data = config.read().data_dir.clone();
    atomic_json(&data.join("removed-videos.json"),&json!({"version":1,"unknown":"preserve","videos":{"v0000000001":{"job_id":job["id"],"removed_at":"2026-10-23T00:00:00Z","other":true}}})).unwrap();
    let (engine, mut rx, _) = setup(config).await;
    assert_eq!(engine.submit(&url(1), None).unwrap()["status"], "removed");
    assert_eq!(
        engine.video_statuses(&["v0000000001".into()]).unwrap()["statuses"]["v0000000001"],
        json!({"status":"removed","saved":false})
    );
    assert!(
        engine
            .request("readDocument", json!({"id":job["id"],"format":"txt"}))
            .await
            .is_err()
    );
    assert!(
        engine
            .request("revealVideo", json!({"video_id":"v0000000001"}))
            .await
            .is_err()
    );
    assert_eq!(
        engine
            .export_jobs(&[job["id"].as_str().unwrap().into()], None)
            .unwrap()["skipped_removed_count"],
        1
    );
    assert!(rx.try_recv().is_err());
    assert_eq!(
        engine.get_job(job["id"].as_str().unwrap()).unwrap()["legacy_field"]["preserve"],
        "yes"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_retry_in_active_batch_counts_one_video_and_keeps_global_issues() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    seed(&config, 99, "failed", json!({"error":"Historical failure"}));
    let (engine, mut rx, _) = setup(config).await;
    let first = engine.submit(&url(1), None).unwrap();
    let other = engine.submit(&url(2), None).unwrap();
    started(&mut rx).await;
    engine.cancel_video("v0000000001").await.unwrap();
    let retry = engine.submit(&url(1), None).unwrap();
    assert_ne!(retry["id"], first["id"]);
    assert_eq!(retry["batch_id"], other["batch_id"]);
    let status = engine.queue_status(Some(10)).unwrap();
    assert_eq!(status["batch_progress"]["total"], 2);
    assert_eq!(status["batch_progress"]["cancelled"], 0);
    assert_eq!(status["batch_progress"]["attention_count"], 0);
    assert_eq!(status["attention_count"], 1);
    engine.cancel_all().await.unwrap();
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_manual_folder_detaches_pending_jobs_before_autosave() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let directory = temp.path().join("exports");
    std::fs::create_dir(&directory).unwrap();
    let library = Library::open(&config.read().data_dir).unwrap();
    library.set_directory(&directory).unwrap();
    let folder = library.create_folder("Deleted category").unwrap();
    let folder_id = folder["id"].as_str().unwrap();
    library.set_default(Some(folder_id)).unwrap();
    let job = seed(&config, 1, "queued", json!({"folder_id":folder_id}));
    std::fs::remove_dir(directory.join("Deleted category")).unwrap();
    let (engine, mut rx, release) = setup(config).await;
    assert!(engine.get_job(job["id"].as_str().unwrap()).unwrap()["folder_id"].is_null());
    started(&mut rx).await;
    release.add_permits(1);
    settle(&engine).await;
    let complete = engine.get_job(job["id"].as_str().unwrap()).unwrap();
    assert_eq!(complete["status"], "completed");
    assert!(complete["export_error"].is_null(), "{complete}");
    assert!(!directory.join("Deleted category").exists());
    assert!(PathBuf::from(complete["export_path"].as_str().unwrap()).is_file());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn corrupt_channel_folder_provenance_blocks_reconciliation_but_cleans_stale_audio() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let data = config.read().data_dir.clone();
    let library = Arc::new(Library::open(&data).unwrap());
    let folder = library.create_folder("Preserved category").unwrap();
    std::fs::write(data.join("channel-folders.json"), "{broken").unwrap();
    std::fs::create_dir_all(data.join("work/stale")).unwrap();
    std::fs::write(data.join("work/stale/audio.mp3"), b"audio").unwrap();
    let (tx, _) = mpsc::unbounded_channel();
    let result = Engine::open_with_pipeline(
        config,
        library.clone(),
        Arc::new(FakePipeline {
            started: tx,
            release: Arc::new(Semaphore::new(0)),
        }),
        true,
    )
    .await;
    assert!(result.is_err());
    assert!(!data.join("work/stale").exists());
    assert!(
        library.snapshot()["folders"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == folder["id"])
    );
}

#[tokio::test]
async fn initial_persistence_failure_never_publishes_phantom_queued_job() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let (engine, mut rx, _) = setup(config.clone()).await;
    let jobs_root = config.read().data_dir.join("jobs");
    std::fs::remove_dir(&jobs_root).unwrap();
    std::fs::write(&jobs_root, b"simulate unavailable storage").unwrap();
    assert!(engine.submit(&url(1), None).is_err());
    assert!(engine.jobs().is_empty());
    assert!(rx.try_recv().is_err());
    std::fs::remove_file(&jobs_root).unwrap();
    std::fs::create_dir(&jobs_root).unwrap();
    engine.submit(&url(1), None).unwrap();
    assert_eq!(started(&mut rx).await, "v0000000001");
    engine.cancel_all().await.unwrap();
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn restored_cancel_intent_is_persisted_and_never_executes() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let job = seed(&config, 1, "queued", json!({"cancel_requested":true}));
    let data = config.read().data_dir.clone();
    let (engine, mut rx, _) = setup(config).await;
    tokio::task::yield_now().await;
    assert!(rx.try_recv().is_err());
    assert_eq!(
        scribe_common::read_json(
            &data
                .join("jobs")
                .join(job["id"].as_str().unwrap())
                .join("job.json")
        )
        .unwrap()["status"],
        "cancelled"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn symbolic_work_directory_is_rejected_without_deleting_external_files() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let data = config.read().data_dir.clone();
    let library = Arc::new(Library::open(&data).unwrap());
    let outside = temp.path().join("user-documents");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"untouched").unwrap();
    std::os::unix::fs::symlink(&outside, data.join("work")).unwrap();
    let (tx, _) = mpsc::unbounded_channel();
    let result = Engine::open_with_pipeline(
        config,
        library,
        Arc::new(FakePipeline {
            started: tx,
            release: Arc::new(Semaphore::new(0)),
        }),
        true,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        std::fs::read(outside.join("keep.txt")).unwrap(),
        b"untouched"
    );
}

#[tokio::test]
async fn repeat_existing_completed_export_cleans_identical_copies_without_azure_or_new_batch() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let job = seed(&config, 1, "completed", json!({}));
    let directory = temp.path().join("exports");
    std::fs::create_dir(&directory).unwrap();
    let library = Library::open(&config.read().data_dir).unwrap();
    library.set_directory(&directory).unwrap();
    let (engine, mut rx, _) = setup(config).await;
    let saved = engine
        .export_jobs(&[job["id"].as_str().unwrap().into()], None)
        .unwrap();
    assert_eq!(saved["saved_count"], 1);
    let original = PathBuf::from(
        engine.get_job(job["id"].as_str().unwrap()).unwrap()["export_path"]
            .as_str()
            .unwrap(),
    );
    let copy = original.with_file_name("Copied transcript (1).txt");
    std::fs::copy(&original, &copy).unwrap();
    let reused = engine.submit(&url(1), None).unwrap();
    assert_eq!(reused["id"], job["id"]);
    assert!(!copy.exists());
    assert!(original.is_file());
    assert!(engine.queue_status(Some(10)).unwrap()["batch_progress"]["id"].is_null());
    assert!(rx.try_recv().is_err());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn restart_recovers_committed_completion_and_partial_tail_without_running_audio() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let metadata = json!({"audio_duration_seconds":2.,"duration_seconds":2.});
    let completed = seed(
        &config,
        1,
        "transcribing",
        json!({"metadata":metadata,"total_chunks":1,"completed_chunks":1}),
    );
    let cancelled = seed(
        &config,
        2,
        "transcribing",
        json!({"metadata":metadata,"total_chunks":1,"completed_chunks":1,"cancel_requested":true}),
    );
    let partial = seed(
        &config,
        3,
        "transcribing",
        json!({"metadata":metadata,"total_chunks":2,"completed_chunks":1}),
    );
    let corrupt = seed(&config, 4, "transcribing", json!({}));
    let jobs = config.read().data_dir.join("jobs");
    let final_chunk =
        json!({"index":1,"offset_ms":0.,"duration_ms":2000.,"text":"Every completed word."});
    for job in [&completed, &cancelled] {
        write_exports(
            &jobs.join(job["id"].as_str().unwrap()),
            &build_document(job, std::slice::from_ref(&final_chunk), true),
        )
        .unwrap();
    }
    let committed_before = std::fs::read(
        jobs.join(completed["id"].as_str().unwrap())
            .join("transcript.json"),
    )
    .unwrap();
    let chunk_one = json!({"index":1,"offset_ms":0.,"duration_ms":1000.,"text":"First words."});
    let chunk_two =
        json!({"index":2,"offset_ms":1000.,"duration_ms":1000.,"text":"Newest recognized tail."});
    let path = jobs.join(partial["id"].as_str().unwrap());
    atomic_json(
        &path.join("transcript.json"),
        &build_document(&partial, std::slice::from_ref(&chunk_one), false),
    )
    .unwrap();
    atomic_json(&path.join("chunks/000001.json"), &chunk_one).unwrap();
    atomic_json(&path.join("chunks/000002.json"), &chunk_two).unwrap();
    scribe_common::atomic_write(
        &jobs
            .join(corrupt["id"].as_str().unwrap())
            .join("transcript.json"),
        b"{broken",
    )
    .unwrap();
    let (engine, mut rx, _) = setup(config.clone()).await;
    assert!(
        rx.try_recv().is_err(),
        "Recovery must never start a new paid request"
    );
    assert_eq!(
        engine.get_job(completed["id"].as_str().unwrap()).unwrap()["status"],
        "completed"
    );
    assert_eq!(
        scribe_common::read_json(
            &jobs
                .join(completed["id"].as_str().unwrap())
                .join("job.json")
        )
        .unwrap()["status"],
        "completed"
    );
    assert_eq!(
        std::fs::read(
            jobs.join(completed["id"].as_str().unwrap())
                .join("transcript.json")
        )
        .unwrap(),
        committed_before
    );
    assert_eq!(
        engine.get_job(cancelled["id"].as_str().unwrap()).unwrap()["status"],
        "cancelled"
    );
    assert_eq!(
        engine.get_job(partial["id"].as_str().unwrap()).unwrap()["status"],
        "failed"
    );
    let restored = engine
        .request("readDocument", json!({"id":partial["id"],"format":"md"}))
        .await
        .unwrap();
    assert!(
        restored["content"]
            .as_str()
            .unwrap()
            .contains("Newest recognized tail.")
    );
    assert!(restored["content"].as_str().unwrap().contains("PARTIAL"));
    assert_eq!(
        engine.get_job(corrupt["id"].as_str().unwrap()).unwrap()["status"],
        "failed"
    );
    assert!(
        engine.get_job(corrupt["id"].as_str().unwrap()).unwrap()["recovery_warning"].is_string()
    );
    engine.shutdown().await.unwrap();
}

struct FailAfterCommittedText;
impl PipelineRunner for FailAfterCommittedText {
    fn run(
        &self,
        _: Settings,
        job: Value,
        update: Update,
        result: PathBuf,
        _: PathBuf,
        _: CancellationToken,
    ) -> RunFuture {
        Box::pin(async move {
            let mut job = job;
            let patch = json!({"status":"transcribing","metadata":{"audio_duration_seconds":1.,"duration_seconds":1.},"total_chunks":1,"completed_chunks":1});
            update(patch.clone())?;
            for (key, value) in patch.as_object().unwrap() {
                job[key] = value.clone();
            }
            let chunk = json!({"index":1,"offset_ms":0.,"duration_ms":1000.,"text":"Fully committed existing words."});
            write_exports(&result, &build_document(&job, &[chunk], true))?;
            Err(scribe_common::Error::internal(
                "Simulated final status callback failure",
            ))
        })
    }
}
#[tokio::test]
async fn final_state_failure_reuses_committed_text_in_process_without_a_second_job() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(&temp);
    let library = Arc::new(Library::open(&config.read().data_dir).unwrap());
    let engine =
        Engine::open_with_pipeline(config, library, Arc::new(FailAfterCommittedText), true)
            .await
            .unwrap();
    let first = engine.submit(&url(1), None).unwrap();
    settle(&engine).await;
    let recovered = engine.get_job(first["id"].as_str().unwrap()).unwrap();
    assert_eq!(recovered["status"], "completed");
    let repeated = engine.submit(&url(1), None).unwrap();
    assert_eq!(repeated["id"], first["id"]);
    assert_eq!(engine.jobs().len(), 1);
    engine.shutdown().await.unwrap();
}
