//! InnerTube request construction (same-origin `fetch`, cookies included by
//! the browser). Response parsing lives in `parse.rs`.

use crate::config::{Auth, Config, ORIGIN};
use serde_json::{Map, Value, json};

/// Channel "Videos" tab params (stable for years: `EgZ2aWRlb3PyBgQKAjoA`).
pub const CHANNEL_VIDEOS_PARAMS: &str = "EgZ2aWRlb3PyBgQKAjoA";

#[derive(Clone, Debug, PartialEq)]
pub enum Req {
    Browse { id: String, params: Option<String> },
    /// YouTube's own Watch later sort-menu command (values supplied by the page).
    WatchLaterOrder { order: u32, params: Option<String> },
    /// Watch later's first page in English, read only for its sort menu (parsed by its English
    /// label) when the page itself goes out in another language.
    WatchLaterMenu,
    /// `params` = YouTube's own search-filter value (verbatim).
    Search { q: String, params: Option<String> },
    Next { v: String },
    /// `next` inside a playlist (`list=`): the response adds the playlist panel.
    NextList { v: String, list: String },
    Resolve { url: String },
    Cont { ep: &'static str, token: String },
}

/// The WEB `context` object every InnerTube body carries.
pub fn context(cfg: &Config, hl: &str, page_url: &str) -> Value {
    let mut client = Map::new();
    client.insert("clientName".into(), "WEB".into());
    client.insert("clientVersion".into(), cfg.version.clone().into());
    client.insert("hl".into(), hl.into());
    client.insert("platform".into(), "DESKTOP".into());
    client.insert("originalUrl".into(), format!("{ORIGIN}{page_url}").into());
    if let Some(vd) = &cfg.visitor {
        client.insert("visitorData".into(), vd.clone().into());
    }
    json!({"client": Value::Object(client), "user": {"lockedSafetyMode": false}})
}

impl Req {
    pub fn endpoint(&self) -> &'static str {
        match self {
            Req::Browse { .. } | Req::WatchLaterMenu => "browse",
            Req::WatchLaterOrder { .. } => "browse/edit_playlist",
            Req::Search { .. } => "search",
            Req::Next { .. } | Req::NextList { .. } => "next",
            Req::Resolve { .. } => "navigation/resolve_url",
            Req::Cont { ep, .. } => ep,
        }
    }

    pub fn url(&self) -> String {
        format!("{ORIGIN}/youtubei/v1/{}?prettyPrint=false", self.endpoint())
    }

    /// The URL as sent: a watch page's `next` carries the field mask (`NEXT_FIELDS`) while
    /// YouTube accepts it (`masked`).
    pub fn send_url(&self) -> String {
        match self {
            Req::Next { .. } | Req::NextList { .. } if masked() => format!("{}&fields={}", self.url(), crate::util::pct_encode(NEXT_FIELDS)),
            _ => self.url(),
        }
    }

    pub fn body(&self, cfg: &Config, hl: &str, page_url: &str) -> String {
        let mut body = Map::new();
        // The owned playlist's sort menu is parsed by its English label (the menu check and the sort
        // command go out in English); the page itself is in the user's language. The order value and
        // context params still come from YouTube, never from a locally reversed chunk.
        let en = matches!(self, Req::WatchLaterMenu | Req::WatchLaterOrder { .. });
        body.insert("context".into(), context(cfg, if en { "en" } else { hl }, page_url));
        match self {
            Req::WatchLaterMenu => {
                body.insert("browseId".into(), "VLWL".into());
            }
            Req::Browse { id, params } => {
                body.insert("browseId".into(), id.clone().into());
                if let Some(p) = params {
                    body.insert("params".into(), p.clone().into());
                }
            }
            Req::WatchLaterOrder { order, params } => {
                body.insert("playlistId".into(), "WL".into());
                body.insert("actions".into(), json!([{"action":"ACTION_SET_PLAYLIST_VIDEO_ORDER","playlistVideoOrder":order}]));
                if let Some(p) = params { body.insert("params".into(), p.clone().into()); }
            }
            Req::Search { q, params } => {
                body.insert("query".into(), q.clone().into());
                if let Some(p) = params {
                    body.insert("params".into(), p.clone().into());
                }
            }
            Req::Next { v } => {
                body.insert("videoId".into(), v.clone().into());
                body.insert("racyCheckOk".into(), true.into());
                body.insert("contentCheckOk".into(), true.into());
            }
            Req::NextList { v, list } => {
                body.insert("videoId".into(), v.clone().into());
                body.insert("playlistId".into(), list.clone().into());
                body.insert("racyCheckOk".into(), true.into());
                body.insert("contentCheckOk".into(), true.into());
            }
            Req::Resolve { url } => {
                body.insert("url".into(), url.clone().into());
            }
            Req::Cont { token, .. } => {
                body.insert("continuation".into(), token.clone().into());
            }
        }
        Value::Object(body).to_string()
    }
}

