//! Tab-lifetime feed snapshots (sessionStorage only, never chrome.storage or
//! localStorage). A snapshot is parsed data, not HTML: on load every field is
//! validated again exactly like a fresh InnerTube response and the page is
//! re-rendered through `render.rs`, so escaping stays in one place.

use crate::parse::{ChannelCard, ChannelTabLink, Item, PlaylistCard, SortOpt, Video, anim_url};
use crate::render::{ChannelNav, Head};
use crate::route::{ChannelTab, valid_sp};
use crate::util::{is_safe_id, is_video_id, safe_img, safe_rel_href};
use crate::json::{self, Value as In};
use serde::{Serialize, Serializer, ser::SerializeMap};

pub const PREFIX: &str = "ytc:snap:";
pub const INDEX: &str = "ytc:snaps";
/// Snapshots kept per tab (most recent pages).
pub const MAX_SNAPS: usize = 6;
/// Items stored per snapshot.
pub const MAX_ITEMS: usize = 150;
/// Serialized size cap per snapshot (sessionStorage is ~5 MB per origin and
/// shared with YouTube's own pages in this tab).
pub const MAX_BYTES: usize = 400_000;
const VERSION: u64 = 1;

#[derive(Clone, Debug, PartialEq)]
pub struct Snap {
    pub url: String,
    pub head: Head,
    /// Rendered items first, then preloaded ones.
    pub items: Vec<Item>,
    /// How many of `items` were on screen.
    pub shown: usize,
    pub cont: Option<String>,
    pub cont_ep: &'static str,
    pub scroll: f64,
    /// Epoch ms when written.
    pub at: f64,
}

/// Outgoing snapshots borrow the feed while it is serialized; restoring still uses owned `Snap`.
pub(crate) struct SnapRef<'a> {
    pub url: &'a str,
    pub head: &'a Head,
    pub items: &'a [Item],
    pub buffered: &'a [Item],
    pub shown: usize,
    pub cont: Option<&'a str>,
    pub cont_ep: &'static str,
    pub scroll: f64,
    pub at: f64,
}

pub fn key(url: &str) -> String {
    format!("{PREFIX}{url}")
}

fn opt<M: SerializeMap>(m: &mut M, k: &str, v: &Option<String>) -> Result<(), M::Error> {
    if let Some(v) = v {
        m.serialize_entry(k, v)?;
    }
    Ok(())
}

fn nonempty<M: SerializeMap>(m: &mut M, k: &str, v: &str) -> Result<(), M::Error> {
    if !v.is_empty() {
        m.serialize_entry(k, v)?;
    }
    Ok(())
}

/// Borrowed records write the same compact schema without building a tree of cloned strings.
struct ItemRecord<'a>(&'a Item);

impl Serialize for ItemRecord<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut m = serializer.serialize_map(None)?;
        // Keep the old map's byte-sorted field order as well as its omissions.
        match self.0 {
            Item::Video(v) => {
                opt(&mut m, "a", &v.anim)?;
                opt(&mut m, "ar", &v.art)?;
                opt(&mut m, "b", &v.channel_bid)?;
                nonempty(&mut m, "c", &v.channel)?;
                nonempty(&mut m, "d", &v.duration)?;
                opt(&mut m, "h", &v.channel_href)?;
                m.serialize_entry("i", &v.id)?;
                m.serialize_entry("k", "v")?;
                if v.live {
                    m.serialize_entry("l", &true)?;
                }
                nonempty(&mut m, "m", &v.meta)?;
                if v.members {
                    m.serialize_entry("mo", &true)?;
                }
                if v.progress > 0 {
                    m.serialize_entry("p", &v.progress)?;
                }
                if v.short {
                    m.serialize_entry("r", &true)?;
                }
                opt(&mut m, "s", &v.thumb)?;
                if v.thumb_w > 0 {
                    m.serialize_entry("sw", &v.thumb_w)?;
                }
                nonempty(&mut m, "t", &v.title)?;
                if v.music {
                    m.serialize_entry("u", &true)?;
                }
                opt(&mut m, "x", &v.thumb2x)?;
                if v.thumb2x_w > 0 {
                    m.serialize_entry("xw", &v.thumb2x_w)?;
                }
            }
            Item::Playlist(p) => {
                nonempty(&mut m, "c", &p.channel)?;
                opt(&mut m, "f", &p.first_video)?;
                opt(&mut m, "h", &p.channel_href)?;
                m.serialize_entry("i", &p.id)?;
                m.serialize_entry("k", "p")?;
                nonempty(&mut m, "n", &p.count)?;
                nonempty(&mut m, "q", p.kind)?;
                opt(&mut m, "s", &p.thumb)?;
                nonempty(&mut m, "t", &p.title)?;
            }
            Item::Channel(c) => {
                opt(&mut m, "b", &c.bid)?;
                m.serialize_entry("h", &c.href)?;
                m.serialize_entry("k", "c")?;
                nonempty(&mut m, "m", &c.meta)?;
                opt(&mut m, "s", &c.avatar)?;
                nonempty(&mut m, "t", &c.title)?;
            }
        }
        m.end()
    }
}

