//! Our own player: VISIONOS streams → index → byte-range segments → MSE,
//! with the WEB `player` response for captions and watch history. All
//! decisions are here (and in the pure submodules); the bridge only performs
//! generic fetch/MediaSource/video calls. A failure stays in the player
//! (`Out::Failed`: the app shows the in-player panel, never YouTube's page); a
//! stall is not a failure: it shows "Still loading — retrying…" and retries in
//! place (`fail::START`, `fail::PLAY`).

use super::captions::{self, Cue};
use super::fail::{self, Step};
use super::formats::{self, Codec, Format, Throughput};
use super::index::{self, Seg};
use super::live;
use super::sched::{self, Next};
use super::throttle;
use super::stats::{self, Recorder, Schedule, Session, Tracking};
use super::storyboard::{self, Storyboard};
use super::ui;
use crate::api::{self, VisionRoute};
use crate::bridge;
use crate::motion;
use crate::config::{Auth, CcStyle, Config, Prefs};
use crate::util::{esc, is_video_id, next_id};
use crate::json::{self, Value};
use std::collections::{HashMap, VecDeque};

mod hedge;
mod pre;
mod sb;
mod stream;
mod tel;
mod warm;

pub use pre::{pre_answer, pre_cancel, pre_progress, prefetch_media, prefetched};
pub use warm::{warm_keep, warm_media, warm_take, warm_timer, warm_try};

/// What the app needs from a request: session values and the page.
pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub auth: &'a Auth,
    pub hl: &'a str,
    pub url: &'a str,
    /// How to send VISIONOS (the session's route after a bot check).
    pub route: &'a VisionRoute,
}

/// Where the VISIONOS response comes from (the app caches and prefetches it:
/// its URLs stay valid for hours).
pub enum Vision {
    /// Not available: request it now.
    Fetch,
    /// A cached OK response body.
    Cached(std::rc::Rc<Vec<u8>>),
    /// A hover prefetch is in flight; the app passes it on with `feed_vision`.
    Pending,
}

/// Effects for the app.
pub enum Out {
    /// Gave up at `t`: the app shows the failure panel in the player box (Try again / Open on
    /// YouTube). Never an automatic navigation (since 2 Nov 2026).
    Failed { t: f64, reason: String },
    /// A debug-center incident with this player's report: "player-stuck" (an episode began) and
    /// "player-recovered" (it played after all; a final failure is the `Failed` incident).
    Incident { kind: &'static str, reason: String },
    /// Preferences changed (persist them).
    Prefs(Prefs),
    /// Theater mode toggled.
    Theater(bool),
    /// VISIONOS answered with the bot check on every route tried: skip it for a while.
    Blocked,
    /// A bot check was recovered on this route: use it for later videos and prefetches.
    Route(VisionRoute),
    /// Playback position for resume/progress (on pause, at the end and every
    /// `POSITION_EVERY_MS` of playback). `dur` is the video's length.
    Position { t: f64, dur: f64, ended: bool },
    /// Playback reached the end (VOD, or a live stream whose playlist ended).
    /// The app decides what comes next (playlist / Up next).
    Ended,
    /// `hint_next`: prefetch this video's watch data and VISIONOS streams.
    Prefetch(String),
    /// A media error was put down to this video codec, and the player went to another codec family
    /// (the app avoids a codec that does this twice for the rest of the tab session).
    CodecFault(Codec),
}

/// How often a playing video reports its position (see `Out::Position`).
pub const POSITION_EVERY_MS: f64 = 10_000.0;

/// Whether a playing video should report its position again.
pub fn position_due(last: f64, now: f64) -> bool {
    now - last >= POSITION_EVERY_MS
}

const VIDEO_EVENTS: &str = "loadedmetadata,loadeddata,play,playing,pause,waiting,canplay,seeking,timeupdate,ended,error,ratechange,volumechange";
const TICK_MS: u32 = 250;
/// No googlevideo answer at all this long after the streams were chosen → fresh URLs once.
/// (Longer waits: the stuck episode, `fail::START` / `fail::PLAY`.)
const NO_ANSWER_MS: f64 = 2500.0;
/// The same, when the streams came from a cached player response.
const NO_ANSWER_CACHED_MS: f64 = 800.0;
const VISION_TIMEOUT_MS: u32 = 8000;
/// A start this close to the end of the media (or past it) plays from the beginning.
const START_END_SLACK: f64 = 0.5;
/// A recovered media error counts as recovered once the picture stayed this long without another.
const MEDIA_SETTLE_MS: f64 = 1500.0;
/// A SourceBuffer error waits at most this long for the element's error and its message.
const SB_ERR_WAIT_MS: u32 = 100;
/// A wake asks again (with fresh URLs) the requests that got no answer in this long.
const STALE_MS: f64 = 3000.0;

/// `streamingData.expiresInSeconds` of a player response.
fn expires_in(json: &Value) -> Option<f64> {
    json.pointer("/streamingData/expiresInSeconds").and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_f64()))
}
const IDLE_MS: f64 = 2500.0;
/// TV: the overlay stays this long after the last key while playing (a remote is slower than a mouse).
const TV_IDLE_MS: f64 = 4000.0;

fn idle_ms() -> f64 {
    if crate::tv::on() { TV_IDLE_MS } else { IDLE_MS }
}
const MAX_RETRIES: u32 = 3;
/// The first video of a page waits this long at most for decodingInfo (seen: 4–55 ms).
const CAPS_WAIT_MS: u32 = 150;
const V: usize = 0;
const A: usize = 1;

enum Req {
    Vision(VisionRoute),
    /// A fresh anonymous visitor for the bot-check recovery.
    Visitor,
    Web,
    Head { itag: u32, kind: usize },
    Seg { kind: usize, idx: usize, itag: u32 },
    Captions,
    Ping,
    /// Live: the HLS master playlist, the followed media playlist, a segment.
    Master,
    Playlist,
    LiveSeg { kind: usize, sq: u64, itag: u32 },
    /// SponsorBlock's k-anonymous segment lookup (`sb.rs`).
    Sponsor,
}

/// Failure reason for upcoming streams/premieres (followed by YouTube's own words, e.g.
/// "Premieres in 2 hours"); not reported as an incident.
pub const UPCOMING_REASON: &str = "upcoming live stream or premiere";

/// YouTube's bot check ("Sign in to confirm you're not a bot").
pub fn bot_check(status: &str, reason: &str) -> bool {
    status == "LOGIN_REQUIRED" && reason.contains("bot")
}

/// A stuck episode: no first frame (`start`) or buffering mid-playback, past `fail::START.report`
/// / `fail::PLAY.report`. The status shows; fresh URLs at the plan's retries; the panel at its end.
struct Stuck {
    start: bool,
    /// The plan's base (ms): the streams chosen, or the stall's start (moved on by a resume).
    base: f64,
    /// When the episode was reported (ms).
    at: f64,
    retries: usize,
    why: String,
}

/// Bot-check recovery in progress: the other routes sent in parallel.
#[derive(Default)]
struct Recover {
    /// Attempts not answered yet.
    pending: u32,
    /// One entry per route tried, with its outcome (for the fallback reason).
    steps: Vec<String>,
    /// Every attempt so far was the bot check (or a failed request).
    all_bot: bool,
    reason: String,
}

/// What a new player of the same video takes over from the last one.
#[derive(Clone, Default)]
pub struct Carry {
    /// It was paused: the next one starts paused.
    pub paused: bool,
    /// The audio track chosen in the menu.
    pub audio_sel: String,
    /// The playback speed (0: not set).
    pub rate: f64,
    /// An automatic retry goes on with the viewing session: history is reported once.
    pub session: Option<Viewing>,
}

/// The viewing session YouTube's history knows (`Carry::session`).
#[derive(Clone)]
pub struct Viewing {
    cpn: String,
    rt0: f64,
    sent: f64,
    schedule: Option<Schedule>,
    tracking: Option<Tracking>,
    /// The failed player's last watchtime ping, not sent yet: (going on, final). The next player
    /// sends the first at its first frame; a session that ends before that, the second.
    last: Option<(String, String)>,
    /// The pings' headers (`ping_headers`).
    headers: String,
}

impl Viewing {
    /// The session ends without another picture: its final ping, with the position it had.
    pub fn end(mut self) {
        if let Some((_, last)) = self.last.take() {
            let headers = if self.headers.len() > 2 { self.headers.as_str() } else { "{}" };
            bridge::fetch_opts(next_id(), "GET", &last, "", headers, "{\"keepalive\":true}");
        }
    }
}

/// Failure reasons that are expected, not bugs (no incident; the panel still shows).
pub fn quiet(reason: &str) -> bool {
    // Expected, not bugs: upcoming streams/premieres, videos our anonymous VISIONOS
    // profile may not play (age-restricted, members-only), the player switched off.
    reason.starts_with(UPCOMING_REASON)
        || reason.starts_with("own player turned off")
        || (reason.starts_with("VISIONOS LOGIN_REQUIRED") && !reason.contains("bot"))
        || reason.contains("members")
}

/// Digit keys: swallowed on watch pages without any effect.
pub fn blocked_key(key: &str) -> bool {
    key.len() == 1 && key.as_bytes()[0].is_ascii_digit()
}

#[cfg(test)]
mod quiet_tests {
    #[test]
    fn expected_handoffs_are_quiet() {
        assert!(super::quiet("VISIONOS LOGIN_REQUIRED: Sign in to confirm your age"));
        assert!(!super::quiet("VISIONOS LOGIN_REQUIRED: Sign in to confirm you're not a bot"));
        assert!(!super::quiet("no first frame 4 s after the streams were chosen"));
        assert!(super::quiet(&format!("{}: Premieres in 2 hours", super::UPCOMING_REASON)));
        assert!(super::quiet("own player turned off (op: false)"));
        assert!(!super::quiet("VISIONOS LOGIN_REQUIRED: Sign in to confirm you’re not a bot · tried: anonymous (bot check), fresh visitor (bot check)"));
    }
    #[test]
    fn bot_check_is_recognised() {
        // YouTube's text uses a typographic apostrophe (2 Nov 2026).
        assert!(super::bot_check("LOGIN_REQUIRED", "Sign in to confirm you’re not a bot"));
        assert!(!super::bot_check("LOGIN_REQUIRED", "Sign in to confirm your age"));
        assert!(!super::bot_check("UNPLAYABLE", "not a bot"));
    }
}

/// Live playback state (see `live.rs`): HLS playlists for the timeline,
/// fMP4 segments by sequence number from the adaptive URLs.
#[derive(Default)]
struct LiveSt {
    manifest: String,
    /// The media playlist we follow ('' = fetch the master first).
    media_url: String,
    win: live::Window,
    /// Next sequence number per track.
    next: [u64; 2],
    /// The first playlist was handled (tracks are ready).
    begun: bool,
    /// A playlist/master request is in flight.
    polling: bool,
    /// Following the live edge (false after a seek back into the window).
    edge: bool,
    /// Consecutive playlist failures.
    fails: u32,
    /// Seconds into the window to start at (ended playlists with `t=`).
    from: f64,
    /// (ingest wall clock ms, media time) of a recent video segment.
    wall: Option<(f64, f64)>,
    /// Media time to start at, until the element has its metadata and the
    /// position is applied (before that `currentTime` reads 0).
    start_at: Option<f64>,
    latency_noted: bool,
}

struct Inflight {
    req: Req,
    sent: f64,
    /// Short request description for the incident log (host and range, no tokens).
    what: String,
    /// googlevideo VOD requests (streamed, `on_fetch_progress`): the URL before any mirror, the
    /// mirror asked, when the headers came (ms after `sent`), body bytes so far and expected.
    base: String,
    mirror: usize,
    ttfb: Option<f64>,
    got: f64,
    want: f64,
    /// The same range asked on another mirror (both run; the first full answer wins).
    twin: Option<u32>,
    /// This request is the hedge (the second of a pair).
    hedge: bool,
    /// Already judged throttled (one verdict per request).
    flagged: bool,
    /// The body arrives in pieces (`on_fetch_chunk`, `stream.rs`): the file offset of its next
    /// byte, and (index requests) the bytes so far until the index is parsed.
    chunked: bool,
    at: u64,
    buf: Vec<u8>,
    /// (Index requests) the index was parsed from the first pieces; later pieces went on as media.
    early: bool,
    /// The exact masked VISIONOS request, for one immediate unmasked retry on HTTP 400.
    unmasked: Option<Box<(String, String, &'static str)>>,
}

impl Inflight {
    fn new(req: Req, what: String) -> Inflight {
        Inflight { req, sent: bridge::now(), what, base: String::new(), mirror: 0, ttfb: None, got: 0.0, want: 0.0, twin: None, hedge: false, flagged: false, chunked: false, at: 0, buf: Vec::new(), early: false, unmasked: None }
    }
}

enum Tm {
    Tick,
    VisionTimeout,
    Retry(usize),
    /// The VISIONOS request has not answered in `throttle::VISION_HEDGE_MS`: ask again.
    VisionHedge,
    /// decodingInfo did not answer in `CAPS_WAIT_MS`: choose the streams without it.
    CapsWait,
    /// Re-request a format's index after a transient (5xx/network) failure.
    Head(u32, usize),
    /// Hover-menu intent/grace (`ui::MenuCtl` generation).
    Menu(u32),
    /// The pointer rested on a Speed/Quality row for `ui::DWELL_MS` (dwell generation).
    Dwell(u32),
    /// The wheel rested on a Quality value for `ui::QUALITY_REST_MS` (wheel generation).
    WheelQ(u32),
    /// Live: refresh the media playlist.
    LivePoll,
    /// SponsorBlock (`sb.rs`).
    Sb(sb::SbTm),
    /// The seek bar's pointer rested (`stream.rs`, generation).
    SeekPre(u32),
    /// The first frame is on screen: the warm first frame's canvas (`warm.rs`) goes.
    Poster,
    /// A SourceBuffer error waited this long for the element's error and its message (`sb_err`).
    SbErr,
    /// A background URL refresh (`proactive_refresh`, generation) got no answer.
    Proactive(u32),
}

struct Rep {
    f: Format,
    segs: Vec<Seg>,
    init: Vec<u8>,
}

enum Op {
    Append(Vec<u8>),
    /// A piece of a media segment appended while it downloads (`stream.rs`): whether it is the
    /// segment's first and last piece.
    Chunk { bytes: Vec<u8>, first: bool, last: bool },
    Remove(f64, f64),
    ChangeType(String),
}

#[derive(Default)]
struct Track {
    sb: u32,
    mime: String,
    /// Representation (itag) segments are fetched from.
    rep: u32,
    /// Whether `next` is valid (index of `rep` loaded).
    ready: bool,
    next: usize,
    inflight: Option<u32>,
    ops: VecDeque<Op>,
    updating: bool,
    /// itag whose init segment was queued last (0 = none since a reset).
    appended: u32,
    retries: u32,
    retry_wait: bool,
    quota: bool,
    /// Switch target (itag) and whether it is immediate (user choice).
    pending: Option<(u32, bool)>,
    /// The segment being downloaded and appended piece by piece (`stream.rs`).
    asm: Option<stream::SegAsm>,
    /// The SourceBuffer holds part of a media segment (its parser is mid-segment): the next new
    /// segment or init segment first drops it (`abort()`).
    mid: bool,
}

impl Track {
    fn drop_appends(&mut self) {
        let before = self.ops.len();
        self.ops.retain(|o| !matches!(o, Op::Append(_) | Op::Chunk { .. }));
        if self.ops.len() != before {
            self.appended = 0;
        }
    }

    /// Seek or immediate switch: queued appends and the segment in progress are dropped.
    fn abandon(&mut self) {
        self.drop_appends();
        if self.asm.take().is_some() {
            self.appended = 0;
        }
    }
}

pub struct Own {
    pub v: String,
    /// Playlist id, passed on to the native page on fallback.
    pub list: Option<String>,
    start: f64,
    created: f64,
    nav_start: f64,
    pub prefs: Prefs,
    pub bw: Throughput,
    pub out: Vec<Out>,
    fetches: HashMap<u32, Inflight>,
    timers: HashMap<u32, Tm>,
    vid: u32,
    ms: u32,
    ms_open: bool,
    sbs_made: bool,
    eos: bool,
    // streams
    started: bool,
    vision_ms: Option<f64>,
    refreshes: u32,
    refreshing: bool,
    /// Any googlevideo answer (index or chunk) has arrived for this video.
    got_media: bool,
    /// googlevideo mirror in use (0 = the URL's own host; n = the n-th `mn` mirror, as YouTube's player falls back).
    mirror: usize,
    /// The streams came from the app's cached/prefetched player response (its URLs can go silent).
    from_cache: bool,
    /// When the streams were chosen (moved on while the tab is hidden; not by a refresh):
    /// the base of a start stall.
    load_at: f64,
    /// A stuck episode (status shown, retrying in place).
    stuck: Option<Stuck>,
    /// The MediaSource was rebuilt once in this player's life.
    mse_rebuilt: bool,
    /// Automatic recoveries after media errors (`media_error`): how many in this player's life,
    /// the recent ones (when, what: the sliding window), and the episode's first error with when
    /// it happened, the count before it and when the picture came back (0: not yet; reported once
    /// it stayed `MEDIA_SETTLE_MS` without another error).
    media_retries: u32,
    recov: Vec<(f64, fail::MediaRetry)>,
    media_err: Option<(String, f64, u32, f64)>,
    /// A media error while hidden and paused, recovered when the tab is shown or played:
    /// (reason, track, the browser's message).
    deferred: Option<(String, Option<usize>, String)>,
    /// A SourceBuffer error waiting (`SB_ERR_WAIT_MS`) for the element's error that follows it: the
    /// browser's words ("Video stream codec … doesn't match") come only with that one.
    sb_err: Option<(String, usize)>,
    /// The last position played (the element can read 0 once its MediaSource is gone).
    play_t: f64,
    /// The playhead and since when it has not moved while playing (the decoder watchdog).
    still: (f64, f64),
    /// When the URLs' expiry was last checked on the tick (a long video outlives its URLs), when
    /// the URLs in use came (wall clock, `bridge::epoch`; 0 = not known yet) and their lifetime (s,
    /// `fail::url_left`), a proactive refresh in flight (its timeout's generation), and no more
    /// proactive refreshes for this player (one failed, timed out, was bot-checked or brought
    /// URLs as short-lived: never a request a minute on healthy playback).
    exp_at: f64,
    urls_epoch: f64,
    urls_life: f64,
    proactive: bool,
    proactive_gen: u32,
    exp_off: bool,
    /// `ytc-test-nothrottle` (tests only), read once.
    no_throttle: bool,
    /// Video codecs not to use: the app's tab-session verdicts, and a codec fault in this player.
    pub avoid: Vec<Codec>,
    /// The player response being refreshed is a media-error retry's: a failed answer keeps the URLs
    /// (like a stuck retry's) so the next retry can try another format.
    media_refresh: bool,
    /// Every route was bot-checked shortly before (the app's block): only this player's route is
    /// tried, no parallel recovery; the panel's Try again does the full recovery.
    pub no_recover: bool,
    vision_cfg: Option<Config>,
    /// The route VISIONOS goes by (the app's session route; changed by a recovery).
    route: VisionRoute,
    /// The account's session headers source (for the account route).
    auth: Auth,
    /// Headers for history pings ("{}" or the TV bearer, `tv::ping_headers`).
    ping_headers: String,
    recover: Option<Recover>,
    /// A fresh OK VISIONOS body for the app's cache (taken by the app).
    pub vision_ok: Option<std::rc::Rc<Vec<u8>>>,
    retry_heads: Vec<(u32, usize)>,
    heads: Vec<u32>,
    formats: Vec<Format>,
    ladder: Vec<Format>,
    /// Video itags googlevideo would not serve (e.g. a 302 to plain http that the
    /// browser blocks as mixed content, seen on 1440p/2160p): never chosen again.
    bad: Vec<u32>,
    /// Transient index failures per itag (retried before giving the format up).
    head_tries: HashMap<u32, u32>,
    /// `MediaCapabilities.decodingInfo` per video itag: (smooth, power-efficient).
    caps: HashMap<u32, (bool, bool)>,
    caps_asked: bool,
    /// Highest rung (p) the dropped-frame watchdog allows on this device (0 = none).
    drop_cap: u32,
    /// Last `getVideoPlaybackQuality` sample (dropped, total) and the totals.
    dq_last: (f64, f64),
    dq_window: Vec<(f64, f64)>,
    /// An instant quality switch in progress (`stream.rs`), and whether the next immediate
    /// video switch is the user's choice (instant).
    flush: Option<stream::Flush>,
    user_switch: bool,
    /// When the start's media covered the playhead with no picture yet (`stream.rs`), and whether
    /// the format was already switched for it.
    nopic_since: f64,
    nopic_done: bool,
    /// Typical googlevideo time to the first byte of a segment (ms, smoothed).
    ttfb_ms: f64,
    /// Segments fetched ahead of a likely seek (`stream.rs`), the spot and its timer generation.
    seek_pre: Vec<stream::SeekPre>,
    seekpre_at: f64,
    seekpre_gen: u32,
    seekpre_deadline: Option<f64>,
    seekpre_pending: bool,
    /// Last periodic ABR check (ms), so quality also rises while paused.
    abr_at: f64,
    /// Stalls during playback: (ms since navigation, duration ms).
    stalls: Vec<(f64, f64)>,
    stall_stepped: bool,
    audio: Option<Format>,
    reps: HashMap<u32, Rep>,
    tracks: [Track; 2],
    duration: f64,
    streams_at: f64,
    last_switch: f64,
    // web
    web_done: bool,
    web_note: String,
    tracking: Option<Tracking>,
    /// Seek-preview frames (WEB `storyboards`; VOD only).
    story: Option<Storyboard>,
    cc_web: captions::List,
    cc_vision: captions::List,
    cc_sel: Option<usize>,
    cc_loaded: Option<usize>,
    cues: Vec<Cue>,
    // stats
    cpn: String,
    session: Session,
    rec: Recorder,
    schedule: Option<Schedule>,
    playback_sent: f64,
    pings: u32,
    pub last_input: f64,
    /// When the last `Out::Position` was reported (ms, `bridge::now`).
    pos_at: f64,
    // ui
    paused: bool,
    blocked_autoplay: bool,
    /// Element/buffer state captured just before a fallback released it.
    el_snap: Option<serde_json::Value>,
    /// The start timeout was held while the tab was hidden.
    hidden_wait: bool,
    waiting: bool,
    waiting_since: f64,
    first_frame: Option<f64>,
    /// The playback speed chosen (a new MediaSource resets the element's).
    rate: f64,
    /// When this viewing session began (`ping_now`; an automatic retry keeps it).
    rt0: f64,
    /// The watchtime ping of a failure, not sent yet (`Viewing::last`).
    last_pings: Option<(String, String)>,
    /// Starts without playing (an automatic Try again of a paused video). Before the first frame
    /// `paused` is true anyway; this says it is meant.
    pub start_paused: bool,
    /// The picture held when this player failed (`failed_state`).
    fail_frame: Option<String>,
    mc: ui::MenuCtl,
    /// CC popup shows the Audio tab.
    cc_audio: bool,
    /// Chosen audio track id ('' = the default track).
    audio_sel: String,
    /// Caption drag: grab offset from the caption's corner (player fractions).
    cc_grab: Option<(f64, f64)>,
    drag: Option<f64>,
    idle_at: f64,
    /// The next `ui()` lands the progress fill exactly (a seek: no glide).
    snap: bool,
    /// The pointer left the player (its zones): controls hide while playing and
    /// pointer moves elsewhere on the page do not wake them (keys still do).
    away: bool,
    /// Caption text on screen, and the flip that restarts its fade for a new cue.
    cc_text: String,
    cue_flip: bool,
    toast_until: f64,
    fs: bool,
    classes: String,
    pub failed: bool,
    pub notes: Vec<String>,
    /// Every request of this player and its outcome (for incident reports).
    reqlog: Vec<String>,
    live: Option<LiveSt>,
    /// The next video the app hinted at (prefetched once).
    next_hint: Option<String>,
    /// A seek we made ourselves (ignored by `on_seeking`): position, when.
    own_seek: Option<(f64, f64)>,
    ch: ChapSt,
    /// The last state change came from the keyboard until this time (ms): `.kf` turns the icon morphs,
    /// the center pulse and the list marks' slides off (keyboard actions are instant).
    kf_until: f64,
    /// What the menu panel shows ("speed", "quality", "cc", "audio"); None while closed. A change while
    /// it shows morphs the panel from one menu to the next.
    menu_kind: Option<&'static str>,
    /// The toast on screen came from a pointer (it popped in; keyboard toasts appear on the key's frame).
    toast_ptr: bool,
    /// Wheel-to-choose over the Speed/Quality shortcuts and menus: gesture state, and the Quality value
    /// the wheel moved to (shown at once, applied once the wheel rests) with its timer generation.
    wheel: ui::Wheel,
    wheel_q: Option<u32>,
    wheel_gen: u32,
    /// When rows last came under a still pointer by themselves (a wheel event scrolled or nudged the list, a
    /// key opened the panel): no dwell starts for `DWELL_HOLD_MS` after.
    hold_at: f64,
    /// Dwell-to-choose: the row (`data-arg`) the pointer rests on, and its timer generation.
    dwell: Option<String>,
    dwell_gen: u32,
    /// SponsorBlock segments, skips and the skip toast (`sb.rs`).
    sb: sb::SbSt,
    /// The hedged VISIONOS request (`throttle::VISION_HEDGE_MS`) and whether one went out.
    vision_hedge: Option<u32>,
    vision_hedged: bool,
    /// The app's VISIONOS prefetch for this video is still on its way (`Vision::Pending`).
    vision_app: bool,
    /// The streams wait (≤ `CAPS_WAIT_MS`) for decodingInfo, so the first requests go to the
    /// format that will play; decodingInfo request ids → cache keys.
    caps_wait: bool,
    caps_keys: HashMap<u32, String>,
    /// Throttle episodes (`hedge.rs`).
    thr: hedge::Thr,
    /// The player box's height in device pixels (0 = not read yet) and when it was read: read on
    /// the tick, at most once a second, never in the click task.
    box_h: f64,
    box_at: f64,
    /// Hover-prefetched first requests (`pre.rs`): taken only while the streams are chosen,
    /// answered right after.
    pre_ok: bool,
    pre_ready: Vec<(u32, usize, u32, Vec<u8>)>,
    /// The warm first frame's canvas over the `<video>` until the player's own first frame shows (`warm.rs`).
    poster: Option<String>,
    /// Speed telemetry (`tel.rs`).
    pt: tel::PTel,
}

/// Chapters on the seek bar, the control bar label and the list's highlight.
#[derive(Default)]
struct ChapSt {
    /// (start s, title), ascending.
    list: Vec<(f64, String)>,
    starts: Vec<f64>,
    /// Duration the segments were drawn for (0 = the single bar).
    drawn: f64,
    /// Segments on the bar (chapters past the end are not drawn).
    segs: usize,
    /// Chapter shown in the label / marked in the list.
    cur: Option<usize>,
    /// Pointer over the seek bar (fraction), the segment grown under it and
    /// the chapter whose title the tooltip shows.
    hover: Option<f64>,
    grown: Option<usize>,
    tip: Option<usize>,
    /// The label was clicked in fullscreen: reveal the list once it is left.
    reveal: bool,
    /// YouTube's chapter thumbnails (shown only when there is no storyboard).
    thumbs: Vec<Option<String>>,
    /// The list's frames were written (storyboard cells or the fallback thumbnails).
    painted: bool,
    /// The seek tooltip's frame: (level, frame number) written last.
    tipf: Option<(usize, u32)>,
    /// The chapter panel under the description (compact → expanded): the pointer
    /// moving over it, keyboard focus inside, pinned by a click, closed by
    /// Escape/a click until the pointer and focus leave; and the class written.
    hov: bool,
    foc: bool,
    pin: bool,
    shut: bool,
    open: bool,
}

/// How the chapter panel's change looks (client.css `.ych`).
#[derive(Clone, Copy, PartialEq)]
enum PanelHow {
    /// Pointer hover: opens on the spring after a short intent delay (`.hv`).
    Hover,
    /// A click: on the spring at once.
    Ptr,
    /// Keyboard: no motion (`.now`).
    Key,
}

/// Frame boxes: list rows / compact row and the seek tooltip (CSS px wide, client.css `.sbf`, `.ptf`).
const ROW_FRAME_W: f64 = 96.0;
const TIP_FRAME_W: f64 = 160.0;

/// The volume slider's value and its accent fill (`--v`, client.css `.pvol`).
fn vol_ui(v: f64) {
    bridge::value(ui::VOL, &format!("{v}"));
    bridge::css_var(ui::VOL, "--v", &format!("{:.3}", v.clamp(0.0, 1.0)));
}

/// A `player` response's caption tracks; on TV with the Auto-translate languages.
fn cc_list_of(json: &Value) -> captions::List {
    if crate::tv::on() { captions::list_tv(json, &bridge::lang()) } else { captions::list(json) }
}

/// Menu panel content order along the toolbar (CC · Speed · Quality): the new menu slides in from its side.
fn menu_order(kind: &str) -> f64 {
    match kind {
        "sb" => -1.0,
        "cc" => 0.0,
        "audio" => 0.5,
        "speed" => 1.0,
        _ => 2.0,
    }
}

/// Menu morph: the panel's size follows the house spring; new content fades in over this.
const MENU_FADE_MS: u32 = 160;
/// How far new menu content travels in (px, × reduced motion).
const MENU_DX: f64 = 10.0;
/// The rubber-band nudge at the end of a wheel list: px toward the wheel and back, over this long.
const EDGE_NUDGE_PX: f64 = 4.0;
const EDGE_NUDGE_MS: u32 = 320;
/// No dwell starts this soon after a wheel event (rows moving under a still pointer are not a choice).
const DWELL_HOLD_MS: f64 = 250.0;
/// After a key, state changes stay instant this long (the media event that flips the class comes later).
const KF_MS: f64 = 450.0;
/// The chapter label's new title rises in this fast; the tooltip's new title fades in this fast.
const LABEL_MS: u32 = 220;
const TIP_FADE_MS: u32 = 140;

/// Movement scale: 0 under reduced motion (fades stay), else 1 (client.css `--mv`).
fn mv() -> f64 {
    if bridge::reduced_motion() { 0.0 } else { 1.0 }
}

fn pairs(v: Vec<f64>) -> Vec<(f64, f64)> {
    v.chunks_exact(2).map(|c| (c[0], c[1])).collect()
}

impl Own {
    pub fn new(v: &str, start: f64, nav_start: f64, prefs: Prefs, bw: Throughput) -> Own {
        let now = bridge::now();
        let cpn = stats::cpn(&format!("{}:{now}:{v}", bridge::epoch()));
        Own {
            v: v.to_string(),
            list: None,
            start,
            created: now,
            nav_start,
            prefs,
            bw,
            out: Vec::new(),
            fetches: HashMap::new(),
            timers: HashMap::new(),
            vid: 0,
            ms: 0,
            ms_open: false,
            sbs_made: false,
            eos: false,
            started: false,
            vision_ms: None,
            refreshes: 0,
            refreshing: false,
            got_media: false,
            mirror: 0,
            from_cache: false,
            load_at: 0.0,
            stuck: None,
            mse_rebuilt: false,
            media_retries: 0,
            recov: Vec::new(),
            media_err: None,
            deferred: None,
            sb_err: None,
            play_t: 0.0,
            still: (0.0, 0.0),
            exp_at: bridge::now(),
            urls_epoch: 0.0,
            urls_life: 0.0,
            proactive: false,
            proactive_gen: 0,
            exp_off: false,
            no_throttle: bridge::local_flag("ytc-test-nothrottle"),
            avoid: Vec::new(),
            media_refresh: false,
            no_recover: false,
            vision_cfg: None,
            route: VisionRoute::Anon,
            auth: Auth::default(),
            ping_headers: "{}".into(),
            recover: None,
            vision_ok: None,
            retry_heads: Vec::new(),
            heads: Vec::new(),
            formats: Vec::new(),
            ladder: Vec::new(),
            bad: Vec::new(),
            head_tries: HashMap::new(),
            caps: HashMap::new(),
            caps_asked: false,
            drop_cap: 0,
            dq_last: (0.0, 0.0),
            dq_window: Vec::new(),
            stalls: Vec::new(),
            flush: None,
            user_switch: false,
            nopic_since: 0.0,
            nopic_done: false,
            ttfb_ms: 0.0,
            seek_pre: Vec::new(),
            seekpre_at: 0.0,
            seekpre_gen: 0,
            seekpre_deadline: None,
            seekpre_pending: false,
            abr_at: 0.0,
            stall_stepped: false,
            audio: None,
            reps: HashMap::new(),
            tracks: [Track::default(), Track::default()],
            duration: 0.0,
            streams_at: 0.0,
            last_switch: f64::NEG_INFINITY,
            web_done: false,
            story: None,
            web_note: String::new(),
            tracking: None,
            cc_web: captions::List::default(),
            cc_vision: captions::List::default(),
            cc_sel: None,
            cc_loaded: None,
            cues: Vec::new(),
            cpn,
            session: Session::default(),
            rec: Recorder::default(),
            schedule: None,
            playback_sent: 0.0,
            pings: 0,
            last_input: now,
            pos_at: now,
            paused: true,
            blocked_autoplay: false,
            el_snap: None,
            hidden_wait: false,
            waiting: true,
            waiting_since: now,
            first_frame: None,
            rate: 1.0,
            rt0: now,
            last_pings: None,
            start_paused: false,
            fail_frame: None,
            mc: ui::MenuCtl::default(),
            cc_audio: false,
            audio_sel: String::new(),
            cc_grab: None,
            drag: None,
            idle_at: now + idle_ms(),
            snap: false,
            away: false,
            cc_text: String::new(),
            cue_flip: false,
            toast_until: 0.0,
            fs: bridge::is_fullscreen(),
            classes: String::new(),
            failed: false,
            notes: Vec::new(),
            reqlog: Vec::new(),
            live: None,
            next_hint: None,
            own_seek: None,
            ch: ChapSt::default(),
            kf_until: 0.0,
            menu_kind: None,
            toast_ptr: false,
            wheel: ui::Wheel::default(),
            wheel_q: None,
            wheel_gen: 0,
            hold_at: f64::NEG_INFINITY,
            dwell: None,
            dwell_gen: 0,
            sb: sb::SbSt::default(),
            vision_hedge: None,
            vision_hedged: false,
            vision_app: false,
            caps_wait: false,
            caps_keys: HashMap::new(),
            thr: hedge::Thr::default(),
            box_h: 0.0,
            box_at: f64::NEG_INFINITY,
            pre_ok: false,
            pre_ready: Vec::new(),
            poster: None,
            pt: tel::PTel::default(),
        }
        .with_tel()
    }

