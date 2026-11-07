//! Application state: navigation, request lifecycle, caches and rendering
//! decisions. All browser effects go through `bridge`.
//!
//! Feeds are a queue: `shown` items are in the DOM, `buf` items are parsed and
//! waiting. Continuations are fetched in the background (idle time, low fetch
//! priority) until `preload_target()` is reached (100 items on Home and
//! Subscriptions); scrolling only moves buffered items into the DOM, 24 cards
//! per write. Feed pages are snapshotted to sessionStorage (tab lifetime) so a
//! reload or a return from YouTube's native watch page paints at once and then
//! refreshes in place (stale-while-revalidate).

use crate::actions::{self, Actions, Click};
use crate::api::{CHANNEL_VIDEOS_PARAMS, Req};
use crate::bridge;
use crate::config::{self, Auth, Config};
use crate::features::upnext;
use crate::parse::{self, Feed, Item, Stats, Video, Watch};
use crate::player::{self, Out, Own, Vision, formats::Throughput};
use crate::render::{self, Head, ImgPlan, Lay, INFO, LIST, MAIN, MORE, PILL, SUGGEST};
use crate::util::next_id;
use crate::route::{self, ChannelKey, Route, native_url};
use crate::snap::{self, Snap};
use crate::util::pct_encode;
use crate::json::{self, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

mod fxglue;
mod homeready;
mod impglue;
mod logo;
mod motionglue;
mod speed;
mod telglue;
mod tvglue;
mod tvwatch;
mod tvkeyboard;
mod upglue;
mod voiceglue;
/// Request context for `actions` (disjoint from `App::acts`).
macro_rules! net {
    ($s:ident) => {
        actions::Net { cfg: &$s.cfg, hl: &$s.hl, url: &$s.url }
    };
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

pub fn with(f: impl FnOnce(&mut App)) {
    APP.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(app) = guard.as_mut() {
                f(app);
            }
        }
    });
}

pub use telglue::route_kind;

/// The current route's kind for telemetry ("" before boot).
pub fn route_now() -> &'static str {
    with_ret(|a| route_kind(&a.route))
}

pub fn with_ret<R: Default>(f: impl FnOnce(&mut App) -> R) -> R {
    let mut out = R::default();
    with(|a| out = f(a));
    out
}

const WATCH_CACHE: usize = 40;
const SAVED_PAGES: usize = 15;
const KNOWN_MAX: usize = 4000;
const HOVER_MS: u32 = 70;
/// Player preference writes are batched this long (range gestures).
const PREFS_SAVE_MS: u32 = 300;
const MAX_PREFETCH: usize = 3;
const CONFIG_REFRESH_DELAY_MS: u32 = 2500;
/// Cards per DOM write when scrolling into preloaded items.
const CHUNK: usize = 24;
/// Items preloaded on Home and Subscriptions (100 videos ready before they are scrolled to).
const FEED_PRELOAD: usize = 100;
/// Other feeds keep this many items buffered beyond the screen.
const AHEAD: usize = 12;
/// Home and Subscriptions keep this many items buffered beyond the screen once 100 are shown (two
/// continuations: a fast scroll through one chunk never reaches the end while the next is asked for;
/// measured at 10,000 px/s with 400 ms continuations: the end in view 373 → 138 ms).
const FEED_AHEAD: usize = 2 * CHUNK;
/// Background continuation waits for an idle main thread, at most this long.
const PRELOAD_IDLE_MS: u32 = 1500;
/// Hover-prefetched channel/playlist/feed responses (raw bytes, memory only).
const PAGE_CACHE: usize = 6;
const PAGE_TTL_MS: f64 = 120_000.0;
/// Back/forward to a snapshot younger than this shows it without refreshing.
const SNAP_FRESH_MS: f64 = 30.0 * 60_000.0;
/// A refreshed feed replaces its snapshot only while the user is near the top.
const SWAP_MAX_Y: f64 = 200.0;
/// How long a restored page keeps every card laid out (see `restore_scroll`).
const RESTORE_LAYOUT_MS: u32 = 1000;
/// The page inside `#ytc-main` (replaced on navigation, taking `.rs` with it).
const RESTORED: &str = "#ytc-main>*";
/// Hover this long on a thumbnail to start YouTube's animated preview.
const PREVIEW_MS: u32 = 500;
const SUGGEST_MS: u32 = 80;
/// Typing pause after which the search results are fetched ahead of Enter (owner, 3 Nov 2026: "≥ ~250 ms").
const SEARCH_WARM_MS: u32 = 250;
/// Rest on a suggestion chosen with the arrow keys before its results are fetched (arrowing through
/// the list fetches nothing).
const SEARCH_PICK_MS: u32 = 120;
/// At most this many searches fetched ahead per minute (a slow typist pausing often, ~100 KB each).
const SEARCH_WARM_BUDGET: usize = 10;
const SUGGEST_CACHE: usize = 64;
/// sessionStorage mirror of the config, so the first request never waits for chrome.storage.
const CFG_SESSION: &str = "ytc:cfg";
/// `fetch_opts` for background requests.
const LOW: &str = "{\"priority\":\"low\"}";
/// The endpoint name of YouTube's Home document request (`crate::homedoc`).
const HOME_DOC: &str = "home-doc";
/// Player failure reasons kept in chrome.storage.local for the debug overlay
/// (across reloads and the manual Open on YouTube).
const PLAYER_LOG_KEY: &str = "ytcPlayerLogV1";
const PLAYER_LOG_MAX: usize = 10;
/// After VISIONOS answers with the bot check on every route the player tried
/// (anonymous, fresh visitor, account session): this long, no prefetches, and a
/// player tries only the remembered route (the panel's Try again recovers fully).
const VISION_BLOCK_MS: f64 = 10.0 * 60.0 * 1000.0;
/// VISIONOS responses kept in memory (their URLs live 6 h; we reuse 30 min).
const VISION_CACHE: usize = 40;
/// How often a failed player re-checks a dropped connection.
const NET_PROBE_MS: u32 = 3000;
const VISION_MAX_AGE_MS: f64 = 30.0 * 60.0 * 1000.0;
/// The pointer or focus rests this long on a video link before its first media is prefetched.
/// The response and decodingInfo may arrive during that dwell; neither restarts it.
const MEDIA_PRE_MS: u32 = 150;
/// Fetch the next video's `next` + VISIONOS this close to the end (seconds).
const UP_PREFETCH_S: f64 = 45.0;
/// Speculative prefetch: this many cards in view, two at a time, after idle.
const SPEC_CARDS: u32 = 8;
const SPEC_CONCURRENT: usize = 2;
const SPEC_IDLE_MS: u32 = 1200;
/// Speculative VISIONOS calls per 10 minutes (more volume risks YouTube's bot check).
const SPEC_VISION_BUDGET: u32 = 30;
/// Home/Subscriptions older than this are refreshed in the background (and a
/// younger snapshot is shown without the slow refresh). sessionStorage
/// `ytc:bgage` (ms) overrides it for tests.
const BG_AGE_MS: f64 = 120_000.0;
const BG_TICK_MS: u32 = 30_000;
/// Background refreshes only while the user used the tab this recently.
const BG_ACTIVE_MS: f64 = 5.0 * 60_000.0;
const CONT_CACHE: usize = 4;

/// Watch later opens newest first (YouTube's own "Date added (newest)", which YouTube remembers for
/// the account). The page and, in another language than English, the English menu check go out
/// together; a page that is not newest first waits only for the sort.
#[derive(Clone, Copy, PartialEq)]
enum WlSort {
    /// Nothing known yet for this visit.
    Open,
    /// The English menu check is out.
    Checking,
    /// YouTube's sort command is out.
    Sorting,
    /// Decided: the next page answer renders.
    Settled,
}

enum Purpose {
    /// Is the network there at all? (`generate_204`; a player failure that looked like a dropped connection)
    NetProbe,
    Page { nav: u32 },
    WatchLaterOrder { nav: u32 },
    /// The English sort-menu check of Watch later (`Req::WatchLaterMenu`).
    WatchLaterMenu { nav: u32 },
    Resolve { nav: u32, path: String },
    Cont { nav: u32, generation: u32 },
    /// `spec` = speculative (cards in view, idle time): cancelled on navigation.
    Prefetch { v: String, list: Option<String>, spec: bool },
    /// `bg` = background refresh of Home/Subscriptions (then its first continuation).
    PagePrefetch { url: String, bg: bool },
    /// A background page's first continuation (kept in `cont_cache`).
    ContPrefetch { token: String },
    Suggest { q: String },
    Config { retry_nav: Option<u32> },
    VisionPrefetch { v: String, spec: bool, route: crate::api::VisionRoute },
    /// The ready Home response (`app/homeready.rs`): its page, then its first continuation; sent
    /// under taint `generation` for account `acct`.
    Ready { generation: u32, acct: String },
    ReadyCont { generation: u32, acct: String },
    /// A batch of Home's "shown" signals (`app/impglue.rs`); `first`: the screen's first batch.
    Imp { first: bool },
}

struct Pending {
    purpose: Purpose,
    sent: f64,
    /// Endpoint name for the perf report.
    ep: &'static str,
    /// A field-masked request: the same request without the mask, sent if
    /// YouTube refuses the mask (HTTP 400).
    unmasked: Option<Box<Resend>>,
}

/// What it takes to send a request again unchanged (URL, body, headers, fetch options).
struct Resend {
    url: String,
    body: String,
    headers: String,
    opts: &'static str,
}

enum Timer {
    /// Autoplay countdown of navigation `nav` ran out.
    Autoplay { nav: u32 },
    /// Idle time after a paint: speculative prefetch of the cards in view.
    Spec { nav: u32 },
    /// Background freshness check for Home/Subscriptions (repeats).
    Bg,
    PageHover(String),
    Preview(String),
    /// The pointer still rests on this video's link (try `n`): prefetch its first media.
    MediaPre(String, u8),
    Pump { nav: u32, generation: u32 },
    Suggest(String),
    /// Typing paused: fetch the results for the query so Enter shows them at once.
    SearchWarm(String),
    ConfigRefresh,
    SavePrefs,
    /// A restored page of navigation `nav` may skip off-screen cards again.
    Unpin { nav: u32 },
    /// The first frame of instant navigation `nav` has rendered (`data-still` off).
    Still { nav: u32 },
    /// The pointer rested on the logo for `logo::DWELL_MS` (dwell generation).
    Logo { generation: u32 },
    /// Probe the network again while a failed player waits for the connection to come back.
    NetProbe,
    /// TV: the first page stops waiting for the shell's token.
    TvHold,
    /// TV: poll the device sign-in (generation).
    TvPoll { generation: u32 },
    /// TV: a focus check after a paint (the old page may drop the focus while it goes).
    TvFocus,
    /// Request `id` passed its hedge deadline (`crate::hedge`).
    Hedge { id: u32 },
    /// Idle time for refilling the ready Home response (`lazy`: after an event or expiry).
    ReadyFill { lazy: bool },
    /// Home navigation `nav`'s request is late: send the hedge.
    HomeHedge { nav: u32 },
    /// Home navigation `nav` still has no answer: show the old Home dimmed.
    HomeSlow { nav: u32 },
    /// TV: the remote rested after scrubbing the seek bar (scrub generation): seek there.
    TvScrub { generation: u32 },
    /// TV: ask the shell about updates (repeats; `fast` = while an app update downloads).
    TvUpdates { fast: bool },
    /// Home's "shown" signals: the batch's time came.
    ImpFlush,
}

#[derive(PartialEq)]
enum Mode {
    Push,
    Pop,
}

/// A rendered page kept in memory for instant back/forward (never persisted).
struct Saved {
    url: String,
    html: String,
    scroll: f64,
    feed: FeedState,
    title: String,
    stats: Stats,
}

/// Everything about the current list of cards.
#[derive(Default)]
struct FeedState {
    head: Option<Head>,
    shown: Vec<Item>,
    buf: Vec<Item>,
    cont: Option<String>,
    cont_ep: &'static str,
    seen: HashSet<String>,
    /// The user scrolled this list (a chunk came in at the sentinel, or a watch page was scrolled):
    /// from now on the next continuation is kept ahead and asked for as soon as the buffer runs low.
    scrolled: bool,
}

