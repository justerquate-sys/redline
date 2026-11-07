//! Buffer scheduling decisions for one track (pure): which segment to fetch
//! next, when to wait, what to evict, where to resume after a seek and where
//! to cut over on a quality switch.

use super::index::Seg;

/// Keep this much media buffered ahead of the playhead.
pub const AHEAD: f64 = 30.0;
/// Evict media more than this far behind the playhead.
pub const BEHIND: f64 = 60.0;
/// Media this far ahead of the playhead that is not contiguous with it (left
/// over from an earlier position) is evicted too.
pub const STALE_AHEAD: f64 = AHEAD + 30.0;
/// Buffered ranges closer than this to `t` count as containing it.
const SLACK: f64 = 0.3;

/// Buffered range containing `t` (with slack for frame gaps).
pub fn range_at(ranges: &[(f64, f64)], t: f64) -> Option<(f64, f64)> {
    ranges.iter().copied().find(|&(a, b)| a - SLACK <= t && t < b)
}

/// Seconds buffered contiguously ahead of `t`.
pub fn ahead(ranges: &[(f64, f64)], t: f64) -> f64 {
    range_at(ranges, t).map_or(0.0, |(_, b)| b - t)
}

/// First segment that still has media after `t`.
pub fn seg_for(segs: &[Seg], t: f64) -> usize {
    segs.iter().position(|s| s.t1 > t + 0.001).unwrap_or(segs.len())
}

/// Where a track starts fetching at `t` when nothing of it is buffered: the segment holding `t`,
/// and the last segment for a `t` at or past the end. Never "nothing to fetch": a track must get
/// its init segment (appended with its first media), and the end of stream before every track
/// has one is an error (Chromium: `DEMUXER_ERROR_COULD_NOT_OPEN`, "endOfStream before demuxer
/// initialization completes").
pub fn start_index(segs: &[Seg], t: f64) -> usize {
    seg_for(segs, t).min(segs.len().saturating_sub(1))
}

/// Where fetching resumes after a seek to `t`: after the buffered range that
/// contains `t`, or at the segment containing `t`.
pub fn resume_index(segs: &[Seg], ranges: &[(f64, f64)], t: f64) -> usize {
    match range_at(ranges, t) {
        Some((_, end)) => seg_for(segs, end),
        None => seg_for(segs, t),
    }
}

#[derive(Debug, PartialEq)]
pub enum Next {
    Fetch(usize),
    Wait,
    Done,
}

/// Fetch `next` unless it starts more than `ahead` seconds past the playhead.
pub fn next(segs: &[Seg], next: usize, t: f64, ahead: f64) -> Next {
    match segs.get(next) {
        None => Next::Done,
        Some(s) if s.t0 > t + ahead => Next::Wait,
        Some(_) => Next::Fetch(next),
    }
}

/// One range to remove: media too far behind, or stale media far ahead.
pub fn evict(ranges: &[(f64, f64)], t: f64, behind: f64, stale_ahead: f64) -> Option<(f64, f64)> {
    for &(a, b) in ranges {
        if a < t - behind {
            return Some((a, b.min(t - behind)));
        }
        if a > t + stale_ahead {
            return Some((a, b));
        }
    }
    None
}

/// Quality switch "now": the first segment of the new representation that
/// starts at least `margin` seconds after `t` (what was buffered before it is
/// kept; everything from its start is replaced).
pub fn switch_index(segs: &[Seg], t: f64, margin: f64) -> usize {
    segs.iter().position(|s| s.t0 >= t + margin).unwrap_or(segs.len())
}

/// Immediate switch with buffered `ranges`: like `switch_index`, but never
/// cutting after the end of the media buffered around `t` (that would leave a
/// gap): then the new segment containing that end is used (overlap only).
///
/// No new segment starting after `t + margin` (a one-segment rung, or `t` in the last segment)
/// is no reason to stop fetching either: the new segment containing that end is used, so only
/// media already buffered to the end counts as done (before, a one-segment rung switched to
/// before anything was appended was "done" without its init segment, and the end of stream
/// that followed failed the start).
pub fn switch_cut(segs: &[Seg], ranges: &[(f64, f64)], t: f64, margin: f64) -> usize {
    let end = range_at(ranges, t).map_or(t, |(_, b)| b);
    let i = switch_index(segs, t, margin);
    match segs.get(i) {
        Some(s) if s.t0 <= end => i,
        _ => seg_for(segs, end),
    }
}

