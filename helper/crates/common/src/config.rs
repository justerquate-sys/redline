use crate::{Error, Result};
use parking_lot::RwLock;
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
};
pub type SharedSettings = Arc<RwLock<Settings>>;
#[derive(Clone)]
pub struct Settings {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    pub speech_key: String,
    pub speech_endpoint: String,
    pub helper_token: String,
    pub ffmpeg_location: String,
    pub cookies_browser: String,
    pub js_runtime: String,
    pub chunk_seconds: u32,
    pub audio_speed: f64,
}
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("root", &self.root)
            .field("data_dir", &self.data_dir)
            .field("configured", &self.configured())
            .finish_non_exhaustive()
    }
}
impl Settings {
    pub fn load(root: impl AsRef<Path>, data_override: Option<PathBuf>) -> Result<Self> {
        let root = fs::canonicalize(root.as_ref())
            .map_err(|_| Error::bad_request("The project directory could not be found."))?;
        let values = read_settings_file(&root)?
            .map(|(_, values)| values)
            .unwrap_or_default();
        let get = |name: &str, default: &str| {
            env::var(name)
                .ok()
                .or_else(|| values.get(name).cloned())
                .unwrap_or_else(|| default.into())
        };
        let data = data_override.unwrap_or_else(|| PathBuf::from(get("DATA_DIR", ".data")));
        let data_dir = if data.is_absolute() {
            data
        } else {
            root.join(data)
        };
        let raw = get("AZURE_SPEECH_ENDPOINT", "")
            .trim()
            .trim_end_matches('/')
            .to_string();
        let speech_endpoint = normalize_speech_endpoint(&raw).unwrap_or(raw);
        let audio_speed = get("AUDIO_SPEED", "1.25")
            .parse::<f64>()
            .ok()
            .filter(|x| x.is_finite())
            .unwrap_or(1.25)
            .clamp(1., 2.);
        Ok(Self {
            root,
            data_dir,
            speech_key: get("AZURE_SPEECH_KEY", "").trim().into(),
            speech_endpoint,
            helper_token: get("HELPER_TOKEN", "").trim().into(),
            ffmpeg_location: get("FFMPEG_LOCATION", ""),
            cookies_browser: get("YTDLP_COOKIES_BROWSER", ""),
            js_runtime: get("YTDLP_JS_RUNTIME", ""),
            chunk_seconds: get("CHUNK_SECONDS", "600")
                .parse::<u32>()
                .unwrap_or(600)
                .clamp(30, 900),
            audio_speed,
        })
    }
    pub fn endpoint_valid(&self) -> bool {
        // Only a test build (the feature is never on in a release) may talk to a Speech stand-in on this machine.
        #[cfg(feature = "loopback-speech")]
        if self.speech_endpoint.starts_with("http://127.0.0.1:") {
            return true;
        }
        normalize_speech_endpoint(&self.speech_endpoint).is_ok()
    }
    pub fn configured(&self) -> bool {
        !self.speech_key.is_empty() && !self.helper_token.is_empty() && self.endpoint_valid()
    }
    pub fn executable(&self, name: &str) -> Option<PathBuf> {
        ToolResolver::new(self).executable(name)
    }
    pub fn dependencies(&self) -> Value {
        // Read runtime metadata at most once in a response. File checks remain
        // fresh and PATH lookup runs only if all explicit candidates fail.
        let mut resolver = ToolResolver::new(self);
        json!({"ffmpeg":resolver.executable("ffmpeg").is_some(),"ffprobe":resolver.executable("ffprobe").is_some(),"yt_dlp":resolver.executable("yt-dlp").is_some()})
    }
    pub fn javascript_runtime(&self) -> Option<String> {
        if !self.js_runtime.is_empty() {
            return Some(self.js_runtime.clone());
        }
        let mut resolver = ToolResolver::new(self);
        ["deno", "node"].iter().find_map(|name| {
            resolver
                .executable(name)
                .map(|path| format!("{name}:{}", path.display()))
        })
    }
    pub fn settings_path(&self) -> PathBuf {
        self.root.join(".env")
    }
}
struct ToolResolver<'a> {
    settings: &'a Settings,
    runtime: Option<Value>,
    runtime_loaded: bool,
}
fn is_executable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}
impl<'a> ToolResolver<'a> {
    fn new(settings: &'a Settings) -> Self {
        Self {
            settings,
            runtime: None,
            runtime_loaded: false,
        }
    }
    fn executable(&mut self, name: &str) -> Option<PathBuf> {
        if !self.settings.ffmpeg_location.is_empty() {
            let location = Path::new(&self.settings.ffmpeg_location);
            let candidate = if location.is_dir() {
                location.join(name)
            } else {
                location.with_file_name(name)
            };
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
        if !self.runtime_loaded {
            self.runtime = crate::read_json(&self.settings.data_dir.join("runtime.json")).ok();
            self.runtime_loaded = true;
        }
        if let Some(runtime) = &self.runtime {
            if matches!(name, "node" | "deno")
                && let Some(path) = runtime[name].as_str().map(Path::new)
                && path.is_absolute()
                && is_executable(path)
            {
                return Some(path.to_path_buf());
            }
            for key in ["ffmpeg_location", "ffmpeg_dir", "FFMPEG_LOCATION"] {
                if let Some(location) = runtime[key].as_str() {
                    let candidate = Path::new(location).join(name);
                    if is_executable(&candidate) {
                        return Some(candidate);
                    }
                }
            }
        }
        for directory in [".tools/bin", ".venv/bin"] {
            let candidate = self.settings.root.join(directory).join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
        which::which(name).ok().filter(|path| is_executable(path))
    }
}

/// Read and fully validate the persisted settings without applying process
/// environment overrides. Never return a credential from a partially parsed
/// file, and never follow a settings-file or ancestor symlink.
///
/// The original UTF-8 text is returned so installation can preserve comments,
/// unrelated options, and intentional formatting when adding a missing token.
pub fn read_settings_file(root: &Path) -> Result<Option<(String, HashMap<String, String>)>> {
    let bytes = match crate::local_io::read(&root.join(".env")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(Error::bad_request(
                "The local settings file could not be read.",
            ));
        }
    };
    let text = String::from_utf8(bytes)
        .map_err(|_| Error::bad_request("The local settings file could not be read."))?;
    let values = dotenvy::from_read_iter(text.as_bytes())
        .collect::<std::result::Result<HashMap<String, String>, _>>()
        .map_err(|_| Error::bad_request("The local settings file could not be read."))?;
    Ok(Some((text, values)))
}

pub fn normalize_speech_endpoint(value: &str) -> Result<String> {
    let err = || {
        Error::bad_request(
            "Use your Azure Speech HTTPS resource endpoint or a Microsoft Foundry project URL.",
        )
    };
    let value = value.trim();
    if value.len() > 2048 || value.chars().any(|c| c.is_control()) {
        return Err(err());
    }
    let u = url::Url::parse(value).map_err(|_| err())?;
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some_and(|p| p != 443)
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(err());
    }
    let host = u.host_str().unwrap_or("").to_ascii_lowercase();
    static HOST: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.(?:cognitiveservices\.azure\.com|api\.cognitive\.microsoft\.com|services\.ai\.azure\.com)$").expect("constant regex")
    });
    if !HOST.is_match(&host) {
        return Err(err());
    }
    if let Some(resource) = host.strip_suffix(".services.ai.azure.com") {
        static PROJECT: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"^/api/projects/[A-Za-z0-9_.-]{1,128}/?$").expect("constant regex")
        });
        if !["", "/"].contains(&u.path()) && !PROJECT.is_match(u.path()) {
            return Err(err());
        }
        return Ok(format!("https://{resource}.cognitiveservices.azure.com"));
    }
    if !["", "/"].contains(&u.path()) {
        return Err(err());
    }
    Ok(format!("https://{host}"))
}
pub fn persist_azure_settings(settings: &Settings, include_browser_login: bool) -> Result<()> {
    normalize_speech_endpoint(&settings.speech_endpoint)?;
    if settings.speech_key.len() > 4096 || settings.speech_key.bytes().any(|b| b <= 32 || b >= 127)
    {
        return Err(Error::bad_request("The API key format is invalid."));
    }
    let path = settings.settings_path();
    let previous = read_settings_file(&settings.root)?
        .map(|(text, _)| text)
        .unwrap_or_default();
    let mut updates = HashMap::from([
        ("AZURE_SPEECH_ENDPOINT", settings.speech_endpoint.clone()),
        ("AZURE_SPEECH_KEY", settings.speech_key.clone()),
    ]);
    if include_browser_login {
        updates.insert("YTDLP_COOKIES_BROWSER", settings.cookies_browser.clone());
    }
    let mut output = String::new();
    for line in previous.lines() {
        let key = line
            .trim_start()
            .strip_prefix("export ")
            .unwrap_or(line.trim_start())
            .split('=')
            .next()
            .unwrap_or("")
            .trim();
        if !updates.contains_key(key) {
            output.push_str(line);
            output.push('\n');
        }
    }
    for key in [
        "AZURE_SPEECH_ENDPOINT",
        "AZURE_SPEECH_KEY",
        "YTDLP_COOKIES_BROWSER",
    ] {
        if let Some(value) = updates.remove(key) {
            output.push_str(&format!(
                "{key}='{}'\n",
                value.replace('\\', "\\\\").replace('\'', "\\'")
            ));
        }
    }
    crate::atomic_write(&path, output.as_bytes())
}

