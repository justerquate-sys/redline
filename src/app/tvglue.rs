//! TV mode wiring (`crate::tv` holds the pure parts). Only active when the TV app's shell set the
//! platform flag; on the desktop extension every entry point returns at once.
//!
//! - Remote: the arrows move focus spatially (`tv::pick` over the focusable elements the bridge
//!   measures, inside an open panel when one is open); on watch pages the player itself is a focus
//!   stop: ←/→ seek 10 s, OK plays/pauses, ↓ goes into the control bar, ↑ from the bar comes back.
//!   Media keys play/pause and seek. Back (the shell's button, `on_back`) closes what is open, then
//!   goes back; on Home it first returns to the top, then lets the app exit.
//! - Focus is never lost: after every paint the focus goes to the first card (feeds) or the player
//!   (watch) when nothing has it.
//! - Account: the device sign-in panel (`#ytc-tvsign`) and the access token from the shell, mirrored
//!   in this tab's sessionStorage; a cold start holds the first page until the shell answered.

use super::{App, Mode, Timer};
use crate::api::Req;
use crate::bridge;
use crate::render::MAIN;
use crate::route::Route;
use crate::tv::{self, DeviceCode, Dir, Poll};
use std::collections::HashMap;

/// Focus stops for the remote (visible ones only; the bridge filters by visibility).
const SEL: &str = "a[href]:not([tabindex='-1']),button:not([disabled]):not([tabindex='-1']),input:not([type=hidden]):not([tabindex='-1']),select,textarea,[tabindex='0']";
/// Not stops on TV: the extra links/buttons inside a card (the card is one stop: its title link),
/// the player's inside (the watch keys walk it), the mic (no voice search on TV), the search clear
/// button and the parts of the desktop page the TV hides anyway.
const SKIP: &str = ".c a:not(.t),.c button,#ytc-player *,#ytc-player,#ytc-mic,.qx,.qs,#ytc-debug *,#ytc-hfx *,.fx-ed *,.fx-tx";
/// Panels that keep the remote inside while open (the innermost one around the focus wins).
const TRAPS: &str = "#ytc-tvabout.on,#ytc-tvsign.on,#ytc-keys.on,#ytc-imp.on,#ytc-player.menu #ytc-menu,.open[id^=ytc-fx-],#ytc-save-p.on,#ytc-share-p.on,#ytc[data-tvq='1'] #ytc-top,#ytc-tvinfo.on";
/// Something is open that Escape closes (Back then means Escape).
const OPEN: &str = "#ytc-tvsign.on,#ytc-keys.on,#ytc-imp.on,#ytc-player.menu #ytc-menu,.open[id^=ytc-fx-],#ytc [aria-expanded=true]:not(#ytc-q),#ytc-chap.open";
/// Panels the remote's search and voice keys never open the recognizer over.
const PANELS: &str = "#ytc-tvsign.on,#ytc-tvabout.on,#ytc-keys.on,#ytc-imp.on,#ytc-player.menu #ytc-menu,.open[id^=ytc-fx-],#ytc-save-p.on,#ytc-share-p.on,#ytc-tvinfo.on,#ytc-comments.on";
/// What `spatial_rects` marks: 1 = a card of the page's grid (rows keep one vertical place), 2 = the rail.
const MARKS: &str = "#ytc-list a.t|#ytc-side *";
/// A card's stop is measured by the card's own box: an off-screen card (`content-visibility:auto`) needs no layout
/// for it, and every card's title link sits at the same place in its card, so rows line up as before.
const CARD: &str = ".c";
const PLAYER: &str = "ytc-player";
const SEEK: &str = "ytc-seek";
/// The overlay's button row and the Up next row (the related cards under it).
const UPNEXT: &str = ".wr";
const INFO: &str = "ytc-tvinfo";
/// The Up next header (the autoplay countdown's Play now / Cancel).
const UPNEXT_ID: &str = crate::render::UPNEXT;
/// The videos the remote's Previous can go back through.
const TRAIL_MAX: usize = 50;
/// The first stop of the player's button row when ↓ arrives from the bar.
const ROW_FIRST: &str = ".prow .pb[data-act=p-toggle]";
/// Hold the first page this long for the shell's token at a cold start, then go on signed out.
const HOLD_MS: u32 = 4000;
/// Scrubbing: the seek goes out once the remote rested this long; presses closer than `HELD_MS` are a
/// held key (key repeat) and speed the steps up.
const SCRUB_REST_MS: u32 = 450;
const HELD_MS: f64 = 260.0;
pub(super) const SIGN: &str = "ytc-tvsign";
const ACCT: &str = "ytc-acct";

/// Seconds per scrub step for the `n`-th press of one held run: 10 s, then faster the longer it is held.
pub(super) fn scrub_step(n: u32) -> f64 {
    match n {
        0..=5 => 10.0,
        6..=14 => 30.0,
        15..=29 => 60.0,
        _ => 120.0,
    }
}

/// About & updates: the screen, and the small notice when an update is ready.
const ABOUT: &str = "ytc-tvabout";
const NOTICE: &str = "ytc-tvup";
/// A short line about voice search (bottom right), this long.
const NOTE: &str = "ytc-tvnote";
const NOTE_MS: f64 = 4000.0;
/// How often the page asks the shell whether its own check (launch + every 6 h) found something; the
/// first look shortly after the shell's launch check.
const UPDATES_FIRST_MS: u32 = 20_000;
const UPDATES_EVERY_MS: u32 = 10 * 60_000;
const UPDATES_FAST_MS: u32 = 700;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Call {
    /// `shell_info` (`boot`: the bundle this page runs).
    Info { boot: bool },
    /// `shell_check` (Check for updates).
    Check,
    /// `shell_install` (Install update).
    Install,
    /// `auth_token`; `boot`: the first page waits for it.
    Token { boot: bool },
    Start,
    Poll { generation: u32 },
    SignOut,
    /// Speed telemetry (About & updates → Developer): `tel_info`, `tel_set`, `tel_find`.
    TelInfo,
    TelSet,
    TelFind,
    /// The ready Home response the shell kept from the last run (a cold start asks once, with the token).
    ReadyTake,
    /// `voice_start`: the speech recognizer opens.
    VoiceStart,
    /// `shell_events`: what waits for the page in the shell.
    Events,
}

/// About & updates → Developer: the address field's form, and the flag that reopens the screen after the page
/// loaded again for a telemetry change (this tab's session; a UI flag only).
pub(super) const DEV_FORM: &str = "ytc-tvdc";
const DEV_FIELD: &str = "ytc-tvdc-q";
const ABOUT_AGAIN: &str = "ytc:tvabout";

