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

/// A media error before the first frame (the element's, a SourceBuffer's, a refused append) is
/// retried in place, never shown at once (owner, 7 Nov 2026: nobody should need Try again for what
/// an automatic retry fixes), in this order; the panel only after the last.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MediaRetry {
    /// A new MediaSource and SourceBuffers on the same `<video>`, with the data already in.
    FreshSource,
    /// A new MediaSource, fresh URLs (a new player response) and the indexes asked again: what
    /// Try again does, in place.
    FreshUrls,
    /// The next video format of the ladder, in a new MediaSource.
    NextFormat,
}

/// The automatic retries a start gets after media errors.
pub const MEDIA_RETRIES: u32 = 3;

/// The retry after `done` earlier ones; None: the panel (also once the picture was there: a media
/// error mid-playback is no start failure).
pub fn media_retry(done: u32, first_frame: bool) -> Option<MediaRetry> {
    if first_frame {
        return None;
    }
    match done {
        0 => Some(MediaRetry::FreshSource),
        1 => Some(MediaRetry::FreshUrls),
        2 => Some(MediaRetry::NextFormat),
        _ => None,
    }
}

/// "2 automatic retries (fresh MediaSource, fresh URLs)" for reasons and incidents.
pub fn media_retried(n: u32) -> String {
    let steps = ["fresh MediaSource", "fresh URLs", "another format"];
    let n = n.min(steps.len() as u32);
    format!("{n} automatic {} ({})", if n == 1 { "retry" } else { "retries" }, steps[..n as usize].join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_errors_before_the_first_frame_are_retried_three_times() {
        assert_eq!(media_retry(0, false), Some(MediaRetry::FreshSource));
        assert_eq!(media_retry(1, false), Some(MediaRetry::FreshUrls));
        assert_eq!(media_retry(2, false), Some(MediaRetry::NextFormat));
        assert_eq!(media_retry(MEDIA_RETRIES, false), None, "then the panel");
        assert_eq!(media_retry(0, true), None, "mid-playback: no start retry");
        assert_eq!(media_retried(1), "1 automatic retry (fresh MediaSource)");
        assert_eq!(media_retried(3), "3 automatic retries (fresh MediaSource, fresh URLs, another format)");
        // The panel's message is unchanged by the retries' note.
        assert_eq!(message(&format!("media error 4 PipelineStatus::DEMUXER_ERROR_COULD_NOT_OPEN: x (after {})", media_retried(3))), "The browser couldn't play this video");
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
