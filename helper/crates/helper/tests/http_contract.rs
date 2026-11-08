#![allow(clippy::unwrap_used)]
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use parking_lot::RwLock;
use scribe_common::{Result, Settings};
use scribe_helper::api::{AppState, EngineApi, router};
use scribe_library::Library;
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tower::ServiceExt;

#[derive(Default)]
struct MockEngine {
    calls: parking_lot::Mutex<Vec<(String, Value)>>,
    active: AtomicBool,
    block_test: AtomicBool,
    test_started: tokio::sync::Notify,
    test_release: tokio::sync::Notify,
    block_work: AtomicBool,
    work_started: tokio::sync::Notify,
    work_release: tokio::sync::Notify,
}
impl EngineApi for MockEngine {
    fn request<'a>(
        &'a self,
        action: &'a str,
        input: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>> {
        Box::pin(async move {
            self.calls.lock().push((action.to_owned(), input));
            if action == "testConnection" && self.block_test.load(Ordering::SeqCst) {
                self.test_started.notify_one();
                self.test_release.notified().await;
            }
            if action == "submitJob" && self.block_work.load(Ordering::SeqCst) {
                self.work_started.notify_one();
                self.work_release.notified().await;
            }
            Ok(json!({"action":action}))
        })
    }
    fn has_active_work(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    state: Arc<AppState>,
    engine: Arc<MockEngine>,
}
fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let data = root.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(
        root.join(".env"),
        "HELPER_TOKEN='test-token'\nCUSTOM_SETTING='preserved'\n",
    )
    .unwrap();
    let mut settings = Settings::load(root, Some(data.clone())).unwrap();
    settings.helper_token = "test-token".into();
    settings.speech_key = "fake-resource-key".into();
    settings.speech_endpoint = "https://fixture.cognitiveservices.azure.com".into();
    let bin = root.join(".tools/bin");
    std::fs::create_dir_all(&bin).unwrap();
    for name in ["ffmpeg", "ffprobe", "yt-dlp"] {
        let path = bin.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 99\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    let library = Arc::new(Library::open(&data).unwrap());
    let engine = Arc::new(MockEngine::default());
    let state = AppState::new(Arc::new(RwLock::new(settings)), library, engine.clone());
    Fixture {
        _temp: temp,
        state,
        engine,
    }
}
async fn call(
    state: &Arc<AppState>,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    raw_call(
        state,
        method,
        path,
        body.map(|v| v.to_string()).unwrap_or_default(),
        "127.0.0.1:8765",
        Some("Bearer test-token"),
        None,
    )
    .await
}
async fn raw_call(
    state: &Arc<AppState>,
    method: &str,
    path: &str,
    body: String,
    host: &str,
    auth: Option<&str>,
    origin: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", host);
    if let Some(a) = auth {
        request = request.header("Authorization", a);
    }
    if let Some(o) = origin {
        request = request.header("Origin", o);
    }
    let response = router(state.clone())
        .oneshot(request.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        },
    )
}

