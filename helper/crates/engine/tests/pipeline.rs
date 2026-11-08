#![allow(clippy::unwrap_used)]
mod support;
use parking_lot::Mutex;
use scribe_common::{
    Settings, atomic_json, build_document, read_json, render_document, write_exports,
};
use scribe_engine::pipeline::{
    Update, completed_checkpoint, probe_duration, restore_partial_exports, run, wav_header,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum Mode {
    Success,
    AuthSecond,
    ShortSecond,
    RetryFirst,
    Hold,
}
struct Server {
    url: String,
    requests: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    payloads: Arc<Mutex<Vec<Vec<u8>>>>,
    multipart: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    async fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let connections = Arc::new(AtomicUsize::new(0));
        let payloads = Arc::new(Mutex::new(vec![]));
        let multipart = Arc::new(Mutex::new(vec![]));
        let stop = CancellationToken::new();
        let token = stop.clone();
        let count = requests.clone();
        let connects = connections.clone();
        let bodies = payloads.clone();
        let complete_bodies = multipart.clone();
        let task = tokio::spawn(async move {
            let mut children = tokio::task::JoinSet::new();
            loop {
                tokio::select! {biased;_=token.cancelled()=>break,accepted=listener.accept()=>{let(stream,_)=accepted.unwrap();connects.fetch_add(1,Ordering::SeqCst);let token=token.clone();let count=count.clone();let bodies=bodies.clone();let multipart=complete_bodies.clone();children.spawn(async move{tokio::select!{_=token.cancelled()=>{},_=serve(stream,mode,count,bodies,multipart)=>{}}});}}
            }
            children.abort_all();
            while children.join_next().await.is_some() {}
        });
        Self {
            url,
            requests,
            connections,
            payloads,
            multipart,
            stop,
            task,
        }
    }
    async fn close(self) {
        self.stop.cancel();
        self.task.await.unwrap();
    }
}
async fn serve(
    mut stream: TcpStream,
    mode: Mode,
    count: Arc<AtomicUsize>,
    payloads: Arc<Mutex<Vec<Vec<u8>>>>,
    multipart: Arc<Mutex<Vec<Vec<u8>>>>,
) {
    let mut buffered = vec![];
    loop {
        let split = loop {
            if let Some(i) = buffered.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            let mut chunk = [0u8; 8192];
            let Ok(n) = stream.read(&mut chunk).await else {
                return;
            };
            if n == 0 {
                return;
            }
            buffered.extend(&chunk[..n]);
        };
        let headers = String::from_utf8_lossy(&buffered[..split]);
        assert!(
            headers.starts_with(
                "POST /speechtotext/transcriptions:transcribe?api-version=2025-10-15 "
            )
        );
        assert!(
            headers
                .to_lowercase()
                .contains("ocp-apim-subscription-key: offline-test-key")
        );
        let len: usize = headers
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                    .map(|(_, v)| v.trim().parse().unwrap())
            })
            .unwrap();
        while buffered.len() < split + len {
            let mut chunk = [0u8; 8192];
            let Ok(n) = stream.read(&mut chunk).await else {
                return;
            };
            if n == 0 {
                return;
            }
            buffered.extend(&chunk[..n]);
        }
        let body = buffered[split..split + len].to_vec();
        multipart.lock().push(body.clone());
        buffered.drain(..split + len);
        let audio_at = body.windows(4).position(|w| w == b"RIFF").unwrap();
        let bytes =
            u32::from_le_bytes(body[audio_at + 40..audio_at + 44].try_into().unwrap()) as usize;
        let audio = body[audio_at..audio_at + 44 + bytes].to_vec();
        let definition = String::from_utf8_lossy(&body[audio_at + 44 + bytes..]);
        assert!(definition.contains("MAI-Transcribe-2"));
        assert!(definition.contains("verbatim"));
        let duration = bytes as f64 / 32.;
        payloads.lock().push(audio);
        let index = count.fetch_add(1, Ordering::SeqCst) + 1;
        if matches!(mode, Mode::Hold) {
            std::future::pending::<()>().await;
        }
        let status = if matches!(mode, Mode::AuthSecond) && index == 2 {
            401
        } else if matches!(mode, Mode::RetryFirst) && index == 1 {
            429
        } else {
            200
        };
        let response=if status!=200{json!({"private":"must never escape service error"})}else{let processed=if matches!(mode,Mode::ShortSecond)&&index==2{duration/2.}else{duration};json!({"durationMilliseconds":processed,"combinedPhrases":[{"text":format!("Full words from chunk {index}. 日本語") }],"phrases":[{"text":format!("segment {index}"),"offsetMilliseconds":100,"durationMilliseconds":duration-100.}]})}.to_string();
        let header = format!(
            "HTTP/1.1 {status} Reply\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\nRetry-After: 1\r\n\r\n",
            response.len()
        );
        if stream.write_all(header.as_bytes()).await.is_err()
            || stream.write_all(response.as_bytes()).await.is_err()
        {
            return;
        }
    }
}
/// FFmpeg and FFprobe from `SCRIBE_TEST_FFMPEG_DIR` or the PATH. The media tools are not part of this
/// repository: without them the tests that decode real audio say so and skip.
fn find_ffmpeg() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("SCRIBE_TEST_FFMPEG_DIR") {
        return Some(path.into());
    }
    which::which("ffmpeg")
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}
fn has_ffmpeg() -> bool {
    let found = find_ffmpeg().is_some();
    if !found {
        eprintln!("skipped: this test needs FFmpeg; put it on PATH or set SCRIBE_TEST_FFMPEG_DIR.");
    }
    found
}
fn ffmpeg_dir() -> PathBuf {
    find_ffmpeg().expect("the test checks has_ffmpeg first")
}
fn fixture(root: &Path, endpoint: &str) -> (Settings, Value, Update, Arc<Mutex<Vec<Value>>>) {
    support::downloader(root);
    let mut settings = support::settings(root);
    settings.speech_endpoint = endpoint.into();
    settings.ffmpeg_location = ffmpeg_dir().to_string_lossy().into_owned();
    let frames = 100_000;
    let mut source = wav_header(frames).unwrap();
    for n in 0..frames {
        let sample = ((n as f64 * 440. * std::f64::consts::TAU / 16000.).sin() * 12000.) as i16;
        source.extend(sample.to_le_bytes());
    }
    std::fs::write(root.join("source.wav"), source).unwrap();
    atomic_json(&root.join("metadata/v0000000001.json"),&json!({"id":"v0000000001","title":"Synthetic full audio","channel":"Offline fixture","release_date":"20261022","duration":6.25,"description":"Unrelated prose\nCreator link https://example.com/?ref=123&tag=keep#fragment","requested_downloads":[{"url":"https://metadata-only.invalid/never-fetched"}]})).unwrap();
    let updates = Arc::new(Mutex::new(vec![]));
    let sink = updates.clone();
    let update: Update = Arc::new(move |v| {
        sink.lock().push(v);
        Ok(())
    });
    let job = json!({"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","video_id":"v0000000001","url":"https://www.youtube.com/watch?v=v0000000001","title":"Before metadata","metadata":{},"created_at":scribe_common::now()});
    (settings, job, update, updates)
}
async fn execute(
    root: &Path,
    server: &Server,
    cancel: CancellationToken,
) -> (scribe_common::Result<()>, Arc<Mutex<Vec<Value>>>) {
    let (config, job, update, updates) = fixture(root, &server.url);
    let result = run(
        config,
        job,
        update,
        root.join("result"),
        root.join("data/work"),
        cancel,
    )
    .await;
    (result, updates)
}
fn clean(root: &Path) {
    assert_eq!(
        std::fs::read_dir(root.join("data/work")).unwrap().count(),
        0,
        "temporary audio must be removed on every result"
    );
}

