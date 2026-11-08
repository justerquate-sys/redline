//! Pure decisions: canonical video and channel URLs, the toolbar's queue state and its success window, labels and
//! document handling. No browser calls here, so every rule has a unit test.

use serde_json::{Value, json};

pub const REMOVED_MESSAGE: &str = "Removed from your save folder. Kept excluded so it is not downloaded again.";
pub const COMPLETION_MS: f64 = 6000.0;

fn number(v: &Value) -> f64 {
    let n = match v {
        Value::Number(n) => n.as_f64().unwrap_or(0.),
        Value::String(s) => s.trim().parse::<f64>().unwrap_or(0.),
        Value::Bool(true) => 1.,
        _ => 0.,
    };
    if n.is_finite() { n.floor().max(0.) } else { 0. }
}
fn count(v: &Value, key: &str) -> f64 {
    number(&v[key])
}

/// What the toolbar and popup show for a `/queue/status` snapshot: the current batch, independent of old history.
pub fn queue_state(snapshot: &Value, unavailable: bool) -> Value {
    let progress = if snapshot["batch_progress"].is_null() { snapshot } else { &snapshot["batch_progress"] };
    let raw = count(progress, "total");
    let saved = raw.min(count(progress, "saved"));
    let removed = (raw - saved).min(count(progress, "removed"));
    let cancelled = (raw - saved - removed).min(count(progress, "cancelled"));
    let total = raw - removed - cancelled;
    let queued = count(progress, "queued");
    let processing = count(progress, "processing");
    let saving = count(progress, "saving");
    let failed = (total - saved).min(count(progress, "failed") + count(progress, "save_failed") + count(progress, "missing"));
    let unsaved = (if progress.get("unsaved").is_none() { count(progress, "completed") - saved - count(progress, "save_failed") } else { count(progress, "unsaved") } - saving).max(0.);
    let gt = (count(snapshot, "total") - count(snapshot, "removed") - count(snapshot, "cancelled")).max(0.);
    let gs = gt.min(count(snapshot, "saved"));
    let gf = (gt - gs).min(count(snapshot, "failed") + count(snapshot, "save_failed") + count(snapshot, "missing"));
    let gu = (if snapshot.get("unsaved").is_none() { count(snapshot, "completed") - gs - count(snapshot, "save_failed") } else { count(snapshot, "unsaved") } - count(snapshot, "saving")).max(0.);
    let persistence_pending = count(snapshot, "persistence_pending");
    let issues = if persistence_pending > 0. { (gf + gu).max(count(snapshot, "attention_count").max(persistence_pending)) } else { gf + gu };
    let remaining = (total - saved - failed).max(0.);
    let unknown = progress["discovery_unknown"] == true || progress.get("discovering_channels") == Some(&Value::Null) || progress.get("waiting_channels") == Some(&Value::Null);
    let complete = progress["discovery_complete"] == true;
    let discovering = count(progress, "discovering_channels");
    let waiting = count(progress, "waiting_channels");
    let active = queued + processing + saving + discovering + waiting > 0.;
    let settled = !snapshot.is_null() && !unavailable && !active && !unknown && progress["finished"] == true && persistence_pending == 0. && count(progress, "persistence_pending") == 0. && complete;
    let kind = if unavailable {
        "offline"
    } else if snapshot.is_null() {
        "loading"
    } else if processing + saving + discovering > 0. {
        "processing"
    } else if queued + waiting > 0. {
        "queued"
    } else if issues > 0. {
        "attention"
    } else if settled && saved == total {
        if total > 0. { "complete" } else { "idle" }
    } else {
        "unknown"
    };
    json!({"kind":kind,"settled":settled,"total":total,"saved":saved,"removed":removed,"failed":failed,"remaining":remaining,"queued":queued,"processing":processing,"saving":saving,"issues":issues,"discoveryUnknown":unknown,"discoveryComplete":complete,"discovering":discovering,"waiting":waiting,"unsaved":unsaved,"cancelled":cancelled,"batchId":progress["id"].as_str()})
}

