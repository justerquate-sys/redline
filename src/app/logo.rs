//! Hover navigation: the logo and desktop sidebar share the same cancellable dwell. Resting the pointer on the
//! header logo (`data-hov="logo"`: the mark or the word) for `DWELL_MS` goes Home; the logo's soft
//! pill fills left to right over that time (client.css `.logo.dw`). Home's first page is fetched the
//! moment the pointer enters, so at the end of the dwell Home renders from memory. Passing over never
//! navigates: leaving the logo cancels, coming back starts over, and a dwell that ran (or a press on
//! the logo) needs the pointer to leave before another one starts. Clicks and keys are unchanged.
//!
//! Home while on Home (click, `g h` or dwell) keeps the page: back to the top at once, then a new
//! Home from YouTube (the ready response when there is one), like YouTube's logo on its Home; no
//! history entry.

use super::{App, Mode, Timer, PAGE_TTL_MS};
use crate::bridge;
use crate::render::{LOGO_ID, PILL};
use crate::route::Route;
use crate::snap;

#[derive(Clone, Copy, PartialEq)]
enum Target {
    Logo,
    Side(&'static str),
}

/// Rest this long on the logo or a sidebar link (client.css draws the fill over the same time).
pub const DWELL_MS: u32 = 250;
/// The pointer moved this recently (the bridge reports moves at most every 150 ms): entering is
/// the pointer's own doing. Otherwise the logo came under a still pointer (the watch page's top bar
/// scrolls away and back) and the dwell waits for the pointer's first move on it.
const MOVED_MS: f64 = 200.0;
/// Nothing starts while one of these is open: a popover or menu (account, notifications, Save,
/// Share, the filter panel, the topic editor, the search dropdown, a player menu), the shortcut
/// sheet or the import dialog, or while a text field has focus (being typed in).
const BUSY: &str = "#ytc [aria-haspopup][aria-expanded=true],#ytc-q[aria-expanded=true],#ytc .open[id^=ytc-fx-],#ytc-keys.on,#ytc-imp.on,#ytc-menu[data-m]:not([data-m='']),\
#ytc :is(input:not([type=range],[type=checkbox],[type=radio],[type=button]),textarea,select,[contenteditable]):focus";

#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub(super) enum Dwell {
    /// The pointer is not on the logo.
    #[default]
    Out,
    /// On it, but it arrived under a still pointer: the first move starts the dwell.
    Waiting,
    /// Resting on it: the timer runs and the pill fills.
    Running,
    /// On it, but this visit is used up (it went Home, was pressed, or something was open):
    /// the pointer has to leave first.
    Spent,
}

#[derive(Default)]
pub(super) struct Logo {
    target: Option<Target>,
    state: Dwell,
    generation: u32,
    /// The last pointer move (`on_move`).
    moved: f64,
}

/// What a hover-zone report does to the dwell (pure; `App::logo_hov` applies it). `moving`: the
/// pointer moved just now. Returns the new state and whether the dwell starts.
fn next(state: Dwell, inside: bool, held: bool, busy: bool, moving: bool) -> (Dwell, bool) {
    match (state, inside) {
        (_, false) => (Dwell::Out, false),
        // Travel with a button held is no intent; a press on the logo is a click.
        (_, true) if held => (Dwell::Spent, false),
        (Dwell::Out, true) if !moving => (Dwell::Waiting, false),
        (Dwell::Out, true) if busy => (Dwell::Spent, false),
        (Dwell::Out, true) => (Dwell::Running, true),
        (s, true) => (s, false),
    }
}

impl App {
    /// Every hover-zone change (`on_hov`): entering the logo starts the dwell, leaving cancels it.
    pub(super) fn logo_hov(&mut self, zone: &str, arg: &str, held: bool) {
        let target = if zone == "logo" { Some(Target::Logo) } else if zone == "side" && !crate::tv::on() {
            crate::render::side_href(arg).and_then(crate::route::parse).map(|r| Target::Side(r.nav_key()))
        } else { None };
        if self.logo.target != target {
            if self.logo.state == Dwell::Running { self.logo_cancel(); }
            self.logo.state = Dwell::Out;
            self.logo.target = target;
        }
        let inside = target.is_some();
        let state = self.logo.state;
        if !inside && state == Dwell::Out {
            return;
        }
        let moving = bridge::now() - self.logo.moved <= MOVED_MS;
        let busy = inside && !held && moving && state == Dwell::Out && self.logo_busy();
        let (to, start) = next(state, inside, held, busy, moving);
        if state == Dwell::Running && to != Dwell::Running {
            self.logo_cancel();
        }
        self.logo.state = to;
        if start {
            self.logo_start();
        }
    }

