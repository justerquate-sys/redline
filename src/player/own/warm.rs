//! The warm first frame (3 Nov 2026, the owner: "when I choose a video, you first get to see, when it zooms
//! in, an animation that shows the preview in super bad quality, but ideally it should just be the start of
//! the video already, without the preview").
//!
//! When the hover/focus media prefetch (`pre.rs`) has the index and first segment of exactly the format the
//! player will start with, and the pointer or focus still rests on that video, a muted `<video>` outside the
//! document with its own MediaSource decodes the first frame from those bytes (nothing is fetched). The frame
//! is drawn into a canvas at the video's own resolution (at most the screen's width in device pixels) and the
//! decoder is released at once, so it holds a decoder for the ~10–50 ms of one keyframe (TV hardware has few).
//! On the click the player box gets that canvas over its `<video>` (`Own::warm_poster`): the zoom grows the
//! real, sharp first frame, the player's own first frame (the same picture) replaces it when presented, and
//! the canvas goes. No warm frame (no rest on the link, a resume point past the first segment, a cold page):
//! the box is the dark player surface until the first frame, never an upscaled thumbnail.
//!
//! Evaluated and not used: adopting the warm `<video>` (with its MediaSource) as the player. Chromium keeps an
//! MSE `<video>` playing when it moves between parents in one task (probe 3 Nov 2026; detaching it for a task
//! pauses it but keeps the MediaSource open), but it would need a second player state machine running hidden
//! (requests, timers, telemetry, UI writes guarded) and it would keep a hardware decoder for as long as the
//! pointer rests; the canvas shows the same picture at the same moment.
//!
//! One warm frame at a time: a new one (another video under the pointer or focus) drops the last.

use super::{formats, index, next_id, pre};
use crate::bridge;
use crate::player::frag;
use formats::Format;
use std::cell::RefCell;

/// A decode that has no frame after this long is given up (its decoder released).
const WARM_MAX_MS: u32 = 2000;
/// A drawn frame nobody opened is dropped after this long (its canvas is up to ~8–33 MB).
const WARM_KEEP_MS: u32 = 60_000;
/// The canvas's class (client.css: over the player's `<video>`, under its controls).
pub const CLS: &str = "pwf";

struct Warm {
    v: String,
    vid: u32,
    ms: u32,
    sb: u32,
    mime: String,
    start: f64,
    /// Appends still to make (init, then the first segment's bytes).
    queue: Vec<Vec<u8>>,
    timer: u32,
    /// The drawn first frame's canvas id, once there (the decoder is gone by then).
    canvas: Option<String>,
}

thread_local! {
    static WARM: RefCell<Option<Warm>> = const { RefCell::new(None) };
    /// Warm decoders alive now and made so far (test marks).
    static COUNT: RefCell<(u32, u32)> = const { RefCell::new((0, 0)) };
}

/// Where the first segment's bytes start in a prefetched head `body` of `f` (index request + the start of
/// segment 0) when they hold a decodable frame at `start` seconds; None otherwise (index only, a start past
/// what came along, a layout not read).
pub fn frame_bytes(f: &Format, body: &[u8], start: f64) -> Option<usize> {
    let need = f.index_end as usize + 1;
    if f.init_end == 0 || f.index_end <= f.init_end || body.len() <= need || !start.is_finite() || start < 0.0 {
        return None;
    }
    let segs = if f.webm { index::parse_cues(&body[..need], f.len, f.dur) } else { index::parse_sidx(&body[..need]) }.ok()?;
    let s0 = segs.first()?;
    // The first segment must follow the index directly and hold `start`.
    if s0.start as usize != need || start >= s0.t1 {
        return None;
    }
    let media = &body[need..];
    let covered = if f.webm { frag::webm_covered(media)? } else { s0.t0 + frag::mp4_covered(media)? * (s0.t1 - s0.t0) };
    // fMP4: the share of whole samples (> 0: the keyframe is in); WebM: the last whole block's time.
    let ok = if f.webm { covered >= start } else { covered > s0.t0 && covered >= start };
    ok.then_some(need)
}

/// The canvas's widest size: the screen's width in device pixels (`[w, h, dpr]`), 1920 when unknown.
pub fn max_width(screen: &[f64]) -> f64 {
    if screen.len() == 3 && screen[0] > 0.0 && screen[2] > 0.0 { (screen[0] * screen[2]).max(screen[1] * screen[2]).round() } else { 1920.0 }
}

