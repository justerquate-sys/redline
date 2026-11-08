//! ALL InnerTube response parsing lives here. YouTube's renderer/view-model
//! formats change often, so this module maps the shapes we know into small
//! structs and never fails a page: unknown renderers are skipped and counted
//! (shown in the debug overlay). Shorts are parsed into ordinary video items
//! marked `short` (and counted); the Shorts switch in `features::filters`
//! decides whether they are shown.

use crate::util::{is_safe_id, is_video_id, query_param, safe_ext_href, safe_img, safe_rel_href, utf16_to_byte};
use crate::json::{self, Value};
use std::collections::HashSet;

// ---------------------------------------------------------------- data types

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub channel_href: Option<String>,
    pub channel_bid: Option<String>,
    /// Views · age, already joined.
    pub meta: String,
    pub duration: String,
    pub live: bool,
    /// Watched percentage (resume bar), 0 = none.
    pub progress: u8,
    pub thumb: Option<String>,
    pub thumb2x: Option<String>,
    /// Pixel widths of `thumb` / `thumb2x` (0 = unknown), for `srcset` w descriptors.
    pub thumb_w: u16,
    pub thumb2x_w: u16,
    /// Animated hover preview (`i.ytimg.com/an_webp/…`), when YouTube sent one.
    pub anim: Option<String>,
    /// YouTube labelled it "Music" / "Music video" in a badge (the Music switch).
    pub music: bool,
    /// A Short (reel endpoint, `/shorts/` URL, SHORTS badge or a Shorts
    /// lockup). `duration` then holds the badge text "Shorts".
    pub short: bool,
    /// YouTube marks it for a channel's members (`members_style`, `members_icon`, `members_label`):
    /// the card's label, the Members-only switch, and next-video selection skips it.
    pub members: bool,
    /// A Short's own vertical art (a channel's Shorts tab shows it; other pages use the 16:9 frames).
    pub art: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlaylistCard {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub channel_href: Option<String>,
    pub count: String,
    pub thumb: Option<String>,
    pub first_video: Option<String>,
    /// What the card calls itself under the title; empty = "Playlist" (a channel tab sets its own kind).
    pub kind: &'static str,
}

/// The names a playlist card can carry under its title (`PlaylistCard::kind`).
pub const PLAYLIST_KINDS: [&str; 4] = ["Podcast", "Course", "Release", "Show"];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChannelCard {
    pub href: String,
    pub bid: Option<String>,
    pub title: String,
    pub meta: String,
    pub avatar: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Video(Video),
    Playlist(PlaylistCard),
    Channel(ChannelCard),
}

impl Item {
    pub fn is_short(&self) -> bool {
        matches!(self, Item::Video(v) if v.short)
    }

