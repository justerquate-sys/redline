//! Cleanup on YouTube's own pages (native mode: `?yt-native=1` fallbacks and pages the client does not
//! render). The old extension's essential rules, slimmed: everything that is only hiding or theming
//! lives in `extension/native.css` (injected on these pages only, by the manifest); this module owns the
//! parts that need decisions:
//! - captions: cyan text at the client's shared position/size (`ytcConfigV1.p.cs`), dragged with the
//!   pointer, sized with the wheel over the cue or a slim range beside it; CC on/off follows the
//!   client's remembered choice (`p.cc`) and a deliberate toggle here updates it;
//! - number keys 0–9, `+`/`-` (native caption size) and `V` (fullscreen "More videos") are swallowed
//!   on the watch page; the fullscreen "More videos" wheel route is blocked on the bare video;
//! - promotional dialogs / mealbars are closed through their own dismiss button;
//! - the client's Shorts switch (`shortsInFeeds`) decides whether Shorts stay in feeds;
//! - "Auto-dubbed" badges (text only, no structural signature) are marked for the stylesheet.

use crate::bridge;
use crate::config::{self, CcStyle, Config};
use crate::features::filters::KEY_SHORTS;
use serde_json::{Value, json};
use std::cell::RefCell;

/// The caption box frame (fractions in `p.cs` are of this box, as in the client player).
const FRAME: &str = ".ytp-caption-window-container";
const CAPTION: &str = ".ytp-caption-window-container > .caption-window";
const PLAYER: &str = "#movie_player";
const KEY_SKIP: &str = "[role=\"dialog\"],dialog,tp-yt-paper-dialog";
const WHEEL_SKIP: &str = "button,a,input,[role=\"button\"],[role=\"slider\"],[role=\"menu\"],[role=\"listbox\"],[role=\"dialog\"],dialog,.ytp-settings-menu,.ytp-popup,.ytp-chrome-controls,.ytc-ccsz";
const CC_BUTTON: &str = "#movie_player:not(.ad-showing):not(.ad-interrupting) .ytp-subtitles-button";
const BADGES: &str = "ytd-watch-metadata :is(yt-badge-view-model,ytd-badge-supported-renderer)";
/// Typed upsell surfaces: their dialog is closed whenever it shows.
const PROMO_DIALOGS: &str = "ytd-popup-container :is(tp-yt-paper-dialog,[role=\"dialog\"]):has(:is(yt-upsell-dialog-renderer,yt-premium-spotlight-banner-renderer,yt-sponsorships-channel-upsell-view-model,.ytSponsorshipsChannelUpsellViewModelHost))";
/// Mealbars also carry account notices: closed only when their text is an offer (`offer`).
const MEALBARS: &str = "ytd-popup-container :is(yt-mealbar-promo-renderer,ytd-mealbar-promo-renderer)";
const DISMISS: &str = "#dismiss-button button,#dismiss-button tp-yt-paper-button,button#dismiss-button,#close-button button,button#close-button,button[aria-label=\"Close\" i],button[aria-label=\"Dismiss\" i],button[aria-label=\"No thanks\" i],button[aria-label=\"Not now\" i]";
const SIZE_INPUT: &str = ".ytc-ccsz input";
/// A video's first seconds: the native CC state is brought to the remembered choice (bounded clicks).
const CC_WINDOW_MS: f64 = 10_000.0;
const CC_TRIES: u8 = 3;
const SAVE_TIMER: u32 = 1;
/// Re-check after a CC click (a quiet page may not change again by itself).
const TICK_TIMER: u32 = 5000;

// Event flags (bridge.js page_guards).
pub const F_MODS: u32 = 1;
pub const F_COMPOSING: u32 = 2;
pub const F_EDITABLE: u32 = 4;
pub const F_SKIP: u32 = 8;
pub const F_SCOPE: u32 = 16;
pub const F_FULLSCREEN: u32 = 32;
pub const F_CAPTION: u32 = 64;

/// What a key press on a native page does.
#[derive(Debug, PartialEq)]
pub enum Key {
    Pass,
    /// Swallowed (0–9 seek, +/- native caption size, V "More videos").
    Block,
    /// `c`: a deliberate CC toggle is coming (remember the result).
    CcIntent,
}

