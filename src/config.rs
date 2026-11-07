//! Client configuration that replaces the stopped page's `ytcfg`: client
//! version, visitor data, session index, and SAPISID-based authorization.
//! Only these small values are persisted (chrome.storage.local `ytcConfigV1`).

use crate::json::{self, Value as In};
use serde_json::json;

pub const STORAGE_KEY: &str = "ytcConfigV1";
/// Known-good WEB client version (1 Nov 2026); replaced by `/sw.js_data`.
pub const FALLBACK_VERSION: &str = "2.20261030.01.00";
pub const ORIGIN: &str = "https://www.youtube.com";
/// Refresh the cached client version / visitor data after this long.
pub const MAX_AGE_MS: f64 = 24.0 * 3600.0 * 1000.0;

#[derive(Clone, Debug)]
pub struct Config {
    pub version: String,
    pub visitor: Option<String>,
    /// `X-Goog-AuthUser` session index; settable in storage (`au`).
    pub auth_user: u32,
    /// Epoch ms of the last successful `/sw.js_data` refresh (0 = never).
    pub fetched_at: f64,
    /// Player preferences (persisted with the config).
    pub prefs: Prefs,
    /// `signatureTimestamp` override for the WEB `player` call (`sts`).
    pub sts: Option<u32>,
    /// Own player off → watch pages open natively (`op: false`).
    pub own_player: bool,
    /// Send the VISIONOS `player` call with the account's cookies (`va`), for
    /// a signed-in check; default is signed out.
    pub vision_auth: bool,
    /// Watch page Autoplay switch (`ap`, default on): play YouTube's autoplay
    /// target when a video ends. Playlists continue regardless.
    pub autoplay: bool,
}

/// Player preferences remembered across videos.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub captions: bool,
    /// 0..=1
    pub volume: f64,
    pub muted: bool,
    /// Maximum quality rung chosen by the user (0 = Auto).
    pub quality: u32,
    /// Preferred video codec: "vp9", "av1", "h264" or "" (default order).
    pub codec: String,
    pub theater: bool,
    /// Caption placement and size: one object, stored only after a deliberate
    /// drag or resize. `x`/`y` = the caption's upper-left corner as a fraction
    /// of the player, `s` = scale 0.5–2 of the default size.
    pub cc_style: Option<CcStyle>,
    /// SponsorBlock lookup and skipping (`sbo`, stored only when off: then nothing is requested).
    pub sb_on: bool,
    /// Auto-skipped SponsorBlock categories (`player::sponsor` bits; `sb`, stored only when not the default).
    pub sb_skip: u16,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CcStyle {
    pub x: f64,
    pub y: f64,
    pub s: f64,
}

impl CcStyle {
    /// Near the upper-left (the reference placement), default size.
    pub const DEFAULT: CcStyle = CcStyle { x: 0.03, y: 0.05, s: 1.0 };
    pub fn clamped(self) -> CcStyle {
        let f = |v: f64, lo: f64, hi: f64, d: f64| if v.is_finite() { v.clamp(lo, hi) } else { d };
        CcStyle { x: f(self.x, 0.0, 0.95, 0.03), y: f(self.y, 0.0, 0.95, 0.05), s: f(self.s, 0.5, 2.0, 1.0) }
    }
}

impl Default for Prefs {
    fn default() -> Self {
        // Captions default on (available CC enabled, a deliberate choice remembered).
        Prefs { captions: true, volume: 1.0, muted: false, quality: 0, codec: String::new(), theater: false, cc_style: None, sb_on: true, sb_skip: crate::player::sponsor::default_mask() }
    }
}

/// Fallback `signatureTimestamp`: base.js 11a45ffb (1 Nov 2026).
pub const FALLBACK_STS: u32 = 20753;

impl Config {
    /// `signatureTimestamp` for the WEB `player` call. It tracks the player
    /// build date in days since 1970, so without base.js we use "a week ago"
    /// (never older than the known value); `sts` in storage overrides.
    pub fn sts(&self, now_ms: f64) -> u32 {
        self.sts.unwrap_or_else(|| ((now_ms / 86_400_000.0) as u32).saturating_sub(7).max(FALLBACK_STS))
    }
}

