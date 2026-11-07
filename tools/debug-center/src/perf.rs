//! Speed telemetry views: `GET /perf` (dashboard) and `GET /perf.json` (raw event rows).
//! The incidents file is streamed line by line; telemetry batches are the lines whose report
//! is `kind: "perf"` with `schema: 2` (`src/telemetry.rs` of the client), each carrying `events`
//! (`{m, v, at, …breakdown}`). Lines received before the time window are skipped unparsed.

use crate::json::{self, J};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const TOP: usize = 20;
const VERSIONS_SHOWN: usize = 8;

pub fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

/// Query string → (key, value) pairs (percent-decoded, `+` = space).
pub fn query(path: &str) -> Vec<(String, String)> {
    let q = path.split_once('?').map(|x| x.1).unwrap_or("");
    q.split('&').filter(|p| !p.is_empty()).map(|p| {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        (decode(k), decode(v))
    }).collect()
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(x) => {
                        out.push(x);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `since`: epoch ms (≥ 1e12), a duration ago in ms, or `30m` / `6h` / `7d`; "" or `all` = everything.
pub fn since(s: &str, now: f64) -> f64 {
    let s = s.trim();
    if s.is_empty() || s == "all" {
        return 0.0;
    }
    let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len()));
    let Ok(n) = num.parse::<f64>() else { return 0.0 };
    let ms = match unit {
        "" if n >= 1e12 => return n,
        "" | "ms" => n,
        "s" => n * 1e3,
        "m" => n * 60e3,
        "h" => n * 3600e3,
        "d" => n * 86400e3,
        _ => return 0.0,
    };
    now - ms
}

/// `{"received":N,` at the start of a line (no parse).
fn received(line: &str) -> Option<f64> {
    let rest = line.strip_prefix("{\"received\":")?;
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

fn is_batch(line: &str) -> bool {
    line.contains("\"schema\":2") && line.contains("\"kind\":\"perf\"")
}

/// Where a report came from: `tv` (the TV app's shell stamps it) or `desktop` (the extension).
pub fn platform_of(r: &J) -> &str {
    r.s("platform").unwrap_or("desktop")
}

/// The `platform` filter of a view: `tv`, `desktop` or "" (both).
pub fn platform_matches(r: &J, want: &str) -> bool {
    want.is_empty() || platform_of(r) == want
}

fn version_of(r: &J) -> String {
    r.s("build").map(str::to_string).unwrap_or_else(|| format!("{} (no build id)", r.s("client").unwrap_or("?")))
}

/// A metric name matches `want`: exactly, or as a prefix when `want` ends in `*` or `.`.
pub fn metric_matches(m: &str, want: &str) -> bool {
    if want.is_empty() {
        return true;
    }
    match want.strip_suffix('*') {
        Some(p) => m.starts_with(p),
        None if want.ends_with('.') => m.starts_with(want),
        None => m == want,
    }
}

pub fn unit(m: &str) -> &'static str {
    match m {
        "lag.cls" => "score",
        "video.drops" => "%",
        "video.session" => "ms (life)",
        _ => "ms",
    }
}

/// The row for `/perf.json` and the slowest lists: the event plus where it came from.
fn row(ev: &J, received: f64, version: &str, r: &J) -> String {
    let mut s = String::from("{");
    let _ = write!(s, "\"received\":{received:.0},\"version\":");
    json::quote(&mut s, version);
    s.push_str(",\"platform\":");
    json::quote(&mut s, platform_of(r));
    if let Some(p) = r.s("page") {
        s.push_str(",\"page\":");
        json::quote(&mut s, p);
    }
    if let J::Obj(o) = ev {
        for (k, v) in o {
            s.push(',');
            json::quote(&mut s, k);
            s.push(':');
            v.write(&mut s);
        }
    }
    s.push('}');
    s
}

/// One line of the file, as `scan` hands it on.
enum Line<'a> {
    /// A telemetry batch of the asked version.
    Batch { r: &'a J, rec: f64, v: &'a str },
    /// A telemetry batch of another version (only listed).
    Other { rec: f64, v: &'a str },
    /// An incident (any version).
    Incident { r: &'a J, rec: f64, v: &'a str },
}

