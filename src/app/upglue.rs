//! Watch page "what's next": the playlist panel, the Up next header with the
//! Autoplay switch (`ap` in `ytcConfigV1`), the end-of-video decision and the
//! near-end prefetch. The player only reports `Out::Ended`; `next_video`
//! decides (the playlist's next item, else YouTube's autoplay target).

use super::{App, Mode, Timer};
use crate::bridge;
use crate::features::upnext::{self, Next};
use crate::player::{fail, ui};
use crate::parse::Watch;
use crate::render;
use crate::route::Route;
use std::collections::HashSet;
use std::rc::Rc;

/// Videos remembered as seen in this tab (`up_skips`).
const UP_SEEN_MAX: usize = 50;
/// An automatic advance moves past this many videos the player refuses, then shows the panel.
const UP_SKIPS: u32 = 3;

impl App {
    /// The current watch page's parsed `next` response, if it has arrived.
    pub(super) fn cur_watch(&self) -> Option<Rc<Watch>> {
        let key = self.route_wkey()?;
        self.watch_cache.iter().find(|(k, _)| *k == key).map(|e| e.1.clone())
    }

    /// A related card that may stand in for a locked target: visible with the filters, not seen in this tab.
    fn up_skips(&self, w: &Watch) -> impl Fn(&str) -> bool {
        let open: HashSet<String> = self.fx_visible(&w.related.items).iter().map(|i| i.key().to_string()).filter(|k| !self.up_seen.contains(k)).collect();
        move |id: &str| !open.contains(id)
    }

    /// What plays after the current video (`None` = stay on this page).
    pub(super) fn next_video(&self) -> Option<Next> {
        let Route::Watch { v, .. } = &self.route else { return None };
        let w = self.cur_watch()?;
        upnext::next_with(&w.up, v, self.cfg.autoplay, &self.up_skips(&w))
    }

    /// What a pressed Next takes (the panel's Next video; never a locked video).
    pub(super) fn up_pressed(&self) -> Option<Next> {
        let Route::Watch { v, .. } = &self.route else { return None };
        let w = self.cur_watch()?;
        upnext::next_pressed(&w.up, v, &self.up_skips(&w))
    }

    /// `next` arrived: the playlist panel (current row scrolled into view) and the header.
    /// Moving on inside the same playlist (a row click, the next item at the end): the current-row mark
    /// springs from the previous video's row to the new one (`mark_slide`); otherwise it is placed at once.
    pub(super) fn up_render(&mut self, w: &Watch, v: &str) {
        if !self.up_seen.iter().any(|x| x == v) {
            if self.up_seen.len() >= UP_SEEN_MAX {
                self.up_seen.remove(0);
            }
            self.up_seen.push(v.to_string());
        }
        bridge::html(render::PLAYLIST, &upnext::panel(w.up.playlist.as_ref(), v));
        if let Some(pl) = w.up.playlist.as_ref() {
            bridge::scroll_in(upnext::LIST, ".cur");
            let prev = self.pl_last.as_ref().filter(|(id, pv)| *id == pl.id && pv != v && !self.mo.calm).and_then(|(_, pv)| upnext::row_of(pl, pv));
            if let Some(cur) = upnext::row_of(pl, v) {
                if let Some(p) = prev {
                    bridge::mark_slide(upnext::LIST, &upnext::row_id(p), "", "plk", false);
                }
                bridge::mark_slide(upnext::LIST, &upnext::row_id(cur), "", "plk", prev.is_some());
            }
            self.pl_last = Some((pl.id.clone(), v.to_string()));
        }
        self.up_head();
        self.panel_sync();
    }

    // -------------------------------------------- a verdict's panel, what it knows from `next`

    /// The failure panel in the player box.
    pub(super) fn show_panel(&mut self, v: &str, reason: &str) {
        self.panel_shown = Some((reason.to_string(), false));
        if self.up_skip_locked(reason) {
            return;
        }
        let next = self.up_pressed().is_some();
        bridge::html(ui::BOX, &fail::panel(v, reason, self.auto.made(), next));
        bridge::attr(ui::BOX, "class", "player failed");
        self.panel_shown = Some((reason.to_string(), next));
        // TV: the panel's first action takes the focus (the controls it replaced had it).
        self.tv_failed();
        self.panel_focus();
    }

    /// A verdict's first action takes the focus unless the viewer is somewhere else (typing a search).
    fn panel_focus(&self) {
        let free = !bridge::any(":focus") || bridge::any("body:focus") || bridge::any("#ytc-player :focus");
        if free && self.panel_shown.as_ref().is_some_and(|p| !fail::offers_retry(&p.0)) {
            bridge::focus_sel(ui::BOX, ".pfail .pfbt");
        }
    }

