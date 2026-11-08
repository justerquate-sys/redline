//! The player boundary: everything the app needs to show a video. Our own
//! MSE player (`own`) plays VISIONOS streams in the page; when it cannot, the
//! failure stays in the player box (`fail`: a panel with Try again and Open on
//! YouTube; the client never leaves for YouTube's page by itself, since
//! 2 Nov 2026). `native_url` is the manual button's target. YouTube's embed is not
//! an option on youtube.com: it refuses to play there (error 152-4).
//! Pure logic lives in the submodules and is unit tested; `own` is the glue
//! to the generic bridge.

pub mod captions;
pub mod fail;
pub mod formats;
pub mod frag;
pub mod index;
pub mod live;
pub mod own;
pub mod sponsor;
pub mod sched;
pub mod stats;
pub mod storyboard;
pub mod throttle;
pub mod ui;

use crate::util::{is_safe_id, is_video_id};

pub use own::{Ctx, Out, Own, Vision};

/// Player box markup for a watch page (with the stuck status, hidden until `.player.stuck`).
pub fn html(video_id: &str) -> String {
    if !is_video_id(video_id) {
        return String::new();
    }
    (if crate::tv::on() { ui::shell_tv() } else { ui::shell() }) + &fail::stuck_html()
}

/// The native watch page at `t` seconds (`yt-native=1` = no client, YouTube's
/// own player; allowed by rules.json and excluded from the content script).
/// Only ever opened by the user (the panel's Open on YouTube).
pub fn native_url(video_id: &str, t: f64, list: Option<&str>) -> String {
    watch_url(video_id, t, list) + "&yt-native=1"
}

/// The client's own watch page of a video at time `t`.
pub fn watch_url(video_id: &str, t: f64, list: Option<&str>) -> String {
    let mut u = format!("/watch?v={video_id}");
    let s = if t.is_finite() && t >= 1.0 { t.floor() as u64 } else { 0 };
    if s > 0 {
        u.push_str(&format!("&t={s}s"));
    }
    if let Some(l) = list.filter(|l| is_safe_id(l)) {
        u.push_str("&list=");
        u.push_str(l);
    }
    u
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_fallback_url() {
        assert_eq!(super::native_url("-Bxc4gJluC0", 65.7, Some("WL")), "/watch?v=-Bxc4gJluC0&t=65s&list=WL&yt-native=1");
        assert_eq!(super::native_url("-Bxc4gJluC0", 0.3, Some("bad\"")), "/watch?v=-Bxc4gJluC0&yt-native=1");
        assert!(super::html("bad\"id").is_empty());
    }
}