impl Default for Config {
    fn default() -> Self {
        Config { version: FALLBACK_VERSION.into(), visitor: None, auth_user: 0, fetched_at: 0.0, prefs: Prefs::default(), sts: None, own_player: true, vision_auth: false, autoplay: true }
    }
}

pub fn is_client_version(s: &str) -> bool {
    // 2.YYYYMMDD.NN.NN
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 4 && p[0] == "2" && p[1].len() == 8 && p.iter().all(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit()))
}

pub fn is_visitor(s: &str) -> bool {
    s.starts_with("Cg") && (16..=512).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"%-_=".contains(&b))
}

impl Config {
    pub fn from_json(s: &str) -> Config {
        let mut c = Config::default();
        let Some(v) = json::parse_str(s) else { return c };
        // (`theme`, the palette choice of 0.13.3–0.13.8, is ignored: Redline has one palette.)
        if let Some(x) = v.get("v").and_then(In::as_str).filter(|x| is_client_version(x)) {
            c.version = x.into();
        }
        c.visitor = v.get("vd").and_then(In::as_str).filter(|x| is_visitor(x)).map(Into::into);
        c.auth_user = v.get("au").and_then(In::as_u64).filter(|&n| n < 10).unwrap_or(0) as u32;
        c.fetched_at = v.get("at").and_then(In::as_f64).unwrap_or(0.0);
        c.sts = v.get("sts").and_then(In::as_u64).filter(|&n| (19000..40000).contains(&n)).map(|n| n as u32);
        c.own_player = v.get("op").and_then(In::as_bool).unwrap_or(true);
        c.vision_auth = v.get("va").and_then(In::as_bool).unwrap_or(false);
        c.autoplay = v.get("ap").and_then(In::as_bool).unwrap_or(true);
        if let Some(p) = v.get("p") {
            let d = Prefs::default();
            c.prefs = Prefs {
                captions: p.get("cc").and_then(In::as_bool).unwrap_or(d.captions),
                volume: p.get("vol").and_then(In::as_f64).filter(|x| (0.0..=1.0).contains(x)).unwrap_or(d.volume),
                muted: p.get("mu").and_then(In::as_bool).unwrap_or(d.muted),
                quality: p.get("q").and_then(In::as_u64).filter(|&q| q <= 4320).unwrap_or(0) as u32,
                codec: p.get("co").and_then(In::as_str).filter(|c| ["vp9", "av1", "h264"].contains(c)).unwrap_or("").to_string(),
                theater: p.get("th").and_then(In::as_bool).unwrap_or(d.theater),
                cc_style: p.get("cs").filter(|c| c.get("v").and_then(In::as_u64) == Some(2)).map(|c| {
                    let n = |k: &str, d: f64| c.get(k).and_then(In::as_f64).unwrap_or(d);
                    CcStyle { x: n("x", 0.03), y: n("y", 0.05), s: n("s", 1.0) }.clamped()
                }),
                sb_on: p.get("sbo").and_then(In::as_bool).unwrap_or(d.sb_on),
                sb_skip: p.get("sb").and_then(In::as_str).map(crate::player::sponsor::mask_from_keys).unwrap_or(d.sb_skip),
            };
        }
        c
    }

    pub fn to_json(&self) -> String {
        let p = &self.prefs;
        let mut j = json!({"v": self.version, "vd": self.visitor, "au": self.auth_user, "at": self.fetched_at,
            "p": {"cc": p.captions, "vol": p.volume, "mu": p.muted, "q": p.quality, "co": p.codec, "th": p.theater}});
        if let Some(c) = p.cc_style {
            let r = |v: f64| (v * 1000.0).round() / 1000.0;
            j["p"]["cs"] = json!({"v": 2, "x": r(c.x), "y": r(c.y), "s": r(c.s)});
        }
        if !p.sb_on {
            j["p"]["sbo"] = false.into();
        }
        if p.sb_skip != crate::player::sponsor::default_mask() {
            j["p"]["sb"] = crate::player::sponsor::mask_to_keys(p.sb_skip).into();
        }
        if let Some(s) = self.sts {
            j["sts"] = s.into();
        }
        if !self.own_player {
            j["op"] = false.into();
        }
        if self.vision_auth {
            j["va"] = true.into();
        }
        if !self.autoplay {
            j["ap"] = false.into();
        }
        j.to_string()
    }

