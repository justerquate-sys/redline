#![allow(clippy::unwrap_used)]
use scribe_common::*;
use serde_json::{Value, json};
#[test]
fn legacy_python_goldens_match_exactly() {
    let fixtures: Value =
        serde_json::from_str(include_str!("fixtures/python-compatibility.json")).unwrap();
    for case in fixtures["links"].as_array().unwrap() {
        assert_eq!(
            json!(extract_description_links(&case["input"])),
            case["expected"],
            "link case: {}",
            case["input"]
        );
    }
    for case in fixtures["dates"].as_array().unwrap() {
        assert_eq!(
            publication_date(&case["input"]),
            case["expected"].as_str().unwrap(),
            "date case: {}",
            case["input"]
        );
    }
    for case in fixtures["documents"].as_array().unwrap() {
        for format in ["txt", "md"] {
            assert_eq!(
                render_document(&case["document"], format).unwrap(),
                case[format].as_str().unwrap(),
                "{format} document {}",
                case["document"]
            );
        }
        assert_eq!(
            serde_json::from_str::<Value>(&render_document(&case["document"], "json").unwrap())
                .unwrap(),
            case["json"]
        );
    }
}
#[test]
fn description_metadata_rejects_multiline_items_and_preserves_repeats() {
    let line = "  Go: https://example.com?a=1&ref=two  ";
    assert_eq!(
        normalize_description_links(&json!([
            line,
            null,
            3,
            {},
            "prose",
            "",
            line,
            format!("{line}\n"),
            format!("Intro\n{line}")
        ])),
        vec![line, line]
    );
}
#[test]
fn credentials_are_redacted_and_targets_are_validated() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Settings::load(dir.path(), None).unwrap();
    assert!(!settings.configured());
    assert!(!format!("{settings:?}").contains("speech_key"));
    for target in [
        "https://resource.cognitiveservices.azure.com.evil.test",
        "http://resource.cognitiveservices.azure.com",
        "https://key@resource.cognitiveservices.azure.com",
        "https://resource.cognitiveservices.azure.com?token=secret",
        "https://resource.cognitiveservices.azure.com/anything",
    ] {
        assert!(normalize_speech_endpoint(target).is_err());
    }
    assert_eq!(
        normalize_speech_endpoint("https://test.services.ai.azure.com/api/projects/my-project")
            .unwrap(),
        "https://test.cognitiveservices.azure.com"
    );
}