pub fn key(key: &str, code: &str, flags: u32) -> Key {
    if flags & (F_MODS | F_COMPOSING | F_EDITABLE) != 0 {
        return Key::Pass;
    }
    if key.eq_ignore_ascii_case("c") && flags & F_SKIP == 0 {
        return Key::CcIntent;
    }
    if flags & F_SKIP != 0 || flags & F_SCOPE == 0 {
        return Key::Pass;
    }
    let digit = key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
    // KeyV also covers the V key on other layouts (Russian "м").
    if digit || matches!(key, "+" | "-" | "=" | "_" | "v" | "V") || code == "KeyV" { Key::Block } else { Key::Pass }
}

/// Wheel: 2 = over a caption (resize, page does not scroll), 1 = fullscreen bare video (the native
/// "More videos" drawer never opens), 0 = untouched.
pub fn wheel(dy: f64, flags: u32) -> u32 {
    if flags & F_MODS != 0 || flags & F_EDITABLE != 0 || dy == 0.0 {
        return 0;
    }
    if flags & F_CAPTION != 0 {
        return 2;
    }
    if flags & F_FULLSCREEN != 0 && flags & F_SCOPE != 0 && flags & F_SKIP == 0 { 1 } else { 0 }
}

/// A mealbar's text is a recognized offer (old extension: Premium/membership upsells with a trial or
/// join call). Account and safety notices that merely mention Premium stay.
pub fn offer(text: &str) -> bool {
    let t = text.to_lowercase();
    let product = ["youtube premium", "youtube music premium", "members-only", "members only", "ad-free", "premium lite"].iter().any(|p| t.contains(p));
    let call = ["try ", "try it", "free", "trial", "get premium", "join", "upgrade", "€0", "$0"].iter().any(|p| t.contains(p));
    product && call
}

pub fn auto_dubbed(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    t == "auto-dubbed" || t == "auto dubbed"
}

/// The `ytcConfigV1` JSON with `p.cs` / `p.cc` replaced; every other field kept as stored.
pub fn edit_config(stored: &str, cs: Option<CcStyle>, cc: Option<bool>) -> String {
    let mut v: Value = serde_json::from_str(stored).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
    if !v.get("p").is_some_and(Value::is_object) {
        v["p"] = json!({});
    }
    if let Some(c) = cs {
        let r = |v: f64| (v * 1000.0).round() / 1000.0;
        v["p"]["cs"] = json!({"v": 2, "x": r(c.x), "y": r(c.y), "s": r(c.s)});
    }
    if let Some(on) = cc {
        v["p"]["cc"] = on.into();
    }
    v.to_string()
}

#[derive(Default)]
struct Clean {
    loaded: bool,
    style: Option<CcStyle>,
    cc: Option<bool>,
    /// Edits waiting for a fresh read of the stored config (read-modify-write).
    pending_cs: Option<CcStyle>,
    pending_cc: Option<bool>,
    grab: Option<(f64, f64)>,
    wheel_acc: f64,
    save_gen: u32,
    video: Option<String>,
    video_at: f64,
    cc_tries: u8,
    cc_last: f64,
    cc_seen: Option<bool>,
    intent_at: f64,
    promo_at: f64,
    shorts_page: Option<bool>,
}

thread_local! { static S: RefCell<Clean> = RefCell::new(Clean::default()); }

/// Re-entrant calls (an event dispatched while Rust is clicking) are dropped, never a double borrow.
fn with<R: Default>(f: impl FnOnce(&mut Clean) -> R) -> R {
    S.with(|s| s.try_borrow_mut().map(|mut c| f(&mut c)).unwrap_or_default())
}

pub fn boot() {
    bridge::root_attr("data-ytc-native", Some("1".into()));
    crate::appearance::forget_old_hint();
    bridge::storage_get(config::STORAGE_KEY);
    bridge::storage_get(KEY_SHORTS);
    with(|c| c.apply());
    bridge::page_guards(PLAYER, KEY_SKIP, WHEEL_SKIP, CAPTION, FRAME, ".ytp-subtitles-button");
    bridge::page_watch("aria-pressed,opened,aria-hidden,hidden");
}