pub struct App {
    cfg: Config,
    /// chrome.storage config read. Feeds never wait for it (they use the tab's
    /// mirror or the fallback); the player does, for its prefs and switches.
    cfg_ready: bool,
    /// This visit's newest-first decision for Watch later (`WlSort`).
    wl: WlSort,
    /// Watch later's page, held while that decision is open (shown in the order received if the
    /// sort fails: the list never waits on YouTube accepting the sort).
    wl_held: Option<Vec<u8>>,
    /// The sort failed: the list shows in the order received, with a note and Try again.
    wl_note: bool,
    /// YouTube already keeps this account's Watch later newest first (seen in this tab): no check
    /// and no hold for the rest of the document.
    wl_newest: bool,
    auth: Auth,
    hl: String,
    route: Route,
    url: String,
    nav: u32,
    nav_start: f64,
    pending: HashMap<u32, Pending>,
    rendered: bool,
    retried: bool,
    watch_cache: Vec<(String, Rc<Watch>)>,
    prefetching: HashMap<String, u32>,
    page_cache: Vec<(String, Vec<u8>, f64)>,
    page_prefetching: HashMap<String, u32>,
    await_page: Option<String>,
    saved: Vec<Saved>,
    known: HashMap<String, Video>,
    channels: HashMap<String, String>,
    feed: FeedState,
    /// Bumped whenever `feed` is replaced; stale continuation responses are dropped.
    generation: u32,
    cont_busy: bool,
    /// The sentinel is visible and the buffer was empty: render on arrival.
    want: bool,
    pump_scheduled: bool,
    preloaded_marked: bool,
    /// Showing a snapshot while the fresh response is on its way.
    swr: bool,
    /// Fresh response held back because the user had scrolled (see FRESH_PILL).
    held: Option<Vec<u8>>,
    cols: usize,
    stats: Stats,
    marks: Vec<(&'static str, f64)>,
    timers: HashMap<u32, Timer>,
    hover: String,
    /// The latest video intent and when its uninterrupted rest started.
    media_rest: Option<(String, f64)>,
    /// The rest (its start time) whose end already has a wake queued: later answers during the
    /// same rest add no timers.
    media_wake: f64,
    preview_v: String,
    preview_on: bool,
    suggest_q: String,
    /// The query whose results a typing pause or a suggestion rest fetches ahead (memory only).
    warm_q: String,
    /// The results URL of the search fetched ahead and still loading (one at a time).
    warm_url: Option<String>,
    /// When the last searches were fetched ahead (the per-minute budget).
    warm_at: Vec<f64>,
    suggest_id: Option<u32>,
    suggest_cache: Vec<(String, Vec<String>)>,
    debug: bool,
    /// `g` was pressed (time): the next key picks a page (`render::go_href`).
    go_at: Option<f64>,
    /// The keyboard shortcut sheet (`?`) is open.
    help: bool,
    logged_out: Option<bool>,
    config_inflight: bool,
    first_ready: bool,
    title: String,
    player: Option<Own>,
    /// The player failed on this page: the panel shows in its box (Try again / Open on YouTube).
    player_failed: Option<PlayerFailed>,
    /// The failed player is waiting for the connection: probing until it answers, then Try again by itself.
    offline: bool,
    /// The next player start asks VISIONOS itself (Try again: no cached or prefetched response).
    vision_fresh: bool,
    /// Waiting for config/visitor data before the player's requests.
    player_wait: bool,
    /// A preferences write is scheduled (`Timer::SavePrefs`).
    prefs_dirty: bool,
    bw: Throughput,
    vision_block_until: f64,
    /// Video codecs the player put a media error down to in this tab (memory only), and those
    /// avoided for the rest of the tab session (a second such fault).
    codec_faults: Vec<crate::player::formats::Codec>,
    codec_avoid: Vec<crate::player::formats::Codec>,
    /// How VISIONOS is sent this session: anonymous until a bot check, then the
    /// route that recovered (memory only; a fresh visitor is never persisted).
    vision_route: crate::api::VisionRoute,
    /// A prefetch met the bot check: no VISIONOS prefetches until a player
    /// request succeeds (the player recovers, then prefetches follow its route).
    vision_hold: bool,
    resume: HashMap<String, f64>,
    player_log: Vec<serde_json::Value>,
    /// Navigation that already sent an API-error incident (one per page).
    api_incident_nav: u32,
    vision_cache: Vec<(String, f64, Rc<Vec<u8>>)>,
    vision_prefetching: HashSet<String>,
    /// The video link last focused or pressed (`on_intent`): resting there counts like hover.
    intent_v: String,
    /// Watch later, filters, blocked websites, topics, search history, resume (features/*).
    fx: fxglue::Fx,
    /// Watch-page actions, comments, notifications and account menu.
    acts: Actions,
    /// Navigation whose next video was prefetched near the end (once per page).
    up_prefetched: u32,
    /// Autoplay countdown: (navigation, what plays next, seconds).
    countdown: Option<(u32, upnext::Next, u32)>,
    /// The playlist panel last rendered (list id, current video): its current-row mark slides from there.
    pl_last: Option<(String, String)>,
    /// Navigation motion state (`motionglue.rs`).
    mo: motionglue::Motion,
    /// Speculative prefetch: cards waiting, a pass scheduled, VISIONOS budget (window start, count).
    spec_queue: Vec<(String, Option<String>)>,
    spec_scheduled: bool,
    spec_vision: (f64, u32),
    /// Background-fetched first continuations: (token, body, epoch ms).
    cont_cache: Vec<(String, Vec<u8>, f64)>,
    /// Last user input (pointer, keys) and the background refresh age.
    last_input: f64,
    /// Last key press and last pointer press: which one moved focus decides
    /// whether the search field morphs (pointer) or changes instantly (keyboard).
    last_key: f64,
    last_ptr: f64,
    /// Row of the sidebar's sliding indicator (it stays there on pages without a link).
    side_at: usize,
    bg_age: f64,
    /// Request/navigation timings for the `perf` debug-center report.
    perf: crate::perf::Perf,
    /// Hedged reads (`crate::hedge`) and the hedge outcome of the answer being handled (fired, won).
    hedge: crate::hedge::Hedger,
    hedge_now: Option<(bool, bool)>,
    /// Navigation whose content-visible time was recorded.
    perf_nav: u32,
    /// Write time of each feed snapshot as read from sessionStorage (`None`:
    /// none or unreadable), so the background check reads a snapshot once per
    /// document, not on every pass. Cleared whenever this document writes one.
    snap_at: HashMap<String, Option<f64>>,
    /// Voice search (`voiceglue.rs`, `features/voice.rs`).
    voice: voiceglue::VoiceUi,
    /// The logo's hover dwell (`logo.rs`).
    logo: logo::Logo,
    /// This navigation paints a prefetched first page straight over the old page where it used to
    /// paint the page's snapshot first: like that repaint, the cards make no entrance.
    direct: bool,
    /// TV mode (`app/tvglue.rs`).
    tv: tvglue::Tv,
    /// The ready Home response, the Home request's hedge (`app/homeready.rs`).
    hr: homeready::HomeReady,
    /// Home's "shown" signals (`app/impglue.rs`).
    imp: impglue::Imp,
    /// A new tab's first Home request waits for the stored config (its visitor: without it a
    /// signed-out visitor gets YouTube's empty Home instead of their own).
    home_hold: bool,
}

/// Watch-data cache key: the video, plus its playlist (the `next` response differs).
fn wkey(v: &str, list: Option<&str>) -> String {
    match list {
        Some(l) => format!("{v}|{l}"),
        None => v.to_string(),
    }
}

fn next_req(v: &str, list: Option<&str>) -> Req {
    match list {
        Some(l) => Req::NextList { v: v.to_string(), list: l.to_string() },
        None => Req::Next { v: v.to_string() },
    }
}

/// A failed player's video (the panel's buttons act on it).
struct PlayerFailed {
    v: String,
    t: f64,
    list: Option<String>,
}

pub fn boot() {
    // TV app: the platform flag first (it changes how requests are sent and the account is read).
    let tv_hold = App::tv_boot_flag();
    // Before any card renders (a snapshot repaint happens below).
    render::set_card_hook(crate::features::card_actions);
    let t0 = bridge::now();
    let url = bridge::href();
    let route = route::parse(&url).unwrap_or(Route::Unknown { path: url.clone() });
    // /shorts/ID, /live/ID → a normal watch URL.
    let url = match &route {
        Route::Watch { .. } if !url.starts_with("/watch") => {
            let u = route.url();
            bridge::replace(&u);
            u
        }
        _ => url,
    };
    let lang = bridge::lang();
    let hl = lang.split(['-', '_']).next().filter(|s| (2..=3).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphabetic())).unwrap_or("en").to_ascii_lowercase();
    // Config: the tab's mirror if there is one, else the built-in fallback. The
    // first request goes out now; chrome.storage only refines later requests.
    let mirror = bridge::session_get(CFG_SESSION);
    let cfg = if mirror.is_empty() { Config::default() } else { Config::from_json(&mirror) };
    // One palette (client.css :root): nothing to apply before the first frame. A palette flag an older version left is removed.
    crate::appearance::forget_old_hint();
    // YouTube's own document for this tab (the extension's loader leaves it running): when it is the Home answer
    // the client would fetch anyway (a Home load from YouTube's document, `homeready::on_doc_script`), it keeps
    // streaming with its nodes held out of the page; else it stops now, before any request of ours goes out (a
    // stop aborts the document's requests).
    let keep_doc = route == Route::Home
        && bridge::nav_type() != "back_forward"
        && !crate::tv::on()
        && crate::ready::shared()
        && cfg.auth_user == 0
        && !bridge::local_flag("ytc-test-nodoc")
        && !bridge::local_flag("ytc-test-nokeep");
    if !keep_doc {
        bridge::page_stop();
    }
    let mut app = App {
        cfg_ready: false,
        wl: WlSort::Open,
        wl_held: None,
        wl_note: false,
        wl_newest: false,
        cfg,
        auth: crate::tv::auth(),
        hl,
        route,
        url,
        nav: 1,
        nav_start: 0.0,
        pending: HashMap::new(),
        rendered: false,
        retried: false,
        watch_cache: Vec::new(),
        prefetching: HashMap::new(),
        page_cache: Vec::new(),
        page_prefetching: HashMap::new(),
        await_page: None,
        saved: Vec::new(),
        known: HashMap::new(),
        channels: HashMap::new(),
        feed: FeedState { cont_ep: "browse", ..Default::default() },
        generation: 0,
        cont_busy: false,
        want: false,
        pump_scheduled: false,
        preloaded_marked: false,
        swr: false,
        held: None,
        cols: render::grid_cols(bridge::width()),
        stats: Stats::default(),
        marks: vec![("boot", t0)],
        timers: HashMap::new(),
        hover: String::new(),
        media_rest: None,
        media_wake: -1.0,
        preview_v: String::new(),
        preview_on: false,
        suggest_q: String::new(),
        warm_q: String::new(),
        warm_url: None,
        warm_at: Vec::new(),
        suggest_id: None,
        suggest_cache: Vec::new(),
        debug: false,
        go_at: None,
        help: false,
        logged_out: None,
        config_inflight: false,
        first_ready: false,
        title: String::new(),
        player: None,
        player_failed: None,
        offline: false,
        vision_fresh: false,
        player_wait: false,
        prefs_dirty: false,
        bw: Throughput::default(),
        vision_block_until: 0.0,
        codec_faults: Vec::new(),
        codec_avoid: Vec::new(),
        vision_route: crate::api::VisionRoute::Anon,
        vision_hold: false,
        resume: HashMap::new(),
        player_log: Vec::new(),
        api_incident_nav: u32::MAX,
        vision_cache: Vec::new(),
        vision_prefetching: HashSet::new(),
        intent_v: String::new(),
        fx: fxglue::Fx::new(crate::tv::auth().signed_in()),
        acts: Actions::default(),
        up_prefetched: 0,
        countdown: None,
        pl_last: None,
        mo: Default::default(),
        spec_queue: Vec::new(),
        spec_scheduled: false,
        spec_vision: (0.0, 0),
        cont_cache: Vec::new(),
        last_input: 0.0,
        last_key: 0.0,
        last_ptr: 0.0,
        side_at: 0,
        bg_age: bridge::session_get("ytc:bgage").parse::<f64>().ok().filter(|x| *x >= 1000.0).unwrap_or(BG_AGE_MS),
        perf: Default::default(),
        hedge: crate::hedge::Hedger::new(bridge::local_flag("ytc-test-nohedge")),
        hedge_now: None,
        perf_nav: 0,
        snap_at: HashMap::new(),
        voice: Default::default(),
        logo: Default::default(),
        direct: false,
        tv: tvglue::Tv::new(tv_hold),
        hr: Default::default(),
        imp: Default::default(),
        home_hold: false,
    };
    app.tel_boot();
    let nav_type = bridge::nav_type();
    if matches!(app.route, Route::Watch { .. }) {
        // A video: YouTube's next Home differs from the ready one.
        crate::ready::taint();
    }
    // Home loaded or reloaded (not back/forward): YouTube's answer for this request, never this
    // tab's old Home first. The ready response when there is one, else the skeleton and the request.
    let home = app.route == Route::Home && nav_type != "back_forward" && !app.tv.hold;
    // Extension: the ready response is shared by the tabs (memory-only extension storage): one
    // asynchronous read (a few ms) before this document's first Home (`on_shared`).
    let shared = crate::ready::shared();
    let taking = home && shared;
    let ready = if home && !shared { app.ready_take() } else { None };
    // Without a visitor YouTube answers a signed-out `browse` with its empty Home; its Home document
    // needs none (the browser's cookies carry it).
    if home && ready.is_none() && app.cfg.visitor.is_none() && !crate::tv::on() && !app.doc_ok() {
        app.home_hold = true;
    }
    // (TV cold start holding for the token: the skeleton until it answers, no request without it.)
    let snap = if home || app.tv.hold { None } else { app.read_snap() };
    let restore_scroll = snap.as_ref().filter(|_| nav_type == "back_forward").map(|s| s.scroll);
    let fresh_enough = snap.as_ref().is_some_and(|s| bridge::epoch() - s.at < SNAP_FRESH_MS);
    // A snapshot's refresh goes out before the snapshot is rendered: it travels while the page
    // paints (the response always arrives in a later task).
    let refresh = snap.is_some() && !(restore_scroll.is_some() && fresh_enough);
    if refresh {
        app.swr = true;
        app.load();
    }
    // Without a scroll position to restore, only the first chunk of cards is written (the rest
    // follows from the buffer as the page scrolls, like any feed).
    let trim = restore_scroll.is_none_or(|y| y <= 0.0);
    let main = snap.map(|s| app.adopt_snap(s, trim));
    app.side_at = render::side_index(app.route.nav_key()).unwrap_or(0);
    bridge::doc(&render::shell(&app.route, &app.url, main.as_deref()));
    bridge::storage_watch(config::STORAGE_KEY);
    bridge::message_listen();
    if keep_doc {
        bridge::doc_hold("var ytInitialData = ", "ytcfg.set(");
        app.hr.nav_doc = homeready::NavDoc::Held;
    }
    bridge::mark("ytcShell");
    app.marks.push(("shell", bridge::now()));
    app.acts.boot();
    bridge::storage_get(config::STORAGE_KEY);
    bridge::storage_get(PLAYER_LOG_KEY);
    fxglue::Fx::boot();
    App::voice_boot();
    app.tv_boot();
    if app.tv.hold {
        // TV cold start: the skeleton shows until the shell's token answer (`tv_release`).
    } else if let Some(r) = ready {
        app.ready_show(r);
    } else if taking {
        // The skeleton (in the shell) until the shared store answers: its ready response or the request.
        app.hr.taking = true;
        bridge::shared_take(crate::ready::KEY);
    } else if main.is_some() {
        crate::telemetry::nav_src("snapshot");
        bridge::mark("ytcRepaint");
        app.mark("repaint");
        if let Some(y) = restore_scroll {
            app.restore_scroll(y);
        }
        app.after_paint();
    } else {
        app.start_route();
    }
    if shared && !taking {
        // What is waiting for the next Home (this tab neither fetches a second one nor misses it).
        bridge::shared_get(crate::ready::KEY);
    }
    let id = app.timer(Timer::Bg);
    bridge::timer(id, BG_TICK_MS);
    // decodingInfo for the usual YouTube formats, after the first requests went out (answers in
    // a few ms, before any player response): a page's first video then waits for none.
    player::own::warm_caps_boot();
    APP.with(|cell| *cell.borrow_mut() = Some(app));
}

impl App {
    /// Scrolls a page just written into `#ytc-main` to `y`. Its cards are
    /// `content-visibility:auto` with a placeholder height and none of the new
    /// nodes has been rendered yet, so the rows above `y` would count at the
    /// placeholder height: the jump would land on other cards, and scroll
    /// anchoring would move it again as the rows near the viewport render.
    /// So the page is laid out in full (`.rs`) for the jump; once frames have
    /// rendered, every card remembers its real size (`auto`) and the class goes.
    fn restore_scroll(&mut self, y: f64) {
        if y > 0.0 {
            bridge::class_all(RESTORED, "rs", true);
            let id = self.timer(Timer::Unpin { nav: self.nav });
            bridge::timer(id, RESTORE_LAYOUT_MS);
        }
        bridge::scroll_to(y);
    }

    fn mark(&mut self, name: &'static str) {
        let t = bridge::now() - self.nav_start;
        self.marks.push((name, t));
    }

    // ------------------------------------------------------------ requests

    fn send(&mut self, req: Req, purpose: Purpose, low: bool) -> u32 {
        let id = next_id();
        self.auth = crate::tv::auth();
        // Home is YouTube's own Home document, as a reload of its page asks for it (`crate::homedoc`).
        if matches!(&req, Req::Browse { id, params: None } if id == "FEwhat_to_watch") && self.doc_ok() {
            bridge::fetch_opts(id, "GET", crate::homedoc::URL, "", crate::homedoc::HEADERS, crate::homedoc::opts(low));
            self.pending.insert(id, Pending { purpose, sent: bridge::now(), ep: HOME_DOC, unmasked: None });
            return id;
        }
        // Home goes out exactly as YouTube's web app sends it (window fields included).
        // (TV signed in: `tv::finish` below turns it into the TVHTML5 request with the access token.)
        let home = matches!(&req, Req::Browse { id, params: None } if id == "FEwhat_to_watch");
        let body = if home { crate::api::home_body(&self.cfg, &self.hl, &crate::api::Viewport::from_json(&bridge::viewport())) } else { req.body(&self.cfg, &self.hl, &self.url) };
        // TV: the account's feeds go out as the TV client with the bearer, everything else anonymous.
        let auth = if self.auth.bearer.is_some() && !self.tv_account_req(&req) { crate::config::Auth::default() } else { self.auth.clone() };
        let (body, headers) = crate::tv::finish(&self.cfg, &auth, body, (bridge::epoch() / 1000.0) as u64);
        let opts = if low { LOW } else { "{}" };
        let url = req.send_url();
        bridge::fetch_opts(id, "POST", &url, &body, &headers, opts);
        let ep = req.endpoint();
        let plain = req.url();
        let unmasked = (url != plain).then(|| Box::new(Resend { url: plain, body: body.clone(), headers: headers.clone(), opts }));
        // Background requests the user may come to wait for (a continuation, a speculative or background page prefetch).
        let cont = matches!(purpose, Purpose::Cont { .. } | Purpose::Prefetch { .. } | Purpose::PagePrefetch { .. });
        self.pending.insert(id, Pending { purpose, sent: bridge::now(), ep, unmasked });
        // Reads someone waits for get a second copy past their deadline (never actions; a
        // background continuation only once the user waits for it; Home's first page is the Home
        // path's). The copy goes out at normal priority.
        let home = matches!(&req, Req::Browse { id, .. } if id == "FEwhat_to_watch");
        if (!low || cont) && !home && self.hedge.allowed(ep, bridge::now()) && crate::hedge::readable(ep) {
            self.hedge.watch(id, crate::hedge::Stored { method: "POST".into(), url, body, headers, opts: "{}".into(), ep, copy: None, fired_at: 0.0, failed: false, armed: false });
            if !low {
                self.hedge_arm(id, 0.0);
            }
        }
        id
    }