fn mark() {
    let s = WARM.with(|w| w.borrow().as_ref().map(|w| format!("{}:{}", if w.canvas.is_some() { "ready" } else { "decoding" }, w.v)).unwrap_or_default());
    bridge::mark_val("ytcWarm", &s);
    let (live, made) = COUNT.with(|c| *c.borrow());
    bridge::mark_val("ytcWarmLive", &live.to_string());
    bridge::mark_val("ytcWarmMade", &made.to_string());
}

/// Releases a warm decoder (its `<video>`, MediaSource and SourceBuffer).
fn free_decoder(w: &mut Warm) {
    if w.vid == 0 {
        return;
    }
    bridge::media_reset(w.vid);
    for h in [w.vid, w.ms, w.sb] {
        if h != 0 {
            bridge::media_free(h);
        }
    }
    w.vid = 0;
    w.ms = 0;
    w.sb = 0;
    COUNT.with(|c| c.borrow_mut().0 -= 1);
}

fn drop_all(w: Option<Warm>) {
    if let Some(mut w) = w {
        free_decoder(&mut w);
        if let Some(id) = w.canvas.take() {
            bridge::held_drop(&id);
        }
    }
}

/// Decode `v`'s first frame (at `start` s) from its finished media prefetch, unless it is warm already.
/// False when there is nothing to decode it from.
pub fn warm_try(v: &str, start: f64) -> bool {
    if WARM.with(|w| w.borrow().as_ref().is_some_and(|w| w.v == v)) {
        return true;
    }
    let Some(Some((mime, init, media))) = pre::with_video(v, |f, body| {
        let at = frame_bytes(f, body, start)?;
        Some((f.mime.clone(), body[..=f.init_end as usize].to_vec(), body[at..].to_vec()))
    }) else {
        return false;
    };
    drop_all(WARM.with(|w| w.borrow_mut().take()));
    let vid = next_id();
    bridge::media_video_new(vid, "loadeddata,seeked,error");
    let ms = next_id();
    bridge::media_source(ms, vid);
    let timer = next_id();
    bridge::timer(timer, WARM_MAX_MS);
    COUNT.with(|c| {
        let mut c = c.borrow_mut();
        c.0 += 1;
        c.1 += 1;
    });
    WARM.with(|w| *w.borrow_mut() = Some(Warm { v: v.to_string(), vid, ms, sb: 0, mime, start, queue: vec![media, init], timer, canvas: None }));
    mark();
    true
}

/// The pointer or focus rests on another video now: drop the warm frame unless it is one of `keep`'s.
pub fn warm_keep(keep: &[&str]) {
    let gone = WARM.with(|w| {
        let mut w = w.borrow_mut();
        if w.as_ref().is_some_and(|x| !keep.contains(&x.v.as_str())) { w.take() } else { None }
    });
    if gone.is_some() {
        drop_all(gone);
        mark();
    }
}

/// The player for `v` mounts: its warm frame's canvas id when one is drawn (the player owns it from now
/// on). Anything else warm is dropped (its decoder must not compete with the player's).
pub fn warm_take(v: &str) -> Option<String> {
    let w = WARM.with(|w| w.borrow_mut().take());
    let out = match w {
        Some(mut w) if w.v == v && w.canvas.is_some() => w.canvas.take(),
        w => {
            drop_all(w);
            None
        }
    };
    mark();
    if out.is_some() {
        bridge::mark_val("ytcWarm", &format!("used:{v}"));
    }
    out
}

fn append_next(w: &mut Warm) -> bool {
    match w.queue.pop() {
        Some(b) => bridge::sb_append(w.sb, &b).is_empty(),
        None => true,
    }
}

