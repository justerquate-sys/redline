#![allow(clippy::unwrap_used)]
mod support;
use scribe_engine::process::{downloader_error, run_process, stream_tab};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn cancellation_reaps_the_owned_descendant_group_before_returning() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("writer");
    support::executable(
        &script,
        "#!/bin/sh\ntrap '' TERM\n( trap '' TERM; while :; do printf x >> heartbeat; sleep 0.02; done ) &\nprintf '%s' $! > child.pid\nwait\n",
    );
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let cwd = root.path().to_path_buf();
    let task = tokio::spawn(async move {
        run_process(
            &[script.to_string_lossy().into_owned()],
            &cwd,
            "fixture failure",
            Duration::from_secs(60),
            &token,
            false,
        )
        .await
    });
    support::wait_file(&root.path().join("child.pid")).await;
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(12), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .status,
        499
    );
    let before = std::fs::metadata(root.path().join("heartbeat"))
        .unwrap()
        .len();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        std::fs::metadata(root.path().join("heartbeat"))
            .unwrap()
            .len(),
        before,
        "no child may continue writing after cleanup starts"
    );
}
#[tokio::test]
async fn timeout_and_warning_errors_are_safe_and_bounded() {
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("slow");
    support::executable(&script, "#!/bin/sh\nsleep 300\n");
    let err = run_process(
        &[script.to_string_lossy().into_owned()],
        root.path(),
        "safe message",
        Duration::from_millis(20),
        &CancellationToken::new(),
        false,
    )
    .await
    .unwrap_err();
    assert!(!err.message.contains(root.path().to_str().unwrap()));
    let script = root.path().join("warning");
    support::executable(
        &script,
        "#!/bin/sh\nprintf 'WARNING: private-detail-key\\n' >&2\nhead -c 90000 /dev/zero >&2\nprintf '{}'\n",
    );
    let err = run_process(
        &[script.to_string_lossy().into_owned()],
        root.path(),
        "safe message",
        Duration::from_secs(5),
        &CancellationToken::new(),
        true,
    )
    .await
    .unwrap_err();
    assert!(!err.message.contains("private-detail"));
}
#[tokio::test]
async fn tab_cutoff_stops_stream_and_cleans_private_directory() {
    let root = tempfile::tempdir().unwrap();
    support::downloader(root.path());
    std::fs::write(
        root.path().join("tabs/videos.jsonl"),
        "{\"id\":\"abcdefghijk\"}\n{\"id\":\"bcdefghijkl\"}\n",
    )
    .unwrap();
    std::fs::write(root.path().join("hold-after-list"), "").unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let n = count.clone();
    tokio::time::timeout(
        Duration::from_secs(5),
        stream_tab(
            &support::settings(root.path()),
            "https://www.youtube.com/@offline/videos",
            &CancellationToken::new(),
            move |_| {
                n.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(false) })
            },
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read_dir(root.path().join("data/work"))
            .unwrap()
            .count(),
        0
    );
}
#[tokio::test]
async fn missing_tab_is_distinct_from_auth_or_incomplete_listing() {
    let root = tempfile::tempdir().unwrap();
    support::downloader(root.path());
    let config = support::settings(root.path());
    for (message, status) in [
        ("ERROR: This channel does not have a shorts tab", 424),
        (
            "ERROR: This channel does not have a shorts tab; sign in to confirm you're not a bot",
            400,
        ),
    ] {
        std::fs::write(root.path().join("tabs/shorts.error"), message).unwrap();
        let e = stream_tab(
            &config,
            "https://www.youtube.com/@offline/shorts",
            &CancellationToken::new(),
            |_| Box::pin(async { Ok(true) }),
        )
        .await
        .unwrap_err();
        assert_eq!(e.status, status);
    }
    for url in [
        "https://attacker.invalid/@name/videos",
        "http://www.youtube.com/@name/videos",
        "https://www.youtube.com/@name/videos?next=bad",
    ] {
        assert!(
            stream_tab(&config, url, &CancellationToken::new(), |_| Box::pin(
                async { Ok(true) }
            ))
            .await
            .is_err()
        );
    }
}
#[test]
fn error_classification_never_returns_raw_downloader_details() {
    assert_eq!(
        downloader_error(
            b"Private video; sign in with cookies private-secret",
            "safe"
        )
        .status,
        410
    );
    let e = downloader_error(
        b"Private video sign in to confirm you're not a bot private-secret",
        "safe",
    );
    assert_eq!(e.status, 400);
    assert!(!e.message.contains("private-secret"));
}

#[test]
fn persisted_node_runtime_is_passed_to_downloader_without_path_lookup() {
    let root = tempfile::tempdir().unwrap();
    support::downloader(root.path());
    let mut settings = support::settings(root.path());
    let node = root.path().join("runtime/node");
    support::executable(&node, "#!/bin/sh\nexit 0\n");
    std::fs::create_dir_all(&settings.data_dir).unwrap();
    scribe_common::atomic_json(
        &settings.data_dir.join("runtime.json"),
        &serde_json::json!({"node":node}),
    )
    .unwrap();
    let args = scribe_engine::process::ytdlp_arguments(&settings).unwrap();
    let index = args.iter().position(|arg| arg == "--js-runtimes").unwrap();
    assert_eq!(args[index + 1], format!("node:{}", node.display()));
    settings.js_runtime = "deno:/explicit/override".into();
    assert_eq!(
        settings.javascript_runtime().as_deref(),
        Some("deno:/explicit/override")
    );
    let error = downloader_error(b"Requested format is not available; private-data", "safe");
    assert!(error.message.contains("JavaScript runtime"));
    assert!(!error.message.contains("private-data"));
}