#[derive(Default)]
pub(super) struct Tv {
    /// The first page request waits for the shell's token (cold start), and for its kept ready Home response.
    pub(super) hold: bool,
    token_in: bool,
    ready_wait: bool,
    seq: u32,
    calls: HashMap<u32, Call>,
    code: Option<DeviceCode>,
    /// Device sign-in generation (a new code or a closed panel ends the old polling).
    generation: u32,
    poll_ms: f64,
    asked_at: f64,
    open: bool,
    token_asked: bool,
    /// The link last opened from each page (page URL → href): coming back focuses it again, as
    /// YouTube's TV app does.
    opened: HashMap<String, String>,
    /// The tab's history, entry by entry: a channel page opened from a video is left by one Back; what lies behind.
    history: tv::History,
    /// The next entry is a channel page opened from a video.
    next_from_watch: bool,
    /// Videos a channel page was opened from: (position, paused) as they were left, used when Back returns (bounded).
    left: HashMap<String, (f64, bool)>,
    /// Back just returned to such a video: the player being mounted takes them.
    pub(super) returned: Option<(f64, bool)>,
    /// The videos of this session, oldest first (the remote's Previous; bounded).
    trail: Vec<Route>,
    /// Scrub run: generation (a newer press ends the older rest timer), last press, presses held.
    scrub_gen: u32,
    scrub_at: f64,
    scrub_n: u32,
    /// The watch page's "More info" panel content (rendered with the page).
    pub(super) info: String,
    /// A search went out from the search screen: its results take the focus when they paint.
    grid_focus: bool,
    pub(super) keyboard: tv::keyboard::State,
    /// The page box holds the focus only because the page was still loading (no key since).
    auto_rail: bool,
    /// Updates: the shell's last answer, the bundle this page started with, the screen open, a line about the
    /// last action, the seq of the update the notice was dismissed for.
    shell: Option<tv::ShellInfo>,
    running: u64,
    running_client: String,
    about: bool,
    about_note: String,
    noticed: u64,
    /// Telemetry: the shell's last answer (None with `tel_known`: an app without telemetry), the Developer section
    /// shown, → presses in a row with nothing to the right (they reveal it), the last action's outcome.
    tel: Option<tv::TelInfo>,
    tel_known: bool,
    dev: bool,
    dev_taps: u32,
    dev_note: String,
    dev_err: bool,
    /// Voice search: the shell has a recognizer; it was opened with the mic control (the focus returns there); it paused
    /// a playing video (which goes on when nothing was heard); when the notice goes (epoch ms, 0 = none).
    voice: bool,
    voice_mic: bool,
    voice_resume: bool,
    note_until: f64,
}

impl Tv {
    pub(super) fn new(hold: bool) -> Tv {
        Tv { hold, ..Default::default() }
    }
}

impl App {
    /// At boot, before the first page request. Returns true when the first page must wait for the
    /// shell's token (`tv_release` starts it).
    pub(super) fn tv_boot_flag() -> bool {
        let on = bridge::platform() == "tv";
        tv::set_on(on);
        if !on {
            return false;
        }
        bridge::root_attr("data-ytc-tv", Some("1".into()));
        let mirror = bridge::session_get(tv::SESSION_KEY);
        if mirror.is_empty() {
            return true;
        }
        tv::set_bearer(tv::parse_token(&mirror));
        false
    }

    /// After the document is written.
    pub(super) fn tv_boot(&mut self) {
        if !tv::on() {
            return;
        }
        bridge::back_hook();
        self.tv.history.push(&self.url, false);
        bridge::append("ytc", &format!("<div id=\"{SIGN}\" class=\"tvs\" role=\"dialog\" aria-modal=\"true\" aria-label=\"Sign in\"></div><aside id=\"{INFO}\" class=\"tvi\" role=\"dialog\" aria-modal=\"true\" aria-label=\"More info\"></aside><aside id=\"{}\" class=\"tvi\" role=\"dialog\" aria-modal=\"true\" aria-label=\"Save to playlist\"><header class=\"tvih\"><button type=\"button\" class=\"tvb tvx\" data-act=\"tv-save-close\">Back to video</button></header><div id=\"{}\" class=\"tvib\"></div></aside><div id=\"{}\" class=\"tvactnote\" role=\"status\"></div>", crate::actions::TV_SAVE, crate::actions::TV_SAVE_BODY, crate::actions::TV_NOTE));
        self.tv_slot();
        self.tv_ask_token(self.tv.hold);
        if self.tv.hold {
            // A cold start: the ready Home response kept from the last run comes with the token (both answered from
            // the shell's memory), so Home paints it on its first frame.
            self.tv.ready_wait = true;
            self.tv_call("ready_take", "null", Call::ReadyTake);
            let id = self.timer(Timer::TvHold);
            bridge::timer(id, HOLD_MS);
        }
        bridge::append("ytc", &format!("<div id=\"{ABOUT}\" class=\"tvs\" role=\"dialog\" aria-modal=\"true\" aria-label=\"About and updates\"></div><div id=\"{NOTICE}\" class=\"tvup\" role=\"status\"><span>Update ready</span><button type=\"button\" class=\"tvb\" data-act=\"tv-about\">See update</button></div>"));
        bridge::append("ytc", &format!("<div id=\"{NOTE}\" class=\"tvnote\" role=\"status\"></div>"));
        self.tv_call("shell_info", "null", Call::Info { boot: true });
        if !self.tv.hold {
            self.on_shell_wake();
        }
        let id = self.timer(Timer::TvUpdates { fast: false });
        bridge::timer(id, UPDATES_FIRST_MS);
        if bridge::session_get(ABOUT_AGAIN) == "1" {
            // The page loaded again for a telemetry change made on About & updates: back on that screen.
            bridge::session_set(ABOUT_AGAIN, "");
            self.tv.dev = true;
            self.tv_about_open();
        }
        // A focus from the first frame (the rail while the page loads).
        self.tv_focus_check();
    }

    fn tv_call(&mut self, cmd: &str, arg: &str, what: Call) {
        self.tv.seq += 1;
        let seq = self.tv.seq;
        self.tv.calls.insert(seq, what);
        bridge::shell(seq, cmd, arg);
    }

    fn tv_ask_token(&mut self, boot: bool) {
        if self.tv.token_asked {
            return;
        }
        self.tv.token_asked = true;
        self.tv.asked_at = bridge::epoch();
        self.tv_call("auth_token", "{}", Call::Token { boot });
    }

    /// The held first page goes out once the token and the kept ready response are in.
    fn tv_try_release(&mut self) {
        if self.tv.token_in && !self.tv.ready_wait {
            self.tv_release();
        }
    }

    /// The held first page goes out (token answered, or the hold ran out).
    pub(super) fn tv_release(&mut self) {
        if std::mem::take(&mut self.tv.hold) {
            // The account as the shell answered it (the boot's snapshot predates the token).
            self.auth = tv::auth();
            self.tv_slot();
            // A link or search that started the app comes after this first page.
            self.on_shell_wake();
            // Home: the ready response the shell kept from the last run (now this tab's copy) is the page, as at a boot.
            if self.route == Route::Home && bridge::nav_type() != "back_forward" {
                if let Some(r) = self.ready_take() {
                    self.ready_show(r);
                    return;
                }
            }
            self.start_route();
        }
    }

    /// Account requests that go out with the bearer as the TV client.
    pub(super) fn tv_account_req(&self, req: &Req) -> bool {
        match req {
            Req::Browse { id, .. } => tv::account_browse(id),
            Req::WatchLaterOrder { .. } | Req::WatchLaterMenu => true,
            Req::Cont { ep: "browse", .. } => matches!(self.route, Route::Home | Route::Subs | Route::History | Route::Playlists | Route::Playlist { .. }),
            _ => false,
        }
    }

    /// Background tick: fetch a fresh access token before the current one runs out.
    pub(super) fn tv_tick(&mut self) {
        if !tv::on() {
            return;
        }
        let exp = tv::bearer_exp();
        if exp > 0.0 && exp - bridge::epoch() < tv::REFRESH_BEFORE_MS && bridge::epoch() - self.tv.asked_at > 30_000.0 {
            self.tv_ask_token(false);
        }
    }