    pub fn key(&self) -> &str {
        match self {
            Item::Video(v) => &v.id,
            Item::Playlist(p) => &p.id,
            Item::Channel(c) => &c.href,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub unknown: Vec<(String, u32)>,
    pub shorts: u32,
    pub ads: u32,
    /// YouTube's recommendation shelves dropped from search results ("People also watched", …).
    pub recs: u32,
    pub dupes: u32,
    pub bad: u32,
}

impl Stats {
    pub fn bump(&mut self, key: &str) {
        match self.unknown.iter_mut().find(|(k, _)| k == key) {
            Some((_, n)) => *n += 1,
            None => self.unknown.push((key.to_string(), 1)),
        }
    }
    pub fn merge(&mut self, o: &Stats) {
        for (k, n) in &o.unknown {
            for _ in 0..*n {
                self.bump(k);
            }
        }
        self.shorts += o.shorts;
        self.ads += o.ads;
        self.recs += o.recs;
        self.dupes += o.dupes;
        self.bad += o.bad;
    }
    pub fn unknown_total(&self) -> u32 {
        self.unknown.iter().map(|(_, n)| n).sum()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Feed {
    pub items: Vec<Item>,
    pub cont: Option<String>,
    pub stats: Stats,
    /// Home "nudge" (signed out, or watch history off): its title.
    pub nudge: Option<String>,
    /// Server-side alert text (e.g. "This playlist does not exist").
    pub alert: Option<String>,
    /// YouTube's tracking ids of each video card (memory only: the "shown" signals, `crate::impress`).
    pub track: Vec<Track>,
}

/// A card's tracking ids as YouTube's page logs them when the card is shown: the grid item's
/// (`richItemRenderer.trackingParams`) and the card's own (a lockup's `loggingDirectives`, a renderer's
/// `trackingParams`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub id: String,
    pub item: Option<String>,
    pub own: Option<String>,
}

/// A tracking id as YouTube sends them (base64, ≤ 200 characters).
pub fn is_tracking(s: &str) -> bool {
    (4..=200).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_=+/".contains(&b))
}

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Ext(String),
    Int(String),
    Seek(u32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub text: String,
    pub link: Option<Link>,
}

#[derive(Clone, Debug, Default)]
pub struct Watch {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub channel_href: Option<String>,
    pub channel_bid: Option<String>,
    pub avatar: Option<String>,
    pub subs: String,
    pub views: String,
    pub date: String,
    pub desc: Vec<Run>,
    pub related: Feed,
    /// Like/subscribe/comments/chapters data for `crate::actions`.
    pub acts: crate::actions::watch::Acts,
    /// Playlist panel and autoplay target (`crate::features::upnext`).
    pub up: crate::features::upnext::Up,
}

#[derive(Clone, Debug, Default)]
pub struct ChannelPage {
    pub title: String,
    pub bid: Option<String>,
    pub avatar: Option<String>,
    pub meta: Vec<String>,
    pub about: String,
    pub feed: Feed,
    /// The content tabs this channel really has, in YouTube's order (`channel_tabs`).
    pub tabs: Vec<ChannelTabLink>,
    /// The last URL segment of the tab YouTube answered with (`videos`, `featured`, …).
    pub selected: Option<String>,
    /// The sort choices of that tab.
    pub sorts: Vec<SortOpt>,
}

/// A tab of a channel: YouTube's own title (in the page's language) and `browse` params.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelTabLink {
    pub tab: crate::route::ChannelTab,
    pub title: String,
    pub params: String,
}

/// A sort choice of a channel tab, as YouTube offers it: a continuation token that reloads the list (the chip
/// bar), or `browse` params (Playlists' sort menu).
#[derive(Clone, Debug, PartialEq)]
pub struct SortOpt {
    pub label: String,
    pub selected: bool,
    pub token: Option<String>,
    pub params: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PlaylistPage {
    pub title: String,
    pub meta: Vec<String>,
    pub feed: Feed,
}

#[derive(Clone, Debug, Default)]
pub struct Ctx {
    pub logged_out: Option<bool>,
    pub visitor: Option<String>,
}

// ------------------------------------------------------------------ helpers

fn at<'a>(v: &'a Value<'a>, path: &[&str]) -> Option<&'a Value<'a>> {
    let mut cur = v;
    for p in path {
        cur = match cur {
            Value::Object(m) => m.get(*p)?,
            Value::Array(a) => a.get(p.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn st<'a>(v: &'a Value<'a>, path: &[&str]) -> Option<&'a str> {
    at(v, path)?.as_str()
}

fn arr<'a>(v: &'a Value<'a>, path: &[&str]) -> &'a [Value<'a>] {
    at(v, path).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

/// Text from `simpleText`, `runs[].text`, view-model `content` or a plain string.
pub fn text(v: &Value) -> Option<String> {
    let t = if let Some(s) = v.as_str() {
        s.to_string()
    } else if let Some(s) = v.get("simpleText").and_then(Value::as_str) {
        s.to_string()
    } else if let Some(runs) = v.get("runs").and_then(Value::as_array) {
        runs.iter().filter_map(|r| r.get("text").and_then(Value::as_str)).collect()
    } else if let Some(s) = v.get("content").and_then(Value::as_str) {
        s.to_string()
    } else {
        return None;
    };
    let t = t.trim();
    (!t.is_empty()).then(|| t.to_string())
}

fn text_at(v: &Value, path: &[&str]) -> Option<String> {
    at(v, path).and_then(text)
}

fn join(parts: &[String]) -> String {
    parts.iter().filter(|p| !p.is_empty()).cloned().collect::<Vec<_>>().join(" · ")
}

/// (src ≥300 px wide, 2x ≥600 px wide) from a thumbnail list.
fn thumbs(list: &[Value]) -> (Option<String>, Option<String>) {
    let (a, b) = thumbs_w(list);
    (a.map(|x| x.0), b.map(|x| x.0))
}

/// Like `thumbs`, with pixel widths.
fn thumbs_w(list: &[Value]) -> (Option<(String, u16)>, Option<(String, u16)>) {
    let mut v: Vec<(u64, String)> = list
        .iter()
        .filter_map(|t| Some((t.get("width").and_then(Value::as_u64).unwrap_or(0), safe_img(t.get("url")?.as_str()?)?)))
        .collect();
    v.sort_by_key(|x| x.0);
    let w = |x: &(u64, String)| (x.1.clone(), x.0.min(4000) as u16);
    let src = v.iter().find(|x| x.0 >= 300).or(v.last()).map(w);
    let x2 = v.iter().find(|x| x.0 >= 600).map(w).filter(|u| Some(&u.0) != src.as_ref().map(|s| &s.0));
    (src, x2)
}

/// Animated preview URL (`/an_webp/` on YouTube's image CDN only).
pub fn anim_url(u: Option<&str>) -> Option<String> {
    safe_img(u?).filter(|u| u.contains("/an_webp/"))
}

fn small_avatar(list: &[Value]) -> Option<String> {
    let mut v: Vec<(u64, String)> = list
        .iter()
        .filter_map(|t| Some((t.get("width").and_then(Value::as_u64).unwrap_or(0), safe_img(t.get("url")?.as_str()?)?)))
        .collect();
    v.sort_by_key(|x| x.0);
    v.iter().find(|x| x.0 >= 64).or(v.last()).map(|x| x.1.clone())
}

/// Internal href for a navigation command/endpoint.
pub fn cmd_href(cmd: &Value) -> Option<String> {
    if let Some(u) = st(cmd, &["commandMetadata", "webCommandMetadata", "url"]).and_then(safe_rel_href) {
        return Some(u);
    }
    if let Some(v) = st(cmd, &["watchEndpoint", "videoId"]).filter(|v| is_video_id(v)) {
        return Some(format!("/watch?v={v}"));
    }
    browse_ref(cmd).map(|(h, _)| h)
}

/// (href, browseId) of a channel browse endpoint. Collaboration bylines open
/// a "Collaborators" dialog instead; its first channel is used.
fn browse_ref(cmd: &Value) -> Option<(String, Option<String>)> {
    if let Some(first) = at(cmd, &["showDialogCommand", "panelLoadingStrategy", "inlineContent", "dialogViewModel", "customContent", "listViewModel", "listItems", "0", "listItemViewModel", "rendererContext", "commandContext", "onTap", "innertubeCommand"]) {
        return browse_ref(first);
    }
    let be = cmd.get("browseEndpoint")?;
    let bid = be.get("browseId").and_then(Value::as_str).filter(|b| b.starts_with("UC") && is_safe_id(b)).map(String::from);
    let href = be
        .get("canonicalBaseUrl")
        .and_then(Value::as_str)
        .and_then(safe_rel_href)
        .or_else(|| st(cmd, &["commandMetadata", "webCommandMetadata", "url"]).and_then(safe_rel_href))
        .or_else(|| bid.as_ref().map(|b| format!("/channel/{b}")))?;
    Some((href, bid))
}

fn is_short_cmd(cmd: &Value) -> bool {
    cmd.get("reelWatchEndpoint").is_some() || st(cmd, &["commandMetadata", "webCommandMetadata", "url"]).is_some_and(|u| u.starts_with("/shorts/"))
}

/// The video id of a Shorts command: `reelWatchEndpoint.videoId`, else the
/// `/shorts/<id>` URL.
fn short_cmd_id<'a>(cmd: &'a Value<'a>) -> Option<&'a str> {
    st(cmd, &["reelWatchEndpoint", "videoId"])
        .or_else(|| st(cmd, &["commandMetadata", "webCommandMetadata", "url"]).and_then(|u| u.strip_prefix("/shorts/")).map(|u| u.split(['?', '/', '&']).next().unwrap_or("")))
        .filter(|v| is_video_id(v))
}

/// The badge shown in place of a Short's duration.
pub const SHORTS_BADGE: &str = "Shorts";

// ------------------------------------------------------------------- walker

enum Res {
    Item(Item),
    /// A Short that could not be parsed into a card (counted only).
    Short,
    Bad,
    /// Unsupported variant, already counted under a more specific key.
    Counted,
}

const CONTAINERS: &[&str] = &[
    "richGridRenderer",
    "sectionListRenderer",
    "itemSectionRenderer",
    "richItemRenderer",
    "richSectionRenderer",
    "richShelfRenderer",
    "shelfRenderer",
    "horizontalListRenderer",
    "verticalListRenderer",
    "expandedShelfContentsRenderer",
    "gridRenderer",
    "playlistVideoListRenderer",
    "twoColumnSearchResultsRenderer",
    "horizontalCardListRenderer",
    "secondarySearchContainerRenderer",
    // TV client (the TV app's signed-in feeds, `crate::tv`).
    "tvBrowseRenderer",
    "tvSurfaceContentRenderer",
    "twoColumnRenderer",
];

/// Renderers that are known UI chrome rather than feed items.
const IGNORED: &[&str] = &[
    "chipCloudRenderer",
    "chipBarViewModel",
    "feedFilterChipBarRenderer",
    "relatedChipCloudRenderer",
    "itemSectionHeaderRenderer",
    "searchSubMenuRenderer",
    "sectionHeaderViewModel",
    "searchHeaderRenderer",
    "buttonRenderer",
    "buttonViewModel",
    "menuRenderer",
    "channelVideoPlayerRenderer",
    "postRenderer",
    "backstagePostThreadRenderer",
    "commentsHeaderRenderer",
    "commentsEntryPointHeaderRenderer",
    "playlistVideoListHeaderRenderer",
    "spacerRenderer",
    "showingResultsForRenderer",
    "didYouMeanRenderer",
    "includingResultsForRenderer",
    // TV client chrome.
    "searchBarRenderer",
    "entityMetadataRenderer",
    "channelHeaderRenderer",
    "shelfHeaderRenderer",
];

const ADS: &[&str] = &[
    "adSlotRenderer",
    "promotedSparklesWebRenderer",
    "promotedSparklesTextSearchRenderer",
    "searchPyvRenderer",
    "promotedVideoRenderer",
    "inFeedAdLayoutRenderer",
    "displayAdRenderer",
    "bannerPromoRenderer",
    "statementBannerRenderer",
    "brandVideoSingletonRenderer",
    "brandVideoShelfRenderer",
    "mealbarPromoRenderer",
    "primetimePromoRenderer",
];

struct Walker<'f> {
    feed: &'f mut Feed,
    seen: HashSet<String>,
    /// Search results: YouTube's recommendation shelves are dropped (`rec_shelf`).
    search: bool,
    /// The grid item being walked (its tracking id) and the card's own, for `Feed::track`.
    item_tp: Option<String>,
    own_tp: Option<String>,
}

fn tp_at(x: &Value, path: &[&str]) -> Option<String> {
    st(x, path).filter(|t| is_tracking(t)).map(String::from)
}

/// Titles of YouTube's recommendation shelves inside search results (lower case;
/// English plus the languages the client is used in). A shelf with one of these
/// titles is never a result.
const REC_SHELVES: &[&str] = &[
    "people also watched",
    "for you",
    "people also search for",
    "previously watched",
    "from related searches",
    "related to your search",
    "new to you",
    "channels new to you",
    "explore more",
    "you might also like",
    "watch again",
    "похожие видео",
    "для вас",
    "люди также смотрели",
    "люди также ищут",
    "вы недавно смотрели",
    "andere zuschauer haben sich auch das angesehen",
    "für dich",
    "nutzer haben auch nach folgenden themen gesucht",
    "zuvor angesehen",
];

fn shelf_title(x: &Value) -> Option<String> {
    let t = x.get("title").and_then(text).or_else(|| text_at(x, &["header", "shelfHeaderRenderer", "title"])).or_else(|| text_at(x, &["header", "richListHeaderRenderer", "title"]))?;
    Some(t.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
}

/// A search shelf that is YouTube recommending rather than a result: a known
/// recommendation title, or a video shelf that leads nowhere (real result
/// shelves such as a channel's "Latest from …" link to their source through the
/// title or `endpoint`).
fn rec_shelf(x: &Value) -> bool {
    if shelf_title(x).is_some_and(|t| REC_SHELVES.contains(&t.trim_end_matches([':', '…', '.']))) {
        return true;
    }
    let linked = x.get("endpoint").is_some()
        || at(x, &["title", "runs", "0", "navigationEndpoint"]).is_some()
        || at(x, &["header", "shelfHeaderRenderer", "title", "runs", "0", "navigationEndpoint"]).is_some();
    let videos = at(x, &["content", "verticalListRenderer"]).is_some() || at(x, &["content", "horizontalListRenderer"]).is_some();
    videos && !linked
}

impl Walker<'_> {
    fn walk(&mut self, v: &Value) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| self.walk(x)),
            Value::Object(m) => m.iter().for_each(|(k, x)| self.visit(k, x)),
            _ => {}
        }
    }

    fn descend(&mut self, x: &Value) {
        for k in ["contents", "items", "content", "primaryContents", "results", "rightColumn"] {
            if let Some(c) = x.get(k) {
                self.walk(c);
            }
        }
    }

    fn push(&mut self, r: Res, kind: &str) {
        let own = self.own_tp.take();
        match r {
            Res::Item(item) => {
                if item.is_short() {
                    self.feed.stats.shorts += 1;
                }
                if self.seen.insert(item.key().to_string()) {
                    if let Item::Video(v) = &item {
                        if self.item_tp.is_some() || own.is_some() {
                            self.feed.track.push(Track { id: v.id.clone(), item: self.item_tp.clone(), own });
                        }
                    }
                    self.feed.items.push(item);
                } else {
                    self.feed.stats.dupes += 1;
                }
            }
            Res::Short => self.feed.stats.shorts += 1,
            Res::Counted => {}
            Res::Bad => {
                self.feed.stats.bad += 1;
                self.feed.stats.bump(kind);
            }
        }
    }

    fn visit(&mut self, k: &str, x: &Value) {
        match k {
            // Search: "People also watched", "For you", … shelves and the "People also search for"
            // card row are YouTube's recommendations, not results (design rule, 2 Nov 2026).
            "shelfRenderer" | "richShelfRenderer" if self.search && rec_shelf(x) => self.feed.stats.recs += 1,
            "horizontalCardListRenderer" if self.search => self.feed.stats.recs += 1,
            "videoRenderer" | "compactVideoRenderer" | "gridVideoRenderer" | "playlistVideoRenderer" | "playlistPanelVideoRenderer" | "videoWithContextRenderer" => {
                let r = video_renderer(x);
                self.own_tp = tp_at(x, &["trackingParams"]);
                self.push(r, k)
            }
            "lockupViewModel" => {
                let r = lockup(x, &mut self.feed.stats);
                self.own_tp = tp_at(x, &["rendererContext", "loggingContext", "loggingDirectives", "trackingParams"]);
                self.push(r, k)
            }
            "tileRenderer" => {
                let r = tile(x, &mut self.feed.stats);
                self.push(r, k)
            }
            // TV Subscriptions / Library: a side list of tabs; only the selected one (else the first
            // with content) is the page.
            "tvSecondaryNavRenderer" => {
                let tabs: Vec<&Value> = arr(x, &["sections"]).iter().flat_map(|s| arr(s, &["tvSecondaryNavSectionRenderer", "tabs"]).iter()).filter_map(|t| t.get("tabRenderer")).collect();
                let tab = tabs.iter().find(|t| t.get("selected").and_then(Value::as_bool) == Some(true) && t.get("content").is_some()).or_else(|| tabs.iter().find(|t| t.get("content").is_some()));
                if let Some(c) = tab.and_then(|t| t.get("content")) {
                    self.walk(c);
                }
            }
            "channelRenderer" | "gridChannelRenderer" => {
                let r = channel_renderer(x);
                self.push(r, k)
            }
            "playlistRenderer" | "gridPlaylistRenderer" | "radioRenderer" | "compactRadioRenderer" | "compactPlaylistRenderer" => {
                let r = playlist_renderer(x);
                self.push(r, k)
            }
            "gridShowRenderer" => {
                let r = show_renderer(x);
                self.push(r, k)
            }
            "continuationItemRenderer" => {
                let tok = st(x, &["continuationEndpoint", "continuationCommand", "token"])
                    .or_else(|| st(x, &["button", "buttonRenderer", "command", "continuationCommand", "token"]));
                if let Some(t) = tok {
                    self.feed.cont = Some(t.to_string());
                }
            }
            "continuationItemViewModel" => {
                if let Some(t) = st(x, &["continuationCommand", "innertubeCommand", "continuationCommand", "token"]) {
                    self.feed.cont = Some(t.to_string());
                }
            }
            "shortsLockupViewModel" => {
                let r = shorts_lockup(x);
                self.own_tp = tp_at(x, &["loggingDirectives", "trackingParams"]);
                self.push(r, k)
            }
            "reelItemRenderer" => {
                let r = reel_item(x);
                self.push(r, k)
            }
            "reelShelfRenderer" | "gridShelfViewModel" => {
                // Shorts shelves: their Shorts become cards; anything else in
                // them stays dropped (counted), as before Shorts were parsed.
                let list = if arr(x, &["items"]).is_empty() { arr(x, &["contents"]) } else { arr(x, &["items"]) };
                if list.is_empty() {
                    self.feed.stats.shorts += 1;
                }
                for it in list {
                    let it = at(it, &["richItemRenderer", "content"]).unwrap_or(it);
                    let r = if let Some(s) = it.get("shortsLockupViewModel") {
                        shorts_lockup(s)
                    } else if let Some(s) = it.get("reelItemRenderer") {
                        reel_item(s)
                    } else {
                        Res::Short
                    };
                    self.push(r, k);
                }
            }
            "feedNudgeRenderer" => {
                self.feed.nudge = Some(text_at(x, &["title"]).unwrap_or_else(|| "Search to get started".into()));
            }
            "messageRenderer" | "backgroundPromoRenderer" => {
                if self.feed.alert.is_none() {
                    self.feed.alert = text_at(x, &["text"]).or_else(|| text_at(x, &["title"]));
                }
            }
            "richItemRenderer" => {
                let outer = std::mem::replace(&mut self.item_tp, tp_at(x, &["trackingParams"]));
                self.descend(x);
                self.item_tp = outer;
            }
            _ if CONTAINERS.contains(&k) || (k.ends_with("Continuation") && x.is_object()) => {
                self.descend(x);
                if let Some(t) = st(x, &["continuations", "0", "nextContinuationData", "continuation"]) {
                    self.feed.cont = Some(t.to_string());
                }
            }
            _ if ADS.contains(&k) => self.feed.stats.ads += 1,
            _ if IGNORED.contains(&k) => {}
            _ if k.ends_with("Renderer") || k.ends_with("ViewModel") => self.feed.stats.bump(k),
            _ => {}
        }
    }
}

fn walk_into(feed: &mut Feed, v: &Value) {
    let mut w = Walker { feed, seen: HashSet::new(), search: false, item_tp: None, own_tp: None };
    w.walk(v);
}

// ----------------------------------------------------------------- items

fn video_renderer(x: &Value) -> Res {
    let Some(id) = x.get("videoId").and_then(Value::as_str).filter(|v| is_video_id(v)) else { return Res::Bad };
    let nav = x.get("navigationEndpoint").unwrap_or(&Value::Null);
    let overlays = arr(x, &["thumbnailOverlays"]);
    let overlay_style = |want: &str| overlays.iter().any(|o| st(o, &["thumbnailOverlayTimeStatusRenderer", "style"]) == Some(want));
    let short = is_short_cmd(nav) || overlay_style("SHORTS");
    let title = text_at(x, &["title"]).or_else(|| text_at(x, &["headline"])).unwrap_or_default();
    let mut v = Video { id: id.into(), title, ..Default::default() };
    for key in ["longBylineText", "shortBylineText", "ownerText"] {
        if let Some(run) = at(x, &[key, "runs", "0"]) {
            v.channel = run.get("text").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some((h, b)) = run.get("navigationEndpoint").and_then(browse_ref) {
                v.channel_href = Some(h);
                v.channel_bid = b;
            }
            break;
        }
    }
    v.duration = text_at(x, &["lengthText"])
        .or_else(|| overlays.iter().find_map(|o| text_at(o, &["thumbnailOverlayTimeStatusRenderer", "text"])))
        .unwrap_or_default();
    v.live = overlay_style("LIVE")
        || arr(x, &["badges"]).iter().any(|b| st(b, &["metadataBadgeRenderer", "style"]) == Some("BADGE_STYLE_TYPE_LIVE_NOW"));
    if v.live && v.duration.eq_ignore_ascii_case("live") {
        v.duration.clear();
    }
    let meta = if let Some(info) = text_at(x, &["videoInfo"]) {
        info.replace(" • ", " · ")
    } else {
        let views = text_at(x, &["shortViewCountText"]).or_else(|| text_at(x, &["viewCountText"])).unwrap_or_default();
        join(&[views, text_at(x, &["publishedTimeText"]).unwrap_or_default()])
    };
    v.meta = meta;
    v.progress = overlays
        .iter()
        .find_map(|o| at(o, &["thumbnailOverlayResumePlaybackRenderer", "percentDurationWatched"]).and_then(Value::as_u64))
        .unwrap_or(0)
        .min(100) as u8;
    let (t, t2) = thumbs_w(arr(x, &["thumbnail", "thumbnails"]));
    (v.thumb, v.thumb_w) = t.map(|(u, w)| (Some(u), w)).unwrap_or_default();
    (v.thumb2x, v.thumb2x_w) = t2.map(|(u, w)| (Some(u), w)).unwrap_or_default();
    v.anim = anim_url(st(x, &["richThumbnail", "movingThumbnailRenderer", "movingThumbnailDetails", "thumbnails", "0", "url"]));
    v.music = arr(x, &["badges"]).iter().any(|b| st(b, &["metadataBadgeRenderer", "label"]).is_some_and(music_label));
    v.members = arr(x, &["badges"]).iter().any(|b| b.get("metadataBadgeRenderer").is_some_and(badge_members));
    if short {
        v.short = true;
        v.live = false;
        v.duration = SHORTS_BADGE.into();
    }
    Res::Item(Item::Video(v))
}

/// A Short from `shortsLockupViewModel` (Home, Search and watch-next shelves):
/// id from the tap command, title and views from the overlay text. The
/// lockup's own art is vertical; the card uses YouTube's 16:9 frames.
fn shorts_lockup(x: &Value) -> Res {
    let cmd = at(x, &["onTap", "innertubeCommand"]).unwrap_or(&Value::Null);
    let id = short_cmd_id(cmd).or_else(|| st(x, &["entityId"]).and_then(|e| e.strip_prefix("shorts-shelf-item-")).filter(|v| is_video_id(v)));
    let Some(id) = id else { return Res::Short };
    let title = text_at(x, &["overlayMetadata", "primaryText"])
        .or_else(|| st(x, &["accessibilityText"]).map(|a| a.split(", ").next().unwrap_or("").trim().to_string()).filter(|t| !t.is_empty()))
        .unwrap_or_default();
    let meta = text_at(x, &["overlayMetadata", "secondaryText"]).unwrap_or_default();
    let art = tallest(arr(x, &["thumbnailViewModel", "thumbnailViewModel", "image", "sources"]));
    Res::Item(Item::Video(Video { art, ..short_video(id, title, meta) }))
}

/// The tallest picture of a list (a Short's vertical art; YouTube lists a cropped one of the same width beside it).
fn tallest(list: &[Value]) -> Option<String> {
    list.iter().filter_map(|t| Some((t.get("height").and_then(Value::as_u64).unwrap_or(0), safe_img(t.get("url")?.as_str()?)?))).max_by_key(|x| x.0).map(|x| x.1)
}

/// A Short from the older `reelItemRenderer` (reel shelves).
fn reel_item(x: &Value) -> Res {
    let nav = x.get("navigationEndpoint").unwrap_or(&Value::Null);
    let Some(id) = x.get("videoId").and_then(Value::as_str).filter(|v| is_video_id(v)).or_else(|| short_cmd_id(nav)) else { return Res::Short };
    let title = text_at(x, &["headline"]).or_else(|| text_at(x, &["title"])).unwrap_or_default();
    let meta = text_at(x, &["viewCountText"]).unwrap_or_default();
    let art = tallest(arr(x, &["thumbnail", "thumbnails"]));
    Res::Item(Item::Video(Video { art, ..short_video(id, title, meta) }))
}

fn short_video(id: &str, title: String, meta: String) -> Video {
    // hqdefault (480 px, 4:3) beside the 320 px mqdefault keeps 2x screens sharp.
    Video {
        id: id.into(),
        title,
        meta,
        duration: SHORTS_BADGE.into(),
        thumb: Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")),
        thumb_w: 480,
        short: true,
        ..Default::default()
    }
}

struct Part<'a> {
    text: String,
    cmd: Option<&'a Value<'a>>,
    icon: bool,
}

fn lockup_rows<'a>(md: &'a Value<'a>) -> Vec<Vec<Part<'a>>> {
    arr(md, &["metadata", "contentMetadataViewModel", "metadataRows"])
        .iter()
        .map(|row| {
            arr(row, &["metadataParts"])
                .iter()
                .filter_map(|p| {
                    let text = text_at(p, &["text"])?;
                    Some(Part {
                        text,
                        cmd: at(p, &["text", "commandRuns", "0", "onTap", "innertubeCommand"]),
                        icon: p.get("leadingIcon").is_some(),
                    })
                })
                .collect::<Vec<_>>()
        })
        .filter(|r| !r.is_empty())
        .collect()
}

/// "149K" next to a play icon (watch-next compact rows) → "149K views".
fn views_label(p: &Part) -> String {
    let t = p.text.as_str();
    let numeric = t.bytes().next().is_some_and(|b| b.is_ascii_digit()) && t.bytes().all(|b| b.is_ascii_digit() || b".,KMB".contains(&b));
    if p.icon && numeric { format!("{t} views") } else { t.to_string() }
}

fn lockup_badges(img: &Value) -> (String, bool, u8) {
    let mut dur = String::new();
    let mut live = false;
    let mut progress = 0u8;
    for o in arr(img, &["overlays"]) {
        let badges = arr(o, &["thumbnailBottomOverlayViewModel", "badges"]).iter().chain(arr(o, &["thumbnailOverlayBadgeViewModel", "thumbnailBadges"]).iter());
        for b in badges {
            let bv = b.get("thumbnailBadgeViewModel").unwrap_or(&Value::Null);
            let style = st(bv, &["badgeStyle"]).unwrap_or("");
            if style.contains("LIVE") {
                live = true;
            } else if dur.is_empty() {
                if let Some(t) = text_at(bv, &["text"]) {
                    dur = t;
                }
            }
        }
        if let Some(p) = at(o, &["thumbnailBottomOverlayViewModel", "progressBar", "thumbnailOverlayProgressBarViewModel", "startPercent"])
            .or_else(|| at(o, &["thumbnailOverlayProgressBarViewModel", "startPercent"]))
            .and_then(Value::as_u64)
        {
            progress = p.min(100) as u8;
        }
    }
    (dur, live, progress)
}

/// An explicit Music badge label (never guessed from titles or channels).
pub fn music_label(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "music" | "music video" | "musik" | "musikvideo")
}

/// Lockup badges: metadata-row badges and thumbnail badges with a Music label.
fn lockup_music(md: &Value, img: &Value) -> bool {
    let row_badges = arr(md, &["metadata", "contentMetadataViewModel", "metadataRows"]).iter().flat_map(|r| arr(r, &["badges"]).iter());
    let found = row_badges.into_iter().any(|b| st(b, &["badgeViewModel", "badgeText"]).is_some_and(music_label));
    found
        || arr(img, &["overlays"]).iter().any(|o| {
            arr(o, &["thumbnailBottomOverlayViewModel", "badges"]).iter().chain(arr(o, &["thumbnailOverlayBadgeViewModel", "thumbnailBadges"]).iter()).any(|b| st(b, &["thumbnailBadgeViewModel", "text"]).is_some_and(music_label))
        })
}

/// A members badge's enum: `BADGE_STYLE_TYPE_MEMBERS_ONLY` (renderers, TV lines), `BADGE_MEMBERS_ONLY` (view
/// models). Any other members enum ("Members first", early access, is locked the same way while YouTube sends
/// it) counts as well: the badge goes when the video turns public.
fn members_style(s: &str) -> bool {
    s.contains("MEMBERS")
}

/// The icon on those badges.
fn members_icon(s: &str) -> bool {
    s == "SPONSORSHIP_STAR"
}

/// The badge's words when it has neither enum: exact labels as YouTube writes them (read from its responses in
/// these languages, 8 Nov 2026), only ever compared with a badge's own text, never a title.
pub fn members_label(s: &str) -> bool {
    matches!(s.trim().to_lowercase().as_str(), "members only" | "members-only" | "members first" | "только для спонсоров" | "nur für kanalmitglieder")
}

/// `metadataBadgeRenderer` (`videoRenderer` and its kin, a TV tile's line, a playlist panel row).
pub fn badge_members(b: &Value) -> bool {
    st(b, &["style"]).is_some_and(members_style) || st(b, &["icon", "iconType"]).is_some_and(members_icon) || st(b, &["label"]).is_some_and(members_label)
}

/// A lockup's badges: `metadataRows[].badges[].badgeViewModel` (where YouTube sends it) and thumbnail badges.
fn lockup_members(md: &Value, img: &Value) -> bool {
    let row = |b: &Value| st(b, &["badgeStyle"]).is_some_and(members_style) || st(b, &["iconName"]).is_some_and(members_icon) || st(b, &["badgeText"]).is_some_and(members_label);
    let thumb = |b: &Value| st(b, &["badgeStyle"]).is_some_and(members_style) || st(b, &["text"]).is_some_and(members_label);
    arr(md, &["metadata", "contentMetadataViewModel", "metadataRows"]).iter().flat_map(|r| arr(r, &["badges"]).iter()).any(|b| b.get("badgeViewModel").is_some_and(row))
        || arr(img, &["overlays"]).iter().any(|o| {
            arr(o, &["thumbnailBottomOverlayViewModel", "badges"]).iter().chain(arr(o, &["thumbnailOverlayBadgeViewModel", "thumbnailBadges"]).iter()).any(|b| b.get("thumbnailBadgeViewModel").is_some_and(thumb))
        })
}

fn lockup(x: &Value, stats: &mut Stats) -> Res {
    let ctype = st(x, &["contentType"]).unwrap_or("");
    let id = st(x, &["contentId"]).unwrap_or("");
    let cmd = at(x, &["rendererContext", "commandContext", "onTap", "innertubeCommand"]).unwrap_or(&Value::Null);
    let short_cmd = is_short_cmd(cmd);
    if short_cmd && ctype != "LOCKUP_CONTENT_TYPE_VIDEO" {
        return Res::Short;
    }
    let md = at(x, &["metadata", "lockupMetadataViewModel"]).unwrap_or(&Value::Null);
    let title = text_at(md, &["title"]).unwrap_or_default();
    let rows = lockup_rows(md);
    // Channel: the avatar's command, else the first row part that browses.
    let avatar_cmd = at(md, &["image", "decoratedAvatarViewModel", "rendererContext", "commandContext", "onTap", "innertubeCommand"]);
    let mut channel_ref = avatar_cmd.and_then(browse_ref);
    let mut channel = String::new();
    let mut rest: Vec<String> = Vec::new();
    for (ri, row) in rows.iter().enumerate() {
        for (pi, p) in row.iter().enumerate() {
            let browses = p.cmd.and_then(browse_ref);
            // A channel's own Playlists tab: "View full playlist" browses to the playlist itself: no channel.
            let to_list = p.cmd.and_then(|c| st(c, &["browseEndpoint", "browseId"])).is_some_and(|b| b.starts_with("VL"));
            if to_list {
                rest.push(views_label(p));
            } else if channel.is_empty() && (browses.is_some() || (ri == 0 && pi == 0 && rows.len() > 1)) {
                channel = p.text.clone();
                if channel_ref.is_none() {
                    channel_ref = browses;
                }
            } else {
                rest.push(views_label(p));
            }
        }
    }
    match ctype {
        "LOCKUP_CONTENT_TYPE_VIDEO" => {
            if !is_video_id(id) {
                return Res::Bad;
            }
            let img = at(x, &["contentImage", "thumbnailViewModel"]).unwrap_or(&Value::Null);
            let (mut duration, mut live, progress) = lockup_badges(img);
            let short = short_cmd || duration.eq_ignore_ascii_case("SHORTS");
            if short {
                duration = SHORTS_BADGE.into();
                live = false;
            }
            let (t, t2) = thumbs_w(arr(img, &["image", "sources"]));
            let (thumb, thumb_w) = t.map(|(u, w)| (Some(u), w)).unwrap_or_default();
            let (thumb2x, thumb2x_w) = t2.map(|(u, w)| (Some(u), w)).unwrap_or_default();
            let anim = arr(img, &["overlays"]).iter().find_map(|o| anim_url(st(o, &["animatedThumbnailOverlayViewModel", "thumbnail", "sources", "0", "url"])));
            let (channel_href, channel_bid) = match channel_ref {
                Some((h, b)) => (Some(h), b),
                None => (None, None),
            };
            let music = lockup_music(md, img);
            let members = lockup_members(md, img);
            Res::Item(Item::Video(Video { id: id.into(), title, channel, channel_href, channel_bid, meta: join(&rest), duration, live, progress, thumb, thumb2x, thumb_w, thumb2x_w, anim, music, short, members, art: None }))
        }
        "LOCKUP_CONTENT_TYPE_PLAYLIST" | "LOCKUP_CONTENT_TYPE_PODCAST" | "LOCKUP_CONTENT_TYPE_ALBUM" | "LOCKUP_CONTENT_TYPE_COURSE" => {
            if !is_safe_id(id) {
                return Res::Bad;
            }
            let img = at(x, &["contentImage", "collectionThumbnailViewModel", "primaryThumbnail", "thumbnailViewModel"]).unwrap_or(&Value::Null);
            let (count, _, _) = lockup_badges(img);
            let (thumb, _) = thumbs(arr(img, &["image", "sources"]));
            // The video rows (titles of the first entries, "View full playlist") are noise for a card.
            let first_video = st(cmd, &["watchEndpoint", "videoId"]).filter(|v| is_video_id(v)).map(String::from);
            Res::Item(Item::Playlist(PlaylistCard { id: id.into(), title, channel, channel_href: channel_ref.map(|r| r.0), count, thumb, first_video, kind: "" }))
        }
        other => {
            stats.bump(&format!("lockupViewModel:{}", other.trim_start_matches("LOCKUP_CONTENT_TYPE_")));
            Res::Counted
        }
    }
}

/// TV client tile (`tileRenderer`: TV feeds, playlists, channel shelves).
fn tile(x: &Value, stats: &mut Stats) -> Res {
    let ctype = st(x, &["contentType"]).unwrap_or("");
    let id = st(x, &["contentId"]).unwrap_or("");
    let md = at(x, &["metadata", "tileMetadataRenderer"]).unwrap_or(&Value::Null);
    let hdr = at(x, &["header", "tileHeaderRenderer"]).unwrap_or(&Value::Null);
    let cmd = x.get("onSelectCommand").unwrap_or(&Value::Null);
    // Shorts tiles carry their title on the thumbnail (`thumbnailOverlays[].tileMetadataRenderer`).
    let title = text_at(md, &["title"])
        .or_else(|| arr(hdr, &["thumbnailOverlays"]).iter().find_map(|o| text_at(o, &["tileMetadataRenderer", "title"])))
        .unwrap_or_default();
    // Lines: the first linked browse (else the first part of the first of several lines) is the channel.
    let lines: Vec<Vec<(String, Option<(String, Option<String>)>)>> = arr(md, &["lines"])
        .iter()
        .map(|l| {
            arr(l, &["lineRenderer", "items"])
                .iter()
                .filter_map(|it| {
                    let li = it.get("lineItemRenderer")?;
                    let t = text_at(li, &["text"])?;
                    Some((t, at(li, &["text", "runs", "0", "navigationEndpoint"]).and_then(browse_ref)))
                })
                .collect::<Vec<_>>()
        })
        .filter(|l| !l.is_empty())
        .collect();
    let mut channel = String::new();
    let mut channel_ref = None;
    let mut rest = Vec::new();
    for (li, line) in lines.iter().enumerate() {
        for (pi, (t, br)) in line.iter().enumerate() {
            if channel.is_empty() && (br.is_some() || (li == 0 && pi == 0 && lines.len() > 1)) {
                channel = t.clone();
                channel_ref = br.clone();
            } else {
                rest.push(t.clone());
            }
        }
    }
    let mut duration = String::new();
    let mut live = false;
    let mut short = ctype == "TILE_CONTENT_TYPE_SHORTS";
    let mut progress = 0u8;
    for o in arr(hdr, &["thumbnailOverlays"]) {
        if let Some(ts) = o.get("thumbnailOverlayTimeStatusRenderer") {
            match st(ts, &["style"]).unwrap_or("") {
                "LIVE" => live = true,
                "SHORTS" => short = true,
                _ => {
                    if duration.is_empty() {
                        duration = text_at(ts, &["text"]).unwrap_or_default();
                    }
                }
            }
        }
        if let Some(p) = at(o, &["thumbnailOverlayResumePlaybackRenderer", "percentDurationWatched"]).and_then(Value::as_u64) {
            progress = p.min(100) as u8;
        }
    }
    match ctype {
        "TILE_CONTENT_TYPE_VIDEO" | "TILE_CONTENT_TYPE_SHORTS" => {
            let vid = Some(id).filter(|v| is_video_id(v)).or_else(|| st(cmd, &["watchEndpoint", "videoId"]).filter(|v| is_video_id(v))).or_else(|| short_cmd_id(cmd));
            let Some(vid) = vid else { return Res::Bad };
            let line_badges = || arr(md, &["lines"]).iter().flat_map(|l| arr(l, &["lineRenderer", "items"]).iter()).filter_map(|it| at(it, &["lineItemRenderer", "badge", "metadataBadgeRenderer"]));
            let members = line_badges().any(badge_members);
            if short {
                return Res::Item(Item::Video(Video { members, ..short_video(vid, title, join(&rest)) }));
            }
            // 16:9 files (the TV lists 4:3 `sddefault` with bars): the 320 px webp, and hq720 when listed.
            let listed = arr(hdr, &["thumbnail", "thumbnails"]).iter().filter_map(|t| t.get("url").and_then(Value::as_str)).find(|u| u.contains("/hq720")).and_then(safe_img);
            let (channel_href, channel_bid) = match channel_ref {
                Some((h, b)) => (Some(h), b),
                None => (None, None),
            };
            let music = line_badges().any(|b| st(b, &["label"]).is_some_and(music_label));
            Res::Item(Item::Video(Video {
                id: vid.into(),
                title,
                channel,
                channel_href,
                channel_bid,
                meta: join(&rest),
                duration,
                live,
                progress,
                thumb: Some(format!("https://i.ytimg.com/vi_webp/{vid}/mqdefault.webp")),
                thumb_w: 320,
                thumb2x_w: if listed.is_some() { 1280 } else { 0 },
                thumb2x: listed,
                anim: None,
                music,
                short: false,
                members,
                art: None,
            }))
        }
        "TILE_CONTENT_TYPE_PLAYLIST" => {
            let pid = st(cmd, &["browseEndpoint", "browseId"]).and_then(|b| b.strip_prefix("VL")).or_else(|| st(cmd, &["watchEndpoint", "playlistId"])).or(Some(id)).filter(|p| !p.is_empty() && is_safe_id(p));
            let Some(pid) = pid else { return Res::Bad };
            let (thumb, _) = thumbs(arr(hdr, &["thumbnail", "thumbnails"]));
            let count = arr(hdr, &["thumbnailOverlays"]).iter().find_map(|o| text_at(o, &["thumbnailOverlayTimeStatusRenderer", "text"])).unwrap_or_default();
            let first_video = st(cmd, &["watchEndpoint", "videoId"]).filter(|v| is_video_id(v)).map(String::from);
            Res::Item(Item::Playlist(PlaylistCard { id: pid.into(), title, channel, channel_href: channel_ref.map(|r| r.0), count, thumb, first_video, kind: "" }))
        }
        "TILE_CONTENT_TYPE_CHANNEL" => {
            let Some((href, bid)) = browse_ref(cmd) else { return Res::Bad };
            let avatar = small_avatar(arr(hdr, &["thumbnail", "thumbnails"]));
            Res::Item(Item::Channel(ChannelCard { href, bid, title, meta: join(&rest), avatar }))
        }
        other => {
            stats.bump(&format!("tileRenderer:{}", other.trim_start_matches("TILE_CONTENT_TYPE_")));
            Res::Counted
        }
    }
}

fn channel_renderer(x: &Value) -> Res {
    let Some(bid) = st(x, &["channelId"]).filter(|b| is_safe_id(b)) else { return Res::Bad };
    let nav = x.get("navigationEndpoint").unwrap_or(&Value::Null);
    let href = browse_ref(nav).map(|r| r.0).unwrap_or_else(|| format!("/channel/{bid}"));
    let title = text_at(x, &["title"]).unwrap_or_default();
    // Search puts the @handle in subscriberCountText and subscribers in videoCountText.
    let meta = join(&[text_at(x, &["subscriberCountText"]).unwrap_or_default(), text_at(x, &["videoCountText"]).unwrap_or_default()]);
    let avatar = small_avatar(arr(x, &["thumbnail", "thumbnails"]));
    Res::Item(Item::Channel(ChannelCard { href, bid: Some(bid.into()), title, meta, avatar }))
}

fn playlist_renderer(x: &Value) -> Res {
    let Some(id) = st(x, &["playlistId"]).filter(|b| is_safe_id(b)) else { return Res::Bad };
    let title = text_at(x, &["title"]).unwrap_or_default();
    let count = text_at(x, &["videoCountText"]).or_else(|| text_at(x, &["videoCountShortText"])).or_else(|| st(x, &["videoCount"]).map(|n| format!("{n} videos"))).unwrap_or_default();
    let (channel, channel_href) = match at(x, &["shortBylineText", "runs", "0"]).or_else(|| at(x, &["longBylineText", "runs", "0"])) {
        Some(r) => (r.get("text").and_then(Value::as_str).unwrap_or("").to_string(), r.get("navigationEndpoint").and_then(browse_ref).map(|r| r.0)),
        None => (String::new(), None),
    };
    let thumb = thumbs(arr(x, &["thumbnails", "0", "thumbnails"]))
        .0
        .or_else(|| thumbs(arr(x, &["thumbnail", "thumbnails"])).0)
        // A channel's Releases tab.
        .or_else(|| thumbs(arr(x, &["thumbnailRenderer", "playlistCustomThumbnailRenderer", "thumbnail", "thumbnails"])).0)
        .or_else(|| thumbs(arr(x, &["thumbnailRenderer", "playlistVideoThumbnailRenderer", "thumbnail", "thumbnails"])).0);
    let first_video = st(x, &["navigationEndpoint", "watchEndpoint", "videoId"]).filter(|v| is_video_id(v)).map(String::from);
    Res::Item(Item::Playlist(PlaylistCard { id: id.into(), title, channel, channel_href, count, thumb, first_video, kind: "" }))
}

/// A channel's Shows tab: a show is a playlist (`VL…`) with its episode count on the thumbnail.
fn show_renderer(x: &Value) -> Res {
    let Some(id) = st(x, &["navigationEndpoint", "browseEndpoint", "browseId"]).and_then(|b| b.strip_prefix("VL")).filter(|b| is_safe_id(b)) else { return Res::Bad };
    let title = text_at(x, &["title"]).unwrap_or_default();
    let count = arr(x, &["thumbnailOverlays"]).iter().find_map(|o| text_at(o, &["thumbnailOverlayBottomPanelRenderer", "text"])).unwrap_or_default();
    let (channel, channel_href) = match at(x, &["shortBylineText", "runs", "0"]).or_else(|| at(x, &["longBylineText", "runs", "0"])) {
        Some(r) => (r.get("text").and_then(Value::as_str).unwrap_or("").to_string(), r.get("navigationEndpoint").and_then(browse_ref).map(|r| r.0)),
        None => (String::new(), None),
    };
    let thumb = thumbs(arr(x, &["thumbnailRenderer", "showCustomThumbnailRenderer", "thumbnail", "thumbnails"])).0;
    Res::Item(Item::Playlist(PlaylistCard { id: id.into(), title, channel, channel_href, count, thumb, first_video: None, kind: "" }))
}

// ------------------------------------------------------------- responses

pub fn ctx(v: &Value) -> Ctx {
    Ctx {
        logged_out: at(v, &["responseContext", "mainAppWebResponseContext", "loggedOut"]).and_then(Value::as_bool),
        visitor: st(v, &["responseContext", "visitorData"]).map(String::from),
    }
}

fn alerts(v: &Value) -> Option<String> {
    arr(v, &["alerts"]).iter().find_map(|a| text_at(a, &["alertRenderer", "text"]).or_else(|| text_at(a, &["alertWithButtonRenderer", "text"])))
}

fn selected_tab<'a>(v: &'a Value<'a>) -> Option<&'a Value<'a>> {
    let tabs = arr(v, &["contents", "twoColumnBrowseResultsRenderer", "tabs"]);
    tabs.iter()
        .filter_map(|t| t.get("tabRenderer"))
        .find(|t| t.get("selected").and_then(Value::as_bool) == Some(true))
        .or_else(|| tabs.iter().filter_map(|t| t.get("tabRenderer")).find(|t| t.get("content").is_some()))
        .and_then(|t| t.get("content"))
}

