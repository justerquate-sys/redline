//! Invoked only by scripts/benchmark_pipeline_parity.py with a private fixture.
#![allow(clippy::unwrap_used)]
use parking_lot::Mutex;
use scribe_common::{Settings, read_json};
use scribe_engine::pipeline::{Update, run};
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Instant};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "Developer-only matched Python/Rust benchmark. Requires SCRIBE_FAIR_BENCH_CONFIG."]
async fn matched_offline_pipeline_round() {
    let path =
        std::env::var_os("SCRIBE_FAIR_BENCH_CONFIG").expect("Private benchmark fixture path");
    let fixture = read_json(&PathBuf::from(path)).unwrap();
    let endpoint = fixture["endpoint"].as_str().unwrap();
    let parsed = url::Url::parse(endpoint).unwrap();
    assert_eq!(parsed.scheme(), "http");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert!(parsed.port().is_some());
    assert_eq!(parsed.path(), "/");
    assert!(parsed.query().is_none());
    assert!(parsed.fragment().is_none());
    assert!(parsed.username().is_empty());
    assert!(parsed.password().is_none());
    let root = PathBuf::from(fixture["fixture_root"].as_str().unwrap())
        .canonicalize()
        .unwrap();
    let round = PathBuf::from(fixture["round_root"].as_str().unwrap())
        .canonicalize()
        .unwrap();
    assert!(round.starts_with(root.parent().unwrap()));
    assert!(root.join(".offline-pipeline-fixture").is_file());
    let settings = Settings {
        root,
        data_dir: round.join("data"),
        speech_key: "offline-fair-benchmark".into(),
        speech_endpoint: endpoint.into(),
        helper_token: "offline-fair-benchmark-token".into(),
        ffmpeg_location: fixture["ffmpeg_dir"].as_str().unwrap().into(),
        cookies_browser: String::new(),
        js_runtime: "node:/offline-fixture-not-executed".into(),
        chunk_seconds: fixture["chunk_seconds"]
            .as_u64()
            .unwrap()
            .try_into()
            .unwrap(),
        audio_speed: fixture["audio_speed"].as_f64().unwrap(),
    };
    let job = json!({"id":"ffffffffffffffffffffffffffffffff","video_id":"v0000000001","url":"https://www.youtube.com/watch?v=v0000000001","title":"Matched offline synthetic audio","metadata":{},"created_at":scribe_common::now()});
    let latest = Arc::new(Mutex::new(job.clone()));
    let sink = latest.clone();
    let update: Update = Arc::new(move |patch| {
        let mut job = sink.lock();
        for (key, value) in patch.as_object().unwrap() {
            job[key] = value.clone();
        }
        Ok(())
    });
    let start = Instant::now();
    run(
        settings,
        job,
        update,
        round.join("result"),
        round.join("work"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let wall = start.elapsed().as_secs_f64();
    let document = read_json(&round.join("result/transcript.json")).unwrap();
    let expected = fixture["expected_frames"].as_u64().unwrap();
    let speed = fixture["audio_speed"].as_f64().unwrap();
    let chunks = document["chunks"].as_array().unwrap();
    assert_eq!(document["complete"], true);
    assert_eq!(
        chunks.len() as u64,
        fixture["expected_requests"].as_u64().unwrap()
    );
    let mut previous = 0.;
    for chunk in chunks {
        assert!((chunk["offset_ms"].as_f64().unwrap() - previous).abs() < 0.001);
        previous += chunk["duration_ms"].as_f64().unwrap();
    }
    assert!((previous - expected as f64 / 16000. * 1000. * speed).abs() < 0.001);
    assert_eq!(std::fs::read_dir(round.join("work")).unwrap().count(), 0);
    let latest = latest.lock().clone();
    assert_eq!(latest["status"], "completed");
    println!(
        "SCRIBE_FAIR_ROUND={}",
        json!({"implementation":"rust","version":scribe_common::VERSION,"wall_seconds":wall,"performance":latest["performance"],"complete":true,"completed_chunks":chunks.len(),"source_coverage_ms":previous,"temporary_entries_remaining":0})
    );
}