    pub fn on_shell(&mut self, seq: u32, ok: bool, json: &str) {
        let Some(call) = self.tv.calls.remove(&seq) else { return };
        match call {
            Call::Info { .. } | Call::Check | Call::Install if !ok => {
                // The shell refused (offline shell, or an app too old for the command).
                let why = short(json);
                self.tv.about_note = match call {
                    Call::Install => format!("{} ({why})", tv::update_error(&why, false)),
                    Call::Check => format!("The check did not run ({why})."),
                    _ => String::new(),
                };
                self.tv_about_paint();
            }
            Call::Info { .. } | Call::Check | Call::Install => {
                let boot = call == Call::Info { boot: true };
                if let Some(i) = tv::parse_shell_info(json) {
                    if boot || self.tv.running == 0 {
                        self.tv.running = i.seq;
                        self.tv.running_client = i.client.clone();
                    }
                    let busy = matches!(i.state, tv::UpdateState::Downloading(..) | tv::UpdateState::Checking);
                    self.tv_voice_cap(i.voice);
                    self.tv.shell = Some(i);
                    if call != Call::Install {
                        self.tv.about_note.clear();
                    }
                    // (After Install the shell starts in the background: follow it until it is done.)
                    if (busy || call == Call::Install) && self.tv.about {
                        let id = self.timer(Timer::TvUpdates { fast: true });
                        bridge::timer(id, UPDATES_FAST_MS);
                    }
                }
                self.tv_about_paint();
                self.tv_notice();
            }
            Call::Token { boot: _ } => {
                self.tv.token_asked = false;
                let was = tv::bearer_at(bridge::epoch()).is_some();
                let tok = if ok { tv::parse_token(json) } else { None };
                match &tok {
                    Some((t, exp)) => {
                        bridge::session_set(tv::SESSION_KEY, &tv::token_json(t, *exp));
                    }
                    None if ok => {
                        bridge::session_set(tv::SESSION_KEY, "{\"token\":null}");
                    }
                    None => {}
                }
                if ok || tok.is_some() {
                    tv::set_bearer(tok);
                }
                let now = tv::bearer_at(bridge::epoch()).is_some();
                crate::features::shared(|s| s.signed_in = now);
                self.tv.token_in = true;
                if self.tv.hold {
                    self.tv_try_release();
                } else if was != now {
                    self.auth = tv::auth();
                    self.tv_slot();
                }
            }
            Call::Start => {
                let code = if ok { tv::parse_device(json) } else { None };
                match code {
                    Some(c) => {
                        self.tv.poll_ms = c.interval_ms;
                        self.tv.code = Some(c);
                        self.tv.generation += 1;
                        self.tv_panel("");
                        self.tv_poll_later();
                    }
                    None => self.tv_panel(&format!("Couldn't get a sign-in code: {}", short(json))),
                }
            }
            Call::Poll { generation } => {
                if generation != self.tv.generation || !self.tv.open {
                    return;
                }
                match if ok { tv::parse_poll(json) } else { Poll::Failed(short(json)) } {
                    Poll::Pending { slow } => {
                        if slow {
                            self.tv.poll_ms += 5000.0;
                        }
                        self.tv_poll_later();
                    }
                    Poll::Done { token, exp } => {
                        tv::set_bearer(Some((token.clone(), exp)));
                        bridge::session_set(tv::SESSION_KEY, &tv::token_json(&token, exp));
                        self.tv.open = false;
                        self.tv.code = None;
                        // The whole client starts again signed in (snapshots of the signed-out
                        // pages are dropped with the tab's session data).
                        bridge::session_clear();
                        bridge::session_set(tv::SESSION_KEY, &tv::token_json(&token, exp));
                        bridge::replace_doc("/");
                    }
                    Poll::Denied => {
                        self.tv.code = None;
                        self.tv_panel("The sign-in was declined on the other device.");
                    }
                    Poll::Expired => {
                        self.tv.code = None;
                        self.tv_panel("That code has expired.");
                    }
                    Poll::Failed(why) => {
                        // A network hiccup: keep polling, show why.
                        self.tv_panel(&format!("Still waiting ({why})"));
                        self.tv_poll_later();
                    }
                }
            }
            Call::SignOut => {
                tv::set_bearer(None);
                bridge::session_clear();
                bridge::replace_doc("/");
            }
            Call::ReadyTake => {
                if let Some(rec) = if ok { tv::parse_ready_take(json) } else { None } {
                    // Into this tab's copy: the Home navigation takes it from there (re-validated: account, age).
                    bridge::session_set(crate::ready::KEY, &rec);
                }
                self.tv.ready_wait = false;
                self.tv_try_release();
            }
            Call::TelInfo => {
                self.tv.tel = if ok { tv::parse_tel_info(json) } else { None };
                self.tv.tel_known = true;
                if self.tv.tel.as_ref().is_some_and(|t| t.on) {
                    self.tv.dev = true;
                }
                self.tv_about_paint();
                if std::mem::take(&mut self.tv.dev_taps) >= tv::DEV_TAPS {
                    // Just revealed: its first action takes the focus.
                    bridge::focus_sel(ABOUT, ".tvdv .tvb");
                }
            }
            Call::TelSet => match if ok { tv::parse_tel_info(json) } else { None } {
                Some(t) if t.reload => {
                    // The shell runs another script now (telemetry on or off): this page loads again with it and
                    // comes back to this screen.
                    bridge::session_set(ABOUT_AGAIN, "1");
                    bridge::replace_doc(&self.url.clone());
                }
                Some(t) => {
                    self.tv.tel = Some(t);
                    let note = if self.tv.dev_note.starts_with("Found") { self.tv.dev_note.clone() } else { "Saved.".into() };
                    self.tv_dev_note(&note, false);
                }
                None => self.tv_dev_note(&short(json), true),
            },
            Call::TelFind => match crate::json::parse_str(json).filter(|_| ok).and_then(|v| v.get("url").and_then(|u| u.as_str()).map(String::from)) {
                Some(url) => {
                    // Kept at once (the next Turn on uses it); the field shows it when the answer comes.
                    self.tv_call("tel_set", &tv::tel_set_json(None, Some(&url)), Call::TelSet);
                    self.tv.dev_note = format!("Found the debug center at {}.", tv::tel_addr(&url));
                }
                None => self.tv_dev_note(&short(json), true),
            },
            Call::VoiceStart => {
                if !ok || !crate::json::parse_str(json).and_then(|v| v.get("started").and_then(|s| s.as_bool())).unwrap_or(false) {
                    self.tv_voice_end(tv::VoiceEnd::Missing);
                }
            }
            Call::Events => {
                // (An app before 0.6.0 refuses the command: nothing ever waits there.)
                if ok {
                    for e in tv::parse_shell_events(json) {
                        self.tv_shell_event(e);
                    }
                }
            }
        }
    }

    /// The shell has something for the page. On every boot too: it may have come before this page existed.
    pub fn on_shell_wake(&mut self) {
        if tv::on() && !self.tv.hold {
            self.tv_call("shell_events", "null", Call::Events);
        }
    }

    fn tv_shell_event(&mut self, e: tv::ShellEvent) {
        match e {
            tv::ShellEvent::Heard(q) => {
                self.tv.voice_resume = false;
                self.tv.voice_mic = false;
                self.tv_search_for(&q);
            }
            tv::ShellEvent::VoiceEnd(end) => self.tv_voice_end(end),
            tv::ShellEvent::Search(q) => {
                self.tv_panels_close();
                self.tv_search_for(&q);
            }
            tv::ShellEvent::Open(path) => {
                // The shell only sends paths it built from checked parts; only these routes open anyway.
                let Some(route @ (Route::Watch { .. } | Route::Channel { .. } | Route::Playlist { .. })) = crate::route::parse(&path) else { return };
                self.tv_panels_close();
                if route != self.route {
                    let url = route.url();
                    self.navigate(route, url, Mode::Push);
                } else {
                    self.tv_focus_check();
                }
            }
        }
    }

    /// Runs a search as if it was typed on the search screen.
    fn tv_search_for(&mut self, q: &str) {
        let q = tv::keyboard::edit("", tv::keyboard::Edit::Insert(q));
        self.on_submit("ytc-search", &q);
    }