/// Streams the file: every line in the window of the `platform` ("" = both), parsed. Returns (lines, bytes).
fn scan(file: &Path, from: f64, version: &str, platform: &str, mut f: impl FnMut(Line)) -> (usize, u64) {
    let Ok(fh) = File::open(file) else { return (0, 0) };
    let mut rd = BufReader::with_capacity(1 << 20, fh);
    let mut line = String::new();
    let (mut lines, mut bytes) = (0usize, 0u64);
    loop {
        line.clear();
        match rd.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes += n as u64,
        }
        lines += 1;
        let l = line.trim_end();
        if received(l).is_some_and(|r| r < from) {
            continue;
        }
        let Some(j) = json::parse(l) else { continue };
        let rec = j.n("received").unwrap_or(0.0);
        let Some(r) = j.get("report") else { continue };
        if !platform_matches(r, platform) {
            continue;
        }
        let v = version_of(r);
        if is_batch(l) && r.n("schema") == Some(2.0) {
            if !version.is_empty() && v != version {
                f(Line::Other { rec, v: &v });
            } else {
                f(Line::Batch { r, rec, v: &v });
            }
        } else if rec >= from {
            f(Line::Incident { r, rec, v: &v });
        }
    }
    (lines, bytes)
}

/// Every telemetry batch of `platform` ("" = both) received since `from`, with its version.
pub fn scan_batches(file: &Path, from: f64, platform: &str, mut f: impl FnMut(&J, &str)) {
    scan(file, from, "", platform, |line| {
        if let Line::Batch { r, v, .. } = line {
            f(r, v);
        }
    });
}

/// `GET /perf.json?metric=<name|prefix*>&since=<ms|6h>&version=<v>&platform=<tv|desktop>&limit=<n>`: a JSON
/// array of event rows (oldest first): `{received, version, platform, page, m, v, at, …breakdown}`.
pub fn rows_json(file: &Path, q: &[(String, String)]) -> String {
    let get = |k: &str| q.iter().find(|x| x.0 == k).map(|x| x.1.as_str()).unwrap_or("");
    let now = now_ms();
    let from = since(get("since"), now);
    let metric = get("metric").to_string();
    let limit: usize = get("limit").parse().unwrap_or(10_000).min(500_000);
    let version = get("version").to_string();
    let mut rows: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    scan(file, from, &version, get("platform"), |line| {
        let Line::Batch { r, rec, v } = line else { return };
        for ev in r.get("events").map(J::arr).unwrap_or(&[]) {
            let m = ev.s("m").unwrap_or("");
            if !metric_matches(m, &metric) || ev.n("at").unwrap_or(rec) < from {
                continue;
            }
            rows.push_back(row(ev, rec, v, r));
            if rows.len() > limit {
                rows.pop_front();
            }
        }
    });
    let mut out = String::with_capacity(rows.iter().map(|r| r.len() + 2).sum::<usize>() + 4);
    out.push_str("[\n");
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str(r);
    }
    out.push_str("\n]\n");
    out
}

#[derive(Default)]
struct Agg {
    vals: Vec<f32>,
    /// The slowest events: (value, row).
    top: Vec<(f32, String)>,
}

#[derive(Default)]
struct Ver {
    first: f64,
    last: f64,
    batches: u64,
    events: u64,
    navs: u64,
    starts: u64,
    overhead: f64,
    js_overhead: f64,
    dropped: f64,
    incidents: HashMap<String, u64>,
    metrics: HashMap<String, Vec<f32>>,
}

#[derive(Clone, Copy)]
pub struct Stats {
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub max: f64,
    pub n: usize,
}

pub fn stats(v: &mut [f32]) -> Option<Stats> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable_by(f32::total_cmp);
    let q = |p: f64| v[(((v.len() - 1) as f64) * p).round() as usize] as f64;
    Some(Stats { p50: q(0.5), p90: q(0.9), p99: q(0.99), max: v[v.len() - 1] as f64, n: v.len() })
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn fmt(v: f64, unit: &str) -> String {
    match unit {
        "score" => format!("{v:.3}"),
        "%" => format!("{v:.1}"),
        _ if v >= 100.0 => format!("{v:.0}"),
        _ => format!("{v:.1}"),
    }
}

