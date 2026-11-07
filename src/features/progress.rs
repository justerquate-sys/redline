//! Resume positions for up to 2,000 videos (
//! `yourTubeProgressV1`, the extension's record shape:
//! `{version:1, entries:{id:{position, duration, completed, updatedAt}}}`).
//! Only the video id, last position, duration, a completion flag and the
//! observation time are stored. Rewinds are kept as the new position.

use crate::json::Value as In;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

pub const KEY: &str = "yourTubeProgressV1";
pub const MAX: usize = 2000;
/// Positions closer than this to the start are not worth resuming.
pub const MIN_RESUME: f64 = 5.0;
/// Within this many seconds (or 2 %) of the end counts as finished.
const END_SLACK: f64 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Entry {
    pub position: f64,
    pub duration: f64,
    pub completed: bool,
    pub updated_at: f64,
}

#[derive(Default, Debug)]
pub struct Store {
    pub map: HashMap<String, Entry>,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

pub(crate) fn near_end(position: f64, duration: f64) -> bool {
    duration - position <= END_SLACK.min(duration * 0.02).max(2.0)
}

impl Store {
    pub fn from_json(v: &In, now: f64) -> Store {
        let mut map = HashMap::new();
        if let Some(o) = v.get("entries").and_then(In::as_object) {
            for (id, e) in o {
                let f = |k: &str| e.get(k).and_then(In::as_f64).unwrap_or(f64::NAN);
                let (position, duration, updated_at) = (f("position"), f("duration"), f("updatedAt"));
                if !valid_id(id) || !position.is_finite() || !duration.is_finite() || duration <= 0.0 || position < 0.0 || position > duration || !(updated_at > 0.0 && updated_at <= now + 60_000.0) {
                    continue;
                }
                map.insert(id.to_string(), Entry { position, duration, completed: e.get("completed").and_then(In::as_bool).unwrap_or(false), updated_at });
            }
        }
        let mut s = Store { map };
        s.cap();
        s
    }

    pub fn cap(&mut self) {
        if self.map.len() <= MAX {
            return;
        }
        let mut by_age: Vec<(String, f64)> = self.map.iter().map(|(k, e)| (k.clone(), e.updated_at)).collect();
        super::filters::sort_keyed(&mut by_age, |e| -e.1);
        for (k, _) in by_age.into_iter().skip(MAX) {
            self.map.remove(&k);
        }
    }

    pub fn to_json(&self) -> String {
        let mut m = Map::new();
        for (id, e) in &self.map {
            m.insert(id.clone(), json!({"position": e.position, "duration": e.duration, "completed": e.completed, "updatedAt": e.updated_at}));
        }
        json!({"version": 1, "entries": Value::Object(m)}).to_string()
    }

    /// Records a playback position; returns true when the store changed.
    /// Reaching the end sets `completed` (kept on later partial rewatches).
    pub fn record(&mut self, id: &str, position: f64, duration: f64, ended: bool, now: f64) -> bool {
        if !valid_id(id) || !duration.is_finite() || duration <= 0.0 || !position.is_finite() || position < 0.0 {
            return false;
        }
        let position = position.min(duration);
        let finished = ended || near_end(position, duration);
        let prev = self.map.get(id).copied();
        // Nothing worth keeping yet: a few seconds into a never-seen video.
        if prev.is_none() && position < MIN_RESUME && !finished {
            return false;
        }
        let e = Entry {
            position: (position * 10.0).round() / 10.0,
            duration: (duration * 10.0).round() / 10.0,
            completed: finished || prev.is_some_and(|p| p.completed),
            updated_at: now.floor(),
        };
        if prev.is_some_and(|p| (p.position - e.position).abs() < 0.5 && p.completed == e.completed && p.duration == e.duration) {
            return false;
        }
        self.map.insert(id.to_string(), e);
        self.cap();
        true
    }

    /// Where to start an unannotated watch URL: the saved partial position,
    /// none when finished or barely started.
    pub fn resume_at(&self, id: &str) -> Option<f64> {
        let e = self.map.get(id)?;
        (e.position >= MIN_RESUME && !near_end(e.position, e.duration)).then_some(e.position)
    }

    /// Thumbnail bar: (watched %, finished before). A finished video that is
    /// being rewatched shows its new position over a faint full track.
    pub fn bar(&self, id: &str) -> Option<(u8, bool)> {
        let e = self.map.get(id)?;
        let pct = if near_end(e.position, e.duration) { 100.0 } else { e.position / e.duration * 100.0 };
        Some((pct.round().clamp(1.0, 100.0) as u8, e.completed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_resume_bar() {
        let mut s = Store::default();
        assert!(!s.record("aaaaaaaaaaa", 2.0, 600.0, false, 1.0), "too early to keep");
        assert!(s.record("aaaaaaaaaaa", 120.0, 600.0, false, 2.0));
        assert!(!s.record("aaaaaaaaaaa", 120.2, 600.0, false, 3.0), "no-op write skipped");
        assert_eq!(s.resume_at("aaaaaaaaaaa"), Some(120.0));
        assert_eq!(s.bar("aaaaaaaaaaa"), Some((20, false)));
        // Rewind is the new position (not a maximum).
        assert!(s.record("aaaaaaaaaaa", 30.0, 600.0, false, 4.0));
        assert_eq!(s.resume_at("aaaaaaaaaaa"), Some(30.0));
        // End → completed, no resume, full bar; a later partial rewatch keeps the flag.
        assert!(s.record("aaaaaaaaaaa", 599.0, 600.0, false, 5.0));
        assert_eq!(s.resume_at("aaaaaaaaaaa"), None);
        assert_eq!(s.bar("aaaaaaaaaaa"), Some((100, true)));
        assert!(s.record("aaaaaaaaaaa", 60.0, 600.0, false, 6.0));
        assert_eq!(s.bar("aaaaaaaaaaa"), Some((10, true)));
        assert!(!s.record("bad id!", 60.0, 600.0, false, 7.0));
        assert!(!s.record("bbbbbbbbbbb", 60.0, 0.0, false, 7.0));
        assert_eq!(s.resume_at("ccccccccccc"), None);
    }

    #[test]
    fn cap_evicts_oldest_and_round_trips() {
        let mut s = Store::default();
        for i in 0..(MAX + 25) {
            s.record(&format!("v{i:010}"), 100.0, 1000.0, false, 1000.0 + i as f64);
        }
        assert_eq!(s.map.len(), MAX);
        assert!(!s.map.contains_key("v0000000000"), "oldest evicted");
        assert!(s.map.contains_key(&format!("v{:010}", MAX + 24)));
        let v: Value = serde_json::from_str(&s.to_json()).unwrap();
        assert_eq!(v["version"], 1);
        let back = Store::from_json(&In::from_serde(&v), 1e12);
        assert_eq!(back.map.len(), MAX);
        let bad = crate::json::jv!({"entries": {"x": {"position": 5, "duration": 4, "updatedAt": 1}, "y": {"position": 1, "duration": 4, "updatedAt": 1, "completed": true}, "bad id": {"position": 1, "duration": 4, "updatedAt": 1}}});
        let s = Store::from_json(&bad, 10.0);
        assert_eq!(s.map.len(), 1);
        assert!(s.map["y"].completed);
    }
}
