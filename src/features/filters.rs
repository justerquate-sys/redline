//! Home & Search display filters: shared minimum duration (Any…180 min), Home order
//! (recommended / popular by view count), Shorts, Music, Live and Members-only switches,
//! Home Playlists-only. They act on already-parsed items only: no request is ever
//! made for them. Unknown data stays visible (unknown duration, unknown view
//! count, no explicit Music label, not confirmed live, no members badge).

use crate::parse::Item;
use crate::json::Value;
use std::collections::HashMap;

pub const KEY_DURATION: &str = "yourTubeMinimumHomeDurationV1";
pub const KEY_ORDER: &str = "yourTubeHomeOrderV1";
pub const KEY_MUSIC: &str = "yourTubeShowMusicV1";
pub const KEY_LIVE: &str = "yourTubeShowLiveV1";
pub const KEY_PLAYLISTS: &str = "yourTubeHomePlaylistsOnlyV1";
/// The Shorts switch: the same boolean key the main YourTube extension uses
/// for its "Shorts in feeds" switch (default off, remembered).
pub const KEY_SHORTS: &str = "shortsInFeeds";
/// The Members-only switch (default on: labelled and shown).
pub const KEY_MEMBERS: &str = "yourTubeShowMembersV1";
pub const KEYS: [&str; 7] = [KEY_DURATION, KEY_ORDER, KEY_MUSIC, KEY_LIVE, KEY_PLAYLISTS, KEY_SHORTS, KEY_MEMBERS];
pub const MAX_MINUTES: u32 = 180;
pub const PRESETS: [u32; 5] = [0, 10, 20, 30, 60];

/// Display preferences. Defaults: Any length, Recommended, Music off, Live
/// off, Playlists-only off, Shorts off (the old extension's defaults), Members-only on. "Off" for
/// Music/Live/Shorts/Members-only means those videos are hidden.
#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub min_minutes: u32,
    pub popular: bool,
    pub show_music: bool,
    pub show_live: bool,
    pub playlists_only: bool,
    /// Shorts cards on Home, Search and next to videos (never on other pages).
    pub show_shorts: bool,
    /// Members-only videos: labelled and shown (on, the owner's public default), hidden on Home,
    /// Subscriptions, Search and next to videos (off).
    pub show_members: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { min_minutes: 0, popular: false, show_music: false, show_live: false, playlists_only: false, show_shorts: false, show_members: true }
    }
}

impl Prefs {
    /// Applies one stored value; false when the key is not a filter key.
    pub fn apply(&mut self, key: &str, v: &Value) -> bool {
        match key {
            KEY_DURATION => self.min_minutes = minutes(v),
            KEY_ORDER => self.popular = v.as_str() == Some("popular"),
            KEY_MUSIC => self.show_music = v.as_bool().unwrap_or(false),
            KEY_LIVE => self.show_live = v.as_bool().unwrap_or(false),
            KEY_PLAYLISTS => self.playlists_only = v.as_bool().unwrap_or(false),
            KEY_SHORTS => self.show_shorts = v.as_bool().unwrap_or(false),
            KEY_MEMBERS => self.show_members = v.as_bool().unwrap_or(true),
            _ => return false,
        }
        true
    }

    /// Stored JSON for one key.
    pub fn stored(&self, key: &str) -> String {
        match key {
            KEY_DURATION => self.min_minutes.to_string(),
            KEY_ORDER => if self.popular { "\"popular\"" } else { "\"recommended\"" }.into(),
            KEY_MUSIC => self.show_music.to_string(),
            KEY_LIVE => self.show_live.to_string(),
            KEY_PLAYLISTS => self.playlists_only.to_string(),
            KEY_SHORTS => self.show_shorts.to_string(),
            KEY_MEMBERS => self.show_members.to_string(),
            _ => "null".into(),
        }
    }

    /// Compact tab mirror (sessionStorage) so a reload filters before chrome.storage answers.
    pub fn mirror(&self) -> String {
        format!("{}|{}|{}|{}|{}|{}|{}", self.min_minutes, self.popular as u8, self.show_music as u8, self.show_live as u8, self.playlists_only as u8, self.show_shorts as u8, self.show_members as u8)
    }