pub(crate) struct ItemRecords<'a>(pub &'a [Item]);

impl Serialize for ItemRecords<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().map(ItemRecord))
    }
}

const TEXT_MAX: usize = 5000;

fn s(v: &In, k: &str) -> String {
    v.get(k).and_then(In::as_str).map(|x| x.chars().take(TEXT_MAX).collect()).unwrap_or_default()
}

fn so<'a>(v: &'a In<'a>, k: &str) -> Option<&'a str> {
    v.get(k).and_then(In::as_str)
}

fn w(v: &In, k: &str) -> u16 {
    v.get(k).and_then(In::as_u64).unwrap_or(0).min(4000) as u16
}

pub(crate) fn item_from(v: &In) -> Option<Item> {
    match so(v, "k")? {
        "v" => {
            let id = so(v, "i").filter(|x| is_video_id(x))?;
            Some(Item::Video(Video {
                id: id.into(),
                title: s(v, "t"),
                channel: s(v, "c"),
                channel_href: so(v, "h").and_then(safe_rel_href),
                channel_bid: so(v, "b").filter(|x| is_safe_id(x)).map(Into::into),
                meta: s(v, "m"),
                duration: s(v, "d"),
                live: v.get("l").and_then(In::as_bool).unwrap_or(false),
                progress: v.get("p").and_then(In::as_u64).unwrap_or(0).min(100) as u8,
                thumb: so(v, "s").and_then(safe_img),
                thumb_w: w(v, "sw"),
                thumb2x: so(v, "x").and_then(safe_img),
                thumb2x_w: w(v, "xw"),
                anim: anim_url(so(v, "a")),
                music: v.get("u").and_then(In::as_bool).unwrap_or(false),
                short: v.get("r").and_then(In::as_bool).unwrap_or(false),
                members: v.get("mo").and_then(In::as_bool).unwrap_or(false),
                art: so(v, "ar").and_then(safe_img),
            }))
        }
        "p" => {
            let id = so(v, "i").filter(|x| is_safe_id(x))?;
            Some(Item::Playlist(PlaylistCard {
                id: id.into(),
                title: s(v, "t"),
                channel: s(v, "c"),
                channel_href: so(v, "h").and_then(safe_rel_href),
                count: s(v, "n"),
                thumb: so(v, "s").and_then(safe_img),
                first_video: so(v, "f").filter(|x| is_video_id(x)).map(Into::into),
                kind: so(v, "q").and_then(|q| crate::parse::PLAYLIST_KINDS.iter().find(|k| **k == q)).copied().unwrap_or(""),
            }))
        }
        "c" => Some(Item::Channel(ChannelCard {
            href: so(v, "h").and_then(safe_rel_href)?,
            bid: so(v, "b").filter(|x| is_safe_id(x)).map(Into::into),
            title: s(v, "t"),
            meta: s(v, "m"),
            avatar: so(v, "s").and_then(safe_img),
        })),
        _ => None,
    }
}

