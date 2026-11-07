//! Player failures and stalls stay inside the player (since 2 Nov 2026: always the client's own
//! player, so every problem can be seen and inspected where it happens). The client never leaves for
//! YouTube's page by itself: a failure shows a calm panel in the player box (a human message,
//! the technical reason, Try again / Open on YouTube), and a stall shows a small "Still
//! loading — retrying…" status while the player keeps retrying in place. Pure parts here
//! (messages, markup, the retry schedule); `own.rs` and `app.rs` drive them.

use crate::util::esc_into;

/// The panel's buttons and the stuck status' buttons (handled by the app, not the player).
pub const ACT_RETRY: &str = "pf-retry";
pub const ACT_NATIVE: &str = "pf-native";
/// The persistent status element inside the player box (shown by `.player.stuck`).
pub const STUCK: &str = "ytc-pstuck";
/// The panel's Try again button (focused when the panel replaces a focused control).
pub const RETRY_BTN: &str = "ytc-pfretry";

/// The panel's reason while the connection is down (the app probes and continues by itself).
pub const OFFLINE_REASON: &str = "offline: youtube.com isn't answering (connection or DNS); continuing when it's back";

/// A failure that may just be a dropped connection: requests that never got an answer.
pub fn network_like(reason: &str) -> bool {
    reason.contains("HTTP 0") || reason.contains("no answer")
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
    if low.contains("members") {
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

/// The failure panel (replaces the player box's content; the box keeps its size). `poster` is the
/// video's thumbnail, dimmed behind the panel.
pub fn panel(video_id: &str, reason: &str) -> String {
    let mut s = String::with_capacity(reason.len() + 900);
    if crate::util::is_video_id(video_id) {
        s.push_str("<img class=\"pfp\" alt=\"\" src=\"https://i.ytimg.com/vi_webp/");
        s.push_str(video_id);
        s.push_str("/mqdefault.webp\">");
    }
    s.push_str("<div class=\"pfail\" role=\"alert\"><p class=\"pfm\">");
    esc_into(&mut s, &message(reason));
    s.push_str("</p><p class=\"pfr\">");
    esc_into(&mut s, reason);
    s.push_str("</p><div class=\"pfb\">");
    s.push_str(&format!("<button type=\"button\" id=\"{RETRY_BTN}\" class=\"pfbt on\" data-act=\"{ACT_RETRY}\">Try again</button>"));
    s.push_str(&format!("<button type=\"button\" class=\"pfbt\" data-act=\"{ACT_NATIVE}\">Open on YouTube</button>"));
    s.push_str("</div></div>");
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
        let h = panel("aqz-KE-bpKQ", "VISIONOS UNPLAYABLE: <b>x</b>");
        assert!(h.contains("&lt;b&gt;x&lt;/b&gt;") && !h.contains("<b>x"));
        assert!(h.contains("data-act=\"pf-retry\">Try again<") && h.contains("data-act=\"pf-native\">Open on YouTube<"));
        assert!(h.contains("vi_webp/aqz-KE-bpKQ/"));
        assert!(!panel("bad\"id", "r").contains("<img"));
        assert!(stuck_html().contains("Still loading — retrying…"));
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