/// Seamless switch at the fetch pointer: the segment of the new representation
/// that contains `at` (overlap of up to one segment, never a gap).
pub fn map_index(segs: &[Seg], at: f64) -> usize {
    seg_for(segs, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(d: f64, n: usize) -> Vec<Seg> {
        (0..n).map(|i| Seg { start: i as u64 * 100, end: i as u64 * 100 + 99, t0: i as f64 * d, t1: (i + 1) as f64 * d }).collect()
    }

    #[test]
    fn fetch_wait_done() {
        let s = segs(5.0, 10);
        assert_eq!(next(&s, 0, 0.0, AHEAD), Next::Fetch(0));
        assert_eq!(next(&s, 6, 0.0, AHEAD), Next::Fetch(6), "t0 = 30 is within 30 s");
        assert_eq!(next(&s, 7, 0.0, AHEAD), Next::Wait);
        assert_eq!(next(&s, 7, 6.0, AHEAD), Next::Fetch(7));
        assert_eq!(next(&s, 10, 49.0, AHEAD), Next::Done);
    }

    #[test]
    fn seek_resume_points() {
        let s = segs(5.0, 10);
        // Seek inside the buffered range: continue after it (no refetch).
        assert_eq!(resume_index(&s, &[(0.0, 14.98)], 7.0), 2, "end 14.98 → segment 10–15 again (one overlap, no gap)");
        assert_eq!(resume_index(&s, &[(0.0, 15.0)], 7.0), 3);
        // Seek outside: the segment containing the target.
        assert_eq!(resume_index(&s, &[(0.0, 15.0)], 32.0), 6);
        assert_eq!(resume_index(&s, &[], 0.0), 0);
        assert_eq!(resume_index(&s, &[], 50.0), 10);
        // Just before a range start (keyframe slack) counts as inside.
        assert_eq!(resume_index(&s, &[(30.1, 40.0)], 30.0), 8);
        assert_eq!(ahead(&[(0.0, 4.0), (10.0, 20.0)], 12.0), 8.0);
        assert_eq!(ahead(&[(0.0, 4.0)], 5.0), 0.0);
    }

    #[test]
    fn eviction() {
        assert_eq!(evict(&[(0.0, 100.0)], 50.0, BEHIND, STALE_AHEAD), None);
        assert_eq!(evict(&[(0.0, 100.0)], 70.0, BEHIND, STALE_AHEAD), Some((0.0, 10.0)));
        assert_eq!(evict(&[(0.0, 5.0), (60.0, 100.0)], 80.0, BEHIND, STALE_AHEAD), Some((0.0, 5.0)));
        // After seeking back to 10 s, media at 200 s is stale.
        assert_eq!(evict(&[(10.0, 20.0), (200.0, 230.0)], 10.0, BEHIND, STALE_AHEAD), Some((200.0, 230.0)));
        // Aggressive eviction (quota exceeded) with a short "behind".
        assert_eq!(evict(&[(0.0, 40.0)], 30.0, 10.0, STALE_AHEAD), Some((0.0, 20.0)));
    }

    #[test]
    fn switching() {
        let old = segs(5.0, 10);
        let new = segs(4.0, 13);
        assert_eq!(switch_index(&new, 9.5, 1.0), 3, "first new segment starting ≥ 10.5 → 12 s");
        assert_eq!(switch_index(&new, 60.0, 1.0), 13);
        // Seamless: pointer at old segment 3 (15 s) → new segment containing 15 s (12–16).
        assert_eq!(map_index(&new, old[3].t0), 3);
        assert_eq!(map_index(&new, 16.0), 4);
        // Buffered to 30 s at t = 9.5: cut at 12 s (kept 9.5–12 in the old quality).
        assert_eq!(switch_cut(&new, &[(0.0, 30.0)], 9.5, 1.0), 3);
        // Buffered only to 10.2: the new segment containing 10.2 (8–12) starts at 8 ≤ 10.2.
        assert_eq!(switch_cut(&new, &[(0.0, 10.2)], 9.5, 1.0), 2);
        assert_eq!(switch_cut(&new, &[], 9.5, 1.0), 2);
        // Nothing new starts after t + margin: the segment holding the buffered end, never "done"
        // unless what is buffered reaches the end.
        let one = segs(4.0, 1);
        assert_eq!(switch_cut(&one, &[], 0.0, 1.0), 0, "a one-segment rung, nothing buffered: its segment");
        assert_eq!(switch_cut(&one, &[(0.0, 2.0)], 0.0, 1.0), 0);
        assert_eq!(switch_cut(&one, &[(0.0, 4.0)], 0.0, 1.0), 1, "buffered to the end: done");
        assert_eq!(switch_cut(&new, &[(0.0, 50.0)], 50.5, 1.0), 12, "in the last segment, buffered to 50: its rest");
        assert_eq!(switch_cut(&new, &[(0.0, 52.0)], 51.5, 1.0), 13);
    }

    #[test]
    fn a_start_always_has_a_segment() {
        let s = segs(5.0, 8);
        assert_eq!(start_index(&s, 0.0), 0);
        assert_eq!(start_index(&s, 12.0), 2);
        assert_eq!(start_index(&s, 39.999), 7);
        // At or past the end (a resume at the very end, a t= past it): the last segment, so the
        // track still gets its init segment instead of being "done" with nothing appended.
        assert_eq!(seg_for(&s, 40.0), 8);
        assert_eq!(start_index(&s, 40.0), 7);
        assert_eq!(start_index(&s, 1e9), 7);
        assert_eq!(start_index(&[], 3.0), 0);
    }
}