/// One success window per observed batch: only saved work that this worker saw active earns the six seconds.
pub fn lifecycle(view: &Value, state: &Value, now: f64) -> Value {
    let mut state = if state.is_object() { state.clone() } else { json!({}) };
    let batch = &view["batchId"];
    let kind = view["kind"].as_str().unwrap_or("loading");
    if state["completionBatchId"] != *batch {
        state["completionUntil"] = json!(0);
    }
    if matches!(kind, "processing" | "queued") {
        if state["activeSeen"] != true || !state["activeBaseline"].is_object() || state["activeBaseline"]["batchId"] != *batch {
            state["activeBaseline"] = json!({"saved":view["saved"],"cancelled":view["cancelled"],"removed":view["removed"],"batchId":batch});
        }
        state["activeSeen"] = json!(true);
        state["completionUntil"] = json!(0);
    } else if kind == "complete" {
        if state["activeSeen"] == true {
            let base = &state["activeBaseline"];
            let ok = base.is_object()
                && base["batchId"] == *batch
                && number(&view["saved"]) > number(&base["saved"])
                && number(&view["cancelled"]) <= number(&base["cancelled"])
                && number(&view["removed"]) <= number(&base["removed"]);
            state["completionUntil"] = json!(if ok { now + COMPLETION_MS } else { 0. });
            state["completionBatchId"] = if ok { batch.clone() } else { Value::Null };
        }
        state["activeSeen"] = json!(false);
        state["activeBaseline"] = Value::Null;
    } else if !matches!(kind, "offline" | "unknown" | "loading") {
        state["activeSeen"] = json!(false);
        state["activeBaseline"] = Value::Null;
        state["completionUntil"] = json!(0);
    } else {
        state["completionUntil"] = json!(0);
    }
    state
}

/// A persisted lifecycle read back at worker start: a damaged baseline or a window that outlives its six seconds is
/// never proof of anything.
pub fn lifecycle_restore(stored: &Value, now: f64) -> Value {
    let active = stored["activeSeen"] == true;
    let baseline = &stored["activeBaseline"];
    let valid = active && ["saved", "cancelled", "removed"].iter().all(|k| baseline[k].as_u64().is_some());
    let until = stored["completionUntil"].as_f64().filter(|u| *u > now && *u <= now + COMPLETION_MS).unwrap_or(0.);
    json!({"activeSeen":active,"activeBaseline":if valid { baseline.clone() } else { Value::Null },"completionUntil":until,"completionBatchId":stored["completionBatchId"].as_str()})
}

/// The toolbar's tooltip for a queue view.
pub fn toolbar_title(view: &Value, check: bool) -> String {
    let n = |k: &str| view[k].as_f64().unwrap_or(0.) as u64;
    let kind = view["kind"].as_str().unwrap_or("loading");
    let base = match kind {
        "processing" if n("discovering") > 0 => "Finding channel videos".to_owned(),
        "processing" if n("saving") > 0 => "Saving transcripts".to_owned(),
        "processing" => format!("{} transcribing, {} queued", n("processing"), n("queued")),
        "queued" if n("waiting") > 0 => format!("{} queued, {} channels waiting", n("queued"), n("waiting")),
        "queued" => format!("{} queued", n("queued")),
        "attention" => format!("{} {} attention", n("issues"), if n("issues") == 1 { "needs" } else { "need" }),
        "complete" if check => "Transcripts saved".to_owned(),
        "complete" | "idle" | "settled" => return String::new(),
        "offline" => "Helper unavailable, last status is unconfirmed".to_owned(),
        _ => "Checking progress".to_owned(),
    };
    let mut title = format!("Transcripts: {base}");
    if n("total") > 0 && kind != "offline" {
        title.push_str(&format!(", {}/{} saved", n("saved"), n("total")));
    }
    if n("issues") > 0 && kind != "attention" {
        title.push_str(&format!(", {} {} attention", n("issues"), if n("issues") == 1 { "needs" } else { "need" }));
    }
    title
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
            (b'%', Some(h), Some(l)) => {
                out.push(h * 16 + l);
                i += 3;
            }
            (b'+', _, _) => {
                out.push(b' ');
                i += 1;
            }
            (c, _, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Url<'a> {
    host: String,
    path: &'a str,
    query: &'a str,
}

/// The parts of an http(s) URL these rules need. Anything a browser might read differently (credentials, control
/// characters, a port) does not parse: the callers reject it.
fn parse_url(value: &str) -> Option<Url<'_>> {
    if value.chars().any(|c| c.is_control() || c == ' ') {
        return None;
    }
    let (scheme, rest) = value.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.contains('@') {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (authority, None),
    };
    match port {
        None | Some("") => {}
        Some("443") if scheme.eq_ignore_ascii_case("https") => {}
        Some("80") if scheme.eq_ignore_ascii_case("http") => {}
        _ => return None,
    }
    if host.is_empty() || tail.starts_with('\\') {
        return None;
    }
    let tail = tail.split('#').next().unwrap_or("");
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    if path.split('/').any(|s| s == ".." || s == "." || s.eq_ignore_ascii_case("%2e%2e")) || path.contains('\\') {
        return None;
    }
    Some(Url { host: host.to_ascii_lowercase(), path, query })
}

