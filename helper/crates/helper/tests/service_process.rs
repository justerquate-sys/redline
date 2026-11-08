#![allow(clippy::unwrap_used)]
#![cfg(feature = "engine")]
use scribe_common::Settings;
use scribe_helper::{native, service};
use serde_json::{Value, json};
use std::{io::Cursor, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn occupied_port_never_starts_recovery_or_cleans_audio() {
    let root = tempfile::tempdir().unwrap();
    let settings = Settings::load(root.path(), None).unwrap();
    let work = settings.data_dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("keep.wav"), b"another instance may own this").unwrap();
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let error = service::serve(settings.clone(), port).await.unwrap_err();
    assert_eq!(error.status, 409);
    assert_eq!(
        std::fs::read(work.join("keep.wav")).unwrap(),
        b"another instance may own this"
    );
    assert!(!settings.data_dir.join("jobs").exists());
    assert!(!settings.data_dir.join("current-batch.json").exists());
}

#[tokio::test]
async fn corrupt_library_startup_still_deletes_only_transient_audio() {
    let root = tempfile::tempdir().unwrap();
    let settings = Settings::load(root.path(), None).unwrap();
    let work = settings.data_dir.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("stale.wav"), b"temporary audio").unwrap();
    std::fs::write(settings.data_dir.join("library.json"), b"{broken").unwrap();
    assert!(service::serve(settings.clone(), 0).await.is_err());
    assert_eq!(std::fs::read_dir(&work).unwrap().count(), 0);
    assert_eq!(
        std::fs::read(settings.data_dir.join("library.json")).unwrap(),
        b"{broken"
    );
    assert!(service::HelperLock::acquire(&settings.data_dir).is_ok());
}

#[tokio::test]
async fn real_binary_serves_an_isolated_loopback_api_and_shuts_down_cleanly() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join(".env"),
        "HELPER_TOKEN='isolated-synthetic-token'\n",
    )
    .unwrap();
    let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_scribe-helper"))
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "serve",
            "--port",
            &port.to_string(),
        ])
        .env_remove("HELPER_TOKEN")
        .env_remove("DATA_DIR")
        .env_remove("AZURE_SPEECH_KEY")
        .env_remove("AZURE_SPEECH_ENDPOINT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let mut ready = false;
    for _ in 0..100 {
        assert!(
            child.try_wait().unwrap().is_none(),
            "isolated helper exited before startup"
        );
        if client
            .get(format!("{base}/health"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert!(ready, "isolated helper did not become ready");
    assert_eq!(
        client
            .get(format!("{base}/jobs"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let jobs: Value = client
        .get(format!("{base}/jobs"))
        .bearer_auth("isolated-synthetic-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(jobs, json!({"jobs":[]}));
    let queue: Value = client
        .get(format!("{base}/queue/status"))
        .bearer_auth("isolated-synthetic-token")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(queue["total"], 0);
    let denied = client
        .get(format!("{base}/health"))
        .header("Origin", "https://youtube.com")
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    #[cfg(unix)]
    unsafe {
        libc::kill(child.id().unwrap() as i32, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    child.kill().await.unwrap();
    let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .unwrap()
        .unwrap();
    #[cfg(unix)]
    assert!(status.success());
    assert!(service::HelperLock::acquire(&root.path().join(".data")).is_ok());
    assert_eq!(
        std::fs::read_dir(root.path().join(".data/work"))
            .unwrap()
            .count(),
        0
    );
}

#[tokio::test]
async fn real_native_binary_frames_denied_origins_without_contacting_any_service() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("extension")).unwrap();
    std::fs::write(
        root.path().join("extension/manifest.json"),
        "{\"key\":\"cHVibGljLWtleQ==\"}",
    )
    .unwrap();
    std::fs::write(
        root.path().join(".env"),
        "HELPER_TOKEN='not-exposed'\nAZURE_SPEECH_KEY='also-private'\n",
    )
    .unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_scribe-helper"))
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "native-host",
            "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut frame = Vec::new();
    native::write_message(&mut frame, &json!({"action":"getConnection"})).unwrap();
    let mut input = child.stdin.take().unwrap();
    let _ = input.write_all(&frame).await;
    drop(input);
    let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    let mut bytes = Cursor::new(output.stdout);
    let mut length = [0_u8; 4];
    std::io::Read::read_exact(&mut bytes, &mut length).unwrap();
    let mut body = vec![0; u32::from_le_bytes(length) as usize];
    std::io::Read::read_exact(&mut bytes, &mut body).unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(value["ok"], false);
    assert!(value["error"].as_str().unwrap().contains("not allowed"));
    assert!(!String::from_utf8_lossy(&body).contains("not-exposed"));
    assert!(output.stderr.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn installed_native_alias_resolves_project_without_the_callers_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("extension")).unwrap();
    std::fs::write(
        root.path().join("extension/manifest.json"),
        "{\"key\":\"cHVibGljLWtleQ==\"}",
    )
    .unwrap();
    scribe_helper::install::install_files(
        root.path(),
        home.path(),
        std::path::Path::new(env!("CARGO_BIN_EXE_scribe-helper")),
    )
    .unwrap();
    let output = tokio::process::Command::new(root.path().join(".data/bin/scribe-native-host"))
        .arg("chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/")
        .current_dir(unrelated.path())
        .env_remove("SCRIBE_ROOT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(
        output.stderr.is_empty(),
        "native invocation must reach framed protocol instead of CLI parsing errors"
    );
    assert!(output.stdout.len() > 4);
    let length = u32::from_le_bytes(output.stdout[..4].try_into().unwrap()) as usize;
    assert_eq!(length, output.stdout.len() - 4);
    let value: Value = serde_json::from_slice(&output.stdout[4..]).unwrap();
    assert_eq!(value["ok"], false);
    assert!(value["error"].as_str().unwrap().contains("not allowed"));
}
