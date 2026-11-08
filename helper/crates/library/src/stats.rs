use crate::{
    Library,
    fs::{self, Dir, Identity, Tree},
    str_field, valid_id,
};
use scribe_common::{Result, render_document};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
};
/// Bounded descriptors belong to one verification request, never a later poll.
#[derive(Default)]
struct ParentPool(VecDeque<(PathBuf, Dir)>);
impl ParentPool {
    fn get(&mut self, path: &Path) -> Result<&Dir> {
        if let Some(index) = self.0.iter().position(|(old, _)| old == path) {
            return Ok(&self.0[index].1);
        }
        let dir = Dir::open(path)?;
        if self.0.len() >= 32 {
            self.0.pop_front();
        }
        self.0.push_back((path.to_owned(), dir));
        Ok(&self.0.back().ok_or_else(fs::io_error)?.1)
    }
}

struct SavedWitness {
    id: String,
    source_parent: (u64, u64),
    source: Identity,
    parent: PathBuf,
    parent_pair: (u64, u64),
    name: String,
    file: Identity,
}
fn pair(identity: &Identity) -> (u64, u64) {
    (identity.dev, identity.ino)
}
#[cfg(test)]
type BatchHook = Box<dyn FnMut(&str)>;
#[cfg(test)]
thread_local! {static AFTER_BATCH_ENTRY:std::cell::RefCell<Option<BatchHook>>=const{std::cell::RefCell::new(None)};}
#[cfg(test)]
thread_local! {static BEFORE_SOURCE_RECHECK:std::cell::RefCell<Option<Box<dyn FnOnce()>>>=const{std::cell::RefCell::new(None)};}
impl Library {
    pub(crate) fn document_signatures(
        &self,
        path: &Path,
    ) -> Result<HashMap<(String, u64), String>> {
        let parent = Dir::open(path.parent().ok_or_else(fs::io_error)?)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(fs::io_error)?;
        self.document_signatures_in(&parent, name, path)
    }
    fn document_signatures_in(
        &self,
        parent: &Dir,
        name: &str,
        path: &Path,
    ) -> Result<HashMap<(String, u64), String>> {
        self.signature_record(parent, name, path)
            .map(|(_, signatures)| signatures)
    }
    fn signature_record(
        &self,
        parent: &Dir,
        name: &str,
        path: &Path,
    ) -> Result<(Identity, crate::DocumentSignatures)> {
        let identity = parent.stat(name)?;
        if !identity.is_file() {
            return Err(fs::io_error());
        }
        if let Some((old, signatures)) = self.document_cache.lock().get(path)
            && old == &identity
        {
            return Ok((identity, signatures.clone()));
        }
        let document: Value =
            serde_json::from_slice(&parent.read(name, 128 * 1024 * 1024, Some(&identity))?)?;
        if document["complete"] != true {
            return Err(fs::safe("Transcript is incomplete."));
        }
        let mut signatures = HashMap::new();
        for format in ["txt", "md", "json"] {
            let content = render_document(&document, format)?;
            signatures.insert(
                (format.to_owned(), content.len() as u64),
                hex::encode(Sha256::digest(content.as_bytes())),
            );
        }
        let mut cache = self.document_cache.lock();
        if cache.len() >= 4096 {
            cache.clear()
        }
        cache.insert(path.to_owned(), (identity.clone(), signatures.clone()));
        Ok((identity, signatures))
    }
    pub(crate) fn file_hash(
        &self,
        tree: &Tree,
        path: &[String],
        identity: &Identity,
    ) -> Result<String> {
        // This identity came from this request's fresh inventory. A cached
        // digest needs no second stat here: unchanged() rechecks every entry
        // after matching and is mandatory before publishing stats/presence.
        if tree.inventory.get(path) != Some(identity) {
            return Err(fs::io_error());
        }
        if let Some(hash) = self.hash_cache.lock().get(identity) {
            return Ok(hash.clone());
        }
        tree.with_parent(&path[..path.len() - 1], |parent| {
            self.hash_in(parent, path.last().ok_or_else(fs::io_error)?, identity)
        })
    }
    fn hash_in(&self, parent: &Dir, name: &str, identity: &Identity) -> Result<String> {
        if let Some(hash) = self.hash_cache.lock().get(identity) {
            if parent.stat(name)? != *identity {
                return Err(fs::io_error());
            }
            return Ok(hash.clone());
        }
        let bytes = parent.read(name, 128 * 1024 * 1024, Some(identity))?;
        let hash = hex::encode(Sha256::digest(&bytes));
        let mut cache = self.hash_cache.lock();
        if cache.len() >= 8192 {
            cache.clear()
        }
        cache.insert(identity.clone(), hash.clone());
        Ok(hash)
    }
    pub fn verified_export_path(&self, job: &Value, jobs_root: &Path) -> Option<String> {
        if job["status"] != "completed" {
            return None;
        }
        let id = str_field(job, "id");
        if !valid_id(id) {
            return None;
        }
        let signatures = self
            .document_signatures(&jobs_root.join(id).join("transcript.json"))
            .ok()?;
        for path in Self::tracked(job, Path::new("/")) {
            let format = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !["txt", "md", "json"].contains(&format.as_str()) {
                continue;
            }
            let matched = (|| -> Result<bool> {
                let parent = Dir::open(path.parent().ok_or_else(fs::io_error)?)?;
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .ok_or_else(fs::io_error)?;
                let identity = parent.stat(name)?;
                if !identity.is_file() {
                    return Ok(false);
                }
                let Some(expected) = signatures.get(&(format, identity.size)) else {
                    return Ok(false);
                };
                Ok(self.hash_in(&parent, name, &identity)? == *expected)
            })();
            if matched.is_ok_and(|v| v) {
                return Some(path.to_string_lossy().into_owned());
            }
        }
        None
    }
    pub fn is_saved_anywhere(&self, job: &Value, jobs_root: &Path) -> bool {
        self.verified_export_path(job, jobs_root).is_some()
    }
    /// Fresh saved facts for one response. Shared roots and a bounded pool of
    /// export parents avoid repeating absolute ancestor traversal for each job.
    /// Every positive fact is revalidated after the last entry; no result cache
    /// or descriptor survives this call. Unreadable/changed entries fail closed.
    pub fn saved_job_facts(&self, jobs: &[Value], jobs_root: &Path) -> HashMap<String, bool> {
        let mut facts = jobs
            .iter()
            .map(|job| str_field(job, "id"))
            .filter(|id| valid_id(id))
            .map(|id| (id.to_owned(), false))
            .collect::<HashMap<_, _>>();
        let Ok(sources) = Dir::open(jobs_root) else {
            return facts;
        };
        let Ok(source_root) = sources.identity() else {
            return facts;
        };
        let mut parents = ParentPool::default();
        let mut witnesses = Vec::new();
        for job in jobs {
            let id = str_field(job, "id");
            if job["status"] != "completed" || !valid_id(id) {
                continue;
            }
            let source = (|| -> Result<_> {
                let parent = sources.child(id)?;
                let parent_pair = pair(&parent.identity()?);
                let (identity, signatures) = self.signature_record(
                    &parent,
                    "transcript.json",
                    &jobs_root.join(id).join("transcript.json"),
                )?;
                Ok((parent_pair, identity, signatures))
            })();
            let Ok((source_parent, source, signatures)) = source else {
                continue;
            };
            for path in Self::tracked(job, Path::new("/")) {
                let format = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !["txt", "md", "json"].contains(&format.as_str()) {
                    continue;
                }
                let found = (|| -> Result<Option<SavedWitness>> {
                    let parent_path = path.parent().ok_or_else(fs::io_error)?;
                    let parent = parents.get(parent_path)?;
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .ok_or_else(fs::io_error)?;
                    let identity = parent.stat(name)?;
                    if !identity.is_file() {
                        return Ok(None);
                    }
                    let Some(expected) = signatures.get(&(format, identity.size)) else {
                        return Ok(None);
                    };
                    if self.hash_in(parent, name, &identity)? != *expected {
                        return Ok(None);
                    }
                    Ok(Some(SavedWitness {
                        id: id.to_owned(),
                        source_parent,
                        source: source.clone(),
                        parent: parent_path.to_owned(),
                        parent_pair: pair(&parent.identity()?),
                        name: name.to_owned(),
                        file: identity,
                    }))
                })();
                if let Ok(Some(witness)) = found {
                    witnesses.push(witness);
                    break;
                }
            }
            #[cfg(test)]
            AFTER_BATCH_ENTRY.with(|hook| {
                if let Some(callback) = hook.borrow_mut().as_mut() {
                    callback(id);
                }
            });
        }
        let root_current = || {
            sources
                .identity()
                .is_ok_and(|now| pair(&now) == pair(&source_root))
                && Dir::open(jobs_root)
                    .and_then(|dir| dir.identity())
                    .is_ok_and(|now| pair(&now) == pair(&source_root))
        };
        if !root_current() {
            return facts;
        }
        // Reopen each distinct export parent exactly once for the final pass.
        // Evicted pool entries are harmless: the accepted pair remains in each
        // witness and a replacement cannot inherit a previous positive fact.
        let mut groups: HashMap<&Path, Vec<&SavedWitness>> = HashMap::new();
        for witness in &witnesses {
            groups.entry(&witness.parent).or_default().push(witness);
        }
        for (path, witnesses) in groups {
            let Ok(parent) = Dir::open(path) else {
                continue;
            };
            let Ok(current_parent) = parent.identity() else {
                continue;
            };
            for witness in &witnesses {
                let valid = (|| -> Result<bool> {
                    if pair(&current_parent) != witness.parent_pair
                        || parent.stat(&witness.name)? != witness.file
                    {
                        return Ok(false);
                    }
                    let source_parent = sources.child(&witness.id)?;
                    Ok(pair(&source_parent.identity()?) == witness.source_parent
                        && source_parent.stat("transcript.json")? == witness.source
                        && pair(&sources.stat(&witness.id)?) == witness.source_parent)
                })();
                if valid.is_ok_and(|v| v) {
                    facts.insert(witness.id.clone(), true);
                }
            }
            if !Dir::open(path)
                .and_then(|dir| dir.identity())
                .is_ok_and(|now| pair(&now) == pair(&current_parent))
            {
                for witness in witnesses {
                    facts.insert(witness.id.clone(), false);
                }
            }
        }
        if !root_current() {
            for fact in facts.values_mut() {
                *fact = false;
            }
        }
        facts
    }
    pub fn is_saved(&self, job: &Value, jobs_root: &Path) -> bool {
        let prefs = self.export_settings();
        let base = Path::new(str_field(&prefs, "directory"));
        self.verified_export_path(job, jobs_root)
            .is_some_and(|p| !base.as_os_str().is_empty() && Path::new(&p).starts_with(base))
    }
    pub fn export_evidence(&self, job: &Value, _jobs_root: &Path) -> Option<Value> {
        let prefs = self.export_settings();
        let base = Path::new(str_field(&prefs, "directory"));
        let root = Dir::open(base).ok()?;
        let pair = root.identity().ok()?.pair();
        let mut files = HashMap::new();
        let previous = &job["export_evidence"];
        if previous["root"] == json!(base) && previous["root_identity"] == pair {
            for f in previous["files"].as_array().into_iter().flatten() {
                if let Some(path) = f["path"].as_str() {
                    files.insert(path.to_owned(), f.clone());
                }
            }
        }
        for path in Self::tracked(job, base) {
            let relative = path
                .strip_prefix(base)
                .ok()?
                .components()
                .map(|c| c.as_os_str().to_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()?;
            if let Some((name, parent)) = relative.split_last()
                && let Ok(dir) = root.nested(parent)
                && let Ok(info) = dir.stat(name)
                && info.is_file()
                && dir.read(name, info.size, Some(&info)).is_ok()
            {
                files.insert(
                    path.to_string_lossy().into_owned(),
                    json!({"path":path,"identity":info.pair()}),
                );
            }
        }
        if files.is_empty() {
            None
        } else {
            Some(
                json!({"root":base,"root_identity":pair,"files":files.into_values().collect::<Vec<_>>()}),
            )
        }
    }
    pub fn file_stats(
        &self,
        jobs: &[Value],
        jobs_root: &Path,
        folder_id: Option<&str>,
    ) -> Result<Value> {
        self.validate_folder(folder_id)?;
        let prefs = self.export_settings();
        let selected = !str_field(&prefs, "directory").is_empty();
        let directory = if selected {
            Some(self.scope(folder_id)?)
        } else {
            None
        };
        let mut result = json!({"saved_here":null,"saved_videos":null,"unlocated":null,"moved_elsewhere":null,"in_trash":null,"availability":{"saved_here":false,"saved_videos":false,"unlocated":false,"moved_elsewhere":false,"in_trash":false},"destination_selected":selected,"destination_available":false,"scan_complete":false});
        if let Some(base) = directory {
            let _ = (|| -> Result<()> {
                let tree = Tree::scan(&base, false, false, BTreeSet::new())?;
                let sources = Dir::open(jobs_root).ok();
                let source_identity = sources.as_ref().and_then(|dir| dir.identity().ok());
                let mut checked_sources = Vec::new();
                result["destination_available"] = json!(true);
                let mut expected: HashMap<(String, u64), HashMap<String, HashSet<String>>> =
                    HashMap::new();
                let mut previously = HashSet::new();
                for job in jobs {
                    if !["completed", "removed"].contains(&str_field(job, "status"))
                        || !valid_id(str_field(job, "id"))
                    {
                        continue;
                    }
                    let id = str_field(job, "id");
                    let signatures = (|| -> Result<_> {
                        let parent = sources.as_ref().ok_or_else(fs::io_error)?.child(id)?;
                        let pair = parent.identity()?.pair();
                        let (document_identity, signatures) = self.signature_record(
                            &parent,
                            "transcript.json",
                            &jobs_root.join(id).join("transcript.json"),
                        )?;
                        checked_sources.push((id, pair, document_identity));
                        Ok(signatures)
                    })();
                    let Ok(signatures) = signatures else {
                        continue;
                    };
                    let video = if str_field(job, "video_id").is_empty() {
                        str_field(job, "id")
                    } else {
                        str_field(job, "video_id")
                    }
                    .to_owned();
                    if job["status"] == "completed" && !Self::tracked(job, &base).is_empty() {
                        previously.insert(video.clone());
                    }
                    for (bucket, hash) in signatures {
                        expected
                            .entry(bucket)
                            .or_default()
                            .entry(hash)
                            .or_default()
                            .insert(video.clone());
                    }
                }
                let mut found = HashSet::new();
                let mut count = 0;
                for (path, info) in &tree.inventory {
                    if !info.is_file() {
                        continue;
                    }
                    let suffix = Path::new(path.last().unwrap())
                        .extension()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if let Some(hashes) = expected.get(&(suffix, info.size)) {
                        let hash = self.file_hash(&tree, path, info)?;
                        if let Some(videos) = hashes.get(&hash) {
                            count += 1;
                            found.extend(videos.iter().cloned());
                        }
                    }
                }
                tree.unchanged()?;
                Self::sources_unchanged(
                    jobs_root,
                    sources.as_ref(),
                    source_identity.as_ref(),
                    &checked_sources,
                )?;
                result["saved_here"] = json!(count);
                result["saved_videos"] = json!(found.len());
                result["unlocated"] = json!(previously.difference(&found).count());
                result["scan_complete"] = json!(true);
                for key in ["saved_here", "saved_videos", "unlocated"] {
                    result["availability"][key] = json!(true)
                }
                Ok(())
            })();
        }
        Ok(result)
    }
    pub fn scan_export_presence(
        &self,
        jobs: &[Value],
        jobs_root: &Path,
        _force: bool,
    ) -> Result<Value> {
        let prefs = self.export_settings();
        let directory = str_field(&prefs, "directory");
        Ok(self.read_presence(directory, jobs, jobs_root))
    }
    fn read_presence(&self, directory: &str, jobs: &[Value], jobs_root: &Path) -> Value {
        let mut result = json!({"complete":false,"present":{},"absent":[],"uncertain":[]});
        if directory.is_empty() {
            return result;
        }
        let base = Path::new(directory);
        let candidates: Vec<_> = jobs
            .iter()
            .filter(|j| j["status"] == "completed")
            .map(|j| (j, Self::tracked(j, base)))
            .filter(|(_, p)| !p.is_empty())
            .collect();
        if candidates.is_empty() {
            result["complete"] = json!(true);
            return result;
        }
        let calculation = (|| -> Result<Value> {
            let tree = Tree::scan(base, true, false, BTreeSet::new())?;
            let sources = Dir::open(jobs_root).ok();
            let source_identity = sources.as_ref().and_then(|dir| dir.identity().ok());
            let mut checked_sources = Vec::new();
            let root_identity = tree.root.identity()?.pair();
            let identities: HashSet<_> = tree.inventory.values().map(|i| (i.dev, i.ino)).collect();
            let mut retained = HashSet::new();
            let mut uncertain = HashSet::new();
            let mut expected: HashMap<(String, u64), HashMap<String, Vec<String>>> = HashMap::new();
            let mut present = serde_json::Map::new();
            for (job, paths) in &candidates {
                let id = str_field(job, "id").to_owned();
                for p in paths {
                    let parts = p
                        .strip_prefix(base)
                        .unwrap()
                        .components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>();
                    if tree.inventory.contains_key(&parts) {
                        retained.insert(id.clone());
                    }
                }
                let proof = &job["export_evidence"];
                if proof["root"] == directory {
                    if proof["root_identity"] != root_identity {
                        uncertain.insert(id.clone());
                    } else if proof["files"].as_array().is_some_and(|f| {
                        f.iter().any(|e| {
                            e["identity"].as_array().is_some_and(|p| {
                                p.len() == 2
                                    && identities.contains(&(
                                        p[0].as_u64().unwrap_or(0),
                                        p[1].as_u64().unwrap_or(0),
                                    ))
                            })
                        })
                    }) {
                        retained.insert(id.clone());
                    }
                }
                let signatures = (|| -> Result<_> {
                    if !valid_id(&id) {
                        return Err(fs::io_error());
                    }
                    let parent = sources.as_ref().ok_or_else(fs::io_error)?.child(&id)?;
                    let pair = parent.identity()?.pair();
                    let (document_identity, signatures) = self.signature_record(
                        &parent,
                        "transcript.json",
                        &jobs_root.join(&id).join("transcript.json"),
                    )?;
                    checked_sources.push((str_field(job, "id"), pair, document_identity));
                    Ok(signatures)
                })();
                match signatures {
                    Ok(signatures) => {
                        for (bucket, hash) in signatures {
                            expected
                                .entry(bucket)
                                .or_default()
                                .entry(hash)
                                .or_default()
                                .push(id.clone());
                        }
                    }
                    Err(_) => {
                        uncertain.insert(id);
                    }
                }
            }
            for (path, info) in &tree.inventory {
                if !info.is_file() {
                    continue;
                }
                let suffix = Path::new(path.last().unwrap())
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if let Some(hashes) = expected.get(&(suffix, info.size)) {
                    let hash = self.file_hash(&tree, path, info)?;
                    if let Some(ids) = hashes.get(&hash) {
                        for id in ids {
                            present
                                .entry(id.clone())
                                .or_insert(json!(base.join(path.iter().collect::<PathBuf>())));
                            retained.insert(id.clone());
                        }
                    }
                }
            }
            tree.unchanged()?;
            Self::sources_unchanged(
                jobs_root,
                sources.as_ref(),
                source_identity.as_ref(),
                &checked_sources,
            )?;
            let mut absent = Vec::new();
            let mut unsure = Vec::new();
            for (job, _) in &candidates {
                let id = str_field(job, "id").to_owned();
                if !retained.contains(&id) {
                    if uncertain.contains(&id) {
                        unsure.push(id)
                    } else {
                        absent.push(id)
                    }
                }
            }
            absent.sort();
            unsure.sort();
            Ok(json!({"complete":true,"present":present,"absent":absent,"uncertain":unsure}))
        })();
        match calculation {
            Ok(v) => v,
            Err(_) => {
                let mut unsure = Vec::new();
                for (job, paths) in candidates {
                    if !paths
                        .iter()
                        .any(|p| fs::read_path(p, 128 * 1024 * 1024).is_ok())
                    {
                        unsure.push(str_field(job, "id").to_owned())
                    }
                }
                unsure.sort();
                result["uncertain"] = json!(unsure);
                result
            }
        }
    }
    fn sources_unchanged(
        path: &Path,
        root: Option<&Dir>,
        before: Option<&Identity>,
        checked: &[(&str, Value, Identity)],
    ) -> Result<()> {
        if checked.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        if let Some(callback) = BEFORE_SOURCE_RECHECK.with(|hook| hook.borrow_mut().take()) {
            callback();
        }
        let root = root.ok_or_else(fs::io_error)?;
        let before = before.ok_or_else(fs::io_error)?;
        if root.identity()?.pair() != before.pair()
            || Dir::open(path)?.identity()?.pair() != before.pair()
        {
            return Err(fs::io_error());
        }
        for (id, pair, document) in checked {
            let parent = root.child(id)?;
            if parent.identity()?.pair() != *pair
                || parent.stat("transcript.json")? != *document
                || root.stat(id)?.pair() != *pair
            {
                return Err(fs::io_error());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    use std::{fs as disk, os::unix::fs::symlink};
    fn fixture(count: usize) -> (tempfile::TempDir, Library, PathBuf, Vec<Value>) {
        let temporary = tempfile::tempdir().unwrap();
        let base = disk::canonicalize(temporary.path()).unwrap();
        let private = base.join("private");
        let jobs_root = private.join("jobs");
        let exports = base.join("Exports");
        disk::create_dir_all(&jobs_root).unwrap();
        disk::create_dir(&exports).unwrap();
        let library = Library::open(&private).unwrap();
        let mut jobs = Vec::new();
        for index in 0..count {
            let id = format!("{index:032x}");
            let parent = exports.join(format!("Folder {index}"));
            disk::create_dir(&parent).unwrap();
            let document = json!({"title":format!("Video {index}"),"source_url":format!("https://www.youtube.com/watch?v=v{index:010}"),"full_text":"Original words.","complete":true});
            disk::create_dir(jobs_root.join(&id)).unwrap();
            disk::write(
                jobs_root.join(&id).join("transcript.json"),
                serde_json::to_vec(&document).unwrap(),
            )
            .unwrap();
            let path = parent.join("transcript.txt");
            disk::write(&path, render_document(&document, "txt").unwrap()).unwrap();
            jobs.push(json!({"id":id,"status":"completed","export_path":path}));
        }
        (temporary, library, jobs_root, jobs)
    }
    #[test]
    fn source_edits_during_inventory_never_become_a_false_zero_or_removal() {
        for presence in [false, true] {
            let (_temporary, library, jobs_root, jobs) = fixture(1);
            let document = jobs_root
                .join(jobs[0]["id"].as_str().unwrap())
                .join("transcript.json");
            if presence {
                disk::remove_file(jobs[0]["export_path"].as_str().unwrap()).unwrap();
            }
            library
                .set_directory(
                    Path::new(jobs[0]["export_path"].as_str().unwrap())
                        .parent()
                        .unwrap()
                        .parent()
                        .unwrap(),
                )
                .unwrap();
            BEFORE_SOURCE_RECHECK.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    let mut value: Value =
                        serde_json::from_slice(&disk::read(&document).unwrap()).unwrap();
                    value["full_text"] = json!("Changed source while inventory was read");
                    disk::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
                }))
            });
            if presence {
                let result = library
                    .scan_export_presence(&jobs, &jobs_root, false)
                    .unwrap();
                assert_eq!(result["complete"], false);
                assert_eq!(result["absent"], json!([]));
                assert_eq!(result["uncertain"], json!([jobs[0]["id"]]));
            } else {
                let result = library.file_stats(&jobs, &jobs_root, None).unwrap();
                assert_eq!(result["scan_complete"], false);
                assert!(result["saved_here"].is_null());
            }
        }
    }
    #[test]
    fn changes_between_batch_entries_cannot_reuse_an_earlier_positive_fact() {
        for change in [
            "edit",
            "delete",
            "export_parent",
            "source",
            "source_parent",
            "private_root",
        ] {
            let (temp, library, jobs_root, jobs) = fixture(2);
            assert!(
                library
                    .saved_job_facts(&jobs, &jobs_root)
                    .values()
                    .all(|saved| *saved)
            );
            let first = PathBuf::from(jobs[0]["export_path"].as_str().unwrap());
            let first_source = jobs_root.join(jobs[0]["id"].as_str().unwrap());
            let private_root = jobs_root.clone();
            let base = disk::canonicalize(temp.path()).unwrap();
            let trigger = jobs[1]["id"].as_str().unwrap().to_owned();
            AFTER_BATCH_ENTRY.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move |id| {
                    if id != trigger {
                        return;
                    }
                    match change {
                        "edit" => {
                            let modified = disk::metadata(&first).unwrap().modified().unwrap();
                            let mut bytes = disk::read(&first).unwrap();
                            bytes[0] ^= 1;
                            disk::write(&first, bytes).unwrap();
                            disk::File::options()
                                .write(true)
                                .open(&first)
                                .unwrap()
                                .set_times(disk::FileTimes::new().set_modified(modified))
                                .unwrap();
                        }
                        "delete" => {
                            disk::remove_file(&first).unwrap();
                        }
                        "export_parent" => {
                            let parent = first.parent().unwrap();
                            let detached = base.join("Old exports");
                            disk::rename(parent, &detached).unwrap();
                            symlink(&detached, parent).unwrap();
                        }
                        "source" => {
                            disk::write(first_source.join("transcript.json"), "{}").unwrap();
                        }
                        "source_parent" => {
                            let detached = base.join("Old source");
                            disk::rename(&first_source, &detached).unwrap();
                            symlink(&detached, &first_source).unwrap();
                        }
                        _ => {
                            disk::rename(&private_root, base.join("Old jobs")).unwrap();
                            disk::create_dir(&private_root).unwrap();
                        }
                    }
                }))
            });
            let facts = library.saved_job_facts(&jobs, &jobs_root);
            AFTER_BATCH_ENTRY.with(|hook| *hook.borrow_mut() = None);
            assert!(
                !facts[jobs[0]["id"].as_str().unwrap()],
                "Earlier fact must be invalidated for {change}"
            );
            assert_eq!(
                facts[jobs[1]["id"].as_str().unwrap()],
                change != "private_root"
            );
        }
    }
    #[test]
    fn descriptor_eviction_is_bounded_and_does_not_skip_final_validation() {
        let (_temporary, library, jobs_root, jobs) = fixture(40);
        let mut pool = ParentPool::default();
        for job in &jobs {
            pool.get(
                Path::new(job["export_path"].as_str().unwrap())
                    .parent()
                    .unwrap(),
            )
            .unwrap();
            assert!(pool.0.len() <= 32);
        }
        drop(pool);
        let first = PathBuf::from(jobs[0]["export_path"].as_str().unwrap());
        let trigger = jobs[39]["id"].as_str().unwrap().to_owned();
        AFTER_BATCH_ENTRY.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |id| {
                if id == trigger {
                    disk::write(&first, "Changed after its descriptor was evicted").unwrap();
                }
            }))
        });
        let facts = library.saved_job_facts(&jobs, &jobs_root);
        AFTER_BATCH_ENTRY.with(|hook| *hook.borrow_mut() = None);
        assert!(!facts[jobs[0]["id"].as_str().unwrap()]);
        assert_eq!(facts.values().filter(|saved| **saved).count(), 39);
    }
}