#[tokio::test]
async fn real_ffmpeg_speed_complete_coverage_metadata_reuse_and_pooled_http() {
    if !has_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::Success).await;
    let (result, updates) = execute(root.path(), &server, CancellationToken::new()).await;
    result.unwrap();
    clean(root.path());
    let document = read_json(&root.path().join("result/transcript.json")).unwrap();
    assert_eq!(document["complete"], true);
    assert_eq!(document["completed_chunks"], 3);
    assert_eq!(document["channel"], "Offline fixture");
    assert_eq!(
        document["description_links"],
        json!(["Creator link https://example.com/?ref=123&tag=keep#fragment"])
    );
    let chunks = document["chunks"].as_array().unwrap();
    for pair in chunks.windows(2) {
        assert!(
            (pair[0]["offset_ms"].as_f64().unwrap() + pair[0]["duration_ms"].as_f64().unwrap()
                - pair[1]["offset_ms"].as_f64().unwrap())
            .abs()
                < 0.001
        );
    }
    assert_eq!(chunks[1]["offset_ms"], 2500.);
    assert_eq!(chunks[1]["segments"][0]["offset_ms"], 2625.);
    assert!(
        document["full_text"]
            .as_str()
            .unwrap()
            .contains("Full words from chunk 3. 日本語")
    );
    assert_eq!(
        server.connections.load(Ordering::SeqCst),
        1,
        "one reqwest pool must reuse the HTTP connection"
    );
    assert_eq!(server.requests.load(Ordering::SeqCst), 3);
    let frame_count: usize = server
        .payloads
        .lock()
        .iter()
        .map(|p| (p.len() - 44) / 2)
        .sum();
    assert!((frame_count as f64 / 16000. * 1.25 - 6.25).abs() < 0.1);
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    assert_eq!(
        calls.lines().filter(|l| l.starts_with("metadata")).count(),
        1
    );
    assert_eq!(
        calls.lines().filter(|l| l.starts_with("download")).count(),
        1
    );
    assert_eq!(
        read_json(&root.path().join("reused-info.json")).unwrap(),
        read_json(&root.path().join("metadata/v0000000001.json")).unwrap()
    );
    assert_eq!(updates.lock().last().unwrap()["status"], "completed");
    assert_eq!(
        updates.lock().last().unwrap()["performance"]["speech_requests"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let performance = updates.lock().last().unwrap()["performance"].clone();
    for (index, request) in performance["speech_requests"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let attempt = &request["attempts"][0];
        assert_eq!(
            attempt["body_stream_bytes"],
            server.multipart.lock()[index].len()
        );
        assert_eq!(attempt["body_stream_complete"], true);
        assert!(
            attempt["body_stream_last_byte_seconds"].as_f64().unwrap()
                <= attempt["response_headers_seconds"].as_f64().unwrap()
        );
        assert!(attempt["upload_seconds"].is_null());
        assert!(attempt["response_wait_seconds"].is_null());
        assert!(
            attempt["timing_note"]
                .as_str()
                .unwrap()
                .contains("not socket flush")
        );
    }
    server.close().await;
}
#[tokio::test]
async fn auth_failure_preserves_partial_text_does_not_retry_and_cleans_prefetched_audio() {
    if !has_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::AuthSecond).await;
    let (result, updates) = execute(root.path(), &server, CancellationToken::new()).await;
    let error = result.unwrap_err();
    assert!(!error.message.contains("must never escape"));
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    clean(root.path());
    let document = read_json(&root.path().join("result/transcript.json")).unwrap();
    assert_eq!(document["complete"], false);
    assert_eq!(document["completed_chunks"], 1);
    assert_derived_exports(root.path(), &document);
    assert!(!updates.lock().iter().any(|v| v["status"] == "completed"));
    server.close().await;
}

