//! The settings page (`settings.html`, the extension's options page): save folder, helper, Azure, manual pairing. This
//! is the one place an unconfigured person's browser ever talks to the helper, and only because they opened it.

use crate::html::esc;
use crate::{core, host};
use crate::ui::{disable, notice, put, send, set, show};
use serde_json::{Value, json};
use std::cell::RefCell;
use wasm_bindgen_futures::spawn_local;

#[derive(Default)]
struct S {
    saved: Option<Value>,
    busy: bool,
    export: Option<Value>,
    export_busy: bool,
    picker: bool,
    /// A settings file waiting for an answer: the keys to write, how many were left out, the secrets it brings.
    import: Option<(Vec<String>, usize, Vec<&'static str>)>,
}
thread_local! {
    static STATE: RefCell<S> = RefCell::new(S::default());
}
fn with<R>(f: impl FnOnce(&mut S) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

pub fn boot() {
    host::html("tr-root", &shell());
    spawn_local(refresh_pair());
    spawn_local(check_connection(true));
    spawn_local(load_export());
}

fn shell() -> String {
    format!(
        r##"<div class="tx-app"><header class="tx-top"><a class="tx-brand" href="reader.html">{mark}<span>Transcripts</span></a><span class="tx-gap"></span><a class="tx-b quiet" href="reader.html">Library</a></header>
<div class="tx-shell settings"><nav class="tx-nav" aria-label="Settings sections"><a href="#save-folder">Save folder</a><a href="#helper">Local helper</a><a href="#azure">Azure</a><a href="#use">Using transcripts</a><a href="#pairing">Manual pairing</a><a href="#settings-file">Settings file</a></nav>
<main class="tx-settings"><h1>Settings</h1>
<section id="switch" class="tx-panel" hidden><div class="tx-row spread wrap"><h2>Transcripts</h2><span id="switch-state" class="tx-state"></span></div><p class="tx-hint">Turning it off takes the controls off YouTube and stops every request to the helper. Your transcripts and settings stay.</p><div class="tx-row"><button type="button" id="turn-off" class="tx-b" data-tx="turn-off" hidden>Turn off Transcripts</button><button type="button" id="turn-on" class="tx-b primary" data-tx="turn-on" hidden>Turn on Transcripts</button></div><div id="switch-notice" class="tx-notice" role="status" hidden></div></section>
<section id="save-folder" class="tx-panel"><div class="tx-row spread wrap"><h2>Save folder</h2><span id="export-status" class="tx-state">Loading</span></div><p class="tx-hint">A folder on this computer. Each finished transcript can save here automatically; library folders become its subfolders.</p><p id="export-directory" class="tx-path">Loading</p><button type="button" id="choose-directory" class="tx-b primary" data-tx="choose-directory" disabled>Choose save folder</button><form id="export-form" class="tx-stack" data-tx-submit="export"><label class="tx-check"><input id="auto-export" type="checkbox" disabled>Save each finished transcript automatically</label><div class="tx-field"><label for="export-format">Document format</label><select id="export-format" disabled><option value="txt">Plain text (.txt)</option><option value="md">Markdown (.md)</option></select></div><p id="export-help" class="tx-hint"></p><div><button type="submit" id="save-export" class="tx-b" disabled>Save these choices</button></div></form><div id="export-notice" class="tx-notice" role="status" hidden></div></section>
<section id="helper" class="tx-panel"><div class="tx-row spread wrap"><h2>Local helper</h2><button type="button" id="check-health" class="tx-b small" data-tx="check">Check connection</button></div><span id="helper-status" class="tx-state" role="status">Connecting</span><p id="helper-description" class="tx-hint">Looking for the helper on this computer.</p><dl id="health-details" class="tx-facts"></dl><div id="helper-notice" class="tx-notice" role="status" hidden></div></section>
<section id="azure" class="tx-panel"><div class="tx-row spread wrap"><h2>Azure connection</h2><span id="azure-status" class="tx-state">Loading</span></div><form id="azure-form" class="tx-stack" data-tx-submit="azure"><div class="tx-field"><label for="azure-endpoint">Azure endpoint or Microsoft Foundry project URL</label><input id="azure-endpoint" type="url" autocomplete="off" spellcheck="false" placeholder="https://your-resource.services.ai.azure.com/api/projects/your-project" required data-tx-input="azure"><p class="tx-hint">Your resource endpoint or Foundry project URL. The helper converts it to the right transcription endpoint.</p></div><div class="tx-field"><label for="azure-key">Azure API key</label><input id="azure-key" type="password" autocomplete="off" spellcheck="false" placeholder="Enter your Azure API key" data-tx-input="azure"><p id="key-state" class="tx-hint">The key is saved by the local helper on this computer, outside browser storage.</p></div><div class="tx-stack tight"><label class="tx-check"><input id="use-brave-login" type="checkbox" data-tx-change="azure">Use my Brave login for YouTube downloads</label><p class="tx-hint">Optional, if YouTube asks you to sign in. The helper reads your local Brave cookies to reach YouTube. Cookies are never sent to Azure.</p></div><div class="tx-row wrap"><button type="submit" id="save-azure" class="tx-b primary" disabled>Save connection</button><button type="button" id="test-azure" class="tx-b" data-tx="test" disabled>Test Azure connection</button></div><p class="tx-hint">The test sends 0.75 seconds of silent audio to Azure. A tiny usage charge may apply.</p></form><div id="notice" class="tx-notice" role="status" hidden></div></section>
<section id="use" class="tx-panel"><h2>Using transcripts</h2><p class="tx-hint">Once Azure is connected, a small transcript control appears when you point at a video card on YouTube, and a Transcript button on each video page. Videos join your queue while you keep browsing. Open the library to read, copy or save them.</p><p class="tx-hint">On a channel page, choose All videos or Past year to queue that channel. Shorts stay out unless you include them.</p><p class="tx-hint">The helper and Azure costs are yours. Setup is described in the <a href="https://github.com/justerquate-sys/redline#transcripts" target="_blank" rel="noopener noreferrer">README</a>.</p></section>
<details id="pairing" class="tx-panel"><summary>Manual pairing</summary><p class="tx-hint">Automatic pairing is recommended. A helper you manage by hand can use a pairing token instead.</p><form id="pair-form" class="tx-stack" data-tx-submit="pair"><div class="tx-field"><label for="pairing-token">Local helper pairing token</label><input id="pairing-token" type="password" autocomplete="off" spellcheck="false" placeholder="Paste a local pairing token" required><p id="pair-state" class="tx-hint">Checking saved pairing</p></div><div class="tx-row wrap"><button type="submit" id="save-token" class="tx-b">Save manual pairing</button><button type="button" id="auto-pair" class="tx-b quiet" data-tx="auto-pair">Pair automatically</button><button type="button" id="forget-token" class="tx-b quiet" data-tx="forget">Forget token</button></div></form><div id="pair-notice" class="tx-notice" role="status" hidden></div></details>
<section id="settings-file" class="tx-panel"><h2>Settings file</h2><p class="tx-hint">Everything Redline keeps in this browser, in one file: preferences, searches, resume positions, topics, blocked websites and filters. Use it to move to another browser or install.</p><p class="tx-hint"><b>It also holds your Deepgram key, if you saved one.</b> Keep the file private, and delete it once you have imported it. Importing replaces these settings here. The helper’s pairing token is never in the file.</p><div class="tx-row wrap"><button type="button" id="export-all" class="tx-b" data-tx="export-all">Export all settings</button><button type="button" id="import-all" class="tx-b" data-tx="import-all">Import all settings</button><input type="file" id="import-file" accept="application/json,.json" data-tx-change="import-file" hidden></div><div id="import-notice" class="tx-notice" role="status" hidden></div><div id="import-confirm" class="tx-row wrap" hidden><button type="button" id="import-replace" class="tx-b" data-tx="import-replace">Replace and import</button><button type="button" id="import-rest" class="tx-b primary" data-tx="import-rest">Import the rest</button><button type="button" id="import-cancel" class="tx-b quiet" data-tx="import-cancel">Cancel</button></div></section>
<p class="tx-hint tx-foot">Transcripts stay on this computer. Audio goes to your own Azure resource for transcription, and nowhere else. Your API key stays in the local helper’s settings and is never returned to the extension or saved in browser storage.</p>
<p class="tx-hint">Version {version}</p>
</main></div></div>"##,
        mark = crate::ui_mark(),
        version = esc(&host::manifest_version()),
    )
}

fn update_export_controls() {
    let (busy, known, dir) = with(|s| (s.export_busy, s.export.is_some(), s.export.as_ref().is_some_and(|e| !e["directory"].as_str().unwrap_or("").is_empty())));
    disable("choose-directory", busy || !known);
    disable("save-export", busy || !known);
    disable("auto-export", busy || !dir);
    disable("export-format", busy || !known);
}

fn render_export(settings: Value) {
    let dir = settings["directory"].as_str().unwrap_or("").to_owned();
    let enabled = settings["enabled"] == true;
    host::text("export-directory", if dir.is_empty() { "No folder chosen yet" } else { &dir });
    host::set_value("export-format", if settings["format"] == "md" { "md" } else { "txt" });
    host::set_checked("auto-export", enabled);
    host::text("export-status", if dir.is_empty() { "Choose a folder" } else if enabled { "Automatic saving on" } else { "Automatic saving off" });
    set("export-status", "class", if !dir.is_empty() && enabled { "tx-state ready" } else { "tx-state" });
    host::text("export-help", if dir.is_empty() { "Choose a folder to enable automatic saving. You can also save a selected set or a whole library folder whenever you want." } else { "Automatic saving applies to future transcripts. Use Save folder or Save selected in the library for existing ones." });
    with(|s| s.export = Some(settings));
    update_export_controls();
}

async fn load_export() {
    match send("getExportSettings", json!({})).await {
        Ok(v) => {
            render_export(v);
            notice("export-notice", "", "");
            if let Ok(choice) = send("getExportDirectoryChoice", json!({})).await
                && choice["pending"] == true
                && !with(|s| s.picker)
            {
                poll_choice(choice).await;
            }
        }
        Err(e) => {
            host::text("export-status", "Connect the helper below");
            host::text("export-directory", "Folder unavailable");
            notice("export-notice", &e, "error");
        }
    }
}

async fn poll_choice(initial: Value) {
    if with(|s| std::mem::replace(&mut s.picker, true)) {
        return;
    }
    with(|s| s.export_busy = true);
    update_export_controls();
    host::text("choose-directory", "Choose a folder in the system dialog");
    let result = async {
        let mut choice = initial;
        while choice["pending"] == true {
            host::wait(1000.).await;
            choice = send("getExportDirectoryChoice", json!({})).await?;
        }
        if let Some(e) = choice["error"].as_str() {
            return Err(e.to_owned());
        }
        Ok(choice)
    }
    .await;
    match result {
        Ok(choice) => {
            let (cancelled, enabled) = (choice["cancelled"] == true, choice["enabled"] == true);
            render_export(choice);
            let m = if cancelled { "Folder selection cancelled. Your existing folder was kept." } else if enabled { "Folder selected. New transcripts will save here automatically. You can change the format or turn automatic saving off below." } else { "Folder selected. Use the library’s Save buttons, or turn on automatic saving below." };
            notice("export-notice", m, if cancelled { "info" } else { "success" });
        }
        Err(e) => notice("export-notice", &e, "error"),
    }
    with(|s| {
        s.picker = false;
        s.export_busy = false;
    });
    host::text("choose-directory", "Choose save folder");
    update_export_controls();
}

fn changed() -> bool {
    let saved = with(|s| s.saved.clone());
    saved.is_some_and(|s| host::read_value("azure-endpoint").trim() != s["endpoint"].as_str().unwrap_or("") || !host::read_value("azure-key").is_empty() || host::read_checked("use-brave-login") != (s["use_brave_login"] == true))
}

fn update_controls() {
    let (busy, known, configured) = with(|s| (s.busy, s.saved.is_some(), s.saved.as_ref().is_some_and(|v| v["configured"] == true)));
    let change = changed();
    disable("save-azure", busy || !known);
    disable("test-azure", busy || !configured || change);
    set("test-azure", "title", if change { "Save your changes before testing the connection" } else { "Send a short test request to Azure" });
    disable("azure-endpoint", busy);
    disable("azure-key", busy);
    disable("use-brave-login", busy);
}

fn render_azure(settings: Value) {
    host::set_value("azure-endpoint", settings["endpoint"].as_str().unwrap_or(""));
    host::set_value("azure-key", "");
    host::set_checked("use-brave-login", settings["use_brave_login"] == true);
    let has_key = settings["has_api_key"] == true;
    set("azure-key", "placeholder", if has_key { "API key saved, leave blank to keep it" } else { "Enter your Azure API key" });
    host::text("key-state", if has_key { "A key is saved in the local helper. It is never returned to this browser. Enter a new key only to replace it." } else { "The key is saved by the local helper on this computer, outside browser storage." });
    let configured = settings["configured"] == true;
    host::text("azure-status", if configured { "Configuration saved" } else { "Add your Azure connection" });
    set("azure-status", "class", if configured { "tx-state ready" } else { "tx-state" });
    with(|s| s.saved = Some(settings));
    update_controls();
}

async fn refresh_pair() {
    match send("getSettings", json!({})).await {
        Ok(v) => {
            let paired = v["paired"] == true;
            host::text("pair-state", if paired { "This browser has a saved helper connection." } else { "Automatic pairing runs when the helper is needed." });
            disable("forget-token", !paired);
            let (on, off) = (v["on"] == true, v["turned_off"] == true);
            show("switch", on || off);
            show("turn-off", on);
            show("turn-on", off);
            host::text("switch-state", if on { "On in this browser" } else { "Turned off" });
        }
        Err(e) => notice("pair-notice", &e, "error"),
    }
}

async fn check_connection(load: bool) {
    disable("check-health", true);
    host::text("helper-status", "Connecting");
    set("helper-status", "class", "tx-state");
    put("health-details", "");
    notice("helper-notice", "", "");
    let result = async {
        // An authenticated read pairs through the native host when needed, and is what turns the feature on: one Set up
        // click is all it takes with an installed helper.
        let settings = send("getAzureSettings", json!({})).await?;
        let health = send("health", json!({})).await?;
        Ok::<_, String>((settings, health))
    }
    .await;
    match result {
        Ok((settings, health)) => {
            if load {
                render_azure(settings);
            }
            host::text("helper-status", "Helper connected");
            set("helper-status", "class", "tx-state ready");
            host::text("helper-description", "The helper runs in the background on this computer.");
            let d = &health["dependencies"];
            let tool = |ok: bool| if ok { "Ready" } else { "Missing: see the README’s Transcripts section" };
            let facts = [("Model", health["model"].as_str().unwrap_or("MAI-Transcribe-2").to_owned()), ("Video downloader", tool(d["yt_dlp"] == true).to_owned()), ("Audio processing", tool(d["ffmpeg"] == true && d["ffprobe"] == true).to_owned())];
            let html: String = facts.iter().map(|(k, v)| format!("<dt>{k}</dt><dd>{}</dd>", esc(v))).collect();
            put("health-details", &html);
            refresh_pair().await;
        }
        Err(e) => {
            host::text("helper-status", "Helper unavailable");
            set("helper-status", "class", "tx-state error");
            host::text("helper-description", "Transcripts need the local helper. On a new computer, run Install Helper.command from the helper folder once; it starts and pairs by itself after that.");
            notice("helper-notice", &e, "error");
            if with(|s| s.saved.is_none()) {
                host::text("azure-status", "Waiting for the helper");
                update_controls();
            }
            refresh_pair().await;
        }
    }
    disable("check-health", false);
}

async fn import_file() {
    let raw = host::peek_file("import-file", core::IMPORT_MAX_BYTES).await;
    let v: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    let failed = match v["error"].as_str() {
        Some("none") => return,
        Some("big") => Some("This file is too big to be a Redline settings file."),
        Some(_) => Some("This is not a Redline settings file. Nothing changed."),
        None => None,
    };
    show("import-confirm", false);
    if let Some(message) = failed {
        return notice("import-notice", message, "error");
    }
    let (keep, skipped) = core::import_choice(&v["items"]);
    if keep.is_empty() {
        host::file_forget();
        return notice("import-notice", "This file holds no Redline settings. Nothing changed.", "error");
    }
    // What says who you are, or where your audio goes, is never replaced without asking.
    let list: Vec<Value> = core::SENSITIVE.iter().map(|(k, f, _)| json!([k, f])).collect();
    let states: Vec<String> = serde_json::from_str(&host::conflicts_of_file(&Value::Array(list).to_string()).await).unwrap_or_default();
    let named = |wanted: &[&str]| -> Vec<&'static str> {
        core::SENSITIVE.iter().zip(&states).filter(|((k, _, _), st)| keep.iter().any(|x| x == k) && wanted.contains(&st.as_str())).map(|((_, _, name), _)| *name).collect()
    };
    let replacing = named(&["replace"]);
    let arriving = named(&["new", "replace"]);
    with(|s| s.import = Some((keep, skipped, arriving.clone())));
    if replacing.is_empty() {
        return finish_import(vec![]).await;
    }
    notice("import-notice", &format!("This file would replace your {}. Replace only if you made this file yourself: a {} from someone else's file sends your data to them.", replacing.join(" and your "), replacing[0]), "info");
    show("import-confirm", true);
}

async fn finish_import(hold: Vec<Value>) {
    show("import-confirm", false);
    let Some((keep, skipped, arriving)) = with(|s| s.import.take()) else { return };
    let kept_back: Vec<&str> = core::SENSITIVE.iter().filter(|(k, f, _)| hold.iter().any(|h| h[0] == json!(k) && h[1] == json!(f))).map(|(_, _, name)| *name).collect();
    let brought: Vec<&&str> = arriving.iter().filter(|n| !kept_back.contains(n)).collect();
    if host::commit_file(&json!(keep).to_string(), &Value::Array(hold).to_string()).await {
        let mut message = format!("Imported {} {}", keep.len(), if keep.len() == 1 { "setting" } else { "settings" });
        if !brought.is_empty() {
            message.push_str(&format!(", including a {}", brought.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" and a ")));
        }
        message.push('.');
        if !kept_back.is_empty() {
            message.push_str(&format!(" Your {} stayed as it was.", kept_back.join(" and ")));
        }
        if skipped > 0 {
            message.push_str(&format!(" {skipped} other {} left out.", if skipped == 1 { "entry" } else { "entries" }));
        }
        message.push_str(" Reload your YouTube tabs to use them.");
        notice("import-notice", &message, "success");
    } else {
        notice("import-notice", "Could not write the settings. Nothing changed.", "error");
    }
}

