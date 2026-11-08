use crate::{
    Library,
    fs::{self, Dir, Identity},
    str_field, valid_id,
};
use scribe_common::{Error, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
const MAX_BYTES: u64 = 128 * 1024 * 1024;
const MAX_ENTRIES: usize = 32;
const RECENT: f64 = 300.;
#[cfg(test)]
type PrepareHook = Box<dyn FnMut(&str, &Library)>;
#[cfg(test)]
thread_local! { static PREPARE_HOOK: std::cell::RefCell<Option<PrepareHook>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn hook(stage: &str, library: &Library) {
    PREPARE_HOOK.with(|h| {
        if let Some(h) = h.borrow_mut().as_mut() {
            h(stage, library);
        }
    });
}
fn seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn public(record: &Value) -> Value {
    let mut out = json!({"id":record["id"],"filename":record["filename"],"count":record["count"],"bytes":record["bytes"]});
    if record["context"].get("excluded_folders").is_some() {
        out["root_token"] = record["context"]["root_token"].clone();
        out["excluded_folders"] = record["context"]["excluded_folders"].clone()
    }
    out
}
struct Record {
    value: Value,
    identity: Identity,
    leased: bool,
}
impl Library {
    fn attachment_root(&self) -> Result<Dir> {
        Dir::open(&self.data_dir)?.mkdir("attachments")
    }
    fn attachment_record(root: &Dir, id: &str, verify: bool) -> Result<Record> {
        if !valid_id(id) {
            return Err(Error::new(
                404,
                "This prepared attachment could not be found. Copy the folder again.",
            ));
        }
        let child = root.child(id)?;
        let value: Value =
            serde_json::from_slice(&child.read("metadata.json", 512 * 1024, None)?)?;
        let filename = str_field(&value, "filename");
        if str_field(&value, "id") != id
            || !fs::safe_part(filename)
            || !filename.ends_with(".zip")
            || filename.len() > 240
            || value["count"].as_u64().is_none()
            || !value["bytes"]
                .as_u64()
                .is_some_and(|n| n > 0 && n <= MAX_BYTES)
            || !value["context"].is_object()
            || str_field(&value, "sha256").len() != 64
            || value["used_at"].as_f64().is_none()
        {
            return Err(fs::safe("This attachment's metadata is invalid."));
        }
        if verify {
            let content = child.read(filename, value["bytes"].as_u64().unwrap(), None)?;
            if content.len() as u64 != value["bytes"].as_u64().unwrap()
                || hex::encode(Sha256::digest(&content)) != str_field(&value, "sha256")
            {
                return Err(Error::new(
                    409,
                    "This prepared attachment changed. Prepare it again.",
                ));
            }
        }
        let leased = child
            .entries()?
            .iter()
            .any(|(n, _)| ["copy-in-progress", "clipboard-unknown"].contains(&n.as_str()));
        Ok(Record {
            value,
            identity: child.identity()?,
            leased,
        })
    }
    fn touch_attachment(root: &Dir, record: &mut Value) -> Result<()> {
        record["used_at"] = json!(seconds());
        root.child(str_field(record, "id"))?
            .atomic_json("metadata.json", record)
    }
    fn unknown_bytes(root: &Dir, name: &str, info: &Identity) -> u64 {
        if info.is_file() {
            return info.size;
        }
        if !info.is_dir() {
            return MAX_BYTES;
        }
        root.child(name)
            .and_then(|d| d.entries())
            .map(|entries| {
                if entries.iter().any(|(_, m)| !m.is_file()) {
                    MAX_BYTES
                } else {
                    entries.iter().map(|(_, m)| m.size).sum()
                }
            })
            .unwrap_or(MAX_BYTES)
    }
    fn remove_owned(dir: &Dir, name: &str, expected: &Identity) -> Result<()> {
        let stage = format!(".discard-{}", uuid::Uuid::new_v4().simple());
        dir.rename(name, dir, &stage)?;
        let actual = dir.stat(&stage)?;
        if actual.dev == expected.dev && actual.ino == expected.ino && actual.is_file() {
            dir.unlink(&stage, false)
        } else {
            dir.restore_link(&stage, name);
            Err(fs::io_error())
        }
    }
    fn prune_attachment(root: &Dir, record: &Record) -> Result<bool> {
        let id = str_field(&record.value, "id");
        let filename = str_field(&record.value, "filename");
        let stage = format!(".prune-{}", uuid::Uuid::new_v4().simple());
        root.rename(id, root, &stage)?;
        let result = (|| -> Result<bool> {
            let child = root.child(&stage)?;
            let info = child.identity()?;
            if info.dev != record.identity.dev || info.ino != record.identity.ino {
                return Ok(false);
            }
            let entries = child.entries()?;
            if entries.len() != 2
                || entries
                    .iter()
                    .any(|(n, m)| !m.is_file() || (n != "metadata.json" && n != filename))
            {
                return Ok(false);
            }
            let content = child.read(filename, MAX_BYTES, None)?;
            if hex::encode(Sha256::digest(&content)) != str_field(&record.value, "sha256") {
                return Ok(false);
            }
            for (name, identity) in entries {
                Self::remove_owned(&child, &name, &identity)?
            }
            root.unlink(&stage, true)?;
            root.sync()?;
            Ok(true)
        })();
        if !matches!(result, Ok(true)) && root.stat(id).is_err() {
            let _ = root.rename(&stage, root, id);
        }
        result
    }
    fn discard_new_attachment(
        root: &Dir,
        id: &str,
        owned: &Identity,
        files: &[(String, Identity)],
    ) {
        let stage = format!(".discard-new-{}", uuid::Uuid::new_v4().simple());
        if root.rename(id, root, &stage).is_err() {
            return;
        }
        let result = (|| -> Result<()> {
            let child = root.child(&stage)?;
            if child.identity()?.pair() != owned.pair() {
                return Err(fs::io_error());
            }
            let entries = child.entries()?;
            if entries.iter().any(|(name, found)| {
                !files.iter().any(|(expected_name, expected)| {
                    name == expected_name && found.is_file() && found.pair() == expected.pair()
                })
            }) {
                return Err(fs::io_error());
            }
            for (name, info) in entries {
                Self::remove_owned(&child, &name, &info)?;
            }
            if root.stat(&stage)?.pair() != owned.pair() {
                return Err(fs::io_error());
            }
            root.unlink(&stage, true)?;
            root.sync()
        })();
        if result.is_err() && root.stat(id).is_err() {
            let _ = root.rename(&stage, root, id);
        }
    }
    pub fn prepare_attachment(
        &self,
        folder_id: Option<&str>,
        excluded_folders: Option<&[String]>,
        root_token: Option<&str>,
    ) -> Result<Value> {
        let _guard = self.attachment_lock.lock();
        if folder_id.is_some() && (excluded_folders.is_some() || root_token.is_some()) {
            return Err(fs::safe("Folder exclusions apply only to the All ZIP."));
        }
        if excluded_folders.is_none() && root_token.is_some() {
            return Err(fs::safe(
                "Send the folder selection with its save location token.",
            ));
        }
        let context = self.attachment_context(folder_id, excluded_folders)?;
        if excluded_folders.is_some() && root_token != context["root_token"].as_str() {
            return Err(Error::new(
                409,
                "The save location changed. Refresh the folder list before copying All.",
            ));
        }
        #[cfg(test)]
        hook("before_archive", self);
        let archive = self.archive(&context, excluded_folders.unwrap_or(&[]))?;
        #[cfg(test)]
        hook("after_archive", self);
        if self.attachment_context(folder_id, excluded_folders)? != context {
            return Err(Error::new(
                409,
                "The save location changed. Copy the folder again.",
            ));
        }
        let digest = hex::encode(Sha256::digest(&archive.bytes));
        let name = context["folder_name"]
            .as_str()
            .or_else(|| {
                std::path::Path::new(str_field(&context, "root"))
                    .file_name()
                    .and_then(|s| s.to_str())
            })
            .ok_or_else(fs::io_error)?;
        let mut label = name.to_owned();
        while label.len() + 4 > 220 {
            label.pop();
        }
        let filename = format!("{label}.zip");
        let root = self.attachment_root()?;
        let mut records = Vec::new();
        let mut occupied = 0;
        let mut size = 0u64;
        for (name, info) in root.entries()? {
            if name == "pin.json" {
                continue;
            }
            occupied += 1;
            if occupied > MAX_ENTRIES + 64 {
                return Err(fs::safe(
                    "The attachment cache needs attention. Existing copied files were kept.",
                ));
            }
            match Self::attachment_record(&root, &name, false) {
                Ok(mut record) => {
                    size = size.saturating_add(record.value["bytes"].as_u64().unwrap());
                    if record.value["sha256"] == digest
                        && record.value["context"] == context
                        && record.value["filename"] == filename
                    {
                        Self::attachment_record(&root, &name, true)?;
                        Self::touch_attachment(&root, &mut record.value)?;
                        return Ok(public(&record.value));
                    }
                    records.push(record)
                }
                Err(_) => size = size.saturating_add(Self::unknown_bytes(&root, &name, &info)),
            }
        }
        let pin = if root.entries()?.iter().any(|(name, _)| name == "pin.json") {
            let value: Value = serde_json::from_slice(&root.read("pin.json", 16384, None)?)?;
            let id = str_field(&value, "id");
            if !valid_id(id) {
                return Err(fs::safe(
                    "The attachment pin could not be checked safely. Existing copied files were kept.",
                ));
            }
            Some(id.to_owned())
        } else {
            None
        };
        records.sort_by(|a, b| {
            a.value["used_at"]
                .as_f64()
                .unwrap()
                .total_cmp(&b.value["used_at"].as_f64().unwrap())
        });
        for record in records {
            if occupied < MAX_ENTRIES && size + archive.bytes.len() as u64 <= MAX_BYTES {
                break;
            }
            if pin.as_deref() == record.value["id"].as_str()
                || record.leased
                || seconds() - record.value["used_at"].as_f64().unwrap() < RECENT
            {
                continue;
            }
            if Self::prune_attachment(&root, &record)? {
                occupied -= 1;
                size = size.saturating_sub(record.value["bytes"].as_u64().unwrap());
            }
        }
        if occupied >= MAX_ENTRIES || size + archive.bytes.len() as u64 > MAX_BYTES {
            return Err(fs::safe(
                "The attachment cache is full of recent or still-copied files. Wait five minutes or copy a smaller folder. Nothing was copied.",
            ));
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let child = root.mkdir(&id)?;
        let owned = child.identity()?;
        let mut files = vec![];
        let record = json!({"id":id,"filename":filename,"count":archive.count,"bytes":archive.bytes.len(),"sha256":digest,"context":context,"used_at":seconds()});
        let result = (|| -> Result<()> {
            files.push((filename.clone(), child.create(&filename, &archive.bytes)?));
            // This record is new and immutable until publication. Exclusive
            // creation gives rollback ownership even if its fsync fails.
            files.push((
                "metadata.json".into(),
                child.create("metadata.json", &serde_json::to_vec(&record)?)?,
            ));
            root.sync()
        })();
        if let Err(error) = result {
            Self::discard_new_attachment(&root, &id, &owned, &files);
            return Err(error);
        }
        Ok(public(&record))
    }
    pub fn begin_attachment_use(&self, id: &str) -> Result<(PathBuf, Value)> {
        let _guard = self.attachment_lock.lock();
        let root = self.attachment_root()?;
        let mut record = Self::attachment_record(&root, id, true)?;
        let excluded = record.value["context"]
            .get("excluded_folders")
            .map(|v| {
                v.as_array().ok_or_else(fs::io_error).and_then(|v| {
                    v.iter()
                        .map(|n| n.as_str().map(str::to_owned).ok_or_else(fs::io_error))
                        .collect::<Result<Vec<_>>>()
                })
            })
            .transpose()?;
        if self.attachment_context(
            record.value["context"]["folder_id"].as_str(),
            excluded.as_deref(),
        )? != record.value["context"]
        {
            return Err(Error::new(
                409,
                "The save location or folder changed. Prepare the attachment again.",
            ));
        }
        Self::touch_attachment(&root, &mut record.value)?;
        let child = root.child(id)?;
        if child.stat("copy-in-progress").is_ok() {
            child.rename("copy-in-progress", &child, "clipboard-unknown")?
        }
        child.create("copy-in-progress", b"")?;
        child.sync()?;
        Ok((
            self.data_dir
                .join("attachments")
                .join(id)
                .join(str_field(&record.value, "filename")),
            public(&record.value),
        ))
    }
    pub fn finish_attachment_use(&self, id: &str, copied: bool) -> Result<()> {
        let _guard = self.attachment_lock.lock();
        let root = self.attachment_root()?;
        Self::attachment_record(&root, id, true)?;
        if copied {
            root.atomic_json("pin.json", &json!({"id":id}))?
        }
        let child = root.child(id)?;
        let lease = child.stat("copy-in-progress")?;
        if !lease.is_file() {
            return Err(fs::io_error());
        }
        Self::remove_owned(&child, "copy-in-progress", &lease)?;
        if copied && let Ok(unknown) = child.stat("clipboard-unknown") {
            Self::remove_owned(&child, "clipboard-unknown", &unknown)?
        }
        child.sync()
    }
    pub fn protect_unknown_copy(&self, id: &str) -> Result<()> {
        let _guard = self.attachment_lock.lock();
        if !valid_id(id) {
            return Err(fs::io_error());
        }
        let root = self.attachment_root()?;
        let child = root.child(id)?;
        child.rename("copy-in-progress", &child, "clipboard-unknown")?;
        child.sync()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selection_aba_cannot_copy_another_root_under_the_original_context() {
        use std::io::{Cursor, Read};
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let a = base.join("Chosen");
        let b = base.join("Other");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        std::fs::write(a.join("selected.txt"), "Chosen contents").unwrap();
        std::fs::write(b.join("excluded.txt"), "Never include this").unwrap();
        let data = base.join("private");
        let library = Library::open(&data).unwrap();
        library.set_directory(&a).unwrap();
        let original = a.clone();
        let alternate = b.clone();
        PREPARE_HOOK.with(|h| {
            *h.borrow_mut() = Some(Box::new(move |stage, library| {
                library
                    .set_directory(if stage == "before_archive" {
                        &alternate
                    } else {
                        &original
                    })
                    .unwrap();
            }))
        });
        let record = library.prepare_attachment(None, None, None);
        PREPARE_HOOK.with(|h| *h.borrow_mut() = None);
        let record = record.unwrap();
        let path = data
            .join("attachments")
            .join(record["id"].as_str().unwrap())
            .join(record["filename"].as_str().unwrap());
        let mut archive = zip::ZipArchive::new(Cursor::new(std::fs::read(path).unwrap())).unwrap();
        let mut contents = String::new();
        archive
            .by_name("Chosen/selected.txt")
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "Chosen contents");
        assert!(archive.by_name("Other/excluded.txt").is_err());
    }
    #[test]
    fn failed_new_attachment_fsync_rolls_back_owned_cache_without_blocking_retry() {
        for failure in 0..3 {
            let temp = tempfile::tempdir().unwrap();
            let base = std::fs::canonicalize(temp.path()).unwrap();
            let save = base.join("Transcripts");
            std::fs::create_dir(&save).unwrap();
            std::fs::write(save.join("original.txt"), "exact original").unwrap();
            let library = Library::open(base.join("private")).unwrap();
            library.set_directory(&save).unwrap();
            fs::fail_sync_after(failure);
            assert!(library.prepare_attachment(None, None, None).is_err());
            assert_eq!(
                std::fs::read_dir(base.join("private/attachments"))
                    .unwrap()
                    .count(),
                0,
                "Failed sync {failure} left unprunable private files"
            );
            let retry = library.prepare_attachment(None, None, None).unwrap();
            assert_eq!(retry["count"], 1);
            assert_eq!(
                std::fs::read_to_string(save.join("original.txt")).unwrap(),
                "exact original"
            );
        }
    }
    #[test]
    fn failed_creation_cleanup_does_not_delete_replaced_directory_or_unowned_files() {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let root = Dir::open(&base).unwrap();
        let child = root.mkdir("new").unwrap();
        let owned = child.identity().unwrap();
        let file = child.create("owned.zip", b"archive").unwrap();
        child.create("foreign.txt", b"keep").unwrap();
        Library::discard_new_attachment(&root, "new", &owned, &[("owned.zip".into(), file)]);
        assert_eq!(
            std::fs::read(base.join("new/foreign.txt")).unwrap(),
            b"keep"
        );
        assert!(base.join("new/owned.zip").is_file());
        root.rename("new", &root, "old").unwrap();
        root.mkdir("new")
            .unwrap()
            .create("foreign.txt", b"replacement")
            .unwrap();
        Library::discard_new_attachment(&root, "new", &owned, &[]);
        assert_eq!(
            std::fs::read(base.join("new/foreign.txt")).unwrap(),
            b"replacement"
        );
        assert!(base.join("old/owned.zip").is_file());
    }
}
