//! Save to playlist: the user's playlists with this video's membership from
//! `playlist/get_add_to_playlist`, and the chooser markup. Opening follows the
//! agreed Save behaviour (hover with intent delay and transit grace,
//! click/keyboard access, nothing selected by hover); the timing lives in
//! `mod.rs`.

use super::{find_all, st, text_at};
use crate::util::{esc_into, is_safe_id};
use crate::json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    /// "Private", "Unlisted", "Public" or "".
    pub privacy: String,
    pub contains: bool,
}

fn privacy(s: &str) -> String {
    match s {
        "PRIVATE" => "Private",
        "UNLISTED" => "Unlisted",
        "PUBLIC" => "Public",
        _ => "",
    }
    .into()
}

/// `None` when the response has no recognizable playlist list.
pub fn parse(v: &Value) -> Option<Vec<Playlist>> {
    let mut out: Vec<Playlist> = Vec::new();
    for o in find_all(v, "playlistAddToOptionRenderer") {
        let Some(id) = st(o, &["playlistId"]).filter(|i| is_safe_id(i)) else { continue };
        if out.iter().any(|p| p.id == id) {
            continue;
        }
        out.push(Playlist {
            id: id.into(),
            title: text_at(o, &["title"]).unwrap_or_else(|| id.into()),
            privacy: privacy(st(o, &["privacy"]).unwrap_or("")),
            contains: matches!(st(o, &["containsSelectedVideos"]), Some("ALL") | Some("SOME")),
        });
    }
    if out.is_empty() {
        // View-model chooser: list items whose commands edit a playlist.
        for it in find_all(v, "listItemViewModel") {
            let edits = find_all(it, "playlistEditEndpoint");
            let Some(id) = edits.iter().find_map(|e| st(e, &["playlistId"])).filter(|i| is_safe_id(i)) else { continue };
            if out.iter().any(|p| p.id == id) {
                continue;
            }
            let mut contains = None;
            super::walk(it, &mut |k, x| {
                if contains.is_none() && matches!(k, "isSelected" | "isToggled" | "selected") {
                    contains = x.as_bool();
                }
            });
            if contains.is_none() {
                // The tap command adds when the video is not in the list yet.
                let action = edits.iter().find_map(|e| super::find(e, "action")).and_then(Value::as_str).unwrap_or("");
                contains = Some(action.starts_with("ACTION_REMOVE"));
            }
            out.push(Playlist {
                id: id.into(),
                title: text_at(it, &["title"]).unwrap_or_else(|| id.into()),
                privacy: text_at(it, &["subtitle"]).unwrap_or_default(),
                contains: contains.unwrap_or(false),
            });
        }
    }
    (!out.is_empty() || v.get("contents").is_some()).then_some(out)
}

pub enum Body<'a> {
    Loading,
    Failed(&'a str),
    /// (playlist, shown membership, request in flight)
    List(&'a [(Playlist, bool, bool)]),
}

pub const SAVE_P: &str = "ytc-save-p";
/// A checkbox: a persistent box whose check mark draws itself in (client.css, stroke-dashoffset).
pub const CK: &str = "<i class=\"ya-ck\"><svg viewBox=\"0 0 12 12\" aria-hidden=\"true\"><path pathLength=\"1\" d=\"M2.6 6.3 4.9 8.6 9.5 3.6\"/></svg></i>";
pub const SAVE_B: &str = "ytc-save-b";

/// The Save button with its chooser. The chooser element always exists (it
/// grows from the button and shrinks back); its content is written only
/// while open (`inner`).
pub fn html(open: bool, saved: bool, body: Body, msg: &str) -> String {
    let mut s = format!(
        "<div class=\"ya-pw\" data-pop=\"save\"><button type=\"button\" id=\"{SAVE_B}\" class=\"{}\" data-act=\"a-save\" data-hov=\"sv\" aria-haspopup=\"menu\" aria-expanded=\"{open}\">{BOOKMARK}<span id=\"ytc-save-l\">{}</span></button>",
        btn_class(open, saved, false),
        label(saved)
    );
    s.push_str(&format!("<div id=\"{SAVE_P}\" class=\"{}\" data-hov=\"svm\" role=\"menu\" aria-label=\"Save to playlist\">", super::notify::pop_class("ya-pop", open, false)));
    if open {
        s.push_str(&inner(body, msg));
    }
    s.push_str("</div></div>");
    s
}

/// The Save button's bookmark: an outline and a filled copy on top of it; the fill sweeps in
/// bottom-up (clip-path, client.css) while the video is in any playlist (`saved`).
pub const BOOKMARK: &str = "<i class=\"ya-bm\" aria-hidden=\"true\"><svg class=\"o\" viewBox=\"0 0 24 24\"><path d=\"M6.5 3.5h11a1 1 0 0 1 1 1v16l-6.5-4.2-6.5 4.2v-16a1 1 0 0 1 1-1z\"/></svg><svg class=\"f\" viewBox=\"0 0 24 24\"><path d=\"M6.5 3.5h11a1 1 0 0 1 1 1v16l-6.5-4.2-6.5 4.2v-16a1 1 0 0 1 1-1z\"/></svg></i>";

/// `saved`: the video is in at least one playlist (shown state, optimistic). `now`: a keyboard
/// change, the fill switches without motion.
pub fn btn_class(open: bool, saved: bool, now: bool) -> String {
    let mut c = String::from("ya-b ya-sv");
    if open {
        c.push_str(" open");
    }
    if saved {
        c.push_str(" saved");
    }
    if now {
        c.push_str(" now");
    }
    c
}

pub fn label(saved: bool) -> &'static str {
    if saved { "Saved" } else { "Save" }
}

