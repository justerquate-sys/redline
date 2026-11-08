//! The toolbar icon, painted here: Redline's mark with the status on it, as RGBA pixels. No canvas, no JavaScript: a
//! small even-odd scanline fill with 4×4 supersampling, so the result has unit tests.
//!
//! Idle or finished: the manifest's own icons (empty pixels). Work active: the mark above a progress line showing the
//! batch's verified saved fraction. Six seconds of success: a check. Attention: a dot. Helper offline: the grey mark.

use serde_json::Value;

/// `browser/icons/redline.svg`'s path (`viewBox 0 0 32 24`; a test keeps them equal).
pub const MARK: &str = "M12.54 2L27.54 2C30.03 2 31.23 4.01 30.22 6.5L25.78 17.5C24.77 19.99 21.95 22 19.46 22L4.46 22C1.97 22 0.77 19.99 1.78 17.5L6.22 6.5C7.23 4.01 10.05 2 12.54 2ZM12.44 6.64C12.09 6.29 12.21 6 12.7 6L16.5 6C17 6 17.69 6.29 18.04 6.64L22.64 11.24C23.06 11.66 23.06 12.34 22.64 12.76L18.04 17.36C17.69 17.71 17 18 16.5 18L12.7 18C12.21 18 12.09 17.71 12.44 17.36L17.29 12.51C17.57 12.23 17.57 11.77 17.29 11.49Z";

const RED: [f64; 3] = [255., 0., 51.];
const GREY: [f64; 3] = [113., 113., 113.];
const WHITE: [f64; 3] = [241., 241., 241.];
const TRACK: [f64; 3] = [113., 113., 113.];
const ERROR: [f64; 3] = [255., 154., 143.];
const INK: [f64; 3] = [15., 15., 15.];

/// Flattens the mark into closed polygons (in `0..32 × 0..24`).
fn polygons() -> Vec<Vec<(f64, f64)>> {
    let mut out = vec![];
    let mut poly: Vec<(f64, f64)> = vec![];
    let mut nums: Vec<f64> = vec![];
    let mut cmd = ' ';
    let flush = |cmd: char, nums: &mut Vec<f64>, poly: &mut Vec<(f64, f64)>| {
        match cmd {
            'M' | 'L' if nums.len() >= 2 => poly.push((nums[0], nums[1])),
            'C' if nums.len() >= 6 => {
                let (x0, y0) = *poly.last().unwrap_or(&(0., 0.));
                for i in 1..=8 {
                    let t = f64::from(i) / 8.;
                    let u = 1. - t;
                    let p = |a: f64, b: f64, c: f64, d: f64| u * u * u * a + 3. * u * u * t * b + 3. * u * t * t * c + t * t * t * d;
                    poly.push((p(x0, nums[0], nums[2], nums[4]), p(y0, nums[1], nums[3], nums[5])));
                }
            }
            _ => {}
        }
        nums.clear();
    };
    let mut word = String::new();
    for c in MARK.chars() {
        if c.is_ascii_alphabetic() {
            if !word.is_empty() {
                nums.push(word.parse().unwrap_or(0.));
                word.clear();
            }
            flush(cmd, &mut nums, &mut poly);
            if c == 'Z' {
                out.push(std::mem::take(&mut poly));
            }
            cmd = c;
        } else if c == ' ' {
            if !word.is_empty() {
                nums.push(word.parse().unwrap_or(0.));
                word.clear();
            }
        } else {
            word.push(c);
        }
    }
    out
}

fn inside(polys: &[Vec<(f64, f64)>], x: f64, y: f64) -> bool {
    let mut odd = false;
    for poly in polys {
        for i in 0..poly.len() {
            let (ax, ay) = poly[i];
            let (bx, by) = poly[(i + 1) % poly.len()];
            if (ay > y) != (by > y) && x < ax + (y - ay) / (by - ay) * (bx - ax) {
                odd = !odd;
            }
        }
    }
    odd
}

struct Canvas {
    size: usize,
    px: Vec<f64>,
}
impl Canvas {
    fn new(size: usize) -> Self {
        Canvas { size, px: vec![0.; size * size * 4] }
    }
    /// Source-over at coverage `a`.
    fn blend(&mut self, x: usize, y: usize, c: [f64; 3], a: f64) {
        if a <= 0. {
            return;
        }
        let i = (y * self.size + x) * 4;
        let da = self.px[i + 3] / 255.;
        let oa = a + da * (1. - a);
        for k in 0..3 {
            self.px[i + k] = (c[k] * a + self.px[i + k] * da * (1. - a)) / oa;
        }
        self.px[i + 3] = oa * 255.;
    }
    /// Paints where `f(x, y)` holds, `x`/`y` in the 32-unit design square.
    fn fill(&mut self, c: [f64; 3], f: impl Fn(f64, f64) -> bool) {
        let scale = 32. / self.size as f64;
        for y in 0..self.size {
            for x in 0..self.size {
                let mut hits = 0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let px = (x as f64 + (f64::from(sx) + 0.5) / 4.) * scale;
                        let py = (y as f64 + (f64::from(sy) + 0.5) / 4.) * scale;
                        if f(px, py) {
                            hits += 1;
                        }
                    }
                }
                self.blend(x, y, c, f64::from(hits) / 16.);
            }
        }
    }
    fn bytes(&self) -> Vec<u8> {
        self.px.iter().map(|v| v.round().clamp(0., 255.) as u8).collect()
    }
}

