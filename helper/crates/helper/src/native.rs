//! Chrome's framed stdio protocol. No diagnostic output is written to stdout.
use crate::{HELPER_URL, HOST_NAME};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use scribe_common::{Error, MODEL, Result, VERSION};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

pub const MAX_REQUEST_BYTES: usize = 16_384;
pub const COPY_SCRIPT: &str = r#"ObjC.import('AppKit');
function run(argv) {
 if (argv.length !== 1) throw new Error('Expected one attachment');
 var url = $.NSURL.fileURLWithPath($(argv[0]));
 var board = $.NSPasteboard.generalPasteboard;
 board.clearContents;
 if (!board.writeObjects($.NSArray.arrayWithObject(url))) throw new Error('File clipboard write failed');
 return 'copied';
}"#;

pub fn extension_id(root: &Path) -> Result<String> {
    let manifest = scribe_common::read_json(&root.join("extension/manifest.json"))?;
    let key = manifest["key"]
        .as_str()
        .ok_or_else(|| Error::internal("The extension public key is missing."))?;
    let bytes = STANDARD
        .decode(key)
        .map_err(|_| Error::internal("The extension public key is invalid."))?;
    if bytes.is_empty() {
        return Err(Error::internal("The extension public key is invalid."));
    }
    Ok(Sha256::digest(bytes)[..16]
        .iter()
        .flat_map(|b| [char::from(b'a' + (b >> 4)), char::from(b'a' + (b & 15))])
        .collect())
}

pub fn read_message<R: Read>(reader: &mut R) -> Result<Value> {
    let mut header = [0_u8; 4];
    reader
        .read_exact(&mut header)
        .map_err(|_| Error::bad_request("Missing native message header."))?;
    let length = u32::from_le_bytes(header) as usize;
    if length == 0 || length > MAX_REQUEST_BYTES {
        return Err(Error::bad_request("Invalid native message size."));
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|_| Error::bad_request("Incomplete native message."))?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| Error::bad_request("Invalid native message."))?;
    if value.as_object().is_none_or(|v| {
        v.len() != 1 || v.get("action").and_then(Value::as_str) != Some("getConnection")
    }) {
        return Err(Error::bad_request("Unsupported native message."));
    }
    Ok(value)
}

pub fn write_message<W: Write>(writer: &mut W, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn check_origin(root: &Path, origin: &str) -> Result<()> {
    if origin != format!("chrome-extension://{}/", extension_id(root)?) {
        return Err(Error::new(
            403,
            "This extension is not allowed to pair with the helper.",
        ));
    }
    Ok(())
}

pub fn pairing_token(root: &Path, origin: &str) -> Result<String> {
    check_origin(root, origin)?; // Never load credentials before validating the browser-supplied origin.
    let values = scribe_common::config::read_settings_file(root)
        .map_err(|_| {
            Error::conflict(
                "The helper settings could not be read. Run Install Helper.command to repair them.",
            )
        })?
        .map(|(_, values)| values)
        .unwrap_or_default();
    if let Some(token) = values
        .get("HELPER_TOKEN")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        return Ok(token.to_owned());
    }

    Err(Error::conflict(
        "The helper has not been installed. Run Install Helper.command once.",
    ))
}

async fn helper_running(client: &reqwest::Client) -> bool {
    helper_running_at(client, HELPER_URL).await
}
async fn helper_running_at(client: &reqwest::Client, base: &str) -> bool {
    let Ok(mut response) = client.get(format!("{base}/health")).send().await else {
        return false;
    };
    if !response.status().is_success() {
        return false;
    }
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len().saturating_add(chunk.len()) <= 64 * 1024 => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(None) => break,
            _ => return false,
        }
    }
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return false;
    };
    value["status"] == "ok" && value["model"] == MODEL && value["provider"] == "azure"
}