/// Home, Subscriptions, History and other generic browse feeds.
pub fn browse(v: &Value) -> Feed {
    let mut f = Feed::default();
    if let Some(c) = selected_tab(v) {
        walk_into(&mut f, c);
    } else if let Some(c) = at(v, &["contents"]) {
        walk_into(&mut f, c);
    }
    if f.alert.is_none() {
        f.alert = alerts(v);
    }
    f
}

pub fn search(v: &Value) -> Feed {
    let mut f = Feed::default();
    let c = at(v, &["contents", "twoColumnSearchResultsRenderer", "primaryContents"]).or_else(|| at(v, &["contents"]));
    if let Some(c) = c {
        Walker { feed: &mut f, seen: HashSet::new(), search: true, item_tp: None, own_tp: None }.walk(c);
    }
    f
}

/// Any continuation response (browse, search, next).
pub fn continuation(v: &Value) -> Feed {
    cont_walk(v, false)
}

/// A search continuation (recommendation shelves dropped as in `search`).
pub fn search_continuation(v: &Value) -> Feed {
    cont_walk(v, true)
}

fn cont_walk(v: &Value, search: bool) -> Feed {
    let mut f = Feed::default();
    let mut w = Walker { feed: &mut f, seen: HashSet::new(), search, item_tp: None, own_tp: None };
    for key in ["onResponseReceivedActions", "onResponseReceivedCommands", "onResponseReceivedEndpoints"] {
        for action in arr(v, &[key]) {
            for inner in ["appendContinuationItemsAction", "reloadContinuationItemsCommand"] {
                if let Some(items) = at(action, &[inner, "continuationItems"]) {
                    w.walk(items);
                }
            }
        }
    }
    if let Some(c) = v.get("continuationContents") {
        w.walk(c);
    }
    f
}

