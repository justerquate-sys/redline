//! The channel page's import panel, decided and written here so the YouTube page's Wasm carries only a slot for it. A tab
//! holds an opaque state and echoes it back; preview ownership stays in the worker (`worker.rs`).

use crate::core;
use crate::html::esc;
use serde_json::{Value, json};

fn n(progress: &Value, k: &str) -> f64 {
    progress[k].as_f64().unwrap_or(0.).floor().max(0.)
}

fn is_discovering(preview: &Value) -> bool {
    matches!(preview["status"].as_str(), Some("waiting" | "scanning"))
}

/// Still changing: the tab asks again in a moment.
pub fn needs_polling(preview: &Value) -> bool {
    if is_discovering(preview) {
        return true;
    }
    let p = &preview["progress"];
    match preview["status"].as_str() {
        Some("failed" | "cancelled") => n(p, "queued") + n(p, "processing") + n(p, "saving") > 0.,
        Some("ready") => p["finished"] != true,
        _ => false,
    }
}

pub fn all_saved(preview: &Value) -> bool {
    let k = counts(&preview["progress"]);
    preview["status"] == "ready" && preview["progress"]["discovery_complete"] == true && k.total > 0. && k.saved == k.total
}

pub struct Counts {
    pub saved: f64,
    pub failed: f64,
    pub remaining: f64,
    pub total: f64,
    pub recorded: f64,
    pub unsaved: f64,
}

pub fn counts(p: &Value) -> Counts {
    let recorded = n(p, "total");
    let saved = recorded.min(n(p, "saved"));
    let removed = (recorded - saved).min(n(p, "removed"));
    let cancelled = (recorded - saved - removed).min(n(p, "cancelled"));
    let total = (recorded - removed - cancelled).max(0.);
    let failed = (total - saved).min(n(p, "failed") + n(p, "save_failed") + n(p, "missing"));
    let unsaved = if p.get("unsaved").is_none() { (n(p, "completed") - saved - n(p, "save_failed")).max(0.) } else { n(p, "unsaved") };
    Counts { saved, failed, remaining: (total - saved - failed).max(0.), total, recorded, unsaved }
}

