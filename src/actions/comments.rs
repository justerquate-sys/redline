//! Comments: parsing of `next` comment continuations in both formats YouTube
//! sends (the entity-based `commentViewModel` + `frameworkUpdates` mutations,
//! and the older `commentRenderer`), and their markup. Unknown items are
//! skipped and counted, never fatal.

use super::{arr, at, st, text_at, watch};
use crate::parse::{Run, Stats, attributed, cmd_href, legacy_runs};
use crate::util::{esc_into, safe_img};
use crate::json::Value;
use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub author_href: Option<String>,
    pub avatar: Option<String>,
    pub text: Vec<Run>,
    pub time: String,
    pub likes: String,
    /// ("13 replies", token)
    pub replies: Option<(String, String)>,
    /// "Pinned by @x"
    pub pinned: Option<String>,
    /// Creator heart (tooltip, e.g. "❤ by @x").
    pub heart: Option<String>,
    /// Written by the video's channel.
    pub creator: bool,
    pub verified: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Page {
    pub comments: Vec<Comment>,
    /// Next page (top level) or "Show more replies" (replies).
    pub cont: Option<String>,
    /// From `commentsHeaderRenderer` (first page only).
    pub count: Option<String>,
    pub sorts: Vec<(String, String)>,
    /// A server notice instead of comments (e.g. comments turned off).
    pub notice: Option<String>,
    pub stats: Stats,
}

/// entityKey → payload (the single value inside `payload`).
fn entities<'a>(v: &'a Value<'a>) -> HashMap<&'a str, &'a Value<'a>> {
    let mut m = HashMap::new();
    for mu in arr(v, &["frameworkUpdates", "entityBatchUpdate", "mutations"]) {
        if let (Some(k), Some(Value::Object(p))) = (st(mu, &["entityKey"]), mu.get("payload")) {
            if let Some(x) = p.values().next() {
                m.insert(k, x);
            }
        }
    }
    m
}

/// "2 years ago (edited)" → "2 years ago" (drop only the trailing suffix).
fn time(s: &str) -> String {
    s.trim().strip_suffix("(edited)").unwrap_or(s.trim()).trim_end().to_string()
}

fn replies_of(t: &Value) -> Option<(String, String)> {
    let r = at(t, &["replies", "commentRepliesRenderer"])?;
    let token = arr(r, &["contents"]).iter().find_map(cont_token).or_else(|| {
        st(r, &["viewReplies", "buttonRenderer", "command", "continuationCommand", "token"]).map(String::from)
    })?;
    let label = text_at(r, &["viewReplies", "buttonRenderer", "text"]).or_else(|| text_at(r, &["moreText"])).unwrap_or_else(|| "Replies".into());
    Some((label, token))
}

fn cont_token(x: &Value) -> Option<String> {
    let c = x.get("continuationItemRenderer")?;
    st(c, &["continuationEndpoint", "continuationCommand", "token"])
        .or_else(|| st(c, &["button", "buttonRenderer", "command", "continuationCommand", "token"]))
        .filter(|t| t.len() < 4096)
        .map(String::from)
}

