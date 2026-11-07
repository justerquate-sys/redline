//! When YouTube is slow (decisions in `player/throttle.rs`): the VISIONOS request is asked a
//! second time after `VISION_HEDGE_MS`; googlevideo index and segment requests stream their
//! progress, and one that is throttled (headers late, or a body far slower than the measured
//! bandwidth or than playback needs) goes through the owner's order: the same range on the next
//! mirror host (both run, the first full answer wins, and later requests use the winner's host),
//! then fresh URLs, then a lower quality held for a while (15 s, doubling), after which Auto
//! tries ≥ 1080p again. Nothing here leaves the player.

use super::{throttle, Inflight, Own, Req, V};
use crate::bridge;

/// The episode of throttling this video is in.
#[derive(Default)]
pub(super) struct Thr {
    /// 0 none, 1 mirrored, 2 fresh URLs, 3 quality lowered.
    level: u8,
    /// Last slow request seen (ms).
    last: f64,
    /// Quality cap while throttled (p; 0 = none) and until when.
    pub(super) cap: u32,
    pub(super) until: f64,
    drops: u32,
    last_drop: f64,
}

impl Own {
    /// No VISIONOS answer yet: ask once more (the first usable answer plays).
    pub(super) fn vision_hedge_due(&mut self) {
        if self.vision_ms.is_some() || self.vision_hedged || self.refreshing || self.recover.is_some() || self.failed {
            return;
        }
        let Some(cfg) = self.vision_cfg.clone() else { return };
        self.vision_hedged = true;
        let r = self.route.clone();
        let before: Vec<u32> = self.fetches.keys().copied().collect();
        self.send_vision(&cfg, &r);
        self.vision_hedge = self.fetches.keys().copied().find(|k| !before.contains(k));
        self.note(format!("VISIONOS: no answer in {:.0} ms, asking again", throttle::VISION_HEDGE_MS));
        self.tel_vision_hedge(false);
    }

    /// An unusable VISIONOS answer while its twin still runs: wait for the twin.
    pub(super) fn vision_twin_pending(&mut self, status: u32) -> bool {
        if self.vision_ms.is_some() || self.refreshing || self.recover.is_some() || !self.vision_hedged {
            return false;
        }
        let other = self.vision_app || self.fetches.values().any(|f| matches!(f.req, Req::Vision(_)));
        if other {
            self.note(format!("VISIONOS HTTP {status}: waiting for the second request"));
        }
        other
    }

    /// A usable VISIONOS answer: the other request of a hedged pair is no longer needed.
    pub(super) fn vision_answered(&mut self, id: u32) {
        if !self.vision_hedged {
            return;
        }
        let rest: Vec<u32> = self.fetches.iter().filter(|(_, f)| matches!(f.req, Req::Vision(_))).map(|(k, _)| *k).collect();
        for k in rest {
            self.fetches.remove(&k);
            bridge::abort(k);
        }
        if self.vision_hedge == Some(id) {
            self.tel_vision_hedge(true);
            self.note("VISIONOS: the second request answered first".into());
        }
    }

    /// Headers or body progress of a streamed googlevideo request.
    pub fn on_fetch_progress(&mut self, id: u32, status: u32, bytes: f64) -> bool {
        let now = bridge::now();
        let Some(f) = self.fetches.get_mut(&id) else { return false };
        if f.ttfb.is_none() {
            f.ttfb = Some(now - f.sent);
        }
        f.got = bytes;
        if (200..300).contains(&status) {
            self.hedge_check(now);
        }
        true
    }

    /// The answer of one request of a mirror pair. True when it is consumed here: a failed or
    /// short answer while the twin still runs is dropped (the twin may still deliver); a full
    /// answer aborts the twin, and a winning hedge moves later requests to its mirror.
    pub(super) fn twin_answer(&mut self, id: u32, f: &Inflight, status: u32, len: usize) -> bool {
        let Some(twin) = f.twin else { return false };
        let full = (200..300).contains(&status) && (f.want <= 0.0 || len as f64 >= f.want);
        if !full {
            if let Some(t) = self.fetches.get_mut(&twin) {
                t.twin = None;
                self.note(format!("{}: HTTP {status}, {len} B; its mirror twin continues", f.what));
                // The twin is now the segment's request (its answer must not be taken as stale).
                if let Req::Seg { kind, .. } = f.req {
                    if self.tracks[kind].inflight == Some(id) {
                        self.tracks[kind].inflight = Some(twin);
                    }
                }
                return true;
            }
            return false;
        }
        if self.fetches.remove(&twin).is_some() {
            bridge::abort(twin);
        }
        if let Req::Seg { kind, .. } = f.req {
            if self.tracks[kind].inflight == Some(twin) {
                self.tracks[kind].inflight = Some(id);
            }
        }
        if f.hedge {
            if f.mirror != self.mirror {
                self.mirror = f.mirror;
            }
            self.note(format!("mirror {} answered first ({:.0} ms): later requests go there", f.mirror, bridge::now() - f.sent));
            self.tel_hedge_won();
        }
        false
    }

