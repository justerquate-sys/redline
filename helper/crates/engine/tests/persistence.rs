#![allow(clippy::unwrap_used)]
use parking_lot::RwLock;
use scribe_common::{Settings, SharedSettings, build_document, read_json, write_exports};
use scribe_engine::{
    Engine,
    pipeline::{PipelineRunner, RunFuture, Update},
};
use scribe_library::Library;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

fn settings(root: &Path) -> SharedSettings {
    let root = root.canonicalize().unwrap();
    Arc::new(RwLock::new(Settings {
        root: root.clone(),
        data_dir: root.join("data"),
        speech_key: "offline-only".into(),
        speech_endpoint: "https://offline.cognitiveservices.azure.com".into(),
        helper_token: "offline-token".into(),
        ffmpeg_location: String::new(),
        cookies_browser: String::new(),
        js_runtime: String::new(),
        chunk_seconds: 600,
        audio_speed: 1.25,
    }))
}
fn url(index: usize) -> String {
    format!("https://www.youtube.com/watch?v=v{index:010}")
}
fn obstruct(directory: &Path) {
    std::fs::rename(
        directory.join("job.json"),
        directory.join("job.before.json"),
    )
    .unwrap();
    std::fs::create_dir(directory.join("job.json")).unwrap();
}
fn repair(directory: &Path) {
    std::fs::remove_dir(directory.join("job.json")).unwrap();
    std::fs::rename(
        directory.join("job.before.json"),
        directory.join("job.json"),
    )
    .unwrap();
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
fn recognized(
    job: &mut Value,
    update: &Update,
    directory: &Path,
    complete: bool,
) -> scribe_common::Result<()> {
    let patch = json!({"status":"transcribing","metadata":{"audio_duration_seconds":1.,"duration_seconds":1.},"total_chunks":1,"completed_chunks":1});
    update(patch.clone())?;
    for (key, value) in patch.as_object().unwrap() {
        job[key] = value.clone();
    }
    let chunk = json!({"index":1,"offset_ms":0.,"duration_ms":1000.,"text":"Recognized offline words, preserved once."});
    write_exports(directory, &build_document(job, &[chunk], complete))
}
struct FaultPipeline {
    calls: Arc<AtomicUsize>,
    complete: bool,
}
impl PipelineRunner for FaultPipeline {
    fn run(
        &self,
        _: Settings,
        mut job: Value,
        update: Update,
        directory: PathBuf,
        _: PathBuf,
        _: CancellationToken,
    ) -> RunFuture {
        let calls = self.calls.clone();
        let complete = self.complete;
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            recognized(&mut job, &update, &directory, complete)?;
            if job["video_id"] == "v0000000001" {
                obstruct(&directory);
            }
            if complete {
                update(json!({"status":"completed","progress":100,"document_available":true}))?;
                Ok(())
            } else {
                Err(scribe_common::Error::internal(
                    "Synthetic partial processing failure",
                ))
            }
        })
    }
}

