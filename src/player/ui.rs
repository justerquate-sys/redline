//! Player markup and the menu interaction rules. Dynamic parts (time,
//! progress, captions) are updated with `text`/`css_var`, never by
//! re-rendering; state is a class list on the player box. Everything here is
//! pure (no bridge calls) and unit tested; `own.rs` is the glue.

use crate::util::esc_into;

pub const BOX: &str = "ytc-player";
pub const VIDEO: &str = "ytc-video";
/// Caption wrapper (placement) and its text / size range.
pub const CCW: &str = "ytc-ccw";
pub const CC: &str = "ytc-cc";
pub const CCS: &str = "ytc-ccs";
pub const CCV: &str = "ytc-ccv";
pub const TIME: &str = "ytc-time";
pub const MENU: &str = "ytc-menu";
/// The menu panel's content box (`MENU` itself carries the surface, which morphs between menus).
pub const MENUI: &str = "ytc-menui";
/// Selection marks inside the menus (`bridge::mark_slide` scopes): the speed list, the quality list, codecs,
/// CC list. The speed and quality lists are also the boxes that scroll (short players) and take the edge nudge.
pub const MPRE: &str = "ytc-mpre";
pub const MQ: &str = "ytc-mq";
pub const MCO: &str = "ytc-mco";
pub const MLS: &str = "ytc-mls";
/// The center pulse a pointer play/pause leaves on the video, and the seek bar's press ripple.
pub const PFX: &str = "ytc-pfx";
pub const RIP: &str = "ytc-rip";
pub const VOL: &str = "ytc-vol";
/// Toolbar shortcuts (menu openers; focus returns here on Escape).
pub const CCB: &str = "ytc-ccb";
pub const SPEED: &str = "ytc-speed";
pub const QUAL: &str = "ytc-qual";
/// The Speed shortcut's label: the current rate (`1×`, `1.25×`).
pub const RATEB: &str = "ytc-rateb";
/// The Quality shortcut's label: the rung playing now (`720`, `1440`, `4K`).
pub const QLBL: &str = "ytc-qlbl";
/// Speed menu: the exact-rate readout between − and + (the wheel over it steps 0.05).
pub const RATE: &str = "ytc-rate";
pub const TOAST: &str = "ytc-toast";
/// The seek bar: carries `--p`/`--b` (so a progress update restyles 4 elements, not the whole player).
pub const SEEK: &str = "ytc-seek";
/// The seek bar's segments (one `.pbar` per chapter, or one for the whole video).
pub const SEGS: &str = "ytc-segs";
/// Segment ids: `ytc-sg0`, `ytc-sg1`… (the hovered one carries `.on`).
pub const SEG: &str = "ytc-sg";
/// Hover tooltip over the seek bar: chapter title and time.
pub const TIPC: &str = "ytc-tipc";
/// The tooltip (its surface morphs between chapter titles) and its content box.
pub const TIP: &str = "ytc-tip";
pub const TIPI: &str = "ytc-tipi";
pub const TIPT: &str = "ytc-tipt";
/// The control bar's current-chapter button (`data-n` = chapter count) and its label.
pub const CHB: &str = "ytc-chb";
pub const CHL: &str = "ytc-chl";
/// The chapter panel under the description (`actions::watch::chapters_html`):
/// the panel (`.ych`, `.open` when expanded), its `<ol>` and item id prefix (`ytc-chi3`),
/// each row's frame picture (`ytc-chf3`).
pub const CHD: &str = "ytc-chd";
pub const CHOL: &str = "ytc-chol";
pub const CHI: &str = "ytc-chi";
pub const CHF: &str = "ytc-chf";
/// The compact row (a button: opens/closes the panel) and its parts: frame
/// picture, text block (slides on a change), title, start time, "Chapter 3 of 30".
pub const CHH: &str = "ytc-chh";
pub const CHHF: &str = "ytc-chhf";
pub const CHHX: &str = "ytc-chhx";
pub const CHHT: &str = "ytc-chht";
pub const CHHC: &str = "ytc-chhc";
pub const CHHN: &str = "ytc-chhn";
/// The seek tooltip's storyboard frame picture (shown with `.player.sb`).
pub const TIPF: &str = "ytc-tipf";
/// TV's remote-operated Captions On/Off switch; desktop toggles on the CC shortcut.
pub const MSW: &str = "ytc-msw";
/// SponsorBlock: the skip toast (label + Undo/Jump button) and the
/// tooltip's category label (`player::sponsor`).
pub const SBT: &str = "ytc-sbt";
pub const SBTL: &str = "ytc-sbtl";
pub const SBTB: &str = "ytc-sbtb";
pub const TIPK: &str = "ytc-tipk";
/// The progress fill glides linearly between updates while playing; Rust aims
/// `--p` this far ahead (same duration as `.player.glide .ppos` in client.css).
pub const GLIDE_MS: f64 = 250.0;

// 24px icons. `o` = outline family (stroke via CSS); play/pause stay filled. Every control that changes state
// keeps one persistent icon whose parts change form in CSS (client.css "Icon morphs"): SVG `d` paths with
// matching commands interpolate between the two shapes, so play becomes pause, the theater frame widens and the
// fullscreen corners turn inward on the house spring; keyboard changes skip it (`.player.kf`).
const PLAY: &str = "<svg class=\"i-play\" viewBox=\"0 0 24 24\"><path d=\"M8 5.5v13a1 1 0 0 0 1.5.86l10.5-6.5a1 1 0 0 0 0-1.72L9.5 4.64A1 1 0 0 0 8 5.5z\"/></svg>";
const PAUSE: &str = "<svg class=\"i-pause\" viewBox=\"0 0 24 24\"><rect x=\"6\" y=\"5\" width=\"4\" height=\"14\" rx=\"1.2\"/><rect x=\"14\" y=\"5\" width=\"4\" height=\"14\" rx=\"1.2\"/></svg>";
/// Play ⇄ pause: the triangle's two halves become the two bars (shapes in client.css).
const PLAYPAUSE: &str = "<svg class=\"i-pp\" viewBox=\"0 0 24 24\"><path class=\"p1\"/><path class=\"p2\"/></svg>";
/// Speaker with level waves (`.vlo` hides the outer one) and the mute cross.
const VOLUME: &str = "<svg class=\"o i-vm\" viewBox=\"0 0 24 24\"><path d=\"M4 9.5v5h3.5l4.5 4v-13l-4.5 4z\"/><path class=\"w1\" d=\"M15.5 9a4 4 0 0 1 0 6\"/><path class=\"w2\" d=\"M18.5 6.5a7.5 7.5 0 0 1 0 11\"/><path class=\"vx\" d=\"M16 9.5l5 5m0-5-5 5\"/></svg>";
/// CC off: thin single-stroke frame with open C strokes; on: filled body with
/// cut-out letters (the old extension's artwork). Stacked; turning on wipes the filled one in.
const CC_OFF: &str = "<svg class=\"o i-ccoff\" viewBox=\"0 0 24 24\"><path d=\"M5 5h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2ZM10 9H8a1.5 1.5 0 0 0-1.5 1.5v3A1.5 1.5 0 0 0 8 15h2M17.5 9h-2a1.5 1.5 0 0 0-1.5 1.5v3a1.5 1.5 0 0 0 1.5 1.5h2\"/></svg>";
const CC_ON: &str = "<svg class=\"i-ccon\" viewBox=\"0 0 24 24\"><path fill-rule=\"evenodd\" d=\"M5 5h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2ZM11 8H9a3 3 0 0 0-3 3v2a3 3 0 0 0 3 3h2v-2H9a1 1 0 0 1-1-1v-2a1 1 0 0 1 1-1h2ZM18 8H16a3 3 0 0 0-3 3v2a3 3 0 0 0 3 3h2v-2H16a1 1 0 0 1-1-1v-2a1 1 0 0 1 1-1h2Z\"/></svg>";
/// Theater: one rounded frame, wide (default view) ⇄ narrow (in theater).
const THEATER: &str = "<svg class=\"o i-th\" viewBox=\"0 0 24 24\"><path/></svg>";
/// Fullscreen: four corners that fold inward in fullscreen.
const FULL: &str = "<svg class=\"o i-fs\" viewBox=\"0 0 24 24\"><path/></svg>";

