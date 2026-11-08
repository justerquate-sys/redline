//! Watch-page action data from the `next` response (like state and counts,
//! subscription, comment tokens, chapters) and the markup of the action row,
//! the subscribe button, the share panel and the chapter list.

use super::{at, find, find_all, st, text_at};
use crate::util::{esc, esc_into, is_safe_id, safe_img};
use crate::json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Like {
    #[default]
    None,
    Like,
    Dislike,
}

impl Like {
    fn from_status(s: &str) -> Option<Like> {
        match s {
            "LIKE" => Some(Like::Like),
            "DISLIKE" => Some(Like::Dislike),
            "INDIFFERENT" => Some(Like::None),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chapter {
    pub title: String,
    pub start: u32,
    pub thumb: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Acts {
    /// Like state; `None` when the page had no like button.
    pub like: Option<Like>,
    /// Count label when not liked / when liked (YouTube's own, e.g. "7.4K").
    pub like_label: String,
    pub liked_label: String,
    pub like_params: Option<String>,
    pub dislike_params: Option<String>,
    pub removelike_params: Option<String>,
    pub channel_id: Option<String>,
    pub channel: String,
    pub subscribed: Option<bool>,
    pub sub_params: Option<String>,
    pub unsub_params: Option<String>,
    /// First comments page (`next` continuation).
    pub comments: Option<String>,
    /// "Comments are turned off." and similar.
    pub comments_off: Option<String>,
    pub comment_count: String,
    /// (label, token) for Top / Newest.
    pub sorts: Vec<(String, String)>,
    pub chapters: Vec<Chapter>,
}

fn token(cmd: &Value) -> Option<String> {
    st(cmd, &["continuationCommand", "token"]).filter(|t| t.len() < 4096).map(String::from)
}

/// `subMenuItems` of a `sortFilterSubMenuRenderer` → (title, token).
pub fn sorts(menu: &Value) -> Vec<(String, String)> {
    super::arr(menu, &["subMenuItems"])
        .iter()
        .filter_map(|i| Some((text_at(i, &["title"])?, token(i.get("serviceEndpoint")?)?)))
        .collect()
}

pub fn parse(v: &Value) -> Acts {
    let mut a = Acts::default();
    let results = super::arr(v, &["contents", "twoColumnWatchNextResults", "results", "results", "contents"]);
    for c in results {
        if let Some(p) = c.get("videoPrimaryInfoRenderer") {
            if let Some(seg) = find(p, "segmentedLikeDislikeButtonViewModel") {
                a.like = find(seg, "likeStatusEntity").and_then(|e| st(e, &["likeStatus"])).and_then(Like::from_status).or(Some(Like::None));
                let tb = |which: &str| at(seg, &["likeButtonViewModel", "likeButtonViewModel", "toggleButtonViewModel", "toggleButtonViewModel", which, "buttonViewModel", "title"]).and_then(Value::as_str).unwrap_or("").to_string();
                a.like_label = tb("defaultButtonViewModel");
                a.liked_label = tb("toggledButtonViewModel");
                for le in find_all(seg, "likeEndpoint") {
                    let (key, slot) = match st(le, &["status"]) {
                        Some("LIKE") => ("likeParams", &mut a.like_params),
                        Some("DISLIKE") => ("dislikeParams", &mut a.dislike_params),
                        Some("INDIFFERENT") => ("removeLikeParams", &mut a.removelike_params),
                        _ => continue,
                    };
                    if slot.is_none() {
                        *slot = st(le, &[key]).map(String::from);
                    }
                }
            }
        } else if let Some(s) = c.get("videoSecondaryInfoRenderer") {
            let owner = at(s, &["owner", "videoOwnerRenderer"]).unwrap_or(&Value::Null);
            a.channel = text_at(owner, &["title"]).unwrap_or_default();
            if let Some(sb) = at(s, &["subscribeButton", "subscribeButtonRenderer"]) {
                a.channel_id = st(sb, &["channelId"]).map(String::from);
                a.subscribed = sb.get("subscribed").and_then(Value::as_bool);
            }
            if let Some(vm) = find(s, "subscribeButtonViewModel") {
                if a.channel_id.is_none() {
                    a.channel_id = st(vm, &["channelId"]).map(String::from);
                }
            }
            if a.channel_id.is_none() {
                a.channel_id = find(owner, "browseEndpoint").and_then(|b| st(b, &["browseId"])).map(String::from);
            }
            a.sub_params = find(s, "subscribeEndpoint").and_then(|e| st(e, &["params"])).map(String::from);
            a.unsub_params = find(s, "unsubscribeEndpoint").and_then(|e| st(e, &["params"])).map(String::from);
        } else if let Some(is) = c.get("itemSectionRenderer") {
            let id = st(is, &["sectionIdentifier"]).unwrap_or("");
            let target = st(is, &["targetId"]).unwrap_or("");
            if id == "comment-item-section" || target == "comments-section" {
                for x in super::arr(is, &["contents"]) {
                    if let Some(t) = x.get("continuationItemRenderer").and_then(|r| r.get("continuationEndpoint")).and_then(token) {
                        a.comments = Some(t);
                    } else if let Some(m) = x.get("messageRenderer") {
                        a.comments_off = text_at(m, &["text"]);
                    }
                }
            }
        }
    }
    a.channel_id = a.channel_id.filter(|c| c.starts_with("UC") && is_safe_id(c));
    // Like/subscription entities (newer pages carry state only here).
    super::walk(at(v, &["frameworkUpdates"]).unwrap_or(&Value::Null), &mut |k, e| {
        if k == "subscriptionStateEntity" && a.subscribed.is_none() {
            a.subscribed = e.get("subscribed").and_then(Value::as_bool);
        }
    });
    for p in super::arr(v, &["engagementPanels"]) {
        let p = p.get("engagementPanelSectionListRenderer").unwrap_or(&Value::Null);
        match st(p, &["panelIdentifier"]).unwrap_or("") {
            "engagement-panel-comments-section" => {
                let h = at(p, &["header", "engagementPanelTitleHeaderRenderer"]).unwrap_or(&Value::Null);
                if let Some(n) = text_at(h, &["contextualInfo"]) {
                    a.comment_count = n;
                }
                if let Some(m) = at(h, &["menu", "sortFilterSubMenuRenderer"]) {
                    a.sorts = sorts(m);
                }
                if a.comments.is_none() {
                    a.comments = find(at(p, &["content"]).unwrap_or(&Value::Null), "continuationEndpoint").and_then(token);
                }
            }
            "engagement-panel-macro-markers-description-chapters" if a.chapters.is_empty() => {
                for it in find_all(p, "macroMarkersListItemRenderer") {
                    let Some(title) = text_at(it, &["title"]) else { continue };
                    let start = at(it, &["onTap", "watchEndpoint", "startTimeSeconds"]).and_then(Value::as_u64).unwrap_or(0) as u32;
                    a.chapters.push(Chapter { title, start, thumb: thumb(it) });
                }
            }
            _ => {}
        }
    }
    // The player bar's chapters are the canonical list (also covers auto chapters).
    if let Some(mm) = at(v, &["playerOverlays"]).and_then(|o| find(o, "multiMarkersPlayerBarRenderer")) {
        let mut list = Vec::new();
        for m in super::arr(mm, &["markersMap"]) {
            if !matches!(st(m, &["key"]), Some("DESCRIPTION_CHAPTERS") | Some("AUTO_CHAPTERS")) {
                continue;
            }
            for ch in super::arr(m, &["value", "chapters"]) {
                let Some(r) = ch.get("chapterRenderer") else { continue };
                let Some(title) = text_at(r, &["title"]) else { continue };
                let start = (at(r, &["timeRangeStartMillis"]).and_then(Value::as_u64).unwrap_or(0) / 1000) as u32;
                list.push(Chapter { title, start, thumb: thumb(r) });
            }
            break;
        }
        if !list.is_empty() {
            a.chapters = list;
        }
    }
    a.chapters.sort_by_key(|c| c.start);
    a.chapters.dedup_by_key(|c| c.start);
    a
}

/// TV: the rating and subscription an answer of the account's own client states, wherever it states them (`None`: it
/// does not say). The watch and channel pages themselves are anonymous WEB calls on TV, so they cannot know these.
/// Read loosely on purpose: the TV client's shapes are not those of WEB, and only a real sign-in shows them; a shape
/// not recognised changes nothing.
pub fn account_state(v: &Value, ch: Option<&str>) -> (Option<Like>, Option<bool>) {
    let (mut like, mut sub) = (None, None);
    super::walk(v, &mut |k, x| match k {
        "likeStatus" if like.is_none() => like = x.as_str().and_then(Like::from_status),
        // A toggle button's own state, on the endpoint it would send (LIKE or DISLIKE).
        "toggleButtonRenderer" if like.is_none() && x.get("isToggled").and_then(Value::as_bool) == Some(true) => {
            like = find_all(x, "likeEndpoint").iter().find_map(|e| st(e, &["status"])).and_then(Like::from_status).filter(|l| *l != Like::None);
        }
        "subscribeButtonRenderer" | "subscribeButtonViewModel" | "subscriptionButton" | "subscriptionStateEntity" if sub.is_none() => {
            let other = st(x, &["channelId"]).is_some_and(|id| ch.is_some_and(|c| c != id));
            if !other {
                sub = x.get("subscribed").and_then(Value::as_bool);
            }
        }
        _ => {}
    });
    (like, sub)
}

// ------------------------------------------------- description timecodes

/// Separators between a timestamp and its title ("0:00 - Intro", "Intro | 0:00").
fn sep(c: char) -> bool {
    c.is_whitespace() || matches!(c, '-' | '–' | '—' | ':' | '|' | '•' | '·' | '.' | ',' | ';' | ')' | ']' | '(' | '[' | '>' | '»' | '→' | '▶' | '►' | '*' | '~' | '=')
}

/// A list number at the start of a line ("1. ", "12) "), then separators.
fn list_number(s: &str) -> &str {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    let rest = &s[digits..];
    match rest.chars().next() {
        Some('.' | ')') if digits > 0 && digits <= 3 && rest[1..].starts_with(char::is_whitespace) => rest[1..].trim_start_matches(sep),
        _ => s,
    }
}

/// `m:ss`, `mm:ss`, `mmm:ss`, `h:mm:ss`, `hh:mm:ss` → seconds.
pub fn timestamp(tok: &str) -> Option<u32> {
    let p: Vec<&str> = tok.split(':').collect();
    if !(2..=3).contains(&p.len()) || p.iter().any(|x| x.is_empty() || !x.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let n = |x: &str| x.parse::<u32>().ok();
    let (s, sl) = (n(p[p.len() - 1])?, p[p.len() - 1].len());
    if sl != 2 || s >= 60 {
        return None;
    }
    if p.len() == 3 {
        let (h, m) = (n(p[0])?, n(p[1])?);
        (p[0].len() <= 2 && p[1].len() == 2 && m < 60).then_some(h * 3600 + m * 60 + s)
    } else {
        (p[0].len() <= 3).then_some(n(p[0])? * 60 + s)
    }
}

/// The leading `[0-9:]` token of `s` and the rest.
fn lead_token(s: &str) -> (&str, &str) {
    let mut i = s.find(|c: char| !(c.is_ascii_digit() || c == ':')).unwrap_or(s.len());
    // "0:00: Intro": the trailing colon is a separator.
    if s[..i].ends_with(':') {
        i -= 1;
    }
    (&s[..i], &s[i..])
}

fn title_of(s: &str) -> Option<String> {
    let t: String = s.trim().chars().take(200).collect();
    (t.chars().any(char::is_alphanumeric)).then_some(t)
}

/// One description line as a chapter candidate: a timestamp at the start of
/// the line (after an optional bullet, dash, bracket or list number) followed
/// by the title ("0:00 Intro", "0:00 - Intro", "1. 0:00 Intro",
/// "[00:00] Intro", "0:00 - 1:30 Intro" ranges), or the title followed by a
/// timestamp that ends the line ("Intro 0:00", "Intro (0:00)").
pub fn line_chapter(line: &str) -> Option<(u32, String)> {
    let l = line.trim();
    let head = list_number(l.trim_start_matches(|c: char| !c.is_alphanumeric()));
    let (tok, rest) = lead_token(head);
    if let Some(t) = timestamp(tok).filter(|_| !rest.starts_with(char::is_alphanumeric)) {
        let mut rest = rest.trim_start_matches(sep);
        // A range ("0:00 - 1:30 Intro"): the second timestamp is not part of the title.
        let (tok2, after) = lead_token(rest);
        if timestamp(tok2).is_some() && !after.starts_with(char::is_alphanumeric) {
            rest = after.trim_start_matches(sep);
        }
        return title_of(rest).map(|title| (t, title));
    }
    // Title first: the line ends with the timestamp (optionally in brackets).
    let body = l.trim_end_matches(|c: char| c.is_whitespace() || c == ')' || c == ']');
    let i = body.rfind(|c: char| !(c.is_ascii_digit() || c == ':')).map_or(0, |i| i + body[i..].chars().next().map_or(1, char::len_utf8));
    let (front, tok) = body.split_at(i);
    let t = timestamp(tok)?;
    if !front.ends_with(|c: char| c.is_whitespace() || c == '(' || c == '[') {
        return None;
    }
    let front = front.trim_end_matches(sep);
    let front = list_number(front.trim_start_matches(|c: char| sep(c) && c != '(' && c != '['));
    title_of(front).map(|title| (t, title))
}

/// Chapters from a video description by YouTube's rules: the list starts at
/// the first line timestamped 0:00 and runs while the timestamps ascend; it
/// needs at least three chapters, each at least 10 seconds long. Anything
/// else gives no chapters.
pub fn desc_chapters(text: &str) -> Vec<Chapter> {
    let cands: Vec<(u32, String)> = text.lines().filter_map(line_chapter).collect();
    let Some(first) = cands.iter().position(|c| c.0 == 0) else { return Vec::new() };
    let mut out: Vec<Chapter> = Vec::new();
    for (start, title) in cands.into_iter().skip(first) {
        if out.last().is_some_and(|l| start <= l.start) {
            break;
        }
        out.push(Chapter { title, start, thumb: None });
    }
    if out.len() < 3 || out.windows(2).any(|w| w[1].start - w[0].start < 10) {
        return Vec::new();
    }
    out
}

fn thumb(x: &Value) -> Option<String> {
    super::arr(x, &["thumbnail", "thumbnails"]).iter().find_map(|t| safe_img(t.get("url")?.as_str()?))
}

pub fn clock(t: u32) -> String {
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

// ---------------------------------------------------------------- markup

pub const I_LIKE: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M7 10v10H3V10zm2 10h8.2a2 2 0 0 0 2-1.6l1.4-6.6A2 2 0 0 0 18.6 9.5H14V5.2A2.2 2.2 0 0 0 11.8 3L9 9.6z\"/></svg>";
pub const I_SHARE: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M14 5l7 7-7 7M20.5 12H10a6 6 0 0 0-6 6v1\"/></svg>";

pub const SUB_B: &str = "ytc-sub-b";

/// Subscribe button state: (class, label, aria-label). `confirm`: the next click unsubscribes.
pub fn sub_view(shown: bool, busy: bool, confirm: bool, channel: &str) -> (String, &'static str, String) {
    let (cls, label) = match (shown, confirm) {
        (true, true) => ("ya-sub on confirm", "Unsubscribe?"),
        (true, false) => ("ya-sub on", "Subscribed"),
        _ => ("ya-sub", "Subscribe"),
    };
    let aria = if shown { format!("Unsubscribe from {channel}") } else { format!("Subscribe to {channel}") };
    (format!("{cls}{}", if busy { " busy" } else { "" }), label, aria)
}

/// Subscribe button (later state changes flip its attributes in place).
pub fn sub_html(shown: bool, busy: bool, confirm: bool, channel: &str) -> String {
    let (cls, label, aria) = sub_view(shown, busy, confirm, channel);
    format!(
        "<button type=\"button\" id=\"{SUB_B}\" class=\"{cls}\" data-act=\"a-sub\" aria-pressed=\"{shown}\" aria-label=\"{aria}\">{label}</button>",
        aria = esc(&aria)
    )
}

/// Like/dislike group (`None`: the page had no like button), then the share
/// and save slots and the status line.
pub fn row_html(like: Option<(Like, &str)>, share: &str, save: &str) -> String {
    let mut s = String::with_capacity(2048);
    if let Some((like, count)) = like {
        s.push_str(&format!(
            "<div class=\"ya-ld\" role=\"group\" aria-label=\"Rate this video\"><button type=\"button\" id=\"ytc-like\" class=\"{}\" data-act=\"a-like\" aria-pressed=\"{}\" aria-label=\"I like this\">{I_LIKE}<span id=\"ytc-like-n\">",
            like_class(like == Like::Like, false),
            like == Like::Like
        ));
        esc_into(&mut s, count_text(count));
        s.push_str(&format!(
            "</span></button><button type=\"button\" id=\"ytc-dis\" class=\"{}\" data-act=\"a-dislike\" aria-pressed=\"{}\" aria-label=\"I dislike this\">{I_LIKE}</button></div>",
            like_class(like == Like::Dislike, true),
            like == Like::Dislike
        ));
    }
    s.push_str("<div id=\"ytc-share\" class=\"ya-slot\">");
    s.push_str(share);
    s.push_str("</div><div id=\"ytc-save\" class=\"ya-slot\">");
    s.push_str(save);
    s.push_str("</div><span id=\"");
    s.push_str(super::MSG);
    s.push_str("\" class=\"ya-msg\" role=\"status\"></span>");
    s
}

/// A person with a plus, and with a check mark once subscribed (TV's player row: both drawn, `.on` picks one).
const I_SUB: &str = "<svg class=\"o tvs0\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M10 11.5a3.75 3.75 0 1 0 0-7.5 3.75 3.75 0 0 0 0 7.5zM3 20c0-3.6 3-6 7-6s7 2.4 7 6M19 7.5v6M16 10.5h6\"/></svg><svg class=\"o tvs1\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M10 11.5a3.75 3.75 0 1 0 0-7.5 3.75 3.75 0 0 0 0 7.5zM3 20c0-3.6 3-6 7-6s7 2.4 7 6M15.5 10.5l2.5 2.5 4.5-5\"/></svg>";

/// TV's player row: the Subscribe action's class and focused label.
pub fn tv_row_sub_view(shown: bool, busy: bool, confirm: bool) -> (String, &'static str) {
    let (_, label, _) = sub_view(shown, busy, confirm, "");
    (format!("pb tvsb{}{}{}", if shown { " on" } else { "" }, if shown && confirm { " confirm" } else { "" }, if busy { " busy" } else { "" }), label)
}

/// TV's player row: the Subscribe action (later state changes flip its attributes in place).
pub fn tv_row_sub(id: &str, label_id: &str, shown: bool, busy: bool, confirm: bool, channel: &str) -> String {
    let (cls, label) = tv_row_sub_view(shown, busy, confirm);
    let (_, _, aria) = sub_view(shown, busy, confirm, channel);
    format!("<button type=\"button\" id=\"{id}\" class=\"{cls}\" data-act=\"a-sub\" aria-pressed=\"{shown}\" aria-label=\"{}\">{I_SUB}<span id=\"{label_id}\" class=\"tvpl\" aria-hidden=\"true\">{label}</span></button>", esc(&aria))
}

pub fn like_class(on: bool, dis: bool) -> &'static str {
    match (on, dis) {
        (true, true) => "ya-b dis on",
        (false, true) => "ya-b dis",
        (true, false) => "ya-b on",
        (false, false) => "ya-b",
    }
}

/// YouTube's count label, or nothing when it is just the word "Like".
pub fn count_text(c: &str) -> &str {
    if c.bytes().any(|b| b.is_ascii_digit()) { c } else { "" }
}

pub const SHARE_P: &str = "ytc-share-p";
pub const SHARE_B: &str = "ytc-share-b";

/// The share button with its panel (always present; content only while open).
pub fn share_html(open: bool, url: &str, at_time: Option<u32>, with_time: bool) -> String {
    let mut s = format!(
        "<div class=\"ya-pw\" data-pop=\"share\"><button type=\"button\" id=\"{SHARE_B}\" class=\"ya-b\" data-act=\"a-share\" aria-expanded=\"{open}\" aria-haspopup=\"dialog\">{I_SHARE}<span>Share</span></button><div id=\"{SHARE_P}\" class=\"{}\" role=\"dialog\" aria-label=\"Share\">",
        super::notify::pop_class("ya-pop", open, false)
    );
    if open {
        s.push_str(&share_inner(url, at_time, with_time));
    }
    s.push_str("</div></div>");
    s
}

/// The share panel's content.
pub fn share_inner(url: &str, at_time: Option<u32>, with_time: bool) -> String {
    let mut s = String::from("<div class=\"ya-sh\"><input id=\"ytc-share-u\" readonly value=\"");
    esc_into(&mut s, url);
    s.push_str("\" aria-label=\"Link\"><button type=\"button\" id=\"ytc-copy\" class=\"ya-cp\" data-act=\"a-copy\" aria-label=\"Copy link\"><span class=\"cp0\">Copy</span><span class=\"cp1\" aria-hidden=\"true\"><svg viewBox=\"0 0 12 12\" aria-hidden=\"true\"><path pathLength=\"1\" d=\"M2.4 6.4 4.8 8.8 9.6 3.4\"/></svg><b id=\"ytc-copied\">Copied</b></span></button></div>");
    if let Some(t) = at_time.filter(|t| *t > 0) {
        s.push_str(&format!(
            "<button type=\"button\" id=\"ytc-share-t\" class=\"ya-opt{}\" role=\"menuitemcheckbox\" aria-checked=\"{with_time}\" data-act=\"a-share-t\">{}Start at {}</button>",
            if with_time { " on" } else { "" },
            super::save::CK,
            clock(t)
        ));
    }
    s
}

pub fn share_url(v: &str, t: Option<u32>) -> String {
    match t.filter(|t| *t > 0) {
        Some(t) => format!("https://youtu.be/{v}?t={t}"),
        None => format!("https://youtu.be/{v}"),
    }
}

pub use crate::player::ui::{CHD, CHF, CHH, CHHC, CHHF, CHHN, CHHT, CHHX, CHI, CHOL};

/// Down chevron (rotates up when the panel is open).
const CHEVRON: &str = "<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"m6 9 6 6 6-6\"/></svg>";

/// The chapter panel under the description: it rests compact, one row
/// showing the chapter playing now (its frame, title, start, "Chapter 3 of
/// 30"), and the full list unfolds over the content below on hover, keyboard
/// focus or a click (the player drives `.open`, client.css `.ych`). Frames are
/// empty boxes until the player writes the storyboard cells (or YouTube's
/// thumbnails when there is none) into `#ytc-chf{i}` / `#ytc-chhf`. A row
/// click seeks our player.
pub fn chapters_html(v: &str, chapters: &[Chapter]) -> String {
    let Some(first) = chapters.first() else { return String::new() };
    let n = chapters.len();
    let mut s = String::with_capacity(400 + n * 260);
    s.push_str(&format!(
        "<section id=\"{CHD}\" class=\"ych\" data-pop=\"chaps\" data-focus=\"chaps\" aria-label=\"Chapters\"><div class=\"ychs\"><div class=\"ychc\" data-track=\"chaps\">\
<button id=\"{CHH}\" class=\"ychh\" type=\"button\" data-act=\"p-chaps\" aria-expanded=\"false\" aria-controls=\"{CHOL}\" aria-label=\"Chapters: now 1 of {n}, "
    ));
    esc_into(&mut s, &first.title);
    s.push_str(&format!("\"><i class=\"sbf\"><i id=\"{CHHF}\"></i></i><span id=\"{CHHX}\" class=\"ychx\"><span id=\"{CHHT}\" class=\"ychn\">"));
    esc_into(&mut s, &first.title);
    s.push_str(&format!(
        "</span><span class=\"ychm\"><b id=\"{CHHC}\">{}</b><span id=\"{CHHN}\">Chapter 1 of {n}</span></span></span><i class=\"ychv\">{CHEVRON}</i></button><ol id=\"{CHOL}\">",
        clock(first.start)
    ));
    for (i, c) in chapters.iter().enumerate() {
        s.push_str(&format!(
            "<li id=\"{CHI}{i}\"><a href=\"/watch?v={v}&amp;t={t}s\" data-act=\"seek\" data-arg=\"{t}\"><i class=\"sbf\"><i id=\"{CHF}{i}\"></i></i><b>{}</b><span>",
            clock(c.start),
            t = c.start
        ));
        esc_into(&mut s, &c.title);
        s.push_str("</span></a></li>");
    }
    s.push_str("</ol></div></div></section>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::tests::fixture;
    #[test]
    fn signed_out_next_page() {
        let a = parse(&fixture("next.json"));
        assert_eq!(a.like, Some(Like::None));
        assert_eq!((a.like_label.as_str(), a.liked_label.as_str()), ("7.4K", "7.4K"));
        assert!(a.like_params.as_deref().is_some_and(|p| p.starts_with("Cg0KCy1CeGM0")), "{:?}", a.like_params);
        assert!(a.removelike_params.is_some());
        assert_eq!(a.channel_id.as_deref(), Some("UCaZO-66UcqyVT9btiqdDryA"));
        assert_eq!(a.channel, "MonkeyDev");
        assert_eq!(a.subscribed, Some(false));
        assert!(a.comments.as_deref().is_some_and(|t| t.starts_with("Eg0SCy1CeGM0")));
        assert_eq!(a.comment_count, "585");
        assert_eq!(a.sorts.iter().map(|s| s.0.as_str()).collect::<Vec<_>>(), ["Top", "Newest"]);
        assert!(a.chapters.is_empty());
    }
    #[test]
    fn chapters() {
        let a = parse(&fixture("chapters_next.json"));
        assert_eq!(a.chapters.len(), 30);
        assert_eq!(a.chapters[0].start, 0);
        assert_eq!(a.chapters[1].title, "reading and exploring the data");
        assert_eq!(a.chapters[1].start, 472);
        assert!(a.chapters[1].thumb.as_deref().is_some_and(|t| t.starts_with("https://i.ytimg.com/")));
        let h = chapters_html("kCc8FmEb1nY", &a.chapters);
        assert!(h.contains("data-act=\"seek\" data-arg=\"472\"") && h.contains("<b>7:52</b>"));
        assert!(h.contains("<section id=\"ytc-chd\" class=\"ych\"") && h.contains("<ol id=\"ytc-chol\">") && h.contains("<li id=\"ytc-chi1\">"));
        // Compact row: the first chapter until the player knows the current one; frames are empty boxes
        // (no cover image repeated per row) until the player writes the storyboard cells.
        assert!(h.contains("<button id=\"ytc-chh\" class=\"ychh\" type=\"button\" data-act=\"p-chaps\" aria-expanded=\"false\""), "{h}");
        assert!(h.contains("Chapter 1 of 30") && h.contains("data-track=\"chaps\"") && h.contains("data-focus=\"chaps\""));
        assert!(h.contains("<i class=\"sbf\"><i id=\"ytc-chf29\"></i></i>") && !h.contains("<img"));
        // Titles are escaped (attribute and text).
        let evil = [Chapter { title: "<b>\"x\"&".into(), start: 0, thumb: None }];
        let h = chapters_html("kCc8FmEb1nY", &evil);
        assert!(!h.contains("<b>\"x\"") && h.contains("&lt;b&gt;"), "{h}");
        assert_eq!(chapters_html("kCc8FmEb1nY", &[]), "");
        // Only the engagement panel present → still found.
        let mut j = fixture("chapters_next.json").to_serde();
        j.as_object_mut().unwrap().remove("playerOverlays").unwrap();
        let j = crate::json::Value::from_serde(&j);
        assert_eq!(parse(&j).chapters.len(), 30);
    }
    fn starts(text: &str) -> Vec<(u32, String)> {
        desc_chapters(text).into_iter().map(|c| (c.start, c.title)).collect()
    }

    #[test]
    fn timestamps() {
        assert_eq!(timestamp("0:00"), Some(0));
        assert_eq!(timestamp("00:00"), Some(0));
        assert_eq!(timestamp("4:05"), Some(245));
        assert_eq!(timestamp("90:00"), Some(5400), "minutes past the hour without hours");
        assert_eq!(timestamp("1:02:03"), Some(3723));
        assert_eq!(timestamp("01:02:03"), Some(3723));
        for bad in ["0:0", "0:60", "1:60:00", "1:2:03", "123:4:05", ":30", "1:", "1:30:", "1234:00", "a:00", "0"] {
            assert_eq!(timestamp(bad), None, "{bad}");
        }
    }

    #[test]
    fn description_line_formats() {
        let one = |l: &str| line_chapter(l).map(|(t, s)| (t, s));
        assert_eq!(one("00:00 Intro"), Some((0, "Intro".into())));
        assert_eq!(one("0:00 - Intro"), Some((0, "Intro".into())));
        assert_eq!(one("0:00 – Intro"), Some((0, "Intro".into())));
        assert_eq!(one("0:00: Intro"), Some((0, "Intro".into())));
        assert_eq!(one("Intro 0:00"), Some((0, "Intro".into())));
        assert_eq!(one("Intro - 0:00"), Some((0, "Intro".into())));
        assert_eq!(one("Intro (0:00)"), Some((0, "Intro".into())));
        assert_eq!(one("1. 0:00 Intro"), Some((0, "Intro".into())));
        assert_eq!(one("12) 1:02:03 The end"), Some((3723, "The end".into())));
        assert_eq!(one("1. Intro 0:00"), Some((0, "Intro".into())));
        assert_eq!(one("- 0:00 Intro"), Some((0, "Intro".into())));
        assert_eq!(one("• 0:00 Intro"), Some((0, "Intro".into())));
        assert_eq!(one("▶ 0:00 Intro"), Some((0, "Intro".into())));
        assert_eq!(one("[00:00] Intro"), Some((0, "Intro".into())));
        assert_eq!(one("(0:00) Intro"), Some((0, "Intro".into())));
        assert_eq!(one("  0:00\tIntro  "), Some((0, "Intro".into())));
        assert_eq!(one("0:00 - 1:30 Intro"), Some((0, "Intro".into())), "range");
        assert_eq!(one("10:00 Part 2: the 3 rules"), Some((600, "Part 2: the 3 rules".into())));
        assert_eq!(one("0:00 Intro https://example.com/x?t=1:00"), Some((0, "Intro https://example.com/x?t=1:00".into())));
        assert_eq!(one("0:00 Привет мир"), Some((0, "Привет мир".into())));
        assert_eq!(one("🎵 Song name 3:45"), Some((225, "🎵 Song name".into())));
        // Not chapters.
        for l in ["0:00", "0:00 -", "2:30pm meeting", "Watch https://youtu.be/x?t=1:00", "ratio 16:9 screens", "Call 555-0100", "", "Intro0:00"] {
            assert_eq!(line_chapter(l), None, "{l:?}");
        }
    }

    #[test]
    fn description_chapters_follow_youtubes_rules() {
        let ok = "My video!\nLinks: https://example.com\n\n00:00 Intro\n0:42 - Setup\n1. 2:05 Build https://x.com/a\n1:02:03 The end\n\nThanks for watching";
        assert_eq!(starts(ok), vec![(0, "Intro".into()), (42, "Setup".into()), (125, "Build https://x.com/a".into()), (3723, "The end".into())]);
        // Title first, CRLF lines.
        assert_eq!(starts("Intro 0:00\r\nMiddle 0:30\r\nEnd 1:00\r\n").len(), 3);
        // Fewer than three.
        assert!(starts("0:00 Intro\n0:30 End").is_empty());
        // The first must be 0:00.
        assert!(starts("0:05 Intro\n0:30 Middle\n1:00 End").is_empty());
        // Each chapter at least 10 s.
        assert!(starts("0:00 Intro\n0:09 Middle\n1:00 End").is_empty());
        assert_eq!(starts("0:00 Intro\n0:10 Middle\n1:00 End").len(), 3);
        // Earlier timestamps (not 0:00) before the list are ignored; the list stops when the order breaks.
        let t = "Best part 5:00\n0:00 Intro\n0:30 Middle\n1:00 End\nCredits:\nSong A 0:10";
        assert_eq!(starts(t).iter().map(|c| c.0).collect::<Vec<_>>(), vec![0, 30, 60]);
        // Not ascending inside the first three.
        assert!(starts("0:00 Intro\n1:00 B\n0:30 C").is_empty());
        // Timestamps mid-sentence are not chapters.
        assert!(starts("At 0:00 we start, at 0:30 we build and at 1:00 we finish.").is_empty());
    }

    #[test]
    fn signed_in_shapes() {
        // Signed in: likeEndpoint directly on the button; newer subscribe view model + entity.
        let j = crate::json::jv!({"contents": {"twoColumnWatchNextResults": {"results": {"results": {"contents": [
            {"videoPrimaryInfoRenderer": {"videoActions": {"menuRenderer": {"topLevelButtons": [{"segmentedLikeDislikeButtonViewModel": {
                "likeButtonViewModel": {"likeButtonViewModel": {"likeStatusEntity": {"likeStatus": "LIKE"}, "toggleButtonViewModel": {"toggleButtonViewModel": {
                    "defaultButtonViewModel": {"buttonViewModel": {"title": "10", "onTap": {"innertubeCommand": {"likeEndpoint": {"status": "LIKE", "target": {"videoId": "v"}, "likeParams": "LP"}}}}},
                    "toggledButtonViewModel": {"buttonViewModel": {"title": "11"}}}}}}}}]}}}},
            {"videoSecondaryInfoRenderer": {"owner": {"videoOwnerRenderer": {"title": {"runs": [{"text": "Ch"}]}, "navigationEndpoint": {"browseEndpoint": {"browseId": "UCabc"}}}},
                "subscribeButton": {"subscribeButtonViewModel": {"channelId": "UCabc", "onSubscribe": {"subscribeEndpoint": {"channelIds": ["UCabc"], "params": "SP"}}}}}},
            {"itemSectionRenderer": {"sectionIdentifier": "comment-item-section", "contents": [{"messageRenderer": {"text": {"runs": [{"text": "Comments are turned off. "}, {"text": "Learn more"}]}}}]}}
        ]}}}}, "frameworkUpdates": {"entityBatchUpdate": {"mutations": [{"payload": {"subscriptionStateEntity": {"subscribed": true}}}]}}});
        let a = parse(&j);
        assert_eq!(a.like, Some(Like::Like));
        assert_eq!((a.like_label.as_str(), a.liked_label.as_str(), a.like_params.as_deref()), ("10", "11", Some("LP")));
        assert_eq!((a.channel_id.as_deref(), a.subscribed, a.sub_params.as_deref()), (Some("UCabc"), Some(true), Some("SP")));
        assert_eq!(a.comments_off.as_deref(), Some("Comments are turned off. Learn more"));
        assert!(a.comments.is_none());
        // Garbage never panics.
        assert_eq!(parse(&crate::json::jv!([1, {"contents": null}])), Acts::default());
    }
    #[test]
    fn the_accounts_own_answer_states_rating_and_subscription() {
        let ch = Some("UCabc");
        // WEB's shapes (the fixture: signed out, not subscribed, no rating).
        assert_eq!(account_state(&fixture("next.json"), Some("UCaZO-66UcqyVT9btiqdDryA")), (Some(Like::None), Some(false)));
        // The shapes seen from TV clients, wherever they sit.
        let j = crate::json::jv!({"contents": {"singleColumnWatchNextResults": {"results": [{"videoOwnerRenderer": {"subscriptionButton": {"subscribed": true}}}]}}, "transportControls": {"promotedActions": [
            {"button": {"toggleButtonRenderer": {"isToggled": false, "defaultServiceEndpoint": {"likeEndpoint": {"status": "LIKE"}}}}},
            {"button": {"toggleButtonRenderer": {"isToggled": true, "defaultServiceEndpoint": {"likeEndpoint": {"status": "DISLIKE"}}, "toggledServiceEndpoint": {"likeEndpoint": {"status": "INDIFFERENT"}}}}}]}});
        assert_eq!(account_state(&j, ch), (Some(Like::Dislike), Some(true)));
        let j = crate::json::jv!({"a": [{"subscribeButtonRenderer": {"channelId": "UCother", "subscribed": true}}, {"subscribeButtonRenderer": {"channelId": "UCabc", "subscribed": false}}, {"likeStatus": "LIKE"}]});
        assert_eq!(account_state(&j, ch), (Some(Like::Like), Some(false)), "another channel's button is not this one's");
        // Nothing stated, nothing known.
        assert_eq!(account_state(&crate::json::jv!({"responseContext": {}, "likeStatus": "UNKNOWN", "toggleButtonRenderer": {"isToggled": false}}), ch), (None, None));
    }
    #[test]
    fn tv_row_subscribe() {
        let h = tv_row_sub("b", "l", true, false, true, "a\"b");
        assert!(h.contains("class=\"pb tvsb on confirm\"") && h.contains("data-act=\"a-sub\"") && h.contains(">Unsubscribe?</span>") && h.contains("a&quot;b"), "{h}");
        assert_eq!(tv_row_sub_view(false, true, false), ("pb tvsb busy".to_string(), "Subscribe"));
        assert_eq!(tv_row_sub_view(true, false, false), ("pb tvsb on".to_string(), "Subscribed"));
    }
    #[test]
    fn markup() {
        let h = row_html(Some((Like::Like, "<b>1")), "", "");
        assert!(h.contains("class=\"ya-b on\" data-act=\"a-like\" aria-pressed=\"true\""));
        assert!(h.contains("&lt;b&gt;1") && !h.contains("<b>1"));
        assert!(!row_html(None, "", "").contains("a-like"));
        assert_eq!(count_text("Like"), "");
        assert!(sub_html(true, false, true, "a\"b").contains("Unsubscribe?") && sub_html(false, false, false, "x\"").contains("x&quot;"));
        assert_eq!(share_url("abcdefghijk", Some(83)), "https://youtu.be/abcdefghijk?t=83");
        assert_eq!(share_url("abcdefghijk", Some(0)), "https://youtu.be/abcdefghijk");
        assert!(share_html(true, "https://youtu.be/x?t=1&\"", Some(83), true).contains("Start at 1:23") && share_html(true, "u\"", None, false).contains("u&quot;"));
        assert_eq!((clock(59), clock(3723)), ("0:59".to_string(), "1:02:03".to_string()));
    }
}
