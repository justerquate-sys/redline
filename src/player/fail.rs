//! Player failures and stalls stay inside the player (since 2 Nov 2026: always the client's own
//! player, so every problem can be seen and inspected where it happens). The client never leaves for
//! YouTube's page by itself: a failure shows a calm panel in the player box (a human message,
//! the technical reason, Try again / Open on YouTube), and a stall shows a small "Still
//! loading — retrying…" status while the player keeps retrying in place. Nobody presses Try again
//! for what Try again would fix (owner, 8 Nov 2026): a reason that isn't permanent gets it by itself
//! first (`Auto`), and once more on coming back to the tab. A verdict Try again can't change
//! (`offers_retry`: members-only, a premiere hours away, sign-in, removed, unplayable) gets no such
//! button: its panel says it in plain words, without our technical prefix, and offers Next video /
//! Open on YouTube. Pure parts here (messages, markup, the retry schedules); `own.rs` and `app.rs` drive them.

use crate::util::esc_into;

/// The panel's buttons and the stuck status' buttons (handled by the app, not the player).
pub const ACT_RETRY: &str = "pf-retry";
pub const ACT_NATIVE: &str = "pf-native";
/// A verdict's panel: the next video (`upnext::next_pressed`).
pub const ACT_NEXT: &str = "pf-next";
/// The persistent status element inside the player box (shown by `.player.stuck`).
pub const STUCK: &str = "ytc-pstuck";
/// The panel's Try again button (focused when the panel replaces a focused control).
pub const RETRY_BTN: &str = "ytc-pfretry";
/// A verdict's panel: its plain line and its buttons (`next` may answer after the player did).
pub const LINE: &str = "ytc-pfl";
pub const BUTTONS: &str = "ytc-pfb";

/// The panel's reason while the connection is down (the app probes and continues by itself).
pub const OFFLINE_REASON: &str = "offline: youtube.com isn't answering (connection or DNS); continuing when it's back";

/// A failure that may just be a dropped connection: requests that never got an answer.
pub fn network_like(reason: &str) -> bool {
    reason.contains("HTTP 0") || reason.contains("no answer")
}

/// A verdict a new player would only repeat: the panel at once, no automatic Try again. Upcoming
/// streams, members-only videos, sign-in and the bot check (every route was tried), YouTube's own
/// `VISIONOS <STATUS>` answers, no format this browser plays, the player turned off. Everything else
/// (media errors, stalls, index and googlevideo failures, "VISIONOS player" without an answer, also
/// reasons not known here) is worth a retry: a wrong guess costs seconds, and the retries are bounded.
pub fn permanent(reason: &str) -> bool {
    super::own::quiet(reason)
        || reason == OFFLINE_REASON
        || reason.starts_with("no compatible formats")
        || reason.strip_prefix("VISIONOS ").is_some_and(|r| r.starts_with(|c: char| c.is_ascii_uppercase()))
}

/// The player's verdict that a video is for a channel's members: YouTube's known phrases only (VISIONOS is
/// always asked in English, `api::vision_request`), never the word alone ("content from Club Members Records").
pub fn members_only(reason: &str) -> bool {
    if !reason.starts_with("VISIONOS ") {
        return false;
    }
    let low = reason.to_ascii_lowercase();
    ["members on level:", "members-only", "members only", "members first"].iter().any(|p| low.contains(p))
}

/// YouTube's words name a time hours or more away, or a date.
fn far_off(said: &str) -> bool {
    const FAR: [&str; 11] = ["hour", "hours", "day", "days", "week", "weeks", "month", "months", "year", "years", "tomorrow"];
    const MONTHS: [&str; 24] = ["jan", "january", "feb", "february", "mar", "march", "apr", "april", "may", "jun", "june", "jul", "july", "aug", "august", "sep", "sept", "september", "oct", "october", "nov", "november", "dec", "december"];
    let low = said.to_ascii_lowercase();
    let words: Vec<&str> = low.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let number = |w: Option<&&str>| w.is_some_and(|w| w.bytes().all(|b| b.is_ascii_digit()));
    words.iter().enumerate().any(|(i, w)| FAR.contains(w) || (MONTHS.contains(w) && (number(words.get(i + 1)) || (i > 0 && number(words.get(i - 1))))))
}

/// Whether the panel offers Try again: everything but a verdict about the video itself. The bot check (Try
/// again asks every route again), a connection that's down, YouTube saying "something went wrong" or "try
/// again", and a stream or premiere that may start any moment can change; a members-only, private, removed
/// or age-restricted video, one this browser has no format for, or a premiere hours away cannot.
pub fn offers_retry(reason: &str) -> bool {
    if !permanent(reason) || reason == OFFLINE_REASON {
        return true;
    }
    if members_only(reason) {
        return false;
    }
    let low = reason.to_ascii_lowercase();
    if ["went wrong", "try again", "error occurred"].iter().any(|w| low.contains(w)) {
        return true;
    }
    if let Some(said) = reason.strip_prefix(super::own::UPCOMING_REASON) {
        return !far_off(said);
    }
    reason.starts_with("VISIONOS LOGIN_REQUIRED") && super::own::bot_check("LOGIN_REQUIRED", reason)
}

/// A verdict that YouTube's own page may well get past (it knows the account): Open on YouTube comes first.
fn open_helps(reason: &str) -> bool {
    reason.starts_with("VISIONOS LOGIN_REQUIRED") || reason.starts_with("VISIONOS CONTENT_CHECK_REQUIRED") || reason.starts_with("no compatible formats") || reason.starts_with("own player turned off")
}

/// How a failure is retried.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Retries {
    /// A verdict: the panel.
    Never,
    /// The usual schedule (`AUTO_DELAYS_MS`).
    Fast,
    /// At most one automatic retry in the window, none if others went out, after this wait (ms).
    Once(u32),
}

/// YouTube's player service says "too many requests": one retry, a minute later.
pub const RATE_LIMIT_WAIT_MS: u32 = 60_000;
/// It refuses the request (403, 400): one retry only.
pub const REFUSED_WAIT_MS: u32 = 4000;