/// Related items as the watch page reads them: lockups trimmed to what `parse::lockup` uses (their
/// menus, palettes and logging are ~60 % of a `next` answer), every other renderer the column can
/// carry whole. Only names YouTube knows today: an unknown name is an HTTP 400 for the whole request.
macro_rules! next_rel {
    () => {
        "lockupViewModel(contentId,contentType,contentImage,metadata/lockupMetadataViewModel(title,image,metadata),rendererContext(commandContext,loggingContext/loggingDirectives/trackingParams)),\
         compactVideoRenderer,compactRadioRenderer,compactPlaylistRenderer,continuationItemRenderer,reelShelfRenderer,shelfRenderer"
    };
}

/// The `next` field mask (3 Nov 2026, measured live signed out on 22 videos: the decoded answer
/// 380–450 KB → ~100 KB, gzip on the wire 55–65 KB → ~20 KB; YouTube's server time unchanged). It
/// keeps everything `parse::next`, `actions::watch::parse`, `features::upnext::parse`, `parse::ctx`
/// and the topbar read (a unit test parses fixtures masked and whole and compares); what it drops
/// is never rendered: the end screen, the structured-description panel, ticket and merchandise
/// shelves, the lockups' menus and colour palettes, tracking (except each lockup's own tracking id: the "shown" signals).
pub const NEXT_FIELDS: &str = concat!(
    "responseContext,currentVideoEndpoint,topbar,frameworkUpdates,",
    "contents/twoColumnWatchNextResults(",
    "results/results/contents(videoPrimaryInfoRenderer(title,viewCount,dateText,relativeDateText,videoActions/menuRenderer/topLevelButtons/segmentedLikeDislikeButtonViewModel),",
    "videoSecondaryInfoRenderer(owner,subscribeButton,attributedDescription,description),itemSectionRenderer),",
    "secondaryResults/secondaryResults/results(",
    next_rel!(),
    ",itemSectionRenderer(sectionIdentifier,targetId,contents(",
    next_rel!(),
    "))),playlist,autoplay),",
    "engagementPanels/engagementPanelSectionListRenderer(panelIdentifier,header,content(sectionListRenderer,macroMarkersListRenderer)),",
    "playerOverlays/playerOverlayRenderer/decoratedPlayerBarRenderer"
);

static UNMASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `next` goes out with `NEXT_FIELDS` (until YouTube refused it once in this document).
pub fn masked() -> bool {
    !UNMASKED.load(std::sync::atomic::Ordering::Relaxed)
}