#[tokio::test]
async fn persistent_terminal_write_failure_preserves_completion_allows_other_accepted_jobs_and_repairs_without_replay()
 {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(temp.path());
    let library = Arc::new(Library::open(&config.read().data_dir).unwrap());
    let output = temp.path().join("saved");
    std::fs::create_dir(&output).unwrap();
    library.set_directory(&output).unwrap();
    library.set_export_settings(true, "txt").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = Engine::open_with_pipeline(
        config.clone(),
        library.clone(),
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: true,
        }),
        true,
    )
    .await
    .unwrap();
    let first = engine.submit(&url(1), None).unwrap();
    let second = engine.submit(&url(2), None).unwrap();
    let id = first["id"].as_str().unwrap();
    until(|| {
        engine.get_job(id).unwrap()["persistence_pending"] == true
            && engine.get_job(second["id"].as_str().unwrap()).unwrap()["status"] == "completed"
    })
    .await;
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "the unrelated accepted job must continue"
    );
    assert_eq!(engine.get_job(id).unwrap()["status"], "completed");
    assert!(
        engine.has_active_work(),
        "unpersisted terminal state must block installer/settings idle guards"
    );
    let status = engine.queue_status(None).unwrap();
    assert_eq!(status["processing"], 0);
    assert_eq!(status["persistence_pending"], 1);
    assert_eq!(status["finished"], false);
    assert_eq!(status["batch_progress"]["persistence_pending"], 1);
    assert!(
        status["attention"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["id"] == first["id"] && item["kind"] == "persistence")
    );
    assert_eq!(engine.submit(&url(1), None).unwrap()["id"], first["id"]);
    assert_eq!(engine.submit(&url(3), None).unwrap_err().status, 503);
    let text = engine
        .request("readDocument", json!({"id":id,"format":"txt"}))
        .await
        .unwrap();
    assert!(
        text["content"]
            .as_str()
            .unwrap()
            .contains("Recognized offline words")
    );
    assert!(
        !library.is_saved_anywhere(
            &engine.get_job(id).unwrap(),
            &config.read().data_dir.join("jobs")
        ),
        "autosave must wait for status repair"
    );
    // Leave the obstruction in place across more than one backoff attempt.
    tokio::time::sleep(Duration::from_millis(850)).await;
    assert_eq!(engine.get_job(id).unwrap()["persistence_pending"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let directory = config.read().data_dir.join("jobs").join(id);
    repair(&directory);
    until(|| !engine.has_active_work()).await;
    let repaired = engine.get_job(id).unwrap();
    assert_eq!(repaired["status"], "completed");
    assert!(repaired.get("persistence_pending").is_none());
    assert_eq!(
        read_json(&directory.join("job.json")).unwrap()["status"],
        "completed"
    );
    assert!(
        library.is_saved_anywhere(&repaired, &config.read().data_dir.join("jobs")),
        "pending={} error={} path={}",
        repaired["export_pending"],
        repaired["export_error"],
        repaired["export_path"]
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        2,
        "repair/autosave must not call the pipeline"
    );
    assert!(
        engine
            .queue_status(None)
            .unwrap()
            .get("persistence_pending")
            .is_none()
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn partial_terminal_write_failure_is_truthful_and_shutdown_refuses_to_claim_durable_state() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(temp.path());
    let library = Arc::new(Library::open(&config.read().data_dir).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = Engine::open_with_pipeline(
        config.clone(),
        library,
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: false,
        }),
        true,
    )
    .await
    .unwrap();
    let first = engine.submit(&url(1), None).unwrap();
    let id = first["id"].as_str().unwrap();
    until(|| engine.get_job(id).unwrap()["persistence_pending"] == true).await;
    let job = engine.get_job(id).unwrap();
    assert_eq!(job["status"], "failed");
    assert!(job["error"].as_str().unwrap().contains("Synthetic partial"));
    assert_eq!(
        engine.queue_status(None).unwrap()["attention_count"],
        1,
        "persistence replaces, rather than duplicates, this job's existing issue"
    );
    assert_eq!(engine.submit(&url(1), None).unwrap_err().status, 503);
    assert_eq!(
        read_json(
            &config
                .read()
                .data_dir
                .join("jobs")
                .join(id)
                .join("transcript.json")
        )
        .unwrap()["complete"],
        false
    );
    assert!(engine.shutdown().await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct CancellationRacePipeline {
    ready: mpsc::UnboundedSender<(Update, PathBuf)>,
    release: Arc<Semaphore>,
}
impl PipelineRunner for CancellationRacePipeline {
    fn run(
        &self,
        _: Settings,
        mut job: Value,
        update: Update,
        directory: PathBuf,
        _: PathBuf,
        _: CancellationToken,
    ) -> RunFuture {
        let ready = self.ready.clone();
        let release = self.release.clone();
        Box::pin(async move {
            recognized(&mut job, &update, &directory, false)?;
            ready.send((update, directory)).unwrap();
            release.acquire().await.unwrap().forget();
            Err(scribe_common::Error::internal("Late processing error"))
        })
    }
}
#[tokio::test]
async fn stale_terminal_patch_cannot_erase_a_newer_cancel_intent_even_when_status_storage_fails() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(temp.path());
    let library = Arc::new(Library::open(&config.read().data_dir).unwrap());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let engine = Engine::open_with_pipeline(
        config.clone(),
        library,
        Arc::new(CancellationRacePipeline {
            ready: tx,
            release: release.clone(),
        }),
        true,
    )
    .await
    .unwrap();
    let first = engine.submit(&url(1), None).unwrap();
    let id = first["id"].as_str().unwrap();
    let (stale_update, directory) = rx.recv().await.unwrap();
    let old_terminal_patch =
        json!({"status":"failed","cancel_requested":null,"error":"Stale worker result"});
    let cancelling = engine.clone();
    let cancel = tokio::spawn(async move { cancelling.cancel_video("v0000000001").await });
    until(|| engine.get_job(id).unwrap()["cancel_requested"] == true).await;
    obstruct(&directory);
    assert!(stale_update(old_terminal_patch).is_err());
    assert_eq!(engine.get_job(id).unwrap()["status"], "cancelled");
    assert_eq!(engine.get_job(id).unwrap()["cancel_requested"], true);
    release.add_permits(1);
    assert_eq!(cancel.await.unwrap().unwrap()["status"], "cancelled");
    assert_eq!(engine.get_job(id).unwrap()["status"], "cancelled");
    repair(&directory);
    until(|| !engine.has_active_work()).await;
    let disk = read_json(&directory.join("job.json")).unwrap();
    assert_eq!(disk["status"], "cancelled");
    assert_eq!(disk["cancel_requested"], true);
    assert_eq!(engine.queue_status(None).unwrap()["attention_count"], 0);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn queued_cancellation_journal_survives_an_unwritable_job_and_restart_without_pipeline_work()
{
    let temp = tempfile::tempdir().unwrap();
    let config = settings(temp.path());
    let library = Arc::new(Library::open(&config.read().data_dir).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = Engine::open_with_pipeline(
        config.clone(),
        library.clone(),
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: true,
        }),
        true,
    )
    .await
    .unwrap();
    // No yield before cancellation: the durable original is still queued.
    let first = engine.submit(&url(1), None).unwrap();
    let id = first["id"].as_str().unwrap();
    let directory = config.read().data_dir.join("jobs").join(id);
    obstruct(&directory);
    assert!(engine.cancel_all().await.is_err());
    assert_eq!(engine.get_job(id).unwrap()["status"], "cancelled");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let journal = read_json(&config.read().data_dir.join("status-repairs.json")).unwrap();
    assert_eq!(journal["jobs"][id]["status"], "cancelled");
    assert_eq!(journal["jobs"][id]["cancel_requested"], true);
    assert!(engine.shutdown().await.is_err());
    repair(&directory);
    assert_eq!(
        read_json(&directory.join("job.json")).unwrap()["status"],
        "queued"
    );
    let reopened = Engine::open_with_pipeline(
        config.clone(),
        library,
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: true,
        }),
        true,
    )
    .await
    .unwrap();
    until(|| !reopened.has_active_work()).await;
    assert_eq!(reopened.get_job(id).unwrap()["status"], "cancelled");
    assert_eq!(
        read_json(&directory.join("job.json")).unwrap()["cancel_requested"],
        true
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "journal replay must precede queued recovery"
    );
    assert_eq!(
        read_json(&config.read().data_dir.join("status-repairs.json")).unwrap()["jobs"],
        json!({})
    );
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_applied_journal_preserves_newer_metadata_and_malformed_journal_blocks_startup() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(temp.path());
    let library = Arc::new(Library::open(&config.read().data_dir).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = Engine::open_with_pipeline(
        config.clone(),
        library.clone(),
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: false,
        }),
        true,
    )
    .await
    .unwrap();
    let first = engine.submit(&url(1), None).unwrap();
    let id = first["id"].as_str().unwrap();
    until(|| engine.get_job(id).unwrap()["persistence_pending"] == true).await;
    let path = config.read().data_dir.join("status-repairs.json");
    let stale = read_json(&path).unwrap();
    let directory = config.read().data_dir.join("jobs").join(id);
    repair(&directory);
    until(|| !engine.has_active_work()).await;
    engine.shutdown().await.unwrap();
    let mut later = read_json(&directory.join("job.json")).unwrap();
    later["unknown_later_metadata"] = json!("preserve");
    scribe_common::atomic_json(&directory.join("job.json"), &later).unwrap();
    scribe_common::atomic_json(&path, &stale).unwrap();
    let reopened = Engine::open_with_pipeline(
        config.clone(),
        library.clone(),
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: false,
        }),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.get_job(id).unwrap()["unknown_later_metadata"],
        "preserve"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    reopened.shutdown().await.unwrap();
    // A later independently repaired episode has a larger generation. An old
    // journal whose clear failed must not overwrite its metadata or paths.
    let mut newer = read_json(&directory.join("job.json")).unwrap();
    newer["persistence_generation"] = json!(newer["persistence_generation"].as_u64().unwrap() + 1);
    newer["persistence_revision"] = json!("cccccccccccccccccccccccccccccccc");
    newer["export_path"] = json!(temp.path().join("newer-export.txt"));
    scribe_common::atomic_json(&directory.join("job.json"), &newer).unwrap();
    scribe_common::atomic_json(&path, &stale).unwrap();
    let reopened = Engine::open_with_pipeline(
        config.clone(),
        library.clone(),
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: false,
        }),
        true,
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.get_job(id).unwrap()["export_path"],
        newer["export_path"]
    );
    assert_eq!(
        reopened.get_job(id).unwrap()["persistence_revision"],
        newer["persistence_revision"]
    );
    reopened.shutdown().await.unwrap();
    // A source mismatch and an unreadable regular record are conflicts, not
    // permission to overwrite that record from a journal.
    let mut conflicting = newer.clone();
    conflicting["video_id"] = json!("v0000000002");
    conflicting["url"] = json!(url(2));
    scribe_common::atomic_json(&directory.join("job.json"), &conflicting).unwrap();
    scribe_common::atomic_json(&path, &stale).unwrap();
    assert!(
        Engine::open_with_pipeline(
            config.clone(),
            library.clone(),
            Arc::new(FaultPipeline {
                calls: calls.clone(),
                complete: false
            }),
            true
        )
        .await
        .is_err()
    );
    assert_eq!(read_json(&directory.join("job.json")).unwrap(), conflicting);
    std::fs::write(directory.join("job.json"), "malformed existing job").unwrap();
    assert!(
        Engine::open_with_pipeline(
            config.clone(),
            library.clone(),
            Arc::new(FaultPipeline {
                calls: calls.clone(),
                complete: false
            }),
            true
        )
        .await
        .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(directory.join("job.json")).unwrap(),
        "malformed existing job"
    );
    scribe_common::atomic_json(&directory.join("job.json"), &newer).unwrap();
    std::fs::write(&path, "not valid recovery JSON").unwrap();
    assert!(
        Engine::open_with_pipeline(
            config,
            library,
            Arc::new(FaultPipeline {
                calls: calls.clone(),
                complete: false
            }),
            true
        )
        .await
        .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn journal_recovers_independently_while_the_original_job_path_remains_unwritable() {
    let temp = tempfile::tempdir().unwrap();
    let config = settings(temp.path());
    let library = Arc::new(Library::open(&config.read().data_dir).unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = Engine::open_with_pipeline(
        config.clone(),
        library.clone(),
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: true,
        }),
        true,
    )
    .await
    .unwrap();
    let first = engine.submit(&url(1), None).unwrap();
    let id = first["id"].as_str().unwrap();
    let directory = config.read().data_dir.join("jobs").join(id);
    obstruct(&directory);
    let journal = config.read().data_dir.join("status-repairs.json");
    std::fs::create_dir(&journal).unwrap();
    assert!(engine.cancel_all().await.is_err());
    assert_eq!(engine.get_job(id).unwrap()["persistence_pending"], true);
    std::fs::remove_dir(&journal).unwrap();
    until(|| journal.is_file()).await;
    assert_eq!(
        read_json(&journal).unwrap()["jobs"][id]["status"],
        "cancelled"
    );
    assert!(directory.join("job.json").is_dir());
    assert_eq!(engine.get_job(id).unwrap()["persistence_pending"], true);
    assert!(engine.shutdown().await.is_err());
    repair(&directory);
    let reopened = Engine::open_with_pipeline(
        config,
        library,
        Arc::new(FaultPipeline {
            calls: calls.clone(),
            complete: true,
        }),
        true,
    )
    .await
    .unwrap();
    assert_eq!(reopened.get_job(id).unwrap()["status"], "cancelled");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    reopened.shutdown().await.unwrap();
}