fn segment_distance(px: f64, py: f64, a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((px - a.0) * dx + (py - a.1) * dy) / (dx * dx + dy * dy)).clamp(0., 1.);
    ((px - a.0 - t * dx).powi(2) + (py - a.1 - t * dy).powi(2)).sqrt()
}

fn paint(size: usize, color: [f64; 3], progress: Option<f64>, dot: Option<[f64; 3]>, check: bool) -> Vec<u8> {
    let polys = polygons();
    let mut c = Canvas::new(size);
    // The mark leaves the bottom band of the square free (art 2..22 of 24, centered).
    c.fill(color, |x, y| inside(&polys, x, y - 2.));
    if let Some(ratio) = progress {
        let ratio = ratio.clamp(0., 1.);
        c.fill(TRACK, |x, y| (28.0..31.0).contains(&y) && (2.0..30.0).contains(&x));
        if ratio > 0. {
            c.fill(WHITE, |x, y| (28.0..31.0).contains(&y) && x >= 2. && x < 2. + 28. * ratio);
        }
    }
    let (bx, by, br) = (24.5, 24.5, 7.5);
    if let Some(d) = dot {
        c.fill(INK, |x, y| ((x - bx).powi(2) + (y - by).powi(2)).sqrt() <= br);
        c.fill(d, |x, y| ((x - bx).powi(2) + (y - by).powi(2)).sqrt() <= br - 2.);
    }
    if check {
        c.fill(WHITE, |x, y| ((x - bx).powi(2) + (y - by).powi(2)).sqrt() <= br);
        c.fill(INK, |x, y| segment_distance(x, y, (20.5, 24.5), (23.5, 27.5)) <= 1.25 || segment_distance(x, y, (23.5, 27.5), (28.5, 21.5)) <= 1.25);
    }
    c.bytes()
}

/// Pixels for 16 and 32 px, or two empty vectors for the manifest's own icons.
pub fn render(view: &Value, check: bool) -> (Vec<u8>, Vec<u8>) {
    let kind = view["kind"].as_str().unwrap_or("idle");
    let (total, saved, issues) = (view["total"].as_f64().unwrap_or(0.), view["saved"].as_f64().unwrap_or(0.), view["issues"].as_f64().unwrap_or(0.));
    let busy = matches!(kind, "processing" | "queued");
    let offline = kind == "offline";
    let attention = issues > 0. && !offline;
    if !busy && !offline && !attention && !check {
        return (vec![], vec![]);
    }
    let progress = busy.then(|| if total > 0. { saved / total } else { 0. });
    let color = if offline { GREY } else { RED };
    let dot = attention.then_some(ERROR);
    (paint(16, color, progress, dot, check), paint(32, color, progress, dot, check))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_path_is_the_marks_one_source() {
        assert!(include_str!("../../browser/icons/redline.svg").contains(MARK));
    }
    fn at(px: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * size + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }
    #[test]
    fn idle_and_finished_use_the_manifest_icons() {
        for kind in ["idle", "complete", "settled", "unknown", "loading"] {
            assert!(render(&json!({"kind":kind}), false).0.is_empty(), "{kind}");
        }
    }
    #[test]
    fn the_mark_is_red_with_its_chevron_cut_out() {
        let (_, px) = render(&json!({"kind":"queued","total":0,"saved":0}), false);
        assert_eq!(px.len(), 32 * 32 * 4);
        let body = at(&px, 32, 8, 16);
        assert!(body[0] > 200 && body[3] > 200, "{body:?}");
        let chevron = at(&px, 32, 18, 14);
        assert!(chevron[3] < 60, "{chevron:?}");
    }
    #[test]
    fn progress_is_the_exact_saved_fraction() {
        let (_, half) = render(&json!({"kind":"processing","total":4,"saved":2}), false);
        assert!(at(&half, 32, 10, 29)[0] > 200, "saved part is white");
        assert!(at(&half, 32, 24, 29)[0] < 150, "the rest is the track");
        let (_, none) = render(&json!({"kind":"processing","total":4,"saved":0}), false);
        assert!(at(&none, 32, 4, 29)[0] < 150);
        let (_, full) = render(&json!({"kind":"processing","total":4,"saved":4}), false);
        assert!(at(&full, 32, 28, 29)[0] > 200);
    }
    #[test]
    fn offline_is_grey_and_attention_keeps_its_dot() {
        let (_, off) = render(&json!({"kind":"offline"}), false);
        let p = at(&off, 32, 8, 16);
        assert_eq!(p[0], p[1]);
        let (_, bad) = render(&json!({"kind":"attention","issues":2}), false);
        let dot = at(&bad, 32, 24, 24);
        assert!(dot[1] > 100, "{dot:?}");
        let (_, busy) = render(&json!({"kind":"processing","total":2,"saved":1,"issues":1}), false);
        assert!(at(&busy, 32, 24, 24)[1] > 100, "active work keeps the attention dot");
    }
    #[test]
    fn success_is_a_check_for_its_window_only() {
        let (_, on) = render(&json!({"kind":"complete","total":1,"saved":1}), true);
        assert!(at(&on, 32, 30, 24)[0] > 200);
        assert!(render(&json!({"kind":"complete","total":1,"saved":1}), false).0.is_empty());
    }
}
