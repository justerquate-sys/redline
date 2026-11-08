//! Watch page "what's next": the playlist panel (a video opened with
//! `list=`), the Up next header with the remembered Autoplay switch, and the
//! choice of the next video. The app owns this; the player only reports
//! `Out::Ended`. Sources are the `next` response only: the playlist panel
//! (`playlist.playlist`) and YouTube's autoplay endpoint
//! (`autoplay.autoplay.sets[0].autoplayVideo`, with `countDownSecs`).

use crate::features::members;
use crate::parse::text;
use crate::util::{esc_into, is_safe_id, is_video_id};
use crate::json::Value;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub v: String,
    pub title: String,
    pub by: String,
    pub len: String,
    /// 0-based position in the playlist.
    pub idx: u32,
    pub cur: bool,
    /// For a channel's members: a badge, or a row of its "Members-only videos" playlist (`UUMO…`).
    pub members: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Playlist {
    pub id: String,
    pub title: String,
    pub owner: String,
    /// 0-based index of the current video.
    pub cur: u32,
    pub total: u32,
    /// Mixes: no fixed length.
    pub infinite: bool,
    /// The window of items YouTube sent (around the current one).
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Up {
    pub playlist: Option<Playlist>,
    /// YouTube's autoplay target: video id and its playlist (if any).
    pub auto: Option<(String, Option<String>)>,
    /// Seconds YouTube counts down before autoplay.
    pub countdown: u32,
    /// The related column's videos, in order (`parse::next`): what plays when YouTube's target is locked.
    pub cards: Vec<Card>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Card {
    pub v: String,
    pub locked: bool,
}

/// What plays after the current video.
#[derive(Clone, Debug, PartialEq)]
pub struct Next {
    pub v: String,
    pub list: Option<String>,
    /// Playlist items follow at once; plain autoplay counts down first.
    pub now: bool,
}

const ITEMS_MAX: usize = 500;

fn s<'a>(v: &'a Value, p: &str) -> Option<&'a str> {
    v.pointer(p).and_then(Value::as_str)
}

pub fn parse(v: &Value) -> Up {
    let root = v.pointer("/contents/twoColumnWatchNextResults").unwrap_or(&Value::Null);
    let mut up = Up { countdown: 5, ..Default::default() };
    if let Some(p) = root.pointer("/playlist/playlist") {
        if let Some(id) = s(p, "/playlistId").filter(|x| is_safe_id(x)) {
            let mut pl = Playlist {
                id: id.into(),
                title: p.get("title").and_then(text).or_else(|| p.get("titleText").and_then(text)).unwrap_or_default(),
                owner: p.get("ownerName").and_then(text).or_else(|| p.get("shortBylineText").and_then(text)).unwrap_or_default(),
                cur: p.get("currentIndex").or_else(|| p.get("localCurrentIndex")).and_then(Value::as_u64).unwrap_or(0) as u32,
                total: p.get("totalVideos").and_then(Value::as_u64).unwrap_or(0) as u32,
                infinite: p.get("isInfinite").and_then(Value::as_bool).unwrap_or(false),
                items: Vec::new(),
            };
            for (i, c) in p.get("contents").and_then(Value::as_array).into_iter().flatten().take(ITEMS_MAX).enumerate() {
                let Some(r) = c.get("playlistPanelVideoRenderer") else { continue };
                let Some(vid) = s(r, "/videoId").filter(|x| is_video_id(x)) else { continue };
                if r.get("unplayableText").is_some() {
                    continue;
                }
                pl.items.push(Item {
                    v: vid.into(),
                    title: r.get("title").and_then(text).unwrap_or_default(),
                    by: r.get("shortBylineText").and_then(text).or_else(|| r.get("longBylineText").and_then(text)).unwrap_or_default(),
                    len: r.get("lengthText").and_then(text).unwrap_or_default(),
                    idx: r.pointer("/navigationEndpoint/watchEndpoint/index").and_then(Value::as_u64).map(|x| x as u32).unwrap_or(i as u32),
                    cur: r.get("selected").and_then(Value::as_bool).unwrap_or(false),
                    members: id.starts_with("UUMO") || r.get("badges").and_then(Value::as_array).into_iter().flatten().any(|b| b.get("metadataBadgeRenderer").is_some_and(crate::parse::badge_members)),
                });
            }
            if pl.total == 0 {
                pl.total = pl.items.len() as u32;
            }
            up.playlist = Some(pl);
        }
    }
    let a = root.pointer("/autoplay/autoplay").unwrap_or(&Value::Null);
    if let Some(ep) = a.pointer("/sets/0/autoplayVideo/watchEndpoint") {
        if let Some(vid) = s(ep, "/videoId").filter(|x| is_video_id(x)) {
            up.auto = Some((vid.into(), s(ep, "/playlistId").filter(|x| is_safe_id(x)).map(String::from)));
        }
    }
    if let Some(n) = a.get("countDownSecs").and_then(Value::as_u64) {
        up.countdown = n.min(15) as u32;
    }
    up
}

