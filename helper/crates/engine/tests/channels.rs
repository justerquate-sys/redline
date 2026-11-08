#![allow(clippy::unwrap_used)]
mod support;
use parking_lot::RwLock;
use scribe_common::{Settings, SharedSettings, atomic_json};
use scribe_engine::{
    Engine,
    pipeline::{PipelineRunner, RunFuture, Update},
};
use scribe_library::Library;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
struct Offline;
impl PipelineRunner for Offline {
    fn run(
        &self,
        _: Settings,
        _: Value,
        update: Update,
        _: PathBuf,
        _: PathBuf,
        cancel: CancellationToken,
    ) -> RunFuture {
        Box::pin(async move {
            update(json!({"status":"transcribing"}))?;
            cancel.cancelled().await;
            Err(scribe_engine::process::cancelled())
        })
    }
}
const CHANNEL: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";
fn entry(n: usize) -> Value {
    json!({"id":format!("v{n:010}"),"title":format!("Offline {n}"),"playlist_channel_id":CHANNEL,"playlist_channel":"Offline / creator","channel_id":CHANNEL,"duration":30})
}
fn tab(root: &std::path::Path, name: &str, ids: &[usize]) {
    std::fs::write(
        root.join(format!("tabs/{name}.jsonl")),
        ids.iter()
            .map(|n| format!("{}\n", entry(*n)))
            .collect::<String>(),
    )
    .unwrap();
}
async fn setup(root: &std::path::Path) -> Engine {
    let settings: SharedSettings = Arc::new(RwLock::new(support::settings(root)));
    let library = Arc::new(Library::open(root.join("data")).unwrap());
    Engine::open_with_pipeline(settings, library, Arc::new(Offline), true)
        .await
        .unwrap()
}
async fn finished(engine: &Engine, id: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let v = engine
                .request("getChannelPreview", json!({"id":id}))
                .await
                .unwrap();
            if !["waiting", "scanning"].contains(&v["status"].as_str().unwrap()) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
async fn start(engine: &Engine, scope: &str, shorts: bool, auto: bool, handle: &str) -> Value {
    engine.request(if auto{"startChannelImport"}else{"startChannelPreview"},json!({"url":format!("https://www.youtube.com/@{handle}"),"scope":scope,"include_shorts":shorts})).await.unwrap()
}

#[tokio::test]
async fn all_imports_queue_incrementally_without_metadata_and_shorts_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    support::downloader(root.path());
    tab(root.path(), "videos", &[1, 2]);
    tab(root.path(), "streams", &[2, 3]);
    tab(root.path(), "shorts", &[4]);
    let engine = setup(root.path()).await;
    let one = start(&engine, "all", false, true, "offline").await;
    let p = finished(&engine, one["id"].as_str().unwrap()).await;
    assert_eq!(p["status"], "ready", "{p}");
    assert_eq!(p["queued_count"], 3);
    assert_eq!(engine.jobs().len(), 3);
    assert_eq!(p["include_shorts"], false);
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    assert!(!calls.contains("metadata"));
    assert!(!calls.contains("/shorts"));
    let same = start(&engine, "all", false, true, "offline").await;
    assert_eq!(same["id"], p["id"]);
    let two = start(&engine, "all", true, true, "offline").await;
    let q = finished(&engine, two["id"].as_str().unwrap()).await;
    assert_eq!(q["queued_count"], 1);
    assert_eq!(q["existing_count"], 3);
    assert_eq!(engine.jobs().len(), 4);
    assert_ne!(p["id"], q["id"]);
    engine.cancel_all().await.unwrap();
    engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn year_cutoff_is_verified_and_stops_each_tab_independently() {
    let root = tempfile::tempdir().unwrap();
    support::downloader(root.path());
    tab(root.path(), "videos", &[1, 2, 3, 4]);
    tab(root.path(), "streams", &[5, 6, 7, 8]);
    tab(root.path(), "shorts", &[9]);
    let today = chrono::Utc::now().date_naive().format("%Y%m%d").to_string();
    for n in 1..=9 {
        let mut info = entry(n);
        info["upload_date"] = json!(if [1, 5, 9].contains(&n) {
            today.as_str()
        } else {
            "20000101"
        });
        atomic_json(&root.path().join(format!("metadata/v{n:010}.json")), &info).unwrap();
    }
    let engine = setup(root.path()).await;
    let start = start(&engine, "year", false, true, "offline").await;
    let p = finished(&engine, start["id"].as_str().unwrap()).await;
    assert_eq!(p["status"], "ready", "{p}");
    assert_eq!(p["queued_count"], 2);
    assert_eq!(p["skipped_old"], 4);
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    assert!(!calls.contains("v0000000004"));
    assert!(!calls.contains("v0000000008"));
    assert!(!calls.contains("/shorts"));
    assert!(calls.contains("v0000000005"));
    engine.cancel_all().await.unwrap();
    engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn preview_is_metadata_only_until_explicit_queue_and_cancelled_videos_are_not_readded() {
    let root = tempfile::tempdir().unwrap();
    support::downloader(root.path());
    tab(root.path(), "videos", &[1]);
    atomic_json(&root.path().join("metadata/v0000000001.json"), &entry(1)).unwrap();
    let engine = setup(root.path()).await;
    let preview = start(&engine, "all", false, false, "offline").await;
    let p = finished(&engine, preview["id"].as_str().unwrap()).await;
    assert_eq!(p["status"], "ready");
    assert!(engine.jobs().is_empty());
    engine
        .request("queueChannelPreview", json!({"id":p["id"]}))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.jobs().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    engine.cancel_video("v0000000001").await.unwrap();
    assert_eq!(engine.jobs().len(), 1);
    assert_eq!(engine.jobs()[0]["status"], "cancelled");
    engine.cancel_all().await.unwrap();
    engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn multiple_scans_have_two_slots_and_waiting_cancel_never_launches_downloader() {
    let root = tempfile::tempdir().unwrap();
    support::downloader(root.path());
    std::fs::write(root.path().join("block-streams"), "").unwrap();
    let engine = setup(root.path()).await;
    let a = start(&engine, "all", false, true, "one").await;
    let b = start(&engine, "all", false, true, "two").await;
    let c = start(&engine, "all", false, true, "three").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(root.path().join("stream-started"))
                .unwrap_or_default()
                .lines()
                .count()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let wait = engine
        .request("getChannelPreview", json!({"id":c["id"]}))
        .await
        .unwrap();
    assert_eq!(wait["status"], "waiting");
    assert_eq!(
        engine.queue_status(Some(10)).unwrap()["batch_progress"]["total"],
        0
    );
    assert_eq!(
        engine.queue_status(Some(10)).unwrap()["batch_progress"]["finished"],
        false
    );
    engine
        .request("cancelChannelPreview", json!({"id":c["id"]}))
        .await
        .unwrap();
    let stopped = engine.cancel_all().await.unwrap();
    assert_eq!(stopped["cancelled_channels"], 2);
    assert_eq!(
        std::fs::read_to_string(root.path().join("stream-started"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    for id in [a["id"].clone(), b["id"].clone(), c["id"].clone()] {
        assert_eq!(
            engine
                .request("getChannelPreview", json!({"id":id}))
                .await
                .unwrap()["status"],
            "cancelled"
        );
    }
    assert!(engine.jobs().is_empty());
    engine.shutdown().await.unwrap();
    let reopened = setup(root.path()).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        std::fs::read_to_string(root.path().join("stream-started"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    reopened.shutdown().await.unwrap();
}
#[tokio::test]
async fn resumed_import_recovers_failed_job_origin_without_second_attempt() {
    let root = tempfile::tempdir().unwrap();
    support::downloader(root.path());
    tab(root.path(), "videos", &[1]);
    let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let job_id = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    atomic_json(&root.path().join(format!("data/jobs/{job_id}/job.json")),&json!({"id":job_id,"video_id":"v0000000001","url":"https://www.youtube.com/watch?v=v0000000001","status":"failed","title":"Original","channel_import_ids":["other",id],"metadata":{},"created_at":"2026-10-22T00:00:00Z"})).unwrap();
    atomic_json(&root.path().join(format!("data/channel-previews/{id}.json")),&json!({"id":id,"channel_url":"https://www.youtube.com/@offline","scope":"all","as_of_date":chrono::Utc::now().date_naive().to_string(),"status":"scanning","auto_queue":true,"include_shorts":false,"queue_state":null,"import_job_ids":{}})).unwrap();
    let engine = setup(root.path()).await;
    let p = finished(&engine, id).await;
    assert_eq!(p["status"], "ready", "{p}");
    assert_eq!(engine.jobs().len(), 1);
    assert_eq!(engine.jobs()[0]["status"], "failed");
    assert_eq!(p["existing_count"], 1);
    assert_eq!(p["queued_count"], 0);
    engine.shutdown().await.unwrap();
}
