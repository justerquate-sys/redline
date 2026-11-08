use crate::{
    Library,
    fs::{self, Dir, Tree, finder_metadata, safe_part},
    str_field,
};
use scribe_common::{Error, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Write},
    path::Path,
};
use zip::{CompressionMethod, DateTime, ZipWriter, write::SimpleFileOptions};
pub(crate) struct Archive {
    pub bytes: Vec<u8>,
    pub count: usize,
}
impl Library {
    pub(crate) fn attachment_context(
        &self,
        folder_id: Option<&str>,
        excluded: Option<&[String]>,
    ) -> Result<Value> {
        self.validate_folder(folder_id)?;
        if folder_id.is_some() && excluded.is_some() {
            return Err(fs::safe("Folder exclusions apply only to the All ZIP."));
        }
        let base = self.scope(None)?;
        let selected = self.scope(folder_id)?;
        let cache = self.data_dir.join("attachments");
        if selected.starts_with(&cache) || cache.starts_with(&selected) {
            return Err(fs::safe(
                "Choose a save folder outside the helper's private attachment cache.",
            ));
        }
        let root = Dir::open(&base)?;
        let info = root.identity()?;
        let chosen = if selected == base {
            info.clone()
        } else {
            Dir::open(&selected)?.identity()?
        };
        let name = folder_id.map(|_| selected.file_name().unwrap().to_string_lossy().into_owned());
        let mut context = json!({"root":base,"root_identity":info.pair(),"folder_id":folder_id,"folder_name":name,"folder_identity":chosen.pair()});
        if let Some(excluded) = excluded {
            let names = fs::exclusions(excluded)?;
            context["excluded_folders"] = json!(names);
            context["root_token"] = json!(root_token(&context)?)
        }
        Ok(context)
    }
    pub fn export_folders(&self) -> Result<Value> {
        let _guard = self.attachment_lock.lock();
        let base = self.scope(None)?;
        let cache = self.data_dir.join("attachments");
        if base.starts_with(&cache) || cache.starts_with(&base) {
            return Err(fs::safe(
                "Choose a save folder outside the helper's private attachment cache.",
            ));
        }
        let root = Dir::open(&base)?;
        let before = root.identity()?;
        let context = json!({"root":base,"root_identity":before.pair()});
        let mut names = Vec::new();
        for (name, info) in root.entries()? {
            if info.is_dir() && !finder_metadata(&name) {
                fs::exclusions(std::slice::from_ref(&name))?;
                names.push(name);
                if names.len() > 1000 {
                    return Err(fs::safe(
                        "This save folder contains more than 1,000 folders. Choose a smaller save folder.",
                    ));
                }
            }
        }
        if root.identity()? != before
            || self.scope(None)? != base
            || Dir::open(&base)?.identity()? != before
        {
            return Err(Error::new(
                409,
                "The save folders changed. Refresh and try again.",
            ));
        }
        names.sort_by_cached_key(|name| (crate::casefold(name), name.clone()));
        Ok(
            json!({"directory":context["root"],"root_token":root_token(&context)?,"folders":names.into_iter().map(|n|json!({"name":n})).collect::<Vec<_>>()}),
        )
    }
    pub(crate) fn archive(&self, context: &Value, excluded: &[String]) -> Result<Archive> {
        // Fast Deflate keeps the standard ZIP format and limits. Level 2 gives
        // most of level 1's text speedup without its incompressible expansion.
        self.archive_with_level(context, excluded, 2)
    }
    fn archive_with_level(
        &self,
        context: &Value,
        excluded: &[String],
        level: i64,
    ) -> Result<Archive> {
        let excluded = fs::exclusions(excluded)?;
        let folder_id = context["folder_id"].as_str();
        if folder_id.is_some() && !excluded.is_empty() {
            return Err(fs::safe("Folder exclusions apply only to All."));
        }
        // Bind the snapshot to the context accepted at request start. Reading
        // settings again permits an A→B→A selection change to mislabel B's ZIP.
        let configured = Path::new(str_field(context, "root"));
        if Dir::open(configured)?.identity()?.pair() != context["root_identity"] {
            return Err(fs::io_error());
        }
        let base = match context["folder_name"].as_str() {
            Some(name) => configured.join(name),
            None => configured.to_owned(),
        };
        let top = base
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(fs::io_error)?;
        if !safe_part(top) {
            return Err(fs::io_error());
        }
        let tree = Tree::scan(&base, true, true, excluded)?;
        if tree.root.identity()?.pair() != context["folder_identity"] {
            return Err(fs::io_error());
        }
        let total: u64 = tree
            .inventory
            .values()
            .filter(|i| i.is_file())
            .map(|i| i.size)
            .sum();
        if total > 128 * 1024 * 1024 {
            return Err(fs::safe(
                "This folder exceeds the 128 MiB ZIP limit. Choose a smaller folder; nothing was copied.",
            ));
        }
        let mut archive = ZipWriter::new(Cursor::new(Vec::new()));
        let mut count = 0;
        for (parts, info) in &tree.inventory {
            tree.check_limit()?;
            if parts.iter().any(|p| !safe_part(p)) {
                return Err(fs::safe(
                    "A filename cannot be represented safely in a ZIP. Rename that entry and try again.",
                ));
            }
            let name = std::iter::once(top)
                .chain(parts.iter().map(String::as_str))
                .collect::<Vec<_>>()
                .join("/");
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(level))
                .last_modified_time(DateTime::default())
                .unix_permissions(if info.is_dir() { 0o700 } else { 0o600 });
            if info.is_dir() {
                archive
                    .add_directory(format!("{name}/"), options)
                    .map_err(|_| fs::io_error())?
            } else {
                if info.size > 32 * 1024 * 1024 {
                    return Err(fs::safe(
                        "A file exceeds the 32 MiB ZIP limit. Choose a smaller folder; nothing was copied.",
                    ));
                }
                let data = tree.read(parts, 32 * 1024 * 1024)?;
                archive
                    .start_file(name, options)
                    .map_err(|_| fs::io_error())?;
                archive.write_all(&data).map_err(|_| fs::io_error())?;
                count += 1;
            }
        }
        tree.unchanged()?;
        if Dir::open(configured)?.identity()?.pair() != context["root_identity"] {
            return Err(fs::io_error());
        }
        let bytes = archive.finish().map_err(|_| fs::io_error())?.into_inner();
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(fs::safe(
                "The ZIP exceeds the private attachment cache size limit.",
            ));
        }
        Ok(Archive { bytes, count })
    }
}