fn assert_derived_exports(root: &Path, document: &Value) {
    for format in ["txt", "md"] {
        assert_eq!(
            std::fs::read(root.join(format!("result/transcript.{format}"))).unwrap(),
            render_document(document, format).unwrap().as_bytes()
        );
    }
}

#[tokio::test]
async fn transient_metadata_collision_stops_before_download_and_cleans_owned_work() {
    if !has_ffmpeg() {
        return;
    }
    for symlink in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::start(Mode::Success).await;
        let (config, job, base_update, _) = fixture(root.path(), &server.url);
        let work = root.path().join("data/work");
        let inspected_work = work.clone();
        let outside = root.path().join("user-file.json");
        std::fs::write(&outside, "User file must remain unchanged").unwrap();
        let protected = outside.clone();
        let update: Update = Arc::new(move |patch| {
            if patch["message"] == "Downloading temporary audio…" {
                let temporary = std::fs::read_dir(&inspected_work)
                    .unwrap()
                    .next()
                    .unwrap()
                    .unwrap()
                    .path();
                let info = temporary.join("source-info.json");
                if symlink {
                    std::os::unix::fs::symlink(&protected, info).unwrap();
                } else {
                    std::fs::write(info, "Conflicting temporary metadata").unwrap();
                }
            }
            base_update(patch)
        });
        let result = run(
            config,
            job,
            update,
            root.path().join("result"),
            work,
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(server.requests.load(Ordering::SeqCst), 0);
        assert_eq!(
            std::fs::read_to_string(&outside).unwrap(),
            "User file must remain unchanged"
        );
        assert!(
            !std::fs::read_to_string(root.path().join("calls"))
                .unwrap()
                .lines()
                .any(|line| line.starts_with("download"))
        );
        clean(root.path());
        server.close().await;
    }
}