    fn with_tel(mut self) -> Own {
        self.tel_init();
        self
    }

    /// Whether this is a live stream played by sequence number.
    pub fn is_live(&self) -> bool {
        self.live.is_some()
    }

    /// The app knows what plays next (playlist / Up next): prefetch its watch
    /// data and VISIONOS streams now, so starting it needs only googlevideo.
    pub fn hint_next(&mut self, id: &str) {
        if is_video_id(id) && id != self.v && self.next_hint.as_deref() != Some(id) {
            self.next_hint = Some(id.to_string());
            self.out.push(Out::Prefetch(id.to_string()));
        }
    }

    fn note(&mut self, s: String) {
        if self.notes.len() < 40 {
            self.notes.push(format!("{:>7.0} ms  {s}", bridge::now() - self.nav_start));
        }
    }

    fn timer(&mut self, t: Tm, ms: u32) {
        let id = next_id();
        self.timers.insert(id, t);
        bridge::timer(id, ms);
    }

    fn get(&mut self, req: Req, url: &str, opts: &str) -> u32 {
        self.get_on(req, url, opts, self.mirror)
    }

    /// `get` on the `mirror`-th googlevideo mirror. VOD index and segment requests stream
    /// (`progress`), so a slow host shows within a fraction of a second (`hedge.rs`).
    fn get_on(&mut self, req: Req, url: &str, opts: &str, mirror: usize) -> u32 {
        let id = next_id();
        let media = matches!(req, Req::Head { .. } | Req::Seg { .. });
        // A stuck googlevideo node: media requests go to the next mirror the URL itself lists.
        let moved = if mirror > 0 && media { mirror_url(url, mirror) } else { None };
        let base = url;
        let url = moved.as_deref().unwrap_or(url);
        // Bodies arrive piece by piece and are appended as they come (`stream.rs`).
        let opts = if media && opts == "{}" { "{\"progress\":1,\"chunks\":1}" } else { opts };
        bridge::fetch_opts(id, "GET", url, "", "{}", opts);
        let what = describe(&req, url);
        let mut f = Inflight::new(req, what);
        if media {
            f.base = base.to_string();
            f.mirror = if moved.is_some() { mirror } else { 0 };
            f.want = range_len(url);
            f.chunked = opts.contains("chunks");
            f.at = range_start(url);
        }
        self.fetches.insert(id, f);
        id
    }

    /// Binds the `<video>`, opens a MediaSource and asks for autoplay; the
    /// streams can arrive while `sourceopen` is pending.
    pub fn attach(&mut self) {
        self.vid = next_id();
        if !bridge::media_video(self.vid, ui::VIDEO, &self.video_events()) {
            self.fail("no <video> element".into());
            return;
        }
        bridge::media_set(self.vid, "volume", self.prefs.volume);
        bridge::media_set(self.vid, "muted", if self.prefs.muted { 1.0 } else { 0.0 });
        vol_ui(if self.prefs.muted { 0.0 } else { self.prefs.volume });
        self.apply_cc_style();
        // The mouse wheel over the captions (or their size control) resizes them (the old extension's).
        bridge::wheel(ui::CCW, "cc");
        // The whole box is a hover zone: leaving it is how the controls know the pointer is gone.
        bridge::attr(ui::BOX, "data-hov", "pl");
        // The wheel over the Speed/Quality shortcuts and inside the menu panel chooses (never scrolls the page).
        for id in [ui::SPEED, ui::QUAL, ui::MENU] {
            bridge::wheel_take(id);
        }
        self.ms = next_id();
        bridge::media_source(self.ms, self.vid);
        if !self.start_paused {
            bridge::media_call(self.vid, "play");
        }
        if self.rate != 1.0 {
            self.apply_rate();
            self.show_rate(self.rate);
        }
        self.timer(Tm::Tick, TICK_MS);
        self.ui();
    }

    /// The warm first frame (`warm.rs`, a canvas held by the bridge) goes over the `<video>` until the first
    /// frame shows: the open's zoom grows the real picture, sharp, instead of a thumbnail or a dark box.
    pub fn warm_poster(&mut self, id: String) {
        if bridge::held_put(&id, ui::VIDEO) {
            self.note(format!("warm first frame shown ({id})"));
            self.poster = Some(id);
        } else {
            bridge::held_drop(&id);
        }
    }

    fn drop_poster(&mut self) {
        if let Some(id) = self.poster.take() {
            bridge::held_drop(&id);
        }
    }

    /// Preferences loaded after the player was created (storage is async).
    pub fn set_prefs(&mut self, prefs: Prefs) {
        self.prefs = prefs;
        bridge::media_set(self.vid, "volume", self.prefs.volume);
        bridge::media_set(self.vid, "muted", if self.prefs.muted { 1.0 } else { 0.0 });
        vol_ui(if self.prefs.muted { 0.0 } else { self.prefs.volume });
        self.quality_label();
        self.apply_cc_style();
        self.ui();
    }

    /// Sends VISIONOS (streams) and WEB (captions, history) `player` calls.
    pub fn start(&mut self, ctx: &Ctx, vision: Vision) {
        if self.started || self.failed {
            return;
        }
        self.started = true;
        self.vision_cfg = Some(ctx.cfg.clone());
        self.route = ctx.route.clone();
        self.auth = ctx.auth.clone();
        match vision {
            Vision::Fetch => self.send_vision(ctx.cfg, &ctx.route.clone()),
            Vision::Pending => {
                self.vision_app = true;
                self.timer(Tm::VisionTimeout, VISION_TIMEOUT_MS);
            }
            Vision::Cached(_) => {}
        }
        // (`ytc-test-nohedge`, localStorage, tests and measurements only: no second request.)
        if !matches!(vision, Vision::Cached(_)) && !bridge::local_flag("ytc-test-nohedge") {
            // A slow or hung `player` request is asked again (the first usable answer plays).
            self.timer(Tm::VisionHedge, throttle::VISION_HEDGE_MS as u32);
        }
        let sts = ctx.cfg.sts(bridge::epoch());
        let body = api::web_player(ctx.cfg, ctx.hl, ctx.url, &self.v, &self.cpn, sts);
        // TV app signed in: the same call as YouTube's TV client with the account's bearer, so the
        // tracking URLs (and the pings below, sent with the bearer) record the watch in its history.
        let tv_session = ctx.auth.bearer.is_some();
        let (body, headers) = crate::tv::finish(ctx.cfg, ctx.auth, body, (bridge::epoch() / 1000.0) as u64);
        self.ping_headers = crate::tv::ping_headers(ctx.auth);
        let id = next_id();
        bridge::fetch(id, "POST", &api::player_url(), &body, &headers);
        self.fetches.insert(id, Inflight::new(Req::Web, "player WEB".into()));
        self.session = Session {
            client: if tv_session { crate::tv::TV_NAME.into() } else { String::new() },
            platform: if tv_session { "TV".into() } else { String::new() },
            cpn: self.cpn.clone(),
            client_version: if tv_session { crate::tv::TV_VERSION.into() } else { ctx.cfg.version.clone() },
            hl: ctx.hl.to_string(),
            referrer: format!("https://www.youtube.com{}", ctx.url),
            ..Default::default()
        };
        // SponsorBlock goes out after the player calls, at low priority; nothing waits for it.
        self.sb_lookup();
        if let Vision::Cached(body) = vision {
            self.note("VISIONOS response from cache".into());
            self.from_cache = true;
            let r = self.route.clone();
            self.on_vision(r, 200, &body, 0.0);
        }
    }

    /// The app's prefetched VISIONOS response for this video.
    pub fn feed_vision(&mut self, status: u32, body: &[u8], ms: f64) {
        self.vision_app = false;
        if self.vision_ms.is_some() || self.failed {
            return;
        }
        if status != 200 && self.fetches.values().any(|f| matches!(f.req, Req::Vision(_))) {
            // Our own (hedged) request is still on its way.
            return;
        }
        if status != 200 {
            // A failed prefetch (network) is not a verdict: ask once more ourselves.
            if let Some(cfg) = self.vision_cfg.clone() {
                self.note(format!("VISIONOS prefetch HTTP {status}: requesting again"));
                let r = self.route.clone();
                self.send_vision(&cfg, &r);
                return;
            }
        }
        let r = self.route.clone();
        self.on_vision(r, status, body, ms);
    }

    fn send_vision(&mut self, cfg: &Config, route: &VisionRoute) {
        let (body, headers, opts) = api::vision_request(cfg, &self.auth, (bridge::epoch() / 1000.0) as u64, route, &self.v);
        let id = next_id();
        let url = api::vision_url();
        bridge::fetch_opts(id, "POST", &url, &body, &headers, opts);
        let what = if *route == VisionRoute::Anon { "player VISIONOS".to_string() } else { format!("player VISIONOS ({})", route.label()) };
        let mut request = Inflight::new(Req::Vision(route.clone()), what);
        request.unmasked = (url != api::player_url()).then(|| Box::new((body, headers, opts)));
        self.fetches.insert(id, request);
        self.timer(Tm::VisionTimeout, VISION_TIMEOUT_MS);
    }

    /// Bot check on `self.route`: try the other routes at once, in parallel
    /// (the account session when signed in, a fresh anonymous visitor), so
    /// the recovery costs one round trip (plus the ~40 ms `visitor_id`).
    fn recover(&mut self, reason: &str) {
        self.tel_bot();
        if self.no_recover {
            // Every route met the bot check moments ago: this route only (Try again recovers fully).
            self.blocked();
            return self.fail(format!("VISIONOS LOGIN_REQUIRED: {reason} · tried: {} (bot check; every route was bot-checked shortly before)", self.route.label()));
        }
        let Some(cfg) = self.vision_cfg.clone() else { return self.fail(format!("VISIONOS LOGIN_REQUIRED: {reason}")) };
        let mut r = Recover { pending: 0, steps: vec![format!("{} (bot check)", self.route.label())], all_bot: true, reason: reason.to_string() };
        if self.auth.signed_in() && self.auth.bearer.is_none() && self.route != VisionRoute::Account {
            self.send_vision(&cfg, &VisionRoute::Account);
            r.pending += 1;
        }
        let (body, headers) = api::visitor_id_request(&cfg);
        let id = next_id();
        bridge::fetch_opts(id, "POST", &api::visitor_id_url(), &body, &headers, "{\"credentials\":\"omit\"}");
        self.fetches.insert(id, Inflight::new(Req::Visitor, "visitor_id (fresh visitor)".into()));
        self.timer(Tm::VisionTimeout, VISION_TIMEOUT_MS);
        r.pending += 1;
        self.note(format!("VISIONOS bot check ({}): trying {}", self.route.label(), if r.pending > 1 { "the account session and a fresh visitor" } else { "a fresh visitor" }));
        self.recover = Some(r);
    }

    /// One recovery attempt did not play: when none is left, give up.
    fn recover_step(&mut self, step: String, bot: bool) {
        if bot {
            self.tel_bot();
        }
        let Some(r) = self.recover.as_mut() else { return };
        r.steps.push(step);
        r.all_bot &= bot;
        r.pending = r.pending.saturating_sub(1);
        if r.pending > 0 {
            return;
        }
        let r = self.recover.take().unwrap_or_default();
        if r.all_bot {
            self.blocked();
        }
        self.fail(format!("VISIONOS LOGIN_REQUIRED: {} · tried: {}", r.reason, r.steps.join(", ")));
    }

    fn on_visitor(&mut self, status: u32, body: &[u8]) {
        if self.vision_ms.is_some() || self.recover.is_none() {
            return;
        }
        match (status == 200).then(|| api::parse_visitor(body)).flatten() {
            Some(vd) => {
                let Some(cfg) = self.vision_cfg.clone() else { return };
                self.send_vision(&cfg, &VisionRoute::Fresh(vd));
            }
            None => self.recover_step(format!("fresh visitor (visitor_id HTTP {status})"), true),
        }
    }

    // ------------------------------------------------------------ responses

    /// Returns true when `id` was one of ours.
    pub fn on_fetch(&mut self, id: u32, status: u32, body: &[u8]) -> bool {
        let Some(mut f) = self.fetches.remove(&id) else { return false };
        if self.failed {
            return true;
        }
        if self.seek_pre_end(id, status) {
            return true;
        }
        let ms = bridge::now() - f.sent;
        self.note_ttfb(&f);
        // A streamed body came in pieces (`on_fetch_chunk`); its end carries no bytes.
        let len = if f.chunked && (200..300).contains(&status) { f.got as usize } else { body.len() };
        self.tel_req(&f, status, len, ms);
        if self.reqlog.len() >= 80 {
            self.reqlog.remove(0);
        }
        self.reqlog.push(format!("{:>7.0} ms  {} → HTTP {status} · {len} B · {ms:.0} ms", f.sent - self.nav_start, f.what));
        if status == 400 {
            if let Some(request) = f.unmasked.take() {
                api::unmask_vision();
                let (body, headers, opts) = *request;
                bridge::fetch_opts(id, "POST", &api::player_url(), &body, &headers, opts);
                self.fetches.insert(id, f);
                return true;
            }
        }
        match f.req {
            Req::Head { .. } | Req::Seg { .. } if self.twin_answer(id, &f, status, len) => return true,
            Req::Vision(_) if status != 200 && self.vision_twin_pending(status) => return true,
            Req::Vision(_) if status == 200 => self.vision_answered(id),
            _ => {}
        }
        match f.req {
            Req::Vision(route) => self.on_vision(route, status, body, ms),
            Req::Visitor => self.on_visitor(status, body),
            Req::Web => self.on_web(status, body),
            Req::Head { itag, kind } => {
                if f.early {
                    // Parsed from its first pieces (`stream.rs`): what is left is the media's.
                    self.head_done(itag, kind, status, ms, len);
                } else {
                    let body = if f.chunked && (200..300).contains(&status) { &f.buf[..] } else { body };
                    self.on_head(itag, kind, status, body, ms);
                }
            }
            Req::Seg { kind, idx, itag } => self.on_seg(id, kind, idx, itag, status, if f.chunked { None } else { Some((f.at, body)) }, ms, len),
            Req::Captions => {
                self.cues = if status == 200 { captions::parse_json3(body) } else { Vec::new() };
                self.note(format!("captions {} cues (HTTP {status})", self.cues.len()));
                self.ui();
            }
            Req::Ping => {}
            Req::Master => self.on_master(status, body),
            Req::Playlist => self.on_playlist(status, body),
            Req::LiveSeg { kind, sq, itag } => self.on_live_seg(id, kind, sq, itag, status, body, ms),
            Req::Sponsor => self.on_sponsor(status, body),
        }
        true
    }

    fn on_vision(&mut self, route: VisionRoute, status: u32, body: &[u8], ms: f64) {
        if self.vision_ms.is_some() && !self.refreshing {
            // A slower parallel recovery attempt: the streams are already chosen.
            return;
        }
        let recovering = self.recover.is_some() && !self.refreshing;
        let Some(json) = (status == 200).then(|| json::parse(body)).flatten() else {
            if recovering {
                return self.recover_step(format!("{} (HTTP {status})", route.label()), true);
            }
            if self.refreshing && (self.stuck.is_some() || self.media_refresh) {
                // A retry while stuck: not a verdict; keep the URLs we have and the schedule going.
                self.note(format!("fresh player response HTTP {status}: keeping the current URLs"));
                self.exp_off |= self.proactive;
                return self.end_refresh();
            }
            self.fail(format!("VISIONOS player HTTP {status}"));
            return;
        };
        let ps = json.pointer("/playabilityStatus/status").and_then(Value::as_str).unwrap_or("?");
        let upcoming = json.pointer("/videoDetails/isUpcoming").and_then(Value::as_bool) == Some(true);
        if (ps == "LIVE_STREAM_OFFLINE" || upcoming) && !self.refreshing {
            let said = json.pointer("/playabilityStatus/reason").and_then(Value::as_str).unwrap_or("").trim();
            self.fail(if said.is_empty() { UPCOMING_REASON.into() } else { format!("{UPCOMING_REASON}: {said}") });
            return;
        }
        if ps != "OK" {
            let reason = json.pointer("/playabilityStatus/reason").and_then(Value::as_str).unwrap_or("");
            let bot = bot_check(ps, reason);
            if self.refreshing && (self.stuck.is_some() || self.media_refresh) {
                self.note(format!("fresh player response {ps}: keeping the current URLs"));
                self.exp_off |= self.proactive;
                if bot {
                    self.blocked();
                }
                return self.end_refresh();
            }
            if recovering {
                return self.recover_step(format!("{} ({})", route.label(), if bot { "bot check".to_string() } else { format!("{ps}: {reason}") }), bot);
            }
            if bot && !self.refreshing {
                return self.recover(reason);
            }
            if bot {
                self.blocked();
            }
            self.fail(format!("VISIONOS {ps}: {reason}"));
            return;
        }
        if let Some(r) = self.recover.take() {
            // The other attempt is no longer needed (and its answer must not look like a refresh).
            let rest: Vec<u32> = self.fetches.iter().filter(|(_, f)| matches!(f.req, Req::Vision(_) | Req::Visitor)).map(|(id, _)| *id).collect();
            for id in rest {
                self.fetches.remove(&id);
                bridge::abort(id);
            }
            self.note(format!("bot check recovered: {} OK ({ms:.0} ms) after {}", route.label(), r.steps.join(", ")));
            self.route = route.clone();
            self.out.push(Out::Route(route));
        }
        let fmts = formats::parse(&json);
        // Warm the video servers' connections while the indexes are being chosen.
        for o in googlevideo_origins(body).iter().take(2) {
            warm_origin(o);
        }
        let hls = json.pointer("/streamingData/hlsManifestUrl").and_then(Value::as_str).filter(|u| u.starts_with("https://") && !u.bytes().any(|b| b <= b' ')).map(str::to_string);
        if self.refreshing {
            self.refreshing = false;
            if let (Some(l), Some(u)) = (self.live.as_mut(), hls) {
                // Playlist URLs expire with the stream URLs: follow the new master.
                l.manifest = u;
                l.media_url.clear();
            }
            let in_use = [self.tracks[V].rep, self.tracks[A].rep];
            let mut kept_old = false;
            for rep in self.reps.values_mut() {
                // The same file only: on a dubbed video every audio track has the same itag, and the
                // index's byte ranges would cut another track's file in the middle of a cluster
                // (`SourceBuffer error (audio/webm; codecs="opus")`, 8 Nov 2026). No such file any
                // more: the old URL stays, until it runs out (the panel's way: a new player, a new index).
                match fmts.iter().find(|f| f.itag == rep.f.itag && f.mime == rep.f.mime && f.xtags == rep.f.xtags && f.audio_id == rep.f.audio_id && f.len == rep.f.len) {
                    Some(f) => rep.f.url = f.url.clone(),
                    None => kept_old |= in_use.contains(&rep.f.itag),
                }
            }
            for f in self.ladder.iter_mut().chain(self.audio.iter_mut()) {
                if let Some(n) = fmts.iter().find(|n| n.itag == f.itag && n.mime == f.mime && !n.xtags && n.audio_id == f.audio_id && n.len == f.len && (f.video || !f.audio_id.is_empty() || !n.alt_audio)) {
                    f.url = n.url.clone();
                }
            }
            // The new URLs' lifetime; a proactive refresh that did not lengthen it is not repeated.
            let life = fail::url_life(expires_in(&json), 0.0);
            if self.proactive && !fail::refresh_helped(true, life) {
                self.exp_off = true;
                self.note("the fresh URLs expire as soon: no more proactive refreshes".into());
            }
            if kept_old {
                // A track in use still has its old URL: those are not fresh, and asking again changes nothing.
                self.exp_off = true;
                self.note("a track's file changed: its old URL stays, no more proactive refreshes".into());
            } else {
                self.urls_epoch = bridge::epoch();
                self.urls_life = life;
            }
            self.note(format!("player refreshed ({ms:.0} ms)"));
            self.end_refresh();
            return;
        }
        self.vision_ms = Some(ms);
        // A cached response's age is not known here: counted as the cache's limit (30 min).
        self.urls_epoch = bridge::epoch();
        self.urls_life = fail::url_life(expires_in(&json), if self.from_cache { 1800.0 } else { 0.0 });
        if ms > 0.0 {
            self.vision_ok = Some(std::rc::Rc::new(body.to_vec()));
        }
        self.duration = json.pointer("/videoDetails/lengthSeconds").and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        self.cc_vision = cc_list_of(&json);
        // Live (and post-live DVR without byte-range files yet): HLS timeline,
        // fMP4 segments by sequence number.
        let is_live = json.pointer("/videoDetails/isLive").and_then(Value::as_bool) == Some(true);
        let ranged = fmts.iter().any(|f| f.video && f.index_end > 0);
        self.formats = fmts;
        if is_live || (!ranged && hls.is_some()) {
            self.start_live(hls, is_live);
            return;
        }
        self.choose_streams();
    }

    /// A refresh answered (new URLs applied, or kept): requests continue.
    fn end_refresh(&mut self) {
        self.refreshing = false;
        self.media_refresh = false;
        self.proactive = false;
        for t in &mut self.tracks {
            t.retry_wait = false;
        }
        for (itag, kind) in std::mem::take(&mut self.retry_heads) {
            if let Some(f) = self.format(itag, kind).cloned() {
                self.fetch_head(kind, &f, false);
            }
        }
        self.pump_all();
    }

    /// A format we use or asked for. The ladder/audio choice can be rebuilt while
    /// a request is in flight (e.g. decodingInfo prefers another codec at the
    /// same rung), so fall back to every offered format: a response for a format
    /// we requested must never be dropped (incident tz8jz7IiQK0, 1 Nov 2026).
    fn format(&self, itag: u32, kind: usize) -> Option<&Format> {
        let chosen = if kind == A { self.audio.as_ref().filter(|f| f.itag == itag) } else { self.ladder.iter().find(|f| f.itag == itag) };
        chosen.or_else(|| self.formats.iter().find(|f| f.itag == itag && f.video == (kind == V) && !f.xtags && !f.alt_audio)).or_else(|| self.formats.iter().find(|f| f.itag == itag && f.video == (kind == V)))
    }

