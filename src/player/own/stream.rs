//! Media as it downloads (pure parts in `player/frag.rs`). Every googlevideo index and segment
//! request streams its body (`chunks`); each piece goes into the SourceBuffer at once, so a first
//! frame, a seek and a quality switch wait only for the bytes they need, not for the whole
//! segment. The index request's first segment bytes are appended while it still loads, and the
//! request for the rest of that segment goes out as soon as the index is parsed (not after the
//! index request ended). Several requests may feed one segment (a mirror twin, a retry from where
//! a broken one stopped): `frag::Asm` appends each byte once, in order.
//!
//! The instant quality switch (a chosen quality, or any cut while paused): YouTube's segments have
//! one keyframe each, so a new quality otherwise shows only at the next segment (live 3 Nov 2026:
//! 3.2–3.9 s after the choice). Instead the new quality's segment under the playhead is appended over
//! the old one (Chromium keeps playing the old frames meanwhile) and, once it covers the playhead,
//! the player seeks to where the playhead is: the decoder restarts from the new keyframe (measured:
//! 51–96 ms of frozen picture for 1080p60 AV1 one to 3.5 s into a segment). When the next segment
//! starts sooner than that would take, its new keyframe is simply appended before the playhead
//! reaches it (no seek, no freeze). Audio is never touched.
//!
//! "Data but no picture": when the start's video and audio cover the playhead for
//! `NOPIC_MS` and the element still has no frame, the video format is given up and the start
//! continues on the next one in a fresh MediaSource (before: retries at 4 s and 6 s).

use super::{pairs, sched, Inflight, Op, Own, Req, V};
use crate::player::frag;
use crate::bridge;

/// Media covering the playhead this long without a first frame: another format.
pub(super) const NOPIC_MS: f64 = 1500.0;

/// One segment being appended piece by piece.
pub(super) struct SegAsm {
    pub itag: u32,
    pub idx: usize,
    pub asm: frag::Asm,
    /// Bytes from here on are not asked for yet (the index request brings the start).
    pub asked: u64,
    /// The instant switch reads how much time the bytes so far cover.
    pub keep: bool,
    pub kept: Vec<u8>,
    mp4: Option<frag::Mp4Coverage>,
}

impl SegAsm {
    pub fn new(itag: u32, idx: usize, start: u64, end: u64, asked: u64) -> SegAsm {
        SegAsm { itag, idx, asm: frag::Asm::new(start, end), asked, keep: false, kept: Vec::new(), mp4: None }
    }

    /// Once the MP4 sample table is complete, release its buffered payload and advance coverage
    /// by the assembler's contiguous byte count. A switch keeps a few KiB of metadata, not a segment.
    fn mp4_covered(&mut self, bytes: &[u8]) -> Option<f64> {
        if self.mp4.is_none() {
            self.kept.extend_from_slice(bytes);
            self.mp4 = frag::Mp4Coverage::parse(&self.kept);
            if self.mp4.is_some() {
                self.kept = Vec::new();
            }
        }
        self.mp4.as_ref().map(|m| m.covered((self.asm.frontier() - self.asm.start) as usize))
    }
}

/// An instant quality switch: the new representation's segment `idx` ([t0, t1]).
pub(super) struct Flush {
    itag: u32,
    idx: usize,
    t0: f64,
    t1: f64,
    /// Seek once the new data covers the playhead (else: the segment's keyframe is appended
    /// before the playhead reaches `t0`).
    seek: bool,
    /// Whether the segment's first bytes were appended before the playhead reached `t0`.
    first_ok: Option<bool>,
    /// Media time the appended new data reaches.
    covered: f64,
    webm: bool,
    paused: bool,
    at: f64,
}