#[tokio::test]
async fn partial_json_is_durable_before_progress_but_previews_are_deferred_until_complete() {
    if !has_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::Success).await;
    let (config, job, base_update, _) = fixture(root.path(), &server.url);
    let result_dir = root.path().join("result");
    let checked_dir = result_dir.clone();
    let observed = Arc::new(AtomicUsize::new(0));
    let count = observed.clone();
    let update: Update = Arc::new(move |patch| {
        if let Some(completed) = patch["completed_chunks"].as_u64().filter(|v| *v > 0) {
            let doc = read_json(&checked_dir.join("transcript.json"))?;
            assert_eq!(doc["complete"], false);
            assert_eq!(doc["completed_chunks"], completed);
            assert!(
                checked_dir
                    .join(format!("chunks/{completed:06}.json"))
                    .is_file()
            );
            assert!(!checked_dir.join("transcript.txt").exists());
            assert!(!checked_dir.join("transcript.md").exists());
            count.fetch_add(1, Ordering::SeqCst);
        }
        base_update(patch)
    });
    run(
        config,
        job,
        update,
        result_dir.clone(),
        root.path().join("data/work"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(observed.load(Ordering::SeqCst), 3);
    let doc = read_json(&result_dir.join("transcript.json")).unwrap();
    assert_eq!(doc["complete"], true);
    assert_derived_exports(root.path(), &doc);
    clean(root.path());
    server.close().await;
}

#[tokio::test]
async fn cancellation_after_checkpoint_restores_previews_without_another_speech_request() {
    if !has_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::Success).await;
    let (config, job, base_update, updates) = fixture(root.path(), &server.url);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let update: Update = Arc::new(move |patch| {
        if patch["completed_chunks"] == 1 {
            token.cancel();
        }
        base_update(patch)
    });
    let result = run(
        config,
        job,
        update,
        root.path().join("result"),
        root.path().join("data/work"),
        cancel,
    )
    .await;
    assert_eq!(result.unwrap_err().status, 499);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    let doc = read_json(&root.path().join("result/transcript.json")).unwrap();
    assert_eq!(doc["complete"], false);
    assert_eq!(doc["completed_chunks"], 1);
    assert_derived_exports(root.path(), &doc);
    assert!(
        !updates
            .lock()
            .iter()
            .any(|patch| patch["status"] == "completed")
    );
    clean(root.path());
    server.close().await;
}