/// In any playlist, as shown (`rows`: playlist, shown membership, in flight).
pub fn any_saved(rows: &[(Playlist, bool, bool)]) -> bool {
    rows.iter().any(|r| r.1)
}

/// The chooser's content.
pub fn inner(body: Body, msg: &str) -> String {
    let mut s = String::from("<div class=\"ya-ph\">Save to…</div>");
    match body {
        Body::Loading => s.push_str("<div class=\"ya-load\"></div>"),
        Body::Failed(r) => {
            s.push_str("<p class=\"ya-err\">");
            esc_into(&mut s, r);
            s.push_str("</p>");
        }
        Body::List(list) => {
            rows_html(&mut s, list);
            s.push_str("<form id=\"ytc-a-newpl\" class=\"ya-new\"><input name=\"title\" maxlength=\"150\" placeholder=\"New private playlist\" aria-label=\"New playlist name\" autocomplete=\"off\"><button type=\"submit\">Create</button></form>");
        }
    }
    s.push_str("<p id=\"ytc-save-m\" class=\"ya-err\" role=\"status\">");
    esc_into(&mut s, msg);
    s.push_str("</p>");
    s
}

fn rows_html(s: &mut String, list: &[(Playlist, bool, bool)]) {
    if list.is_empty() {
        s.push_str("<p class=\"ya-err\">No playlists yet.</p>");
    }
    s.push_str("<div class=\"ya-pls\">");
    for (i, (p, on, busy)) in list.iter().enumerate() {
        s.push_str(&format!(
            "<button type=\"button\" id=\"ytc-pl-{i}\" class=\"{}\" role=\"menuitemcheckbox\" aria-checked=\"{on}\" data-act=\"a-pl\" data-arg=\"{i}\">{CK}<span>",
            row_class(*on, *busy)
        ));
        esc_into(s, &p.title);
        s.push_str("</span>");
        if !p.privacy.is_empty() {
            s.push_str("<small>");
            esc_into(s, &p.privacy);
            s.push_str("</small>");
        }
        s.push_str("</button>");
    }
    s.push_str("</div>");
}

/// TV's Save panel: the same rows, no new-playlist form (a name needs a keyboard the remote only has on Search).
pub fn tv_inner(body: Body) -> String {
    let mut s = String::from("<h2>Save to playlist</h2>");
    match body {
        Body::Loading => s.push_str("<p class=\"tvd\" tabindex=\"0\">Loading your playlists…</p>"),
        Body::Failed(r) => {
            s.push_str("<p class=\"tvd\" tabindex=\"0\">");
            esc_into(&mut s, r);
            s.push_str("</p>");
        }
        Body::List(list) => rows_html(&mut s, list),
    }
    s.push_str("<p id=\"ytc-save-m\" class=\"tvam\" role=\"status\"></p>");
    s
}