/// The player's inner markup (inside the 16:9 `#ytc-player` box, so there is
/// no layout shift between loading, playing and errors).
/// No loading indicator (1 Nov 2026) and no poster (3 Nov 2026): the box is the dark player surface
/// until the first frame, or shows the warm first frame (`own/warm.rs`) from the start; never the
/// 320 px thumbnail scaled up to the player.
pub fn shell() -> String {
    format!(
        "<video id=\"{VIDEO}\" playsinline preload=\"auto\"></video>\
<div class=\"pov\" data-act=\"p-toggle\" data-arg=\"v\" data-dbl=\"p-fs\"></div>\
<div id=\"{CCW}\" class=\"pcw\"><div id=\"{CC}\" class=\"pcc\" data-drag=\"cc\" data-drag-box=\"{BOX}\" aria-live=\"off\"></div>\
<div class=\"pct\"><input id=\"{CCS}\" class=\"pcs\" type=\"range\" min=\"0.5\" max=\"2\" step=\"0.05\" value=\"1\" data-input=\"ccs\" aria-label=\"Caption size\"><output id=\"{CCV}\" class=\"pcv\">100%</output></div></div>\
<div class=\"pbig\" data-act=\"p-toggle\" data-arg=\"v\">{PLAY}</div>\
<div id=\"{PFX}\" class=\"pfx\" aria-hidden=\"true\">{PLAY}{PAUSE}</div>\
<div id=\"{TOAST}\" class=\"ptoast\"></div>\
<div id=\"{SBT}\" class=\"psbt\" role=\"status\"><span id=\"{SBTL}\"></span><button type=\"button\" id=\"{SBTB}\" data-act=\"p-sb\" data-arg=\"undo\" tabindex=\"-1\">Undo</button></div>\
<div class=\"pctl\">\
<div id=\"{SEEK}\" class=\"pseek\" data-drag=\"seek\" data-track=\"seek\"><div id=\"{SEGS}\" class=\"psegs\">{BAR}</div><i id=\"{RIP}\" class=\"prip\"></i><b class=\"pknob\"></b>\
<div id=\"{TIP}\" class=\"ptip\" aria-hidden=\"true\"><div id=\"{TIPI}\" class=\"pti\"><i class=\"sbf ptf\"><i id=\"{TIPF}\"></i></i><em id=\"{TIPK}\" class=\"ptk\"></em><b id=\"{TIPC}\"></b><span id=\"{TIPT}\"></span></div></div></div>\
<div class=\"prow\">\
<button type=\"button\" class=\"pb\" data-act=\"p-toggle\" aria-label=\"Play/pause (k)\">{PLAYPAUSE}</button>\
<button type=\"button\" class=\"pb\" data-act=\"p-mute\" aria-label=\"Mute (m)\">{VOLUME}</button>\
<input id=\"{VOL}\" class=\"pvol\" type=\"range\" min=\"0\" max=\"1\" step=\"0.02\" value=\"1\" data-input=\"vol\" aria-label=\"Volume\">\
<button type=\"button\" class=\"plive\" data-act=\"p-live\" aria-label=\"Jump to live\">LIVE</button>\
<span id=\"{TIME}\" class=\"ptime\">0:00 / 0:00</span>\
<button type=\"button\" id=\"{CHB}\" class=\"pchap\" data-n=\"0\" data-act=\"p-chap\" aria-label=\"Chapters\"><span id=\"{CHL}\" class=\"pchl\"></span></button><span class=\"pfill\"></span>\
<button type=\"button\" class=\"pb\" id=\"{CCB}\" data-act=\"p-cc\" data-arg=\"cc\" data-hov=\"tb\" aria-pressed=\"false\" aria-haspopup=\"dialog\" aria-controls=\"{MENU}\" aria-expanded=\"false\" aria-keyshortcuts=\"c ArrowUp ArrowDown\" aria-label=\"Captions (c); Up or Down for options\">{CC_OFF}{CC_ON}</button>\
<button type=\"button\" class=\"pb pw\" id=\"{SPEED}\" data-act=\"p-menu\" data-arg=\"speed\" data-hov=\"tb\" data-wheel=\"speed\" aria-haspopup=\"true\" aria-label=\"Speed (&lt; &gt;)\"><span id=\"{RATEB}\" class=\"plbl\">1×</span></button>\
<button type=\"button\" class=\"pb pw\" id=\"{QUAL}\" data-act=\"p-menu\" data-arg=\"quality\" data-hov=\"tb\" data-wheel=\"quality\" aria-haspopup=\"true\" aria-label=\"Quality\"><span id=\"{QLBL}\" class=\"plbl pq\">Auto</span></button>\
<button type=\"button\" class=\"pb\" data-act=\"p-theater\" aria-label=\"Theater (t)\">{THEATER}</button>\
<button type=\"button\" class=\"pb\" data-act=\"p-fs\" aria-label=\"Fullscreen (f)\">{FULL}</button>\
</div></div><div id=\"{MENU}\" class=\"pmenu\" data-hov=\"menu\" role=\"dialog\"><div id=\"{MENUI}\" class=\"pmi\"></div></div>"
    )
}

/// TV: the remaining time (the bar's right end; `TIME` is the elapsed time on its left).
pub const TREM: &str = "ytc-trem";
/// TV: the slot the Watch later toggle is written into (the app owns that button, `features::watch_later`).
pub const TVWL: &str = "ytc-tvwl";
/// TV: the slots of the Channel and Subscribe actions (they come with the watch page's data).
pub const TVCH: &str = "ytc-tvch";
pub const TVSUB: &str = "ytc-tvsubr";
/// TV: the Audio action (`.on` while the video has several tracks), Like/Dislike and Save to playlist.
pub const AUB: &str = "ytc-aub";
pub const TVLK: &str = "ytc-tvlk";
pub const TVDL: &str = "ytc-tvdl";
pub const TVSV: &str = "ytc-tvsv";
/// TV: the focused labels of Like and Dislike ("Liked", "Disliked" while pressed).
pub const TVLK_L: &str = "ytc-tvlk-l";
pub const TVDL_L: &str = "ytc-tvdl-l";
const I_LIKE: &str = "<svg class=\"o\" viewBox=\"0 0 24 24\"><path d=\"M7.5 10.5V20H4v-9.5zm0 0 3.8-6.5c1.4 0 2.4 1 2.1 2.5l-.7 3.5H19a2 2 0 0 1 2 2.3l-1.1 6a2 2 0 0 1-2 1.7H7.5\"/></svg>";
const I_INFO: &str = "<svg class=\"o\" viewBox=\"0 0 24 24\"><path d=\"M20.5 12a8.5 8.5 0 1 1-17 0 8.5 8.5 0 0 1 17 0zM12 11v5.5M12 7.6v.1\"/></svg>";
/// Headphones (the Audio action).
const I_AUDIO: &str = "<svg class=\"o\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M4 15v-3a8 8 0 0 1 16 0v3M4 14.5h3.5V20H5a1 1 0 0 1-1-1zM20 14.5h-3.5V20H19a1 1 0 0 0 1-1z\"/></svg>";
/// A list with a plus (Save to playlist).
const I_SAVE: &str = "<svg class=\"o\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M4 6h14M4 11h14M4 16h8M17 14v6M14 17h6\"/></svg>";
const I_COMMENTS: &str = "<svg class=\"o\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M20 4H4v13h4v4l5-4h7zM8 8h8M8 12h6\"/></svg>";