impl Own {
    /// A piece of a streamed googlevideo body. True when the request is ours.
    pub fn on_fetch_chunk(&mut self, id: u32, status: u32, bytes: &[u8]) -> bool {
        let Some(f) = self.fetches.get_mut(&id) else { return false };
        if self.failed || !(200..300).contains(&status) {
            return true;
        }
        let at = f.at;
        f.at += bytes.len() as u64;
        f.got += bytes.len() as f64;
        if f.ttfb.is_none() {
            f.ttfb = Some(bridge::now() - f.sent);
        }
        self.got_media = true;
        let req = match f.req {
            Req::Seg { kind, idx, itag } => Some((kind, idx, itag)),
            _ => None,
        };
        if let Some((kind, idx, itag)) = req {
            if !self.seek_pre_chunk(id, at, bytes) {
                self.feed(kind, itag, idx, at, bytes);
            }
            return true;
        }
        let Some(f) = self.fetches.get_mut(&id) else { return true };
        match f.req {
            Req::Head { itag, kind } if f.early => self.feed(kind, itag, 0, at, bytes),
            Req::Head { itag, kind } => {
                f.buf.extend_from_slice(bytes);
                let need = self.format(itag, kind).map_or(usize::MAX, |x| x.index_end as usize + 1);
                let Some(f) = self.fetches.get_mut(&id) else { return true };
                if f.buf.len() < need {
                    return true;
                }
                // The index is in: parse it now; the rest of the body goes on as media.
                f.early = true;
                let buf = std::mem::take(&mut f.buf);
                let upto = super::range_start(&f.base) + f.want as u64;
                if self.reps.contains_key(&itag) {
                    self.feed(kind, itag, 0, need as u64, &buf[need..]);
                } else if self.head_parse(itag, kind, &buf, upto, false) {
                    // The rest of the first segment is asked for now, in parallel.
                    self.pump(kind);
                }
            }
            _ => {}
        }
        true
    }

    /// An index request parsed from its first pieces has ended.
    pub(super) fn head_done(&mut self, itag: u32, kind: usize, status: u32, ms: f64, len: usize) {
        self.got_media = true;
        self.heads.retain(|&x| x != itag);
        if (200..300).contains(&status) {
            self.bw.add(len, ms);
        }
        // Whatever it did not bring is asked for by `pump` (from the frontier).
        self.pump(kind);
    }

    /// Smoothed time to the first byte of segment requests (the instant switch's estimate).
    pub(super) fn note_ttfb(&mut self, f: &Inflight) {
        if let (Req::Seg { .. }, Some(t)) = (&f.req, f.ttfb) {
            self.ttfb_ms = if self.ttfb_ms > 0.0 { self.ttfb_ms * 0.7 + t * 0.3 } else { t };
        }
    }

    /// Bytes of segment `idx` of `itag` at file offset `at`: append what is new, in order.
    pub(super) fn feed(&mut self, kind: usize, itag: u32, idx: usize, at: u64, bytes: &[u8]) {
        let fl = self.flush.as_ref().map(|f| (f.webm, f.t0, f.t1));
        let tr = &mut self.tracks[kind];
        let Some(a) = tr.asm.as_mut().filter(|a| a.itag == itag && a.idx == idx) else { return };
        let first = a.asm.frontier() == a.asm.start;
        let out = a.asm.add(at, bytes);
        if out.is_empty() {
            return;
        }
        let done = a.asm.done();
        // The instant switch's segment: how far its appended bytes reach.
        let kept = match (a.keep, fl) {
            (true, Some((webm, t0, t1))) => {
                if webm {
                    a.kept.extend_from_slice(&out);
                }
                Some(if done {
                    Some(t1)
                } else if webm {
                    frag::webm_covered(&a.kept)
                } else {
                    a.mp4_covered(&out).map(|x| t0 + x * (t1 - t0))
                })
            }
            _ => None,
        };
        if done {
            tr.asm = None;
            if tr.rep == itag {
                tr.next = idx + 1;
            }
        }
        self.queue_chunk(kind, itag, out, first, done);
        if let Some(c) = kept {
            self.flush_progress(itag, idx, first, c);
        }
    }