pub fn row_class(on: bool, busy: bool) -> &'static str {
    match (on, busy) {
        (true, true) => "ya-opt on busy",
        (true, false) => "ya-opt on",
        (false, true) => "ya-opt busy",
        (false, false) => "ya-opt",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_options() {
        let j = crate::json::jv!({"contents": [{"addToPlaylistRenderer": {"playlists": [
            {"playlistAddToOptionRenderer": {"playlistId": "WL", "title": {"simpleText": "Watch later"}, "privacy": "PRIVATE", "containsSelectedVideos": "ALL"}},
            {"playlistAddToOptionRenderer": {"playlistId": "PLabc", "title": {"runs": [{"text": "<Mix>"}]}, "privacy": "PUBLIC", "containsSelectedVideos": "NONE"}},
            {"playlistAddToOptionRenderer": {"playlistId": "bad id!", "containsSelectedVideos": "ALL"}}]}}]});
        let l = parse(&j).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!((l[0].id.as_str(), l[0].contains, l[0].privacy.as_str()), ("WL", true, "Private"));
        assert_eq!((l[1].title.as_str(), l[1].contains), ("<Mix>", false));
        let rows: Vec<_> = l.into_iter().map(|p| (p, false, false)).collect();
        assert!(any_saved(&rows) == false);
        let h = html(true, false, Body::List(&rows), "");
        assert!(h.contains("&lt;Mix&gt;") && h.contains("data-act=\"a-pl\" data-arg=\"1\"") && h.contains("aria-checked=\"false\""));
        assert!(h.contains("id=\"ytc-a-newpl\""));
        // Closed: the chooser element exists (for its exit motion) but is empty and not `open`.
        let closed = html(false, false, Body::Loading, "");
        assert!(closed.contains("<div id=\"ytc-save-p\" class=\"ya-pop\"") && !closed.contains("ya-load") && !closed.contains(" open"));
        assert!(h.contains("class=\"ya-pop open\"") && h.contains("aria-expanded=\"true\""));
        // The bookmark: empty while in no playlist, filled ("saved") and labelled Saved otherwise.
        assert!(h.contains("class=\"ya-b ya-sv open\"") && h.contains(">Save</span>") && h.contains("ya-bm"));
        let rows: Vec<_> = rows.into_iter().enumerate().map(|(i, (p, _, b))| (p, i == 1, b)).collect();
        assert!(any_saved(&rows));
        let s = html(false, any_saved(&rows), Body::List(&rows), "");
        assert!(s.contains("class=\"ya-b ya-sv saved\"") && s.contains(">Saved</span>"));
        assert_eq!(btn_class(true, true, true), "ya-b ya-sv open saved now");
        // TV: the same rows and ids, no form.
        let tv = tv_inner(Body::List(&rows));
        assert!(tv.contains("data-act=\"a-pl\" data-arg=\"1\"") && tv.contains("id=\"ytc-pl-1\"") && tv.contains("&lt;Mix&gt;") && tv.contains("id=\"ytc-save-m\"") && !tv.contains("<form"));
        assert!(tv_inner(Body::Loading).contains("tabindex=\"0\""), "something to focus while it loads");
        assert!(tv_inner(Body::Failed("<no>")).contains("&lt;no&gt;"));
        assert!(tv_inner(Body::List(&[])).contains("No playlists yet."));
    }
    #[test]
    fn view_model_options() {
        let j = crate::json::jv!({"contents": [{"sheet": {"listViewModel": {"listItems": [
            {"listItemViewModel": {"title": {"content": "Roblox"}, "subtitle": {"content": "Private"}, "rendererContext": {"commandContext": {"onTap": {"innertubeCommand": {"playlistEditEndpoint": {"playlistId": "PLr", "actions": [{"action": "ACTION_REMOVE_VIDEO_BY_VIDEO_ID"}]}}}}}}},
            {"listItemViewModel": {"title": {"content": "Later"}, "isSelected": false, "rendererContext": {"commandContext": {"onTap": {"innertubeCommand": {"playlistEditEndpoint": {"playlistId": "WL", "actions": [{"action": "ACTION_ADD_VIDEO"}]}}}}}}},
            {"listItemViewModel": {"title": {"content": "New playlist"}}}]}}}]});
        let l = parse(&j).unwrap();
        assert_eq!(l.iter().map(|p| (p.id.as_str(), p.contains)).collect::<Vec<_>>(), [("PLr", true), ("WL", false)]);
        assert!(parse(&crate::json::jv!({"responseContext": {}})).is_none());
        assert_eq!(parse(&crate::json::jv!({"contents": []})), Some(vec![]));
    }
}