    /// Every streamed googlevideo request in flight: is one throttled?
    pub(super) fn hedge_check(&mut self, now: f64) {
        // (`ytc-test-nothrottle`, localStorage, tests only: requests held on purpose are no throttling.)
        if self.failed || self.live.is_some() || self.no_throttle {
            return;
        }
        let started = self.first_frame.is_some();
        let t = if started { self.time() } else { 0.0 };
        let mut slow: Vec<(u32, &'static str, Option<f64>)> = Vec::new();
        for (id, f) in &self.fetches {
            let (kind, itag) = match f.req {
                Req::Head { kind, itag } | Req::Seg { kind, itag, .. } => (kind, itag),
                _ => continue,
            };
            if f.flagged || f.base.is_empty() {
                continue;
            }
            // A cached player response that hangs gets fresh URLs at 0.8 s instead (`tick`).
            if !started && self.from_cache && f.ttfb.is_none() {
                continue;
            }
            let sb = self.tracks[kind].sb;
            let ahead = if sb == 0 || !started { 0.0 } else { super::sched::ahead(&super::pairs(bridge::media_ranges(sb)), t) };
            let need = self.format(itag, kind).map(|x| x.bitrate as f64).unwrap_or(0.0);
            // A mirror twin opens a connection to a new host first (DNS + TLS, up to ~0.6 s cold):
            // it gets twice the time before it counts as slow.
            let elapsed = if f.hedge { (now - f.sent) / 2.0 } else { now - f.sent };
            let p = throttle::Probe { elapsed, ttfb: f.ttfb.map(|t| if f.hedge { t / 2.0 } else { t }), got: f.got, want: f.want, started, ahead, need_bps: need, bw: self.bw.bps };
            if let Some(why) = throttle::judge(&p) {
                slow.push((*id, why, throttle::rate(&p)));
            }
        }
        for (id, why, rate) in slow {
            self.throttled(id, why, rate, now);
            if self.failed {
                return;
            }
        }
    }

    /// One request judged throttled: the next step of the owner's order.
    fn throttled(&mut self, id: u32, why: &'static str, rate: Option<f64>, now: f64) {
        if now - self.thr.last > throttle::CALM_MS && self.thr.level < 3 {
            self.thr.level = 0;
        }
        self.thr.last = now;
        let Some(f) = self.fetches.get_mut(&id) else { return };
        f.flagged = true;
        let (req_kind, itag) = match f.req {
            Req::Head { kind, itag } | Req::Seg { kind, itag, .. } => (kind, itag),
            _ => return,
        };
        let paired = f.twin.is_some() || f.hedge;
        let (fm, base, elapsed, what) = (f.mirror, f.base.clone(), now - f.sent, f.what.clone());
        let alt = self.mirror_alt(fm, &base);
        // A recent drop that did not help: lower again rather than start over.
        let recent_drop = self.thr.drops > 0 && now - self.thr.last_drop < 120_000.0;
        let can_refresh = self.vision_cfg.is_some() && self.vision_ms.is_some() && !self.refreshing && self.refreshes < 3;
        let step = if recent_drop { throttle::Step::Drop } else { throttle::step(self.thr.level, paired, alt.is_some(), can_refresh) };
        self.tel_throttle(req_kind, itag, why, step, rate, elapsed);
        match step {
            throttle::Step::Mirror => {
                let Some(n) = alt else { return };
                self.thr.level = self.thr.level.max(1);
                self.note(format!("throttled ({why}, {elapsed:.0} ms): {what} → also on mirror {n}"));
                self.hedge_on(id, n);
            }
            throttle::Step::Refresh => {
                self.thr.level = 2;
                self.fresh_urls(&format!("throttled ({why}, {what})"));
            }
            // Only a measured rate says what a lower quality would gain; a host that sends
            // nothing at all is the stuck schedule's (fresh URLs at 6 s and 15 s).
            throttle::Step::Drop if rate.is_some() => {
                self.thr.level = 3;
                self.throttle_drop(rate, now, why);
            }
            throttle::Step::Drop => {}
        }
    }

    /// Another mirror than `cur` for this URL, if the URL lists one.
    fn mirror_alt(&self, cur: usize, base: &str) -> Option<usize> {
        let host = |n: usize| if n == 0 { Some(super::host_of(base)) } else { super::mirror_url(base, n).map(|u| super::host_of(&u)) };
        let here = host(cur)?;
        (1..4).map(|d| cur + d).find(|&n| host(n).is_some_and(|h| h != here))
    }

    /// The same range of request `id` on mirror `n`, as its twin.
    fn hedge_on(&mut self, id: u32, n: usize) {
        let Some(f) = self.fetches.get(&id) else { return };
        let (req, base) = (match f.req {
            Req::Head { kind, itag } => Req::Head { kind, itag },
            Req::Seg { kind, idx, itag } => Req::Seg { kind, idx, itag },
            _ => return,
        }, f.base.clone());
        let h = self.get_on(req, &base, "{}", n);
        if let Some(t) = self.fetches.get_mut(&h) {
            t.hedge = true;
            t.twin = Some(id);
        }
        if let Some(f) = self.fetches.get_mut(&id) {
            f.twin = Some(h);
        }
        self.tel_hedge_fired();
    }

    /// Mirror and fresh URLs did not help: the highest rung the throughput seen sustains, for
    /// a while (`throttle::hold_ms`); then Auto tries ≥ 1080p again.
    fn throttle_drop(&mut self, rate: Option<f64>, now: f64, why: &str) {
        if now - self.thr.last_drop < 3000.0 {
            return;
        }
        let cur = self.format(self.tracks[V].rep, V).map(|f| f.p).unwrap_or(0);
        let audio = self.audio.as_ref().map(|a| a.bitrate as f64).unwrap_or(160_000.0);
        let bps = rate.or(self.bw.bps).unwrap_or(1_000_000.0) - audio;
        let rungs: Vec<(u32, u64)> = self.ladder.iter().map(|f| (f.p, f.bitrate)).collect();
        let Some(p) = throttle::drop_to(&rungs, bps.max(0.0), cur) else { return };
        self.thr.drops += 1;
        self.thr.last_drop = now;
        self.thr.cap = p;
        self.thr.until = now + throttle::hold_ms(self.thr.drops);
        self.note(format!("throttled ({why}, ~{:.0} kb/s): {p}p for {:.0} s, then ≥ 1080p again", bps / 1000.0, throttle::hold_ms(self.thr.drops) / 1000.0));
        let Some(i) = self.desired() else { return };
        let f = self.ladder[i].clone();
        if f.itag == self.tracks[V].rep {
            return;
        }
        if !self.tracks[V].ready {
            // Its index never came: start from the lower rung instead (nothing was appended).
            let old = self.tracks[V].rep;
            let stale: Vec<u32> = self.fetches.iter().filter(|(_, x)| matches!(x.req, Req::Head { kind: V, itag } if itag == old)).map(|(k, _)| *k).collect();
            for k in stale {
                self.abort_fetch(k);
            }
            self.heads.retain(|&x| x != old);
            let tr = &mut self.tracks[V];
            tr.rep = f.itag;
            tr.pending = None;
            if tr.sb == 0 {
                tr.mime = f.mime.clone();
            }
            self.fetch_head(V, &f, self.start < 4.0);
            self.quality_label();
            return;
        }
        // Little buffered: the slow request is abandoned and the lower rung starts now.
        let t = self.time();
        let ahead = super::sched::ahead(&super::pairs(bridge::media_ranges(self.tracks[V].sb)), t);
        self.switch_to(f.itag, ahead < 4.0 || self.first_frame.is_none());
    }

    /// Drop a request (and its mirror twin).
    pub(super) fn abort_fetch(&mut self, id: u32) {
        if let Some(f) = self.fetches.remove(&id) {
            bridge::abort(id);
            if let Some(t) = f.twin {
                if self.fetches.remove(&t).is_some() {
                    bridge::abort(t);
                }
            }
        }
    }

    /// The throttle cap, while it holds (`limit`).
    pub(super) fn thr_cap(&self) -> Option<u32> {
        (self.thr.cap > 0 && bridge::now() < self.thr.until).then_some(self.thr.cap)
    }

    /// Every tick: a lapsed cap is lifted (the periodic ABR then goes back to ≥ 1080p), and
    /// requests in flight are judged.
    pub(super) fn thr_tick(&mut self, now: f64) {
        if self.thr.cap > 0 && now >= self.thr.until {
            self.thr.cap = 0;
            self.thr.level = 0;
            self.note("throttle cap lapsed: trying ≥ 1080p again".into());
            self.tel_throttle_back();
            self.abr_at = 0.0;
        }
        self.hedge_check(now);
    }
}