    /// The user now waits for request `id` (a background one): its hedge deadline runs from its send.
    fn hedge_waits(&mut self, id: u32) {
        let waited = self.pending.get(&id).map(|p| bridge::now() - p.sent).unwrap_or(0.0);
        self.hedge_arm(id, waited);
    }

    /// Starts request `id`'s hedge deadline (`waited`: ms it has already been out).
    fn hedge_arm(&mut self, id: u32, waited: f64) {
        let Some(ep) = self.pending.get(&id).map(|p| p.ep) else { return };
        if self.hedge.arm(id) {
            let ms = (self.hedge.deadline(ep) - waited).max(250.0);
            let t = self.timer(Timer::Hedge { id });
            bridge::timer(t, ms.round() as u32);
        }
    }

    /// Request `id`'s hedge deadline passed: the same request once more, if it is still waited for.
    fn hedge_fire(&mut self, id: u32) {
        if !self.pending.contains_key(&id) {
            if let Some(x) = self.hedge.forget(id) {
                bridge::abort(x);
            }
            return;
        }
        let Some(c) = self.hedge.due(id) else { return };
        let copy = next_id();
        bridge::fetch_opts(copy, &c.method, &c.url, &c.body, &c.headers, &c.opts);
        self.hedge.sent(id, copy, bridge::now());
    }

    /// Aborts request `id` (and its hedge copy).
    fn abort_req(&mut self, id: u32) {
        bridge::abort(id);
        if let Some(x) = self.hedge.forget(id) {
            bridge::abort(x);
        }
    }

    fn page_request(&mut self, req: Req) {
        if self.marks.iter().all(|m| m.0 != "request") {
            self.mark("request");
        }
        let nav = self.nav;
        let wl = matches!(&req, Req::Browse { id, .. } if id == "VLWL");
        let id = self.send(req, Purpose::Page { nav }, false);
        self.home_sent(id);
        if wl {
            self.wl_check();
        }
    }

    fn wl_reset(&mut self) {
        self.wl = if self.wl_newest { WlSort::Settled } else { WlSort::Open };
        self.wl_held = None;
        self.wl_note = false;
    }

    /// Not English: the page's own menu labels cannot be read, so the English menu goes out beside it.
    fn wl_check(&mut self) {
        if self.wl == WlSort::Open && !self.hl.eq_ignore_ascii_case("en") {
            self.wl = WlSort::Checking;
            let nav = self.nav;
            self.send(Req::WatchLaterMenu, Purpose::WatchLaterMenu { nav }, false);
        }
    }

    /// The sort decision is in: the held page (if it is here) renders now.
    fn wl_settle(&mut self) {
        self.wl = WlSort::Settled;
        if let Some(body) = self.wl_held.take() {
            self.on_page_body(200, &body);
        }
    }

    /// A menu (from the page in English, or the English check) that is not newest first: YouTube's
    /// own sort command (`Sorting`); otherwise `Settled`.
    fn wl_menu(&mut self, json: &Value) {
        match crate::features::watch_later::newest_order(json) {
            Some(req) => {
                self.wl = WlSort::Sorting;
                let nav = self.nav;
                self.send(req, Purpose::WatchLaterOrder { nav }, false);
            }
            None => {
                self.wl_newest = crate::features::watch_later::newest_selected(json);
                self.wl = WlSort::Settled;
            }
        }
    }

    fn refresh_config(&mut self, retry_nav: Option<u32>) {
        if self.config_inflight {
            return;
        }
        self.config_inflight = true;
        let id = next_id();
        bridge::fetch_opts(id, "GET", "https://www.youtube.com/sw.js_data", "", "{}", if retry_nav.is_none() { LOW } else { "{}" });
        self.pending.insert(id, Pending { purpose: Purpose::Config { retry_nav }, sent: bridge::now(), ep: "sw.js_data", unmasked: None });
    }