/// The TV player (10-foot overlay): the same ids as `shell` where the glue updates them, laid out for the
/// remote. Over the picture: the time row (elapsed · chapter · remaining), the thick seek bar (a focus stop:
/// ←/→ scrub it), and one row of round buttons (channel, subscribe, play/pause, captions, audio, speed, quality, like,
/// dislike, Watch later, save, comments, more info). No volume, theater, fullscreen, hover tooltips or caption sizing
/// (pointer-only on desktop).
pub fn shell_tv() -> String {
    format!(
        "<video id=\"{VIDEO}\" playsinline preload=\"auto\"></video>\
<div class=\"pov\" data-act=\"p-toggle\" data-arg=\"v\"></div>\
<div id=\"{CCW}\" class=\"pcw\"><div id=\"{CC}\" class=\"pcc\" aria-live=\"off\"></div></div>\
<div class=\"pbig\" data-act=\"p-toggle\" data-arg=\"v\">{PLAY}</div>\
<div id=\"{PFX}\" class=\"pfx\" aria-hidden=\"true\">{PLAY}{PAUSE}</div>\
<div id=\"{TOAST}\" class=\"ptoast\"></div>\
<div id=\"{SBT}\" class=\"psbt\" role=\"status\"><span id=\"{SBTL}\"></span><button type=\"button\" id=\"{SBTB}\" data-act=\"p-sb\" data-arg=\"undo\">Undo</button></div>\
<div class=\"pctl\">\
<div class=\"ptr\"><span id=\"{TIME}\" class=\"ptime\">0:00</span><span id=\"{CHB}\" class=\"pchap\" data-n=\"0\"><span id=\"{CHL}\" class=\"pchl\"></span></span><button type=\"button\" class=\"plive\" data-act=\"p-live\" aria-label=\"Jump to live\">LIVE</button><span id=\"{TREM}\" class=\"prem\"></span></div>\
<div id=\"{SEEK}\" class=\"pseek\" tabindex=\"0\" role=\"slider\" aria-label=\"Seek: ← and → move 10 s, OK plays or pauses\" data-drag=\"seek\"><div id=\"{SEGS}\" class=\"psegs\">{BAR}</div><i id=\"{RIP}\" class=\"prip\"></i><b class=\"pknob\"></b>\
<div id=\"{TIP}\" class=\"ptip\" aria-hidden=\"true\"><div id=\"{TIPI}\" class=\"pti\"><i class=\"sbf ptf\"><i id=\"{TIPF}\"></i></i><em id=\"{TIPK}\" class=\"ptk\"></em><b id=\"{TIPC}\"></b><span id=\"{TIPT}\"></span></div></div></div>\
<div class=\"prow\"><span id=\"{TVCH}\" class=\"pslot\"></span><span id=\"{TVSUB}\" class=\"pslot\"></span>\
<button type=\"button\" class=\"pb\" data-act=\"p-toggle\" aria-label=\"Play/pause\">{PLAYPAUSE}<span class=\"tvpl\" aria-hidden=\"true\">Play / pause</span></button>\
<button type=\"button\" class=\"pb\" id=\"{CCB}\" data-act=\"p-menu\" data-arg=\"cc\" aria-haspopup=\"true\" aria-label=\"Captions\">{CC_OFF}{CC_ON}<span class=\"tvpl\" aria-hidden=\"true\">Captions</span></button>\
<button type=\"button\" class=\"pb tvau\" id=\"{AUB}\" data-act=\"p-menu\" data-arg=\"audio\" aria-haspopup=\"true\" aria-label=\"Audio\">{I_AUDIO}<span class=\"tvpl\" aria-hidden=\"true\">Audio</span></button>\
<button type=\"button\" class=\"pb pw\" id=\"{SPEED}\" data-act=\"p-menu\" data-arg=\"speed\" aria-haspopup=\"true\" aria-label=\"Speed\"><span id=\"{RATEB}\" class=\"plbl\">1×</span><span class=\"tvpl\" aria-hidden=\"true\">Speed</span></button>\
<button type=\"button\" class=\"pb pw\" id=\"{QUAL}\" data-act=\"p-menu\" data-arg=\"quality\" aria-haspopup=\"true\" aria-label=\"Quality\"><span id=\"{QLBL}\" class=\"plbl pq\">Auto</span><span class=\"tvpl\" aria-hidden=\"true\">Quality</span></button>\
<button type=\"button\" class=\"pb tvlk tvac\" id=\"{TVLK}\" data-act=\"a-like\" aria-pressed=\"false\" aria-label=\"I like this\">{I_LIKE}<span id=\"{TVLK_L}\" class=\"tvpl\" aria-hidden=\"true\">Like</span></button>\
<button type=\"button\" class=\"pb tvlk tvac tvdl\" id=\"{TVDL}\" data-act=\"a-dislike\" aria-pressed=\"false\" aria-label=\"I dislike this\">{I_LIKE}<span id=\"{TVDL_L}\" class=\"tvpl\" aria-hidden=\"true\">Dislike</span></button>\
<span id=\"{TVWL}\" class=\"pslot\"></span>\
<button type=\"button\" class=\"pb tvac\" id=\"{TVSV}\" data-act=\"tv-save\" aria-haspopup=\"dialog\" aria-label=\"Save to playlist\">{I_SAVE}<span class=\"tvpl\" aria-hidden=\"true\">Save</span></button>\
<button type=\"button\" class=\"pb\" data-act=\"tv-comments\" aria-label=\"Comments\" aria-haspopup=\"dialog\">{I_COMMENTS}<span class=\"tvpl\" aria-hidden=\"true\">Comments</span></button>\
<button type=\"button\" class=\"pb\" data-act=\"tv-info\" aria-label=\"More info\" aria-haspopup=\"dialog\">{I_INFO}<span class=\"tvpl\" aria-hidden=\"true\">More info</span></button>\
</div></div><div id=\"{MENU}\" class=\"pmenu\" role=\"dialog\"><div id=\"{MENUI}\" class=\"pmi\"></div></div>"
    )
}

/// One seek-bar segment: track, buffered, hover and progress fills. Without
/// `--s`/`--e` it spans the whole bar.
const BAR: &str = "<div class=\"pbar\"><i class=\"pbuf\"></i><i class=\"phv\"></i><i class=\"ppos\"></i></div>";

/// Chapter segments as fractions of `0..d`: (start, end) per chapter that
/// starts inside the video (chapter `i` → segment `i`). Empty unless there
/// are at least two.
pub fn segments(starts: &[f64], d: f64) -> Vec<(f64, f64)> {
    if !(d.is_finite() && d > 0.0) {
        return Vec::new();
    }
    let inside: Vec<f64> = starts.iter().copied().take_while(|&s| s < d - 0.5).collect();
    if inside.len() < 2 {
        return Vec::new();
    }
    (0..inside.len()).map(|i| ((if i == 0 { 0.0 } else { inside[i] }) / d, inside.get(i + 1).map_or(1.0, |&e| e / d))).collect()
}

/// Seek-bar markup: one `.pbar` per segment (positions as `--s`/`--e`; CSS
/// leaves a gap at the end of every segment but the last), or the single bar.
pub fn bars_html(segs: &[(f64, f64)]) -> String {
    bars_html_with(segs, &|_, _| String::new())
}

/// `bars_html` with extra marks inside each segment (`marks(lo, hi)`: the
/// segment's span as fractions of the video; SponsorBlock ranges), above the fills.
pub fn bars_html_with(segs: &[(f64, f64)], marks: &dyn Fn(f64, f64) -> String) -> String {
    if segs.is_empty() {
        let m = marks(0.0, 1.0);
        return if m.is_empty() { BAR.to_string() } else { format!("<div class=\"pbar\"><i class=\"pbuf\"></i><i class=\"phv\"></i><i class=\"ppos\"></i>{m}</div>") };
    }
    let mut out = String::with_capacity(segs.len() * 160);
    for (i, (s, e)) in segs.iter().enumerate() {
        out.push_str(&format!("<div id=\"{SEG}{i}\" class=\"pbar\" style=\"--s:{s:.6};--e:{e:.6}\"><i class=\"pbuf\"></i><i class=\"phv\"></i><i class=\"ppos\"></i>{}</div>", marks(*s, *e)));
    }
    out
}

/// The chapter playing at `t` (index into `starts`, ascending).
pub fn chapter_at(starts: &[f64], t: f64) -> Option<usize> {
    starts.partition_point(|&s| s <= t).checked_sub(1).or((!starts.is_empty()).then_some(0))
}

/// Ctrl/⌘+→ / ←: the next chapter's start; the previous one, or the start of
/// the current one when more than 3 s into it (like a track list).
pub fn chapter_step(starts: &[f64], t: f64, next: bool) -> Option<f64> {
    let i = chapter_at(starts, t)?;
    if next {
        return starts.get(i + 1).copied();
    }
    let cur = starts[i];
    Some(if t - cur > 3.0 || i == 0 { cur.min(t) } else { starts[i - 1] })
}

/// The Quality shortcut's text for a rung: `4K`/`8K` for 2160/4320, else
/// the number (`144` … `1440`); empty → `Auto`.
pub fn quality_short(p: u32) -> String {
    match p {
        0 => "Auto".into(),
        p if p >= 4320 => "8K".into(),
        p if p >= 2160 => "4K".into(),
        p => p.to_string(),
    }
}