    /// `next` answered after the player's verdict: Next video arrives, or an automatic advance goes on.
    fn panel_sync(&mut self) {
        let Some((reason, had)) = self.panel_shown.clone() else { return };
        if fail::offers_retry(&reason) || !bridge::any("#ytc-player .pfail") {
            return;
        }
        if self.up_skip_locked(&reason) {
            return;
        }
        let next = self.up_pressed().is_some();
        if next != had {
            bridge::html(fail::BUTTONS, &fail::verdict_buttons(&reason, next));
            self.panel_shown = Some((reason, next));
            self.tv_failed();
            self.panel_focus();
        }
    }

    /// The panel's Next video.
    pub(super) fn panel_go_next(&mut self) -> bool {
        let Some(n) = self.up_pressed() else { return false };
        self.up_cancel();
        self.up_open(n);
        true
    }

    /// An automatic advance landed on a video the player says is for a channel's members, its card without
    /// the signal: on to the next one, a few times at most. (Only what the app itself opened: a viewer's click
    /// gets the panel.) False: the panel, or it waits for `next`.
    fn up_skip_locked(&mut self, reason: &str) -> bool {
        if self.up_chain.0 != self.nav || self.up_chain.1 > UP_SKIPS || !fail::members_only(reason) {
            return false;
        }
        let Some(n) = self.next_video() else { return false };
        self.up_advance(n);
        true
    }

    /// The app itself moves on: what `up_skip_locked` counts.
    fn up_advance(&mut self, n: Next) {
        let hops = if self.up_chain.0 == self.nav { self.up_chain.1 } else { 0 };
        self.up_open(n);
        self.up_chain = (self.nav, hops + 1);
    }

    /// A prefetch learned that the video it asked about is for a channel's members: what plays next may have
    /// changed, and the replacement gets its prefetch too.
    pub(super) fn up_prefetch_again(&mut self) {
        self.up_prefetched = 0;
    }

    fn up_title(&self, v: &str) -> String {
        if let Some(k) = self.known.get(v) {
            return k.title.clone();
        }
        let w = self.cur_watch();
        w.as_ref().and_then(|w| w.up.playlist.as_ref()).and_then(|p| p.items.iter().find(|i| i.v == v)).map(|i| i.title.clone()).unwrap_or_default()
    }

    pub(super) fn up_head(&mut self) {
        if !matches!(self.route, Route::Watch { .. }) {
            return;
        }
        let pending = self.countdown.as_ref().filter(|c| c.0 == self.nav).map(|c| (self.up_title(&c.1.v), c.2));
        bridge::html(render::UPNEXT, &upnext::head(self.cfg.autoplay, pending.as_ref().map(|(t, s)| (t.as_str(), *s))));
        self.tv_countdown(pending.is_some());
    }

    /// Near the end: fetch the next video's `next` + VISIONOS once (no other requests).
    pub(super) fn up_prefetch(&mut self) {
        if self.up_prefetched == self.nav {
            return;
        }
        if let Some(n) = self.next_video() {
            self.up_prefetched = self.nav;
            self.prefetch(&n.v, n.list.as_deref());
        }
    }

    /// The player reported the end: the next playlist item at once, or
    /// YouTube's autoplay target after its countdown (Play now / Cancel).
    pub(super) fn up_ended(&mut self) {
        let Some(n) = self.next_video() else { return };
        let secs = self.cur_watch().map(|w| w.up.countdown).unwrap_or(5);
        if n.now || secs == 0 {
            self.up_advance(n);
            return;
        }
        let nav = self.nav;
        self.countdown = Some((nav, n, secs));
        let id = self.timer(Timer::Autoplay { nav });
        bridge::timer(id, secs * 1000);
        self.up_head();
    }

    /// Countdown over or Play now.
    pub(super) fn up_go(&mut self) {
        if let Some((nav, n, _)) = self.countdown.take() {
            if nav == self.nav {
                self.up_open(n);
            }
        }
    }

    /// The countdown ran out.
    pub(super) fn up_timer(&mut self) {
        if let Some((nav, n, _)) = self.countdown.take() {
            if nav == self.nav {
                self.up_advance(n);
            }
        }
    }

    /// Cancel, or the user took over the player.
    pub(super) fn up_cancel(&mut self) {
        if self.countdown.take().is_some() {
            self.up_head();
        }
    }

    pub(super) fn up_open(&mut self, n: Next) {
        let route = Route::Watch { v: n.v, t: 0, list: n.list };
        let url = route.url();
        self.navigate(route, url, Mode::Push);
    }
}
