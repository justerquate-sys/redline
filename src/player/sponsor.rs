//! Sponsor skipping with SponsorBlock's public database (since
//! 2 Nov 2026). Pure logic, unit tested; `own.rs` is the glue.
//!
//! Privacy: the lookup is k-anonymous. Only the first 4 hex characters of
//! SHA-256(video id) leave the browser (`/api/skipSegments/<prefix>`); the
//! server answers with every video in that bucket (about 150) and the video is
//! picked here. No cookies, no referrer, no HTTP cache, nothing persisted: the
//! answers live in memory for this tab (`cache_get`/`cache_put`). Skips are
//! never reported back (no `viewedVideoSponsorTime`).

use crate::json::{self, Value};
use crate::util::is_video_id;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

pub const API: &str = "https://sponsor.ajay.app/api/skipSegments/";
/// A playing segment is "entered" this early (seconds), and a skip lands at its end.
pub const TOL: f64 = 0.1;
/// No answer in this long → nothing happens for this video.
pub const TIMEOUT_MS: u32 = 6000;
/// Videos kept in memory (this tab).
const CACHE_MAX: usize = 100;
/// Segments taken from one answer (the busiest real videos have ~15).
const MAX_SEGS: usize = 64;

// ------------------------------------------------------------------ SHA-256

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// SHA-256 (FIPS 180-4). Small and dependency-free; the test vectors below are NIST's.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64).wrapping_mul(8)).to_be_bytes());
    let mut w = [0u32; 64];
    for block in msg.chunks_exact(64) {
        for (i, c) in block.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, x) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
    }
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The k-anonymity bucket: the first 4 hex characters of SHA-256(video id).
pub fn prefix(video_id: &str) -> String {
    hex(&sha256(video_id.as_bytes())[..2])
}

// --------------------------------------------------------------- categories

/// SponsorBlock categories the client shows. `Highlight` is a point, the others are ranges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cat {
    Sponsor,
    SelfPromo,
    Interaction,
    Intro,
    Outro,
    Preview,
    MusicOfftopic,
    Filler,
    Highlight,
}

pub const ALL: [Cat; 9] = [Cat::Sponsor, Cat::SelfPromo, Cat::Interaction, Cat::Intro, Cat::Outro, Cat::Preview, Cat::MusicOfftopic, Cat::Filler, Cat::Highlight];
/// Categories that can be auto-skipped (the menu's choices, in this order).
pub const SKIPPABLE: [Cat; 8] = [Cat::Sponsor, Cat::SelfPromo, Cat::Interaction, Cat::Intro, Cat::Outro, Cat::Preview, Cat::MusicOfftopic, Cat::Filler];
/// SponsorBlock's own defaults: sponsor, self-promotion and interaction reminders are skipped;
/// the rest are only shown on the seek bar.
pub const DEFAULT_SKIP: [Cat; 3] = [Cat::Sponsor, Cat::SelfPromo, Cat::Interaction];

impl Cat {
    pub fn key(self) -> &'static str {
        match self {
            Cat::Sponsor => "sponsor",
            Cat::SelfPromo => "selfpromo",
            Cat::Interaction => "interaction",
            Cat::Intro => "intro",
            Cat::Outro => "outro",
            Cat::Preview => "preview",
            Cat::MusicOfftopic => "music_offtopic",
            Cat::Filler => "filler",
            Cat::Highlight => "poi_highlight",
        }
    }
    pub fn from_key(k: &str) -> Option<Cat> {
        ALL.into_iter().find(|c| c.key() == k)
    }
    /// Menu label.
    pub fn label(self) -> &'static str {
        match self {
            Cat::Sponsor => "Sponsor",
            Cat::SelfPromo => "Self-promo",
            Cat::Interaction => "Interaction",
            Cat::Intro => "Intro",
            Cat::Outro => "Outro",
            Cat::Preview => "Preview",
            Cat::MusicOfftopic => "Non-music",
            Cat::Filler => "Filler",
            Cat::Highlight => "Highlight",
        }
    }
    /// Toast ("Skipped sponsor") and tooltip wording.
    pub fn noun(self) -> &'static str {
        match self {
            Cat::Sponsor => "sponsor",
            Cat::SelfPromo => "self-promotion",
            Cat::Interaction => "interaction reminder",
            Cat::Intro => "intro",
            Cat::Outro => "outro",
            Cat::Preview => "preview",
            Cat::MusicOfftopic => "non-music part",
            Cat::Filler => "filler",
            Cat::Highlight => "highlight",
        }
    }
    /// Color class (`k0`…`k8`, client.css: SponsorBlock's colors adapted to the dark surfaces, apart from the red progress).
    pub fn idx(self) -> usize {
        ALL.iter().position(|&c| c == self).unwrap_or(0)
    }
    pub fn bit(self) -> u16 {
        1 << self.idx()
    }
}