    fn build_ladder(&mut self) {
        let mut support: HashMap<String, bool> = HashMap::new();
        for f in &self.formats {
            support.entry(f.mime.clone()).or_insert_with(|| bridge::mse_ok(&f.mime));
        }
        let ok = |m: &str| support.get(m).copied().unwrap_or(false);
        let codecs = formats::order(Codec::from_key(&self.prefs.codec));
        let bad = self.bad.clone();
        if self.live.is_some() {
            self.ladder = formats::live_ladder(&self.formats, &codecs, &|m: &str| ok(m)).into_iter().filter(|f| !bad.contains(&f.itag)).cloned().collect();
            self.audio = formats::live_audio(&self.formats, &|m: &str| ok(m)).cloned();
            return;
        }
        let caps = &self.caps;
        let known = |i: u32| caps.get(&i).copied();
        let screen = formats::caps_for_screen(&self.formats, bridge::matches("(dynamic-range: high)"), &known);
        self.ladder = formats::ladder_with(&self.formats, &codecs, &|m: &str| ok(m), &screen).into_iter().filter(|f| !bad.contains(&f.itag)).cloned().collect();
        // Codecs that failed (`next_codec`, the app's tab-session verdicts) are left out while
        // another codec remains (`avoid` filters the candidates before the per-rung choice).
        if !self.avoid.is_empty() {
            let avoid = &self.avoid;
            let kept: Vec<Format> = formats::ladder_with(&self.formats, &codecs, &|m: &str| ok(m) && !avoid.iter().any(|c| formats::codec_of(m) == *c), &screen).into_iter().filter(|f| !bad.contains(&f.itag)).cloned().collect();
            if !kept.is_empty() {
                self.ladder = kept;
            }
        }
        let usable: Vec<Format> = self.formats.iter().filter(|f| !bad.contains(&f.itag)).cloned().collect();
        if crate::tv::on() {
            // A track picked on this video earlier in the session counts as picked now; else the language chosen
            // last, where this video has it. Never over a choice made for this video.
            if self.audio_sel.is_empty() {
                self.audio_sel = formats::chosen_audio(&self.v).unwrap_or_default();
            }
            self.audio = formats::tv_audio(&usable, &self.audio_sel, &self.prefs.audio_lang, &ok).or_else(|| formats::audio(&usable, &ok)).cloned();
            bridge::class_all(&format!("#{}", ui::AUB), "on", formats::audio_tracks(&usable, &ok).len() > 1);
        } else {
            let chosen = (!self.audio_sel.is_empty()).then(|| formats::audio_in(&usable, &self.audio_sel, &ok)).flatten();
            self.audio = chosen.or_else(|| formats::audio(&usable, &ok)).cloned();
        }
    }

    /// Size cap (Auto) or the user's choice.
    fn limit(&self) -> u32 {
        if self.prefs.quality > 0 {
            return self.prefs.quality;
        }
        // No layout read here: this runs in the click task right after the watch page was written,
        // where each `bridge::size` forced a layout (boot builder, 3 Nov 2026: 5 calls ≈ 13 ms, 52 ms
        // at 4× CPU). The screen decides the cap; the player box (read on the tick) can only raise it.
        let d = bridge::screen_px();
        let screen = if d.len() == 3 { d[1].min(d[0] * 9.0 / 16.0) * d[2] } else { 1080.0 };
        let cap = formats::display_cap(screen, self.box_h);
        let cap = if self.drop_cap > 0 { cap.min(self.drop_cap) } else { cap };
        self.thr_cap().map_or(cap, |t| cap.min(t))
    }

    /// Seconds to keep buffered ahead for the current video rung.
    fn ahead_target(&self) -> f64 {
        if self.tracks.iter().any(|t| t.quota) {
            return sched::AHEAD;
        }
        // Below the quality ABR wants (e.g. the low start rung): keep the buffer
        // short so the upgrade shows within seconds, not after minutes.
        if self.tracks[V].pending.is_some() || self.below_target() {
            return 10.0;
        }
        let bps = self.reps.get(&self.tracks[V].rep).map(|r| r.f.bitrate).unwrap_or(0);
        formats::ahead_secs(bps)
    }

    /// Whether the playing video rung is lower than the rung ABR currently wants.
    fn below_target(&self) -> bool {
        let Some(i) = self.desired() else { return false };
        let cur = self.format(self.tracks[V].rep, V).map(|f| f.p).unwrap_or(0);
        self.ladder.get(i).is_some_and(|t| t.p > cur)
    }

    /// Ask the browser which video formats it decodes smoothly / in hardware. Answers are kept
    /// for this page (`CAPS`): later videos with the same formats need no question. True when
    /// some answer is still missing (asked now).
    fn ask_caps(&mut self) -> bool {
        if self.caps_asked {
            return false;
        }
        self.caps_asked = true;
        self.tel_caps_asked();
        let mut list = Vec::new();
        let mut unknown = false;
        for f in self.formats.iter().filter(|f| f.video) {
            let key = caps_key(f);
            if let Some(c) = CAPS.with(|c| c.borrow().get(&key).copied()) {
                self.caps.insert(f.itag, c);
                continue;
            }
            // The broad answer (asked at boot) decides now; the exact one is asked anyway.
            match CAPS.with(|c| c.borrow().get(&caps_norm(f)).copied()) {
                Some(c) => {
                    self.caps.insert(f.itag, c);
                }
                None => unknown = true,
            }
            self.caps_keys.insert(f.itag, key);
            list.push(caps_question(f.itag as u64, f));
        }
        if !list.is_empty() {
            bridge::media_caps(&serde_json::Value::Array(list).to_string());
        }
        if !unknown {
            self.tel_caps();
        }
        unknown
    }

    /// `decodingInfo` results arrived: rebuild the ladder and let ABR use them.
    pub fn on_caps(&mut self, json: &str) {
        self.tel_caps();
        let Some(Value::Array(list)) = json::parse_str(json) else { return };
        for r in list {
            let Some(i) = r["i"].as_u64() else { continue };
            let c = if r["ok"].as_bool() == Some(false) { (false, false) } else { (r["s"].as_bool().unwrap_or(true), r["e"].as_bool().unwrap_or(false)) };
            self.caps.insert(i as u32, c);
            if let Some(key) = self.caps_keys.remove(&(i as u32)) {
                CAPS.with(|m| {
                    let mut m = m.borrow_mut();
                    if m.len() < 400 {
                        m.insert(key, c);
                    }
                });
            }
        }
        let eff = self.caps.values().filter(|c| c.1).count();
        let smooth = self.caps.values().filter(|c| c.0).count();
        self.note(format!("decodingInfo: {} formats, {smooth} smooth, {eff} power-efficient", self.caps.len()));
        if self.failed || self.formats.is_empty() {
            return;
        }
        if self.caps_wait {
            self.caps_wait = false;
            self.choose_streams_now();
            return;
        }
        self.build_ladder();
        if self.live.is_none() && self.restart_streams() {
            return;
        }
        // A switch target the rebuilt ladder no longer offers (e.g. VP9 1080p
        // when only AV1 decodes smoothly here): forget it, ABR picks again.
        if let Some((p, _)) = self.tracks[V].pending {
            if !self.ladder.iter().any(|f| f.itag == p) {
                self.tracks[V].pending = None;
            }
        }
        // The playing format turned out not to decode smoothly: replace it now
        // (a chosen quality caps the resolution; the codec is still ours to pick).
        let cur = self.tracks[V].rep;
        if self.caps.get(&cur).is_some_and(|c| !c.0) {
            if let Some(i) = self.desired() {
                let itag = self.ladder[i].itag;
                if itag != cur {
                    self.note(format!("itag {cur} not smooth here: switching to {itag} now"));
                    self.switch_to(itag, true);
                    return;
                }
            }
        }
        self.abr();
    }

    /// decodingInfo arrived before anything of the starting video format was
    /// appended (it usually lands ~2 ms after the streams are chosen): start
    /// from the rebuilt ladder's start rung and target instead, so no time goes
    /// into a format that is about to be replaced (live 1 Nov 2026: VP9 chosen,
    /// then ruled out, while the AV1 1080p target sat unused).
    fn restart_streams(&mut self) -> bool {
        let tr = &self.tracks[V];
        if tr.rep == 0 || tr.ready || tr.appended != 0 || self.first_frame.is_some() {
            return false;
        }
        let Some(target) = self.desired() else { return false };
        let sv = self.ladder[target].clone();
        let old = tr.rep;
        if sv.itag == old {
            return false;
        }
        self.note(format!("decodingInfo: starting from {} {} instead of {old}", sv.itag, sv.label));
        // The ruled-out format's first request is no longer needed.
        let stale: Vec<u32> = self.fetches.iter().filter(|(_, f)| matches!(f.req, Req::Head { kind: V, itag } if itag == old)).map(|(k, _)| *k).collect();
        for k in stale {
            self.abort_fetch(k);
        }
        self.heads.retain(|&x| x != old);
        let tr = &mut self.tracks[V];
        tr.rep = sv.itag;
        tr.pending = None;
        if tr.sb == 0 {
            tr.mime = sv.mime.clone();
        }
        self.fetch_head(V, &sv, self.start < 4.0);
        self.quality_label();
        true
    }

    fn desired(&self) -> Option<usize> {
        let refs: Vec<&Format> = self.ladder.iter().collect();
        if self.prefs.quality > 0 { formats::pick(&refs, self.prefs.quality) } else { formats::abr(&refs, self.limit(), self.bw.bps) }
    }

    /// decodingInfo first (≤ `CAPS_WAIT_MS`, usually a few ms; answers cached for the page), so
    /// the first requests already go to the format that will play.
    fn choose_streams(&mut self) {
        if self.ask_caps() {
            self.caps_wait = true;
            self.timer(Tm::CapsWait, CAPS_WAIT_MS);
            return;
        }
        self.choose_streams_now();
    }

    fn choose_streams_now(&mut self) {
        if self.failed || self.tracks[V].rep != 0 {
            return;
        }
        self.build_ladder();
        let (Some(target), Some(audio)) = (self.desired(), self.audio.clone()) else {
            let mut support: HashMap<String, bool> = HashMap::new();
            for f in &self.formats {
                support.entry(f.mime.clone()).or_insert_with(|| bridge::mse_ok(&f.mime));
            }
            let why = formats::diagnose(&self.formats, &|m: &str| support.get(m).copied().unwrap_or(false));
            self.fail(format!("no compatible formats ({} offered: {why})", self.formats.len()));
            return;
        };
        // Straight to the target rung (≥ 1080p when the screen and connection allow): its
        // index and first segment in one request. A low start rung only took bandwidth from it
        // (live 2 Nov 2026: the 360p data was replaced before its first frame showed).
        let sv = self.ladder[target].clone();
        self.note(format!(
            "VISIONOS OK {:.0} ms · {} formats · start {} {} {} · audio {} {}",
            self.vision_ms.unwrap_or(0.0),
            self.formats.len(),
            sv.itag,
            sv.label,
            sv.codec.map_or("?", |c| c.name()),
            audio.itag,
            audio.codec.map_or("?", |c| c.name())
        ));
        if self.duration <= 0.0 {
            self.duration = sv.dur;
        }
        // A start at (or past) the end of the media plays from the beginning (a resume point left
        // at the very end, a `t=` past it). Before, every track found nothing to fetch, was "done"
        // without its init segment, and the end of stream that followed failed the start ("endOfStream
        // before demuxer initialization completes").
        let end = [sv.dur, audio.dur, self.duration].into_iter().filter(|d| *d > 0.0).fold(f64::INFINITY, f64::min);
        if self.start > 0.0 && end.is_finite() && self.start >= end - START_END_SLACK {
            self.note(format!("start {:.2} s is at the end ({end:.2} s): from the beginning", self.start));
            self.start = 0.0;
        }
        let extra = self.start < 4.0;
        self.tracks[V].rep = sv.itag;
        self.tracks[V].mime = sv.mime.clone();
        self.tracks[A].rep = audio.itag;
        self.tracks[A].mime = audio.mime.clone();
        self.pre_ok = true;
        self.fetch_head(V, &sv, extra);
        self.fetch_head(A, &audio, extra);
        self.pre_ok = false;
        self.streams_at = bridge::now();
        self.load_at = self.streams_at;
        self.tel_streams(sv.p, sv.p);
        self.quality_label();
        self.setup_mse();
        self.pre_flush();
    }

    fn fetch_head(&mut self, kind: usize, f: &Format, extra: bool) {
        if self.heads.contains(&f.itag) {
            return;
        }
        // With `extra`, the first segment comes in the same request (one round trip less).
        let end = pre::head_end(f, kind, extra);
        self.heads.push(f.itag);
        if self.pre_ok && self.pre_take(kind, f, end) {
            return;
        }
        self.get(Req::Head { itag: f.itag, kind }, &format!("{}&range=0-{end}", f.url), "{}");
    }

    fn on_head(&mut self, itag: u32, kind: usize, status: u32, body: &[u8], ms: f64) {
        self.got_media = true;
        self.heads.retain(|&x| x != itag);
        if status != 200 && status != 206 {
            self.media_http_error(kind, status, Some(itag));
            return;
        }
        if self.head_parse(itag, kind, body, body.len() as u64, true) {
            self.bw.add(body.len(), ms);
        }
        self.pump(kind);
    }

    /// An index request's bytes from its start: parse the index (once `body` holds it; `whole` =
    /// that is all there will be), start the track and append the first segment's bytes that came
    /// along (`stream.rs` appends the rest as it arrives). True when the index was parsed.
    fn head_parse(&mut self, itag: u32, kind: usize, body: &[u8], upto: u64, whole: bool) -> bool {
        let Some(f) = self.format(itag, kind).cloned() else { return false };
        let need = f.index_end as usize + 1;
        if body.len() < need {
            if whole {
                self.fail(format!("index of itag {itag}: {} of {need} bytes", body.len()));
            }
            return false;
        }
        let parsed = if f.webm { index::parse_cues(&body[..need], f.len, f.dur.max(self.duration)) } else { index::parse_sidx(&body[..need]) };
        let segs = match parsed {
            Ok(s) => index::clip(s, self.duration),
            Err(e) => {
                self.fail(format!("index of itag {itag}: {e}"));
                return false;
            }
        };
        let init = body[..=(f.init_end as usize).min(need - 1)].to_vec();
        let n = segs.len();
        self.reps.insert(itag, Rep { f, segs, init });
        let tr = &self.tracks[kind];
        if tr.rep == itag && !tr.ready {
            let next = sched::start_index(&self.reps[&itag].segs, self.start);
            self.tracks[kind].ready = true;
            self.tracks[kind].next = next;
            // The first segment's start came in the same request: append it now.
            if let Some(s) = self.reps[&itag].segs.first().filter(|s| next == 0 && s.start as usize == need) {
                let (start, end) = (s.start, s.end);
                self.tracks[kind].asm = Some(stream::SegAsm::new(itag, 0, start, end, upto.max(start)));
                self.feed(kind, itag, 0, need as u64, &body[need..]);
            }
            self.note(format!("{} index itag {itag}: {n} segments", if kind == V { "video" } else { "audio" }));
        } else if self.tracks[kind].pending == Some((itag, true)) {
            self.apply_switch(kind);
        } else if body.len() > need && self.tracks[kind].asm.as_ref().is_some_and(|a| a.itag == itag && a.idx == 0) {
            // A second index request (a mirror twin) brings the same first bytes: only new ones count.
            self.feed(kind, itag, 0, need as u64, &body[need..]);
        }
        true
    }

    fn on_seg(&mut self, id: u32, kind: usize, idx: usize, itag: u32, status: u32, body: Option<(u64, &[u8])>, ms: f64, len: usize) {
        self.got_media = true;
        let tr = &mut self.tracks[kind];
        if tr.inflight != Some(id) {
            return;
        }
        tr.inflight = None;
        if status != 200 && status != 206 {
            self.media_http_error(kind, status, None);
            return;
        }
        // A body that came whole (not streamed: e.g. a hover prefetch taken over).
        if let Some((at, b)) = body {
            self.feed(kind, itag, idx, at, b);
        }
        if self.failed {
            return;
        }
        let tr = &self.tracks[kind];
        if let Some(a) = tr.asm.as_ref().filter(|a| a.itag == itag && a.idx == idx) {
            // The request ended before the segment did: ask for the rest from where it stopped.
            let (have, want) = (a.asm.frontier() - a.asm.start, a.asm.end - a.asm.start + 1);
            self.retry(kind, format!("segment {idx} of itag {itag}: {have} of {want} bytes"));
            return;
        }
        self.bw.add(len, ms);
        self.tracks[kind].retries = 0;
        if kind == V {
            self.abr();
        }
        self.pump(kind);
    }

    /// 403/410 → one `player` refresh (new URLs, same position); other
    /// errors → retries with backoff; then give up.
    fn media_http_error(&mut self, kind: usize, status: u32, head: Option<u32>) {
        if status == 403 || status == 410 {
            if self.refreshing {
                // Both tracks usually hit the expiry together: wait for the refresh.
                self.tracks[kind].retry_wait = true;
                if let Some(itag) = head {
                    self.retry_heads.push((itag, kind));
                }
                return;
            }
            if self.refreshes >= 1 {
                self.fail(format!("googlevideo HTTP {status} after a player refresh"));
                return;
            }
            self.refreshes += 1;
            self.refreshing = true;
            self.note(format!("googlevideo HTTP {status}: refreshing the player response"));
            for t in &mut self.tracks {
                t.retry_wait = true;
            }
            if let Some(itag) = head {
                self.retry_heads.push((itag, kind));
            }
            if let Some(cfg) = self.vision_cfg.clone() {
                let r = self.route.clone();
                self.send_vision(&cfg, &r);
            }
            return;
        }
        if let Some(itag) = head {
            let current = self.tracks[kind].rep == itag;
            let pending = self.tracks[kind].pending.map(|p| p.0) == Some(itag);
            // A format we no longer start from or switch to (e.g. a switch target
            // dropped when the starting format was replaced): never fatal.
            if !current && !pending {
                self.bad.push(itag);
                self.build_ladder();
                self.note(format!("itag {itag} unreachable (HTTP {status}), not in use: ignored"));
                return;
            }
            // 5xx / network errors are often momentary on one googlevideo node:
            // ask again twice (250 ms, 750 ms) before giving the format up.
            if status == 0 || status >= 500 {
                let tries = self.head_tries.entry(itag).or_insert(0);
                if *tries < 2 {
                    *tries += 1;
                    let ms = if *tries == 1 { 250 } else { 750 };
                    self.note(format!("itag {itag} index HTTP {status}: asking again in {ms} ms"));
                    self.timer(Tm::Head(itag, kind), ms);
                    return;
                }
            }
            if self.replace_rep(kind, itag) {
                self.note(format!("itag {itag} unreachable (HTTP {status}): using another format"));
                return;
            }
            if kind == V && self.drop_upgrade(itag) {
                self.note(format!("itag {itag} unreachable (HTTP {status}): staying on a lower quality"));
                return;
            }
            self.fail(format!("index of itag {itag}: HTTP {status}"));
            return;
        }
        self.retry(kind, format!("googlevideo HTTP {status}"));
    }

    /// The format a track is about to start from failed before any of it was
    /// appended (live: googlevideo can 302 a range to plain http, which the
    /// browser blocks): mark it bad and start the track from the next best one.
    fn replace_rep(&mut self, kind: usize, itag: u32) -> bool {
        if self.tracks[kind].rep != itag || self.tracks[kind].appended == itag || self.bad.len() > 12 {
            return false;
        }
        self.bad.push(itag);
        self.build_ladder();
        let next = if kind == A {
            self.audio.clone()
        } else {
            let target = self.desired();
            target.map(|t| self.ladder[t].clone())
        };
        let Some(f) = next.filter(|f| f.itag != itag) else { return false };
        let tr = &mut self.tracks[kind];
        tr.rep = f.itag;
        if tr.sb == 0 {
            tr.mime = f.mime.clone();
        }
        if kind == V {
            tr.pending = None;
        }
        self.fetch_head(kind, &f, self.start < 4.0);
        true
    }

    /// A video format we were only switching up to failed: forget it and keep
    /// playing the current one. False when it is the format already playing.
    fn drop_upgrade(&mut self, itag: u32) -> bool {
        if self.tracks[V].rep == itag || self.tracks[V].pending.map(|p| p.0) != Some(itag) {
            return false;
        }
        self.tracks[V].pending = None;
        self.bad.push(itag);
        self.build_ladder();
        true
    }

    fn retry(&mut self, kind: usize, why: String) {
        self.tracks[kind].retries += 1;
        // A video quality whose chunks keep failing: step down one rung instead
        // of abandoning playback (live 1 Nov 2026: random googlevideo redirects).
        if kind == V && self.tracks[kind].retries == 2 && self.step_down() {
            self.note(format!("{why}: switching to a lower quality"));
            return;
        }
        let tr = &mut self.tracks[kind];
        if tr.retries > MAX_RETRIES {
            self.fail(format!("{why} ({MAX_RETRIES} retries)"));
            return;
        }
        tr.retry_wait = true;
        let ms = 400 << tr.retries;
        self.note(format!("{why}; retry in {ms} ms"));
        self.timer(Tm::Retry(kind), ms);
    }

    fn on_web(&mut self, status: u32, body: &[u8]) {
        self.web_done = true;
        let Some(json) = (status == 200).then(|| json::parse(body)).flatten() else {
            self.web_note = format!("WEB player HTTP {status}: no captions/history");
            self.note(self.web_note.clone());
            return;
        };
        let ps = json.pointer("/playabilityStatus/status").and_then(Value::as_str).unwrap_or("?").to_string();
        self.tracking = stats::tracking(&json);
        self.cc_web = cc_list_of(&json);
        self.story = if self.live.is_some() { None } else { storyboard::from_player(&json) };
        self.web_note = format!(
            "WEB player {ps} · history pings {} · {} caption tracks · storyboard {}",
            if self.tracking.is_some() { "yes" } else { "no" },
            self.cc_web.tracks.len(),
            self.story.as_ref().map_or("none".to_string(), |s| s.levels.iter().map(|l| format!("{}×{}", l.w, l.h)).collect::<Vec<_>>().join("/"))
        );
        self.note(self.web_note.clone());
        if self.prefs.captions {
            self.load_captions();
        }
        if !self.paused && self.first_frame.is_some() && self.playback_sent == 0.0 {
            self.send_playback();
        }
    }

    // ------------------------------------------------------------------ MSE

    fn setup_mse(&mut self) {
        if !self.ms_open || self.sbs_made || self.tracks[V].mime.is_empty() || self.failed {
            return;
        }
        self.sbs_made = true;
        if self.live.is_some() {
            bridge::media_set(self.ms, "duration", f64::INFINITY);
        } else if self.duration > 0.0 {
            bridge::media_set(self.ms, "duration", self.duration);
        }
        for k in [V, A] {
            let h = next_id();
            if !bridge::source_buffer(h, self.ms, &self.tracks[k].mime) {
                let m = self.tracks[k].mime.clone();
                self.fail(format!("addSourceBuffer({m}) failed"));
                return;
            }
            self.tracks[k].sb = h;
        }
        if self.start > 0.0 && self.live.is_none() {
            if self.first_frame.is_some() {
                // A recovery's fresh MediaSource goes on where it was: our own positioning, no seek
                // of the viewer's (no request redone, SponsorBlock unaffected).
                self.own_seek = Some((self.start, bridge::now()));
                self.sb_own_seek(self.start);
            }
            bridge::media_set(self.vid, "currentTime", self.start);
        }
        self.pump_all();
    }

    fn flush(&mut self, kind: usize) {
        if !self.ms_open || self.failed {
            return;
        }
        loop {
            let t = self.time();
            let tr = &mut self.tracks[kind];
            if tr.updating || tr.sb == 0 {
                return;
            }
            let Some(op) = tr.ops.pop_front() else { return };
            // A new segment (or init/type change) after a partly appended one: drop that part first.
            if tr.mid && !matches!(op, Op::Remove(..) | Op::Chunk { first: false, .. }) {
                bridge::sb_abort(tr.sb);
                tr.mid = false;
            }
            match op {
                Op::Chunk { bytes, first, last } => {
                    let r = bridge::sb_append(tr.sb, &bytes);
                    if r.is_empty() {
                        tr.updating = true;
                        tr.quota = false;
                        tr.mid = !last;
                        return;
                    }
                    if r == "QuotaExceededError" && !tr.quota {
                        tr.quota = true;
                        tr.ops.push_front(Op::Chunk { bytes, first, last });
                        tr.ops.push_front(Op::Remove(0.0, (t - 10.0).max(0.0)));
                        continue;
                    }
                    let detail = bridge::media_str(self.vid, "error.message");
                    self.media_error(format!("appendBuffer: {r}"), Some(kind), &detail);
                    return;
                }
                Op::ChangeType(m) => {
                    if !bridge::sb_change_type(tr.sb, &m) {
                        // The browser refused the codec change: a codec fault (another format or codec family).
                        self.media_error(format!("changeType({m}) failed"), Some(kind), "codec");
                        return;
                    }
                }
                Op::Remove(a, b) => {
                    if b > a && bridge::sb_remove(tr.sb, a, b) {
                        tr.updating = true;
                        return;
                    }
                }
                Op::Append(bytes) => {
                    let r = bridge::sb_append(tr.sb, &bytes);
                    if r.is_empty() {
                        tr.updating = true;
                        tr.quota = false;
                        return;
                    }
                    if r == "QuotaExceededError" && !tr.quota {
                        tr.quota = true;
                        tr.ops.push_front(Op::Append(bytes));
                        tr.ops.push_front(Op::Remove(0.0, (t - 10.0).max(0.0)));
                        continue;
                    }
                    let detail = bridge::media_str(self.vid, "error.message");
                    self.media_error(format!("appendBuffer: {r}"), Some(kind), &detail);
                    return;
                }
            }
        }
    }

    pub fn time(&self) -> f64 {
        let t = bridge::media_get(self.vid, "currentTime");
        if t.is_finite() { t } else { 0.0 }
    }

    fn pump_all(&mut self) {
        self.pump(V);
        self.pump(A);
        self.maybe_eos();
    }

    fn pump(&mut self, kind: usize) {
        if self.failed || !self.ms_open || !self.sbs_made {
            return;
        }
        if self.live.is_some() {
            self.pump_live(kind);
            return;
        }
        self.flush(kind);
        // Immediate switches whose index came in before the starting format's
        // (then `on_head` could not apply them) are applied here too, else the
        // video stayed on the 360p start rung for good (live 1 Nov 2026).
        if let Some((itag, now)) = self.tracks[kind].pending {
            if self.tracks[kind].ready && (now || self.tracks[kind].inflight.is_none()) && self.reps.contains_key(&itag) {
                self.apply_switch(kind);
            }
        }
        let t = self.time();
        let tr = &self.tracks[kind];
        if !tr.ready || tr.retry_wait || tr.sb == 0 {
            return;
        }
        self.evict(kind, t);
        let tr = &self.tracks[kind];
        if tr.inflight.is_some() || tr.ops.len() > 3 {
            return;
        }
        // The segment in progress first: the bytes no request is bringing (the index request may
        // still be delivering its start, `SegAsm::asked`).
        if let Some(a) = tr.asm.as_ref() {
            let from = if self.heads.contains(&a.itag) { a.asked } else { a.asm.frontier() };
            let (itag, idx, end) = (a.itag, a.idx, a.asm.end);
            if from > end {
                return;
            }
            let Some(rep) = self.reps.get(&itag) else { return };
            let url = format!("{}&range={from}-{end}", rep.f.url);
            let id = self.get(Req::Seg { kind, idx, itag }, &url, "{}");
            if let Some(a) = self.tracks[kind].asm.as_mut() {
                a.asked = end + 1;
            }
            self.tracks[kind].inflight = Some(id);
            return;
        }
        let ahead = self.ahead_target();
        let tr = &self.tracks[kind];
        let Some(rep) = self.reps.get(&tr.rep) else { return };
        match sched::next(&rep.segs, tr.next, t, ahead) {
            Next::Fetch(i) => {
                let s = &rep.segs[i];
                let url = format!("{}&range={}-{}", rep.f.url, s.start, s.end);
                let itag = tr.rep;
                let asm = stream::SegAsm::new(itag, i, s.start, s.end, s.end + 1);
                let id = self.get(Req::Seg { kind, idx: i, itag }, &url, "{}");
                let tr = &mut self.tracks[kind];
                tr.asm = Some(asm);
                tr.inflight = Some(id);
                self.flush_keep(kind);
            }
            Next::Done => self.maybe_eos(),
            Next::Wait => {}
        }
    }