    /// A pointer move (`on_move`): a logo that came under the pointer starts its dwell now.
    pub(super) fn logo_moved(&mut self, now: f64) {
        self.logo.moved = now;
        if self.logo.state == Dwell::Waiting {
            if self.logo_busy() {
                self.logo.state = Dwell::Spent;
            } else {
                self.logo.state = Dwell::Running;
                self.logo_start();
            }
        }
    }

    fn logo_start(&mut self) {
        match self.logo.target {
            Some(Target::Logo | Target::Side("home")) => self.logo_prefetch(),
            Some(Target::Side(key)) => {
                if let Some(url) = crate::render::side_href(key) { self.prefetch_page(url); }
            }
            None => return,
        }
        self.logo.generation = self.logo.generation.wrapping_add(1);
        self.logo_mark(true);
        let id = self.timer(Timer::Logo { generation: self.logo.generation });
        bridge::timer(id, DWELL_MS);
    }

    fn logo_cancel(&mut self) {
        self.logo.generation = self.logo.generation.wrapping_add(1);
        self.logo_mark(false);
    }

    fn logo_mark(&self, on: bool) {
        let id = match self.logo.target {
            Some(Target::Logo) => LOGO_ID.to_string(),
            Some(Target::Side(key)) => format!("ytc-nav-{key}"),
            None => return,
        };
        bridge::class_all(&format!("#{id}"), "dw", on);
    }

    /// A navigation started some other way (a click on the logo, a key): a running dwell is over.
    pub(super) fn logo_done(&mut self) {
        match self.logo.state {
            Dwell::Running => {
                self.logo_cancel();
                self.logo.state = Dwell::Spent;
            }
            Dwell::Waiting => self.logo.state = Dwell::Spent,
            _ => {}
        }
    }

    fn logo_busy(&self) -> bool {
        self.help || self.fx.imp_open || self.fx.ed_pin || bridge::any(BUSY)
    }

    /// The dwell ran out with the pointer still on the logo.
    pub(super) fn logo_fire(&mut self, generation: u32) {
        if generation != self.logo.generation || self.logo.state != Dwell::Running {
            return;
        }
        let route = match self.logo.target {
            Some(Target::Logo) => Route::Home,
            Some(Target::Side(key)) => {
                let Some(route) = crate::render::side_href(key).and_then(crate::route::parse) else { return };
                route
            }
            None => return,
        };
        let side = matches!(self.logo.target, Some(Target::Side(_)));
        self.logo_done();
        if self.logo_busy() {
            return;
        }
        crate::telemetry::act_label(if side { "sidebar.navigate" } else { "logo.home" }, Some("hover"), "");
        self.last_input = bridge::now();
        if route == Route::Home && self.home_in_place() {
            self.home_again();
            return;
        }
        if route == self.route { return; }
        let url = route.url();
        self.mo_click(false, &route);
        self.navigate(route, url, Mode::Push);
        self.mo_flush();
    }

    /// Home while on Home keeps the page (no Topic/YouTube chip feed is showing: then Home goes
    /// back to the plain feed through a navigation, as before).
    pub(super) fn home_in_place(&self) -> bool {
        self.route == Route::Home && !self.fx.topic_on() && !self.fx.chip_on()
    }