    fn queue_chunk(&mut self, kind: usize, itag: u32, bytes: Vec<u8>, first: bool, last: bool) {
        let Some(rep) = self.reps.get(&itag) else { return };
        let tr = &mut self.tracks[kind];
        if tr.appended != itag {
            if rep.f.mime != tr.mime && tr.sb != 0 {
                tr.ops.push_back(Op::ChangeType(rep.f.mime.clone()));
            }
            tr.mime = rep.f.mime.clone();
            tr.ops.push_back(Op::Append(rep.init.clone()));
            tr.appended = itag;
        }
        // A piece still waiting for its append grows instead of queueing another op.
        if let (false, Some(Op::Chunk { bytes: b, last: l, .. })) = (first, tr.ops.back_mut()) {
            b.extend_from_slice(&bytes);
            *l = last;
        } else {
            tr.ops.push_back(Op::Chunk { bytes, first, last });
        }
        self.flush(kind);
    }

    /// A new video segment request: keep its bytes when the instant switch waits for it.
    pub(super) fn flush_keep(&mut self, kind: usize) {
        let Some(fl) = self.flush.as_ref().filter(|_| kind == V) else { return };
        if let Some(a) = self.tracks[V].asm.as_mut().filter(|a| a.itag == fl.itag && a.idx == fl.idx) {
            a.keep = true;
        }
    }

    /// Switch to `itag` now (the playhead at `t`): its segment under the playhead, or the next
    /// one when that starts sooner than the seek could show the new quality.
    pub(super) fn flush_switch(&mut self, itag: u32, t: f64, paused: bool) {
        let Some(rep) = self.reps.get(&itag) else { return };
        let segs = &rep.segs;
        let cur = sched::seg_for(segs, t);
        let Some(s) = segs.get(cur) else { return };
        let ttfb = if self.ttfb_ms > 0.0 { self.ttfb_ms } else { 150.0 } / 1000.0;
        let bps = self.bw.bps.unwrap_or(20e6).max(1e6);
        let span = (s.t1 - s.t0).max(0.1);
        let size = (s.end - s.start + 1) as f64;
        // Seeking: the bytes from the keyframe to the playhead, then the decoder catching up.
        let seek_eta = ttfb + size * ((t - s.t0) / span + 0.05).clamp(0.05, 1.0) * 8.0 / bps + 0.1;
        // The next segment's keyframe (its first tenth, roughly) must be in before `t1`.
        let next_need = segs.get(cur + 1).map(|n| ttfb + (n.end - n.start + 1) as f64 * 0.1 * 8.0 / bps + 0.1);
        let natural = !paused && next_need.is_some_and(|need| s.t1 - t >= need && s.t1 - t <= seek_eta + 0.25);
        let idx = if natural { cur + 1 } else { cur };
        let s = &segs[idx];
        self.flush = Some(Flush { itag, idx, t0: s.t0, t1: s.t1, seek: !natural, first_ok: None, covered: s.t0, webm: rep.f.webm, paused, at: bridge::now() });
        self.note(format!("instant switch to {itag}: segment {idx} ({:.2}–{:.2} s) at {t:.2} s, {}", s.t0, s.t1, if natural { "at its keyframe" } else { "then a seek" }));
        let tr = &mut self.tracks[V];
        tr.rep = itag;
        tr.next = idx;
        tr.appended = 0;
        self.pump(V);
    }

    fn flush_progress(&mut self, itag: u32, idx: usize, first: bool, c: Option<f64>) {
        let t = self.time();
        let Some(fl) = self.flush.as_mut().filter(|f| f.itag == itag && f.idx == idx) else { return };
        if first {
            fl.first_ok = Some(t < fl.t0 - 0.03);
        }
        if let Some(c) = c {
            fl.covered = c.clamp(fl.t0, fl.t1);
        }
        self.flush_check();
    }