    /// The element has its metadata, i.e. every SourceBuffer has had its init segment: only then may
    /// the stream end. Chromium treats `endOfStream()` before that as a fatal error
    /// (`DEMUXER_ERROR_COULD_NOT_OPEN`, "endOfStream before demuxer initialization completes").
    fn initialised(&self) -> bool {
        bridge::media_get(self.vid, "readyState") >= 1.0
    }

    fn maybe_eos(&mut self) {
        if self.eos || !self.ms_open || !self.sbs_made || self.failed || !self.initialised() {
            return;
        }
        if let Some(l) = &self.live {
            let last = l.win.last().unwrap_or(u64::MAX);
            let done = l.win.ended && self.tracks.iter().enumerate().all(|(k, t)| t.ready && t.inflight.is_none() && t.ops.is_empty() && !t.updating && l.next[k] > last);
            if done && bridge::media_str(self.ms, "readyState") == "open" {
                self.eos = true;
                bridge::media_call(self.ms, "endOfStream");
                self.note("live stream ended: end of stream".into());
            }
            return;
        }
        // A switch still pending once the playing rung is fully fetched can never
        // take effect; it must not hold back the end of stream.
        let done = self.tracks.iter().all(|t| t.ready && t.inflight.is_none() && t.ops.is_empty() && !t.updating && self.reps.get(&t.rep).is_some_and(|r| t.next >= r.segs.len()));
        if done && bridge::media_str(self.ms, "readyState") == "open" {
            self.eos = true;
            bridge::media_call(self.ms, "endOfStream");
            self.note("end of stream".into());
        }
    }


    // ----------------------------------------------------------------- live

    fn start_live(&mut self, hls: Option<String>, is_live: bool) {
        let Some(manifest) = hls else {
            self.fail(format!("live stream without an HLS manifest ({} formats)", self.formats.len()));
            return;
        };
        self.live = Some(LiveSt { manifest: manifest.clone(), edge: is_live, from: if is_live { 0.0 } else { self.start }, ..Default::default() });
        self.build_ladder();
        let (Some(target), Some(audio)) = (self.desired(), self.audio.clone()) else {
            let n = self.formats.iter().filter(|f| formats::live_ok(f)).count();
            self.fail(format!("live stream: no playable fMP4 formats ({} offered, {n} by sequence number)", self.formats.len()));
            return;
        };
        let refs: Vec<&Format> = self.ladder.iter().collect();
        let sv = self.ladder[formats::start_rung(&refs, target)].clone();
        let tv = self.ladder[target].clone();
        self.note(format!(
            "VISIONOS OK {:.0} ms · {} · {} formats · start {} {} → target {} · audio {} {}",
            self.vision_ms.unwrap_or(0.0),
            if is_live { "LIVE" } else { "post-live (sequence numbers)" },
            self.formats.len(),
            sv.itag,
            sv.label,
            tv.label,
            audio.itag,
            audio.codec.map_or("?", |c| c.name())
        ));
        self.tracks[V].rep = sv.itag;
        self.tracks[V].mime = sv.mime.clone();
        self.tracks[A].rep = audio.itag;
        self.tracks[A].mime = audio.mime.clone();
        self.quality_label();
        self.live_get(Req::Master, &manifest);
        self.setup_mse();
    }

    fn live_get(&mut self, req: Req, url: &str) {
        if let Some(l) = self.live.as_mut() {
            l.polling = true;
        }
        self.get(req, url, "{}");
    }

    fn live_poll(&mut self) {
        let Some(l) = self.live.as_ref() else { return };
        if l.polling || self.refreshing {
            return;
        }
        if l.media_url.is_empty() {
            let u = l.manifest.clone();
            self.live_get(Req::Master, &u);
        } else {
            let u = l.media_url.clone();
            self.live_get(Req::Playlist, &u);
        }
    }

    /// A failed playlist request: 403/410 → one `player` refresh (new URLs);
    /// otherwise retry at the next poll; give up after five in a row.
    fn live_list_failed(&mut self, what: &str, status: u32) {
        let Some(l) = self.live.as_mut() else { return };
        l.polling = false;
        l.fails += 1;
        let (fails, begun, target) = (l.fails, l.begun, l.win.target);
        if (status == 403 || status == 410) && self.refreshes < 1 && !self.refreshing {
            self.refreshes += 1;
            self.refreshing = true;
            self.note(format!("{what} HTTP {status}: refreshing the player response"));
            if let Some(cfg) = self.vision_cfg.clone() {
                let r = self.route.clone();
                self.send_vision(&cfg, &r);
            }
        } else if fails > 4 || (!begun && fails > 2) {
            self.fail(format!("live {what}: HTTP {status} ({fails} times)"));
            return;
        } else {
            self.note(format!("live {what}: HTTP {status}; retrying"));
        }
        let ms = if target > 0.0 { live::refresh_ms(target, &live::Update::Same) } else { 1000 };
        self.timer(Tm::LivePoll, ms);
    }

    fn on_master(&mut self, status: u32, body: &[u8]) {
        let vs = if status == 200 { live::parse_master(&String::from_utf8_lossy(body)) } else { Vec::new() };
        let p = self.ladder.iter().find(|f| f.itag == self.tracks[V].rep).map_or(360, |f| f.p);
        let Some(i) = live::pick_variant(&vs, p) else {
            if status == 200 {
                self.fail(format!("live master playlist: no variants ({} bytes)", body.len()));
            } else {
                self.live_list_failed("master playlist", status);
            }
            return;
        };
        let Some(l) = self.live.as_mut() else { return };
        l.polling = false;
        l.media_url = vs[i].uri.clone();
        if !l.begun {
            self.note(format!("live master: {} variants, following {}p", vs.len(), vs[i].height));
        }
        self.live_poll();
    }

    fn on_playlist(&mut self, status: u32, body: &[u8]) {
        let parsed = if status == 200 { live::parse_media(&String::from_utf8_lossy(body)) } else { None };
        let Some(p) = parsed else {
            self.live_list_failed("media playlist", if status == 200 { 0 } else { status });
            return;
        };
        let Some(l) = self.live.as_mut() else { return };
        l.polling = false;
        l.fails = 0;
        let u = l.win.update(&p);
        let mut note = None;
        if !l.begun {
            l.begun = true;
            let start = if l.edge {
                l.win.start_sq(live::target_latency(p.target))
            } else {
                // Ended/post-live: from `t=` seconds into the window.
                let mut acc = 0.0;
                l.win.segs.iter().find(|s| {
                    acc += s.1;
                    acc > l.from
                }).map(|s| s.0).or(l.win.first())
            };
            let s = start.unwrap_or(0);
            l.next = [s, s];
            note = Some(format!(
                "live playlist: {} segments ({:.0} s), target {} s{}, starting at sq {s}",
                p.segs.len(),
                l.win.span(),
                p.target,
                if p.ended { ", ended" } else { "" }
            ));
        }
        let ended = p.ended;
        let ms = live::refresh_ms(p.target, &u);
        if let Some(n) = note {
            self.note(n);
            for t in &mut self.tracks {
                t.ready = true;
            }
            self.streams_at = bridge::now();
            self.load_at = self.streams_at;
        }
        if !ended {
            self.timer(Tm::LivePoll, ms);
        }
        self.pump_all();
    }

    fn pump_live(&mut self, kind: usize) {
        self.flush(kind);
        let start_at = self.live_position();
        let t = start_at.unwrap_or_else(|| self.time());
        let tr = &self.tracks[kind];
        if !tr.ready || tr.retry_wait || tr.sb == 0 {
            return;
        }
        if start_at.is_none() {
            self.evict(kind, t);
        }
        let tr = &self.tracks[kind];
        if tr.inflight.is_some() || tr.ops.len() > 3 {
            return;
        }
        let Some(l) = self.live.as_ref() else { return };
        match live::next(&l.win, l.next[kind], t, true) {
            live::Next::Fetch(sq) => {
                let itag = tr.rep;
                let Some(f) = self.format(itag, kind) else { return };
                let url = format!("{}&sq={sq}", f.url);
                let id = self.get(Req::LiveSeg { kind, sq, itag }, &url, "{}");
                self.tracks[kind].inflight = Some(id);
            }
            live::Next::Done => self.maybe_eos(),
            live::Next::Wait => {}
        }
    }

    fn on_live_seg(&mut self, id: u32, kind: usize, sq: u64, itag: u32, status: u32, body: &[u8], ms: f64) {
        if self.tracks[kind].inflight != Some(id) {
            return;
        }
        self.tracks[kind].inflight = None;
        if status == 404 || status == 204 {
            // Out of the window (or not there yet): continue from what the playlist offers.
            let first = self.live.as_ref().and_then(|l| l.win.first()).unwrap_or(sq);
            if sq < first {
                if let Some(l) = self.live.as_mut() {
                    l.next[kind] = first;
                }
                self.pump(kind);
                return;
            }
        }
        if status != 200 {
            self.media_http_error(kind, status, None);
            return;
        }
        let parts = match live::split(body) {
            Ok(p) => p,
            Err(e) => {
                self.retry(kind, format!("live segment {sq} itag {itag}: {e}"));
                return;
            }
        };
        self.bw.add(body.len(), ms);
        self.tracks[kind].retries = 0;
        let Some(l) = self.live.as_mut() else { return };
        if let Some(t0) = parts.t0 {
            if l.win.anchor.is_none() {
                l.start_at = Some(t0 + if l.edge { 0.0 } else { (l.from - l.win.offset(sq).unwrap_or(0.0)).max(0.0) });
            }
            l.win.note_anchor(sq, t0);
            if kind == V {
                l.wall = parts.wall_ms.map(|w| (w, t0));
            }
        }
        l.next[kind] = l.next[kind].max(sq + 1);
        if self.tracks[kind].appended != itag {
            if parts.init.is_empty() {
                self.fail(format!("live segment {sq} itag {itag} has no init segment"));
                return;
            }
            let mime = self.format(itag, kind).map(|f| f.mime.clone()).unwrap_or_default();
            let tr = &mut self.tracks[kind];
            if !mime.is_empty() && mime != tr.mime {
                tr.ops.push_back(Op::ChangeType(mime.clone()));
                tr.mime = mime;
            }
            tr.ops.push_back(Op::Append(parts.init));
            tr.appended = itag;
        }
        self.tracks[kind].ops.push_back(Op::Append(parts.media));
        self.flush(kind);
        if kind == V {
            self.abr();
        }
        self.pump(kind);
    }

    /// The start position, applied once the element has metadata (Chrome
    /// does not keep a position set before that). Returns it while pending:
    /// until the playhead holds it, since Chrome can also drop a position set
    /// right as metadata arrives (the playhead stays at 0, and eviction or
    /// fetch decisions taken at 0 would discard the start segments).
    fn live_position(&mut self) -> Option<f64> {
        let at = self.live.as_ref()?.start_at?;
        if bridge::media_get(self.vid, "readyState") < 1.0 {
            return Some(at);
        }
        if (self.time() - at).abs() <= 0.25 {
            if let Some(l) = self.live.as_mut() {
                l.start_at = None;
            }
            return None;
        }
        if bridge::media_get(self.vid, "seeking") != 1.0 {
            bridge::media_set(self.vid, "currentTime", at);
            self.own_seek = Some((at, bridge::now()));
        }
        Some(at)
    }

    /// Live quality change: later sequence numbers come from `itag` (every
    /// segment carries its own init); `now` also replaces what is buffered
    /// after the current segment.
    fn live_switch(&mut self, itag: u32, now: bool) {
        self.last_switch = bridge::now();
        self.tracks[V].rep = itag;
        if now {
            let t = self.time();
            let tr = &mut self.tracks[V];
            if let Some(id) = tr.inflight.take() {
                bridge::abort(id);
                self.fetches.remove(&id);
            }
            tr.drop_appends();
            if let Some(l) = self.live.as_mut() {
                if let Some(sq) = l.win.sq_at(t) {
                    l.next[V] = sq + 1;
                    if let Some(t0) = l.win.t_of(sq + 1) {
                        self.tracks[V].ops.push_back(Op::Remove(t0, f64::INFINITY));
                    }
                }
            }
            self.tracks[V].appended = 0;
            self.eos = false;
        }
        let label = self.ladder.iter().find(|f| f.itag == itag).map(|f| f.label.clone()).unwrap_or_default();
        self.note(format!("live: switch to {itag} {label} ({})", if now { "now" } else { "next segment" }));
        self.quality_label();
        self.pump(V);
    }

    fn live_seeking(&mut self, t: f64) {
        for k in [V, A] {
            let tr = &mut self.tracks[k];
            if let Some(id) = tr.inflight.take() {
                bridge::abort(id);
                self.fetches.remove(&id);
            }
            tr.drop_appends();
            let ranges = pairs(bridge::media_ranges(self.tracks[k].sb));
            if let Some(l) = self.live.as_mut() {
                let n = match sched::range_at(&ranges, t) {
                    Some((_, end)) => l.win.after(end),
                    None => l.win.sq_at(t),
                };
                if let Some(n) = n {
                    l.next[k] = n;
                }
            }
        }
        if let Some(l) = self.live.as_mut() {
            if let Some(edge) = l.win.t_edge() {
                l.edge = live::at_edge(&l.win, edge - t);
            }
        }
        self.eos = false;
        self.pump_all();
    }

    /// Every tick: keep up with the live edge (after a stall or a hidden tab).
    fn live_tick(&mut self) {
        let Some(l) = self.live.as_mut() else { return };
        if self.first_frame.is_none() {
            return;
        }
        if !l.latency_noted {
            if let Some(lat) = live_latency(l, bridge::media_get(self.vid, "currentTime"), bridge::epoch()) {
                l.latency_noted = true;
                let n = format!("live latency {lat:.1} s behind the broadcast ({:.1} s of media behind the newest segment)", l.win.t_edge().map_or(0.0, |e| e - self.time()));
                self.note(n);
            }
        }
        let l = self.live.as_ref().unwrap();
        if self.paused || !l.edge {
            return;
        }
        let t = self.time();
        if let Some(to) = live::catch_up(&l.win, t) {
            let behind = l.win.t_edge().map_or(0.0, |e| e - t);
            self.note(format!("live: {behind:.0} s behind the edge, catching up"));
            self.seek_to(to);
        }
    }

    /// Seekable span for the UI: the video, or the live window.
    fn span(&self) -> Option<(f64, f64)> {
        if let Some(l) = &self.live {
            return Some((l.win.t_first()?, l.win.t_edge()?));
        }
        let d = if self.duration > 0.0 { self.duration } else { bridge::media_get(self.vid, "duration") };
        (d.is_finite() && d > 0.0).then_some((0.0, d))
    }

    fn evict(&mut self, kind: usize, t: f64) {
        let tr = &self.tracks[kind];
        if !tr.updating && !tr.ops.iter().any(|o| matches!(o, Op::Remove(..))) {
            let stale = self.ahead_target() + 30.0;
            let tr = &self.tracks[kind];
            if let Some((a, b)) = sched::evict(&pairs(bridge::media_ranges(tr.sb)), t, sched::BEHIND, stale) {
                self.tracks[kind].ops.push_back(Op::Remove(a, b));
                self.flush(kind);
            }
        }
    }

    // ------------------------------------------------------- quality switch

    fn abr(&mut self) {
        if self.tracks[V].pending.is_some() || (self.live.is_some() && !self.tracks[V].ready) {
            return;
        }
        let Some(i) = self.desired() else { return };
        let want = self.ladder[i].itag;
        let cur = self.tracks[V].rep;
        if want == cur {
            return;
        }
        // Not in the last 10 s: nothing would be gained and the end must stay clean.
        if self.live.is_none() && self.duration > 0.0 && self.time() > self.duration - 10.0 {
            return;
        }
        let cur_i = self.ladder.iter().position(|f| f.itag == cur).unwrap_or(usize::MAX);
        if i < cur_i && bridge::now() - self.last_switch < 8000.0 {
            return; // hold upgrades for a while after a switch
        }
        // Upgrades cut over now (keep ~1 s; while paused the frame on screen too),
        // like YouTube; downgrades keep what is buffered.
        let cur_p = self.format(cur, V).map(|f| f.p).unwrap_or(0);
        let now = self.ladder[i].p > cur_p;
        self.switch_to(want, now);
    }

    /// The next rung below the playing video format (by resolution), even when
    /// the playing format is no longer in the rebuilt ladder.
    fn rung_below(&self) -> Option<(u32, u32)> {
        let cur = self.tracks[V].rep;
        let p = self.format(cur, V).map(|f| f.p)?;
        self.ladder.iter().find(|f| f.p < p).map(|f| (f.itag, f.p))
    }

    /// Mark the playing video rung bad and switch immediately to the next lower one.
    fn step_down(&mut self) -> bool {
        let cur = self.tracks[V].rep;
        let Some((next, _)) = self.rung_below() else { return false };
        self.bad.push(cur);
        self.build_ladder();
        self.tracks[V].retries = 0;
        self.tracks[V].retry_wait = false;
        self.tracks[V].inflight = None;
        self.switch_to(next, true);
        true
    }

    fn switch_to(&mut self, itag: u32, now: bool) {
        // Only the switch the user's choice asked for is instant (`stream.rs`).
        let user = std::mem::take(&mut self.user_switch);
        self.tel_switch(itag);
        if self.live.is_some() {
            self.live_switch(itag, now);
            return;
        }
        self.tracks[V].pending = Some((itag, now));
        self.user_switch = user;
        self.last_switch = bridge::now();
        if self.reps.contains_key(&itag) {
            if now {
                self.apply_switch(V);
            }
        } else if let Some(f) = self.format(itag, V).cloned() {
            self.fetch_head(V, &f, false);
        }
        self.pump(V);
    }

    fn apply_switch(&mut self, kind: usize) {
        let Some((itag, now)) = self.tracks[kind].pending.take() else { return };
        if !self.tracks[kind].ready {
            self.tracks[kind].pending = Some((itag, now));
            return;
        }
        let t = self.time();
        let ranges = pairs(bridge::media_ranges(self.tracks[kind].sb));
        let Some(rep) = self.reps.get(&itag) else { return };
        let old_at = self.reps.get(&self.tracks[kind].rep).and_then(|r| r.segs.get(self.tracks[kind].next)).map(|s| s.t0);
        let label = rep.f.label.clone();
        let paused = self.paused && kind == V && self.first_frame.is_some() && self.live.is_none();
        let user = kind == V && std::mem::take(&mut self.user_switch);
        if now {
            if let Some(id) = self.tracks[kind].inflight.take() {
                self.abort_fetch(id);
            }
            self.tracks[kind].abandon();
        }
        // A chosen quality (or any cut while paused) shows within a fraction of a second: the new
        // quality's segment under the playhead, then a seek to where the playhead is (`stream.rs`).
        if now && kind == V && self.first_frame.is_some() && self.live.is_none() && (paused || user) {
            self.flush_switch(itag, t, paused);
            self.eos = false;
            self.note(format!("switch to {itag} {label} (instant{})", if paused { ", paused" } else { "" }));
            self.quality_label();
            self.flush(kind);
            return;
        }
        let Some(rep) = self.reps.get(&itag) else { return };
        let tr = &mut self.tracks[kind];
        if now {
            let i = sched::switch_cut(&rep.segs, &ranges, t, 1.0);
            if let Some(s) = rep.segs.get(i) {
                tr.ops.push_back(Op::Remove(s.t0, f64::INFINITY));
            }
            tr.next = i;
            tr.appended = 0;
        } else {
            tr.next = old_at.map_or(rep.segs.len(), |at| sched::map_index(&rep.segs, at));
        }
        tr.rep = itag;
        self.eos = false;
        self.note(format!("switch to {itag} {label} ({})", if now { "now" } else { "next segment" }));
        if kind == V {
            self.quality_label();
        }
        self.flush(kind);
    }

    // ---------------------------------------------------------------- events

    /// Media/SourceBuffer/document events; true when the handle is ours.
    pub fn on_media(&mut self, h: u32, ev: &str) -> bool {
        if self.failed {
            return h == self.vid || h == self.ms || self.tracks.iter().any(|t| t.sb == h);
        }
        if h == 0 && ev == "fullscreenchange" {
            self.fs = bridge::is_fullscreen();
            if !self.fs && std::mem::take(&mut self.ch.reveal) {
                self.reveal_chapters(false);
            }
            self.abr();
            self.ui();
            return false;
        }
        if h == self.ms {
            match ev {
                "sourceopen" => {
                    self.tel_ms_open();
                    self.ms_open = true;
                    self.setup_mse();
                }
                // The MediaSource ended or closed without us (the element failed or dropped it):
                // a media error like any other (a replaced MediaSource's events have another handle).
                // (`sourceended` alone can be our own endOfStream's, queued after a seek or a switch
                // reset `eos`: only one that comes with an element error counts.)
                _ if self.sb_err.is_some() => {} // part of the SourceBuffer error being handled
                "sourceended" if bridge::media_get(self.vid, "error.code") > 0.0 => self.media_error("MediaSource ended unexpectedly".into(), None, &bridge::media_str(self.vid, "error.message")),
                "sourceclose" if self.ms_open => self.media_error("MediaSource closed unexpectedly".into(), None, &bridge::media_str(self.vid, "error.message")),
                _ => {}
            }
            return true;
        }
        if let Some(k) = self.tracks.iter().position(|t| t.sb == h && h != 0) {
            match ev {
                "updateend" => {
                    self.tracks[k].updating = false;
                    self.flush(k);
                    if k == V {
                        self.flush_check();
                    }
                    self.pump(k);
                    self.maybe_eos();
                }
                _ => {
                    let why = format!("SourceBuffer {ev} ({})", self.tracks[k].mime);
                    let detail = bridge::media_str(self.vid, "error.message");
                    if detail.is_empty() && self.sb_err.is_none() {
                        // The element's error (with the browser's message) follows in a moment.
                        self.sb_err = Some((why, k));
                        self.timer(Tm::SbErr, SB_ERR_WAIT_MS);
                    } else if self.sb_err.is_none() {
                        self.media_error(why, Some(k), &detail);
                    }
                }
            }
            return true;
        }
        if h != self.vid || h == 0 {
            return false;
        }
        self.tel_media(ev);
        if ev == "resize" || ev == "seeked" {
            return true; // telemetry only
        }
        let now = bridge::now();
        match ev {
            // Every track is initialised: a stream already fully appended may end now.
            "loadedmetadata" => self.maybe_eos(),
            "loadeddata" => {
                if self.first_frame.is_none() {
                    let ms = now - self.nav_start;
                    self.first_frame = Some(ms);
                    self.media_ok(now);
                    // The viewing session goes on: what the failed player had not reported yet.
                    if let Some((on, _)) = self.last_pings.take() {
                        self.ping(on, false);
                    }
                    bridge::mark("ytcFirstFrame");
                    if self.poster.is_some() {
                        // The canvas shows the same picture: it goes once the video has presented it
                        // (or after a second, should no frame be presented, e.g. in a hidden tab).
                        let id = next_id();
                        self.timers.insert(id, Tm::Poster);
                        bridge::media_frame(self.vid, id);
                        self.timer(Tm::Poster, 1000);
                    }
                    self.tel_first_frame();
                    self.note(format!("first frame ({:.0} ms after the streams were chosen)", now - self.streams_at));
                } else if self.media_err.is_some() {
                    // A recovery's fresh MediaSource shows the picture at the position again.
                    self.media_ok(now);
                    if self.poster.is_some() {
                        let id = next_id();
                        self.timers.insert(id, Tm::Poster);
                        bridge::media_frame(self.vid, id);
                        self.timer(Tm::Poster, 1000);
                    }
                }
            }
            "play" => {
                let resumed = self.paused && self.first_frame.is_some();
                self.paused = false;
                self.blocked_autoplay = false;
                if resumed {
                    // Played again after a pause: after a long idle, URLs and media checked first.
                    self.wake_check("play");
                    if self.failed {
                        return true;
                    }
                }
                // Resumed while stuck mid-playback: the schedule goes on from its report point.
                if let Some(s) = self.stuck.as_mut().filter(|s| !s.start) {
                    s.base = s.base.max(now - fail::PLAY.report);
                }
            }
            "playing" => {
                if self.waiting && self.first_frame.is_some() && self.stalls.len() < 50 {
                    self.stalls.push((self.waiting_since - self.nav_start, now - self.waiting_since));
                }
                self.stall_stepped = false;
                self.paused = false;
                self.waiting = false;
                if self.playback_sent == 0.0 {
                    self.send_playback();
                }
            }
            "pause" => {
                self.paused = true;
                self.report_position(now, false);
                self.rec.close();
                if self.playback_sent > 0.0 && self.first_frame.is_some() {
                    self.send_watchtime(false);
                }
            }
            "waiting" => {
                if !self.waiting {
                    self.waiting_since = now;
                }
                self.waiting = true;
            }
            "canplay" => self.waiting = false,
            "seeking" => {
                self.snap = true;
                self.sb_seeking();
                self.on_seeking();
            }
            "timeupdate" => {
                self.sb_check();
                if !self.paused {
                    let t = self.time();
                    if t > 0.0 {
                        self.play_t = t;
                    }
                    self.rec.tick(t, bridge::media_get(self.vid, "playbackRate"));
                    if position_due(self.pos_at, now) {
                        self.report_position(now, false);
                    }
                }
            }
            "ended" => {
                self.paused = true;
                self.report_position(now, true);
                self.out.push(Out::Ended);
                self.rec.close();
                if self.playback_sent > 0.0 {
                    self.send_watchtime(false);
                }
            }
            "error" => {
                let code = bridge::media_get(self.vid, "error.code");
                if !(code > 0.0) {
                    return true; // no error on the element any more: a stale event of a replaced MediaSource
                }
                let msg = bridge::media_str(self.vid, "error.message");
                match self.sb_err.take() {
                    // The SourceBuffer's error, now with the browser's words.
                    Some((why, k)) => self.media_error(why, Some(k), &msg),
                    None => self.media_error(format!("media error {code} {msg}"), None, &msg),
                }
                return true;
            }
            "volumechange" => {
                let muted = bridge::media_get(self.vid, "muted") > 0.0;
                let vol = bridge::media_get(self.vid, "volume");
                vol_ui(if muted { 0.0 } else { vol });
            }
            "ratechange" => {
                // Readouts only: re-rendering the open Speed menu would end a slider drag.
                let r = bridge::media_get(self.vid, "playbackRate");
                self.show_rate(r);
            }
            e if e.starts_with("play:") => {
                // Autoplay refused (NotAllowedError) or interrupted: wait for a click.
                self.paused = true;
                self.blocked_autoplay = e == "play:NotAllowedError";
                self.note(format!("play() rejected: {}", &e[5..]));
            }
            _ => {}
        }
        self.unstuck(now);
        self.ui();
        true
    }

    /// Current (position, duration) once the video has shown a frame.
    pub fn position(&self) -> Option<(f64, f64)> {
        (self.first_frame.is_some() && !self.failed && self.duration > 0.0 && self.live.is_none()).then(|| (self.time(), self.duration))
    }

    /// Playback is going well enough that one background request cannot hurt it: playing for at least
    /// `played_ms` since its first frame, not stuck or failed, and at least `ahead_s` buffered ahead in both
    /// tracks (or up to the end; a live stream: 6 s, about what it keeps behind the edge).
    pub fn healthy(&self, played_ms: f64, ahead_s: f64) -> bool {
        let Some(ff) = self.first_frame else { return false };
        if self.failed || self.paused || self.stuck.is_some() || bridge::now() - (self.nav_start + ff) < played_ms {
            return false;
        }
        let t = self.time();
        let need = if self.live.is_some() { ahead_s.min(6.0) } else { ahead_s.min((self.duration - t).max(0.0)) };
        [V, A].iter().all(|&k| sched::ahead(&pairs(bridge::media_ranges(self.tracks[k].sb)), t) >= need - 0.5)
    }

    fn report_position(&mut self, now: f64, ended: bool) {
        self.pos_at = now;
        if let Some((t, dur)) = self.position() {
            self.out.push(Out::Position { t: if ended { dur } else { t }, dur, ended });
        }
    }

