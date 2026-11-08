//! TV mode: the Google TV / Android TV app (`tv/` at the repository root) runs this same client in
//! its WebView and sets the platform flag (`YTC_PLATFORM = "tv"`, read once at boot through
//! `bridge::platform`). Everything here is inert on the desktop extension.
//!
//! - **Remote control**: D-pad spatial navigation (`pick`, pure and unit-tested), OK = Enter,
//!   Back (the shell calls `on_back`), media keys.
//! - **Account**: Google blocks password sign-in inside WebViews, so the TV app signs in like
//!   YouTube's own TV app: the OAuth device flow of the TV client (the shell holds the refresh
//!   token in Android Keystore-backed storage; the page only ever sees short-lived access tokens).
//!   Account calls go out as YouTube's own TV client does them: `TVHTML5` with `Authorization:
//!   Bearer` (tokens of the TV OAuth client are meant for it; WEB + bearer is reported refused since
//!   late 2024 — not verified here: with an invalid token every client answers 401, so only a real
//!   sign-in tells). The shared parser reads the TV response shapes (`tileRenderer`,
//!   `tvBrowseRenderer`, ...).
//!   Anonymous calls (search, watch page, channel, comments, VISIONOS streams) stay as on desktop.

use crate::config::{Auth, Config, ORIGIN};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};

pub mod keyboard;

/// YouTube's TV client (`/tv`, read 2 Nov 2026: `INNERTUBE_CLIENT_NAME` TVHTML5, version
/// `7.20261027.15.00`). Kept as data so the version can be bumped.
pub const TV_NAME: &str = "TVHTML5";
pub const TV_ID: &str = "7";
pub const TV_VERSION: &str = "7.20261027.15.00";
/// sessionStorage (this WebView tab only) mirror of the current access token: a reload sends its
/// first account request without waiting for the shell. Never chrome.storage / localStorage.
pub const SESSION_KEY: &str = "ytc:tvauth";
/// Ask the shell for a fresh access token this long before the current one expires.
pub const REFRESH_BEFORE_MS: f64 = 10.0 * 60.0 * 1000.0;

thread_local! {
    static ON: Cell<bool> = const { Cell::new(false) };
    /// (access token, expiry epoch ms)
    static BEARER: RefCell<Option<(String, f64)>> = const { RefCell::new(None) };
}

/// Called once at boot (before anything renders).
pub fn set_on(on: bool) {
    ON.with(|c| c.set(on));
}

/// The TV app is running this client.
pub fn on() -> bool {
    ON.with(Cell::get)
}

pub fn set_bearer(token: Option<(String, f64)>) {
    BEARER.with(|b| *b.borrow_mut() = token.filter(|(t, _)| valid_token(t)));
}

/// The current access token while it is valid at `now_ms`.
pub fn bearer_at(now_ms: f64) -> Option<String> {
    BEARER.with(|b| b.borrow().as_ref().filter(|(_, exp)| *exp > now_ms).map(|(t, _)| t.clone()))
}

/// Expiry of the token held (0 = none).
pub fn bearer_exp() -> f64 {
    BEARER.with(|b| b.borrow().as_ref().map_or(0.0, |x| x.1))
}

/// An OAuth access token as Google issues them (`ya29.` + URL-safe characters); anything else is refused.
pub fn valid_token(t: &str) -> bool {
    (20..=4096).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~+/=".contains(&b))
}

/// The shell's `auth_token` answer: `{"token": "...", "exp": epoch_ms}` or `{"token": null}`.
pub fn parse_token(json: &str) -> Option<(String, f64)> {
    let v = crate::json::parse_str(json)?;
    let t = v.get("token")?.as_str()?;
    let exp = v.get("exp")?.as_f64()?;
    valid_token(t).then(|| (t.to_string(), exp))
}

pub fn token_json(t: &str, exp: f64) -> String {
    json!({"token": t, "exp": exp}).to_string()
}

/// The account of the TV app: bearer when signed in. Cookies are never used on TV (Google's sign-in
/// does not run in the WebView, so there are none worth reading).
pub fn auth_at(now_ms: f64) -> Auth {
    Auth { bearer: bearer_at(now_ms), ..Auth::default() }
}

/// The account for requests made now: on TV the bearer, elsewhere the browser's cookies.
pub fn auth() -> Auth {
    if on() { auth_at(crate::bridge::epoch()) } else { Auth::from_cookies(&crate::bridge::cookie()) }
}

// ------------------------------------------------------------------ requests

/// The TVHTML5 `context`.
pub fn context(hl: &str) -> Value {
    json!({"client": {"clientName": TV_NAME, "clientVersion": TV_VERSION, "hl": hl, "platform": "TV"}, "user": {"lockedSafetyMode": false}})
}

/// Turns a WEB InnerTube body into the same request from the TV client (only `context` changes;
/// WEB-only fields such as `playbackContext.contentPlaybackContext.currentUrl` are harmless).
pub fn tv_body(web_body: &str) -> String {
    let Ok(mut v) = serde_json::from_str::<Value>(web_body) else { return web_body.to_string() };
    let hl = v.pointer("/context/client/hl").and_then(Value::as_str).unwrap_or("en").to_string();
    v["context"] = context(&hl);
    v.to_string()
}

/// Headers for a TV account request.
pub fn headers(token: &str) -> String {
    json!({"Content-Type": "application/json", "X-Youtube-Client-Name": TV_ID, "X-Youtube-Client-Version": TV_VERSION,
        "X-Origin": ORIGIN, "Authorization": format!("Bearer {token}")})
    .to_string()
}