/// The panel's inside. `state`: `{url, scope, shorts, id, preview, error}`.
pub fn panel(state: &Value) -> String {
    let item = &state["preview"];
    let p = &item["progress"];
    let status = item["status"].as_str().unwrap_or("");
    let k = counts(p);
    let has_id = state["id"].is_string();
    let discovering = is_discovering(item);
    let complete = p["discovery_complete"] == true;
    let saved_all = all_saved(item);
    let nothing = status == "ready" && complete && k.total == 0.;
    let polling = needs_polling(item);
    let settled = p["finished"] == true || (matches!(status, "failed" | "cancelled") && !polling);
    let all_settled = status == "cancelled" && settled && k.failed == 0. && k.unsaved == 0. && k.remaining == 0.;
    let error = state["error"].as_str().unwrap_or("");
    let unavailable = !error.is_empty();
    let line = if unavailable {
        "Status unavailable, last confirmed counts".to_owned()
    } else if status == "waiting" {
        match item["waiting_position"].as_f64() {
            Some(pos) => format!("Waiting to find videos, position {pos}"),
            None => "Waiting to find videos".to_owned(),
        }
    } else if discovering {
        format!("Finding videos: {} checked, {} found so far", item["processed_count"].as_f64().unwrap_or(0.), item["found_count"].as_f64().unwrap_or(0.))
    } else if saved_all {
        "All transcripts saved".to_owned()
    } else if status == "cancelled" {
        if polling { "Stopped, queued work continues".to_owned() } else { "Stopped".to_owned() }
    } else if nothing {
        if k.recorded > 0. { "No videos left to process".to_owned() } else { "No eligible videos found".to_owned() }
    } else if status == "failed" {
        "Discovery stopped, needs attention".to_owned()
    } else if n(p, "processing") > 0. {
        "Transcribing".to_owned()
    } else if n(p, "queued") > 0. {
        "Waiting to transcribe".to_owned()
    } else if n(p, "saving") > 0. {
        "Saving transcripts".to_owned()
    } else if settled {
        "Needs attention".to_owned()
    } else {
        "Updating progress".to_owned()
    };
    let name = item["channel_name"].as_str().unwrap_or("Channel transcripts");
    let scope = item["scope"].as_str().or(state["scope"].as_str()).unwrap_or("all");
    let shorts = item["include_shorts"].as_bool().or(state["shorts"].as_bool()).unwrap_or(false);
    let ratio = if k.total > 0. && !discovering && !unavailable { k.saved / k.total } else { 0. };
    let problem = if unavailable {
        if has_id { "Could not refresh. Retry to reconnect.".to_owned() } else { "Could not start the import. Retry.".to_owned() }
    } else if !item["error"].is_null() && status != "cancelled" {
        "Discovery failed. See Details.".to_owned()
    } else if n(p, "save_failed") > 0. {
        format!("{} could not save. Check your save folder in Settings.", n(p, "save_failed"))
    } else if n(p, "failed") > 0. {
        format!("{} failed. See the library for details.", n(p, "failed"))
    } else {
        String::new()
    };
    let retry = unavailable || matches!(status, "failed" | "cancelled") || (settled && !saved_all && !all_settled && !nothing);
    let retry_label = if has_id && (unavailable || polling) {
        "Retry status"
    } else if status == "ready" {
        "Refresh status"
    } else if status == "cancelled" {
        "Resume discovery"
    } else {
        "Retry import"
    };
    let f = |key: &str| item[key].as_f64().unwrap_or(0.);
    let mut details: Vec<String> = vec![];
    if unavailable {
        details.push(error.to_owned());
    } else if let Some(e) = item["error"].as_str() {
        details.push(e.to_owned());
    }
    details.extend(item["warnings"].as_array().into_iter().flatten().filter_map(|w| w.as_str().map(str::to_owned)));
    let mut out = format!(
        "<div class=\"tsc-chh\"><b id=\"ytc-tsc-chn\">{name}</b><span id=\"ytc-tsc-chs\" class=\"m\">{range}, Shorts {shorts}</span><button type=\"button\" class=\"tsc-chx\" data-act=\"tsc-chclose\" aria-label=\"Close progress. The import continues\" title=\"Close. The import continues\"><svg viewBox=\"0 0 16 16\" aria-hidden=\"true\"><path d=\"m4 4 8 8m0-8-8 8\"/></svg></button></div><p id=\"ytc-tsc-cht\" class=\"tsc-cht\" role=\"status\">{line}</p><div id=\"ytc-tsc-chb\" class=\"tsc-bar\" role=\"progressbar\" aria-label=\"Channel transcript saving progress\" aria-valuetext=\"{line}\" aria-busy=\"{busy}\"><i id=\"ytc-tsc-chf\" style=\"transform:scaleX({ratio})\"></i></div><p id=\"ytc-tsc-chc\" class=\"m\">{saved} saved, {failed} failed, {remaining} remaining</p>",
        name = esc(name),
        range = if scope == "year" { "Past year" } else { "All videos" },
        shorts = if shorts { "included" } else { "excluded" },
        line = esc(&line),
        busy = !unavailable && (discovering || polling),
        saved = k.saved,
        failed = k.failed,
        remaining = k.remaining,
    );
    if !problem.is_empty() {
        out.push_str(&format!("<p id=\"ytc-tsc-che\" class=\"tsc-che\">{}</p>", esc(&problem)));
    }
    if retry || discovering {
        out.push_str("<div class=\"tsc-cha\">");
        if retry {
            out.push_str(&format!("<button type=\"button\" id=\"ytc-tsc-chr\" class=\"ya-b tsc-b\" data-act=\"tsc-chretry\">{retry_label}</button>"));
        }
        if discovering {
            out.push_str("<button type=\"button\" id=\"ytc-tsc-chk\" class=\"ya-b tsc-b\" data-act=\"tsc-chcancel\" title=\"Stop finding more videos. Videos already queued continue\">Cancel discovery</button>");
        }
        out.push_str("</div>");
    }
    out.push_str(&format!(
        "<details class=\"tsc-chd\"><summary>Details</summary><p id=\"ytc-tsc-chd\" class=\"m\">{} newly queued, {} existing videos reused. Skipped: {} older, {} unknown dates, {} unavailable</p>{}<p class=\"m\">Cancel stops discovery; queued videos continue. Existing transcripts are reused.</p></details>",
        f("queued_count"),
        f("existing_count"),
        f("skipped_old"),
        f("skipped_unknown"),
        f("skipped_unavailable"),
        if details.is_empty() { String::new() } else { format!("<p id=\"ytc-tsc-chw\" class=\"m\">{}</p>", esc(&details.join("\n"))) },
    ));
    out
}