pub fn on_event(kind: &str, name: &str, _arg: &str, _value: &str) {
    match (kind, name) {
        ("click", "export-all") => spawn_local(async {
            if let Err(e) = send("openExportPage", json!({})).await {
                notice("import-notice", &e, "error");
            }
        }),
        ("click", "import-all") => host::pick("import-file"),
        ("change", "import-file") => spawn_local(import_file()),
        ("click", "import-replace") => spawn_local(finish_import(vec![])),
        ("click", "import-rest") => {
            let hold: Vec<Value> = core::SENSITIVE.iter().map(|(k, f, _)| json!([k, f])).collect();
            spawn_local(finish_import(hold));
        }
        ("click", "import-cancel") => {
            with(|s| s.import = None);
            host::file_forget();
            show("import-confirm", false);
            notice("import-notice", "Nothing changed.", "info");
        }
        ("click", "turn-off") => {
            disable("turn-off", true);
            spawn_local(async {
                if let Err(e) = send("turnOff", json!({})).await {
                    notice("switch-notice", &e, "error");
                }
                refresh_pair().await;
                disable("turn-off", false);
            });
        }
        ("click", "turn-on") => {
            disable("turn-on", true);
            spawn_local(async {
                if let Err(e) = send("turnOn", json!({})).await {
                    notice("switch-notice", &e, "error");
                }
                refresh_pair().await;
                disable("turn-on", false);
            });
        }
        ("click", "check") => {
            let load = with(|s| s.saved.is_none());
            spawn_local(check_connection(load));
            spawn_local(load_export());
        }
        ("input", "azure") | ("change", "azure") => update_controls(),
        ("submit", "azure") => {
            with(|s| s.busy = true);
            let message = json!({"endpoint":host::read_value("azure-endpoint"),"apiKey":host::read_value("azure-key"),"useBraveLogin":host::read_checked("use-brave-login")});
            update_controls();
            notice("notice", "", "");
            spawn_local(async move {
                match send("saveAzureSettings", message).await {
                    Ok(v) => {
                        render_azure(v);
                        notice("notice", "Azure connection saved. Test it below, or start a transcript on YouTube.", "success");
                        refresh_pair().await;
                    }
                    Err(e) => notice("notice", &e, "error"),
                }
                host::set_value("azure-key", "");
                with(|s| s.busy = false);
                update_controls();
            });
        }
        ("click", "test") => {
            with(|s| s.busy = true);
            update_controls();
            host::text("test-azure", "Testing Azure");
            notice("notice", "Sending a 0.75-second silent test to Azure. This can take up to 30 seconds.", "info");
            spawn_local(async move {
                let r = send("testAzureConnection", json!({})).await.and_then(|v| if v["ok"] == true { Ok(v) } else { Err(v["message"].as_str().unwrap_or("Azure did not accept the test request.").to_owned()) });
                match r {
                    Ok(v) => {
                        host::text("azure-status", "Azure connection verified");
                        set("azure-status", "class", "tx-state ready");
                        notice("notice", v["message"].as_str().unwrap_or("Azure accepted the connection test."), "success");
                    }
                    Err(e) => {
                        host::text("azure-status", "Azure test failed");
                        set("azure-status", "class", "tx-state error");
                        notice("notice", &e, "error");
                    }
                }
                with(|s| s.busy = false);
                host::text("test-azure", "Test Azure connection");
                update_controls();
            });
        }
        ("submit", "pair") => {
            disable("save-token", true);
            let token = host::read_value("pairing-token");
            spawn_local(async move {
                let r = async {
                    send("saveSettings", json!({"pairingToken":token})).await?;
                    host::set_value("pairing-token", "");
                    send("getAzureSettings", json!({})).await?;
                    Ok::<_, String>(())
                }
                .await;
                match r {
                    Ok(()) => {
                        notice("pair-notice", "Manual pairing saved and verified.", "success");
                        check_connection(true).await;
                    }
                    Err(e) => notice("pair-notice", &e, "error"),
                }
                disable("save-token", false);
            });
        }
        ("click", "auto-pair") => {
            disable("auto-pair", true);
            spawn_local(async move {
                match send("connectHelper", json!({})).await {
                    Ok(_) => {
                        check_connection(true).await;
                        notice("pair-notice", "Automatic pairing complete.", "success");
                    }
                    Err(e) => notice("pair-notice", &e, "error"),
                }
                disable("auto-pair", false);
            });
        }
        ("click", "forget") => spawn_local(async move {
            match send("forgetSettings", json!({})).await {
                Ok(_) => {
                    refresh_pair().await;
                    notice("pair-notice", "Saved browser token removed. The installed helper pairs automatically on the next request.", "info");
                }
                Err(e) => notice("pair-notice", &e, "error"),
            }
        }),
        ("click", "choose-directory") => {
            with(|s| s.export_busy = true);
            update_export_controls();
            host::text("choose-directory", "Choose a folder in the system dialog");
            notice("export-notice", "", "");
            spawn_local(async move {
                match send("chooseExportDirectory", json!({})).await {
                    Ok(choice) => poll_choice(choice).await,
                    Err(e) => notice("export-notice", &e, "error"),
                }
                with(|s| s.export_busy = false);
                host::text("choose-directory", "Choose save folder");
                update_export_controls();
            });
        }
        ("submit", "export") => {
            with(|s| s.export_busy = true);
            update_export_controls();
            notice("export-notice", "", "");
            let message = json!({"enabled":host::read_checked("auto-export"),"format":host::read_value("export-format")});
            spawn_local(async move {
                match send("saveExportSettings", message).await {
                    Ok(v) => {
                        let on = v["enabled"] == true;
                        render_export(v);
                        notice("export-notice", if on { "Saved. New transcripts will be written to your folder automatically." } else { "Saved. Use the library’s Save buttons whenever you want to write documents to this folder." }, "success");
                    }
                    Err(e) => notice("export-notice", &e, "error"),
                }
                with(|s| s.export_busy = false);
                update_export_controls();
            });
        }
        _ => {}
    }
}