fn youtube_host(host: &str) -> bool {
    host == "youtube.com" || host.ends_with(".youtube.com")
}

/// A sender's page: https on YouTube's own hosts.
pub fn is_https_youtube(value: &str) -> bool {
    value.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) && parse_url(value).is_some_and(|u| youtube_host(&u.host))
}

pub fn valid_video_id(id: &str) -> bool {
    id.len() == 11 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

/// Canonical `(id, url)` of a video, Short or recorded live URL: the whole recording, without time or playlist.
pub fn normalize_video(value: &str) -> Result<(String, String), String> {
    let value = value.trim();
    if !value.contains("://") {
        return Err("Paste a complete YouTube video URL.".into());
    }
    let u = parse_url(value).ok_or("Use a regular YouTube video URL.")?;
    let segment = |n: usize| u.path.split('/').nth(n).map(str::to_owned);
    let id = if u.host == "youtu.be" {
        segment(1)
    } else if youtube_host(&u.host) {
        if u.path == "/watch" {
            u.query.split('&').find_map(|p| p.strip_prefix("v=")).map(percent_decode)
        } else if ["/shorts/", "/embed/", "/live/"].iter().any(|p| u.path.starts_with(p)) {
            segment(2)
        } else {
            None
        }
    } else {
        None
    };
    let id = id.filter(|id| valid_video_id(id)).ok_or("Choose a YouTube video, Short, or recorded live video. Channel and playlist pages are not videos.")?;
    let url = format!("https://www.youtube.com/watch?v={id}");
    Ok((id, url))
}

/// The canonical channel URL of any page under a channel.
pub fn normalize_channel(value: &str) -> Result<String, String> {
    let value = value.trim();
    if !value.contains("://") {
        return Err("Open a YouTube channel page to import its videos.".into());
    }
    let u = parse_url(value).ok_or("Choose a regular YouTube channel URL.")?;
    if !youtube_host(&u.host) {
        return Err("Choose a regular YouTube channel URL.".into());
    }
    let parts: Vec<_> = u.path.split('/').collect();
    let path = if parts.get(1).is_some_and(|s| s.starts_with('@') && s.len() > 1) {
        format!("/{}", parts[1])
    } else if parts.len() >= 3 && matches!(parts[1], "channel" | "c" | "user") && !parts[2].is_empty() {
        format!("/{}/{}", parts[1], parts[2])
    } else {
        return Err("Choose a YouTube channel, such as youtube.com/@channel.".into());
    };
    Ok(format!("https://www.youtube.com{path}"))
}

pub fn status_label(status: &str) -> &'static str {
    match status {
        "queued" => "Queued",
        "downloading" => "Retrieving full video audio",
        "preparing" => "Preparing full recording",
        "transcribing" => "Transcribing with Azure",
        "completed" => "Transcript ready",
        "failed" => "Needs attention",
        "cancelled" => "Cancelled",
        "removed" => "Removed",
        _ => "Working",
    }
}

pub fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled" | "removed")
}

pub fn format_duration(v: &Value) -> String {
    let raw = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) if !s.is_empty() => s.trim().parse::<f64>().ok(),
        _ => None,
    };
    let Some(n) = raw.filter(|n| n.is_finite() && *n >= 0.) else { return String::new() };
    clock(n)
}

/// `m:ss` or `h:mm:ss`.
pub fn clock(seconds: f64) -> String {
    let n = seconds.floor() as u64;
    if n >= 3600 { format!("{}:{:02}:{:02}", n / 3600, n / 60 % 60, n % 60) } else { format!("{}:{:02}", n / 60, n % 60) }
}