struct HeadRecord<'a>(&'a Head);

impl Serialize for HeadRecord<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut m = serializer.serialize_map(None)?;
        match self.0 {
            // A failed sort's note is not kept: a restored page is a plain feed.
            Head::Feed { title } | Head::Unsorted { title } => {
                m.serialize_entry("k", "f")?;
                m.serialize_entry("t", title)?;
            }
            Head::Channel { title, avatar, meta, about, nav } => {
                m.serialize_entry("a", avatar)?;
                m.serialize_entry("ab", about)?;
                m.serialize_entry("k", "c")?;
                m.serialize_entry("m", meta)?;
                if !nav.path.is_empty() {
                    m.serialize_entry("n", &nav_record(nav))?;
                }
                m.serialize_entry("t", title)?;
            }
            Head::Playlist { title, meta } => {
                m.serialize_entry("k", "p")?;
                m.serialize_entry("m", meta)?;
                m.serialize_entry("t", title)?;
            }
        }
        m.end()
    }
}

/// A channel page's tabs and sort choices (this tab's `sessionStorage`, like its continuation token).
fn nav_record(n: &ChannelNav) -> serde_json::Value {
    serde_json::json!({
        "p": n.path,
        "s": n.tab.slug(),
        "t": n.tabs.iter().map(|t| [t.tab.slug(), t.title.as_str(), t.params.as_str()]).collect::<Vec<_>>(),
        "o": n.sorts.iter().map(|o| serde_json::json!({"l": o.label, "s": o.selected, "c": o.token, "b": o.params})).collect::<Vec<_>>(),
    })
}

fn nav_from(v: &In) -> ChannelNav {
    let Some(n) = v.get("n") else { return ChannelNav::default() };
    let tabs = n.get("t").and_then(In::as_array).map(|a| {
        a.iter()
            .take(12)
            .filter_map(|t| {
                let tab = ChannelTab::from_slug(t.get(0)?.as_str()?)?;
                let params = t.get(2)?.as_str().filter(|p| valid_sp(p))?;
                Some(ChannelTabLink { tab, title: t.get(1)?.as_str()?.chars().take(40).collect(), params: params.to_string() })
            })
            .collect()
    });
    let sorts = n.get("o").and_then(In::as_array).map(|a| {
        a.iter()
            .take(8)
            .filter_map(|o| {
                Some(SortOpt {
                    label: o.get("l")?.as_str()?.chars().take(40).collect(),
                    selected: o.get("s").and_then(In::as_bool).unwrap_or(false),
                    token: so(o, "c").filter(|t| (8..4096).contains(&t.len())).map(String::from),
                    params: so(o, "b").filter(|p| valid_sp(p)).map(String::from),
                })
            })
            .collect()
    });
    ChannelNav {
        path: so(n, "p").and_then(safe_rel_href).unwrap_or_default(),
        tab: so(n, "s").and_then(ChannelTab::from_slug).unwrap_or_default(),
        tabs: tabs.unwrap_or_default(),
        sorts: sorts.unwrap_or_default(),
    }
}

fn strings(v: &In, k: &str) -> Vec<String> {
    v.get(k).and_then(In::as_array).map(|a| a.iter().filter_map(In::as_str).take(8).map(|x| x.chars().take(TEXT_MAX).collect()).collect()).unwrap_or_default()
}

fn head_from(v: &In) -> Option<Head> {
    Some(match so(v, "k")? {
        "f" => Head::Feed { title: s(v, "t") },
        "c" => Head::Channel { title: s(v, "t"), avatar: so(v, "a").and_then(safe_img), meta: strings(v, "m"), about: s(v, "ab"), nav: nav_from(v) },
        "p" => Head::Playlist { title: s(v, "t"), meta: strings(v, "m") },
        _ => return None,
    })
}

pub fn ep(s: &str) -> Option<&'static str> {
    ["browse", "search", "next"].into_iter().find(|e| *e == s)
}

