use crate::{
    Library,
    fs::{self, Dir, Identity, Tree},
    jobs_document, str_field,
};
use scribe_common::{Error, Result, export_filename, render_document};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};
#[derive(Clone, Debug)]
pub struct ExportOutcome {
    pub path: String,
    pub replacements: HashMap<String, String>,
    pub removed_duplicate_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_replacement_never_reports_success_or_removes_files_from_replacement() {
        for stage in [
            "before_scan",
            "before_write",
            "before_delete",
            "before_success",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let base = std::fs::canonicalize(temp.path()).unwrap();
            let save = base.join("Transcripts");
            let detached = base.join("Detached");
            std::fs::create_dir(&save).unwrap();
            let data = base.join("private");
            let library = Library::open(&data).unwrap();
            library.set_directory(&save).unwrap();
            let id = "1".repeat(32);
            let doc = json!({"title":"Original","source_url":"https://www.youtube.com/watch?v=abcdefghijk","full_text":"Every word","complete":true});
            scribe_common::atomic_json(&data.join("jobs").join(&id).join("transcript.json"), &doc)
                .unwrap();
            let bytes = render_document(&doc, "txt").unwrap();
            std::fs::write(save.join("Original.txt"), &bytes).unwrap();
            std::fs::write(save.join("Duplicate.txt"), &bytes).unwrap();
            let job = json!({"id":id,"video_id":"abcdefghijk","title":"Original","status":"completed","document_available":true});
            let path = save.clone();
            let old = detached.clone();
            let mut changed = false;
            EXPORT_HOOK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |point| {
                    if point == stage && !changed {
                        changed = true;
                        std::fs::rename(&path, &old).unwrap();
                        std::fs::create_dir(&path).unwrap();
                        std::fs::write(path.join("Original.txt"), "Unrelated replacement").unwrap();
                    }
                }))
            });
            let result = library.export_job(&job, &data.join("jobs"), None);
            EXPORT_HOOK.with(|hook| *hook.borrow_mut() = None);
            assert!(
                result.is_err(),
                "A root replaced at {stage} must not return a misleading path"
            );
            assert_eq!(
                std::fs::read_to_string(save.join("Original.txt")).unwrap(),
                "Unrelated replacement"
            );
            assert_eq!(
                std::fs::read_to_string(detached.join("Original.txt")).unwrap(),
                bytes
            );
            if stage != "before_success" {
                assert!(detached.join("Duplicate.txt").is_file());
            }
        }
    }
    #[test]
    fn target_folder_replacement_is_detected_before_duplicate_deletion() {
        let temp = tempfile::tempdir().unwrap();
        let base = std::fs::canonicalize(temp.path()).unwrap();
        let save = base.join("Transcripts");
        std::fs::create_dir(&save).unwrap();
        let data = base.join("private");
        let library = Library::open(&data).unwrap();
        library.set_directory(&save).unwrap();
        let folder = library.create_folder("Motion").unwrap();
        let id = "2".repeat(32);
        let doc = json!({"title":"Original","source_url":"https://www.youtube.com/watch?v=abcdefghijk","full_text":"Every word","complete":true});
        scribe_common::atomic_json(&data.join("jobs").join(&id).join("transcript.json"), &doc)
            .unwrap();
        let bytes = render_document(&doc, "txt").unwrap();
        std::fs::write(save.join("Original.txt"), &bytes).unwrap();
        let job = json!({"id":id,"video_id":"abcdefghijk","title":"Original","status":"completed","document_available":true,"folder_id":folder["id"]});
        let path = save.clone();
        let mut changed = false;
        EXPORT_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |point| {
                if point == "before_delete" && !changed {
                    changed = true;
                    std::fs::rename(path.join("Motion"), path.join("OldMotion")).unwrap();
                    std::fs::create_dir(path.join("Motion")).unwrap();
                }
            }))
        });
        let result = library.export_job(&job, &data.join("jobs"), None);
        EXPORT_HOOK.with(|hook| *hook.borrow_mut() = None);
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_to_string(save.join("Original.txt")).unwrap(),
            bytes
        );
        assert_eq!(std::fs::read_dir(save.join("Motion")).unwrap().count(), 0);
        assert!(save.join("OldMotion/Original.txt").is_file());
    }
}
#[derive(Clone)]
struct Record {
    parts: Vec<String>,
    identity: Identity,
}
fn matches(root: &Dir, record: &Record, content: &[u8]) -> bool {
    record.parts.split_last().is_some_and(|(name, parent)| {
        root.nested(parent)
            .and_then(|p| {
                let current = p.stat(name)?;
                // Renaming/unlinking a duplicate hardlink changes the shared
                // inode's ctime. Exact bytes and the stable export identity
                // remain authoritative; cache identities still include ctime.
                if !pair_same(&record.identity, &current) {
                    return Err(fs::io_error());
                }
                p.read(name, content.len() as u64 + 1, Some(&current))
            })
            .is_ok_and(|b| b == content)
    })
}
fn pair_same(a: &Identity, b: &Identity) -> bool {
    a.dev == b.dev
        && a.ino == b.ino
        && a.size == b.size
        && a.mtime == b.mtime
        && a.is_file()
        && b.is_file()
}
fn unlink_verified(
    root: &Dir,
    record: &Record,
    keeper: &Record,
    content: &[u8],
    current: &impl Fn() -> Result<()>,
) -> Result<()> {
    current()?;
    if !matches(root, record, content) || !matches(root, keeper, content) {
        return Err(fs::safe(
            "A transcript changed during duplicate cleanup. Its copies were preserved.",
        ));
    }
    let (name, parts) = record.parts.split_last().ok_or_else(fs::io_error)?;
    let parent = root.nested(parts)?;
    let stage = format!(
        ".full-transcript-duplicate-{}.tmp",
        uuid::Uuid::new_v4().simple()
    );
    parent.rename(name, &parent, &stage)?;
    let result = (|| {
        let staged = parent.stat(&stage)?;
        if !pair_same(&record.identity, &staged)
            || !matches(root, keeper, content)
            || parent.read(&stage, content.len() as u64 + 1, Some(&staged))? != content
        {
            return Err(fs::safe(
                "A transcript changed during duplicate cleanup. Its recovery copy was retained.",
            ));
        }
        current()?;
        parent.unlink(&stage, false)?;
        parent.sync()
    })();
    if result.is_err() {
        parent.restore_link(&stage, name)
    }
    result
}
#[cfg(test)]
type ExportHook = Box<dyn FnMut(&str)>;
#[cfg(test)]
thread_local! { static EXPORT_HOOK: std::cell::RefCell<Option<ExportHook>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn hook(stage: &str) {
    EXPORT_HOOK.with(|h| {
        if let Some(h) = h.borrow_mut().as_mut() {
            h(stage);
        }
    });
}
impl Library {
    pub fn export_job(
        &self,
        job: &Value,
        jobs_root: &Path,
        format: Option<&str>,
    ) -> Result<String> {
        Ok(self.export_job_detail(job, jobs_root, format)?.path)
    }
    pub fn export_job_detail(
        &self,
        job: &Value,
        jobs_root: &Path,
        format: Option<&str>,
    ) -> Result<ExportOutcome> {
        let _guard = self.operations.lock();
        if job["status"] != "completed" || job["document_available"] != true {
            return Err(Error::new(
                409,
                "Only completed transcripts can be exported. Removed videos will not be recreated.",
            ));
        }
        let prefs = self.export_settings();
        let chosen = format.unwrap_or_else(|| str_field(&prefs, "format"));
        if !["txt", "md", "json"].contains(&chosen) {
            return Err(fs::safe("Choose TXT, Markdown, or JSON."));
        }
        let base = self.scope(None)?;
        let presence = self.scan_export_presence(std::slice::from_ref(job), jobs_root, true)?;
        let id = str_field(job, "id");
        if presence["absent"]
            .as_array()
            .is_some_and(|v| v.iter().any(|x| x == id))
        {
            return Err(Error::new(
                409,
                "This transcript was removed from your save folder and will not be recreated.",
            ));
        }
        if presence["uncertain"]
            .as_array()
            .is_some_and(|v| v.iter().any(|x| x == id))
        {
            return Err(Error::new(
                409,
                "The previous saved file could not be checked. Reconnect the save folder before trying again.",
            ));
        }
        let document = jobs_document(jobs_root, job)?;
        let content = render_document(&document, chosen)?.into_bytes();
        let destination = self.scope(job["folder_id"].as_str())?;
        if self.data_dir.starts_with(&base) {
            return Err(fs::safe(
                "The save folder must not contain the helper's private recovery folder.",
            ));
        }
        let root = Dir::open(&base)?;
        let root_identity = root.identity()?.pair();
        let parts = destination
            .strip_prefix(&base)
            .map_err(|_| fs::io_error())?
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let mut target = root.clone_dir()?;
        for part in &parts {
            if Dir::open(&base)?.identity()?.pair() != root_identity {
                return Err(fs::io_error());
            }
            target = target.mkdir(part)?
        }
        let target_identity = target.identity()?.pair();
        let current = || -> Result<()> {
            if self.scope(None)? != base
                || self.scope(job["folder_id"].as_str())? != destination
                || Dir::open(&base)?.identity()?.pair() != root_identity
                || Dir::open(&destination)?.identity()?.pair() != target_identity
                || root.nested(&parts)?.identity()?.pair() != target_identity
            {
                return Err(fs::safe(
                    "The destination changed while saving. Existing transcript copies were preserved; save again after checking the folder.",
                ));
            }
            Ok(())
        };
        #[cfg(test)]
        hook("before_scan");
        let tree = Tree::scan(&base, false, false, BTreeSet::new())?;
        if tree.root.identity()?.pair() != root_identity {
            return Err(fs::io_error());
        }
        current()?;
        let filename = export_filename(str_field(job, "title"), str_field(job, "video_id"), chosen);
        let stem = Path::new(&filename)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Transcript");
        let legacy = format!("{stem} [{}]", str_field(job, "video_id"));
        let mut matching = Vec::new();
        for (path, info) in &tree.inventory {
            if info.is_file()
                && info.size == content.len() as u64
                && Path::new(path.last().unwrap())
                    .extension()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case(chosen))
            {
                let record = Record {
                    parts: path.clone(),
                    identity: info.clone(),
                };
                if matches(&root, &record, &content) {
                    matching.push(record)
                }
            }
        }
        let mut local: Vec<_> = matching
            .iter()
            .filter(|r| {
                r.parts[..r.parts.len() - 1] == parts
                    && r.parts.last().is_some_and(|n| {
                        let s = Path::new(n)
                            .file_stem()
                            .and_then(|x| x.to_str())
                            .unwrap_or("");
                        s != legacy
                            && !(s.starts_with(&(legacy.clone() + " (")) && s.ends_with(')'))
                    })
            })
            .cloned()
            .collect();
        local.sort_by_key(|r| (r.parts.last() != Some(&filename), r.parts.last().cloned()));
        tree.unchanged()?;
        #[cfg(test)]
        hook("before_write");
        current()?;
        let created = local.is_empty();
        let keeper = if let Some(record) = local.first() {
            record.clone()
        } else {
            let mut written = None;
            for attempt in 0..10000 {
                let name = if attempt == 0 {
                    filename.clone()
                } else {
                    format!("{stem} ({}).{chosen}", attempt + 1)
                };
                current()?;
                if target.stat(&name).is_ok() {
                    continue;
                }
                match target.create(&name, &content) {
                    Ok(identity) => {
                        let mut location = parts.clone();
                        location.push(name);
                        written = Some(Record {
                            parts: location,
                            identity,
                        });
                        break;
                    }
                    Err(e) => {
                        if target.stat(&name).is_ok() {
                            continue;
                        }
                        return Err(e);
                    }
                }
            }
            written.ok_or_else(|| fs::safe("Too many documents share this filename."))?
        };
        let keeper_path = base.join(keeper.parts.iter().collect::<PathBuf>());
        let mut outcome = ExportOutcome {
            path: keeper_path.to_string_lossy().into_owned(),
            replacements: HashMap::new(),
            removed_duplicate_count: 0,
        };
        let cleanup = (|| -> Result<()> {
            current()?;
            for record in &matching {
                if record.parts == keeper.parts {
                    continue;
                }
                if !matches(&root, record, &content) || !matches(&root, &keeper, &content) {
                    return Err(fs::io_error());
                }
                let backup_root = Dir::open(&self.data_dir)?.mkdir("duplicate-backups")?;
                let backup_id = uuid::Uuid::new_v4().simple().to_string();
                let backup_name = format!("{backup_id}.{chosen}");
                backup_root.create(&backup_name, &content)?;
                if backup_root.read(&backup_name, content.len() as u64 + 1, None)? != content {
                    return Err(fs::io_error());
                }
                let original = base.join(record.parts.iter().collect::<PathBuf>());
                backup_root.atomic_json(&format!("{backup_id}.recovery.json"),&json!({"original_path":original,"keeper_path":keeper_path,"sha256":hex::encode(Sha256::digest(&content)),"backup_file":backup_name}))?;
                #[cfg(test)]
                hook("before_delete");
                unlink_verified(&root, record, &keeper, &content, &current)?;
                outcome.replacements.insert(
                    original.to_string_lossy().into_owned(),
                    outcome.path.clone(),
                );
                outcome.removed_duplicate_count += 1;
            }
            #[cfg(test)]
            hook("before_success");
            current()?;
            if !matches(&root, &keeper, &content) {
                return Err(fs::io_error());
            }
            Ok(())
        })();
        if let Err(error) = cleanup {
            if created
                && current().is_ok()
                && let Some(original) = matching.iter().find(|r| matches(&root, r, &content))
            {
                let _ = unlink_verified(&root, &keeper, original, &content, &current);
            }
            return Err(error);
        }
        Ok(outcome)
    }
}