    /// Home on Home (like YouTube's logo on Home): the top of the page at once, then a new Home
    /// from YouTube: a held fresh feed (the New videos pill), else the ready response, else the
    /// request (the logo's prefetch when it has landed); the page stays until it answers.
    pub(super) fn home_again(&mut self) {
        self.logo_done();
        if let Some(body) = self.held.take() {
            bridge::html(PILL, "");
            bridge::scroll_to(0.0);
            self.hr.doc_meta = self.hr.held_meta.take();
            self.on_page_body(200, &body);
            return;
        }
        bridge::scroll_to(0.0);
        if let Some(r) = self.ready_take() {
            self.ready_show(r);
            return;
        }
        if self.rendered && !self.swr && self.await_page.is_none() {
            self.swr = true;
            self.load();
        }
    }

    /// The pointer entered the logo: Home's first page now (not after the hover delay), unless a
    /// ready response will paint it without a request.
    fn logo_prefetch(&mut self) {
        if self.ready_waiting() {
            return;
        }
        let url = Route::Home.url();
        if self.route == Route::Home && !self.home_in_place() {
            return;
        }
        self.prefetch_page_now(&url);
    }

    /// When a feed's snapshot in this tab was written (epoch ms; read once per document).
    pub(super) fn snap_written(&mut self, url: &str) -> Option<f64> {
        *self.snap_at.entry(url.to_string()).or_insert_with(|| {
            let s = bridge::session_get(&snap::key(url));
            if s.is_empty() { None } else { snap::from_json(&s, url).map(|sn| sn.at) }
        })
    }

    /// A navigation to `url` would paint its snapshot without asking YouTube (see `navigate`).
    pub(super) fn snap_fresh(&mut self, url: &str) -> bool {
        let bg = self.bg_age;
        self.snap_written(url).is_some_and(|at| bridge::epoch() - at < bg)
    }

    /// This tab has a snapshot of `url` (its index only: no snapshot is read).
    pub(super) fn snap_listed(&self, url: &str) -> bool {
        snap::index_from(&bridge::session_get(snap::INDEX)).iter().any(|u| u == url)
    }

    /// A prefetched first page of `url` is in memory.
    pub(super) fn page_cached(&self, url: &str) -> bool {
        let now = bridge::epoch();
        self.page_cache.iter().any(|(u, _, at)| u == url && now - at < PAGE_TTL_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passing_over_resting_and_pressing() {
        // Entering starts; staying is no new start; leaving resets.
        assert_eq!(next(Dwell::Out, true, false, false, true), (Dwell::Running, true));
        assert_eq!(next(Dwell::Running, true, false, false, true), (Dwell::Running, false));
        assert_eq!(next(Dwell::Running, false, false, false, true), (Dwell::Out, false));
        // A spent visit needs the pointer to leave first (after going Home, the pointer still rests there).
        assert_eq!(next(Dwell::Spent, true, false, false, true), (Dwell::Spent, false));
        assert_eq!(next(Dwell::Spent, false, false, false, true), (Dwell::Out, false));
        // A press (or held travel) is no dwell; its release over the logo starts none either.
        assert_eq!(next(Dwell::Running, true, true, false, true), (Dwell::Spent, false));
        assert_eq!(next(Dwell::Out, true, true, false, true), (Dwell::Spent, false));
        // Something open: no dwell on this visit.
        assert_eq!(next(Dwell::Out, true, false, true, true), (Dwell::Spent, false));
        // The logo came under a still pointer (scrolled back): wait for a move (`logo_moved`); leaving resets.
        assert_eq!(next(Dwell::Out, true, false, false, false), (Dwell::Waiting, false));
        assert_eq!(next(Dwell::Waiting, true, false, false, false), (Dwell::Waiting, false));
        assert_eq!(next(Dwell::Waiting, false, false, false, false), (Dwell::Out, false));
        assert_eq!(next(Dwell::Waiting, true, true, false, false), (Dwell::Spent, false));
    }

    #[test]
    fn the_fill_lasts_the_dwell() {
        let css = include_str!("../../extension/client.css");
        assert!(css.contains(&format!("transition:clip-path {DWELL_MS}ms linear")), "client.css draws the logo fill over DWELL_MS");
        assert!(crate::render::shell(&Route::Home, "/", None).contains("id=\"ytc-logo\" class=\"logo\" href=\"/\" aria-label=\"Redline home\" data-hov=\"logo\""));
    }
}