/// Bit set of auto-skipped categories.
pub fn mask(cats: &[Cat]) -> u16 {
    cats.iter().fold(0, |m, c| m | c.bit())
}

pub fn default_mask() -> u16 {
    mask(&DEFAULT_SKIP)
}

/// Stored form of the skip choice (`p.sb` in `ytcConfigV1`): category keys, comma separated.
pub fn mask_to_keys(m: u16) -> String {
    SKIPPABLE.iter().filter(|c| m & c.bit() != 0).map(|c| c.key()).collect::<Vec<_>>().join(",")
}

pub fn mask_from_keys(s: &str) -> u16 {
    s.split(',').filter_map(Cat::from_key).filter(|c| *c != Cat::Highlight).fold(0, |m, c| m | c.bit())
}

/// The lookup URL: only the hash prefix of the video id; every category is
/// asked for (the answer does not depend on, or reveal, the user's choice).
pub fn url(video_id: &str) -> Option<String> {
    if !is_video_id(video_id) {
        return None;
    }
    let cats = ALL.iter().map(|c| format!("%22{}%22", c.key())).collect::<Vec<_>>().join(",");
    Some(format!("{API}{}?categories=%5B{cats}%5D&actionTypes=%5B%22skip%22,%22poi%22%5D", prefix(video_id)))
}

// ----------------------------------------------------------------- segments

#[derive(Clone, Debug, PartialEq)]
pub struct Seg {
    pub cat: Cat,
    pub start: f64,
    /// Equal to `start` for the highlight point.
    pub end: f64,
}

impl Seg {
    pub fn is_point(&self) -> bool {
        self.cat == Cat::Highlight
    }
}