    fn abort_nav_requests(&mut self) {
        let stale: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, p)| matches!(p.purpose, Purpose::Page { .. } | Purpose::WatchLaterOrder { .. } | Purpose::WatchLaterMenu { .. } | Purpose::Resolve { .. } | Purpose::Cont { .. }))
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.pending.remove(&id);
            self.abort_req(id);
        }
    }

    /// The request that loads `route`, when it needs no resolve step.
    fn page_req(&self, route: &Route) -> Option<Req> {
        Some(match route {
            Route::Home => Req::Browse { id: "FEwhat_to_watch".into(), params: None },
            Route::Subs => Req::Browse { id: "FEsubscriptions".into(), params: None },
            Route::History => Req::Browse { id: "FEhistory".into(), params: None },
            Route::Playlists => Req::Browse { id: "FEplaylist_aggregation".into(), params: None },
            Route::Search { q, sp } => Req::Search { q: q.clone(), params: sp.clone() },
            Route::Playlist { list } => Req::Browse { id: format!("VL{list}"), params: None },
            Route::Channel { key: ChannelKey::Id(id), .. } => Req::Browse { id: id.clone(), params: Some(CHANNEL_VIDEOS_PARAMS.into()) },
            Route::Channel { key: ChannelKey::Path(p), .. } => Req::Browse { id: self.channels.get(p)?.clone(), params: Some(CHANNEL_VIDEOS_PARAMS.into()) },
            Route::Watch { .. } | Route::Unknown { .. } => return None,
        })
    }

    // ----------------------------------------------------------- snapshots

    fn snapshot(&self) -> Option<snap::SnapRef<'_>> {
        let head = self.feed.head.as_ref()?;
        if !self.rendered || self.feed.shown.is_empty() {
            return None;
        }
        Some(snap::SnapRef {
            url: &self.url,
            head,
            shown: self.feed.shown.len().min(snap::MAX_ITEMS),
            items: &self.feed.shown,
            buffered: &self.feed.buf,
            cont: self.feed.cont.as_deref(),
            cont_ep: self.feed.cont_ep,
            scroll: bridge::scroll_y(),
            at: bridge::epoch(),
        })
    }

    /// Writes the current feed page to sessionStorage (tab lifetime only).
    fn write_snap(&mut self) {
        if self.fx.topic_on() || self.fx.chip_on() {
            return; // topic and YouTube-chip cards stay in memory only
        }
        let Some(sn) = self.snapshot() else { return };
        let json = snap::to_json_ref(&sn);
        self.snap_at.clear();
        if json.len() > snap::MAX_BYTES {
            return;
        }
        let mut idx = snap::index_from(&bridge::session_get(snap::INDEX));
        idx.retain(|u| *u != self.url);
        idx.insert(0, self.url.clone());
        while idx.len() > snap::MAX_SNAPS {
            if let Some(u) = idx.pop() {
                bridge::session_del(&snap::key(&u));
            }
        }
        if !bridge::session_set(&snap::key(&self.url), &json) {
            // Quota (shared with YouTube's own pages in this tab): keep only this one.
            for u in idx.drain(1..) {
                bridge::session_del(&snap::key(&u));
            }
            if !bridge::session_set(&snap::key(&self.url), &json) {
                idx.clear();
            }
        }
        bridge::session_set(snap::INDEX, &snap::index_json(&idx));
    }

    fn read_snap(&self) -> Option<Snap> {
        if !matches!(self.route, Route::Home | Route::Subs | Route::History | Route::Playlists | Route::Search { .. } | Route::Channel { .. } | Route::Playlist { .. }) {
            return None;
        }
        if self.route.needs_sign_in() && !self.auth.signed_in() {
            return None;
        }
        let s = bridge::session_get(&snap::key(&self.url));
        if s.is_empty() {
            return None;
        }
        snap::from_json(&s, &self.url).filter(|s| {
            // Pre-0.13.4 Watch later snapshots have a playlist metadata header and the old
            // server order. New Watch later pages use the plain feed header.
            !s.items.is_empty() && (self.url != crate::features::watch_later::PAGE_URL || matches!(s.head, Head::Feed { .. }))
        })
    }

    /// Takes over a snapshot's state; returns its page HTML. `trim`: the page opens at its top,
    /// so at most one chunk of cards is written (the others go back to the buffer).
    fn adopt_snap(&mut self, sn: Snap, trim: bool) -> String {
        let mut items = sn.items;
        let shown = if sn.shown == 0 { items.len().min(CHUNK) } else { sn.shown };
        let shown = if trim { shown.min(CHUNK) } else { shown };
        let buf = items.split_off(shown.min(items.len()));
        self.remember(&items);
        self.remember(&buf);
        self.generation += 1;
        self.cont_busy = false;
        self.want = false;
        self.feed = FeedState {
            seen: items.iter().chain(buf.iter()).map(|i| i.key().to_string()).collect(),
            head: Some(sn.head),
            shown: items,
            buf,
            cont: sn.cont,
            cont_ep: sn.cont_ep,
            ..Default::default()
        };
        self.fx_new_feed();
        let title = self.head_title();
        self.set_title(&title);
        let head = self.feed.head.clone().unwrap_or_else(|| render::feed_head(""));
        render::page(&head, &self.fx_visible(&self.feed.shown), self.has_more(), "", ImgPlan::first(Lay::Grid, self.cols))
    }

    fn head_title(&self) -> String {
        match (&self.route, &self.feed.head) {
            (Route::Home, _) => String::new(),
            (Route::Search { q, .. }, _) => q.clone(),
            (_, Some(h)) => h.title().to_string(),
            _ => String::new(),
        }
    }

    // ---------------------------------------------------------- navigation

    fn save_current(&mut self) {
        if !self.rendered {
            return;
        }
        self.write_snap();
        let html = bridge::get_html(MAIN);
        let s = Saved {
            url: self.url.clone(),
            html,
            scroll: bridge::scroll_y(),
            feed: std::mem::take(&mut self.feed),
            title: self.title.clone(),
            stats: std::mem::take(&mut self.stats),
        };
        self.saved.retain(|x| x.url != s.url);
        self.saved.push(s);
        if self.saved.len() > SAVED_PAGES {
            self.saved.remove(0);
        }
    }

    /// The sidebar's active indicator follows the page: its clip insets move on the
    /// house spring (client.css), the leading edge first (`data-dir`). Keyboard and
    /// back/forward paints set `data-still` first, which makes the move instant.
    fn side_move(&mut self) {
        let Some(to) = render::side_index(self.route.nav_key()) else { return };
        if to == self.side_at {
            return;
        }
        bridge::attr("ytc-side", "data-dir", if to > self.side_at { "d" } else { "u" });
        let (t, b) = render::side_insets(to);
        bridge::css_var(render::SIDE_PILL, "--st", &t);
        bridge::css_var(render::SIDE_PILL, "--sb", &b);
        self.side_at = to;
    }

    /// The last press was a key (not the pointer): focus moves and closes are instant.
    pub(super) fn keyed(&self) -> bool {
        self.last_key > self.last_ptr
    }

    fn navigate(&mut self, route: Route, url: String, mode: Mode) {
        self.media_rest = None;
        self.imp_end();
        self.voice_leave();
        self.tel_leave();
        self.logo_done();
        self.tv_keyboard_leave();
        self.direct = false;
        self.acts.leave();
        self.tv_drawers_reset();
        self.stop_player();
        self.save_current();
        self.fx_leave();
        self.abort_nav_requests();
        self.nav += 1;
        self.mo_nav(mode == Mode::Pop);
        self.nav_start = bridge::now();
        self.marks = vec![("navigate", 0.0)];
        self.rendered = false;
        self.retried = false;
        self.wl_reset();
        self.generation += 1;
        self.feed = FeedState { cont_ep: "browse", ..Default::default() };
        self.cont_busy = false;
        self.want = false;
        self.swr = false;
        self.await_page = None;
        self.preloaded_marked = false;
        if self.held.take().is_some() {
            bridge::html(PILL, "");
        }
        self.stats = Stats::default();
        if mode == Mode::Push {
            bridge::push(&url);
        }
        self.route = route;
        self.url = url;
        if matches!(self.route, Route::Watch { .. }) {
            crate::ready::taint();
        }
        self.tel_nav(mode == Mode::Pop);
        self.spec_cancel();
        bridge::attr("ytc", "data-nav", self.route.nav_key());
        self.side_move();
        bridge::value("ytc-q", if let Route::Search { q, .. } = &self.route { q } else { "" });
        self.tv_keyboard_label();
        if mode == Mode::Pop {
            if let Some(i) = self.saved.iter().position(|s| s.url == self.url) {
                let s = self.saved.remove(i);
                crate::telemetry::nav_src("memory");
                bridge::html(MAIN, &s.html);
                self.restore_scroll(s.scroll);
                self.feed = s.feed;
                self.fx_restored();
                self.stats = s.stats;
                self.set_title(&s.title.clone());
                if let Route::Watch { v, t, list } = self.route.clone() {
                    bridge::html(player::ui::BOX, &player::html(&v));
                    let w = self.cached_watch(&wkey(&v, list.as_deref()));
                    self.mount_player(&v, t, list);
                    self.acts.on_restore(&v, w.as_deref());
                    if let Some(w) = w.as_deref() {
                        self.player_chapters(&v, w);
                        self.tv_watch_data(w, &v);
                    }
                    self.up_head();
                }
                self.mark("restored");
                self.after_paint();
                return;
            }
        }
        // Home: the ready response (YouTube's answer, fetched ahead) is the page.
        if mode == Mode::Push && self.route == Route::Home {
            if let Some(r) = self.ready_take() {
                bridge::scroll_to(0.0);
                self.ready_show(r);
                return;
            }
        }
        // A first page already in memory (the logo's or a hover prefetch, the background refresh)
        // is the page: written at once over the old one, instead of its older snapshot followed by
        // a second render of the same page in the same task.
        if mode == Mode::Push && self.page_req(&self.route).is_some() && self.page_cached(&self.route.url()) {
            // Where the old path painted a snapshot (no entrance), the cards make none either.
            self.direct = self.snap_listed(&self.route.url());
            bridge::scroll_to(0.0);
            self.load();
            return;
        }
        // Going Home is a new Home from YouTube: the skeleton, never the old Home first.
        let sn = if mode == Mode::Push && self.route == Route::Home { None } else { self.read_snap() };
        if matches!(self.route, Route::Home | Route::Subs) {
            self.perf.hit("feed-snapshot", sn.is_some());
        }
        if let Some(sn) = sn {
            let (scroll, age) = (sn.scroll, bridge::epoch() - sn.at);
            let html = self.adopt_snap(sn, mode != Mode::Pop || scroll <= 0.0);
            crate::telemetry::nav_src("snapshot");
            bridge::html(MAIN, &html);
            self.restore_scroll(if mode == Mode::Pop { scroll } else { 0.0 });
            self.mark("repaint");
            bridge::mark("ytcRepaint");
            self.after_paint();
            // A snapshot younger than the background age is fresh enough: no slow refresh.
            if !(mode == Mode::Pop && age < SNAP_FRESH_MS) && age >= self.bg_age {
                self.swr = true;
                self.load();
            }
            return;
        }
        bridge::scroll_to(0.0);
        self.start_route();
    }

    /// A page painted from memory or a snapshot is ready to use.
    fn after_paint(&mut self) {
        self.rendered = true;
        self.ready_lazy(false);
        self.tv_after_paint();
        self.spec_schedule();
        if self.perf_nav != self.nav && self.nav > 1 {
            self.perf_nav = self.nav;
            let what = match self.route {
                Route::Home => "nav:home-cards",
                Route::Subs => "nav:subs-cards",
                Route::Search { .. } => "nav:search-cards",
                Route::Watch { .. } => "nav:watch-related",
                _ => "nav:page-cards",
            };
            self.perf.nav(what, bridge::now() - self.nav_start);
        }
        self.fx_painted();
        if self.has_more() {
            bridge::observe(MORE);
        }
        self.mark_items();
        self.ready();
        self.pump();
    }

    fn set_title(&mut self, t: &str) {
        self.title = t.to_string();
        bridge::title(&if t.is_empty() { crate::render::SITE.to_string() } else { format!("{t} - {}", crate::render::SITE) });
    }

    /// Immediate UI for the current route, then `load()`.
    fn start_route(&mut self) {
        self.wl_reset();
        let native = native_url(&self.url);
        match self.route.clone() {
            Route::Unknown { .. } => {
                // Pages the client does not render open YouTube's own page directly.
                bridge::replace_doc(&native);
                return;
            }
            r if r.needs_sign_in() && !self.auth.signed_in() => {
                bridge::html(MAIN, &render::message("Sign in to see this page", "Subscriptions, History and Watch later need a signed-in YouTube account.", None, &native));
                self.set_title("");
                self.rendered = true;
                self.ready();
                return;
            }
            Route::Watch { v, t, list } => {
                let known = self.known.get(&v).cloned();
                self.mo.info_sk = known.is_none();
                let shell = render::watch_shell(&player::html(&v), &render::watch_info_partial(known.as_ref()), self.cfg.prefs.theater);
                bridge::html(MAIN, &shell);
                let key = wkey(&v, list.as_deref());
                let cached = self.watch_cache.iter().any(|(k, _)| *k == key);
                self.perf.hit("watch-next", cached);
                if !cached {
                    self.perf.hit("watch-next-inflight", self.prefetching.contains_key(&key));
                }
                self.mount_player(&v, t, list);
                if let Some(k) = known {
                    self.set_title(&k.title);
                }
                if let Some(w) = self.cached_watch(&key) {
                    crate::telemetry::nav_src("cache");
                    self.render_watch(&w);
                    return;
                }
            }
            _ => {
                if !self.first_ready_pending() {
                    bridge::html(MAIN, &render::skeleton(&self.route));
                }
                // TV: a focus while the page loads (the rail; the content takes it when it arrives).
                self.tv_focus_check();
                if self.home_hold {
                    // Sent as soon as the stored config answers (`on_storage`).
                    return;
                }
            }
        }
        self.load();
    }

    /// True only for the very first page (the shell already shows a skeleton).
    fn first_ready_pending(&self) -> bool {
        !self.first_ready && self.nav == 1
    }

    fn load(&mut self) {
        if let Route::Watch { v, list, .. } = &self.route {
            if let Some(&id) = self.prefetching.get(&wkey(v, list.as_deref())) {
                // The hover prefetch will render this page when it lands (hedged from now on if it was a background one).
                crate::telemetry::nav_src("inflight");
                self.mark("request");
                self.hedge_waits(id);
                return;
            }
            let req = next_req(v, list.as_deref());
            self.page_request(req);
            return;
        }
        let key = self.route.url();
        let hit = self.page_cache.iter().position(|(u, _, at)| *u == key && bridge::epoch() - at < PAGE_TTL_MS);
        self.perf.hit("page-prefetch", hit.is_some());
        if let Some(i) = hit {
            let (_, body, _) = self.page_cache.remove(i);
            if self.route == Route::Home {
                self.hr.doc_meta = self.hr.prefetch_meta.take();
            }
            crate::telemetry::nav_src("prefetch");
            bridge::mark_val("ytcHomeSrc", "prefetch");
            self.mark("request");
            self.mark("prefetched");
            self.on_page_body(200, &body);
            return;
        }
        if let Some(&id) = self.page_prefetching.get(&key) {
            crate::telemetry::nav_src("inflight");
            bridge::mark_val("ytcHomeSrc", "inflight");
            self.mark("request");
            self.await_page = Some(key);
            self.hedge_waits(id);
            return;
        }
        if let Some(req) = self.page_req(&self.route) {
            self.page_request(req);
            return;
        }
        if let Route::Channel { key: ChannelKey::Path(p), .. } = &self.route {
            let p = p.clone();
            self.mark("request");
            let nav = self.nav;
            self.send(Req::Resolve { url: format!("https://www.youtube.com{p}") }, Purpose::Resolve { nav, path: p }, false);
        }
    }

    fn ready(&mut self) {
        // TV: a freshly rendered page gets a focus (never lost on a remote).
        self.tv_focus_check();
        if !self.first_ready {
            self.first_ready = true;
            bridge::mark("ytcReady");
        }
        bridge::mark("ytcNavReady");
        self.mark("ready");
        crate::telemetry::nav_inserted(&self.marks);
        if self.debug {
            self.show_debug();
        }
    }

    // ----------------------------------------------------------- callbacks

    pub fn on_storage(&mut self, key: &str, json: &str) {
        if self.fx_storage(key, json) {
            return;
        }
        if key != config::STORAGE_KEY && key != PLAYER_LOG_KEY {
            return;
        }
        if key == PLAYER_LOG_KEY {
            if let Some(Value::Array(a)) = json::parse_str(json) {
                let mine = std::mem::take(&mut self.player_log);
                self.player_log = a.iter().map(Value::to_serde).collect();
                self.player_log.extend(mine);
            }
            return;
        }
        let mut au_changed = false;
        if json != "null" {
            // The first request did not wait for this: adopt it for the next ones.
            let mut stored = Config::from_json(json);
            au_changed = stored.auth_user != self.cfg.auth_user;
            if self.cfg.fetched_at > stored.fetched_at {
                stored.version = self.cfg.version.clone();
                stored.fetched_at = self.cfg.fetched_at;
            }
            if stored.visitor.is_none() {
                stored.visitor = self.cfg.visitor.take();
            }
            self.cfg = stored;
            bridge::session_set(CFG_SESSION, &self.cfg.to_json());
        }
        self.cfg_ready = true;
        let hold = std::mem::take(&mut self.home_hold);
        if hold && !au_changed && self.route == Route::Home && !self.rendered {
            if self.cfg.visitor.is_some() {
                self.load();
            } else {
                // Never stored either (a first run): YouTube's own visitor first (sw.js_data), then Home.
                self.refresh_config(Some(self.nav));
            }
        }
        // (TV: a first page still held for the token has not gone out; it uses this config when it does.)
        // (A first Home still reading the shared ready response starts with this config when it answers.)
        if au_changed && !self.tv.hold && !self.hr.taking && !matches!(self.route, Route::Watch { .. }) {
            // The first request went out for another account index: redo the page.
            self.abort_nav_requests();
            self.swr = false;
            self.rendered = false;
            self.generation += 1;
            self.start_route();
        }
        self.kick_player();
        if self.cfg.stale(bridge::epoch()) {
            let id = self.timer(Timer::ConfigRefresh);
            bridge::timer(id, CONFIG_REFRESH_DELAY_MS);
        }
    }

    /// Toolbar changes apply in place, without navigation or restarting playback.
    pub fn on_storage_change(&mut self, key: &str, json: &str) {
        if key != config::STORAGE_KEY { return; }
        let cfg = Config::from_json(json);
        self.cfg.prefs.sb_on = cfg.prefs.sb_on;
        self.cfg.prefs.sb_skip = cfg.prefs.sb_skip;
        bridge::session_set(CFG_SESSION, &self.cfg.to_json());
        if let Some(p) = self.player.as_mut() { p.apply_sponsor_prefs(cfg.prefs.sb_on, cfg.prefs.sb_skip); }
    }

    pub fn toolbar_message(&mut self, message: &str) -> String {
        if !matches!(message, "sponsor-status" | "sponsor-undo") { return "{}".into(); }
        if message == "sponsor-undo" {
            if let Some(p) = self.player.as_mut() { p.toolbar_sponsor_undo(); }
            self.after_player();
        }
        self.player.as_ref().map_or_else(|| "{}".into(), |p| p.toolbar_sponsor_status().to_string())
    }

    /// chrome.storage (persistent) plus this tab's sessionStorage mirror.
    fn save_cfg(&self) {
        let j = self.cfg.to_json();
        bridge::storage_set(config::STORAGE_KEY, &j);
        bridge::session_set(CFG_SESSION, &j);
    }

    fn timer(&mut self, t: Timer) -> u32 {
        let id = next_id();
        self.timers.insert(id, t);
        id
    }

    pub fn on_timer(&mut self, id: u32) {
        if self.voice_timer(id) {
            return;
        }
        if self.fx_timer(id) {
            return;
        }
        if self.acts.owns_timer(id) {
            self.acts.on_timer(&net!(self), id);
            return;
        }
        if !self.timers.contains_key(&id) {
            if player::own::warm_timer(id) {
                return;
            }
            if let Some(p) = self.player.as_mut() {
                if p.on_timer(id) {
                    self.after_player();
                }
            }
            return;
        }
        match self.timers.remove(&id) {
            Some(Timer::ConfigRefresh) => self.refresh_config(None),
            Some(Timer::SavePrefs) => {
                self.prefs_dirty = false;
                self.save_cfg();
            }
            Some(Timer::Spec { nav }) => {
                self.spec_scheduled = false;
                if nav == self.nav {
                    self.spec_pass();
                    self.bg_check();
                }
            }
            Some(Timer::NetProbe) => {
                if self.hr.offline || (self.offline && self.player_failed.is_some()) {
                    self.net_probe();
                }
            }
            Some(Timer::TvHold) => self.tv_release(),
            Some(Timer::TvPoll { generation }) => self.tv_poll(generation),
            Some(Timer::TvFocus) => self.tv_focus_check(),
            Some(Timer::Hedge { id }) => self.hedge_fire(id),
            Some(Timer::ReadyFill { lazy }) => self.ready_timer(lazy),
            Some(Timer::ImpFlush) => self.imp_timer(),
            Some(Timer::HomeHedge { nav }) => self.home_hedge(nav),
            Some(Timer::HomeSlow { nav }) => self.home_slow(nav),
            Some(Timer::TvScrub { generation }) => self.tv_scrub_rest(generation),
            Some(Timer::TvUpdates { fast }) => self.tv_updates_tick(fast),
            Some(Timer::Bg) => {
                self.tv_tick();
                self.bg_check();
                self.ready_lazy(false);
                let id = self.timer(Timer::Bg);
                bridge::timer(id, BG_TICK_MS);
            }
            Some(Timer::Autoplay { nav }) => {
                if nav == self.nav {
                    self.up_go();
                }
            }
            Some(Timer::PageHover(url)) => {
                if self.hover == url {
                    self.prefetch_page(&url);
                }
            }
            Some(Timer::MediaPre(v, n)) => {
                // A wake that fires a little early queues the next one.
                self.media_wake = -1.0;
                self.media_pre(&v, n);
            }
            Some(Timer::Preview(v)) => {
                if self.preview_v == v {
                    if let Some(a) = self.known.get(&v).and_then(|k| k.anim.clone()) {
                        bridge::hover_insert(".th", &render::preview(&a));
                        // It fades in over the thumbnail once it lands (at once when cached).
                        bridge::fade_imgs("#ytc .th>.pv");
                        self.preview_on = true;
                    }
                }
            }
            Some(Timer::Pump { nav, generation }) => {
                self.pump_scheduled = false;
                if nav == self.nav && generation == self.generation {
                    if let Some(token) = self.feed.cont.clone().filter(|_| !self.cont_busy && self.wants_more()) {
                        let low = !self.want;
                        self.send_cont(token, low);
                    }
                } else {
                    self.pump();
                }
            }
            Some(Timer::Unpin { nav }) => {
                // A hidden tab renders no frames (sizes not remembered yet): wait.
                if nav == self.nav && !bridge::visible() {
                    let id = self.timer(Timer::Unpin { nav });
                    bridge::timer(id, RESTORE_LAYOUT_MS);
                } else if nav == self.nav {
                    bridge::class_all(RESTORED, "rs", false);
                }
            }
            Some(Timer::Still { nav }) => self.mo_still_done(nav),
            Some(Timer::Logo { generation }) => self.logo_fire(generation),
            Some(Timer::Suggest(q)) => {
                if self.suggest_q == q {
                    self.suggest(&q);
                }
            }
            Some(Timer::SearchWarm(q)) => {
                if self.warm_q == q {
                    self.search_warm(&q);
                }
            }
            None => {}
        }
    }

    /// `next` (and the VISIONOS streams) of a video before it is opened.
    /// Intent (hover, press, focus) goes out at once, making room by dropping
    /// the oldest other prefetch; speculative ones wait for a free slot.
    fn prefetch(&mut self, v: &str, list: Option<&str>) {
        self.prefetch_as(v, list, false);
    }

    fn prefetch_as(&mut self, v: &str, list: Option<&str>, spec: bool) -> bool {
        let key = wkey(v, list);
        if self.prefetching.contains_key(&key) || self.watch_cache.iter().any(|(k, _)| *k == key) || self.route_wkey().as_deref() == Some(key.as_str()) {
            if !spec {
                // Promote: the streams too, now.
                self.prefetch_vision(v, false);
            }
            return false;
        }
        if self.prefetching.len() >= MAX_PREFETCH {
            if spec {
                return false;
            }
            let oldest = self.pending.iter().filter(|(_, p)| matches!(p.purpose, Purpose::Prefetch { .. })).min_by(|a, b| a.1.sent.total_cmp(&b.1.sent)).map(|(id, _)| *id);
            if let Some(id) = oldest {
                if let Some(Pending { purpose: Purpose::Prefetch { v, list, .. }, .. }) = self.pending.remove(&id) {
                    self.prefetching.remove(&wkey(&v, list.as_deref()));
                    self.abort_req(id);
                }
            }
        }
        let id = self.send(next_req(v, list), Purpose::Prefetch { v: v.to_string(), list: list.map(String::from), spec }, spec);
        self.prefetching.insert(key, id);
        if !spec {
            self.prefetch_vision(v, false);
        }
        true
    }

    /// The pointer rests on a link to `v`.
    fn hovering(&self, v: &str) -> bool {
        self.hover.strip_prefix("w:").and_then(|k| k.split('|').next()) == Some(v) || self.intent_v == v
    }

    /// Start the intent's dwell now, even while its VISIONOS response is still loading.
    fn media_pre_soon(&mut self, v: &str) {
        if self.media_rest.as_ref().is_none_or(|(old, _)| old != v) {
            self.media_rest = Some((v.to_string(), bridge::now()));
        }
        if self.vision_cached(v).is_some() {
            self.media_pre(v, 0);
        }
    }

    /// Still on the link after `MEDIA_PRE_MS`: the first media of the formats the player would
    /// start with (`player::own::prefetch_media`; decodingInfo may still be on its way: two more tries).
    fn media_pre(&mut self, v: &str, n: u8) {
        if !self.hovering(v) || matches!(&self.route, Route::Watch { v: w, .. } if w == v) || self.player.as_ref().is_some_and(|p| p.v == v) {
            return;
        }
        let Some(&(_, at)) = self.media_rest.as_ref().filter(|(rest, _)| rest == v) else { return };
        let wait = (MEDIA_PRE_MS as f64 - (bridge::now() - at)).ceil();
        if wait > 0.0 {
            if self.media_wake != at {
                self.media_wake = at;
                let id = self.timer(Timer::MediaPre(v.to_string(), n));
                bridge::timer(id, wait as u32);
            }
            return;
        }
        // Its media came with an earlier rest: the first frame can be decoded now, and nothing more is asked.
        self.warm_frame(v);
        if player::own::prefetched(v) {
            return;
        }
        let Some(body) = self.vision_cached(v) else { return };
        let start = self.fx_resume(v).or_else(|| self.resume.get(v).copied()).unwrap_or(0.0);
        if !player::own::prefetch_media(v, &body, &self.cfg.prefs, self.bw.bps, start) && n < 2 {
            let id = self.timer(Timer::MediaPre(v.to_string(), n + 1));
            bridge::timer(id, MEDIA_PRE_MS);
        }
    }

    /// Pointer press or keyboard focus on a link: the same prefetch as hover, now.
    pub fn on_intent(&mut self, href: &str) {
        self.last_input = bridge::now();
        // Focus (TV remote, keyboard) rests on a link like a pointer does: the media prefetch
        // follows it, and what was loading for the link focus left is cancelled.
        let v = match route::parse(href) {
            Some(Route::Watch { v, .. }) => v,
            _ => String::new(),
        };
        let here = if let Route::Watch { v, .. } = &self.route { v.clone() } else { String::new() };
        player::own::pre_cancel(&[&v, &here]);
        if !v.is_empty() {
            player::own::warm_keep(&[&v]);
        }
        self.intent_v = v;
        match route::parse(href) {
            Some(Route::Watch { v, list, .. }) => {
                self.prefetch(&v, list.as_deref());
                self.media_pre_soon(&v);
            }
            Some(r @ (Route::Channel { .. } | Route::Playlist { .. } | Route::Home | Route::Subs | Route::History | Route::Playlists | Route::Search { .. })) => {
                let url = r.url();
                if self.page_req(&r).is_some() {
                    self.prefetch_page(&url);
                }
            }
            _ => {}
        }
        self.media_rest_check();
    }

    fn media_rest_check(&mut self) {
        if self.media_rest.as_ref().is_some_and(|(v, _)| !self.hovering(v)) {
            self.media_rest = None;
        }
    }

    /// The current watch page's cache key.
    fn route_wkey(&self) -> Option<String> {
        if let Route::Watch { v, list, .. } = &self.route { Some(wkey(v, list.as_deref())) } else { None }
    }

    /// Hover: also fetch the VISIONOS streams, so a click needs only googlevideo.
    fn prefetch_vision(&mut self, v: &str, spec: bool) {
        if spec {
            let now = bridge::now();
            if now - self.spec_vision.0 > 600_000.0 {
                self.spec_vision = (now, 0);
            }
            if self.spec_vision.1 >= SPEC_VISION_BUDGET {
                return;
            }
        }
        if !self.cfg.own_player || self.cfg.visitor.is_none() || self.vision_hold || self.vision_block_until > bridge::now() || self.vision_prefetching.contains(v) || self.vision_cached(v).is_some() || self.vision_prefetching.len() >= if spec { MAX_PREFETCH } else { MAX_PREFETCH + 3 } {
            return;
        }
        let route = self.vroute();
        if route == crate::api::VisionRoute::Account {
            self.auth = crate::tv::auth();
        }
        let (body, headers, opts) = crate::api::vision_request(&self.cfg, &self.auth, (bridge::epoch() / 1000.0) as u64, &route, v);
        let id = next_id();
        let plain = crate::api::player_url();
        let url = crate::api::vision_url();
        bridge::fetch_opts(id, "POST", &url, &body, &headers, opts);
        let unmasked = (url != plain).then(|| Box::new(Resend { url: plain, body, headers, opts }));
        self.pending.insert(id, Pending { purpose: Purpose::VisionPrefetch { v: v.to_string(), spec, route }, sent: bridge::now(), ep: "player-visionos", unmasked });
        self.vision_prefetching.insert(v.to_string());
        if spec {
            self.spec_vision.1 += 1;
        }
    }

    /// The VISIONOS route for the next request (`va: true` asks for the account session).
    fn vroute(&self) -> crate::api::VisionRoute {
        match &self.vision_route {
            crate::api::VisionRoute::Anon if self.cfg.vision_auth => crate::api::VisionRoute::Account,
            r => r.clone(),
        }
    }

    fn vision_cached(&self, v: &str) -> Option<Rc<Vec<u8>>> {
        let now = bridge::now();
        self.vision_cache.iter().find(|(k, at, _)| k == v && now - at < VISION_MAX_AGE_MS).map(|e| e.2.clone())
    }

    fn cache_vision(&mut self, v: &str, body: Rc<Vec<u8>>) {
        self.vision_cache.retain(|(k, _, _)| k != v);
        self.vision_cache.push((v.to_string(), bridge::now(), body));
        if self.vision_cache.len() > VISION_CACHE {
            self.vision_cache.remove(0);
        }
    }

    /// Hover prefetch of a channel, playlist or feed page (raw response kept in memory).
    /// Home and Subscriptions are skipped while their snapshot is younger than the background
    /// age: opening them paints it without asking YouTube.
    fn prefetch_page(&mut self, url: &str) {
        if url == self.route.url() {
            return;
        }
        match route::parse(url) {
            // Home paints the ready response when there is one (no request needed).
            Some(Route::Home) if self.ready_waiting() => return,
            Some(Route::Subs) if self.snap_fresh(url) => return,
            _ => {}
        }
        self.prefetch_page_now(url);
    }

    fn prefetch_page_now(&mut self, url: &str) {
        if self.page_prefetching.contains_key(url) || self.page_prefetching.len() >= MAX_PREFETCH {
            return;
        }
        let now = bridge::epoch();
        if self.page_cache.iter().any(|(u, _, at)| u == url && now - at < PAGE_TTL_MS) {
            return;
        }
        let Some(route) = route::parse(url) else { return };
        if route.needs_sign_in() && !self.auth.signed_in() {
            return;
        }
        let Some(req) = self.page_req(&route) else { return };
        let id = self.send(req, Purpose::PagePrefetch { url: url.to_string(), bg: false }, false);
        self.page_prefetching.insert(url.to_string(), id);
    }

    pub fn on_hover(&mut self, href: &str, class: &str) {
        // Media prefetches still loading for a link the pointer left are cancelled.
        let next = match route::parse(href) {
            Some(Route::Watch { v, .. }) => v,
            _ => String::new(),
        };
        let here = if let Route::Watch { v, .. } = &self.route { v.clone() } else { String::new() };
        // Only the pointer on another video link ends a focus rest (the browser also reports
        // pointer changes when focus scrolls the page under a still pointer).
        if !next.is_empty() {
            self.intent_v.clear();
            player::own::warm_keep(&[&next]);
        }
        player::own::pre_cancel(&[&next, &here, &self.intent_v]);
        self.hover.clear();
        if self.preview_on {
            bridge::remove_all(".pv");
            self.preview_on = false;
        }
        self.preview_v.clear();
        match route::parse(href) {
            Some(Route::Watch { v, list, .. }) => {
                let key = wkey(&v, list.as_deref());
                self.hover = format!("w:{key}");
                if class.split(' ').any(|c| c == "th") && self.known.get(&v).is_some_and(|k| k.anim.is_some()) {
                    self.preview_v = v.clone();
                    let id = self.timer(Timer::Preview(v.clone()));
                    bridge::timer(id, PREVIEW_MS);
                }
                // No delay for videos: the related list must be there on click.
                self.prefetch(&v, list.as_deref());
                self.media_pre_soon(&v);
            }
            Some(r @ (Route::Channel { .. } | Route::Playlist { .. } | Route::Home | Route::Subs | Route::History | Route::Playlists | Route::Search { .. })) => {
                let url = r.url();
                if self.page_req(&r).is_some() {
                    self.hover = url.clone();
                    let id = self.timer(Timer::PageHover(url));
                    bridge::timer(id, HOVER_MS);
                }
            }
            _ => {}
        }
        self.media_rest_check();
    }

    pub fn on_click(&mut self, href: &str, act: &str, arg: &str, mods: u32) -> bool {
        // Bit 32 = keyboard activation (not a modifier).
        let kbd = mods & 32 != 0;
        self.last_input = bridge::now();
        if mods & !32 != 0 {
            return false;
        }
        crate::telemetry::input(if kbd { "keyboard" } else { "pointer" });
        if act.starts_with("tv-kb-") {
            return self.tv_keyboard_click(act, arg);
        }
        if act.starts_with("tv-") {
            return self.tv_click(act);
        }
        self.tv_opened(href);
        if act.starts_with("vk-") {
            return self.voice_click(act, kbd);
        }
        self.player_input();
        if act == "fx-sf" {
            return self.fx_search_filter(href, arg, kbd);
        }
        if act.starts_with("fx-") {
            self.fx.kbd = kbd;
            return self.fx_click(act, arg);
        }
        if act == player::fail::ACT_RETRY || act == player::fail::ACT_NATIVE {
            return self.player_panel_act(act == player::fail::ACT_RETRY);
        }
        if act.starts_with("p-") {
            self.mo.theater = act == "p-theater" && !kbd;
            self.up_cancel();
            let handled = self.player.as_mut().is_some_and(|p| p.on_act(act, arg, kbd));
            self.after_player();
            return handled;
        }
        if act.starts_with("a-") {
            let now = self.player.as_ref().map(|p| p.time()).unwrap_or(0.0);
            match self.acts.on_click(&net!(self), act, arg, now, kbd) {
                Click::Handled => {
                    self.tv_drawer_focus();
                    return true;
                }
                Click::Ignored => return false,
                Click::Pass => {}
            }
        }
        match act {
            "seek" => {
                if let (Route::Watch { .. }, Ok(t)) = (&self.route, arg.parse::<u32>()) {
                    if let Some(p) = self.player.as_mut() {
                        p.input(kbd);
                        p.seek_to(t as f64);
                    }
                    bridge::scroll_to(0.0);
                    return true;
                }
            }
            "up-auto" => {
                self.cfg.autoplay = !self.cfg.autoplay;
                self.save_cfg();
                if !self.cfg.autoplay {
                    self.up_cancel();
                }
                // The switch stays (its knob transitions); only a countdown re-renders the header.
                bridge::attr("ytc-up-auto", "aria-checked", if self.cfg.autoplay { "true" } else { "false" });
                bridge::focus("ytc-up-auto");
                return true;
            }
            "up-now" => {
                self.up_go();
                return true;
            }
            "up-cancel" => {
                self.up_cancel();
                return true;
            }
            "retry" => {
                self.retried = false;
                self.start_route();
                return true;
            }
            "fresh" => {
                bridge::html(PILL, "");
                if let Some(body) = self.held.take() {
                    bridge::scroll_to(0.0);
                    self.hr.doc_meta = self.hr.held_meta.take();
                    self.on_page_body(200, &body);
                }
                return true;
            }
            _ => {}
        }
        if href.is_empty() || href.contains("yt-native=1") {
            return false;
        }
        let Some(route) = route::parse(href) else { return false };
        if route == Route::Home && self.home_in_place() {
            self.home_again();
            return true;
        }
        let url = match &route {
            Route::Channel { .. } | Route::Unknown { .. } => route::local_part(href).unwrap_or("/").split('#').next().unwrap_or("/").to_string(),
            r => r.url(),
        };
        self.mo_click(kbd, &route);
        self.navigate(route, url, Mode::Push);
        self.mo_flush();
        true
    }

    pub fn on_submit(&mut self, form: &str, value: &str) -> bool {
        if form == tvglue::DEV_FORM {
            return self.tv_dev_submit(value);
        }
        if form.starts_with("fx-") {
            return self.fx_submit(form, value);
        }
        if form.starts_with("ytc-a-") {
            return self.acts.on_submit(&net!(self), form, value);
        }
        let q = value.trim();
        if q.is_empty() {
            return true;
        }
        crate::telemetry::input(if self.keyed() { "keyboard" } else { "pointer" });
        self.fx_searched(q);
        self.tv_searched();
        self.suggest_q.clear();
        bridge::html(SUGGEST, "");
        let route = Route::Search { q: q.chars().take(500).collect(), sp: None };
        let url = route.url();
        // Enter is a key: the results paint with no motion (no pill fade, no entrances).
        self.mo.kbd_nav = self.keyed();
        self.navigate(route, url, Mode::Push);
        true
    }

    /// Search-as-you-type: YouTube's own suggest endpoint (same origin).
    pub fn on_input(&mut self, name: &str, value: &str, kind: &str) {
        if name != "q" {
            if self.fx_input(name, value) {
                return;
            }
            if let Some(p) = self.player.as_mut() {
                p.on_input(name, value);
                self.after_player();
            }
            return;
        }
        if kind == "insertReplacementText" {
            // A suggestion was picked from the list.
            self.on_submit("", value);
            return;
        }
        self.fx_typed(value);
        let q: String = value.trim().chars().take(100).collect();
        self.suggest_q = q.clone();
        self.search_warm_soon(&q, SEARCH_WARM_MS);
        if q.is_empty() {
            return;
        }
        if let Some((_, list)) = self.suggest_cache.iter().find(|(k, _)| *k == q) {
            let list = list.clone();
            self.fx_suggestions(&list);
            return;
        }
        let id = self.timer(Timer::Suggest(q));
        bridge::timer(id, SUGGEST_MS);
    }

    /// The search box now holds `q` (typed, or a suggestion chosen with the arrows): its results
    /// are fetched ahead after `ms` without a change (measured: a search answer takes ~0.6–0.9 s,
    /// so Enter paints from memory). A search still loading for other text is dropped at once.
    pub(super) fn search_warm_soon(&mut self, q: &str, ms: u32) {
        self.warm_q = q.to_string();
        let target = Route::Search { q: q.to_string(), sp: Default::default() }.url();
        if let Some(url) = self.warm_url.take() {
            // (Not the one a submitted search is waiting for.)
            if url != target && self.await_page.as_deref() != Some(url.as_str()) {
                if let Some(id) = self.page_prefetching.remove(&url) {
                    self.pending.remove(&id);
                    self.abort_req(id);
                }
            } else {
                self.warm_url = Some(url);
            }
        }
        if q.chars().count() >= 2 {
            let warm = self.timer(Timer::SearchWarm(q.to_string()));
            bridge::timer(warm, ms);
        }
    }

    /// Fetches `q`'s results ahead: low priority, one at a time, ≤ `SEARCH_WARM_BUDGET` a minute,
    /// kept in memory only (the page cache); telemetry sees `req.search` `via: prefetch`, never the text.
    fn search_warm(&mut self, q: &str) {
        let url = Route::Search { q: q.to_string(), sp: Default::default() }.url();
        if url == self.route.url() || self.page_prefetching.contains_key(&url) || self.page_cached(&url) {
            return;
        }
        if self.page_prefetching.len() >= MAX_PREFETCH {
            return;
        }
        let Some(req) = self.page_req(&Route::Search { q: q.to_string(), sp: Default::default() }) else { return };
        if !crate::util::budget_take(&mut self.warm_at, bridge::now(), SEARCH_WARM_BUDGET, 60_000.0) {
            return;
        }
        let id = self.send(req, Purpose::PagePrefetch { url: url.clone(), bg: false }, true);
        self.page_prefetching.insert(url.clone(), id);
        self.warm_url = Some(url);
    }

    fn suggest(&mut self, q: &str) {
        if let Some(old) = self.suggest_id.take() {
            self.pending.remove(&old);
            bridge::abort(old);
        }
        let id = next_id();
        let url = format!("https://www.youtube.com/complete/search?client=youtube&ds=yt&xhr=t&oe=utf-8&hl={}&q={}", self.hl, pct_encode(q));
        bridge::fetch(id, "GET", &url, "", "{}");
        self.pending.insert(id, Pending { purpose: Purpose::Suggest { q: q.to_string() }, sent: bridge::now(), ep: "suggest", unmasked: None });
        self.suggest_id = Some(id);
    }

    pub fn on_key(&mut self, key: &str, editable: bool) -> bool {
        self.logo_done();
        self.last_input = bridge::now();
        crate::telemetry::input("keyboard");
        self.last_key = self.last_input;
        self.fx_form_now(true);
        if self.voice_key(key) {
            return true;
        }
        if self.fx.imp_open && key == "Escape" {
            self.fx_import_open(false, true);
            return true;
        }
        if self.acts.on_key(key) {
            return true;
        }
        if let Some(handled) = self.tv_key(key, editable) {
            return handled;
        }
        if editable {
            return self.fx_key(key);
        }
        // Keyboard-first navigation: g + letter jumps to a page, arrows walk the cards.
        if let Some(at) = self.go_at.take() {
            if let Some(href) = render::go_href(key).filter(|_| bridge::now() - at < 1500.0) {
                self.on_click(href, "", "", 32);
                return true;
            }
        }
        if self.help && (key == "Escape" || key == "?") {
            self.help = false;
            bridge::attr(render::KEYS, "class", "");
            return true;
        }
        let watch = matches!(self.route, Route::Watch { .. });
        match key {
            "g" => {
                self.go_at = Some(bridge::now());
                return true;
            }
            "?" => {
                self.help = true;
                bridge::html(render::KEYS, &render::keys_help());
                bridge::attr(render::KEYS, "class", "on");
                return true;
            }
            "ArrowLeft" | "ArrowRight" | "ArrowUp" | "ArrowDown" if !watch => {
                let delta = match key {
                    "ArrowLeft" => -1,
                    "ArrowRight" => 1,
                    "ArrowUp" => -1000,
                    _ => 1000,
                };
                return bridge::focus_grid(LIST, "a.t", delta);
            }
            _ => {}
        }
        match key {
            // Backquote is a dead key on some layouts (e.g. German): Shift+D too.
            "`" | "D" => {
                self.debug = !self.debug;
                if self.debug {
                    self.show_debug();
                } else {
                    bridge::attr(render::DEBUG, "class", "");
                }
                true
            }
            "/" => {
                bridge::focus("ytc-q");
                true
            }
            // Blocked 0–9 (see `player::own::blocked_key`): taken without waking the player or
            // cancelling Up next.
            k if watch && crate::player::own::blocked_key(k) => true,
            _ => {
                let handled = matches!(self.route, Route::Watch { .. }) && self.player.as_mut().is_some_and(|p| p.on_key(key));
                if handled {
                    self.up_cancel();
                }
                self.after_player();
                handled
            }
        }
    }

    pub fn on_pop(&mut self) {
        let url = bridge::href();
        let url = url.split('#').next().unwrap_or("/").to_string();
        if url == self.url {
            return;
        }
        let route = route::parse(&url).unwrap_or(Route::Unknown { path: url.clone() });
        self.navigate(route, url, Mode::Pop);
    }

    pub fn on_hide(&mut self) {
        self.imp_flush(true);
        self.voice_leave();
        self.fx_flush();
        self.write_snap();
        self.perf_on_hide();
        self.tel_flush(true);
    }

    pub fn on_show(&mut self) {
        self.ready_shown();
        // A video left paused in a forgotten tab: its media and URLs are checked before a play needs them.
        if let Some(p) = self.player.as_mut() {
            p.on_show();
            self.after_player();
        }
    }

    pub fn on_visible(&mut self, id: &str) {
        if self.acts.on_visible(&net!(self), id) {
            return;
        }
        if id != MORE {
            return;
        }
        if self.fx_on_visible() {
            return;
        }
        // Only a real scroll counts: a short list's end is in range at load (a watch page's related column always).
        if bridge::scroll_y() > 0.0 {
            self.feed.scrolled = true;
        }
        if !self.feed.buf.is_empty() {
            self.render_chunk();
        } else if self.feed.cont.is_some() {
            self.want = true;
            crate::telemetry::wait_start("feed.more");
            // The user now waits for a background continuation: it may be hedged from here on.
            let waiting: Vec<u32> = self.pending.iter().filter(|(_, p)| matches!(p.purpose, Purpose::Cont { .. })).map(|(id, _)| *id).collect();
            for id in waiting {
                self.hedge_waits(id);
            }
        }
        self.pump();
    }

    // ------------------------------------------------------------- preload

    fn has_more(&self) -> bool {
        self.feed.cont.is_some() || !self.feed.buf.is_empty() || self.fx.topic_more()
    }

    /// Loaded cards that count for preloading, chunks and marks: Shorts the
    /// Shorts switch hides are left out, so with it off everything counts as
    /// before Shorts were parsed.
    pub(super) fn counted(&self, items: &[Item]) -> usize {
        if !crate::features::filters::short_hidden(self.fx_surface(), &self.fx.prefs) {
            return items.len();
        }
        items.iter().filter(|i| !i.is_short()).count()
    }

    /// Pages that never show Shorts (Subscriptions, History, channels,
    /// playlists) drop them on arrival, exactly as before they were parsed.
    fn admit(&self, items: &mut Vec<Item>) {
        if self.fx_surface() == crate::features::filters::Surface::Other {
            items.retain(|i| !i.is_short());
        }
    }

    fn preload_target(&self) -> usize {
        let shown = self.counted(&self.feed.shown);
        match self.route {
            Route::Home | Route::Subs => FEED_PRELOAD.max(shown + FEED_AHEAD),
            // The related column: nothing ahead until the page is scrolled (most watch pages never are).
            Route::Watch { .. } if !self.feed.scrolled => shown,
            Route::Watch { .. } => shown + CHUNK,
            // (Search answers ~20 per page, one page after the other: more ahead would not arrive sooner.)
            _ => shown + AHEAD,
        }
    }

    fn wants_more(&self) -> bool {
        self.want || self.counted(&self.feed.shown) + self.counted(&self.feed.buf) < self.preload_target()
    }

    /// Fetches the next continuation: now if the user is waiting for it,
    /// otherwise in idle time with low priority until the preload target.
    fn pump(&mut self) {
        if self.cont_busy || !self.rendered || self.feed.cont.is_none() || !self.wants_more() {
            return;
        }
        if self.want {
            let token = self.feed.cont.clone().unwrap_or_default();
            self.send_cont(token, false);
            return;
        }
        // Scrolling, with less than a chunk buffered: the next one is asked for now (low priority),
        // not after the main thread goes idle, which a scroll can delay for up to `PRELOAD_IDLE_MS`.
        if self.feed.scrolled && self.counted(&self.feed.buf) < CHUNK {
            let token = self.feed.cont.clone().unwrap_or_default();
            self.send_cont(token, true);
            return;
        }
        if !self.pump_scheduled {
            self.pump_scheduled = true;
            let id = self.timer(Timer::Pump { nav: self.nav, generation: self.generation });
            bridge::idle(id, PRELOAD_IDLE_MS);
        }
    }

    fn send_cont(&mut self, token: String, low: bool) {
        if let Some(i) = self.cont_cache.iter().position(|(t, _, at)| *t == token && bridge::epoch() - at < self.bg_age * 2.0) {
            // Fetched in the background with its page: no wait.
            let (_, body, _) = self.cont_cache.remove(i);
            self.perf.hit("cont", true);
            self.on_cont_body(200, &body);
            return;
        }
        self.cont_busy = true;
        let (nav, generation) = (self.nav, self.generation);
        self.send(Req::Cont { ep: self.feed.cont_ep, token }, Purpose::Cont { nav, generation }, low);
    }

    /// Moves up to CHUNK buffered items into the DOM (one write).
    fn render_chunk(&mut self) {
        // CHUNK counted cards (hidden Shorts ride along without taking a slot).
        let hide_shorts = self.counted(&self.feed.buf) != self.feed.buf.len();
        let (mut n, mut counted) = (0, 0);
        for it in &self.feed.buf {
            if counted == CHUNK {
                break;
            }
            n += 1;
            if !(hide_shorts && it.is_short()) {
                counted += 1;
            }
        }
        if n == 0 {
            return;
        }
        let items: Vec<Item> = self.feed.buf.drain(..n).collect();
        let vis = self.fx_visible(&items);
        let held = self.fx_hold_pick(&vis);
        let mut s = String::with_capacity(vis.len() * 1000);
        render::cards(&mut s, &vis, ImgPlan::lazy(self.lay()));
        if !s.is_empty() {
            bridge::append(LIST, &s);
            self.fade_thumbs();
            self.imp_scan();
        }
        self.fx_hold(held);
        self.fx_entrance(false, self.feed.shown.is_empty());
        self.feed.shown.extend(items);
        self.fx_chunk(counted, vis.len());
        if self.has_more() {
            bridge::observe(MORE);
        } else {
            bridge::attr(MORE, "class", "more done");
        }
        self.mark_items();
        self.spec_schedule();
    }

    fn mark_items(&mut self) {
        let total = self.counted(&self.feed.shown) + self.counted(&self.feed.buf);
        bridge::mark_val("ytcItems", &total.to_string());
        bridge::mark_val("ytcShown", &self.counted(&self.feed.shown).to_string());
        if !self.preloaded_marked && total >= FEED_PRELOAD {
            self.preloaded_marked = true;
            bridge::mark("ytcPreloaded");
            self.mark("preloaded");
        }
        if self.debug {
            self.show_debug();
        }
    }

    fn lay(&self) -> Lay {
        if matches!(self.route, Route::Watch { .. }) { Lay::Rel } else { Lay::Grid }
    }

    // ------------------------------------------------------------ responses

    /// Body progress of a streaming fetch (only the player asks for it).
    pub fn on_fetch_progress(&mut self, id: u32, status: u32, bytes: f64) {
        if self.player.as_mut().is_some_and(|p| p.on_fetch_progress(id, status, bytes)) {
            self.after_player();
            return;
        }
        player::own::pre_progress(id, bytes);
    }

    /// A piece of a streamed body (only the player's media requests ask for them).
    pub fn on_fetch_chunk(&mut self, id: u32, status: u32, bytes: &[u8]) {
        if self.player.as_mut().is_some_and(|p| p.on_fetch_chunk(id, status, bytes)) {
            self.after_player();
        }
    }

    pub fn on_fetch(&mut self, id: u32, status: u32, body: &[u8]) {
        if let Some(v) = player::own::pre_answer(id, status, body) {
            self.warm_frame(&v);
            return;
        }
        // Hedged reads: the first good answer of the two copies is handled as the original.
        let pending = &self.pending;
        let id = match self.hedge.answer(id, status == 200, |p| pending.contains_key(&p)) {
            crate::hedge::Answer::Plain => {
                self.hedge_now = None;
                id
            }
            crate::hedge::Answer::First { abort, fired } => {
                if let Some(x) = abort {
                    bridge::abort(x);
                }
                self.hedge_now = Some((fired, false));
                id
            }
            crate::hedge::Answer::Copy { primary } => {
                bridge::abort(primary);
                self.hedge_now = Some((true, true));
                primary
            }
            crate::hedge::Answer::Wait | crate::hedge::Answer::Drop => return,
        };
        if self.fx_fetch(id, status, body) {
            return;
        }
        if self.acts.owns_fetch(id) {
            if let Some((v, add)) = self.acts.on_fetch(&net!(self), id, status, body) {
                use crate::features::watch_later::{Edit, Outcome};
                self.fx_wl_settle(&v, if add { Edit::Add } else { Edit::Remove }, &Outcome::Changed);
                if matches!(&self.route, Route::Playlist { list } if list == "WL") { self.fx_wl_redraw(); }
            }
            self.tv_drawer_focus();
            for (kind, reason) in std::mem::take(&mut self.acts.incidents) {
                self.incident(kind, &reason, None);
            }
            return;
        }
        let Some(mut p) = self.pending.remove(&id) else {
            if self.player.as_mut().is_some_and(|p| p.on_fetch(id, status, body)) {
                self.after_player();
            }
            return;
        };
        // YouTube's Home document: its Home data stands in for the `browse` response (the page's facts are
        // kept for the "shown" signals); without Home data, the `browse` request instead.
        let mut body = body;
        if p.ep == HOME_DOC && status != 0 {
            match crate::homedoc::split(body).filter(|_| status == 200) {
                Some((range, meta)) => {
                    body = &body[range];
                    self.doc_learn(&meta);
                    if matches!(p.purpose, Purpose::PagePrefetch { .. }) {
                        self.hr.prefetch_meta = Some(meta);
                    } else {
                        self.hr.doc_meta = Some(meta);
                    }
                }
                None => {
                    self.perf.req(p.ep, bridge::now() - p.sent, false);
                    self.doc_failed(id, p.purpose, status);
                    return;
                }
            }
        }
        // YouTube refused the field mask (a field it no longer knows): the same request whole, at
        // once and under the same id, and no mask for the rest of this document.
        if status == 400 {
            if let Some(r) = p.unmasked.take() {
                if p.ep == "player-visionos" { crate::api::unmask_vision(); } else { crate::api::unmask(); }
                if let Some(x) = self.hedge.forget(id) {
                    bridge::abort(x);
                }
                bridge::fetch_opts(id, "POST", &r.url, &r.body, &r.headers, r.opts);
                // Seen in the speed telemetry with status 400 (numbers only: an
                // incident would carry the page's URL, a search page's terms included).
                self.tel_req(&p, status, body.len());
                self.pending.insert(id, p);
                return;
            }
        }
        let ok = status == 200;
        if status != 0 {
            self.perf.req(p.ep, bridge::now() - p.sent, ok);
        }
        // The original's own latency (also when a copy went out and lost): the deadlines learn a slow YouTube.
        if let Some((fired, won)) = self.hedge_now {
            if ok && !won {
                self.hedge.record(p.ep, bridge::now() - p.sent);
            }
            if fired {
                self.hedge.outcome(p.ep, won, bridge::now());
            }
        }
        self.tel_req(&p, status, body.len());
        if !ok && status != 0 && self.api_incident_nav != self.nav && matches!(p.purpose, Purpose::Page { .. } | Purpose::Cont { .. } | Purpose::Resolve { .. }) {
            self.api_incident_nav = self.nav;
            let snippet: String = String::from_utf8_lossy(&body[..body.len().min(600)]).into_owned();
            self.incident("api-error", &format!("InnerTube HTTP {status}: {snippet}"), None);
        }
        match p.purpose {
            Purpose::Config { retry_nav } => {
                self.config_inflight = false;
                if ok {
                    if let Some((version, visitor)) = config::parse_sw_data(body) {
                        self.cfg.version = version;
                        if visitor.is_some() {
                            self.cfg.visitor = visitor;
                        }
                        self.cfg.fetched_at = bridge::epoch();
                        self.save_cfg();
                    }
                }
                if retry_nav == Some(self.nav) {
                    self.load();
                }
                self.kick_player();
            }
            Purpose::NetProbe => {
                if !self.home_probe(status) {
                    self.on_net_probe(status);
                }
            }
            Purpose::Imp { first } => self.imp_answered(first),
            Purpose::Ready { generation, acct } => self.ready_body(id, generation, acct, false, status, body),
            Purpose::ReadyCont { generation, acct } => self.ready_body(id, generation, acct, true, status, body),
            Purpose::VisionPrefetch { v, route, .. } => {
                self.vision_prefetching.remove(&v);
                let ms = bridge::now() - p.sent;
                let json = if ok { json::parse(body) } else { None };
                let ps = json.as_ref().and_then(|j| j.pointer("/playabilityStatus/status")).and_then(Value::as_str).unwrap_or("");
                let reason = json.as_ref().and_then(|j| j.pointer("/playabilityStatus/reason")).and_then(Value::as_str).unwrap_or("");
                let bot = player::own::bot_check(ps, reason);
                if bot {
                    // Not a verdict yet: the player recovers on its own request.
                    self.vision_hold = true;
                }
                if ps == "OK" {
                    // Warm the video server's connection now (a slow DNS/handshake to a new googlevideo
                    // host cost ~4 s live on 2 Nov 2026), so a click finds it ready.
                    for o in player::own::googlevideo_origins(body).iter().take(2) {
                        player::own::warm_origin(o);
                    }
                    // decodingInfo for its formats now too: the click then chooses at once.
                    if let Some(json) = json.as_ref() {
                        player::own::warm_caps(json);
                    }
                    self.cache_vision(&v, Rc::new(body.to_vec()));
                    self.media_pre(&v, 0);
                }
                let stale = bot && route != self.vroute();
                if let Some(pl) = self.player.as_mut().filter(|pl| pl.v == v) {
                    // Sent on a route a recovery has since replaced: the player asks again on its own.
                    if stale { pl.feed_vision(0, &[], ms) } else { pl.feed_vision(status, body, ms) }
                    self.after_player();
                }
            }
            Purpose::Suggest { q } => {
                if self.suggest_id == Some(id) {
                    self.suggest_id = None;
                }
                let Some(list) = ok.then(|| parse::suggestions(body)).flatten() else { return };
                if self.suggest_q == q {
                    self.fx_suggestions(&list);
                }
                self.suggest_cache.retain(|(k, _)| *k != q);
                self.suggest_cache.push((q, list));
                if self.suggest_cache.len() > SUGGEST_CACHE {
                    self.suggest_cache.remove(0);
                }
            }
            Purpose::Prefetch { v, list, spec } => {
                let key = wkey(&v, list.as_deref());
                self.prefetching.remove(&key);
                if spec {
                    if ok {
                        self.prefetch_vision(&v, true);
                    }
                    self.spec_pump();
                }
                let here = self.route_wkey().as_deref() == Some(key.as_str());
                let Some(json) = ok.then(|| json::parse(body)).flatten() else {
                    if here && !self.rendered {
                        self.page_request(next_req(&v, list.as_deref()));
                    }
                    return;
                };
                self.learn(&json);
                let w = Rc::new(parse::next(&json));
                self.cache_watch(&key, w.clone());
                if here && !self.rendered {
                    self.mark("response");
                    self.render_watch(&w);
                }
            }
            Purpose::ContPrefetch { token } => {
                if ok {
                    self.cont_cache.retain(|(t, _, _)| *t != token);
                    self.cont_cache.push((token, body.to_vec(), bridge::epoch()));
                    if self.cont_cache.len() > CONT_CACHE {
                        self.cont_cache.remove(0);
                    }
                }
            }
            Purpose::PagePrefetch { url, bg } => {
                self.page_prefetching.remove(&url);
                if bg && ok && self.await_page.as_deref() != Some(url.as_str()) {
                    // Home/Subscriptions refreshed in the background: also their first continuation.
                    if let Some(token) = json::parse(body).and_then(|j| parse::browse(&j).cont) {
                        self.send(Req::Cont { ep: "browse", token: token.clone() }, Purpose::ContPrefetch { token }, true);
                    }
                }
                if self.await_page.as_deref() == Some(url.as_str()) {
                    self.await_page = None;
                    if ok {
                        self.mark("prefetched");
                        self.hr.doc_meta = self.hr.prefetch_meta.take();
                        self.on_page_body(status, body);
                    } else if let Some(req) = self.page_req(&self.route) {
                        self.page_request(req);
                    }
                    return;
                }
                if ok {
                    self.page_cache.retain(|(u, _, _)| *u != url);
                    self.page_cache.push((url, body.to_vec(), bridge::epoch()));
                    if self.page_cache.len() > PAGE_CACHE {
                        self.page_cache.remove(0);
                    }
                }
            }
            Purpose::Resolve { nav, path } => {
                if nav != self.nav {
                    return;
                }
                let bid = ok.then(|| json::parse(body)).flatten().and_then(|j| parse::resolve(&j));
                match bid {
                    Some(bid) => {
                        self.channels.insert(path, bid.clone());
                        self.page_request(Req::Browse { id: bid, params: Some(CHANNEL_VIDEOS_PARAMS.into()) });
                    }
                    None => self.fail(status, "This channel could not be found."),
                }
            }
            Purpose::Cont { nav, generation } => {
                if nav != self.nav || generation != self.generation {
                    return;
                }
                self.on_cont_body(status, body);
            }
            Purpose::Page { nav } => {
                if nav != self.nav || !self.home_answered(id, status) {
                    return;
                }
                if status == 0 && self.route == Route::Home {
                    // A dropped connection: the old Home dimmed (if any) until it is back.
                    self.home_offline();
                    return;
                }
                self.on_page_body(status, body);
            }
            Purpose::WatchLaterOrder { nav } => {
                if nav != self.nav { return; }
                if ok && json::parse(body).and_then(|v| v.get("status").and_then(Value::as_str).map(str::to_string)).as_deref() == Some("STATUS_SUCCEEDED") {
                    self.wl_newest = true;
                    self.wl = WlSort::Settled;
                    self.wl_held = None;
                    // A first page still on its way is in the old order: only the newly ordered one renders.
                    let old: Vec<u32> = self.pending.iter().filter(|(_, p)| matches!(p.purpose, Purpose::Page { nav: n } if n == nav)).map(|(id, _)| *id).collect();
                    for id in old {
                        self.pending.remove(&id);
                        self.abort_req(id);
                    }
                    self.fx_wl_invalidate();
                    self.cont_cache.clear();
                    self.page_request(Req::Browse { id: "VLWL".into(), params: None });
                } else {
                    // The list in the order received, with a note and Try again: never a blocked page.
                    self.wl_note = true;
                    self.wl_settle();
                }
            }
            Purpose::WatchLaterMenu { nav } => {
                if nav != self.nav || self.wl != WlSort::Checking { return; }
                match ok.then(|| json::parse(body)).flatten() {
                    Some(json) => {
                        self.wl_menu(&json);
                        if self.wl == WlSort::Settled { self.wl_settle(); }
                    }
                    // The check failed: the page shows as YouTube sent it.
                    None => self.wl_settle(),
                }
            }
        }
    }

    /// A continuation of the current feed (network or background cache).
    fn on_cont_body(&mut self, status: u32, body: &[u8]) {
        let ok = status == 200;
        self.cont_busy = false;
        match ok.then(|| json::parse(body)).flatten() {
            Some(json) => self.take_items(if matches!(self.route, Route::Search { .. }) { parse::search_continuation(&json) } else { parse::continuation(&json) }),
            // Stop paging on errors rather than retrying in a loop.
            None => self.feed.cont = None,
        }
        if self.want && !self.feed.buf.is_empty() {
            self.want = false;
            self.render_chunk();
            crate::telemetry::wait_end("feed.more", serde_json::json!({"route": telglue::route_kind(&self.route)}));
        } else if !self.has_more() {
            self.want = false;
            bridge::attr(MORE, "class", "more done");
        }
        self.mark_items();
        self.pump();
    }

    fn on_page_body(&mut self, status: u32, body: &[u8]) {
        let t0 = bridge::now();
        let swr = self.swr && self.rendered;
        self.on_page_body_inner(status, body);
        if status == 200 {
            self.perf.nav("parse+render", bridge::now() - t0);
            self.tel_body(swr, t0);
        }
    }

    fn on_page_body_inner(&mut self, status: u32, body: &[u8]) {
        self.mark("response");
        let ok = status == 200;
        if !ok && (400..500).contains(&status) && status != 404 && !self.retried {
            // Most likely an outdated client version: refresh it once and retry.
            self.retried = true;
            self.refresh_config(Some(self.nav));
            return;
        }
        let json = ok.then(|| json::parse(body)).flatten();
        let Some(json) = json else {
            if self.swr {
                // Keep showing the snapshot.
                self.swr = false;
                return;
            }
            self.fail(status, if ok { "YouTube returned an unreadable response." } else { "YouTube did not return this page." });
            return;
        };
        if matches!(&self.route, Route::Playlist { list } if list == "WL") && self.wl != WlSort::Settled {
            // A page from a prefetch or the page cache has no check out yet.
            self.wl_check();
            if self.wl == WlSort::Open {
                // English: the page's own menu decides.
                self.wl_menu(&json);
            }
            if self.wl != WlSort::Settled {
                self.wl_held = Some(body.to_vec());
                return;
            }
        }
        if self.swr {
            self.swr = false;
            if bridge::scroll_y() > SWAP_MAX_Y {
                self.held = Some(body.to_vec());
                self.hr.held_meta = self.hr.doc_meta.take();
                bridge::html(PILL, render::FRESH_PILL);
                self.home_fresh_painted();
                return;
            }
        }
        self.learn(&json);
        self.render_page(&json);
    }

    fn learn(&mut self, json: &Value) {
        self.acts.learn(json);
        let ctx = parse::ctx(json);
        if ctx.logged_out.is_some() {
            self.logged_out = ctx.logged_out;
        }
        if let Some(vd) = ctx.visitor {
            if self.cfg.learn_visitor(&vd) {
                self.save_cfg();
                self.kick_player();
            }
        }
    }

    fn fail(&mut self, status: u32, detail: &str) {
        let detail = if status == 0 { "Network error: YouTube could not be reached.".to_string() } else { format!("{detail} (HTTP {status})") };
        let url = self.url.clone();
        bridge::html(MAIN, &render::message("Couldn't load this page", &detail, Some(&url), &native_url(&url)));
        self.feed = FeedState { cont_ep: "browse", ..Default::default() };
        self.rendered = true;
        self.ready();
    }

    // ----------------------------------------------------------- rendering

    fn remember(&mut self, items: &[Item]) {
        if self.known.len() > KNOWN_MAX {
            self.known.clear();
        }
        for it in items {
            if let Item::Video(v) = it {
                if let (Some(h), Some(b)) = (&v.channel_href, &v.channel_bid) {
                    self.channels.entry(h.clone()).or_insert_with(|| b.clone());
                }
                self.known.entry(v.id.clone()).or_insert_with(|| v.clone());
            }
        }
    }

    /// New continuation items (deduplicated) go to the buffer.
    fn take_items(&mut self, mut feed: Feed) {
        self.imp_more(std::mem::take(&mut feed.track));
        self.admit(&mut feed.items);
        let total = feed.items.len();
        let items: Vec<Item> = feed.items.into_iter().filter(|i| self.feed.seen.insert(i.key().to_string())).collect();
        self.stats.merge(&feed.stats);
        self.stats.dupes += (total - items.len()) as u32;
        self.remember(&items);
        // A page without new items ends paging (prevents request loops).
        self.feed.cont = if items.is_empty() { None } else { feed.cont };
        let start = self.feed.buf.len();
        self.feed.buf.extend(items);
        let new = self.feed.buf[start..].to_vec();
        self.fx_more_items(&new);
    }

    fn render_page(&mut self, json: &Value) {
        let route = self.route.clone();
        let (head, title, feed, ep, empty) = match &route {
            Route::Watch { v, list, .. } => {
                let w = Rc::new(parse::next(json));
                self.mark("parsed");
                self.cache_watch(&wkey(v, list.as_deref()), w.clone());
                self.render_watch(&w);
                return;
            }
            Route::Search { q, .. } => {
                self.fx_search_response(json);
                (render::feed_head(&format!("Results for {q}")), q.clone(), parse::search(json), "search", "No results.")
            }
            Route::Channel { path, .. } => {
                let c = parse::channel(json);
                if let Some(b) = &c.bid {
                    self.channels.insert(path.clone(), b.clone());
                }
                (render::channel_head(&c), c.title.clone(), c.feed, "browse", "This channel has no videos.")
            }
            Route::Playlist { list } => {
                let p = parse::playlist(json);
                let head = render::playlist_head(&p, list == "WL", self.wl_note && list == "WL");
                (head.clone(), head.title().to_string(), p.feed, "browse", "This playlist is empty.")
            }
            Route::Home => {
                self.home_from_json(json);
                return;
            }
            Route::Subs => (render::feed_head("Subscriptions"), "Subscriptions".into(), parse::browse(json), "browse", "No videos from your subscriptions."),
            Route::History => (render::feed_head("History"), "History".into(), parse::browse(json), "browse", "Your watch history is empty or turned off."),
            Route::Playlists => (render::feed_head("Playlists"), "Playlists".into(), parse::browse(json), "browse", "No playlists yet."),
            Route::Unknown { .. } => return,
        };
        self.mark("parsed");
        let empty = feed.alert.clone().unwrap_or_else(|| empty.to_string());
        self.show_feed(head, feed, ep, &empty, Vec::new());
        self.set_title(&title);
        self.mark("rendered");
        if self.first_ready {
            bridge::mark("ytcFresh");
        }
        self.after_paint();
    }

    /// Home from YouTube (a response, or the ready response: `extra` its preloaded continuation
    /// items).
    fn show_home(&mut self, f: Feed, extra: Vec<Item>) {
        // Nothing to show (hidden Shorts don't count): the nudge page, as before.
        if self.counted(&f.items) + self.counted(&extra) == 0 {
            let nudge = f.nudge.clone().unwrap_or_else(|| "Search to get started.".into());
            self.mark("parsed");
            bridge::html(MAIN, &render::home_nudge(&nudge, self.auth.signed_in()));
            self.home_fresh_painted();
            self.stats.merge(&f.stats);
            self.feed = FeedState { cont_ep: "browse", ..Default::default() };
            self.set_title("");
            self.rendered = true;
            self.swr = false;
            self.mark("rendered");
            self.ready();
            return;
        }
        if self.nav > 1 {
            self.perf.nav("nav:home-fresh", bridge::now() - self.nav_start);
        }
        self.mark("parsed");
        let empty = f.alert.clone().unwrap_or_else(|| "Nothing here yet.".to_string());
        self.show_feed(render::feed_head("Home"), f, "browse", &empty, extra);
        self.home_fresh_painted();
        self.swr = false;
        self.set_title("");
        self.mark("rendered");
        if self.first_ready {
            bridge::mark("ytcFresh");
        }
        // The next Home is fetched in idle time (before `after_paint` schedules a lazy one).
        self.ready_after_home();
        self.after_paint();
    }

    /// Replaces the page with a fresh feed (first response).
    fn show_feed(&mut self, head: Head, mut feed: Feed, ep: &'static str, empty: &str, mut extra: Vec<Item>) {
        self.admit(&mut feed.items);
        self.admit(&mut extra);
        self.generation += 1;
        self.cont_busy = false;
        self.want = false;
        self.cols = render::grid_cols(bridge::width());
        self.stats.merge(&feed.stats);
        self.remember(&feed.items);
        self.remember(&extra);
        let mut seen: HashSet<String> = feed.items.iter().map(|i| i.key().to_string()).collect();
        extra.retain(|i| seen.insert(i.key().to_string()));
        self.feed = FeedState {
            seen,
            head: Some(head),
            shown: feed.items,
            buf: extra,
            cont: feed.cont,
            cont_ep: ep,
            ..Default::default()
        };
        self.fx_new_feed();
        let head = self.feed.head.as_ref().expect("head set above");
        let html = render::page(head, &self.fx_visible(&self.feed.shown), self.has_more(), empty, ImgPlan::first(Lay::Grid, self.cols));
        let skeleton = !self.rendered && !self.direct;
        bridge::html(MAIN, &html);
        self.fade_thumbs();
        self.fx_entrance(skeleton, true);
    }

    /// Feed thumbnails still loading fade in when they land (fresh renders
    /// only: restored pages and snapshot repaints stay instant).
    pub(crate) fn fade_thumbs(&self) {
        if self.lay() == Lay::Grid {
            bridge::fade_imgs("#ytc-list img");
        }
    }

    fn render_watch(&mut self, w: &Watch) {
        let Route::Watch { v, .. } = self.route.clone() else { return };
        bridge::html(INFO, &render::watch_info(w, &v));
        self.acts.on_watch(&v, w);
        self.tv_watch_data(w, &v);
        self.player_chapters(&v, w);
        let mut s = String::with_capacity(w.related.items.len() * 1000);
        render::related(&mut s, &self.fx_visible(&w.related.items));
        bridge::html(LIST, &s);
        self.generation += 1;
        self.cont_busy = false;
        self.want = false;
        self.stats.merge(&w.related.stats);
        self.remember(&w.related.items);
        self.feed = FeedState {
            seen: w.related.items.iter().map(|i| i.key().to_string()).collect(),
            head: None,
            shown: w.related.items.clone(),
            buf: Vec::new(),
            cont: w.related.cont.clone(),
            cont_ep: "next",
            ..Default::default()
        };
        self.set_title(&w.title);
        self.up_render(w, &v);
        if self.feed.cont.is_some() {
            bridge::attr(MORE, "class", "more");
        }
        self.mark("rendered");
        self.mo_settle();
        self.after_paint();
    }

    fn cached_watch(&mut self, v: &str) -> Option<Rc<Watch>> {
        let i = self.watch_cache.iter().position(|(k, _)| k == v)?;
        let e = self.watch_cache.remove(i);
        let w = e.1.clone();
        self.watch_cache.push(e);
        Some(w)
    }

    fn cache_watch(&mut self, v: &str, w: Rc<Watch>) {
        self.fx_learn_watch(&w);
        self.watch_cache.retain(|(k, _)| k != v);
        self.watch_cache.push((v.to_string(), w));
        if self.watch_cache.len() > WATCH_CACHE {
            self.watch_cache.remove(0);
        }
    }

    fn show_debug(&mut self) {
        let route = format!("{:?} {}", self.route, self.url);
        let mut player = match (&self.player, &self.player_failed) {
            (Some(p), _) => p.debug(),
            (None, Some(f)) => util_esc(&format!("player failed: {} at {:.0}s (panel shown)\n", f.v, f.t)),
            (None, None) => "player -\n".into(),
        };
        player.push_str(&util_esc(&self.acts.debug()));
        player.push_str(&util_esc(&self.fx_debug()));
        player.push_str(&util_esc(&self.voice_debug()));
        if self.vision_block_until > bridge::now() {
            player.push_str(&format!("       VISIONOS skipped for {:.0} s more (bot check on every route)\n", (self.vision_block_until - bridge::now()) / 1000.0));
        }
        if self.vroute() != crate::api::VisionRoute::Anon || self.vision_hold {
            player.push_str(&format!("       VISIONOS route: {}{}\n", self.vroute().label(), if self.vision_hold { " · prefetch held (bot check)" } else { "" }));
        }
        let now = bridge::epoch();
        for e in self.player_log.iter().rev().take(PLAYER_LOG_MAX) {
            let ago = (now - e["at"].as_f64().unwrap_or(now)) / 60000.0;
            player.push_str(&util_esc(&format!("       player failed {ago:.0} min ago · {} at {}s · {}\n", e["v"].as_str().unwrap_or("?"), e["t"].as_u64().unwrap_or(0), e["r"].as_str().unwrap_or(""))));
        }
        let html = render::debug(&render::DebugInfo {
            player: &player,
            route: &route,
            stats: &self.stats,
            marks: &self.marks,
            version: &self.cfg.version,
            visitor: self.cfg.visitor.is_some(),
            cookie_signed_in: self.auth.signed_in(),
            server_logged_out: self.logged_out,
            auth_user: self.cfg.auth_user,
            cache: (self.watch_cache.len(), self.saved.len(), self.page_cache.len()),
            feed: (self.feed.shown.len(), self.feed.buf.len(), self.feed.cont.is_some()),
        });
        bridge::html(render::DEBUG, &html);
        bridge::attr(render::DEBUG, "class", "on");
    }
}