    /// After each video append (and on the tick): is it time for the seek that shows the switch?
    pub(super) fn flush_check(&mut self) {
        let Some(fl) = self.flush.as_mut() else { return };
        if self.tracks[V].rep != fl.itag || self.failed {
            self.flush = None;
            return;
        }
        let t = bridge::media_get(self.vid, "currentTime");
        if !fl.seek {
            match fl.first_ok {
                Some(true) => {
                    if t >= fl.t0 {
                        let ms = bridge::now() - fl.at;
                        self.flush = None;
                        self.note(format!("instant switch shown at the keyframe ({ms:.0} ms)"));
                    }
                    return;
                }
                // The keyframe came (or will come) too late: seek instead.
                Some(false) => fl.seek = true,
                None if t < fl.t0 - 0.04 => return,
                None => fl.seek = true,
            }
        }
        let tr = &self.tracks[V];
        if !tr.ops.is_empty() || tr.updating {
            return; // what is queued is not in the SourceBuffer yet
        }
        let fl = self.flush.as_ref().expect("checked above");
        if !fl.paused && t > fl.t1 - 0.08 && fl.covered < fl.t1 {
            // The playhead left the segment first: the next new segments start at their keyframes.
            self.flush = None;
            self.note("instant switch: the playhead passed the segment first".into());
            return;
        }
        let margin = if fl.paused { 0.0 } else { 0.12 };
        if fl.covered <= fl.t0 || fl.covered < t + margin {
            return;
        }
        let ms = bridge::now() - fl.at;
        self.flush = None;
        self.own_seek = Some((t, bridge::now()));
        self.sb_own_seek(t);
        bridge::media_set(self.vid, "currentTime", t);
        self.note(format!("instant switch: seek at {t:.2} s ({ms:.0} ms after the choice)"));
    }

    /// The tick before a first frame: media covers the playhead but no picture comes.
    pub(super) fn nopic_tick(&mut self, now: f64) {
        if self.first_frame.is_some() || self.nopic_done || self.live.is_some() || !self.sbs_made || !self.ms_open || bridge::hidden() {
            self.nopic_since = 0.0;
            return;
        }
        let t = self.time();
        let covered = [V, super::A].iter().all(|&k| self.tracks[k].sb != 0 && sched::range_at(&pairs(bridge::media_ranges(self.tracks[k].sb)), t).is_some());
        if !covered || bridge::media_get(self.vid, "readyState") >= 2.0 {
            self.nopic_since = 0.0;
            return;
        }
        if self.nopic_since == 0.0 {
            self.nopic_since = now;
            return;
        }
        if now - self.nopic_since < NOPIC_MS {
            return;
        }
        self.nopic_done = true;
        let old = self.tracks[V].rep;
        self.bad.push(old);
        self.build_ladder();
        let next = self.desired().map(|i| self.ladder[i].clone()).filter(|f| f.itag != old);
        self.note(format!("itag {old}: media covers {t:.1} s for {:.1} s but no picture: {}", (now - self.nopic_since) / 1000.0, next.as_ref().map_or("a fresh MediaSource".to_string(), |f| format!("{} {} in a fresh MediaSource", f.itag, f.label))));
        self.pt.nopic = true;
        self.rebuild_mse("data but no picture");
        if let Some(f) = next {
            self.start_video_on(f);
        }
    }

    /// After a fresh MediaSource (`rebuild_mse`): the video track starts again on format `f`.
    pub(super) fn start_video_on(&mut self, f: super::Format) {
        let ready = self.reps.contains_key(&f.itag);
        let tr = &mut self.tracks[V];
        tr.rep = f.itag;
        tr.mime = f.mime.clone();
        tr.pending = None;
        tr.ready = ready;
        if ready {
            tr.next = sched::start_index(&self.reps[&f.itag].segs, self.start);
        } else {
            self.fetch_head(V, &f, self.start < 4.0);
        }
        self.quality_label();
    }
}

/// A segment fetched ahead of a likely seek (the pointer resting on the seek bar, a TV scrub
/// target): taken by the track when the seek lands in it.
pub(super) struct SeekPre {
    kind: usize,
    itag: u32,
    idx: usize,
    /// The request while it runs.
    pub(super) id: Option<u32>,
    asm: frag::Asm,
    data: Vec<u8>,
}

