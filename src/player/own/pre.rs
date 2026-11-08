//! Media prefetch on hover intent: once the pointer has rested on a video link and its VISIONOS
//! response is in (`App` timer `MediaPre`), the index + first segment of exactly the formats a
//! player would start with (the same ladder, decodingInfo, limit and ABR rules) are fetched, so
//! the click needs only the append. One video at a time; what is still loading is cancelled when
//! the pointer moves on to something else; finished ones are kept a few minutes (memory only).
//! The player takes them in `fetch_head` (a finished body is used at once; one in flight becomes
//! the player's own request).

use super::{caps_known, formats, next_id, Inflight, Own, Req, V};
use crate::bridge;
use crate::config::Prefs;
use crate::json;
use formats::{Codec, Format};
use std::cell::RefCell;
use std::collections::HashMap;

struct Pre {
    v: String,
    itag: u32,
    kind: usize,
    /// The format (the warm first frame, `warm.rs`, reads its init and index ranges).
    f: Format,
    end: u64,
    id: u32,
    sent: f64,
    url: String,
    /// Response headers (ms after `sent`) and body bytes so far.
    ttfb: Option<f64>,
    got: f64,
    /// (status, bytes, ms) once answered.
    body: Option<(u32, Vec<u8>, f64)>,
}

thread_local! {
    static PRE: RefCell<Vec<Pre>> = const { RefCell::new(Vec::new()) };
}

/// Finished prefetches kept (two per video) and for how long.
const KEEP: usize = 6;
const MAX_AGE_MS: f64 = 300_000.0;

/// End byte of the first request for `f` (index, plus the first segment when starting near 0).
/// The first segment's size is unknown until the index is parsed (live 2 Nov 2026: 108 KB to
/// 4.4 MB at 1080p), so the guess is bounded: what of the segment came along is kept and only the
/// rest is asked for (`stream.rs`); a first frame never waits for bytes past segment 0.
pub(super) fn head_end(f: &Format, kind: usize, extra: bool) -> u64 {
    head_end_max(f, kind, extra, HEAD_EXTRA)
}

fn head_end_max(f: &Format, kind: usize, extra: bool, max: u64) -> u64 {
    let mut end = f.index_end;
    if extra {
        let secs = if kind == V { 6.0 } else { 11.0 };
        end = f.index_end + ((f.bitrate as f64 / 8.0 * secs) as u64).min(max);
    }
    end.min(f.len.saturating_sub(1))
}

/// Bytes of the first segment asked for together with the index. Live 3 Nov 2026 (bench, cold
/// connections): first segments of 33–108 KB were common, and 512 KB here cost ~150 ms of slow
/// start before the first frame; 128 KB covers most of them, a longer one fetches only its rest.
const HEAD_EXTRA: u64 = 128 * 1024;
/// While the pointer rests there is time: a hover prefetch takes up to 1 MB of the first segment
/// (most first segments whole; the player keeps what it needs and asks only for any rest).
const PRE_EXTRA: u64 = 1024 * 1024;

/// The video and audio formats a player would start this response with.
fn plan(v: &str, body: &[u8], prefs: &Prefs, bw: Option<f64>) -> Option<(Format, Format)> {
    let json = json::parse(body)?;
    let fmts = formats::parse(&json);
    if json.pointer("/videoDetails/isLive").and_then(json::Value::as_bool) == Some(true) || !fmts.iter().any(|f| f.video && f.index_end > 0) {
        return None;
    }
    let mut support: HashMap<String, bool> = HashMap::new();
    for f in &fmts {
        support.entry(f.mime.clone()).or_insert_with(|| bridge::mse_ok(&f.mime));
    }
    let ok = |m: &str| support.get(m).copied().unwrap_or(false);
    let codecs = formats::order(Codec::from_key(&prefs.codec));
    let caps: HashMap<u32, (bool, bool)> = fmts.iter().filter(|f| f.video).filter_map(|f| caps_known(f).map(|x| (f.itag, x))).collect();
    // Not every format's decodingInfo answer is in yet: the player might pick another codec.
    if fmts.iter().any(|f| f.video && !caps.contains_key(&f.itag)) {
        return None;
    }
    let known = |i: u32| caps.get(&i).copied();
    let screen = formats::caps_for_screen(&fmts, bridge::matches("(dynamic-range: high)"), &known);
    let ladder = formats::ladder_with(&fmts, &codecs, &ok, &screen);
    // (TV: the track it will really start on, not the default one it would then throw away.)
    let tv = crate::tv::on().then(|| formats::tv_audio(&fmts, &formats::chosen_audio(v).unwrap_or_default(), &prefs.audio_lang, &ok)).flatten();
    let audio = tv.or_else(|| formats::audio(&fmts, &ok))?.clone();
    let i = if prefs.quality > 0 {
        formats::pick(&ladder, prefs.quality)?
    } else {
        let d = bridge::screen_px();
        let screen = if d.len() == 3 { d[1].min(d[0] * 9.0 / 16.0) * d[2] } else { 1080.0 };
        formats::abr(&ladder, formats::display_cap(screen, 0.0), bw)?
    };
    Some((ladder[i].clone(), audio))
}