pub(crate) fn root_token(context: &Value) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(&json!([
        context["root"],
        context["root_identity"]
    ]))?)))
}

#[cfg(test)]
mod compression_tests {
    use super::*;
    use std::{collections::BTreeMap, fs as disk, io::Read, time::Instant};
    type Unpacked = BTreeMap<String, (Vec<u8>, u32, [u16; 6], CompressionMethod)>;
    fn unpack(bytes: &[u8]) -> Unpacked {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).unwrap();
        let mut files = BTreeMap::new();
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).unwrap();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            let date = entry.last_modified().unwrap();
            files.insert(
                entry.name().unwrap().into_owned(),
                (
                    bytes,
                    entry.unix_mode().unwrap(),
                    [
                        date.year(),
                        date.month() as u16,
                        date.day() as u16,
                        date.hour() as u16,
                        date.minute() as u16,
                        date.second() as u16,
                    ],
                    entry.compression(),
                ),
            );
        }
        files
    }
    fn next(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }
    fn content(index: usize, binary: bool) -> Vec<u8> {
        let mut seed = 0x1234abcd7890_u64.wrapping_add(index as u64 * 97);
        if binary {
            return (0..65536).map(|_| next(&mut seed) as u8).collect();
        }
        let words="the a this that we you they it when where because although before after while today later now first second people design system language picture example problem solution reading thought experience motion analysis detail change choice place time sound video transcript reference source sequence scale scene color frame animation rendering software hardware knowledge memory precise actual dynamic realistic interesting useful clear small large different same new old natural independent public private complete available original repeated every some most several all more less only another together between within about through under above around slowly quickly evenly carefully correctly".split_whitespace().collect::<Vec<_>>();
        let mut text = format!(
            "Video {index}\n\nChannel: Offline fixture\nPublished: 2026-10-01\nSource: https://www.youtube.com/watch?v=v{index:010}\n\n"
        );
        for paragraph in 0..300 {
            for word in 0..24 {
                if word > 0 {
                    text.push(' ');
                }
                text.push_str(words[next(&mut seed) as usize % words.len()]);
            }
            text.push_str(if paragraph % 4 == 0 { ".\n\n" } else { ". " });
        }
        text.into_bytes()
    }
    fn fixture(kind: &str) -> (tempfile::TempDir, Library, Value, u64) {
        let temporary = tempfile::tempdir().unwrap();
        let base = disk::canonicalize(temporary.path()).unwrap();
        let root = base.join("Transcripts");
        disk::create_dir(&root).unwrap();
        disk::create_dir_all(root.join("Topic/Empty")).unwrap();
        disk::create_dir(root.join("Тема")).unwrap();
        let mut total = 0;
        for index in 0..48 {
            let binary = kind == "incompressible" || (kind == "mixed" && index % 2 == 1);
            let bytes = content(index, binary);
            total += bytes.len() as u64;
            let folder = if index % 2 == 0 { "Topic" } else { "Тема" };
            disk::write(
                root.join(format!(
                    "{folder}/File {index:02}.{}",
                    if binary { "bin" } else { "txt" }
                )),
                bytes,
            )
            .unwrap();
        }
        let library = Library::open(base.join("private")).unwrap();
        library.set_directory(&root).unwrap();
        let context = library.attachment_context(None, None).unwrap();
        (temporary, library, context, total)
    }
    #[test]
    fn fast_archive_preserves_exact_files_metadata_empty_directories_and_selection() {
        let (_temporary, library, context, _) = fixture("mixed");
        let full = library.archive(&context, &[]).unwrap();
        let normal = library.archive_with_level(&context, &[], 6).unwrap();
        assert_eq!(full.count, 48);
        assert_eq!(unpack(&full.bytes), unpack(&normal.bytes));
        let extracted = unpack(&full.bytes);
        assert!(extracted.contains_key("Transcripts/Topic/Empty/"));
        for index in 0..48 {
            let binary = index % 2 == 1;
            let folder = if binary { "Тема" } else { "Topic" };
            let name = format!(
                "Transcripts/{folder}/File {index:02}.{}",
                if binary { "bin" } else { "txt" }
            );
            let (bytes, mode, date, method) = &extracted[&name];
            assert_eq!(bytes, &content(index, binary));
            assert_eq!(*mode & 0o777, 0o600);
            assert_eq!(*date, [1980, 1, 1, 0, 0, 0]);
            assert_eq!(*method, CompressionMethod::Deflated);
        }
        for (name, (_, mode, date, _)) in &extracted {
            if name.ends_with('/') {
                assert_eq!(*mode & 0o777, 0o700);
                assert_eq!(*date, [1980, 1, 1, 0, 0, 0]);
            }
        }
        let excluded = library.archive(&context, &["Тема".into()]).unwrap();
        assert_eq!(excluded.count, 24);
        assert_eq!(
            unpack(&excluded.bytes),
            extracted
                .into_iter()
                .filter(|(name, _)| !name.starts_with("Transcripts/Тема/"))
                .collect()
        );
        assert_eq!(
            library.archive(&context, &[]).unwrap().bytes,
            full.bytes,
            "Unchanged inputs remain deterministic for attachment reuse"
        );
    }
    #[test]
    #[ignore = "isolated paired ZIP benchmark; run --release --ignored --nocapture"]
    fn compare_deflate_levels_on_generated_folders() {
        let mut results = Vec::new();
        for kind in ["transcript_text", "mixed", "incompressible"] {
            let (_temporary, library, context, total) = fixture(kind);
            let levels = [6, 3, 2, 1];
            let mut sizes = BTreeMap::new();
            let mut timings: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
            let mut expected = None;
            for level in levels {
                let output = library.archive_with_level(&context, &[], level).unwrap();
                let decoded = unpack(&output.bytes);
                if let Some(old) = &expected {
                    assert_eq!(old, &decoded);
                } else {
                    expected = Some(decoded);
                }
                sizes.insert(level, output.bytes.len());
                timings.insert(level, Vec::new());
            }
            for round in 0..10 {
                let mut order = levels;
                if round % 2 == 1 {
                    order.reverse();
                }
                for level in order {
                    let start = Instant::now();
                    let found = library.archive_with_level(&context, &[], level).unwrap();
                    let elapsed = start.elapsed().as_secs_f64() * 1000.;
                    assert_eq!(found.count, 48);
                    timings.get_mut(&level).unwrap().push(elapsed);
                }
            }
            let mut measured = Vec::new();
            for level in levels {
                let values = &timings[&level];
                let mut ordered = values.clone();
                ordered.sort_by(f64::total_cmp);
                measured.push(json!({"level":level,"bytes":sizes[&level],"median_ms":(ordered[4]+ordered[5])/2.,"samples_ms_by_round":values}));
            }
            results.push(json!({"kind":kind,"input_bytes":total,"files":48,"samples_each":10,"levels":measured}));
        }
        println!(
            "ZIP_BENCH {}",
            json!({"scope":"Synthetic isolated directories; full production scan/read/revalidation/compression path; no clipboard or live files","results":results})
        );
    }
}