    pub fn stale(&self, now_ms: f64) -> bool {
        self.fetched_at <= 0.0 || now_ms - self.fetched_at > MAX_AGE_MS || now_ms < self.fetched_at
    }

    /// Adopts visitor data echoed by InnerTube when we have none yet.
    pub fn learn_visitor(&mut self, vd: &str) -> bool {
        if self.visitor.is_none() && is_visitor(vd) {
            self.visitor = Some(vd.into());
            return true;
        }
        false
    }
}

/// Parses `https://www.youtube.com/sw.js_data` (`)]}'` + JSON). Today the
/// client record is at `[0][2][0][0]`: index 13 is visitor data, 16 the WEB
/// client version. Falls back to scanning that record by value shape.
pub fn parse_sw_data(bytes: &[u8]) -> Option<(String, Option<String>)> {
    let start = bytes.iter().position(|&b| b == b'[')?;
    let v = json::parse(&bytes[start..])?;
    let rec = v.get(0)?.get(2)?.get(0)?.get(0)?.as_array()?;
    let s = |i: usize| rec.get(i).and_then(In::as_str);
    let version = s(16).filter(|x| is_client_version(x)).or_else(|| rec.iter().filter_map(In::as_str).find(|x| is_client_version(x)))?;
    let visitor = s(13).filter(|x| is_visitor(x)).or_else(|| rec.iter().filter_map(In::as_str).find(|x| is_visitor(x)));
    Some((version.to_string(), visitor.map(Into::into)))
}

#[derive(Clone, Debug, Default)]
pub struct Auth {
    pub sapisid: Option<String>,
    pub p1: Option<String>,
    pub p3: Option<String>,
    /// TV app only (`crate::tv`): an OAuth access token from the shell's device sign-in. Requests
    /// carrying it are sent as the TV client (`tv::finish`), never with the cookie hashes.
    pub bearer: Option<String>,
}

impl Auth {
    pub fn from_cookies(cookie: &str) -> Auth {
        let mut a = Auth::default();
        for part in cookie.split(';') {
            let Some((k, v)) = part.trim().split_once('=') else { continue };
            if v.is_empty() {
                continue;
            }
            match k {
                "SAPISID" => a.sapisid = Some(v.into()),
                "__Secure-1PAPISID" => a.p1 = Some(v.into()),
                "__Secure-3PAPISID" => a.p3 = Some(v.into()),
                _ => {}
            }
        }
        a
    }

    pub fn signed_in(&self) -> bool {
        self.bearer.is_some() || self.sapisid.is_some() || self.p3.is_some()
    }