    /// Reads the mirror; a five-field mirror from before the Shorts switch means Shorts off, one without
    /// the Members-only field means on.
    pub fn from_mirror(s: &str) -> Option<Prefs> {
        let p: Vec<&str> = s.split('|').collect();
        if !(5..=7).contains(&p.len()) {
            return None;
        }
        let b = |x: &str| x == "1";
        Some(Prefs { min_minutes: p[0].parse::<u32>().ok()?.min(MAX_MINUTES), popular: b(p[1]), show_music: b(p[2]), show_live: b(p[3]), playlists_only: b(p[4]), show_shorts: p.get(5).is_some_and(|x| b(x)), show_members: p.get(6).is_none_or(|x| b(x)) })
    }
}

/// Integer minutes 0–180; anything else is Any (0).
pub fn minutes(v: &Value) -> u32 {
    v.as_f64().filter(|m| m.fract() == 0.0 && (0.0..=MAX_MINUTES as f64).contains(m)).map(|m| m as u32).unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Surface {
    Home,
    Search,
    /// The watch page's related column.
    Related,
    /// Subscriptions: as YouTube sends it, but for the Members-only switch.
    Subs,
    /// History, channels, playlists: shown as YouTube sends them.
    Other,
}

/// "1:02:03" / "12:34" → seconds. Anything else (LIVE, UPCOMING, empty) → None.
pub fn duration_secs(s: &str) -> Option<u32> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut total = 0u32;
    for (i, p) in parts.iter().enumerate() {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) || (i > 0 && p.len() != 2) {
            return None;
        }
        total = total.checked_mul(60)?.checked_add(p.parse::<u32>().ok()?)?;
    }
    Some(total)
}

/// A view count from a card's meta line ("1.2M views · 3 days ago",
/// "149K views", "1.234 Aufrufe", "1,2 Mio. Aufrufe", "No views"). Also a bare
/// "count · age" pair (lockups that drop the "views" word). None if unknown.
pub fn view_count(meta: &str) -> Option<u64> {
    let parts: Vec<&str> = meta.split(" · ").map(str::trim).collect();
    for (i, p) in parts.iter().enumerate() {
        let lower = p.to_lowercase();
        if lower == "no views" || lower == "keine aufrufe" {
            return Some(0);
        }
        let views_word = ["view", "aufruf", "vue", "visualizac", "visualizzazion", "weergave", "wyświetle", "просмотр"].iter().any(|w| lower.contains(w));
        let age_next = parts.get(i + 1).is_some_and(|n| looks_like_age(n));
        if views_word || (i == 0 && age_next) {
            if let Some(n) = count_prefix(p) {
                return Some(n);
            }
        }
    }
    None
}

fn looks_like_age(s: &str) -> bool {
    let l = s.to_lowercase();
    l.ends_with(" ago") || l.starts_with("vor ") || l.starts_with("il y a") || l.starts_with("hace ") || l.contains("streamed") || l.contains("gestreamt")
}

/// Leading number with an optional magnitude word: "1.2M", "149K", "1,234",
/// "1.234" (German grouping), "1,2 Mio.", "12 Tsd.".
fn count_prefix(s: &str) -> Option<u64> {
    let s = s.trim();
    let num_end = s.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ',')).unwrap_or(s.len());
    let num = &s[..num_end];
    if num.is_empty() || !num.as_bytes()[0].is_ascii_digit() {
        return None;
    }
    let rest = s[num_end..].trim_start();
    let word: String = rest.chars().take_while(|c| c.is_alphabetic() || *c == '.').collect();
    let w = word.trim_end_matches('.').to_lowercase();
    let scale: f64 = match w.as_str() {
        "k" | "tsd" | "mil" | "k." => 1e3,
        "m" | "mio" | "mn" => 1e6,
        "b" | "mrd" | "bn" => 1e9,
        _ => 1.0,
    };
    let value = if scale > 1.0 {
        // With a magnitude, a single separator followed by 1–2 digits is a decimal point.
        num.replace(',', ".").parse::<f64>().ok()?
    } else {
        // Without one, separators are digit grouping ("1,234", "1.234").
        let digits: String = num.chars().filter(char::is_ascii_digit).collect();
        digits.parse::<f64>().ok()?
    };
    let n = (value * scale).round();
    (n.is_finite() && n >= 0.0 && n < 9.0e15).then_some(n as u64)
}