    fn on_seeking(&mut self) {
        let t = self.time();
        if let Some((at, when)) = self.own_seek.take() {
            if (t - at).abs() < 0.25 && bridge::now() - when < 3000.0 {
                return; // our own positioning (live start), nothing to redo
            }
        }
        self.rec.close();
        if self.live.is_some() {
            self.live_seeking(t);
            return;
        }
        self.flush = None;
        for k in [V, A] {
            if let Some(id) = self.tracks[k].inflight.take() {
                self.abort_fetch(id);
            }
            let tr = &mut self.tracks[k];
            tr.abandon();
            if tr.ready {
                if let Some(rep) = self.reps.get(&tr.rep) {
                    tr.next = sched::resume_index(&rep.segs, &pairs(bridge::media_ranges(tr.sb)), t);
                }
            }
            self.seek_pre_take(k);
        }
        self.eos = false;
        self.pump_all();
    }

    pub fn on_timer(&mut self, id: u32) -> bool {
        let Some(t) = self.timers.remove(&id) else { return false };
        if self.failed {
            return true;
        }
        match t {
            Tm::Poster => self.drop_poster(),
            Tm::Proactive(g) => {
                if self.proactive && self.refreshing && g == self.proactive_gen {
                    let ids: Vec<u32> = self.fetches.iter().filter(|(_, f)| matches!(f.req, Req::Vision(_))).map(|(id, _)| *id).collect();
                    for id in ids {
                        self.fetches.remove(&id);
                        bridge::abort(id);
                    }
                    self.exp_off = true;
                    self.note(format!("background URL refresh: no answer in {} s: keeping the URLs, no more proactive refreshes", VISION_TIMEOUT_MS / 1000));
                    self.end_refresh();
                }
            }
            Tm::SbErr => {
                if let Some((why, k)) = self.sb_err.take() {
                    let msg = bridge::media_str(self.vid, "error.message");
                    self.media_error(why, Some(k), &msg);
                }
            }
            Tm::Tick => {
                self.tick();
                if !self.failed {
                    self.timer(Tm::Tick, TICK_MS);
                }
            }
            Tm::VisionTimeout => {
                if self.vision_ms.is_none() && !self.refreshing {
                    let tried = self.recover.take().map(|r| format!(" · tried: {}", r.steps.join(", "))).unwrap_or_default();
                    self.fail(format!("VISIONOS player: no answer in {} s{tried}", VISION_TIMEOUT_MS / 1000));
                }
            }
            Tm::Retry(k) => {
                self.tracks[k].retry_wait = false;
                self.pump(k);
            }
            Tm::VisionHedge => self.vision_hedge_due(),
            Tm::CapsWait => {
                if self.caps_wait {
                    self.caps_wait = false;
                    self.note(format!("decodingInfo: no answer in {CAPS_WAIT_MS} ms, choosing without it"));
                    self.choose_streams_now();
                }
            }
            Tm::Head(itag, kind) => {
                if let Some(f) = self.format(itag, kind).cloned() {
                    if self.tracks[kind].rep == itag || self.tracks[kind].pending.map(|p| p.0) == Some(itag) {
                        self.fetch_head(kind, &f, self.start < 4.0 && self.first_frame.is_none());
                    }
                }
            }
            Tm::Menu(g) => {
                let d = self.mc.timer(g);
                self.menu_do(d);
            }
            Tm::Dwell(g) if g == self.dwell_gen => {
                if let Some(arg) = self.dwell.take() {
                    bridge::class_all(&format!("#{} .dw", ui::MENU), "dw", false);
                    // The row the pointer rests on wins over a value the wheel left pending.
                    self.wheel_q = None;
                    crate::telemetry::act_label("player.dwell", Some("hover"), "watch");
                    self.on_act("p-set", &arg, false);
                }
            }
            Tm::WheelQ(g) if g == self.wheel_gen => {
                if let Some(q) = self.wheel_q.take() {
                    if q != self.prefs.quality {
                        crate::telemetry::act_label("player.quality.wheel", Some("wheel"), "watch");
                        self.on_act("p-set", &format!("q:{q}"), false);
                    } else {
                        self.sync_menu();
                    }
                }
            }
            Tm::Dwell(_) | Tm::WheelQ(_) => {}
            Tm::LivePoll => self.live_poll(),
            Tm::Sb(k) => self.sb_timer(k),
            Tm::SeekPre(g) => self.seek_pre_due(g),
        }
        true
    }

    fn tick(&mut self) {
        // The MediaSource opened but its `sourceopen` never reached us (live 2 Nov 2026: a tab
        // opened in the background and shown 23 min later; the browser reported "open" while
        // the player still waited): continue as if the event had come.
        if !self.ms_open && !self.failed && self.ms != 0 && bridge::media_str(self.ms, "readyState") == "open" {
            self.note("MediaSource open without its sourceopen event: continuing".into());
            self.tel_ms_open();
            self.ms_open = true;
            self.setup_mse();
        }
        self.pump_all();
        let now_ms = bridge::now();
        if now_ms - self.box_at > 1000.0 {
            self.box_at = now_ms;
            let s = bridge::size(ui::BOX);
            if s.len() == 3 {
                self.box_h = s[1].min(s[0] * 9.0 / 16.0) * s[2];
            }
        }
        // Re-check quality every second, also while paused or fully buffered.
        if self.first_frame.is_some() && now_ms - self.abr_at > 1000.0 {
            self.abr_at = now_ms;
            self.abr();
        }
        self.live_tick();
        if self.failed {
            return;
        }
        let now = bridge::now();
        // A tab opened in the background: the browser does not load media until
        // the tab is first shown, so the start timeout only runs while visible
        // (live 1 Nov 2026: ⌘-clicked videos fell back to YouTube unseen).
        if self.first_frame.is_none() && self.streams_at > 0.0 && bridge::hidden() {
            self.streams_at = now;
            self.load_at = now;
            if let Some(s) = self.stuck.as_mut().filter(|s| s.start) {
                s.base = now;
            }
            if !self.hidden_wait {
                self.hidden_wait = true;
                self.note("tab hidden: waiting until it is shown".into());
            }
        }
        // googlevideo has not answered at all 2.5 s after the streams were chosen (live 2 Nov
        // 2026: every index request of a prefetched, cached player response hung): fetch fresh
        // URLs once and ask again before giving up; the start timeout restarts with them.
        // Cached URLs are the usual culprit (live 2 Nov: they hung while fresh ones answered in
        // 185 ms), so a cached start gets fresh URLs much sooner and keeps the same mirror.
        let wait = if self.from_cache { NO_ANSWER_CACHED_MS } else { NO_ANSWER_MS };
        if self.first_frame.is_none() && self.streams_at > 0.0 && !self.got_media && !self.refreshing && self.refreshes == 0 && self.live.is_none() && now - self.streams_at > wait {
            let stuck: Vec<(u32, u32, usize)> = self.fetches.iter().filter_map(|(id, f)| if let Req::Head { itag, kind } = f.req { Some((*id, itag, kind)) } else { None }).collect();
            if !stuck.is_empty() {
                if let Some(cfg) = self.vision_cfg.clone() {
                    for (id, itag, kind) in stuck {
                        bridge::abort(id);
                        self.fetches.remove(&id);
                        self.heads.retain(|&x| x != itag);
                        self.retry_heads.push((itag, kind));
                    }
                    self.refreshes += 1;
                    self.refreshing = true;
                    self.streams_at = now;
                    if !self.from_cache {
                        self.mirror += 1;
                    }
                    self.note(format!("no googlevideo answer in {} s: refreshing the player response{}", wait / 1000.0, if self.from_cache { " (it was cached)" } else { ", next mirror" }));
                    self.from_cache = false;
                    let route = self.route.clone();
                    self.send_vision(&cfg, &route);
                }
            }
        }
        self.thr_tick(now);
        self.nopic_tick(now);
        self.flush_check();
        self.stuck_tick(now);
        if self.failed {
            return;
        }
        // A long stall mid-playback: drop one rung before giving up.
        if self.waiting && !self.paused && self.first_frame.is_some() && !self.stall_stepped && now - self.waiting_since > 3000.0 {
            self.stall_stepped = true;
            if let Some(next) = self.rung_below() {
                self.drop_cap = next.1;
                self.note(format!("stalled 3 s: dropping to {}p", next.1));
                self.switch_to(next.0, true);
            }
        }
        if !self.paused && self.first_frame.is_some() {
            self.watch_drops();
        }
        self.decoder_watch(now);
        if self.failed {
            return;
        }
        self.media_settled(now);
        // Once a minute: URLs within `fail::EXPIRY_MARGIN_S` of their expiry are replaced before
        // they run out (a long video, a tab left open for hours), not on the 403.
        if now - self.exp_at > 60_000.0 {
            self.exp_at = now;
            if self.first_frame.is_some() {
                self.proactive_refresh("the tick", false);
            }
        }
        // Safety net: stuck within 0.5 s of the end with nothing buffered ahead
        // for over a second means the end of stream was missed: signal it, so
        // `ended` (and Up next / the playlist) always follows.
        if self.live.is_none() && !self.eos && self.waiting && !self.paused && self.duration > 0.0 && self.time() > self.duration - 0.5 && now - self.waiting_since > 1000.0 && self.initialised() && bridge::media_str(self.ms, "readyState") == "open" {
            self.eos = true;
            bridge::media_call(self.ms, "endOfStream");
            self.note("stuck at the end: end of stream signalled".into());
        }
        // (A carried session's flush waits for this player's first frame: nothing was played before it.)
        if self.playback_sent > 0.0 && self.first_frame.is_some() {
            let rt = (now - self.playback_sent) / 1000.0;
            if self.schedule.as_mut().is_some_and(|s| s.due(rt)) {
                self.send_watchtime(false);
            }
        }
        self.sb_check();
        self.ui();
    }

    /// Stuck episodes (2 Nov 2026: stalls are not failures): one begins when the first
    /// frame is `fail::START.report` late or playback has buffered `fail::PLAY.report` (status in
    /// the player + a "player-stuck" incident); fresh URLs at the plan's retries; it ends when the
    /// picture comes ("player-recovered") or at the plan's end (the panel: the `Failed` incident).
    fn stuck_tick(&mut self, now: f64) {
        self.unstuck(now);
        let Some(s) = self.stuck.as_ref() else {
            let start = self.first_frame.is_none() && self.load_at > 0.0;
            let play = self.waiting && !self.paused && self.first_frame.is_some();
            let (plan, base) = match (start, play) {
                (true, _) => (&fail::START, self.load_at),
                (_, true) => (&fail::PLAY, self.waiting_since),
                _ => return,
            };
            if fail::step(plan, now - base, false, 0) != Step::Report {
                return;
            }
            let why = if start { format!("no first frame {} s after the streams were chosen", plan.report / 1000.0) } else { format!("stalled for {} s at {}", plan.report / 1000.0, ui::clock(self.time())) };
            self.note(format!("stuck: {why}: retrying in place"));
            self.stuck = Some(Stuck { start, base, at: now, retries: 0, why: why.clone() });
            self.out.push(Out::Incident { kind: "player-stuck", reason: format!("{why}: retrying in place") });
            self.pt.stuck += 1;
            return;
        };
        if !s.start && self.paused {
            return; // paused while stuck: the schedule waits (resuming moves its base on)
        }
        let plan = if s.start { &fail::START } else { &fail::PLAY };
        match fail::step(plan, now - s.base, true, s.retries) {
            Step::Retry => self.stuck_retry(),
            Step::GiveUp => {
                let Some(s) = self.stuck.take() else { return };
                let how = self.retried(s.retries);
                self.fail(if s.start { format!("no first frame {} s after the streams were chosen ({how})", plan.give_up / 1000.0) } else { format!("stalled for {} s at {} ({how})", plan.give_up / 1000.0, ui::clock(self.time())) });
            }
            Step::Wait | Step::Report => {}
        }
    }

    /// The picture came (start) or playback resumed: the episode ends, reported as recovered.
    fn unstuck(&mut self, now: f64) {
        let Some(s) = self.stuck.as_ref() else { return };
        if (s.start && self.first_frame.is_none()) || (!s.start && self.waiting) || self.failed {
            return;
        }
        let Some(s) = self.stuck.take() else { return };
        let reason = format!("{}: recovered after {:.1} s ({})", s.why, (now - s.at) / 1000.0, self.retried(s.retries));
        self.note(format!("stuck → {reason}"));
        self.out.push(Out::Incident { kind: "player-recovered", reason });
    }

    /// "2 retries with fresh URLs, MediaSource rebuilt" (incident reasons).
    fn retried(&self, n: usize) -> String {
        format!("{n} {} with fresh URLs{}", if n == 1 { "retry" } else { "retries" }, if self.mse_rebuilt { ", MediaSource rebuilt" } else { "" })
    }

    fn stuck_retry(&mut self) {
        let Some(s) = self.stuck.as_mut() else { return };
        s.retries += 1;
        let (n, start) = (s.retries, s.start);
        // googlevideo answered but no picture came: a fresh MediaSource, once.
        if start && self.got_media && !self.mse_rebuilt && self.live.is_none() && self.ms_open {
            self.rebuild_mse("data but no picture");
        }
        self.fresh_urls(&format!("stuck retry {n}"));
    }

    /// A fresh player response now (new URLs); requests in flight are dropped and asked again
    /// with the new URLs once it answers (`end_refresh`).
    fn fresh_urls(&mut self, why: &str) {
        let Some(cfg) = self.vision_cfg.clone() else { return };
        if self.vision_ms.is_none() {
            return; // no streams yet: the VISIONOS timeout governs
        }
        let ids: Vec<u32> = self.fetches.iter().filter(|(_, f)| !matches!(f.req, Req::Ping | Req::Web | Req::Captions | Req::Visitor)).map(|(id, _)| *id).collect();
        for id in ids {
            let Some(f) = self.fetches.remove(&id) else { continue };
            bridge::abort(id);
            match f.req {
                Req::Head { itag, kind } => {
                    self.heads.retain(|&x| x != itag);
                    // An index already parsed from its first pieces: the segment's rest is asked again by `pump`.
                    if !self.reps.contains_key(&itag) && !self.retry_heads.contains(&(itag, kind)) {
                        self.retry_heads.push((itag, kind));
                    }
                }
                Req::Seg { kind, .. } | Req::LiveSeg { kind, .. } => {
                    if self.tracks[kind].inflight == Some(id) {
                        self.tracks[kind].inflight = None;
                    }
                }
                Req::Master | Req::Playlist => {
                    if let Some(l) = self.live.as_mut() {
                        l.polling = false;
                    }
                }
                _ => {} // an earlier refresh that never answered
            }
        }
        for t in &mut self.tracks {
            t.retry_wait = true;
        }
        self.seek_pre.retain(|p| p.id.is_none());
        self.from_cache = false;
        self.refreshes += 1;
        self.refreshing = true;
        // Nothing from googlevideo at all: the node is stuck, so the retries also try the next mirror.
        if !self.got_media {
            self.mirror += 1;
        }
        self.note(format!("{why}: fresh player response{}", if self.mirror > 0 { format!(" (mirror {})", self.mirror) } else { String::new() }));
        let route = self.route.clone();
        self.send_vision(&cfg, &route);
    }

    /// A media error (the element's, a SourceBuffer's, an append the browser refused, the
    /// MediaSource closed or ended by itself, a decoder that stopped), at any time: recovered in
    /// place at the current position and play/pause state (`fail::media_retry`: a fresh
    /// MediaSource; fresh URLs and indexes, like Try again; another codec family for a codec fault;
    /// the next format), at most `fail::MEDIA_RETRIES` within `fail::MEDIA_WINDOW_MS`; the panel
    /// only after those. `kind`: the track it came from (None: the element); `detail`: the
    /// browser's own message.
    fn media_error(&mut self, reason: String, kind: Option<usize>, detail: &str) {
        // A SourceBuffer error is waiting for the element's: what follows it (an append refused by
        // the closed MediaSource) is the same failure.
        if self.failed || self.sb_err.is_some() {
            return;
        }
        // The browser's words as they are now (a MediaSource closed before the element's error came).
        let now_msg;
        let detail = if detail.is_empty() {
            now_msg = bridge::media_str(self.vid, "error.message");
            now_msg.as_str()
        } else {
            detail
        };
        if self.live.is_some() || self.vision_ms.is_none() {
            return self.fail(reason);
        }
        let now = bridge::now();
        let mid = self.first_frame.is_some();
        // A paused video in a background tab: nothing plays, and the browser may hold its decoders
        // back until the tab is shown; recovering now could spend the retries for nothing.
        if mid && self.paused && bridge::hidden() {
            if self.deferred.is_none() {
                self.note(format!("{reason} while hidden and paused: recovering when the tab is shown or played"));
                self.deferred = Some((reason, kind, detail.to_string()));
            }
            return;
        }
        self.recov.retain(|r| now - r.0 < fail::MEDIA_WINDOW_MS);
        let done = fail::recent(&self.recov, now);
        let mut fault = fail::fault(kind.map(|k| k == A), detail);
        // No other codec family offered: a codec fault is recovered like any other.
        fault.codec &= self.other_codec().is_some();
        let old = self.urls_left().is_some_and(fail::urls_old);
        let t = self.time();
        let t = if t > 0.0 { t } else { self.play_t };
        let at = if mid { format!(" at {}", ui::clock(t)) } else { " before the first frame".to_string() };
        let Some(retry) = fail::media_retry(&done, self.media_retries, old, fault) else {
            let how = if !done.is_empty() {
                format!(" (after {})", fail::media_retried(&done))
            } else if self.media_retries > 0 {
                format!(" (after {} automatic retries in this video)", self.media_retries)
            } else {
                String::new()
            };
            return self.fail(format!("{reason}{}{how}", if mid { at.as_str() } else { "" }));
        };
        match self.media_err.as_mut() {
            Some(e) => e.3 = 0.0, // the episode goes on
            None => self.media_err = Some((format!("{reason}{at}"), now, self.media_retries, 0.0)),
        }
        self.media_retries += 1;
        self.recov.push((now, retry));
        let why = format!("{reason}{at}{} (automatic retry {} of {}: {})", if fault.codec { " (codec)" } else { "" }, self.recov.len(), fail::MEDIA_RETRIES, retry.label());
        if mid {
            // It plays on where it was, paused or not; the picture on screen stays (a canvas) until
            // the fresh MediaSource shows the same spot.
            self.start = t;
            self.hold_frame();
            self.flush = None;
            self.tracks[V].pending = None;
        }
        // The stuck path's own one-time MediaSource rebuild stays available.
        let rebuilt = self.mse_rebuilt;
        self.rebuild_mse(&why);
        self.mse_rebuilt = rebuilt;
        match retry {
            fail::MediaRetry::FreshSource => {}
            fail::MediaRetry::FreshUrls => self.media_fresh_urls(),
            fail::MediaRetry::NextCodec => {
                if !self.next_codec() {
                    self.next_format(fault.audio);
                }
            }
            fail::MediaRetry::NextFormat => self.next_format(fault.audio),
        }
    }

    /// A recovery's fresh URLs: the indexes and init segments are asked again too, with the new URLs.
    fn media_fresh_urls(&mut self) {
        for k in [V, A] {
            let itag = self.tracks[k].rep;
            self.reps.remove(&itag);
            self.tracks[k].ready = false;
            self.heads.retain(|&x| x != itag);
            if itag != 0 && !self.retry_heads.contains(&(itag, k)) {
                self.retry_heads.push((itag, k));
            }
        }
        self.recovery_refresh("media error");
        if !self.refreshing {
            // No player response to refresh with: the indexes again on the URLs we have.
            self.end_refresh();
        }
    }

    /// Fresh URLs for a recovery or a wake: neither the player's one refresh for an expired URL (a
    /// 403 later in the video still gets it) nor a verdict when it fails (the URLs we have stay).
    fn recovery_refresh(&mut self, why: &str) {
        let refreshes = self.refreshes;
        self.media_refresh = true;
        self.fresh_urls(why);
        self.refreshes = refreshes;
        if !self.refreshing {
            self.media_refresh = false;
        }
    }

    /// The failing track's next format, in the fresh MediaSource (video: the ladder's next; the
    /// same one once more when there is none).
    fn next_format(&mut self, audio: bool) {
        if audio {
            let old = self.tracks[A].rep;
            self.bad.push(old);
            self.build_ladder();
            let lang = self.format(old, A).map(|f| f.audio_id.clone()).unwrap_or_default();
            match self.audio.clone().filter(|f| f.itag != old && f.audio_id == lang) {
                Some(f) => {
                    self.note(format!("audio itag {old} given up: {}", f.itag));
                    self.start_track_on(A, f);
                }
                None => {
                    self.bad.retain(|&x| x != old);
                    self.build_ladder();
                }
            }
            return;
        }
        let old = self.tracks[V].rep;
        self.bad.push(old);
        self.build_ladder();
        match self.desired().map(|i| self.ladder[i].clone()).filter(|f| f.itag != old) {
            Some(f) => {
                self.note(format!("itag {old} given up: {} {}", f.itag, f.label));
                self.start_video_on(f);
            }
            None => {
                // No other format: the same one once more.
                self.bad.retain(|&x| x != old);
                self.build_ladder();
            }
        }
    }

    /// The video codec family in use, when the formats offer the video in another one too.
    fn other_codec(&self) -> Option<Codec> {
        let cur = self.format(self.tracks[V].rep, V).and_then(|f| f.codec)?;
        let others = self.formats.iter().any(|f| f.video && f.codec.is_some_and(|c| c != cur && !self.avoid.contains(&c)) && bridge::mse_ok(&f.mime));
        others.then_some(cur)
    }

    /// A codec fault: the video goes on in another codec family (at the quality the ladder picks,
    /// the chosen one where offered), and this codec is avoided from now on (`Out::CodecFault`).
    fn next_codec(&mut self) -> bool {
        let Some(codec) = self.other_codec() else { return false };
        let old = self.tracks[V].rep;
        self.avoid.push(codec);
        self.build_ladder();
        let Some(f) = self.desired().map(|i| self.ladder[i].clone()).filter(|f| f.codec != Some(codec)) else {
            self.avoid.retain(|c| *c != codec);
            self.build_ladder();
            return false;
        };
        self.note(format!("{} given up after a codec fault (itag {old}): {} {} {}", codec.name(), f.itag, f.label, f.codec.map_or("?", |c| c.name())));
        self.out.push(Out::CodecFault(codec));
        self.start_video_on(f);
        true
    }

    /// Seconds until the URLs in use run out (their own lifetime from when they came), if known.
    fn urls_left(&self) -> Option<f64> {
        (self.urls_epoch > 0.0).then(|| fail::url_left(self.urls_epoch, bridge::epoch(), self.urls_life))
    }

    /// Fresh URLs before the ones in use run out (within `fail::EXPIRY_MARGIN_S`), in the
    /// background: requests in flight go on and playback is not held; the new URLs serve the next
    /// requests. With `drop_stale` (a wake after a pause), requests that never got an answer in
    /// `STALE_MS` are asked again with the new URLs. Never while the bot check blocks VISIONOS,
    /// and once one did not help, no more for this player.
    fn proactive_refresh(&mut self, why: &str, drop_stale: bool) {
        if self.failed || self.live.is_some() || self.vision_ms.is_none() || self.refreshing || self.exp_off || self.no_recover {
            return;
        }
        let Some(left) = self.urls_left().filter(|l| fail::urls_old(*l)) else { return };
        let Some(cfg) = self.vision_cfg.clone() else { return };
        if drop_stale {
            let now = bridge::now();
            let stale: Vec<u32> = self.fetches.iter().filter(|(_, f)| matches!(f.req, Req::Head { .. } | Req::Seg { .. }) && f.ttfb.is_none() && now - f.sent > STALE_MS).map(|(id, _)| *id).collect();
            for id in stale {
                let Some(f) = self.fetches.remove(&id) else { continue };
                bridge::abort(id);
                match f.req {
                    Req::Head { itag, kind } => {
                        self.heads.retain(|&x| x != itag);
                        if !self.reps.contains_key(&itag) && !self.retry_heads.contains(&(itag, kind)) {
                            self.retry_heads.push((itag, kind));
                        }
                        self.tracks[kind].retry_wait = true;
                    }
                    Req::Seg { kind, .. } => {
                        if self.tracks[kind].inflight == Some(id) {
                            self.tracks[kind].inflight = None;
                        }
                        self.tracks[kind].retry_wait = true;
                    }
                    _ => {}
                }
            }
        }
        self.refreshing = true;
        self.media_refresh = true;
        self.proactive = true;
        // Its own timeout (the player's VISIONOS timeout only guards the start).
        self.proactive_gen += 1;
        self.timer(Tm::Proactive(self.proactive_gen), VISION_TIMEOUT_MS);
        self.note(format!("{why}: URLs run out in {:.0} min: fresh player response in the background", left / 60.0));
        let route = self.route.clone();
        self.send_vision(&cfg, &route);
    }

    /// VISIONOS answered with the bot check: the app blocks it for a while; no proactive refreshes.
    fn blocked(&mut self) {
        self.exp_off = true;
        self.out.push(Out::Blocked);
    }

    /// The tab was shown again, or playback resumed after a pause (once the picture was there):
    /// the element and the MediaSource are checked (broken: recovered now), and URLs near their
    /// expiry are replaced before playback needs data. A media error that waited (hidden and
    /// paused) is recovered now.
    fn wake_check(&mut self, why: &str) {
        if self.failed || self.live.is_some() || self.vision_ms.is_none() || self.first_frame.is_none() {
            return;
        }
        if let Some((reason, kind, detail)) = self.deferred.take() {
            return self.media_error(reason, kind, &detail);
        }
        // A recovery still on its way (a fresh MediaSource before its metadata reads "lost its
        // media"): not a second fault.
        let pending = self.media_err.as_ref().is_some_and(|e| e.3 == 0.0) || !self.ms_open;
        if !pending {
            if let Some(state) = self.broken() {
                return self.media_error(format!("{state} (found on {why})"), None, "");
            }
        }
        self.proactive_refresh(why, true);
    }

    /// The tab is visible again (`App::on_show`).
    pub fn on_show(&mut self) {
        self.wake_check("tab shown");
    }

    /// What is wrong with the element or the MediaSource, if anything (checked on wake).
    fn broken(&self) -> Option<String> {
        let code = bridge::media_get(self.vid, "error.code");
        if code > 0.0 {
            return Some(format!("media error {code} {}", bridge::media_str(self.vid, "error.message")));
        }
        if !self.ms_open || !self.sbs_made {
            return None;
        }
        // ("ended" is no fault: a seek after the end of stream leaves it ended until the next append.)
        if bridge::media_str(self.ms, "readyState") == "closed" {
            return Some("MediaSource closed".into());
        }
        (self.first_frame.is_some() && bridge::media_get(self.vid, "readyState") == 0.0).then(|| "the element lost its media (readyState 0)".into())
    }

    /// Playing, with media ahead in both tracks, and the playhead has not moved for
    /// `fail::DECODER_STALL_MS` (the tab visible): the decoder stopped; recovered like a media error.
    /// Also with everything fetched (the end of stream signalled): a short video can freeze fully buffered. A true end
    /// has nothing ahead (`av`/`aa`) or sits within its last second, so it never counts, also when the media is shorter
    /// than YouTube's `lengthSeconds` (its element duration comes from the media once the stream has ended).
    fn decoder_watch(&mut self, now: f64) {
        let t = self.time();
        if self.paused || self.first_frame.is_none() || self.live.is_some() || bridge::hidden() || self.flush.is_some() || !self.sbs_made || (self.duration > 0.0 && t > self.duration - 1.0) || (t - self.still.0).abs() > 0.05 {
            self.still = (t, now);
            return;
        }
        let ahead = |k: usize| sched::ahead(&pairs(bridge::media_ranges(self.tracks[k].sb)), t);
        let (av, aa) = (ahead(V), ahead(A));
        if av < 1.5 || aa < 1.5 {
            // Waiting for data (a network stall): only the time with media buffered counts.
            self.still = (t, now);
            return;
        }
        if fail::decoder_stalled(now - self.still.1, av, aa) {
            self.still = (t, now);
            // (No codec fault: a stall says nothing about the codec.)
            self.media_error(format!("decoder stopped: the playhead stayed at {} for {:.0} s with media buffered", ui::clock(t), fail::DECODER_STALL_MS / 1000.0), Some(V), "-");
        }
    }

