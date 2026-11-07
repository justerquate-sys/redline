//! Interaction views: the `ui.act` summary and the hedged-request rates for the `/perf` dashboard,
//! and `GET /timeline` (the documents seen, then one document's every event in order with ms).
//! A document is one tab's page load (`page` of its batches): its interactions, navigations,
//! requests and long frames, oldest first.

use crate::json::{self, J};
use crate::perf::{self, Stats};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::Path;

const SLOW: usize = 25;
const PAGES: usize = 60;
/// Events shown for one document.
const EVENTS_MAX: usize = 5000;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn ms(v: f64) -> String {
    if v >= 100.0 { format!("{v:.0}") } else { format!("{v:.1}") }
}

fn cell(s: Option<Stats>, f: impl Fn(&Stats) -> f64) -> String {
    s.map(|s| ms(f(&s))).unwrap_or_else(|| "–".into())
}

#[derive(Default)]
struct ActAgg {
    paint: Vec<f32>,
    content: Vec<f32>,
    handler: Vec<f32>,
    dom: Vec<f32>,
    inputs: HashMap<String, u32>,
    waits: HashMap<String, u32>,
}

#[derive(Default)]
struct HedgeAgg {
    n: u32,
    fired: u32,
    won: u32,
    ms: Vec<f32>,
    fired_ms: Vec<f32>,
}

/// What the dashboard collects from the events it reads (any metric filter).
#[derive(Default)]
pub struct Acts {
    acts: HashMap<String, ActAgg>,
    hedges: HashMap<String, HedgeAgg>,
    /// The slowest interactions: (ms, received, page, event json).
    slow: Vec<(f32, String, String)>,
}

/// An interaction's user-visible time: its content when it navigated, else its next paint.
pub fn act_ms(ev: &J) -> Option<f64> {
    ev.n("content_ms").or_else(|| ev.n("v"))
}

fn mix(m: &HashMap<String, u32>) -> String {
    let mut v: Vec<(&String, &u32)> = m.iter().collect();
    v.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    v.iter().take(4).map(|(k, n)| format!("{} {n}", esc(k))).collect::<Vec<_>>().join(", ")
}

impl Acts {
    pub fn add(&mut self, ev: &J, page: &str) {
        let m = ev.s("m").unwrap_or("");
        if m == "ui.act" {
            let name = ev.s("act").unwrap_or("?").to_string();
            let a = self.acts.entry(name).or_default();
            if let Some(v) = ev.n("v") {
                a.paint.push(v as f32);
            }
            if let Some(v) = ev.n("content_ms") {
                a.content.push(v as f32);
            }
            if let Some(v) = ev.n("handler_ms") {
                a.handler.push(v as f32);
            }
            if let Some(v) = ev.n("dom_ms") {
                a.dom.push(v as f32);
            }
            *a.inputs.entry(ev.s("input").unwrap_or("?").to_string()).or_default() += 1;
            *a.waits.entry(ev.s("wait").unwrap_or("none").to_string()).or_default() += 1;
            if let Some(t) = act_ms(ev) {
                let t = t as f32;
                if self.slow.len() < SLOW || self.slow.last().is_some_and(|x| t > x.0) {
                    let mut s = String::new();
                    ev.write(&mut s);
                    let pos = self.slow.iter().position(|x| t > x.0).unwrap_or(self.slow.len());
                    self.slow.insert(pos, (t, page.to_string(), s));
                    self.slow.truncate(SLOW);
                }
            }
        } else if let Some(ep) = m.strip_prefix("req.") {
            let h = self.hedges.entry(ep.to_string()).or_default();
            h.n += 1;
            let v = ev.n("v").unwrap_or(0.0) as f32;
            h.ms.push(v);
            if ev.get("hedge_fired") == Some(&J::Bool(true)) {
                h.fired += 1;
                h.fired_ms.push(v);
                if ev.get("hedge_won") == Some(&J::Bool(true)) {
                    h.won += 1;
                }
            }
        }
    }

