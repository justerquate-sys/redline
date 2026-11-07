//! Home from YouTube's own Home document (owner, 3 Nov 2026: a Home refresh should be what YouTube itself
//! would show on a reload, not a faster copy of an answer YouTube would not have given).
//!
//! YouTube's page reload is a `GET https://www.youtube.com/` with the browser's cookies; its server renders
//! the Home feed into the document as `var ytInitialData = {…};` (the same shape as a `browse` response) and
//! names that answer in `ytcfg` (`EVENT_ID`), which its page then quotes in the "shown" signals it sends
//! (`crate::impress`). Redline asks for the same document and reads that data (the continuation that follows
//! is the usual `browse` continuation, as on YouTube's page). Measured live (signed out, 3 Nov 2026,
//! tools/bench/home-reload.cjs): the document answers in the same time as the `browse` request (p50 1030
//! vs 1022 ms, its head after 121 ms), 140 KB on the wire instead of 61 KB.
//!
//! Pure functions; `app/homeready.rs` sends the request and falls back to `browse` when a document has
//! no Home data (a consent page, a changed format) or another session index is in use.

use crate::config::{is_client_version, is_visitor};

/// What a reload of Home asks for.
pub const URL: &str = "https://www.youtube.com/";

/// The headers of a reload that a page can set (the browser adds the rest, cookies included).
pub const HEADERS: &str = r#"{"Accept":"text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8"}"#;

/// Fetch options: never the HTTP cache (a reload revalidates; YouTube sends `no-cache` anyway), redirects
/// not followed (a consent or sign-in page is not Home: the `browse` request stands in).
pub fn opts(low: bool) -> &'static str {
    if low { r#"{"cache":"no-store","redirect":"manual","priority":"low"}"# } else { r#"{"cache":"no-store","redirect":"manual"}"# }
}