/// The player service's HTTP status in a failure reason.
fn service_status(reason: &str) -> Option<u32> {
    reason.strip_prefix("VISIONOS player HTTP ")?.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

/// How a failure reason is retried. `played`: the failed player had shown a picture. YouTube's
/// `ERROR` during a refresh of a video that was playing seconds ago may be a passing one (one retry;
/// the new player's answer at its start is the verdict). The bot check is never retried.
pub fn retries(reason: &str, played: bool) -> Retries {
    if let Some(code) = service_status(reason) {
        return match code {
            429 => Retries::Once(RATE_LIMIT_WAIT_MS),
            400 | 403 => Retries::Once(REFUSED_WAIT_MS),
            _ => Retries::Fast,
        };
    }
    if played && reason.starts_with("VISIONOS ERROR") && !super::own::quiet(reason) {
        return Retries::Once(AUTO_DELAYS_MS[0]);
    }
    if permanent(reason) { Retries::Never } else { Retries::Fast }
}

/// The automatic Try again's waits: the first after `AUTO_DELAYS_MS[0]`, and so on.
pub const AUTO_DELAYS_MS: [u32; 3] = [1000, 4000, 10_000];
/// As many as `AUTO_DELAYS_MS` within this sliding window, per video.
pub const AUTO_WINDOW_MS: f64 = 300_000.0;
/// And at most this many for a video while it stays open: failures that keep coming back slowly never loop.
pub const AUTO_MAX: u32 = 9;
/// Coming back to the panel's tab: at most this many retries while the video stays open,
pub const RETURN_MAX: u32 = 3;
/// never two within this time (a return fires both `visibilitychange` and `focus`),
pub const RETURN_GAP_MS: f64 = 5000.0;
/// and a window that only lost the focus (still visible) must have been away this long.
pub const RETURN_AWAY_MS: f64 = 60_000.0;
/// A window focused again retries this much later: the press that focused it may be on the panel.
pub const RETURN_FOCUS_WAIT_MS: u32 = 300;

/// The automatic retries of the video that is open (a new navigation starts over).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Auto {
    /// When the retries inside the window went out.
    at: Vec<f64>,
    total: u32,
    returns: u32,
    last_return: f64,
    /// The player running now is a return's one retry: it gets no chain of its own unless it played.
    returning: bool,
    /// When the player service last said "too many requests".
    limited: Option<f64>,
}

impl Auto {
    /// A player failed at `now` for `reason`.
    pub fn failed(&mut self, reason: &str, now: f64) {
        if service_status(reason) == Some(429) {
            self.limited = Some(now);
        }
    }

    /// The wait before the next automatic Try again after a failure at `now`; None: the panel.
    /// `played`: the failed player had shown a picture; `how`: `retries(reason, played)`.
    pub fn delay(&mut self, now: f64, played: bool, how: Retries) -> Option<u32> {
        if how == Retries::Never || (self.returning && !played) {
            return None;
        }
        self.returning = false;
        self.at.retain(|t| now - t < AUTO_WINDOW_MS);
        if self.total >= AUTO_MAX {
            return None;
        }
        match how {
            Retries::Once(ms) => self.at.is_empty().then_some(ms),
            _ => AUTO_DELAYS_MS.get(self.at.len()).copied(),
        }
    }

    /// The retry goes out (only now it counts: a wait ended by a dropped connection costs nothing).
    pub fn spend(&mut self, now: f64) {
        self.at.push(now);
        self.total += 1;
    }

    /// Whether coming back to the panel at `now` retries once more. `away_ms`: how long a window
    /// that only got the focus back was without it (None: the tab became visible).
    pub fn may_return(&self, now: f64, away_ms: Option<f64>) -> bool {
        self.returns < RETURN_MAX
            && (self.returns == 0 || now - self.last_return >= RETURN_GAP_MS)
            && away_ms.is_none_or(|ms| ms >= RETURN_AWAY_MS)
            && self.limited.is_none_or(|at| now - at >= f64::from(RATE_LIMIT_WAIT_MS))
    }

    pub fn returned(&mut self, now: f64) {
        self.returns += 1;
        self.last_return = now;
        self.returning = true;
    }

    /// Every Try again made without being asked, for the panel's technical line.
    pub fn made(&self) -> u32 {
        self.total + self.returns
    }
}

/// `sessionStorage` key of a stale tab's reload: `epoch ms|paused 0/1|video id`.
pub const STALE_KEY: &str = "ytc:stale";
/// A tab reloads for this reason at most once in this time, however it gets here again.
pub const STALE_GUARD_MS: f64 = 600_000.0;
/// The reloaded page takes the paused state only this soon after the mark.
pub const STALE_PAUSED_MS: f64 = 60_000.0;

pub fn stale_mark(now_ms: f64, paused: bool, v: &str) -> String {
    format!("{now_ms:.0}|{}|{v}", u8::from(paused))
}

fn stale_parts(mark: &str) -> Option<(f64, bool, &str)> {
    let mut p = mark.splitn(3, '|');
    Some((p.next()?.parse().ok()?, p.next()? == "1", p.next()?))
}

/// May this tab reload now, given its mark (empty: none yet)? A clock set back counts as long ago.
pub fn stale_may_reload(mark: &str, now_ms: f64) -> bool {
    stale_parts(mark).is_none_or(|(at, _, _)| now_ms < at || now_ms - at >= STALE_GUARD_MS)
}

/// The reloaded page's video `v` was paused when the old page reloaded.
pub fn stale_paused(mark: &str, now_ms: f64, v: &str) -> bool {
    stale_parts(mark).is_some_and(|(at, paused, mv)| paused && mv == v && now_ms >= at && now_ms - at < STALE_PAUSED_MS)
}

/// What follows the reason on the panel's technical line: the Try agains made without being asked
/// and the version that ran, so a screenshot says which code ran and what it tried.
pub fn diagnostics(made: u32) -> String {
    let ver = env!("CARGO_PKG_VERSION");
    match made {
        0 => format!(" · {ver}"),
        1 => format!(" · 1 automatic Try again · {ver}"),
        n => format!(" · {n} automatic Try agains · {ver}"),
    }
}

/// The human headline for a failure reason (the technical reason is shown under it).
pub fn message(reason: &str) -> String {
    let r = reason;
    if r == OFFLINE_REASON {
        return "No connection — continuing when it's back".into();
    }
    let yt = |prefix: &str| r.strip_prefix(prefix).map(|s| s.split(" · ").next().unwrap_or("").trim().to_string());
    if let Some(rest) = r.strip_prefix(super::own::UPCOMING_REASON) {
        // YouTube's own words ("Premieres in 2 hours", "This live event will begin in a few moments.").
        let said = rest.trim_start_matches(':').trim();
        return if said.is_empty() { "This live stream hasn't started yet".into() } else { said.to_string() };
    }
    if r.starts_with("own player turned off") {
        return "The client's own player is turned off".into();
    }
    let low = r.to_ascii_lowercase();
    if members_only(r) {
        return "Members-only video".into();
    }
    if r.starts_with("VISIONOS LOGIN_REQUIRED") {
        if super::own::bot_check("LOGIN_REQUIRED", r) {
            return "YouTube asks to sign in to confirm you're not a bot".into();
        }
        if low.contains("your age") || low.contains("age-restricted") || low.contains("inappropriate") {
            return "Age-restricted — sign-in needed".into();
        }
        return "Sign-in needed to watch this video".into();
    }
    if r.starts_with("VISIONOS CONTENT_CHECK_REQUIRED") {
        return "YouTube asks to confirm before showing this video".into();
    }
    if let Some(said) = yt("VISIONOS ERROR:") {
        return if said.is_empty() { "Video unavailable".into() } else { said };
    }
    if r.starts_with("VISIONOS UNPLAYABLE") {
        return "YouTube won't play this video here".into();
    }
    if r.starts_with("VISIONOS player") {
        return "YouTube's player service didn't answer".into();
    }
    if r.starts_with("no compatible formats") {
        return "No format this browser can play".into();
    }
    if r.starts_with("media error") || r.starts_with("appendBuffer") || r.starts_with("SourceBuffer") || r.starts_with("changeType") || r.starts_with("addSourceBuffer") || r.starts_with("no <video>") {
        return "The browser couldn't play this video".into();
    }
    if r.starts_with("live ") {
        return "This live stream isn't loading".into();
    }
    "This video isn't loading".into()
}