    /// The dashboard's sections.
    pub fn html(&mut self, h: &mut String) {
        h.push_str("<h2 id=interactions>Interactions (ui.act)</h2><p class=dim>Every click, handled key, wheel choice and hover activation. \
<b>paint</b>: input → the next frame after its handler; <b>content</b>: input → the page it opened was visible (navigations); \
handler: input → the app's handler; dom: input → the DOM written. <a href=/timeline>timeline per document</a></p>");
        h.push_str("<table><tr><th>action</th><th>count</th><th>p50</th><th>p90</th><th>max</th><th>content p50</th><th>content p90</th><th>content max</th><th>handler p50</th><th>dom p50</th><th>input</th><th>waited on</th></tr>");
        let mut names: Vec<String> = self.acts.keys().cloned().collect();
        names.sort_by(|a, b| self.acts[b].paint.len().cmp(&self.acts[a].paint.len()).then(a.cmp(b)));
        for n in &names {
            let a = self.acts.get_mut(n).expect("named");
            let count = a.paint.len();
            let p = perf::stats(&mut a.paint);
            let c = perf::stats(&mut a.content);
            let hd = perf::stats(&mut a.handler);
            let d = perf::stats(&mut a.dom);
            let _ = write!(
                h,
                "<tr><td>{}</td><td>{count}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td style=text-align:left>{}</td><td style=text-align:left>{}</td></tr>",
                esc(n),
                cell(p, |s| s.p50),
                cell(p, |s| s.p90),
                cell(p, |s| s.max),
                cell(c, |s| s.p50),
                cell(c, |s| s.p90),
                cell(c, |s| s.max),
                cell(hd, |s| s.p50),
                cell(d, |s| s.p50),
                mix(&a.inputs),
                mix(&a.waits)
            );
        }
        h.push_str("</table>");
        let _ = write!(h, "<h2>Slowest {SLOW} interactions (content, else paint)</h2><table><tr><th>ms</th><th>action</th><th>route</th><th>input</th><th>waited on</th><th>when</th><th></th></tr>");
        for (t, page, ev) in &self.slow {
            let j = json::parse(ev).unwrap_or(J::Null);
            let _ = write!(
                h,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=dim>{}</td><td style=text-align:left><a href=\"/timeline?page={}#t{}\">timeline</a> <span class=dim>{}</span></td></tr>",
                ms(*t as f64),
                esc(j.s("act").unwrap_or("?")),
                esc(j.s("route").unwrap_or("")),
                esc(j.s("input").unwrap_or("")),
                esc(j.s("wait").unwrap_or("")),
                j.n("at").map(clock).unwrap_or_default(),
                esc(page),
                j.n("at").unwrap_or(0.0) as u64,
                esc(ev)
            );
        }
        h.push_str("</table>");
        h.push_str("<h2 id=hedges>Hedged requests</h2><p class=dim>A read request that has not answered by its deadline (≈ the session's p90) is sent again; the first answer wins and the other is aborted. \
fired: share of requests that needed the second copy; won: share of those the second copy answered first.</p>");
        h.push_str("<table><tr><th>endpoint</th><th>requests</th><th>p50</th><th>p90</th><th>max</th><th>hedge fired</th><th>fired %</th><th>hedge won</th><th>won % of fired</th><th>p50 when fired</th></tr>");
        let mut eps: Vec<String> = self.hedges.keys().cloned().collect();
        eps.sort();
        for e in &eps {
            let x = self.hedges.get_mut(e).expect("named");
            let fired_pct = if x.n > 0 { x.fired as f64 * 100.0 / x.n as f64 } else { 0.0 };
            let won_pct = if x.fired > 0 { format!("{:.0}", x.won as f64 * 100.0 / x.fired as f64) } else { "–".into() };
            let s = perf::stats(&mut x.ms);
            let fp50 = perf::stats(&mut x.fired_ms).map(|s| ms(s.p50)).unwrap_or_else(|| "–".into());
            let _ = write!(
                h,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{fired_pct:.1}</td><td>{}</td><td>{won_pct}</td><td>{fp50}</td></tr>",
                esc(e),
                x.n,
                cell(s, |s| s.p50),
                cell(s, |s| s.p90),
                cell(s, |s| s.max),
                x.fired,
                x.won
            );
        }
        h.push_str("</table>");
    }
}

