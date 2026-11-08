//! URL → page routing. Only www.youtube.com URLs are handled; anything else is
//! left to the browser.

use crate::util::{is_safe_id, is_video_id, parse_t, pct_encode, query_param, safe_rel_href};

#[derive(Clone, Debug, PartialEq)]
pub enum ChannelKey {
    /// `UC…` browse id.
    Id(String),
    /// A path that needs `navigation/resolve_url` (`/@handle`, `/c/x`, `/user/x`).
    Path(String),
}

/// The channel tabs the client renders as a list of cards, in no particular order (the row follows the
/// response's own order). Home, Posts, Store, Membership, Playables and Search are not here on purpose.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum ChannelTab {
    #[default]
    Videos,
    Shorts,
    Streams,
    Podcasts,
    Courses,
    Releases,
    Shows,
    Playlists,
}

impl ChannelTab {
    /// The last URL segment YouTube uses for the tab.
    pub fn slug(self) -> &'static str {
        match self {
            ChannelTab::Videos => "videos",
            ChannelTab::Shorts => "shorts",
            ChannelTab::Streams => "streams",
            ChannelTab::Podcasts => "podcasts",
            ChannelTab::Courses => "courses",
            ChannelTab::Releases => "releases",
            ChannelTab::Shows => "shows",
            ChannelTab::Playlists => "playlists",
        }
    }

    pub fn from_slug(s: &str) -> Option<ChannelTab> {
        [ChannelTab::Videos, ChannelTab::Shorts, ChannelTab::Streams, ChannelTab::Podcasts, ChannelTab::Courses, ChannelTab::Releases, ChannelTab::Shows, ChannelTab::Playlists].into_iter().find(|t| t.slug() == s)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Route {
    Home,
    Subs,
    History,
    /// `/feed/playlists`: the account's saved and own playlists.
    Playlists,
    /// `sp` = YouTube's own search-filter `params` (from a filter option's
    /// `searchEndpoint`), sent verbatim; `None` = no filters.
    Search { q: String, sp: Option<String> },
    Watch { v: String, t: u32, list: Option<String> },
    /// `path` = the channel's own page (`/@x`, `/channel/UC…`, `/c/x`, `/user/x`), without the tab.
    Channel { key: ChannelKey, path: String, tab: ChannelTab },
    Playlist { list: String },
    Unknown { path: String },
}

impl Route {
    /// Canonical same-origin URL used for history entries.
    pub fn url(&self) -> String {
        match self {
            Route::Home => "/".into(),
            Route::Subs => "/feed/subscriptions".into(),
            Route::History => "/feed/history".into(),
            Route::Playlists => "/feed/playlists".into(),
            Route::Search { q, sp } => match sp {
                Some(sp) => format!("/results?search_query={}&sp={}", pct_encode(q), pct_encode(sp)),
                None => format!("/results?search_query={}", pct_encode(q)),
            },
            Route::Watch { v, t, list } => {
                let mut u = format!("/watch?v={v}");
                if let Some(l) = list {
                    u.push_str("&list=");
                    u.push_str(l);
                }
                if *t > 0 {
                    u.push_str(&format!("&t={t}s"));
                }
                u
            }
            // The Videos tab is the channel's own page.
            Route::Channel { path, tab: ChannelTab::Videos, .. } => path.clone(),
            Route::Channel { path, tab, .. } => format!("{path}/{}", tab.slug()),
            Route::Playlist { list } => format!("/playlist?list={list}"),
            Route::Unknown { path } => path.clone(),
        }
    }

    pub fn needs_sign_in(&self) -> bool {
        matches!(self, Route::Subs | Route::History | Route::Playlists) || matches!(self, Route::Playlist { list } if list == "WL" || list == "LL")
    }

    /// Key for the top-bar "active" state.
    pub fn nav_key(&self) -> &'static str {
        match self {
            Route::Home => "home",
            Route::Subs => "subs",
            Route::History => "history",
            Route::Playlist { list } if list == "WL" => "wl",
            Route::Playlist { list } if list == "LL" => "liked",
            Route::Playlists => "playlists",
            Route::Search { .. } => "search",
            // The sidebar is hidden on watch pages (and its width released).
            Route::Watch { .. } => "watch",
            _ => "",
        }
    }
}

/// A search-filter `params` value as YouTube sends it (base64, usually
/// percent-encoded): short and URL/JSON-safe characters only.
pub fn valid_sp(s: &str) -> bool {
    (1..=300).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"%-_+/=".contains(&b))
}

