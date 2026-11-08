//! Synchronous message policy and fixed-loopback HTTP request planning.
//!
//! `None` means the worker's orchestration is required (`worker.rs`: storage, native messaging, tab ownership and
//! in-flight request coalescing), never that validation or the trusted-page gate can be skipped.
use serde_json::{Value, json};

const CONTENT_ACTIONS: &[&str] = &[
    "startJob",
    "cancelVideo",
    "getVideoStatuses",
    "revealVideoFile",
    "previewChannel",
    "startChannelImport",
    "getChannelPreview",
    "queueChannelPreview",
    "cancelChannelPreview",
    "openReaderVideo",
    "channelPanel",
];
const DENIED: &str = "This request is not allowed.";

/// Return true for an extension page, false for an authorized YouTube content
/// script, or an error. The runtime ID is required in both cases.
pub fn sender_trust(
    message: &Value,
    sender: &Value,
    runtime_id: &str,
    extension_base: &str,
) -> Result<bool, String> {
    if runtime_id.is_empty() || sender["id"].as_str() != Some(runtime_id) {
        return Err(DENIED.into());
    }
    let sender_url = sender["url"].as_str().unwrap_or("");
    let expected_base = format!("chrome-extension://{runtime_id}/");
    if extension_base != expected_base {
        return Err(DENIED.into());
    }
    if sender_url.starts_with(extension_base) {
        return Ok(true);
    }
    let action = message["action"].as_str().unwrap_or("");
    if !CONTENT_ACTIONS.contains(&action) || !sender["tab"].is_object() {
        return Err(DENIED.into());
    }
    if crate::core::is_https_youtube(sender_url) {
        Ok(false)
    } else {
        Err(DENIED.into())
    }
}

fn ascii_id(value: &Value, message: &str) -> Result<String, String> {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 100
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .map(str::to_owned)
        .ok_or_else(|| message.to_owned())
}
fn job_id(value: &Value) -> Result<String, String> {
    ascii_id(value, "Invalid transcript identifier.")
}
fn folder_id(value: &Value) -> Result<Value, String> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    ascii_id(value, "Choose a valid library folder.").map(Value::String)
}
fn required_folder(message: &Value) -> Result<Value, String> {
    message
        .get("folderId")
        .ok_or_else(|| "Choose a valid library folder.".to_owned())
        .and_then(folder_id)
}
fn selected_folder(message: &Value) -> Result<Value, String> {
    message
        .get("folderId")
        .map(folder_id)
        .unwrap_or(Ok(Value::Null))
}
fn video_id(value: &Value) -> Result<String, String> {
    value
        .as_str()
        .filter(|s| {
            s.len() == 11
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .map(str::to_owned)
        .ok_or_else(|| "Invalid YouTube video identifier.".to_owned())
}
fn preview_id(value: &Value) -> Result<String, String> {
    ascii_id(value, "Invalid channel preview.")
}
fn job_ids(value: &Value) -> Result<Value, String> {
    let items = value
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| "Select at least one completed transcript.".to_owned())?;
    let mut unique = Vec::new();
    for item in items {
        let id = job_id(item)?;
        if !unique.contains(&id) {
            unique.push(id);
        }
    }
    Ok(json!(unique))
}
fn request(path: impl Into<String>, method: &str) -> Value {
    json!({"path":path.into(),"method":method})
}
fn with_body(path: impl Into<String>, body: Value) -> Value {
    let mut plan = request(path, "POST");
    plan["body"] = body;
    plan
}
fn scope_path(base: &str, folder: &Value) -> String {
    if let Some(folder) = folder.as_str() {
        format!("{base}?folder_id={folder}")
    } else {
        base.to_owned()
    }
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.),
        _ => true,
    }
}
fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(items) => items
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        _ => value.to_string(),
    }
}
fn optional_string(value: &Value) -> String {
    if truthy(value) {
        js_string(value)
    } else {
        String::new()
    }
}
fn format(value: &Value) -> Result<&str, String> {
    value
        .as_str()
        .filter(|s| ["md", "txt", "json"].contains(s))
        .ok_or_else(|| "Choose Markdown, text, or JSON.".into())
}