/// HH:MM:SS.mmm (UTC) of an epoch-ms time.
fn clock(t: f64) -> String {
    let ms = t.max(0.0) as u64;
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}.{:03}", (s / 3600) % 24, (s / 60) % 60, s % 60, ms % 1000)
}

/// An event's start (epoch ms): durations end at `at` (requests, navigations, frames), an
/// interaction starts at its input.
pub fn start_of(ev: &J) -> f64 {
    let at = ev.n("at").unwrap_or(0.0);
    let m = ev.s("m").unwrap_or("");
    let dur = matches!(m.split('.').next().unwrap_or(""), "nav" | "req" | "gv" | "feed" | "video" | "ui")
        && !matches!(m, "ui.act" | "video.session" | "video.drops" | "video.stall");
    if dur { at - ev.n("v").unwrap_or(0.0) } else { at }
}

/// The breakdown shown beside an event (no raw JSON for the common fields).
fn summary(ev: &J) -> String {
    let mut parts = Vec::new();
    if let J::Obj(o) = ev {
        for (k, v) in o {
            if matches!(k.as_str(), "m" | "v" | "at" | "t" | "reqs" | "scripts" | "boot") {
                continue;
            }
            let mut s = String::new();
            v.write(&mut s);
            parts.push(format!("{k}={}", s.trim_matches('"')));
        }
        for k in ["reqs", "boot", "scripts"] {
            if let Some(v) = ev.get(k) {
                let mut s = String::new();
                v.write(&mut s);
                parts.push(format!("{k}={s}"));
            }
        }
    }
    parts.join(" ")
}

#[derive(Default)]
struct PageInfo {
    version: String,
    platform: String,
    first: f64,
    last: f64,
    events: u32,
    acts: u32,
    navs: u32,
    loafs: u32,
}