pub async fn ensure_helper_running() -> Result<()> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(750))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| Error::internal("The local helper could not connect."))?;
    if helper_running(&client).await {
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        let mut command = tokio::process::Command::new("/bin/launchctl");
        command.args([
            "kickstart",
            &format!("gui/{}/{HOST_NAME}", unsafe { libc::getuid() }),
        ]);
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), command.status()).await;
    }
    for _ in 0..20 {
        if helper_running(&client).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    Err(Error::new(
        503,
        "The local helper could not start. Run Install Helper.command once to repair it.",
    ))
}

pub async fn run_host(root: &Path, origin: &str) -> Result<bool> {
    let result = async {
        check_origin(root, origin)?;
        read_message(&mut std::io::stdin().lock())?;
        let token = pairing_token(root, origin)?;
        ensure_helper_running().await?;
        Ok::<_, Error>(json!({"ok":true,"pairingToken":token,"url":HELPER_URL,"version":VERSION}))
    }
    .await;
    let success = result.is_ok();
    let value = result.unwrap_or_else(|error| json!({"ok":false,"error":error.message}));
    write_message(&mut std::io::stdout().lock(), &value)?;
    Ok(success)
}

async fn native_output(
    program: &str,
    args: &[&std::ffi::OsStr],
    timeout: Duration,
    message: &str,
) -> Result<std::process::Output> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn().map_err(|_| Error::new(502, message))?;
    // kill_on_drop makes timeout and request cancellation terminate the owned child.
    tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .map_err(|_| Error::new(504, message))?
        .map_err(|_| Error::new(502, message))
}

pub async fn choose_export_directory() -> Result<Option<PathBuf>> {
    if !cfg!(target_os = "macos") {
        return Err(Error::bad_request(
            "The folder picker currently supports macOS.",
        ));
    }
    let script = "POSIX path of (choose folder with prompt \"Choose a folder for your transcript documents\")";
    let output = native_output(
        "/usr/bin/osascript",
        &["-e".as_ref(), script.as_ref()],
        Duration::from_secs(120),
        "The folder picker could not open. Try choosing the destination again.",
    )
    .await?;
    if !output.status.success() {
        if String::from_utf8_lossy(&output.stderr).contains("-128") {
            return Ok(None);
        }
        return Err(Error::bad_request(
            "The folder picker could not open. Try choosing the destination again.",
        ));
    }
    let selected = String::from_utf8(output.stdout)
        .map_err(|_| Error::bad_request("The destination folder could not be selected."))?;
    let selected = selected.trim();
    Ok((!selected.is_empty()).then(|| PathBuf::from(selected)))
}