/// `/complete/search?client=youtube&xhr=t&oe=utf-8` → `["q", [["s", 0, [...]], …], …]`
/// (a `window.google.ac.h(…)` wrapper is tolerated). At most 10 suggestions.
pub fn suggestions(body: &[u8]) -> Option<Vec<String>> {
    let start = body.iter().position(|&b| b == b'[')?;
    let end = body.iter().rposition(|&b| b == b']')?;
    let v = json::parse(body.get(start..=end)?)?;
    let list = v.get(1)?.as_array()?;
    Some(list.iter().filter_map(|x| x.get(0)?.as_str()).filter(|s| !s.trim().is_empty() && s.len() < 200).take(10).map(String::from).collect())
}

pub fn resolve(v: &Value) -> Option<String> {
    st(v, &["endpoint", "browseEndpoint", "browseId"]).filter(|b| b.starts_with("UC") && is_safe_id(b)).map(String::from)
}

/// The `browse` params YouTube names for a channel URL (a tab's URL carries its tab's).
pub fn resolve_params(v: &Value) -> Option<String> {
    st(v, &["endpoint", "browseEndpoint", "params"]).filter(|p| crate::route::valid_sp(p)).map(String::from)
}

pub(crate) fn desc_link(cmd: &Value, current: &str) -> Option<Link> {
    if let Some(url) = st(cmd, &["urlEndpoint", "url"]).or_else(|| st(cmd, &["commandMetadata", "webCommandMetadata", "url"]).filter(|u| u.starts_with("http"))) {
        let target = if url.contains("youtube.com/redirect") {
            url.split_once('?').and_then(|(_, q)| query_param(q, "q")).unwrap_or_default()
        } else {
            url.to_string()
        };
        if let Some(rest) = target.strip_prefix("https://www.youtube.com") {
            if let Some(h) = safe_rel_href(rest) {
                return Some(Link::Int(h));
            }
        }
        return safe_ext_href(&target).map(Link::Ext);
    }
    if let Some(we) = cmd.get("watchEndpoint") {
        let vid = we.get("videoId").and_then(Value::as_str).filter(|v| is_video_id(v))?;
        let t = we.get("startTimeSeconds").and_then(Value::as_u64).unwrap_or(0) as u32;
        if vid == current {
            return Some(Link::Seek(t));
        }
        return Some(Link::Int(if t > 0 { format!("/watch?v={vid}&t={t}s") } else { format!("/watch?v={vid}") }));
    }
    cmd_href(cmd).map(Link::Int)
}

fn description(sec: &Value, current: &str) -> Vec<Run> {
    if let Some(ad) = sec.get("attributedDescription") {
        return attributed(ad, current);
    }
    legacy_runs(arr(sec, &["description", "runs"]), current)
}

/// Legacy `runs[]` with `navigationEndpoint` links.
pub(crate) fn legacy_runs(runs: &[Value], current: &str) -> Vec<Run> {
    runs.iter()
        .filter_map(|r| {
            let text = r.get("text")?.as_str()?.to_string();
            let link = r.get("navigationEndpoint").and_then(|c| desc_link(c, current));
            Some(Run { text, link })
        })
        .collect()
}

/// An attributed string (`content` + `commandRuns` with UTF-16 offsets) as
/// runs; links to `current` become seeks. Used for descriptions and comments.
pub(crate) fn attributed(ad: &Value, current: &str) -> Vec<Run> {
    {
        let content = ad.get("content").and_then(Value::as_str).unwrap_or("");
        let mut runs: Vec<(usize, usize, Option<Link>)> = arr(ad, &["commandRuns"])
            .iter()
            .filter_map(|r| {
                let s = r.get("startIndex")?.as_u64()? as usize;
                let l = r.get("length")?.as_u64()? as usize;
                let link = desc_link(at(r, &["onTap", "innertubeCommand"])?, current);
                Some((s, s + l, link))
            })
            .collect();
        runs.sort_by_key(|r| r.0);
        let mut out = Vec::new();
        let mut pos_u16 = 0usize;
        for (s, e, link) in runs {
            if s < pos_u16 || link.is_none() {
                continue;
            }
            let (bs, be) = (utf16_to_byte(content, pos_u16), utf16_to_byte(content, s));
            if be > bs {
                out.push(Run { text: content[bs..be].to_string(), link: None });
            }
            let (ls, le) = (utf16_to_byte(content, s), utf16_to_byte(content, e));
            if le > ls {
                out.push(Run { text: content[ls..le].to_string(), link });
            }
            pos_u16 = e;
        }
        let bs = utf16_to_byte(content, pos_u16);
        if bs < content.len() {
            out.push(Run { text: content[bs..].to_string(), link: None });
        }
        out
    }
}