/// `GET /timeline?since=<…>&platform=<tv|desktop>` (documents) or `GET /timeline?page=<id>` (one document's events).
pub fn page(file: &Path, q: &[(String, String)]) -> String {
    let get = |k: &str| q.iter().find(|x| x.0 == k).map(|x| x.1.as_str()).unwrap_or("");
    let now = perf::now_ms();
    let want = get("page").to_string();
    let since_s = if q.iter().any(|x| x.0 == "since") { get("since").to_string() } else { "7d".into() };
    let from = if want.is_empty() { perf::since(&since_s, now) } else { 0.0 };
    let mut pages: HashMap<String, PageInfo> = HashMap::new();
    let mut events: Vec<(f64, f64, J)> = Vec::new();
    let platform = get("platform").to_string();
    perf::scan_batches(file, from, &platform, |r, version| {
        let p = r.s("page").unwrap_or("").to_string();
        if !want.is_empty() && p != want {
            return;
        }
        for ev in r.get("events").map(J::arr).unwrap_or(&[]) {
            let at = ev.n("at").unwrap_or(0.0);
            if at < from {
                continue;
            }
            if want.is_empty() {
                let pi = pages.entry(p.clone()).or_default();
                if pi.version.is_empty() {
                    pi.version = version.to_string();
                    pi.platform = perf::platform_of(r).to_string();
                }
                let st = start_of(ev);
                if pi.first == 0.0 || st < pi.first {
                    pi.first = st;
                }
                pi.last = pi.last.max(at);
                pi.events += 1;
                match ev.s("m").unwrap_or("") {
                    "ui.act" => pi.acts += 1,
                    "lag.loaf" | "lag.longtask" => pi.loafs += 1,
                    m if m.starts_with("nav.") => pi.navs += 1,
                    _ => {}
                }
            } else if events.len() < EVENTS_MAX {
                events.push((start_of(ev), ev.n("t").unwrap_or(f64::MAX), ev.clone()));
            }
        }
    });
    let mut h = String::with_capacity(32 * 1024);
    h.push_str("<!doctype html><meta charset=utf-8><title>Redline timeline</title><style>\
body{background:#111;color:#ddd;font:13px ui-monospace,monospace;margin:20px}h1,h2{font:600 18px system-ui;color:#b7ebca;margin:22px 0 8px}\
a{color:#9cc8ff}table{border-collapse:collapse;margin:6px 0}td,th{padding:2px 8px;border-bottom:1px solid #222;text-align:right;white-space:nowrap;vertical-align:top}\
th{color:#999;font-weight:500}td.l{text-align:left;white-space:normal;word-break:break-all}tr:hover td{background:#1b1b1b}tr:target td{background:#26332b}\
.ui td{color:#b7ebca}.nav td{color:#9cc8ff}.lag td{color:#e8a87c}.dim{color:#777}.bar{display:inline-block;height:7px;background:#4a7;border-radius:2px;vertical-align:middle}</style>");
    if want.is_empty() {
        let _ = write!(
            h,
            "<h1>Timeline: documents</h1><p class=dim>One row per document (a tab's or the TV app's page load), newest first. <a href=/perf>dashboard</a> · window {} · <a href=\"/timeline?since={0}\">TV and desktop</a> · <a href=\"/timeline?since={0}&platform=desktop\">desktop</a> · <a href=\"/timeline?since={0}&platform=tv\">TV</a></p>",
            esc(&since_s)
        );
        h.push_str("<table><tr><th>document</th><th>platform</th><th>version</th><th>started</th><th>length s</th><th>events</th><th>interactions</th><th>navigations</th><th>long frames</th></tr>");
        let mut list: Vec<(&String, &PageInfo)> = pages.iter().collect();
        list.sort_by(|a, b| b.1.first.total_cmp(&a.1.first));
        for (p, i) in list.into_iter().take(PAGES) {
            let _ = write!(
                h,
                "<tr><td style=text-align:left><a href=\"/timeline?page={0}\">{0}</a></td><td>{8}</td><td>{1}</td><td>{2}</td><td>{3:.0}</td><td>{4}</td><td>{5}</td><td>{6}</td><td>{7}</td></tr>",
                esc(p),
                esc(&i.version),
                clock(i.first),
                (i.last - i.first) / 1000.0,
                i.events,
                i.acts,
                i.navs,
                i.loafs,
                esc(&i.platform)
            );
        }
        h.push_str("</table>");
        return h;
    }
    events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let t0 = events.first().map(|e| e.0).unwrap_or(0.0);
    let longest = events.iter().filter_map(|e| e.2.n("v")).fold(1.0, f64::max).min(5000.0);
    let _ = write!(h, "<h1>Timeline: {}</h1><p class=dim>{} events in order of their start; +ms from the first. Interactions are mint, navigations blue, long frames orange. <a href=/timeline>all documents</a> · <a href=/perf>dashboard</a></p>", esc(&want), events.len());
    h.push_str("<table><tr><th>+ms</th><th>clock</th><th>event</th><th>ms</th><th></th><th>breakdown</th></tr>");
    for (st, _, ev) in &events {
        let m = ev.s("m").unwrap_or("?");
        let v = ev.n("v").unwrap_or(0.0);
        let class = if m == "ui.act" { "ui" } else if m.starts_with("nav.") { "nav" } else if m.starts_with("lag.") { "lag" } else { "" };
        let label = if m == "ui.act" { format!("{} · {}", m, ev.s("act").unwrap_or("?")) } else { m.to_string() };
        let w = (v / longest * 160.0).clamp(1.0, 160.0);
        let _ = write!(
            h,
            "<tr class={class} id=t{}><td>{}</td><td class=dim>{}</td><td style=text-align:left>{}</td><td>{}</td><td style=text-align:left><span class=bar style=width:{w:.0}px></span></td><td class=l>{}</td></tr>",
            ev.n("at").unwrap_or(0.0) as u64,
            ms(st - t0),
            clock(*st),
            esc(&label),
            ms(v),
            esc(&summary(ev))
        );
    }
    h.push_str("</table>");
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interactions_hedges_and_timeline() {
        let dir = std::env::temp_dir().join(format!("ytc-dc-tl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("i.ndjson");
        let now = perf::now_ms();
        let mut s = String::new();
        for i in 0..10 {
            let at = now - 1000.0 * (10 - i) as f64;
            s.push_str(&format!(
                "{{\"received\":{at:.0},\"report\":{{\"build\":\"0.9.0 (bbbb)\",\"kind\":\"perf\",\"schema\":2,\"page\":\"pg{}\",\"events\":[\
{{\"m\":\"ui.act\",\"v\":{},\"at\":{at:.0},\"t\":5.5,\"act\":\"card.open\",\"input\":\"pointer\",\"route\":\"home\",\"wait\":\"network\",\"handler_ms\":1.2,\"dom_ms\":300,\"content_ms\":{}}},\
{{\"m\":\"nav.watch\",\"v\":{},\"at\":{:.0},\"act\":\"card.open\"}},\
{{\"m\":\"req.next\",\"v\":1500,\"at\":{at:.0},\"hedge_fired\":{},\"hedge_won\":{}}},\
{{\"m\":\"ui.act\",\"v\":16,\"at\":{at:.0},\"act\":\"key.k\",\"input\":\"keyboard\",\"route\":\"watch\",\"wait\":\"none\"}}]}}}}\n",
                i % 2,
                20 + i,
                400 + i * 10,
                400 + i * 10,
                at + 400.0,
                i < 4,
                i < 1
            ));
        }
        std::fs::write(&f, s).unwrap();
        let page = perf::dashboard(&f, &perf::query("/perf"));
        assert!(page.contains("Interactions (ui.act)") && page.contains("<td>card.open</td><td>10</td>"), "per action");
        assert!(page.contains("<td>key.k</td><td>10</td>"));
        assert!(page.contains("<td>490</td>"), "content max");
        assert!(page.contains("<td>next</td><td>10</td>") && page.contains("<td>4</td><td>40.0</td><td>1</td><td>25</td>"), "hedge rates");
        assert!(page.contains("/timeline?page=pg1"), "slowest link to their timeline");
        let list = page_list(&f);
        assert!(list.contains("/timeline?page=pg0") && list.contains("/timeline?page=pg1"));
        let tl = super::page(&f, &perf::query("/timeline?page=pg1"));
        assert!(tl.contains("ui.act · card.open") && tl.contains("nav.watch") && tl.contains("req.next"));
        assert!(!tl.contains("pg0\""), "only that document");
        // Ordered by start: the request (started 1.5 s before it answered) comes first.
        assert!(tl.find("req.next").unwrap() < tl.find("ui.act · card.open").unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn page_list(f: &Path) -> String {
        super::page(f, &perf::query("/timeline"))
    }

    #[test]
    fn starts() {
        let e = json::parse("{\"m\":\"req.next\",\"v\":100,\"at\":1000}").unwrap();
        assert_eq!(start_of(&e), 900.0);
        let e = json::parse("{\"m\":\"ui.act\",\"v\":100,\"at\":1000}").unwrap();
        assert_eq!(start_of(&e), 1000.0);
        assert_eq!(clock(3_723_004.0), "01:02:03.004");
    }
}
