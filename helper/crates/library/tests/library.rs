use scribe_common::{atomic_json, render_document};
use scribe_library::{Library, RemovalRegistry};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Cursor, Read},
    os::unix::fs::symlink,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
struct Fixture {
    _temp: TempDir,
    base: PathBuf,
    data: PathBuf,
    jobs: PathBuf,
    root: PathBuf,
    library: Library,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let data = base.join("private");
        let jobs = data.join("jobs");
        let root = base.join("Transcripts");
        fs::create_dir_all(&jobs).unwrap();
        fs::create_dir(&root).unwrap();
        let library = Library::open(&data).unwrap();
        library.set_directory(&root).unwrap();
        Self {
            _temp: temp,
            base,
            data,
            jobs,
            root,
            library,
        }
    }
    fn job(&self, index: u8) -> Value {
        let id = format!("{index:032x}");
        let video = format!("v{index:010}");
        let doc = json!({"title":format!("Video {index}"),"channel":"Creator","published_date":"20261025","source_url":format!("https://www.youtube.com/watch?v={video}"),"full_text":"Full words.\n\n  Preserved middle spacing.\n","complete":true});
        atomic_json(&self.jobs.join(&id).join("transcript.json"), &doc).unwrap();
        json!({"id":id,"video_id":video,"url":doc["source_url"],"title":doc["title"],"status":"completed","document_available":true,"folder_id":null,"metadata":{"channel":"Creator","published_date":"20261025"},"unknown_job_field":{"kept":true}})
    }
    fn save(&self, job: &mut Value) {
        let path = self.library.export_job(job, &self.jobs, None).unwrap();
        job["export_path"] = json!(path);
        job["export_paths"] = json!([path]);
        job["export_evidence"] = self.library.export_evidence(job, &self.jobs).unwrap();
    }
    fn archive(&self, record: &Value) -> HashMap<String, Vec<u8>> {
        let path = self
            .data
            .join("attachments")
            .join(record["id"].as_str().unwrap())
            .join(record["filename"].as_str().unwrap());
        let mut zip = zip::ZipArchive::new(Cursor::new(fs::read(path).unwrap())).unwrap();
        let mut result = HashMap::new();
        for i in 0..zip.len() {
            let mut f = zip.by_index(i).unwrap();
            let mut raw = Vec::new();
            f.read_to_end(&mut raw).unwrap();
            result.insert(f.name().unwrap().into_owned(), raw);
        }
        result
    }
}
#[test]
fn state_round_trip_preserves_unknown_fields_and_folder_attributes() {
    let f = Fixture::new();
    let folder = f.library.create_folder("Motion").unwrap();
    let path = f.data.join("library.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    state["future"] = json!({"x":[1,2]});
    state["folders"][0]["color"] = json!("mint");
    state["export_settings"]["future_setting"] = json!(true);
    atomic_json(&path, &state).unwrap();
    let library = Library::open(&f.data).unwrap();
    library.set_default(folder["id"].as_str()).unwrap();
    library.set_export_settings(false, "md").unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["future"], state["future"]);
    assert_eq!(saved["folders"][0]["color"], "mint");
    assert_eq!(saved["export_settings"]["future_setting"], true);
}
#[test]
fn legacy_state_defaults_and_normalized_names_preserve_unknown_fields() {
    let f = Fixture::new();
    atomic_json(&f.data.join("library.json"), &json!({"future":1})).unwrap();
    let loaded = Library::open(&f.data).unwrap();
    assert_eq!(loaded.snapshot()["folders"], json!([]));
    assert_eq!(
        loaded.export_settings(),
        json!({"directory":"","format":"txt","enabled":false})
    );
    let id = "5".repeat(32);
    atomic_json(
        &f.data.join("library.json"),
        &json!({"folders":[{"id":id,"name":"  cafe\u{301}  ","future":2}],"future":1}),
    )
    .unwrap();
    let loaded = Library::open(&f.data).unwrap();
    assert_eq!(loaded.snapshot()["folders"][0]["name"], "café");
    assert_eq!(loaded.snapshot()["folders"][0]["future"], 2);
    atomic_json(
        &f.data.join("library.json"),
        &json!({"export_settings":{"directory":4}}),
    )
    .unwrap();
    assert!(Library::open(&f.data).is_err());
    atomic_json(
        &f.data.join("library.json"),
        &json!({"export_settings":null}),
    )
    .unwrap();
    assert!(Library::open(&f.data).is_err());
}
#[test]
fn bundle_keeps_physical_evidence_from_every_completed_attempt_of_one_video() {
    let f = Fixture::new();
    let mut older = f.job(1);
    f.save(&mut older);
    let old = PathBuf::from(older["export_path"].as_str().unwrap());
    fs::write(&old, "Edited older attempt").unwrap();
    fs::rename(&old, f.root.join("Edited without extension")).unwrap();
    let mut newer = older.clone();
    newer["id"] = json!("9".repeat(32));
    newer["export_path"] = Value::Null;
    newer["export_paths"] = json!([]);
    newer["export_evidence"] = Value::Null;
    let result = f
        .library
        .folder_documents(&[older, newer], &f.jobs, None)
        .unwrap();
    assert_eq!(result["count"], 1);
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("Edited older attempt")
    );
}
#[test]
fn restored_removed_documents_count_physically_without_restoring_export_permission() {
    let f = Fixture::new();
    let mut job = f.job(1);
    f.save(&mut job);
    let text = fs::read(job["export_path"].as_str().unwrap()).unwrap();
    job["status"] = json!("removed");
    job["document_available"] = json!(false);
    fs::create_dir(f.root.join("Restored")).unwrap();
    fs::write(f.root.join("Restored/copy.txt"), &text).unwrap();
    let stats = f.library.file_stats(&[job.clone()], &f.jobs, None).unwrap();
    assert_eq!(stats["saved_here"], 2);
    assert_eq!(stats["saved_videos"], 1);
    assert_eq!(stats["unlocated"], 0);
    assert!(f.library.export_job(&job, &f.jobs, None).is_err());
    assert!(!f.library.is_saved_anywhere(&job, &f.jobs));
}
#[test]
fn counts_formats_copies_and_scoped_files_not_logical_membership() {
    let f = Fixture::new();
    let folder = f.library.create_folder("Motion").unwrap();
    let mut job = f.job(2);
    f.save(&mut job);
    let doc: Value = serde_json::from_slice(
        &fs::read(
            f.jobs
                .join(job["id"].as_str().unwrap())
                .join("transcript.json"),
        )
        .unwrap(),
    )
    .unwrap();
    for format in ["txt", "md", "json"] {
        fs::write(
            f.root.join("Motion").join(format!("copy.{format}")),
            render_document(&doc, format).unwrap(),
        )
        .unwrap();
    }
    fs::write(f.root.join("Motion/edited.txt"), "edited unrelated words").unwrap();
    let stats = f
        .library
        .file_stats(&[job.clone()], &f.jobs, folder["id"].as_str())
        .unwrap();
    assert_eq!(stats["saved_here"], 3);
    assert_eq!(stats["saved_videos"], 1);
    assert_eq!(stats["unlocated"], 0);
    assert_eq!(
        f.library.file_stats(&[job], &f.jobs, None).unwrap()["saved_here"],
        4
    );
}
#[test]
fn export_exact_duplicates_are_backed_up_and_removed_without_overwriting_edits() {
    let f = Fixture::new();
    let mut job = f.job(3);
    f.save(&mut job);
    let original = PathBuf::from(job["export_path"].as_str().unwrap());
    fs::copy(&original, f.root.join("Copy.txt")).unwrap();
    fs::write(f.root.join("Edited.txt"), "User annotations").unwrap();
    let outcome = f.library.export_job_detail(&job, &f.jobs, None).unwrap();
    assert_eq!(outcome.removed_duplicate_count, 1);
    assert!(!f.root.join("Copy.txt").exists());
    assert_eq!(
        fs::read_to_string(f.root.join("Edited.txt")).unwrap(),
        "User annotations"
    );
    assert_eq!(
        fs::read_dir(f.data.join("duplicate-backups"))
            .unwrap()
            .count(),
        2
    );
    assert!(f.library.is_saved_anywhere(&job, &f.jobs));
}
#[test]
fn export_hardlink_duplicates_keep_one_verified_copy_despite_ctime_changes() {
    let f = Fixture::new();
    let mut job = f.job(3);
    f.save(&mut job);
    let original = PathBuf::from(job["export_path"].as_str().unwrap());
    let expected = fs::read(&original).unwrap();
    fs::hard_link(&original, f.root.join("Hardlink.txt")).unwrap();
    fs::hard_link(&original, f.root.join("Another hardlink.txt")).unwrap();
    let outcome = f.library.export_job_detail(&job, &f.jobs, None).unwrap();
    assert_eq!(outcome.removed_duplicate_count, 2);
    assert_eq!(fs::read(&outcome.path).unwrap(), expected);
    assert!(!f.root.join("Hardlink.txt").exists());
    assert!(!f.root.join("Another hardlink.txt").exists());
    assert_eq!(
        fs::read_dir(f.data.join("duplicate-backups"))
            .unwrap()
            .count(),
        4
    );
}
#[test]
fn saving_into_new_folder_reconciles_old_exact_paths() {
    let f = Fixture::new();
    let mut job = f.job(4);
    f.save(&mut job);
    let old = job["export_path"].as_str().unwrap().to_owned();
    let folder = f.library.create_folder("Motion").unwrap();
    job["folder_id"] = folder["id"].clone();
    let result = f.library.export_job_detail(&job, &f.jobs, None).unwrap();
    assert!(result.path.contains("/Motion/"));
    assert_eq!(result.replacements.get(&old), Some(&result.path));
    assert!(!Path::new(&old).exists());
}
#[test]
fn presence_detects_deletion_but_never_recreates_it() {
    let f = Fixture::new();
    let mut job = f.job(5);
    f.save(&mut job);
    fs::remove_file(job["export_path"].as_str().unwrap()).unwrap();
    let presence = f
        .library
        .scan_export_presence(&[job.clone()], &f.jobs, true)
        .unwrap();
    assert_eq!(presence["absent"], json!([job["id"]]));
    assert!(f.library.export_job(&job, &f.jobs, None).is_err());
}
#[test]
fn presence_retains_edited_and_renamed_files() {
    let f = Fixture::new();
    let mut job = f.job(6);
    f.save(&mut job);
    let old = job["export_path"].as_str().unwrap();
    fs::write(old, "My edited transcript").unwrap();
    fs::rename(old, f.root.join("Renamed notes")).unwrap();
    let presence = f
        .library
        .scan_export_presence(&[job], &f.jobs, true)
        .unwrap();
    assert_eq!(presence["complete"], true);
    assert_eq!(presence["absent"], json!([]));
    assert_eq!(presence["uncertain"], json!([]));
}
#[test]
fn symlinks_make_absence_uncertain_but_do_not_poison_exact_file_counts() {
    let f = Fixture::new();
    let mut job = f.job(7);
    f.save(&mut job);
    let original = job["export_path"].as_str().unwrap();
    symlink(&f.base, f.root.join("unsafe")).unwrap();
    assert_eq!(
        f.library.file_stats(&[job.clone()], &f.jobs, None).unwrap()["saved_here"],
        1
    );
    fs::remove_file(original).unwrap();
    let result = f
        .library
        .scan_export_presence(&[job.clone()], &f.jobs, true)
        .unwrap();
    assert_eq!(result["complete"], false);
    assert_eq!(result["absent"], json!([]));
    assert_eq!(result["uncertain"], json!([job["id"]]));
    assert!(f.library.prepare_attachment(None, None, None).is_err());
}
#[test]
fn saved_verification_never_follows_replaced_file_or_ancestor_symlink() {
    let f = Fixture::new();
    let mut job = f.job(8);
    let folder = f.library.create_folder("Motion").unwrap();
    job["folder_id"] = folder["id"].clone();
    f.save(&mut job);
    let external = f.base.join("external");
    fs::rename(f.root.join("Motion"), &external).unwrap();
    symlink(&external, f.root.join("Motion")).unwrap();
    assert!(!f.library.is_saved_anywhere(&job, &f.jobs));
    assert!(
        f.library
            .prepare_attachment(folder["id"].as_str(), None, None)
            .is_err()
    );
}
#[test]
fn folder_renames_follow_inode_and_deleted_manual_entries_reset_default() {
    let f = Fixture::new();
    let folder = f.library.create_folder("Motion").unwrap();
    f.library.set_default(folder["id"].as_str()).unwrap();
    fs::rename(f.root.join("Motion"), f.root.join("New name")).unwrap();
    assert!(f.library.reconcile_folders(&HashSet::new(), &[]).unwrap());
    assert_eq!(f.library.snapshot()["folders"][0]["name"], "New name");
    fs::remove_dir(f.root.join("New name")).unwrap();
    f.library.reconcile_folders(&HashSet::new(), &[]).unwrap();
    assert_eq!(f.library.snapshot()["folders"], json!([]));
    assert!(f.library.snapshot()["default_folder_id"].is_null());
}
#[test]
fn unavailable_or_changed_roots_do_not_delete_folder_entries_and_channel_entries_remain() {
    let f = Fixture::new();
    let folder = f.library.create_folder("Channel").unwrap();
    fs::remove_dir(f.root.join("Channel")).unwrap();
    let channels = HashSet::from([folder["id"].as_str().unwrap().to_owned()]);
    f.library.reconcile_folders(&channels, &[]).unwrap();
    assert_eq!(f.library.snapshot()["folders"].as_array().unwrap().len(), 1);
    let next = f.base.join("Other");
    fs::create_dir(&next).unwrap();
    f.library.set_directory(&next).unwrap();
    f.library.reconcile_folders(&HashSet::new(), &[]).unwrap();
    assert_eq!(f.library.snapshot()["folders"].as_array().unwrap().len(), 1);
}
#[test]
fn removal_registry_is_durable_and_preserves_future_fields() {
    let f = Fixture::new();
    let job = f.job(9);
    let registry = RemovalRegistry::open(&f.data).unwrap();
    registry.add(std::slice::from_ref(&job)).unwrap();
    assert!(
        RemovalRegistry::open(&f.data)
            .unwrap()
            .contains(job["video_id"].as_str().unwrap())
    );
    let path = f.data.join("removed-videos.json");
    let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value["future"] = json!(true);
    atomic_json(&path, &value).unwrap();
    let registry = RemovalRegistry::open(&f.data).unwrap();
    registry.add(&[f.job(10)]).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["future"],
        true
    );
}
#[test]
fn malformed_removal_registry_fails_closed() {
    let f = Fixture::new();
    fs::write(f.data.join("removed-videos.json"), "{\"videos\":[]}").unwrap();
    assert!(RemovalRegistry::open(&f.data).is_err());
}
#[test]
fn literal_zip_preserves_bytes_empty_dirs_and_root_files_and_excludes_exact_subtrees() {
    let f = Fixture::new();
    fs::create_dir_all(f.root.join("Motion/empty")).unwrap();
    fs::create_dir(f.root.join("Exclude")).unwrap();
    fs::write(f.root.join("root.bin"), [0, 255, 13, 10]).unwrap();
    fs::write(f.root.join("Motion/readme"), "unchanged\r\n").unwrap();
    fs::write(f.root.join(".DS_Store"), "finder").unwrap();
    symlink(&f.base, f.root.join("Exclude/link")).unwrap();
    let listing = f.library.export_folders().unwrap();
    let token = listing["root_token"].as_str().unwrap();
    let record = f
        .library
        .prepare_attachment(None, Some(&["Exclude".into()]), Some(token))
        .unwrap();
    assert_eq!(record["count"], 2);
    let contents = f.archive(&record);
    assert_eq!(contents["Transcripts/root.bin"], vec![0, 255, 13, 10]);
    assert_eq!(contents["Transcripts/Motion/readme"], b"unchanged\r\n");
    assert!(contents.contains_key("Transcripts/Motion/empty/"));
    assert!(
        !contents
            .keys()
            .any(|x| x.contains("Exclude") || x.contains("DS_Store"))
    );
}
#[test]
fn all_excluded_empty_root_still_produces_valid_zip() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("Motion")).unwrap();
    let listing = f.library.export_folders().unwrap();
    let record = f
        .library
        .prepare_attachment(
            None,
            Some(&["Motion".into()]),
            listing["root_token"].as_str(),
        )
        .unwrap();
    assert_eq!(record["count"], 0);
    assert_eq!(
        f.archive(&record),
        HashMap::from([("Transcripts/".into(), vec![])])
    );
}
#[test]
fn exclusions_are_exact_and_root_bound_and_new_root_cannot_reuse_attachment() {
    let f = Fixture::new();
    fs::create_dir(f.root.join("Motion")).unwrap();
    let listing = f.library.export_folders().unwrap();
    let token = listing["root_token"].as_str().unwrap();
    assert!(
        f.library
            .prepare_attachment(None, Some(&["motion".into()]), Some(token))
            .is_err()
    );
    assert!(
        f.library
            .prepare_attachment(None, Some(&["../private".into()]), Some(token))
            .is_err()
    );
    let record = f
        .library
        .prepare_attachment(None, Some(&[]), Some(token))
        .unwrap();
    fs::rename(&f.root, f.base.join("Old")).unwrap();
    fs::create_dir(&f.root).unwrap();
    assert_ne!(f.library.export_folders().unwrap()["root_token"], token);
    assert!(
        f.library
            .prepare_attachment(None, Some(&[]), Some(token))
            .is_err()
    );
    assert!(
        f.library
            .begin_attachment_use(record["id"].as_str().unwrap())
            .is_err()
    );
}
#[test]
fn stale_exclusions_never_expand_scope() {
    for kind in ["missing", "file", "symlink"] {
        let f = Fixture::new();
        fs::create_dir(f.root.join("Motion")).unwrap();
        let listing = f.library.export_folders().unwrap();
        fs::remove_dir(f.root.join("Motion")).unwrap();
        match kind {
            "file" => fs::write(f.root.join("Motion"), "file").unwrap(),
            "symlink" => symlink(&f.base, f.root.join("Motion")).unwrap(),
            _ => {}
        }
        let error = f
            .library
            .prepare_attachment(
                None,
                Some(&["Motion".into()]),
                listing["root_token"].as_str(),
            )
            .unwrap_err();
        assert_eq!(error.status, 409);
        assert!(!f.data.join("attachments").exists());
    }
}
#[test]
fn attachment_reuse_and_persistent_unknown_copy_lease_require_verified_bytes() {
    let f = Fixture::new();
    fs::write(f.root.join("one.txt"), "One").unwrap();
    let record = f.library.prepare_attachment(None, None, None).unwrap();
    let id = record["id"].as_str().unwrap();
    assert_eq!(
        f.library.prepare_attachment(None, None, None).unwrap(),
        record
    );
    let (path, _) = f.library.begin_attachment_use(id).unwrap();
    f.library.protect_unknown_copy(id).unwrap();
    let restarted = Library::open(&f.data).unwrap();
    restarted.begin_attachment_use(id).unwrap();
    restarted.finish_attachment_use(id, false).unwrap();
    assert!(
        f.data
            .join("attachments")
            .join(id)
            .join("clipboard-unknown")
            .exists()
    );
    restarted.begin_attachment_use(id).unwrap();
    restarted.finish_attachment_use(id, true).unwrap();
    assert!(
        !f.data
            .join("attachments")
            .join(id)
            .join("clipboard-unknown")
            .exists()
    );
    let pin: Value =
        serde_json::from_slice(&fs::read(f.data.join("attachments/pin.json")).unwrap()).unwrap();
    assert_eq!(pin["id"], id);
    fs::write(path, "tamper").unwrap();
    assert!(restarted.begin_attachment_use(id).is_err());
}
#[test]
fn folder_bundle_reads_actual_edits_deduplicates_formats_and_does_not_use_private_body() {
    let f = Fixture::new();
    let mut job = f.job(11);
    f.save(&mut job);
    let doc: Value = serde_json::from_slice(
        &fs::read(
            f.jobs
                .join(job["id"].as_str().unwrap())
                .join("transcript.json"),
        )
        .unwrap(),
    )
    .unwrap();
    fs::write(
        f.root.join("Duplicate.md"),
        render_document(&doc, "md").unwrap(),
    )
    .unwrap();
    let bundle = f
        .library
        .folder_documents(&[job.clone()], &f.jobs, None)
        .unwrap();
    assert_eq!(bundle["count"], 1);
    assert_eq!(bundle["duplicate_count"], 1);
    fs::remove_file(f.root.join("Duplicate.md")).unwrap();
    fs::write(
        job["export_path"].as_str().unwrap(),
        "My complete edited note",
    )
    .unwrap();
    let bundle = f
        .library
        .folder_documents(&[job.clone()], &f.jobs, None)
        .unwrap();
    assert!(
        bundle["content"]
            .as_str()
            .unwrap()
            .contains("My complete edited note")
    );
    assert!(!bundle["content"].as_str().unwrap().contains("Full words"));
    job["status"] = json!("removed");
    assert_eq!(
        f.library.folder_documents(&[job], &f.jobs, None).unwrap()["count"],
        0
    );
}
#[test]
fn folder_bundle_preserves_creator_links_unknown_json_fields_and_partial_is_rejected() {
    let f = Fixture::new();
    let mut job = f.job(12);
    f.save(&mut job);
    let content = format!(
        "# Edited title\n\nPublished: Unknown\nSource: {}\n\nDescription links (creator-provided):\nSponsor https://example.com/?affiliate=keep\n\nTranscript:\n\nWhole transcript\n",
        job["url"].as_str().unwrap()
    );
    fs::write(job["export_path"].as_str().unwrap(), &content).unwrap();
    let bundle = f
        .library
        .folder_documents(&[job.clone()], &f.jobs, None)
        .unwrap();
    assert_eq!(bundle["content"], content);
    fs::write(
        job["export_path"].as_str().unwrap(),
        content.replace(
            "Description links (creator-provided):",
            "PARTIAL — transcription did not finish.\n\nDescription links (creator-provided):",
        ),
    )
    .unwrap();
    assert!(f.library.folder_documents(&[job], &f.jobs, None).is_err());
}
#[test]
fn invalid_folder_names_and_private_cache_recursive_archive_are_rejected() {
    let f = Fixture::new();
    for name in [
        "../escape",
        "a/b",
        "a\\b",
        "NUL",
        "con.txt",
        "trailing.",
        "",
        "   ",
    ] {
        assert!(f.library.create_folder(name).is_err(), "{name}");
    }
    f.library.set_directory(&f.data).unwrap();
    assert!(f.library.prepare_attachment(None, None, None).is_err());
}
#[test]
fn unicode_casefold_folder_collisions_match_python() {
    let f = Fixture::new();
    f.library.create_folder("Straße").unwrap();
    assert!(f.library.create_folder("STRASSE").is_err());
    f.library.create_folder("ﬃ").unwrap();
    assert!(f.library.create_folder("FFI").is_err());
}
#[test]
fn count_hash_cache_detects_same_length_edits_and_missing_source_is_not_a_fake_zero_for_other_jobs()
{
    let f = Fixture::new();
    let mut job = f.job(13);
    f.save(&mut job);
    assert_eq!(
        f.library.file_stats(&[job.clone()], &f.jobs, None).unwrap()["saved_here"],
        1
    );
    let path = job["export_path"].as_str().unwrap();
    let bytes = fs::read(path).unwrap();
    let edited = bytes
        .iter()
        .map(|b| if *b == b'F' { b'X' } else { *b })
        .collect::<Vec<_>>();
    fs::write(path, edited).unwrap();
    assert_eq!(
        f.library.file_stats(&[job.clone()], &f.jobs, None).unwrap()["saved_here"],
        0
    );
    let mut second = f.job(14);
    f.save(&mut second);
    fs::remove_file(
        f.jobs
            .join(job["id"].as_str().unwrap())
            .join("transcript.json"),
    )
    .unwrap();
    assert_eq!(
        f.library.file_stats(&[job, second], &f.jobs, None).unwrap()["saved_here"],
        1
    );
}
#[test]
fn presence_and_counts_are_fresh_without_job_changes_or_waiting_for_a_ttl() {
    let f = Fixture::new();
    let mut job = f.job(21);
    f.save(&mut job);
    let path = PathBuf::from(job["export_path"].as_str().unwrap());
    let bytes = fs::read(&path).unwrap();
    let jobs = vec![job.clone()];
    assert_eq!(
        f.library.file_stats(&jobs, &f.jobs, None).unwrap()["saved_here"],
        1
    );
    assert!(
        f.library
            .scan_export_presence(&jobs, &f.jobs, false)
            .unwrap()["absent"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    fs::remove_file(&path).unwrap();
    assert_eq!(
        f.library.file_stats(&jobs, &f.jobs, None).unwrap()["saved_here"],
        0
    );
    assert_eq!(
        f.library
            .scan_export_presence(&jobs, &f.jobs, false)
            .unwrap()["absent"],
        json!([job["id"]])
    );
    fs::write(&path, &bytes).unwrap();
    assert_eq!(
        f.library.file_stats(&jobs, &f.jobs, None).unwrap()["saved_here"],
        1
    );
    assert_eq!(
        f.library
            .scan_export_presence(&jobs, &f.jobs, false)
            .unwrap()["absent"],
        json!([])
    );
    assert!(f.library.is_saved_anywhere(&job, &f.jobs));
}
#[test]
fn saved_hash_cache_revalidates_inode_ctime_and_internal_document() {
    use std::fs::{File, FileTimes};
    let f = Fixture::new();
    let mut job = f.job(22);
    f.save(&mut job);
    let path = PathBuf::from(job["export_path"].as_str().unwrap());
    let bytes = fs::read(&path).unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    assert!(f.library.is_saved_anywhere(&job, &f.jobs));
    let mut edited = bytes.clone();
    edited[0] ^= 1;
    fs::write(&path, &edited).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(modified))
        .unwrap();
    assert!(
        !f.library.is_saved_anywhere(&job, &f.jobs),
        "Same length and restored mtime do not hide changed ctime"
    );
    fs::write(&path, &bytes).unwrap();
    assert!(f.library.is_saved_anywhere(&job, &f.jobs));
    let document = f
        .jobs
        .join(job["id"].as_str().unwrap())
        .join("transcript.json");
    let original = fs::read(&document).unwrap();
    let mut value: Value = serde_json::from_slice(&original).unwrap();
    value["full_text"] = json!("Changed source text");
    fs::write(&document, serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!f.library.is_saved_anywhere(&job, &f.jobs));
    fs::write(&document, &original).unwrap();
    assert!(f.library.is_saved_anywhere(&job, &f.jobs));
    fs::remove_file(&document).unwrap();
    symlink(f.root.join("outside.json"), &document).unwrap();
    assert!(!f.library.is_saved_anywhere(&job, &f.jobs));
}
#[test]
fn inventory_updates_immediately_and_keeps_root_tokens_identity_bound() {
    let f = Fixture::new();
    let first = f.library.export_folders().unwrap();
    fs::create_dir(f.root.join("Physical only")).unwrap();
    let next = f.library.export_folders().unwrap();
    assert_eq!(next["root_token"], first["root_token"]);
    assert_eq!(next["folders"], json!([{"name":"Physical only"}]));
    fs::rename(f.root.join("Physical only"), f.root.join("Renamed")).unwrap();
    assert_eq!(
        f.library.export_folders().unwrap()["folders"],
        json!([{"name":"Renamed"}])
    );
    fs::remove_dir(f.root.join("Renamed")).unwrap();
    let outside = f.base.join("Outside");
    fs::create_dir(&outside).unwrap();
    symlink(&outside, f.root.join("Linked")).unwrap();
    assert_eq!(f.library.export_folders().unwrap()["folders"], json!([]));
    fs::remove_file(f.root.join("Linked")).unwrap();
    fs::rename(&f.root, f.base.join("Detached")).unwrap();
    fs::create_dir(&f.root).unwrap();
    assert_ne!(
        f.library.export_folders().unwrap()["root_token"],
        first["root_token"]
    );
}
#[test]
fn batched_saved_facts_match_single_checks_and_refresh_each_request() {
    let f = Fixture::new();
    let mut first = f.job(30);
    f.save(&mut first);
    let mut second = f.job(31);
    f.save(&mut second);
    let mut removed = first.clone();
    removed["id"] = json!("e".repeat(32));
    removed["status"] = json!("removed");
    let mut invalid = first.clone();
    invalid["id"] = json!("../invalid");
    let mut jobs = vec![first.clone(), second.clone(), removed, invalid];
    let check = |jobs: &[Value]| {
        let facts = f.library.saved_job_facts(jobs, &f.jobs);
        assert_eq!(facts.len(), 3);
        for job in &jobs[..3] {
            assert_eq!(
                facts[job["id"].as_str().unwrap()],
                f.library.is_saved_anywhere(job, &f.jobs)
            );
        }
    };
    check(&jobs);
    fs::write(first["export_path"].as_str().unwrap(), "Edited export").unwrap();
    check(&jobs);
    fs::remove_file(second["export_path"].as_str().unwrap()).unwrap();
    check(&jobs);
    jobs[0]["status"] = json!("cancelled");
    check(&jobs);
}
#[test]
fn unknown_and_symlink_cache_entries_are_never_deleted() {
    let f = Fixture::new();
    fs::write(f.root.join("file.txt"), "body").unwrap();
    let record = f.library.prepare_attachment(None, None, None).unwrap();
    let cache = f.data.join("attachments");
    let target = f.base.join("must-keep");
    fs::write(&target, "untouched").unwrap();
    symlink(&target, cache.join("unexpected")).unwrap();
    fs::write(f.root.join("file.txt"), "new body").unwrap();
    assert!(f.library.prepare_attachment(None, None, None).is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "untouched");
    assert!(cache.join(record["id"].as_str().unwrap()).exists());
    assert!(
        cache
            .join("unexpected")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
#[test]
fn attachment_pruning_retains_clipboard_pin_and_unknown_leases() {
    let f = Fixture::new();
    let mut records = Vec::new();
    for i in 0..32 {
        fs::write(f.root.join("file.txt"), format!("version {i}")).unwrap();
        records.push(f.library.prepare_attachment(None, None, None).unwrap());
    }
    let pinned = records[0]["id"].as_str().unwrap();
    f.library.begin_attachment_use(pinned).unwrap();
    f.library.finish_attachment_use(pinned, true).unwrap();
    let leased = records[1]["id"].as_str().unwrap();
    f.library.begin_attachment_use(leased).unwrap();
    f.library.protect_unknown_copy(leased).unwrap();
    for record in &records {
        let path = f
            .data
            .join("attachments")
            .join(record["id"].as_str().unwrap())
            .join("metadata.json");
        let mut data: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        data["used_at"] = json!(0);
        atomic_json(&path, &data).unwrap();
    }
    fs::write(f.root.join("file.txt"), "newest version").unwrap();
    let newest = f.library.prepare_attachment(None, None, None).unwrap();
    let cache = f.data.join("attachments");
    assert!(cache.join(pinned).exists());
    assert!(cache.join(leased).exists());
    assert!(cache.join(newest["id"].as_str().unwrap()).exists());
    assert_eq!(fs::read_dir(cache).unwrap().count(), 33);
}
#[test]
fn oversized_zip_inputs_fail_before_attachment_creation_and_symlink_files_are_not_read() {
    let f = Fixture::new();
    let file = fs::File::create(f.root.join("oversized.bin")).unwrap();
    file.set_len(32 * 1024 * 1024 + 1).unwrap();
    assert!(f.library.prepare_attachment(None, None, None).is_err());
    assert!(!f.data.join("attachments").exists());
    fs::remove_file(f.root.join("oversized.bin")).unwrap();
    symlink(f.base.join("does-not-exist"), f.root.join("dangling.txt")).unwrap();
    assert!(f.library.prepare_attachment(None, None, None).is_err());
}
#[test]
fn named_zip_scope_ignores_membership_and_rejects_all_exclusion_options() {
    let f = Fixture::new();
    let folder = f.library.create_folder("Motion").unwrap();
    fs::write(f.root.join("Motion/not-a-transcript.asset"), [1, 2, 3]).unwrap();
    fs::write(f.root.join("not-in-scope"), "excluded").unwrap();
    let record = f
        .library
        .prepare_attachment(folder["id"].as_str(), None, None)
        .unwrap();
    assert_eq!(
        f.archive(&record),
        HashMap::from([
            ("Motion/".into(), vec![]),
            ("Motion/not-a-transcript.asset".into(), vec![1, 2, 3])
        ])
    );
    assert!(record.get("excluded_folders").is_none());
    assert!(
        f.library
            .prepare_attachment(folder["id"].as_str(), Some(&[]), None)
            .is_err()
    );
}
#[test]
fn unknown_json_annotations_are_kept_by_folder_text_copy() {
    let f = Fixture::new();
    let job = f.job(15);
    let item = json!({"title":"Custom","source_url":job["url"],"channel":"Creator","published_date":"2026-10-25","full_text":"words","annotation":{"my_note":"keep"}});
    let source = serde_json::to_string_pretty(&item).unwrap();
    fs::write(f.root.join("annotated.json"), &source).unwrap();
    let bundle = f.library.folder_documents(&[job], &f.jobs, None).unwrap();
    assert_eq!(bundle["count"], 1);
    assert_eq!(bundle["content"], format!("{source}\n"));
}