    /// Something from outside the app takes over the screen.
    fn tv_panels_close(&mut self) {
        // (Search can be open over the video a link names: it closes, and the focus goes back to the page.)
        self.tv_keyboard_leave();
        if self.tv.open {
            self.tv_close();
        }
        if self.tv.about {
            self.tv_about_close();
        }
        if bridge::any(OPEN) {
            self.on_key("Escape", false);
        }
    }

    // ---------------------------------------------------------- voice search

    /// The mic control shows only while the shell can open a recognizer (`shell_info`'s `caps`): never on an older app.
    fn tv_voice_cap(&mut self, on: bool) {
        if self.tv.voice != on {
            self.tv.voice = on;
            bridge::root_attr(tv::keyboard::MIC_ROOT, on.then(|| "1".to_string()));
        }
    }

    /// A key for the player itself. Not through `on_key`: with the search screen open (over a playing video) that would
    /// type the letter into the query.
    fn tv_player_key(&mut self, key: &str) -> bool {
        let Some(p) = self.player.as_mut() else { return false };
        let handled = p.on_key(key);
        self.after_player();
        handled
    }

    fn tv_voice_start(&mut self) {
        if !self.tv.voice {
            return;
        }
        // The recognizer listens: a playing video waits (and goes on if nothing comes of it).
        if matches!(self.route, Route::Watch { .. }) && self.player.as_ref().is_some_and(|p| !p.paused()) {
            self.tv.voice_resume = true;
            self.tv_player_key("k");
        }
        self.tv_call("voice_start", "null", Call::VoiceStart);
    }

    /// The recognizer closed without a search: the focus is back where it was, and a notice unless the owner closed it.
    fn tv_voice_end(&mut self, end: tv::VoiceEnd) {
        if end == tv::VoiceEnd::Missing {
            self.tv_voice_cap(false);
        }
        if std::mem::take(&mut self.tv.voice_resume) && self.player.as_ref().is_some_and(|p| p.paused()) {
            self.tv_player_key("k");
        }
        if std::mem::take(&mut self.tv.voice_mic) && self.tv_search_on() {
            bridge::focus(if self.tv.voice { tv::keyboard::MIC } else { tv::keyboard::QUERY });
        }
        self.tv_focus_check();
        if !end.note().is_empty() {
            bridge::html(NOTE, &crate::util::esc(end.note()));
            bridge::attr(NOTE, "class", "tvnote on");
            self.tv.note_until = bridge::epoch() + NOTE_MS;
            let id = self.timer(Timer::TvFocus);
            bridge::timer(id, NOTE_MS as u32 + 50);
        }
    }

    fn tv_dev_note(&mut self, note: &str, err: bool) {
        self.tv.dev_note = note.to_string();
        self.tv.dev_err = err;
        self.tv_about_paint();
    }

    /// The Developer section's address field was submitted (Enter / the TV keyboard's action key).
    pub(super) fn tv_dev_submit(&mut self, value: &str) -> bool {
        self.tv.dev_note = "Saving…".into();
        self.tv.dev_err = false;
        self.tv_call("tel_set", &tv::tel_set_json(None, Some(value)), Call::TelSet);
        true
    }

    fn tv_poll_later(&mut self) {
        let id = self.timer(Timer::TvPoll { generation: self.tv.generation });
        bridge::timer(id, self.tv.poll_ms.clamp(1000.0, 60_000.0) as u32);
    }

    pub(super) fn tv_poll(&mut self, generation: u32) {
        if generation == self.tv.generation && self.tv.open && self.tv.code.is_some() {
            self.tv_call("auth_poll", "{}", Call::Poll { generation });
        }
    }

    /// The account control in the top bar.
    fn tv_slot(&mut self) {
        let signed = tv::bearer_at(bridge::epoch()).is_some();
        bridge::html(ACCT, &crate::render::tv_slot(signed));
    }

    /// Writes the sign-in panel (`status`: a line under the code).
    fn tv_panel(&mut self, status: &str) {
        let signed = tv::bearer_at(bridge::epoch()).is_some();
        bridge::html(SIGN, &crate::render::tv_panel(self.tv.code.as_ref().map(|c| (c.user_code.as_str(), c.url.as_str())), signed, status));
        if !self.tv.open || !bridge::any("#ytc-tvsign.on") {
            bridge::focus_save("signin");
        }
        self.tv.open = true;
        bridge::attr(SIGN, "class", "tvs on");
        if !bridge::any("#ytc-tvsign :focus") {
            bridge::focus_sel(SIGN, "button");
        }
    }

    fn tv_close(&mut self) {
        self.tv.open = false;
        self.tv.generation += 1;
        self.tv.code = None;
        bridge::attr(SIGN, "class", "tvs");
        if !bridge::focus_load("signin") {
            bridge::focus_sel(ACCT, "button");
        }
    }

    /// `tv-*` clicks.
    pub(super) fn tv_click(&mut self, act: &str) -> bool {
        match act {
            "tv-signin" | "tv-newcode" => {
                self.tv.code = None;
                self.tv.generation += 1;
                self.tv_panel("Getting a code…");
                self.tv_call("auth_start", "{}", Call::Start);
            }
            "tv-account" => self.tv_panel(""),
            "tv-signout" => {
                self.tv_panel("Signing out…");
                self.tv_call("auth_signout", "{}", Call::SignOut);
            }
            "tv-close" => self.tv_close(),
            "tv-search" => self.tv_search_open(),
            "tv-mic" => {
                self.tv.voice_mic = true;
                self.tv_voice_start();
            }
            "tv-about" => self.tv_about_open(),
            "tv-about-close" => self.tv_about_close(),
            "tv-check" => {
                if let Some(i) = self.tv.shell.as_mut() {
                    i.state = tv::UpdateState::Checking;
                }
                self.tv.about_note.clear();
                self.tv_about_paint();
                self.tv_call("shell_check", "null", Call::Check);
            }
            "tv-apply" => {
                // The shell has the new interface in place (verified, injected for the next load): load it now,
                // here (the sign-in lives in the shell and this tab's session mirror).
                bridge::replace_doc(&self.url.clone());
            }
            "tv-install" => {
                self.tv.about_note.clear();
                self.tv_call("shell_install", "null", Call::Install);
            }
            "tv-info" => self.tv_info_open(),
            "tv-info-close" => self.tv_info_close(),
            "tv-comments" => self.tv_comments_open(),
            "tv-comments-close" => self.tv_comments_close(),
            "tv-save" => {
                self.tv_save_open();
            }
            "tv-save-close" => self.tv_save_close(),
            "tv-tel-on" => {
                // (An address typed but not submitted yet goes along.)
                let typed = bridge::input_value(DEV_FIELD);
                let known = self.tv.tel.as_ref().map(|t| tv::tel_addr(&t.url).to_string()).unwrap_or_default();
                let url = (!typed.trim().is_empty() && typed.trim() != known).then_some(typed);
                self.tv_dev_note("Turning on…", false);
                self.tv_call("tel_set", &tv::tel_set_json(Some(true), url.as_deref()), Call::TelSet);
            }
            "tv-tel-off" => {
                self.tv_dev_note("Turning off…", false);
                self.tv_call("tel_set", &tv::tel_set_json(Some(false), None), Call::TelSet);
            }
            "tv-tel-find" => {
                self.tv_dev_note("Looking for the debug center on your network…", false);
                self.tv_call("tel_find", "null", Call::TelFind);
            }
            _ => return false,
        }
        true
    }

    // ---------------------------------------------------------- search screen

    /// The search screen is open (the header shown as a full screen: the field, recent searches and
    /// suggestions as a list; client.css `#ytc[data-tvq]`).
    pub(super) fn tv_search_on(&self) -> bool {
        tv::on() && bridge::any("#ytc[data-tvq='1']")
    }