/// Body and headers for an account request: TVHTML5 + bearer when `auth` carries a token, else the
/// WEB request unchanged.
pub fn finish(cfg: &Config, auth: &Auth, web_body: String, ts_secs: u64) -> (String, String) {
    match &auth.bearer {
        Some(t) => (tv_body(&web_body), headers(t)),
        None => (web_body, crate::api::headers(cfg, auth, ts_secs)),
    }
}

/// Browse pages that belong to the account (sent with the bearer on TV): the feeds, the library and
/// the account's playlists (`VL…`: Watch later, Liked, own private playlists).
pub fn account_browse(id: &str) -> bool {
    matches!(id, "FEwhat_to_watch" | "FEsubscriptions" | "FEhistory" | "FEplaylist_aggregation" | "FElibrary") || id.starts_with("VL")
}

/// Authorization header for history pings (`/api/stats/...`) of a TV session.
pub fn ping_headers(auth: &Auth) -> String {
    match &auth.bearer {
        Some(t) => json!({"Authorization": format!("Bearer {t}")}).to_string(),
        None => "{}".into(),
    }
}

// ------------------------------------------------------- spatial navigation

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

pub fn dir(key: &str) -> Option<Dir> {
    Some(match key {
        "ArrowUp" => Dir::Up,
        "ArrowDown" => Dir::Down,
        "ArrowLeft" => Dir::Left,
        "ArrowRight" => Dir::Right,
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    fn r(&self) -> f64 {
        self.x + self.w
    }
    fn b(&self) -> f64 {
        self.y + self.h
    }
}


/// Gap between two intervals (0 when they overlap).
fn gap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    (b0 - a1).max(a0 - b1).max(0.0)
}

/// Overlap of two intervals as a fraction of the smaller one.
fn overlap(a0: f64, a1: f64, b0: f64, b1: f64) -> f64 {
    let o = (a1.min(b1) - a0.max(b0)).max(0.0);
    let m = (a1 - a0).min(b1 - b0);
    if m > 0.0 { o / m } else { 0.0 }
}

/// The element the remote moves to from `cur` in `dir` (index into `rects`), like a TV launcher:
/// only candidates that lie beyond `cur` in that direction; the nearest along the axis wins,
/// with sideways distance weighed three times and items that overlap `cur` sideways (the same row
/// or column) preferred, so ↓ from a card lands on the card below, not a closer one diagonally.
pub fn pick(cur: Rect, rects: &[Rect], d: Dir, skip: Option<usize>) -> Option<usize> {
    const EPS: f64 = 1.0;
    let mut best: Option<(f64, usize)> = None;
    for (i, c) in rects.iter().enumerate() {
        if Some(i) == skip || c.w <= 0.0 || c.h <= 0.0 {
            continue;
        }
        let (ahead, along, side, ov) = match d {
            Dir::Right => (c.x >= cur.r() - EPS || (c.x > cur.x + EPS && c.r() > cur.r() + EPS), (c.x - cur.r()).max(0.0), gap(cur.y, cur.b(), c.y, c.b()), overlap(cur.y, cur.b(), c.y, c.b())),
            Dir::Left => (c.r() <= cur.x + EPS || (c.r() < cur.r() - EPS && c.x < cur.x - EPS), (cur.x - c.r()).max(0.0), gap(cur.y, cur.b(), c.y, c.b()), overlap(cur.y, cur.b(), c.y, c.b())),
            Dir::Down => (c.y >= cur.b() - EPS || (c.y > cur.y + EPS && c.b() > cur.b() + EPS), (c.y - cur.b()).max(0.0), gap(cur.x, cur.r(), c.x, c.r()), overlap(cur.x, cur.r(), c.x, c.r())),
            Dir::Up => (c.b() <= cur.y + EPS || (c.b() < cur.b() - EPS && c.y < cur.y - EPS), (cur.y - c.b()).max(0.0), gap(cur.x, cur.r(), c.x, c.r()), overlap(cur.x, cur.r(), c.x, c.r())),
        };
        if !ahead {
            continue;
        }
        // Center offset across the axis breaks ties between items of the same row/column.
        let center = match d {
            Dir::Left | Dir::Right => ((c.y + c.h / 2.0) - (cur.y + cur.h / 2.0)).abs(),
            Dir::Up | Dir::Down => ((c.x + c.w / 2.0) - (cur.x + cur.w / 2.0)).abs(),
        };
        let score = along + side * 3.0 + center * 0.05 - if ov > 0.0 { 40.0 * ov } else { 0.0 } + if ov == 0.0 { 200.0 } else { 0.0 };
        if best.is_none_or(|(s, _)| score < s) {
            best = Some((score, i));
        }
    }
    best.map(|b| b.1)
}

/// The window scroll that puts grid card `i` where the grid's first row sits, so the focused row keeps one
/// vertical place while the remote walks down and up (`rects` in viewport coordinates, `marks` = 1 for the
/// grid's cards; the focused card `cur` is left out: it is drawn scaled). Equals target − first row, which
/// does not depend on the current scroll.
pub fn row_scroll(rects: &[Rect], marks: &[u8], cur: usize, i: usize) -> f64 {
    let first = rects.iter().zip(marks).enumerate().filter(|(k, (_, m))| **m == 1 && *k != cur).map(|(_, (r, _))| r.y).fold(f64::INFINITY, f64::min);
    if !first.is_finite() {
        return -1.0;
    }
    (rects[i].y - first).max(0.0)
}

/// What a key means on the remote.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Remote {
    PlayPause,
    Play,
    Pause,
    Rewind,
    Forward,
    Next,
    Prev,
    Stop,
}