impl Serialize for Snap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        SnapRef {
            url: &self.url, head: &self.head, items: &self.items, buffered: &[], shown: self.shown,
            cont: self.cont.as_deref(), cont_ep: self.cont_ep, scroll: self.scroll, at: self.at,
        }.serialize(serializer)
    }
}

struct SnapshotItems<'a>(&'a [Item], &'a [Item]);

impl Serialize for SnapshotItems<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.0.iter().chain(self.1).take(MAX_ITEMS).map(ItemRecord))
    }
}

impl Serialize for SnapRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut m = serializer.serialize_map(Some(9))?;
        m.serialize_entry("c", &self.cont)?;
        m.serialize_entry("e", self.cont_ep)?;
        m.serialize_entry("h", &HeadRecord(self.head))?;
        m.serialize_entry("i", &SnapshotItems(self.items, self.buffered))?;
        m.serialize_entry("n", &self.shown.min(MAX_ITEMS))?;
        m.serialize_entry("t", &self.at)?;
        m.serialize_entry("u", &self.url)?;
        m.serialize_entry("v", &VERSION)?;
        m.serialize_entry("y", &self.scroll.max(0.0).round())?;
        m.end()
    }
}

pub fn to_json(sn: &Snap) -> String {
    serde_json::to_string(sn).expect("snapshot serializes to JSON")
}

pub(crate) fn to_json_ref(sn: &SnapRef<'_>) -> String {
    serde_json::to_string(sn).expect("snapshot serializes to JSON")
}

/// Parses and re-validates a snapshot written for `url`.
pub fn from_json(s: &str, url: &str) -> Option<Snap> {
    let v = json::parse_str(s)?;
    if v.get("v").and_then(In::as_u64) != Some(VERSION) || so(&v, "u") != Some(url) {
        return None;
    }
    let items: Vec<Item> = v.get("i")?.as_array()?.iter().take(MAX_ITEMS).filter_map(item_from).collect();
    let shown = (v.get("n").and_then(In::as_u64).unwrap_or(0) as usize).min(items.len());
    Some(Snap {
        url: url.to_string(),
        head: head_from(v.get("h")?)?,
        cont: so(&v, "c").filter(|c| !c.is_empty() && c.len() < 8192).map(Into::into),
        cont_ep: ep(so(&v, "e").unwrap_or(""))?,
        scroll: v.get("y").and_then(In::as_f64).unwrap_or(0.0).clamp(0.0, 1e7),
        at: v.get("t").and_then(In::as_f64).unwrap_or(0.0),
        items,
        shown,
    })
}

/// Most-recent-first list of snapshot URLs.
pub fn index_from(s: &str) -> Vec<String> {
    json::parse_str(s)
        .and_then(|v| v.as_array().map(|a| a.iter().filter_map(In::as_str).map(String::from).collect()))
        .unwrap_or_default()
}