pub fn next(v: &Value) -> Watch {
    let mut w = Watch::default();
    w.id = st(v, &["currentVideoEndpoint", "watchEndpoint", "videoId"]).filter(|x| is_video_id(x)).unwrap_or("").to_string();
    let root = at(v, &["contents", "twoColumnWatchNextResults"]).unwrap_or(&Value::Null);
    for c in arr(root, &["results", "results", "contents"]) {
        if let Some(p) = c.get("videoPrimaryInfoRenderer") {
            w.title = text_at(p, &["title"]).unwrap_or_default();
            w.views = text_at(p, &["viewCount", "videoViewCountRenderer", "viewCount"])
                .or_else(|| text_at(p, &["viewCount", "videoViewCountRenderer", "shortViewCount"]))
                .unwrap_or_default();
            w.date = text_at(p, &["dateText"]).or_else(|| text_at(p, &["relativeDateText"])).unwrap_or_default();
        } else if let Some(s) = c.get("videoSecondaryInfoRenderer") {
            let o = at(s, &["owner", "videoOwnerRenderer"]).unwrap_or(&Value::Null);
            if let Some(run) = at(o, &["title", "runs", "0"]) {
                w.channel = run.get("text").and_then(Value::as_str).unwrap_or("").to_string();
                if let Some((h, b)) = run.get("navigationEndpoint").and_then(browse_ref) {
                    w.channel_href = Some(h);
                    w.channel_bid = b;
                }
            }
            if w.channel_href.is_none() {
                if let Some((h, b)) = o.get("navigationEndpoint").and_then(browse_ref) {
                    w.channel_href = Some(h);
                    w.channel_bid = b;
                }
            }
            w.subs = text_at(o, &["subscriberCountText"]).unwrap_or_default();
            w.avatar = small_avatar(arr(o, &["thumbnail", "thumbnails"]));
            let id = w.id.clone();
            w.desc = description(s, &id);
        }
    }
    w.acts = crate::actions::watch::parse(v);
    if w.acts.chapters.is_empty() {
        // No chapters in `next`: YouTube's description-timecode rules.
        let text: String = w.desc.iter().map(|r| r.text.as_str()).collect();
        w.acts.chapters = crate::actions::watch::desc_chapters(&text);
    }
    w.up = crate::features::upnext::parse(v);
    if let Some(sr) = at(root, &["secondaryResults", "secondaryResults"]) {
        let mut wk = Walker { feed: &mut w.related, seen: HashSet::new(), search: false, item_tp: None, own_tp: None };
        wk.descend(sr);
    }
    // What next-video selection may fall back on when YouTube's target is locked.
    w.up.cards = w.related.items.iter().filter_map(|i| match i {
        Item::Video(c) if !c.short && !c.live => Some(crate::features::upnext::Card { v: c.id.clone(), locked: c.members }),
        _ => None,
    }).collect();
    w
}

/// A tab's last URL segment (`/@x/streams` → `streams`).
fn tab_slug<'a>(t: &'a Value<'a>) -> Option<&'a str> {
    let url = st(t, &["endpoint", "commandMetadata", "webCommandMetadata", "url"])?;
    url.split(['?', '#']).next()?.rsplit('/').next()
}

/// The content tabs YouTube lists for this channel that the client renders as cards, in its order, and the
/// segment of the tab it answered with. Titles follow the page's language; a tab the channel lacks is not in the
/// answer, so nothing is assumed.
fn channel_tabs(v: &Value) -> (Vec<ChannelTabLink>, Option<String>) {
    let (mut out, mut selected) = (Vec::new(), None);
    for t in arr(v, &["contents", "twoColumnBrowseResultsRenderer", "tabs"]).iter().filter_map(|t| t.get("tabRenderer")) {
        let Some(slug) = tab_slug(t) else { continue };
        if t.get("selected").and_then(Value::as_bool) == Some(true) {
            selected = Some(slug.to_string());
        }
        let Some(tab) = crate::route::ChannelTab::from_slug(slug) else { continue };
        let Some(params) = st(t, &["endpoint", "browseEndpoint", "params"]).filter(|p| crate::route::valid_sp(p)) else { continue };
        let Some(title) = text_at(t, &["title"]) else { continue };
        if out.iter().all(|l: &ChannelTabLink| l.tab != tab) {
            out.push(ChannelTabLink { tab, title: title.chars().take(40).collect(), params: params.to_string() });
        }
    }
    (out, selected)
}

/// A chip that would filter by paid membership is never a sort choice (no shape carries one today; the bar with
/// YouTube's "Members only" chip has its sort choices in a drop-down, and `chip_sorts` reads only those).
fn members_chip(s: &str) -> bool {
    let s = s.to_lowercase();
    s.contains("member") || s.contains("спонсор") || s.contains("mitglied")
}

fn token_ok(t: &&str) -> bool {
    (8..4096).contains(&t.len())
}

/// A chip bar's sort choices. Two shapes (live, Nov 2026): Latest / Popular / Oldest as three chips, each with its
/// continuation; or, on channels with members-only videos, one drop-down chip holding those three in a sheet beside
/// "Members only" and "Public" filter chips (which are not sorts and stay out).
fn chip_sorts(bar: &Value) -> Vec<SortOpt> {
    let chips: Vec<&Value> = arr(bar, &["chips"]).iter().filter_map(|c| c.get("chipViewModel")).collect();
    let sheet = chips.iter().find_map(|c| {
        let items = arr(c, &["tapCommand", "innertubeCommand", "showSheetCommand", "panelLoadingStrategy", "inlineContent", "sheetViewModel", "content", "listViewModel", "listItems"]);
        (!items.is_empty()).then_some(items)
    });
    let mut out: Vec<SortOpt> = Vec::new();
    match sheet {
        Some(items) => {
            for it in items.iter().filter_map(|i| i.get("listItemViewModel")) {
                let Some(label) = text_at(it, &["title"]) else { continue };
                let cmd = at(it, &["rendererContext", "commandContext", "onTap", "innertubeCommand"]).unwrap_or(&Value::Null);
                let token = st(cmd, &["continuationCommand", "token"]).or_else(|| arr(cmd, &["commandExecutorCommand", "commands"]).iter().find_map(|c| st(c, &["continuationCommand", "token"])));
                let selected = it.get("isSelected").and_then(Value::as_bool) == Some(true);
                out.push(SortOpt { label, selected, token: token.filter(token_ok).map(String::from), params: None });
            }
        }
        None => {
            for c in chips {
                let Some(label) = text_at(c, &["text"]).filter(|l| !members_chip(l)) else { continue };
                let selected = c.get("selected").and_then(Value::as_bool) == Some(true);
                let token = st(c, &["tapCommand", "innertubeCommand", "continuationCommand", "token"]);
                out.push(SortOpt { label, selected, token: token.filter(token_ok).map(String::from), params: None });
            }
        }
    }
    sorts_done(out)
}

/// Playlists' sort menu: choices with `browse` params.
fn menu_sorts(menu: &Value) -> Vec<SortOpt> {
    let mut out = Vec::new();
    for it in arr(menu, &["subMenuItems"]) {
        let Some(label) = text_at(it, &["title"]) else { continue };
        let params = st(it, &["navigationEndpoint", "browseEndpoint", "params"]).filter(|p| crate::route::valid_sp(p));
        let selected = it.get("selected").and_then(Value::as_bool) == Some(true);
        out.push(SortOpt { label, selected, token: None, params: params.map(String::from) });
    }
    sorts_done(out)
}

/// A choice only when there is something to choose: two or more, each reachable (the selected one needs no command).
fn sorts_done(mut out: Vec<SortOpt>) -> Vec<SortOpt> {
    out.retain(|o| o.selected || o.token.is_some() || o.params.is_some());
    for o in &mut out {
        o.label = o.label.chars().take(40).collect();
    }
    if out.len() < 2 { Vec::new() } else { out }
}

/// A channel tab's sort choices: from its first page, or from a sort's answer (the header slot of a reload).
pub fn channel_sorts(v: &Value) -> Vec<SortOpt> {
    if let Some(c) = selected_tab(v) {
        if let Some(bar) = at(c, &["richGridRenderer", "header", "chipBarViewModel"]) {
            return chip_sorts(bar);
        }
        if let Some(menu) = at(c, &["sectionListRenderer", "subMenu", "channelSubMenuRenderer", "sortSetting", "sortFilterSubMenuRenderer"]) {
            return menu_sorts(menu);
        }
        return Vec::new();
    }
    for a in arr(v, &["onResponseReceivedActions"]) {
        for it in arr(a, &["reloadContinuationItemsCommand", "continuationItems"]) {
            if let Some(bar) = it.get("chipBarViewModel") {
                return chip_sorts(bar);
            }
        }
    }
    Vec::new()
}

/// A channel's description line without the list marks channels start theirs with ("• https://…").
fn about_line(s: &str) -> String {
    let t = s.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '•' | '·' | '‣' | '▪' | '▫' | '●' | '○' | '◦' | '⁃' | '∙' | '■' | '□'));
    let t = t.strip_prefix(['-', '–', '—', '*']).filter(|r| r.starts_with(char::is_whitespace)).unwrap_or(t);
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn channel(v: &Value) -> ChannelPage {
    let mut c = ChannelPage { feed: browse(v), ..Default::default() };
    (c.tabs, c.selected) = channel_tabs(v);
    c.sorts = channel_sorts(v);
    let md = at(v, &["metadata", "channelMetadataRenderer"]).unwrap_or(&Value::Null);
    c.title = st(md, &["title"]).unwrap_or("").to_string();
    c.bid = st(md, &["externalId"]).filter(|b| is_safe_id(b)).map(String::from);
    if let Some(h) = at(v, &["header", "pageHeaderRenderer"]) {
        if c.title.is_empty() {
            c.title = st(h, &["pageTitle"]).unwrap_or("").to_string();
        }
        let vm = at(h, &["content", "pageHeaderViewModel"]).unwrap_or(&Value::Null);
        c.avatar = small_avatar(arr(vm, &["image", "decoratedAvatarViewModel", "avatar", "avatarViewModel", "image", "sources"]));
        for row in arr(vm, &["metadata", "contentMetadataViewModel", "metadataRows"]) {
            for p in arr(row, &["metadataParts"]) {
                if let Some(t) = text_at(p, &["text"]) {
                    c.meta.push(t);
                }
            }
        }
        // (A channel without a description: the preview holds only its "More about this channel" link.)
        if st(md, &["description"]).is_some_and(|d| !d.trim().is_empty()) {
            c.about = text_at(vm, &["description", "descriptionPreviewViewModel", "description"]).unwrap_or_default();
        }
    } else if let Some(h) = at(v, &["header", "c4TabbedHeaderRenderer"]) {
        if c.title.is_empty() {
            c.title = st(h, &["title"]).unwrap_or("").to_string();
        }
        c.avatar = small_avatar(arr(h, &["avatar", "thumbnails"]));
        for k in ["channelHandleText", "subscriberCountText", "videosCountText"] {
            if let Some(t) = text_at(h, &[k]) {
                c.meta.push(t);
            }
        }
    }
    if c.about.is_empty() {
        c.about = st(md, &["description"]).unwrap_or("").lines().next().unwrap_or("").to_string();
    }
    c.about = about_line(&c.about);
    c
}