/// Media keys as the WebView reports them (`KeyboardEvent.key`); the shell forwards the ones the
/// WebView keeps for itself under the same names.
pub fn remote(key: &str) -> Option<Remote> {
    Some(match key {
        "MediaPlayPause" => Remote::PlayPause,
        "MediaPlay" => Remote::Play,
        "MediaPause" => Remote::Pause,
        "MediaRewind" | "MediaTrackPreviousSeek" => Remote::Rewind,
        "MediaFastForward" => Remote::Forward,
        "MediaTrackNext" => Remote::Next,
        "MediaTrackPrevious" => Remote::Prev,
        "MediaStop" => Remote::Stop,
        _ => return None,
    })
}

/// The player key a remote key stands for (YouTube's own keys, which the player handles).
pub fn remote_player_key(r: Remote, paused: bool) -> Option<&'static str> {
    match r {
        Remote::PlayPause => Some("k"),
        Remote::Play => paused.then_some("k"),
        Remote::Pause => (!paused).then_some("k"),
        Remote::Rewind => Some("j"),
        Remote::Forward => Some("l"),
        Remote::Stop => (!paused).then_some("k"),
        Remote::Next | Remote::Prev => None,
    }
}

/// The session's trail of videos: a video opened goes on the end (not twice in a row, bounded).
pub fn trail_push<T: PartialEq>(trail: &mut Vec<T>, r: T, max: usize) {
    if trail.last() != Some(&r) {
        trail.push(r);
    }
    if trail.len() > max {
        trail.remove(0);
    }
}

/// The remote's Previous: leaves the current video and returns the one before it, which opening puts back on the
/// trail's end. Nothing before it: the trail stays.
pub fn trail_back<T>(trail: &mut Vec<T>) -> Option<T> {
    if trail.len() < 2 {
        return None;
    }
    trail.pop();
    trail.pop()
}

// ------------------------------------------------------------- updates (About & updates)

/// The shell's `shell_info` / `shell_check` answer (tv/shell/src-tauri/src/shell.rs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShellInfo {
    /// The app (shell) version.
    pub version: String,
    /// The client bundle the shell holds now (a newer one than the page runs = an update is ready).
    pub seq: u64,
    pub client: String,
    pub source: String,
    pub state: UpdateState,
    /// When the last check ran (epoch ms; 0 = none yet).
    pub at: f64,
    /// A newer app (APK) the latest release offers: its version.
    pub apk: Option<String>,
    /// The release's interface needs a newer app first.
    pub needs_shell: bool,
    /// The shell tries a failed check again by itself (`update.retry_at`; shells before 0.6.0 do not).
    pub retrying: bool,
    /// The shell can open a speech recognizer (`caps.voice`: shell 0.6.0+, and the TV has one).
    pub voice: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    Ok,
    Failed(String),
    /// The app update downloads: bytes so far, of (0 = unknown).
    Downloading(u64, u64),
    /// Android's installer was opened.
    Installing,
}

pub fn parse_shell_info(json: &str) -> Option<ShellInfo> {
    let v = crate::json::parse_str(json)?;
    let b = v.get("bundle")?;
    let u = v.get("update");
    let st = u.and_then(|u| u.get("state")).and_then(|x| x.as_str()).unwrap_or("idle");
    let num = |k: &str| u.and_then(|u| u.get(k)).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let text = |x: Option<&crate::json::Value>| x.and_then(|x| x.as_str()).map(|s| s.chars().take(300).collect::<String>());
    let state = match st {
        "checking" => UpdateState::Checking,
        "ok" => UpdateState::Ok,
        "failed" => UpdateState::Failed(text(u.and_then(|u| u.get("error"))).unwrap_or_default()),
        "downloading" => UpdateState::Downloading(num("got") as u64, num("total") as u64),
        "installing" => UpdateState::Installing,
        _ => UpdateState::Idle,
    };
    let apk = text(u.and_then(|u| u.get("apk")).and_then(|a| a.get("version"))).or_else(|| text(v.get("offer").and_then(|o| o.get("version"))));
    Some(ShellInfo {
        version: text(v.get("version")).unwrap_or_default(),
        seq: b.get("seq").and_then(|x| x.as_f64()).unwrap_or(0.0) as u64,
        client: text(b.get("client")).unwrap_or_default(),
        source: text(b.get("source")).unwrap_or_default(),
        state,
        at: num("at"),
        apk: apk.filter(|a| !a.is_empty()),
        needs_shell: u.and_then(|u| u.get("needs_shell")).and_then(|x| x.as_bool()).unwrap_or(false),
        retrying: num("retry_at") > 0.0,
        voice: v.get("caps").and_then(|c| c.get("voice")).and_then(|x| x.as_bool()).unwrap_or(false),
    })
}

/// What the About & updates screen says now (`running` = the bundle seq this page started with).
#[derive(Clone, Debug, PartialEq)]
pub enum UpdateView {
    NotChecked,
    Checking,
    UpToDate,
    /// A newer interface is in place: one reload runs it.
    Ready,
    /// A newer app: Install update.
    App(String),
    /// The app update downloads (percent, None = unknown size).
    Downloading(Option<u8>),
    Installing,
    /// The release's interface needs the newer app (offered, or not yet published).
    NeedsApp,
    Failed(String),
}

pub fn update_view(i: &ShellInfo, running: u64) -> UpdateView {
    match &i.state {
        UpdateState::Downloading(got, total) => return UpdateView::Downloading((*total > 0).then(|| ((*got * 100) / *total).min(100) as u8)),
        UpdateState::Installing => return UpdateView::Installing,
        UpdateState::Checking => return UpdateView::Checking,
        _ => {}
    }
    if running > 0 && i.seq > running {
        return UpdateView::Ready;
    }
    if let Some(v) = &i.apk {
        return UpdateView::App(v.clone());
    }
    match &i.state {
        UpdateState::Failed(e) => UpdateView::Failed(e.clone()),
        _ if i.needs_shell => UpdateView::NeedsApp,
        UpdateState::Ok => UpdateView::UpToDate,
        _ => UpdateView::NotChecked,
    }
}