#[tokio::test]
async fn progress_write_failure_retains_the_newest_recognized_chunk_and_partial_previews() {
    if !has_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::Success).await;
    let (config, job, base_update, updates) = fixture(root.path(), &server.url);
    let update: Update = Arc::new(move |patch| {
        if patch["completed_chunks"] == 2 {
            return Err(scribe_common::Error::internal(
                "Synthetic job checkpoint failure",
            ));
        }
        base_update(patch)
    });
    let result = run(
        config,
        job,
        update,
        root.path().join("result"),
        root.path().join("data/work"),
        CancellationToken::new(),
    )
    .await;
    assert!(
        result
            .unwrap_err()
            .message
            .contains("Synthetic job checkpoint failure")
    );
    assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    let doc = read_json(&root.path().join("result/transcript.json")).unwrap();
    assert_eq!(doc["complete"], false);
    assert_eq!(doc["completed_chunks"], 2);
    assert!(doc["full_text"].as_str().unwrap().contains("chunk 2"));
    assert_derived_exports(root.path(), &doc);
    assert!(
        !updates
            .lock()
            .iter()
            .any(|patch| patch["status"] == "completed")
    );
    clean(root.path());
    server.close().await;
}

fn recovery_fixture() -> (Value, Vec<Value>) {
    let job = json!({"video_id":"v0000000001","url":"https://www.youtube.com/watch?v=v0000000001","title":"Recovered transcript","total_chunks":3,"metadata":{"channel":"Fixture"}});
    let chunks = (1..=3).map(|index| json!({"index":index,"offset_ms":(index-1)*1000,"duration_ms":1000,"text":format!("Retained words {index}"),"segments":[]})).collect();
    (job, chunks)
}

#[test]
fn completed_checkpoint_requires_exact_counts_source_text_and_complete_duration() {
    let root = tempfile::tempdir().unwrap();
    let result = root.path().join("result");
    let (mut job, chunks) = recovery_fixture();
    job["metadata"]["audio_duration_seconds"] = json!(3.);
    job["metadata"]["duration_seconds"] = json!(3.);
    let complete = build_document(&job, &chunks, true);
    atomic_json(&result.join("transcript.json"), &complete).unwrap();
    assert!(completed_checkpoint(&job, &result).unwrap());
    let bytes = std::fs::read(result.join("transcript.json")).unwrap();
    assert!(completed_checkpoint(&job, &result).unwrap());
    assert_eq!(
        std::fs::read(result.join("transcript.json")).unwrap(),
        bytes
    );
    assert!(
        !result.join("transcript.txt").exists(),
        "completion validation has no derived-file writes"
    );
    for (name, pointer, value) in [
        ("partial", "/complete", json!(false)),
        ("wrong video", "/video_id", json!("v0000000002")),
        (
            "wrong source",
            "/source_url",
            json!("https://www.youtube.com/watch?v=v0000000002"),
        ),
        ("completed count", "/completed_chunks", json!(2)),
        ("total count", "/total_chunks", json!(4)),
        ("empty chunks", "/chunks", json!([])),
        ("initial gap", "/chunks/0/offset_ms", json!(1)),
        ("negative offset", "/chunks/0/offset_ms", json!(-0.0001)),
        ("middle gap", "/chunks/1/offset_ms", json!(1001)),
        ("index", "/chunks/1/index", json!(1)),
        ("negative duration", "/chunks/1/duration_ms", json!(-1000)),
        (
            "non-numeric duration",
            "/chunks/1/duration_ms",
            json!("NaN"),
        ),
        ("non-string text", "/chunks/1/text", Value::Null),
        (
            "mismatched text",
            "/full_text",
            json!("Missing recognized words"),
        ),
        ("audio mismatch", "/audio_duration_seconds", json!(3.002)),
        ("unknown audio", "/audio_duration_seconds", Value::Null),
        ("source mismatch", "/video_duration_seconds", json!(3.002)),
    ] {
        let mut changed = complete.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        atomic_json(&result.join("transcript.json"), &changed).unwrap();
        assert!(!completed_checkpoint(&job, &result).unwrap(), "{name}");
        assert_eq!(read_json(&result.join("transcript.json")).unwrap(), changed);
    }
    for (pointer, value) in [
        ("/total_chunks", json!(0)),
        ("/total_chunks", json!(2)),
        ("/metadata/audio_duration_seconds", json!(3.002)),
        ("/metadata/audio_duration_seconds", Value::Null),
    ] {
        let mut changed = job.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        atomic_json(&result.join("transcript.json"), &complete).unwrap();
        assert!(!completed_checkpoint(&changed, &result).unwrap());
    }
    job["metadata"]["duration_seconds"] = json!(6.1);
    let too_short = build_document(&job, &chunks, true);
    atomic_json(&result.join("transcript.json"), &too_short).unwrap();
    assert!(!completed_checkpoint(&job, &result).unwrap());
}