#[tokio::test]
async fn loopback_origin_authorization_and_health_are_separate() {
    let f = fixture();
    let (status, health) = raw_call(
        &f.state,
        "GET",
        "/health",
        String::new(),
        "localhost:8765",
        None,
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(health["model"], "MAI-Transcribe-2");
    assert!(health.get("api_key").is_none());
    for host in ["evil.example", "127.0.0.1.evil", "[::1]:8765", ""] {
        assert_eq!(
            raw_call(&f.state, "GET", "/health", String::new(), host, None, None)
                .await
                .0,
            403
        );
    }
    for origin in [
        "https://www.youtube.com",
        "null",
        "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
        "chrome-extension://zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
    ] {
        assert_eq!(
            raw_call(
                &f.state,
                "GET",
                "/health",
                String::new(),
                "127.0.0.1",
                None,
                Some(origin)
            )
            .await
            .0,
            403
        );
    }
    assert_eq!(
        raw_call(
            &f.state,
            "GET",
            "/jobs",
            String::new(),
            "127.0.0.1",
            None,
            None
        )
        .await
        .0,
        401
    );
    assert_eq!(
        raw_call(
            &f.state,
            "GET",
            "/jobs",
            String::new(),
            "127.0.0.1",
            Some("Bearer wrong"),
            None
        )
        .await
        .0,
        401
    );
    let response = router(f.state.clone())
        .oneshot(
            Request::builder()
                .uri("/health")
                .header("Host", "localhost")
                .header(
                    "Origin",
                    "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert!(f.engine.calls.lock().is_empty());
}

#[tokio::test]
async fn preflight_is_safe_and_missing_token_fails_closed() {
    let f = fixture();
    let origin = "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    assert_eq!(
        raw_call(
            &f.state,
            "OPTIONS",
            "/jobs",
            String::new(),
            "localhost",
            None,
            Some(origin)
        )
        .await
        .0,
        204
    );
    f.state.settings.write().helper_token.clear();
    assert_eq!(call(&f.state, "GET", "/jobs", None).await.0, 503);
    assert_eq!(call(&f.state, "GET", "/health", None).await.0, 200);
}

#[tokio::test]
async fn all_engine_routes_preserve_action_and_payload_contract() {
    let f = fixture();
    let cases = vec![
        ("GET", "/library", None, "librarySnapshot"),
        (
            "GET",
            "/library/stats?folder_id=motion",
            None,
            "libraryStats",
        ),
        (
            "GET",
            "/library/documents?folder_id=motion",
            None,
            "folderDocuments",
        ),
        ("GET", "/jobs", None, "jobs"),
        ("GET", "/queue/status", None, "queueStatus"),
        ("GET", "/queue/attention", None, "queueAttention"),
        ("POST", "/queue/cancel", None, "cancelQueue"),
        (
            "POST",
            "/jobs",
            Some(json!({"url":"https://youtu.be/abcdefghijk"})),
            "submitJob",
        ),
        ("GET", "/jobs/job-1", None, "getJob"),
        ("POST", "/jobs/job-1/cancel", None, "cancelJob"),
        (
            "GET",
            "/jobs/job-1/document?format=json",
            None,
            "readDocument",
        ),
        (
            "POST",
            "/videos/status",
            Some(json!({"video_ids":["abcdefghijk"]})),
            "videoStatuses",
        ),
        ("POST", "/videos/abcdefghijk/cancel", None, "cancelVideo"),
        (
            "POST",
            "/jobs/move",
            Some(json!({"job_ids":["job-1"],"folder_id":null})),
            "moveJobs",
        ),
        (
            "POST",
            "/exports",
            Some(json!({"folder_id":null,"format":"txt"})),
            "exportJobs",
        ),
        (
            "POST",
            "/channels/preview",
            Some(json!({"url":"https://youtube.com/@test","scope":"year"})),
            "startChannelPreview",
        ),
        (
            "POST",
            "/channels/import",
            Some(json!({"url":"https://youtube.com/@test","scope":"all","include_shorts":true})),
            "startChannelImport",
        ),
        (
            "GET",
            "/channels/previews/preview-1",
            None,
            "getChannelPreview",
        ),
        (
            "POST",
            "/channels/previews/preview-1/queue",
            None,
            "queueChannelPreview",
        ),
        (
            "POST",
            "/channels/previews/preview-1/cancel",
            None,
            "cancelChannelPreview",
        ),
        ("DELETE", "/folders/folder-1", None, "removeMissingFolder"),
    ];
    for (method, path, body, action) in cases {
        let (status, value) = call(&f.state, method, path, body).await;
        assert_eq!(status, 200, "{method} {path}: {value}");
        assert_eq!(value["action"], action);
    }
    let calls = f.engine.calls.lock();
    let submission = calls.iter().find(|(name, _)| name == "submitJob").unwrap();
    assert!(
        submission.1.get("folder_id").is_none(),
        "omitted destination must preserve default semantics"
    );
    let preview = calls
        .iter()
        .find(|(name, _)| name == "startChannelPreview")
        .unwrap();
    assert_eq!(preview.1["include_shorts"], false);
}

#[tokio::test]
async fn malformed_requests_are_redacted_and_never_reach_engine() {
    let f = fixture();
    let secret = "DO-NOT-ECHO-RESOURCE-KEY";
    let cases = vec![
        ("/videos/status", json!({"video_ids":[secret]})),
        ("/videos/status", json!({"video_ids":[],"secret":secret})),
        ("/jobs", json!({"url":12,"secret":secret})),
        (
            "/channels/import",
            json!({"url":"https://youtube.com/@test","scope":"all","include_shorts":1,"api_key":secret}),
        ),
        ("/exports", json!({"format":secret})),
        ("/export-settings", json!({"enabled":"true","format":"txt"})),
        ("/jobs/move", json!({"job_ids":[],"folder_id":null})),
        ("/library/default-folder", json!({})),
        (
            "/library/attachments",
            json!({"excluded_folders":[null],"root_token":secret}),
        ),
    ];
    for (path, value) in cases {
        let (status, result) = call(&f.state, "POST", path, Some(value)).await;
        assert_eq!(status, 422, "{path}: {result}");
        assert!(!result.to_string().contains(secret));
    }
    let (status, result) = raw_call(
        &f.state,
        "POST",
        "/jobs",
        format!("{{\"url\":\"{secret}"),
        "localhost",
        Some("Bearer test-token"),
        None,
    )
    .await;
    assert_eq!(status, 422);
    assert!(!result.to_string().contains(secret));
    for path in [
        "/videos/not-valid/reveal",
        "/videos/not-valid/cancel",
        "/library/attachments/not-valid/copy",
        "/jobs/a/document?format=exe",
    ] {
        assert_eq!(
            call(
                &f.state,
                if path.contains("document") {
                    "GET"
                } else {
                    "POST"
                },
                path,
                None
            )
            .await
            .0,
            422
        );
    }
    assert!(f.engine.calls.lock().is_empty());
}

#[tokio::test]
async fn settings_preserve_token_and_secrets_and_reject_active_changes() {
    let f = fixture();
    let key = "synthetic-new-private-key";
    let(status,value)=call(&f.state,"POST","/settings",Some(json!({"endpoint":"https://newresource.services.ai.azure.com/api/projects/demo","api_key":key,"use_brave_login":false}))).await;
    assert_eq!(status, 200);
    assert_eq!(
        value["endpoint"],
        "https://newresource.cognitiveservices.azure.com"
    );
    assert_eq!(value["has_api_key"], true);
    assert!(!value.to_string().contains(key));
    assert!(!value.to_string().contains("test-token"));
    let env = std::fs::read_to_string(f._temp.path().join(".env")).unwrap();
    assert!(env.contains("CUSTOM_SETTING='preserved'"));
    assert!(env.contains("HELPER_TOKEN='test-token'"));
    assert!(env.contains(key));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f._temp.path().join(".env"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    f.engine.active.store(true, Ordering::SeqCst);
    assert_eq!(
        call(
            &f.state,
            "POST",
            "/settings",
            Some(json!({"endpoint":"https://newresource.cognitiveservices.azure.com"}))
        )
        .await
        .0,
        409
    );
    assert_eq!(
        std::fs::read_to_string(f._temp.path().join(".env")).unwrap(),
        env
    );
}

#[tokio::test]
async fn settings_limits_invalid_keys_and_endpoint_changes_do_not_echo_credentials() {
    let f = fixture();
    let (status, value) = raw_call(
        &f.state,
        "POST",
        "/settings",
        "x".repeat(16_385),
        "localhost",
        Some("Bearer test-token"),
        None,
    )
    .await;
    assert_eq!(status, 413);
    assert!(value["detail"].as_str().unwrap().contains("large"));
    for input in [
        json!({"endpoint":"https://evil.example","api_key":"secret"}),
        json!({"endpoint":"https://fixture.cognitiveservices.azure.com","api_key":"secret\nnotkey"}),
        json!({"endpoint":"https://fixture.cognitiveservices.azure.com","api_key":123}),
        json!({"endpoint":"https://other.cognitiveservices.azure.com"}),
        json!({"endpoint":"https://fixture.cognitiveservices.azure.com","use_brave_login":"true"}),
    ] {
        let (status, result) = call(&f.state, "POST", "/settings", Some(input)).await;
        assert_eq!(status, 400);
        assert!(!result.to_string().contains("secret"));
    }
    assert!(f.engine.calls.lock().is_empty());
}

#[tokio::test]
async fn library_mutations_and_picker_read_use_private_temp_state_only() {
    let f = fixture();
    let (status, folder) = call(&f.state, "POST", "/folders", Some(json!({"name":"Motion"}))).await;
    assert_eq!(status, 200);
    assert_eq!(folder["name"], "Motion");
    assert_eq!(folder["kind"], "manual");
    let (status, _) = call(
        &f.state,
        "POST",
        "/library/default-folder",
        Some(json!({"folder_id":folder["id"]})),
    )
    .await;
    assert_eq!(status, 200);
    let (status, prefs) = call(&f.state, "GET", "/export-settings", None).await;
    assert_eq!(status, 200);
    assert_eq!(prefs["directory"], "");
    let (status, picker) = call(&f.state, "GET", "/export-settings/choose-directory", None).await;
    assert_eq!(status, 200);
    assert_eq!(picker["pending"], false);
    assert_eq!(picker["cancelled"], false);
    let (status, _) = call(
        &f.state,
        "POST",
        "/export-settings",
        Some(json!({"enabled":false,"format":"md"})),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = call(
        &f.state,
        "POST",
        "/exports",
        Some(json!({"job_ids":[],"folder_id":null})),
    )
    .await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn a_pending_connection_test_rejects_another_test_or_settings_change_immediately() {
    let f = fixture();
    f.engine.block_test.store(true, Ordering::SeqCst);
    let state = f.state.clone();
    let first = tokio::spawn(async move { call(&state, "POST", "/settings/test", None).await });
    f.engine.test_started.notified().await;
    let second = tokio::time::timeout(
        std::time::Duration::from_millis(250),
        call(&f.state, "POST", "/settings/test", None),
    )
    .await
    .unwrap();
    assert_eq!(second.0, 409);
    let update=tokio::time::timeout(std::time::Duration::from_millis(250),call(&f.state,"POST","/settings",Some(json!({"endpoint":"https://fixture.cognitiveservices.azure.com","api_key":"must-not-be-saved"})))).await.unwrap();
    assert_eq!(update.0, 409);
    assert!(
        !std::fs::read_to_string(f._temp.path().join(".env"))
            .unwrap()
            .contains("must-not-be-saved")
    );
    f.engine.test_release.notify_one();
    assert_eq!(first.await.unwrap().0, 200);
    assert_eq!(
        f.engine
            .calls
            .lock()
            .iter()
            .filter(|(action, _)| action == "testConnection")
            .count(),
        1
    );
}

async fn slow_post(
    state: Arc<AppState>,
    path: &'static str,
    value: Value,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
) -> StatusCode {
    let stream = futures_util::stream::once(async move {
        started.notify_one();
        release.notified().await;
        Ok::<_, std::io::Error>(value.to_string())
    });
    router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("host", "127.0.0.1:8765")
                .header("authorization", "Bearer test-token")
                .body(Body::from_stream(stream))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn incomplete_request_bodies_never_hold_settings_or_work_admission_locks() {
    for waiting_path in ["/settings", "/jobs"] {
        let f = fixture();
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let value = if waiting_path == "/settings" {
            json!({"endpoint":"https://fixture.cognitiveservices.azure.com","api_key":"new-fixture-key"})
        } else {
            json!({"url":"https://www.youtube.com/watch?v=abcdefghijk"})
        };
        let waiting = tokio::spawn(slow_post(
            f.state.clone(),
            waiting_path,
            value,
            started.clone(),
            release.clone(),
        ));
        started.notified().await;
        let (path, body) = if waiting_path == "/settings" {
            (
                "/jobs",
                json!({"url":"https://www.youtube.com/watch?v=abcdefghijk"}),
            )
        } else {
            (
                "/settings",
                json!({"endpoint":"https://fixture.cognitiveservices.azure.com","api_key":"new-fixture-key"}),
            )
        };
        let immediate = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            call(&f.state, "POST", path, Some(body)),
        )
        .await;
        release.notify_one();
        assert_eq!(waiting.await.unwrap(), 200);
        assert_eq!(
            immediate
                .expect("An incomplete body must not reserve global admission")
                .0,
            200
        );
    }
}

#[tokio::test]
async fn actual_work_acceptance_still_serializes_settings_persistence() {
    let f = fixture();
    f.engine.block_work.store(true, Ordering::SeqCst);
    let state = f.state.clone();
    let job = tokio::spawn(async move {
        call(
            &state,
            "POST",
            "/jobs",
            Some(json!({"url":"https://www.youtube.com/watch?v=abcdefghijk"})),
        )
        .await
    });
    f.engine.work_started.notified().await;
    let state = f.state.clone();
    let change = tokio::spawn(async move {
        call(&state,"POST","/settings",Some(json!({"endpoint":"https://fixture.cognitiveservices.azure.com","api_key":"after-admission"}))).await
    });
    tokio::task::yield_now().await;
    assert!(!change.is_finished());
    assert!(
        !std::fs::read_to_string(f._temp.path().join(".env"))
            .unwrap()
            .contains("after-admission")
    );
    f.engine.active.store(true, Ordering::SeqCst);
    f.engine.work_release.notify_one();
    assert_eq!(job.await.unwrap().0, 200);
    assert_eq!(change.await.unwrap().0, 409);
}

#[tokio::test]
#[ignore = "isolated read-only helper latency measurement"]
async fn helper_read_only_latency_benchmark() {
    let f = fixture();
    let settings = f.state.settings.read().clone();
    std::fs::write(
        settings.data_dir.join("runtime.json"),
        json!({"ffmpeg_location":settings.root.join(".tools/bin")}).to_string(),
    )
    .unwrap();
    let app = router(f.state.clone());
    let mut results = Vec::new();
    for path in ["/health", "/settings", "/export-settings"] {
        let mut samples = Vec::new();
        for index in 0..220 {
            let start = std::time::Instant::now();
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("host", "127.0.0.1:8765")
                        .header("authorization", "Bearer test-token")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            to_bytes(response.into_body(), 16384).await.unwrap();
            if index >= 20 {
                samples.push(start.elapsed().as_secs_f64() * 1e6);
            }
        }
        samples.sort_by(f64::total_cmp);
        results.push(json!({"path":path,"median_us":(samples[99]+samples[100])/2.,"p95_us":samples[189],"samples":samples.len()}));
    }
    eprintln!(
        "{}",
        json!({"scope":"In-process production router, isolated synthetic runtime files, no network or paid actions","results":results})
    );
}