/// A server error in a reason: `HTTP 5xx`.
fn server_error(e: &str) -> Option<&str> {
    let i = e.find("HTTP 5")?;
    let code = e.get(i..i + 8)?;
    code[5..].bytes().all(|b| b.is_ascii_digit()).then_some(code)
}

/// A failed check in words (the raw reason stays under it). `retrying`: the shell tries again by itself.
pub fn update_error(e: &str, retrying: bool) -> String {
    const AGAIN: &str = "Trying again automatically.";
    let l = e.to_ascii_lowercase();
    // (A shell that retries names a rate limit when it is one, so its bare 403 is something else. Older shells do not.)
    if l.contains("rate limit") || l.contains("http 429") || (l.contains("http 403") && !retrying) {
        format!("GitHub's limit for update checks was reached. {}", if retrying { AGAIN } else { "Try again in an hour." })
    } else if l.contains("signature") || l.contains("hash mismatch") || l.contains("verify") {
        "The download did not pass Redline's signature check, so nothing was installed.".into()
    } else if let Some(code) = server_error(e) {
        format!("GitHub isn't answering right now ({code}). {}", if retrying { AGAIN } else { "Try again in a few minutes." })
    } else if l.contains("dns") || l.contains("connect") || l.contains("resolve") || l.contains("timed out") || l.contains("timeout") || l.contains("network") || l.contains("io:") {
        format!("No connection to GitHub. {}", if retrying { AGAIN } else { "Check the TV's network and try again." })
    } else if l.contains("apk") || l.contains("larger than") || l.contains("body") {
        "The app update could not be downloaded. Try again.".into()
    } else if l.contains("unknown command") {
        "This app installs app updates from its own update dialog (it appears after a check, once a new app is published); the next one installs from here.".into()
    } else if retrying {
        format!("The update check did not work. {AGAIN}")
    } else {
        "The update check did not work.".into()
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(at: f64, now: f64) -> String {
    let m = ((now - at) / 60_000.0).max(0.0) as u64;
    match m {
        0 => "just now".into(),
        1..=59 => format!("{m} min ago"),
        60..=2879 => format!("{} h ago", m / 60),
        _ => format!("{} days ago", m / 1440),
    }
}

// ------------------------------------------------------------- what the shell hands the page

/// The key the shell sends for the remote's search and voice keys (`KEYCODE_SEARCH`, `KEYCODE_VOICE_ASSIST`).
pub const VOICE_KEY: &str = "BrowserSearch";

/// One of the shell's `shell_events` (shell.rs): what the speech recognizer heard, a search or a link from outside.
#[derive(Clone, Debug, PartialEq)]
pub enum ShellEvent {
    Heard(String),
    /// The recognizer closed with nothing to search for.
    VoiceEnd(VoiceEnd),
    Search(String),
    /// A path on this site, re-read with the routes before anything opens it.
    Open(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceEnd {
    Cancel,
    NoMatch,
    Failed,
    /// No recognizer after all.
    Missing,
}

impl VoiceEnd {
    /// The calm notice it deserves ("" = none: the owner closed it).
    pub fn note(self) -> &'static str {
        match self {
            VoiceEnd::Cancel => "",
            VoiceEnd::NoMatch => "Didn't catch that. Press the mic to try again.",
            VoiceEnd::Failed => "Voice search didn't work this time.",
            VoiceEnd::Missing => "Voice search isn't available on this TV.",
        }
    }
}

pub fn parse_shell_events(json: &str) -> Vec<ShellEvent> {
    let Some(v) = crate::json::parse_str(json) else { return Vec::new() };
    let Some(list) = v.get("events").and_then(|e| e.as_array()) else { return Vec::new() };
    let text = |e: &crate::json::Value, k: &str| e.get(k).and_then(|x| x.as_str()).map(String::from);
    list.iter()
        .filter_map(|e| match e.get("kind")?.as_str()? {
            "voice" => Some(match e.get("state")?.as_str()? {
                "text" => ShellEvent::Heard(text(e, "text")?),
                "cancel" => ShellEvent::VoiceEnd(VoiceEnd::Cancel),
                "none" => ShellEvent::VoiceEnd(VoiceEnd::NoMatch),
                "missing" => ShellEvent::VoiceEnd(VoiceEnd::Missing),
                _ => ShellEvent::VoiceEnd(VoiceEnd::Failed),
            }),
            "search" => Some(ShellEvent::Search(text(e, "q")?)),
            "open" => Some(ShellEvent::Open(text(e, "path")?)),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------- device sign-in

/// The shell's `auth_start` answer (the code the owner types on their phone).
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceCode {
    pub user_code: String,
    pub url: String,
    pub interval_ms: f64,
    pub expires_ms: f64,
}

pub fn parse_device(json: &str) -> Option<DeviceCode> {
    let v = crate::json::parse_str(json)?;
    let code = v.get("user_code")?.as_str()?;
    let url = v.get("verification_url")?.as_str()?;
    let ok_code = (4..=24).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let ok_url = url.starts_with("https://") && url.len() < 120 && !url.contains(['"', '<', '>', ' ']);
    if !ok_code || !ok_url {
        return None;
    }
    let interval = v.get("interval").and_then(|x| x.as_f64()).unwrap_or(5.0).clamp(1.0, 60.0);
    let expires = v.get("expires_in").and_then(|x| x.as_f64()).unwrap_or(1800.0).clamp(30.0, 3600.0);
    Some(DeviceCode { user_code: code.into(), url: url.into(), interval_ms: interval * 1000.0, expires_ms: expires * 1000.0 })
}

/// The shell's `auth_poll` answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Poll {
    /// Not entered yet (`authorization_pending`); `slow` = Google asked to poll less often.
    Pending { slow: bool },
    Done { token: String, exp: f64 },
    Denied,
    Expired,
    Failed(String),
}

pub fn parse_poll(json: &str) -> Poll {
    let Some(v) = crate::json::parse_str(json) else { return Poll::Failed("bad answer".into()) };
    match v.get("state").and_then(|s| s.as_str()).unwrap_or("") {
        "pending" => Poll::Pending { slow: v.get("slow").and_then(|s| s.as_bool()).unwrap_or(false) },
        "done" => match (v.get("token").and_then(|t| t.as_str()), v.get("exp").and_then(|e| e.as_f64())) {
            (Some(t), Some(e)) if valid_token(t) => Poll::Done { token: t.into(), exp: e },
            _ => Poll::Failed("bad token".into()),
        },
        "denied" => Poll::Denied,
        "expired" => Poll::Expired,
        _ => Poll::Failed(v.get("error").and_then(|e| e.as_str()).unwrap_or("unknown").chars().take(120).collect()),
    }
}

/// The TV app keeps the ready Home response for the next app start (`ready_put`, shell-side sealed, one account, 10 min,
/// read once): the page hands it every record it keeps. Fire-and-forget calls (seq 0: no answer is awaited).
pub fn ready_put(record: &str) {
    if on() {
        crate::bridge::shell(0, "ready_put", &json!({"record": record}).to_string());
    }
}

/// The ready response is gone (used, or something personalizing happened): the shell's copy goes too.
pub fn ready_drop() {
    if on() {
        crate::bridge::shell(0, "ready_drop", "null");
    }
}

/// `ready_take`'s answer: the record kept from the last run, if any.
pub fn parse_ready_take(json: &str) -> Option<String> {
    let v = crate::json::parse_str(json)?;
    v.get("record").and_then(|r| r.as_str()).filter(|r| !r.is_empty()).map(String::from)
}

/// The shell's speed telemetry (`tel_info`, `tel_set`): off unless the owner turned it on (About & updates →
/// Developer); while on, the shell runs the interface's telemetry build (`dev`; without one only errors go) and
/// delivers the reports to the debug center at `url` on the owner's network.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TelInfo {
    pub on: bool,
    pub url: String,
    pub dev: bool,
    pub sent: u64,
    pub queued: u64,
    pub error: String,
    /// The page must load again (the shell changed its script).
    pub reload: bool,
}

pub fn parse_tel_info(json: &str) -> Option<TelInfo> {
    let v = crate::json::parse_str(json)?;
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|s| s.chars().take(200).collect::<String>()).unwrap_or_default();
    let num = |k: &str| v.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0) as u64;
    let flag = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    Some(TelInfo { on: v.get("on")?.as_bool()?, url: text("url"), dev: flag("dev"), sent: num("sent"), queued: num("queued"), error: text("error"), reload: flag("reload") })
}

/// The debug center's address as the owner reads and types it (no scheme).
pub fn tel_addr(url: &str) -> &str {
    url.strip_prefix("http://").unwrap_or(url)
}

/// The `tel_set` argument.
pub fn tel_set_json(on: Option<bool>, url: Option<&str>) -> String {
    let mut m = serde_json::Map::new();
    if let Some(on) = on {
        m.insert("on".into(), on.into());
    }
    if let Some(u) = url {
        m.insert("url".into(), u.trim().into());
    }
    serde_json::Value::Object(m).to_string()
}

/// → presses with nothing to the right, in a row, that show the Developer section of About & updates.
pub const DEV_TAPS: u32 = 5;

#[cfg(test)]
mod tests {
    use super::{trail_back, trail_push};

    #[test]
    fn previous_walks_the_sessions_videos() {
        let mut t = Vec::new();
        trail_push(&mut t, "a", 3);
        assert_eq!(trail_back(&mut t), None, "nothing before the first: it restarts, and stays on the trail");
        assert_eq!(t, ["a"]);
        trail_push(&mut t, "a", 3);
        assert_eq!(t, ["a"], "a retry of the same video is not a new step");
        trail_push(&mut t, "b", 3);
        trail_push(&mut t, "c", 3);
        assert_eq!(trail_back(&mut t), Some("b"));
        trail_push(&mut t, "b", 3);
        assert_eq!(trail_back(&mut t), Some("a"));
        trail_push(&mut t, "a", 3);
        assert_eq!(trail_back(&mut t), None);
        for x in ["b", "c", "d", "e"] {
            trail_push(&mut t, x, 3);
        }
        assert_eq!(t, ["c", "d", "e"], "bounded");
    }

    use super::*;

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    /// A 4-column card grid (200×180 cards, 20 px gaps) under a header row.
    fn grid() -> Vec<Rect> {
        let mut v = vec![r(20.0, 10.0, 100.0, 40.0), r(400.0, 10.0, 300.0, 40.0)];
        for row in 0..3 {
            for col in 0..4 {
                v.push(r(100.0 + col as f64 * 220.0, 80.0 + row as f64 * 200.0, 200.0, 180.0));
            }
        }
        v
    }

    #[test]
    fn shell_info_reads_every_update_state() {
        let i = parse_shell_info(r#"{"version":"0.1.0","version_code":1000,"bundle":{"seq":200,"client":"0.9.0","source":"release"},"update":{"state":"ok","at":1000,"needs_shell":false,"installed":{"client":"0.9.0","seq":200},"apk":{"version":"0.2.0","version_code":2000}},"offer":null,"repo":"o/r"}"#).unwrap();
        assert_eq!((i.version.as_str(), i.seq, i.client.as_str(), i.source.as_str(), &i.state, i.at, i.apk.as_deref()), ("0.1.0", 200, "0.9.0", "release", &UpdateState::Ok, 1000.0, Some("0.2.0")));
        let d = parse_shell_info(r#"{"version":"0.1.0","bundle":{"seq":1,"client":"x","source":"builtin"},"update":{"state":"downloading","got":512,"total":2048,"apk":{"version":"0.2.0"}}}"#).unwrap();
        assert_eq!(d.state, UpdateState::Downloading(512, 2048));
        let f = parse_shell_info(r#"{"version":"0.1.0","bundle":{"seq":1,"client":"x","source":"builtin"},"update":{"state":"failed","at":5,"error":"release lookup: HTTP 403"}}"#).unwrap();
        assert_eq!(f.state, UpdateState::Failed("release lookup: HTTP 403".into()));
        assert!(update_error("release lookup: HTTP 403", false).contains("limit"));
        assert!(update_error("manifest signature does not verify", false).contains("signature"));
        assert!(update_error("io: failed to lookup address (dns error)", false).contains("connection"));
        assert!(update_error("APK: hash mismatch", false).contains("signature"));
        assert!(update_error("APK: HTTP 404", false).contains("could not be downloaded"));
        // An old shell (no offer key, idle) and a broken answer.
        let o = parse_shell_info(r#"{"version":"0.1.0","bundle":{"seq":7,"client":"0.8.7","source":"builtin"},"update":{"state":"idle"}}"#).unwrap();
        assert_eq!((&o.state, &o.apk), (&UpdateState::Idle, &None));
        assert!(parse_shell_info("null").is_none());
        // What the screen shows: a newer interface than the page runs first, then an app update, else the check.
        assert_eq!(update_view(&i, 100), UpdateView::Ready);
        assert_eq!(update_view(&i, 200), UpdateView::App("0.2.0".into()));
        assert_eq!(update_view(&d, 1), UpdateView::Downloading(Some(25)));
        assert_eq!(update_view(&f, 1), UpdateView::Failed("release lookup: HTTP 403".into()));
        assert_eq!(update_view(&o, 7), UpdateView::NotChecked);
        let ok = ShellInfo { state: UpdateState::Ok, seq: 7, ..o.clone() };
        assert_eq!(update_view(&ok, 7), UpdateView::UpToDate);
        assert_eq!(ago(0.0, 30_000.0), "just now");
        assert_eq!(ago(0.0, 5.0 * 60_000.0), "5 min ago");
        assert_eq!(ago(0.0, 3.0 * 3_600_000.0), "3 h ago");
        assert_eq!(ago(0.0, 3.0 * 86_400_000.0), "3 days ago");
    }

    #[test]
    fn update_failures_in_words() {
        // The reasons shells give: 0.6.0 (the first), and from before it (no second route, no retries).
        let down = "GitHub isn't answering (lumen-tv-manifest.json: download HTTP 504, API HTTP 502)";
        assert_eq!(update_error(down, true), "GitHub isn't answering right now (HTTP 504). Trying again automatically.");
        assert_eq!(update_error("lumen-tv-manifest.json: HTTP 504", false), "GitHub isn't answering right now (HTTP 504). Try again in a few minutes.");
        assert_eq!(update_error("release lookup: HTTP 502", true), "GitHub isn't answering right now (HTTP 502). Trying again automatically.");
        let limit = "GitHub's rate limit was reached (release lookup: HTTP 403, resets in 23 min)";
        assert_eq!(update_error(limit, true), "GitHub's limit for update checks was reached. Trying again automatically.");
        assert_eq!(update_error(limit, false), "GitHub's limit for update checks was reached. Try again in an hour.");
        assert_eq!(update_error("GitHub's rate limit was reached (x: download HTTP 504, API HTTP 429)", true), "GitHub's limit for update checks was reached. Trying again automatically.");
        // A real 403 from a shell that names its rate limits is not one; from an older shell it may be.
        assert_eq!(update_error("lumen-tv-manifest.json: HTTP 403", true), "The update check did not work. Trying again automatically.");
        assert_eq!(update_error("release lookup: HTTP 403", false), "GitHub's limit for update checks was reached. Try again in an hour.");
        assert_eq!(update_error("No connection to GitHub (release lookup: io: refused)", true), "No connection to GitHub. Trying again automatically.");
        assert_eq!(update_error("No connection to GitHub (x: download timeout, API timeout)", false), "No connection to GitHub. Check the TV's network and try again.");
        assert_eq!(update_error("GitHub isn't answering (x: download timeout: global, API HTTP 503)", true), "GitHub isn't answering right now (HTTP 503). Trying again automatically.");
        assert_eq!(update_error("APK: GitHub isn't answering", false), "The app update could not be downloaded. Try again.");
        assert!(update_error("signature does not match the release key", true).contains("nothing was installed"), "a bad signature stays what it is");
        assert_eq!(update_error("release has no x", true), "The update check did not work. Trying again automatically.");
        assert_eq!(update_error("HTTP 5", true), "The update check did not work. Trying again automatically.", "no code to name");
        // What a retrying shell says, and an older one.
        let f = parse_shell_info(r#"{"version":"0.6.0","bundle":{"seq":1,"client":"x","source":"builtin"},"update":{"state":"failed","at":5,"error":"x","retry_at":600},"caps":{"voice":true}}"#).unwrap();
        assert!(f.retrying && f.voice);
        let o = parse_shell_info(r#"{"version":"0.5.0","bundle":{"seq":1,"client":"x","source":"builtin"},"update":{"state":"failed","at":5,"error":"x"}}"#).unwrap();
        assert!(!o.retrying && !o.voice, "a shell before 0.6.0 has no mic and no retries");
    }

    #[test]
    fn what_the_shell_hands_the_page() {
        assert_eq!(
            parse_shell_events(r#"{"events":[{"kind":"voice","state":"text","text":"lo fi"},{"kind":"voice","state":"cancel"},{"kind":"voice","state":"none"},{"kind":"voice","state":"missing"},{"kind":"voice","state":"error"},{"kind":"search","q":"rain"},{"kind":"open","path":"/watch?v=dQw4w9WgXcQ"},{"kind":"new"},{"kind":"voice","state":"text"}]}"#),
            vec![
                ShellEvent::Heard("lo fi".into()),
                ShellEvent::VoiceEnd(VoiceEnd::Cancel),
                ShellEvent::VoiceEnd(VoiceEnd::NoMatch),
                ShellEvent::VoiceEnd(VoiceEnd::Missing),
                ShellEvent::VoiceEnd(VoiceEnd::Failed),
                ShellEvent::Search("rain".into()),
                ShellEvent::Open("/watch?v=dQw4w9WgXcQ".into()),
            ]
        );
        assert_eq!(parse_shell_events("null"), vec![]);
        assert_eq!(parse_shell_events("\"unknown command shell_events\""), vec![]);
        assert_eq!(VoiceEnd::Cancel.note(), "", "closing it needs no words");
        assert!(!VoiceEnd::NoMatch.note().is_empty());
    }

    #[test]
    fn rows_keep_one_vertical_place() {
        // The grid scrolled by 150: the first row's cards sit at y = 80 - 150; the header is not marked.
        let mut g = grid();
        for c in g.iter_mut().skip(2) {
            c.y -= 150.0;
        }
        let mut marks = vec![0u8; g.len()];
        for m in marks.iter_mut().skip(2) {
            *m = 1;
        }
        // From row 1 (focused, index 7) down to row 2 col 1 (index 11): row 2 lands where row 0 sits at scroll 0.
        assert_eq!(row_scroll(&g, &marks, 7, 11), 400.0);
        // Back up to row 0: the top.
        assert_eq!(row_scroll(&g, &marks, 7, 3), 0.0);
        // The focused card (drawn scaled) is never the reference, even when it is the first one.
        let mut s = g.clone();
        s[2].y -= 9.0;
        assert_eq!(row_scroll(&s, &marks, 2, 7), 200.0);
        // No other grid card: no anchoring.
        assert_eq!(row_scroll(&g[..3], &marks[..3], 2, 2), -1.0);
    }

    #[test]
    fn grid_walks_like_a_launcher() {
        let g = grid();
        // Card (row 0, col 1) = index 3.
        let cur = g[3];
        assert_eq!(pick(cur, &g, Dir::Right, Some(3)), Some(4));
        assert_eq!(pick(cur, &g, Dir::Left, Some(3)), Some(2));
        assert_eq!(pick(cur, &g, Dir::Down, Some(3)), Some(7), "the card below, not a diagonal one");
        assert_eq!(pick(cur, &g, Dir::Up, Some(3)), Some(1), "the search box above it (overlaps its column)");
        // Last column: nothing further right.
        assert_eq!(pick(g[5], &g, Dir::Right, Some(5)), None);
        // Bottom row: nothing below.
        assert_eq!(pick(g[13], &g, Dir::Down, Some(13)), None);
        // Up from the first column reaches the logo.
        assert_eq!(pick(g[2], &g, Dir::Up, Some(2)), Some(0));
    }

    #[test]
    fn sidebar_and_grid() {
        // Sidebar links at the left edge, grid to the right.
        let mut v: Vec<Rect> = (0..6).map(|i| r(0.0, 80.0 + i as f64 * 44.0, 80.0, 40.0)).collect();
        v.push(r(100.0, 80.0, 200.0, 180.0));
        v.push(r(100.0, 280.0, 200.0, 180.0));
        // Left from a card in the second row goes to the sidebar link at its height.
        let i = pick(v[7], &v, Dir::Left, Some(7)).unwrap();
        assert!(i < 6 && v[i].y >= 280.0 - 44.0, "picked {i}");
        // Right from a link goes to the card beside it.
        assert_eq!(pick(v[0], &v, Dir::Right, Some(0)), Some(6));
        // Down walks the links.
        assert_eq!(pick(v[0], &v, Dir::Down, Some(0)), Some(1));
    }

    #[test]
    fn zero_size_and_self_skipped() {
        let v = vec![r(0.0, 0.0, 10.0, 10.0), r(20.0, 0.0, 0.0, 0.0), r(40.0, 0.0, 10.0, 10.0)];
        assert_eq!(pick(v[0], &v, Dir::Right, Some(0)), Some(2));
    }

    #[test]
    fn requests_switch_to_the_tv_client() {
        let cfg = Config::default();
        let web = crate::api::Req::Browse { id: "FEsubscriptions".into(), params: None }.body(&cfg, "de", "/feed/subscriptions");
        let tok = "ya29.a0AfB_byC-test_token-0123456789";
        let auth = Auth { bearer: Some(tok.into()), ..Auth::default() };
        let (b, h) = finish(&cfg, &auth, web.clone(), 1);
        let b: Value = serde_json::from_str(&b).unwrap();
        let h: Value = serde_json::from_str(&h).unwrap();
        assert_eq!(b["context"]["client"]["clientName"], "TVHTML5");
        assert_eq!(b["context"]["client"]["clientVersion"], TV_VERSION);
        assert_eq!(b["context"]["client"]["hl"], "de");
        assert_eq!(b["browseId"], "FEsubscriptions");
        assert_eq!(h["Authorization"], format!("Bearer {tok}"));
        assert_eq!(h["X-Youtube-Client-Name"], "7");
        assert!(h.get("X-Goog-AuthUser").is_none() && h.get("X-Goog-Visitor-Id").is_none());
        // No token: the WEB request is untouched.
        let (b2, h2) = finish(&cfg, &Auth::default(), web.clone(), 1);
        assert_eq!(b2, web);
        assert!(!h2.contains("Authorization"));
        assert!(account_browse("FEwhat_to_watch") && account_browse("VLWL") && account_browse("VLLL"));
        assert!(!account_browse("UCabcdefghijklmnopqrstuv"));
        assert_eq!(ping_headers(&auth), format!("{{\"Authorization\":\"Bearer {tok}\"}}"));
        assert_eq!(ping_headers(&Auth::default()), "{}");
    }

    #[test]
    fn tokens_and_answers() {
        assert!(valid_token("ya29.a0AfB_byC-abc.def_ghi-0123456789"));
        assert!(!valid_token("ya29.\"><script>"));
        assert!(!valid_token("short"));
        assert_eq!(parse_token(r#"{"token":"ya29.a0AfB_byC-abc.def_ghi-0123456789","exp":5000}"#), Some(("ya29.a0AfB_byC-abc.def_ghi-0123456789".into(), 5000.0)));
        assert_eq!(parse_token(r#"{"token":null}"#), None);
        set_bearer(Some(("ya29.a0AfB_byC-abc.def_ghi-0123456789".into(), 5000.0)));
        assert!(bearer_at(4999.0).is_some() && bearer_at(5000.0).is_none());
        assert!(auth_at(1.0).signed_in() && !auth_at(6000.0).signed_in());
        set_bearer(None);
        // Recorded 2 Nov 2026 (shape) from POST /o/oauth2/device/code via the shell.
        let d = parse_device(r#"{"user_code":"TBX-NZG-FMQW","verification_url":"https://www.google.com/device","expires_in":1800,"interval":5}"#).unwrap();
        assert_eq!((d.user_code.as_str(), d.url.as_str(), d.interval_ms, d.expires_ms), ("TBX-NZG-FMQW", "https://www.google.com/device", 5000.0, 1_800_000.0));
        assert_eq!(parse_device(r#"{"user_code":"<b>","verification_url":"https://x"}"#), None);
        assert_eq!(parse_device(r#"{"user_code":"ABC-DEF","verification_url":"javascript:alert(1)"}"#), None);
        assert_eq!(parse_poll(r#"{"state":"pending"}"#), Poll::Pending { slow: false });
        assert_eq!(parse_poll(r#"{"state":"pending","slow":true}"#), Poll::Pending { slow: true });
        assert_eq!(parse_poll(r#"{"state":"done","token":"ya29.a0AfB_byC-abc.def_ghi-0123456789","exp":9}"#), Poll::Done { token: "ya29.a0AfB_byC-abc.def_ghi-0123456789".into(), exp: 9.0 });
        assert_eq!(parse_poll(r#"{"state":"done","token":"x","exp":9}"#), Poll::Failed("bad token".into()));
        assert_eq!(parse_poll(r#"{"state":"denied"}"#), Poll::Denied);
        assert_eq!(parse_poll("nope"), Poll::Failed("bad answer".into()));
    }

    #[test]
    fn remote_keys() {
        assert_eq!(remote("MediaPlayPause"), Some(Remote::PlayPause));
        assert_eq!(remote_player_key(Remote::Forward, false), Some("l"));
        assert_eq!(remote_player_key(Remote::Rewind, true), Some("j"));
        assert_eq!(remote_player_key(Remote::Play, false), None, "already playing");
        assert_eq!(remote_player_key(Remote::Pause, false), Some("k"));
        assert_eq!(remote("a"), None);
        assert_eq!(dir("ArrowLeft"), Some(Dir::Left));
    }

    #[test]
    fn telemetry_info() {
        let t = super::parse_tel_info(r#"{"on":true,"url":"http://192.168.1.20:47321","dev":true,"queued":2,"sent":14,"error":null}"#).unwrap();
        assert!(t.on && t.dev && !t.reload && t.error.is_empty());
        assert_eq!((super::tel_addr(&t.url), t.sent, t.queued), ("192.168.1.20:47321", 14, 2));
        let t = super::parse_tel_info(r#"{"on":false,"url":"","dev":false,"queued":0,"sent":0,"error":"debug center: HTTP 503","reload":true}"#).unwrap();
        assert!(!t.on && t.reload && t.error.contains("503"));
        assert!(super::parse_tel_info("\"unknown command tel_info\"").is_none() && super::parse_tel_info("{}").is_none());
        assert_eq!(super::tel_set_json(Some(true), None), r#"{"on":true}"#);
        assert_eq!(super::tel_set_json(None, Some(" 192.168.1.20 ")), r#"{"url":"192.168.1.20"}"#);
        assert_eq!(super::tel_set_json(Some(true), Some("a\"b")), r#"{"on":true,"url":"a\"b"}"#);
    }

    #[test]
    fn ready_take_answer() {
        assert_eq!(super::parse_ready_take(r#"{"record":"{\"v\":1}"}"#).as_deref(), Some(r#"{"v":1}"#));
        assert_eq!(super::parse_ready_take(r#"{"record":null}"#), None);
        assert_eq!(super::parse_ready_take(r#"{"record":""}"#), None);
        assert_eq!(super::parse_ready_take("\"unknown command ready_take\""), None);
    }
}