/// Whether a Short is hidden on `surf` by the Shorts switch alone. Off: hidden
/// everywhere (as before Shorts were parsed). On: shown on Home, Search and
/// next to videos only; other pages never show them (the app drops them on
/// arrival there).
pub fn short_hidden(surf: Surface, p: &Prefs) -> bool {
    !p.show_shorts || matches!(surf, Surface::Subs | Surface::Other)
}

/// Whether `item` stays visible on `surf`. `blocked(id)` reports a confirmed
/// blocked-website verdict for a video.
pub fn keep(item: &Item, surf: Surface, p: &Prefs, blocked: &dyn Fn(&str) -> bool) -> bool {
    if item.is_short() && short_hidden(surf, p) {
        return false;
    }
    if surf == Surface::Other {
        return true;
    }
    match item {
        Item::Video(v) => {
            if !p.show_members && v.members {
                return false;
            }
            if surf == Surface::Subs {
                return true;
            }
            if blocked(&v.id) {
                return false;
            }
            if surf == Surface::Home && p.playlists_only {
                return false;
            }
            if !p.show_music && v.music {
                return false;
            }
            // Search keeps its own (native) Live choice.
            if !p.show_live && v.live && matches!(surf, Surface::Home | Surface::Related) {
                return false;
            }
            // A Short has no duration to compare, and the switch asked for them: kept at any minimum length.
            if matches!(surf, Surface::Home | Surface::Search) && p.min_minutes > 0 && !v.live && !v.short {
                if let Some(d) = duration_secs(&v.duration) {
                    return d >= p.min_minutes * 60;
                }
            }
            true
        }
        Item::Playlist(_) => true,
        Item::Channel(_) => !(surf == Surface::Home && p.playlists_only),
    }
}

/// Whether any filter could hide something on `surf` (cheap check before work).
pub fn active(surf: Surface, p: &Prefs, any_blocked: bool) -> bool {
    match surf {
        Surface::Home => any_blocked || p.min_minutes > 0 || !p.show_music || !p.show_live || p.playlists_only || !p.show_shorts || !p.show_members,
        Surface::Search => any_blocked || p.min_minutes > 0 || !p.show_music || !p.show_shorts || !p.show_members,
        Surface::Related => any_blocked || !p.show_music || !p.show_live || !p.show_shorts || !p.show_members,
        Surface::Subs => !p.show_members,
        Surface::Other => false,
    }
}

/// Native arrival order of the current feed (Relevance restores it).
#[derive(Default)]
pub struct Order {
    seq: HashMap<String, u32>,
    next: u32,
}

impl Order {
    pub fn reset(&mut self, items: &[Item]) {
        self.seq.clear();
        self.next = 0;
        self.add(items);
    }
    pub fn add(&mut self, items: &[Item]) {
        for it in items {
            if !self.seq.contains_key(it.key()) {
                self.seq.insert(it.key().to_string(), self.next);
                self.next += 1;
            }
        }
    }
    fn of(&self, it: &Item) -> u32 {
        self.seq.get(it.key()).copied().unwrap_or(u32::MAX)
    }
}

/// Stable ascending sort by `key`, through one `(f64, u32)` sort for every
/// feature list (each `sort_by` on a new type costs Wasm size).
pub fn sort_keyed<T>(v: &mut Vec<T>, key: impl Fn(&T) -> f64) {
    let mut k: Vec<(f64, u32)> = v.iter().enumerate().map(|(i, x)| (key(x), i as u32)).collect();
    k.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut slots: Vec<Option<T>> = v.drain(..).map(Some).collect();
    v.extend(k.into_iter().filter_map(|(_, i)| slots[i as usize].take()));
}

fn views_of(it: &Item) -> Option<u64> {
    match it {
        Item::Video(v) if !v.live => view_count(&v.meta),
        _ => None,
    }
}

