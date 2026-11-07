//! The "shown" signals YouTube's own Home page sends, sent the same way for Redline's Home (owner decision,
//! 3 Nov 2026: so that YouTube rotates Home as it does for its own page, i.e. stops repeating videos that
//! were shown and passed over; CONTRIBUTING "Privacy").
//!
//! What YouTube's page sends for its Home (captured live from its own page, signed out, 3 Nov 2026, in
//! `log_event` batches): a `screenCreated` naming the answer it shows (`EVENT_ID` of the Home document, also
//! `serializedClientEventId`), then for every card that comes into view `visualElementStateChanged` with its
//! grid place and `visualElementShown` (eventType 1) for the grid item's and the card's own tracking ids with
//! the card's box in the viewport, and `scrollEvent` per scroll. Only that is sent here, only for cards that
//! were really on screen (≥ half visible, an IntersectionObserver), batched. Nothing else YouTube's page logs
//! (latency, heartbeats, experiments, ads, hovers, offline state) and nothing on other pages.
//!
//! Pure: the app (`app/impglue.rs`) feeds it the screen, the cards that came into view and the scrolls, and
//! sends what `batch` returns.

use crate::parse::Track;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

/// Where YouTube's page sends them. (`prettyPrint=false` is what tells the extension's request rules this
/// is Redline's own batch: they block every other `log_event`, e.g. on YouTube's own pages; rules.json 33.)
pub const URL: &str = "https://www.youtube.com/youtubei/v1/log_event?alt=json&prettyPrint=false";
/// Home's page type in YouTube's logs (`pageVe.veType` of its Home screen).
pub const HOME_VE: u32 = 3854;
/// A batch goes out this long after its first event (YouTube's page: its first batch right after the page
/// loads, then every few seconds).
pub const FLUSH_MS: u32 = 1000;
/// Events per request at most (YouTube's own batches: up to ~200).
pub const MAX_BATCH: usize = 200;
/// Shown: at least this share of the card in the viewport (YouTube's page logged cards 68–74 % in view as shown).
pub const SHOWN_RATIO: f64 = 0.5;

/// A card's box in the viewport (CSS px), as the page reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BoxPx {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One Home screen: the answer on screen and what has been logged for it.
#[derive(Debug, Default)]
pub struct Screen {
    /// The page's screen nonce (16 characters, new for every Home answer shown).
    pub csn: String,
    /// The answer's id (`EVENT_ID` of the Home document); none for a `browse` answer.
    pub eid: Option<String>,
    pub active: bool,
    track: HashMap<String, (Option<String>, Option<String>)>,
    shown: HashSet<String>,
    queue: Vec<Value>,
    /// Cards first shown in the queued events (a batch with new ones changes what YouTube answers next).
    new_cards: usize,
    counter: u64,
    scroll_y: f64,
}

fn ev(name: &str, body: Value, t: f64, idle_ms: f64) -> Value {
    let mut e = json!({"eventTimeMs": t.round() as u64, "context": {"lastActivityMs": format!("{:.0}", idle_ms.max(0.0))}});
    e[name] = body;
    e
}

impl Screen {
    /// A new Home answer is on screen: a new screen nonce and its `screenCreated`.
    pub fn start(&mut self, csn: String, eid: Option<String>, track: Vec<Track>, t: f64, idle_ms: f64) {
        let queue = std::mem::take(&mut self.queue);
        *self = Screen { csn, eid, active: true, queue, counter: self.counter, ..Default::default() };
        self.add(track);
        let page = match &self.eid {
            Some(e) => json!({"veType": HOME_VE, "youtubeData": {"servletData": {"serializedServletEventId": e}}}),
            None => json!({"veType": HOME_VE}),
        };
        self.queue.push(ev("screenCreated", json!({"csn": self.csn, "pageVe": page}), t, idle_ms));
    }

    /// More cards of this screen (a continuation's).
    pub fn add(&mut self, track: Vec<Track>) {
        for t in track {
            self.track.entry(t.id).or_insert((t.item, t.own));
        }
    }