/// YouTube answered a masked request with HTTP 400 (a field it no longer knows): whole answers from now on.
pub fn unmask() {
    UNMASKED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The window, as YouTube's web app describes it in its requests (`bridge::viewport`).
#[derive(Clone, Debug, PartialEq)]
pub struct Viewport {
    pub w: u32,
    pub h: u32,
    pub dpr: f64,
    pub dark: bool,
    pub share: bool,
}

impl Viewport {
    pub fn from_json(s: &str) -> Viewport {
        let v = crate::json::parse_str(s);
        let n = |k: &str| v.as_ref().and_then(|v| v.get(k)).and_then(crate::json::Value::as_f64).filter(|x| x.is_finite() && *x > 0.0);
        let b = |k: &str| v.as_ref().and_then(|v| v.get(k)).and_then(crate::json::Value::as_bool).unwrap_or(false);
        Viewport { w: n("w").unwrap_or(1280.0).min(20_000.0) as u32, h: n("h").unwrap_or(800.0).min(20_000.0) as u32, dpr: n("dpr").unwrap_or(1.0).min(8.0), dark: b("dark"), share: b("share") }
    }
}

/// The Home request (`browse FEwhat_to_watch`) as YouTube's web app sends it for its Home: the
/// usual context plus how the window looks (size, pixel density, form factor, theme, app state),
/// for the Home URL wherever it is sent from. Measured live (tools/bench/home-parity.cjs, 3 Nov
/// 2026, signed out): without these fields YouTube's answers to Redline shared 37–40 % of their top 24
/// with the answers to YouTube's own context (vs 44–53 % between two of YouTube's own) and carried
/// its sticky picks less often (61–67 % vs 74–87 %); with them 49 % and 78 %.
pub fn home_body(cfg: &Config, hl: &str, vp: &Viewport) -> String {
    let mut body = Map::new();
    let mut ctx = context(cfg, hl, "/");
    if let Some(Value::Object(c)) = ctx.get_mut("client") {
        c.insert("screenWidthPoints".into(), vp.w.into());
        c.insert("screenHeightPoints".into(), vp.h.into());
        c.insert("windowWidthPoints".into(), vp.w.into());
        c.insert("screenPixelDensity".into(), (vp.dpr.round().max(1.0) as u32).into());
        c.insert("screenDensityFloat".into(), json!(vp.dpr));
        c.insert("clientFormFactor".into(), "UNKNOWN_FORM_FACTOR".into());
        c.insert("userInterfaceTheme".into(), if vp.dark { "USER_INTERFACE_THEME_DARK" } else { "USER_INTERFACE_THEME_LIGHT" }.into());
        c.insert("applicationState".into(), "ACTIVE".into());
        c.insert("mainAppWebInfo".into(), json!({"graftUrl": "/", "webDisplayMode": "WEB_DISPLAY_MODE_BROWSER", "isWebNativeShareAvailable": vp.share}));
    }
    body.insert("context".into(), ctx);
    body.insert("browseId".into(), "FEwhat_to_watch".into());
    Value::Object(body).to_string()
}

/// The VISIONOS (Apple Vision Pro app) InnerTube client: its `player`
/// response has direct googlevideo URLs without cipher, `n` or PO token
/// (found by research on 1 Nov 2026; see docs/architecture.md "Player"). Kept as data so versions can be bumped.
pub const VISION_NAME: &str = "VISIONOS";
pub const VISION_ID: &str = "101";
pub const VISION_VERSION: &str = "1.02";
const VISION_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

pub fn player_url() -> String {
    format!("{ORIGIN}/youtubei/v1/player?prettyPrint=false")
}

/// All VISIONOS consumers use these groups. Keep each subtree whole, including every format,
/// codec, audio track and caption option; WEB's tracking/storyboards call remains unmasked.
pub const VISION_FIELDS: &str = "responseContext,playabilityStatus,streamingData,captions,videoDetails";
static VISION_UNMASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn vision_url() -> String {
    let url = player_url();
    if VISION_UNMASKED.load(std::sync::atomic::Ordering::Relaxed) {
        url
    } else {
        format!("{url}&fields={}", crate::util::pct_encode(VISION_FIELDS))
    }
}

/// A refused VISIONOS mask disables only that mask for this document.
pub fn unmask_vision() {
    VISION_UNMASKED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// VISIONOS `player` body and headers. `hl` is always "en" so playability
/// reasons (e.g. the bot check) are recognisable; metadata comes from WEB.
pub fn vision_player(cfg: &Config, video_id: &str) -> (String, String) {
    let mut client = json!({"clientName": VISION_NAME, "clientVersion": VISION_VERSION, "deviceMake": "Apple", "deviceModel": "RealityDevice17,1",
        "osName": "visionOS", "osVersion": "26.5.23O471", "userAgent": VISION_UA, "hl": "en"});
    let mut h = Map::new();
    h.insert("Content-Type".into(), "application/json".into());
    h.insert("X-Youtube-Client-Name".into(), VISION_ID.into());
    h.insert("X-Youtube-Client-Version".into(), VISION_VERSION.into());
    if let Some(vd) = &cfg.visitor {
        client["visitorData"] = vd.clone().into();
        h.insert("X-Goog-Visitor-Id".into(), vd.clone().into());
    }
    let body = json!({"context": {"client": client}, "videoId": video_id, "contentCheckOk": true, "racyCheckOk": true});
    (body.to_string(), Value::Object(h).to_string())
}

/// How the VISIONOS `player` call is sent. The default is anonymous with the
/// configured visitor; after a bot check the player tries the others and the
/// app keeps the one that worked for the rest of the session (memory only).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum VisionRoute {
    /// Credentials omitted, the configured visitor (the happy path).
    #[default]
    Anon,
    /// Credentials omitted, a freshly minted anonymous visitor (`visitor_id`).
    Fresh(String),
    /// With the account's session: cookies plus the SAPISIDHASH headers.
    Account,
}

impl VisionRoute {
    /// Short name for notes and incident reports (never the visitor itself).
    pub fn label(&self) -> &'static str {
        match self {
            VisionRoute::Anon => "anonymous",
            VisionRoute::Fresh(_) => "fresh visitor",
            VisionRoute::Account => "account session",
        }
    }
}