/// The page's own facts about this answer (from `ytcfg.set({...})` in the document's head).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    /// The server's id of this answer (`EVENT_ID`): the page's "shown" signals name it.
    pub event_id: Option<String>,
    pub visitor: Option<String>,
    pub version: Option<String>,
    /// Country as YouTube placed the request (`INNERTUBE_CONTEXT_GL`), for the signals' context.
    pub gl: Option<String>,
    pub logged_in: Option<bool>,
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let mut i = from;
    while i + needle.len() <= hay.len() {
        let off = hay[i..hay.len() - needle.len() + 1].iter().position(|&b| b == first)?;
        i += off;
        if &hay[i..i + needle.len()] == needle {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// The byte range of the Home data (`ytInitialData`, a JSON object) inside a document.
pub fn initial_data(html: &[u8]) -> Option<std::ops::Range<usize>> {
    const STARTS: [&[u8]; 2] = [b"var ytInitialData = ", b"window[\"ytInitialData\"] = "];
    let (at, len) = STARTS.iter().find_map(|s| find(html, s, 0).map(|i| (i, s.len())))?;
    let start = at + len;
    if html.get(start) != Some(&b'{') {
        return None; // another encoding (a JS string): not this format
    }
    // The object ends where its script does (`<` inside strings is escaped in the document).
    let end = find(html, b";</script>", start)?;
    (end > start + 1 && html[end - 1] == b'}').then_some(start..end)
}

fn cfg_str(head: &[u8], key: &str) -> Option<String> {
    let pat = format!("\"{key}\":\"");
    let at = find(head, pat.as_bytes(), 0)? + pat.len();
    let len = head[at..].iter().take(600).position(|&b| b == b'"')?;
    std::str::from_utf8(&head[at..at + len]).ok().map(String::from)
}

fn cfg_bool(head: &[u8], key: &str) -> Option<bool> {
    let pat = format!("\"{key}\":");
    let at = find(head, pat.as_bytes(), 0)? + pat.len();
    if head[at..].starts_with(b"true") {
        Some(true)
    } else if head[at..].starts_with(b"false") {
        Some(false)
    } else {
        None
    }
}

/// The answer's id and the page's client facts, from the document before its Home data.
pub fn meta(html: &[u8], data_at: usize) -> Meta {
    let head = &html[..data_at.min(html.len())];
    let id = |s: &String| (8..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
    Meta {
        event_id: cfg_str(head, "EVENT_ID").filter(id),
        visitor: cfg_str(head, "VISITOR_DATA").filter(|v| is_visitor(v)),
        version: cfg_str(head, "INNERTUBE_CLIENT_VERSION").filter(|v| is_client_version(v)),
        gl: cfg_str(head, "INNERTUBE_CONTEXT_GL").filter(|g| g.len() == 2 && g.bytes().all(|b| b.is_ascii_uppercase())),
        logged_in: cfg_bool(head, "LOGGED_IN"),
    }
}

/// A Home document: the Home data's range and the page's facts (None: not a Home document).
pub fn split(html: &[u8]) -> Option<(std::ops::Range<usize>, Meta)> {
    let r = initial_data(html)?;
    let m = meta(html, r.start);
    Some((r, m))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape of YouTube's Home document (recorded signed out, 3 Nov 2026; trimmed, the visitor replaced by
    /// the fixtures' fake).
    pub const DOC: &str = concat!(
        r#"<!DOCTYPE html><html><head><script nonce="x">var ytcfg={d:function(){}};ytcfg.set({"CLIENT_CANARY_STATE":"none","DEVICE":"cbr=Chrome","EVENT_ID":"08zpaoXgNPKx-cAP6oehsQQ","EXPERIMENT_FLAGS":{"a":true},"#,
        r#""INNERTUBE_CLIENT_VERSION":"2.20261030.02.00","INNERTUBE_CONTEXT_GL":"DE","LOGGED_IN":false,"VISITOR_DATA":"CgtlMmVGYWtlVmlzMCjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D"});</script></head><body>"#,
        r#"<script nonce="x">var ytInitialData = {"responseContext":{"serviceTrackingParams":[]},"contents":{"twoColumnBrowseResultsRenderer":{"tabs":[{"tabRenderer":{"selected":true,"content":{"richGridRenderer":{"contents":[]}}}}]}},"trackingParams":"CAAQhGciEwj","title":"a </script> b"};</script>"#,
        r#"<script nonce="x">if (window.ytcsi) {window.ytcsi.tick('pdr', null, '');}</script></body></html>"#
    );

    #[test]
    fn finds_the_home_data_and_the_page_facts() {
        let (r, m) = split(DOC.as_bytes()).expect("a Home document");
        let data = &DOC.as_bytes()[r];
        assert!(data.starts_with(b"{\"responseContext\"") && data.ends_with(b"}"));
        let v = crate::json::parse(data).expect("valid JSON");
        assert_eq!(v.get("trackingParams").and_then(crate::json::Value::as_str), Some("CAAQhGciEwj"));
        assert_eq!(m.event_id.as_deref(), Some("08zpaoXgNPKx-cAP6oehsQQ"));
        assert_eq!(m.version.as_deref(), Some("2.20261030.02.00"));
        assert_eq!(m.visitor.as_deref(), Some("CgtlMmVGYWtlVmlzMCjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D"));
        assert_eq!(m.gl.as_deref(), Some("DE"));
        assert_eq!(m.logged_in, Some(false));
    }

    #[test]
    fn other_documents_are_not_home() {
        assert!(split(b"<html><body>Before you continue to YouTube</body></html>").is_none());
        assert!(split(b"").is_none());
        // The JS-string encoding (another client's page) and an unterminated script.
        assert!(split(b"<script>var ytInitialData = '\\x7b\\x22a\\x22:1\\x7d';</script>").is_none());
        assert!(split(b"<script>var ytInitialData = {\"a\":1").is_none());
        // The window[...] form.
        let (r, _) = split(b"<script>window[\"ytInitialData\"] = {\"a\":1};</script>").unwrap();
        assert_eq!(r.len(), 7);
        // Hostile or odd page facts are dropped.
        let m = meta(br#"{"EVENT_ID":"a\"><x","VISITOR_DATA":"nope","INNERTUBE_CLIENT_VERSION":"3.0","INNERTUBE_CONTEXT_GL":"de","LOGGED_IN":maybe}"#, usize::MAX);
        assert_eq!(m, Meta::default());
    }

    #[test]
    fn fetch_options() {
        assert!(opts(true).contains("\"priority\":\"low\"") && !opts(false).contains("priority"));
        assert!(opts(false).contains("\"redirect\":\"manual\""));
    }
}