pub fn index_json(urls: &[String]) -> String {
    serde_json::to_string(urls).expect("snapshot index serializes to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn borrowed_feed_preparation_matches_owned_snapshot_bytes() {
        let items: Vec<_> = (0..MAX_ITEMS + 30).map(|i| Item::Video(Video {
            id: format!("{i:011}"), title: format!("Video {i} — \"東京\"\n"), ..Default::default()
        })).collect();
        let head = Head::Channel { title: "Channel".into(), avatar: None, meta: vec!["123 videos".into()], about: "About this channel".into(), nav: Default::default() };
        for shown in [0, 1, 24, MAX_ITEMS - 1, MAX_ITEMS, MAX_ITEMS + 1] {
            for buffered in [0, 1, 24, 100] {
                let shown_items = &items[..shown];
                let buffered_items = &items[shown..(shown + buffered).min(items.len())];
                // The old App::snapshot preparation: shown items, then buffered items, capped once.
                let owned_items: Vec<_> = shown_items.iter().chain(buffered_items).take(MAX_ITEMS).cloned().collect();
                let owned = Snap { url: "/feed/subscriptions".into(), head: head.clone(), shown: shown.min(owned_items.len()), items: owned_items, cont: Some("tok".into()), cont_ep: "browse", scroll: 123.4, at: 1000.0 };
                let borrowed = SnapRef { url: &owned.url, head: &head, items: shown_items, buffered: buffered_items, shown: shown.min(MAX_ITEMS), cont: owned.cont.as_deref(), cont_ep: "browse", scroll: 123.4, at: 1000.0 };
                assert_eq!(to_json_ref(&borrowed), to_json(&owned), "shown={shown}, buffered={buffered}");
                assert_eq!(from_json(&to_json_ref(&borrowed), &owned.url), Some(Snap { scroll: 123.0, ..owned }));
            }
        }
    }

    /// `cargo test --release --locked snapshot_preparation_benchmark -- --ignored --nocapture`
    #[test]
    #[ignore = "manual navigation snapshot preparation benchmark"]
    fn snapshot_preparation_benchmark() {
        use std::{hint::black_box, time::Instant};
        fn measure(name: &str, encode: impl Fn() -> String) {
            const RUNS: usize = 1000;
            let start = Instant::now();
            for _ in 0..RUNS { black_box(encode()); }
            println!("{name}: {:.3} ms per snapshot", start.elapsed().as_secs_f64() * 1000.0 / RUNS as f64);
        }
        let feed = crate::parse::search(&crate::util::test_fixture("search.json"));
        let items: Vec<_> = feed.items.iter().cycle().take(MAX_ITEMS).cloned().collect();
        let (shown, buffered) = items.split_at(24);
        let head = Head::Feed { title: "Search".into() };
        let view = SnapRef { url: "/results?search_query=rust", head: &head, items: shown, buffered, shown: shown.len(), cont: feed.cont.as_deref(), cont_ep: "search", scroll: 123.0, at: 1000.0 };
        let prepare_owned = || {
            let items: Vec<_> = shown.iter().chain(buffered).take(MAX_ITEMS).cloned().collect();
            Snap { url: view.url.into(), head: head.clone(), shown: shown.len().min(items.len()), items, cont: feed.cont.clone(), cont_ep: view.cont_ep, scroll: view.scroll, at: view.at }
        };
        assert_eq!(to_json(&prepare_owned()), to_json_ref(&view));
        measure("clone + serialize", || to_json(black_box(&prepare_owned())));
        measure("borrow + serialize", || to_json_ref(black_box(&view)));
    }

    #[test]
    fn compact_schema_bytes_and_escaping_stay_compatible() {
        let text = "A \"quote\", \\ slash, \n\0東京🦀";
        let items = vec![
            Item::Video(Video { id: "abcdefghijk".into(), title: text.into(), channel: "Channel".into(), channel_href: Some(String::new()), channel_bid: Some("UCchannel".into()), meta: "1.2M views".into(), duration: "1:23".into(), live: true, progress: 42, thumb: Some("https://i.ytimg.com/one.jpg".into()), thumb2x: Some("https://i.ytimg.com/two.jpg".into()), thumb_w: 360, thumb2x_w: 720, anim: Some(String::new()), music: true, short: true, members: false, art: None }),
            Item::Playlist(PlaylistCard { id: "PLone".into(), title: text.into(), channel: "Channel".into(), channel_href: Some("/@channel".into()), count: "12 videos".into(), thumb: Some("https://i.ytimg.com/list.jpg".into()), first_video: Some("abcdefghijk".into()), kind: "" }),
            Item::Channel(ChannelCard { href: "/@channel".into(), bid: Some("UCchannel".into()), title: text.into(), meta: "12 videos".into(), avatar: Some("https://yt3.ggpht.com/a.jpg".into()) }),
            Item::Video(Video::default()), Item::Playlist(PlaylistCard::default()), Item::Channel(ChannelCard::default()),
        ];
        let compact = json!([
            {"a":"", "b":"UCchannel", "c":"Channel", "d":"1:23", "h":"", "i":"abcdefghijk", "k":"v", "l":true, "m":"1.2M views", "p":42, "r":true, "s":"https://i.ytimg.com/one.jpg", "sw":360, "t":text, "u":true, "x":"https://i.ytimg.com/two.jpg", "xw":720},
            {"c":"Channel", "f":"abcdefghijk", "h":"/@channel", "i":"PLone", "k":"p", "n":"12 videos", "s":"https://i.ytimg.com/list.jpg", "t":text},
            {"b":"UCchannel", "h":"/@channel", "k":"c", "m":"12 videos", "s":"https://yt3.ggpht.com/a.jpg", "t":text},
            {"i":"", "k":"v"}, {"i":"", "k":"p"}, {"h":"", "k":"c"},
        ]);
        for (head, compact_head) in [
            (Head::Feed { title: text.into() }, json!({"k":"f", "t":text})),
            (Head::Channel { title: text.into(), avatar: Some(String::new()), meta: vec![text.into()], about: text.into(), nav: Default::default() }, json!({"a":"", "ab":text, "k":"c", "m":[text], "t":text})),
            (Head::Channel { title: String::new(), avatar: None, meta: vec![], about: String::new(), nav: Default::default() }, json!({"a":null, "ab":"", "k":"c", "m":[], "t":""})),
            (Head::Playlist { title: text.into(), meta: vec![text.into()] }, json!({"k":"p", "m":[text], "t":text})),
        ] {
            let sn = Snap { url: "/".into(), head, items: items.clone(), shown: 3, cont: None, cont_ep: "browse", scroll: 19.6, at: 1000.25 };
            assert_eq!(to_json(&sn), json!({"c":null, "e":"browse", "h":compact_head, "i":compact, "n":3, "t":1000.25, "u":"/", "v":1, "y":20.0}).to_string());
        }
        let index = vec![text.to_string(), "/results?search_query=x".into()];
        assert_eq!(index_json(&index), json!(index).to_string());
    }

    #[test]
    fn the_members_label_survives_a_snapshot() {
        let on = Item::Video(Video { id: "abcdefghijk".into(), members: true, ..Default::default() });
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&ItemRecords(std::slice::from_ref(&on))).unwrap()).unwrap();
        assert_eq!(v[0]["mo"], json!(true));
        let text = v[0].to_string();
        let back = crate::json::parse_str(&text).unwrap();
        assert_eq!(item_from(&back), Some(on), "a snapshot's card stays labelled");
        let old = crate::json::parse_str(r#"{"i":"abcdefghijk","k":"v"}"#).unwrap();
        assert!(matches!(item_from(&old), Some(Item::Video(v)) if !v.members), "snapshots from before the label");
    }

    #[test]
    fn serialization_keeps_item_caps_and_float_policy() {
        let mut sn = Snap { url: "/".into(), head: Head::Feed { title: "Home".into() }, items: vec![Item::Video(Video::default()); MAX_ITEMS + 1], shown: MAX_ITEMS + 1, cont: None, cont_ep: "browse", scroll: -12.5, at: f64::NAN };
        let v: serde_json::Value = serde_json::from_str(&to_json(&sn)).unwrap();
        assert_eq!(v["i"].as_array().unwrap().len(), MAX_ITEMS);
        assert_eq!(v["n"], MAX_ITEMS);
        assert_eq!(v["t"], json!(f64::NAN));
        assert_eq!(v["y"], json!(0.0));
        sn.scroll = f64::INFINITY;
        sn.at = f64::NEG_INFINITY;
        let v: serde_json::Value = serde_json::from_str(&to_json(&sn)).unwrap();
        assert_eq!((v["t"].clone(), v["y"].clone()), (json!(f64::NEG_INFINITY), json!(f64::INFINITY)));
    }

    #[test]
    fn roundtrip_and_validation() {
        let v = Video {
            id: "abcdefghijk".into(),
            title: "<b>t</b>".into(),
            channel_href: Some("/@x".into()),
            thumb: Some("https://i.ytimg.com/vi/abcdefghijk/hq720.jpg".into()),
            thumb_w: 360,
            anim: Some("https://i.ytimg.com/an_webp/abcdefghijk/mqdefault_6s.webp?du=3000".into()),
            ..Default::default()
        };
        // A Short keeps its mark through a snapshot (the Shorts switch hides it again after a reload).
        let short = Video { id: "shortshort1".into(), duration: "Shorts".into(), short: true, ..Default::default() };
        let sn = Snap {
            url: "/".into(),
            head: Head::Feed { title: "Home".into() },
            items: vec![Item::Video(v), Item::Channel(ChannelCard { href: "/@y".into(), ..Default::default() })],
            shown: 1,
            cont: Some("tok".into()),
            cont_ep: "browse",
            scroll: 120.0,
            at: 5.0,
        };
        let j = to_json(&sn);
        assert_eq!(from_json(&j, "/"), Some(sn.clone()));
        let mut sn2 = sn.clone();
        sn2.items.push(Item::Video(short));
        let back = from_json(&to_json(&sn2), "/").unwrap();
        assert!(back.items[2].is_short(), "the Short mark survives a snapshot");
        assert_eq!(back, sn2);
        assert!(from_json(&j, "/feed/subscriptions").is_none(), "keyed by URL");
        // Tampered storage: hostile ids/URLs are dropped exactly like in parse.rs.
        let bad = j.replace("\"i\":\"abcdefghijk\"", "\"i\":\"abc\\\"><x\"").replace("/@y", "javascript:alert(1)").replace("https://i.ytimg.com/an_webp", "https://evil.example/an_webp");
        let back = from_json(&bad, "/").unwrap();
        assert!(back.items.is_empty(), "{back:?}");
        let bad2 = j.replace("https://i.ytimg.com/vi/", "https://evil.example/vi/");
        let Item::Video(v) = &from_json(&bad2, "/").unwrap().items[0] else { panic!() };
        assert!(v.thumb.is_none());
        assert!(from_json("{\"v\":1}", "/").is_none());
        assert!(from_json("garbage", "/").is_none());
        assert_eq!(index_from(&index_json(&["/".into(), "/x".into()])), vec!["/".to_string(), "/x".to_string()]);
    }

    #[test]
    fn channel_nav_round_trips_and_is_validated() {
        use crate::parse::{ChannelTabLink, SortOpt};
        let nav = ChannelNav {
            path: "/@x".into(),
            tab: ChannelTab::Streams,
            tabs: vec![ChannelTabLink { tab: ChannelTab::Videos, title: "Videos".into(), params: "EgZ2aWRlb3M%3D".into() }, ChannelTabLink { tab: ChannelTab::Streams, title: "Live".into(), params: "EgdzdHJlYW1z".into() }],
            sorts: vec![SortOpt { label: "Latest".into(), selected: true, token: Some("4qmFsgJkEhh".into()), params: None }, SortOpt { label: "Oldest".into(), selected: false, token: None, params: Some("EglwbGF5".into()) }],
        };
        let head = Head::Channel { title: "T".into(), avatar: None, meta: vec![], about: String::new(), nav: nav.clone() };
        let json = serde_json::to_string(&HeadRecord(&head)).unwrap();
        let back = json::parse_str(&json).and_then(|v| head_from(&v)).unwrap();
        assert_eq!(back.nav(), Some(&nav));
        // A snapshot from before the tabs: no row, no failure.
        let old = json::parse_str(r#"{"k":"c","t":"T","m":[],"ab":""}"#).and_then(|v| head_from(&v)).unwrap();
        assert_eq!(old.nav(), Some(&ChannelNav::default()));
        // Hostile values are dropped like a response's.
        let bad = json::parse_str(r#"{"k":"c","t":"T","n":{"p":"//evil.com","s":"posts","t":[["store","Store","x"],["videos","V","\"><script>"]],"o":[{"l":"L","c":"short"}]}}"#).and_then(|v| head_from(&v)).unwrap();
        let n = bad.nav().unwrap();
        assert!(n.path.is_empty() && n.tab == ChannelTab::Videos && n.tabs.is_empty());
        assert_eq!(n.sorts[0].token, None);
    }
}