/// A media event of the warm decoder. True when `h` was one of its handles.
pub fn warm_media(h: u32, ev: &str) -> bool {
    let mine = WARM.with(|w| w.borrow().as_ref().is_some_and(|w| h != 0 && (h == w.vid || h == w.ms || h == w.sb)));
    if !mine {
        return false;
    }
    let ok = WARM.with(|w| {
        let mut b = w.borrow_mut();
        let w = b.as_mut().expect("warm");
        match ev {
            "sourceopen" if h == w.ms => {
                w.sb = next_id();
                if !bridge::source_buffer(w.sb, w.ms, &w.mime) {
                    w.sb = 0;
                    return false;
                }
                if w.start > 0.0 {
                    bridge::media_set(w.vid, "currentTime", w.start);
                }
                append_next(w)
            }
            "updateend" if h == w.sb => append_next(w),
            "loadeddata" | "seeked" if h == w.vid => {
                // The frame at the start is decoded (a start past 0 waits for the seek to it).
                if (ev == "loadeddata") != (w.start == 0.0) {
                    return true;
                }
                if w.canvas.is_some() {
                    return true;
                }
                let id = format!("ytc-wf{}", w.vid);
                let drawn = bridge::media_grab(w.vid, &id, CLS, max_width(&bridge::screen_px()));
                free_decoder(w);
                if drawn {
                    w.canvas = Some(id);
                    w.timer = next_id();
                    bridge::timer(w.timer, WARM_KEEP_MS);
                }
                drawn
            }
            // (Reported for every MediaSource since the player's recovery uses them; not the warm decoder's concern.)
            "sourceended" | "sourceclose" => true,
            _ => false,
        }
    });
    if !ok {
        drop_all(WARM.with(|w| w.borrow_mut().take()));
    }
    mark();
    true
}

/// A timer of the warm frame (the decode's deadline, then the drawn frame's age). True when `id` was it.
pub fn warm_timer(id: u32) -> bool {
    let mine = WARM.with(|w| w.borrow().as_ref().is_some_and(|w| w.timer == id));
    if mine {
        drop_all(WARM.with(|w| w.borrow_mut().take()));
        mark();
    }
    mine
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::player::frag::tests::fragment;
    use crate::player::index::tests::sidx_box;

    #[test]
    fn canvas_width_is_the_screens() {
        assert_eq!(max_width(&[1512.0, 982.0, 2.0]), 3024.0);
        assert_eq!(max_width(&[960.0, 540.0, 2.0]), 1920.0);
        assert_eq!(max_width(&[]), 1920.0);
    }

    #[test]
    fn a_frame_needs_the_keyframe_after_the_index() {
        // fMP4: init (ftyp + moov stand-ins), a sidx of two 5 s segments, then the first segment's fragment.
        let init = [vec![0, 0, 0, 8], b"ftyp".to_vec(), vec![0, 0, 0, 8], b"moov".to_vec()].concat();
        let sidx = sidx_box(0, 1000, 0, &[(400, 5000), (400, 5000)]);
        let head = [init.clone(), sidx.clone()].concat();
        let f = Format { init_end: init.len() as u64 - 1, index_end: head.len() as u64 - 1, len: 10_000, dur: 10.0, video: true, mime: "video/mp4; codecs=\"av01.0.08M.08\"".into(), ..Format::default() };
        // Five samples of 1 s: a 50-byte keyframe, then 10-byte frames.
        let frag = fragment(&[(1000, 50), (1000, 10), (1000, 10), (1000, 10), (1000, 10)]);
        let mdat = frag.len() - 90;
        let with = |n: usize| [head.clone(), frag[..n].to_vec()].concat();
        assert_eq!(frame_bytes(&f, &head, 0.0), None, "the index only");
        assert_eq!(frame_bytes(&f, &with(20), 0.0), None, "the moof is not complete");
        assert_eq!(frame_bytes(&f, &with(mdat + 49), 0.0), None, "the keyframe is not complete");
        assert_eq!(frame_bytes(&f, &with(mdat + 50), 0.0), Some(head.len()), "the keyframe is in");
        assert_eq!(frame_bytes(&f, &with(mdat + 50), 2.0), None, "a start the bytes do not reach");
        assert_eq!(frame_bytes(&f, &with(mdat + 70), 2.0), Some(head.len()));
        assert_eq!(frame_bytes(&f, &with(frag.len()), 6.0), None, "a start in the second segment");
        assert_eq!(frame_bytes(&f, &with(frag.len()), f64::NAN), None);
        assert_eq!(frame_bytes(&Format { init_end: 0, ..f.clone() }, &with(frag.len()), 0.0), None, "no init range");
        // The first segment must start right after the index.
        let gap = sidx_box(0, 1000, 16, &[(400, 5000)]);
        let g = Format { index_end: (init.len() + gap.len()) as u64 - 1, ..f.clone() };
        assert_eq!(frame_bytes(&g, &[init.clone(), gap, frag.clone()].concat(), 0.0), None);
    }
}