pub fn plan(message: &Value, trusted: bool) -> Result<Option<Value>, String> {
    let action = message["action"]
        .as_str()
        .ok_or_else(|| "Unknown extension request.".to_owned())?;
    if !trusted && !CONTENT_ACTIONS.contains(&action) {
        return Err(DENIED.into());
    }
    let value = match action {
        "startJob" => {
            crate::core::normalize_video(message["url"].as_str().unwrap_or(""))?;
            if trusted && message.get("folderId").is_some() {
                required_folder(message)?;
            }
            return Ok(None);
        }
        "cancelVideo" | "revealVideoFile" => {
            video_id(&message["videoId"])?;
            return Ok(None);
        }
        "cancelQueue" => return Ok(None),
        "getVideoStatuses" => {
            let list = message["videoIds"]
                .as_array()
                .filter(|v| v.len() <= 200)
                .ok_or_else(|| "Check up to 200 video statuses at a time.".to_owned())?;
            for id in list {
                video_id(id)?;
            }
            return Ok(None);
        }
        "previewChannel" | "startChannelImport" => {
            if !matches!(message["scope"].as_str(), Some("year" | "all")) {
                return Err("Choose past year or all available videos.".into());
            }
            if message
                .get("include_shorts")
                .is_some_and(|v| !v.is_boolean())
            {
                return Err("Choose whether to include Shorts.".into());
            }
            crate::core::normalize_channel(message["url"].as_str().unwrap_or(""))?;
            return Ok(None);
        }
        "getChannelPreview" | "queueChannelPreview" | "cancelChannelPreview" => {
            let id = preview_id(&message["id"])?;
            if !trusted || action == "queueChannelPreview" {
                return Ok(None);
            }
            if action == "getChannelPreview" {
                request(format!("/channels/previews/{id}"), "GET")
            } else {
                request(format!("/channels/previews/{id}/cancel"), "POST")
            }
        }
        "health" => {
            let mut p = request("/health", "GET");
            p["publicRequest"] = json!(true);
            p
        }
        "getQueueStatus" => return Ok(None),
        "getLibraryStats" => {
            selected_folder(message)?;
            return Ok(None);
        }
        "getFolderDocuments" => {
            let mut p = request(
                scope_path("/library/documents", &selected_folder(message)?),
                "GET",
            );
            p["canReconnect"] = json!(false);
            p["canPair"] = json!(false);
            p["timeoutMs"] = json!(30000);
            p["notFoundMessage"] =
                json!("The installed helper needs an update before saved folders can be copied.");
            p
        }
        "prepareFolderAttachment" => {
            let folder = selected_folder(message)?;
            let mut body = json!({"folder_id":folder});
            if message.get("excludedFolders").is_some() || message.get("rootToken").is_some() {
                let error = || "Refresh the All ZIP folder selection and try again.".to_owned();
                if !folder.is_null() {
                    return Err(error());
                }
                let exclusions = message["excludedFolders"]
                    .as_array()
                    .filter(|v| v.len() <= 1000)
                    .ok_or_else(error)?;
                let mut names = Vec::new();
                for value in exclusions {
                    let name = value.as_str().ok_or_else(error)?;
                    if name.is_empty()
                        || name.encode_utf16().count() > 255
                        || name.len() > 255
                        || name
                            .chars()
                            .any(|c| c < ' ' || c == '\x7f' || c == '/' || c == '\\')
                        || matches!(name, "." | "..")
                        || (name.len() >= 2
                            && name.as_bytes()[0].is_ascii_alphabetic()
                            && name.as_bytes()[1] == b':')
                    {
                        return Err(error());
                    }
                    if !names.iter().any(|n| n == name) {
                        names.push(name.to_owned());
                    }
                }
                let token = message["rootToken"]
                    .as_str()
                    .filter(|s| {
                        s.len() == 64
                            && s.bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
                    .ok_or_else(error)?;
                // JavaScript's default sort compares UTF-16 code units.
                names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
                body["excluded_folders"] = json!(names);
                body["root_token"] = json!(token);
            }
            let mut p = with_body("/library/attachments", body);
            p["canReconnect"] = json!(false);
            p["canPair"] = json!(false);
            p["timeoutMs"] = json!(60000);
            p["notFoundMessage"] = json!("Update the helper to copy folders as file attachments.");
            p
        }
        "copyFolderAttachment" | "revealFolderAttachment" => {
            let id = message["id"]
                .as_str()
                .filter(|s| {
                    s.len() == 32
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
                .ok_or_else(|| "Prepare this folder's attachment again.".to_owned())?;
            let mut p = request(
                format!(
                    "/library/attachments/{id}/{}",
                    if action == "copyFolderAttachment" {
                        "copy"
                    } else {
                        "reveal"
                    }
                ),
                "POST",
            );
            p["canReconnect"] = json!(false);
            p["canPair"] = json!(false);
            p["timeoutMs"] = json!(60000);
            p
        }
        "listJobs" => request("/jobs", "GET"),
        "getQueueIssues" => {
            let mut p = request("/queue/attention", "GET");
            p["canReconnect"] = json!(false);
            p
        }
        "getLibrary" => request("/library", "GET"),
        "getExportFolders" => {
            let mut p = request("/library/export-folders", "GET");
            p["canReconnect"] = json!(false);
            p["canPair"] = json!(false);
            p["notFoundMessage"] = json!("Update the helper to choose folders for the All ZIP.");
            p
        }
        "createFolder" => {
            let name = optional_string(&message["name"]).trim().to_owned();
            if name.is_empty() {
                return Err("Give your folder a name.".into());
            }
            with_body("/folders", json!({"name":name}))
        }
        "setDefaultFolder" => with_body(
            "/library/default-folder",
            json!({"folder_id":required_folder(message)?}),
        ),
        "moveJobs" => with_body(
            "/jobs/move",
            json!({"job_ids":job_ids(&message["jobIds"])? ,"folder_id":required_folder(message)?}),
        ),
        "getExportSettings" => request("/export-settings", "GET"),
        "saveExportSettings" => {
            if !message["enabled"].is_boolean()
                || !matches!(message["format"].as_str(), Some("txt" | "md"))
            {
                return Err("Choose text or Markdown and whether to save automatically.".into());
            }
            with_body(
                "/export-settings",
                json!({"enabled":message["enabled"],"format":message["format"]}),
            )
        }
        "chooseExportDirectory" => {
            let mut p = request("/export-settings/choose-directory", "POST");
            p["canReconnect"] = json!(false);
            p
        }
        "getExportDirectoryChoice" => request("/export-settings/choose-directory", "GET"),
        "revealExportDirectory" => {
            let mut p = request("/export-settings/reveal-directory", "POST");
            p["canReconnect"] = json!(false);
            if message.get("folderId").is_some() {
                p["body"] = json!({"folder_id":required_folder(message)?});
            }
            p
        }
        "exportJobs" => {
            if message.get("jobIds").is_some() && message.get("folderId").is_some() {
                return Err("Choose selected transcripts or a folder to save.".into());
            }
            let mut body = if message.get("jobIds").is_some() {
                json!({"job_ids":job_ids(&message["jobIds"])?})
            } else if message.get("folderId").is_some() {
                json!({"folder_id":required_folder(message)?})
            } else {
                json!({})
            };
            if message.get("format").is_some() {
                body["format"] = json!(format(&message["format"])?);
            }
            let mut p = with_body("/exports", body);
            p["timeoutMs"] = json!(60000);
            p
        }
        "getJob" => request(format!("/jobs/{}", job_id(&message["id"])?), "GET"),
        "cancelJob" => request(format!("/jobs/{}/cancel", job_id(&message["id"])?), "POST"),
        "getDocument" => {
            let format = format(&message["format"])?;
            request(
                format!("/jobs/{}/document?format={format}", job_id(&message["id"])?),
                "GET",
            )
        }
        "openReader" => {
            if message.get("id").is_some_and(|v| !v.is_null() && v != "") {
                job_id(&message["id"])?;
            }
            if message.get("view").is_some_and(|v| v != "attention") {
                return Err("Choose a valid library view.".into());
            }
            return Ok(None);
        }
        "openReaderVideo" => {
            video_id(&message["videoId"])?;
            return Ok(None);
        }
        "channelPanel" => {
            if !matches!(message["op"].as_str(), Some("start" | "poll" | "cancel" | "retry")) {
                return Err("Unknown extension request.".into());
            }
            return Ok(None);
        }
        "openSettings" | "openExportPage" | "connectHelper" | "getSettings" | "forgetSettings" => return Ok(None),
        "getAzureSettings" => request("/settings", "GET"),
        "saveAzureSettings" => {
            let endpoint = optional_string(&message["endpoint"]).trim().to_owned();
            if endpoint.is_empty() {
                return Err("Enter your Azure endpoint or Microsoft Foundry project URL.".into());
            }
            let mut body = json!({"endpoint":endpoint});
            let key = message["apiKey"].as_str().unwrap_or("").trim();
            if !key.is_empty() {
                body["api_key"] = json!(key);
            }
            if message["useBraveLogin"].is_boolean() {
                body["use_brave_login"] = message["useBraveLogin"].clone();
            }
            with_body("/settings", body)
        }
        "testAzureConnection" => {
            let mut p = request("/settings/test", "POST");
            p["timeoutMs"] = json!(45000);
            p
        }
        "saveSettings" => {
            let token = optional_string(&message["pairingToken"]).trim().to_owned();
            if !(16..=1024).contains(&token.encode_utf16().count())
                || token.chars().any(char::is_whitespace)
            {
                return Err("Paste the complete local helper pairing token.".into());
            }
            return Ok(None);
        }
        _ => return Err("Unknown extension request.".into()),
    };
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(value: Value) -> Value {
        plan(&value, true).unwrap().unwrap()
    }
    #[test]
    fn every_privileged_orchestrated_action_is_gated() {
        for action in [
            "cancelQueue",
            "getQueueStatus",
            "getLibraryStats",
            "openReader",
            "openSettings",
            "openExportPage",
            "connectHelper",
            "getSettings",
            "saveSettings",
            "forgetSettings",
            "getAzureSettings",
            "saveAzureSettings",
            "testAzureConnection",
            "prepareFolderAttachment",
            "copyFolderAttachment",
            "revealFolderAttachment",
            "getExportFolders",
            "getFolderDocuments",
            "getLibrary",
            "listJobs",
            "createFolder",
            "moveJobs",
            "getJob",
            "cancelJob",
            "getDocument",
            "chooseExportDirectory",
            "revealExportDirectory",
            "getQueueIssues",
        ] {
            assert_eq!(
                plan(&json!({"action":action}), false).unwrap_err(),
                DENIED,
                "{action}"
            );
        }
    }
    #[test]
    fn sender_requires_own_runtime_and_https_youtube_tab() {
        let id = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let base = format!("chrome-extension://{id}/");
        let m = json!({"action":"startJob"});
        assert!(
            sender_trust(
                &m,
                &json!({"id":id,"url":format!("{base}popup.html")}),
                id,
                &base
            )
            .unwrap()
        );
        assert!(!sender_trust(&m,&json!({"id":id,"url":"https://www.youtube.com/watch?v=abcdefghijk","tab":{"id":3}}),id,&base).unwrap());
        for sender in [
            json!({"id":"other","url":format!("{base}popup.html")}),
            json!({"id":id,"url":"http://youtube.com","tab":{}}),
            json!({"id":id,"url":"https://youtube.com"}),
            json!({"id":id,"url":"https://youtube.com.evil.test","tab":{}}),
        ] {
            assert!(sender_trust(&m, &sender, id, &base).is_err());
        }
        assert!(
            sender_trust(
                &json!({"action":"getSettings"}),
                &json!({"id":id,"url":"https://youtube.com","tab":{}}),
                id,
                &base
            )
            .is_err()
        );
    }
    #[test]
    fn direct_plans_ignore_arbitrary_paths_and_keep_explicit_action_scope() {
        let planned = p(
            json!({"action":"revealExportDirectory","folderId":"motion","path":"/arbitrary","url":"https://evil.test"}),
        );
        assert_eq!(planned["path"], "/export-settings/reveal-directory");
        assert_eq!(planned["body"], json!({"folder_id":"motion"}));
        assert_eq!(planned["canReconnect"], false);
        let all = p(json!({"action":"exportJobs"}));
        assert_eq!(all["body"], json!({}));
    }
    #[test]
    fn folder_and_document_identifiers_cannot_change_routes() {
        for id in [
            json!(""),
            json!("../private"),
            json!("id?format=json"),
            json!("folder/file"),
            json!(7),
            json!([]),
            json!({}),
            json!("a".repeat(101)),
        ] {
            assert!(plan(&json!({"action":"getJob","id":id}), true).is_err());
            assert!(plan(&json!({"action":"getFolderDocuments","folderId":id}), true).is_err());
        }
        assert!(plan(&json!({"action":"setDefaultFolder"}), true).is_err());
        assert_eq!(
            p(json!({"action":"getFolderDocuments","folderId":null}))["path"],
            "/library/documents"
        );
        assert_eq!(
            p(json!({"action":"getFolderDocuments","folderId":"motion"}))["path"],
            "/library/documents?folder_id=motion"
        );
    }
    #[test]
    fn attachment_selection_is_root_bound_safe_and_utf16_sorted() {
        let token = "a".repeat(64);
        let planned = p(
            json!({"action":"prepareFolderAttachment","excludedFolders":["Motion","Motion","日本語"],"rootToken":token}),
        );
        assert_eq!(
            planned["body"],
            json!({"folder_id":null,"excluded_folders":["Motion","日本語"],"root_token":token})
        );
        assert_eq!(planned["canPair"], false);
        assert_eq!(planned["canReconnect"], false);
        assert_eq!(planned["timeoutMs"], 60000);
        for name in [".", "..", "a/b", "a\\b", "C:folder", "bad\nname", ""] {
            assert!(plan(&json!({"action":"prepareFolderAttachment","excludedFolders":[name],"rootToken":token}),true).is_err());
        }
        assert!(plan(&json!({"action":"prepareFolderAttachment","excludedFolders":["a".repeat(256)],"rootToken":token}),true).is_err());
        assert!(plan(&json!({"action":"prepareFolderAttachment","folderId":"motion","excludedFolders":[],"rootToken":token}),true).is_err());
        assert!(
            plan(
                &json!({"action":"prepareFolderAttachment","excludedFolders":[]}),
                true
            )
            .is_err()
        );
        assert!(
            plan(
                &json!({"action":"prepareFolderAttachment","rootToken":token}),
                true
            )
            .is_err()
        );
    }
    #[test]
    fn native_attachment_operations_accept_only_opaque_ids() {
        for action in ["copyFolderAttachment", "revealFolderAttachment"] {
            let planned = p(
                json!({"action":action,"id":"a".repeat(32),"path":"/private","content":"ignored"}),
            );
            assert!(planned["body"].is_null());
            assert_eq!(planned["canPair"], false);
            assert_eq!(planned["canReconnect"], false);
            for id in ["../bad", "ABCDEF0123456789ABCDEF0123456789", "short"] {
                assert!(plan(&json!({"action":action,"id":id}), true).is_err());
            }
        }
    }
    #[test]
    fn scopes_preserve_null_and_omission_and_deduplicate_jobs() {
        assert_eq!(p(json!({"action":"exportJobs"}))["body"], json!({}));
        assert_eq!(
            p(json!({"action":"exportJobs","folderId":null}))["body"],
            json!({"folder_id":null})
        );
        assert_eq!(
            p(json!({"action":"moveJobs","jobIds":["a","a","b"],"folderId":null}))["body"],
            json!({"job_ids":["a","b"],"folder_id":null})
        );
        assert!(
            plan(
                &json!({"action":"exportJobs","jobIds":["a"],"folderId":null}),
                true
            )
            .is_err()
        );
        assert!(plan(&json!({"action":"exportJobs","jobIds":[]}), true).is_err());
    }
    #[test]
    fn orchestrated_messages_still_validate_every_field() {
        assert!(
            plan(
                &json!({"action":"startJob","url":"https://evil.test"}),
                false
            )
            .is_err()
        );
        assert!(plan(&json!({"action":"startJob","url":"https://youtu.be/abcdefghijk","folderId":"../ignored"}),false).unwrap().is_none());
        assert!(plan(&json!({"action":"startJob","url":"https://youtu.be/abcdefghijk","folderId":"../invalid"}),true).is_err());
        assert!(
            plan(
                &json!({"action":"getVideoStatuses","videoIds":vec!["abcdefghijk";201]}),
                false
            )
            .is_err()
        );
        assert!(
            plan(&json!({"action":"getVideoStatuses","videoIds":[]}), false)
                .unwrap()
                .is_none()
        );
        assert!(plan(&json!({"action":"startChannelImport","url":"https://youtube.com/@test","scope":"all","include_shorts":1}),false).is_err());
        assert!(
            plan(&json!({"action":"getChannelPreview","id":"valid"}), false)
                .unwrap()
                .is_none(),
            "content preview ownership remains required"
        );
        assert!(
            plan(&json!({"action":"queueChannelPreview","id":"valid"}), true)
                .unwrap()
                .is_none(),
            "pending Stop all remains required"
        );
        assert!(plan(&json!({"action":"openReader","id":"../bad"}), true).is_err());
        assert!(plan(&json!({"action":"openReader","view":"unknown"}), true).is_err());
        assert!(
            plan(
                &json!({"action":"saveSettings","pairingToken":"short"}),
                true
            )
            .is_err()
        );
    }
    #[test]
    fn direct_settings_only_send_declared_fields() {
        let planned = p(
            json!({"action":"saveAzureSettings","endpoint":" https://resource.cognitiveservices.azure.com ","apiKey":" key ","useBraveLogin":true,"token":"ignored"}),
        );
        assert_eq!(
            planned["body"],
            json!({"endpoint":"https://resource.cognitiveservices.azure.com","api_key":"key","use_brave_login":true})
        );
        assert_eq!(
            p(json!({"action":"testAzureConnection"}))["timeoutMs"],
            45000
        );
        assert_eq!(p(json!({"action":"health"}))["publicRequest"], true);
    }
}
