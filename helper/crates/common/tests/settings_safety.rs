#![allow(clippy::unwrap_used)]
use scribe_common::{Settings, persist_azure_settings};
#[test]
fn malformed_settings_are_rejected_without_partial_loading_or_rewriting() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join(".env");
    let invalid = "HELPER_TOKEN=private-test-token\nnot a valid setting\n";
    std::fs::write(&path, invalid).expect("settings fixture");
    assert!(Settings::load(directory.path(), None).is_err());
    std::fs::remove_file(&path).expect("remove fixture");
    let mut settings = Settings::load(directory.path(), None).expect("empty config");
    settings.speech_endpoint = "https://test.cognitiveservices.azure.com".into();
    settings.speech_key = "private-test-key".into();
    std::fs::write(&path, invalid).expect("settings fixture");
    assert!(persist_azure_settings(&settings, false).is_err());
    assert_eq!(
        std::fs::read_to_string(path).expect("preserved fixture"),
        invalid
    );
}
#[cfg(unix)]
#[test]
fn symlinked_settings_are_never_read_or_replaced() {
    let directory = tempfile::tempdir().expect("fixture");
    let target = directory.path().join("private-settings");
    let path = directory.path().join(".env");
    std::fs::write(&target, "HELPER_TOKEN=private-test-token\n").expect("fixture");
    let mut settings = Settings::load(directory.path(), None).expect("empty config");
    settings.speech_endpoint = "https://test.cognitiveservices.azure.com".into();
    settings.speech_key = "private-test-key".into();
    std::os::unix::fs::symlink(&target, &path).expect("symlink fixture");
    assert!(Settings::load(directory.path(), None).is_err());
    assert!(persist_azure_settings(&settings, false).is_err());
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("fixture")
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(target).expect("fixture"),
        "HELPER_TOKEN=private-test-token\n"
    );
}
