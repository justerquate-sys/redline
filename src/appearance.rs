//! The toolbar's preference patches (SponsorSkip). Patches preserve the rest of the config, including
//! fields this version no longer reads (e.g. `theme` from 0.13.3–0.13.8: Redline has one palette, in
//! client.css; a stored `theme` is ignored and never an error).

use crate::{config::Config, player::sponsor};
use serde_json::{Value, json};

/// The `localStorage` flag 0.13.3–0.13.8 kept for a new tab's first frame while the (removed) YouTube palette
/// was chosen. Redline has one palette, so nothing is needed before `chrome.storage` answers; a flag
/// left from those versions is removed once (one synchronous read per boot, nothing written when absent).
const OLD_HINT: &str = "ytc-palette-youtube";

pub fn forget_old_hint() {
    if crate::bridge::local_flag(OLD_HINT) {
        crate::bridge::local_flag_set(OLD_HINT, false);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub sponsor_on: Option<bool>,
    pub skip_on: u16,
    pub skip_off: u16,
}

impl Patch {
    pub fn empty(self) -> bool { self == Self::default() }

    pub fn merge(&mut self, next: Self) {
        if next.sponsor_on.is_some() { self.sponsor_on = next.sponsor_on; }
        self.skip_on = (self.skip_on & !next.skip_off) | next.skip_on;
        self.skip_off = (self.skip_off & !next.skip_on) | next.skip_off;
    }

    /// Re-read before writing; change only explicitly edited fields and category bits.
    pub fn apply(self, stored: &str) -> String {
        let mut v: Value = serde_json::from_str(stored).ok().filter(Value::is_object).unwrap_or_else(|| json!({}));
        if self.sponsor_on.is_some() || self.skip_on != 0 || self.skip_off != 0 {
            let old = Config::from_json(stored).prefs;
            if !v.get("p").is_some_and(Value::is_object) { v["p"] = json!({}); }
            let p = v["p"].as_object_mut().unwrap();
            if let Some(on) = self.sponsor_on {
                if on { p.remove("sbo"); } else { p.insert("sbo".into(), false.into()); }
            }
            if self.skip_on != 0 || self.skip_off != 0 {
                let mask = (old.sb_skip | self.skip_on) & !self.skip_off;
                if mask == sponsor::default_mask() { p.remove("sb"); }
                else { p.insert("sb".into(), sponsor::mask_to_keys(mask).into()); }
            }
        }
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_preserve_unrelated_and_concurrent_preferences() {
        let old = r#"{"v":"2.20261030.01.00","vd":"visitor","au":2,"future":7,"theme":"youtube","p":{"vol":0.3,"cc":false,"cs":{"v":2,"x":0.2},"future":"kept"}}"#;
        let patch = Patch { sponsor_on: Some(false), skip_on: sponsor::Cat::Intro.bit(), ..Default::default() };
        let v: Value = serde_json::from_str(&patch.apply(old)).unwrap();
        // A field this version no longer reads stays as it was.
        assert_eq!(v["theme"], "youtube");
        assert_eq!(v["future"], 7);
        assert_eq!(v["p"]["vol"], 0.3);
        assert_eq!(v["p"]["cc"], false);
        assert_eq!(v["p"]["cs"]["x"], 0.2);
        assert_eq!(v["p"]["future"], "kept");
        assert_eq!(v["p"]["sbo"], false);
        let cfg = Config::from_json(&v.to_string());
        assert_ne!(cfg.prefs.sb_skip & sponsor::Cat::Intro.bit(), 0);
        let concurrent = r#"{"p":{"vol":0.8,"sb":"sponsor,selfpromo,interaction,outro"}}"#;
        let changed = patch.apply(concurrent);
        let cfg = Config::from_json(&changed);
        assert_eq!(cfg.prefs.volume, 0.8);
        assert_ne!(cfg.prefs.sb_skip & sponsor::Cat::Outro.bit(), 0);
        assert_ne!(cfg.prefs.sb_skip & sponsor::Cat::Intro.bit(), 0);
    }

    #[test]
    fn latest_edits_win_and_defaults_stay_compact() {
        let mut p = Patch { sponsor_on: Some(false), skip_on: sponsor::Cat::Intro.bit(), ..Default::default() };
        p.merge(Patch { sponsor_on: Some(true), skip_off: sponsor::Cat::Intro.bit(), ..Default::default() });
        let v: Value = serde_json::from_str(&p.apply(r#"{"theme":"lumen","p":{"sbo":false,"sb":"sponsor,selfpromo,interaction,intro"}}"#)).unwrap();
        assert!(v["p"].get("sbo").is_none());
        assert!(v["p"].get("sb").is_none());
    }

    #[test]
    fn a_stored_palette_choice_is_harmless() {
        // 0.13.3–0.13.8 stored `theme` ("youtube"; "lumen" never, but any value is tolerated): read as if absent.
        for raw in [r#"{"theme":"lumen","au":1,"p":{"vol":0.5}}"#, r#"{"theme":"youtube","au":1,"p":{"vol":0.5}}"#, r#"{"theme":7,"au":1,"p":{"vol":0.5}}"#, r#"{"theme":null,"au":1,"p":{"vol":0.5}}"#] {
            let cfg = Config::from_json(raw);
            assert_eq!((cfg.auth_user, cfg.prefs.volume), (1, 0.5), "{raw}");
            // The client's own config writes no longer carry it.
            assert!(!cfg.to_json().contains("theme"), "{raw}");
        }
    }
}