pub fn on_storage(key: &str, json: &str) {
    if key == KEY_SHORTS {
        bridge::root_attr("data-ytc-shorts", (json == "true").then(|| "on".to_string()));
        return;
    }
    if key != config::STORAGE_KEY {
        return;
    }
    with(|c| {
        let (cs, cc) = (c.pending_cs.take(), c.pending_cc.take());
        if cs.is_some() || cc.is_some() {
            bridge::storage_set(key, &edit_config(json, cs, cc));
        }
        let cfg = Config::from_json(json);
        let prefs = cfg.prefs;
        if cs.is_none() && c.grab.is_none() {
            c.style = prefs.cc_style;
        }
        if cc.is_none() {
            c.cc = Some(prefs.captions);
        }
        c.loaded = true;
        c.apply();
        // The page may be quiet by now: act on the loaded choice without waiting for a DOM change.
        c.tick(super::video_of(&bridge::href()));
    });
}

pub fn on_timer(id: u32) {
    with(|c| {
        if id == TICK_TIMER {
            c.tick(super::video_of(&bridge::href()));
        } else if id == SAVE_TIMER + c.save_gen {
            c.save(true, false);
        }
    });
}

pub fn on_key(k: &str, code: &str, flags: u32) -> bool {
    match key(k, code, flags) {
        Key::Block => true,
        Key::CcIntent => {
            with(|c| c.intent_at = bridge::now());
            false
        }
        Key::Pass => false,
    }
}

pub fn on_wheel(dy: f64, flags: u32) -> u32 {
    let r = wheel(dy, flags);
    if r == 2 {
        with(|c| c.wheel_size(dy));
    }
    r
}

pub fn on_drag(fx: f64, fy: f64, ex: f64, ey: f64, phase: u32) {
    with(|c| c.drag(fx, fy, ex, ey, phase));
}

pub fn on_input(name: &str, value: &str, committed: bool) {
    let Ok(v) = value.parse::<f64>() else { return };
    if name != "ccs" {
        return;
    }
    with(|c| {
        c.style = Some(CcStyle { s: v, ..c.current() }.clamped());
        c.apply();
        if committed {
            c.save(true, false);
        }
    });
}

pub fn on_intent() {
    with(|c| c.intent_at = bridge::now());
}

pub fn on_dom(video: Option<String>) {
    with(|c| c.tick(video));
}

impl Clean {
    fn current(&self) -> CcStyle {
        self.style.unwrap_or(CcStyle::DEFAULT)
    }

    fn apply(&self) {
        let s = self.current();
        bridge::root_var("--ytc-cx", &format!("{:.4}", s.x));
        bridge::root_var("--ytc-cy", &format!("{:.4}", s.y));
        bridge::root_var("--ytc-cs", &format!("{:.3}", s.s));
        bridge::root_var("--ytc-csp", &format!("{:.0}", s.s * 100.0));
        bridge::page_value(SIZE_INPUT, &format!("{:.2}", s.s));
    }

    fn save(&mut self, cs: bool, cc: bool) {
        if cs {
            self.pending_cs = Some(self.current());
        }
        if cc {
            self.pending_cc = self.cc;
        }
        bridge::storage_get(config::STORAGE_KEY);
    }

    fn drag(&mut self, fx: f64, fy: f64, ex: f64, ey: f64, phase: u32) {
        if phase == 0 {
            self.grab = Some((fx - ex, fy - ey));
            bridge::root_attr("data-ytc-ccdrag", Some(String::new()));
        }
        let Some((gx, gy)) = self.grab else { return };
        let before = self.current();
        self.style = Some(CcStyle { x: fx - gx, y: fy - gy, s: before.s }.clamped());
        self.apply();
        if phase == 2 {
            self.grab = None;
            bridge::root_attr("data-ytc-ccdrag", None);
            self.save(true, false);
        }
    }