    fn tv_search_open(&mut self) {
        if !self.tv_search_on() {
            bridge::focus_save("search");
        }
        bridge::attr("ytc", "data-tvq", "1");
        self.tv_keyboard_open();
    }

    /// Closes the search screen; `refocus`: the focus returns to where it was before it opened.
    pub(super) fn tv_search_close(&mut self, refocus: bool) {
        if !self.tv_search_on() {
            return;
        }
        self.tv_keyboard_hide();
        bridge::attr("ytc", "data-tvq", "");
        self.fx_sug_end();
        if refocus && !bridge::focus_load("search") {
            bridge::focus_sel("ytc-side", "[data-nav=search]");
        }
    }

    /// The search field took the focus (D-pad or OK): on TV it always shows as the search screen.
    pub(super) fn tv_search_focus(&mut self, on: bool) {
        if tv::on() && on && !self.tv_search_on() {
            bridge::focus_save("search");
            bridge::attr("ytc", "data-tvq", "1");
        }
        if on {
            self.tv_keyboard_label();
        }
    }

    /// A search was submitted (field or list): the screen closes and the results take the focus.
    pub(super) fn tv_searched(&mut self) {
        if tv::on() {
            self.tv.grid_focus = true;
            self.tv_search_close(false);
        }
    }

    // ---------------------------------------------------------- watch page

    /// A player was mounted: the overlay's Save button (Watch later, signed in only).
    pub(super) fn tv_mounted(&mut self, v: &str) {
        if !tv::on() {
            return;
        }
        self.tv.info.clear();
        let mut out = String::new();
        crate::features::wl_button(&mut out, v);
        if let Some(end) = out.rfind("</button>") {
            out.insert_str(end, "<span class=\"tvpl\" aria-hidden=\"true\">Watch later</span>");
        }
        bridge::html(crate::player::ui::TVWL, &out);
        // (A retried player's row is new: its other actions from what the page already knows.)
        if let Some(w) = self.cur_watch() {
            bridge::html(crate::player::ui::TVCH, &crate::render::tv_channel(&w));
            self.acts.tv_row_refresh();
        }
        if matches!(self.route, Route::Watch { .. }) {
            tv::trail_push(&mut self.tv.trail, self.route.clone(), TRAIL_MAX);
        }
        self.tv_focus_check();
    }

    /// The remote's Next: Up next's first (a playlist's next item, else YouTube's next video).
    fn tv_next(&mut self) {
        if let Some(n) = self.tv_next_video() {
            self.up_cancel();
            self.up_open(n);
        }
    }

    fn tv_next_video(&self) -> Option<crate::features::upnext::Next> {
        let Route::Watch { v, .. } = &self.route else { return None };
        let w = self.cur_watch()?;
        crate::features::upnext::next(&w.up, v, true).or_else(|| w.related.items.iter().find_map(|i| match i {
            crate::parse::Item::Video(c) if c.id != *v => Some(crate::features::upnext::Next { v: c.id.clone(), list: None, now: true }),
            _ => None,
        }))
    }

    /// The remote's Previous: the playlist's previous item, else the previous video of this session, else this one from
    /// the start. A video right behind this entry is a step back through the history (as if Next was never pressed: Back
    /// then leaves as it would have), anything else opens as a new entry.
    fn tv_prev(&mut self) {
        self.up_cancel();
        let listed = match (&self.route, self.cur_watch()) {
            (Route::Watch { v, .. }, Some(w)) => crate::features::upnext::prev(&w.up, v),
            _ => None,
        };
        let target = match listed {
            Some(n) => {
                // (Walking a playlist backward, this video is no longer on the way back.)
                self.tv.trail.pop();
                Some(Route::Watch { v: n.v, t: 0, list: n.list })
            }
            None => tv::trail_back(&mut self.tv.trail),
        };
        match target {
            Some(r) => {
                if self.tv.history.behind().and_then(crate::route::parse).as_ref() == Some(&r) {
                    bridge::history_back();
                } else {
                    let url = r.url();
                    self.navigate(r, url, super::Mode::Push);
                }
            }
            None => {
                if let Some(p) = self.player.as_mut() {
                    p.input(true);
                    p.seek_to(0.0);
                    p.wake();
                }
                self.after_player();
            }
        }
    }

    /// A channel page opened from a video is one step away from it (`on_back`), and the video comes back as it was left.
    pub(super) fn tv_channel_opened(&mut self, route: &Route) {
        if tv::on() && matches!(route, Route::Channel { .. }) && matches!(self.route, Route::Watch { .. }) {
            self.tv.next_from_watch = true;
            if let Some(p) = self.player.as_ref() {
                if self.tv.left.len() > 64 {
                    self.tv.left.clear();
                }
                self.tv.left.insert(self.url.clone(), (p.resume_at(), p.paused_now()));
            }
        }
    }

    /// A navigation took place (`self.url` is the new page): the history mirror, and what a video returned to takes back.
    pub(super) fn tv_navigated(&mut self, pop: bool) {
        if !tv::on() {
            return;
        }
        let from_watch = std::mem::take(&mut self.tv.next_from_watch);
        self.tv.returned = None;
        if pop {
            self.tv.history.pop(&self.url);
            self.tv.returned = self.tv.left.remove(&self.url);
        } else {
            self.tv.history.push(&self.url, from_watch);
            // (Opened anew: how it was left on an older entry is not where it is now.)
            self.tv.left.remove(&self.url);
        }
    }

    /// This channel page's id (from the route, or learned with the page) and title: its Subscribe button.
    fn tv_channel_paint(&mut self) {
        let Route::Channel { key, .. } = &self.route else { return };
        let id = match key {
            crate::route::ChannelKey::Id(id) => Some(id.clone()),
            crate::route::ChannelKey::Path(p) => self.channels.get(p).cloned(),
        };
        let title = match self.feed.head.as_ref() {
            Some(crate::render::Head::Channel { title, .. }) => title.clone(),
            _ => String::new(),
        };
        if let Some(id) = id.filter(|i| i.starts_with("UC") && crate::util::is_safe_id(i)) {
            self.acts.tv_channel(&crate::actions::Net { cfg: &self.cfg, hl: &self.hl, url: &self.url }, &id, &title);
        }
    }

    /// The autoplay countdown appeared (Play now takes the focus) or went (the focus returns to the bar).
    pub(super) fn tv_countdown(&mut self, pending: bool) {
        if !tv::on() {
            return;
        }
        if pending {
            bridge::focus_sel(UPNEXT_ID, "#ytc-up-now");
            self.tv_wake();
        } else if !bridge::any(":focus") || bridge::any("body:focus") {
            bridge::focus(SEEK);
        }
    }

    // ---------------------------------------------------------- About & updates

    fn tv_about_open(&mut self) {
        if !self.tv.about {
            bridge::focus_save("about");
        }
        self.tv.about = true;
        self.tv.noticed = self.tv.shell.as_ref().map_or(0, |i| i.seq.max(1));
        bridge::attr(NOTICE, "class", "tvup");
        self.tv_about_paint();
        bridge::attr(ABOUT, "class", "tvs on");
        bridge::focus_sel(ABOUT, ".tvb");
        // Fresh from the shell (what its own check found meanwhile), and its telemetry state.
        self.tv_call("shell_info", "null", Call::Info { boot: false });
        self.tv_call("tel_info", "null", Call::TelInfo);
    }

    fn tv_about_close(&mut self) {
        self.tv.about = false;
        self.tv.dev_taps = 0;
        self.tv.dev_note.clear();
        bridge::attr(ABOUT, "class", "tvs");
        if !bridge::focus_load("about") {
            bridge::focus_sel("ytc-side", "[data-nav=about]");
        }
    }