/// The pointer rests this long on a seek-bar position before its segments are fetched.
pub(super) const SEEK_PRE_MS: u32 = 200;
/// Fetched-ahead segments kept per track.
const SEEK_PRE_KEEP: usize = 2;

fn seek_pre_wait(deadline: f64, now: f64) -> Option<u32> {
    (deadline > now).then(|| (deadline - now).ceil() as u32)
}

impl Own {
    /// The seek bar's pointer (or a TV scrub) is at `t` (None: it left): fetch that spot's
    /// segments once it rests there.
    pub(super) fn seek_pre_hint(&mut self, t: Option<f64>) {
        let Some(t) = t else {
            // Keep the one wake already queued: re-entering before it fires can reuse it.
            // With no current deadline that wake does nothing, including after a leave.
            self.seekpre_deadline = None;
            // Gone without a click: what is still loading is no longer needed.
            let ids: Vec<u32> = self.seek_pre.iter().filter_map(|p| p.id).collect();
            for id in ids {
                self.abort_fetch(id);
            }
            self.seek_pre.retain(|p| p.id.is_none());
            return;
        };
        self.seekpre_at = t;
        self.seekpre_deadline = Some(bridge::now() + f64::from(SEEK_PRE_MS));
        if !self.seekpre_pending {
            self.seekpre_pending = true;
            self.seekpre_gen = self.seekpre_gen.wrapping_add(1);
            self.timer(super::Tm::SeekPre(self.seekpre_gen), SEEK_PRE_MS);
        }
    }

    pub(super) fn seek_pre_due(&mut self, g: u32) {
        if g != self.seekpre_gen {
            return;
        }
        self.seekpre_pending = false;
        let Some(deadline) = self.seekpre_deadline else { return };
        if let Some(ms) = seek_pre_wait(deadline, bridge::now()) {
            self.seekpre_pending = true;
            self.timer(super::Tm::SeekPre(g), ms);
            return;
        }
        self.seekpre_deadline = None;
        if self.live.is_some() || self.first_frame.is_none() || self.failed || self.refreshing || !self.sbs_made {
            return;
        }
        let t = self.seekpre_at;
        for kind in [V, super::A] {
            let tr = &self.tracks[kind];
            if !tr.ready || tr.sb == 0 {
                continue;
            }
            let itag = tr.rep;
            let Some(rep) = self.reps.get(&itag) else { continue };
            let idx = sched::seg_for(&rep.segs, t);
            let Some(s) = rep.segs.get(idx) else { continue };
            // Buffered already (a seek there needs nothing), or fetched already.
            if sched::range_at(&pairs(bridge::media_ranges(tr.sb)), t).is_some() || self.seek_pre.iter().any(|p| p.kind == kind && p.itag == itag && p.idx == idx) {
                continue;
            }
            if tr.asm.as_ref().is_some_and(|a| a.itag == itag && a.idx == idx) {
                continue;
            }
            let (start, end) = (s.start, s.end);
            let url = format!("{}&range={start}-{end}", rep.f.url);
            // One position at a time: the previous spot's request is dropped.
            let old: Vec<u32> = self.seek_pre.iter().filter(|p| p.kind == kind).filter_map(|p| p.id).collect();
            for id in old {
                self.abort_fetch(id);
            }
            self.seek_pre.retain(|p| p.kind != kind || p.id.is_none());
            while self.seek_pre.iter().filter(|p| p.kind == kind).count() >= SEEK_PRE_KEEP {
                let Some(i) = self.seek_pre.iter().position(|p| p.kind == kind) else { break };
                self.seek_pre.remove(i);
            }
            let id = self.get(Req::Seg { kind, idx, itag }, &url, "{}");
            // Not the track's request: never judged throttled (no mirror, refresh or quality drop for it).
            if let Some(f) = self.fetches.get_mut(&id) {
                f.flagged = true;
            }
            self.seek_pre.push(SeekPre { kind, itag, idx, id: Some(id), asm: frag::Asm::new(start, end), data: Vec::new() });
            self.note(format!("seek bar rests at {t:.1} s: fetching {} segment {idx} ahead", if kind == V { "video" } else { "audio" }));
        }
    }