#[test]
fn completed_checkpoint_handles_missing_corrupt_silent_and_absent_source_duration() {
    let root = tempfile::tempdir().unwrap();
    let result = root.path().join("result");
    let (mut job, mut chunks) = recovery_fixture();
    job["metadata"]["audio_duration_seconds"] = json!(3.);
    assert!(!completed_checkpoint(&job, &result).unwrap());
    for chunk in &mut chunks {
        chunk["text"] = json!("");
    }
    let document = build_document(&job, &chunks, true);
    atomic_json(&result.join("transcript.json"), &document).unwrap();
    assert!(
        completed_checkpoint(&job, &result).unwrap(),
        "a fully processed silent video has valid empty text"
    );
    std::fs::write(result.join("transcript.json"), "invalid saved JSON").unwrap();
    assert!(completed_checkpoint(&job, &result).is_err());
}

#[test]
fn recovery_merges_newer_durable_journal_tail_and_is_idempotent() {
    use std::os::unix::fs::MetadataExt;
    let root = tempfile::tempdir().unwrap();
    let result = root.path().join("result");
    let (job, chunks) = recovery_fixture();
    let mut old = build_document(&job, &chunks[..1], false);
    old["unknown_metadata"] = json!({"preserve":true});
    write_exports(&result, &old).unwrap();
    for (index, chunk) in chunks.iter().enumerate() {
        atomic_json(&result.join(format!("chunks/{:06}.json", index + 1)), chunk).unwrap();
    }
    restore_partial_exports(&job, &result).unwrap();
    let recovered = read_json(&result.join("transcript.json")).unwrap();
    assert_eq!(
        recovered["complete"], false,
        "all recognized chunks do not imply a completed pipeline"
    );
    assert_eq!(recovered["completed_chunks"], 3);
    assert_eq!(recovered["unknown_metadata"], old["unknown_metadata"]);
    assert_eq!(recovered["transcribed_at"], old["transcribed_at"]);
    assert_derived_exports(root.path(), &recovered);
    let before = ["json", "txt", "md"].map(|format| {
        std::fs::metadata(result.join(format!("transcript.{format}")))
            .unwrap()
            .ino()
    });
    restore_partial_exports(&job, &result).unwrap();
    let after = ["json", "txt", "md"].map(|format| {
        std::fs::metadata(result.join(format!("transcript.{format}")))
            .unwrap()
            .ino()
    });
    assert_eq!(
        before, after,
        "already-current derivations should not be rewritten"
    );
}

#[test]
fn recovery_without_json_retains_only_the_valid_contiguous_journal_prefix() {
    let root = tempfile::tempdir().unwrap();
    let result = root.path().join("result");
    let (job, chunks) = recovery_fixture();
    atomic_json(&result.join("chunks/000001.json"), &chunks[0]).unwrap();
    atomic_json(&result.join("chunks/000003.json"), &chunks[2]).unwrap();
    restore_partial_exports(&job, &result).unwrap();
    let recovered = read_json(&result.join("transcript.json")).unwrap();
    assert_eq!(recovered["complete"], false);
    assert_eq!(recovered["chunks"], json!([chunks[0]]));
    assert!(result.join("chunks/000003.json").is_file());
    assert_derived_exports(root.path(), &recovered);
}