    /// Writes the screen (when open), keeping the focus on the button with the same action if it is still there.
    fn tv_about_paint(&mut self) {
        if !self.tv.about {
            return;
        }
        // (Never under the owner's fingers: the address field keeps what is being typed until it is submitted.)
        if bridge::any("#ytc-tvdc-q:focus") {
            return;
        }
        let dev = if self.tv.dev && self.tv.tel_known { crate::render::tv_dev(self.tv.tel.as_ref(), &self.tv.dev_note, self.tv.dev_err) } else { String::new() };
        let html = crate::render::tv_about(self.tv.shell.as_ref(), self.tv.running, &self.tv.running_client, bridge::epoch(), &self.tv.about_note, &dev);
        if html == bridge::get_html(ABOUT) {
            return;
        }
        let had = bridge::any("#ytc-tvabout :focus");
        // The focus stays on the control with the same action (the Developer section's buttons swap: on ↔ off).
        let act = ["tv-tel-on", "tv-tel-off", "tv-tel-find"].into_iter().find(|a| bridge::any(&format!("#ytc-tvabout [data-act={a}]:focus")));
        bridge::focus_save("about-btn");
        bridge::html(ABOUT, &html);
        if let Some(a) = act {
            bridge::focus_sel(ABOUT, &format!(".tvdv [data-act={a}],.tvdv [data-act^=tv-tel-]"));
        } else if had && !bridge::focus_load("about-btn") {
            bridge::focus_sel(ABOUT, ".tvb");
        }
    }

    /// An update is ready (a newer interface in the shell, or a newer app offered): the rail's item gets an accent dot
    /// and, away from videos, a small notice the remote can reach (top right) until the screen was opened for it.
    fn tv_notice(&mut self) {
        let Some(i) = &self.tv.shell else { return };
        let ready = (self.tv.running > 0 && i.seq > self.tv.running) || i.apk.is_some();
        bridge::class_all("#ytc-side .rl[data-nav=about]", "upd", ready);
        let show = ready && !self.tv.about && self.tv.noticed != i.seq.max(1);
        bridge::attr(NOTICE, "class", if show { "tvup on" } else { "tvup" });
    }

    pub(super) fn tv_updates_tick(&mut self, fast: bool) {
        if !tv::on() {
            return;
        }
        if fast && !self.tv.about {
            return;
        }
        self.tv_call("shell_info", "null", Call::Info { boot: false });
        if !fast {
            let id = self.timer(Timer::TvUpdates { fast: false });
            bridge::timer(id, UPDATES_EVERY_MS);
        }
    }

    // ---------------------------------------------------------- Back

    /// The platform Back button: 1 = handled here, 0 = the shell goes back (or leaves the app when there is
    /// nothing to go back to), 2 = leave the app (Back on the rail at Home).
    pub fn on_back(&mut self) -> u32 {
        if !tv::on() {
            return 0;
        }
        if self.tv_keyboard_back() {
            return 1;
        }
        if self.tv.open {
            self.tv_close();
            return 1;
        }
        if self.tv.about {
            self.tv_about_close();
            return 1;
        }
        if self.tv_search_on() {
            self.tv_search_close(true);
            return 1;
        }
        if self.tv_drawer_back() {
            return 1;
        }
        if bridge::any(OPEN) {
            self.on_key("Escape", false);
            return 1;
        }
        if matches!(self.route, Route::Watch { .. }) {
            if bridge::any("#ytc-up .upc") {
                // The autoplay countdown: Back cancels it.
                self.up_cancel();
                bridge::focus(SEEK);
                return 1;
            }
            let Some(p) = self.player.as_mut() else { return 0 };
            if p.tv_scrubbing() {
                p.tv_scrub_end();
                return 1;
            }
            // Up next → the buttons; the overlay open while playing → it hides; else the watch page is left.
            if bridge::any(".wr :focus") {
                if !bridge::focus_load("row") {
                    bridge::focus_sel(PLAYER, ROW_FIRST);
                }
                p.wake();
                return 1;
            }
            if !p.paused() && !p.idle() && bridge::any("#ytc-player :focus") {
                p.rest();
                bridge::focus(SEEK);
                return 1;
            }
            return 0;
        }
        // A channel page opened from a video: Back is the way back to it.
        if self.tv.history.from_watch() && !bridge::any("#ytc-side :focus") {
            return 0;
        }
        if !bridge::any("#ytc-side :focus") {
            // Back from the content focuses the rail (on the page's own item).
            bridge::focus_save("content");
            self.tv_rail_focus();
            return 1;
        }
        if matches!(self.route, Route::Home) {
            return 2;
        }
        0
    }

    /// The rail's item for this page (else Home).
    fn tv_rail_focus(&mut self) {
        let nav = self.route.nav_key();
        let nav = if crate::render::side_index(nav).is_some() { nav } else { "home" };
        bridge::focus_sel("ytc-side", &format!("[data-nav={nav}]"));
    }

    // ---------------------------------------------------------- keys

    /// TV keys (before the desktop handling). Some(handled) ends the key here.
    pub(super) fn tv_key(&mut self, key: &str, editable: bool) -> Option<bool> {
        if !tv::on() {
            return None;
        }
        // The remote's search and voice keys, where the system delivers them.
        if key == tv::VOICE_KEY {
            if !bridge::any(PANELS) {
                self.tv_voice_start();
            }
            return Some(true);
        }
        if let Some(handled) = self.tv_keyboard_key(key, editable) {
            return Some(handled);
        }
        if key == "/" && !editable {
            self.tv_search_open();
            return Some(true);
        }
        self.tv.auto_rail = false;
        let watch = matches!(self.route, Route::Watch { .. });
        if let Some(r) = tv::remote(key) {
            if !watch {
                return Some(false);
            }
            match r {
                // (Not from under the search screen: what is being typed stays.)
                tv::Remote::Next | tv::Remote::Prev if self.tv_search_on() => return Some(true),
                tv::Remote::Next => {
                    self.tv_next();
                    return Some(true);
                }
                tv::Remote::Prev => {
                    self.tv_prev();
                    return Some(true);
                }
                _ => {}
            }
            let paused = self.player.as_ref().is_none_or(|p| p.paused());
            let k = tv::remote_player_key(r, paused)?;
            if let Some(p) = self.player.as_mut() {
                p.wake();
            }
            return Some(self.tv_player_key(k));
        }
        let d = tv::dir(key);
        if editable {
            // The search field: ↓ goes to the screen's list (its first row; the remove buttons are → from a row),
            // ↑/↓ otherwise leave it (the field keeps the TV keyboard's arrows while the keyboard is up: those
            // never reach the page).
            if d == Some(Dir::Down) && self.tv_search_on() && bridge::any("#ytc-sug .sg a") {
                bridge::focus_sel(crate::render::SUGGEST, ".sg a");
                return Some(true);
            }
            return match d {
                Some(dir @ (Dir::Up | Dir::Down)) => Some(self.tv_move(dir)),
                _ => None,
            };
        }
        if let Some(handled) = self.tv_drawer_key(key, d) {
            return Some(handled);
        }
        if watch {
            return self.tv_watch_key(key, d);
        }
        d.map(|d| self.tv_move(d))
    }

