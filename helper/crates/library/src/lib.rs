//! Durable library state and conservative, descriptor-relative local exports.
mod archive;
mod attachments;
mod export;
mod folder_copy;
mod fs;
mod removals;
mod stats;
pub use export::ExportOutcome;
use fs::{Dir, Identity, safe};
use parking_lot::{Mutex, RwLock};
pub use removals::{REMOVED_MESSAGE, RemovalRegistry};
use scribe_common::Result;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;
pub(crate) fn casefold(value: &str) -> String {
    value.case_fold().collect()
}
type DocumentSignatures = HashMap<(String, u64), String>;
type DocumentCache = HashMap<PathBuf, (Identity, DocumentSignatures)>;

pub struct Library {
    pub(crate) data_dir: PathBuf,
    pub(crate) state: RwLock<Value>,
    pub(crate) operations: Mutex<()>,
    pub(crate) attachment_lock: Mutex<()>,
    pub(crate) hash_cache: Mutex<HashMap<Identity, String>>,
    pub(crate) document_cache: Mutex<DocumentCache>,
}
fn folder_name(raw: &str) -> Result<String> {
    let name = raw.nfc().collect::<String>().trim().to_owned();
    let reserved = name.split('.').next().unwrap_or("").to_ascii_lowercase();
    if name.is_empty()
        || name.chars().count() > 80
        || name.len() > 160
        || matches!(name.as_str(), "." | "..")
        || name.ends_with('.')
        || name
            .chars()
            .any(|c| c < ' ' || c == '\x7f' || "/\\:*?\"<>|".contains(c))
        || ["con", "prn", "aux", "nul"].contains(&reserved.as_str())
        || ((reserved.starts_with("com") || reserved.starts_with("lpt"))
            && reserved.len() == 4
            && matches!(reserved.as_bytes()[3], b'1'..=b'9'))
    {
        return Err(safe(
            "Use a folder name of up to 80 characters without slashes or special path characters.",
        ));
    }
    Ok(name)
}
pub(crate) fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub(crate) fn valid_video(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
pub(crate) fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(crate) fn jobs_document(jobs_root: &Path, job: &Value) -> Result<Value> {
    let id = str_field(job, "id");
    if !valid_id(id) {
        return Err(safe("The transcript record identifier is invalid."));
    }
    let data = fs::read_path(
        &jobs_root.join(id).join("transcript.json"),
        128 * 1024 * 1024,
    )?;
    let value: Value = serde_json::from_slice(&data)?;
    if value["complete"] != true {
        return Err(safe("The saved transcript is incomplete."));
    }
    Ok(value)
}
impl Library {
    pub fn open(data_dir: impl AsRef<Path>) -> Result<Self> {
        std::fs::create_dir_all(data_dir.as_ref())?;
        let data_dir = std::fs::canonicalize(data_dir)?;
        let root = Dir::open(&data_dir)?;
        let mut state = match data_dir.join("library.json").symlink_metadata() {
            Ok(_) => serde_json::from_slice::<Value>(&root.read(
                "library.json",
                8 * 1024 * 1024,
                None,
            )?)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                json!({"folders":[],"folder_evidence":{},"folder_root_adopted":true,"default_folder_id":null,"export_settings":{"directory":"","format":"txt","enabled":false}})
            }
            Err(_) => return Err(safe("The library settings could not be read safely.")),
        };
        if !state.is_object() {
            return Err(safe(
                "The saved transcript library settings could not be read.",
            ));
        }
        if state.get("folders").is_none() {
            state["folders"] = json!([]);
        }
        let list = state
            .get_mut("folders")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| safe("The saved folders could not be read."))?;
        let mut ids = HashSet::new();
        let mut names = HashSet::new();
        for item in list {
            let name = folder_name(str_field(item, "name"))?;
            let id = str_field(item, "id");
            if !valid_id(id) || !ids.insert(id.to_owned()) || !names.insert(casefold(&name)) {
                return Err(safe("The saved folders could not be read."));
            }
            item["name"] = json!(name);
        }
        if !state["default_folder_id"].is_null()
            && !ids.contains(str_field(&state, "default_folder_id"))
        {
            state["default_folder_id"] = Value::Null
        }
        if !state["folder_evidence"].is_object() {
            state["folder_evidence"] = json!({})
        }
        if state.get("export_settings").is_none() {
            state["export_settings"] = json!({})
        } else if !state["export_settings"].is_object() {
            return Err(safe("The saved destination settings could not be read."));
        }
        let prefs = &mut state["export_settings"];
        if prefs.get("directory").is_none() {
            prefs["directory"] = json!("")
        };
        if !prefs["directory"].is_string() {
            return Err(safe("The saved destination could not be read."));
        }
        if !str_field(prefs, "directory").is_empty()
            && !Path::new(str_field(prefs, "directory")).is_absolute()
        {
            return Err(safe(
                "The saved destination is not an absolute folder path.",
            ));
        }
        if !["txt", "md"].contains(&str_field(prefs, "format")) {
            prefs["format"] = json!("txt")
        };
        if !prefs["enabled"].is_boolean() {
            prefs["enabled"] = json!(false)
        }
        Ok(Self {
            data_dir,
            state: RwLock::new(state),
            operations: Mutex::new(()),
            attachment_lock: Mutex::new(()),
            hash_cache: Mutex::new(HashMap::new()),
            document_cache: Mutex::new(HashMap::new()),
        })
    }
    fn commit(&self, state: &Value) -> Result<()> {
        Dir::open(&self.data_dir)?.atomic_json("library.json", state)?;
        Ok(())
    }
    pub fn snapshot(&self) -> Value {
        let s = self.state.read();
        json!({"folders":s["folders"],"default_folder_id":s["default_folder_id"]})
    }
    pub fn export_settings(&self) -> Value {
        self.state.read()["export_settings"].clone()
    }
    pub fn validate_folder(&self, id: Option<&str>) -> Result<()> {
        let state = self.state.read();
        Self::validate_in(&state, id)
    }
    fn validate_in(state: &Value, id: Option<&str>) -> Result<()> {
        if id.is_some_and(|id| {
            !state["folders"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| str_field(f, "id") == id)
        }) {
            return Err(safe("That library folder no longer exists."));
        }
        Ok(())
    }
    pub(crate) fn scope(&self, id: Option<&str>) -> Result<PathBuf> {
        let s = self.state.read();
        Self::validate_in(&s, id)?;
        let directory = str_field(&s["export_settings"], "directory");
        if directory.is_empty() {
            return Err(safe("Choose a main save folder first."));
        }
        let mut path = PathBuf::from(directory);
        if let Some(id) = id {
            let f = s["folders"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| str_field(f, "id") == id)
                .unwrap();
            path.push(str_field(f, "name"))
        }
        Ok(path)
    }
    pub fn create_folder(&self, name: &str) -> Result<Value> {
        let name = folder_name(name)?;
        let mut current = self.state.write();
        if current["folders"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| casefold(str_field(f, "name")) == casefold(&name))
        {
            return Err(safe("A library folder with that name already exists."));
        }
        let mut next = current.clone();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let directory = str_field(&next["export_settings"], "directory");
        let evidence = if !directory.is_empty() {
            let root = Dir::open(Path::new(directory))?;
            let child = root.mkdir(&name)?;
            Some(
                json!({"root":directory,"root_identity":root.identity()?.pair(),"identity":child.identity()?.pair()}),
            )
        } else {
            None
        };
        let folder = json!({"id":id,"name":name});
        next["folders"].as_array_mut().unwrap().push(folder.clone());
        if let Some(e) = evidence {
            next["folder_evidence"][&id] = e
        }
        self.commit(&next)?;
        *current = next;
        Ok(folder)
    }
    pub fn set_default(&self, id: Option<&str>) -> Result<Value> {
        let mut current = self.state.write();
        Self::validate_in(&current, id)?;
        let mut next = current.clone();
        next["default_folder_id"] = json!(id);
        self.commit(&next)?;
        *current = next;
        Ok(json!({"folders":current["folders"],"default_folder_id":current["default_folder_id"]}))
    }
    pub fn set_export_settings(&self, enabled: bool, format: &str) -> Result<Value> {
        if !["txt", "md"].contains(&format) {
            return Err(safe("Choose TXT or Markdown."));
        }
        let mut current = self.state.write();
        if enabled && str_field(&current["export_settings"], "directory").is_empty() {
            return Err(safe(
                "Choose a destination folder before enabling automatic saving.",
            ));
        }
        let mut next = current.clone();
        next["export_settings"]["enabled"] = json!(enabled);
        next["export_settings"]["format"] = json!(format);
        self.commit(&next)?;
        *current = next;
        Ok(current["export_settings"].clone())
    }
    pub fn set_directory(&self, directory: &Path) -> Result<Value> {
        let directory = std::fs::canonicalize(directory)
            .map_err(|_| safe("The selected folder is unavailable."))?;
        let root = Dir::open(&directory)?;
        let probe = format!(".scribe-write-check-{}", uuid::Uuid::new_v4().simple());
        root.create(&probe, b"")?;
        root.unlink(&probe, false)?;
        let mut current = self.state.write();
        let mut next = current.clone();
        if str_field(&next["export_settings"], "directory").is_empty() {
            next["export_settings"]["enabled"] = json!(true)
        } else if Path::new(str_field(&next["export_settings"], "directory")) != directory {
            next["folder_root_adopted"] = json!(true)
        }
        next["export_settings"]["directory"] = json!(directory);
        self.commit(&next)?;
        *current = next;
        Ok(current["export_settings"].clone())
    }
    fn folder_inventory(&self) -> Result<(Value, Vec<(String, Identity)>)> {
        let prefs = self.export_settings();
        let path = Path::new(str_field(&prefs, "directory"));
        let root = Dir::open(path)?;
        let before = root.identity()?;
        let entries = root.entries()?;
        if root.identity()? != before || Dir::open(path)?.identity()?.pair() != before.pair() {
            return Err(fs::io_error());
        }
        Ok((json!({"root":path,"root_identity":before.pair()}), entries))
    }
    pub fn reconcile_folders(&self, channel_ids: &HashSet<String>, jobs: &[Value]) -> Result<bool> {
        let Ok((root, children)) = self.folder_inventory() else {
            return Ok(false);
        };
        let mut current = self.state.write();
        if current["export_settings"]["directory"] != root["root"] {
            return Ok(false);
        }
        let mut next = current.clone();
        let adopt = next["folder_root_adopted"] != true;
        next["folder_root_adopted"] = json!(true);
        let mut evidence = next["folder_evidence"].clone();
        let mut kept = Vec::new();
        let mut claimed: HashSet<String> = next["folders"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| casefold(str_field(f, "name")))
            .collect();
        for mut folder in next["folders"].as_array().unwrap().clone() {
            let id = str_field(&folder, "id").to_owned();
            let previous = &evidence[&id];
            let same_root = previous["root"] == root["root"]
                && previous["root_identity"] == root["root_identity"];
            let old_name = str_field(&folder, "name").to_owned();
            let mut details = children.iter().find(|(n, _)| n == &old_name);
            if details.is_none() && same_root {
                let matches: Vec<_> = children
                    .iter()
                    .filter(|(_, m)| m.is_dir() && m.pair() == previous["identity"])
                    .collect();
                if matches.len() == 1
                    && !claimed.contains(&casefold(&matches[0].0))
                    && folder_name(&matches[0].0).ok().as_deref() == Some(matches[0].0.as_str())
                {
                    claimed.remove(&casefold(&old_name));
                    claimed.insert(casefold(&matches[0].0));
                    folder["name"] = json!(matches[0].0);
                    details = Some(matches[0]);
                }
            }
            if let Some((_, info)) = details {
                if info.is_dir() {
                    evidence[&id] = json!({"root":root["root"],"root_identity":root["root_identity"],"identity":info.pair()})
                }
                kept.push(folder);
                continue;
            }
            let wanted = PathBuf::from(str_field(&root, "root")).join(str_field(&folder, "name"));
            let known = adopt
                || same_root
                || jobs.iter().any(|j| {
                    let p = &j["export_evidence"];
                    p["root"] == root["root"]
                        && p["root_identity"] == root["root_identity"]
                        && p["files"].as_array().is_some_and(|files| {
                            files
                                .iter()
                                .any(|f| Path::new(str_field(f, "path")).starts_with(&wanted))
                        })
                });
            if known && !channel_ids.contains(&id) {
                evidence.as_object_mut().unwrap().remove(&id);
                if next["default_folder_id"] == id {
                    next["default_folder_id"] = Value::Null
                }
            } else {
                kept.push(folder)
            }
        }
        next["folders"] = json!(kept);
        next["folder_evidence"] = evidence;
        if next != *current {
            self.commit(&next)?;
            *current = next
        }
        Ok(true)
    }
    pub fn remove_missing_folder(&self, id: &str, channel_ids: &HashSet<String>) -> Result<()> {
        self.validate_folder(Some(id))?;
        if channel_ids.contains(id) {
            return Err(safe("Channel folders cannot be removed with this action."));
        }
        let (root, children) = self.folder_inventory()?;
        let mut current = self.state.write();
        let f = current["folders"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| str_field(f, "id") == id)
            .ok_or_else(|| safe("Folder no longer exists."))?;
        if current["export_settings"]["directory"] != root["root"]
            || children.iter().any(|(n, _)| n == str_field(f, "name"))
        {
            return Err(safe(
                "This action only removes an entry for an already missing folder.",
            ));
        }
        let mut next = current.clone();
        next["folders"]
            .as_array_mut()
            .unwrap()
            .retain(|f| str_field(f, "id") != id);
        next["folder_evidence"].as_object_mut().unwrap().remove(id);
        if next["default_folder_id"] == id {
            next["default_folder_id"] = Value::Null
        }
        self.commit(&next)?;
        *current = next;
        Ok(())
    }
    pub(crate) fn tracked(job: &Value, base: &Path) -> Vec<PathBuf> {
        let mut values = vec![job["export_path"].clone()];
        if let Some(paths) = job["export_paths"].as_array() {
            values.extend(paths.iter().cloned())
        }
        if job["export_evidence"]["root"]
            .as_str()
            .is_some_and(|s| Path::new(s) == base)
            && let Some(files) = job["export_evidence"]["files"].as_array()
        {
            values.extend(files.iter().map(|f| f["path"].clone()))
        }
        let mut out = Vec::new();
        for value in values {
            if let Some(s) = value.as_str() {
                let p = PathBuf::from(s);
                if p.is_absolute()
                    && p.strip_prefix(base)
                        .is_ok_and(|r| r.components().next().is_some())
                    && !p.components().any(|c| {
                        matches!(
                            c,
                            std::path::Component::ParentDir | std::path::Component::CurDir
                        )
                    })
                    && !out.contains(&p)
                {
                    out.push(p)
                }
            }
        }
        out
    }
}