/// `m:ss` or `h:mm:ss`.
pub fn clock(t: f64) -> String {
    let s = if t.is_finite() && t > 0.0 { t as u64 } else { 0 };
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Where the progress fill should aim now: the position `GLIDE_MS` ahead
/// while it glides (playing), else exactly `t`; as a fraction of `a..d`.
pub fn progress(t: f64, a: f64, d: f64, rate: f64, glide: bool) -> f64 {
    let lead = if glide && rate.is_finite() && rate > 0.0 { rate * GLIDE_MS / 1000.0 } else { 0.0 };
    ((t + lead - a) / (d - a)).clamp(0.0, 1.0)
}

/// A caption change that deserves the quick fade: a new cue replacing another
/// back to back (no line in common). Lines added to or kept from the current
/// text (rolling auto-captions) are updates and never move; appearing after a
/// gap fades by itself (CSS: the wrapper was `display:none`).
pub fn fresh_cue(old: &str, new: &str) -> bool {
    !old.is_empty() && !new.is_empty() && !new.lines().any(|l| old.lines().any(|o| o == l))
}

/// Speed limits (cap 2.5×) and the `<`/`>` steps.
pub const RATE_MIN: f64 = 0.25;
pub const RATE_MAX: f64 = 2.5;
pub const RATE_STEP: f64 = 0.05;
pub const RATES: [f64; 10] = [0.25, 0.5, 0.75, 1.0, 1.25, 1.5, 1.75, 2.0, 2.25, 2.5];
/// The Speed list, bottom → top (2 Nov 2026: 1× at the bottom, up to 2.5× at the top). Slower rates stay reachable with −, the wheel over the readout (0.05 steps) and `<`.
pub const PRESETS: [f64; 6] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5];

/// Caption scale after one wheel notch (`dir` −1 = up = larger): 5 % steps on the 5 % grid, 0.5–2.
pub fn cc_wheel(s: f64, dir: i32) -> f64 {
    let pct = ((s * 100.0 / 5.0).round() * 5.0 - dir.signum() as f64 * 5.0).clamp(50.0, 200.0);
    pct / 100.0
}

pub fn rate_label(r: f64) -> String {
    format!("{}×", super::stats::n3(r))
}

/// Clamped to the range and snapped to the 0.05 grid.
pub fn clamp_rate(r: f64) -> f64 {
    if !r.is_finite() {
        return 1.0;
    }
    ((r.clamp(RATE_MIN, RATE_MAX) / RATE_STEP).round() * RATE_STEP * 100.0).round() / 100.0
}

/// Next `<`/`>` rate from `r` (also from rates between the steps).
pub fn rate_step(r: f64, up: bool) -> f64 {
    if up { RATES.iter().copied().find(|&x| x > r + 1e-6).unwrap_or(RATE_MAX) } else { RATES.iter().rev().copied().find(|&x| x < r - 1e-6).unwrap_or(RATE_MIN) }
}

/// `n` steps along an ascending list (+ = up) from `cur`, which may sit between entries (a rate set with
/// −/+): up goes to the next entry above it, down to the next below. Returns the value and whether the
/// end of the list stopped it (the glue nudges the list then).
pub fn list_step(list: &[f64], cur: f64, n: i32) -> (f64, bool) {
    let mut v = cur;
    for _ in 0..n.unsigned_abs() {
        let next = if n > 0 { list.iter().copied().find(|&x| x > v + 1e-6) } else { list.iter().rev().copied().find(|&x| x < v - 1e-6) };
        match next {
            Some(x) => v = x,
            None => return (v, true),
        }
    }
    (v, false)
}

/// The wheel over the Speed shortcut or list: `n` presets up/down.
pub fn preset_step(r: f64, n: i32) -> (f64, bool) {
    list_step(&PRESETS, r, n)
}

/// The wheel over the Speed readout: `n` fine (0.05) steps, 0.25–2.5.
pub fn fine_step(r: f64, n: i32) -> (f64, bool) {
    let to = clamp_rate(r + n as f64 * RATE_STEP);
    (to, (to - r).abs() < 1e-6)
}

/// Quality list values bottom → top: Auto (0), then the rungs low → high.
pub fn quality_list(rungs: &[u32]) -> Vec<u32> {
    let mut v: Vec<u32> = rungs.iter().copied().filter(|&p| p > 0).collect();
    v.sort_unstable();
    v.dedup();
    v.insert(0, 0);
    v
}

/// `n` steps along the Quality list from `cur` (Auto sits below the lowest rung).
pub fn quality_step(list: &[u32], cur: u32, n: i32) -> (u32, bool) {
    let rank = |p: u32| if p == 0 { -1.0 } else { p as f64 };
    let ranks: Vec<f64> = list.iter().map(|&p| rank(p)).collect();
    let (r, edge) = list_step(&ranks, rank(cur), n);
    (if r < 0.0 { 0 } else { r as u32 }, edge)
}

// ------------------------------------------------------------ wheel and dwell (zero clicks)

/// Resting the pointer on a Speed/Quality row selects it after this long (2 Nov 2026: zero clicks,
/// hover alone); passing over rows on the way does not. client.css draws the same time as the row's fill.
pub const DWELL_MS: u32 = 220;
/// A wheel gesture: events closer than this belong to one (a trackpad and its momentum send one every
/// 8–16 ms); a mouse wheel turned notch by notch starts a new one every notch.
pub const WHEEL_GAP_MS: f64 = 120.0;
/// Pixels before the first step of a gesture (any real notch or swipe; ignores 1–2 px of trackpad jitter).
pub const WHEEL_LEAD_PX: f64 = 3.0;
/// Pixels for the second step of a gesture; each later one costs `WHEEL_RAMP_PX` more, so a short swipe
/// moves one or two rows and a long flick with momentum a handful, never the whole list at once.
pub const WHEEL_STEP_PX: f64 = 50.0;
pub const WHEEL_RAMP_PX: f64 = 40.0;
/// Quality applies once the wheel rests this long (the list follows every step at once).
pub const QUALITY_REST_MS: u32 = 200;

/// Turns wheel events into list steps (+ = up the list: faster, better). Mouse-wheel clicks (`notches`,
/// counted by the bridge) are one step each; pixel deltas (trackpads, macOS mice) accumulate with the
/// lead/ramp costs above, at most one step per event.
#[derive(Debug, Default)]
pub struct Wheel {
    target: String,
    last: f64,
    /// Direction of the current gesture (+1 up, −1 down, 0 none yet), pixels gathered, steps taken.
    dir: i32,
    acc: f64,
    steps: u32,
    nudged: bool,
}

impl Wheel {
    /// `dy` = the event's vertical pixels (+ = down, as the browser reports), `notches` = mouse-wheel
    /// clicks in it (+ = down, 0 = a precise device). Returns signed steps (+ = up).
    pub fn feed(&mut self, target: &str, dy: f64, notches: i32, now: f64) -> i32 {
        if target != self.target || now - self.last > WHEEL_GAP_MS {
            *self = Wheel { target: target.to_string(), ..Wheel::default() };
        }
        self.last = now;
        if notches != 0 {
            self.dir = -notches.signum();
            self.acc = 0.0;
            self.steps += notches.unsigned_abs();
            return -notches;
        }
        if !dy.is_finite() || dy == 0.0 {
            return 0;
        }
        let d = if dy < 0.0 { 1 } else { -1 };
        if d != self.dir {
            // A reversal inside a gesture needs a full step's travel (trackpad wobble at the end of a swipe).
            self.steps = if self.dir == 0 { 0 } else { 1 };
            self.dir = d;
            self.acc = 0.0;
        }
        self.acc += dy.abs();
        let cost = if self.steps == 0 { WHEEL_LEAD_PX } else { WHEEL_STEP_PX + WHEEL_RAMP_PX * (self.steps - 1) as f64 };
        if self.acc + 1e-9 < cost {
            return 0;
        }
        self.acc -= cost;
        self.steps += 1;
        d
    }

    /// The end of the list was hit: true once per gesture (one nudge, not one per event).
    pub fn edge(&mut self) -> bool {
        !std::mem::replace(&mut self.nudged, true)
    }
}

/// A row that dwell may select: Speed presets, quality rungs, Auto and codecs (not − and +, which repeat).
pub fn dwell_row(arg: &str) -> bool {
    arg.starts_with("rate:") && !arg.ends_with(['+', '-']) || arg.starts_with("q:") || arg.starts_with("codec:")
}