#[cfg(all(test, unix))]
mod tool_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::{cell::Cell, rc::Rc};
    fn tool(directory: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(directory).unwrap();
        let path = directory.join(name);
        fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    #[test]
    fn dependencies_share_one_runtime_read_and_recheck_the_next_request() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings::load(root.path(), None).unwrap();
        settings.ffmpeg_location = String::new();
        let directory = root.path().join("runtime-tools");
        for name in ["ffmpeg", "ffprobe", "yt-dlp"] {
            tool(&directory, name);
        }
        let runtime = settings.data_dir.join("runtime.json");
        crate::atomic_json(&runtime, &json!({"ffmpeg_location":directory})).unwrap();
        let reads = Rc::new(Cell::new(0));
        let seen = reads.clone();
        crate::local_io::HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |stage, path| {
                if stage == "read_opened"
                    && path.file_name().is_some_and(|name| name == "runtime.json")
                {
                    seen.set(seen.get() + 1);
                }
            }))
        });
        assert_eq!(
            settings.dependencies(),
            json!({"ffmpeg":true,"ffprobe":true,"yt_dlp":true})
        );
        assert_eq!(
            reads.get(),
            1,
            "One runtime file is shared only within this response"
        );
        assert_eq!(
            settings.dependencies(),
            json!({"ffmpeg":true,"ffprobe":true,"yt_dlp":true})
        );
        assert_eq!(
            reads.get(),
            2,
            "The next response must freshly read runtime configuration"
        );
        crate::local_io::HOOK.with(|hook| *hook.borrow_mut() = None);
    }
    #[test]
    fn resolution_preserves_priority_and_observes_runtime_and_permission_changes() {
        let root = tempfile::tempdir().unwrap();
        let mut settings = Settings::load(root.path(), None).unwrap();
        settings.ffmpeg_location = String::new();
        let name = "scribe-offline-fixture-unavailable-on-path";
        let a = root.path().join("first");
        let b = root.path().join("second");
        let c = root.path().join("configured");
        let first = tool(&a, name);
        let second = tool(&b, name);
        let explicit = tool(&c, name);
        let runtime = settings.data_dir.join("runtime.json");
        crate::atomic_json(&runtime, &json!({"ffmpeg_location":a})).unwrap();
        assert_eq!(settings.executable(name), Some(first));
        crate::atomic_json(&runtime, &json!({"ffmpeg_location":b})).unwrap();
        assert_eq!(settings.executable(name), Some(second.clone()));
        fs::set_permissions(second, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(settings.executable(name), None);
        settings.ffmpeg_location = c.to_string_lossy().into_owned();
        crate::atomic_write(&runtime, b"invalid optional runtime JSON").unwrap();
        assert_eq!(settings.executable(name), Some(explicit));
        fs::remove_file(c.join(name)).unwrap();
        let fallback = tool(&root.path().join(".tools/bin"), name);
        assert_eq!(
            settings.executable(name),
            Some(fallback.canonicalize().unwrap())
        );
    }
}