    /// The watch page: the picture with the overlay (time, seek bar, buttons) and the Up next row under it.
    fn tv_watch_key(&mut self, key: &str, d: Option<Dir>) -> Option<bool> {
        // A player menu opened from the remote owns the arrows (the player walks its rows).
        if bridge::any("#ytc-player.menu #ytc-menu") {
            return None;
        }
        let ok = matches!(key, "Enter" | " ");
        if !ok && d.is_none() {
            return None;
        }
        // A held OK is one press: not a play and pause on every repeat, and the hold that opened this video is not its pause.
        if ok && bridge::key_held() {
            return Some(true);
        }
        if bridge::any("#ytc-player .pfail") {
            // The failure panel: ←/→ between its actions (OK presses the focused one).
            if !bridge::any("#ytc-player .pfail :focus") {
                bridge::focus_sel(PLAYER, ".pfail .pfbt");
                return Some(true);
            }
            return match d {
                Some(d @ (Dir::Left | Dir::Right)) => {
                    self.tv_move_in("#ytc-player .pfb", d);
                    Some(true)
                }
                Some(_) => Some(true),
                None => None,
            };
        }
        if bridge::any("#ytc-player .pstuck :focus") {
            // The stuck status: ←/→ between its actions, ↓ back to the bar (OK presses the focused one).
            return match d {
                Some(d @ (Dir::Left | Dir::Right)) => {
                    self.tv_move_in("#ytc-player .pstuck", d);
                    Some(true)
                }
                Some(_) => {
                    bridge::focus(SEEK);
                    Some(true)
                }
                None => None,
            };
        }
        let in_up = bridge::any(".wr :focus");
        let on_seek = bridge::any("#ytc-seek:focus");
        let in_row = bridge::any("#ytc-player .prow :focus");
        let in_row_or_skip = in_row || bridge::any("#ytc-sbtb:focus");
        let hidden = self.player.as_ref().is_some_and(|p| p.idle()) && !in_up;
        if hidden || !(in_up || on_seek || in_row_or_skip) {
            // The overlay is hidden (or nothing of it has the focus): the first key shows it on the seek bar.
            // OK plays/pauses, ←/→ already scrub, ↑/↓ only show it.
            bridge::focus(SEEK);
            return Some(match d {
                Some(Dir::Left) => self.tv_scrub(-1.0),
                Some(Dir::Right) => self.tv_scrub(1.0),
                _ if ok => self.tv_toggle(),
                _ => {
                    self.tv_wake();
                    true
                }
            });
        }
        self.tv_wake();
        if on_seek {
            return Some(match d {
                Some(Dir::Left) => self.tv_scrub(-1.0),
                Some(Dir::Right) => self.tv_scrub(1.0),
                Some(Dir::Down) => {
                    if !bridge::focus_load("row") {
                        bridge::focus_sel(PLAYER, ROW_FIRST);
                    }
                    true
                }
                Some(Dir::Up) => {
                    // The stuck status's actions (Try again, Open on YouTube) sit above, top right.
                    if bridge::any("#ytc-player.stuck .pstuck .pfbt") {
                        bridge::focus_sel(PLAYER, ".pstuck .pfbt");
                    }
                    true
                }
                None => {
                    // OK on the bar: a running scrub seeks now, else play/pause.
                    if self.player.as_ref().is_some_and(|p| p.tv_scrubbing()) {
                        if let Some(p) = self.player.as_mut() {
                            p.tv_scrub_end();
                        }
                        true
                    } else {
                        self.tv_toggle()
                    }
                }
            });
        }
        let d = d?;
        let skip = bridge::any("#ytc-player .psbt.on");
        if bridge::any("#ytc-sbtb:focus") {
            // The skip notice (above the row's right end).
            match d {
                Dir::Up => bridge::focus(SEEK),
                Dir::Down | Dir::Left => {
                    if !bridge::focus_load("row") {
                        bridge::focus_sel(PLAYER, ".prow > :last-child");
                    }
                }
                Dir::Right => {}
            }
            return Some(true);
        }
        if in_row {
            let last = bridge::any("#ytc-player .prow > :last-child:focus");
            return Some(match d {
                Dir::Left | Dir::Right => {
                    if !self.tv_move_in("#ytc-player .prow", d) && d == Dir::Right && skip {
                        bridge::focus_save("row");
                        bridge::focus(crate::player::ui::SBTB);
                    }
                    true
                }
                Dir::Up => {
                    bridge::focus_save("row");
                    if last && skip {
                        bridge::focus(crate::player::ui::SBTB);
                    } else {
                        bridge::focus(SEEK);
                    }
                    true
                }
                Dir::Down => {
                    bridge::focus_save("row");
                    if !bridge::focus_load("up") {
                        self.tv_up_first();
                    }
                    true
                }
            });
        }
        // The Up next row.
        Some(match d {
            Dir::Left | Dir::Right => {
                self.tv_move_in(UPNEXT, d);
                bridge::focus_save("up");
                true
            }
            Dir::Up => {
                bridge::focus_save("up");
                if !bridge::focus_load("row") {
                    bridge::focus_sel(PLAYER, ROW_FIRST);
                }
                true
            }
            Dir::Down => true,
        })
    }

    /// The first related card in view of the Up next row.
    fn tv_up_first(&mut self) {
        bridge::focus_sel("ytc-watch", ".wr #ytc-list a.t");
    }

    fn tv_wake(&mut self) {
        if let Some(p) = self.player.as_mut() {
            p.wake();
        }
    }

    fn tv_toggle(&mut self) -> bool {
        self.tv_wake();
        self.on_key("k", false)
    }

    /// ←/→ on the seek bar: one step (faster while the key is held); the seek goes out when the remote rests.
    fn tv_scrub(&mut self, dir: f64) -> bool {
        let now = bridge::now();
        self.tv.scrub_n = if now - self.tv.scrub_at < HELD_MS { self.tv.scrub_n + 1 } else { 0 };
        self.tv.scrub_at = now;
        let step = scrub_step(self.tv.scrub_n);
        let Some(p) = self.player.as_mut() else { return true };
        if !p.tv_scrub(dir * step) {
            // Live: plain 10 s seeks.
            return self.on_key(if dir < 0.0 { "j" } else { "l" }, false);
        }
        self.after_player();
        self.tv.scrub_gen += 1;
        let id = self.timer(Timer::TvScrub { generation: self.tv.scrub_gen });
        bridge::timer(id, SCRUB_REST_MS);
        true
    }

    pub(super) fn tv_scrub_rest(&mut self, generation: u32) {
        if generation != self.tv.scrub_gen {
            return;
        }
        if let Some(p) = self.player.as_mut() {
            if p.tv_scrubbing() {
                p.tv_scrub_end();
            }
        }
        self.after_player();
    }

    /// Moves focus inside `scope` only; false when nothing lies that way.
    pub(super) fn tv_move_in(&mut self, scope: &str, d: Dir) -> bool {
        let flat = bridge::spatial_rects(scope, SEL, "", "", CARD);
        let Some((rects, _, cur)) = split(&flat) else { return false };
        let Some(c) = cur else { return false };
        match tv::pick(rects[c], &rects, d, Some(c)) {
            Some(i) => {
                bridge::spatial_focus(i as u32, -1.0);
                true
            }
            None => false,
        }
    }