impl App {
    /// Send an incident report to the debug center (background worker → local collector, in
    /// development builds; the public build's worker drops it). No cookies/tokens.
    pub fn incident(&mut self, kind: &str, reason: &str, player: Option<serde_json::Value>) {
        let report = serde_json::json!({
            "kind": kind,
            "reason": reason,
            "at": bridge::epoch(),
            "client": env!("CARGO_PKG_VERSION"),
            "path": bridge::href(),
            "route": format!("{:?}", self.route),
            "client_version": self.cfg.version,
            "env": {"ua": bridge::ua(), "screen": bridge::screen_px(), "lang": bridge::lang()},
            "signed_in_cookie": self.auth.signed_in(),
            "server_logged_out": self.logged_out,
            "marks": self.marks.iter().map(|(k, v)| format!("{k} {v:.1}")).collect::<Vec<_>>(),
            "recent_fallbacks": self.player_log,
            "player": player,
        });
        bridge::incident(&report.to_string());
    }
}

fn util_esc(s: &str) -> String {
    crate::util::esc(s)
}

impl App {
    // -------------------------------------------------------------- player

    fn mount_player(&mut self, v: &str, t: u32, list: Option<String>) {
        self.player_failed = None;
        if self.cfg_ready && !self.cfg.own_player {
            self.show_failed(v, t as f64, list, "own player turned off (op: false)");
            return;
        }
        let start = if t > 0 { t as f64 } else { self.fx_resume(v).or_else(|| self.resume.get(v).copied()).unwrap_or(0.0) };
        let mut p = Own::new(v, start, self.nav_start, self.cfg.prefs.clone(), self.bw);
        p.list = list;
        // Every route met the bot check a moment ago: try the remembered route only (no
        // parallel recovery); blocked again → the panel, whose Try again recovers fully.
        p.no_recover = self.vision_block_until > bridge::now();
        p.avoid = self.codec_avoid.clone();
        p.attach();
        // The first frame decoded while the pointer or focus rested on the link: the box shows it from the
        // open's first frame (the zoom grows the real picture); anything else warm is dropped.
        if let Some(id) = player::own::warm_take(v) {
            p.warm_poster(id);
        }
        self.player = Some(p);
        self.tv_mounted(v);
        self.player_wait = true;
        self.kick_player();
        self.after_player();
    }