/// Sorts `items` by popularity: known view counts high → low, then cards
/// without a count in their native order (stable ties by native order).
pub fn sort_popular(items: &mut Vec<Item>, order: &Order) {
    // Known counts: −views·1e5 + native index (counts capped at 8e10, < 1e5 items);
    // unknown counts after all known ones, in native order.
    sort_keyed(items, |it| {
        let idx = order.of(it).min(99_999) as f64;
        match views_of(it) {
            Some(v) => -((v.min(80_000_000_000)) as f64) * 1e5 + idx,
            None => 1e16 + idx,
        }
    });
}

pub fn sort_native(items: &mut Vec<Item>, order: &Order) {
    sort_keyed(items, |it| order.of(it) as f64);
}

/// Re-sorts everything loaded (on screen + preloaded) and splits it again at
/// the same on-screen count: the first screen shows the most popular of all
/// loaded cards, later chunks continue in that order.
pub fn arrange(shown: &mut Vec<Item>, buf: &mut Vec<Item>, order: &Order, popular: bool) {
    let n = shown.len();
    let mut all: Vec<Item> = std::mem::take(shown);
    all.append(buf);
    if popular { sort_popular(&mut all, order) } else { sort_native(&mut all, order) }
    *buf = all.split_off(n.min(all.len()));
    *shown = all;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{PlaylistCard, Video};

    fn vid(id: &str, dur: &str, meta: &str) -> Item {
        Item::Video(Video { id: id.into(), duration: dur.into(), meta: meta.into(), ..Default::default() })
    }

    #[test]
    fn durations() {
        assert_eq!(duration_secs("12:34"), Some(754));
        assert_eq!(duration_secs("1:02:03"), Some(3723));
        assert_eq!(duration_secs("0:59"), Some(59));
        assert_eq!(duration_secs("LIVE"), None);
        assert_eq!(duration_secs(""), None);
        assert_eq!(duration_secs("1:2"), None);
        assert_eq!(duration_secs("SHORTS"), None);
    }

    #[test]
    fn view_counts() {
        assert_eq!(view_count("1.2M views · 3 days ago"), Some(1_200_000));
        assert_eq!(view_count("149K views"), Some(149_000));
        assert_eq!(view_count("1,234 views · 1 year ago"), Some(1234));
        assert_eq!(view_count("No views · 1 minute ago"), Some(0));
        assert_eq!(view_count("1.234 Aufrufe · vor 2 Tagen"), Some(1234));
        assert_eq!(view_count("1,2 Mio. Aufrufe · vor 2 Tagen"), Some(1_200_000));
        assert_eq!(view_count("12 Tsd. Aufrufe"), Some(12_000));
        // Count/age pair without the word "views" (lockups).
        assert_eq!(view_count("3.4K · 2 weeks ago"), Some(3400));
        assert_eq!(view_count("Streamed 2 days ago"), None);
        assert_eq!(view_count("Updated today"), None);
        assert_eq!(view_count(""), None);
    }

    #[test]
    fn prefs_storage_and_mirror() {
        let mut p = Prefs::default();
        assert!(p.apply(KEY_DURATION, &crate::json::jv!(20)));
        assert!(p.apply(KEY_ORDER, &crate::json::jv!("popular")));
        assert!(p.apply(KEY_MUSIC, &crate::json::jv!(true)));
        assert!(!p.apply("other", &crate::json::jv!(1)));
        assert_eq!(p.min_minutes, 20);
        assert!(p.popular && p.show_music && !p.show_live);
        assert_eq!(p.stored(KEY_ORDER), "\"popular\"");
        assert_eq!(Prefs::from_mirror(&p.mirror()), Some(p.clone()));
        // Out-of-range or fractional minutes → Any.
        p.apply(KEY_DURATION, &crate::json::jv!(181));
        assert_eq!(p.min_minutes, 0);
        p.apply(KEY_DURATION, &crate::json::jv!(10.5));
        assert_eq!(p.min_minutes, 0);
        p.apply(KEY_DURATION, &crate::json::jv!("30"));
        assert_eq!(p.min_minutes, 0);
        assert_eq!(Prefs::from_mirror("garbage"), None);
        // Shorts: stored under the main extension's key, mirrored; old five-field mirrors read as off.
        assert!(!p.show_shorts, "Shorts off by default");
        assert!(p.apply(KEY_SHORTS, &crate::json::jv!(true)));
        assert!(p.show_shorts);
        assert_eq!(p.stored(KEY_SHORTS), "true");
        assert_eq!(Prefs::from_mirror(&p.mirror()), Some(p.clone()));
        assert_eq!(Prefs::from_mirror("20|1|1|0|0").map(|p| (p.show_shorts, p.min_minutes, p.popular)), Some((false, 20, true)));
        p.apply(KEY_SHORTS, &crate::json::jv!("yes"));
        assert!(!p.show_shorts, "non-boolean → off");
    }

    #[test]
    fn shorts_switch() {
        let none = |_: &str| false;
        let short = Item::Video(Video { id: "shortshort1".into(), duration: "Shorts".into(), short: true, meta: "402K views".into(), ..Default::default() });
        let all = [Surface::Home, Surface::Search, Surface::Related, Surface::Other];
        // Off (default): hidden everywhere.
        let mut p = Prefs::default();
        for s in all {
            assert!(!keep(&short, s, &p, &none), "{s:?}");
            assert!(s == Surface::Other || active(s, &p, false), "{s:?} filters while Shorts are off");
        }
        // On: Home, Search and related only.
        p.show_shorts = true;
        assert!(keep(&short, Surface::Home, &p, &none));
        assert!(keep(&short, Surface::Search, &p, &none));
        assert!(keep(&short, Surface::Related, &p, &none));
        assert!(!keep(&short, Surface::Other, &p, &none), "never on Subscriptions/History/channels");
        // Kept at any minimum length (no duration; the switch asked for them).
        p.min_minutes = 60;
        assert!(keep(&short, Surface::Home, &p, &none));
        assert!(keep(&short, Surface::Search, &p, &none));
        // The other filters still apply.
        let blocked = |id: &str| id == "shortshort1";
        assert!(!keep(&short, Surface::Home, &p, &blocked));
        p.playlists_only = true;
        assert!(!keep(&short, Surface::Home, &p, &none));
        p.playlists_only = false;
        let Item::Video(mut sv) = short.clone() else { unreachable!() };
        sv.music = true;
        assert!(!keep(&Item::Video(sv), Surface::Search, &p, &none), "Music label still hides it");
        // Popularity sorts them by their view text like other cards.
        assert_eq!(views_of(&short), Some(402_000));
        // A normal video is unaffected by the switch.
        let long = vid("bbbbbbbbbbb", "70:00", "");
        p.show_shorts = false;
        assert!(keep(&long, Surface::Home, &p, &none));
    }

    #[test]
    fn members_switch() {
        let none = |_: &str| false;
        let locked = Item::Video(Video { id: "J3M2L2pLhxA".into(), duration: "3:22".into(), members: true, ..Default::default() });
        let open = vid("aaaaaaaaaaa", "3:22", "");
        // On by default: labelled and shown everywhere, nothing to filter for it.
        let mut p = Prefs { show_music: true, show_live: true, show_shorts: true, ..Default::default() };
        assert!(p.show_members);
        for s in [Surface::Home, Surface::Search, Surface::Related, Surface::Subs, Surface::Other] {
            assert!(keep(&locked, s, &p, &none), "{s:?}");
            assert!(!active(s, &p, false), "{s:?}");
        }
        // Off: hidden on Home, Subscriptions, Search and next to videos; other pages as YouTube sends them.
        p.show_members = false;
        for s in [Surface::Home, Surface::Search, Surface::Related, Surface::Subs] {
            assert!(!keep(&locked, s, &p, &none), "{s:?}");
            assert!(active(s, &p, false), "{s:?}");
            assert!(keep(&open, s, &p, &none), "{s:?}: only labelled cards");
        }
        assert!(keep(&locked, Surface::Other, &p, &none), "channels, playlists, History");
        assert!(!active(Surface::Other, &p, false));
        // Subscriptions keeps every other rule off (minimum length, Music), and never shows Shorts.
        let mut q = Prefs { min_minutes: 60, ..Default::default() };
        assert!(keep(&open, Surface::Subs, &q, &none));
        q.show_shorts = true;
        assert!(short_hidden(Surface::Subs, &q));
        // Stored, mirrored; a mirror or a value from before the switch means on.
        assert!(!Prefs::from_mirror(&p.mirror()).unwrap().show_members);
        assert!(Prefs::from_mirror("20|1|1|0|0|1").unwrap().show_members);
        assert!(Prefs::from_mirror("20|1|1|0|0").unwrap().show_members);
        assert_eq!(p.stored(KEY_MEMBERS), "false");
        assert!(p.apply(KEY_MEMBERS, &crate::json::jv!("yes")));
        assert!(p.show_members, "non-boolean → the default");
    }

    #[test]
    fn keep_rules() {
        let none = |_: &str| false;
        let mut p = Prefs { min_minutes: 10, ..Default::default() };
        let short = vid("aaaaaaaaaaa", "4:00", "");
        let long = vid("bbbbbbbbbbb", "12:00", "");
        let unknown = vid("ccccccccccc", "", "");
        assert!(!keep(&short, Surface::Home, &p, &none));
        assert!(!keep(&short, Surface::Search, &p, &none));
        assert!(keep(&short, Surface::Related, &p, &none), "duration is Home/Search only");
        assert!(keep(&short, Surface::Other, &p, &none));
        assert!(keep(&long, Surface::Home, &p, &none));
        assert!(keep(&unknown, Surface::Home, &p, &none), "unknown duration stays");
        let mut live = Video { id: "ddddddddddd".into(), live: true, ..Default::default() };
        assert!(!keep(&Item::Video(live.clone()), Surface::Home, &p, &none));
        assert!(!keep(&Item::Video(live.clone()), Surface::Related, &p, &none));
        assert!(keep(&Item::Video(live.clone()), Surface::Search, &p, &none), "Search keeps native Live");
        p.show_live = true;
        assert!(keep(&Item::Video(live.clone()), Surface::Home, &p, &none), "live has no duration to filter");
        live.live = false;
        live.music = true;
        assert!(!keep(&Item::Video(live.clone()), Surface::Search, &p, &none));
        p.show_music = true;
        assert!(keep(&Item::Video(live), Surface::Search, &p, &none));
        let pl = Item::Playlist(PlaylistCard { id: "PLx".into(), ..Default::default() });
        p.playlists_only = true;
        assert!(keep(&pl, Surface::Home, &p, &none));
        assert!(!keep(&long, Surface::Home, &p, &none));
        assert!(keep(&long, Surface::Search, &p, &none), "playlists-only is Home only");
        let blocked = |id: &str| id == "bbbbbbbbbbb";
        assert!(!keep(&long, Surface::Search, &p, &blocked));
        assert!(!keep(&long, Surface::Related, &p, &blocked));
    }

    #[test]
    fn popular_order_and_restore() {
        let a = vid("aaaaaaaaaaa", "1:00", "10 views · 1 day ago");
        let b = vid("bbbbbbbbbbb", "1:00", "2M views · 1 day ago");
        let c = vid("ccccccccccc", "1:00", "");
        let d = vid("ddddddddddd", "1:00", "5K views · 1 day ago");
        let e = vid("eeeeeeeeeee", "1:00", "5K views · 2 days ago");
        let mut shown = vec![a.clone(), b.clone(), c.clone()];
        let mut buf = vec![d.clone(), e.clone()];
        let mut order = Order::default();
        order.reset(&shown);
        order.add(&buf);
        arrange(&mut shown, &mut buf, &order, true);
        let keys = |v: &[Item]| v.iter().map(|i| i.key().chars().next().unwrap()).collect::<String>();
        assert_eq!(keys(&shown), "bde", "most popular of all loaded first, ties stable");
        assert_eq!(keys(&buf), "ac", "unknown counts last");
        arrange(&mut shown, &mut buf, &order, false);
        assert_eq!(keys(&shown), "abc");
        assert_eq!(keys(&buf), "de");
    }
}
