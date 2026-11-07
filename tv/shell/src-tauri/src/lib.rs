//! Redline for TV: the Google TV / Android TV app. A thin Tauri shell around the shared client
//! (`src/`, `browser/`, `extension/`): it opens https://www.youtube.com in the WebView with the
//! client injected at document start (as the browser extension does) and gives it what the
//! extension's runtime would — see `shell.rs` (commands, updates), `host.rs` (Android),
//! `script.rs` (the injected script) and `tv/core` (the logic, tested on the host). The Kotlin side
//! is `gen/android/app/src/main/java/app/lumen/tv/` (activity hooks, Keystore, installer).

mod host;
mod script;
mod shell;

use tauri::{WebviewUrl, WebviewWindowBuilder};

/// The page's one command: `YTC_SHELL.call(cmd, json)` (tv/shim/shim.js). Network work runs off the
/// main thread.
#[tauri::command]
async fn ytc(cmd: String, json: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || match shell::get() {
        Some(s) => s.call(&cmd, &json),
        None => Err("shell not ready".into()),
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![ytc])
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle()
                    .plugin(tauri_plugin_log::Builder::default().level(log::LevelFilter::Info).build())?;
            }
            // Android: MainActivity has called Native.init(filesDir) before Tauri started. Elsewhere
            // (desktop development) the app data directory stands in.
            if shell::get().is_none() {
                use tauri::Manager;
                shell::init(app.path().app_data_dir()?);
            }
            let url = "https://www.youtube.com/".parse().expect("url");
            let b = WebviewWindowBuilder::new(app, "main", WebviewUrl::External(url))
                .title("Redline")
                .background_color(tauri::window::Color(0x0f, 0x0f, 0x0f, 0xff));
            #[cfg(desktop)]
            let b = b
                .initialization_script(shell::get().map(|s| s.script()).unwrap_or_default())
                .user_agent(&script::user_agent(""))
                .inner_size(960.0, 540.0);
            b.build()?;
            shell::spawn_updates();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Redline");
}