    /// Starts the player's requests once config (and visitor data) is known.
    fn kick_player(&mut self) {
        if !self.player_wait || self.player.is_none() {
            return;
        }
        if !self.cfg_ready {
            return;
        }
        if !self.cfg.own_player {
            if let Some(p) = self.player.as_mut() {
                p.fail("own player turned off (op: false)".into());
            }
            self.after_player();
            self.vision_fresh = false;
            return;
        }
        if self.cfg.visitor.is_none() {
            // VISIONOS needs visitor data (else "confirm you're not a bot").
            self.refresh_config(None);
            return;
        }
        self.player_wait = false;
        self.auth = crate::tv::auth();
        let route = self.vroute();
        let ctx = player::Ctx { cfg: &self.cfg, auth: &self.auth, hl: &self.hl, url: &self.url, route: &route };
        if let Some(p) = self.player.as_mut() {
            if p.prefs != self.cfg.prefs {
                p.set_prefs(self.cfg.prefs.clone());
            }
            let now = bridge::now();
            let fresh = std::mem::take(&mut self.vision_fresh);
            let cached = if fresh { None } else { self.vision_cache.iter().find(|(k, at, _)| *k == p.v && now - at < VISION_MAX_AGE_MS).map(|e| e.2.clone()) };
            if !fresh {
                self.perf.hit("visionos", cached.is_some());
            }
            let vision = match cached {
                Some(b) => Vision::Cached(b),
                None if !fresh && self.vision_prefetching.contains(&p.v) => Vision::Pending,
                None => Vision::Fetch,
            };
            let age = self.vision_cache.iter().find(|(k, _, _)| *k == p.v).map(|e| now - e.1);
            p.tel_vision(match vision { Vision::Cached(_) => "cache", Vision::Pending => "prefetch", Vision::Fetch => "fetch" }, age.filter(|_| matches!(vision, Vision::Cached(_))));
            p.start(&ctx, vision);
        }
        self.after_player();
    }