#[test]
fn recovery_preserves_complete_exports_and_rejects_conflicting_or_unsafe_partial_data() {
    let (job, chunks) = recovery_fixture();
    let root = tempfile::tempdir().unwrap();
    let result = root.path().join("result");
    let complete = build_document(&job, &chunks, true);
    write_exports(&result, &complete).unwrap();
    std::fs::write(
        result.join("transcript.txt"),
        "Existing complete cache bytes",
    )
    .unwrap();
    restore_partial_exports(&job, &result).unwrap();
    assert_eq!(
        std::fs::read_to_string(result.join("transcript.txt")).unwrap(),
        "Existing complete cache bytes"
    );
    let partial = build_document(&job, &chunks[..1], false);
    write_exports(&result, &partial).unwrap();
    let before = std::fs::read(result.join("transcript.json")).unwrap();
    let mut conflicting = chunks[0].clone();
    conflicting["text"] = json!("Conflicting checkpoint");
    atomic_json(&result.join("chunks/000001.json"), &conflicting).unwrap();
    assert!(restore_partial_exports(&job, &result).is_err());
    assert_eq!(
        std::fs::read(result.join("transcript.json")).unwrap(),
        before
    );
    std::fs::remove_dir_all(result.join("chunks")).unwrap();
    let outside = root.path().join("outside");
    atomic_json(&outside.join("000001.json"), &chunks[0]).unwrap();
    std::os::unix::fs::symlink(&outside, result.join("chunks")).unwrap();
    assert!(restore_partial_exports(&job, &result).is_err());
    assert_eq!(
        std::fs::read(result.join("transcript.json")).unwrap(),
        before
    );
}
#[tokio::test]
async fn incomplete_azure_duration_never_marks_complete() {
    if !has_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::ShortSecond).await;
    let (result, _) = execute(root.path(), &server, CancellationToken::new()).await;
    assert!(result.unwrap_err().message.contains("less than"));
    assert_eq!(
        read_json(&root.path().join("result/transcript.json")).unwrap()["complete"],
        false
    );
    clean(root.path());
    server.close().await;
}
#[tokio::test]
async fn retry_uses_identical_chunk_bytes_and_respects_bounded_transient_policy() {
    if !has_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::RetryFirst).await;
    let (result, updates) = execute(root.path(), &server, CancellationToken::new()).await;
    result.unwrap();
    assert_eq!(server.requests.load(Ordering::SeqCst), 4);
    assert_eq!(server.connections.load(Ordering::SeqCst), 1);
    {
        let raw = server.multipart.lock();
        assert_eq!(raw[0], raw[1]);
    }
    let performance = updates.lock().last().unwrap()["performance"].clone();
    let attempts = performance["speech_requests"][0]["attempts"]
        .as_array()
        .unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0]["status_code"], 429);
    assert_eq!(attempts[1]["status_code"], 200);
    for attempt in attempts {
        assert_eq!(attempt["body_stream_complete"], true);
        assert_eq!(
            attempt["body_stream_bytes"],
            attempt["body_stream_expected_bytes"]
        );
        assert!(
            attempt["body_stream_first_poll_seconds"].as_f64().unwrap()
                <= attempt["body_stream_last_byte_seconds"].as_f64().unwrap()
        );
    }
    {
        let payloads = server.payloads.lock();
        assert_eq!(payloads[0], payloads[1]);
    }
    clean(root.path());
    server.close().await;
}
#[tokio::test]
async fn cancellation_while_azure_waits_joins_prefetch_and_removes_all_audio() {
    if !has_ffmpeg() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::Hold).await;
    let (config, job, update, updates) = fixture(root.path(), &server.url);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let result_dir = root.path().join("result");
    let work = root.path().join("data/work");
    let run = tokio::spawn(run(config, job, update, result_dir, work, token));
    tokio::time::timeout(Duration::from_secs(8), async {
        while server.requests.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .status,
        499
    );
    clean(root.path());
    let performance = updates.lock().last().unwrap()["performance"].clone();
    let attempt = &performance["speech_requests"][0]["attempts"][0];
    assert!(attempt["status_code"].is_null());
    assert!(attempt["response_headers_seconds"].is_null());
    assert_eq!(
        attempt["body_stream_bytes"],
        server.multipart.lock()[0].len()
    );
    assert_eq!(attempt["body_stream_complete"], true);
    server.close().await;
}
#[test]
fn probe_duration_is_strict_and_falls_back_only_when_stream_duration_unavailable() {
    for stream in [Value::Null, json!("N/A"), json!("")] {
        assert_eq!(
            probe_duration(&json!({"streams":[{"duration":stream}],"format":{"duration":"6.25"}}))
                .unwrap(),
            6.25
        );
    }
    for stream in [
        json!("NaN"),
        json!("inf"),
        json!(-1),
        json!("broken"),
        json!({}),
    ] {
        assert!(
            probe_duration(&json!({"streams":[{"duration":stream}],"format":{"duration":"6.25"}}))
                .is_err()
        );
    }
    assert!(probe_duration(&json!({})).is_err());
}

