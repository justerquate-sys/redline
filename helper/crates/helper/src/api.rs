//! Strict loopback gateway. Parsing errors never contain submitted values.
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use scribe_common::{
    Error, MODEL, Result, SharedSettings, VERSION, normalize_speech_endpoint,
    persist_azure_settings,
};
#[cfg(feature = "engine")]
use scribe_engine::Engine;
use scribe_library::Library;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

type RequestFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;
pub trait EngineApi: Send + Sync {
    fn request<'a>(&'a self, action: &'a str, value: Value) -> RequestFuture<'a>;
    fn has_active_work(&self) -> bool;
}
#[cfg(feature = "engine")]
impl EngineApi for Engine {
    fn request<'a>(&'a self, action: &'a str, value: Value) -> RequestFuture<'a> {
        Box::pin(Engine::request(self, action, value))
    }
    fn has_active_work(&self) -> bool {
        Engine::has_active_work(self)
    }
}

#[derive(Default)]
struct Picker {
    task: Option<tokio::task::JoinHandle<()>>,
    result: Option<Value>,
}
pub struct AppState {
    pub settings: SharedSettings,
    pub library: Arc<Library>,
    pub engine: Arc<dyn EngineApi>,
    picker: Mutex<Picker>,
    connection_test: Mutex<()>,
    native_action: Arc<Mutex<()>>,
    settings_gate: tokio::sync::RwLock<()>,
}
impl AppState {
    pub fn new(
        settings: SharedSettings,
        library: Arc<Library>,
        engine: Arc<dyn EngineApi>,
    ) -> Arc<Self> {
        Arc::new(Self {
            settings,
            library,
            engine,
            picker: Mutex::new(Picker::default()),
            connection_test: Mutex::new(()),
            native_action: Arc::new(Mutex::new(())),
            settings_gate: tokio::sync::RwLock::new(()),
        })
    }
    pub async fn shutdown(&self) {
        if let Some(task) = self.picker.lock().await.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", any(gateway))
        .route("/{*path}", any(gateway))
        .with_state(state)
}
pub fn valid_origin(origin: &str) -> bool {
    origin
        .strip_prefix("chrome-extension://")
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b)))
}
fn validation() -> Error {
    Error::new(422, "Invalid request. Check the supplied fields.")
}
fn response(result: Result<Value>) -> Response {
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(error) => (
            StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            axum::Json(json!({"detail":error.message})),
        )
            .into_response(),
    }
}
async fn gateway(State(state): State<Arc<AppState>>, request: Request) -> Response {
    let host = request
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or("");
    if !["localhost", "127.0.0.1"].contains(&host) {
        return response(Err(Error::new(
            403,
            "This helper accepts only localhost requests.",
        )));
    }
    let origin = request
        .headers()
        .get("origin")
        .map(|v| v.to_str().unwrap_or("").to_owned());
    if origin.as_deref().is_some_and(|v| !valid_origin(v)) {
        return response(Err(Error::new(
            403,
            "Only the browser extension may access this helper.",
        )));
    }
    let preflight = request.method() == Method::OPTIONS;
    let result = if preflight {
        Ok(json!({}))
    } else {
        let anonymous = request.method() == Method::GET && request.uri().path() == "/health";
        let auth = if anonymous {
            Ok(())
        } else {
            let settings = state.settings.read();
            if settings.helper_token.is_empty() {
                Err(Error::new(
                    503,
                    "HELPER_TOKEN is missing. Run the setup script and restart the helper.",
                ))
            } else {
                let header = request
                    .headers()
                    .get("authorization")
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");
                let provided = header.strip_prefix("Bearer ").unwrap_or("");
                // Hashing both sides also avoids exposing a token-length timing difference.
                if bool::from(
                    Sha256::digest(provided.as_bytes())
                        .ct_eq(&Sha256::digest(settings.helper_token.as_bytes())),
                ) {
                    Ok(())
                } else {
                    Err(Error::new(
                        401,
                        "The helper token is missing or incorrect. Update the extension settings.",
                    ))
                }
            }
        };
        match auth {
            Ok(()) => dispatch(state.clone(), request).await,
            Err(error) => Err(error),
        }
    };
    let mut response = response(result);
    if preflight {
        *response.status_mut() = StatusCode::NO_CONTENT;
    }
    response
        .headers_mut()
        .insert("Cache-Control", HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    if let Some(origin) = origin {
        if let Ok(value) = HeaderValue::from_str(&origin) {
            response
                .headers_mut()
                .insert("Access-Control-Allow-Origin", value);
        }
        response
            .headers_mut()
            .insert("Vary", HeaderValue::from_static("Origin"));
        response.headers_mut().insert(
            "Access-Control-Allow-Methods",
            HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
        );
        response.headers_mut().insert(
            "Access-Control-Allow-Headers",
            HeaderValue::from_static("Authorization, Content-Type"),
        );
    }
    response
}
async fn body(request: Request, settings: bool, optional: bool) -> Result<Value> {
    let bytes = to_bytes(
        request.into_body(),
        if settings { 16_384 } else { 2 * 1024 * 1024 },
    )
    .await
    .map_err(|_| {
        Error::new(
            413,
            if settings {
                "Settings request is too large."
            } else {
                "Request is too large."
            },
        )
    })?;
    if bytes.is_empty() && optional {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        if settings {
            Error::bad_request("Send endpoint and optional API key as a JSON object.")
        } else {
            validation()
        }
    })?;
    if !value.is_object() {
        return Err(if settings {
            Error::bad_request("Supply a valid endpoint and optional API key.")
        } else {
            validation()
        });
    }
    Ok(value)
}
fn keys(value: &Value, allowed: &[&str]) -> Result<()> {
    if value
        .as_object()
        .is_none_or(|map| map.keys().any(|k| !allowed.contains(&k.as_str())))
    {
        return Err(validation());
    }
    Ok(())
}
fn string(value: &Value, name: &str, min: usize, max: usize, required: bool) -> Result<()> {
    match value.get(name) {
        Some(Value::String(v)) if (min..=max).contains(&v.chars().count()) => Ok(()),
        None if !required => Ok(()),
        _ => Err(validation()),
    }
}
fn nullable_string(value: &Value, name: &str, required: bool) -> Result<()> {
    match value.get(name) {
        Some(Value::Null | Value::String(_)) => Ok(()),
        None if !required => Ok(()),
        _ => Err(validation()),
    }
}
fn enumeration(value: &Value, name: &str, allowed: &[&str], required: bool) -> Result<()> {
    match value.get(name) {
        Some(Value::String(v)) if allowed.contains(&v.as_str()) => Ok(()),
        None if !required => Ok(()),
        _ => Err(validation()),
    }
}
fn id_list(value: &Value, name: &str, min: usize, max: usize, required: bool) -> Result<()> {
    match value.get(name) {
        Some(Value::Array(v))
            if (min..=max).contains(&v.len()) && v.iter().all(Value::is_string) =>
        {
            Ok(())
        }
        None if !required => Ok(()),
        _ => Err(validation()),
    }
}
fn video_id(id: &str) -> Result<()> {
    if id.len() != 11
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(validation());
    }
    Ok(())
}
fn attachment_id(id: &str) -> Result<()> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(validation());
    }
    Ok(())
}
async fn blocking<T, F>(f: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| Error::internal("The local file operation could not be completed."))?
}
fn public_settings(state: &AppState) -> Value {
    let s = state.settings.read();
    json!({"endpoint":normalize_speech_endpoint(&s.speech_endpoint).unwrap_or_default(),"has_api_key":!s.speech_key.is_empty(),"configured":s.configured(),"model":MODEL,"provider":"azure","use_brave_login":s.cookies_browser.split([':', '+']).next().is_some_and(|v| v.eq_ignore_ascii_case("brave"))})
}
fn require_ready(state: &AppState, metadata_only: bool) -> Result<()> {
    let settings = state.settings.read();
    if !metadata_only && !settings.configured() {
        return Err(Error::new(
            503,
            "Add your Azure resource endpoint and API key in the extension settings.",
        ));
    }
    let dependencies = settings.dependencies();
    let missing: Vec<_> = dependencies
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, value)| {
            !value.as_bool().unwrap_or(false) && (!metadata_only || name.as_str() == "yt_dlp")
        })
        .map(|(name, _)| name.as_str())
        .collect();
    if !missing.is_empty() {
        return Err(Error::new(
            503,
            format!(
                "Missing dependencies: {}. Run setup again.",
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}
async fn dispatch(state: Arc<AppState>, request: Request) -> Result<Value> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if method == Method::GET && path == "/health" {
        // Filesystem dependency checks must not hold the settings read lock.
        // The returned fields describe one coherent settings snapshot.
        let settings = state.settings.read().clone();
        return Ok(
            json!({"status":"ok","configured":settings.configured(),"provider":"azure","model":MODEL,"dependencies":settings.dependencies(),"version":VERSION}),
        );
    }
    let query: HashMap<String, String> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let segments: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    let folder = || query.get("folder_id").cloned();
    match (method.as_str(), path.as_str()) {
        ("GET", "/settings") => return Ok(public_settings(&state)),
        ("POST", "/settings") => {
            return save_settings(state, body(request, true, false).await?).await;
        }
        ("POST", "/settings/test") => {
            let _guard = state
                .connection_test
                .try_lock()
                .map_err(|_| Error::conflict("An Azure connection test is already running."))?;
            if !state.settings.read().configured() {
                return Err(Error::new(
                    503,
                    "Save an Azure resource endpoint and API key before testing the connection.",
                ));
            }
            let snapshot = state.settings.read().clone();
            tokio::time::timeout(std::time::Duration::from_secs(30), state.engine.request("testConnection", json!({}))).await.map_err(|_| Error::new(504, "The Azure connection test timed out. Check the endpoint and internet connection, then try again."))??;
            return Ok(
                json!({"ok":true,"endpoint":snapshot.speech_endpoint,"model":MODEL,"provider":"azure","message":"Azure accepted the MAI-Transcribe-2 test request. This short test may incur a small Azure charge."}),
            );
        }
        ("GET", "/library") => return state.engine.request("librarySnapshot", json!({})).await,
        ("GET", "/library/stats") => {
            return state
                .engine
                .request("libraryStats", json!({"folder_id":folder()}))
                .await;
        }
        ("GET", "/library/documents") => {
            return state
                .engine
                .request("folderDocuments", json!({"folder_id":folder()}))
                .await;
        }
        ("GET", "/library/export-folders") => {
            let library = state.library.clone();
            return blocking(move || library.export_folders()).await;
        }
        ("POST", "/library/attachments") => {
            let value = body(request, false, false).await?;
            keys(&value, &["folder_id", "excluded_folders", "root_token"])?;
            nullable_string(&value, "folder_id", false)?;
            if !value["excluded_folders"].is_null() {
                id_list(&value, "excluded_folders", 0, 1000, true)?;
                if value["excluded_folders"].as_array().is_none_or(|v| {
                    v.iter().any(|n| {
                        n.as_str()
                            .is_none_or(|s| s.is_empty() || s.chars().count() > 255)
                    })
                }) {
                    return Err(validation());
                }
            }
            if let Some(token) = value.get("root_token").filter(|v| !v.is_null())
                && token.as_str().is_none_or(|t| {
                    t.len() != 64
                        || !t
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            {
                return Err(validation());
            }
            let library = state.library.clone();
            return blocking(move || {
                let excluded = value["excluded_folders"].as_array().map(|v| {
                    v.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect::<Vec<_>>()
                });
                library.prepare_attachment(
                    value["folder_id"].as_str(),
                    excluded.as_deref(),
                    value["root_token"].as_str(),
                )
            })
            .await;
        }
        ("POST", "/folders") => {
            let v = body(request, false, false).await?;
            string(&v, "name", 1, 160, true)?;
            let library = state.library.clone();
            return blocking(move || {
                let mut result = library.create_folder(v["name"].as_str().unwrap_or(""))?;
                result["kind"] = json!("manual");
                Ok(result)
            })
            .await;
        }
        ("POST", "/library/default-folder") => {
            let v = body(request, false, false).await?;
            nullable_string(&v, "folder_id", true)?;
            let library = state.library.clone();
            blocking(move || library.set_default(v["folder_id"].as_str())).await?;
            return state.engine.request("librarySnapshot", json!({})).await;
        }
        ("GET", "/export-settings") => return Ok(state.library.export_settings()),
        ("POST", "/export-settings") => {
            let v = body(request, false, false).await?;
            keys(&v, &["enabled", "format"])?;
            if !v["enabled"].is_boolean() {
                return Err(validation());
            }
            enumeration(&v, "format", &["txt", "md"], true)?;
            let library = state.library.clone();
            return blocking(move || {
                library.set_export_settings(
                    v["enabled"].as_bool().unwrap_or(false),
                    v["format"].as_str().unwrap_or(""),
                )
            })
            .await;
        }
        ("POST", "/export-settings/choose-directory") => return start_picker(state).await,
        ("GET", "/export-settings/choose-directory") => {
            let picker = state.picker.lock().await;
            if picker.task.as_ref().is_some_and(|t| !t.is_finished()) {
                return Ok(json!({"pending":true}));
            }
            let mut value = picker
                .result
                .clone()
                .unwrap_or_else(|| state.library.export_settings());
            if value.get("pending").is_none() {
                value["pending"] = json!(false);
                value["cancelled"] = json!(false);
            }
            return Ok(value);
        }
        ("POST", "/export-settings/reveal-directory") => {
            let value = body(request, false, true).await?;
            nullable_string(&value, "folder_id", false)?;
            let library = state.library.clone();
            let path = blocking(move || {
                let selected = value["folder_id"].as_str();
                library.validate_folder(selected)?;
                let prefs = library.export_settings();
                let directory = prefs["directory"].as_str().unwrap_or("");
                if directory.is_empty() {
                    return Err(Error::conflict(
                        "Choose a destination folder before opening it.",
                    ));
                }
                let mut path = std::path::PathBuf::from(directory);
                if let Some(id) = selected {
                    let snapshot = library.snapshot();
                    let name = snapshot["folders"]
                        .as_array()
                        .and_then(|a| a.iter().find(|f| f["id"] == id))
                        .and_then(|f| f["name"].as_str())
                        .ok_or_else(|| Error::not_found("Library folder not found."))?;
                    path.push(name);
                }
                Ok(path)
            })
            .await?;
            crate::native::reveal_export_directory(&path).await?;
            return Ok(json!({"revealed":true}));
        }
        ("GET", "/jobs") => return state.engine.request("jobs", json!({})).await,
        ("GET", "/queue/status") => return state.engine.request("queueStatus", json!({})).await,
        ("GET", "/queue/attention") => {
            return state.engine.request("queueAttention", json!({})).await;
        }
        ("POST", "/queue/cancel") => return state.engine.request("cancelQueue", json!({})).await,
        ("POST", "/videos/status") => {
            let value = body(request, false, false).await?;
            keys(&value, &["video_ids"])?;
            id_list(&value, "video_ids", 0, 200, true)?;
            for id in value["video_ids"].as_array().into_iter().flatten() {
                video_id(id.as_str().unwrap_or(""))?;
            }
            return state.engine.request("videoStatuses", value).await;
        }
        ("POST", "/jobs") => {
            let value = body(request, false, false).await?;
            string(&value, "url", 1, 4096, true)?;
            nullable_string(&value, "folder_id", false)?;
            // Engine checks removed registry before requiring Azure or enqueueing.
            let _gate = state.settings_gate.read().await;
            return state.engine.request("submitJob", value).await;
        }
        ("POST", "/jobs/move") => {
            let value = body(request, false, false).await?;
            id_list(&value, "job_ids", 1, 10000, true)?;
            nullable_string(&value, "folder_id", true)?;
            return state.engine.request("moveJobs", value).await;
        }
        ("POST", "/exports") => {
            let value = body(request, false, true).await?;
            keys(&value, &["job_ids", "folder_id", "format"])?;
            if !value["job_ids"].is_null() {
                id_list(&value, "job_ids", 0, 10000, false)?;
            }
            nullable_string(&value, "folder_id", false)?;
            if !value["format"].is_null() {
                enumeration(&value, "format", &["txt", "md", "json"], false)?;
            }
            if value.get("job_ids").is_some() && value.get("folder_id").is_some() {
                return Err(Error::bad_request(
                    "Select either transcript jobs or a library folder for export.",
                ));
            }
            return state.engine.request("exportJobs", value).await;
        }
        ("POST", "/channels/preview" | "/channels/import") => {
            let mut value = body(request, false, false).await?;
            keys(&value, &["url", "scope", "include_shorts"])?;
            string(&value, "url", 1, 4096, true)?;
            enumeration(&value, "scope", &["year", "all"], true)?;
            if let Some(v) = value.get("include_shorts") {
                if !v.is_boolean() {
                    return Err(validation());
                }
            } else {
                value["include_shorts"] = json!(false);
            }
            let _gate = state.settings_gate.read().await;
            require_ready(&state, path.ends_with("preview"))?;
            return state
                .engine
                .request(
                    if path.ends_with("preview") {
                        "startChannelPreview"
                    } else {
                        "startChannelImport"
                    },
                    value,
                )
                .await;
        }
        _ => {}
    }
    match (method.as_str(), segments.as_slice()) {
        ("POST", ["library", "attachments", id, action @ ("copy" | "reveal")]) => {
            attachment_id(id)?;
            return attachment_action(state, id, action == &"copy").await;
        }
        ("DELETE", ["folders", id]) => {
            return state
                .engine
                .request("removeMissingFolder", json!({"folder_id":id}))
                .await;
        }
        ("GET", ["jobs", id]) => state.engine.request("getJob", json!({"id":id})).await,
        ("POST", ["jobs", id, "cancel"]) => {
            state.engine.request("cancelJob", json!({"id":id})).await
        }
        ("GET", ["jobs", id, "document"]) => {
            let format = query.get("format").map(String::as_str).unwrap_or("md");
            if !["md", "txt", "json"].contains(&format) {
                return Err(validation());
            }
            state
                .engine
                .request("readDocument", json!({"id":id,"format":format}))
                .await
        }
        ("POST", ["videos", id, "cancel"]) => {
            video_id(id)?;
            state
                .engine
                .request("cancelVideo", json!({"video_id":id}))
                .await
        }
        ("POST", ["videos", id, "reveal"]) => {
            video_id(id)?;
            let value = state
                .engine
                .request("revealVideo", json!({"video_id":id}))
                .await?;
            let path = value["path"]
                .as_str()
                .ok_or_else(|| Error::internal("The saved transcript could not be verified."))?;
            crate::native::reveal_export_file(std::path::Path::new(path)).await?;
            Ok(json!({"revealed":true}))
        }
        ("GET", ["channels", "previews", id]) => {
            state
                .engine
                .request("getChannelPreview", json!({"id":id}))
                .await
        }
        ("POST", ["channels", "previews", id, "queue"]) => {
            let _gate = state.settings_gate.read().await;
            require_ready(&state, false)?;
            state
                .engine
                .request("queueChannelPreview", json!({"id":id}))
                .await
        }
        ("POST", ["channels", "previews", id, "cancel"]) => {
            state
                .engine
                .request("cancelChannelPreview", json!({"id":id}))
                .await
        }
        _ => Err(Error::not_found(
            "The requested helper endpoint was not found.",
        )),
    }
}
async fn save_settings(state: Arc<AppState>, value: Value) -> Result<Value> {
    if keys(&value, &["endpoint", "api_key", "use_brave_login"]).is_err()
        || !value["endpoint"].is_string()
        || value
            .get("api_key")
            .is_some_and(|v| !v.is_string() && !v.is_null())
    {
        return Err(Error::bad_request(
            "Supply a valid endpoint and optional API key.",
        ));
    }
    if value
        .get("use_brave_login")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(Error::bad_request(
            "The Brave login setting must be true or false.",
        ));
    }
    let endpoint = normalize_speech_endpoint(value["endpoint"].as_str().unwrap_or(""))?;
    let key = value["api_key"].as_str().unwrap_or("").trim();
    if key.len() > 4096 || key.bytes().any(|b| b <= 32 || b >= 127) {
        return Err(Error::bad_request(
            "The API key format is invalid. Paste the resource key without extra text.",
        ));
    }
    // Only complete, validated requests reserve admission. An unfinished HTTP
    // body must not block other clients from queueing work or changing settings.
    let _gate = state.settings_gate.write().await;
    let _guard=state.connection_test.try_lock().map_err(|_|Error::conflict("Wait for active transcription jobs or the connection test to finish before changing Azure settings."))?;
    if state.engine.has_active_work() {
        return Err(Error::conflict(
            "Wait for active transcription jobs or the connection test to finish before changing Azure settings.",
        ));
    }
    let mut updated = state.settings.read().clone();
    if key.is_empty() && (endpoint != updated.speech_endpoint || updated.speech_key.is_empty()) {
        return Err(Error::bad_request(
            "Enter the Azure API key for this resource. A saved key can only be reused with its current endpoint.",
        ));
    }
    updated.speech_endpoint = endpoint;
    if !key.is_empty() {
        updated.speech_key = key.to_owned();
    }
    if let Some(login) = value["use_brave_login"].as_bool() {
        updated.cookies_browser = if login { "brave" } else { "" }.to_owned();
    }
    let saved = updated.clone();
    let include = value.get("use_brave_login").is_some();
    blocking(move||persist_azure_settings(&saved,include)).await.map_err(|_|Error::internal("Could not save Azure settings securely. Check the helper folder's write permissions."))?;
    *state.settings.write() = updated;
    Ok(public_settings(&state))
}
async fn start_picker(state: Arc<AppState>) -> Result<Value> {
    let mut picker = state.picker.lock().await;
    if picker.task.as_ref().is_some_and(|t| !t.is_finished()) {
        return Ok(json!({"pending":true}));
    }
    picker.result = None;
    let task_state = state.clone();
    picker.task = Some(tokio::spawn(async move {
        let chosen = crate::native::choose_export_directory().await;
        let mut result = match chosen {
            Ok(Some(path)) => {
                let library = task_state.library.clone();
                match blocking(move || library.set_directory(&path)).await {
                    Ok(mut v) => {
                        v["cancelled"] = json!(false);
                        v
                    }
                    Err(e) => {
                        let mut v = task_state.library.export_settings();
                        v["error"] = json!(e.message);
                        v["cancelled"] = json!(false);
                        v
                    }
                }
            }
            Ok(None) => {
                let mut v = task_state.library.export_settings();
                v["cancelled"] = json!(true);
                v
            }
            Err(e) => {
                let mut v = task_state.library.export_settings();
                v["error"] = json!(e.message);
                v["cancelled"] = json!(false);
                v
            }
        };
        result["pending"] = json!(false);
        task_state.picker.lock().await.result = Some(result);
    }));
    Ok(json!({"pending":true}))
}
async fn attachment_action(state: Arc<AppState>, id: &str, copy: bool) -> Result<Value> {
    // An owned operation survives a dropped HTTP response: clipboard completion and
    // the durable lease must be settled even when the popup closes mid-action.
    let id = id.to_owned();
    tokio::spawn(async move {
        let _guard = state.native_action.lock().await;
        let library = state.library.clone();
        let begin_id = id.clone();
        let (path, mut record) = blocking(move || library.begin_attachment_use(&begin_id)).await?;
        let result = if copy {
            crate::native::copy_attachment_file(&path).await
        } else {
            crate::native::reveal_export_file(&path).await
        };
        let library = state.library.clone();
        if let Err(error) = result {
            if copy {
                blocking(move || library.protect_unknown_copy(&id)).await?;
            } else {
                blocking(move || library.finish_attachment_use(&id, false)).await?;
            }
            return Err(error);
        }
        blocking(move || library.finish_attachment_use(&id, copy)).await?;
        if copy {
            record["copied"] = json!(true);
            Ok(record)
        } else {
            Ok(json!({"revealed":true}))
        }
    })
    .await
    .map_err(|_| Error::internal("The native attachment action could not finish."))?
}