    fn stop_player(&mut self) {
        if let Some(mut p) = self.player.take() {
            let mut dur = 0.0;
            if let Some((t, d)) = p.position() {
                let v = p.v.clone();
                dur = d;
                self.fx_position(&v, t, d, false, true);
            }
            let t = p.stop();
            self.bw = p.bw;
            // Watched to (nearly) the end: the next open starts over, like YouTube and the saved progress.
            if dur > 0.0 && crate::features::progress::near_end(t, dur) {
                self.resume.remove(&p.v);
            } else if t > 5.0 {
                if self.resume.len() > 200 {
                    self.resume.clear();
                }
                self.resume.insert(p.v.clone(), t);
            }
        }
        self.player_wait = false;
        self.player_failed = None;
    }

    fn player_input(&mut self) {
        if let Some(p) = self.player.as_mut() {
            p.last_input = bridge::now();
        }
    }

    fn after_player(&mut self) {
        let mut ended = false;
        let Some(p) = self.player.as_mut() else { return };
        let outs = std::mem::take(&mut p.out);
        if let Some(body) = p.vision_ok.take() {
            let v = p.v.clone();
            self.cache_vision(&v, body);
            self.vision_hold = false;
            // VISIONOS answered OK again: prefetches may resume.
            self.vision_block_until = 0.0;
        }
        for o in outs {
            match o {
                Out::Prefs(prefs) => {
                    // Range gestures (volume, caption size) send many changes: write once they settle.
                    self.cfg.prefs = prefs;
                    if !self.prefs_dirty {
                        self.prefs_dirty = true;
                        let id = self.timer(Timer::SavePrefs);
                        bridge::timer(id, PREFS_SAVE_MS);
                    }
                }
                Out::Theater(on) => self.mo_theater(on),
                Out::Blocked => self.vision_block_until = bridge::now() + VISION_BLOCK_MS,
                Out::Route(r) => {
                    self.vision_route = r;
                    self.vision_hold = false;
                }
                Out::Position { t, dur, ended } => {
                    let v = self.player.as_ref().map(|p| p.v.clone()).unwrap_or_default();
                    self.fx_position(&v, t, dur, ended, false);
                    if !ended {
                        self.ready_watch_check();
                    }
                    if !ended && dur > 0.0 && dur - t <= UP_PREFETCH_S {
                        self.up_prefetch();
                    }
                }
                // Playback finished: the playlist / Up next logic decides what
                // comes next (after the loop, see `up_ended`).
                Out::Ended => {
                    bridge::mark("ytcEnded");
                    ended = true;
                }
                Out::Prefetch(v) => self.prefetch(&v, None),
                // A codec that fails twice in this tab is not used again in it.
                Out::CodecFault(c) => {
                    if self.codec_faults.contains(&c) && !self.codec_avoid.contains(&c) {
                        self.codec_avoid.push(c);
                    }
                    self.codec_faults.push(c);
                }
                Out::Incident { kind, reason } => {
                    let report = self.player.as_ref().map(|p| p.report());
                    self.incident(kind, &reason, report);
                }
                // The player gave up: the panel in its box. Never an automatic navigation.
                Out::Failed { t, reason } => {
                    let Some(p) = self.player.take() else { return };
                    if !player::own::quiet(&reason) {
                        self.incident("player-failed", &reason, Some(p.report()));
                    }
                    self.bw = p.bw;
                    self.show_failed(&p.v, t, p.list.clone(), &reason);
                    // Everything failing at once (HTTP 0 / no answer) is usually the connection or
                    // DNS (live 2 Nov 2026): check; if it's down, wait for it and continue by itself.
                    self.offline = false;
                    if player::fail::network_like(&reason) {
                        self.net_probe();
                    }
                    if self.debug {
                        self.show_debug();
                    }
                    return;
                }
            }
        }
        if self.debug {
            self.show_debug();
        }
        if ended {
            self.up_ended();
        }
    }