/// A publication date as the source gave it, without timezone shifts.
pub fn format_published(v: &Value) -> String {
    let s = v.as_str().unwrap_or("").trim();
    let digits = |t: &str| t.bytes().all(|c| c.is_ascii_digit());
    if s.len() == 8 && digits(s) {
        format!("{}-{}-{}", &s[..4], &s[4..6], &s[6..8])
    } else if s.len() >= 10 && s.is_char_boundary(10) && digits(&s[..4]) && digits(&s[5..7]) && digits(&s[8..10]) && s.as_bytes()[4] == b'-' && s.as_bytes()[7] == b'-' && (s.len() == 10 || s.as_bytes()[10] == b'T') {
        s[..10].to_owned()
    } else {
        "Unknown".into()
    }
}

/// Exact duplicates once, whole documents otherwise.
pub fn unique_documents(docs: &[String]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for d in docs {
        let t = d.trim();
        if !t.is_empty() && !out.iter().any(|o| o == t) {
            out.push(t.to_owned());
        }
    }
    out
}
pub fn combine_documents(docs: &[String]) -> String {
    docs.join("\n\n---\n\n")
}

/// The word count of a document (`12,345` is formatted by the page).
pub fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The reader's view of a saved Markdown document: the transcript's own text, without the source header the helper
/// writes above it (title, channel, published, source, description links). The header stays in what is copied.
pub fn document_body(content: &str) -> &str {
    if let Some(i) = content.find("\nTranscript:\n") {
        return content[i + 13..].trim_start_matches('\n');
    }
    let mut rest = content;
    if rest.starts_with("# ")
        && let Some(i) = rest.find('\n')
    {
        rest = rest[i + 1..].trim_start_matches('\n');
    }
    let mut lines = rest.split_inclusive('\n');
    let mut skipped = 0;
    for line in &mut lines {
        let t = line.trim_end();
        if t.starts_with("Channel: ") || t.starts_with("Published: ") || t.starts_with("Source: ") || t.starts_with("PARTIAL —") || t.is_empty() {
            skipped += line.len();
        } else {
            break;
        }
    }
    rest[skipped..].trim_start_matches('\n')
}

/// Paragraphs of a transcript body: blank lines separate them; long unbroken runs of sentences stay one paragraph per
/// recognized section as the helper wrote them.
pub fn paragraphs(body: &str) -> Vec<&str> {
    body.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).collect()
}

/// A live snapshot for the tabs from `/jobs`: only unfinished jobs, only `{video: {s: status, p: percent}}`.
pub fn live_jobs(jobs: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for job in jobs.as_array().into_iter().flatten() {
        let (Some(id), Some(status)) = (job["video_id"].as_str(), job["status"].as_str()) else { continue };
        if !valid_video_id(id) || !matches!(status, "queued" | "downloading" | "preparing" | "transcribing") {
            continue;
        }
        let p = job["progress"].as_f64().unwrap_or(0.).clamp(0., 100.).round();
        out.insert(id.to_owned(), json!({"s":status,"p":p}));
    }
    Value::Object(out)
}

/// A settings file is at most this big (2,000 resume records are about 200 KB).
pub const IMPORT_MAX_BYTES: f64 = 4.0 * 1024.0 * 1024.0;

/// What may be imported, by exact key and JSON kind: the records the client documents. A pairing token, the Transcripts
/// switch, anything unknown or of another kind stays out, however the file was made.
const IMPORTABLE: &[(&str, &[&str])] = &[
    ("ytcConfigV1", &["object"]),
    ("ytcVoiceKeyV1", &["string"]),
    ("ytcPlayerLogV1", &["array"]),
    ("yourTubeHistoryV1", &["object", "array"]),
    ("yourTubeProgressV1", &["object", "array"]),
    ("yourTubeBlockedWebsitesV1", &["object", "array"]),
    ("yourTubeRecommendationTopicsV1", &["object", "array"]),
    ("yourTubeNativeTopicsV1", &["object", "array"]),
    ("yourTubeMinimumHomeDurationV1", &["number", "string"]),
    ("yourTubeHomeOrderV1", &["string"]),
    ("yourTubeShowMusicV1", &["boolean"]),
    ("yourTubeShowLiveV1", &["boolean"]),
    ("yourTubeHomePlaylistsOnlyV1", &["boolean"]),
    ("yourTubeShowMembersV1", &["boolean"]),
    ("shortsInFeeds", &["boolean"]),
];