pub fn playlist(v: &Value) -> PlaylistPage {
    let mut p = PlaylistPage { feed: browse(v), ..Default::default() };
    if let Some(h) = at(v, &["header", "pageHeaderRenderer"]) {
        p.title = st(h, &["pageTitle"]).unwrap_or("").to_string();
        let vm = at(h, &["content", "pageHeaderViewModel"]).unwrap_or(&Value::Null);
        for row in arr(vm, &["metadata", "contentMetadataViewModel", "metadataRows"]) {
            for part in arr(row, &["metadataParts"]) {
                if let Some(t) = text_at(part, &["text"]).or_else(|| text_at(part, &["avatarStack", "avatarStackViewModel", "text"])) {
                    p.meta.push(t);
                }
            }
        }
    } else if let Some(h) = at(v, &["header", "playlistHeaderRenderer"]) {
        p.title = text_at(h, &["title"]).unwrap_or_default();
        for k in ["ownerText", "numVideosText", "viewCountText"] {
            if let Some(t) = text_at(h, &[k]) {
                p.meta.push(t);
            }
        }
    }
    if p.title.is_empty() {
        p.title = st(v, &["metadata", "playlistMetadataRenderer", "title"]).unwrap_or("").to_string();
    }
    p
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value<'static> {
        crate::util::test_fixture(name)
    }

    fn videos(f: &Feed) -> Vec<&Video> {
        f.items.iter().filter_map(|i| if let Item::Video(v) = i { Some(v) } else { None }).collect()
    }

    #[test]
    fn search_fixture() {
        let v = fixture("search.json");
        let f = search(&v);
        // Shorts are checked in `shorts_are_parsed_and_marked`.
        let vids: Vec<&Video> = videos(&f).into_iter().filter(|x| !x.short).collect();
        assert!(vids.len() >= 10, "videos: {}", vids.len());
        // The first videoRenderer in the fixture is a Short (reelWatchEndpoint).
        assert!(vids.iter().all(|x| x.id != "DQcrB2wLu04"));
        assert!(f.stats.shorts >= 2, "shorts {}", f.stats.shorts);
        for x in &vids {
            assert!(is_video_id(&x.id));
            assert!(!x.title.is_empty());
            assert!(!x.channel.is_empty(), "channel for {}", x.id);
            assert!(x.channel_href.as_deref().is_some_and(|h| h.starts_with('/')), "{x:?}");
            assert!(x.thumb.as_deref().is_some_and(|t| t.starts_with("https://i.ytimg.com/")));
        }
        assert!(vids.iter().filter(|x| !x.duration.is_empty()).count() >= 10);
        assert!(vids.iter().any(|x| x.duration.contains(':')));
        assert!(vids.iter().any(|x| x.meta.contains("views")));
        // Playlist lockups become playlist cards.
        let pl: Vec<_> = f.items.iter().filter_map(|i| if let Item::Playlist(p) = i { Some(p) } else { None }).collect();
        assert!(pl.iter().any(|p| p.id == "PL_pGtHsOvhtxX3WHo-Hi268L78XBziem_" && p.title == "Roblox Animation Tutorials" && p.channel == "MonkeyDev" && p.count == "6 videos"));
        assert!(f.cont.as_deref().is_some_and(|c| c.len() > 50));
        assert_eq!(f.stats.bad, 0);
        // The recorded "People also watched" shelf (10 videos) is YouTube recommending, not results.
        assert_eq!(f.stats.recs, 1);
        for id in ["AH30avEEC9A", "1AGWEnrPeQY", "ILaV0JYHzwY"] {
            assert!(f.items.iter().all(|i| i.key() != id), "shelf video {id} dropped");
        }
        // Results around the shelf keep their order.
        let ids: Vec<&str> = vids.iter().map(|v| v.id.as_str()).collect();
        let (a, b) = (ids.iter().position(|i| *i == "8j7tSdCMON8").unwrap(), ids.iter().position(|i| *i == "n8DAYzxTEO8").unwrap());
        assert_eq!(b, a + 1, "the result after the shelf follows the one before it");
        // Elsewhere (Home, related) the walker keeps shelves as before.
        let mut home = Feed::default();
        walk_into(&mut home, at(&v, &["contents", "twoColumnSearchResultsRenderer", "primaryContents"]).unwrap());
        assert!(home.items.iter().any(|i| i.key() == "AH30avEEC9A") && home.stats.recs == 0);
    }

    /// Recommendation shelves inside search results, by title (any case, several languages) or by
    /// structure (a video shelf that links nowhere); real result shelves (a channel's "Latest from …",
    /// linked to the channel) and channel/playlist results stay.
    #[test]
    fn search_recommendation_shelves() {
        let vr = |id: &str| crate::json::jv!({"videoRenderer": {"videoId": id, "title": {"runs": [{"text": format!("Video {id}")}]}, "longBylineText": {"runs": [{"text": "Chan", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCabcdefghijklmnopqrstuv", "canonicalBaseUrl": "/@chan"}}}]}}}).to_serde();
        let shelf = |title: serde_json::Value, ids: &[&str], linked: bool| {
            let items: Vec<serde_json::Value> = ids.iter().map(|i| vr(i)).collect();
            let mut s = serde_json::json!({"shelfRenderer": {"title": title, "content": {"verticalListRenderer": {"items": items}}}});
            if linked {
                s["shelfRenderer"]["endpoint"] = serde_json::json!({"browseEndpoint": {"browseId": "UCabcdefghijklmnopqrstuv"}});
            }
            s
        };
        let contents = serde_json::json!([
            vr("result00001"),
            shelf(serde_json::json!({"simpleText": "People also watched"}), &["recommend01", "recommend02"], false),
            shelf(serde_json::json!({"runs": [{"text": "For "}, {"text": "you"}]}), &["recommend03"], true),
            shelf(serde_json::json!({"simpleText": "PEOPLE ALSO SEARCH FOR"}), &["recommend04"], true),
            shelf(serde_json::json!({"simpleText": "Люди также смотрели"}), &["recommend05"], false),
            shelf(serde_json::json!({"simpleText": "Something new YouTube invents"}), &["recommend06"], false),
            shelf(serde_json::json!({"runs": [{"text": "Latest from Chan", "navigationEndpoint": {"browseEndpoint": {"browseId": "UCabcdefghijklmnopqrstuv"}}}]}), &["channelvid1", "channelvid2"], true),
            {"horizontalCardListRenderer": {"header": {"richListHeaderRenderer": {"title": {"simpleText": "People also search for"}}}, "cards": [{"searchRefinementCardRenderer": {"query": {"runs": [{"text": "roblox"}]}}}]}},
            {"channelRenderer": {"channelId": "UCabcdefghijklmnopqrstuv", "title": {"simpleText": "Chan"}, "navigationEndpoint": {"browseEndpoint": {"browseId": "UCabcdefghijklmnopqrstuv", "canonicalBaseUrl": "/@chan"}}}},
            vr("result00002"),
        ]);
        let resp = serde_json::json!({"contents": {"twoColumnSearchResultsRenderer": {"primaryContents": {"sectionListRenderer": {"contents": [{"itemSectionRenderer": {"contents": contents}}]}}}}});
        let v = Value::from_serde(&resp);
        let f = search(&v);
        let keys: Vec<&str> = f.items.iter().map(|i| i.key()).collect();
        assert_eq!(keys, ["result00001", "channelvid1", "channelvid2", "/@chan", "result00002"], "{keys:?}");
        assert_eq!(f.stats.recs, 6);
        // The same items in a search continuation.
        let cont = serde_json::json!({"onResponseReceivedCommands": [{"appendContinuationItemsAction": {"continuationItems": [{"itemSectionRenderer": {"contents": resp["contents"]["twoColumnSearchResultsRenderer"]["primaryContents"]["sectionListRenderer"]["contents"][0]["itemSectionRenderer"]["contents"].clone()}}]}}]});
        let c = search_continuation(&Value::from_serde(&cont));
        assert_eq!(c.items.iter().map(|i| i.key()).collect::<Vec<_>>(), keys);
        assert_eq!(continuation(&Value::from_serde(&cont)).stats.recs, 0, "browse/next continuations untouched");
    }

    #[test]
    fn shorts_are_parsed_and_marked() {
        // Real search response: six Short videoRenderers and a Shorts shelf
        // (gridShelfViewModel of shortsLockupViewModel).
        let f = search(&fixture("search.json"));
        let shorts: Vec<&Video> = videos(&f).into_iter().filter(|v| v.short).collect();
        assert_eq!(shorts.len(), 11, "{:?}", shorts.iter().map(|v| &v.id).collect::<Vec<_>>());
        assert_eq!(f.stats.shorts, 11);
        for v in &shorts {
            assert!(is_video_id(&v.id) && !v.title.is_empty() && v.meta.contains("views"), "{v:?}");
            assert_eq!(v.duration, SHORTS_BADGE, "badge in place of the duration");
            assert!(!v.live);
        }
        // A Short videoRenderer keeps its channel and YouTube's 16:9 thumbnail.
        let vr = shorts.iter().find(|v| v.id == "DQcrB2wLu04").unwrap();
        assert!(!vr.channel.is_empty() && vr.channel_href.is_some());
        assert!(vr.thumb.as_deref().is_some_and(|t| t.contains("/hq720")));
        assert_eq!(vr.meta, "891K views · 1 year ago");
        // A shelf Short: id from reelWatchEndpoint, overlay title/views, 16:9 frames.
        let sl = shorts.iter().find(|v| v.id == "KNRAGVnfjw4").unwrap();
        assert_eq!(sl.title, "HOW I ANIMATE MY FACES IN ROBLOX! | #roblox #tutorial #animation");
        assert_eq!(sl.meta, "402K views");
        assert_eq!(sl.thumb.as_deref(), Some("https://i.ytimg.com/vi/KNRAGVnfjw4/hqdefault.jpg"));
        assert_eq!(sl.thumb_w, 480);
        // Real-shaped continuation: Short videoRenderers too; ordinary videos unchanged.
        let c = continuation(&fixture("search_cont.json"));
        assert_eq!(videos(&c).iter().filter(|v| v.short).count(), 10);
        assert!(videos(&c).iter().filter(|v| !v.short).all(|v| v.duration.contains(':') || v.duration.is_empty()));

        // Home / watch-next shapes: a richShelfRenderer of shortsLockupViewModel
        // (id from the /shorts/ URL only), a reel shelf of reelItemRenderer, a
        // lockup with a SHORTS badge, and a non-Short in a Shorts shelf (dropped, counted).
        let v = json::parse_str(r#"{"contents":{"twoColumnBrowseResultsRenderer":{"tabs":[{"tabRenderer":{"selected":true,"content":{"richGridRenderer":{"contents":[
            {"richSectionRenderer":{"content":{"richShelfRenderer":{"contents":[{"richItemRenderer":{"content":{"shortsLockupViewModel":{
                "onTap":{"innertubeCommand":{"commandMetadata":{"webCommandMetadata":{"url":"/shorts/aaaaaaaaaaa"}}}},
                "overlayMetadata":{"primaryText":{"content":"Home <b>Short</b>"},"secondaryText":{"content":"1.2M views"}}}}}}]}}}},
            {"reelShelfRenderer":{"items":[
                {"reelItemRenderer":{"videoId":"bbbbbbbbbbb","headline":{"simpleText":"Reel"},"viewCountText":{"simpleText":"5K views"},"navigationEndpoint":{"reelWatchEndpoint":{"videoId":"bbbbbbbbbbb"}}}},
                {"reelItemRenderer":{"videoId":"bad"}}]}},
            {"gridShelfViewModel":{"contents":[{"lockupViewModel":{"contentType":"LOCKUP_CONTENT_TYPE_VIDEO","contentId":"ddddddddddd"}}]}},
            {"richItemRenderer":{"content":{"lockupViewModel":{"contentType":"LOCKUP_CONTENT_TYPE_VIDEO","contentId":"ccccccccccc",
                "contentImage":{"thumbnailViewModel":{"overlays":[{"thumbnailBottomOverlayViewModel":{"badges":[{"thumbnailBadgeViewModel":{"text":"SHORTS"}}]}}]}},
                "metadata":{"lockupMetadataViewModel":{"title":{"content":"Lockup Short"}}}}}}},
            {"richItemRenderer":{"content":{"videoRenderer":{"videoId":"eeeeeeeeeee","title":{"simpleText":"Normal"},"lengthText":{"simpleText":"12:00"}}}}}
        ]}}}}]}}}"#).unwrap();
        let f = browse(&v);
        let got: Vec<(&str, &str, &str, bool)> = videos(&f).iter().map(|v| (v.id.as_str(), v.title.as_str(), v.duration.as_str(), v.short)).collect();
        assert_eq!(got, vec![
            ("aaaaaaaaaaa", "Home <b>Short</b>", "Shorts", true),
            ("bbbbbbbbbbb", "Reel", "Shorts", true),
            ("ccccccccccc", "Lockup Short", "Shorts", true),
            ("eeeeeeeeeee", "Normal", "12:00", false),
        ]);
        assert_eq!(videos(&f)[0].meta, "1.2M views");
        assert_eq!(f.stats.shorts, 5, "3 cards + the reel without an id + the non-Short in a Shorts shelf");
        assert!(f.items[0].is_short() && !f.items[3].is_short());
    }

    #[test]
    fn home_cards_carry_their_tracking_ids() {
        // The two ids YouTube's page logs for a shown Home card (live capture, 3 Nov 2026): the grid item's
        // and the lockup's own; an old videoRenderer card has its own `trackingParams`.
        let v = json::parse_str(r#"{"contents":{"twoColumnBrowseResultsRenderer":{"tabs":[{"tabRenderer":{"selected":true,"content":{"richGridRenderer":{"contents":[
            {"richItemRenderer":{"trackingParams":"CL4DEJmNBRgBIhMI","content":{"lockupViewModel":{"contentType":"LOCKUP_CONTENT_TYPE_VIDEO","contentId":"ccccccccccc",
                "rendererContext":{"loggingContext":{"loggingDirectives":{"trackingParams":"CL8DENwwIhMI","visibility":{"types":"12"}}}},
                "metadata":{"lockupMetadataViewModel":{"title":{"content":"Lockup"}}}}}}},
            {"richItemRenderer":{"trackingParams":"CAEQ","content":{"lockupViewModel":{"contentType":"LOCKUP_CONTENT_TYPE_PLAYLIST","contentId":"PLxxxxxxxxxx",
                "rendererContext":{"loggingContext":{"loggingDirectives":{"trackingParams":"CAIQ"}}}}}}},
            {"richItemRenderer":{"trackingParams":"bad\"><x","content":{"videoRenderer":{"videoId":"eeeeeeeeeee","trackingParams":"CAMQ","title":{"simpleText":"Normal"}}}}},
            {"videoRenderer":{"videoId":"fffffffffff","title":{"simpleText":"Bare"}}}
        ]}}}}]}}}"#).unwrap();
        let f = browse(&v);
        assert_eq!(f.track, vec![
            Track { id: "ccccccccccc".into(), item: Some("CL4DEJmNBRgBIhMI".into()), own: Some("CL8DENwwIhMI".into()) },
            Track { id: "eeeeeeeeeee".into(), item: None, own: Some("CAMQ".into()) },
        ]);
    }

    #[test]
    fn search_continuation_fixture() {
        let f = continuation(&fixture("search_cont.json"));
        assert!(videos(&f).len() >= 10);
        assert!(f.cont.is_some());
    }

    #[test]
    fn next_fixture() {
        let v = fixture("next.json");
        let w = next(&v);
        assert_eq!(w.id, "-Bxc4gJluC0");
        assert_eq!(w.title, "How to make SMOOTH animations in ROBLOX STUDIO! [Moon Animator Tutorial]");
        assert_eq!(w.channel, "MonkeyDev");
        assert_eq!(w.channel_href.as_deref(), Some("/@monkeydeveloper"));
        assert_eq!(w.channel_bid.as_deref(), Some("UCaZO-66UcqyVT9btiqdDryA"));
        assert_eq!(w.views, "536,432 views");
        assert_eq!(w.date, "Mar 10, 2023");
        assert_eq!(w.subs, "33.2K subscribers");
        let text: String = w.desc.iter().map(|r| r.text.as_str()).collect();
        assert!(text.starts_with("MOON ANIMATOR: "));
        let ext: Vec<_> = w.desc.iter().filter_map(|r| if let Some(Link::Ext(u)) = &r.link { Some(u.as_str()) } else { None }).collect();
        assert!(ext.iter().any(|u| u.starts_with("https://create.roblox.com/marketplace/asset/4725618216")), "{ext:?}");
        let rel = videos(&w.related);
        assert_eq!(rel.len(), 20);
        assert_eq!(rel[0].id, "UURYhAVph5g");
        assert_eq!(rel[0].duration, "11:29");
        assert_eq!(rel[0].channel, "MonkeyDev");
        assert_eq!(rel[0].channel_href.as_deref(), Some("/@monkeydeveloper"));
        assert_eq!(rel[0].meta, "149K views · 3y ago");
        assert!(w.related.cont.is_some());
    }

    #[test]
    fn next_continuation_fixture() {
        let f = continuation(&fixture("next_cont.json"));
        assert!(videos(&f).len() >= 15);
        assert!(f.cont.is_some());
    }

    #[test]
    fn members_only_cards_in_every_shape() {
        // Real signed-out responses (8 Nov 2026), trimmed: a channel's "Members-only videos" playlist on WEB
        // (lockupViewModel, `metadataRows[].badges[].badgeViewModel`) and on the TV client (tileRenderer,
        // `lines[].lineItemRenderer.badge.metadataBadgeRenderer`).
        let f = fixture("members.json");
        let web = browse(f.get("web_playlist").unwrap());
        let vs = videos(&web);
        assert_eq!(vs.len(), 2);
        assert!(vs.iter().all(|v| v.members), "{vs:?}");
        assert_eq!((vs[0].id.as_str(), vs[0].duration.as_str(), vs[0].channel.as_str()), ("J3M2L2pLhxA", "3:22", "Grey Wiese"));
        assert!(!vs[0].meta.contains("Members"), "the badge isn't meta text: {}", vs[0].meta);
        let tv = browse(f.get("tv_playlist").unwrap());
        let ts = videos(&tv);
        assert_eq!(ts.len(), 2);
        assert!(ts.iter().all(|v| v.members), "{ts:?}");
        assert_eq!((ts[0].id.as_str(), ts[0].duration.as_str()), ("J3M2L2pLhxA", "3:22"));
        // Ordinary recordings carry none (a title may say "members"; only a badge counts).
        for name in ["search.json", "channel_videos.json", "playlist.json", "tv_playlist.json", "tv_shelves.json", "home.json"] {
            let v = fixture(name);
            assert!(videos(&browse(&v)).iter().chain(videos(&search(&v)).iter()).all(|x| !x.members), "{name}");
        }

        // A TV Shorts tile reads the same line.
        let short_tile = crate::json::jv!({"tileRenderer": {"contentType": "TILE_CONTENT_TYPE_SHORTS", "contentId": "aaaaaaaaaaa", "metadata": {"tileMetadataRenderer": {"title": {"simpleText": "t"}, "lines": [
            {"lineRenderer": {"items": [{"lineItemRenderer": {"badge": {"metadataBadgeRenderer": {"style": "BADGE_STYLE_TYPE_MEMBERS_ONLY"}}}}]}}]}}}});
        let mut feed = Feed::default();
        walk_into(&mut feed, &short_tile);
        assert!(videos(&feed)[0].short && videos(&feed)[0].members);

        // `videoRenderer` and its kin: the enum, the icon alone, the label alone; the words of a title never.
        let vr = |badges: &str, title: &str| {
            let text = format!(r#"{{"videoRenderer": {{"videoId": "aaaaaaaaaaa", "title": {{"simpleText": "{title}"}}, "badges": {badges}}}}}"#);
            let mut feed = Feed::default();
            walk_into(&mut feed, &crate::json::parse_str(&text).unwrap());
            videos(&feed)[0].members
        };
        assert!(vr(r#"[{"metadataBadgeRenderer": {"style": "BADGE_STYLE_TYPE_MEMBERS_ONLY", "label": "Только для спонсоров"}}]"#, "x"));
        assert!(vr(r#"[{"metadataBadgeRenderer": {"icon": {"iconType": "SPONSORSHIP_STAR"}, "label": "Какая-то надпись"}}]"#, "x"));
        assert!(vr(r#"[{"metadataBadgeRenderer": {"label": "Nur für Kanalmitglieder"}}]"#, "x"));
        assert!(vr(r#"[{"metadataBadgeRenderer": {"style": "BADGE_STYLE_TYPE_MEMBERS_FIRST", "label": "Members first"}}]"#, "x"), "early access, locked while YouTube says so");
        assert!(!vr("[]", "Members only: a story about members-only videos"));
        assert!(!vr(r#"[{"metadataBadgeRenderer": {"style": "BADGE_STYLE_TYPE_SIMPLE", "label": "4K"}}]"#, "members only"));
        assert!(!vr(r#"[{"metadataBadgeRenderer": {"style": "BADGE_STYLE_TYPE_SIMPLE", "label": "Top members of the month"}}]"#, "x"), "another badge's words");
        // A verified channel's own badge is no members badge (`ownerBadges`, not read).
        let owner = crate::json::jv!({"videoRenderer": {"videoId": "aaaaaaaaaaa", "ownerBadges": [{"metadataBadgeRenderer": {"style": "BADGE_STYLE_TYPE_VERIFIED", "label": "Members only"}}]}});
        let mut feed = Feed::default();
        walk_into(&mut feed, &owner);
        assert!(!videos(&feed)[0].members);

        // A lockup's title, or a metadata part that says it, doesn't count; a thumbnail badge does.
        let lk = |row: &str, thumb: &str, title: &str| {
            let text = format!(r#"{{"lockupViewModel": {{"contentType": "LOCKUP_CONTENT_TYPE_VIDEO", "contentId": "aaaaaaaaaaa",
                "metadata": {{"lockupMetadataViewModel": {{"title": {{"content": "{title}"}}, "metadata": {{"contentMetadataViewModel": {{"metadataRows": [{row}]}}}}}}}},
                "contentImage": {{"thumbnailViewModel": {{"overlays": [{{"thumbnailBottomOverlayViewModel": {{"badges": [{{"thumbnailBadgeViewModel": {{"text": "{thumb}"}}}}]}}}}]}}}}}}}}"#);
            let mut feed = Feed::default();
            walk_into(&mut feed, &crate::json::parse_str(&text).unwrap());
            videos(&feed)[0].members
        };
        assert!(lk(r#"{"badges": [{"badgeViewModel": {"badgeStyle": "BADGE_MEMBERS_ONLY", "badgeText": "Только для спонсоров"}}]}"#, "3:22", "x"));
        assert!(lk(r#"{"badges": [{"badgeViewModel": {"iconName": "SPONSORSHIP_STAR"}}]}"#, "3:22", "x"));
        assert!(lk("{}", "Members only", "x"));
        assert!(!lk(r#"{"metadataParts": [{"text": {"content": "Members only"}}]}"#, "3:22", "Members only"));
        assert!(!lk(r#"{"badges": [{"badgeViewModel": {"badgeStyle": "BADGE_COMMERCE", "badgeText": "Fundraiser"}}]}"#, "3:22", "for members"));
        // Next: a locked target is known from its card.
        let nx = crate::json::jv!({"contents": {"twoColumnWatchNextResults": {"secondaryResults": {"secondaryResults": {"results": [
            {"lockupViewModel": {"contentType": "LOCKUP_CONTENT_TYPE_VIDEO", "contentId": "aaaaaaaaaaa", "metadata": {"lockupMetadataViewModel": {"title": {"content": "a"}, "metadata": {"contentMetadataViewModel": {"metadataRows": [{"badges": [{"badgeViewModel": {"badgeStyle": "BADGE_MEMBERS_ONLY"}}]}]}}}}}},
            {"lockupViewModel": {"contentType": "LOCKUP_CONTENT_TYPE_VIDEO", "contentId": "bbbbbbbbbbb", "metadata": {"lockupMetadataViewModel": {"title": {"content": "b"}}}}}]}}}}});
        let up = next(&nx).up;
        assert_eq!(up.cards, vec![crate::features::upnext::Card { v: "aaaaaaaaaaa".into(), locked: true }, crate::features::upnext::Card { v: "bbbbbbbbbbb".into(), locked: false }]);
    }

    #[test]
    fn tv_client_shapes() {
        // Real TVHTML5 responses (anonymous, 2 Nov 2026): a playlist page (tvBrowseRenderer →
        // twoColumnRenderer → playlistVideoListRenderer of tileRenderer) and shelves of tiles
        // (sectionListRenderer → shelfRenderer → horizontalListRenderer), as signed-in TV Home is built.
        let f = browse(&fixture("tv_playlist.json"));
        let vs = videos(&f);
        assert!(vs.len() == 15, "{} videos", vs.len());
        let v = vs[0];
        assert_eq!(v.id, "fOT0BUpITw8");
        assert_eq!(v.title, "BELLAKEO (Video Oficial) - Peso Pluma, Anitta");
        assert_eq!(v.channel, "Peso Pluma");
        assert_eq!(v.channel_bid.as_deref(), Some("UCzrM_068Odho89mTRrrxqbA"));
        assert_eq!(v.duration, "3:55");
        assert!(v.meta.contains("views"), "{}", v.meta);
        assert_eq!(v.thumb.as_deref(), Some("https://i.ytimg.com/vi_webp/fOT0BUpITw8/mqdefault.webp"));
        assert!(f.cont.is_some(), "playlist continuation");
        assert_eq!(f.stats.unknown_total(), 0, "{:?}", f.stats);
        let f = browse(&fixture("tv_shelves.json"));
        let vs = videos(&f);
        assert!(vs.len() >= 20, "{} videos", vs.len());
        assert!(vs.iter().all(|v| crate::util::is_video_id(&v.id) && !v.title.is_empty()));
        assert!(vs.iter().any(|v| v.short), "Shorts tiles become Shorts");
        assert_eq!(f.cont.as_deref(), Some("TVSECTIONCONT"), "the page continuation, not a row's");
        assert_eq!(f.stats.unknown_total(), 0, "{:?}", f.stats);
        // A TV continuation answer.
        let c = json::parse_str(r#"{"continuationContents":{"sectionListContinuation":{"contents":[{"shelfRenderer":{"content":{"horizontalListRenderer":{"items":[
            {"tileRenderer":{"contentType":"TILE_CONTENT_TYPE_VIDEO","contentId":"aqz-KE-bpKQ","header":{"tileHeaderRenderer":{"thumbnail":{"thumbnails":[{"url":"https://i.ytimg.com/vi/aqz-KE-bpKQ/hq720.jpg","width":1280,"height":720}]},
              "thumbnailOverlays":[{"thumbnailOverlayTimeStatusRenderer":{"text":{"simpleText":"10:35"},"style":"DEFAULT"}},{"thumbnailOverlayResumePlaybackRenderer":{"percentDurationWatched":42}}]}},
              "metadata":{"tileMetadataRenderer":{"title":{"simpleText":"Big Buck Bunny"},"lines":[{"lineRenderer":{"items":[{"lineItemRenderer":{"text":{"runs":[{"text":"Blender","navigationEndpoint":{"browseEndpoint":{"browseId":"UCSMOQeBJ2RAnuFungnQOxLg"}}}]}}}]}},{"lineRenderer":{"items":[{"lineItemRenderer":{"text":{"simpleText":"16M views"}}},{"lineItemRenderer":{"text":{"simpleText":"16 years ago"}}}]}}]}},
              "onSelectCommand":{"watchEndpoint":{"videoId":"aqz-KE-bpKQ"}}}},
            {"tileRenderer":{"contentType":"TILE_CONTENT_TYPE_PLAYLIST","contentId":"PLx","metadata":{"tileMetadataRenderer":{"title":{"simpleText":"Mix"}}},"onSelectCommand":{"browseEndpoint":{"browseId":"VLPLabcdefghij"}}}},
            {"tileRenderer":{"contentType":"TILE_CONTENT_TYPE_CHANNEL","metadata":{"tileMetadataRenderer":{"title":{"simpleText":"Blender"}}},"onSelectCommand":{"browseEndpoint":{"browseId":"UCSMOQeBJ2RAnuFungnQOxLg"}}}}
          ]}}}}],"continuations":[{"nextContinuationData":{"continuation":"NEXT2"}}]}}}"#).unwrap();
        let f = continuation(&c);
        assert_eq!(f.cont.as_deref(), Some("NEXT2"));
        let v = videos(&f)[0];
        assert_eq!((v.progress, v.duration.as_str(), v.channel.as_str(), v.meta.as_str()), (42, "10:35", "Blender", "16M views · 16 years ago"));
        assert_eq!(v.thumb2x.as_deref(), Some("https://i.ytimg.com/vi/aqz-KE-bpKQ/hq720.jpg"));
        assert!(matches!(&f.items[1], Item::Playlist(p) if p.id == "PLabcdefghij"));
        assert!(matches!(&f.items[2], Item::Channel(c) if c.bid.as_deref() == Some("UCSMOQeBJ2RAnuFungnQOxLg")));
        // TV Subscriptions: only the selected side tab is the page.
        let subs = json::parse_str(r#"{"contents":{"tvBrowseRenderer":{"content":{"tvSecondaryNavRenderer":{"sections":[{"tvSecondaryNavSectionRenderer":{"tabs":[
            {"tabRenderer":{"title":"All","selected":true,"content":{"tvSurfaceContentRenderer":{"content":{"gridRenderer":{"items":[{"tileRenderer":{"contentType":"TILE_CONTENT_TYPE_VIDEO","contentId":"aqz-KE-bpKQ","metadata":{"tileMetadataRenderer":{"title":{"simpleText":"A"}}}}}],"continuations":[{"nextContinuationData":{"continuation":"SUBS2"}}]}}}}}},
            {"tabRenderer":{"title":"Channel","content":{"tvSurfaceContentRenderer":{"content":{"gridRenderer":{"items":[{"tileRenderer":{"contentType":"TILE_CONTENT_TYPE_VIDEO","contentId":"-Bxc4gJluC0","metadata":{"tileMetadataRenderer":{"title":{"simpleText":"B"}}}}}]}}}}}}
          ]}}]}}}}}"#).unwrap();
        let f = browse(&subs);
        assert_eq!(videos(&f).iter().map(|v| v.id.as_str()).collect::<Vec<_>>(), ["aqz-KE-bpKQ"]);
        assert_eq!(f.cont.as_deref(), Some("SUBS2"));
    }

    #[test]
    fn channel_fixtures() {
        let c = channel(&fixture("channel_videos.json"));
        assert_eq!(c.title, "MonkeyDev");
        assert_eq!(c.bid.as_deref(), Some("UCaZO-66UcqyVT9btiqdDryA"));
        assert!(c.meta.iter().any(|m| m == "@monkeydeveloper"));
        assert!(c.meta.iter().any(|m| m.contains("subscribers")));
        assert!(c.avatar.is_some());
        let vids = videos(&c.feed);
        assert_eq!(vids.len(), 30);
        assert!(vids.iter().all(|v| !v.duration.is_empty() && v.meta.contains("ago")));
        assert!(c.feed.cont.is_some());
        let more = continuation(&fixture("browse_cont.json"));
        assert_eq!(videos(&more).len(), 30);
        // Home tab (featured) still yields items from shelves without crashing.
        let home = channel(&fixture("channel.json"));
        assert!(!home.feed.items.is_empty());
        assert_eq!(resolve(&fixture("resolve.json")).as_deref(), Some("UCaZO-66UcqyVT9btiqdDryA"));
    }

    #[test]
    fn playlist_fixture() {
        let p = playlist(&fixture("playlist.json"));
        assert_eq!(p.title, "Roblox Animation Tutorials");
        assert!(p.meta.iter().any(|m| m.contains("MonkeyDev")));
        let vids = videos(&p.feed);
        assert_eq!(vids.len(), 6);
        assert_eq!(vids[0].id, "-Bxc4gJluC0");
        assert_eq!(vids[0].duration, "5:03");
        assert!(p.feed.cont.is_some());
    }

    #[test]
    fn home_signed_out_nudge() {
        let f = browse(&fixture("home.json"));
        assert!(f.items.is_empty());
        assert_eq!(f.nudge.as_deref(), Some("Your YouTube history is off"));
        let c = ctx(&fixture("home.json"));
        assert_eq!(c.logged_out, Some(true));
        assert!(c.visitor.is_some());
    }

    #[test]
    fn unknown_items_are_counted_not_fatal() {
        let v = json::parse_str(r#"{"contents":{"twoColumnBrowseResultsRenderer":{"tabs":[{"tabRenderer":{"selected":true,"content":{"richGridRenderer":{"contents":[
            {"richItemRenderer":{"content":{"fancyNewRenderer":{"x":1}}}},
            {"richItemRenderer":{"content":{"videoRenderer":{"videoId":"bad"}}}},
            {"richItemRenderer":{"content":{"videoRenderer":{"videoId":"abcdefghijk","title":{"runs":[{"text":"<b>x</b>"}]}}}}},
            {"richItemRenderer":{"content":{"videoRenderer":{"videoId":"abcdefghijk","title":{"simpleText":"dup"}}}}},
            {"richItemRenderer":{"content":{"lockupViewModel":{"contentType":"LOCKUP_CONTENT_TYPE_SOMETHING","contentId":"x1"}}}},
            {"richItemRenderer":{"content":{"shortsLockupViewModel":{}}}},
            {"adSlotRenderer":{}},
            {"richSectionRenderer":{"content":{"richShelfRenderer":{"contents":[{"richItemRenderer":{"content":{"shortsLockupViewModel":{}}}}]}}}},
            {"continuationItemRenderer":{"continuationEndpoint":{"continuationCommand":{"token":"TOK"}}}}
        ]}}}}]}}}"#).unwrap();
        let f = browse(&v);
        assert_eq!(f.items.len(), 1);
        assert_eq!(videos(&f)[0].title, "<b>x</b>");
        assert_eq!(f.stats.shorts, 2);
        assert_eq!(f.stats.ads, 1);
        assert_eq!(f.stats.dupes, 1);
        assert!(f.stats.unknown.iter().any(|(k, n)| k == "fancyNewRenderer" && *n == 1));
        assert!(f.stats.unknown.iter().any(|(k, _)| k == "lockupViewModel:SOMETHING"));
        assert!(!f.stats.unknown.iter().any(|(k, _)| k == "lockupViewModel"), "counted once");
        assert!(f.stats.unknown.iter().any(|(k, _)| k == "videoRenderer"));
        assert_eq!(f.cont.as_deref(), Some("TOK"));
        // Garbage never panics.
        for junk in ["null", "[]", "{\"contents\":5}", "{\"contents\":{\"twoColumnWatchNextResults\":[]}}"] {
            let j = json::parse_str(junk).unwrap();
            let _ = (browse(&j), search(&j), next(&j), channel(&j), playlist(&j), continuation(&j), resolve(&j));
        }
    }

    #[test]
    fn description_links_are_sanitized() {
        let v = json::parse_str(r#"{"attributedDescription":{"content":"😀 go js 0:30 tag","commandRuns":[
            {"startIndex":3,"length":2,"onTap":{"innertubeCommand":{"urlEndpoint":{"url":"https://www.youtube.com/redirect?q=https%3A%2F%2Fexample.com%2F%3Fa%3D1"}}}},
            {"startIndex":6,"length":2,"onTap":{"innertubeCommand":{"urlEndpoint":{"url":"javascript:alert(1)"}}}},
            {"startIndex":9,"length":4,"onTap":{"innertubeCommand":{"watchEndpoint":{"videoId":"abcdefghijk","startTimeSeconds":30}}}},
            {"startIndex":14,"length":3,"onTap":{"innertubeCommand":{"commandMetadata":{"webCommandMetadata":{"url":"/hashtag/tag"}},"browseEndpoint":{"browseId":"FEhashtag"}}}}
        ]}}"#).unwrap();
        let runs = description(&v, "abcdefghijk");
        let texts: Vec<_> = runs.iter().map(|r| (r.text.as_str(), r.link.clone())).collect();
        assert_eq!(texts[0], ("😀 ", None));
        assert_eq!(texts[1], ("go", Some(Link::Ext("https://example.com/?a=1".into()))));
        // javascript: link dropped → plain text (merged into surrounding text runs).
        assert!(runs.iter().all(|r| !matches!(&r.link, Some(Link::Ext(u)) if u.starts_with("javascript"))));
        assert!(runs.iter().any(|r| r.text == "0:30" && r.link == Some(Link::Seek(30))));
        assert!(runs.iter().any(|r| r.text == "tag" && r.link == Some(Link::Int("/hashtag/tag".into()))));
        let all: String = runs.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(all, "😀 go js 0:30 tag");
    }

    // ------------------------------------------------- channel tabs (real, trimmed answers, Nov 2026)

    fn ct(key: &str) -> Value<'static> {
        fixture("channel_tabs.json").get(key).expect(key).clone().into_owned()
    }

    fn slugs(c: &ChannelPage) -> Vec<&'static str> {
        c.tabs.iter().map(|t| t.tab.slug()).collect()
    }

    #[test]
    fn channel_tabs_come_from_the_response() {
        let c = channel(&ct("sky.videos"));
        assert_eq!(slugs(&c), ["videos", "shorts", "streams", "shows", "podcasts", "playlists"], "Home, Posts and Search stay out");
        assert_eq!(c.selected.as_deref(), Some("videos"));
        assert_eq!(c.tabs[2].title, "Live", "YouTube's own title");
        assert!(c.tabs.iter().all(|t| crate::route::valid_sp(&t.params)));
        // A channel without live streams or Shorts lists none.
        let c = channel(&ct("arthas.videos"));
        assert_eq!(slugs(&c), ["videos", "streams", "playlists"]);
        // Asked for a tab it lacks, YouTube answers with its Home.
        let c = channel(&ct("beast.streams"));
        assert_eq!(c.selected.as_deref(), Some("featured"));
        assert_eq!(slugs(&c), ["videos", "shorts", "shows"]);
        // Only Home and Posts: no tab the client renders.
        let c = channel(&ct("f1.videos"));
        assert_eq!(c.selected.as_deref(), Some("featured"));
        assert!(c.tabs.is_empty());
        // A kind of tab the client cannot show is not one of its tabs (Playables, Store).
        let c = channel(&ct("veritasium.playables"));
        assert_eq!(c.selected.as_deref(), Some("playables"));
        assert!(!c.tabs.is_empty() && c.tabs.iter().all(|t| !matches!(t.tab.slug(), "playables" | "store" | "posts")));
        assert!(c.feed.items.is_empty());
    }

    #[test]
    fn channel_tab_identity_does_not_depend_on_the_words() {
        // The same answer in Russian: the titles follow it, the tabs and sort choices stay what they are.
        let raw = String::from_utf8(crate::util::fixture_bytes("channel_tabs.json")).unwrap();
        let ru = raw.replace("\"title\":\"Videos\"", "\"title\":\"Видео\"").replace("\"title\":\"Live\"", "\"title\":\"Трансляции\"").replace("\"title\":\"Playlists\"", "\"title\":\"Плейлисты\"").replace("\"text\":\"Latest\"", "\"text\":\"Новые\"").replace("\"text\":\"Popular\"", "\"text\":\"Популярные\"").replace("\"text\":\"Oldest\"", "\"text\":\"Старые\"");
        assert_ne!(ru, raw);
        let doc = crate::json::parse(ru.as_bytes()).unwrap();
        let c = channel(doc.get("sky.videos").unwrap());
        assert_eq!(slugs(&c), ["videos", "shorts", "streams", "shows", "podcasts", "playlists"]);
        assert_eq!(c.selected.as_deref(), Some("videos"));
        let title = |slug: &str| c.tabs.iter().find(|t| t.tab.slug() == slug).map(|t| t.title.clone()).unwrap();
        assert_eq!((title("videos").as_str(), title("streams").as_str(), title("playlists").as_str()), ("Видео", "Трансляции", "Плейлисты"));
        assert_eq!(c.sorts.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Новые", "Популярные", "Старые"]);
        assert!(c.sorts[0].selected && c.sorts.iter().all(|o| o.token.is_some()));
        // YouTube's Russian label for its members chip never becomes a sort.
        assert!(members_chip("Только для спонсоров") && members_chip("Members only") && !members_chip("Популярные"));
    }

    #[test]
    fn channel_sorts_from_both_chip_shapes() {
        let labels = |v: &[SortOpt]| v.iter().map(|o| o.label.clone()).collect::<Vec<_>>();
        // Three chips, each with its token.
        let sorts = channel_sorts(&ct("sky.videos"));
        assert_eq!(labels(&sorts), ["Latest", "Popular", "Oldest"]);
        assert!(sorts[0].selected && !sorts[1].selected);
        assert!(sorts.iter().all(|o| o.token.is_some() && o.params.is_none()));
        // A channel with members-only videos: the three in a drop-down's sheet, "Members only" and "Public" stay out.
        let sorts = channel_sorts(&ct("arthas.videos"));
        assert_eq!(labels(&sorts), ["Latest", "Popular", "Oldest"]);
        assert!(sorts[0].selected && sorts.iter().all(|o| o.token.is_some()));
        // A sort's answer: the header slot says which is selected now, the body slot holds the cards.
        for key in ["sky.videos.popular", "arthas.videos.popular"] {
            let a = ct(key);
            let sorts = channel_sorts(&a);
            assert_eq!(sorts.iter().find(|o| o.selected).map(|o| o.label.as_str()), Some("Popular"), "{key}");
            let f = continuation(&a);
            assert!(f.items.len() >= 10 && f.cont.is_some(), "{key}: {} cards", f.items.len());
            assert_eq!(f.stats.unknown_total(), 0, "{key}: {:?}", f.stats.unknown);
        }
        // Playlists' sort menu: choices with `browse` params.
        let sorts = channel_sorts(&ct("arthas.playlists"));
        assert_eq!(labels(&sorts), ["Date added (newest)", "Last video added"]);
        assert!(sorts[0].selected && sorts.iter().all(|o| o.params.is_some() && o.token.is_none()));
        let sorts = channel_sorts(&ct("arthas.playlists.last"));
        assert_eq!(sorts.iter().find(|o| o.selected).map(|o| o.label.as_str()), Some("Last video added"));
        // Nothing to choose: no row. A tab with no chip bar at all (Numberphile's Shorts and Live, Nov 2026).
        assert!(channel_sorts(&ct("sky.shows")).is_empty());
        for key in ["numberphile.shorts", "numberphile.streams"] {
            let c = channel(&ct(key));
            assert!(c.sorts.is_empty() && !c.feed.items.is_empty(), "{key}: {} sorts, {} cards", c.sorts.len(), c.feed.items.len());
        }
    }

    #[test]
    fn channel_tab_cards() {
        // Live: live now, upcoming and past streams; none is dropped.
        let f = channel(&ct("sky.streams")).feed;
        let vids: Vec<&Video> = f.items.iter().filter_map(|i| if let Item::Video(v) = i { Some(v) } else { None }).collect();
        assert!(vids.len() >= 8, "{}", vids.len());
        assert!(vids.iter().any(|v| v.live && v.meta.contains("watching")), "a live stream");
        let up = vids.iter().find(|v| v.duration == "Upcoming").expect("an upcoming stream, labelled by YouTube");
        assert!(up.meta.contains("Scheduled for") && !up.live);
        assert!(vids.iter().any(|v| !v.live && v.meta.contains("Streamed")), "a past stream");
        assert!(f.cont.is_some());
        // Shorts: vertical art and views.
        let f = channel(&ct("beast.shorts")).feed;
        let Some(Item::Video(s)) = f.items.first() else { panic!("a Short") };
        assert!(s.short && s.art.as_deref().is_some_and(|a| a.contains("sardefault")), "{:?}", s.art);
        assert!(s.meta.contains("views") && !s.title.is_empty());
        assert!(f.cont.is_some());
        // Playlists: a count and the page to open.
        let f = channel(&ct("arthas.playlists")).feed;
        let Some(Item::Playlist(p)) = f.items.first() else { panic!("a playlist") };
        assert!(p.count.contains("video") && p.id.starts_with("PL"), "{p:?}");
        assert!(f.cont.is_some());
        // Podcasts, Courses, Releases and Shows are playlists with a count.
        for key in ["sky.podcasts", "cc.courses", "lofi.releases", "sky.shows"] {
            let f = channel(&ct(key)).feed;
            let Some(Item::Playlist(p)) = f.items.first() else { panic!("{key}: {:?}", f.items.first()) };
            assert!(!p.id.is_empty() && !p.title.is_empty() && p.thumb.is_some(), "{key}: {p:?}");
            assert!(!p.count.is_empty(), "{key}: {p:?}");
            assert_eq!(f.stats.unknown_total(), 0, "{key}: {:?}", f.stats.unknown);
        }
    }

    #[test]
    fn channel_about_loses_its_list_mark() {
        assert_eq!(about_line("• https://t.me/x  - text"), "https://t.me/x - text");
        assert_eq!(about_line("  \u{2022}\u{a0}· ▪ text"), "text");
        assert_eq!(about_line("- a list"), "a list");
        assert_eq!(about_line("-5 degrees"), "-5 degrees", "a minus sign is no list mark");
        assert_eq!(about_line("Plain line."), "Plain line.");
        assert_eq!(about_line(" • "), "");
        let about = channel(&ct("arthas.videos")).about;
        assert!(about.starts_with("https://t.me/"), "{about}");
        assert_eq!(channel(&ct("sky.videos")).about, "The full story, first.");
        assert_eq!(channel(&ct("f1.videos")).about, "", "no description: no line");
    }
}