/// A VISIONOS `player` request for `route`: body, headers and fetch options.
/// `Anon` is byte-for-byte the plain `vision_player` request.
pub fn vision_request(cfg: &Config, auth: &Auth, ts_secs: u64, route: &VisionRoute, video_id: &str) -> (String, String, &'static str) {
    const OMIT: &str = "{\"credentials\":\"omit\"}";
    match route {
        VisionRoute::Anon => {
            let (b, h) = vision_player(cfg, video_id);
            (b, h, OMIT)
        }
        VisionRoute::Fresh(vd) => {
            let mut c = cfg.clone();
            c.visitor = Some(vd.clone());
            let (b, h) = vision_player(&c, video_id);
            (b, h, OMIT)
        }
        VisionRoute::Account => {
            let (b, h) = vision_player(cfg, video_id);
            let mut h: Value = serde_json::from_str(&h).unwrap_or_default();
            h["X-Origin"] = ORIGIN.into();
            h["X-Goog-AuthUser"] = cfg.auth_user.to_string().into();
            if let Some(a) = auth.header(ts_secs) {
                h["Authorization"] = a.into();
            }
            (b, h.to_string(), "{}")
        }
    }
}

/// `visitor_id`: mints a new anonymous visitor (`responseContext.visitorData`),
/// sent without cookies or visitor so it is not tied to this browser's.
pub fn visitor_id_url() -> String {
    format!("{ORIGIN}/youtubei/v1/visitor_id?prettyPrint=false")
}

pub fn visitor_id_request(cfg: &Config) -> (String, String) {
    let body = json!({"context": {"client": {"clientName": "WEB", "clientVersion": cfg.version, "hl": "en"}}});
    let h = json!({"Content-Type": "application/json", "X-Youtube-Client-Name": "1", "X-Youtube-Client-Version": cfg.version});
    (body.to_string(), h.to_string())
}

/// The visitor in a `visitor_id` (or any InnerTube) response.
pub fn parse_visitor(bytes: &[u8]) -> Option<String> {
    let v = crate::json::parse(bytes)?;
    v.pointer("/responseContext/visitorData").and_then(crate::json::Value::as_str).filter(|x| crate::config::is_visitor(x)).map(Into::into)
}

/// WEB `player` body (with the user's session headers from `headers`): used
/// for metadata, captions and `playbackTracking`; its SABR streams are ignored.
pub fn web_player(cfg: &Config, hl: &str, page_url: &str, video_id: &str, cpn: &str, sts: u32) -> String {
    let mut b: Value = crate::json::parse_str(&Req::Next { v: video_id.into() }.body(cfg, hl, page_url)).map(|v| v.to_serde()).unwrap_or_default();
    b["cpn"] = cpn.into();
    b["playbackContext"] = json!({"contentPlaybackContext": {"html5Preference": "HTML5_PREF_WANTS", "signatureTimestamp": sts,
        "referer": format!("{ORIGIN}{page_url}"), "currentUrl": page_url, "autonavState": "STATE_NONE", "vis": 0, "splay": false,
        "autoCaptionsDefaultOn": false, "lactMilliseconds": "-1"}});
    b.to_string()
}