/// Element id of a menu choice (`data-arg` → `ytc-m-…`), the target of its selection mark.
pub fn choice_id(arg: &str) -> String {
    format!("ytc-m-{arg}")
}

/// A menu choice; `dwell` rows are hover zones (`dw`) that select after `DWELL_MS`.
fn choice(out: &mut String, arg: &str, label: &str, class: &str, dwell: bool) {
    out.push_str("<button type=\"button\" id=\"ytc-m-");
    esc_into(out, arg);
    out.push_str("\" data-act=\"p-set\" data-arg=\"");
    esc_into(out, arg);
    out.push('"');
    if dwell {
        out.push_str(" data-hov=\"dw\"");
    }
    if !class.is_empty() {
        out.push_str(" class=\"");
        out.push_str(class);
        out.push('"');
    }
    out.push('>');
    esc_into(out, label);
    out.push_str("</button>");
}

/// Speed: the exact rate between − and + (fine control), then the presets as a vertical list, 2.5× at
/// the top down to 1× at the bottom (next to the shortcut). The whole panel takes the wheel.
pub fn speed_menu(rate: f64) -> String {
    let mut out = format!(
        "<div class=\"msp\" data-wheel=\"speed\"><div class=\"mrt\" data-wheel=\"rate\"><button type=\"button\" data-act=\"p-set\" data-arg=\"rate:-\" aria-label=\"Slower\">−</button>\
<b id=\"{RATE}\">{}</b><button type=\"button\" data-act=\"p-set\" data-arg=\"rate:+\" aria-label=\"Faster\">+</button></div><div id=\"{MPRE}\" class=\"mpre mvl\">",
        rate_label(rate)
    );
    for p in PRESETS.iter().rev() {
        choice(&mut out, &format!("rate:{p}"), &rate_label(*p), if (p - rate).abs() < 1e-6 { "on" } else { "" }, true);
    }
    out.push_str("</div></div>");
    out
}

/// One Quality rung (deduplicated by `p` in the ladder).
pub struct Rung {
    pub p: u32,
    pub fps: u32,
    pub label: String,
}

/// Quality: the codec row (when > 1) as a compact segmented control on top, then one vertical list, the
/// best rung at the top down to the worst, and Auto at the bottom next to the shortcut (the default, like
/// 1× for Speed). Premium/enhanced rungs sit at their numeric tier. `pref` = the choice shown selected
/// (0 = Auto), `cur` = the rung playing now (a dot). The whole panel takes the wheel.
pub fn quality_menu(rungs: &[Rung], pref: u32, cur: u32, codecs: &[(&str, &str)], codec: &str) -> String {
    let mut out = String::from("<div class=\"mq\" data-wheel=\"quality\">");
    if codecs.len() > 1 {
        out.push_str(&format!("<div id=\"{MCO}\" class=\"mco\" role=\"group\" aria-label=\"Codec\">"));
        choice(&mut out, "codec:", "Any", if codec.is_empty() { "on" } else { "" }, true);
        for (k, name) in codecs {
            choice(&mut out, &format!("codec:{k}"), name, if codec == *k { "on" } else { "" }, true);
        }
        out.push_str("</div>");
    }
    out.push_str(&format!("<div id=\"{MQ}\" class=\"mql mvl\">"));
    let mut sorted: Vec<&Rung> = rungs.iter().collect();
    sorted.sort_by_key(|r| (r.p, r.fps));
    for r in sorted.iter().rev() {
        let class = match (pref == r.p, cur == r.p) {
            (true, true) => "on cur",
            (true, false) => "on",
            (false, true) => "cur",
            _ => "",
        };
        choice(&mut out, &format!("q:{}", r.p), &r.label, class, true);
    }
    choice(&mut out, "q:0", "Auto", if pref == 0 { "on" } else { "" }, true);
    out.push_str("</div></div>");
    out
}

/// TV: the Audio panel, the tracks by YouTube's names with the playing one marked.
pub fn audio_menu(audio: &[(String, String)], audio_sel: &str) -> String {
    let mut out = format!("<div class=\"mcc\"><div id=\"{MLS}\" class=\"mls\">");
    for (id, name) in audio {
        choice(&mut out, &format!("au:{id}"), name, if id == audio_sel { "on" } else { "" }, false);
    }
    out.push_str("</div></div>");
    out
}

/// CC popup. Tabs only when there are several audio tracks; hovering a tab
/// switches it. `toggle`: keep the remembered On/Off row for a TV remote without hover.
/// `tracks` = caption names (selected = `sel` while captions are on),
/// `audio` = (id, name) with `audio_sel` the playing id.
/// `translated`: the first Auto-translate row (TV), where its heading goes.
#[allow(clippy::too_many_arguments)]
pub fn cc_menu(audio_tab: bool, tracks: &[String], sel: Option<usize>, on: bool, audio: &[(String, String)], audio_sel: &str, toggle: bool, translated: Option<usize>) -> String {
    let mut out = String::from("<div class=\"mcc\">");
    let tabs = audio.len() > 1;
    let audio_tab = tabs && audio_tab;
    if tabs {
        out.push_str("<div class=\"mtab\" role=\"tablist\">");
        for (arg, label, cur) in [("cc", "Captions", !audio_tab), ("audio", "Audio", audio_tab)] {
            out.push_str(&format!(
                "<button type=\"button\" role=\"tab\" aria-selected=\"{cur}\" data-act=\"p-tab\" data-hov=\"tab\" data-arg=\"{arg}\"{}>{label}</button>",
                if cur { " class=\"on\"" } else { "" }
            ));
        }
        out.push_str("</div>");
    }
    out.push_str(&format!("<div id=\"{MLS}\" class=\"mls\">"));
    if audio_tab {
        for (id, name) in audio {
            choice(&mut out, &format!("au:{id}"), name, if id == audio_sel { "on" } else { "" }, false);
        }
    } else if tracks.is_empty() {
        out.push_str("<p class=\"mno\">No captions for this video</p>");
    } else {
        for (i, t) in tracks.iter().enumerate() {
            if translated == Some(i) {
                out.push_str("<p class=\"msec\">Auto-translate</p>");
            }
            choice(&mut out, &format!("cc:{i}"), t, if on && sel == Some(i) { "on" } else { "" }, false);
        }
    }
    out.push_str("</div>");
    if toggle {
        out.push_str(&format!("<button type=\"button\" id=\"{MSW}\" class=\"msw\" role=\"switch\" aria-checked=\"{on}\" data-act=\"p-set\" data-arg=\"cc:toggle\">Captions<i></i></button>"));
    }
    out.push_str("</div>");
    out
}

// ------------------------------------------------------------ hover menus

/// How the open menu was opened (decides what closes it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Src {
    /// Pointer intent: departure closes it after a grace period.
    Hover,
    /// A click pinned it: another click, Escape or a click outside close it.
    Click,
    /// Keyboard: arrows move between choices, Escape closes and refocuses.
    Key,
}

/// What the glue should do after an event.
#[derive(Debug, PartialEq)]
pub enum Do {
    None,
    /// Call `timer(generation)` after this many ms (`generation` = the current generation).
    Wait(u32),
    /// `open` changed to a menu (render it).
    Open,
    /// The menu closed (clear it).
    Close,
}

/// Mouse intent before a hovered shortcut opens (16 ms, also when
/// switching between open menus); grace after leaving: 400 ms before the
/// pointer has entered the panel, 180 ms after.
pub const INTENT_MS: u32 = 16;
pub const GRACE_MS: u32 = 400;
pub const GRACE_IN_MS: u32 = 180;

/// Hover-to-open menus for the toolbar shortcuts. Hover opens and closes
/// menus; in the Speed and Quality menus resting on a row also chooses it
/// (dwell, since 2 Nov 2026, timed by the glue); CC choices are clicks.
#[derive(Debug)]
pub struct MenuCtl {
    pub open: Option<&'static str>,
    pub src: Src,
    /// Bumped by every event that invalidates a scheduled timer.
    pub generation: u32,
    pending: Option<&'static str>,
    closing: bool,
    entered: bool,
    held: bool,
}

impl Default for MenuCtl {
    fn default() -> Self {
        MenuCtl { open: None, src: Src::Hover, generation: 0, pending: None, closing: false, entered: false, held: false }
    }
}