    /// The picture on screen stays as a canvas over the `<video>` while a fresh MediaSource gets to
    /// the same spot (no black frame); it goes when the video shows the spot again.
    fn hold_frame(&mut self) {
        if self.poster.is_some() {
            return;
        }
        let id = format!("ytc-rf{}", self.vid);
        if bridge::media_grab(self.vid, &id, warm::CLS, warm::max_width(&bridge::screen_px())) {
            if bridge::held_put(&id, ui::VIDEO) {
                self.poster = Some(id);
            } else {
                bridge::held_drop(&id);
            }
        }
    }

    /// The picture is back after a recovery (it counts once it stays, `media_settled`).
    fn media_ok(&mut self, now: f64) {
        if let Some(e) = self.media_err.as_mut() {
            e.3 = now;
        }
    }

    /// The picture stayed `MEDIA_SETTLE_MS` after recoveries without another error: one
    /// `player-recovered` incident for the episode.
    fn media_settled(&mut self, now: f64) {
        if !self.media_err.as_ref().is_some_and(|e| e.3 > 0.0 && now - e.3 >= MEDIA_SETTLE_MS) {
            return;
        }
        let Some((why, at, n0, ok)) = self.media_err.take() else { return };
        let n = self.media_retries - n0;
        let reason = format!("{why}: recovered after {:.1} s ({n} automatic {})", (ok - at) / 1000.0, if n == 1 { "retry" } else { "retries" });
        self.note(format!("media error → {reason}"));
        self.out.push(Out::Incident { kind: "player-recovered", reason });
    }

    /// A new MediaSource and SourceBuffers on the same `<video>`; the indexes stay, the tracks
    /// start again (init segment first) at the start position (a recovery mid-playback: where it
    /// was), playing only if it was.
    fn rebuild_mse(&mut self, why: &str) {
        self.mse_rebuilt = true;
        self.note(format!("{why}: a fresh MediaSource"));
        for k in [V, A] {
            if let Some(id) = self.tracks[k].inflight.take() {
                bridge::abort(id);
                self.fetches.remove(&id);
            }
            let sb = std::mem::take(&mut self.tracks[k].sb);
            if sb != 0 {
                bridge::media_free(sb);
            }
            let rep = self.tracks[k].rep;
            let fresh = self.reps.get(&rep).map(|r| (r.f.mime.clone(), sched::start_index(&r.segs, self.start)));
            let tr = &mut self.tracks[k];
            tr.ops.clear();
            tr.updating = false;
            tr.appended = 0;
            tr.quota = false;
            tr.asm = None;
            tr.mid = false;
            tr.retries = 0;
            if let Some((mime, next)) = fresh {
                tr.mime = mime;
                if tr.ready {
                    tr.next = next;
                }
            }
        }
        if self.ms != 0 {
            bridge::media_free(self.ms);
        }
        self.ms_open = false;
        self.sbs_made = false;
        self.eos = false;
        self.ms = next_id();
        bridge::media_source(self.ms, self.vid);
        self.apply_rate();
        if self.first_frame.is_some() {
            // The position at once (no 0:00 in the controls while the MediaSource opens).
            bridge::media_set(self.vid, "currentTime", self.start);
        }
        if !self.paused || (self.first_frame.is_none() && !self.start_paused) {
            bridge::media_call(self.vid, "play");
        }
    }

    /// Dropped-frame watchdog: if the current rung drops > 5 % of frames over
    /// the last ~5 s, cap Auto one rung lower for this video.
    fn watch_drops(&mut self) {
        // Not while hidden (browsers throttle decoding), not in the first 3 s
        // after a quality switch, and off in the e2e harness (headless software decode).
        if bridge::local_flag("ytc-test-nowatchdog") || bridge::hidden() || bridge::now() - self.last_switch < 3000.0 {
            self.dq_last = (0.0, 0.0);
            self.dq_window.clear();
            return;
        }
        let q = bridge::playback_quality(self.vid);
        if q.len() != 2 {
            return;
        }
        let (d, t) = (q[0], q[1]);
        let (pd, pt) = self.dq_last;
        self.dq_last = (d, t);
        if t < pt || pt == 0.0 {
            self.dq_window.clear();
            return;
        }
        self.dq_window.push((d - pd, t - pt));
        if self.dq_window.len() > 20 {
            self.dq_window.remove(0);
        }
        let (wd, wt) = self.dq_window.iter().fold((0.0, 0.0), |a, x| (a.0 + x.0, a.1 + x.1));
        if formats::too_many_drops(wd, wt) && self.prefs.quality == 0 {
            if let Some((itag, p)) = self.rung_below() {
                self.drop_cap = p;
                self.dq_window.clear();
                self.note(format!("dropped {wd:.0} of {wt:.0} frames: capping at {p}p"));
                self.switch_to(itag, false);
            }
        }
    }

    // -------------------------------------------------------------- history

    fn ping_now(&self) -> stats::Now {
        let now = bridge::now();
        stats::Now {
            cmt: self.resume_at(),
            rt: (now - self.rt0) / 1000.0,
            lact: now - self.last_input,
            fs: self.fs,
            volume: (bridge::media_get(self.vid, "volume").clamp(0.0, 1.0) * 100.0).round() as u32,
            muted: bridge::media_get(self.vid, "muted") > 0.0,
            playing: !self.paused,
            rate: bridge::media_get(self.vid, "playbackRate"),
        }
    }

    fn session_now(&self) -> Session {
        Session { len: self.duration, fmt: self.tracks[V].rep, afmt: self.tracks[A].rep, ..self.session.clone() }
    }

    fn ping(&mut self, url: String, keepalive: bool) {
        self.pings += 1;
        if self.ping_headers.len() > 2 {
            // TV: the bearer goes with the ping (no cookies in the TV app's WebView).
            let id = next_id();
            bridge::fetch_opts(id, "GET", &url, "", &self.ping_headers, if keepalive { "{\"keepalive\":true}" } else { "{}" });
            self.fetches.insert(id, Inflight::new(Req::Ping, describe(&Req::Ping, &url)));
            return;
        }
        self.get(Req::Ping, &url, if keepalive { "{\"keepalive\":true}" } else { "{}" });
    }

    fn send_playback(&mut self) {
        let Some(t) = self.tracking.clone() else { return };
        self.playback_sent = bridge::now();
        self.schedule = Some(Schedule::new(&t));
        let url = stats::playback_url(&t.playback, &self.session_now(), &self.ping_now());
        self.ping(url, false);
    }

    fn send_watchtime(&mut self, fin: bool) {
        let Some(t) = self.tracking.clone() else { return };
        let n = self.ping_now();
        let segs = self.rec.take(n.cmt);
        let url = stats::watchtime_url(&t.watchtime, &self.session_now(), &n, &segs, fin);
        self.ping(url, fin);
    }

    // ------------------------------------------------------------- captions

    fn cc_list(&self) -> &captions::List {
        if self.cc_web.tracks.is_empty() { &self.cc_vision } else { &self.cc_web }
    }

    fn cc_tracks(&self) -> &[captions::Track] {
        &self.cc_list().tracks
    }

    fn load_captions(&mut self) {
        let hl = self.session.hl.clone();
        // (`cc_lang` is only ever set on TV: an Auto-translate language chosen earlier.)
        let remembered = (!self.prefs.cc_lang.is_empty()).then(|| captions::prefer_lang(self.cc_list(), &self.prefs.cc_lang)).flatten();
        let sel = self.cc_sel.or_else(|| captions::chosen(&self.v, self.cc_list())).or(remembered).or_else(|| captions::prefer(self.cc_list(), &hl));
        let Some(i) = sel else {
            if self.web_done {
                self.toast("No captions");
            }
            return;
        };
        self.cc_sel = Some(i);
        if self.cc_loaded == Some(i) {
            return;
        }
        self.cc_loaded = Some(i);
        self.cues.clear();
        let url = captions::json3_url(&self.cc_tracks()[i]);
        self.get(Req::Captions, &url, "{}");
    }

    fn set_captions(&mut self, on: bool) {
        self.prefs.captions = on;
        self.out.push(Out::Prefs(self.prefs.clone()));
        if on {
            self.load_captions();
        }
        self.ui();
    }

    // ------------------------------------------------------------------- ui

    // ------------------------------------------------------------ chapters

    /// The watch page's chapters (from `next`, else the description's
    /// timecodes). Drawn on the seek bar once the duration is known.
    /// `thumbs`: YouTube's chapter thumbnails, shown in the list only when
    /// there is no storyboard.
    pub fn set_chapters(&mut self, list: Vec<(f64, String)>, thumbs: Vec<Option<String>>) {
        if self.live.is_some() {
            return;
        }
        // The list may have been (re)rendered: its frames, its mark and the compact row are written again
        // and the panel rests compact.
        self.ch.painted = false;
        self.ch.thumbs = thumbs;
        if list != self.ch.list {
            self.ch.starts = list.iter().map(|c| c.0).collect();
            self.ch.list = list;
            self.ch.drawn = -1.0;
        }
        self.ch.cur = None;
        self.ch.hov = false;
        self.ch.foc = false;
        self.ch.pin = false;
        self.ch.shut = false;
        self.ch.open = false;
        bridge::attr(ui::CHD, "class", "ych");
        bridge::class_all(&format!("#{} li.on", ui::CHOL), "on", false);
        self.ui();
    }

    /// Writes every chapter's frame into the list (once per list, after the
    /// first frame so the sheets never compete with the start): the
    /// storyboard cell at the chapter's start, else YouTube's thumbnail.
    fn paint_frames(&mut self, d: f64) {
        self.ch.painted = true;
        let dpr = bridge::screen_px().get(2).copied().unwrap_or(1.0);
        let n = self.ch.list.len();
        for i in 0..n {
            let style = self.chapter_frame(i, d, dpr).or_else(|| self.ch.thumbs.get(i).cloned().flatten().and_then(|u| storyboard::thumb_style(&u)));
            if let Some(st) = style {
                bridge::attr(&format!("{}{i}", ui::CHF), "style", &st);
            }
        }
        if let Some(i) = self.ch.cur.or((n > 0).then_some(0)) {
            self.head_frame(i, d);
        }
    }

    /// The storyboard frame for chapter `i` (inline style), if any.
    fn chapter_frame(&self, i: usize, d: f64, dpr: f64) -> Option<String> {
        let sb = self.story.as_ref()?;
        let start = self.ch.starts.get(i).copied()?;
        let end = self.ch.starts.get(i + 1).copied().unwrap_or(f64::INFINITY).min(if d > 0.0 { d } else { f64::INFINITY });
        sb.frame_in(sb.level_for(ROW_FRAME_W, dpr), start, end, d).map(|c| storyboard::style(&c))
    }

    /// The compact row's frame = chapter `i`'s (same cell, same sheet: cached).
    fn head_frame(&self, i: usize, d: f64) {
        let dpr = bridge::screen_px().get(2).copied().unwrap_or(1.0);
        let style = self.chapter_frame(i, d, dpr).or_else(|| self.ch.thumbs.get(i).cloned().flatten().and_then(|u| storyboard::thumb_style(&u)));
        if let Some(st) = style {
            bridge::attr(ui::CHHF, "style", &st);
        }
    }

    /// The compact row follows the current chapter (title, time, count).
    fn head_text(&self, i: usize) {
        let Some((start, title)) = self.ch.list.get(i) else { return };
        bridge::text(ui::CHHT, title);
        bridge::text(ui::CHHC, &ui::clock(*start));
        bridge::text(ui::CHHN, &format!("Chapter {} of {}", i + 1, self.ch.list.len()));
        bridge::attr(ui::CHH, "aria-label", &format!("Chapters: now {} of {}, {title}", i + 1, self.ch.list.len()));
    }

    /// Opens/closes the chapter panel from its inputs; writes only on a change.
    /// Opening by pointer brings the current chapter's row into the list's view first.
    fn chap_panel(&mut self, how: PanelHow) {
        let c = &self.ch;
        let open = !c.shut && (c.hov || c.foc || c.pin);
        if open == c.open {
            return;
        }
        self.ch.open = open;
        if open && how != PanelHow::Key {
            bridge::scroll_in(ui::CHOL, "li.on");
        }
        let mode = match how {
            PanelHow::Hover if open => " hv",
            PanelHow::Key => " now",
            _ => "",
        };
        bridge::attr(ui::CHD, "class", &format!("ych{}{mode}", if open { " open" } else { "" }));
        bridge::attr(ui::CHH, "aria-expanded", if open { "true" } else { "false" });
    }

    /// Keyboard focus entered/left the chapter panel (`data-focus`). Focus a
    /// click leaves on a row is not a reason to stay open.
    pub fn chap_focus(&mut self, on: bool, kbd: bool) {
        let foc = on && kbd;
        if foc && !self.ch.foc {
            // Tabbing on after Escape opens it again.
            self.ch.shut = false;
        }
        self.ch.foc = foc;
        if !on && !self.ch.hov {
            self.ch.shut = false;
        }
        self.chap_panel(if kbd { PanelHow::Key } else { PanelHow::Ptr });
    }

    /// A pointer press (`inside` = within the chapter panel): a press elsewhere unpins it.
    pub fn chap_press(&mut self, inside: bool) {
        if !inside && self.ch.pin {
            self.ch.pin = false;
            self.chap_panel(PanelHow::Ptr);
        }
    }

    /// The compact row clicked: open (pinned) or close it.
    fn chap_toggle(&mut self, kbd: bool) {
        if self.ch.open {
            self.ch.shut = true;
            self.ch.pin = false;
        } else {
            self.ch.shut = false;
            self.ch.pin = true;
        }
        self.chap_panel(if kbd { PanelHow::Key } else { PanelHow::Ptr });
    }

    /// Per `ui()`: (re)draw the segments when the duration first becomes
    /// known (or changes), then follow the current chapter. Writes only when
    /// something changed: the bar once per video, the label and the list's
    /// mark when a chapter boundary is crossed.
    fn chapter_ui(&mut self, span: Option<(f64, f64)>, t: f64) {
        let marks = self.sb_marks();
        let d = match span {
            Some((_, d)) if self.live.is_none() && (!self.ch.list.is_empty() || !marks.is_empty() || self.sb.drawn) => d,
            _ => return,
        };
        if !self.ch.painted && self.web_done && self.first_frame.is_some() {
            self.paint_frames(d);
        }
        // SponsorBlock ranges arrived (or were turned off): redraw the bar with them, inside each chapter segment.
        let dirty = std::mem::take(&mut self.sb.dirty);
        if (d - self.ch.drawn).abs() > 0.5 || dirty {
            let segs = ui::segments(&self.ch.starts, d);
            self.ch.drawn = d;
            self.ch.segs = segs.len();
            self.ch.grown = None;
            self.sb.drawn = !marks.is_empty();
            bridge::html(ui::SEGS, &ui::bars_html_with(&segs, &|lo, hi| super::sponsor::marks_html(&marks, d, lo, hi)));
            bridge::attr(ui::CHB, "data-n", &segs.len().to_string());
        }
        if self.ch.segs == 0 {
            return;
        }
        let cur = ui::chapter_at(&self.ch.starts, t).filter(|&i| i < self.ch.segs);
        if cur != self.ch.cur {
            // Playback (or a click) crossing into another chapter: the list's highlight springs to the new row
            // and the label's new title rises in; a keyboard jump or the first mark is instant.
            let moved = self.ch.cur.is_some() && !self.kf();
            if let Some(o) = self.ch.cur {
                bridge::attr(&format!("{}{o}", ui::CHI), "class", "");
            }
            if let Some(i) = cur {
                bridge::text(ui::CHL, &self.ch.list[i].1);
                bridge::attr(&format!("{}{i}", ui::CHI), "class", "on");
                self.head_text(i);
                if self.ch.painted {
                    self.head_frame(i, d);
                }
                if moved {
                    bridge::nudge(ui::CHL, 6.0 * mv(), motion::E_OUT, LABEL_MS);
                    // The compact row's new chapter slides in from the side it lies on (later → from below).
                    let dy = if self.ch.cur.is_some_and(|o| o > i) { -6.0 } else { 6.0 };
                    bridge::nudge(ui::CHHX, dy * mv(), motion::E_OUT, LABEL_MS);
                }
            }
            bridge::mark_slide(ui::CHOL, &cur.map(|i| format!("{}{i}", ui::CHI)).unwrap_or_default(), "a", "ychk", moved);
            self.ch.cur = cur;
        }
    }

    /// The pointer over the seek bar (`data-track`, fraction; < 0 = it left).
    pub fn on_track(&mut self, name: &str, fx: f64) {
        if name == "seek" {
            let shown = self.ch.hover.is_some();
            self.ch.hover = (fx >= 0.0).then_some(fx);
            let d = self.duration;
            if self.drag.is_none() && d > 0.0 {
                self.seek_pre_hint(self.ch.hover.map(|f| f * d));
            }
            self.hover_ui(shown);
        } else if name == "chaps" {
            // Real pointer movement only (`data-track`): a page scrolled under a still pointer does not open it.
            let hov = fx >= 0.0;
            if hov != self.ch.hov {
                self.ch.hov = hov;
                if hov {
                    // Coming back in opens it again, whatever folded it before.
                    self.ch.shut = false;
                } else {
                    // Leaving ends a click's pin and an Escape.
                    self.ch.pin = false;
                    if !self.ch.foc {
                        self.ch.shut = false;
                    }
                }
                self.chap_panel(if hov { PanelHow::Hover } else { PanelHow::Ptr });
            }
        }
    }

    /// Tooltip (time, chapter title) and the grown segment under the pointer
    /// (or the drag). Text is written only when it changes.
    /// `shown`: the tooltip was already up (a new chapter title morphs its width instead of jumping).
    fn hover_ui(&mut self, shown: bool) {
        let at = self.drag.or(self.ch.hover);
        let (f, (a, b)) = match (at, self.span()) {
            (Some(f), Some(s)) if s.1 > s.0 => (f.clamp(0.0, 1.0), s),
            _ => {
                self.grow(None);
                return;
            }
        };
        let t = a + f * (b - a);
        bridge::css_var(ui::SEEK, "--h", &format!("{f:.5}"));
        self.tip_frame(t, b);
        bridge::text(ui::TIPT, &if self.live.is_some() { format!("−{}", ui::clock(b - t)) } else { ui::clock(t) });
        let ch = if self.ch.segs > 0 { ui::chapter_at(&self.ch.starts, t).filter(|&i| i < self.ch.segs) } else { None };
        let k = if self.prefs.sb_on { super::sponsor::at(&self.sb.segs, t) } else { None };
        if ch != self.ch.tip || k != self.sb.tip {
            let morph = shown && !bridge::reduced_motion();
            if morph {
                bridge::box_mark(ui::TIP);
            }
            bridge::text(ui::TIPC, ch.map_or("", |i| self.ch.list[i].1.as_str()));
            self.sb_tip(t);
            if morph {
                let (curve, ms) = motion::ui();
                bridge::box_morph(ui::TIP, ui::TIPI, curve, ms, 0.0, 0);
                bridge::nudge(ui::TIPC, 0.0, motion::E_OUT, TIP_FADE_MS);
            }
            self.ch.tip = ch;
        }
        self.grow(ch);
    }

    /// The seek tooltip's storyboard frame at `t` (written only when the frame changes).
    fn tip_frame(&mut self, t: f64, d: f64) {
        let Some(sb) = self.story.as_ref().filter(|_| self.live.is_none()) else { return };
        let dpr = bridge::screen_px().get(2).copied().unwrap_or(1.0);
        let level = sb.level_for(TIP_FRAME_W, dpr);
        let Some(n) = sb.frame_index(level, t, d) else { return };
        if self.ch.tipf == Some((level, n)) {
            return;
        }
        let c = sb.cell(level, n);
        self.ch.tipf = Some((level, n));
        bridge::attr(ui::TIPF, "style", &storyboard::style(&c));
        bridge::attr(ui::TIPF, "data-n", &n.to_string());
    }

    fn grow(&mut self, seg: Option<usize>) {
        if seg != self.ch.grown {
            if let Some(o) = self.ch.grown {
                bridge::attr(&format!("{}{o}", ui::SEG), "class", "pbar");
            }
            if let Some(i) = seg {
                bridge::attr(&format!("{}{i}", ui::SEG), "class", "pbar on");
            }
            self.ch.grown = seg;
        }
    }

    /// The chapter label: open the chapter list under the description, scroll
    /// it into view on the current chapter (YouTube opens its chapters panel).
    /// In fullscreen it leaves fullscreen first.
    fn reveal_chapters(&mut self, kbd: bool) {
        bridge::into_view(ui::CHD);
        self.ch.shut = false;
        self.ch.pin = true;
        self.ch.foc = self.ch.foc || kbd;
        self.chap_panel(if kbd { PanelHow::Key } else { PanelHow::Ptr });
        bridge::scroll_in(ui::CHOL, "li.on");
        if kbd {
            bridge::focus_sel(ui::CHOL, "li.on a");
        }
    }

    fn toast(&mut self, s: &str) {
        self.tel_toast();
        bridge::text(ui::TOAST, s);
        self.toast_until = bridge::now() + 900.0;
        self.toast_ptr = !self.kf();
    }

    fn ui(&mut self) {
        if self.failed {
            return;
        }
        let now = bridge::now();
        let span = self.span();
        let t = match (self.drag, span) {
            (Some(f), Some((a, b))) => a + f * (b - a),
            _ => self.time(),
        };
        let edge = self.live.as_ref().is_some_and(|l| l.edge);
        if let (true, Some((_, e))) = (self.live.is_some(), span) {
            bridge::text(ui::TIME, &if edge && self.drag.is_none() { String::new() } else { format!("−{}", ui::clock(e - t)) });
        } else {
            let d = span.map_or(0.0, |s| s.1);
            if crate::tv::on() {
                // TV: elapsed on the bar's left end, remaining on its right.
                bridge::text(ui::TIME, &ui::clock(t));
                bridge::text(ui::TREM, &format!("−{}", ui::clock(d - t)));
            } else {
                bridge::text(ui::TIME, &format!("{} / {}", ui::clock(t), ui::clock(d)));
            }
        }
        // Playing VOD: the fill glides (CSS, linear, GLIDE_MS) toward where the video will be one
        // glide later, so it moves at real speed between updates; anything else lands exactly.
        let moving = !self.paused && !self.waiting && self.first_frame.is_some() && self.drag.is_none() && self.live.is_none();
        let glide = moving && !std::mem::take(&mut self.snap);
        if let Some((a, d)) = span.filter(|(a, b)| b > a) {
            let buf = pairs(bridge::media_ranges(self.vid));
            let b = sched::range_at(&buf, t).map_or(t, |r| r.1);
            let rate = if glide { bridge::media_get(self.vid, "playbackRate") } else { 1.0 };
            bridge::css_var(ui::SEEK, "--p", &format!("{:.5}", ui::progress(t, a, d, rate, glide)));
            bridge::css_var(ui::SEEK, "--b", &format!("{:.4}", ((b - a) / (d - a)).clamp(0.0, 1.0)));
        }
        self.chapter_ui(span, t);
        let text = if self.prefs.captions { captions::active(&self.cues, t) } else { String::new() };
        if text != self.cc_text {
            if ui::fresh_cue(&self.cc_text, &text) {
                self.cue_flip = !self.cue_flip;
                bridge::attr(ui::CC, "class", if self.cue_flip { "pcc cf" } else { "pcc" });
            }
            bridge::text(ui::CC, &text);
            self.cc_text = text;
        }
        let text = &self.cc_text;
        let mut c = String::from("player own");
        let idle = !self.paused && self.mc.open.is_none() && self.drag.is_none() && self.cc_grab.is_none() && now > self.idle_at;
        for (on, name) in [
            (self.paused, " paused"),
            (self.blocked_autoplay || (self.paused && self.first_frame.is_some() && t < 0.5), " big"),
            (self.waiting && !self.paused || self.first_frame.is_none(), " wait"),
            (self.waiting && !self.paused && self.first_frame.is_some(), " stall"),
            (self.stuck.is_some(), " stuck"),
            (glide, " glide"),
            (idle, " idle"),
            (self.prefs.captions, " cc"),
            (self.prefs.volume < 0.5, " vlo"),
            (now < self.kf_until, " kf"),
            (!text.is_empty(), " cctext"),
            (self.mc.open.is_some(), " menu"),
            (self.cc_grab.is_some(), " ccdrag"),
            (bridge::media_get(self.vid, "muted") > 0.0, " muted"),
            (now < self.toast_until, " toast"),
            (now < self.toast_until && self.toast_ptr, " tp"),
            (self.drag.is_some(), " drag"),
            (self.live.is_some(), " live"),
            (edge, " edge"),
            (self.story.is_some() && self.live.is_none(), " sb"),
            (!self.prefs.sb_on, " sboff"),
        ] {
            if on {
                c.push_str(name);
            }
        }
        if c != self.classes {
            bridge::attr(ui::BOX, "class", &c);
            if !crate::tv::on() { bridge::attr(ui::CCB, "aria-pressed", if self.prefs.captions { "true" } else { "false" }); }
            self.classes = c;
        }
    }

    /// The Quality shortcut's accessible name carries the playing rung.
    fn quality_label(&self) {
        let p = self.ladder.iter().find(|f| f.itag == self.tracks[V].rep).map_or(0, |f| f.p);
        bridge::text(ui::QLBL, &ui::quality_short(p));
        let cur = self.ladder.iter().find(|f| f.itag == self.tracks[V].rep).map(|f| f.label.clone()).unwrap_or_default();
        let label = match (self.prefs.quality, cur.is_empty()) {
            (0, true) => "Quality: Auto".to_string(),
            (0, false) => format!("Quality: Auto ({cur})"),
            (_, _) => format!("Quality: {cur}"),
        };
        bridge::attr(ui::QUAL, "aria-label", &label);
        if self.mc.open == Some("quality") {
            self.sync_menu();
        }
    }

    fn show_rate(&mut self, r: f64) {
        bridge::text(ui::RATEB, &ui::rate_label(r));
        if self.mc.open == Some("speed") {
            bridge::text(ui::RATE, &ui::rate_label(r));
            self.sync_menu();
        }
    }

    fn apply_cc_style(&self) {
        let c = self.prefs.cc_style.unwrap_or(CcStyle::DEFAULT);
        bridge::css_var(ui::BOX, "--cx", &format!("{:.4}", c.x));
        bridge::css_var(ui::BOX, "--cy", &format!("{:.4}", c.y));
        bridge::css_var(ui::BOX, "--cs", &format!("{:.3}", c.s));
        bridge::text(ui::CCV, &format!("{:.0}%", c.s * 100.0));
        bridge::value(ui::CCS, &format!("{:.2}", c.s));
    }

    /// Playable audio tracks for the CC popup's Audio tab (empty = single track).
    fn audio_tracks(&self) -> Vec<(String, String)> {
        formats::audio_tracks(&self.formats, &|m: &str| bridge::mse_ok(m))
    }