/// Request headers as a JSON object for the bridge.
pub fn headers(cfg: &Config, auth: &Auth, ts_secs: u64) -> String {
    let mut h = Map::new();
    h.insert("Content-Type".into(), "application/json".into());
    h.insert("X-Youtube-Client-Name".into(), "1".into());
    h.insert("X-Youtube-Client-Version".into(), cfg.version.clone().into());
    h.insert("X-Origin".into(), ORIGIN.into());
    h.insert("X-Goog-AuthUser".into(), cfg.auth_user.to_string().into());
    if let Some(vd) = &cfg.visitor {
        h.insert("X-Goog-Visitor-Id".into(), vd.clone().into());
    }
    if let Some(a) = auth.header(ts_secs) {
        h.insert("Authorization".into(), a.into());
    }
    Value::Object(h).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn body_shape() {
        let cfg = Config::default();
        let b: Value = serde_json::from_str(&Req::Search { q: "a\"b".into(), params: None }.body(&cfg, "en", "/results?search_query=a")).unwrap();
        assert_eq!(b["query"], "a\"b");
        assert!(b.get("params").is_none());
        let b: Value = serde_json::from_str(&Req::Search { q: "a".into(), params: Some("EgIIAw%3D%3D".into()) }.body(&cfg, "en", "/")).unwrap();
        assert_eq!(b["params"], "EgIIAw%3D%3D", "YouTube's params verbatim");
        let b: Value = serde_json::from_str(&Req::NextList { v: "-Bxc4gJluC0".into(), list: "PLx".into() }.body(&cfg, "en", "/")).unwrap();
        assert_eq!((b["videoId"].as_str(), b["playlistId"].as_str()), (Some("-Bxc4gJluC0"), Some("PLx")));
        assert_eq!(b["context"]["client"]["clientName"], "WEB");
        assert_eq!(b["context"]["client"]["clientVersion"], crate::config::FALLBACK_VERSION);
        let h: Value = serde_json::from_str(&headers(&cfg, &Auth::from_cookies("SAPISID=x"), 1)).unwrap();
        assert_eq!(h["X-Youtube-Client-Name"], "1");
        assert!(h["Authorization"].as_str().unwrap().starts_with("SAPISIDHASH 1_"));
        let h: Value = serde_json::from_str(&headers(&cfg, &Auth::default(), 1)).unwrap();
        assert!(h.get("Authorization").is_none());
    }
    /// A `fields=` mask applied the way InnerTube applies it (arrays are transparent; a name
    /// without a sub-selection keeps its whole subtree).
    #[derive(Default, Debug)]
    struct Mask(std::collections::BTreeMap<String, Mask>);

    fn mask_parse(s: &str) -> Mask {
        fn list(b: &[u8], i: &mut usize, m: &mut Mask) {
            loop {
                item(b, i, m);
                if *i < b.len() && b[*i] == b',' {
                    *i += 1;
                } else {
                    return;
                }
            }
        }
        fn item(b: &[u8], i: &mut usize, m: &mut Mask) {
            let start = *i;
            while *i < b.len() && (b[*i].is_ascii_alphanumeric() || b[*i] == b'_' || b[*i] == b'*') {
                *i += 1;
            }
            let name = std::str::from_utf8(&b[start..*i]).unwrap().to_string();
            assert!(!name.is_empty(), "mask syntax at {start}");
            let sub = m.0.entry(name).or_default();
            if *i < b.len() && b[*i] == b'/' {
                *i += 1;
                item(b, i, sub);
            } else if *i < b.len() && b[*i] == b'(' {
                *i += 1;
                list(b, i, sub);
                assert_eq!(b.get(*i), Some(&b')'), "unbalanced mask");
                *i += 1;
            }
        }
        let mut m = Mask::default();
        let mut i = 0;
        list(s.as_bytes(), &mut i, &mut m);
        assert_eq!(i, s.len(), "mask parsed to the end");
        m
    }

    fn mask_apply(m: &Mask, v: &Value) -> Value {
        if m.0.is_empty() {
            return v.clone();
        }
        match v {
            Value::Object(o) => Value::Object(o.iter().filter_map(|(k, x)| m.0.get(k).map(|sub| (k.clone(), mask_apply(sub, x)))).collect()),
            Value::Array(a) => Value::Array(a.iter().map(|x| mask_apply(m, x)).collect()),
            x => x.clone(),
        }
    }

    /// Whole and masked parse alike: everything the watch page reads survives `NEXT_FIELDS`.
    fn masked_parses_alike(full: &Value) -> (usize, usize) {
        let m = mask_parse(NEXT_FIELDS);
        let masked = mask_apply(&m, full);
        let (a, b) = (full.to_string(), masked.to_string());
        let wa = crate::parse::next(&crate::json::parse_str(&a).unwrap());
        let wb = crate::parse::next(&crate::json::parse_str(&b).unwrap());
        assert_eq!(format!("{wa:?}"), format!("{wb:?}"));
        let ca = crate::parse::ctx(&crate::json::parse_str(&a).unwrap());
        let cb = crate::parse::ctx(&crate::json::parse_str(&b).unwrap());
        assert_eq!(format!("{ca:?}"), format!("{cb:?}"));
        let ta = crate::actions::notify::topbar(&crate::json::parse_str(&a).unwrap());
        let tb = crate::actions::notify::topbar(&crate::json::parse_str(&b).unwrap());
        assert_eq!(format!("{ta:?}"), format!("{tb:?}"));
        (a.len(), b.len())
    }

    #[test]
    fn next_mask_keeps_what_the_watch_page_reads() {
        let m = mask_parse(NEXT_FIELDS);
        assert!(["contents", "engagementPanels", "playerOverlays", "responseContext"].iter().all(|k| m.0.contains_key(*k)));
        for f in ["next.json", "chapters_next.json", "next_list.json"] {
            let full = crate::util::test_fixture(f).to_serde();
            let w = crate::parse::next(&crate::util::test_fixture(f));
            assert!(!w.title.is_empty() && !w.related.items.is_empty(), "{f}: a real watch page");
            let (a, b) = masked_parses_alike(&full);
            assert!(b < a, "{f}: {b} < {a}");
        }
        // The URL: only `next` carries it; every other request is unchanged.
        assert!(Req::Next { v: "aqz-KE-bpKQ".into() }.send_url().contains("&fields=responseContext%2CcurrentVideoEndpoint"));
        assert!(Req::NextList { v: "aqz-KE-bpKQ".into(), list: "PLx".into() }.send_url().contains("&fields="));
        assert_eq!(Req::Search { q: "a".into(), params: None }.send_url(), Req::Search { q: "a".into(), params: None }.url());
        assert_eq!(Req::Cont { ep: "next", token: "t".into() }.send_url(), Req::Cont { ep: "next", token: "t".into() }.url());
    }

    /// Live check (by hand): `YTC_NEXT_DIR=<dir of whole next answers> cargo test -- --ignored next_mask_live`.
    #[test]
    #[ignore]
    fn next_mask_live() {
        let dir = std::env::var("YTC_NEXT_DIR").expect("YTC_NEXT_DIR");
        let (mut n, mut a, mut b) = (0, 0, 0);
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let name = p.to_string_lossy().to_string();
            if name.ends_with(".masked.json") {
                continue;
            }
            let full: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
            let (x, y) = masked_parses_alike(&full);
            (n, a, b) = (n + 1, a + x, b + y);
            // YouTube's own masked answer (fetched right after): the same page, field by field.
            let Ok(m) = std::fs::read(name.replace(".json", ".masked.json")) else { continue };
            let (wa, wb) = (crate::parse::next(&crate::json::parse(&serde_json::to_vec(&full).unwrap()).unwrap()), crate::parse::next(&crate::json::parse(&m).unwrap()));
            let sum = |w: &crate::parse::Watch| {
                ((w.id.clone(), w.title.clone(), w.channel.clone(), w.channel_href.clone(), w.avatar.is_some(), w.subs.is_empty(), w.date.is_empty(), w.desc.len(), w.acts.like.is_some(), w.acts.channel_id.clone()),
                    (w.acts.sub_params.is_some(), w.acts.comments.is_some(), w.acts.comments_off.clone(), w.acts.sorts.len(), w.acts.chapters.len(), w.up.auto.is_some(), w.up.playlist.as_ref().map(|p| p.items.len()), w.related.items.len() > 10, w.related.cont.is_some()))
            };
            assert_eq!(sum(&wa), sum(&wb), "{name}");
        }
        eprintln!("{n} answers: whole {a} bytes, masked {b} bytes ({:.0} %)", 100.0 * b as f64 / a.max(1) as f64);
    }

    fn vision_mask_parity(full: &Value) -> (usize, usize) {
        let masked = mask_apply(&mask_parse(VISION_FIELDS), full);
        // These groups are deliberately whole: new codecs, tracks and live fields survive too.
        for k in ["responseContext", "playabilityStatus", "streamingData", "captions", "videoDetails"] {
            assert_eq!(full.get(k), masked.get(k), "{k}");
        }
        let (a, b) = (full.to_string(), masked.to_string());
        let (pa, pb) = (crate::json::parse_str(&a).unwrap(), crate::json::parse_str(&b).unwrap());
        assert_eq!(format!("{:?}", crate::player::formats::parse(&pa)), format!("{:?}", crate::player::formats::parse(&pb)));
        assert_eq!(crate::player::captions::list(&pa), crate::player::captions::list(&pb));
        (a.len(), b.len())
    }

    #[test]
    fn vision_mask_keeps_all_playback_formats_captions_and_live_fields() {
        let full = json!({
            "responseContext": {"visitorData": "fixture"},
            "playabilityStatus": {"status": "OK", "reason": "fixture"},
            "streamingData": {"hlsManifestUrl": "https://manifest.googlevideo.com/fixture.m3u8",
                "adaptiveFormats": [
                    {"itag": 337, "url": "https://rr1---sn-fixture.googlevideo.com/video", "mimeType": "video/webm; codecs=\"vp09.02.51.10\"",
                        "width": 3840, "height": 2160, "fps": 60, "qualityLabel": "2160p60 HDR", "bitrate": 14000000,
                        "initRange": {"start": "0", "end": "219"}, "indexRange": {"start": "220", "end": "400"},
                        "contentLength": "90000000", "approxDurationMs": "240000", "colorInfo": {"transferCharacteristics": "COLOR_TRANSFER_CHARACTERISTICS_HLG"}},
                    {"itag": 140, "url": "https://rr2---sn-fixture.googlevideo.com/audio", "mimeType": "audio/mp4; codecs=\"mp4a.40.2\"",
                        "bitrate": 128000, "initRange": {"end": "199"}, "indexRange": {"end": "399"}, "contentLength": "4000000",
                        "approxDurationMs": "240000", "audioTrack": {"id": "ja.0", "displayName": "Japanese", "audioIsDefault": false}}
                ]},
            "captions": {"playerCaptionsTracklistRenderer": {
                "captionTracks": [{"baseUrl": "https://www.youtube.com/api/timedtext?lang=ja", "languageCode": "ja", "kind": "asr",
                    "name": {"simpleText": "Japanese"}, "isTranslatable": true}],
                "audioTracks": [{"defaultCaptionTrackIndex": 0}], "defaultAudioTrackIndex": 0,
                "translationLanguages": [{"languageCode": "en"}]}},
            "videoDetails": {"lengthSeconds": "240", "isLive": true, "isUpcoming": false},
            "playerConfig": {"unused": [1, 2, 3]}, "playbackTracking": {"unused": "WEB owns history"},
            "storyboards": {"unused": "WEB owns storyboards"}
        });
        let encoded = full.to_string();
        let parsed = crate::json::parse_str(&encoded).unwrap();
        let formats = crate::player::formats::parse(&parsed);
        assert_eq!(formats.len(), 2);
        assert!(formats[0].hdr && formats[0].hlg && formats[1].alt_audio);
        let captions = crate::player::captions::list(&parsed);
        assert_eq!((captions.tracks.len(), captions.default), (2, Some(0)));
        let (a, b) = vision_mask_parity(&full);
        assert!(b < a);
        vision_mask_parity(&crate::util::test_fixture("player.json").to_serde());
        vision_mask_parity(&json!({"playabilityStatus": {"status": "LIVE_STREAM_OFFLINE", "reason": "Premieres later"},
            "videoDetails": {"isUpcoming": true, "isLiveContent": true}, "playerConfig": {"unused": true}}));
    }

    /// Local successful VISIONOS captures, kept outside the repository (may contain signed URLs).
    /// `YTC_VISION_FILE=<whole answer> cargo test -- --ignored vision_mask_live`
    #[test]
    #[ignore]
    fn vision_mask_live() {
        let file = std::env::var("YTC_VISION_FILE").expect("YTC_VISION_FILE");
        let full: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        assert_eq!(full.pointer("/playabilityStatus/status").and_then(Value::as_str), Some("OK"));
        let (a, b) = vision_mask_parity(&full);
        let encoded = full.to_string();
        let parsed = crate::json::parse_str(&encoded).unwrap();
        let n = crate::player::formats::parse(&parsed).len();
        assert!(n > 0, "successful playable formats");
        eprintln!("{n} formats unchanged: whole {a} bytes, masked {b} bytes ({:.0} %)", 100.0 * b as f64 / a as f64);
    }

    #[test]
    fn home_body_like_youtubes() {
        let mut cfg = Config::default();
        cfg.visitor = Some("CgtlMmVGYWtlVmlzMCjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D".into());
        let vp = Viewport::from_json(r#"{"w":1400,"h":900,"dpr":2,"dark":true,"share":false}"#);
        let b: Value = serde_json::from_str(&home_body(&cfg, "en", &vp)).unwrap();
        let c = &b["context"]["client"];
        assert_eq!(b["browseId"], "FEwhat_to_watch");
        assert_eq!((c["clientName"].as_str(), c["visitorData"].as_str()), (Some("WEB"), cfg.visitor.as_deref()));
        assert_eq!((c["screenWidthPoints"].as_u64(), c["screenHeightPoints"].as_u64(), c["windowWidthPoints"].as_u64()), (Some(1400), Some(900), Some(1400)));
        assert_eq!((c["screenPixelDensity"].as_u64(), c["screenDensityFloat"].as_f64()), (Some(2), Some(2.0)));
        assert_eq!(c["userInterfaceTheme"], "USER_INTERFACE_THEME_DARK");
        assert_eq!(c["originalUrl"], "https://www.youtube.com/", "the Home URL wherever it is sent from");
        assert_eq!(c["mainAppWebInfo"]["graftUrl"], "/");
        // The plain browse body is unchanged for every other page.
        let plain: Value = serde_json::from_str(&Req::Browse { id: "FEwhat_to_watch".into(), params: None }.body(&cfg, "en", "/")).unwrap();
        assert!(plain["context"]["client"].get("screenWidthPoints").is_none());
        assert_eq!(Viewport::from_json("garbage"), Viewport { w: 1280, h: 800, dpr: 1.0, dark: false, share: false });
    }

    #[test]
    fn player_bodies() {
        let mut cfg = Config::default();
        cfg.visitor = Some("CgtlMmVGYWtlVmlzMCjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D".into());
        let (b, h) = vision_player(&cfg, "aqz-KE-bpKQ");
        let b: Value = serde_json::from_str(&b).unwrap();
        let h: Value = serde_json::from_str(&h).unwrap();
        assert_eq!(b["context"]["client"]["clientName"], "VISIONOS");
        assert_eq!(b["context"]["client"]["visitorData"], h["X-Goog-Visitor-Id"]);
        assert_eq!(h["X-Youtube-Client-Name"], "101");
        assert_eq!(b["videoId"], "aqz-KE-bpKQ");
        let w: Value = serde_json::from_str(&web_player(&cfg, "de", "/watch?v=aqz-KE-bpKQ", "aqz-KE-bpKQ", "abc", 20753)).unwrap();
        assert_eq!(w["context"]["client"]["clientName"], "WEB");
        assert_eq!(w["context"]["client"]["hl"], "de");
        assert_eq!(w["playbackContext"]["contentPlaybackContext"]["signatureTimestamp"], 20753);
        assert_eq!(w["cpn"], "abc");
        assert_eq!(w["racyCheckOk"], true);
    }
    #[test]
    fn vision_routes() {
        let mut cfg = Config::default();
        cfg.visitor = Some("CgtlMmVGYWtlVmlzMCjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D".into());
        let auth = Auth::from_cookies("SAPISID=x");
        // The happy path is exactly the old request.
        let (b, h, o) = vision_request(&cfg, &auth, 1, &VisionRoute::Anon, "aqz-KE-bpKQ");
        assert_eq!((b, h), vision_player(&cfg, "aqz-KE-bpKQ"));
        assert_eq!(o, "{\"credentials\":\"omit\"}");
        // Fresh visitor: anonymous, the new visitor in body and header, nothing else changes.
        let fresh = "CgtlMmVGYWtlVmlzMSjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D";
        let (b, h, o) = vision_request(&cfg, &auth, 1, &VisionRoute::Fresh(fresh.into()), "aqz-KE-bpKQ");
        let (b, h): (Value, Value) = (serde_json::from_str(&b).unwrap(), serde_json::from_str(&h).unwrap());
        assert_eq!((b["context"]["client"]["visitorData"].as_str(), h["X-Goog-Visitor-Id"].as_str()), (Some(fresh), Some(fresh)));
        assert!(h.get("Authorization").is_none() && o.contains("omit"));
        assert_eq!(b["context"]["client"]["clientName"], "VISIONOS");
        // Account: cookies (default credentials) + SAPISIDHASH, the configured visitor.
        let (b, h, o) = vision_request(&cfg, &auth, 1, &VisionRoute::Account, "aqz-KE-bpKQ");
        let (b, h): (Value, Value) = (serde_json::from_str(&b).unwrap(), serde_json::from_str(&h).unwrap());
        assert_eq!(o, "{}");
        assert!(h["Authorization"].as_str().unwrap().starts_with("SAPISIDHASH 1_"));
        assert_eq!((h["X-Goog-AuthUser"].as_str(), h["X-Origin"].as_str(), h["X-Youtube-Client-Name"].as_str()), (Some("0"), Some(ORIGIN), Some("101")));
        assert_eq!(b["context"]["client"]["visitorData"], cfg.visitor.clone().unwrap().as_str());
        assert_eq!(VisionRoute::Fresh(fresh.into()).label(), "fresh visitor");
    }
    #[test]
    fn visitor_id() {
        let cfg = Config::default();
        let (b, h) = visitor_id_request(&cfg);
        let (b, h): (Value, Value) = (serde_json::from_str(&b).unwrap(), serde_json::from_str(&h).unwrap());
        assert_eq!(b["context"]["client"]["clientName"], "WEB");
        assert!(b["context"]["client"].get("visitorData").is_none(), "a new visitor, not ours");
        assert!(h.get("X-Goog-Visitor-Id").is_none() && h.get("Authorization").is_none());
        // Recorded 2 Nov 2026 (shape): responseContext.visitorData.
        let r = br#"{"responseContext":{"visitorData":"CgtlMmVGYWtlVmlzMSjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D","serviceTrackingParams":[]}}"#;
        assert_eq!(parse_visitor(r).as_deref(), Some("CgtlMmVGYWtlVmlzMSjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D"));
        assert_eq!(parse_visitor(br#"{"responseContext":{"visitorData":"<x>"}}"#), None);
        assert_eq!(parse_visitor(b"not json"), None);
    }
}