    /// A card came into view (at grid place `col`, `row`). True when this queued something.
    pub fn shown(&mut self, id: &str, col: usize, row: usize, b: BoxPx, t: f64, idle_ms: f64) -> bool {
        if !self.active || self.shown.contains(id) {
            return false;
        }
        let Some((item, own)) = self.track.get(id).cloned() else { return false };
        self.shown.insert(id.to_string());
        self.new_cards += 1;
        let view = |h: f64| json!({"viewData": {"originXPoints": b.x.round() as i64, "originYPoints": b.y.round() as i64, "widthPoints": b.w.round() as i64, "heightPoints": h.round() as i64, "position": "POSITION_UNSPECIFIED"}});
        if let Some(tp) = &item {
            self.queue.push(ev("visualElementStateChanged", json!({"csn": self.csn, "ve": {"trackingParams": tp}, "clientData": {"gridData": {"veColumnCoordinate": col, "veRowCoordinate": row}}}), t, idle_ms));
            self.queue.push(ev("visualElementShown", json!({"csn": self.csn, "ve": {"trackingParams": tp}, "eventType": 1, "clientData": view(b.h)}), t, idle_ms));
        }
        if let Some(tp) = &own {
            self.queue.push(ev("visualElementShown", json!({"csn": self.csn, "ve": {"trackingParams": tp}, "eventType": 1, "clientData": view(b.h)}), t, idle_ms));
        }
        true
    }

    /// The page came to rest at `y` after a scroll. True when this queued something.
    pub fn scrolled(&mut self, y: f64, t: f64, idle_ms: f64) -> bool {
        let net = (y - self.scroll_y).round();
        if !self.active || net.abs() < 1.0 {
            return false;
        }
        self.scroll_y = y;
        // (As YouTube's page reports it: the offset where the scroll ended, and the distance.)
        self.queue.push(ev("scrollEvent", json!({"clientScreenNonce": self.csn, "orientation": "SCROLL_ORIENTATION_VERTICAL", "startContentOffsetPoints": y.round() as i64, "netDisplacementPoints": net as i64}), t, idle_ms));
        true
    }

    /// The screen is gone (another page, another answer).
    pub fn end(&mut self) {
        self.active = false;
        self.track.clear();
    }

    pub fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// The next request body (≤ `MAX_BATCH` events) with the client's `context`, and how many cards it shows
    /// for the first time. None when nothing is queued.
    pub fn batch(&mut self, client: Value) -> Option<(String, usize)> {
        if self.queue.is_empty() {
            return None;
        }
        let n = self.queue.len().min(MAX_BATCH);
        let events: Vec<Value> = self.queue.drain(..n).collect();
        let cards = if self.queue.is_empty() { std::mem::take(&mut self.new_cards) } else { 0 };
        self.counter += 1;
        let mut body = json!({"context": {"client": client}, "events": events});
        if let Some(e) = &self.eid {
            body["serializedClientEventId"] = json!({"serializedEventId": e, "clientCounter": self.counter.to_string()});
        }
        Some((body.to_string(), cards))
    }
}