    /// A piece of a fetched-ahead segment. True when `id` is one.
    pub(super) fn seek_pre_chunk(&mut self, id: u32, at: u64, bytes: &[u8]) -> bool {
        let Some(p) = self.seek_pre.iter_mut().find(|p| p.id == Some(id)) else { return false };
        let out = p.asm.add(at, bytes);
        p.data.extend_from_slice(&out);
        true
    }

    /// A fetched-ahead request ended. True when `id` was one (complete: kept; else dropped).
    pub(super) fn seek_pre_end(&mut self, id: u32, status: u32) -> bool {
        let Some(i) = self.seek_pre.iter().position(|p| p.id == Some(id)) else { return false };
        self.seek_pre[i].id = None;
        if !(200..300).contains(&status) || !self.seek_pre[i].asm.done() {
            self.seek_pre.remove(i);
        }
        true
    }

    /// A seek landed: the track's next segment fetched ahead (or still loading) becomes its own.
    pub(super) fn seek_pre_take(&mut self, kind: usize) {
        let tr = &self.tracks[kind];
        if !tr.ready || tr.asm.is_some() {
            return;
        }
        let (itag, idx) = (tr.rep, tr.next);
        let Some(i) = self.seek_pre.iter().position(|p| p.kind == kind && p.itag == itag && p.idx == idx) else { return };
        let p = self.seek_pre.remove(i);
        let (start, end) = (p.asm.start, p.asm.end);
        self.note(format!("seek: {} segment {idx} {}", if kind == V { "video" } else { "audio" }, if p.id.is_some() { "fetched ahead, still loading: taken over" } else { "fetched ahead: appended at once" }));
        self.pt.seek_pre += 1;
        self.pt.seek_hit = true;
        self.tracks[kind].asm = Some(SegAsm::new(itag, idx, start, end, end + 1));
        if let Some(id) = p.id {
            self.tracks[kind].inflight = Some(id);
            if let Some(f) = self.fetches.get_mut(&id) {
                f.flagged = false;
            }
        }
        if !p.data.is_empty() {
            self.feed(kind, itag, idx, start, &p.data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seek_prefetch_waits_until_the_latest_pointer_rest_deadline() {
        // A move at 0 queues one wake at 200; moving at 180 shifts the rest deadline
        // to 380. The old wake waits the remaining time rather than prefetching early.
        assert_eq!(seek_pre_wait(380.0, 200.0), Some(180));
        assert_eq!(seek_pre_wait(560.0, 380.0), Some(180));
        assert_eq!(seek_pre_wait(560.0, 559.25), Some(1));
        assert_eq!(seek_pre_wait(560.0, 560.0), None);
        assert_eq!(seek_pre_wait(560.0, 600.0), None);
    }

    #[test]
    fn mp4_switch_releases_payload_and_tracks_contiguous_bytes() {
        let data = frag::tests::fragment(&[(100, 50), (0, 10), (200, 0), (50, 30)]);
        let mut a = SegAsm::new(299, 2, 1000, 999 + data.len() as u64, 1000);
        let mut planned = false;
        for (i, b) in data.iter().enumerate() {
            let out = a.asm.add(1000 + i as u64, &[*b]);
            assert_eq!(a.mp4_covered(&out), frag::mp4_covered(&data[..=i]), "byte {i}");
            if a.mp4.is_some() {
                assert_eq!(a.kept.capacity(), 0, "metadata is enough; no retained payload allocation");
                planned = true;
            }
        }
        assert!(planned && a.asm.done());
        assert_eq!(a.mp4.as_ref().unwrap().covered(data.len()), 1.0);
        assert!(SegAsm::new(299, 3, 2000, 2999, 2000).mp4.is_none(), "a new segment starts without the old plan");
    }
}