/// Explicit offline diagnostic, not a wall-clock performance assertion.
/// Run: cargo test -p scribe-engine --release --test pipeline offline_release_benchmark -- --ignored --exact --nocapture
#[tokio::test]
#[ignore = "Run explicitly in release mode to emit an offline pipeline benchmark JSON record."]
async fn offline_release_benchmark() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::start(Mode::Success).await;
    let (config, job, update, updates) = fixture(root.path(), &server.url);
    let started = std::time::Instant::now();
    run(
        config,
        job,
        update,
        root.path().join("result"),
        root.path().join("data/work"),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let wall = started.elapsed().as_secs_f64();
    let document = read_json(&root.path().join("result/transcript.json")).unwrap();
    let performance = updates.lock().last().unwrap()["performance"].clone();
    let (processed_frames, audio_bytes) = {
        let payloads = server.payloads.lock();
        (
            payloads
                .iter()
                .map(|wav| (wav.len() - 44) / 2)
                .sum::<usize>(),
            payloads.iter().map(Vec::len).sum::<usize>(),
        )
    };
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    let result = json!({
        "benchmark":"scribe-engine-offline-pipeline",
        "profile":if cfg!(debug_assertions){"debug"}else{"release"},
        "synthetic_source_seconds":6.25,
        "audio_speed":1.25,
        "wall_seconds":wall,
        "metadata_requests":calls.lines().filter(|line|line.starts_with("metadata ")).count(),
        "download_invocations":calls.lines().filter(|line|line.starts_with("download ")).count(),
        "speech_requests":server.requests.load(Ordering::SeqCst),
        "http_connections":server.connections.load(Ordering::SeqCst),
        "audio_bytes":audio_bytes,
        "processed_audio_frames":processed_frames,
        "completed_chunks":document["completed_chunks"],
        "complete":document["complete"],
        "temporary_entries_remaining":std::fs::read_dir(root.path().join("data/work")).unwrap().count(),
        "performance":performance,
        "limitations":"Synthetic audio, local fixture downloader and loopback Azure-shaped mock. Measures local pipeline/FFmpeg/HTTP overhead; excludes real YouTube download, WAN upload, Azure inference and human transcript quality. Stage totals overlap and must not be summed as wall time.",
    });
    println!(
        "SCRIBE_OFFLINE_BENCHMARK={}",
        serde_json::to_string(&result).unwrap()
    );
    clean(root.path());
    server.close().await;
}