    /// `SAPISIDHASH <ts>_<sha1> SAPISID1PHASH … SAPISID3PHASH …`
    pub fn header(&self, ts_secs: u64) -> Option<String> {
        if let Some(t) = &self.bearer {
            return Some(format!("Bearer {t}"));
        }
        let one = |scheme: &str, sid: &str| {
            let hash = sha1_smol::Sha1::from(format!("{ts_secs} {sid} {ORIGIN}")).digest().to_string();
            format!("{scheme} {ts_secs}_{hash}")
        };
        let main = self.sapisid.as_deref().or(self.p3.as_deref())?;
        let mut parts = vec![one("SAPISIDHASH", main)];
        if let Some(p1) = &self.p1 {
            parts.push(one("SAPISID1PHASH", p1));
        }
        if let Some(p3) = &self.p3 {
            parts.push(one("SAPISID3PHASH", p3));
        }
        Some(parts.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sapisidhash_matches_reference() {
        // Reference: printf '1700000000 abc/DEF https://www.youtube.com' | shasum -a 1
        let a = Auth::from_cookies("PREF=x; SAPISID=abc/DEF; __Secure-3PAPISID=abc/DEF; other=1");
        assert!(a.signed_in());
        let h = a.header(1_700_000_000).unwrap();
        assert!(h.starts_with("SAPISIDHASH 1700000000_"));
        assert!(h.contains(" SAPISID3PHASH 1700000000_"));
        assert!(!h.contains("SAPISID1PHASH"));
        assert!(h.starts_with("SAPISIDHASH 1700000000_5fd5d7d359feeca62c40f4787443c839988983b5 SAPISID3PHASH 1700000000_5fd5d7d359feeca62c40f4787443c839988983b5"));
        assert!(Auth::from_cookies("PREF=1").header(1).is_none());
    }
    #[test]
    fn config_roundtrip() {
        let mut c = Config::default();
        c.visitor = Some("CgtlMmVGYWtlVmlzMCjAhPzWBjIKCgJaWhIEEgAgAA%3D%3D".into());
        c.fetched_at = 5.0;
        let back = Config::from_json(&c.to_json());
        assert_eq!(back.visitor, c.visitor);
        assert_eq!(back.version, FALLBACK_VERSION);
        assert!(Config::from_json("{\"v\":\"<script>\"}").version == FALLBACK_VERSION);
        c.prefs = Prefs { captions: false, volume: 0.5, muted: true, quality: 720, codec: "h264".into(), theater: true, cc_style: Some(CcStyle { x: 0.25, y: 0.5, s: 1.5 }), sb_on: false, sb_skip: 0b1001 };
        c.vision_auth = true;
        let back = Config::from_json(&c.to_json());
        assert_eq!(back.prefs, c.prefs);
        assert!(back.vision_auth && back.own_player);
        let bad = Config::from_json(r#"{"p":{"vol":7,"co":"<x>","q":99999},"op":false,"sts":5}"#);
        assert_eq!(bad.prefs.volume, 1.0);
        assert_eq!(bad.prefs.codec, "");
        assert_eq!(bad.prefs.quality, 0);
        assert!(!bad.own_player);
        assert_eq!(bad.sts, None);
        // Caption style: version-2 object only, clamped; other versions ignored.
        let cs = Config::from_json(r#"{"p":{"cs":{"v":2,"x":3,"y":-1,"s":9}}}"#).prefs.cc_style.unwrap();
        assert_eq!((cs.x, cs.y, cs.s), (0.95, 0.0, 2.0));
        assert_eq!(Config::from_json(r#"{"p":{"cs":{"v":1,"x":0.5}}}"#).prefs.cc_style, None);
        assert!(Config::default().prefs.captions, "captions default on");
        // SponsorBlock: on with SponsorBlock's defaults, nothing extra stored; off / another choice is stored small.
        let d = Config::default();
        assert!(d.prefs.sb_on && d.prefs.sb_skip == crate::player::sponsor::default_mask());
        assert!(!d.to_json().contains("\"sb"), "{}", d.to_json());
        let mut c = Config::default();
        c.prefs.sb_on = false;
        c.prefs.sb_skip = crate::player::sponsor::mask(&[crate::player::sponsor::Cat::Sponsor]);
        assert!(c.to_json().contains(r#""sbo":false"#) && c.to_json().contains(r#""sb":"sponsor""#), "{}", c.to_json());
        assert_eq!(Config::from_json(&c.to_json()).prefs, c.prefs);
        assert_eq!(Config::from_json(r#"{"p":{"sb":""}}"#).prefs.sb_skip, 0, "nothing chosen stays nothing");
        // 1 Nov 2026 = day 20758 → 20751 → never below the known value.
        assert_eq!(Config::default().sts(20758.5 * 86_400_000.0), FALLBACK_STS);
        assert_eq!(Config::default().sts(20800.0 * 86_400_000.0), 20793);
    }
}