fn view_model(vm: &Value, ents: &HashMap<&str, &Value>, current: &str) -> Option<Comment> {
    let e = ents.get(st(vm, &["commentKey"])?)?;
    let p = e.get("properties")?;
    let author = e.get("author").unwrap_or(&Value::Null);
    let tb = e.get("toolbar").unwrap_or(&Value::Null);
    let state = st(vm, &["toolbarStateKey"]).or_else(|| st(p, &["toolbarStateKey"])).and_then(|k| ents.get(k));
    let liked = state.and_then(|s| st(s, &["likeState"])) == Some("TOOLBAR_LIKE_STATE_LIKED");
    let hearted = state.and_then(|s| st(s, &["heartState"])) == Some("TOOLBAR_HEART_STATE_HEARTED");
    let likes = st(tb, &[if liked { "likeCountLiked" } else { "likeCountNotliked" }]).unwrap_or("").trim().to_string();
    Some(Comment {
        id: st(p, &["commentId"]).unwrap_or("").to_string(),
        author: st(author, &["displayName"]).unwrap_or("").to_string(),
        author_href: at(author, &["channelCommand", "innertubeCommand"]).and_then(cmd_href),
        avatar: st(author, &["avatarThumbnailUrl"]).and_then(safe_img),
        text: p.get("content").map(|c| attributed(c, current)).unwrap_or_default(),
        time: time(st(p, &["publishedTime"]).unwrap_or("")),
        likes: if likes == "0" { String::new() } else { likes },
        replies: None,
        pinned: vm.get("pinnedText").and_then(crate::parse::text),
        heart: hearted.then(|| st(tb, &["heartActiveTooltip"]).unwrap_or("").to_string()),
        creator: author.get("isCreator").and_then(Value::as_bool).unwrap_or(false),
        verified: author.get("isVerified").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn legacy(c: &Value, current: &str) -> Option<Comment> {
    let id = st(c, &["commentId"])?.to_string();
    let heart = at(c, &["actionButtons", "commentActionButtonsRenderer", "creatorHeart", "creatorHeartRenderer"]);
    Some(Comment {
        id,
        author: text_at(c, &["authorText"]).unwrap_or_default(),
        author_href: c.get("authorEndpoint").and_then(cmd_href),
        avatar: arr(c, &["authorThumbnail", "thumbnails"]).iter().find_map(|t| safe_img(t.get("url")?.as_str()?)),
        text: legacy_runs(arr(c, &["contentText", "runs"]), current),
        time: time(&text_at(c, &["publishedTimeText"]).unwrap_or_default()),
        likes: text_at(c, &["voteCount"]).unwrap_or_default(),
        replies: None,
        pinned: text_at(c, &["pinnedCommentBadge", "pinnedCommentBadgeRenderer", "label"]),
        heart: heart.filter(|h| h.get("isHearted").and_then(Value::as_bool) == Some(true)).map(|h| text_at(h, &["heartedTooltip"]).unwrap_or_default()),
        creator: c.get("authorIsChannelOwner").and_then(Value::as_bool).unwrap_or(false),
        verified: c.get("authorCommentBadge").is_some(),
    })
}

/// One comments, replies or sort-change response. `current` is the watched
/// video (timestamps to it become seeks).
pub fn page(v: &Value, current: &str) -> Page {
    let ents = entities(v);
    let mut out = Page::default();
    for key in ["onResponseReceivedEndpoints", "onResponseReceivedActions", "onResponseReceivedCommands"] {
        for action in arr(v, &[key]) {
            for inner in ["reloadContinuationItemsCommand", "appendContinuationItemsAction"] {
                for it in arr(action, &[inner, "continuationItems"]) {
                    item(it, &ents, current, &mut out);
                }
            }
        }
    }
    out
}

fn item(it: &Value, ents: &HashMap<&str, &Value>, current: &str, out: &mut Page) {
    if let Some(h) = it.get("commentsHeaderRenderer") {
        out.count = text_at(h, &["countText"]);
        if let Some(m) = at(h, &["sortMenu", "sortFilterSubMenuRenderer"]) {
            out.sorts = watch::sorts(m);
        }
    } else if let Some(t) = it.get("commentThreadRenderer") {
        let c = at(t, &["commentViewModel", "commentViewModel"])
            .and_then(|vm| view_model(vm, ents, current))
            .or_else(|| at(t, &["comment", "commentRenderer"]).and_then(|c| legacy(c, current)));
        match c {
            Some(mut c) => {
                c.replies = replies_of(t);
                out.comments.push(c);
            }
            None => out.stats.bad += 1,
        }
    } else if let Some(vm) = it.get("commentViewModel") {
        match view_model(vm, ents, current) {
            Some(c) => out.comments.push(c),
            None => out.stats.bad += 1,
        }
    } else if let Some(c) = it.get("commentRenderer") {
        match legacy(c, current) {
            Some(c) => out.comments.push(c),
            None => out.stats.bad += 1,
        }
    } else if it.get("continuationItemRenderer").is_some() {
        out.cont = cont_token(it);
    } else if let Some(m) = it.get("messageRenderer") {
        out.notice = text_at(m, &["text"]);
    } else if let Some(Value::Object(o)) = Some(it) {
        out.stats.bump(o.keys().next().unwrap_or("?"));
    }
}

// ---------------------------------------------------------------- markup

/// One comment. `reply`: index of its replies token in the comments state.
pub fn comment_html(out: &mut String, c: &Comment, v: &str, reply: Option<usize>) {
    out.push_str("<div class=\"ycm");
    if c.pinned.is_some() {
        out.push_str(" pin");
    }
    out.push_str("\">");
    let href = c.author_href.as_deref();
    match (&c.avatar, href) {
        (Some(a), Some(h)) => {
            out.push_str("<a class=\"yc-av\" href=\"");
            esc_into(out, h);
            out.push_str("\" tabindex=\"-1\"><img class=\"av\" src=\"");
            esc_into(out, a);
            out.push_str("\" alt=\"\" loading=\"lazy\" decoding=\"async\"></a>");
        }
        (Some(a), None) => {
            out.push_str("<span class=\"yc-av\"><img class=\"av\" src=\"");
            esc_into(out, a);
            out.push_str("\" alt=\"\" loading=\"lazy\" decoding=\"async\"></span>");
        }
        _ => out.push_str("<span class=\"yc-av\"></span>"),
    }
    out.push_str("<div class=\"ycb\">");
    if let Some(p) = &c.pinned {
        out.push_str("<div class=\"ycp\">");
        esc_into(out, p);
        out.push_str("</div>");
    }
    out.push_str("<div class=\"ych-h\">");
    let cls = if c.creator { "yca own" } else { "yca" };
    match href {
        Some(h) => {
            out.push_str(&format!("<a class=\"{cls}\" href=\""));
            esc_into(out, h);
            out.push_str("\">");
        }
        None => out.push_str(&format!("<span class=\"{cls}\">")),
    }
    esc_into(out, &c.author);
    if c.verified {
        out.push_str(" <i class=\"ycv\" title=\"Verified\">✓</i>");
    }
    out.push_str(if href.is_some() { "</a>" } else { "</span>" });
    out.push_str("<span class=\"yctm\">");
    esc_into(out, &c.time);
    // The text is the remote's reader stop. The surrounding thread contains the author/reply
    // actions and expanded replies; making that entire box focusable traps its contained controls.
    out.push_str(if crate::tv::on() { "</span></div><div class=\"yct\" tabindex=\"0\">" } else { "</span></div><div class=\"yct\">" });
    crate::render::runs(out, &c.text, v);
    out.push_str("</div><div class=\"ycf\"><span class=\"ycl\">");
    out.push_str(watch::I_LIKE);
    esc_into(out, &c.likes);
    out.push_str("</span>");
    if let Some(h) = &c.heart {
        out.push_str("<span class=\"ycht\" title=\"");
        esc_into(out, if h.is_empty() { "Hearted by the creator" } else { h });
        out.push_str("\" aria-label=\"");
        esc_into(out, if h.is_empty() { "Hearted by the creator" } else { h });
        out.push_str("\">♥</span>");
    }
    if let (Some((label, _)), Some(i)) = (&c.replies, reply) {
        out.push_str(&format!("<button type=\"button\" id=\"ytc-rb-{i}\" class=\"ycrb\" data-act=\"a-rep\" data-arg=\"{i}\" aria-expanded=\"false\">"));
        esc_into(out, label);
        out.push_str(&format!("</button></div><div id=\"ytc-rp-{i}\" class=\"ycr\"></div>"));
    } else {
        out.push_str("</div>");
    }
    out.push_str("</div></div>");
}

/// The Top/Newest group (scope of its sliding accent mark, `bridge::mark_slide`).
pub const SORT: &str = "ytc-csort";

/// Element id of the `i`th sort tab (Top, Newest).
pub fn sort_id(i: usize) -> String {
    format!("ytc-cs{i}")
}

/// The comments section frame (header with sort, list, sentinel).
pub fn frame_html(count: &str, sorts: &[(String, String)], sort: usize) -> String {
    let mut s = String::from("<div class=\"yc-h\"><h2>");
    esc_into(&mut s, if count.is_empty() { "Comments" } else { count });
    if !count.is_empty() && count.bytes().all(|b| b.is_ascii_digit() || b == b',' || b == b'.' || b.is_ascii_uppercase()) {
        s.push_str(" Comments");
    }
    s.push_str("</h2>");
    if sorts.len() > 1 {
        s.push_str("<div id=\"ytc-csort\" class=\"yc-sort\" role=\"group\" aria-label=\"Sort comments\">");
        for (i, (label, _)) in sorts.iter().enumerate() {
            s.push_str(&format!("<button type=\"button\" id=\"{}\" data-act=\"a-csort\" data-arg=\"{i}\" aria-pressed=\"{}\"{}>", sort_id(i), i == sort, if i == sort { " class=\"on\"" } else { "" }));
            esc_into(&mut s, label);
            s.push_str("</button>");
        }
        s.push_str("</div>");
    }
    s.push_str("</div><div id=\"");
    s.push_str(super::CM_LIST);
    s.push_str("\"></div><div id=\"");
    s.push_str(super::CM_MORE);
    s.push_str("\" class=\"more\"></div>");
    s
}

/// A notice in place of the list (no End marker).
pub fn notice_html(text: &str, retry: bool) -> String {
    let mut s = String::from("<p class=\"yc-note\">");
    esc_into(&mut s, text);
    if retry {
        s.push_str(" <button type=\"button\" class=\"lnk2\" data-act=\"a-cretry\">Try again</button>");
    }
    s.push_str("</p>");
    s
}

/// A TV reply error keeps its own retry action, including after "Show more replies" was hidden.
pub fn reply_error_html(text: &str, slot: usize) -> String {
    let mut s = String::from("<p class=\"yc-note\">");
    esc_into(&mut s, text);
    s.push_str(&format!(" <button type=\"button\" class=\"ycrb\" data-act=\"a-rep\" data-arg=\"{slot}\">Try again</button></p>"));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::tests::fixture;
    use crate::parse::Link;
    #[test]
    fn tv_comment_is_readable_by_remote_without_changing_desktop_tab_order() {
        let c = Comment { author: "A <viewer>".into(), text: vec![Run { text: "Read this".into(), link: None }], ..Default::default() };
        let mut tv = String::new();
        crate::tv::set_on(true);
        comment_html(&mut tv, &c, "abcdefghijk", None);
        crate::tv::set_on(false);
        let mut desktop = String::new();
        comment_html(&mut desktop, &c, "abcdefghijk", None);
        assert!(tv.starts_with("<div class=\"ycm\">"));
        assert!(tv.contains("<div class=\"yct\" tabindex=\"0\">Read this</div>"));
        assert!(tv.contains("A &lt;viewer&gt;") && desktop.contains("Read this"));
        assert!(!desktop.contains("tabindex=\"0\""));
    }

    #[test]
    fn tv_reply_error_has_an_escaped_message_and_the_original_slot() {
        let h = reply_error_html("Couldn't load <replies> & retry", 17);
        assert!(h.contains("&lt;replies&gt; &amp; retry"));
        assert!(h.contains("data-act=\"a-rep\" data-arg=\"17\">Try again</button>"));
        assert!(!h.contains("data-act=\"a-cretry\""), "reply Retry must not reload the top-level comments");
    }

    #[test]
    fn first_page_entity_format() {
        let p = page(&fixture("comments.json"), "-Bxc4gJluC0");
        assert_eq!(p.comments.len(), 20);
        assert_eq!(p.count.as_deref(), Some("585 Comments"));
        assert_eq!(p.sorts.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["Top", "Newest"]);
        assert!(p.cont.is_some());
        assert_eq!(p.stats.bad + p.stats.unknown_total(), 0);
        let c = &p.comments[0];
        assert_eq!(c.author, "@EdutainmentGames");
        assert_eq!(c.author_href.as_deref(), Some("/@EdutainmentGames"));
        assert!(c.avatar.as_deref().is_some_and(|a| a.starts_with("https://yt3.ggpht.com/")));
        assert_eq!(c.text, vec![Run { text: "my right ear didnt gain this information but my left one did".into(), link: None }]);
        assert_eq!((c.time.as_str(), c.likes.as_str()), ("2 years ago", "760"));
        assert_eq!(c.replies.as_ref().map(|r| r.0.as_str()), Some("13 replies"));
        assert!(!c.creator && c.heart.is_none() && c.pinned.is_none());
        let p2 = page(&fixture("comments2.json"), "-Bxc4gJluC0");
        assert_eq!(p2.comments.len(), 20);
        assert!(p2.cont.is_some() && p2.count.is_none());
    }
    #[test]
    fn replies_page() {
        let p = page(&fixture("replies.json"), "-Bxc4gJluC0");
        assert_eq!(p.comments.len(), 10);
        assert!(p.cont.is_some(), "Show more replies token");
        let m = p.comments.iter().find(|c| c.text.len() > 1).expect("a reply with an @mention");
        assert!(matches!(&m.text[0].link, Some(Link::Int(h)) if h.starts_with("/channel/UC")), "{:?}", m.text);
        assert!(p.comments.iter().all(|c| c.replies.is_none()));
    }
    #[test]
    fn legacy_and_hostile() {
        let evil = "<img src=x onerror=alert(1)>";
        let j = crate::json::jv!({"onResponseReceivedEndpoints": [{"appendContinuationItemsAction": {"continuationItems": [
            {"commentThreadRenderer": {"comment": {"commentRenderer": {"commentId": "c1", "authorText": {"simpleText": evil}, "authorEndpoint": {"browseEndpoint": {"browseId": "UCx", "canonicalBaseUrl": "/@x"}},
                "contentText": {"runs": [{"text": "at "}, {"text": "1:05", "navigationEndpoint": {"watchEndpoint": {"videoId": "abcdefghijk", "startTimeSeconds": 65}}}, {"text": evil}]},
                "publishedTimeText": {"runs": [{"text": "1 day ago (edited)"}]}, "voteCount": {"simpleText": "12"}, "authorIsChannelOwner": true,
                "pinnedCommentBadge": {"pinnedCommentBadgeRenderer": {"label": {"runs": [{"text": "Pinned by "}, {"text": "@x"}]}}},
                "actionButtons": {"commentActionButtonsRenderer": {"creatorHeart": {"creatorHeartRenderer": {"isHearted": true, "heartedTooltip": "❤ by @x"}}}}}},
                "replies": {"commentRepliesRenderer": {"contents": [{"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "R1"}}}}], "viewReplies": {"buttonRenderer": {"text": {"simpleText": "2 replies"}}}}}}},
            {"commentThreadRenderer": {"commentViewModel": {"commentViewModel": {"commentKey": "missing"}}}},
            {"someNewRenderer": {}},
            {"continuationItemRenderer": {"continuationEndpoint": {"continuationCommand": {"token": "NEXT"}}}}
        ]}}]});
        let p = page(&j, "abcdefghijk");
        assert_eq!((p.comments.len(), p.stats.bad, p.stats.unknown_total()), (1, 1, 1));
        assert_eq!(p.cont.as_deref(), Some("NEXT"));
        let c = &p.comments[0];
        assert_eq!(c.text[1].link, Some(Link::Seek(65)));
        assert_eq!((c.time.as_str(), c.likes.as_str(), c.creator), ("1 day ago", "12", true));
        assert_eq!((c.pinned.as_deref(), c.heart.as_deref()), (Some("Pinned by @x"), Some("❤ by @x")));
        assert_eq!(c.replies, Some(("2 replies".into(), "R1".into())));
        let mut h = String::new();
        comment_html(&mut h, c, "abcdefghijk", Some(3));
        assert!(!h.contains("<img src=x") && h.contains("&lt;img src=x"));
        assert!(h.contains("data-act=\"seek\" data-arg=\"65\"") && h.contains("id=\"ytc-rp-3\"") && h.contains("yca own"));
        // Garbage never panics.
        assert!(page(&crate::json::jv!({"onResponseReceivedEndpoints": [1, {"appendContinuationItemsAction": {"continuationItems": [null, 2]}}]}), "x").comments.is_empty());
    }
    #[test]
    fn frame() {
        let f = frame_html("585", &[("Top".into(), "a".into()), ("Newest".into(), "b".into())], 1);
        assert!(f.contains("585 Comments") && f.contains("data-arg=\"1\" aria-pressed=\"true\" class=\"on\">Newest"));
        assert!(frame_html("585 Comments", &[], 0).contains("<h2>585 Comments</h2>"));
    }
}
