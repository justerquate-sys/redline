#![allow(clippy::unwrap_used)]
#![cfg(unix)]
use scribe_common::{atomic_json, atomic_write, read_json, read_json_optional};
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

#[test]
fn creates_private_parents_and_replaces_regular_json_atomically() {
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("new/nested/job.json");
    atomic_json(&target, &json!({"phase":1,"unknown":{"kept":true}})).unwrap();
    assert_eq!(read_json(&target).unwrap()["phase"], 1);
    assert_eq!(
        fs::metadata(&target).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(target.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    atomic_json(&target, &json!({"phase":2})).unwrap();
    assert_eq!(
        read_json_optional(&target).unwrap(),
        Some(json!({"phase":2}))
    );
    assert_eq!(fs::read_dir(target.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn optional_read_distinguishes_absence_from_dangling_or_parent_symlinks() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    assert_eq!(
        read_json_optional(&base.join("absent/nested.json")).unwrap(),
        None
    );
    symlink(base.join("not-created"), base.join("dangling.json")).unwrap();
    assert!(read_json_optional(&base.join("dangling.json")).is_err());
    fs::create_dir(base.join("actual")).unwrap();
    symlink(base.join("actual"), base.join("alias")).unwrap();
    assert!(read_json_optional(&base.join("alias/missing.json")).is_err());
}

#[test]
fn rejects_symlink_parent_without_reading_or_writing_its_target() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let outside = base.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret.json"), br#"{"token":"do-not-expose"}"#).unwrap();
    let inside = base.join("inside");
    fs::create_dir(&inside).unwrap();
    symlink(&outside, inside.join("redirect")).unwrap();
    let target = inside.join("redirect/secret.json");
    for error in [
        read_json(&target).unwrap_err(),
        atomic_write(&target, b"replacement").unwrap_err(),
    ] {
        assert!(!error.message.contains("do-not-expose"));
        assert!(!error.message.contains(base.to_str().unwrap()));
    }
    assert_eq!(
        fs::read(outside.join("secret.json")).unwrap(),
        br#"{"token":"do-not-expose"}"#
    );
    assert!(atomic_json(&inside.join("redirect/new/tree.json"), &json!({})).is_err());
    assert!(!outside.join("new").exists());
}

#[test]
fn rejects_symlink_leaf_including_dangling_links_and_preserves_hardlink_targets() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let source = base.join("source.json");
    fs::write(&source, br#"{"old":true}"#).unwrap();
    for (name, destination) in [
        ("link.json", source.clone()),
        ("dangling.json", base.join("absent")),
    ] {
        let path = base.join(name);
        symlink(destination, &path).unwrap();
        assert!(read_json(&path).is_err());
        assert!(atomic_json(&path, &json!({"new":true})).is_err());
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }
    let link = base.join("hardlink.json");
    fs::hard_link(&source, &link).unwrap();
    atomic_json(&link, &json!({"new":true})).unwrap();
    assert_eq!(read_json(&source).unwrap(), json!({"old":true}));
    assert_eq!(read_json(&link).unwrap(), json!({"new":true}));
}

#[test]
fn oversized_and_nonregular_inputs_fail_without_blocking_or_disclosing_contents() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let huge = base.join("oversized.json");
    let file = fs::File::create(&huge).unwrap();
    file.set_len(64 * 1024 * 1024 + 1).unwrap();
    assert!(read_json(&huge).is_err());
    let fifo = base.join("pipe.json");
    let raw = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
    assert!(read_json(&fifo).is_err());
    assert!(atomic_json(&fifo, &json!({})).is_err());
    assert!(read_json(&base).is_err());
}

#[test]
fn rejects_parent_traversal_and_supports_unicode_spaces_and_long_components() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    fs::create_dir(base.join("inside")).unwrap();
    assert!(atomic_json(&base.join("inside/../escape.json"), &json!({})).is_err());
    assert!(!base.join("escape.json").exists());
    let path = base.join("Записи 🎬/space in name/metadata.json");
    atomic_json(&path, &json!({"text":"字幕 café 🎬"})).unwrap();
    assert_eq!(read_json(&path).unwrap()["text"], "字幕 café 🎬");
    let name = "n".repeat(240) + ".json";
    atomic_json(&base.join(&name), &json!({})).unwrap();
    assert_eq!(read_json(&base.join(name)).unwrap(), json!({}));
}

#[cfg(target_os = "macos")]
#[test]
fn supports_only_system_macos_aliases_not_similarly_named_user_links() {
    let temp = tempfile::tempdir().unwrap();
    let real = fs::canonicalize(temp.path()).unwrap();
    if let Ok(suffix) = real.strip_prefix("/private/var") {
        let alias = Path::new("/var").join(suffix).join("safe.json");
        atomic_json(&alias, &json!({"alias":"system"})).unwrap();
        assert_eq!(
            read_json(&real.join("safe.json")).unwrap(),
            json!({"alias":"system"})
        );
    }
    symlink(&real, real.join("var")).unwrap();
    assert!(atomic_json(&real.join("var/user-link.json"), &json!({})).is_err());
    assert!(!real.join("user-link.json").exists());
}

#[test]
fn transient_inputs_are_complete_private_and_never_overwrite_existing_files() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("source-info.json");
    scribe_common::create_transient_json(
        &path,
        &serde_json::json!({"id":"abcdefghijk","description":"Every UTF-8 byte 日本語"}),
    )
    .unwrap();
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&before).unwrap()["id"],
        "abcdefghijk"
    );
    assert!(
        scribe_common::create_transient_json(&path, &serde_json::json!({"overwrite":true}))
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = temporary.path().join("link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(scribe_common::create_transient_json(&link, &serde_json::json!({})).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let linked_parent = temporary.path().join("linked");
        std::os::unix::fs::symlink(temporary.path(), &linked_parent).unwrap();
        assert!(
            scribe_common::create_transient_json(
                &linked_parent.join("never.json"),
                &serde_json::json!({})
            )
            .is_err()
        );
        assert!(!temporary.path().join("never.json").exists());
    }
}