    /// One size step per 50 px of wheel travel (a mouse notch is ~100), saved once the wheel rests.
    fn wheel_size(&mut self, dy: f64) {
        self.wheel_acc += dy;
        let steps = (self.wheel_acc / 50.0).trunc();
        if steps == 0.0 {
            return;
        }
        self.wheel_acc -= steps * 50.0;
        let c = self.current();
        self.style = Some(CcStyle { s: ((c.s - steps * 0.05) * 20.0).round() / 20.0, ..c }.clamped());
        self.apply();
        self.save_gen = (self.save_gen + 1) % 1000;
        bridge::timer(SAVE_TIMER + self.save_gen, 400);
    }

    fn tick(&mut self, video: Option<String>) {
        let now = bridge::now();
        // The Shorts player itself (/shorts/…) keeps its Shorts.
        let href = bridge::href();
        let shorts = href == "/shorts" || href.starts_with("/shorts/") || href.starts_with("/shorts?");
        if self.shorts_page != Some(shorts) {
            self.shorts_page = Some(shorts);
            bridge::root_attr("data-ytc-shorts-page", shorts.then(String::new));
        }
        if video != self.video {
            self.video = video;
            self.video_at = now;
            self.cc_tries = 0;
            self.cc_seen = None;
        }
        if self.video.is_some() {
            self.captions(now);
            for (i, t) in bridge::page_texts(BADGES).split('\u{1e}').enumerate() {
                if auto_dubbed(t) {
                    bridge::page_mark(BADGES, i as u32, "data-ytc-hide");
                }
            }
        }
        self.promotions(now);
    }

    fn captions(&mut self, now: f64) {
        if bridge::page_attr(CAPTION, "class").is_some() {
            let s = self.current().s;
            bridge::page_mount(PLAYER, "ytc-ccsz", &format!("<div class=\"ytc-ccsz\"><input type=\"range\" min=\"0.5\" max=\"2\" step=\"0.05\" value=\"{s:.2}\" data-nin=\"ccs\" aria-label=\"Caption size\"></div>"));
        }
        let pressed = match bridge::page_attr(CC_BUTTON, "aria-pressed").as_deref() {
            Some("true") => true,
            Some("false") => false,
            _ => return,
        };
        let changed = self.cc_seen.is_some_and(|p| p != pressed);
        self.cc_seen = Some(pressed);
        if changed && self.intent_at != 0.0 && now - self.intent_at < 2000.0 {
            // A deliberate toggle (click on CC or the `c` key): it becomes the remembered choice.
            self.intent_at = 0.0;
            self.cc_tries = CC_TRIES;
            if self.cc != Some(pressed) {
                self.cc = Some(pressed);
                self.save(false, true);
            }
            return;
        }
        let Some(want) = self.cc.filter(|_| self.loaded) else { return };
        if want != pressed && self.cc_tries < CC_TRIES && now - self.video_at < CC_WINDOW_MS && (self.cc_last == 0.0 || now - self.cc_last > 1200.0) && (self.intent_at == 0.0 || now - self.intent_at > 2000.0) {
            if bridge::page_click(CC_BUTTON, 0, "") {
                self.cc_tries += 1;
                self.cc_last = now;
                bridge::timer(TICK_TIMER, 1300);
            }
        }
    }