/// Picks `video_id` out of a bucket answer (`[{videoID, segments:[{category,
/// actionType, segment:[s,e]}]}]`). Unknown categories/action types and bad
/// numbers are dropped; ranges sorted by start. An empty bucket or another
/// video's entries only → no segments.
pub fn parse(body: &[u8], video_id: &str) -> Vec<Seg> {
    let Some(v) = json::parse(body) else { return Vec::new() };
    let Some(entry) = v.as_array().and_then(|a| a.iter().find(|e| e.get("videoID").and_then(Value::as_str) == Some(video_id))) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for s in entry.get("segments").and_then(Value::as_array).into_iter().flatten() {
        let Some(cat) = s.get("category").and_then(Value::as_str).and_then(Cat::from_key) else { continue };
        let action = s.get("actionType").and_then(Value::as_str).unwrap_or("skip");
        if action != if cat == Cat::Highlight { "poi" } else { "skip" } {
            continue;
        }
        let r = s.get("segment");
        let (Some(a), Some(b)) = (r.and_then(|r| r.get(0usize)).and_then(Value::as_f64), r.and_then(|r| r.get(1usize)).and_then(Value::as_f64)) else { continue };
        if !(a.is_finite() && b.is_finite() && a >= 0.0) {
            continue;
        }
        let seg = if cat == Cat::Highlight { Seg { cat, start: a, end: a } } else { Seg { cat, start: a, end: b } };
        // Ranges shorter than a skip tolerance are noise.
        if !seg.is_point() && seg.end - seg.start < 2.0 * TOL {
            continue;
        }
        out.push(seg);
        if out.len() >= MAX_SEGS {
            break;
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

thread_local! {
    static CACHE: RefCell<VecDeque<(String, Rc<Vec<Seg>>)>> = const { RefCell::new(VecDeque::new()) };
}

/// This tab's answers (memory only, never persisted).
pub fn cache_get(video_id: &str) -> Option<Rc<Vec<Seg>>> {
    CACHE.with(|c| c.borrow().iter().find(|(k, _)| k == video_id).map(|(_, v)| v.clone()))
}

pub fn cache_put(video_id: &str, segs: Rc<Vec<Seg>>) {
    CACHE.with(|c| {
        let mut c = c.borrow_mut();
        c.retain(|(k, _)| k != video_id);
        c.push_back((video_id.to_string(), segs));
        while c.len() > CACHE_MAX {
            c.pop_front();
        }
    });
}

// ---------------------------------------------------------------- decisions

/// Whether segment `i` auto-skips: its category is chosen and Undo did not exempt it.
fn skippable(segs: &[Seg], i: usize, mask: u16, undone: &[usize]) -> bool {
    !segs[i].is_point() && mask & segs[i].cat.bit() != 0 && !undone.contains(&i)
}

/// `t` is inside segment `s` for skipping purposes (entered up to `TOL` early, not in its last `TOL`).
fn inside(s: &Seg, t: f64) -> bool {
    t >= s.start - TOL && t < s.end - TOL
}

/// Playback is at `t`: the segment to skip and where to land (its end, carried
/// through any chosen segment that overlaps or touches it), or None.
/// `manual` = where the user last seeked to: a segment that contains it was
/// entered on purpose and only shows (until playback leaves it, see `keep_manual`).
pub fn skip_at(segs: &[Seg], mask: u16, undone: &[usize], manual: Option<f64>, t: f64) -> Option<(usize, f64)> {
    if !t.is_finite() {
        return None;
    }
    let i = (0..segs.len()).find(|&i| skippable(segs, i, mask, undone) && inside(&segs[i], t) && !manual.is_some_and(|m| m >= segs[i].start - TOL && m < segs[i].end))?;
    let mut to = segs[i].end;
    loop {
        let next = (0..segs.len()).filter(|&j| skippable(segs, j, mask, undone) && inside(&segs[j], to) && segs[j].end > to).map(|j| segs[j].end).fold(None, |m: Option<f64>, e| Some(m.map_or(e, |m| m.max(e))));
        match next {
            Some(e) => to = e,
            None => break,
        }
    }
    Some((i, to))
}

/// A manual seek target keeps suppressing skips only while playback is still
/// inside a segment that contains it.
pub fn keep_manual(segs: &[Seg], m: f64, t: f64) -> bool {
    segs.iter().any(|s| !s.is_point() && m >= s.start - TOL && m < s.end && t >= s.start - TOL && t < s.end)
}

/// Seconds of playback until the next skip starts (for a precise timer when it
/// is closer than one player tick), at `rate`.
pub fn until_next(segs: &[Seg], mask: u16, undone: &[usize], t: f64, rate: f64) -> Option<f64> {
    if !(rate.is_finite() && rate > 0.0) {
        return None;
    }
    (0..segs.len()).filter(|&i| skippable(segs, i, mask, undone) && segs[i].start - TOL > t).map(|i| (segs[i].start - TOL - t) / rate).fold(None, |m: Option<f64>, d| Some(m.map_or(d, |m| m.min(d))))
}

/// The highlight point, if the video has one.
pub fn highlight(segs: &[Seg]) -> Option<f64> {
    segs.iter().find(|s| s.is_point()).map(|s| s.start)
}

/// The range segment under `t` (the topmost: the latest start), for the tooltip.
pub fn at(segs: &[Seg], t: f64) -> Option<Cat> {
    segs.iter().rev().find(|s| !s.is_point() && t >= s.start && t < s.end).map(|s| s.cat)
}

// ------------------------------------------------------------------- markup

/// Colored ranges (and the highlight tick) for one seek-bar segment that spans
/// `lo..hi` of the video (fractions); positions are local to it (`--a`/`--z`).
pub fn marks_html(segs: &[Seg], d: f64, lo: f64, hi: f64) -> String {
    let mut out = String::new();
    if !(d.is_finite() && d > 0.0 && hi > lo) {
        return out;
    }
    let local = |x: f64| ((x / d - lo) / (hi - lo)).clamp(0.0, 1.0);
    for s in segs {
        let (a, z) = (s.start / d, s.end.min(d) / d);
        if s.is_point() {
            if a >= lo && a < hi {
                out.push_str(&format!("<i class=\"psbp k{}\" style=\"--a:{:.5}\"></i>", s.cat.idx(), local(s.start)));
            }
        } else if z > lo && a < hi {
            out.push_str(&format!("<i class=\"psb k{}\" style=\"--a:{:.5};--z:{:.5}\"></i>", s.cat.idx(), local(s.start), local(s.end.min(d))));
        }
    }
    out
}

/// Toast text after a skip.
pub fn skipped_label(c: Cat) -> String {
    format!("Skipped {}", c.noun())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_nist_vectors() {
        assert_eq!(hex(&sha256(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(hex(&sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")), "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1");
        assert_eq!(
            hex(&sha256(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
            "cf5b16a778af8380036ce59e7b0492370b249b11e8f07a51afac45037afee9d1"
        );
        assert_eq!(hex(&sha256(&vec![b'a'; 1_000_000])), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // Padding edges: 55, 56 and 64 bytes (one / two blocks).
        assert_eq!(hex(&sha256(&[b'a'; 55])), "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318");
        assert_eq!(hex(&sha256(&[b'a'; 56])), "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a");
        assert_eq!(hex(&sha256(&[b'a'; 64])), "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb");
    }

    #[test]
    fn lookup_sends_only_the_hash_prefix() {
        // Checked against the live API on 2 Nov 2026 (bucket 671e holds 151 videos).
        assert_eq!(prefix("aqz-KE-bpKQ"), "671e");
        let u = url("aqz-KE-bpKQ").unwrap();
        assert!(u.starts_with("https://sponsor.ajay.app/api/skipSegments/671e?categories=%5B%22sponsor%22,"), "{u}");
        assert!(!u.contains("aqz-KE-bpKQ"));
        assert!(u.contains("%22poi_highlight%22") && u.ends_with("actionTypes=%5B%22skip%22,%22poi%22%5D"), "{u}");
        assert!(url("bad\"id").is_none());
    }

    #[test]
    fn parse_picks_the_video_from_its_bucket() {
        let body = br#"[{"videoID":"Cw7jmJ42cck","segments":[{"category":"intro","actionType":"skip","segment":[0,9],"UUID":"d2","videoDuration":151.8,"locked":0,"votes":0,"description":""}]},
            {"videoID":"aqz-KE-bpKQ","segments":[
              {"category":"selfpromo","actionType":"skip","segment":[24,27.5],"UUID":"a"},
              {"category":"sponsor","actionType":"skip","segment":[10,16],"UUID":"b"},
              {"category":"poi_highlight","actionType":"poi","segment":[32,32],"UUID":"c"},
              {"category":"sponsor","actionType":"mute","segment":[1,5],"UUID":"m"},
              {"category":"chapter","actionType":"chapter","segment":[0,40],"UUID":"x"},
              {"category":"sponsor","actionType":"skip","segment":[20,20.1],"UUID":"tiny"},
              {"category":"sponsor","actionType":"skip","segment":["a",3],"UUID":"bad"}]}]"#;
        let s = parse(body, "aqz-KE-bpKQ");
        assert_eq!(
            s,
            vec![Seg { cat: Cat::Sponsor, start: 10.0, end: 16.0 }, Seg { cat: Cat::SelfPromo, start: 24.0, end: 27.5 }, Seg { cat: Cat::Highlight, start: 32.0, end: 32.0 }]
        );
        assert!(parse(body, "zzzzzzzzzzz").is_empty(), "another video's bucket entry is not ours");
        assert!(parse(b"Not Found", "aqz-KE-bpKQ").is_empty());
        assert!(parse(b"{}", "aqz-KE-bpKQ").is_empty());
    }

    fn segs() -> Vec<Seg> {
        vec![
            Seg { cat: Cat::Intro, start: 0.0, end: 3.0 },
            Seg { cat: Cat::Sponsor, start: 10.0, end: 16.0 },
            Seg { cat: Cat::Interaction, start: 16.0, end: 18.0 },
            Seg { cat: Cat::SelfPromo, start: 24.0, end: 27.0 },
            Seg { cat: Cat::Highlight, start: 32.0, end: 32.0 },
        ]
    }

    #[test]
    fn skips_chosen_categories_with_tolerance_and_chains() {
        let s = segs();
        let m = default_mask();
        assert_eq!(skip_at(&s, m, &[], None, 1.0), None, "intro is shown, not skipped, by default");
        assert_eq!(skip_at(&s, m, &[], None, 9.85), None);
        // Entered 0.1 s early; lands at the end of the touching interaction reminder.
        assert_eq!(skip_at(&s, m, &[], None, 9.92), Some((1, 18.0)));
        assert_eq!(skip_at(&s, m, &[], None, 12.0), Some((1, 18.0)));
        assert_eq!(skip_at(&s, m, &[], None, 15.95), Some((2, 18.0)), "in the sponsor's last 0.1 s the next one takes over");
        assert_eq!(skip_at(&s, m, &[], None, 17.95), None, "the last 0.1 s is not worth a seek");
        assert_eq!(skip_at(&s, m, &[], None, 25.0), Some((3, 27.0)));
        assert_eq!(skip_at(&s, m, &[], None, 32.0), None, "the highlight is a point, never skipped");
        // Only sponsor chosen: interaction no longer extends the jump.
        assert_eq!(skip_at(&s, Cat::Sponsor.bit(), &[], None, 12.0), Some((1, 16.0)));
        assert_eq!(skip_at(&s, mask(&[Cat::Intro]), &[], None, 0.0), Some((0, 3.0)));
        assert_eq!(skip_at(&s, 0, &[], None, 12.0), None, "nothing chosen");
        // Undo exempts that segment only.
        assert_eq!(skip_at(&s, m, &[1], None, 12.0), None);
        assert_eq!(skip_at(&s, m, &[1], None, 16.5), Some((2, 18.0)));
    }

    #[test]
    fn a_manual_seek_into_a_segment_only_shows_it() {
        let s = segs();
        let m = default_mask();
        assert_eq!(skip_at(&s, m, &[], Some(12.0), 13.0), None, "seeked into the sponsor: it plays");
        assert!(keep_manual(&s, 12.0, 15.9));
        // Playback carries on into the touching interaction reminder: still inside a segment that held the seek? No:
        // the interaction segment does not contain 12, so it skips.
        assert!(!keep_manual(&s, 12.0, 16.2));
        assert_eq!(skip_at(&s, m, &[], None, 16.2), Some((2, 18.0)));
        // Seeking to just before a segment and playing into it skips (the seek target is outside it).
        assert!(!keep_manual(&s, 8.0, 8.5));
        assert_eq!(skip_at(&s, m, &[], Some(8.0), 9.95), Some((1, 18.0)));
        // A seek that lands within the 0.1 s lead-in counts as inside.
        assert_eq!(skip_at(&s, m, &[], Some(9.95), 10.2), None);
    }

    #[test]
    fn next_skip_timing_highlight_and_tooltip() {
        let s = segs();
        let m = default_mask();
        assert!((until_next(&s, m, &[], 9.0, 1.0).unwrap() - 0.9).abs() < 1e-9);
        assert!((until_next(&s, m, &[], 9.0, 2.0).unwrap() - 0.45).abs() < 1e-9);
        assert!((until_next(&s, m, &[], 19.0, 1.0).unwrap() - 4.9).abs() < 1e-9);
        assert_eq!(until_next(&s, m, &[], 30.0, 1.0), None);
        assert_eq!(until_next(&s, m, &[], 9.0, 0.0), None);
        assert_eq!(highlight(&s), Some(32.0));
        assert_eq!(at(&s, 16.0), Some(Cat::Interaction));
        assert_eq!(at(&s, 20.0), None);
    }

    #[test]
    fn stored_choice_round_trips() {
        assert_eq!(mask_to_keys(default_mask()), "sponsor,selfpromo,interaction");
        assert_eq!(mask_from_keys("sponsor,intro,poi_highlight,bogus"), mask(&[Cat::Sponsor, Cat::Intro]));
        assert_eq!(mask_from_keys(""), 0);
        assert_eq!(mask_from_keys(&mask_to_keys(mask(&SKIPPABLE))), mask(&SKIPPABLE));
    }

    #[test]
    fn bar_marks_are_local_to_each_chapter_segment() {
        let s = segs();
        // Whole bar (no chapters), 40 s.
        let all = marks_html(&s, 40.0, 0.0, 1.0);
        assert!(all.contains("<i class=\"psb k0\" style=\"--a:0.25000;--z:0.40000\"></i>"), "{all}");
        assert!(all.contains("<i class=\"psbp k8\" style=\"--a:0.80000\"></i>"), "{all}");
        // A chapter segment 0.3..0.7: the sponsor is cut at its left edge, the highlight is outside it.
        let mid = marks_html(&s, 40.0, 0.3, 0.7);
        assert!(mid.starts_with("<i class=\"psb k0\" style=\"--a:0.00000;--z:0.25000\">"), "{mid}");
        assert!(!mid.contains("psbp"));
        assert!(marks_html(&s, 0.0, 0.0, 1.0).is_empty());
    }

    #[test]
    fn player_keeps_skip_feedback_and_bar_marks_without_settings() {
        use crate::player::ui;
        let sh = ui::shell();
        assert!(!sh.contains("ytc-sbb") && !sh.contains("data-arg=\"sb\""), "settings live in the browser toolbar");
        assert!(sh.contains("id=\"ytc-sbt\" class=\"psbt\" role=\"status\"") && sh.contains("data-act=\"p-sb\" data-arg=\"undo\" tabindex=\"-1\""));
        assert_eq!(ui::bars_html_with(&[], &|_, _| String::new()), ui::bars_html(&[]), "no marks: the plain bar");
        let s = segs();
        let one = ui::bars_html_with(&[], &|lo, hi| marks_html(&s, 40.0, lo, hi));
        assert!(one.starts_with("<div class=\"pbar\"><i class=\"pbuf\"></i><i class=\"phv\"></i><i class=\"ppos\"></i><i class=\"psb k3\""), "{one}");
        let two = ui::bars_html_with(&[(0.0, 0.5), (0.5, 1.0)], &|lo, hi| marks_html(&s, 40.0, lo, hi));
        assert_eq!(two.matches("class=\"psb ").count(), 4, "{two}");
        assert!(two.contains("id=\"ytc-sg1\"") && two.contains("<i class=\"psb k1\" style=\"--a:0.20000;--z:0.35000\">"), "{two}");
    }
}