/// YouTube's own sentence: the reason without our `VISIONOS <STATUS>: ` prefix and our notes after it.
fn said(reason: &str) -> &str {
    let Some((_, rest)) = reason.strip_prefix("VISIONOS ").and_then(|r| r.split_once(": ")) else { return "" };
    rest.split(" · ").next().unwrap_or("").trim()
}

/// The members verdict's line (owner, 8 Nov 2026: "I'd rather not get the wantiness to buy anything": no tier,
/// no invitation).
pub const MEMBERS_LINE: &str = "This video is for this channel's members.";

/// YouTube's sentences without its invitations and, on a members verdict, its tiers.
fn plain_words(words: &str, members: bool) -> String {
    let keep = |s: &&str| {
        let low = s.to_ascii_lowercase();
        !(low.starts_with("join") || low.starts_with("become a member")) && !(members && (low.contains("join ") || low.contains("level")))
    };
    words.split_inclusive(". ").map(str::trim).filter(|s| !s.is_empty()).filter(keep).collect::<Vec<_>>().join(" ")
}

/// A verdict's one plain line under the headline: what YouTube adds beyond the headline, never an invitation.
/// Members-only: it's for this channel's members (anything else YouTube says stays: when early access ends).
pub fn line(reason: &str) -> String {
    let words = plain_words(said(reason), members_only(reason));
    if members_only(reason) {
        return if words.is_empty() { MEMBERS_LINE.into() } else { words };
    }
    if words == message(reason) { String::new() } else { words }
}

fn button(s: &mut String, act: &str, label: &str, on: bool, id: &str) {
    s.push_str("<button type=\"button\"");
    if !id.is_empty() {
        s.push_str(&format!(" id=\"{id}\""));
    }
    s.push_str(&format!(" class=\"{}\" data-act=\"{act}\">{label}</button>", if on { "pfbt on" } else { "pfbt" }));
}

/// A verdict's buttons, the primary (focused) one first: Next video when there is one, unless YouTube's page
/// may well play it.
pub fn verdict_buttons(reason: &str, next: bool) -> String {
    let mut s = String::new();
    if next && !open_helps(reason) {
        button(&mut s, ACT_NEXT, "Next video", true, "");
        button(&mut s, ACT_NATIVE, "Open on YouTube", false, "");
    } else {
        button(&mut s, ACT_NATIVE, "Open on YouTube", true, "");
        if next {
            button(&mut s, ACT_NEXT, "Next video", false, "");
        }
    }
    s
}

/// The failure panel (replaces the player box's content; the box keeps its size). `poster` is the
/// video's thumbnail, dimmed behind the panel. `made`: `Auto::made`. `next` (an up-next video exists) only
/// matters to a verdict's panel. The raw reason stays in the overlay, the reports and, on a verdict's
/// panel, `data-r`.
pub fn panel(video_id: &str, reason: &str, made: u32, next: bool) -> String {
    let mut s = String::with_capacity(reason.len() + 900);
    if crate::util::is_video_id(video_id) {
        s.push_str("<img class=\"pfp\" alt=\"\" src=\"https://i.ytimg.com/vi_webp/");
        s.push_str(video_id);
        s.push_str("/mqdefault.webp\">");
    }
    s.push_str("<div class=\"pfail\" data-pop=\"pfail\" role=\"alert\"");
    if !offers_retry(reason) {
        s.push_str(" data-r=\"");
        esc_into(&mut s, reason);
        s.push('"');
    }
    s.push_str("><p class=\"pfm\">");
    esc_into(&mut s, &message(reason));
    s.push_str("</p>");
    if offers_retry(reason) {
        s.push_str("<p class=\"pfr\"><span class=\"pfw\">");
        esc_into(&mut s, reason);
        s.push_str("</span><span class=\"pfv\">");
        esc_into(&mut s, &diagnostics(made));
        s.push_str("</span></p><div class=\"pfb\">");
        button(&mut s, ACT_RETRY, "Try again", true, RETRY_BTN);
        button(&mut s, ACT_NATIVE, "Open on YouTube", false, "");
        s.push_str("</div></div>");
        return s;
    }
    let plain = line(reason);
    if !plain.is_empty() {
        s.push_str(&format!("<p class=\"pfl\" id=\"{LINE}\">"));
        esc_into(&mut s, &plain);
        s.push_str("</p>");
    }
    s.push_str(&format!("<div class=\"pfb\" id=\"{BUTTONS}\">"));
    s.push_str(&verdict_buttons(reason, next));
    s.push_str("</div><p class=\"pfv\">");
    esc_into(&mut s, diagnostics(made).trim_start_matches(" · "));
    s.push_str("</p></div>");
    s
}

/// The stuck status (appended to the player box once; `.player.stuck` shows it). Non-blocking:
/// only its buttons take the pointer.
pub fn stuck_html() -> String {
    format!(
        "<div id=\"{STUCK}\" class=\"pstuck\" role=\"status\"><span class=\"pst\">Still loading — retrying…</span>\
<button type=\"button\" class=\"pfbt sm on\" data-act=\"{ACT_RETRY}\">Try again</button>\
<button type=\"button\" class=\"pfbt sm\" data-act=\"{ACT_NATIVE}\">Open on YouTube</button></div>"
    )
}

/// Retry schedule of a stuck episode, in ms from its base (the streams chosen, or the stall's start).
pub struct Plan {
    /// Show the status and report the episode.
    pub report: f64,
    /// A fresh player response (new URLs) at each of these.
    pub retries: &'static [f64],
    /// Still stuck: give up (the panel).
    pub give_up: f64,
}

/// No first frame: status at 4 s, fresh URLs at 6 s and 15 s (0.7.6's own refresh at 2.5 s when
/// googlevideo never answered comes before), the panel at 30 s.
pub const START: Plan = Plan { report: 4000.0, retries: &[6000.0, 15000.0], give_up: 30000.0 };
/// Buffering mid-playback (a rung down at 3 s comes before): status and fresh URLs at 15 s, again
/// at 25 s, the panel at 40 s.
pub const PLAY: Plan = Plan { report: 15000.0, retries: &[15000.0, 25000.0], give_up: 40000.0 };

#[derive(Debug, PartialEq)]
pub enum Step {
    Wait,
    Report,
    Retry,
    GiveUp,
}

/// What a stuck episode does next, `elapsed` ms after its base with `retries` fresh responses sent.
pub fn step(plan: &Plan, elapsed: f64, reported: bool, retries: usize) -> Step {
    if !reported {
        return if elapsed >= plan.report { Step::Report } else { Step::Wait };
    }
    if elapsed >= plan.give_up {
        return Step::GiveUp;
    }
    if plan.retries.get(retries).is_some_and(|&at| elapsed >= at) {
        return Step::Retry;
    }
    Step::Wait
}

