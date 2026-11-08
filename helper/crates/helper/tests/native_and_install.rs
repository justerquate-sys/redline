#![allow(clippy::unwrap_used)]
use base64::{Engine as _, engine::general_purpose::STANDARD};
use scribe_helper::{install, native, service::HelperLock};
use serde_json::{Value, json};
use std::io::{Cursor, Read};

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("extension")).unwrap();
    std::fs::write(
        root.path().join("extension/manifest.json"),
        json!({"key":STANDARD.encode(b"synthetic public extension key")}).to_string(),
    )
    .unwrap();
    root
}
#[test]
fn chrome_frames_are_little_endian_bounded_strict_and_support_partial_reads() {
    struct OneByte(Cursor<Vec<u8>>);
    impl Read for OneByte {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(1);
            self.0.read(&mut buf[..n])
        }
    }
    let mut bytes = Vec::new();
    native::write_message(&mut bytes, &json!({"action":"getConnection"})).unwrap();
    assert_eq!(
        u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize,
        bytes.len() - 4
    );
    assert_eq!(
        native::read_message(&mut OneByte(Cursor::new(bytes))).unwrap(),
        json!({"action":"getConnection"})
    );
    for bad in [
        vec![],
        vec![1, 0],
        0_u32.to_le_bytes().to_vec(),
        16_385_u32.to_le_bytes().to_vec(),
        vec![8, 0, 0, 0, b'{'],
    ] {
        assert!(native::read_message(&mut Cursor::new(bad)).is_err());
    }
    for value in [
        json!(null),
        json!([]),
        json!({"action":"getConnection","token":"secret"}),
        json!({"action":"serve"}),
        json!({"action":1}),
    ] {
        let mut frame = Vec::new();
        native::write_message(&mut frame, &value).unwrap();
        assert!(native::read_message(&mut Cursor::new(frame)).is_err());
    }
}
#[test]
fn extension_identity_is_publickey_derived_and_origin_checked_before_credentials() {
    let root = root();
    let id = native::extension_id(root.path()).unwrap();
    assert_eq!(id.len(), 32);
    assert!(id.bytes().all(|b| (b'a'..=b'p').contains(&b)));
    std::fs::create_dir(root.path().join(".env")).unwrap();
    let unauthorized = native::pairing_token(
        root.path(),
        "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/",
    )
    .unwrap_err();
    assert_eq!(unauthorized.status, 403);
    std::fs::remove_dir(root.path().join(".env")).unwrap();
    std::fs::write(
        root.path().join(".env"),
        "AZURE_SPEECH_KEY='private-not-returned'\nHELPER_TOKEN='pairing-only'\n",
    )
    .unwrap();
    assert_eq!(
        native::pairing_token(root.path(), &format!("chrome-extension://{id}/")).unwrap(),
        "pairing-only"
    );
    for origin in [
        format!("chrome-extension://{id}"),
        format!("chrome-extension://{id}/extra"),
        "https://youtube.com/".into(),
    ] {
        assert_eq!(
            native::pairing_token(root.path(), &origin)
                .unwrap_err()
                .status,
            403
        );
    }
}
#[test]
fn installer_keeps_credentials_and_uses_only_binary_runtime_in_isolated_home() {
    let root = root();
    let home = tempfile::tempdir().unwrap();
    let binary = root.path().join("synthetic-binary");
    std::fs::write(&binary, b"synthetic Rust executable").unwrap();
    let env =
        "AZURE_SPEECH_KEY='never-printed'\nHELPER_TOKEN='keep-existing'\nAUDIO_SPEED='1.25'\n";
    std::fs::write(root.path().join(".env"), env).unwrap();
    let (agent, host) = install::install_files(root.path(), home.path(), &binary).unwrap();
    assert_eq!(
        std::fs::read_to_string(root.path().join(".env")).unwrap(),
        env
    );
    let manifest: Value = serde_json::from_slice(&std::fs::read(&host).unwrap()).unwrap();
    assert_eq!(manifest["name"], "com.fulltranscript.helper");
    assert_eq!(manifest["type"], "stdio");
    assert!(
        manifest["path"]
            .as_str()
            .unwrap()
            .ends_with(".data/bin/scribe-native-host")
    );
    assert_eq!(
        manifest["allowed_origins"][0],
        format!(
            "chrome-extension://{}/",
            native::extension_id(root.path()).unwrap()
        )
    );
    assert!(
        host.to_string_lossy()
            .contains("Google/Chrome/NativeMessagingHosts")
    );
    let config = plist::Value::from_file(&agent).unwrap();
    let args = config.as_dictionary().unwrap()["ProgramArguments"]
        .as_array()
        .unwrap();
    assert_eq!(args.last().unwrap().as_string(), Some("serve"));
    let serialized = std::fs::read_to_string(&agent).unwrap();
    assert!(!serialized.contains("python"));
    assert!(!serialized.contains("never-printed"));
    assert!(serialized.contains("Standard"));
    assert_eq!(
        std::fs::read(root.path().join(".data/bin/scribe-native-host")).unwrap(),
        b"synthetic Rust executable"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [root.path().join(".env"), host, agent] {
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            std::fs::metadata(root.path().join(".data/bin/scribe-helper"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
}
#[tokio::test]
async fn uninstall_preserves_history_and_credentials() {
    let root = root();
    let home = tempfile::tempdir().unwrap();
    let binary = root.path().join("binary");
    std::fs::write(&binary, b"test").unwrap();
    let (agent, host) = install::install_files(root.path(), home.path(), &binary).unwrap();
    let token = native::pairing_token(
        root.path(),
        &format!(
            "chrome-extension://{}/",
            native::extension_id(root.path()).unwrap()
        ),
    )
    .unwrap();
    assert!(token.len() >= 32);
    let history = root.path().join(".data/history.json");
    std::fs::write(&history, b"history stays").unwrap();
    install::uninstall(root.path(), home.path(), false)
        .await
        .unwrap();
    assert!(!agent.exists());
    assert!(!host.exists());
    assert_eq!(std::fs::read(history).unwrap(), b"history stays");
    assert!(root.path().join(".env").is_file());
}
#[test]
fn startup_lock_prevents_second_worker_and_never_cleans_before_lock() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    let audio = work.join("active.wav");
    std::fs::write(&audio, b"owned active media").unwrap();
    let lock = HelperLock::acquire(root.path()).unwrap();
    assert!(HelperLock::acquire(root.path()).is_err());
    assert_eq!(std::fs::read(&audio).unwrap(), b"owned active media");
    drop(lock);
    assert!(HelperLock::acquire(root.path()).is_ok());
}

#[test]
fn corrupt_library_does_not_prevent_owned_temporary_audio_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("stale.wav"), b"transient audio").unwrap();
    std::fs::write(root.path().join("library.json"), b"{broken").unwrap();
    let lock = HelperLock::acquire(root.path()).unwrap();
    scribe_helper::service::cleanup_work(&lock, root.path()).unwrap();
    assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    assert_eq!(
        std::fs::read(root.path().join("library.json")).unwrap(),
        b"{broken"
    );
}
#[cfg(unix)]
#[test]
fn lock_and_installer_refuse_sensitive_symlink_files() {
    let root = root();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("keep");
    std::fs::write(&target, b"preserve").unwrap();
    let data = root.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::os::unix::fs::symlink(&target, data.join(".helper.lock")).unwrap();
    assert!(HelperLock::acquire(&data).is_err());
    std::os::unix::fs::symlink(&target, root.path().join(".env")).unwrap();
    let fake = root.path().join("binary");
    std::fs::write(&fake, b"test").unwrap();
    let home = tempfile::tempdir().unwrap();
    assert!(install::install_files(root.path(), home.path(), &fake).is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"preserve");
}