/// Whether `v` is known to be for a channel's members: a card's label (here or anywhere this tab has
/// seen it), or the player's verdict.
fn locked(up: &Up, v: &str) -> bool {
    members::has(v) || up.cards.iter().any(|c| c.locked && c.v == v) || up.playlist.as_ref().is_some_and(|p| p.items.iter().any(|i| i.members && i.v == v))
}

/// The first video of the related column that isn't locked, nor `cur` (`now`: no countdown).
fn open_card(up: &Up, cur: &str, now: bool) -> Option<Next> {
    up.cards.iter().find(|c| c.v != cur && !locked(up, &c.v)).map(|c| Next { v: c.v.clone(), list: None, now })
}

/// The video after `cur`: the next playlist item (always, like YouTube),
/// else YouTube's autoplay target when Autoplay is on. A video known to be for a channel's members is
/// never chosen, whatever the Members-only switch says (a member can still open it): the next playlist
/// item that isn't, and in place of a locked autoplay target the first related card that isn't.
pub fn next(up: &Up, cur: &str, autoplay: bool) -> Option<Next> {
    if let Some(pl) = &up.playlist {
        let pos = pl.items.iter().position(|i| i.v == cur).or_else(|| pl.items.iter().position(|i| i.cur));
        let after = match pos {
            Some(p) => pl.items.iter().skip(p + 1).find(|i| !locked(up, &i.v)),
            None => pl.items.iter().find(|i| i.idx > pl.cur && !locked(up, &i.v)),
        };
        if let Some(n) = after {
            return Some(Next { v: n.v.clone(), list: Some(pl.id.clone()), now: true });
        }
    }
    let (v, list) = up.auto.as_ref().filter(|_| autoplay)?;
    if locked(up, v) {
        return open_card(up, cur, false);
    }
    // Never wrap around to an earlier item of the same playlist (no unasked loop).
    if let Some(pl) = up.playlist.as_ref().filter(|p| Some(&p.id) == list.as_ref()) {
        let at = |x: &str| pl.items.iter().position(|i| i.v == x);
        if at(v).is_some_and(|a| at(cur).is_none_or(|c| a <= c)) {
            return None;
        }
    }
    (v != cur).then(|| Next { v: v.clone(), now: list.is_some() && up.playlist.as_ref().is_some_and(|p| Some(&p.id) == list.as_ref()), list: list.clone() })
}

/// The playlist item before `cur` (the remote's Previous). `None`: no playlist, or its first item.
pub fn prev(up: &Up, cur: &str) -> Option<Next> {
    let pl = up.playlist.as_ref()?;
    let before = match pl.items.iter().position(|i| i.v == cur).or_else(|| pl.items.iter().position(|i| i.cur)) {
        Some(p) => p.checked_sub(1).and_then(|p| pl.items.get(p)),
        None => pl.items.iter().rev().find(|i| i.idx < pl.cur),
    };
    before.map(|n| Next { v: n.v.clone(), list: Some(pl.id.clone()), now: true })
}

/// What a pressed Next takes: `next` as with Autoplay on (the press asked for it), else the first
/// related card that isn't locked.
pub fn next_pressed(up: &Up, cur: &str) -> Option<Next> {
    next(up, cur, true).or_else(|| open_card(up, cur, true))
}

