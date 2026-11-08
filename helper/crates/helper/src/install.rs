//! Explicit installation only. Importing or serving never modifies launchd.
use crate::{HOST_NAME, native::extension_id};
use scribe_common::{Error, MODEL, Result, atomic_write};
use serde_json::{Value, json};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

const INSTALL_ADDRESS: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8765);
const UNKNOWN_SERVICE: &str = "The running local helper could not be safely checked. Installation was not changed. Check the helper connection and try again.";
const ACTIVE_SERVICE: &str = "The helper is still processing or saving videos, or discovering a channel. Wait for the queue to finish before installing; the running helper was left untouched.";

fn unknown_service() -> Error {
    Error::conflict(UNKNOWN_SERVICE)
}

async fn probe_json(mut response: reqwest::Response) -> Result<Value> {
    if !response.status().is_success() {
        return Err(unknown_service());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unknown_service())? {
        if bytes.len().saturating_add(chunk.len()) > 64 * 1024 {
            return Err(unknown_service());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| unknown_service())
}

fn require_idle(snapshot: &Value, scope: &str) -> Result<()> {
    if snapshot["scope"] != scope
        || snapshot["finished"].as_bool().is_none()
        || snapshot["discovery_complete"].as_bool().is_none()
        || snapshot
            .get("discovery_unknown")
            .is_some_and(|v| v != false)
    {
        return Err(unknown_service());
    }
    let mut busy = false;
    for field in [
        "queued",
        "processing",
        "saving",
        "discovering_channels",
        "waiting_channels",
    ] {
        busy |= snapshot[field].as_u64().ok_or_else(unknown_service)? > 0;
    }
    if busy || snapshot["finished"] != true || snapshot["discovery_complete"] != true {
        return Err(Error::conflict(ACTIVE_SERVICE));
    }
    Ok(())
}

async fn preflight_install(root: &Path, address: SocketAddrV4) -> Result<()> {
    // Once a listener is observed, every subsequent failure is unknown rather
    // than "offline", including a listener that closes or exits during HTTP.
    match tokio::time::timeout(
        Duration::from_secs(2),
        tokio::net::TcpStream::connect(address),
    )
    .await
    {
        Ok(Ok(stream)) => drop(stream),
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => return Ok(()),
        _ => return Err(unknown_service()),
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unknown_service())?;
    let base = format!("http://{address}");
    let response = client
        .get(format!("{base}/health"))
        .send()
        .await
        .map_err(|_| unknown_service())?;
    let health = probe_json(response).await?;
    if health["status"] != "ok" || health["model"] != MODEL || health["provider"] != "azure" {
        return Err(unknown_service());
    }
    let (_, values) = scribe_common::config::read_settings_file(root)
        .map_err(|_| unknown_service())?
        .ok_or_else(unknown_service)?;
    let token = values
        .get("HELPER_TOKEN")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .ok_or_else(unknown_service)?;
    let response = client
        .get(format!("{base}/queue/status"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| unknown_service())?;
    let status = probe_json(response).await?;
    require_idle(&status, "all")?;
    // New helpers expose legacy/manual channel producers only in batch progress.
    if let Some(batch) = status.get("batch_progress") {
        require_idle(batch, "batch")?;
    }
    Ok(())
}

async fn prepare_install(
    root: &Path,
    home: &Path,
    binary: &Path,
    start: bool,
    address: SocketAddrV4,
) -> Result<(PathBuf, PathBuf)> {
    if start {
        preflight_install(root, address).await?;
    }
    install_files(root, home, binary)
}

pub fn registration_paths(home: &Path) -> (PathBuf, PathBuf) {
    (
        home.join("Library/LaunchAgents")
            .join(format!("{HOST_NAME}.plist")),
        home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts")
            .join(format!("{HOST_NAME}.json")),
    )
}
fn private_write(path: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    atomic_write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
        )?;
    }
    Ok(())
}
fn ensure_token(root: &Path) -> Result<()> {
    let path = root.join(".env");
    let (current,values)=scribe_common::config::read_settings_file(root)
        .map_err(|_|Error::conflict("The helper settings could not be read. Existing settings were preserved; repair the file before installing."))?
        .unwrap_or_else(||("# Configure Azure in the extension's Transcripts settings.\n".to_owned(),std::collections::HashMap::new()));
    let token = values
        .get("HELPER_TOKEN")
        .map(String::as_str)
        .unwrap_or_default();
    let mut content = current;
    if token.trim().is_empty() {
        // UUIDv4 entropy is OS-generated; two independent values provide 244 bits.
        let token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        content.push_str(&format!("\nHELPER_TOKEN='{token}'\n"));
    }
    private_write(&path, content.as_bytes(), false)
}
pub fn install_files(root: &Path, home: &Path, binary: &Path) -> Result<(PathBuf, PathBuf)> {
    let root = std::fs::canonicalize(root)?;
    let id = extension_id(&root)?;
    let data = root.join(".data");
    std::fs::create_dir_all(&data)?;
    if std::fs::symlink_metadata(&data)?.file_type().is_symlink() {
        return Err(Error::conflict(
            "The helper data folder must not be a symbolic link.",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700))?;
    }
    ensure_token(&root)?;
    let deployed = data.join("bin/scribe-helper");
    if std::fs::symlink_metadata(data.join("bin"))
        .is_ok_and(|m| !m.is_dir() || m.file_type().is_symlink())
    {
        return Err(Error::conflict(
            "The helper binary folder must not be a symbolic link.",
        ));
    }
    // Copy atomically so a running old executable is not truncated in place.
    if std::fs::canonicalize(binary).ok() != std::fs::canonicalize(&deployed).ok() {
        private_write(&deployed, &std::fs::read(binary)?, true)?;
    }
    let native = data.join("bin/scribe-native-host");
    #[cfg(unix)]
    {
        let temporary = data.join(format!("bin/.native-{}", uuid::Uuid::new_v4().simple()));
        std::os::unix::fs::symlink("scribe-helper", &temporary)?;
        if let Err(error) = std::fs::rename(&temporary, &native) {
            let _ = std::fs::remove_file(temporary);
            return Err(error.into());
        }
    }
    #[cfg(not(unix))]
    private_write(&native, &std::fs::read(binary)?, true)?;
    let (agent, host) = registration_paths(home);
    private_write(
        &host,
        &serde_json::to_vec_pretty(
            &json!({"name":HOST_NAME,"description":"Redline transcript helper connection","path":native,"type":"stdio","allowed_origins":[format!("chrome-extension://{id}/")]}),
        )?,
        false,
    )?;
    let log = data.join("helper.log");
    if !log.exists() {
        private_write(&log, b"", false)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::symlink_metadata(&log)?.file_type().is_symlink() {
            return Err(Error::conflict(
                "The helper log must not be a symbolic link.",
            ));
        }
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o600))?;
    }
    let mut configuration = plist::Dictionary::new();
    configuration.insert("Label".into(), HOST_NAME.into());
    configuration.insert(
        "ProgramArguments".into(),
        plist::Value::Array(
            [
                deployed.to_string_lossy().as_ref(),
                "--root",
                root.to_string_lossy().as_ref(),
                "serve",
            ]
            .into_iter()
            .map(|v| plist::Value::String(v.to_owned()))
            .collect(),
        ),
    );
    configuration.insert(
        "WorkingDirectory".into(),
        root.to_string_lossy().into_owned().into(),
    );
    configuration.insert("RunAtLoad".into(), true.into());
    configuration.insert("KeepAlive".into(), true.into());
    configuration.insert("ProcessType".into(), "Standard".into());
    configuration.insert("ThrottleInterval".into(), 10_i64.into());
    configuration.insert(
        "StandardOutPath".into(),
        log.to_string_lossy().into_owned().into(),
    );
    configuration.insert(
        "StandardErrorPath".into(),
        log.to_string_lossy().into_owned().into(),
    );
    let mut bytes = Vec::new();
    plist::Value::Dictionary(configuration)
        .to_writer_xml(&mut bytes)
        .map_err(|_| {
            Error::internal("The background service configuration could not be written.")
        })?;
    private_write(&agent, &bytes, false)?;
    remove_legacy_registration(&root, home)?;
    Ok((agent, host))
}
fn remove_legacy_registration(root: &Path, home: &Path) -> Result<()> {
    let path = home
        .join("Library/Application Support/BraveSoftware/Brave-Browser/NativeMessagingHosts")
        .join(format!("{HOST_NAME}.json"));
    if let Ok(value) = scribe_common::read_json(&path)
        && value["name"] == HOST_NAME
        && value["path"].as_str() == root.join(".data/native-host").to_str()
    {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
async fn launchctl(args: &[String]) -> Result<bool> {
    let mut command = tokio::process::Command::new("/bin/launchctl");
    command
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    Ok(
        tokio::time::timeout(Duration::from_secs(10), command.status())
            .await
            .map_err(|_| Error::internal("The background service manager timed out."))?
            .map_err(|_| Error::internal("The background service manager could not run."))?
            .success(),
    )
}
pub async fn install(root: &Path, home: &Path, binary: &Path, start: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(Error::bad_request(
            "Automatic helper installation currently supports macOS. Use scribe-helper serve for manual startup.",
        ));
    }
    let (agent, _) = prepare_install(root, home, binary, start, INSTALL_ADDRESS).await?;
    if start {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        let _ = launchctl(&["bootout".into(), format!("{domain}/{HOST_NAME}")]).await;
        for _ in 0..40 {
            if launchctl(&[
                "bootstrap".into(),
                domain.clone(),
                agent.to_string_lossy().into_owned(),
            ])
            .await?
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        return Err(Error::internal(
            "The helper files were installed, but automatic startup could not finish. Run Install Helper.command again.",
        ));
    }
    Ok(())
}
pub async fn uninstall(root: &Path, home: &Path, stop: bool) -> Result<()> {
    if stop && cfg!(target_os = "macos") {
        let _ = launchctl(&[
            "bootout".into(),
            format!("gui/{}/{HOST_NAME}", unsafe { libc::getuid() }),
        ])
        .await;
    }
    let (agent, host) = registration_paths(home);
    for path in [agent, host] {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    remove_legacy_registration(root, home)?;
    Ok(()) // Transcripts, credentials and cache are deliberately preserved.
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
        response::IntoResponse,
        routing::any,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use std::sync::{Arc, Mutex};

    type ObservedCalls = Arc<Mutex<Vec<(String, Option<String>)>>>;
    struct Probe {
        address: SocketAddrV4,
        calls: ObservedCalls,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    async fn mock(health: (StatusCode, String), status: (StatusCode, String)) -> Probe {
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, listener.local_addr().unwrap().port());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let router = Router::new().fallback(any(move |request: Request<Body>| {
            let seen = seen.clone();
            let response = if request.uri().path() == "/health" {
                health.clone()
            } else {
                status.clone()
            };
            async move {
                assert_eq!(request.method(), "GET");
                seen.lock().unwrap().push((
                    request.uri().path().to_owned(),
                    request
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned),
                ));
                response.into_response()
            }
        }));
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Probe {
            address,
            calls,
            task,
        }
    }
    fn health() -> (StatusCode, String) {
        (
            StatusCode::OK,
            json!({"status":"ok","provider":"azure","model":MODEL}).to_string(),
        )
    }
    fn idle() -> Value {
        json!({"scope":"all","queued":0,"processing":0,"saving":0,"discovering_channels":0,"waiting_channels":0,"discovery_complete":true,"finished":true,"failed":4,"unsaved":2})
    }
    fn root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("extension")).unwrap();
        std::fs::write(
            root.path().join("extension/manifest.json"),
            json!({"key":STANDARD.encode(b"synthetic public extension key")}).to_string(),
        )
        .unwrap();
        std::fs::write(
            root.path().join(".env"),
            "HELPER_TOKEN='synthetic-pairing-token'\nAZURE_SPEECH_KEY='synthetic-never-return'\n",
        )
        .unwrap();
        std::fs::write(root.path().join("binary"), b"synthetic binary").unwrap();
        root
    }

    #[tokio::test]
    async fn busy_preflight_never_changes_configuration_or_registrations() {
        for field in [
            "queued",
            "processing",
            "saving",
            "discovering_channels",
            "waiting_channels",
            "unfinished",
            "batch",
        ] {
            let root = root();
            let home = tempfile::tempdir().unwrap();
            let (agent, host) = registration_paths(home.path());
            for path in [&agent, &host] {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b"keep registration exactly").unwrap();
            }
            let env = std::fs::read(root.path().join(".env")).unwrap();
            let mut status = idle();
            match field {
                "unfinished" => status["finished"] = json!(false),
                "batch" => {
                    let mut batch = idle();
                    batch["scope"] = json!("batch");
                    batch["waiting_channels"] = json!(1);
                    status["batch_progress"] = batch;
                }
                _ => status[field] = json!(1),
            }
            let probe = mock(health(), (StatusCode::OK, status.to_string())).await;
            let error = prepare_install(
                root.path(),
                home.path(),
                &root.path().join("binary"),
                true,
                probe.address,
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, 409, "{field}");
            assert!(error.message.contains("Wait for the queue"), "{field}");
            assert!(!root.path().join(".data").exists());
            assert_eq!(std::fs::read(root.path().join(".env")).unwrap(), env);
            for path in [agent, host] {
                assert_eq!(std::fs::read(path).unwrap(), b"keep registration exactly");
            }
            assert_eq!(
                *probe.calls.lock().unwrap(),
                vec![
                    ("/health".into(), None),
                    (
                        "/queue/status".into(),
                        Some("Bearer synthetic-pairing-token".into())
                    )
                ]
            );
        }
    }

    #[tokio::test]
    async fn authenticated_idle_allows_file_install_despite_old_settled_errors() {
        let root = root();
        let home = tempfile::tempdir().unwrap();
        let mut status = idle();
        let mut batch = idle();
        batch["scope"] = json!("batch");
        status["batch_progress"] = batch;
        let probe = mock(health(), (StatusCode::OK, status.to_string())).await;
        let (agent, host) = prepare_install(
            root.path(),
            home.path(),
            &root.path().join("binary"),
            true,
            probe.address,
        )
        .await
        .unwrap();
        assert!(agent.is_file() && host.is_file());
        assert_eq!(probe.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn offline_refused_connection_allows_first_install_without_credentials() {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, listener.local_addr().unwrap().port());
        drop(listener);
        preflight_install(root.path(), address).await.unwrap();
        assert!(!root.path().join(".env").exists());
    }

    #[tokio::test]
    async fn authentication_mismatch_and_missing_token_fail_closed_without_secrets() {
        for code in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
            let root = root();
            let probe = mock(health(), (code, "synthetic-never-return".into())).await;
            let error = preflight_install(root.path(), probe.address)
                .await
                .unwrap_err();
            assert_eq!(error.status, 409);
            assert!(!error.message.contains("synthetic"));
        }
        let root = tempfile::tempdir().unwrap();
        let probe = mock(health(), (StatusCode::OK, idle().to_string())).await;
        assert_eq!(
            preflight_install(root.path(), probe.address)
                .await
                .unwrap_err()
                .status,
            409
        );
        assert_eq!(*probe.calls.lock().unwrap(), vec![("/health".into(), None)]);
    }

    #[tokio::test]
    async fn observed_listener_exiting_before_health_is_unknown_not_offline() {
        let root = root();
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, listener.local_addr().unwrap().port());
        let task = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.unwrap();
            drop(listener);
            drop(connection);
        });
        let error = preflight_install(root.path(), address).await.unwrap_err();
        task.await.unwrap();
        assert_eq!(error.status, 409);
        assert_eq!(error.message, UNKNOWN_SERVICE);
    }

    #[tokio::test]
    async fn unrecognized_listener_is_never_sent_the_pairing_token() {
        for response in [
            (
                StatusCode::OK,
                json!({"status":"ok","model":"other","provider":"azure"}).to_string(),
            ),
            (StatusCode::OK, "not json".into()),
            (StatusCode::OK, "x".repeat(65_537)),
            (StatusCode::TEMPORARY_REDIRECT, String::new()),
            (StatusCode::INTERNAL_SERVER_ERROR, String::new()),
        ] {
            let root = root();
            let probe = mock(response, (StatusCode::OK, idle().to_string())).await;
            assert_eq!(
                preflight_install(root.path(), probe.address)
                    .await
                    .unwrap_err()
                    .status,
                409
            );
            assert_eq!(*probe.calls.lock().unwrap(), vec![("/health".into(), None)]);
        }
    }

    #[tokio::test]
    async fn unknown_activity_is_not_treated_as_idle() {
        let mut cases = vec![json!({}), json!(null)];
        for field in [
            "queued",
            "processing",
            "saving",
            "discovering_channels",
            "waiting_channels",
            "finished",
            "discovery_complete",
        ] {
            let mut missing = idle();
            missing.as_object_mut().unwrap().remove(field);
            cases.push(missing);
            let mut malformed = idle();
            malformed[field] = json!("0");
            cases.push(malformed);
        }
        let mut legacy = idle();
        legacy["discovery_unknown"] = json!(true);
        cases.push(legacy);
        for status in cases {
            let root = root();
            let probe = mock(health(), (StatusCode::OK, status.to_string())).await;
            assert_eq!(
                preflight_install(root.path(), probe.address)
                    .await
                    .unwrap_err()
                    .status,
                409
            );
        }
    }

    #[tokio::test]
    async fn no_start_file_install_never_probes_any_listener() {
        let root = root();
        let home = tempfile::tempdir().unwrap();
        let probe = mock(
            (StatusCode::FORBIDDEN, String::new()),
            (StatusCode::FORBIDDEN, String::new()),
        )
        .await;
        let (agent, host) = prepare_install(
            root.path(),
            home.path(),
            &root.path().join("binary"),
            false,
            probe.address,
        )
        .await
        .unwrap();
        assert!(agent.is_file() && host.is_file());
        assert!(probe.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn malformed_settings_are_never_silently_repaired_or_partially_loaded() {
        let root = root();
        let home = tempfile::tempdir().unwrap();
        let text = "# Existing preferences\nHELPER_TOKEN='keep-private-token'\nAZURE_SPEECH_KEY='keep-private-key'\nnot valid settings\n";
        std::fs::write(root.path().join(".env"), text).unwrap();
        let error =
            install_files(root.path(), home.path(), &root.path().join("binary")).unwrap_err();
        assert_eq!(error.status, 409);
        assert!(!error.message.contains("keep-private"));
        assert_eq!(
            std::fs::read_to_string(root.path().join(".env")).unwrap(),
            text
        );
        let (agent, host) = registration_paths(home.path());
        assert!(!agent.exists() && !host.exists());
        assert!(!root.path().join(".data/bin/scribe-helper").exists());
    }

    #[tokio::test]
    async fn preflight_never_sends_token_from_malformed_or_symlinked_settings() {
        let root = root();
        let env = root.path().join(".env");
        std::fs::write(
            &env,
            "HELPER_TOKEN='must-never-be-sent'\nnot valid settings\n",
        )
        .unwrap();
        let probe = mock(health(), (StatusCode::OK, idle().to_string())).await;
        assert!(preflight_install(root.path(), probe.address).await.is_err());
        assert_eq!(*probe.calls.lock().unwrap(), vec![("/health".into(), None)]);
        #[cfg(unix)]
        {
            std::fs::remove_file(&env).unwrap();
            let outside = tempfile::tempdir().unwrap();
            let target = outside.path().join("credentials");
            std::fs::write(&target, "HELPER_TOKEN='outside-token-never-sent'\n").unwrap();
            std::os::unix::fs::symlink(&target, &env).unwrap();
            let probe = mock(health(), (StatusCode::OK, idle().to_string())).await;
            assert!(preflight_install(root.path(), probe.address).await.is_err());
            assert_eq!(*probe.calls.lock().unwrap(), vec![("/health".into(), None)]);
            assert!(ensure_token(root.path()).is_err());
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                "HELPER_TOKEN='outside-token-never-sent'\n"
            );
            assert!(
                std::fs::symlink_metadata(&env)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
    }

    #[test]
    fn token_installation_preserves_valid_existing_settings_exactly() {
        let root = root();
        let path = root.path().join(".env");
        let text = "# Comment with Unicode: ü\nAZURE_SPEECH_KEY='preserved-private-key'\nHELPER_TOKEN='preserved-token'\nYTDLP_COOKIES_BROWSER='brave'\nCUSTOM_OPTION='some value'\n";
        std::fs::write(&path, text).unwrap();
        ensure_token(root.path()).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let missing = "# Comment\nCUSTOM_OPTION='do not change'\n";
        std::fs::write(&path, missing).unwrap();
        ensure_token(root.path()).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(missing));
        let (_, values) = scribe_common::config::read_settings_file(root.path())
            .unwrap()
            .unwrap();
        assert_eq!(values["CUSTOM_OPTION"], "do not change");
        assert_eq!(values["HELPER_TOKEN"].len(), 64);
    }
}