    fn render_menu(&mut self) {
        if !crate::tv::on() { bridge::attr(ui::CCB, "aria-expanded", if self.mc.open == Some("cc") { "true" } else { "false" }); }
        let html = match self.mc.open {
            Some("speed") => ui::speed_menu(bridge::media_get(self.vid, "playbackRate")),
            Some("quality") => {
                let cur = self.ladder.iter().find(|f| f.itag == self.tracks[V].rep).map_or(0, |f| f.p);
                let rungs: Vec<ui::Rung> = self.ladder.iter().map(|f| ui::Rung { p: f.p, fps: f.fps, label: f.label.clone() }).collect();
                let codecs: Vec<(&str, &str)> = formats::DEFAULT_ORDER
                    .into_iter()
                    .filter(|c| self.formats.iter().any(|f| f.video && f.codec == Some(*c) && bridge::mse_ok(&f.mime)))
                    .map(|c| (c.key(), c.name()))
                    .collect();
                ui::quality_menu(&rungs, self.shown_quality(), cur, &codecs, &self.prefs.codec)
            }
            // TV: Audio has its own action and panel; the Captions panel holds captions only.
            Some("audio") => {
                let playing = self.audio.as_ref().map(|f| f.audio_id.clone()).unwrap_or_default();
                ui::audio_menu(&self.audio_tracks(), &playing)
            }
            Some(_) => {
                let names: Vec<String> = self.cc_tracks().iter().map(|t| if t.name.is_empty() { t.lang.clone() } else { t.name.clone() }).collect();
                let tv = crate::tv::on();
                let audio = if tv { Vec::new() } else { self.audio_tracks() };
                let playing = self.audio.as_ref().map(|f| f.audio_id.clone()).unwrap_or_default();
                ui::cc_menu(self.cc_audio, &names, self.cc_sel, self.prefs.captions, &audio, &playing, tv, captions::first_translated(self.cc_list()).filter(|_| tv))
            }
            // Closing keeps the content: the panel fades out with it (hidden and inert after).
            None => {
                self.menu_kind = None;
                return;
            }
        };
        let kind = match self.mc.open {
            Some("cc") if self.cc_audio && !self.audio_tracks().is_empty() => "audio",
            Some(m) => m,
            None => return,
        };
        let key = self.mc.src == ui::Src::Key;
        // Moving to another menu (or tab, or a codec that changes the rungs) while the panel shows: the panel
        // morphs from the old menu's size to the new one's and the new content slides in from its side.
        // A first open grows from its shortcut instead (CSS); keyboard and reduced motion swap at once.
        let morph = self.menu_kind.is_some() && !key && !self.kf() && !bridge::reduced_motion();
        let dx = self.menu_kind.map_or(0.0, |o| (menu_order(kind) - menu_order(o)).signum() * MENU_DX);
        // The box it paints now, before `data-m` moves and resizes the panel to the new menu's shortcut.
        if morph {
            bridge::box_mark(ui::MENU);
        }
        bridge::attr(ui::MENU, "data-m", self.mc.open.unwrap_or(""));
        // A keyboard open (Enter on a shortcut) has no motion (client.css `.pmenu[data-k="1"]`).
        bridge::attr(ui::MENU, "data-k", if key { "1" } else { "0" });
        bridge::html(ui::MENUI, &html);
        if self.menu_kind.is_none() {
            self.tel_menu(kind);
        }
        if morph {
            let (curve, ms) = motion::ui();
            bridge::box_morph(ui::MENU, ui::MENUI, curve, ms, dx, MENU_FADE_MS);
        } else {
            bridge::box_morph(ui::MENU, "", "", 0, 0.0, 0);
        }
        self.menu_kind = Some(kind);
        self.menu_marks(false);
        self.keep_selected();
        if key {
            self.hold_at = bridge::now();
            bridge::focus_in(ui::MENU, 0);
            if crate::tv::on() && kind == "quality" {
                // TV: the focus on the current quality (the codec row is not shown there).
                bridge::focus_sel(ui::MENU, ".mql button.on");
            }
            if crate::tv::on() && kind == "audio" {
                bridge::focus_sel(ui::MENU, ".mls button.on");
            }
        }
    }

    /// The open menu's selection marks (`bridge::mark_slide`): one per choice group, on its selected choice
    /// (none when nothing in the group is selected). `slide`: a pointer choice springs it from the old one.
    fn menu_marks(&self, slide: bool) {
        let mark = |scope: &str, arg: Option<String>| bridge::mark_slide(scope, &arg.map(|a| ui::choice_id(&a)).unwrap_or_default(), "", "pmk", slide);
        match self.mc.open {
            Some("speed") => {
                let r = bridge::media_get(self.vid, "playbackRate");
                mark(ui::MPRE, ui::PRESETS.iter().find(|p| (*p - r).abs() < 1e-6).map(|p| format!("rate:{p}")));
            }
            Some("quality") => {
                mark(ui::MQ, Some(format!("q:{}", self.shown_quality())));
                mark(ui::MCO, Some(format!("codec:{}", self.prefs.codec)));
            }
            Some("audio") => mark(ui::MLS, Some(format!("au:{}", self.audio.as_ref().map(|f| f.audio_id.clone()).unwrap_or_default()))),
            Some(_) => {
                let arg = if self.cc_audio && !self.audio_tracks().is_empty() {
                    Some(format!("au:{}", self.audio.as_ref().map(|f| f.audio_id.clone()).unwrap_or_default()))
                } else {
                    self.cc_sel.filter(|_| self.prefs.captions).map(|i| format!("cc:{i}"))
                };
                mark(ui::MLS, arg);
            }
            None => {}
        }
    }

    /// A choice changes the open menu in place (class/attribute toggles, no
    /// re-render): hover, focus and selection motion survive, and nothing flickers.
    fn sync_menu(&self) {
        let pick = |group: &str, arg: Option<String>, class: &str| {
            bridge::class_all(&format!("#{} {group}", ui::MENU), class, false);
            if let Some(a) = arg {
                bridge::class_all(&format!("#{} [data-arg=\"{a}\"]", ui::MENU), class, true);
            }
        };
        match self.mc.open {
            Some("speed") => {
                let r = bridge::media_get(self.vid, "playbackRate");
                pick(".mpre button", ui::PRESETS.iter().find(|p| (*p - r).abs() < 1e-6).map(|p| format!("rate:{p}")), "on");
            }
            Some("quality") => {
                let cur = self.ladder.iter().find(|f| f.itag == self.tracks[V].rep).map_or(0, |f| f.p);
                pick(".mql button", Some(format!("q:{}", self.shown_quality())), "on");
                pick(".mql button", (cur > 0).then(|| format!("q:{cur}")), "cur");
                pick(".mco button", Some(format!("codec:{}", self.prefs.codec)), "on");
            }
            Some(_) => {
                let on = self.prefs.captions;
                pick(".mls [data-arg^=\"cc:\"]", self.cc_sel.filter(|_| on).map(|i| format!("cc:{i}")), "on");
                let playing = self.audio.as_ref().map(|f| f.audio_id.clone()).unwrap_or_default();
                pick(".mls [data-arg^=\"au:\"]", Some(format!("au:{playing}")), "on");
                if crate::tv::on() { bridge::attr(ui::MSW, "aria-checked", if on { "true" } else { "false" }); }
            }
            None => {}
        }
        self.menu_marks(!self.kf());
    }

    /// The current state change comes from the keyboard (no morphs, no slides).
    fn kf(&self) -> bool {
        bridge::now() < self.kf_until
    }

    /// A pointer play/pause on the video leaves the action's icon in the middle, growing and fading out.
    fn pulse(&self, play: bool) {
        bridge::attr(ui::PFX, "class", if play { "pfx on pl" } else { "pfx on pa" });
        bridge::replay(ui::PFX);
    }

    fn menu_do(&mut self, d: ui::Do) {
        if matches!(d, ui::Do::Open | ui::Do::Close) {
            self.cancel_dwell();
        }
        match d {
            ui::Do::Wait(ms) => self.timer(Tm::Menu(self.mc.generation), ms),
            // Class first: a keyboard-opened menu must be visible before its choice is focused.
            ui::Do::Open => {
                self.cc_audio = false;
                self.ui();
                self.render_menu();
            }
            ui::Do::Close => {
                self.ui();
                self.render_menu();
            }
            ui::Do::None => {}
        }
    }

    /// Hover zones from the bridge (`data-hov`): shortcuts `tb`, the panel
    /// `menu` and the CC popup's `tab`s (which switch immediately on hover).
    pub fn on_hov(&mut self, zone: &str, arg: &str, held: bool) {
        if self.failed {
            return;
        }
        let away = !matches!(zone, "pl" | "tb" | "menu" | "tab" | "dw");
        if away != self.away {
            self.away = away;
            // Leaving hides at once (eased out); coming back shows at once. Keys still show the
            // controls for IDLE_MS while the pointer is away; pointer moves elsewhere do not.
            self.idle_at = bridge::now() + if away { -1.0 } else { idle_ms() };
            self.ui();
        }
        let d = self.mc.hover(zone, arg, held);
        if zone == "tab" && self.mc.open == Some("cc") && !held {
            let audio = arg == "audio";
            if audio != self.cc_audio {
                self.cc_audio = audio;
                self.render_menu();
            }
        }
        self.menu_do(d);
        self.dwell_hover(zone, arg, held);
    }

    // ------------------------------------------------------------ zero clicks (2 Nov 2026)

    /// The Quality value the menu shows selected: the wheel's pending one, else the choice.
    fn shown_quality(&self) -> u32 {
        self.wheel_q.unwrap_or(self.prefs.quality)
    }

    /// Whether a Speed/Quality row's value is already the selected one (resting there does nothing).
    fn is_selected(&self, arg: &str) -> bool {
        let (k, v) = arg.split_once(':').unwrap_or((arg, ""));
        match k {
            "rate" => v.parse::<f64>().is_ok_and(|x| (x - bridge::media_get(self.vid, "playbackRate")).abs() < 1e-6),
            "q" => v.parse::<u32>().is_ok_and(|q| q == self.shown_quality()),
            "codec" => v == self.prefs.codec,
            _ => true,
        }
    }

    /// Dwell: entering a Speed/Quality row starts its fill (client.css `.dw`) and a `DWELL_MS` timer;
    /// leaving it (another row, a gap, the panel) cancels both. Rows passed over on the way never select.
    fn dwell_hover(&mut self, zone: &str, arg: &str, held: bool) {
        let target = (zone == "dw" && !held && matches!(self.mc.open, Some("speed" | "quality")) && ui::dwell_row(arg)).then_some(arg);
        if self.dwell.as_deref() == target && target.is_some() {
            return;
        }
        self.cancel_dwell();
        let Some(arg) = target else { return };
        // Right after a wheel step or a keyboard open the row came under the pointer (the list scrolled, the panel
        // appeared), the pointer did not move there: no dwell until it enters another row.
        if self.is_selected(arg) || bridge::now() - self.hold_at < DWELL_HOLD_MS {
            return;
        }
        bridge::class_all(&format!("#{} [data-arg=\"{arg}\"]", ui::MENU), "dw", true);
        self.dwell = Some(arg.to_string());
        self.dwell_gen = self.dwell_gen.wrapping_add(1);
        self.timer(Tm::Dwell(self.dwell_gen), ui::DWELL_MS);
    }

    fn cancel_dwell(&mut self) {
        if self.dwell.take().is_some() {
            self.dwell_gen = self.dwell_gen.wrapping_add(1);
            bridge::class_all(&format!("#{} .dw", ui::MENU), "dw", false);
        }
    }

    /// The open list scrolls (instantly, only itself) so its selected row is in view (short players).
    fn keep_selected(&self) {
        let (list, arg) = match self.mc.open {
            Some("speed") => (ui::MPRE, ui::PRESETS.iter().find(|p| (*p - bridge::media_get(self.vid, "playbackRate")).abs() < 1e-6).map(|p| format!("rate:{p}"))),
            Some("quality") => (ui::MQ, Some(format!("q:{}", self.shown_quality()))),
            _ => return,
        };
        if let Some(a) = arg {
            bridge::scroll_keep(list, &format!("[data-arg=\"{a}\"]"));
        }
    }

    /// Wheel-to-choose (`data-wheel`): over the Speed/Quality shortcut (open or not) or anywhere in their
    /// menu, wheel up = the next value up the list (faster, better), down = down; `rate` (the Speed readout)
    /// steps 0.05. Speed applies per step; Quality shows each step (list mark, toast) at once and switches
    /// the stream once the wheel rests. `dy`/`notches` as `ui::Wheel::feed`.
    pub fn on_wheel(&mut self, name: &str, dy: f64, notches: i32) {
        if self.failed {
            return;
        }
        let now = bridge::now();
        self.hold_at = now;
        let n = self.wheel.feed(name, dy, notches, now);
        if n == 0 {
            return;
        }
        self.last_input = now;
        self.idle_at = now + idle_ms();
        self.kf_until = 0.0;
        // A step overrides the row the pointer happens to rest on.
        self.cancel_dwell();
        let up = n > 0;
        let edge = match name {
            "speed" | "rate" => {
                let r = bridge::media_get(self.vid, "playbackRate");
                let (to, edge) = if name == "rate" { ui::fine_step(r, n) } else { ui::preset_step(r, n) };
                if (to - r).abs() > 1e-6 {
                    self.set_rate(to);
                    self.keep_selected();
                }
                edge
            }
            "quality" => {
                let list = ui::quality_list(&self.ladder.iter().map(|f| f.p).collect::<Vec<_>>());
                if list.len() < 2 {
                    return;
                }
                let cur = self.shown_quality();
                let (to, edge) = ui::quality_step(&list, cur, n);
                if to != cur {
                    self.wheel_q = Some(to);
                    let label = self.ladder.iter().find(|f| f.p == to).map_or_else(|| "Auto".to_string(), |f| f.label.clone());
                    self.toast(&label);
                    if self.mc.open == Some("quality") {
                        self.sync_menu();
                        self.keep_selected();
                    }
                    self.wheel_gen = self.wheel_gen.wrapping_add(1);
                    self.timer(Tm::WheelQ(self.wheel_gen), ui::QUALITY_REST_MS);
                }
                edge
            }
            _ => return,
        };
        if edge && self.wheel.edge() {
            // The end of the list: a small rubber-band nudge of the list (or the closed shortcut's label)
            // toward the wheel, and the value it stays at.
            let open = self.mc.open == Some(if name == "quality" { "quality" } else { "speed" });
            let id = match (name, open) {
                ("rate", true) => ui::RATE,
                ("speed", true) => ui::MPRE,
                ("quality", true) => ui::MQ,
                ("quality", false) => ui::QLBL,
                _ => ui::RATEB,
            };
            if mv() > 0.0 {
                bridge::band(id, if up { -EDGE_NUDGE_PX } else { EDGE_NUDGE_PX }, EDGE_NUDGE_MS);
            }
            if name == "quality" {
                let q = self.shown_quality();
                let label = self.ladder.iter().find(|f| f.p == q).map_or_else(|| "Auto".to_string(), |f| f.label.clone());
                self.toast(&label);
            } else {
                let r = bridge::media_get(self.vid, "playbackRate");
                self.toast(&ui::rate_label(r));
            }
        }
        self.ui();
    }

    /// Switches the audio language (CC popup → Audio): same itag, other
    /// track, so the representation is fetched again and cut in now.
    fn set_audio(&mut self, id: &str) {
        let Some(f) = formats::audio_in(&self.formats, id, &|m: &str| bridge::mse_ok(m)).cloned() else { return };
        // TV: the choice stays this video's, also when it is the track already playing. A dub's language is remembered
        // for the next videos; a video's original means "no dubs", so it clears that (the next video plays its own
        // original, whatever language that is).
        if crate::tv::on() {
            self.audio_sel = id.to_string();
            formats::remember_audio(&self.v, id);
            self.prefs.audio_lang = formats::remembered_lang(id, f.alt_audio);
            self.out.push(Out::Prefs(self.prefs.clone()));
        }
        if self.audio.as_ref().is_some_and(|a| a.itag == f.itag && a.audio_id == f.audio_id) {
            return;
        }
        self.audio_sel = id.to_string();
        self.note(format!("audio track {id} (itag {})", f.itag));
        self.reps.remove(&f.itag);
        self.audio = Some(f.clone());
        self.tracks[A].pending = Some((f.itag, true));
        self.fetch_head(A, &f, false);
    }

    fn toggle(&mut self) {
        if self.paused {
            bridge::media_call(self.vid, "play");
        } else {
            bridge::media_call(self.vid, "pause");
        }
    }

    /// An input outside the player's own controls drives it (a chapter row, a timestamp link): `kbd` keeps
    /// the state change it causes instant.
    pub fn input(&mut self, kbd: bool) {
        self.kf_until = if kbd { bridge::now() + KF_MS } else { 0.0 };
    }

    pub fn seek_to(&mut self, t: f64) {
        let t = match self.span() {
            Some((a, b)) if self.live.is_some() => t.clamp(a, (b - 0.5).max(a)),
            Some((_, d)) => t.clamp(0.0, (d - 0.25).max(0.0)),
            None => t.max(0.0),
        };
        if !self.sbs_made {
            self.start = t;
        }
        bridge::media_set(self.vid, "currentTime", t);
        self.snap = true;
        self.ui();
    }

    fn seek_by(&mut self, dt: f64) {
        let t = self.time() + dt;
        self.seek_to(t);
        self.toast(&format!("{}{} s", if dt > 0.0 { "+" } else { "−" }, dt.abs()));
    }

    /// (A new source resets `playbackRate` to the default: both, so no moment runs at another speed.)
    fn apply_rate(&self) {
        bridge::media_set(self.vid, "defaultPlaybackRate", self.rate);
        bridge::media_set(self.vid, "playbackRate", self.rate);
    }

    fn set_rate(&mut self, r: f64) {
        let r = ui::clamp_rate(r);
        self.rate = r;
        self.apply_rate();
        self.show_rate(r);
        self.toast(&ui::rate_label(r));
    }

    fn set_volume(&mut self, v: f64, muted: bool) {
        self.prefs.volume = v.clamp(0.0, 1.0);
        self.prefs.muted = muted;
        bridge::media_set(self.vid, "volume", self.prefs.volume);
        bridge::media_set(self.vid, "muted", if muted { 1.0 } else { 0.0 });
        bridge::css_var(ui::VOL, "--v", &format!("{:.3}", if muted { 0.0 } else { self.prefs.volume }));
        self.out.push(Out::Prefs(self.prefs.clone()));
        self.ui();
    }

    /// Clicks on `data-act="p-…"` (`kbd` = Enter/Space); true when handled.
    pub fn on_act(&mut self, act: &str, arg: &str, kbd: bool) -> bool {
        self.last_input = bridge::now();
        self.idle_at = self.last_input + idle_ms();
        self.kf_until = if kbd { self.last_input + KF_MS } else { 0.0 };
        let mut keep_focus = false;
        match act {
            "p-toggle" => {
                if self.mc.open.is_some() {
                    let d = self.mc.close();
                    self.menu_do(d);
                } else {
                    if arg == "v" && !kbd {
                        self.pulse(self.paused);
                    }
                    self.toggle();
                }
            }
            "p-mute" => {
                let m = bridge::media_get(self.vid, "muted") > 0.0;
                let v = if self.prefs.volume <= 0.0 { 1.0 } else { self.prefs.volume };
                self.set_volume(v, !m);
                bridge::value(ui::VOL, &format!("{}", if m { v } else { 0.0 }));
            }
            "p-cc" => {
                self.set_captions(!self.prefs.captions);
                if self.mc.open == Some("cc") { self.sync_menu(); }
                keep_focus = kbd;
            }
            "p-menu" => {
                let Some(m) = ui::menu_name(arg) else { return false };
                let d = self.mc.click(m, kbd);
                self.menu_do(d);
                keep_focus = kbd;
            }
            "p-tab" => {
                self.cc_audio = arg == "audio";
                self.render_menu();
                keep_focus = kbd;
            }
            "p-set" => {
                let (k, v) = arg.split_once(':').unwrap_or((arg, ""));
                // A click (or a completed dwell) is the choice: no dwell or wheel value is left to land after it.
                self.cancel_dwell();
                if k == "q" || k == "codec" {
                    self.wheel_q = None;
                }
                match k {
                    "rate" => {
                        let r = bridge::media_get(self.vid, "playbackRate");
                        let r = match v {
                            "+" => r + ui::RATE_STEP,
                            "-" => r - ui::RATE_STEP,
                            v => v.parse::<f64>().unwrap_or(r),
                        };
                        self.set_rate(r);
                    }
                    "q" => {
                        self.prefs.quality = v.parse().unwrap_or(0);
                        self.out.push(Out::Prefs(self.prefs.clone()));
                        if let Some(i) = self.desired() {
                            let itag = self.ladder[i].itag;
                            if itag != self.tracks[V].rep || self.tracks[V].pending.is_some() {
                                // A chosen rung, or Auto going up: show it now (also while paused).
                                let cur_p = self.format(self.tracks[V].rep, V).map(|f| f.p).unwrap_or(0);
                                let up = self.ladder[i].p > cur_p;
                                self.user_switch = true;
                                self.switch_to(itag, self.prefs.quality > 0 || up);
                            }
                        }
                        self.quality_label();
                    }
                    "codec" => {
                        self.prefs.codec = if Codec::from_key(v).is_some() { v.to_string() } else { String::new() };
                        self.out.push(Out::Prefs(self.prefs.clone()));
                        self.build_ladder();
                        if let Some(i) = self.desired() {
                            let itag = self.ladder[i].itag;
                            if itag != self.tracks[V].rep {
                                self.user_switch = true;
                                self.switch_to(itag, true);
                            }
                        }
                    }
                    "cc" => {
                        if v == "toggle" {
                            self.set_captions(!self.prefs.captions);
                        } else if let Ok(i) = v.parse::<usize>() {
                            if i < self.cc_tracks().len() {
                                self.cc_sel = Some(i);
                                captions::remember(&self.v, &self.cc_tracks()[i]);
                                // TV: an Auto-translate language stays for the next videos; a track of the video's
                                // own goes back to the fixed preference. (`set_captions` writes the prefs.)
                                if crate::tv::on() {
                                    self.prefs.cc_lang = self.cc_tracks()[i].tlang.clone().unwrap_or_default();
                                }
                                self.set_captions(true);
                            }
                        }
                    }
                    "au" => self.set_audio(v),
                    _ => {}
                }
                // Menus stay open after a choice; a mouse-pinned one closes when the pointer leaves.
                self.mc.chose(kbd);
                if k == "codec" {
                    self.render_menu();
                } else {
                    self.sync_menu();
                }
                keep_focus = kbd;
            }
            "p-sb" => self.sb_act(arg),
            "p-theater" => {
                self.prefs.theater = !self.prefs.theater;
                self.out.push(Out::Theater(self.prefs.theater));
                self.out.push(Out::Prefs(self.prefs.clone()));
            }
            "p-fs" => bridge::fullscreen(ui::BOX),
            "p-chaps" => {
                self.chap_toggle(kbd);
                keep_focus = kbd;
            }
            "p-chap" => {
                if self.fs {
                    self.ch.reveal = true;
                    bridge::fullscreen(ui::BOX);
                } else {
                    self.reveal_chapters(kbd);
                    keep_focus = kbd;
                }
            }
            "p-live" => {
                if let Some(l) = self.live.as_mut() {
                    l.edge = true;
                    if let Some(e) = l.win.t_edge() {
                        let to = e - live::target_latency(l.win.target);
                        self.seek_to(to);
                    }
                    if self.paused {
                        bridge::media_call(self.vid, "play");
                    }
                }
            }
            _ => return false,
        }
        // (TV: the remote's focus stays on the control it pressed.)
        if !keep_focus && !crate::tv::on() {
            bridge::blur();
        }
        self.ui();
        true
    }

    fn shortcut_id(m: &str) -> &'static str {
        match m {
            "speed" => ui::SPEED,
            "quality" => ui::QUAL,
            "audio" => ui::AUB,
            _ => ui::CCB,
        }
    }

    /// Whether playback is paused (the TV remote's OK toggles it).
    pub fn paused(&self) -> bool {
        self.paused
    }

    /// Keyboard shortcuts (YouTube's): space/k, j/l, ←/→, ↑/↓ (volume), f,
    /// m, c, t, 0–9, < >. In a keyboard-opened menu the arrows move between
    /// its choices and Escape returns to its shortcut.
    pub fn on_key(&mut self, key: &str) -> bool {
        // 0–9 are blocked (2 Nov 2026, the old extension's behaviour): no "jump to 0–90 %",
        // and the key does nothing else either (taken, so the page never acts on it).
        if blocked_key(key) {
            return true;
        }
        // Ctrl/⌘ combinations are ours only for chapter jumps (others must not even wake the controls).
        if key.starts_with("Mod+") && !(self.ch.segs > 0 && matches!(key, "Mod+ArrowLeft" | "Mod+ArrowRight")) {
            return false;
        }
        self.last_input = bridge::now();
        self.idle_at = self.last_input + idle_ms();
        self.kf_until = self.last_input + KF_MS;
        if key == "Escape" && self.ch.open && self.mc.open.is_none() {
            // The chapter panel closes at once; keyboard focus inside goes to its compact row.
            // Focus stays on the compact row but no longer holds the list open (Tab on opens it again).
            let foc = std::mem::take(&mut self.ch.foc);
            self.ch.shut = true;
            self.ch.pin = false;
            self.chap_panel(PanelHow::Key);
            if foc {
                bridge::focus(ui::CHH);
            }
            return true;
        }
        // CC activation toggles captions; arrows on its focused desktop shortcut open options.
        if matches!(key, "ArrowUp" | "ArrowDown") && !crate::tv::on() && bridge::focus_id() == ui::CCB {
            if self.mc.open == Some("cc") && self.mc.src == ui::Src::Key {
                bridge::focus_in(ui::MENU, 0);
            } else {
                let d = self.mc.click("cc", true);
                self.menu_do(d);
            }
            return true;
        }
        if let Some(m) = self.mc.open {
            if key == "Escape" {
                // Closed from the keyboard: at once, without the fade.
                bridge::attr(ui::MENU, "data-k", "1");
                let d = self.mc.close();
                self.menu_do(d);
                bridge::focus(Self::shortcut_id(m));
                return true;
            }
            if self.mc.src == ui::Src::Key {
                let delta = match key {
                    "ArrowDown" | "ArrowRight" => 1,
                    "ArrowUp" | "ArrowLeft" => -1,
                    _ => 0,
                };
                if delta != 0 {
                    bridge::focus_in(ui::MENU, delta);
                    return true;
                }
            }
        }
        match key {
            // YouTube's previous/next chapter (⌘ too: Ctrl+arrows switch Spaces on macOS). Without
            // chapters the keys stay the browser's (⌘+← is Back).
            "Mod+ArrowLeft" | "Mod+ArrowRight" if self.ch.segs > 0 => {
                let next = key == "Mod+ArrowRight";
                if let Some(to) = ui::chapter_step(&self.ch.starts[..self.ch.segs], self.time(), next) {
                    self.seek_to(to);
                    if let Some(i) = ui::chapter_at(&self.ch.starts, to) {
                        let title = self.ch.list[i].1.clone();
                        self.toast(&title);
                    }
                }
            }
            " " | "k" => self.toggle(),
            "j" => self.seek_by(-10.0),
            "l" => self.seek_by(10.0),
            "ArrowLeft" => self.seek_by(-5.0),
            "ArrowRight" => self.seek_by(5.0),
            "ArrowUp" | "ArrowDown" => {
                let v = (self.prefs.volume + if key == "ArrowUp" { 0.05 } else { -0.05 }).clamp(0.0, 1.0);
                self.set_volume(v, false);
                bridge::value(ui::VOL, &format!("{v}"));
                self.toast(&format!("{:.0}%", v * 100.0));
            }
            "f" => bridge::fullscreen(ui::BOX),
            "m" => return self.on_act("p-mute", "", true),
            "c" => {
                self.set_captions(!self.prefs.captions);
                if self.mc.open == Some("cc") {
                    self.sync_menu();
                }
            }
            "t" => return self.on_act("p-theater", "", true),
            "<" | ">" => {
                let r = bridge::media_get(self.vid, "playbackRate");
                self.set_rate(ui::rate_step(r, key == ">"));
            }
            _ => return false,
        }
        self.ui();
        true
    }

    /// `seek` (the progress bar) and `cc` (moving the captions).
    pub fn on_drag(&mut self, name: &str, fx: f64, fy: f64, phase: u32) {
        self.last_input = bridge::now();
        match name {
            "seek" if self.span().is_some() => {
                let Some((a, b)) = self.span() else { return };
                if phase < 2 {
                    self.drag = Some(fx);
                } else {
                    self.drag = None;
                    self.seek_to(a + fx * (b - a));
                }
                if phase == 0 {
                    // Pressing the bar leaves a red ripple where it landed (on pointerdown; the fill is already there).
                    self.kf_until = 0.0;
                    bridge::css_var(ui::SEEK, "--r", &format!("{fx:.5}"));
                    bridge::attr(ui::RIP, "class", "prip on");
                    bridge::replay(ui::RIP);
                }
                self.hover_ui(true);
            }
            "cc" => {
                let c = self.prefs.cc_style.unwrap_or(CcStyle::DEFAULT);
                let (gx, gy) = *self.cc_grab.get_or_insert((fx - c.x, fy - c.y));
                let moved = CcStyle { x: fx - gx, y: fy - gy, s: c.s }.clamped();
                self.prefs.cc_style = Some(moved);
                self.apply_cc_style();
                if phase == 2 {
                    self.cc_grab = None;
                    self.out.push(Out::Prefs(self.prefs.clone()));
                }
            }
            _ => return,
        }
        self.ui();
    }