fn anchor(m: &str) -> String {
    m.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// `GET /perf?since=<6h|7d|all|ms>&version=<v>&metric=<prefix*>`: the dashboard.
pub fn dashboard(file: &Path, q: &[(String, String)]) -> String {
    let t_start = std::time::Instant::now();
    let get = |k: &str| q.iter().find(|x| x.0 == k).map(|x| x.1.as_str()).unwrap_or("");
    let now = now_ms();
    let since_s = if q.iter().any(|x| x.0 == "since") { get("since").to_string() } else { "7d".to_string() };
    let from = since(&since_s, now);
    let version = get("version").to_string();
    let metric = get("metric").to_string();
    let platform = get("platform").to_string();
    let mut aggs: HashMap<String, Agg> = HashMap::new();
    let mut vers: HashMap<String, Ver> = HashMap::new();
    let mut seen_versions: HashMap<String, f64> = HashMap::new();
    let (mut batches, mut events) = (0u64, 0u64);
    let mut acts = crate::timeline::Acts::default();
    let (lines, bytes) = scan(file, from, &version, &platform, |line| {
        let (rec, v) = match &line {
            Line::Batch { rec, v, .. } | Line::Other { rec, v } | Line::Incident { rec, v, .. } => (*rec, *v),
        };
        let last = seen_versions.entry(v.to_string()).or_insert(0.0);
        *last = last.max(rec);
        match line {
            Line::Other { .. } => {}
            Line::Incident { r, .. } => {
                if !version.is_empty() && v != version {
                    return;
                }
                let kind = r.s("kind").unwrap_or("?").to_string();
                if kind == "perf" {
                    return; // the older 10-minute summaries
                }
                let ver = vers.entry(v.to_string()).or_default();
                *ver.incidents.entry(kind).or_default() += 1;
                if ver.first == 0.0 || rec < ver.first {
                    ver.first = rec;
                }
                ver.last = ver.last.max(rec);
            }
            Line::Batch { r, .. } => {
                batches += 1;
                let ver = vers.entry(v.to_string()).or_default();
                if ver.first == 0.0 || rec < ver.first {
                    ver.first = rec;
                }
                ver.last = ver.last.max(rec);
                ver.batches += 1;
                ver.overhead += r.n("overhead_ms").unwrap_or(0.0);
                ver.js_overhead += r.n("js_overhead_ms").unwrap_or(0.0);
                ver.dropped += r.n("dropped").unwrap_or(0.0);
                for ev in r.get("events").map(J::arr).unwrap_or(&[]) {
                    let (Some(m), Some(val)) = (ev.s("m"), ev.n("v")) else { continue };
                    if ev.n("at").unwrap_or(rec) < from {
                        continue;
                    }
                    ver.events += 1;
                    acts.add(ev, r.s("page").unwrap_or(""));
                    if m.starts_with("nav.") && m != "nav.fresh" {
                        ver.navs += 1;
                    }
                    if m == "video.start" {
                        ver.starts += 1;
                    }
                    if !metric_matches(m, &metric) {
                        continue;
                    }
                    events += 1;
                    ver.metrics.entry(m.to_string()).or_default().push(val as f32);
                    let a = aggs.entry(m.to_string()).or_default();
                    a.vals.push(val as f32);
                    let v32 = val as f32;
                    if a.top.len() < TOP || a.top.last().is_some_and(|t| v32 > t.0) {
                        let pos = a.top.iter().position(|t| v32 > t.0).unwrap_or(a.top.len());
                        a.top.insert(pos, (v32, row(ev, rec, v, r)));
                        a.top.truncate(TOP);
                    }
                }
            }
        }
    });
    let mut names: Vec<String> = aggs.keys().cloned().collect();
    names.sort();
    let mut order: Vec<(String, f64)> = vers.iter().map(|(k, v)| (k.clone(), v.last)).collect();
    order.sort_by(|a, b| a.1.total_cmp(&b.1));
    let shown: Vec<String> = order.iter().rev().take(VERSIONS_SHOWN).rev().map(|x| x.0.clone()).collect();

    let mut h = String::with_capacity(64 * 1024);
    h.push_str("<!doctype html><meta charset=utf-8><title>Redline speed</title><style>\
body{background:#111;color:#ddd;font:13px ui-monospace,monospace;margin:20px}h1,h2{font:600 18px system-ui;color:#b7ebca;margin:22px 0 8px}h2{font-size:15px}\
a{color:#9cc8ff}table{border-collapse:collapse;margin:6px 0}td,th{padding:3px 10px;border-bottom:1px solid #2a2a2a;text-align:right;white-space:nowrap}\
th{color:#999;font-weight:500}td:first-child,th:first-child{text-align:left}tr:hover td{background:#1b1b1b}\
.bar{display:inline-block;height:8px;background:#4a7;border-radius:2px;vertical-align:middle;margin-left:6px}\
form{display:flex;gap:10px;align-items:center;flex-wrap:wrap}select,input,button{background:#1d1d1d;color:#ddd;border:1px solid #333;border-radius:6px;padding:4px 8px;font:inherit}\
details{margin:4px 0}summary{cursor:pointer;color:#b7ebca}pre{white-space:pre-wrap;word-break:break-all;background:#1a1a1a;padding:8px;border-radius:8px;margin:4px 0}.dim{color:#777}</style>");
    let _ = write!(h, "<h1>Redline speed telemetry</h1><form method=get action=/perf><label>Window <select name=since>");
    for (v, label) in [("1h", "last hour"), ("6h", "6 hours"), ("24h", "24 hours"), ("7d", "7 days"), ("30d", "30 days"), ("all", "everything")] {
        let _ = write!(h, "<option value={v}{}>{label}</option>", if since_s == v { " selected" } else { "" });
    }
    h.push_str("</select></label><label>Platform <select name=platform>");
    for (v, label) in [("", "TV and desktop"), ("desktop", "desktop"), ("tv", "TV")] {
        let _ = write!(h, "<option value=\"{v}\"{}>{label}</option>", if platform == v { " selected" } else { "" });
    }
    h.push_str("</select></label><label>Version <select name=version><option value=\"\">all</option>");
    let mut sv: Vec<(&String, &f64)> = seen_versions.iter().collect();
    sv.sort_by(|a, b| b.1.total_cmp(a.1));
    for (v, _) in sv {
        let _ = write!(h, "<option value=\"{0}\"{1}>{0}</option>", esc(v), if *v == version { " selected" } else { "" });
    }
    let _ = write!(h, "</select></label><label>Metric <input name=metric size=14 placeholder=\"nav.* or video.start\" value=\"{}\"></label><button>Show</button> <a href=/>incidents</a> <a href=#interactions>interactions</a> <a href=#hedges>hedges</a> <a href=/timeline>timeline</a></form>", esc(&metric));
    let _ = write!(
        h,
        "<p class=dim>{events} events in {batches} batches · {lines} lines ({:.1} MB) read in {} ms · raw rows: <a href=\"/perf.json?since={}&metric={}&version={}&platform={}\">/perf.json</a></p>",
        bytes as f64 / 1e6,
        t_start.elapsed().as_millis(),
        esc(&since_s),
        esc(&metric),
        esc(&version),
        esc(&platform)
    );

    // Per metric.
    h.push_str("<h2>Per metric</h2><table><tr><th>metric</th><th>unit</th><th>count</th><th>p50</th><th>p90</th><th>p99</th><th>max</th><th></th></tr>");
    let mut all_stats: Vec<(String, Stats)> = Vec::new();
    for m in &names {
        let a = aggs.get_mut(m).expect("named");
        if let Some(s) = stats(&mut a.vals) {
            all_stats.push((m.clone(), s));
        }
    }
    let worst = all_stats.iter().filter(|(m, _)| unit(m) == "ms").map(|(_, s)| s.p90).fold(1.0, f64::max);
    for (m, s) in &all_stats {
        let u = unit(m);
        let w = if u == "ms" { (s.p90 / worst * 120.0).clamp(1.0, 120.0) } else { 0.0 };
        let _ = write!(
            h,
            "<tr><td><a href=\"#{}\">{}</a></td><td class=dim>{u}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td style=text-align:left>{}</td></tr>",
            anchor(m),
            esc(m),
            s.n,
            fmt(s.p50, u),
            fmt(s.p90, u),
            fmt(s.p99, u),
            fmt(s.max, u),
            if w > 0.0 { format!("<span class=bar style=width:{w:.0}px title=\"p90\"></span>") } else { String::new() }
        );
    }
    h.push_str("</table>");
    acts.html(&mut h);

    // Trend per version.
    if !shown.is_empty() {
        h.push_str("<h2>Trend per version (p50 · count)</h2><table><tr><th>metric</th>");
        for v in &shown {
            let _ = write!(h, "<th title=\"{0}\">{1}</th>", esc(v), esc(v.split(" (").nth(1).map(|x| x.trim_end_matches(')')).map(|x| &x[..x.len().min(8)]).unwrap_or(v)));
        }
        h.push_str("</tr>");
        for m in &names {
            let _ = write!(h, "<tr><td>{}</td>", esc(m));
            let mut prev: Option<f64> = None;
            for v in &shown {
                let cell = vers.get_mut(v).and_then(|x| x.metrics.get_mut(m)).and_then(|vals| stats(vals));
                match cell {
                    Some(s) => {
                        let arrow = match prev {
                            Some(p) if s.p50 > p * 1.2 && s.p50 - p > 5.0 => " <span style=color:#e88>▲</span>",
                            Some(p) if s.p50 < p * 0.8 && p - s.p50 > 5.0 => " <span style=color:#7c7>▼</span>",
                            _ => "",
                        };
                        let _ = write!(h, "<td>{}{arrow} <span class=dim>· {}</span></td>", fmt(s.p50, unit(m)), s.n);
                        prev = Some(s.p50);
                    }
                    None => h.push_str("<td class=dim>–</td>"),
                }
            }
            h.push_str("</tr>");
        }
        h.push_str("</table>");
    }

    // Errors and incidents per version.
    h.push_str("<h2>Incidents and errors per version</h2><table><tr><th>version</th><th>first</th><th>last</th><th>batches</th><th>events</th><th>navigations</th><th>video starts</th><th>incidents</th><th>per 100 navigations</th><th>by kind</th><th>overhead ms/batch (Rust + observers)</th><th>dropped</th></tr>");
    for (v, _) in order.iter().rev() {
        let x = &vers[v];
        let inc: u64 = x.incidents.values().sum();
        let mut kinds: Vec<(&String, &u64)> = x.incidents.iter().collect();
        kinds.sort_by(|a, b| b.1.cmp(a.1));
        let kinds = kinds.iter().take(5).map(|(k, n)| format!("{} {n}", esc(k))).collect::<Vec<_>>().join(", ");
        let per = if x.navs > 0 { format!("{:.2}", inc as f64 * 100.0 / x.navs as f64) } else { "–".into() };
        let ov = if x.batches > 0 { format!("{:.2} + {:.2}", x.overhead / x.batches as f64, x.js_overhead / x.batches as f64) } else { "–".into() };
        let _ = write!(
            h,
            "<tr><td>{}</td><td class=dim>{}</td><td class=dim>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{inc}</td><td>{per}</td><td style=text-align:left>{kinds}</td><td>{ov}</td><td>{}</td></tr>",
            esc(v),
            ago(now, x.first),
            ago(now, x.last),
            x.batches,
            x.events,
            x.navs,
            x.starts,
            x.dropped
        );
    }
    h.push_str("</table>");

    // The slowest events.
    let _ = write!(h, "<h2>Slowest {TOP} per metric (full breakdown)</h2>");
    for m in &names {
        let a = &aggs[m];
        let _ = write!(h, "<details id=\"{}\"><summary>{} · max {}</summary>", anchor(m), esc(m), a.top.first().map(|t| fmt(t.0 as f64, unit(m))).unwrap_or_default());
        for (_, r) in &a.top {
            let _ = write!(h, "<pre>{}</pre>", esc(r));
        }
        h.push_str("</details>");
    }
    let _ = write!(h, "<p class=dim>Rendered in {} ms.</p>", t_start.elapsed().as_millis());
    h
}

fn ago(now: f64, t: f64) -> String {
    if t <= 0.0 {
        return "–".into();
    }
    let s = (now - t) / 1000.0;
    if s < 3600.0 {
        format!("{:.0} min ago", s / 60.0)
    } else if s < 86400.0 * 2.0 {
        format!("{:.1} h ago", s / 3600.0)
    } else {
        format!("{:.0} d ago", s / 86400.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_and_window() {
        let q = query("/perf.json?metric=nav.*&since=6h&version=0.8.0+%28abc%29");
        assert_eq!(q[0], ("metric".into(), "nav.*".into()));
        assert_eq!(q[2].1, "0.8.0 (abc)");
        assert_eq!(since("6h", 10_000_000_000_000.0), 10_000_000_000_000.0 - 6.0 * 3600e3);
        assert_eq!(since("1700000000000", 0.0), 1_700_000_000_000.0);
        assert_eq!(since("60000", 1_000_000.0), 940_000.0);
        assert_eq!(since("all", 5.0), 0.0);
        assert!(metric_matches("nav.watch", "nav.*") && metric_matches("nav.watch", "nav.") && metric_matches("nav.watch", "nav.watch") && !metric_matches("nav.watch", "nav"));
        assert_eq!(received("{\"received\":123,\"report\":{}}"), Some(123.0));
    }

    #[test]
    fn dashboard_and_rows() {
        let dir = std::env::temp_dir().join(format!("ytc-dc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("i.ndjson");
        let now = now_ms();
        let mut s = String::new();
        for i in 0..50 {
            let at = now - 1000.0 * (50 - i) as f64;
            s.push_str(&format!("{{\"received\":{at:.0},\"report\":{{\"build\":\"0.8.0 (aaaa)\",\"kind\":\"perf\",\"schema\":2,\"page\":\"p1\",\"overhead_ms\":0.5,\"events\":[{{\"m\":\"nav.watch\",\"v\":{},\"at\":{at:.0},\"src\":\"cache\"}},{{\"m\":\"video.start\",\"v\":{},\"at\":{at:.0},\"vid\":\"abc\"}}]}}}}\n", 10 + i, 100 + i * 10));
        }
        s.push_str(&format!("{{\"received\":{now:.0},\"report\":{{\"build\":\"0.8.0 (aaaa)\",\"kind\":\"player-failed\",\"reason\":\"x\"}}}}\n"));
        s.push_str("not json\n");
        std::fs::write(&f, s).unwrap();
        let rows = rows_json(&f, &query("/perf.json?metric=video.start&since=1h"));
        let j = json::parse(&rows).expect("valid json");
        assert_eq!(j.arr().len(), 50);
        assert_eq!(j.arr()[49].n("v"), Some(590.0));
        assert_eq!(j.arr()[0].s("version"), Some("0.8.0 (aaaa)"));
        assert_eq!(json::parse(&rows_json(&f, &query("/perf.json?metric=nav.*&version=other"))).map(|j| j.arr().len()), Some(0));
        assert_eq!(json::parse(&rows_json(&f, &query("/perf.json?metric=nav.*&limit=5"))).map(|j| j.arr().len()), Some(5));
        let page = dashboard(&f, &query("/perf"));
        assert!(page.contains("nav.watch") && page.contains("video.start"));
        // Desktop and TV apart: the TV app's reports carry "platform":"tv".
        assert_eq!(j.arr()[0].s("platform"), Some("desktop"));
        let mut s = std::fs::read_to_string(&f).unwrap();
        s.push_str(&format!("{{\"received\":{now:.0},\"report\":{{\"build\":\"0.10.0 (b) (TV 0.2.0, builtin, telemetry)\",\"platform\":\"tv\",\"kind\":\"perf\",\"schema\":2,\"page\":\"tv1\",\"events\":[{{\"m\":\"ui.act\",\"v\":33,\"at\":{now:.0},\"act\":\"key.ArrowRight\"}}]}}}}\n"));
        std::fs::write(&f, s).unwrap();
        let tv = json::parse(&rows_json(&f, &query("/perf.json?platform=tv"))).unwrap();
        assert_eq!((tv.arr().len(), tv.arr()[0].s("platform"), tv.arr()[0].s("page")), (1, Some("tv"), Some("tv1")));
        assert_eq!(json::parse(&rows_json(&f, &query("/perf.json?platform=desktop&metric=ui.act"))).map(|j| j.arr().len()), Some(0));
        assert_eq!(json::parse(&rows_json(&f, &query("/perf.json?metric=ui.act"))).map(|j| j.arr().len()), Some(1));
        let tvp = dashboard(&f, &query("/perf?platform=tv"));
        assert!(tvp.contains(">ui.act</a>") && !tvp.contains(">video.start</a>") && tvp.contains("<option value=\"tv\" selected>"));
        assert!(!dashboard(&f, &query("/perf?platform=desktop")).contains("TV 0.2.0"));
        assert!(page.contains("player-failed 1"), "incident counted");
        assert!(page.contains("<td>590</td>"), "max of video.start");
        std::fs::remove_dir_all(&dir).ok();
    }
}