pub fn watch_href(v: &str, list: Option<&str>) -> String {
    match list {
        Some(l) => format!("/watch?v={v}&list={l}"),
        None => format!("/watch?v={v}"),
    }
}

/// The playlist panel (collapsible; the current row is marked and scrolled to).
pub fn panel(pl: Option<&Playlist>, cur: &str) -> String {
    let Some(pl) = pl else { return String::new() };
    let mut out = String::with_capacity(256 + pl.items.len() * 360);
    out.push_str("<details class=\"pl\" open><summary><a class=\"plt\" href=\"/playlist?list=");
    out.push_str(&pl.id);
    out.push_str("\">");
    esc_into(&mut out, if pl.title.is_empty() { "Playlist" } else { &pl.title });
    out.push_str("</a><span class=\"plm\">");
    if !pl.owner.is_empty() {
        esc_into(&mut out, &pl.owner);
        out.push_str(" · ");
    }
    let pos = pl.items.iter().find(|i| i.v == cur).map(|i| i.idx).unwrap_or(pl.cur);
    if pl.infinite {
        out.push_str("Mix");
    } else {
        out.push_str(&format!("{} / {}", pos + 1, pl.total));
    }
    out.push_str("</span></summary><ol class=\"pll\" id=\"");
    out.push_str(LIST);
    out.push_str("\">");
    for (i, it) in pl.items.iter().enumerate() {
        let here = it.v == cur;
        out.push_str(&format!("<li><a id=\"{}\" ", row_id(i)));
        out.push_str(if here { "class=\"pli cur\" aria-current=\"true\" href=\"" } else { "class=\"pli\" href=\"" });
        out.push_str(&watch_href(&it.v, Some(&pl.id)));
        out.push_str("\"><span class=\"pln\">");
        if here {
            out.push('▶');
        } else {
            out.push_str(&(it.idx + 1).to_string());
        }
        out.push_str("</span><span class=\"plth\"><img src=\"https://i.ytimg.com/vi/");
        out.push_str(&it.v);
        out.push_str("/default.jpg\" alt=\"\" loading=\"lazy\" decoding=\"async\">");
        if !it.len.is_empty() {
            out.push_str("<b>");
            esc_into(&mut out, &it.len);
            out.push_str("</b>");
        }
        out.push_str("</span><span class=\"pld\"><span class=\"pltt\">");
        esc_into(&mut out, &it.title);
        out.push_str("</span><span class=\"plb\">");
        esc_into(&mut out, &it.by);
        if it.members || members::has(&it.v) {
            if !it.by.is_empty() {
                out.push_str(" · ");
            }
            out.push_str(members::LABEL);
        }
        out.push_str("</span></span></a></li>");
    }
    out.push_str("</ol></details>");
    out
}

/// Scrollable list inside the panel (`bridge::scroll_in` target).
pub const LIST: &str = "ytc-pll";

/// Element id of the playlist panel's `i`th row (the current-row mark's target).
pub fn row_id(i: usize) -> String {
    format!("ytc-plr{i}")
}

/// Index of `v`'s row in the panel.
pub fn row_of(pl: &Playlist, v: &str) -> Option<usize> {
    pl.items.iter().position(|i| i.v == v)
}