/// The client part of the batch's context, as YouTube's page describes itself in it (the subset Redline knows;
/// `clientName` is the number here, as in YouTube's own `log_event`).
pub fn client(version: &str, hl: &str, gl: Option<&str>, w: u32, h: u32) -> Value {
    let mut c = json!({"hl": hl, "clientName": 1, "clientVersion": version, "platform": "DESKTOP", "windowWidthPoints": w, "windowHeightPoints": h,
        "mainAppWebInfo": {"graftUrl": "/", "webDisplayMode": "WEB_DISPLAY_MODE_BROWSER"}});
    if let Some(g) = gl {
        c["gl"] = g.into();
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(id: &str, item: Option<&str>, own: Option<&str>) -> Track {
        Track { id: id.into(), item: item.map(Into::into), own: own.map(Into::into) }
    }

    #[test]
    fn a_screen_logs_like_youtubes_home() {
        let mut s = Screen::default();
        s.start("P405wTBgKqj6LBCd".into(), Some("Ks3pasSOKMKrxdwPjPrMsQg".into()), vec![tr("aaaaaaaaaaa", Some("CL4D"), Some("CL8D")), tr("bbbbbbbbbbb", None, Some("CAMQ"))], 1000.0, 105.0);
        let b = BoxPx { x: 264.4, y: 136.0, w: 360.0, h: 300.0 };
        assert!(s.shown("aaaaaaaaaaa", 0, 0, b, 1187.0, 293.0));
        assert!(!s.shown("aaaaaaaaaaa", 0, 0, b, 1190.0, 293.0), "once per screen");
        assert!(!s.shown("ccccccccccc", 1, 0, b, 1190.0, 293.0), "a card without tracking ids: nothing");
        assert!(s.shown("bbbbbbbbbbb", 1, 0, b, 1190.0, 293.0));
        assert!(s.scrolled(700.0, 5000.0, 10.0) && !s.scrolled(700.4, 5100.0, 10.0));
        let (body, cards) = s.batch(client("2.20261030.02.00", "en", Some("DE"), 1400, 900)).unwrap();
        assert_eq!(cards, 2);
        let v: Value = serde_json::from_str(&body).unwrap();
        let ev = v["events"].as_array().unwrap();
        let kinds: Vec<&str> = ev.iter().map(|e| e.as_object().unwrap().keys().find(|k| *k != "eventTimeMs" && *k != "context").unwrap().as_str()).collect();
        assert_eq!(kinds, ["screenCreated", "visualElementStateChanged", "visualElementShown", "visualElementShown", "visualElementShown", "scrollEvent"]);
        // Exactly the captured shapes.
        assert_eq!(ev[0]["screenCreated"], json!({"csn": "P405wTBgKqj6LBCd", "pageVe": {"veType": 3854, "youtubeData": {"servletData": {"serializedServletEventId": "Ks3pasSOKMKrxdwPjPrMsQg"}}}}));
        assert_eq!(ev[0]["context"], json!({"lastActivityMs": "105"}));
        assert_eq!(ev[1]["visualElementStateChanged"], json!({"csn": "P405wTBgKqj6LBCd", "ve": {"trackingParams": "CL4D"}, "clientData": {"gridData": {"veColumnCoordinate": 0, "veRowCoordinate": 0}}}));
        assert_eq!(ev[2]["visualElementShown"], json!({"csn": "P405wTBgKqj6LBCd", "ve": {"trackingParams": "CL4D"}, "eventType": 1, "clientData": {"viewData": {"originXPoints": 264, "originYPoints": 136, "widthPoints": 360, "heightPoints": 300, "position": "POSITION_UNSPECIFIED"}}}));
        assert_eq!(ev[3]["visualElementShown"]["ve"]["trackingParams"], "CL8D");
        assert_eq!(ev[4]["visualElementShown"]["ve"]["trackingParams"], "CAMQ");
        assert_eq!(ev[5]["scrollEvent"], json!({"clientScreenNonce": "P405wTBgKqj6LBCd", "orientation": "SCROLL_ORIENTATION_VERTICAL", "startContentOffsetPoints": 700, "netDisplacementPoints": 700}));
        assert_eq!(ev[1]["eventTimeMs"], 1187);
        assert_eq!(v["serializedClientEventId"]["serializedEventId"], "Ks3pasSOKMKrxdwPjPrMsQg");
        assert_eq!(v["context"]["client"]["clientName"], 1);
        assert_eq!(v["context"]["client"]["gl"], "DE");
        assert!(s.batch(Value::Null).is_none(), "nothing left");
    }

    #[test]
    fn batches_are_bounded_and_end_with_the_screen() {
        let mut s = Screen::default();
        let track: Vec<Track> = (0..150).map(|i| tr(&format!("v{i:010}"), Some("CAEQ"), Some("CAIQ"))).collect();
        s.start("x".repeat(16), None, track, 0.0, 0.0);
        for i in 0..150 {
            s.shown(&format!("v{i:010}"), i % 4, i / 4, BoxPx::default(), 1.0, 0.0);
        }
        let (b1, c1) = s.batch(json!({})).unwrap();
        let v: Value = serde_json::from_str(&b1).unwrap();
        assert_eq!(v["events"].as_array().unwrap().len(), MAX_BATCH);
        assert!(v.get("serializedClientEventId").is_none(), "a browse answer has no document id");
        assert_eq!(c1, 0, "the cards are counted with the batch that empties the queue");
        let (_, c2) = s.batch(json!({})).unwrap();
        let (b3, c3) = s.batch(json!({})).unwrap();
        assert_eq!((c2, c3), (0, 150), "451 events: 200 + 200 + 51");
        assert_eq!(serde_json::from_str::<Value>(&b3).unwrap()["events"].as_array().unwrap().len(), 51);
        s.end();
        assert!(!s.shown("v0000000001", 0, 0, BoxPx::default(), 2.0, 0.0) && !s.scrolled(500.0, 2.0, 0.0), "nothing after the screen ended");
        // A new screen keeps what was still queued from the last one (it goes out first).
        s.start("y".repeat(16), None, vec![], 3.0, 0.0);
        s.scroll_y = 0.0;
        assert!(s.pending());
    }
}