pub fn menu_name(arg: &str) -> Option<&'static str> {
    match arg {
        "speed" => Some("speed"),
        "quality" => Some("quality"),
        "cc" => Some("cc"),
        // The watch page's Save chooser (`actions`, its own `MenuCtl`; zones `sv`/`svm`).
        "save" => Some("save"),
        // TV's own Audio action (the desktop has it as a tab of the CC popup).
        "audio" => Some("audio"),
        _ => None,
    }
}

impl MenuCtl {
    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// The pointer entered a zone: `tb` (a shortcut, `arg` = its menu),
    /// `menu`/`tab` (inside the open panel) or anything else ('').
    pub fn hover(&mut self, zone: &str, arg: &str, held: bool) -> Do {
        self.held = held;
        match (zone, menu_name(arg)) {
            ("tb", Some(m)) => {
                if self.open == Some(m) {
                    if self.closing || self.pending.is_some() {
                        self.bump();
                        self.closing = false;
                        self.pending = None;
                    }
                    return Do::None;
                }
                if held {
                    // Travel with a button down (e.g. a range drag) is not intent.
                    return self.leave();
                }
                self.bump();
                self.pending = Some(m);
                self.closing = false;
                Do::Wait(INTENT_MS)
            }
            // `dw`: a Speed/Quality row (dwell selects it; the glue times that, the menu just stays).
            ("menu" | "tab" | "dw", _) if self.open.is_some() => {
                if self.closing || self.pending.is_some() {
                    self.bump();
                    self.closing = false;
                    self.pending = None;
                }
                self.entered = true;
                Do::None
            }
            _ => self.leave(),
        }
    }

    fn leave(&mut self) -> Do {
        if self.pending.take().is_some() {
            self.bump();
        }
        if self.open.is_some() && self.src == Src::Hover && !self.held && !self.closing {
            self.bump();
            self.closing = true;
            return Do::Wait(if self.entered { GRACE_IN_MS } else { GRACE_MS });
        }
        Do::None
    }

    pub fn timer(&mut self, generation: u32) -> Do {
        if generation != self.generation {
            return Do::None;
        }
        if let Some(m) = self.pending.take() {
            self.open = Some(m);
            self.src = Src::Hover;
            self.entered = false;
            self.closing = false;
            return Do::Open;
        }
        if self.closing {
            self.closing = false;
            self.open = None;
            return Do::Close;
        }
        Do::None
    }

    /// Click (or Enter/Space) on a shortcut. A click on a menu that hover
    /// just opened pins it rather than closing it under the pointer.
    pub fn click(&mut self, m: &'static str, keyboard: bool) -> Do {
        self.bump();
        self.pending = None;
        self.closing = false;
        if self.open == Some(m) {
            if self.src == Src::Hover && !keyboard {
                self.src = Src::Click;
                return Do::None;
            }
            if keyboard && self.src != Src::Key {
                self.src = Src::Key;
                return Do::Open;
            }
            self.open = None;
            return Do::Close;
        }
        self.open = Some(m);
        self.src = if keyboard { Src::Key } else { Src::Click };
        self.entered = false;
        Do::Open
    }

    /// A choice was made: a mouse-pinned menu returns to hover dismissal
    /// (it closes when the pointer leaves); keyboard menus stay.
    pub fn chose(&mut self, keyboard: bool) {
        if keyboard {
            self.src = Src::Key;
        } else if self.src == Src::Click {
            self.src = Src::Hover;
            self.entered = true;
        }
    }

    pub fn close(&mut self) -> Do {
        self.bump();
        self.pending = None;
        self.closing = false;
        if self.open.take().is_some() { Do::Close } else { Do::None }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn toolbar_labels() {
        assert_eq!(super::quality_short(0), "Auto");
        assert_eq!(super::quality_short(360), "360");
        assert_eq!(super::quality_short(1440), "1440");
        assert_eq!(super::quality_short(2160), "4K");
        assert_eq!(super::quality_short(4320), "8K");
        assert_eq!(super::rate_label(1.0), "1×");
        assert_eq!(super::rate_label(1.25), "1.25×");
        let sh = super::shell();
        assert!(!sh.contains("poster") && !super::shell_tv().contains("poster"), "no thumbnail poster (never scaled up)");
        assert!(!sh.contains("pspin"), "no loading indicator");
        assert!(sh.contains("id=\"ytc-rateb\" class=\"plbl\">1×") && sh.contains("id=\"ytc-qlbl\""));
        let tv = super::shell_tv();
        assert!(tv.contains("data-act=\"tv-comments\"") && tv.contains("class=\"tvpl\""));
        assert!(!sh.contains("tv-comments") && !sh.contains("tvpl"), "TV actions do not alter desktop controls");
        let row = &tv[tv.find("class=\"prow\"").unwrap()..];
        let order: Vec<usize> = ["ytc-tvch", "ytc-tvsubr", "p-toggle", "ytc-ccb", "ytc-aub", "ytc-speed", "ytc-qual", "a-like", "a-dislike", "ytc-tvwl", "tv-save", "tv-comments", "tv-info"].iter().map(|k| row.find(k).unwrap_or_else(|| panic!("{k}"))).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "one row, in this order: {order:?}");
        for k in ["ytc-aub", "tv-save", "a-dislike", "ytc-tvch"] {
            assert!(!sh.contains(k), "{k} is TV only");
        }
    }

    use super::*;