    /// Moves focus in direction `d` (inside an open panel when there is one). A card of the page's grid
    /// lands where the first row sits (the focused row keeps one vertical place; instant, so a held key
    /// never waits); entering the rail focuses this page's item and leaving it returns to where the
    /// focus was in the content.
    fn tv_move(&mut self, d: Dir) -> bool {
        let flat = bridge::spatial_rects(TRAPS, SEL, SKIP, MARKS, CARD);
        let Some((rects, marks, cur)) = split(&flat) else {
            self.tv_focus_default();
            return true;
        };
        let Some(c) = cur else {
            self.tv_focus_default();
            return true;
        };
        // In the rail ↑/↓ walk the rail only (the open rail lies over the cards).
        let pick = if marks[c] == 2 && matches!(d, Dir::Up | Dir::Down) {
            let only: Vec<tv::Rect> = rects.iter().zip(&marks).map(|(r, m)| if *m == 2 { *r } else { tv::Rect { x: 0.0, y: 0.0, w: 0.0, h: 0.0 } }).collect();
            tv::pick(rects[c], &only, d, Some(c))
        } else {
            tv::pick(rects[c], &rects, d, Some(c))
        };
        let Some(i) = pick else {
            if d == Dir::Up && marks[c] != 2 {
                bridge::scroll_to(0.0);
            }
            // About & updates: → five times where nothing lies to the right shows the Developer section.
            if self.tv.about && d == Dir::Right && !self.tv.dev {
                self.tv.dev_taps += 1;
                if self.tv.dev_taps >= tv::DEV_TAPS {
                    self.tv.dev = true;
                    self.tv_call("tel_info", "null", Call::TelInfo);
                }
            }
            return true;
        };
        self.tv.dev_taps = 0;
        let in_rail = marks[c] == 2;
        if !in_rail && marks[i] == 2 && !self.tv_search_on() {
            bridge::focus_save("content");
            self.tv_rail_focus();
            return true;
        }
        if in_rail && marks[i] != 2 && bridge::focus_load("content") {
            return true;
        }
        let y = if marks[i] == 1 { tv::row_scroll(&rects, &marks, c, i) } else { -1.0 };
        bridge::spatial_focus(i as u32, y);
        if self.held.is_some() && bridge::scroll_y() < 1.0 {
            // A fresher feed arrived while the page was scrolled down (the desktop's "New videos" pill): it takes
            // the place once the focus is back at the top.
            self.on_click("", "fresh", "", 32);
        }
        true
    }

    /// The player's failure panel replaced the controls: its first action takes the focus.
    pub(super) fn tv_failed(&mut self) {
        if tv::on() {
            bridge::focus_sel(PLAYER, ".pfail .pfbt");
        }
    }

    /// A link was opened (remote or pointer): remembered for coming back to this page.
    pub(super) fn tv_opened(&mut self, href: &str) {
        if tv::on() && !href.is_empty() && !href.contains(['"', '\\']) {
            if self.tv.opened.len() > 64 {
                self.tv.opened.clear();
            }
            self.tv.opened.insert(self.url.clone(), href.to_string());
        }
    }

    /// Nothing focused: the seek bar on watch pages, else the link last opened from this page, else
    /// the first card in view, else the first stop of the page, else the rail.
    pub(super) fn tv_focus_default(&mut self) {
        if self.tv_search_on() {
            if bridge::any("#ytc[data-tvk='1']") {
                bridge::focus_sel(tv::keyboard::ID, ".tvk-keys button");
            } else {
                bridge::focus(tv::keyboard::QUERY);
            }
            return;
        }
                if matches!(self.route, Route::Watch { .. }) {
            if bridge::any("#ytc-player .pfail .pfbt") {
                bridge::focus_sel(PLAYER, ".pfail .pfbt");
                return;
            }
            bridge::focus(SEEK);
            if !bridge::any("#ytc-seek:focus") {
                bridge::attr(PLAYER, "tabindex", "-1");
                bridge::focus(PLAYER);
            }
            return;
        }
        if let Some(h) = self.tv.opened.get(&self.url) {
            bridge::focus_sel(MAIN, &format!("a.t[href=\"{h}\"]"));
            if bridge::any("#ytc-main :focus") {
                return;
            }
        }
        // The first card whose top is in view, its row put where the first row sits.
        let flat = bridge::spatial_rects("", SEL, SKIP, MARKS, CARD);
        if let Some((rects, marks, _)) = split(&flat) {
            if let Some(i) = (0..rects.len()).find(|&i| marks[i] == 1 && rects[i].y >= 0.0).or_else(|| marks.iter().position(|&m| m == 1)) {
                bridge::spatial_focus(i as u32, tv::row_scroll(&rects, &marks, usize::MAX, i));
                return;
            }
        }
        bridge::focus_sel(MAIN, ".tvb,a[href],button");
        if !bridge::any("#ytc-main :focus") {
            // Nothing in the page yet (loading): the page itself holds the focus (no ring, the rail stays closed)
            // until the content arrives and takes it.
            bridge::attr(MAIN, "tabindex", "-1");
            bridge::focus_sel("ytc", "#ytc-main");
            self.tv.auto_rail = true;
        }
    }

    /// After a page paint: keep a focus on screen (never lost).
    pub(super) fn tv_after_paint(&mut self) {
        if !tv::on() {
            return;
        }
        self.tv_channel_paint();
        if std::mem::take(&mut self.tv.grid_focus) && !matches!(self.route, Route::Watch { .. }) {
            // The search went out from the search screen: its results take the focus.
            bridge::blur();
            bridge::scroll_to(0.0);
        }
        self.tv_focus_check();
        // The page that was left may still drop the focus while it is torn down (the player): look
        // again once that has run.
        let id = self.timer(Timer::TvFocus);
        bridge::timer(id, 300);
    }

    /// Focus is never lost on TV: nothing focused → the default stop.
    pub(super) fn tv_focus_check(&mut self) {
        if !tv::on() {
            return;
        }

        if self.tv.note_until > 0.0 && bridge::epoch() >= self.tv.note_until {
            self.tv.note_until = 0.0;
            bridge::attr(NOTE, "class", "tvnote");
        }

        if self.tv_drawer_focus() {
            return;
        }

        if !bridge::any(":focus") || bridge::any("body:focus") {
            self.tv.auto_rail = false;
            self.tv_focus_default();
        } else if bridge::any("#ytc-player:focus") && bridge::any("#ytc-seek,#ytc-player .pfail .pfbt") {
            // The player box held the focus until its controls were written.
            self.tv_focus_default();
        } else if bridge::any("#ytc-main:focus") && bridge::any("#ytc-main a.t,#ytc-main .tvb,#ytc-player .pfail .pfbt,#ytc-seek") {
            // The page only held the focus while it loaded: the content takes it.
            self.tv.auto_rail = false;
            bridge::blur();
            self.tv_focus_default();
        }
    }
}

/// `[x, y, w, h, m]*n, current` → rects, marks and the focused index.
pub(super) fn split(flat: &[f64]) -> Option<(Vec<tv::Rect>, Vec<u8>, Option<usize>)> {
    if flat.len() < 6 {
        return None;
    }
    let n = (flat.len() - 1) / 5;
    let cur = flat[flat.len() - 1];
    let cur = (cur >= 0.0 && (cur as usize) < n).then_some(cur as usize);
    let rows = &flat[..n * 5];
    let rects = rows.chunks_exact(5).map(|c| tv::Rect { x: c[0], y: c[1], w: c[2], h: c[3] }).collect();
    let marks = rows.chunks_exact(5).map(|c| c[4] as u8).collect();
    Some((rects, marks, cur))
}

fn short(json: &str) -> String {
    let s = crate::json::parse_str(json).and_then(|v| v.as_str().map(String::from)).unwrap_or_else(|| json.to_string());
    s.chars().take(120).collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn split_reads_the_bridge_answer() {
        let (r, m, c) = super::split(&[0.0, 0.0, 10.0, 10.0, 0.0, 20.0, 0.0, 10.0, 10.0, 2.0, 1.0]).unwrap();
        assert_eq!((r.len(), m, c), (2, vec![0, 2], Some(1)));
        let (_, _, c) = super::split(&[0.0, 0.0, 10.0, 10.0, 1.0, -1.0]).unwrap();
        assert_eq!(c, None);
        assert!(super::split(&[-1.0]).is_none());
    }

    #[test]
    fn scrub_speeds_up_while_held() {
        assert_eq!(super::scrub_step(0), 10.0);
        assert_eq!(super::scrub_step(5), 10.0);
        assert!(super::scrub_step(6) > 10.0 && super::scrub_step(20) > super::scrub_step(6) && super::scrub_step(40) > super::scrub_step(20));
    }
}