/// Native-YouTube URL for the escape hatch.
pub fn native_url(url: &str) -> String {
    let (base, frag) = url.split_once('#').unwrap_or((url, ""));
    let sep = if base.contains('?') { '&' } else { '?' };
    let mut out = format!("{base}{sep}yt-native=1");
    if !frag.is_empty() {
        out.push('#');
        out.push_str(frag);
    }
    out
}

/// Strips the origin from an href; `None` for other origins and non-http links.
pub fn local_part(href: &str) -> Option<&str> {
    for origin in ["https://www.youtube.com", "http://www.youtube.com", "//www.youtube.com"] {
        if let Some(rest) = href.strip_prefix(origin) {
            return if rest.is_empty() { Some("/") } else if rest.starts_with('/') || rest.starts_with('?') { Some(rest) } else { None };
        }
    }
    (href.starts_with('/') && !href.starts_with("//")).then_some(href)
}

pub fn parse(href: &str) -> Option<Route> {
    let local = local_part(href)?;
    let local = local.split('#').next().unwrap_or("");
    let (path, query) = local.split_once('?').unwrap_or((local, ""));
    let path = if path.len() > 1 { path.trim_end_matches('/') } else { path };
    let path = if path.is_empty() { "/" } else { path };
    let q = |n: &str| query_param(query, n);
    let mut segs = path[1..].split('/');
    let first = segs.next().unwrap_or("");
    let second = segs.next().unwrap_or("");
    let third = segs.next().unwrap_or("");
    // A segment that names no tab the client renders (`/featured`, `/posts`, …) opens Videos, as a bare channel URL does.
    let tab_of = |s: &str| ChannelTab::from_slug(s).unwrap_or_default();
    let route = match first {
        "" => Route::Home,
        "feed" => match second {
            "subscriptions" => Route::Subs,
            "history" => Route::History,
            "playlists" => Route::Playlists,
            "" | "what_to_watch" => Route::Home,
            _ => Route::Unknown { path: local.to_string() },
        },
        "results" | "search" => {
            let term = q("search_query").or_else(|| q("q")).unwrap_or_default();
            let term = term.trim();
            let sp = q("sp").filter(|s| valid_sp(s));
            if term.is_empty() { Route::Home } else { Route::Search { q: term.chars().take(500).collect(), sp } }
        }
        "watch" => match q("v").filter(|v| is_video_id(v)) {
            Some(v) => Route::Watch {
                v,
                t: q("t").or_else(|| q("start")).map(|t| parse_t(&t)).unwrap_or(0),
                list: q("list").filter(|l| is_safe_id(l)),
            },
            None => Route::Unknown { path: local.to_string() },
        },
        "shorts" | "live" | "v" if is_video_id(second) => Route::Watch { v: second.to_string(), t: 0, list: None },
        "playlist" => match q("list").filter(|l| is_safe_id(l)) {
            Some(list) => Route::Playlist { list },
            None => Route::Unknown { path: local.to_string() },
        },
        "channel" if second.starts_with("UC") && is_safe_id(second) => {
            Route::Channel { key: ChannelKey::Id(second.to_string()), path: format!("/channel/{second}"), tab: tab_of(third) }
        }
        f if f.starts_with('@') && f.len() > 1 => {
            let p = format!("/{f}");
            match safe_rel_href(&p) {
                Some(p) => Route::Channel { key: ChannelKey::Path(p.clone()), path: p, tab: tab_of(second) },
                None => Route::Unknown { path: local.to_string() },
            }
        }
        "c" | "user" if !second.is_empty() => {
            let p = format!("/{first}/{second}");
            match safe_rel_href(&p) {
                Some(p) => Route::Channel { key: ChannelKey::Path(p.clone()), path: p, tab: tab_of(third) },
                None => Route::Unknown { path: local.to_string() },
            }
        }
        _ => Route::Unknown { path: local.to_string() },
    };
    Some(route)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn routes() {
        assert_eq!(parse("/"), Some(Route::Home));
        assert_eq!(parse("https://www.youtube.com"), Some(Route::Home));
        assert_eq!(parse("https://www.youtube.com/feed/subscriptions"), Some(Route::Subs));
        assert_eq!(parse("/results?search_query=roblox+studio%21"), Some(Route::Search { q: "roblox studio!".into(), sp: None }));
        let f = parse("/results?search_query=roblox&sp=EgIIAw%253D%253D").unwrap();
        assert_eq!(f, Route::Search { q: "roblox".into(), sp: Some("EgIIAw%3D%3D".into()) });
        assert_eq!(f.url(), "/results?search_query=roblox&sp=EgIIAw%253D%253D");
        assert_eq!(parse("/results?search_query=x&sp=%22%3E"), Some(Route::Search { q: "x".into(), sp: None }), "hostile sp dropped");
        assert_eq!(parse("/watch?v=-Bxc4gJluC0&t=1m5s&list=WL"), Some(Route::Watch { v: "-Bxc4gJluC0".into(), t: 65, list: Some("WL".into()) }));
        assert_eq!(parse("/shorts/DQcrB2wLu04"), Some(Route::Watch { v: "DQcrB2wLu04".into(), t: 0, list: None }));
        assert_eq!(parse("/@monkeydeveloper/videos"), Some(Route::Channel { key: ChannelKey::Path("/@monkeydeveloper".into()), path: "/@monkeydeveloper".into(), tab: ChannelTab::Videos }));
        assert_eq!(parse("/channel/UCaZO-66UcqyVT9btiqdDryA"), Some(Route::Channel { key: ChannelKey::Id("UCaZO-66UcqyVT9btiqdDryA".into()), path: "/channel/UCaZO-66UcqyVT9btiqdDryA".into(), tab: ChannelTab::Videos }));
        assert_eq!(parse("/playlist?list=WL"), Some(Route::Playlist { list: "WL".into() }));
        assert_eq!(parse("/feed/playlists"), Some(Route::Playlists));
        assert_eq!(Route::Playlists.url(), "/feed/playlists");
        assert!(Route::Playlists.needs_sign_in());
        assert_eq!(parse("/playlist?list=LL").unwrap().nav_key(), "liked");
        assert_eq!(parse("/watch?v=-Bxc4gJluC0").unwrap().nav_key(), "watch");
        assert_eq!(parse("https://example.com/watch?v=-Bxc4gJluC0"), None);
        assert_eq!(parse("//evil.com/"), None);
        assert_eq!(parse("javascript:alert(1)"), None);
        assert!(matches!(parse("/hashtag/roblox"), Some(Route::Unknown { .. })));
        assert_eq!(native_url("/watch?v=x#a"), "/watch?v=x&yt-native=1#a");
        assert_eq!(native_url("/"), "/?yt-native=1");
        assert_eq!(Route::Search { q: "a&b c".into(), sp: None }.url(), "/results?search_query=a%26b%20c");
    }

    #[test]
    fn channel_tabs_in_every_url_form() {
        for (url, path, key_id) in [("/@x", "/@x", false), ("/channel/UCaZO-66UcqyVT9btiqdDryA", "/channel/UCaZO-66UcqyVT9btiqdDryA", true), ("/c/name", "/c/name", false), ("/user/name", "/user/name", false)] {
            let tab_at = |t: &str| match parse(&format!("{url}{t}")) {
                Some(Route::Channel { path: p, tab, key }) => {
                    assert_eq!(p, path);
                    assert_eq!(matches!(key, ChannelKey::Id(_)), key_id);
                    tab
                }
                other => panic!("{url}{t}: {other:?}"),
            };
            assert_eq!(tab_at(""), ChannelTab::Videos, "the bare URL opens Videos");
            assert_eq!(tab_at("/"), ChannelTab::Videos);
            assert_eq!(tab_at("/videos"), ChannelTab::Videos);
            assert_eq!(tab_at("/streams"), ChannelTab::Streams);
            assert_eq!(tab_at("/shorts"), ChannelTab::Shorts);
            assert_eq!(tab_at("/playlists?view=1"), ChannelTab::Playlists);
            assert_eq!(tab_at("/podcasts"), ChannelTab::Podcasts);
            assert_eq!(tab_at("/courses"), ChannelTab::Courses);
            assert_eq!(tab_at("/releases"), ChannelTab::Releases);
            assert_eq!(tab_at("/shows"), ChannelTab::Shows);
            for out in ["/featured", "/posts", "/community", "/store", "/membership", "/search", "/about", "/playables"] {
                assert_eq!(tab_at(out), ChannelTab::Videos, "{out} is not a tab of the client");
            }
        }
        // The canonical URL: the channel's own page for Videos, the tab's otherwise.
        assert_eq!(parse("/@x/videos").unwrap().url(), "/@x");
        assert_eq!(parse("/@x/streams").unwrap().url(), "/@x/streams");
        assert_eq!(parse("/@x/shorts").unwrap().url(), "/@x/shorts");
        // A video is still a video (`/shorts/ID`), a tab is not.
        assert!(matches!(parse("/shorts/DQcrB2wLu04"), Some(Route::Watch { .. })));
        assert!(matches!(parse("/@x/shorts"), Some(Route::Channel { tab: ChannelTab::Shorts, .. })));
    }
}
