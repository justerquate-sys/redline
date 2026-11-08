//! The logic of the TV app's shell (`tv/shell`), kept free of Tauri and Android so it is tested on
//! the host (`cargo test` here):
//!
//! - [`release`]: the client bundle the app runs (signed manifest from GitHub Releases, verified,
//!   cached, the newest of cache and the APK's built-in copy wins) and shell (APK) updates.
//! - [`auth`]: YouTube's TV sign-in (OAuth device flow of the YouTube TV client), token refresh.
//! - [`rules`]: `extension/rules.json` (the extension's declarativeNetRequest rules) for the WebView.
//! - [`store`]: `chrome.storage.local` for the client, a JSON file in the app's private storage.
//! - [`net`]: the HTTP agent (rustls, bundled roots, timeouts, statuses as values).
//! - [`readystore`]: the ready Home response kept across app starts (sealed, one account, 10 min, read once).
//! - [`tel`]: speed telemetry to the owner's debug center on their network (off unless turned on).
//! - [`incoming`]: what other apps and the system hand the app (a search, a YouTube link), validated.

pub mod auth;
pub mod incoming;
pub mod net;
pub mod readystore;
pub mod release;
pub mod rules;
pub mod store;
pub mod tel;

/// Writes `bytes` to `path` atomically (temp file + rename in the same directory).
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}