/// Decides from a file's manifest (`[{"k":key,"t":kind}]`: names and kinds only, the values stay out of Wasm because a
/// Deepgram key is among them) which keys to write: `(keep, skipped)`.
pub fn import_choice(manifest: &Value) -> (Vec<String>, usize) {
    let mut keep = vec![];
    let mut skipped = 0;
    for entry in manifest.as_array().into_iter().flatten() {
        let (k, t) = (entry["k"].as_str().unwrap_or(""), entry["t"].as_str().unwrap_or(""));
        let known = IMPORTABLE.iter().any(|(key, kinds)| *key == k && kinds.contains(&t));
        let zip = t == "array" && k.strip_prefix("allZipExclusions:").is_some_and(|r| r.len() == 64 && r.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        if known || zip {
            keep.push(k.to_owned());
        } else {
            skipped += 1;
        }
    }
    (keep, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_settings_file_brings_documented_records_only() {
        let zip = format!("allZipExclusions:{}", "a".repeat(64));
        let m = json!([
            {"k":"ytcConfigV1","t":"object"},{"k":"ytcVoiceKeyV1","t":"string"},{"k":"shortsInFeeds","t":"boolean"},{"k":zip,"t":"array"},
            {"k":"pairingToken","t":"string"},{"k":"ytcTranscriptsV1","t":"object"},{"k":"ytcSockTestV1","t":"string"},{"k":"ytcCollectorV1","t":"string"},
            {"k":"ytcConfigV1x","t":"object"},{"k":"ytcVoiceKeyV1","t":"object"},{"k":"allZipExclusions:short","t":"array"},{"k":"unknown","t":"string"}
        ]);
        let (keep, skipped) = import_choice(&m);
        assert_eq!(keep, vec!["ytcConfigV1".to_owned(), "ytcVoiceKeyV1".to_owned(), "shortsInFeeds".to_owned(), zip]);
        assert_eq!(skipped, 8);
        assert_eq!(import_choice(&json!("nonsense")), (vec![], 0));
    }
    #[test]
    fn cancelled_does_not_produce_success() {
        let view = json!({"kind":"processing","saved":5,"cancelled":0,"removed":0,"batchId":"b"});
        let active = lifecycle(&view, &json!({}), 0.);
        let done = lifecycle(&json!({"kind":"complete","saved":5,"cancelled":1,"removed":0,"batchId":"b"}), &active, 1.);
        assert_eq!(done["completionUntil"].as_f64(), Some(0.));
    }
    #[test]
    fn observed_saved_work_earns_one_six_second_window() {
        let view = json!({"kind":"processing","saved":0,"cancelled":0,"removed":0,"batchId":"b"});
        let active = lifecycle(&view, &json!({}), 0.);
        let done = lifecycle(&json!({"kind":"complete","saved":1,"cancelled":0,"removed":0,"batchId":"b"}), &active, 100.);
        assert_eq!(done["completionUntil"].as_f64(), Some(6100.));
        // A later read of the same completed history does not replay it.
        let again = lifecycle(&json!({"kind":"complete","saved":1,"cancelled":0,"removed":0,"batchId":"b"}), &done, 200.);
        assert_eq!(again["completionUntil"].as_f64(), Some(6100.));
        assert_eq!(again["activeSeen"], false);
    }
    #[test]
    fn startup_with_completed_history_never_celebrates() {
        let done = lifecycle(&json!({"kind":"complete","saved":9,"cancelled":0,"removed":0,"batchId":"b"}), &json!({}), 1.);
        assert_eq!(done["completionUntil"].as_f64().unwrap_or(0.), 0.);
    }
    #[test]
    fn a_new_batch_cannot_borrow_an_old_baseline() {
        let first = lifecycle(&json!({"kind":"processing","saved":5,"cancelled":0,"removed":0,"batchId":"a"}), &json!({}), 0.);
        let done = lifecycle(&json!({"kind":"complete","saved":6,"cancelled":0,"removed":0,"batchId":"b"}), &first, 1.);
        assert_eq!(done["completionUntil"].as_f64(), Some(0.));
    }
    #[test]
    fn errors_and_offline_clear_the_window() {
        let view = json!({"kind":"processing","saved":0,"cancelled":0,"removed":0,"batchId":"b"});
        let active = lifecycle(&view, &json!({}), 0.);
        let done = lifecycle(&json!({"kind":"complete","saved":1,"cancelled":0,"removed":0,"batchId":"b"}), &active, 1.);
        let off = lifecycle(&json!({"kind":"offline","batchId":"b"}), &done, 2.);
        assert_eq!(off["completionUntil"], json!(0));
        let bad = lifecycle(&json!({"kind":"attention","batchId":"b"}), &done, 2.);
        assert_eq!(bad["completionUntil"], json!(0));
    }
    #[test]
    fn restored_lifecycle_rejects_damage() {
        let s = lifecycle_restore(&json!({"activeSeen":true,"activeBaseline":{"saved":"x"},"completionUntil":99999,"completionBatchId":"b"}), 10.);
        assert_eq!(s["activeBaseline"], Value::Null);
        assert_eq!(s["completionUntil"], json!(0.0));
        let s = lifecycle_restore(&json!({"activeSeen":true,"activeBaseline":{"saved":1,"cancelled":0,"removed":0,"batchId":"b"},"completionUntil":5000}), 10.);
        assert_eq!(s["completionUntil"], json!(5000.0));
    }
    #[test]
    fn video_urls_canonicalize_to_the_whole_recording() {
        for u in ["https://youtu.be/abcdefghijk?t=5", "https://www.youtube.com/watch?v=abcdefghijk&t=99&list=PL1", "https://m.youtube.com/shorts/abcdefghijk", "https://www.youtube.com/live/abcdefghijk?si=x", "http://youtube.com/embed/abcdefghijk", " https://www.youtube.com:443/watch?x=1&v=abcdefghijk#t=1 "] {
            assert_eq!(normalize_video(u).unwrap().1, "https://www.youtube.com/watch?v=abcdefghijk", "{u}");
        }
    }
    #[test]
    fn arbitrary_sites_credentials_ports_and_bad_ids_are_rejected() {
        for u in ["https://evil.test/watch?v=abcdefghijk", "https://youtube.com.evil.test/watch?v=abcdefghijk", "https://evil.test\\@youtube.com/watch?v=abcdefghijk", "https://user:pw@youtube.com/watch?v=abcdefghijk", "https://youtube.com:8443/watch?v=abcdefghijk", "ftp://youtube.com/watch?v=abcdefghijk", "https://youtube.com/watch?v=short", "https://youtube.com/watch?v=abc/defghij", "https://youtube.com/@channel", "https://youtube.com/playlist?list=PL1", "abcdefghijk", ""] {
            assert!(normalize_video(u).is_err(), "{u}");
        }
    }
    #[test]
    fn channel_subpages_normalize_and_unsafe_urls_do_not() {
        assert_eq!(normalize_channel("https://www.youtube.com/@creator/videos?view=0").unwrap(), "https://www.youtube.com/@creator");
        assert_eq!(normalize_channel("https://m.youtube.com/channel/UC123/about").unwrap(), "https://www.youtube.com/channel/UC123");
        assert_eq!(normalize_channel("https://youtube.com/c/name").unwrap(), "https://www.youtube.com/c/name");
        for u in ["https://evil.test/@creator", "https://www.youtube.com/watch?v=abcdefghijk", "https://www.youtube.com/channel/", "https://user@youtube.com/@creator", "https://www.youtube.com/@"] {
            assert!(normalize_channel(u).is_err(), "{u}");
        }
    }
    #[test]
    fn durations_stay_unknown_and_keep_hours() {
        assert_eq!(format_duration(&Value::Null), "");
        assert_eq!(format_duration(&json!("")), "");
        assert_eq!(format_duration(&json!(61)), "1:01");
        assert_eq!(format_duration(&json!(3725.9)), "1:02:05");
    }
    #[test]
    fn publication_dates_keep_the_source_date() {
        assert_eq!(format_published(&json!("20260828")), "2026-08-28");
        assert_eq!(format_published(&json!("2026-08-28T23:59:00-11:00")), "2026-08-28");
        assert_eq!(format_published(&json!("nonsense")), "Unknown");
        assert_eq!(format_published(&Value::Null), "Unknown");
    }
    #[test]
    fn combined_copies_keep_whole_documents_once() {
        let u = unique_documents(&["a\n".into(), "b".into(), " a".into(), "".into()]);
        assert_eq!(u, vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(combine_documents(&u), "a\n\n---\n\nb");
    }
    #[test]
    fn batch_counts_exclude_history() {
        let v = queue_state(&json!({"total":200,"saved":180,"batch_progress":{"id":"a","total":20,"saved":4,"queued":15,"processing":1}}), false);
        assert_eq!(v["total"], 20.);
        assert_eq!(v["saved"], 4.);
        assert_eq!(v["kind"], "processing");
    }
    #[test]
    fn queue_kinds() {
        assert_eq!(queue_state(&Value::Null, false)["kind"], "loading");
        assert_eq!(queue_state(&Value::Null, true)["kind"], "offline");
        let empty = json!({"total":0,"saved":0,"finished":true,"discovery_complete":true,"discovering_channels":0,"waiting_channels":0});
        assert_eq!(queue_state(&empty, false)["kind"], "idle");
        let done = json!({"total":3,"saved":3,"finished":true,"discovery_complete":true,"discovering_channels":0,"waiting_channels":0,"unsaved":0});
        assert_eq!(queue_state(&done, false)["kind"], "complete");
        let unknown = json!({"total":3,"saved":3,"finished":true,"discovery_complete":true,"discovering_channels":null,"waiting_channels":0,"unsaved":0});
        assert_eq!(queue_state(&unknown, false)["kind"], "unknown");
    }
    #[test]
    fn cancelled_is_excluded_and_failures_keep_attention() {
        let v = queue_state(&json!({"total":3,"saved":1,"cancelled":1,"failed":1,"unsaved":0,"finished":true,"discovery_complete":true,"discovering_channels":0,"waiting_channels":0}), false);
        assert_eq!(v["total"], 2.);
        assert_eq!(v["kind"], "attention");
        assert_eq!(v["issues"], 1.);
    }
    #[test]
    fn removed_videos_are_settled_exclusions() {
        let v = queue_state(&json!({"total":2,"saved":1,"removed":1,"unsaved":0,"finished":true,"discovery_complete":true,"discovering_channels":0,"waiting_channels":0}), false);
        assert_eq!(v["total"], 1.);
        assert_eq!(v["remaining"], 0.);
        assert_eq!(v["kind"], "complete");
    }
    #[test]
    fn unsaved_text_needs_attention_while_saving_stays_active() {
        let v = queue_state(&json!({"total":1,"saved":0,"completed":1,"finished":true,"discovery_complete":true,"discovering_channels":0,"waiting_channels":0}), false);
        assert_eq!(v["kind"], "attention");
        let v = queue_state(&json!({"total":1,"saved":0,"completed":1,"saving":1,"discovering_channels":0,"waiting_channels":0}), false);
        assert_eq!(v["kind"], "processing");
    }
    #[test]
    fn persistence_warnings_stay_visible() {
        let v = queue_state(&json!({"total":1,"saved":1,"persistence_pending":1,"attention_count":1,"finished":true,"discovery_complete":true,"discovering_channels":0,"waiting_channels":0}), false);
        assert_eq!(v["kind"], "attention");
    }
    #[test]
    fn titles() {
        let v = queue_state(&json!({"batch_progress":{"id":"a","total":3,"saved":1,"queued":1,"processing":1}}), false);
        assert_eq!(toolbar_title(&v, false), "Transcripts: 1 transcribing, 1 queued, 1/3 saved");
        let idle = queue_state(&json!({"total":0,"finished":true,"discovery_complete":true,"discovering_channels":0,"waiting_channels":0}), false);
        assert_eq!(toolbar_title(&idle, false), "");
        assert!(toolbar_title(&queue_state(&Value::Null, true), false).contains("unavailable"));
    }
    #[test]
    fn the_reader_body_leaves_out_the_source_header() {
        let doc = "# Title\n\nChannel: C\nPublished: 2026-01-02\nSource: https://www.youtube.com/watch?v=abcdefghijk\n\nFirst words.\n\nSecond part.\n";
        assert_eq!(document_body(doc).trim_end(), "First words.\n\nSecond part.");
        let links = "# T\n\nPublished: Unknown\n\nDescription links (creator-provided):\nhttps://example.com\n\nTranscript:\n\nBody here.\n";
        assert_eq!(document_body(links).trim_end(), "Body here.");
        assert_eq!(paragraphs("a\n\n\n\n b \n\nc").len(), 3);
    }
    #[test]
    fn live_snapshot_carries_only_unfinished_jobs() {
        let v = live_jobs(&json!([{"video_id":"abcdefghijk","status":"transcribing","progress":41.6,"title":"private","url":"x"},{"video_id":"abcdefghijl","status":"completed","progress":100},{"video_id":"bad","status":"queued"}]));
        assert_eq!(v, json!({"abcdefghijk":{"s":"transcribing","p":42.0}}));
    }
}