pub async fn reveal_export_file(path: &Path) -> Result<()> {
    reveal(path, true).await
}
pub async fn reveal_export_directory(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || std::fs::canonicalize(path).ok().as_deref() != Some(path)
        || !path.is_dir()
    {
        return Err(Error::conflict(
            "The selected destination folder is unavailable. Choose it again in settings.",
        ));
    }
    reveal(path, false).await
}
async fn reveal(path: &Path, select: bool) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(Error::bad_request(
            "Showing saved files currently supports macOS.",
        ));
    }
    let args: Vec<&std::ffi::OsStr> = if select {
        vec!["-R".as_ref(), path.as_os_str()]
    } else {
        vec![path.as_os_str()]
    };
    let output = native_output(
        "/usr/bin/open",
        &args,
        Duration::from_secs(10),
        "Finder could not show the saved file. Try again.",
    )
    .await?;
    if !output.status.success() {
        return Err(Error::new(
            502,
            "Finder could not show the saved file. Try again.",
        ));
    }
    Ok(())
}
pub async fn copy_attachment_file(path: &Path) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(Error::bad_request(
            "File attachment copying currently supports macOS. Use Show file to attach the document.",
        ));
    }
    let output = native_output(
        "/usr/bin/osascript",
        &[
            "-l".as_ref(),
            "JavaScript".as_ref(),
            "-e".as_ref(),
            COPY_SCRIPT.as_ref(),
            path.as_os_str(),
        ],
        Duration::from_secs(30),
        "macOS could not confirm the file attachment copy. Use Show file or try again.",
    )
    .await?;
    if !output.status.success() || output.stdout.trim_ascii() != b"copied" {
        return Err(Error::new(
            502,
            "macOS could not confirm the file attachment copy. Use Show file to attach it instead.",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, String) {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("extension")).unwrap();
        std::fs::write(
            root.path().join("extension/manifest.json"),
            json!({"key":STANDARD.encode(b"public offline native fixture")}).to_string(),
        )
        .unwrap();
        let origin = format!("chrome-extension://{}/", extension_id(root.path()).unwrap());
        (root, origin)
    }
    #[test]
    fn pairing_rejects_malformed_trailing_settings_before_returning_any_token() {
        let (root, origin) = fixture();
        for text in [
            "HELPER_TOKEN='never-return-this-token'\nnot valid settings\n",
            "HELPER_TOKEN='never-return-this-token'\nOTHER='unterminated\n",
        ] {
            std::fs::write(root.path().join(".env"), text).unwrap();
            let error = pairing_token(root.path(), &origin).unwrap_err();
            assert_eq!(error.status, 409);
            assert!(!error.message.contains("never-return"));
            assert_eq!(
                std::fs::read_to_string(root.path().join(".env")).unwrap(),
                text
            );
        }
        std::fs::write(
            root.path().join(".env"),
            b"HELPER_TOKEN='never-return-this-token'\n\xff",
        )
        .unwrap();
        assert!(pairing_token(root.path(), &origin).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn pairing_never_reads_symlinked_credentials_or_replaces_the_link() {
        let (root, origin) = fixture();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("private-settings");
        std::fs::write(&target, "HELPER_TOKEN='never-read-outside-token'\n").unwrap();
        std::os::unix::fs::symlink(&target, root.path().join(".env")).unwrap();
        let error = pairing_token(root.path(), &origin).unwrap_err();
        assert_eq!(error.status, 409);
        assert!(!error.message.contains("never-read"));
        assert!(!error.message.contains(outside.path().to_str().unwrap()));
        assert!(
            std::fs::symlink_metadata(root.path().join(".env"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_file(&target).unwrap();
        assert!(pairing_token(root.path(), &origin).is_err());
    }
    #[test]
    fn persisted_values_are_fully_parsed_with_no_process_environment_override() {
        let (root, origin) = fixture();
        let text = "# Preserve all formatting\nHELPER_TOKEN='old-literal-token'\nPATH='literal-local-value'\nHELPER_TOKEN='final-literal-token'\n";
        std::fs::write(root.path().join(".env"), text).unwrap();
        let (raw, values) = scribe_common::config::read_settings_file(root.path())
            .unwrap()
            .unwrap();
        assert_eq!(raw, text);
        assert_eq!(values.get("PATH").unwrap(), "literal-local-value");
        assert_eq!(
            pairing_token(root.path(), &origin).unwrap(),
            "final-literal-token"
        );
    }
    #[tokio::test]
    async fn startup_health_requires_bounded_successful_azure_response() {
        use axum::{
            Router,
            body::Body,
            http::{Request, StatusCode},
            routing::any,
        };
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()
            .unwrap();
        let good = json!({"status":"ok","model":MODEL,"provider":"azure"}).to_string();
        for (status, body, expected) in [
            (StatusCode::OK, good.clone(), true),
            (StatusCode::INTERNAL_SERVER_ERROR, good.clone(), false),
            (StatusCode::TEMPORARY_REDIRECT, good, false),
            (
                StatusCode::OK,
                json!({"status":"ok","model":MODEL,"provider":"other"}).to_string(),
                false,
            ),
            (StatusCode::OK, "not-json".into(), false),
            (StatusCode::OK, "x".repeat(65537), false),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let router = Router::new().fallback(any(move |request: Request<Body>| {
                assert_eq!(request.uri().path(), "/health");
                assert!(request.headers().get("authorization").is_none());
                let body = body.clone();
                async move { (status, body) }
            }));
            let task = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            assert_eq!(helper_running_at(&client, &base).await, expected);
            task.abort();
            let _ = task.await;
        }
    }
}