    /// A player failure: the panel in the player box (records why for the debug overlay). The
    /// page stays; only the panel's Open on YouTube leaves (since 2 Nov 2026).
    fn show_failed(&mut self, v: &str, t: f64, list: Option<String>, reason: &str) {
        self.player_log.push(serde_json::json!({"at": bridge::epoch(), "v": v, "t": t.max(0.0) as u64, "r": reason.chars().take(300).collect::<String>()}));
        let n = self.player_log.len();
        if n > PLAYER_LOG_MAX {
            self.player_log.drain(..n - PLAYER_LOG_MAX);
        }
        bridge::storage_set(PLAYER_LOG_KEY, &serde_json::Value::Array(self.player_log.clone()).to_string());
        bridge::html(player::ui::BOX, &player::fail::panel(v, reason));
        bridge::attr(player::ui::BOX, "class", "player failed");
        self.player_wait = false;
        self.player_failed = Some(PlayerFailed { v: v.to_string(), t, list });
        // TV: the panel's first action takes the focus (the controls it replaced had it).
        self.tv_failed();
    }

    /// One connectivity probe (same origin, tiny, never cached).
    fn net_probe(&mut self) {
        let id = next_id();
        bridge::fetch_opts(id, "GET", "https://www.youtube.com/generate_204", "", "{}", "{\"cache\":\"no-store\",\"credentials\":\"omit\"}");
        self.pending.insert(id, Pending { purpose: Purpose::NetProbe, sent: bridge::now(), ep: "net-probe", unmasked: None });
    }

    /// A probe answered (or failed): back → continue the failed video by itself; still down →
    /// say so in the panel and probe again in a few seconds.
    fn on_net_probe(&mut self, status: u32) {
        let Some(f) = self.player_failed.as_ref() else {
            self.offline = false;
            return;
        };
        if status == 204 || status == 200 {
            if std::mem::take(&mut self.offline) {
                self.player_panel_act(true);
            }
            return;
        }
        if !self.offline {
            self.offline = true;
            bridge::html(player::ui::BOX, &player::fail::panel(&f.v, player::fail::OFFLINE_REASON));
            self.tv_failed();
        }
        let id = self.timer(Timer::NetProbe);
        bridge::timer(id, NET_PROBE_MS);
    }

    /// The panel's (or the stuck status') buttons. Try again: the same video and time in a new
    /// player, VISIONOS asked afresh (no cache, no prefetch, the bot-check block lifted). Open on
    /// YouTube: YouTube's own watch page at that time (`?yt-native=1`).
    fn player_panel_act(&mut self, retry: bool) -> bool {
        let (v, t, list) = match (&self.player_failed, &self.player) {
            (Some(f), _) => (f.v.clone(), f.t, f.list.clone()),
            (None, Some(p)) => (p.v.clone(), p.resume_at(), p.list.clone()),
            (None, None) => return false,
        };
        if !matches!(&self.route, Route::Watch { v: rv, .. } if *rv == v) {
            return false;
        }
        self.up_cancel();
        self.stop_player();
        self.player_failed = None;
        if !retry {
            bridge::go(&player::native_url(&v, t, list.as_deref()));
            return true;
        }
        self.vision_cache.retain(|(k, _, _)| *k != v);
        self.vision_block_until = 0.0;
        self.vision_fresh = true;
        bridge::html(player::ui::BOX, &player::html(&v));
        bridge::attr(player::ui::BOX, "class", "player");
        let key = wkey(&v, list.as_deref());
        self.mount_player(&v, t.max(0.0).floor() as u32, list);
        if let Some(w) = self.cached_watch(&key) {
            self.player_chapters(&v, &w);
        }
        true
    }

    pub fn on_caps(&mut self, json: &str) {
        if player::own::warm_answer(json) {
            if let Some((v, _)) = self.media_rest.as_ref() {
                let v = v.clone();
                self.media_pre(&v, 0);
            }
            return;
        }
        if let Some(p) = self.player.as_mut() {
            p.on_caps(json);
            self.after_player();
        }
    }

    pub fn on_media(&mut self, h: u32, ev: &str) {
        if player::own::warm_media(h, ev) {
            return;
        }
        if let Some(p) = self.player.as_mut() {
            p.on_media(h, ev);
            self.after_player();
        }
    }

    pub fn on_track(&mut self, name: &str, fx: f64) {
        if let Some(p) = self.player.as_mut() {
            p.on_track(name, fx);
        }
    }

    /// The watch page's chapters go to its player (seek bar, label, keys).
    fn player_chapters(&mut self, v: &str, w: &Watch) {
        if let Some(p) = self.player.as_mut().filter(|p| p.v == v) {
            p.set_chapters(w.acts.chapters.iter().map(|c| (c.start as f64, c.title.clone())).collect(), w.acts.chapters.iter().map(|c| c.thumb.clone()).collect());
        }
    }

    pub fn on_wheel(&mut self, name: &str, dy: f64, notches: i32) {
        if let Some(p) = self.player.as_mut() {
            p.on_wheel(name, dy, notches);
            self.after_player();
        }
    }

    pub fn on_wheel_step(&mut self, name: &str, dir: i32) -> bool {
        let Some(p) = self.player.as_mut() else { return false };
        let handled = p.on_wheel_step(name, dir);
        self.after_player();
        handled
    }

    pub fn on_drag(&mut self, name: &str, fx: f64, fy: f64, phase: u32) {
        if let Some(p) = self.player.as_mut() {
            p.on_drag(name, fx, fy, phase);
            self.after_player();
        }
    }

    /// Hover zones (`data-hov`): the player's menus open/close from these.
    pub fn on_hov(&mut self, zone: &str, arg: &str, held: bool) {
        self.voice_hov(zone, held);
        self.logo_hov(zone, arg, held);
        self.fx.ed_in = zone == "fxed";
        if zone == "wl" {
            if !held {
                self.fx_wl_hover(arg);
            }
            // The player still learns the pointer left it (its bar hides on leave).
            if let Some(p) = self.player.as_mut() {
                p.on_hov(zone, arg, held);
                self.after_player();
            }
            return;
        }
        self.acts.on_hov(&net!(self), zone, held);
        if let Some(p) = self.player.as_mut() {
            p.on_hov(zone, arg, held);
            self.after_player();
        }
    }

    pub fn on_press(&mut self, pop: &str) {
        self.last_ptr = bridge::now();
        // A press is a click on its way, not a dwell.
        self.logo_done();
        self.fx_form_now(false);
        self.voice_press(pop);
        self.acts.on_press(pop);
        if let Some(p) = self.player.as_mut() {
            p.chap_press(pop == "chaps");
        }
    }

    pub fn on_move(&mut self) {
        self.last_input = bridge::now();
        self.logo_moved(self.last_input);
        // Real pointer movement outside the pinned + Topic editor closes it (a re-render that
        // moved it away from a still pointer does not).
        if self.fx.ed_pin && !self.fx.ed_in {
            self.fx.ed_pin = false;
            bridge::attr("ytc-fx-ed", "class", "fx-ed");
        }
        if let Some(p) = self.player.as_mut() {
            p.on_move();
        }
    }

    /// pagehide: stop the player and write the feed snapshot for a reload/back.
    pub fn on_unload(&mut self) {
        if self.prefs_dirty {
            self.prefs_dirty = false;
            self.save_cfg();
        }
        self.voice_leave();
        self.stop_player();
        self.fx_flush();
        self.write_snap();
        self.perf_on_hide();
        self.tel_flush(true);
        self.imp_flush(true);
    }
}