    fn promotions(&mut self, now: f64) {
        if self.promo_at != 0.0 && now - self.promo_at < 1500.0 {
            return;
        }
        let mut clicked = bridge::page_click(PROMO_DIALOGS, 0, DISMISS);
        if !clicked {
            let texts = bridge::page_texts(MEALBARS);
            if !texts.is_empty() {
                for (i, t) in texts.split('\u{1e}').enumerate() {
                    if offer(t) && bridge::page_click(MEALBARS, i as u32, DISMISS) {
                        clicked = true;
                        break;
                    }
                }
            }
        }
        if clicked {
            self.promo_at = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_keys_are_swallowed_on_the_watch_page_only_outside_text_fields() {
        for k in ["0", "5", "9", "+", "-", "=", "_", "v", "V"] {
            assert_eq!(key(k, "", F_SCOPE), Key::Block, "{k}");
        }
        assert_eq!(key("м", "KeyV", F_SCOPE), Key::Block, "V on a Russian layout");
        assert_eq!(key("5", "Digit5", F_SCOPE | F_EDITABLE), Key::Pass, "typing in a field");
        assert_eq!(key("5", "Digit5", F_SCOPE | F_MODS), Key::Pass, "browser shortcuts");
        assert_eq!(key("5", "Digit5", F_SCOPE | F_COMPOSING), Key::Pass);
        assert_eq!(key("5", "Digit5", F_SCOPE | F_SKIP), Key::Pass, "inside a dialog");
        assert_eq!(key("5", "Digit5", 0), Key::Pass, "outside the player and page");
        assert_eq!(key("k", "KeyK", F_SCOPE), Key::Pass);
        assert_eq!(key("ArrowLeft", "ArrowLeft", F_SCOPE), Key::Pass);
        assert_eq!(key("c", "KeyC", F_SCOPE), Key::CcIntent);
        assert_eq!(key("c", "KeyC", F_SCOPE | F_EDITABLE), Key::Pass);
    }

    #[test]
    fn wheel_resizes_over_captions_and_never_opens_the_fullscreen_drawer() {
        assert_eq!(wheel(-100.0, F_CAPTION | F_SCOPE), 2);
        assert_eq!(wheel(100.0, F_FULLSCREEN | F_SCOPE), 1);
        assert_eq!(wheel(100.0, F_FULLSCREEN | F_SCOPE | F_SKIP), 0, "menus and controls scroll");
        assert_eq!(wheel(100.0, F_SCOPE), 0, "page scrolling outside fullscreen");
        assert_eq!(wheel(100.0, F_FULLSCREEN | F_SCOPE | F_MODS), 0, "pinch zoom");
        assert_eq!(wheel(0.0, F_CAPTION), 0);
    }

    #[test]
    fn only_offer_mealbars_are_closed() {
        assert!(offer("Try YouTube Premium free for 1 month No thanks Try it free"));
        assert!(offer("Discover members-only content Join now"));
        assert!(offer("Get YouTube Premium Ad-free and offline. Get Premium"));
        assert!(!offer("Important account notice Learn more"));
        assert!(!offer("Keep your account safe Review your YouTube Premium account security. Review now"));
        assert!(!offer("Sign in to like videos Sign in"));
    }

    #[test]
    fn auto_dubbed_badges() {
        assert!(auto_dubbed(" Auto-dubbed "));
        assert!(auto_dubbed("auto dubbed"));
        assert!(!auto_dubbed("Auto-dubbed from Spanish into English, see description"));
        assert!(!auto_dubbed("4K"));
    }

    #[test]
    fn caption_preferences_are_written_into_the_shared_client_config() {
        let stored = r#"{"v":"2.20261029.11.00","vd":"CgtABCDEFGHIJK","p":{"cc":true,"vol":0.5,"th":true}}"#;
        let out = edit_config(stored, Some(CcStyle { x: 0.12345, y: 0.5, s: 1.25 }), None);
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["p"]["cs"], json!({"v": 2, "x": 0.123, "y": 0.5, "s": 1.25}));
        assert_eq!(v["p"]["vol"], json!(0.5), "other preferences kept");
        assert_eq!(v["vd"], json!("CgtABCDEFGHIJK"));
        let c = Config::from_json(&out);
        assert_eq!(c.prefs.cc_style.map(|c| (c.x, c.y, c.s)), Some((0.123, 0.5, 1.25)), "the client reads it back");
        assert!(c.prefs.theater);
        let off = edit_config(&out, None, Some(false));
        assert!(!Config::from_json(&off).prefs.captions);
        assert_eq!(Config::from_json(&off).prefs.cc_style.unwrap().s, 1.25);
        // No stored config yet (new profile) / damaged JSON.
        assert!(!Config::from_json(&edit_config("null", None, Some(false))).prefs.captions);
        assert_eq!(Config::from_json(&edit_config("{\"p\":3}", Some(CcStyle::DEFAULT), None)).prefs.cc_style.unwrap().x, 0.03);
    }
}
