#![allow(clippy::unwrap_used)]
use parking_lot::RwLock;
use scribe_common::{Settings, build_document, render_document};
use scribe_engine::{
    Engine,
    pipeline::{PipelineRunner, RunFuture, Update},
};
use scribe_library::Library;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};
use tokio_util::sync::CancellationToken;

struct NoPipeline;
impl PipelineRunner for NoPipeline {
    fn run(
        &self,
        _: Settings,
        _: Value,
        _: Update,
        _: PathBuf,
        _: PathBuf,
        _: CancellationToken,
    ) -> RunFuture {
        Box::pin(async { panic!("Completed-history fixtures must never start work") })
    }
}
const BATCH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
fn seed(root: &Path, id: usize, video: usize, saved: bool, batch: bool, payload: &str) -> Value {
    let id = format!("{id:032x}");
    let export = root.join("exports").join(format!("{id}.txt"));
    let job = json!({"id":id,"video_id":format!("v{video:010}"),"url":format!("https://www.youtube.com/watch?v=v{video:010}"),"title":format!("Video {video}"),"status":"completed","created_at":format!("2026-01-01T00:00:{video:02}Z"),"updated_at":"2026-01-01T01:00:00Z","metadata":{"channel":"Offline fixture","original_description":payload},"document_available":true,"export_path":if saved {json!(export)}else{Value::Null},"batch_id":if batch {json!(BATCH)}else{Value::Null},"queue_order":video});
    let document = build_document(
        &job,
        &[json!({"text":format!("Video {video}. {payload}")})],
        true,
    );
    let directory = root.join("data/jobs").join(&id);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(
        directory.join("job.json"),
        serde_json::to_vec(&job).unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.join("transcript.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
    if saved {
        std::fs::create_dir_all(export.parent().unwrap()).unwrap();
        std::fs::write(export, render_document(&document, "txt").unwrap()).unwrap();
    }
    job
}
async fn open(root: &Path) -> Engine {
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(
        root.join("data/current-batch.json"),
        json!({"id":BATCH,"created_at":"2026-01-01T00:00:00Z"}).to_string(),
    )
    .unwrap();
    let settings = Arc::new(RwLock::new(
        Settings::load(root, Some(root.join("data"))).unwrap(),
    ));
    let library = Arc::new(Library::open(root.join("data")).unwrap());
    Engine::open_with_pipeline(settings, library, Arc::new(NoPipeline), true)
        .await
        .unwrap()
}

#[tokio::test]
async fn queue_verification_is_fresh_for_edits_symlinks_and_missing_internal_documents() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let job = seed(&root, 1, 1, true, true, "All spoken words.");
    let exported = PathBuf::from(job["export_path"].as_str().unwrap());
    let bytes = std::fs::read(&exported).unwrap();
    let engine = open(&root).await;
    let status = engine.queue_status(None).unwrap();
    assert_eq!(status["saved"], 1);
    assert_eq!(status["batch_progress"]["saved"], 1);
    std::fs::write(&exported, vec![b'x'; bytes.len()]).unwrap();
    let status = engine.queue_status(None).unwrap();
    assert_eq!(status["saved"], 0);
    assert_eq!(status["unsaved"], 1);
    assert_eq!(status["batch_progress"]["saved"], 0);
    std::fs::write(&exported, &bytes).unwrap();
    assert_eq!(engine.queue_status(None).unwrap()["saved"], 1);
    #[cfg(unix)]
    {
        let outside = root.join("outside.txt");
        std::fs::write(&outside, &bytes).unwrap();
        std::fs::remove_file(&exported).unwrap();
        std::os::unix::fs::symlink(&outside, &exported).unwrap();
        assert_eq!(engine.queue_status(None).unwrap()["saved"], 0);
        std::fs::remove_file(&exported).unwrap();
        std::fs::write(&exported, &bytes).unwrap();
    }
    std::fs::remove_file(
        root.join("data/jobs")
            .join(job["id"].as_str().unwrap())
            .join("transcript.json"),
    )
    .unwrap();
    let status = engine.queue_status(None).unwrap();
    assert_eq!(status["saved"], 0);
    assert_eq!(status["missing"], 1);
    assert_eq!(status["batch_progress"]["missing"], 1);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn batch_reuses_saved_same_video_attempt_outside_batch_without_counting_other_videos() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let old = seed(&root, 1, 1, true, false, "Original text.");
    seed(&root, 2, 1, false, true, "New attempt text.");
    seed(&root, 3, 2, true, false, "Another video.");
    seed(&root, 4, 3, false, true, "Unsaved video.");
    let engine = open(&root).await;
    let status = engine.queue_status(None).unwrap();
    assert_eq!(status["total"], 3);
    assert_eq!(status["saved"], 2);
    assert_eq!(status["unsaved"], 1);
    assert_eq!(status["batch_progress"]["total"], 2);
    assert_eq!(status["batch_progress"]["saved"], 1);
    assert_eq!(status["batch_progress"]["unsaved"], 1);
    std::fs::remove_file(old["export_path"].as_str().unwrap()).unwrap();
    let status = engine.queue_status(None).unwrap();
    assert_eq!(status["saved"], 1);
    assert_eq!(status["batch_progress"]["saved"], 0);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "isolated synthetic 482-job performance measurement, no network"]
async fn completed_history_benchmark() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let payload = "A complete spoken sentence with original words. ".repeat(400);
    for n in 1..=482 {
        seed(&root, n, n, n <= 400, true, &payload);
    }
    let start = Instant::now();
    let engine = open(&root).await;
    let startup = start.elapsed().as_secs_f64() * 1000.;
    let mut samples = Vec::new();
    for _ in 0..6 {
        let start = Instant::now();
        let status = engine.queue_status(Some(10)).unwrap();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
        assert_eq!(status["total"], 482);
        assert_eq!(status["saved"], 400);
        assert_eq!(status["batch_progress"]["unsaved"], 82);
    }
    eprintln!(
        "{}",
        json!({"fixture":"482 synthetic completed jobs, 400 saved, 82 unsaved, same current batch","startup_ms":startup,"queue_ms":samples})
    );
    engine.shutdown().await.unwrap();
}
