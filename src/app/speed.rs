//! Speed without waiting: speculative prefetch of the video cards in view
//! (idle time, `next` then VISIONOS, two at a time, cancelled on
//! navigation), background refresh of Subscriptions while the tab is in use
//! (Home has its ready response: `homeready.rs`), and the `perf` debug-center report.

use super::{App, BG_ACTIVE_MS, LIST, Pending, Purpose, SPEC_CARDS, SPEC_CONCURRENT, SPEC_IDLE_MS, Timer, wkey};
use crate::bridge;
use crate::perf;
use crate::route::{self, Route};

const PERF_AT: &str = "ytc:perfat";

impl App {
    pub(super) fn spec_schedule(&mut self) {
        if self.spec_scheduled {
            return;
        }
        self.spec_scheduled = true;
        let id = self.timer(Timer::Spec { nav: self.nav });
        bridge::idle(id, SPEC_IDLE_MS);
    }

    /// The video cards in view (≤ 8) join the speculative queue.
    pub(super) fn spec_pass(&mut self) {
        // (`ytc-test-nospec`, localStorage, tests only.)
        if !bridge::visible() || bridge::local_flag("ytc-test-nospec") {
            return;
        }
        let hrefs = bridge::visible_hrefs(LIST, ".c", "a.t", SPEC_CARDS);
        for h in hrefs.lines() {
            if let Some(Route::Watch { v, list, .. }) = route::parse(h) {
                if self.spec_queue.len() < SPEC_CARDS as usize && !self.spec_queue.iter().any(|(x, l)| *x == v && *l == list) {
                    self.spec_queue.push((v, list));
                }
            }
        }
        self.spec_pump();
    }

    fn spec_inflight(&self) -> usize {
        self.pending.values().filter(|p| matches!(p.purpose, Purpose::Prefetch { spec: true, .. })).count()
    }

    pub(super) fn spec_pump(&mut self) {
        let mut n = self.spec_inflight();
        while n < SPEC_CONCURRENT && !self.spec_queue.is_empty() {
            let (v, list) = self.spec_queue.remove(0);
            if self.prefetch_as(&v, list.as_deref(), true) {
                n += 1;
            }
        }
    }

    /// Navigation: speculative requests stop, except the ones for the page being opened.
    pub(super) fn spec_cancel(&mut self) {
        self.spec_queue.clear();
        self.spec_scheduled = false;
        let keep = self.route_wkey();
        let keep_v = if let Route::Watch { v, .. } = &self.route { Some(v.clone()) } else { None };
        let ids: Vec<u32> = self
            .pending
            .iter()
            .filter(|(_, p)| match &p.purpose {
                Purpose::Prefetch { v, list, spec: true } => keep.as_deref() != Some(wkey(v, list.as_deref()).as_str()),
                Purpose::VisionPrefetch { v, spec: true, .. } => keep_v.as_deref() != Some(v.as_str()),
                _ => false,
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.abort_req(id);
            match self.pending.remove(&id) {
                Some(Pending { purpose: Purpose::Prefetch { v, list, .. }, .. }) => {
                    self.prefetching.remove(&wkey(&v, list.as_deref()));
                }
                Some(Pending { purpose: Purpose::VisionPrefetch { v, .. }, .. }) => {
                    self.vision_prefetching.remove(&v);
                }
                _ => {}
            }
        }
    }

    /// Every 30 s: Home and Subscriptions older than the background age are
    /// fetched again (first page into the page cache, its first continuation
    /// into `cont_cache`), only while the tab is visible and in use.
    pub(super) fn bg_check(&mut self) {
        let now = bridge::now();
        if self.perf.due(now) {
            self.perf_send();
        }
        self.tel_flush(false);
        // `ytc-test-nobg` (localStorage, tests only): no background refresh.
        if !self.first_ready || !bridge::visible() || now - self.last_input > BG_ACTIVE_MS || bridge::local_flag("ytc-test-nobg") {
            return;
        }
        // Home is not refreshed here: the ready Home response (`app/homeready.rs`) is its next page.
        let mut targets = Vec::new();
        if self.auth.signed_in() {
            targets.push(Route::Subs);
        }
        let epoch = bridge::epoch();
        for r in targets {
            let url = r.url();
            if self.route.url() == url || self.page_prefetching.contains_key(&url) {
                continue;
            }
            if self.page_cache.iter().any(|(u, _, at)| *u == url && epoch - at < self.bg_age) {
                continue;
            }
            let at = self.snap_written(&url);
            if at.is_some_and(|at| epoch - at < self.bg_age) {
                continue;
            }
            let Some(req) = self.page_req(&r) else { continue };
            let id = self.send(req, Purpose::PagePrefetch { url: url.clone(), bg: true }, true);
            self.page_prefetching.insert(url, id);
        }
    }

    /// The `perf` report to the debug center (numbers only).
    pub(super) fn perf_send(&mut self) {
        let now = bridge::now();
        let report = serde_json::json!({
            "kind": "perf",
            "reason": "timings from this tab",
            "at": bridge::epoch(),
            "client": env!("CARGO_PKG_VERSION"),
            "client_version": self.cfg.version,
            "signed_in_cookie": self.auth.signed_in(),
            "perf": self.perf.take(now),
        });
        bridge::incident(&report.to_string());
    }

    /// Tab hidden or document left: report what was measured (not more than once a minute,
    /// so reports never crowd real incidents out of the debug center's last 100).
    pub(super) fn perf_on_hide(&mut self) {
        // The spacing spans documents of this tab (sessionStorage `ytc:perfat`, epoch ms).
        let last = bridge::session_get(PERF_AT).parse::<f64>().unwrap_or(0.0);
        let now = bridge::epoch();
        if self.perf.fresh > 0 && now - last >= perf::HIDE_MIN_MS {
            bridge::session_set(PERF_AT, &format!("{now:.0}"));
            self.perf_send();
        }
    }
}