/// `v`'s first media is prefetched (or on its way).
pub fn prefetched(v: &str) -> bool {
    PRE.with(|p| p.borrow().iter().any(|e| e.v == v))
}

/// Prefetch `v`'s first media (VISIONOS `body`), starting at `start` seconds. False when nothing
/// was asked (live, already prefetched, decodingInfo not known yet).
pub fn prefetch_media(v: &str, body: &[u8], prefs: &Prefs, bw: Option<f64>, start: f64) -> bool {
    if PRE.with(|p| p.borrow().iter().any(|e| e.v == v)) {
        return false;
    }
    let Some((video, audio)) = plan(v, body, prefs, bw) else { return false };
    pre_cancel(&[v]);
    let now = bridge::now();
    PRE.with(|p| {
        let mut p = p.borrow_mut();
        p.retain(|e| now - e.sent < MAX_AGE_MS);
        while p.len() + 2 > KEEP {
            p.remove(0);
        }
        for (kind, f) in [(V, &video), (super::A, &audio)] {
            let end = head_end_max(f, kind, start < 4.0, PRE_EXTRA);
            let id = next_id();
            let url = format!("{}&range=0-{end}", f.url);
            bridge::fetch_opts(id, "GET", &url, "", "{}", "{\"progress\":1}");
            p.push(Pre { v: v.to_string(), itag: f.itag, kind, f: f.clone(), end, id, sent: now, url: f.url.clone(), ttfb: None, got: 0.0, body: None });
        }
    });
    true
}

/// The pointer moved on: drop unfinished prefetches of every video but `keep`.
pub fn pre_cancel(keep: &[&str]) {
    PRE.with(|p| {
        p.borrow_mut().retain(|e| {
            let drop = e.body.is_none() && !keep.contains(&e.v.as_str());
            if drop {
                bridge::abort(e.id);
            }
            !drop
        });
    });
}

/// Headers / body progress of a prefetch.
pub fn pre_progress(id: u32, bytes: f64) {
    PRE.with(|p| {
        if let Some(e) = p.borrow_mut().iter_mut().find(|e| e.id == id) {
            e.ttfb.get_or_insert(bridge::now() - e.sent);
            e.got = bytes;
        }
    });
}

/// A prefetch answered: its video's id when `id` was one.
pub fn pre_answer(id: u32, status: u32, body: &[u8]) -> Option<String> {
    PRE.with(|p| {
        let mut p = p.borrow_mut();
        let i = p.iter().position(|e| e.id == id)?;
        let v = p[i].v.clone();
        if (200..300).contains(&status) && body.len() as u64 > p[i].end.min(1) {
            let ms = bridge::now() - p[i].sent;
            p[i].body = Some((status, body.to_vec(), ms));
        } else {
            p.remove(i);
        }
        Some(v)
    })
}

/// `v`'s finished video prefetch (its format and body), lent to `f`.
pub(super) fn with_video<R>(v: &str, f: impl FnOnce(&Format, &[u8]) -> R) -> Option<R> {
    PRE.with(|p| {
        let p = p.borrow();
        let e = p.iter().rev().find(|e| e.v == v && e.kind == V && e.body.is_some())?;
        let (_, body, _) = e.body.as_ref()?;
        Some(f(&e.f, body))
    })
}

impl Own {
    /// `fetch_head` at the start: a prefetched first request for this format (covering `end`)?
    /// A finished one is answered right away; one in flight becomes ours.
    pub(super) fn pre_take(&mut self, kind: usize, f: &Format, end: u64) -> bool {
        let v = self.v.clone();
        let Some(e) = PRE.with(|p| {
            let mut p = p.borrow_mut();
            let i = p.iter().position(|e| e.v == v && e.itag == f.itag && e.kind == kind && e.end >= end && e.url == f.url)?;
            Some(p.remove(i))
        }) else {
            return false;
        };
        let what = format!("{} head itag {} (hover prefetch)", if kind == V { "video" } else { "audio" }, f.itag);
        match e.body {
            Some((status, bytes, ms)) => {
                self.tel_pre(kind, "hit");
                self.note(format!("{what}: ready ({ms:.0} ms)"));
                self.pre_ready.push((f.itag, kind, status, bytes));
            }
            None => {
                self.tel_pre(kind, "inflight");
                self.note(format!("{what}: still loading, taken over"));
                let mut i = Inflight::new(Req::Head { itag: f.itag, kind }, what);
                i.sent = e.sent;
                i.base = format!("{}&range=0-{}", f.url, e.end);
                i.want = (e.end + 1) as f64;
                i.ttfb = e.ttfb;
                i.got = e.got;
                self.fetches.insert(e.id, i);
            }
        }
        true
    }

    /// Answer the prefetched first requests taken in `fetch_head`.
    pub(super) fn pre_flush(&mut self) {
        for (itag, kind, status, bytes) in std::mem::take(&mut self.pre_ready) {
            self.reqlog.push(format!("{:>7.0} ms  {} head itag {itag} (hover prefetch) → HTTP {status} · {} B", bridge::now() - self.nav_start, if kind == V { "video" } else { "audio" }, bytes.len()));
            self.on_head(itag, kind, status, &bytes, 0.0);
            if self.failed {
                return;
            }
        }
    }
}