/// The next state after an import was asked for: the channel's canonical URL and clean fields.
pub fn start_state(url: &str, scope: &str, shorts: bool) -> Result<Value, String> {
    let url = core::normalize_channel(url)?;
    if !matches!(scope, "all" | "year") {
        return Err("Choose past year or all available videos.".into());
    }
    Ok(json!({"url":url,"scope":scope,"shorts":shorts,"id":null,"preview":null,"error":""}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(preview: Value) -> Value {
        json!({"url":"https://www.youtube.com/@c","scope":"year","shorts":false,"id":"pv1","preview":preview,"error":""})
    }
    #[test]
    fn counts_exclude_removed_and_cancelled() {
        let c = counts(&json!({"total":10,"saved":4,"removed":1,"cancelled":1,"failed":1,"unsaved":0}));
        assert_eq!((c.total, c.saved, c.failed, c.remaining), (8., 4., 1., 3.));
    }
    #[test]
    fn discovery_shows_its_numbers_and_can_be_cancelled() {
        let h = panel(&state(json!({"status":"scanning","processed_count":4,"found_count":2,"channel_name":"<C>","progress":{"total":2,"queued":2}})));
        assert!(h.contains("Finding videos: 4 checked, 2 found so far"));
        assert!(h.contains("tsc-chcancel") && !h.contains("tsc-chretry"));
        assert!(h.contains("&lt;C&gt;"), "text is escaped");
        assert!(h.contains("Past year, Shorts excluded"));
    }
    #[test]
    fn finished_and_failed() {
        let done = json!({"status":"ready","progress":{"total":2,"saved":2,"discovery_complete":true,"finished":true,"unsaved":0}});
        assert!(all_saved(&done) && !needs_polling(&done));
        let h = panel(&state(done));
        assert!(h.contains("All transcripts saved") && h.contains("scaleX(1)") && !h.contains("tsc-chretry"));
        let h = panel(&state(json!({"status":"failed","error":"boom","progress":{"total":0}})));
        assert!(h.contains("Discovery failed") && h.contains("boom") && h.contains("Retry import"));
        let mut s = state(json!({"status":"scanning","progress":{}}));
        s["error"] = json!("gone");
        let h = panel(&s);
        assert!(h.contains("Status unavailable") && h.contains("Retry status"));
    }
    #[test]
    fn a_stopped_discovery_keeps_polling_while_work_is_queued() {
        assert!(needs_polling(&json!({"status":"cancelled","progress":{"queued":1}})));
        assert!(!needs_polling(&json!({"status":"cancelled","progress":{"queued":0}})));
    }
    #[test]
    fn start_state_is_canonical_and_validated() {
        let s = start_state("https://www.youtube.com/@c/videos", "all", true).unwrap();
        assert_eq!(s["url"], "https://www.youtube.com/@c");
        assert!(start_state("https://evil.example/@c", "all", false).is_err());
        assert!(start_state("https://www.youtube.com/@c", "month", false).is_err());
    }
}