/// The Up next header: the Autoplay switch, and while a countdown runs, the
/// next title with Play now / Cancel.
pub fn head(autoplay: bool, pending: Option<(&str, u32)>) -> String {
    let mut out = String::from("<div class=\"upn\"><span>Up next</span><button type=\"button\" class=\"fx-sw\" role=\"switch\" id=\"ytc-up-auto\" data-act=\"up-auto\" aria-checked=\"");
    out.push_str(if autoplay { "true" } else { "false" });
    out.push_str("\" title=\"Play the next video when this one ends\"><span>Autoplay</span><i aria-hidden=\"true\"></i></button></div>");
    if let Some((title, secs)) = pending {
        out.push_str("<div class=\"upc\" role=\"status\">Next");
        if !title.is_empty() {
            out.push_str(": <b>");
            esc_into(&mut out, title);
            out.push_str("</b>");
        }
        // The countdown as a linear progress line along the panel's bottom edge (CSS keyframes, compositor-only).
        out.push_str(&format!(" in {secs} s <button type=\"button\" class=\"pill upnow\" data-act=\"up-now\" id=\"ytc-up-now\" style=\"--cd:{secs}s\">Play now<span class=\"upf\" aria-hidden=\"true\">Play now</span></button><button type=\"button\" class=\"lnk2\" data-act=\"up-cancel\">Cancel</button><i class=\"upbar\" style=\"--cd:{secs}s\" aria-hidden=\"true\"></i></div>"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::test_fixture;

    #[test]
    fn playlist_from_next() {
        // Recorded signed out 1 Nov 2026: -Bxc4gJluC0 in PL_pGtHsOvhtxX3WHo-Hi268L78XBziem_.
        let up = parse(&test_fixture("next_list.json"));
        let pl = up.playlist.as_ref().unwrap();
        assert_eq!((pl.title.as_str(), pl.cur, pl.total, pl.items.len()), ("Roblox Animation Tutorials", 0, 6, 6));
        assert!(pl.items[0].cur && pl.items[0].v == "-Bxc4gJluC0" && pl.items[0].len == "5:03");
        assert_eq!(pl.items[1].idx, 1);
        assert_eq!(up.auto, Some(("UURYhAVph5g".into(), Some(pl.id.clone()))));
        assert_eq!(up.countdown, 5);
        let n = next(&up, "-Bxc4gJluC0", false).unwrap();
        assert_eq!(n, Next { v: pl.items[1].v.clone(), list: Some(pl.id.clone()), now: true }, "playlists continue even with Autoplay off");
        let last = pl.items.last().unwrap().v.clone();
        assert_eq!(next(&up, &last, false), None, "end of the playlist, Autoplay off");
        assert_eq!(next(&up, &last, true), None, "no wrap to the playlist's own earlier item");
        // The remote's Previous walks the same list backward; it ends at the first item.
        assert_eq!(prev(&up, &pl.items[2].v), Some(Next { v: pl.items[1].v.clone(), list: Some(pl.id.clone()), now: true }));
        assert_eq!(prev(&up, &pl.items[0].v), None);
        assert_eq!(prev(&Up::default(), "x"), None);
        let h = panel(Some(pl), &pl.items[2].v);
        assert!(h.contains("3 / 6") && h.contains("class=\"pli cur\""));
        assert_eq!(h.matches("<li>").count(), 6);
        assert!(h.contains(&format!("href=\"/watch?v={}&list={}\"", pl.items[3].v, pl.id)));
    }

    #[test]
    fn autoplay_without_playlist() {
        let up = parse(&test_fixture("next.json"));
        assert!(up.playlist.is_none());
        assert_eq!(up.auto.as_ref().map(|a| a.0.as_str()), Some("UURYhAVph5g"));
        assert_eq!(next(&up, "-Bxc4gJluC0", true), Some(Next { v: "UURYhAVph5g".into(), list: None, now: false }), "plain autoplay counts down");
        assert_eq!(next(&up, "-Bxc4gJluC0", false), None);
        assert_eq!(next(&up, "UURYhAVph5g", true), None, "never itself");
        assert!(panel(None, "x").is_empty());
    }

    #[test]
    fn locked_videos_are_never_next() {
        let it = |v: &str, idx: u32, members: bool| Item { v: v.into(), idx, members, ..Default::default() };
        let card = |v: &str, locked: bool| Card { v: v.into(), locked };
        // A locked playlist row is skipped; a playlist of nothing else ends in the autoplay target.
        let pl = Playlist { id: "PLx".into(), total: 4, items: vec![it("aaaaaaaaaaa", 0, false), it("bbbbbbbbbbb", 1, true), it("ccccccccccc", 2, false), it("ddddddddddd", 3, true)], ..Default::default() };
        let mut up = Up { playlist: Some(pl), auto: Some(("eeeeeeeeeee".into(), None)), cards: vec![card("eeeeeeeeeee", false)], ..Default::default() };
        assert_eq!(next(&up, "aaaaaaaaaaa", false).unwrap().v, "ccccccccccc");
        assert_eq!(next(&up, "ccccccccccc", true).unwrap().v, "eeeeeeeeeee", "the last row is locked");
        assert_eq!(next(&up, "ccccccccccc", false), None);
        // YouTube's target carries a label in the related column: the first open card instead, with its countdown.
        up.playlist = None;
        up.cards = vec![card("eeeeeeeeeee", true), card("fffffffffff", true), card("ggggggggggg", false)];
        assert_eq!(next(&up, "aaaaaaaaaaa", true), Some(Next { v: "ggggggggggg".into(), list: None, now: false }));
        assert_eq!(next(&up, "aaaaaaaaaaa", false), None, "Autoplay off");
        up.cards = vec![card("eeeeeeeeeee", true)];
        assert_eq!(next(&up, "aaaaaaaaaaa", true), None, "nothing open: stay");
        // A verdict the card lacked (the player said so).
        up.auto = Some(("hhhhhhhhhhh".into(), None));
        up.cards = vec![card("hhhhhhhhhhh", false), card("iiiiiiiiiii", false)];
        assert_eq!(next(&up, "aaaaaaaaaaa", true).unwrap().v, "hhhhhhhhhhh");
        members::learn("hhhhhhhhhhh");
        assert_eq!(next(&up, "aaaaaaaaaaa", true).unwrap().v, "iiiiiiiiiii");
        members::forget("hhhhhhhhhhh");
        // A press takes the first open card even without a target, without a countdown.
        up.auto = None;
        assert_eq!(next(&up, "aaaaaaaaaaa", true), None);
        assert_eq!(next_pressed(&up, "hhhhhhhhhhh"), Some(Next { v: "iiiiiiiiiii".into(), list: None, now: true }));
        // The real "Members-only videos" panel carries no badge: its id says it (recorded 8 Nov 2026).
        let v = crate::json::jv!({"contents": {"twoColumnWatchNextResults": {"playlist": {"playlist": {"playlistId": "UUMO1cyGje-DRza7ei9M_oYHNA", "contents": [
            {"playlistPanelVideoRenderer": {"videoId": "J3M2L2pLhxA", "title": {"simpleText": "t"}}}]}}}}});
        assert!(parse(&v).playlist.unwrap().items[0].members);
        // The row says so.
        let h = panel(Some(&Playlist { id: "PLx".into(), total: 1, items: vec![Item { v: "aaaaaaaaaaa".into(), by: "Ch".into(), members: true, ..Default::default() }], ..Default::default() }), "");
        assert!(h.contains("Ch · Members only"), "{h}");
    }

    #[test]
    fn markup_escapes() {
        let pl = Playlist { id: "PLx".into(), title: "<b>t</b>".into(), owner: "\"o".into(), total: 1, items: vec![Item { v: "aaaaaaaaaaa".into(), title: "<img src=x>".into(), ..Default::default() }], ..Default::default() };
        let h = panel(Some(&pl), "aaaaaaaaaaa");
        assert!(!h.contains("<b>t") && !h.contains("<img src=x>") && h.contains("&quot;o"));
        let hd = head(true, Some(("<i>n</i>", 5)));
        assert!(hd.contains("aria-checked=\"true\"") && hd.contains("&lt;i&gt;n") && hd.contains("in 5 s") && hd.contains("--cd:5s"));
        assert!(!head(false, None).contains("upc"));
        // Hostile ids are dropped by the parser.
        let v = crate::json::jv!({"contents": {"twoColumnWatchNextResults": {"playlist": {"playlist": {"playlistId": "PL\"x", "contents": []}},
            "autoplay": {"autoplay": {"sets": [{"autoplayVideo": {"watchEndpoint": {"videoId": "bad\"id"}}}], "countDownSecs": 99}}}}});
        let up = parse(&v);
        assert!(up.playlist.is_none() && up.auto.is_none());
        assert_eq!(up.countdown, 15);
    }
}
