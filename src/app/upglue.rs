//! Watch page "what's next": the playlist panel, the Up next header with the
//! Autoplay switch (`ap` in `ytcConfigV1`), the end-of-video decision and the
//! near-end prefetch. The player only reports `Out::Ended`; `next_video`
//! decides (the playlist's next item, else YouTube's autoplay target).

use super::{App, Mode, Timer};
use crate::bridge;
use crate::features::upnext::{self, Next};
use crate::parse::Watch;
use crate::render;
use crate::route::Route;
use std::rc::Rc;

impl App {
    /// The current watch page's parsed `next` response, if it has arrived.
    pub(super) fn cur_watch(&self) -> Option<Rc<Watch>> {
        let key = self.route_wkey()?;
        self.watch_cache.iter().find(|(k, _)| *k == key).map(|e| e.1.clone())
    }

    /// What plays after the current video (`None` = stay on this page).
    pub(super) fn next_video(&self) -> Option<Next> {
        let Route::Watch { v, .. } = &self.route else { return None };
        upnext::next(&self.cur_watch()?.up, v, self.cfg.autoplay)
    }

    /// `next` arrived: the playlist panel (current row scrolled into view) and the header.
    /// Moving on inside the same playlist (a row click, the next item at the end): the current-row mark
    /// springs from the previous video's row to the new one (`mark_slide`); otherwise it is placed at once.
    pub(super) fn up_render(&mut self, w: &Watch, v: &str) {
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
            self.up_open(n);
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