    /// The wheel over the captions: up = larger, 5 % per notch, 50–200 %, remembered like the range.
    pub fn on_wheel_step(&mut self, name: &str, dir: i32) -> bool {
        if name != "cc" {
            return false;
        }
        if dir == 0 {
            return true;
        }
        self.last_input = bridge::now();
        let c = self.prefs.cc_style.unwrap_or(CcStyle::DEFAULT);
        let s = ui::cc_wheel(c.s, dir);
        if s != c.s {
            self.prefs.cc_style = Some(CcStyle { s, ..c });
            self.apply_cc_style();
            self.out.push(Out::Prefs(self.prefs.clone()));
        }
        true
    }

    pub fn on_input(&mut self, name: &str, value: &str) {
        let Ok(v) = value.parse::<f64>() else { return };
        self.last_input = bridge::now();
        match name {
            "vol" => self.set_volume(if v > 0.0 { v } else { self.prefs.volume }, v <= 0.0),
            "ccs" => {
                let c = self.prefs.cc_style.unwrap_or(CcStyle::DEFAULT);
                self.prefs.cc_style = Some(CcStyle { s: v, ..c }.clamped());
                self.apply_cc_style();
                self.out.push(Out::Prefs(self.prefs.clone()));
            }
            _ => {}
        }
    }

    /// TV: a remote key shows the overlay (as a pointer move does on desktop).
    pub fn wake(&mut self) {
        self.last_input = bridge::now();
        self.idle_at = self.last_input + idle_ms();
        self.ui();
    }

    /// TV: Back hides the overlay at once.
    pub fn rest(&mut self) {
        self.idle_at = bridge::now() - 1.0;
        self.ui();
    }

    /// The overlay is hidden (playing and idle).
    pub fn idle(&self) -> bool {
        self.classes.contains(" idle")
    }

    /// TV scrub: moves the pending seek target `secs` from the current one (or the playing position). The
    /// fill, knob, time and the storyboard frame above the bar follow it at once (shown as a drag);
    /// `tv_scrub_end` seeks there, once, when the remote rests.
    pub fn tv_scrub(&mut self, secs: f64) -> bool {
        let Some((a, b)) = self.span().filter(|(a, b)| b > a) else { return false };
        if self.live.is_some() {
            return false;
        }
        let cur = self.drag.map_or_else(|| self.time(), |f| a + f * (b - a));
        let to = (cur + secs).clamp(a, (b - 1.0).max(a));
        self.drag = Some((to - a) / (b - a));
        // The scrub target's segments load while the remote rests.
        self.seek_pre_hint(Some(to));
        self.wake();
        self.hover_ui(true);
        true
    }

    pub fn tv_scrubbing(&self) -> bool {
        self.drag.is_some() && crate::tv::on()
    }

    /// Ends a TV scrub: seeks to its target.
    pub fn tv_scrub_end(&mut self) {
        let (Some(f), Some((a, b))) = (self.drag.take(), self.span()) else { return };
        self.seek_to(a + f * (b - a));
        self.wake();
        self.hover_ui(false);
    }

    pub fn on_move(&mut self) {
        if self.away {
            return;
        }
        let now = bridge::now();
        let was_idle = self.classes.contains(" idle");
        self.idle_at = now + idle_ms();
        if was_idle {
            self.ui();
        }
    }

    // ------------------------------------------------------------ lifecycle

    /// Where a new player for this video would start (the playing position, or the start).
    pub fn resume_at(&self) -> f64 {
        // (Before the first frame the element knows nothing; one that lost its media reads 0.)
        if self.first_frame.is_none() {
            return self.start;
        }
        let t = self.time();
        if t > 0.0 { t } else { self.play_t.max(self.start) }
    }

    /// Final watchtime ping (keepalive), abort requests, release the media.
    /// Returns the position for a later resume.
    pub fn stop(&mut self) -> f64 {
        self.tel_end(None);
        let t = if self.first_frame.is_some() { self.time() } else { self.start };
        if self.playback_sent > 0.0 && !self.failed {
            if self.first_frame.is_some() {
                self.rec.close();
                self.send_watchtime(true);
            } else if let Some((_, last)) = self.last_pings.take() {
                // Left while a retried player loads: the failed player's position, not this empty one.
                self.ping(last, true);
            }
        }
        self.release();
        t
    }

    fn release(&mut self) {
        for (id, f) in self.fetches.iter() {
            if !matches!(f.req, Req::Ping) {
                bridge::abort(*id);
            }
        }
        self.fetches.retain(|_, f| matches!(f.req, Req::Ping));
        self.timers.clear();
        self.drop_poster();
        if self.vid != 0 {
            bridge::media_call(self.vid, "pause");
            bridge::media_reset(self.vid);
            bridge::media_free(self.vid);
        }
        for h in [self.ms, self.tracks[V].sb, self.tracks[A].sb] {
            if h != 0 {
                bridge::media_free(h);
            }
        }
    }

    /// Gives up: the media is released and the app shows the failure panel (never YouTube's page).
    pub fn fail(&mut self, reason: String) {
        if self.failed {
            return;
        }
        let reason = match self.stuck.take() {
            Some(s) => format!("{reason} (while stuck: {})", s.why),
            None => reason,
        };
        let t = self.resume_at();
        self.tel_end(Some(&reason));
        self.note(format!("FAILED: {reason}"));
        self.el_snap = Some(self.element_state());
        // What comes next decides whether this viewing session ends (`Viewing::last`). Before a first
        // frame this player has nothing to add: the last player's stays.
        if self.playback_sent > 0.0 && self.first_frame.is_some() {
            self.rec.close();
            if let Some(t) = self.tracking.clone() {
                let n = self.ping_now();
                let segs = self.rec.take(n.cmt);
                let s = self.session_now();
                self.last_pings = Some((stats::watchtime_url(&t.watchtime, &s, &n, &segs, false), stats::watchtime_url(&t.watchtime, &s, &n, &segs, true)));
            }
        }
        // The picture on screen outlives this player (`failed_state`): the next one takes it over.
        self.fail_frame = self.poster.take().or_else(|| {
            let id = format!("ytc-rf{}", self.vid);
            (self.vid != 0 && bridge::media_grab(self.vid, &id, warm::CLS, warm::max_width(&bridge::screen_px()))).then_some(id)
        });
        self.release();
        self.failed = true;
        self.out.push(Out::Failed { t, reason });
    }

    /// A failed player's state for the next one: what it takes over (paused: before a first frame,
    /// asked to start paused), whether it had shown a picture, and the held frame's id (the caller
    /// places or drops it).
    pub fn failed_state(&mut self) -> (Carry, bool, Option<String>) {
        let played = self.first_frame.is_some();
        let viewing = (self.playback_sent > 0.0).then(|| Viewing { cpn: self.cpn.clone(), rt0: self.rt0, sent: self.playback_sent, schedule: self.schedule.take(), tracking: self.tracking.clone(), last: self.last_pings.take(), headers: self.ping_headers.clone() });
        let carry = Carry { paused: if played { self.paused } else { self.start_paused }, session: viewing, ..self.carry() };
        (carry, played, self.fail_frame.take())
    }

    /// What a new player started by a button takes over from this running one.
    pub fn carry(&self) -> Carry {
        Carry { paused: false, audio_sel: self.audio_sel.clone(), rate: self.rate, session: None }
    }

    /// Before `attach`: goes on from the last player of this video.
    pub fn take_over(&mut self, c: Carry) {
        self.start_paused = c.paused;
        self.audio_sel = c.audio_sel;
        if c.rate > 0.0 {
            self.rate = c.rate;
        }
        if let Some(s) = c.session {
            self.cpn = s.cpn;
            self.rt0 = s.rt0;
            self.playback_sent = s.sent;
            self.schedule = s.schedule;
            self.tracking = s.tracking;
            self.last_pings = s.last;
        }
    }

    /// An automatic Try again's player says so in its notes (debug overlay, incidents).
    pub fn note_retry(&mut self, what: String) {
        self.note(what);
    }

    /// The `<video>` and MediaSource state (also kept at a fallback, which frees them).
    fn element_state(&self) -> serde_json::Value {
        let v = self.vid;
        serde_json::json!({"readyState": bridge::media_get(v, "readyState"), "networkState": bridge::media_get(v, "networkState"), "paused": bridge::media_get(v, "paused"), "currentTime": bridge::media_get(v, "currentTime"), "error_code": bridge::media_get(v, "error.code"), "error_message": bridge::media_str(v, "error.message"), "videoWidth": bridge::media_get(v, "videoWidth"), "videoHeight": bridge::media_get(v, "videoHeight"), "media_source": bridge::media_str(self.ms, "readyState"), "buffered_video": bridge::media_ranges(self.tracks[V].sb), "buffered_audio": bridge::media_ranges(self.tracks[A].sb),
            "requests_in_flight": self.fetches.values().filter(|f| !matches!(f.req, Req::Ping)).map(|f| format!("{:>7.0} ms  {} (open {:.0} ms)", f.sent - self.nav_start, f.what, bridge::now() - f.sent)).collect::<Vec<_>>()})
    }

    /// Everything needed to investigate a failure, without tokens or cookies.
    pub fn report(&self) -> serde_json::Value {
        let stuck = self.stuck.as_ref().map(|s| serde_json::json!({"why": s.why, "start": s.start, "retries": s.retries, "for_ms": (bridge::now() - s.at).round()}));
        let recoveries = serde_json::json!({"total": self.media_retries, "window": self.recov.iter().map(|r| format!("{:.0} ms {}", r.0 - self.nav_start, r.1.label())).collect::<Vec<_>>(), "avoided_codecs": self.avoid.iter().map(|c| c.name()).collect::<Vec<_>>()});
        let fmt = |f: &Format| serde_json::json!({"itag": f.itag, "mime": f.mime, "label": f.label, "bitrate": f.bitrate, "len": f.len, "index_end": f.index_end, "host": host_of(&f.url)});
        serde_json::json!({
            "video": self.v,
            "start": self.start,
            "time": if self.first_frame.is_some() { self.time() } else { self.start },
            "first_frame_ms": self.first_frame,
            "duration": self.duration,
            "refreshes": self.refreshes,
            "vision_route": self.route.label(),
            "bandwidth_bps": self.bw.bps,
            "bad_itags": self.bad,
            "video_track": {"rep": self.tracks[V].rep, "mime": self.tracks[V].mime, "pending": self.tracks[V].pending.map(|p| p.0), "appended": self.tracks[V].appended, "retries": self.tracks[V].retries},
            "audio_track": {"rep": self.tracks[A].rep, "mime": self.tracks[A].mime, "appended": self.tracks[A].appended, "retries": self.tracks[A].retries},
            "ladder": self.ladder.iter().map(fmt).collect::<Vec<_>>(),
            "audio": self.audio.as_ref().map(fmt),
            "formats_offered": self.formats.len(),
            "live": self.live.as_ref().map(|l| serde_json::json!({"first": l.win.first(), "last": l.win.last(), "target": l.win.target, "ended": l.win.ended, "next": l.next, "edge": l.edge, "anchor": l.win.anchor, "playlist_fails": l.fails})),
            "notes": self.notes,
            "requests": self.reqlog,
            "prefs": {"quality": self.prefs.quality, "speed": bridge::media_get(self.vid, "playbackRate"), "codec": self.prefs.codec},
            "limit_p": self.limit(),
            "drop_cap": self.drop_cap,
            "ahead_target_s": self.ahead_target(),
            "buffered_ahead_s": {"video": sched::ahead(&pairs(bridge::media_ranges(self.tracks[V].sb)), self.time()), "audio": sched::ahead(&pairs(bridge::media_ranges(self.tracks[A].sb)), self.time())},
            "decoding_info": self.caps.iter().map(|(i, c)| format!("{i}:{}{}", if c.0 { "smooth" } else { "choppy" }, if c.1 { "+hw" } else { "" })).collect::<Vec<_>>(),
            "frames": {"dropped": self.dq_last.0, "total": self.dq_last.1},
            "stalls_ms": self.stalls,
            "video_element": self.el_snap.clone().unwrap_or_else(|| self.element_state()),
            "media_source_open": self.ms_open,
            "source_buffers": self.sbs_made,
            "tracks_ready": [self.tracks[V].ready, self.tracks[A].ready],
            "queued_ops": [self.tracks[V].ops.len(), self.tracks[A].ops.len()],
            "updating": [self.tracks[V].updating, self.tracks[A].updating],
            "hidden_now": bridge::hidden(),
            "waited_hidden": self.hidden_wait,
            "autoplay_blocked": self.blocked_autoplay,
            "paused": self.paused,
            "screen": bridge::screen_px(),
            "waiting": self.waiting,
            "stuck": stuck,
            "mse_rebuilt": self.mse_rebuilt,
            "media_recoveries": recoveries,
        })
    }

    /// Debug overlay lines.
    pub fn debug(&self) -> String {
        let mut s = String::new();
        let t = self.time();
        let v = self.reps.get(&self.tracks[V].rep).map(|r| &r.f).or_else(|| self.ladder.iter().find(|f| f.itag == self.tracks[V].rep && self.live.is_some())).map(|f| format!("{} {} {}", f.itag, f.label, f.codec.map_or("?", |c| c.name()))).unwrap_or_else(|| "-".into());
        let a = self.audio.as_ref().map(|f| format!("{} {}", f.itag, f.codec.map_or("?", |c| c.name()))).unwrap_or_else(|| "-".into());
        let ahead = |k: usize| sched::ahead(&pairs(bridge::media_ranges(self.tracks[k].sb)), t);
        s.push_str(&format!(
            "player own (VISIONOS{}) · video {v} · audio {a} · first frame {} · ahead {:.1}/{:.1} s · {} · pings {} · cpn {}\n",
            if self.route == VisionRoute::Anon { String::new() } else { format!(", {}", self.route.label()) },
            self.first_frame.map_or("-".into(), |x| format!("{x:.0} ms")),
            ahead(V),
            ahead(A),
            self.bw.bps.map_or("bw ?".into(), |b| format!("bw {:.1} Mbit/s", b / 1e6)),
            self.pings,
            self.cpn
        ));
        if let Some(l) = &self.live {
            s.push_str(&format!(
                "       live · window sq {}–{} ({:.0} s, target {} s{}) · next v {} a {} · {} · latency {}\n",
                l.win.first().unwrap_or(0),
                l.win.last().unwrap_or(0),
                l.win.span(),
                l.win.target,
                if l.win.ended { ", ended" } else { "" },
                l.next[V],
                l.next[A],
                if l.edge { "at the edge" } else { "behind (DVR)" },
                live_latency(l, t, bridge::epoch()).map_or("?".into(), |x| format!("{x:.1} s"))
            ));
        }
        s.push_str(&self.sb_debug());
        for n in &self.notes {
            s.push_str("       ");
            s.push_str(n);
            s.push('\n');
        }
        esc(&s)
    }
}

/// Seconds between the broadcast (YouTube's ingest wall clock in the
/// segment's `emsg`) and what plays at media time `t`.
fn live_latency(l: &LiveSt, t: f64, epoch_ms: f64) -> Option<f64> {
    let (wall, t0) = l.wall?;
    (t.is_finite() && t >= t0 - 60.0).then(|| (epoch_ms - (wall + (t - t0) * 1000.0)) / 1000.0)
}

/// Host of a URL (googlevideo URLs carry signatures and the IP in the query).
/// The distinct googlevideo stream origins in a player response (`https://rr…googlevideo.com`),
/// in order of appearance, without parsing the JSON.
pub fn googlevideo_origins(body: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(body);
    let mut out: Vec<String> = Vec::new();
    let mut rest = &s[..];
    while let Some(i) = rest.find("https://rr") {
        let tail = &rest[i..];
        let end = tail.find(".googlevideo.com").map(|e| e + ".googlevideo.com".len());
        match end {
            Some(e) if e < 80 && tail[8..e].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') => {
                let o = tail[..e].to_string();
                if !out.contains(&o) {
                    out.push(o);
                }
                rest = &tail[e..];
            }
            _ => rest = &tail[10..],
        }
    }
    out
}

#[cfg(test)]
mod origin_tests {
    #[test]
    fn finds_stream_origins() {
        let b = br#"{"url":"https://rr4---sn-4g5ednde.googlevideo.com/videoplayback?x=1","u2":"https://rr4---sn-4g5ednde.googlevideo.com/v?y","u3":"https://rr2---sn-u14g55-cj.googlevideo.com/v","bad":"https://rr1---x"evil.googlevideo.com"}"#;
        assert_eq!(super::googlevideo_origins(b), vec!["https://rr4---sn-4g5ednde.googlevideo.com", "https://rr2---sn-u14g55-cj.googlevideo.com"]);
    }
}

/// The same googlevideo URL on its `n`-th fallback mirror: YouTube lists mirrors in `mn`
/// (comma-separated node names, the first is the URL's own) and the host is `rr{fvip}---{node}`;
/// its player retries on the next one with `fallback_count`. None when there is no other mirror.
pub fn mirror_url(url: &str, n: usize) -> Option<String> {
    let (base, query) = url.split_once('?')?;
    let host = base.strip_prefix("https://")?.split('/').next()?;
    if !host.ends_with(".googlevideo.com") || !host.starts_with("rr") {
        return None;
    }
    let param = |k: &str| query.split('&').find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('=')));
    let mn: Vec<String> = param("mn")?.replace("%2C", ",").replace("%2c", ",").split(',').filter(|s| s.starts_with("sn-")).map(str::to_string).collect();
    if mn.len() < 2 {
        return None;
    }
    let fvip = param("fvip").filter(|v| v.bytes().all(|b| b.is_ascii_digit()) && !v.is_empty()).unwrap_or("1");
    let node = &mn[n % mn.len()];
    let new_host = format!("rr{fvip}---{node}.googlevideo.com");
    let rest: String = query.split('&').filter(|kv| !kv.starts_with("fallback_count=")).collect::<Vec<_>>().join("&");
    Some(format!("https://{new_host}{}?{rest}&fallback_count={n}", &base["https://".len() + host.len()..]))
}

#[cfg(test)]
mod mirror_tests {
    #[test]
    fn next_mirror_like_youtube() {
        let u = "https://rr4---sn-4g5ednrz.googlevideo.com/videoplayback?expire=1&mn=sn-4g5ednrz%2Csn-4g5e6nsd&fvip=4&itag=396&range=0-100";
        assert_eq!(super::mirror_url(u, 1).unwrap(), "https://rr4---sn-4g5e6nsd.googlevideo.com/videoplayback?expire=1&mn=sn-4g5ednrz%2Csn-4g5e6nsd&fvip=4&itag=396&range=0-100&fallback_count=1");
        assert_eq!(super::mirror_url(u, 2).unwrap(), "https://rr4---sn-4g5ednrz.googlevideo.com/videoplayback?expire=1&mn=sn-4g5ednrz%2Csn-4g5e6nsd&fvip=4&itag=396&range=0-100&fallback_count=2");
        assert_eq!(super::mirror_url("https://rr4---sn-a.googlevideo.com/videoplayback?mn=sn-a&fvip=4", 1), None, "one mirror only");
        assert_eq!(super::mirror_url("https://manifest.googlevideo.com/api?mn=sn-a,sn-b", 1), None);
        assert_eq!(super::mirror_url("https://example.com/x?mn=sn-a,sn-b", 1), None);
    }
}

thread_local! {
    /// decodingInfo answers of this page: `caps_key` → (smooth, power-efficient).
    static CAPS: std::cell::RefCell<HashMap<String, (bool, bool)>> = std::cell::RefCell::new(HashMap::new());
}

/// Warm the connection to a googlevideo host. In the TV app googlevideo is fetched by the shell
/// (its Kotlin proxy adds the CORS header the extension's rule 50 adds), so a `<link rel=preconnect>`
/// would only warm the WebView's own, unused connection: there a tiny request (`/generate_204`, what
/// YouTube's own player warms with) goes through the shell and leaves its connection open for the
/// media requests.
pub fn warm_origin(origin: &str) {
    if crate::tv::on() {
        bridge::fetch_opts(next_id(), "GET", &format!("{origin}/generate_204"), "", "{}", "{\"credentials\":\"omit\"}");
    } else {
        bridge::preconnect(origin);
    }
}

/// decodingInfo questions asked ahead of a player (`warm_caps`) carry ids from here on (the
/// player's own carry the itag).
const WARM_ID: u64 = 1_000_000;

thread_local! {
    static WARM: std::cell::RefCell<HashMap<u64, String>> = std::cell::RefCell::new(HashMap::new());
    static WARM_BOOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A prefetched VISIONOS response (hover, focus, Up next): ask decodingInfo for its video formats
/// now, so starting that video needs no wait for it.
pub fn warm_caps(json: &Value) {
    let mut list = Vec::new();
    WARM.with(|w| {
        let mut w = w.borrow_mut();
        for f in formats::parse(json).iter().filter(|f| f.video) {
            let key = caps_key(f);
            if CAPS.with(|c| c.borrow().contains_key(&key)) || w.values().any(|k| *k == key) || w.len() > 200 {
                continue;
            }
            let i = WARM_ID + next_id() as u64;
            w.insert(i, key);
            list.push(caps_question(i, f));
        }
    });
    if !list.is_empty() {
        bridge::media_caps(&serde_json::Value::Array(list).to_string());
    }
}

/// The usual YouTube video formats per codec, rung and frame rate (AV1 levels as YouTube labels
/// them), asked at boot so a page's first video finds answers under `caps_norm` (its exact
/// formats are still asked, in the background, and win once they answer).
pub fn warm_caps_boot() {
    if WARM_BOOT.with(|w| w.replace(true)) {
        return;
    }
    let mut list = Vec::new();
    let rungs: [(u32, u32, u32); 4] = [(720, 1280, 2_500_000), (1080, 1920, 4_500_000), (1440, 2560, 10_000_000), (2160, 3840, 20_000_000)];
    WARM.with(|w| {
        let mut w = w.borrow_mut();
        for (p, width, bitrate) in rungs {
            for fps in [30u32, 60] {
                let av1 = match (p, fps) {
                    (720, 30) => "05",
                    (720, _) | (1080, 30) => "08",
                    (1080, _) => "09",
                    (1440, _) | (2160, 30) => "12",
                    _ => "13",
                };
                let avc = match (p, fps) {
                    (720, 30) => Some("4D401F"),
                    (720, _) => Some("4D4020"),
                    (1080, 30) => Some("640028"),
                    (1080, _) => Some("64002A"),
                    _ => None,
                };
                let mut mimes = vec![format!("video/mp4; codecs=\"av01.0.{av1}M.08\""), "video/webm; codecs=\"vp9\"".to_string()];
                if let Some(a) = avc {
                    mimes.push(format!("video/mp4; codecs=\"avc1.{a}\""));
                }
                for mime in mimes {
                    if !bridge::mse_ok(&mime) {
                        continue;
                    }
                    let f = Format { mime, width, height: p, fps, p, bitrate: (bitrate as f64 * if fps > 30 { 1.0 } else { 0.66 }) as u64, video: true, ..Default::default() };
                    let key = caps_norm(&f);
                    if CAPS.with(|c| c.borrow().contains_key(&key)) {
                        continue;
                    }
                    let i = WARM_ID + next_id() as u64;
                    w.insert(i, key);
                    list.push(caps_question(i, &f));
                }
            }
        }
    });
    if !list.is_empty() {
        bridge::media_caps(&serde_json::Value::Array(list).to_string());
    }
}

/// decodingInfo answers to `warm_caps` go into the page's cache. True when the answer was one.
pub fn warm_answer(json: &str) -> bool {
    let Some(Value::Array(list)) = json::parse_str(json) else { return false };
    let mut warm = false;
    for r in list {
        let Some(i) = r["i"].as_u64().filter(|i| *i >= WARM_ID) else { continue };
        warm = true;
        let Some(key) = WARM.with(|w| w.borrow_mut().remove(&i)) else { continue };
        let c = if r["ok"].as_bool() == Some(false) { (false, false) } else { (r["s"].as_bool().unwrap_or(true), r["e"].as_bool().unwrap_or(false)) };
        CAPS.with(|m| {
            let mut m = m.borrow_mut();
            if m.len() < 400 {
                m.insert(key, c);
            }
        });
    }
    warm
}

/// What a decodingInfo question depends on.
fn caps_key(f: &Format) -> String {
    format!("{}|{}x{}|{}|{}", f.mime, f.width, f.height, f.fps, caps_tf(f))
}

/// The same question in broad strokes: codec family, rung, 30/60 fps, transfer function (the
/// answer of a typical format of that kind, `warm_caps_boot`; used until the exact one is in).
fn caps_norm(f: &Format) -> String {
    let m = f.mime.as_str();
    let family = if m.contains("av01") { "av01" } else if m.contains("vp9") || m.contains("vp09") { "vp9" } else if m.contains("avc1") { "avc1" } else { m };
    format!("n|{family}|{}|{}|{}", f.p, if f.fps > 30 { 60 } else { 30 }, caps_tf(f))
}

/// A cached answer for `f`: its own, else the broad one.
fn caps_known(f: &Format) -> Option<(bool, bool)> {
    CAPS.with(|c| {
        let c = c.borrow();
        c.get(&caps_key(f)).or_else(|| c.get(&caps_norm(f))).copied()
    })
}

/// HDR formats are asked with their transfer function (and BT.2020): without it a decoder or screen
/// that cannot show HDR answers "smooth" for them (live 3 Nov 2026, Android TV emulator: 1440p60 HDR
/// AV1 chosen, no picture, MediaSource rebuilt), and the SDR rung must win there.
fn caps_tf(f: &Format) -> &'static str {
    match (f.hdr, f.hlg) {
        (false, _) => "",
        (true, true) => "hlg",
        (true, false) => "pq",
    }
}

fn caps_question(i: u64, f: &Format) -> serde_json::Value {
    serde_json::json!({"i": i, "mime": f.mime, "w": f.width.max(1), "h": f.height.max(1), "b": f.bitrate.max(1), "f": f.fps.max(1), "tf": caps_tf(f)})
}

/// The first byte a `&range=a-b` URL asks for (0 when it names no range).
fn range_start(url: &str) -> u64 {
    url.split("&range=").nth(1).and_then(|r| r.split('-').next()).and_then(|a| a.parse().ok()).unwrap_or(0)
}

/// Bytes a `&range=a-b` URL asks for (0 when it names no range).
fn range_len(url: &str) -> f64 {
    let r = url.split("&range=").nth(1).map(|r| r.split('&').next().unwrap_or("")).unwrap_or("");
    match r.split_once('-').map(|(a, b)| (a.parse::<u64>(), b.parse::<u64>())) {
        Some((Ok(a), Ok(b))) if b >= a => (b - a + 1) as f64,
        _ => 0.0,
    }
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split(['/', '?']).next().unwrap_or("").to_string()
}

fn describe(req: &Req, url: &str) -> String {
    let range = url.split("&range=").nth(1).map(|r| r.split('&').next().unwrap_or("")).unwrap_or("");
    match req {
        Req::Head { itag, kind } => format!("{} head itag {itag} {} range {range}", if *kind == V { "video" } else { "audio" }, host_of(url)),
        Req::Seg { kind, idx, itag } => format!("{} seg {idx} itag {itag} {} range {range}", if *kind == V { "video" } else { "audio" }, host_of(url)),
        Req::Captions => "captions json3".into(),
        Req::Ping => format!("stats ping {}", url.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("")),
        Req::Vision(r) if *r == VisionRoute::Anon => "player VISIONOS".into(),
        Req::Vision(r) => format!("player VISIONOS ({})", r.label()),
        Req::Visitor => "visitor_id".into(),
        Req::Web => "player WEB".into(),
        Req::Master => format!("live master {}", host_of(url)),
        Req::Playlist => format!("live playlist {}", host_of(url)),
        Req::LiveSeg { kind, sq, itag } => format!("{} live sq {sq} itag {itag} {}", if *kind == V { "video" } else { "audio" }, host_of(url)),
        // The bucket only (the URL carries nothing else about the video).
        Req::Sponsor => format!("sponsorblock bucket {}", url.split('?').next().unwrap_or("").rsplit('/').next().unwrap_or("")),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn position_reports_every_ten_seconds() {
        assert!(!super::position_due(1000.0, 1000.0 + super::POSITION_EVERY_MS - 1.0));
        assert!(super::position_due(1000.0, 1000.0 + super::POSITION_EVERY_MS));
    }
}
