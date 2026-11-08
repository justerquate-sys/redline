//! Speed telemetry hooks of the app (`crate::telemetry`): one-line calls from `app.rs`
//! name what happened; the events and their breakdowns are built here.

use super::{App, Purpose, wkey};
use crate::bridge;
use crate::route::Route;
use crate::telemetry as tel;
use serde_json::json;

/// The route's kind for telemetry (never its query, ids or path).
pub fn route_kind(r: &Route) -> &'static str {
    match r {
        Route::Home => "home",
        Route::Subs => "subs",
        Route::History => "history",
        Route::Playlists => "playlists",
        Route::Search { .. } => "search",
        Route::Watch { .. } => "watch",
        Route::Channel { .. } => "channel",
        Route::Playlist { list } if list == "WL" => "wl",
        Route::Playlist { list } if list == "LL" => "liked",
        Route::Playlist { .. } => "playlist",
        Route::Unknown { .. } => "other",
    }
}

fn purpose(p: &Purpose) -> &'static str {
    match p {
        Purpose::Page { .. } | Purpose::WatchLaterOrder { .. } | Purpose::WatchLaterMenu { .. } => "page",
        Purpose::Resolve { .. } => "resolve",
        Purpose::Cont { .. } => "cont",
        Purpose::ChanSort { .. } => "page",
        Purpose::Prefetch { spec: true, .. } => "spec",
        Purpose::Prefetch { .. } => "prefetch",
        Purpose::PagePrefetch { bg: true, .. } => "bg",
        Purpose::PagePrefetch { .. } => "prefetch",
        Purpose::ContPrefetch { .. } => "bg-cont",
        Purpose::Suggest { .. } => "suggest",
        Purpose::Config { .. } => "config",
        Purpose::VisionPrefetch { spec: true, .. } => "spec",
        Purpose::VisionPrefetch { .. } => "prefetch",
        Purpose::NetProbe => "net-probe",
        Purpose::Ready { .. } => "ready",
        Purpose::ReadyCont { .. } => "ready-cont",
        Purpose::Imp { .. } => "shown",
    }
}

impl App {
    /// Document start: the gate, then the first page's navigation event (from navigation start).
    pub(super) fn tel_boot(&mut self) {
        let page = format!("{:x}{:04x}", bridge::epoch() as u64, (bridge::now() * 1000.0) as u64 & 0xffff);
        tel::boot(&page);
        tel::nav_start(route_kind(&self.route), "boot");
        if tel::enabled() {
            // Where the boot's handler_ms went: the document's own request (navigation timing), the
            // content script's start, the Wasm instantiated, the app's boot (performance.now ms).
            let mut b = serde_json::from_str::<serde_json::Value>(&bridge::nav_timing()).unwrap_or_else(|_| json!({}));
            if let Some(o) = b.as_object_mut() {
                o.insert("app".into(), json!(tel::round1(bridge::now())));
            }
            tel::nav_extra("boot", b);
        }
    }

    /// Navigation begins (the page being left reports its layout shift).
    pub(super) fn tel_nav(&mut self, pop: bool) {
        if !tel::enabled() {
            return;
        }
        tel::nav_start(route_kind(&self.route), if pop { "pop" } else { "push" });
    }

    pub(super) fn tel_leave(&mut self) {
        if self.rendered {
            tel::page_left(route_kind(&self.route));
        }
    }

    /// An InnerTube (or config/suggest) request answered: `req.<endpoint>`; a request
    /// the current navigation waits for also joins its breakdown.
    pub(super) fn tel_req(&self, p: &super::Pending, status: u32, bytes: usize) {
        if !tel::enabled() {
            return;
        }
        let ms = bridge::now() - p.sent;
        let mine = match &p.purpose {
            Purpose::Page { nav } | Purpose::WatchLaterOrder { nav } | Purpose::Resolve { nav, .. } => *nav == self.nav,
            Purpose::Prefetch { v, list, .. } => !self.rendered && self.route_wkey().as_deref() == Some(wkey(v, list.as_deref()).as_str()),
            Purpose::PagePrefetch { url, .. } => self.await_page.as_deref() == Some(url.as_str()),
            _ => false,
        };
        let via = purpose(&p.purpose);
        let mut r = json!({"ep": p.ep, "ms": tel::round1(ms), "bytes": bytes, "status": status, "via": via});
        let mut e = json!({"bytes": bytes, "status": status, "via": via, "nav": mine, "route": route_kind(&self.route)});
        if let Some((fired, won)) = self.hedge_now {
            // Hedged read (`crate::hedge`): whether the second copy went out and answered first.
            for o in [&mut r, &mut e] {
                o["hedge_fired"] = fired.into();
                if fired {
                    o["hedge_won"] = won.into();
                    o["path"] = if won { "hedge" } else { "primary" }.into();
                }
            }
        }
        if mine {
            tel::nav_req(r);
        }
        tel::emit(&format!("req.{}", p.ep), ms, e);
    }

    /// A page response was parsed and rendered (`t0` before): a snapshot or cached page shown
    /// first is replaced by the fresh one (`nav.fresh`, from the navigation's input).
    pub(super) fn tel_body(&self, swr: bool, t0: f64) {
        if swr && tel::enabled() {
            let now = bridge::now();
            tel::emit("nav.fresh", now - tel::nav_t0().unwrap_or(self.nav_start), json!({"route": route_kind(&self.route), "parse_render_ms": tel::round1(now - t0)}));
        }
    }

    /// The batch goes out when due (`force`: the tab is hidden or the document left).
    pub(super) fn tel_flush(&mut self, force: bool) {
        tel::flush(force, &self.cfg.version, self.auth.signed_in(), route_kind(&self.route));
    }
}