/// A media error (the element's, a SourceBuffer's, a refused append, the MediaSource closed or
/// ended unexpectedly, a decoder that stopped) is recovered in place at the current position and
/// play/pause state, never shown at once (owner, 7 Nov 2026: nobody should need Try again for what
/// an automatic recovery fixes); the panel only after `MEDIA_RETRIES` within `MEDIA_WINDOW_MS`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MediaRetry {
    /// A new MediaSource and SourceBuffers on the same `<video>`, with the data already in.
    FreshSource,
    /// A new MediaSource, fresh URLs (a new player response) and the indexes asked again: what
    /// Try again does, in place.
    FreshUrls,
    /// The next format of the failing track (video: the ladder's next; audio: the next audio).
    NextFormat,
    /// The video in another codec family (AV1 → VP9/H.264), the failing one avoided from now on.
    NextCodec,
}

impl MediaRetry {
    pub fn label(self) -> &'static str {
        match self {
            MediaRetry::FreshSource => "fresh MediaSource",
            MediaRetry::FreshUrls => "fresh URLs",
            MediaRetry::NextFormat => "another format",
            MediaRetry::NextCodec => "another codec",
        }
    }
}

/// At most this many automatic recoveries within `MEDIA_WINDOW_MS` (a sliding window), then the panel.
pub const MEDIA_RETRIES: u32 = 3;
pub const MEDIA_WINDOW_MS: f64 = 120_000.0;
/// And at most this many in a player's life: errors that keep coming back slowly never loop.
pub const MEDIA_RETRIES_MAX: u32 = 8;

/// What failed, from the error's source and the browser's message (`detail`: the element's
/// `error.message`, never our own text with the MIME type in it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fault {
    /// The audio track (its SourceBuffer, its appends, or a message about audio).
    pub audio: bool,
    /// The video decoder or codec, by the browser's own words (decoding, the codec, a hardware
    /// decoder): never the codec in use alone (an AV1 parse error is a data error) nor a stall.
    pub codec: bool,
}

/// `audio`: the error came from the audio track (Some(false): the video track; None: the element).
pub fn fault(audio: Option<bool>, detail: &str) -> Fault {
    let d = detail.to_ascii_lowercase();
    let audio = audio.unwrap_or_else(|| d.contains("audio") && !d.contains("video"));
    let hint = ["decod", "codec", "vda", "videotoolbox", "dav1d", "vpx", "ffmpeg", "gpu"].iter().any(|h| d.contains(h));
    Fault { audio, codec: !audio && hint }
}

/// The recoveries within the sliding window ending at `now` (`recov`: when, what; oldest first).
pub fn recent(recov: &[(f64, MediaRetry)], now: f64) -> Vec<MediaRetry> {
    recov.iter().filter(|r| now - r.0 < MEDIA_WINDOW_MS).map(|r| r.1).collect()
}

/// The next recovery after the ones `done` within the window (oldest first) and `total` in this
/// player's life; None: the panel. Order: a fresh MediaSource (it also recreates the decoders);
/// fresh URLs; the next format. A codec fault tries another codec family before fresh URLs (new
/// URLs do not mend a decoder); URLs that are old (near their `expire`, or after a long idle) are
/// refreshed first (with a fresh MediaSource too).
pub fn media_retry(done: &[MediaRetry], total: u32, urls_old: bool, f: Fault) -> Option<MediaRetry> {
    if done.len() as u32 >= MEDIA_RETRIES || total >= MEDIA_RETRIES_MAX {
        return None;
    }
    use MediaRetry::*;
    let mut order = if f.codec { vec![FreshSource, NextCodec, FreshUrls, NextFormat] } else { vec![FreshSource, FreshUrls, NextFormat] };
    if urls_old {
        order.retain(|s| *s != FreshUrls);
        order.insert(0, FreshUrls);
    }
    order.into_iter().find(|s| !done.contains(s))
}

/// "2 automatic retries (fresh MediaSource, fresh URLs)" for reasons and incidents.
pub fn media_retried(steps: &[MediaRetry]) -> String {
    let n = steps.len();
    format!("{n} automatic {} ({})", if n == 1 { "retry" } else { "retries" }, steps.iter().map(|s| s.label()).collect::<Vec<_>>().join(", "))
}

/// URLs this close to their expiry (or past it) count as old.
pub const EXPIRY_MARGIN_S: f64 = 30.0 * 60.0;
/// A player response without `expiresInSeconds`: googlevideo URLs live about six hours.
pub const URL_LIFE_S: f64 = 5.5 * 3600.0;

/// Seconds left of URLs that arrived at `arrived_ms` (wall clock, `Date.now`) with `life_s` to live
/// (`streamingData.expiresInSeconds`, else `URL_LIFE_S`; minus a cached response's age), at `now_ms`.
/// Only the difference of two wall-clock readings is used: it counts a system sleep (the monotonic
/// clock stops during one on macOS), and a device clock that is hours off cancels out. A clock set
/// back (a negative difference) counts as run out: one refresh puts it right.
pub fn url_left(arrived_ms: f64, now_ms: f64, life_s: f64) -> f64 {
    let elapsed = now_ms - arrived_ms;
    if elapsed < 0.0 {
        return -1.0;
    }
    life_s - elapsed / 1000.0
}

/// A response's URL lifetime (s): `expiresInSeconds` when given, else `URL_LIFE_S`, minus `age_s`.
pub fn url_life(expires_in_s: Option<f64>, age_s: f64) -> f64 {
    expires_in_s.filter(|s| *s > 0.0).unwrap_or(URL_LIFE_S) - age_s
}

/// Whether URLs with `left_s` seconds to go should be replaced now.
pub fn urls_old(left_s: f64) -> bool {
    left_s < EXPIRY_MARGIN_S
}

/// Whether a proactive refresh helped: the new URLs last past the margin. If not (the answer
/// failed, was bot-checked, or brought URLs as short-lived), no more proactive refreshes for
/// this player: never a request a minute on healthy playback.
pub fn refresh_helped(ok: bool, new_left_s: f64) -> bool {
    ok && !urls_old(new_left_s)
}