    #[test]
    fn markup() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(65.9), "1:05");
        assert_eq!(clock(3723.0), "1:02:03");
        assert_eq!(clock(f64::NAN), "0:00");
        assert_eq!(rate_label(1.25), "1.25×");
        let s = shell();
        assert!(s.contains("id=\"ytc-video\"") && s.contains("data-hov=\"menu\""));
        for id in [SPEED, QUAL] {
            assert!(s.contains(&format!("id=\"{id}\" data-act=\"p-menu\"")), "{id}");
        }
        assert!(s.contains(&format!("id=\"{CCB}\" data-act=\"p-cc\"")) && s.contains("aria-pressed=\"false\""));
        assert!(!s.contains("title="), "no custom tooltips");
    }

    #[test]
    fn progress_leads_only_while_gliding() {
        assert_eq!(progress(50.0, 0.0, 100.0, 1.0, false), 0.5);
        assert!((progress(50.0, 0.0, 100.0, 2.0, true) - 0.505).abs() < 1e-9);
        assert_eq!(progress(99.9, 0.0, 100.0, 2.0, true), 1.0, "clamped");
        assert_eq!(progress(10.0, 0.0, 100.0, f64::NAN, true), 0.1);
        let s = shell();
        assert!(s.contains("id=\"ytc-seek\" class=\"pseek\""));
    }

    #[test]
    fn chapter_segments() {
        let st = [0.0, 20.0, 30.0, 472.0];
        let segs = segments(&st, 40.0);
        assert_eq!(segs, vec![(0.0, 0.5), (0.5, 0.75), (0.75, 1.0)], "chapters past the end dropped");
        assert!(segments(&[0.0, 50.0], 40.0).is_empty(), "one chapter inside: no segments");
        assert!(segments(&st, f64::NAN).is_empty());
        assert!(segments(&[5.0, 20.0], 40.0)[0].0 == 0.0, "the first segment starts at 0");
        let h = bars_html(&segs);
        assert_eq!(h.matches("class=\"pbar\"").count(), 3);
        assert!(h.contains("id=\"ytc-sg1\" class=\"pbar\" style=\"--s:0.500000;--e:0.750000\""));
        assert!(h.contains("id=\"ytc-sg2\" class=\"pbar\" style=\"--s:0.750000;--e:1.000000\""));
        assert_eq!(bars_html(&[]), BAR);
        assert!(shell().contains(&format!("id=\"ytc-segs\" class=\"psegs\">{BAR}</div>")));
    }

    #[test]
    fn chapter_lookup_and_steps() {
        let st = [0.0, 20.0, 30.0];
        assert_eq!(chapter_at(&st, 0.0), Some(0));
        assert_eq!(chapter_at(&st, 19.9), Some(0));
        assert_eq!(chapter_at(&st, 20.0), Some(1));
        assert_eq!(chapter_at(&st, 99.0), Some(2));
        assert_eq!(chapter_at(&[], 5.0), None);
        assert_eq!(chapter_step(&st, 5.0, true), Some(20.0));
        assert_eq!(chapter_step(&st, 31.0, true), None, "last chapter");
        assert_eq!(chapter_step(&st, 25.0, false), Some(20.0), "into the chapter: its start");
        assert_eq!(chapter_step(&st, 21.0, false), Some(0.0), "just started: the previous one");
        assert_eq!(chapter_step(&st, 2.0, false), Some(0.0));
    }

    #[test]
    fn only_a_replaced_cue_fades() {
        assert!(fresh_cue("Hello there", "General Kenobi"));
        assert!(!fresh_cue("", "First line"), "after a gap CSS fades it");
        assert!(!fresh_cue("Line one", "Line one\nLine two"), "a line added");
        assert!(!fresh_cue("Line one\nLine two", "Line two\nLine three"), "rolling");
        assert!(!fresh_cue("Line one", ""));
    }

    #[test]
    fn rates_cap_at_2_5() {
        assert_eq!(clamp_rate(3.0), 2.5);
        assert_eq!(clamp_rate(0.1), 0.25);
        assert_eq!(clamp_rate(1.333), 1.35);
        assert_eq!(clamp_rate(f64::NAN), 1.0);
        assert_eq!(rate_step(1.0, true), 1.25);
        assert_eq!(rate_step(2.5, true), 2.5);
        assert_eq!(rate_step(1.3, false), 1.25);
        assert_eq!(rate_step(0.25, false), 0.25);
        let m = speed_menu(1.55);
        assert!(m.contains("id=\"ytc-rate\">1.55×<") && m.contains("data-wheel=\"rate\"") && !m.contains("type=\"range\""));
        assert!(!m.contains("class=\"on\""), "an off-list rate selects no preset");
        let m = speed_menu(1.5);
        assert!(m.contains("data-arg=\"rate:1.5\" data-hov=\"dw\" class=\"on\">1.5×<"), "{m}");
        assert!(m.contains(">2.5×<") && !m.contains("rate:3"));
        assert!(!m.contains("data-arg=\"rate:+\" data-hov"), "− and + are not dwell rows");
    }

    /// Labels of the list rows top → bottom in a menu's markup.
    fn rows(m: &str, list: &str) -> Vec<String> {
        let at = m.find(&format!("id=\"{list}\"")).unwrap();
        m[at..].split("data-hov=\"dw\"").skip(1).map(|r| r[r.find('>').unwrap() + 1..r.find("</button>").unwrap()].to_string()).collect()
    }

    #[test]
    fn speed_list_is_vertical_1x_at_the_bottom() {
        assert_eq!(rows(&speed_menu(1.0), MPRE), ["2.5×", "2×", "1.75×", "1.5×", "1.25×", "1×"]);
        let s = shell();
        assert!(s.contains("id=\"ytc-speed\" data-act=\"p-menu\" data-arg=\"speed\" data-hov=\"tb\" data-wheel=\"speed\""));
        assert!(s.contains("data-arg=\"quality\" data-hov=\"tb\" data-wheel=\"quality\""));
    }

    fn rung(p: u32, fps: u32) -> Rung {
        Rung { p, fps, label: if fps > 30 { format!("{p}p{fps}") } else { format!("{p}p") } }
    }

    #[test]
    fn quality_list_is_vertical_auto_at_the_bottom_best_on_top() {
        let rungs = [rung(1080, 60), rung(144, 30), rung(720, 60), rung(360, 30), rung(240, 30), rung(480, 30)];
        let m = quality_menu(&rungs, 0, 360, &[("vp9", "VP9"), ("h264", "H.264")], "");
        assert_eq!(rows(&m, MQ), ["1080p60", "720p60", "480p", "360p", "240p", "144p", "Auto"]);
        assert!(m.find("id=\"ytc-mco\"").unwrap() < m.find("id=\"ytc-mq\"").unwrap(), "codec row on top");
        assert!(m.starts_with("<div class=\"mq\" data-wheel=\"quality\">"));
        assert!(m.ends_with("data-arg=\"q:0\" data-hov=\"dw\" class=\"on\">Auto</button></div></div>"), "Auto last: {m}");
        assert!(m.contains("data-arg=\"q:360\" data-hov=\"dw\" class=\"cur\">"));
        assert!(m.contains("data-arg=\"codec:\" data-hov=\"dw\" class=\"on\">Any<"));
        // Fixed choice, one codec: no codec row.
        let m = quality_menu(&rungs[..2], 144, 144, &[("vp9", "VP9")], "vp9");
        assert!(m.contains("data-arg=\"q:144\" data-hov=\"dw\" class=\"on cur\">144p<") && !m.contains("mco"));
        assert_eq!(rows(&m, MQ), ["1080p60", "144p", "Auto"]);
        // A Premium rung sits at its numeric tier.
        let prem = [rung(720, 30), Rung { p: 1080, fps: 30, label: "1080p Premium".into() }, rung(1440, 30), rung(480, 30)];
        assert_eq!(rows(&quality_menu(&prem, 0, 0, &[], ""), MQ), ["1440p", "1080p Premium", "720p", "480p", "Auto"]);
    }

    #[test]
    fn wheel_steps_along_the_lists_and_stops_at_the_ends() {
        assert_eq!(preset_step(1.0, 1), (1.25, false));
        assert_eq!(preset_step(1.0, 2), (1.5, false));
        assert_eq!(preset_step(2.5, 1), (2.5, true), "top");
        assert_eq!(preset_step(1.0, -1), (1.0, true), "1× is the bottom");
        assert_eq!(preset_step(1.55, 1), (1.75, false), "from between presets: the next one up");
        assert_eq!(preset_step(1.55, -1), (1.5, false));
        assert_eq!(preset_step(0.5, 1), (1.0, false), "a slow rate wheels up into the list");
        assert_eq!(preset_step(2.0, 3), (2.5, true), "stopped by the end part-way");
        assert_eq!(fine_step(1.0, 1), (1.05, false));
        assert_eq!(fine_step(0.25, -1), (0.25, true));
        assert_eq!(fine_step(1.0, -3), (0.85, false));
        let l = quality_list(&[1080, 144, 720, 360, 360]);
        assert_eq!(l, [0, 144, 360, 720, 1080]);
        assert_eq!(quality_step(&l, 0, 1), (144, false), "Auto → the worst rung");
        assert_eq!(quality_step(&l, 144, -1), (0, false), "down to Auto");
        assert_eq!(quality_step(&l, 0, -1), (0, true));
        assert_eq!(quality_step(&l, 720, 1), (1080, false));
        assert_eq!(quality_step(&l, 1080, 1), (1080, true));
        assert_eq!(quality_step(&l, 480, 1), (720, false), "a remembered rung not offered here");
        assert_eq!(quality_step(&l, 360, -2), (0, false));
    }

    #[test]
    fn wheel_notch_is_one_step_swipe_is_a_few() {
        // Mouse wheel clicks: one step each, also fast ones.
        let mut w = Wheel::default();
        assert_eq!(w.feed("speed", 100.0, 1, 0.0), -1);
        assert_eq!(w.feed("speed", -100.0, -1, 30.0), 1);
        assert_eq!(w.feed("speed", -240.0, -2, 60.0), 2, "coalesced clicks");
        // One pixel event of a notch's size (no click count): one step, not two.
        let mut w = Wheel::default();
        assert_eq!(w.feed("speed", -100.0, 0, 0.0), 1);
        // A macOS mouse notch turned slowly (4 px each, far apart): one step each.
        let mut w = Wheel::default();
        let mut t = 0.0;
        for _ in 0..3 {
            assert_eq!(w.feed("speed", -4.000244140625, 0, t), 1);
            t += 250.0;
        }
        // A trackpad swipe: events every 8 ms. Jitter alone does nothing; the first step comes at once.
        let swipe = |px: &[f64]| {
            let mut w = Wheel::default();
            px.iter().enumerate().map(|(i, &d)| w.feed("quality", d, 0, i as f64 * 8.0)).sum::<i32>()
        };
        assert_eq!(swipe(&[-1.0, -1.0]), 0, "jitter");
        assert_eq!(swipe(&[-2.0, -2.0]), 1, "a tiny swipe is one step");
        assert_eq!(swipe(&[-6.0; 10]), 2, "60 px: two rows");
        let fling: Vec<f64> = (0..60).map(|i| -(40.0 - i as f64 * 0.6)).collect(); // ≈ 1340 px with momentum
        let n = swipe(&fling);
        assert!((5..=8).contains(&n), "a long flick moves a handful of rows, not the whole list: {n}");
        // Momentum's wobble back at the end does not step back.
        let mut w = Wheel::default();
        assert_eq!(w.feed("speed", -10.0, 0, 0.0), 1);
        assert_eq!(w.feed("speed", 4.0, 0, 8.0), 0);
        // A pause starts a new gesture (immediate first step again); so does another target.
        assert_eq!(w.feed("speed", -4.0, 0, 8.0 + WHEEL_GAP_MS + 1.0), 1);
        assert_eq!(w.feed("rate", -4.0, 0, 8.0 + WHEEL_GAP_MS + 2.0), 1);
        assert_eq!(w.feed("rate", 0.0, 0, 8.0 + WHEEL_GAP_MS + 3.0), 0);
        // One edge nudge per gesture.
        assert!(w.edge());
        assert!(!w.edge());
        assert_eq!(w.feed("rate", -4.0, 0, 1000.0), 1);
        assert!(w.edge(), "a new gesture nudges again");
    }

    #[test]
    fn dwell_rows_and_their_time_in_css() {
        for a in ["rate:1.5", "rate:1", "q:0", "q:1080", "codec:", "codec:vp9"] {
            assert!(dwell_row(a), "{a}");
        }
        for a in ["rate:+", "rate:-", "cc:1", "cc:toggle", "au:en.4"] {
            assert!(!dwell_row(a), "{a}");
        }
        let css = include_str!("../../extension/client.css");
        assert!(css.contains(&format!("transition:clip-path {DWELL_MS}ms linear")), "client.css draws the dwell over DWELL_MS");
    }

    #[test]
    fn cc_popup() {
        let tr = vec!["English".to_string(), "Deutsch <b>".to_string()];
        let m = cc_menu(false, &tr, Some(1), true, &[], "", false, None);
        assert!(!m.contains("mtab"), "caption-only: no tabs");
        assert!(m.contains("data-arg=\"cc:1\" class=\"on\">Deutsch &lt;b&gt;<"));
        assert!(!m.contains("cc:toggle") && !m.contains("role=\"switch\""), "desktop uses the CC shortcut");
        let off = cc_menu(false, &tr, Some(1), false, &[], "", false, None);
        assert!(!off.contains("class=\"on\""));
        assert!(cc_menu(false, &[], None, true, &[], "", false, None).contains("No captions"));
        let au = vec![("en.4".to_string(), "English original".to_string()), ("de.3".to_string(), "German".to_string())];
        let m = cc_menu(true, &tr, Some(0), true, &au, "de.3", false, None);
        assert!(m.contains("data-hov=\"tab\" data-arg=\"audio\" class=\"on\">Audio<"));
        assert!(m.contains("data-arg=\"au:de.3\" class=\"on\">German<") && !m.contains("cc:0"));
        assert!(!m.contains("cc:toggle"), "no desktop switch in the Audio view either");
        let tv = cc_menu(false, &tr, Some(0), false, &au, "de.3", true, None);
        assert!(tv.contains("id=\"ytc-msw\"") && tv.contains("aria-checked=\"false\""), "TV retains its remote toggle");
        // One audio track: Audio view requested but there is no Audio tab.
        assert!(!cc_menu(true, &tr, None, true, &au[..1], "en.4", false, None).contains("au:"));
        // TV: captions alone (no tabs), the Auto-translate heading over its first row; Audio is its own panel.
        let tv = cc_menu(false, &tr, Some(0), true, &[], "", true, Some(1));
        assert!(!tv.contains("mtab") && !tv.contains("au:"));
        let (head, row) = (tv.find("class=\"msec\">Auto-translate<").unwrap(), tv.find("data-arg=\"cc:1\"").unwrap());
        assert!(tv.find("data-arg=\"cc:0\"").unwrap() < head && head < row);
        assert!(!cc_menu(false, &tr, Some(0), true, &[], "", false, None).contains("msec"), "desktop: as before");
        let a = audio_menu(&au, "de.3");
        assert!(a.contains("data-arg=\"au:de.3\" class=\"on\">German<") && a.contains(">English original<") && !a.contains("cc:"));
        assert_eq!(menu_name("audio"), Some("audio"));
    }

    #[test]
    fn hover_opens_after_intent_and_closes_after_grace() {
        let mut c = MenuCtl::default();
        assert_eq!(c.hover("tb", "speed", false), Do::Wait(INTENT_MS));
        let g = c.generation;
        assert_eq!(c.timer(g), Do::Open);
        assert_eq!(c.open, Some("speed"));
        // Leave toward the panel: grace 400 ms; entering cancels it.
        assert_eq!(c.hover("", "", false), Do::Wait(GRACE_MS));
        let stale = c.generation;
        assert_eq!(c.hover("menu", "", false), Do::None);
        assert_eq!(c.timer(stale), Do::None, "cancelled");
        assert_eq!(c.open, Some("speed"));
        // Leave after having entered: the shorter grace, then closed.
        assert_eq!(c.hover("", "", false), Do::Wait(GRACE_IN_MS));
        let g = c.generation;
        assert_eq!(c.timer(g), Do::Close);
        assert_eq!(c.open, None);
    }

    #[test]
    fn passing_over_a_shortcut_does_not_open_it() {
        let mut c = MenuCtl::default();
        assert_eq!(c.hover("tb", "quality", false), Do::Wait(INTENT_MS));
        let g = c.generation;
        assert_eq!(c.hover("", "", false), Do::None);
        assert_eq!(c.timer(g), Do::None);
        assert_eq!(c.open, None);
        // Held travel (a drag) is not intent.
        assert_eq!(c.hover("tb", "quality", true), Do::None);
        assert_eq!(c.open, None);
    }

    #[test]
    fn hover_switches_between_menus() {
        let mut c = MenuCtl::default();
        c.hover("tb", "speed", false);
        let g = c.generation;
        c.timer(g);
        assert_eq!(c.hover("tb", "cc", false), Do::Wait(INTENT_MS));
        let g = c.generation;
        assert_eq!(c.timer(g), Do::Open);
        assert_eq!(c.open, Some("cc"));
    }

    #[test]
    fn held_range_drag_is_not_dismissed_by_departure() {
        let mut c = MenuCtl::default();
        c.hover("tb", "speed", false);
        let g = c.generation;
        c.timer(g);
        c.hover("menu", "", true);
        assert_eq!(c.hover("", "", true), Do::None, "slider held outside");
        assert_eq!(c.open, Some("speed"));
        // Release outside: the bridge re-reports the zone without the button.
        assert_eq!(c.hover("", "", false), Do::Wait(GRACE_IN_MS));
    }

    #[test]
    fn clicks_pin_and_keyboard_keeps() {
        let mut c = MenuCtl::default();
        // Click before the intent timer fired: opens pinned.
        c.hover("tb", "quality", false);
        let stale = c.generation;
        assert_eq!(c.click("quality", false), Do::Open);
        assert_eq!(c.timer(stale), Do::None);
        assert_eq!(c.src, Src::Click);
        assert_eq!(c.hover("", "", false), Do::None, "pinned: departure keeps it");
        // A choice by mouse returns to hover dismissal.
        c.chose(false);
        assert_eq!(c.hover("", "", false), Do::Wait(GRACE_IN_MS));
        // Click on a hover-opened menu pins it; a second click closes.
        let mut c = MenuCtl::default();
        c.hover("tb", "cc", false);
        let g = c.generation;
        c.timer(g);
        assert_eq!(c.click("cc", false), Do::None);
        assert_eq!(c.src, Src::Click);
        assert_eq!(c.click("cc", false), Do::Close);
        // Keyboard.
        assert_eq!(c.click("speed", true), Do::Open);
        assert_eq!(c.src, Src::Key);
        assert_eq!(c.hover("", "", false), Do::None);
        c.chose(true);
        assert_eq!(c.src, Src::Key);
        assert_eq!(c.close(), Do::Close);
        assert_eq!(c.close(), Do::None);
    }

    #[test]
    fn caption_wheel_steps() {
        assert_eq!(cc_wheel(1.0, -1), 1.05);
        assert_eq!(cc_wheel(1.0, 1), 0.95);
        assert_eq!(cc_wheel(1.52, -1), 1.55, "snaps to the 5 % grid");
        assert_eq!(cc_wheel(2.0, -1), 2.0, "capped at 200 %");
        assert_eq!(cc_wheel(0.5, 1), 0.5, "floor at 50 %");
        let mut s = 1.0;
        for _ in 0..40 {
            s = cc_wheel(s, -1);
        }
        assert_eq!(s, 2.0);
    }
}