/// A decoder that stopped: playing (not paused, the tab visible), media buffered ahead of the
/// playhead in both tracks, and the playhead has not moved for `DECODER_STALL_MS`.
pub const DECODER_STALL_MS: f64 = 8000.0;
pub fn decoder_stalled(still_ms: f64, ahead_v: f64, ahead_a: f64) -> bool {
    still_ms >= DECODER_STALL_MS && ahead_v >= 1.5 && ahead_a >= 1.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_errors_are_recovered_three_times_in_a_window() {
        use MediaRetry::*;
        let plain = fault(Some(false), "");
        assert_eq!(plain, Fault { audio: false, codec: false });
        assert_eq!(media_retry(&[], 0, false, plain), Some(FreshSource));
        assert_eq!(media_retry(&[FreshSource], 1, false, plain), Some(FreshUrls));
        assert_eq!(media_retry(&[FreshSource, FreshUrls], 2, false, plain), Some(NextFormat));
        assert_eq!(media_retry(&[FreshSource, FreshUrls, NextFormat], 3, false, plain), None, "then the panel");
        // The window slid on (earlier steps forgotten): from the start again, up to the life cap.
        assert_eq!(media_retry(&[], 5, false, plain), Some(FreshSource));
        assert_eq!(media_retry(&[], MEDIA_RETRIES_MAX, false, plain), None, "never a loop");
        // Old URLs (near their expiry, after a long idle): fresh URLs first.
        assert_eq!(media_retry(&[], 0, true, plain), Some(FreshUrls));
        assert_eq!(media_retry(&[FreshUrls], 1, true, plain), Some(FreshSource));
        assert_eq!(media_retried(&[FreshSource]), "1 automatic retry (fresh MediaSource)");
        assert_eq!(media_retried(&[FreshSource, FreshUrls, NextFormat]), "3 automatic retries (fresh MediaSource, fresh URLs, another format)");
        // The panel's message is unchanged by the retries' note.
        assert_eq!(message(&format!("media error 4 PipelineStatus::DEMUXER_ERROR_COULD_NOT_OPEN: x (after {})", media_retried(&[FreshSource, FreshUrls, NextFormat]))), "The browser couldn't play this video");
    }

    #[test]
    fn codec_faults_step_down_to_another_codec_family() {
        use MediaRetry::*;
        // A video error the browser puts down to the codec (an AV1 track refused, a decoder failing).
        let av1 = fault(Some(false), "PipelineStatus::CHUNK_DEMUXER_ERROR_APPEND_FAILED: Video stream codec vp9 doesn't match SourceBuffer codecs.");
        assert!(av1.codec && !av1.audio);
        assert!(!fault(Some(false), "").codec, "AV1 in use alone is no codec fault (a parse error, a stall)");
        assert_eq!(media_retry(&[], 0, false, av1), Some(FreshSource), "a fresh decoder first");
        assert_eq!(media_retry(&[FreshSource], 1, false, av1), Some(NextCodec), "then another codec, before fresh URLs");
        assert_eq!(media_retry(&[FreshSource, NextCodec], 2, false, av1), Some(FreshUrls));
        // Decoder messages of any codec; parse errors are no codec fault.
        assert!(fault(None, "PipelineStatus::PIPELINE_ERROR_DECODE: VDA Error 4").codec);
        assert!(!fault(None, "PipelineStatus::CHUNK_DEMUXER_ERROR_APPEND_FAILED: Append: stream parsing failed.").codec);
        // Audio: its own track, never a video codec change.
        let a = fault(None, "PIPELINE_ERROR_DECODE: audio decode error");
        assert!(a.audio && !a.codec);
        assert!(fault(Some(true), "codec").audio);
        assert_eq!(media_retry(&[FreshSource, FreshUrls], 2, false, a), Some(NextFormat));
    }

    #[test]
    fn old_urls_and_stopped_decoders() {
        // The URLs' own lifetime from when they came: wall-clock elapsed, so a sleep counts.
        assert_eq!(url_life(Some(21540.0), 0.0), 21540.0);
        assert_eq!(url_life(None, 0.0), URL_LIFE_S);
        assert_eq!(url_life(Some(3600.0), 1800.0), 1800.0, "a cached response is older");
        let t0 = 1.0e12; // any device clock, right or hours off: only the difference counts
        assert_eq!(url_left(t0, t0 + 3_600_000.0, 21540.0), 21540.0 - 3600.0);
        assert!(urls_old(url_left(t0, t0 + 5.6 * 3_600_000.0, 21540.0)), "a laptop asleep for 5.6 h: near the end");
        assert!(urls_old(url_left(t0, t0 - 60_000.0, 21540.0)), "the clock set back: refresh once");
        assert!(urls_old(url_left(t0, t0 + 30.0 * 86_400_000.0, 21540.0)), "a jump forward: run out");
        assert!(!urls_old(6.0 * 3600.0));
        assert!(urls_old(20.0 * 60.0), "20 min to the expiry");
        assert!(urls_old(-5.0), "expired");
        assert!(refresh_helped(true, 6.0 * 3600.0));
        assert!(!refresh_helped(false, 6.0 * 3600.0), "failed or bot-checked: no more proactive refreshes");
        assert!(!refresh_helped(true, 20.0 * 60.0), "as short-lived as before: no more either");
        assert!(decoder_stalled(DECODER_STALL_MS, 4.0, 3.0));
        assert!(!decoder_stalled(DECODER_STALL_MS - 1.0, 4.0, 3.0));
        assert!(!decoder_stalled(DECODER_STALL_MS * 2.0, 0.5, 3.0), "nothing buffered: a network stall, not the decoder");
    }

    #[test]
    fn the_window_slides_and_the_life_cap_holds() {
        use MediaRetry::*;
        let plain = fault(Some(false), "");
        let r = [(0.0, FreshSource), (10_000.0, FreshUrls), (20_000.0, NextFormat)];
        assert_eq!(recent(&r, 30_000.0), vec![FreshSource, FreshUrls, NextFormat]);
        assert_eq!(media_retry(&recent(&r, 30_000.0), 3, false, plain), None, "three within 2 min: the panel");
        assert_eq!(recent(&r, MEDIA_WINDOW_MS + 5_000.0), vec![FreshUrls, NextFormat], "the oldest slid out");
        assert_eq!(media_retry(&recent(&r, MEDIA_WINDOW_MS + 5_000.0), 3, false, plain), Some(FreshSource));
        assert_eq!(recent(&r, MEDIA_WINDOW_MS + 30_000.0), vec![]);
        for total in 0..MEDIA_RETRIES_MAX {
            assert!(media_retry(&[], total, false, plain).is_some());
        }
        assert_eq!(media_retry(&[], MEDIA_RETRIES_MAX, false, plain), None, "errors coming back slowly end at the cap");
    }

    #[test]
    fn messages_per_reason() {
        let up = format!("{}: Premieres in 2 hours", super::super::own::UPCOMING_REASON);
        assert_eq!(message(&up), "Premieres in 2 hours");
        assert_eq!(message(super::super::own::UPCOMING_REASON), "This live stream hasn't started yet");
        assert_eq!(message("VISIONOS LOGIN_REQUIRED: Sign in to confirm you’re not a bot · tried: anonymous (bot check)"), "YouTube asks to sign in to confirm you're not a bot");
        assert_eq!(message("VISIONOS LOGIN_REQUIRED: Sign in to confirm your age"), "Age-restricted — sign-in needed");
        assert_eq!(message("VISIONOS LOGIN_REQUIRED: This video is private"), "Sign-in needed to watch this video");
        assert_eq!(message("VISIONOS UNPLAYABLE: Join this channel to get access to members-only content like this video, and other exclusive perks."), "Members-only video");
        assert_eq!(message("VISIONOS UNPLAYABLE: Forced failure (e2e)"), "YouTube won't play this video here");
        assert_eq!(message("VISIONOS ERROR: This video has been removed by the uploader"), "This video has been removed by the uploader");
        assert_eq!(message("VISIONOS ERROR: "), "Video unavailable");
        assert_eq!(message("VISIONOS player HTTP 500"), "YouTube's player service didn't answer");
        assert_eq!(message("VISIONOS player: no answer in 8 s"), "YouTube's player service didn't answer");
        assert_eq!(message("no compatible formats (12 offered: av01 only)"), "No format this browser can play");
        assert_eq!(message("media error 3 PIPELINE_ERROR_DECODE"), "The browser couldn't play this video");
        assert_eq!(message("live master playlist: no variants (0 bytes)"), "This live stream isn't loading");
        assert_eq!(message("googlevideo HTTP 403 after a player refresh"), "This video isn't loading");
        assert_eq!(message(OFFLINE_REASON), "No connection — continuing when it's back");
        assert!(network_like("index of itag 139: HTTP 0 (while stuck: no first frame 4 s after the streams were chosen)"));
        assert!(network_like("VISIONOS player: no answer in 8 s"));
        assert!(!network_like("googlevideo HTTP 403 after a player refresh"));
        assert_eq!(message("index of itag 243: HTTP 503"), "This video isn't loading");
        assert_eq!(message("no first frame 30 s after the streams were chosen"), "This video isn't loading");
        assert_eq!(message("own player turned off (op: false)"), "The client's own player is turned off");
    }

    #[test]
    fn panel_escapes_and_has_both_buttons() {
        let h = panel("aqz-KE-bpKQ", "index of itag 243: <b>x</b>", 0, false);
        assert!(h.contains("&lt;b&gt;x&lt;/b&gt;") && !h.contains("<b>x"));
        assert!(h.contains("data-act=\"pf-retry\">Try again<") && h.contains("data-act=\"pf-native\">Open on YouTube<"));
        assert!(h.contains("vi_webp/aqz-KE-bpKQ/"));
        assert!(!panel("bad\"id", "r", 0, false).contains("<img"));
        assert!(stuck_html().contains("Still loading — retrying…"));
    }

    // YouTube's words, read 8 Nov 2026 (VISIONOS, hl=en, signed out).
    const MEMBERS: &str = "VISIONOS UNPLAYABLE: This video is available to this channel's members on level: Супер-Звезды (or any higher level). Join this YouTube channel from your computer or mobile app.";

    #[test]
    fn a_members_verdict_is_plain_no_tier_no_invitation() {
        assert!(members_only(MEMBERS) && permanent(MEMBERS) && !offers_retry(MEMBERS));
        assert_eq!(message(MEMBERS), "Members-only video");
        assert_eq!(line(MEMBERS), MEMBERS_LINE, "no tier, nothing to buy");
        for r in [
            "VISIONOS UNPLAYABLE: This video is available to this channel's members on level: Gold. Join this channel to get access.",
            "VISIONOS UNPLAYABLE: Join this channel to get access to members-only content like this video, and other exclusive perks.",
            "VISIONOS UNPLAYABLE: Join to get access to members-only content like this video.",
            "VISIONOS UNPLAYABLE: Become a member to watch this members-only video.",
        ] {
            assert!(members_only(r), "{r}");
            assert_eq!(line(r), MEMBERS_LINE, "{r}");
        }
        // Another sentence stays as YouTube said it: when early access ends.
        let first = "VISIONOS UNPLAYABLE: This video is for members first and goes public on 1 Dec. Join this channel to watch now.";
        assert_eq!(line(first), "This video is for members first and goes public on 1 Dec.");
        // Known phrases only: the word alone isn't a verdict, and no reason of another kind counts.
        let club = "VISIONOS UNPLAYABLE: This video contains content from Club Members Records, who has blocked it in your country on copyright grounds.";
        assert!(!members_only(club) && !members_only("VISIONOS ERROR: Our members asked us to remove this video"));
        assert_eq!(message(club), "YouTube won't play this video here");
        assert!(!crate::player::own::quiet(club), "a real failure, not an expected one");
        assert!(!members_only("googlevideo HTTP 403 after a player refresh"));
        assert!(!offers_retry(first));
        assert_eq!(line("VISIONOS UNPLAYABLE: Not in your country. Become a member of nothing."), "Not in your country.", "no invitation on any verdict");
        // The panel: no prefix, no Try again, Next video first when there is one; the raw reason for developers only.
        let v = env!("CARGO_PKG_VERSION");
        let h = panel("aqz-KE-bpKQ", MEMBERS, 0, true);
        assert!(!h.contains("Try again") && !h.contains("pfr"), "{h}");
        assert!(!h.replace("data-r=\"VISIONOS", "").contains("VISIONOS"), "{h}");
        assert!(h.contains("<p class=\"pfl\" id=\"ytc-pfl\">This video is for this channel&#39;s members.") || h.contains("<p class=\"pfl\" id=\"ytc-pfl\">This video is for this channel's members."), "{h}");
        assert!(!h.replace("data-r=", "").split("<p class=\"pfm\">").nth(1).unwrap().contains("Супер"), "the tier stays out of sight: {h}");
        assert!(h.contains(&format!("<p class=\"pfv\">{v}</p>")), "{h}");
        let (a, b) = (h.find("Next video").unwrap(), h.find("Open on YouTube").unwrap());
        assert!(a < b && h.contains("class=\"pfbt on\" data-act=\"pf-next\""), "{h}");
        let alone = panel("aqz-KE-bpKQ", MEMBERS, 0, false);
        assert!(!alone.contains("Next video") && alone.contains("class=\"pfbt on\" data-act=\"pf-native\""), "{alone}");
    }

    #[test]
    fn try_again_only_where_it_can_help() {
        let up = super::super::own::UPCOMING_REASON;
        // Verdicts: no Try again, YouTube's words without our prefix, or nothing when the headline said them.
        for (r, line_is) in [
            (format!("{up}: Premieres in 2 hours"), ""),
            (format!("{up}: Premieres in 3 days"), ""),
            (format!("{up}: This live event will begin on Nov 12, 2026 at 5:00 PM"), ""),
            (format!("{up}: Premiere will begin tomorrow"), ""),
            ("VISIONOS LOGIN_REQUIRED: Sign in to confirm your age".to_string(), "Sign in to confirm your age"),
            ("VISIONOS LOGIN_REQUIRED: This video is private".to_string(), "This video is private"),
            ("VISIONOS ERROR: This video has been removed by the uploader".to_string(), ""),
            ("VISIONOS ERROR: ".to_string(), ""),
            ("VISIONOS UNPLAYABLE: Forced failure (e2e)".to_string(), "Forced failure (e2e)"),
            ("VISIONOS CONTENT_CHECK_REQUIRED: x".to_string(), "x"),
            ("no compatible formats (12 offered: av01 only)".to_string(), ""),
            ("own player turned off (op: false)".to_string(), ""),
        ] {
            assert!(!offers_retry(&r), "{r}");
            assert_eq!(line(&r), line_is, "{r}");
            let h = panel("aqz-KE-bpKQ", &r, 0, true);
            assert!(!h.contains("Try again") && !h.contains("class=\"pfr\""), "{h}");
            assert!(h.contains("Next video") && h.contains("Open on YouTube"), "{h}");
            assert!(!h.replace(&format!("data-r=\"{}\"", crate::util::esc(&r)), "").contains("VISIONOS"), "{h}");
        }
        // Open on YouTube first where YouTube's page may play it.
        let age = verdict_buttons("VISIONOS LOGIN_REQUIRED: Sign in to confirm your age", true);
        assert!(age.find("Open on YouTube").unwrap() < age.find("Next video").unwrap() && age.contains("class=\"pfbt on\" data-act=\"pf-native\""), "{age}");
        // What can change keeps it, with the technical line and the version.
        for r in [
            "VISIONOS LOGIN_REQUIRED: Sign in to confirm you’re not a bot · tried: anonymous (bot check), fresh visitor (bot check)",
            "VISIONOS ERROR: Something went wrong",
            "VISIONOS ERROR: An error occurred. Please try again later.",
            "VISIONOS UNPLAYABLE: This content isn’t available, try again later.",
            "index of itag 243: HTTP 503",
            "no first frame 30 s after the streams were chosen (fresh URLs)",
            "VISIONOS player HTTP 500",
            OFFLINE_REASON,
        ] {
            assert!(offers_retry(r), "{r}");
            let h = panel("aqz-KE-bpKQ", r, 1, true);
            assert!(h.contains("data-act=\"pf-retry\">Try again<") && !h.contains("Next video"), "{h}");
            assert!(h.contains(&format!("· 1 automatic Try again · {}", env!("CARGO_PKG_VERSION"))), "{h}");
        }
        // A stream or a premiere that may start any moment.
        for said in ["This live event will begin in a few moments.", "Premieres in 5 minutes", "This live event will begin in 12 minutes.", "Premiere will begin shortly", "This live stream is offline.", "May begin at any time"] {
            assert!(offers_retry(&format!("{up}: {said}")), "{said}");
        }
        assert!(offers_retry(up), "nothing said: it may be about to");
    }

    #[test]
    fn permanent_reasons_go_straight_to_the_panel() {
        let up = super::super::own::UPCOMING_REASON;
        for r in [
            up.to_string(),
            format!("{up}: Premieres in 2 hours"),
            "own player turned off (op: false)".to_string(),
            "VISIONOS LOGIN_REQUIRED: Sign in to confirm your age".to_string(),
            "VISIONOS LOGIN_REQUIRED: Sign in to confirm you’re not a bot · tried: anonymous (bot check), fresh visitor (bot check)".to_string(),
            "VISIONOS UNPLAYABLE: Join this channel to get access to members-only content".to_string(),
            "VISIONOS UNPLAYABLE: Forced failure (e2e)".to_string(),
            "VISIONOS ERROR: This video has been removed by the uploader".to_string(),
            "VISIONOS CONTENT_CHECK_REQUIRED: x".to_string(),
            "no compatible formats (12 offered: av01 only)".to_string(),
            OFFLINE_REASON.to_string(),
        ] {
            assert!(permanent(&r), "{r}");
        }
        for r in [
            "SourceBuffer error (audio/webm; codecs=\"opus\")",
            "SourceBuffer error (audio/webm; codecs=\"opus\") at 1:02 (after 3 automatic retries (fresh MediaSource, fresh URLs, another format))",
            "appendBuffer: InvalidStateError",
            "media error 3 PIPELINE_ERROR_DECODE",
            "no first frame 30 s after the streams were chosen (fresh URLs)",
            "stalled for 40 s at 3:01 (fresh URLs) (while stuck: x)",
            "index of itag 243: HTTP 503",
            "index of itag 139: HTTP 0 (while stuck: no first frame 4 s after the streams were chosen)",
            "googlevideo HTTP 403 after a player refresh",
            "VISIONOS player HTTP 500",
            "VISIONOS player: no answer in 8 s",
            "VISIONOS ?: ",
            "live master playlist: no variants (0 bytes)",
            "live segment 12 itag 140 has no init segment",
            "addSourceBuffer(audio/webm) failed",
            "no <video> element",
            "something new",
        ] {
            assert!(!permanent(r), "{r}");
        }
    }

    #[test]
    fn the_automatic_try_again_backs_off_and_stops() {
        let mut a = Auto::default();
        assert_eq!(a.delay(0.0, false, Retries::Fast), Some(1000));
        a.spend(1000.0);
        assert_eq!(a.delay(2000.0, false, Retries::Fast), Some(4000));
        a.spend(6000.0);
        assert_eq!(a.delay(7000.0, true, Retries::Fast), Some(10_000), "mid-video or not: the same");
        a.spend(17_000.0);
        assert_eq!(a.delay(18_000.0, false, Retries::Fast), None, "three within the window: the panel");
        assert_eq!(a.made(), 3);
        // Nothing spent while it only waits (a dropped connection ends the wait).
        let mut w = Auto::default();
        assert_eq!(w.delay(0.0, false, Retries::Fast), Some(1000));
        assert_eq!(w.delay(10.0, false, Retries::Fast), Some(1000));
        assert_eq!(w.made(), 0);
        // The window slides: failures far apart each get their retries, up to the cap.
        let mut s = Auto::default();
        let mut now = 0.0;
        for i in 0..AUTO_MAX {
            assert!(s.delay(now, true, Retries::Fast).is_some(), "retry {i}");
            s.spend(now);
            now += AUTO_WINDOW_MS + 1.0;
        }
        assert_eq!(s.delay(now, true, Retries::Fast), None, "never a loop");
        assert_eq!(s.delay(now + 1e9, true, Retries::Fast), None);
    }

    #[test]
    fn the_player_service_is_not_hammered() {
        assert_eq!(retries("VISIONOS player HTTP 429", false), Retries::Once(RATE_LIMIT_WAIT_MS));
        assert!(RATE_LIMIT_WAIT_MS >= 60_000);
        assert_eq!(retries("VISIONOS player HTTP 429 (while stuck: x)", true), Retries::Once(RATE_LIMIT_WAIT_MS));
        assert_eq!(retries("VISIONOS player HTTP 403", false), Retries::Once(REFUSED_WAIT_MS));
        assert_eq!(retries("VISIONOS player HTTP 400", false), Retries::Once(REFUSED_WAIT_MS));
        assert_eq!(retries("VISIONOS player HTTP 500", false), Retries::Fast);
        assert_eq!(retries("VISIONOS player HTTP 0", false), Retries::Fast);
        assert_eq!(retries("VISIONOS player: no answer in 8 s", false), Retries::Fast);
        // One, and only when nothing else went out in the window.
        let mut a = Auto::default();
        assert_eq!(a.delay(0.0, false, Retries::Once(RATE_LIMIT_WAIT_MS)), Some(60_000));
        a.spend(60_000.0);
        assert_eq!(a.delay(60_100.0, false, Retries::Once(RATE_LIMIT_WAIT_MS)), None, "the panel, not a second one");
        let mut b = Auto::default();
        b.spend(0.0);
        assert_eq!(b.delay(100.0, false, Retries::Once(REFUSED_WAIT_MS)), None);
        assert_eq!(b.delay(AUTO_WINDOW_MS + 1.0, false, Retries::Once(REFUSED_WAIT_MS)), Some(REFUSED_WAIT_MS), "the window slid");
        assert_eq!(Auto::default().delay(0.0, true, Retries::Never), None);
    }

    #[test]
    fn a_return_to_the_tab_respects_the_rate_limit() {
        let mut a = Auto::default();
        assert!(a.may_return(1000.0, None));
        a.failed("VISIONOS player HTTP 500", 1000.0);
        assert!(a.may_return(1000.0, None), "only 429 holds a return back");
        a.failed("VISIONOS player HTTP 429", 1000.0);
        assert!(!a.may_return(1000.0, None), "just refused");
        assert!(!a.may_return(1000.0 + f64::from(RATE_LIMIT_WAIT_MS) - 1.0, None));
        assert!(a.may_return(1000.0 + f64::from(RATE_LIMIT_WAIT_MS), None), "a minute later");
        a.failed("VISIONOS player HTTP 429 (while stuck: x)", 90_000.0);
        assert!(!a.may_return(90_001.0, Some(RETURN_AWAY_MS)), "the focus path too");
    }

    #[test]
    fn a_verdict_during_a_refresh_of_a_playing_video_gets_one_retry() {
        let e = "VISIONOS ERROR: Something went wrong";
        assert_eq!(retries(e, true), Retries::Once(AUTO_DELAYS_MS[0]));
        assert_eq!(retries(e, false), Retries::Never, "at a start it is the verdict");
        assert_eq!(retries("VISIONOS LOGIN_REQUIRED: Sign in to confirm you’re not a bot", true), Retries::Never);
        assert_eq!(retries("VISIONOS UNPLAYABLE: x", true), Retries::Never);
        assert_eq!(retries("VISIONOS ERROR: for members only", true), Retries::Never);
        assert_eq!(retries("no compatible formats (x)", true), Retries::Never);
        assert_eq!(retries("googlevideo HTTP 403 after a player refresh", true), Retries::Fast);
    }

    #[test]
    fn coming_back_retries_once_per_return_with_a_cap() {
        let mut a = Auto::default();
        assert!(a.may_return(0.0, None), "the tab became visible");
        assert!(!a.may_return(0.0, Some(RETURN_AWAY_MS - 1.0)), "a short trip to another window");
        assert!(a.may_return(0.0, Some(RETURN_AWAY_MS)), "a long one");
        a.returned(1000.0);
        assert!(!a.may_return(1000.0 + RETURN_GAP_MS - 1.0, None), "visibility and focus of one return: one retry");
        assert!(a.may_return(1000.0 + RETURN_GAP_MS, None));
        // The return's retry fails before a picture: the panel again, no chain of its own.
        assert_eq!(a.delay(2000.0, false, Retries::Fast), None);
        assert_eq!(a.made(), 1);
        // It played and fails later: an ordinary failure.
        assert_eq!(a.delay(3000.0, true, Retries::Fast), Some(1000));
        assert_eq!(a.delay(4000.0, false, Retries::Fast), Some(1000), "the return is over");
        let mut c = Auto::default();
        let mut now = 0.0;
        for _ in 0..RETURN_MAX {
            assert!(c.may_return(now, None));
            c.returned(now);
            now += RETURN_GAP_MS;
        }
        assert!(!c.may_return(now, None), "the cap");
    }

    #[test]
    fn a_stale_tab_reloads_once() {
        assert!(stale_may_reload("", 1000.0));
        assert!(stale_may_reload("garbage", 1000.0));
        let m = stale_mark(1000.0, true, "aqz-KE-bpKQ");
        assert_eq!(m, "1000|1|aqz-KE-bpKQ");
        assert!(!stale_may_reload(&m, 1000.0 + STALE_GUARD_MS - 1.0), "reloaded: not again");
        assert!(stale_may_reload(&m, 1000.0 + STALE_GUARD_MS));
        assert!(stale_may_reload(&m, 999.0), "a clock set back");
        assert!(stale_paused(&m, 2000.0, "aqz-KE-bpKQ"));
        assert!(!stale_paused(&m, 2000.0, "otherVideo1"), "another video");
        assert!(!stale_paused(&m, 1000.0 + STALE_PAUSED_MS, "aqz-KE-bpKQ"), "long ago");
        assert!(!stale_paused(&stale_mark(1000.0, false, "aqz-KE-bpKQ"), 2000.0, "aqz-KE-bpKQ"));
        assert!(!stale_paused("", 2000.0, "aqz-KE-bpKQ"));
    }

    #[test]
    fn the_technical_line_names_the_version_and_the_retries() {
        let v = env!("CARGO_PKG_VERSION");
        assert_eq!(diagnostics(0), format!(" · {v}"));
        assert_eq!(diagnostics(1), format!(" · 1 automatic Try again · {v}"));
        assert_eq!(diagnostics(3), format!(" · 3 automatic Try agains · {v}"));
        let reason = "SourceBuffer error (audio/webm; codecs=\"opus\")";
        let h = panel("aqz-KE-bpKQ", reason, 3, false);
        assert!(h.contains(&format!("<span class=\"pfv\"> · 3 automatic Try agains · {v}</span>")), "{h}");
        // The headline comes from the reason alone, in every class of reason.
        assert!(h.contains("The browser couldn"), "{h}");
        let up = format!("{}: Premieres in 2 hours", super::super::own::UPCOMING_REASON);
        assert!(panel("aqz-KE-bpKQ", &up, 0, false).contains("<p class=\"pfm\">Premieres in 2 hours</p>"));
        assert!(panel("aqz-KE-bpKQ", "VISIONOS ERROR: Gone · x", 0, false).contains("<p class=\"pfm\">Gone</p>"));
    }

    #[test]
    fn retry_schedule() {
        assert_eq!(step(&START, 3999.0, false, 0), Step::Wait);
        assert_eq!(step(&START, 4000.0, false, 0), Step::Report);
        assert_eq!(step(&START, 5000.0, true, 0), Step::Wait);
        assert_eq!(step(&START, 6000.0, true, 0), Step::Retry);
        assert_eq!(step(&START, 9000.0, true, 1), Step::Wait);
        assert_eq!(step(&START, 15000.0, true, 1), Step::Retry);
        assert_eq!(step(&START, 20000.0, true, 2), Step::Wait);
        assert_eq!(step(&START, 30000.0, true, 2), Step::GiveUp);
        // A late report (tab shown late) still retries before it gives up.
        assert_eq!(step(&PLAY, 15000.0, false, 0), Step::Report);
        assert_eq!(step(&PLAY, 15250.0, true, 0), Step::Retry);
        assert_eq!(step(&PLAY, 25000.0, true, 1), Step::Retry);
        assert_eq!(step(&PLAY, 40000.0, true, 2), Step::GiveUp);
    }
}
