//! HTML generation. Every piece of text or attribute from YouTube passes
//! through `esc`; URLs are validated in `parse.rs`/`util.rs` before they get
//! here. Each region is written with one `innerHTML`.

use crate::parse::{ChannelCard, ChannelPage, Item, Link, PlaylistCard, PlaylistPage, Run, Stats, Video, Watch};
use crate::route::Route;
use crate::util::{esc, esc_into, is_video_id};

pub const MAIN: &str = "ytc-main";
pub const LIST: &str = "ytc-list";
pub const MORE: &str = "ytc-more";
pub const INFO: &str = "ytc-info";
pub const DEBUG: &str = "ytc-debug";
pub const PILL: &str = "ytc-pill";
pub const SUGGEST: &str = "ytc-sug";

/// "New videos" button shown when a refreshed feed arrived while the user was
/// scrolled down in its snapshot (swapping then would move the page).
pub const FRESH_PILL: &str = "<button type=\"button\" class=\"pill\" data-act=\"fresh\">New videos</button>";

/// `<option>`s for the search box's datalist.
pub fn suggestions(list: &[String]) -> String {
    let mut out = String::new();
    for s in list {
        out.push_str("<option value=\"");
        esc_into(&mut out, s);
        out.push_str("\">");
    }
    out
}

/// The header logo: a link Home and a hover zone (`logo`: resting on it goes Home, `app/logo.rs`).
pub const LOGO_ID: &str = "ytc-logo";
/// The extension's and the TV app's own name: the toolbar popup and About & updates (the launcher label and the
/// manifest carry it too). Inside the product the interface is YouTube's: its logo, its name (`SITE`) and its titles
/// (owner decision, 7 Nov 2026: "it's just a name for extension and github").
pub const NAME: &str = "Redline";
/// The name the interface shows: the top bar's and the TV rail's wordmark, the window title ("<page> - YouTube").
pub const SITE: &str = "YouTube";

/// YouTube's logo (viewBox 0 0 28 20: the red rounded rectangle, `--brand`, and its white play mark, `--brand-ink`),
/// the same in the top bar (`LOGO`) and on TV (`MARK`: the rail, the sign-in and About headers, calm cards).
macro_rules! yt_logo {
    ($attrs:literal) => {
        concat!("<svg", $attrs, " viewBox=\"0 0 28 20\" aria-hidden=\"true\"><rect width=\"28\" height=\"20\" rx=\"5\"/><path d=\"M11 5.5v9l8-4.5z\"/></svg>")
    };
}
const LOGO: &str = yt_logo!("");
/// YouTube's logo in the TV rail and on the TV's full-screen pages.
pub const MARK: &str = yt_logo!(" class=\"lmk\"");

/// The Redline mark, viewBox 0 0 32 24: a red rounded parallelogram leaning forward with a chevron cut out
/// (one even-odd path). It is the extension's own icon, never shown inside the product: its one source is
/// `browser/icons/redline.svg` (a test keeps them equal), from which the toolbar icons and the TV launcher art are
/// rendered; inline it appears only in the toolbar popup's header (the extension's own UI).
macro_rules! mark_path {
    () => {
        "M12.54 2L27.54 2C30.03 2 31.23 4.01 30.22 6.5L25.78 17.5C24.77 19.99 21.95 22 19.46 22L4.46 22C1.97 22 0.77 19.99 1.78 17.5L6.22 6.5C7.23 4.01 10.05 2 12.54 2ZM12.44 6.64C12.09 6.29 12.21 6 12.7 6L16.5 6C17 6 17.69 6.29 18.04 6.64L22.64 11.24C23.06 11.66 23.06 12.34 22.64 12.76L18.04 17.36C17.69 17.71 17 18 16.5 18L12.7 18C12.21 18 12.09 17.71 12.44 17.36L17.29 12.51C17.57 12.23 17.57 11.77 17.29 11.49Z"
    };
}
pub const MARK_PATH: &str = mark_path!();
/// The browser toolbar popup's header (the fill is the brand red, `--brand`).
pub const POPUP_MARK: &str = concat!("<svg class=\"mk\" viewBox=\"0 0 32 24\" aria-hidden=\"true\"><path fill-rule=\"evenodd\" d=\"", mark_path!(), "\"/></svg>");

// ---------------------------------------------------------------- hooks
//
// Feature modules (src/features/*, src/actions/*) plug into the markup here
// instead of editing the card/page code. See docs/architecture.md "Hook points".

/// Writes extra markup into a video card's text column (`.d`), e.g. a
/// Watch later button. Anything with class `ca` sits centered beside the
/// title; other markup follows the metadata lines.
pub type CardHook = fn(&mut String, &Video);

thread_local! {
    static CARD_HOOK: std::cell::Cell<Option<CardHook>> = const { std::cell::Cell::new(None) };
}

/// Registers the card hook (once, at boot).
pub fn set_card_hook(f: CardHook) {
    CARD_HOOK.with(|h| h.set(Some(f)));
}

/// Page-level slots (empty elements the features fill by id).
/// Filter bar on feed pages (Home, Search, …): right side of the heading row.
pub const FILTERS: &str = "ytc-filters";
/// Header slot left of the bell: the Filters button and its panel (Home, Search).
pub const HFX: &str = "ytc-hfx";
/// Watch page: beside the channel name (Subscribe).
pub const SUBSCRIBE: &str = "ytc-sub";
/// Watch page: the actions row right of the channel (Like/Dislike/Save…).
pub const ACTIONS: &str = "ytc-acts";
/// Watch page: comments, below the description (kept while the info re-renders).
pub const COMMENTS: &str = "ytc-comments";
/// Watch page: the playlist panel and the Up next header above the related list.
pub const PLAYLIST: &str = "ytc-pl";
pub const UPNEXT: &str = "ytc-up";

// Sidebar: these links in this order, without Shorts
// (never shown in the client). 22px outline icons; `g` + the key in GO opens each.
const SIDE: [(&str, &str, &str, &str); 6] = [
    ("/", "home", "Home", "M4 10.5 12 4l8 6.5V20h-5v-6h-6v6H4z"),
    ("/feed/history", "history", "History", "M4.5 12a7.5 7.5 0 1 0 2.2-5.3M4.5 4v3.5H8M12 8v4l3 2"),
    ("/playlist?list=WL", "wl", "Watch later", "M12 4a8 8 0 1 0 0 16 8 8 0 0 0 0-16zm0 4v4l3 2"),
    ("/feed/subscriptions", "subs", "Subscriptions", "M5 8h14a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-8a2 2 0 0 1 2-2zm2-3h10M10.5 11.5v5l4-2.5z"),
    ("/feed/playlists", "playlists", "Playlists", "M4 6h12M4 11h12M4 16h7m6-3v7l4.5-3.5z"),
    ("/playlist?list=LL", "liked", "Liked videos", "M7.5 10.5V20H4v-9.5zm0 0 3.8-6.5c1.4 0 2.4 1 2.1 2.5l-.7 3.5H19a2 2 0 0 1 2 2.3l-1.1 6a2 2 0 0 1-2 1.7H7.5"),
];

/// `g` then this key → page (keyboard-first navigation).
const GO: [(&str, &str); 6] = [("h", "/"), ("y", "/feed/history"), ("w", "/playlist?list=WL"), ("s", "/feed/subscriptions"), ("p", "/feed/playlists"), ("l", "/playlist?list=LL")];

pub fn go_href(key: &str) -> Option<&'static str> {
    GO.iter().find(|(k, _)| *k == key).map(|(_, h)| *h)
}

pub fn side_href(key: &str) -> Option<&'static str> {
    SIDE.iter().find(|s| s.1 == key).map(|s| s.0)
}

/// The keyboard sheet (`?`).
pub const KEYS: &str = "ytc-keys";

pub fn keys_help() -> String {
    let rows: [(&str, &str); 20] = [
        ("/", "Search"),
        ("Hold ⌥ Option / Alt", "Voice search: speak, let go to search"),
        ("g h · g s · g y", "Home · Subscriptions · History"),
        ("g w · g p · g l", "Watch later · Playlists · Liked"),
        ("← → ↑ ↓", "Move between videos (pages)"),
        ("Enter", "Open the focused video"),
        ("Alt+←", "Back"),
        ("k / Space", "Play / pause"),
        ("j · l", "−10 s · +10 s"),
        ("← · →", "−5 s · +5 s (watch page)"),
        ("↑ · ↓", "Volume (watch page)"),
        ("Ctrl/⌘+← · →", "Previous · next chapter"),
        ("< · >", "Speed (up to 2.5×)"),
        ("m", "Mute"),
        ("c", "Captions on/off"),
        ("f · t", "Fullscreen · Theater"),
        ("Enter on CC/Speed/Quality", "Open its menu; arrows move, Esc closes"),
        ("`", "Debug overlay"),
        ("?", "This sheet"),
        ("Esc", "Close"),
    ];
    let mut out = String::from("<h2>Keyboard</h2><dl>");
    for (k, v) in rows {
        out.push_str("<dt>");
        esc_into(&mut out, k);
        out.push_str("</dt><dd>");
        esc_into(&mut out, v);
        out.push_str("</dd>");
    }
    out.push_str("</dl>");
    out
}

/// Pitch of the sidebar rows: a 40 px link + 4 px gap (client.css `#ytc-side a`; the strip `.sdi` is 6 × 44 − 4 px).
const SIDE_ROW: usize = 44;
/// The sliding active indicator (`.sdi`), moved by its clip insets `--st`/`--sb`.
pub const SIDE_PILL: &str = "ytc-sdi";

/// Position of a page's link in the sidebar (None: the page has no link there).
pub fn side_index(nav: &str) -> Option<usize> {
    if crate::tv::on() {
        return TV_RAIL.iter().position(|s| s.0 == nav);
    }
    SIDE.iter().position(|s| s.1 == nav)
}

/// The indicator's clip insets (top, bottom) for link `i`: a full-height strip clipped to one row
/// (TV: rows of `--tvrow`, client.css).
pub fn side_insets(i: usize) -> (String, String) {
    if crate::tv::on() {
        return (format!("calc({i} * var(--tvrow))"), format!("calc({} * var(--tvrow))", TV_RAIL.len() - 1 - i));
    }
    (format!("{}px", i * SIDE_ROW), format!("{}px", (SIDE.len() - 1 - i) * SIDE_ROW))
}

const GLASS: &str = "M15.5 15.5 21 21M17 10.5a6.5 6.5 0 1 1-13 0 6.5 6.5 0 0 1 13 0z";
/// About & updates: two arrows turning round (an update).
const UPDATE: &str = "M19.5 12a7.5 7.5 0 0 1-13.1 5M4.5 12a7.5 7.5 0 0 1 13.1-5M17.6 3.5V7h-3.5M6.4 20.5V17h3.5";
const PERSON: &str = "M15.5 8.5a3.5 3.5 0 1 1-7 0 3.5 3.5 0 0 1 7 0zM5 19.5c1.2-3.2 3.8-5 7-5s5.8 1.8 7 5";

/// The TV rail, top to bottom (nav key, label, icon, href; Search opens its screen instead of a page).
const TV_RAIL: [(&str, &str, &str, &str); 7] = [
    ("search", "Search", GLASS, ""),
    ("home", "Home", SIDE[0].3, "/"),
    ("subs", "Subscriptions", SIDE[3].3, "/feed/subscriptions"),
    ("history", "History", SIDE[1].3, "/feed/history"),
    ("wl", "Watch later", SIDE[2].3, "/playlist?list=WL"),
    ("playlists", "Playlists", SIDE[4].3, "/feed/playlists"),
    ("liked", "Liked videos", SIDE[5].3, "/playlist?list=LL"),
];

/// One rail item: an icon (the collapsed rail) and its label (shown while the rail is open).
pub fn tv_rail_item(out: &mut String, nav: &str, label: &str, icon: &str, href: &str, act: &str) {
    if href.is_empty() {
        out.push_str(&format!("<button type=\"button\" class=\"rl\" data-nav=\"{nav}\" data-act=\"{act}\">"));
    } else {
        out.push_str(&format!("<a class=\"rl\" href=\"{href}\" data-nav=\"{nav}\">"));
    }
    out.push_str(&format!("<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"{icon}\"/></svg><span>{label}</span>"));
    out.push_str(if href.is_empty() { "</button>" } else { "</a>" });
}

/// TV: the rail (YouTube's logo, Search, the pages, the account at the bottom) instead of the sidebar.
fn tv_rail(out: &mut String, at: usize) {
    let (t, b) = side_insets(at);
    out.push_str(&format!("<nav id=\"ytc-side\" class=\"tvr\" aria-label=\"Browse\"><span class=\"rmk\">{MARK}<b>{SITE}</b></span><div class=\"rls\"><i id=\"{SIDE_PILL}\" class=\"sdi\" aria-hidden=\"true\" style=\"--st:{t};--sb:{b}\"></i>"));
    for (nav, label, icon, href) in TV_RAIL {
        tv_rail_item(out, nav, label, icon, href, "tv-search");
    }
    out.push_str("</div><div class=\"rla\"><div id=\"ytc-acct\"></div>");
    tv_rail_item(out, "about", "About & updates", UPDATE, "", "tv-about");
    out.push_str("</div></nav>");
}

fn sidebar(out: &mut String, at: usize) {
    let (t, b) = side_insets(at);
    out.push_str(&format!("<nav id=\"ytc-side\" aria-label=\"Browse\"><i id=\"{SIDE_PILL}\" class=\"sdi\" aria-hidden=\"true\" style=\"--st:{t};--sb:{b}\"></i>"));
    for (href, key, label, d) in SIDE {
        let go = GO.iter().find(|g| g.1 == href).map_or("", |g| g.0);
        out.push_str(&format!("<a id=\"ytc-nav-{key}\" href=\"{href}\" data-hov=\"side\" data-arg=\"{key}\" data-nav=\"{key}\" aria-keyshortcuts=\"g {go}\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"{d}\"/></svg>{label}</a>"));
    }
    out.push_str("</nav>");
}

/// The whole document. `main` is the first content (a snapshot repaint);
/// `None` shows the skeleton. The preconnects start the image CDN handshakes
/// at document_start, before the first InnerTube response.
pub fn shell(route: &Route, _url: &str, main: Option<&str>) -> String {
    let query = if let Route::Search { q, .. } = route { q.as_str() } else { "" };
    let q = esc(query);
    let tv = crate::tv::on();
    let mut side = String::with_capacity(1600);
    if tv {
        tv_rail(&mut side, side_index(route.nav_key()).unwrap_or(1));
    } else {
        sidebar(&mut side, side_index(route.nav_key()).unwrap_or(0));
    }
    format!(
        "<head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{SITE}</title>\
<link rel=\"preconnect\" href=\"https://i.ytimg.com\"><link rel=\"preconnect\" href=\"https://yt3.ggpht.com\"></head>\
<body id=\"ytc\" data-nav=\"{nav}\"><header id=\"ytc-top\">\
<a id=\"{LOGO_ID}\" class=\"logo\" href=\"/\" aria-label=\"{SITE} home\" data-hov=\"logo\">{LOGO}<span>{SITE}</span></a>\
<form id=\"ytc-search\" role=\"search\" action=\"/results\"{key_input}><i class=\"qf\" aria-hidden=\"true\"></i>{tv_query}<input id=\"ytc-q\" name=\"search_query\" type=\"{query_type}\" placeholder=\"Search\" aria-label=\"Search\" autocomplete=\"off\" spellcheck=\"false\" role=\"combobox\" aria-autocomplete=\"list\" aria-controls=\"ytc-sug\" aria-expanded=\"false\" data-input=\"q\" data-focus=\"q\" value=\"{q}\"><button type=\"button\" class=\"qx\" data-act=\"fx-qx\" data-keep-focus aria-label=\"Clear search\" tabindex=\"-1\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"m7 7 10 10M17 7 7 17\"/></svg></button><span id=\"ytc-qv\" class=\"qv\" aria-hidden=\"true\"></span><button type=\"button\" id=\"ytc-mic\" class=\"qm\" data-hov=\"mic\" data-act=\"vk-mic\" data-ctx=\"vk\" data-pop=\"vk\" data-keep-focus aria-label=\"Voice search\" aria-pressed=\"false\" aria-keyshortcuts=\"Alt\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><rect x=\"9\" y=\"3\" width=\"6\" height=\"11.5\" rx=\"3\"/><path d=\"M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21\"/></svg><i class=\"wv\" aria-hidden=\"true\"><b></b><b></b><b></b><b></b><b></b></i></button>{tv_keyboard}<ul id=\"ytc-sug\" class=\"sug\" role=\"listbox\" aria-label=\"Search history and suggestions\" data-keep-focus></ul><button type=\"submit\" class=\"qs\" aria-label=\"Search\"><svg viewBox=\"0.5 0.5 24 24\" aria-hidden=\"true\"><circle cx=\"10.5\" cy=\"10.5\" r=\"6.5\"/><path d=\"M15.5 15.5 21 21\"/></svg></button></form><div id=\"ytc-vk\" class=\"vk\" data-pop=\"vk\" role=\"dialog\" aria-label=\"Voice search\"></div>\
<div class=\"tr\"><div id=\"ytc-hfx\"></div>{acct}</div>\
</header>{side}<div id=\"{PILL}\"></div><main id=\"{MAIN}\">{main}</main><div id=\"{KEYS}\" role=\"dialog\" aria-label=\"Keyboard shortcuts\"></div><div id=\"ytc-imp\" class=\"imp\"></div><div id=\"{DEBUG}\" hidden></div></body>",
        nav = route.nav_key(),
        main = main.map(String::from).unwrap_or_else(|| skeleton(route)),
        // TV: the account control is the rail's last item.
        acct = if tv { "" } else { "<div id=\"ytc-acct\"></div>" },
        key_input = if tv { " data-key-input" } else { "" },
        query_type = if tv { "hidden" } else { "search" },
        tv_query = if tv { crate::tv::keyboard::query(query) } else { String::new() },
        tv_keyboard = if tv { crate::tv::keyboard::shell() } else { String::new() },
    )
}

/// A card placeholder with the final card's geometry (thumbnail + two title
/// lines + two metadata lines), so nothing moves when the cards arrive.
const SK_CARD: &str = "<div class=\"c sk\"><div class=\"th\"></div><div class=\"d\"><div class=\"t\"><i></i><i class=\"s\"></i></div><div class=\"m\"><i></i></div><div class=\"m\"><i class=\"s\"></i></div></div></div>";

/// The heading every feed page and its skeleton start with: same height
/// either way (the filter slot keeps it even without a title).
fn feed_heading(out: &mut String, title: &str) {
    out.push_str("<div class=\"fh\"><h1>");
    esc_into(out, title);
    out.push_str("</h1><div id=\"");
    out.push_str(FILTERS);
    out.push_str("\" class=\"fb\"></div></div>");
}

pub fn skeleton(route: &Route) -> String {
    if matches!(route, Route::Watch { .. }) {
        return watch_shell("", &watch_info_partial(None), false);
    }
    let mut s = String::with_capacity(2400);
    s.push_str("<section class=\"feed\">");
    match route {
        Route::Home => feed_heading(&mut s, "Home"),
        Route::Subs => feed_heading(&mut s, "Subscriptions"),
        Route::History => feed_heading(&mut s, "History"),
        Route::Playlists => feed_heading(&mut s, "Playlists"),
        Route::Search { q, .. } => feed_heading(&mut s, &format!("Results for {q}")),
        Route::Playlist { list } if list == "WL" => feed_heading(&mut s, "Watch later"),
        Route::Channel { .. } => s.push_str("<div class=\"ch sk\"><div class=\"av lg\"></div><div><div class=\"l w30\"></div><div class=\"l w60\"></div></div></div>"),
        _ => s.push_str("<div class=\"fh\"><div class=\"sk l w30\"></div></div>"),
    }
    s.push_str("<div class=\"grid\">");
    for _ in 0..12 {
        s.push_str(SK_CARD);
    }
    s.push_str("</div></section>");
    s
}

/// `sizes` of a feed-grid thumbnail (see `Lay::sizes`).
pub const GRID_SIZES: &str = "(max-width:560px) calc(100vw - 40px),(max-width:759px) calc(50vw - 29px),(max-width:950px) calc(50vw - 131px),\
(max-width:1300px) calc(33.33vw - 93px),(max-width:1700px) calc(25vw - 75px),calc(20vw - 64px)";

/// Which layout a card list is for: decides `sizes` and the first-row priority.
#[derive(Clone, Copy, PartialEq)]
pub enum Lay {
    /// The feed grid (five cards per row on wide screens, ~300-460 CSS px).
    Grid,
    /// The watch page's related column (thumbnails 46 % of the column: 168–204 CSS px).
    Rel,
}

impl Lay {
    fn sizes(self) -> &'static str {
        if crate::tv::on() {
            return match self {
                Lay::Grid => "18.4vw",
                Lay::Rel => "14.2vw",
            };
        }
        match self {
            // The card's own width per `.grid` breakpoint (client.css: 5/4/3/2/1 columns at 1700/1300/950/560 px,
            // the 224 px sidebar from 760 px, 20 px main padding, 18 px gaps). It was a flat 460 px above 950 px,
            // so every 1x screen took the 720w file for a ~275 px card: 43 KB instead of the 320w webp's 11 KB
            // (measured on i.ytimg.com, 3 Nov 2026).
            Lay::Grid => GRID_SIZES,
            Lay::Rel => "(max-width:1640px) 168px,204px",
        }
    }
}

/// How a card list loads its images: the first `eager` skip `loading=lazy`,
/// the first `high` (the first visible row) get `fetchpriority=high`.
#[derive(Clone, Copy)]
pub struct ImgPlan {
    pub lay: Lay,
    pub eager: usize,
    pub high: usize,
}

impl ImgPlan {
    /// Later chunks: everything lazy, normal priority.
    pub fn lazy(lay: Lay) -> ImgPlan {
        ImgPlan { lay, eager: 0, high: 0 }
    }
    /// First screen of a page; `cols` = grid columns at the current width.
    pub fn first(lay: Lay, cols: usize) -> ImgPlan {
        match lay {
            Lay::Grid => ImgPlan { lay, eager: (cols * 2).clamp(4, 16), high: cols.clamp(1, 8) },
            Lay::Rel => ImgPlan { lay, eager: 6, high: 3 },
        }
    }
}

/// Sidebar width and the window width below which it is hidden (client.css `--sw`).
pub const SIDE_W: f64 = 224.0;
pub const SIDE_MIN: f64 = 760.0;

/// Grid columns at viewport width `w` on a browsing page (mirrors the
/// `.grid` media queries in client.css).
pub fn grid_cols(w: f64) -> usize {
    match w {
        w if w > 1700.0 => 5,
        w if w > 1300.0 => 4,
        w if w > 950.0 => 3,
        w if w > 560.0 => 2,
        _ => 1,
    }
}

/// Cards per row on the TV app at every resolution (client.css `html[data-ytc-tv] .grid`).
pub const TV_COLS: usize = 5;

/// Grid columns of a browsing page: the TV app's fixed grid, or the desktop's at width `w`.
pub fn page_cols(w: f64, tv: bool) -> usize {
    if tv { TV_COLS } else { grid_cols(w) }
}

/// Cards per DOM write on a fixed grid of `cols`: `chunk`, plus what completes the last row once `shown` cards
/// are on the page (TV: a batch never leaves a ragged row while the next one is on its way).
pub fn row_chunk(shown: usize, cols: usize, chunk: usize) -> usize {
    if cols == 0 { chunk } else { chunk + (cols - (shown + chunk) % cols) % cols }
}

fn img_tail(out: &mut String, eager: bool, high: bool) {
    out.push_str(" alt=\"\"");
    if !eager {
        out.push_str(" loading=\"lazy\"");
    }
    if high {
        out.push_str(" fetchpriority=\"high\"");
    }
    out.push_str(" decoding=\"async\">");
}

fn img(out: &mut String, src: &str, src2x: Option<&str>, eager: bool, class: &str) {
    out.push_str("<img");
    if !class.is_empty() {
        out.push_str(" class=\"");
        out.push_str(class);
        out.push('"');
    }
    out.push_str(" src=\"");
    esc_into(out, src);
    out.push('"');
    if let Some(x2) = src2x {
        out.push_str(" srcset=\"");
        esc_into(out, src);
        out.push_str(" 1x, ");
        esc_into(out, x2);
        out.push_str(" 2x\"");
    }
    img_tail(out, eager, false);
}

/// Video thumbnail: `srcset` with width descriptors so the browser takes the
/// smallest file that is sharp at this layout and pixel density. Measured on
/// i.ytimg.com (1 Nov 2026, 11 videos): `vi_webp/…/mqdefault.webp` (320 px)
/// averages 9.3 KB vs 14.1 KB for the 360 px `hq720` YouTube lists and 34 KB
/// for its 720 px one (both served as WebP).
fn video_img(out: &mut String, v: &Video, plan: ImgPlan, i: usize) {
    let (eager, high) = (i < plan.eager, i < plan.high);
    let mq = format!("https://i.ytimg.com/vi_webp/{}/mqdefault.webp", v.id);
    let src = v.thumb.as_deref().unwrap_or(&mq);
    if v.thumb.is_some() && v.thumb_w == 0 {
        // Widths unknown: plain 1x/2x.
        return img(out, src, v.thumb2x.as_deref(), eager, "");
    }
    out.push_str("<img src=\"");
    esc_into(out, src);
    out.push_str("\" srcset=\"");
    out.push_str(&mq);
    out.push_str(" 320w");
    for (u, w) in [(v.thumb.as_deref(), v.thumb_w), (v.thumb2x.as_deref(), v.thumb2x_w)] {
        if let Some(u) = u.filter(|_| w > 320) {
            out.push_str(", ");
            esc_into(out, u);
            out.push_str(&format!(" {w}w"));
        }
    }
    out.push_str("\" sizes=\"");
    out.push_str(plan.lay.sizes());
    out.push('"');
    img_tail(out, eager, high);
}

fn video_card(out: &mut String, v: &Video, plan: ImgPlan, i: usize) {
    let href = format!("/watch?v={}", v.id);
    out.push_str("<div class=\"c\" data-k=\"");
    out.push_str(&v.id);
    out.push_str("\" data-v=\"");
    out.push_str(&v.id);
    out.push_str("\"><a class=\"th\" href=\"");
    out.push_str(&href);
    out.push_str("\" tabindex=\"-1\">");
    video_img(out, v, plan, i);
    if v.live {
        out.push_str("<b class=\"lv\">LIVE</b>");
    } else if !v.duration.is_empty() {
        out.push_str("<b>");
        esc_into(out, &v.duration);
        out.push_str("</b>");
    }
    crate::features::card_bar(out, v);
    out.push_str("</a><div class=\"d\"><a class=\"t\" href=\"");
    out.push_str(&href);
    out.push_str("\" title=\"");
    esc_into(out, &v.title);
    out.push_str("\">");
    esc_into(out, &v.title);
    out.push_str("</a>");
    if !v.channel.is_empty() {
        out.push_str("<div class=\"m\">");
        match &v.channel_href {
            Some(h) => {
                out.push_str("<a href=\"");
                esc_into(out, h);
                out.push_str("\">");
                esc_into(out, &v.channel);
                out.push_str("</a>");
            }
            None => esc_into(out, &v.channel),
        }
        out.push_str("</div>");
    }
    if !v.meta.is_empty() {
        out.push_str("<div class=\"m\">");
        esc_into(out, &v.meta);
        out.push_str("</div>");
    }
    if let Some(hook) = CARD_HOOK.with(|h| h.get()) {
        hook(out, v);
    }
    out.push_str("</div></div>");
}

/// The animated hover preview, laid over a card's thumbnail.
pub fn preview(url: &str) -> String {
    let mut out = String::from("<img class=\"pv\" src=\"");
    esc_into(&mut out, url);
    out.push_str("\" alt=\"\" decoding=\"async\">");
    out
}

fn playlist_card(out: &mut String, p: &PlaylistCard, eager: bool) {
    // Mixes (RD…) cannot be browsed; open their first video instead.
    let href = match (&p.first_video, p.id.starts_with("RD")) {
        (Some(v), true) => format!("/watch?v={v}&list={}", p.id),
        _ => format!("/playlist?list={}", p.id),
    };
    out.push_str("<div class=\"c plc\"><a class=\"th\" href=\"");
    out.push_str(&href);
    out.push_str("\" tabindex=\"-1\">");
    let fallback = p.first_video.as_ref().filter(|v| is_video_id(v)).map(|v| format!("https://i.ytimg.com/vi/{v}/mqdefault.jpg"));
    if let Some(src) = p.thumb.as_deref().or(fallback.as_deref()) {
        img(out, src, None, eager, "");
    }
    out.push_str("<b class=\"n\">");
    esc_into(out, if p.count.is_empty() { "Playlist" } else { &p.count });
    out.push_str("</b></a><div class=\"d\"><a class=\"t\" href=\"");
    out.push_str(&href);
    out.push_str("\">");
    esc_into(out, &p.title);
    out.push_str("</a><div class=\"m\">");
    match &p.channel_href {
        Some(h) if !p.channel.is_empty() => {
            out.push_str("<a href=\"");
            esc_into(out, h);
            out.push_str("\">");
            esc_into(out, &p.channel);
            out.push_str("</a> · Playlist");
        }
        _ => {
            esc_into(out, &p.channel);
            out.push_str(if p.channel.is_empty() { "Playlist" } else { " · Playlist" });
        }
    }
    out.push_str("</div></div></div>");
}

fn channel_card(out: &mut String, c: &ChannelCard, eager: bool) {
    out.push_str("<div class=\"c chc\"><a class=\"th\" href=\"");
    esc_into(out, &c.href);
    out.push_str("\" tabindex=\"-1\">");
    if let Some(a) = &c.avatar {
        img(out, a, None, eager, "av");
    }
    out.push_str("</a><div class=\"d\"><a class=\"t\" href=\"");
    esc_into(out, &c.href);
    out.push_str("\">");
    esc_into(out, &c.title);
    out.push_str("</a><div class=\"m\">");
    esc_into(out, &c.meta);
    out.push_str("</div></div></div>");
}

/// Cards for `items`; see `ImgPlan` for image loading.
pub fn cards(out: &mut String, items: &[Item], plan: ImgPlan) {
    for (i, it) in items.iter().enumerate() {
        let eager = i < plan.eager;
        match it {
            Item::Video(v) => video_card(out, v, plan, i),
            Item::Playlist(p) => playlist_card(out, p, eager),
            Item::Channel(c) => channel_card(out, c, eager),
        }
    }
}

fn more(out: &mut String, cont: bool) {
    out.push_str("<div id=\"");
    out.push_str(MORE);
    out.push_str(if cont { "\" class=\"more\"></div>" } else { "\" class=\"more done\"></div>" });
}

/// The non-card part of a feed page (kept with snapshots so a page can be
/// repainted from data, never from stored HTML).
#[derive(Clone, Debug, PartialEq)]
pub enum Head {
    Feed { title: String },
    Channel { title: String, avatar: Option<String>, meta: Vec<String>, about: String },
    Playlist { title: String, meta: Vec<String> },
    /// Watch later in the order YouTube sent it: its newest-first sort failed (a note and Try again).
    Unsorted { title: String },
}

impl Head {
    pub fn title(&self) -> &str {
        match self {
            Head::Feed { title } | Head::Channel { title, .. } | Head::Playlist { title, .. } | Head::Unsorted { title } => title,
        }
    }
}

/// A feed page: header, the first `items` (already chosen by the caller), the
/// continuation sentinel. `empty` is shown when there are no items.
pub fn page(head: &Head, items: &[Item], more_items: bool, empty: &str, plan: ImgPlan) -> String {
    let mut out = String::with_capacity(1024 + items.len() * 1000);
    out.push_str("<section class=\"feed\">");
    match head {
        Head::Feed { title } => feed_heading(&mut out, title),
        Head::Channel { title, avatar, meta, about } => {
            out.push_str("<div class=\"ch\">");
            if let Some(a) = avatar {
                img(&mut out, a, None, true, "av lg");
            }
            out.push_str("<div><h1>");
            esc_into(&mut out, title);
            out.push_str("</h1><div class=\"m\">");
            esc_into(&mut out, &meta.join(" · "));
            out.push_str("</div>");
            if !about.is_empty() {
                out.push_str("<div class=\"m about\">");
                esc_into(&mut out, about);
                out.push_str("</div>");
            }
            out.push_str("</div></div><h2>Videos</h2>");
        }
        Head::Playlist { title, meta } => {
            feed_heading(&mut out, title);
            out.push_str("<div class=\"m sub\">");
            esc_into(&mut out, &meta.join(" · "));
            out.push_str("</div>");
        }
        Head::Unsorted { title } => {
            feed_heading(&mut out, title);
            out.push_str("<div class=\"m sub\" role=\"status\">Couldn’t sort newest first · <a href=\"");
            out.push_str(crate::features::watch_later::PAGE_URL);
            out.push_str("\" data-act=\"retry\">Try again</a></div>");
        }
    }
    if items.is_empty() {
        out.push_str("<p class=\"empty\">");
        esc_into(&mut out, empty);
        out.push_str("</p>");
    }
    out.push_str("<div id=\"");
    out.push_str(LIST);
    out.push_str("\" class=\"grid\">");
    cards(&mut out, items, plan);
    out.push_str("</div>");
    more(&mut out, more_items);
    out.push_str("</section>");
    out
}

pub fn feed_head(title: &str) -> Head {
    Head::Feed { title: title.to_string() }
}

pub fn channel_head(c: &ChannelPage) -> Head {
    Head::Channel { title: c.title.clone(), avatar: c.avatar.clone(), meta: c.meta.clone(), about: c.about.clone() }
}

pub fn playlist_head(p: &PlaylistPage, wl: bool, unsorted: bool) -> Head {
    if unsorted { return Head::Unsorted { title: "Watch later".into() }; }
    if wl { return feed_head("Watch later"); }
    Head::Playlist { title: p.title.clone(), meta: p.meta.clone() }
}

/// TV: a calm card in the middle of the content (a heading, a line, its buttons; the first is focused).
pub fn tv_prompt(title: &str, text: &str, buttons: &[(&str, &str, bool)]) -> String {
    let mut out = format!("<section class=\"tvpc\"><div class=\"tvcd\">{MARK}<h2>");
    esc_into(&mut out, title);
    out.push_str("</h2><p>");
    esc_into(&mut out, text);
    out.push_str("</p><div class=\"tvbs\">");
    for (act, label, primary) in buttons {
        out.push_str(&format!("<button type=\"button\" class=\"{}\" data-act=\"{act}\">", if *primary { "tvb" } else { "tvb tvx" }));
        if *act == "tv-search" {
            out.push_str(&format!("<svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"{GLASS}\"/></svg>"));
        }
        esc_into(&mut out, label);
        out.push_str("</button>");
    }
    out.push_str("</div></div></section>");
    out
}

pub fn home_nudge(nudge: &str, signed_in: bool) -> String {
    if crate::tv::on() {
        return if signed_in {
            // YouTube's own reason (e.g. history off), as a calm card with a way on.
            tv_prompt("Nothing on Home yet", nudge, &[("tv-search", "Search", true)])
        } else {
            tv_prompt("Make YouTube yours", "Sign in with your phone to see your recommendations, subscriptions, history and Watch later here. Search works without an account.", &[("tv-signin", "Sign in with your phone", true), ("tv-search", "Search", false)])
        };
    }
    // TV: no autofocus (a focused field opens the on-screen keyboard, which takes the remote's arrows).
    let autofocus = if crate::tv::on() { "" } else { " autofocus" };
    let mut out = format!("<section class=\"hero\"><h1>What do you want to watch?</h1><form id=\"ytc-search2\" role=\"search\" action=\"/results\"><input name=\"search_query\" type=\"search\" placeholder=\"Search YouTube\" autocomplete=\"off\"{autofocus}></form><p class=\"m\">");
    esc_into(&mut out, nudge);
    out.push_str("</p>");
    if !signed_in && crate::tv::on() {
        // TV: Google's sign-in does not run in the app; the device sign-in panel does (app/tvglue.rs).
        // The button gets its own row under the sentence (inline, it would overflow the text's line box).
        out.push_str("<p class=\"m\">Sign in for Home recommendations, Subscriptions, History and Watch later.</p><button type=\"button\" class=\"tvb tvh\" data-act=\"tv-signin\">Sign in with your phone</button>");
    } else if !signed_in {
        out.push_str("<p class=\"m\"><a href=\"https://accounts.google.com/ServiceLogin?service=youtube&amp;passive=true&amp;continue=https%3A%2F%2Fwww.youtube.com%2F\">Sign in</a> for Home recommendations, Subscriptions, History and Watch later.</p>");
    }
    out.push_str("</section>");
    out
}

/// TV: the rail's account item (Sign in with the device code, or the account screen).
pub fn tv_slot(signed_in: bool) -> String {
    let mut out = String::new();
    if signed_in {
        tv_rail_item(&mut out, "acct", "Account", PERSON, "", "tv-account");
    } else {
        tv_rail_item(&mut out, "acct", "Sign in", PERSON, "", "tv-signin");
    }
    out
}

/// The QR code of `url` as an inline SVG (dark modules on the light tile), or "" when it does not fit.
pub fn qr_svg(url: &str) -> String {
    let Some(q) = crate::qr::Qr::encode(url.as_bytes()) else { return String::new() };
    let n = q.size + 8;
    format!("<svg class=\"tvq\" viewBox=\"-4 -4 {n} {n}\" shape-rendering=\"crispEdges\" role=\"img\" aria-label=\"QR code of the sign-in page\"><path d=\"{}\"/></svg>", q.svg_path())
}

/// TV sign-in / account screen (full screen over the app). `code`: (user code, verification URL) while a
/// device sign-in runs; `status`: a line under the code.
pub fn tv_panel(code: Option<(&str, &str)>, signed_in: bool, status: &str) -> String {
    let mut out = format!("<div class=\"tvp\"><span class=\"rmk\">{MARK}<b>{SITE}</b></span><div class=\"tvl\">");
    let mut new_code = false;
    match code {
        Some((c, url)) => {
            // youtube.com/activate and Google's answer (google.com/device) open the same page
            // (accounts.google.com/device, checked 2 Nov 2026); the URL shown is the one Google returned, and the
            // QR code opens it.
            let shown = url.trim_start_matches("https://").trim_start_matches("www.");
            out.push_str("<h2>Sign in with your phone</h2><ol><li>Scan the code, or go to<b class=\"tvu\">");
            esc_into(&mut out, shown);
            out.push_str("</b></li><li>Enter this code</li></ol><p class=\"tvc\" aria-live=\"polite\">");
            esc_into(&mut out, c);
            out.push_str("</p><p class=\"tvst\">Then pick your Google account. This screen continues by itself.</p>");
            if !status.is_empty() {
                out.push_str("<p class=\"tvst\" role=\"status\">");
                esc_into(&mut out, status);
                out.push_str("</p>");
            }
            out.push_str("</div><figure class=\"tvqr\">");
            out.push_str(&qr_svg(url));
            out.push_str("<figcaption>Scan with your phone's camera</figcaption></figure>");
            new_code = true;
        }
        None if signed_in => {
            out.push_str(&format!("<span class=\"tva\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"{PERSON}\"/></svg></span><h2>Signed in</h2><p class=\"tvst\">This TV uses your YouTube account: history, subscriptions, Watch later and playlists are the same as on your other devices.</p></div>"));
        }
        None => {
            out.push_str("<h2>Sign in with your phone</h2>");
            if !status.is_empty() {
                out.push_str("<p class=\"tvst\" role=\"status\">");
                esc_into(&mut out, status);
                out.push_str("</p>");
            }
            out.push_str("</div>");
            new_code = !status.is_empty() && !status.ends_with('…');
        }
    }
    // Back is first (and focused): OK pressed by mistake never throws away a code being typed on the phone.
    out.push_str("<div class=\"tvbs\"><button type=\"button\" class=\"tvb tvx\" data-act=\"tv-close\">Back</button>");
    if new_code {
        out.push_str("<button type=\"button\" class=\"tvb tvx\" data-act=\"tv-newcode\">Get a new code</button>");
    }
    if signed_in && code.is_none() {
        out.push_str("<button type=\"button\" class=\"tvb tvx\" data-act=\"tv-signout\">Sign out</button>");
    }
    out.push_str("</div></div>");
    out
}

/// TV: the About & updates screen. `info` = the shell's last answer (None: not answered yet), `running` =
/// the interface build this page runs, `note` = a line about the last action (e.g. an install that failed).
/// `dev`: the Developer section (`tv_dev`), or "" while it is hidden.
pub fn tv_about(info: Option<&crate::tv::ShellInfo>, running: u64, running_client: &str, now: f64, note: &str, dev: &str) -> String {
    use crate::tv::UpdateView as V;
    let mut out = format!("<div class=\"tvp\"><span class=\"rmk\">{MARK}<b>{SITE}</b></span><div class=\"tvl tvab\"><h2>About &amp; updates</h2><dl class=\"tvv\">");
    let row = |out: &mut String, k: &str, v: &str| {
        out.push_str("<dt>");
        esc_into(out, k);
        out.push_str("</dt><dd>");
        esc_into(out, v);
        out.push_str("</dd>");
    };
    let view = info.map(|i| crate::tv::update_view(i, running));
    match info {
        Some(i) => {
            row(&mut out, "App", &format!("Redline for TV {}", i.version));
            // (The interface this screen runs; a newer one in the shell is the update.)
            let (client, build) = if running > 0 && !running_client.is_empty() { (running_client, running) } else { (i.client.as_str(), i.seq) };
            // (The build id in brackets is for bug reports: the version and the build number say enough here.)
            let client = client.split(" (").next().unwrap_or(client);
            row(&mut out, "Interface", &format!("{client} · build {build}"));
            let when = if i.at > 0.0 { crate::tv::ago(i.at, now) } else { "not yet".to_string() };
            let result = match &view {
                Some(V::Failed(_)) => " · failed",
                Some(V::UpToDate) => " · up to date",
                Some(V::Ready | V::App(_) | V::NeedsApp) => " · update found",
                _ => "",
            };
            row(&mut out, "Last check", &format!("{when}{result}"));
        }
        None => row(&mut out, "App", "Asking the app…"),
    }
    out.push_str("</dl>");
    // The state, and the buttons it offers (the first is focused).
    let (cls, head, detail, buttons): (&str, String, String, Vec<(&str, &str, bool)>) = match &view {
        None | Some(V::NotChecked) => ("", "Check for a new version".into(), "Redline also checks by itself when it starts and every 6 hours.".into(), vec![("tv-check", "Check for updates", true)]),
        Some(V::Checking) => ("busy", "Checking for updates…".into(), "Asking GitHub for the latest release.".into(), vec![("tv-check", "Check for updates", true)]),
        Some(V::UpToDate) => ("ok", "Redline is up to date".into(), info.map(|i| format!("App {} · interface {} (build {}).", i.version, i.client.split(" (").next().unwrap_or(&i.client), i.seq)).unwrap_or_default(), vec![("tv-check", "Check again", true)]),
        Some(V::Ready) => (
            "new",
            "Update ready".into(),
            info.map(|i| format!("A new interface ({}, build {}) is downloaded and verified. Update now restarts the screen with it; you stay signed in.", i.client.split(" (").next().unwrap_or(&i.client), i.seq)).unwrap_or_default(),
            if info.and_then(|i| i.apk.as_ref()).is_some() { vec![("tv-apply", "Update now", true), ("tv-install", "Install app update", false)] } else { vec![("tv-apply", "Update now", true)] },
        ),
        Some(V::App(v)) => ("new", format!("Redline {v} is available"), "Install update downloads it, checks its signature and opens Android's installer. When Android has installed it, choose Open.".into(), vec![("tv-install", "Install update", true)]),
        Some(V::Downloading(p)) => ("busy", match p { Some(p) => format!("Downloading the update… {p} %"), None => "Downloading the update…".into() }, "Android's installer opens when it is complete.".into(), vec![]),
        Some(V::Installing) => ("busy", "Opening Android's installer…".into(), "Choose Install, then Open. (The first time, Android asks to allow installs from this app: allow it, then come back.)".into(), vec![("tv-install", "Install again", true)]),
        Some(V::NeedsApp) => ("", "A new version needs a newer app".into(), "It will be offered here as soon as it is published.".into(), vec![("tv-check", "Check again", true)]),
        Some(V::Failed(e)) => ("err", crate::tv::update_error(e).into(), e.clone(), vec![("tv-check", "Retry", true)]),
    };
    out.push_str(&format!("<div class=\"tvus {cls}\" role=\"status\" aria-live=\"polite\"><b>"));
    esc_into(&mut out, &head);
    out.push_str("</b>");
    if let Some(V::Downloading(Some(p))) = &view {
        out.push_str(&format!("<i class=\"tvpg\" style=\"--pg:{}\"><i></i></i>", *p as f64 / 100.0));
    }
    if !detail.is_empty() {
        out.push_str("<p>");
        esc_into(&mut out, &detail);
        out.push_str("</p>");
    }
    if !note.is_empty() {
        out.push_str("<p class=\"tvun\">");
        esc_into(&mut out, note);
        out.push_str("</p>");
    }
    out.push_str("</div></div><div class=\"tvbs\">");
    for (act, label, primary) in &buttons {
        out.push_str(&format!("<button type=\"button\" class=\"{}\" data-act=\"{act}\">", if *primary { "tvb" } else { "tvb tvx" }));
        esc_into(&mut out, label);
        out.push_str("</button>");
    }
    out.push_str("<button type=\"button\" class=\"tvb tvx\" data-act=\"tv-about-close\">Back</button></div>");
    out.push_str(dev);
    out.push_str("</div>");
    out
}

/// About & updates → Developer (hidden until revealed, or while telemetry is on): speed telemetry to the owner's debug
/// center. `t`: the shell's answer (None: this app has no telemetry); `note`: the last action's outcome (`err`: a failure).
pub fn tv_dev(t: Option<&crate::tv::TelInfo>, note: &str, err: bool) -> String {
    let mut out = String::from("<section class=\"tvus tvdv\" aria-label=\"Developer\"><b>Developer · speed telemetry</b><p>");
    let Some(t) = t else {
        out.push_str("This app version cannot send telemetry: install the newest Redline for TV first.</p></section>");
        return out;
    };
    let addr = crate::tv::tel_addr(&t.url);
    if t.on {
        out.push_str("On: this TV's speed measurements go to the debug center at ");
        esc_into(&mut out, addr);
        out.push_str(&format!(" ({} sent, {} waiting).", t.sent, t.queued));
        if !t.dev {
            out.push_str(" This interface has no telemetry build: only errors are sent.");
        }
    } else {
        out.push_str("Off. Turned on, this TV sends its speed measurements (timings and video ids, never titles or searches) to the debug center on your computer, started with --lan.");
    }
    out.push_str("</p><form id=\"ytc-tvdc\" class=\"tvdc\"><input id=\"ytc-tvdc-q\" name=\"dc\" type=\"text\" inputmode=\"url\" autocomplete=\"off\" spellcheck=\"false\" aria-label=\"Debug center address\" placeholder=\"Its address, e.g. 192.168.1.20\" value=\"");
    esc_into(&mut out, addr);
    out.push_str("\"></form><div class=\"tvdb\">");
    out.push_str(if t.on { "<button type=\"button\" class=\"tvb tvx\" data-act=\"tv-tel-off\">Turn off</button>" } else { "<button type=\"button\" class=\"tvb\" data-act=\"tv-tel-on\">Turn on</button>" });
    out.push_str("<button type=\"button\" class=\"tvb tvx\" data-act=\"tv-tel-find\">Find on network</button></div>");
    let (line, err) = if !note.is_empty() { (note.to_string(), err) } else if !t.error.is_empty() { (format!("Last delivery failed: {}", t.error), true) } else { (String::new(), false) };
    if !line.is_empty() {
        out.push_str(if err { "<p class=\"tvun\">" } else { "<p>" });
        esc_into(&mut out, &line);
        out.push_str("</p>");
    }
    out.push_str("</section>");
    out
}

pub fn watch_shell(player_html: &str, info: &str, theater: bool) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str(if theater { "<div id=\"ytc-watch\" class=\"watch theater\">" } else { "<div id=\"ytc-watch\" class=\"watch\">" });
    out.push_str("<div class=\"wp\"><div id=\"ytc-player\" class=\"player own paused wait\">");
    out.push_str(player_html);
    out.push_str("</div><div id=\"");
    out.push_str(INFO);
    out.push_str("\">");
    out.push_str(info);
    out.push_str("</div><section id=\"");
    out.push_str(COMMENTS);
    if crate::tv::on() {
        out.push_str("\" class=\"cmts tvcm\" role=\"dialog\" aria-modal=\"true\" aria-label=\"Comments\"><header class=\"tvcm-head\"><button type=\"button\" class=\"tvb tvx\" data-act=\"tv-comments-close\">Back to video</button><h2>Comments</h2></header><div id=\"ytc-tvcm-body\" class=\"tvcm-body\"><p class=\"yc-note\">Loading comments…</p></div>");
    } else {
        out.push_str("\" class=\"cmts\">");
    }
    out.push_str("</section></div><aside class=\"wr\"><section id=\"");
    out.push_str(PLAYLIST);
    out.push_str("\" class=\"plw\"></section><div id=\"");
    out.push_str(UPNEXT);
    out.push_str("\" class=\"up\"></div><div id=\"");
    out.push_str(LIST);
    out.push_str("\" class=\"rel\">");
    for _ in 0..8 {
        out.push_str(SK_CARD);
    }
    out.push_str("</div>");
    more(&mut out, false);
    out.push_str("</aside></div>");
    out
}

/// The channel row: avatar, name + subscribers, the Subscribe slot, then the
/// actions-row slot on the right. Same geometry while loading.
fn owner_row(out: &mut String, avatar: Option<&str>, name: &str, href: Option<&str>, subs: &str) {
    out.push_str("<div class=\"wo\">");
    match avatar {
        Some(a) => img(out, a, None, true, "av"),
        None => out.push_str("<span class=\"av\"></span>"),
    }
    out.push_str("<div class=\"wn\">");
    match href {
        Some(h) => {
            out.push_str("<a class=\"wc\" href=\"");
            esc_into(out, h);
            out.push_str("\">");
            esc_into(out, name);
            out.push_str("</a>");
        }
        None => {
            out.push_str("<span class=\"wc\">");
            esc_into(out, name);
            out.push_str("</span>");
        }
    }
    out.push_str("<div class=\"m\">");
    esc_into(out, subs);
    out.push_str(&format!("</div></div><div id=\"{SUBSCRIBE}\" class=\"wsub\"></div><div id=\"{ACTIONS}\" class=\"wa\"></div></div>"));
}

/// Title/channel from a feed card while `next` is still loading.
pub fn watch_info_partial(v: Option<&Video>) -> String {
    let mut out = String::from("<h1 class=\"wt\">");
    match v {
        Some(v) => esc_into(&mut out, &v.title),
        None => out.push_str("<i class=\"sk l w60\"></i>"),
    }
    out.push_str("</h1>");
    owner_row(&mut out, None, v.map_or("", |v| v.channel.as_str()), None, "");
    out.push_str("<div class=\"desc sk\"></div>");
    out
}

pub(crate) fn runs(out: &mut String, runs: &[Run], video_id: &str) {
    for r in runs {
        match &r.link {
            None => esc_into(out, &r.text),
            Some(Link::Ext(u)) => {
                out.push_str("<a href=\"");
                esc_into(out, u);
                out.push_str("\" target=\"_blank\" rel=\"noopener noreferrer nofollow\">");
                esc_into(out, &r.text);
                out.push_str("</a>");
                crate::features::link_control(out, u);
            }
            Some(Link::Int(h)) => {
                out.push_str("<a href=\"");
                esc_into(out, h);
                out.push_str("\">");
                esc_into(out, &r.text);
                out.push_str("</a>");
            }
            Some(Link::Seek(t)) => {
                out.push_str(&format!("<a href=\"/watch?v={video_id}&amp;t={t}s\" data-act=\"seek\" data-arg=\"{t}\">"));
                esc_into(out, &r.text);
                out.push_str("</a>");
            }
        }
    }
}

pub fn watch_info(w: &Watch, video_id: &str) -> String {
    let mut out = String::with_capacity(4096);
    out.push_str("<h1 class=\"wt\">");
    esc_into(&mut out, &w.title);
    out.push_str("</h1>");
    owner_row(&mut out, w.avatar.as_deref(), &w.channel, w.channel_href.as_deref(), &w.subs);
    description(&mut out, w, video_id);
    out
}

/// The description panel as the old extension showed it:
/// views and date on top (separate, one typography), then only the lines that
/// carry a resource link (`crate::description`), fully open at natural height.
/// No views/date and no resources: the panel stays hidden.
fn description(out: &mut String, w: &Watch, video_id: &str) {
    let rows = crate::description::resource_rows(&w.desc, video_id);
    let meta: Vec<&str> = [w.views.as_str(), w.date.as_str()].into_iter().map(str::trim).filter(|s| !s.is_empty()).collect();
    out.push_str("<section class=\"desc\" id=\"ytc-desc\" aria-label=\"Video description\"");
    if meta.is_empty() && rows.is_empty() {
        out.push_str(" hidden");
    }
    out.push('>');
    if !meta.is_empty() {
        out.push_str("<div class=\"ws\">");
        for m in meta {
            out.push_str("<span>");
            esc_into(out, m);
            out.push_str("</span>");
        }
        out.push_str("</div>");
    }
    if !rows.is_empty() {
        out.push_str("<div class=\"dl\">");
        for row in &rows {
            out.push_str("<p class=\"dr\">");
            runs(out, row, video_id);
            out.push_str("</p>");
        }
        out.push_str("</div>");
    }
    out.push_str("</section>");
}

/// TV: the watch page's readable details, with the same links, timestamps and chapters as the video.
pub fn tv_info(w: &Watch, video_id: &str) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("<header class=\"tvih\"><button type=\"button\" class=\"tvb tvx\" data-act=\"tv-info-close\">Back to video</button></header><div id=\"ytc-tvinfo-body\" class=\"tvib\"><h2>");
    esc_into(&mut out, &w.title);
    out.push_str("</h2><p class=\"m tvim\">");
    let meta: Vec<&str> = [w.channel.as_str(), w.views.as_str(), w.date.as_str()].into_iter().map(str::trim).filter(|s| !s.is_empty()).collect();
    esc_into(&mut out, &meta.join(" · "));
    out.push_str("</p><div class=\"tvia\">");
    if let Some(href) = w.channel_href.as_deref().filter(|_| !w.channel.trim().is_empty()) {
        out.push_str("<a class=\"tvb tvx\" href=\"");
        esc_into(&mut out, href);
        out.push_str("\">View channel</a>");
    }
    out.push_str("<span id=\"ytc-tvsub\"></span></div><div id=\"ytc-tvact-msg\" class=\"tvam\" role=\"status\"></div><div class=\"tvd\" tabindex=\"0\" aria-label=\"Description\">");
    if w.desc.iter().any(|r| !r.text.trim().is_empty()) {
        runs(&mut out, &w.desc, video_id);
    } else {
        out.push_str("<span class=\"m\">No description</span>");
    }
    out.push_str("</div>");
    if !w.acts.chapters.is_empty() {
        out.push_str("<section class=\"tvchap\" aria-label=\"Chapters\"><h3>Chapters</h3><ol>");
        for chapter in &w.acts.chapters {
            out.push_str(&format!("<li><button type=\"button\" data-act=\"seek\" data-arg=\"{}\"><span>", chapter.start));
            esc_into(&mut out, &crate::actions::watch::clock(chapter.start));
            out.push_str("</span><span>");
            esc_into(&mut out, &chapter.title);
            out.push_str("</span></button></li>");
        }
        out.push_str("</ol></section>");
    }
    out.push_str("</div>");
    out
}

pub fn related(out: &mut String, items: &[Item]) {
    cards(out, items, ImgPlan::first(Lay::Rel, 1));
}

pub fn message(title: &str, detail: &str, retry_href: Option<&str>, native: &str) -> String {
    if crate::tv::on() {
        return match retry_href {
            Some(r) => {
                // A failed page: the reason and Try again (a link, so OK reloads the page).
                let mut out = format!("<section class=\"tvpc\"><div class=\"tvcd\">{MARK}<h2>");
                esc_into(&mut out, title);
                out.push_str("</h2><p>");
                esc_into(&mut out, detail);
                out.push_str("</p><div class=\"tvbs\"><a class=\"tvb\" href=\"");
                esc_into(&mut out, r);
                out.push_str("\" data-act=\"retry\">Try again</a></div></div></section>");
                out
            }
            None => tv_prompt(title, "Subscriptions, History, Watch later and your playlists need your YouTube account. Sign in with your phone: it takes a minute.", &[("tv-signin", "Sign in with your phone", true)]),
        };
    }
    let mut out = String::from("<section class=\"msg\"><h1>");
    esc_into(&mut out, title);
    out.push_str("</h1><p class=\"m\">");
    esc_into(&mut out, detail);
    out.push_str("</p><p>");
    if let Some(r) = retry_href {
        out.push_str("<a href=\"");
        esc_into(&mut out, r);
        out.push_str("\" data-act=\"retry\">Try again</a> · ");
    }
    let _ = native; // No escape link to YouTube's own site in the header (the player panel has one).
    out.push_str("</p></section>");
    out
}

pub struct DebugInfo<'a> {
    pub route: &'a str,
    /// Already escaped.
    pub player: &'a str,
    pub stats: &'a Stats,
    pub marks: &'a [(&'static str, f64)],
    pub version: &'a str,
    pub visitor: bool,
    pub cookie_signed_in: bool,
    pub server_logged_out: Option<bool>,
    pub auth_user: u32,
    /// (watch, pages, prefetched pages)
    pub cache: (usize, usize, usize),
    /// (cards shown, preloaded in memory, more available)
    pub feed: (usize, usize, bool),
}

pub fn debug(d: &DebugInfo) -> String {
    let mut s = format!("<b>Redline {} debug</b> (press ` or Shift+D to close)\n", env!("CARGO_PKG_VERSION"));
    s.push_str(&format!("route  {}\n", esc(d.route)));
    s.push_str(&format!(
        "client WEB {} · visitor {} · SAPISID cookie {} · server loggedOut {} · authuser {}\n",
        esc(d.version),
        if d.visitor { "yes" } else { "no" },
        if d.cookie_signed_in { "yes" } else { "no" },
        d.server_logged_out.map(|b| b.to_string()).unwrap_or_else(|| "?".into()),
        d.auth_user
    ));
    s.push_str(d.player);
    s.push_str(&format!("cache  {} watch · {} pages · {} prefetched (memory) · feed snapshots in sessionStorage\n", d.cache.0, d.cache.1, d.cache.2));
    s.push_str(&format!("feed   {} shown · {} preloaded · more {}\n", d.feed.0, d.feed.1, if d.feed.2 { "yes" } else { "no" }));
    s.push_str(&format!("parse  shorts hidden {} · ads skipped {} · recommendation shelves {} · duplicates {} · invalid {} · unknown {}\n", d.stats.shorts, d.stats.ads, d.stats.recs, d.stats.dupes, d.stats.bad, d.stats.unknown_total()));
    for (k, n) in &d.stats.unknown {
        s.push_str(&format!("       {} × {}\n", esc(k), n));
    }
    s.push_str("timing (ms since navigation start)\n");
    for (k, t) in d.marks {
        s.push_str(&format!("       {k:<14} {t:>8.1}\n"));
    }
    format!("<pre>{s}</pre>")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_product_shows_youtube_and_redline_is_only_the_extensions_name() {
        // Inside the product: YouTube's logo, wordmark, accessible name and title (as before 0.14.0).
        for m in [LOGO, MARK] {
            assert!(m.contains("viewBox=\"0 0 28 20\"") && m.contains("<rect width=\"28\" height=\"20\" rx=\"5\"/><path d=\"M11 5.5v9l8-4.5z\"/>") && m.contains("aria-hidden=\"true\""));
            assert!(!m.contains(MARK_PATH));
        }
        let page = shell(&Route::Home, "/", Some(""));
        assert!(page.contains("<title>YouTube</title>") && page.contains("aria-label=\"YouTube home\" data-hov=\"logo\"><svg viewBox=\"0 0 28 20\"") && page.contains("<span>YouTube</span></a>"));
        assert!(!page.contains("Redline") && !page.contains("Lumen") && !page.contains(MARK_PATH));
        // The extension's own icon: one source file, inlined only in the toolbar popup.
        let svg = include_str!("../browser/icons/redline.svg");
        assert!(svg.contains(&format!("d=\"{MARK_PATH}\"")), "browser/icons/redline.svg and render.rs carry the same mark");
        assert!(svg.contains("viewBox=\"0 0 32 24\"") && svg.contains("fill-rule=\"evenodd\""));
        assert!(POPUP_MARK.contains(MARK_PATH) && POPUP_MARK.contains("viewBox=\"0 0 32 24\"") && POPUP_MARK.contains("aria-hidden=\"true\""));
    }

    #[test]
    fn tv_details_keep_safe_links_and_chapter_actions() {
        let mut w = Watch {
            title: "A <video>".into(),
            channel: "A channel".into(),
            channel_href: Some("/@a?x=\"bad\"".into()),
            desc: vec![Run { text: "At 0:12 <script>".into(), link: Some(Link::Seek(12)) }],
            ..Default::default()
        };
        w.acts.chapters.push(crate::actions::watch::Chapter { start: 12, title: "Setup <script>".into(), ..Default::default() });
        let h = tv_info(&w, "abcdefghijk");
        assert!(h.contains("A &lt;video&gt;") && !h.contains("<script>"));
        assert!(h.contains("/@a?x=&quot;bad&quot;") && h.contains("View channel"));
        assert_eq!(h.matches("data-act=\"seek\" data-arg=\"12\"").count(), 2);
        assert!(h.contains("data-act=\"tv-info-close\"") && h.contains("id=\"ytc-tvsub\""));
        crate::tv::set_on(true);
        let tv = watch_shell("", "", false);
        crate::tv::set_on(false);
        let desktop = watch_shell("", "", false);
        assert!(tv.contains("id=\"ytc-tvcm-body\"") && tv.contains("aria-label=\"Comments\""));
        assert_eq!(tv.matches("id=\"ytc-comments\"").count(), 1);
        assert!(!desktop.contains("tvcm-body") && !desktop.contains("tv-comments-close"));
    }

    #[test]
    fn hostile_text_is_escaped() {
        let evil = "<img src=x onerror=\"alert(1)\">'&";
        let v = Video {
            id: "abcdefghijk".into(),
            title: evil.into(),
            channel: evil.into(),
            channel_href: Some("/@x\"onmouseover=\"y".into()),
            meta: evil.into(),
            duration: evil.into(),
            ..Default::default()
        };
        let mut out = String::new();
        cards(&mut out, &[Item::Video(v)], ImgPlan::first(Lay::Grid, 4));
        assert!(!out.contains("<img src=x"));
        assert!(!out.contains("\"alert"));
        assert!(!out.contains("\"onmouseover"));
        assert!(out.contains("&lt;img src=x onerror=&quot;alert(1)&quot;&gt;&#39;&amp;"));
        let w = Watch { title: evil.into(), desc: vec![Run { text: evil.into(), link: Some(Link::Ext("https://e.com/?a=\"><script>".into())) }], ..Default::default() };
        let h = watch_info(&w, "abcdefghijk");
        assert!(!h.contains("<script>") && !h.contains("<img src=x"));
        let s = shell(&Route::Search { q: evil.into(), sp: None }, "/results?search_query=%3C", None);
        assert!(!s.contains("<img src=x"));
        assert!(!suggestions(&[evil.into()]).contains("<img src=x"));
        assert!(!preview("x\"><img src=x>").contains("<img src=x"));
    }

    #[test]
    fn description_panel_markup() {
        let r = |text: &str, link: Option<Link>| Run { text: text.into(), link };
        let w = Watch {
            views: "386,361 views".into(),
            date: "Oct 9, 2026".into(),
            desc: vec![
                r("Intro prose. #tag\nFiles:\n", None),
                r("Download", Some(Link::Ext("https://files.example/dl".into()))),
                r("\n", None),
                r("0:30", Some(Link::Seek(30))),
                r(" intro", None),
            ],
            ..Default::default()
        };
        let h = watch_info(&w, "abcdefghijk");
        let d = &h[h.find("<section class=\"desc\"").unwrap()..];
        assert!(d.starts_with("<section class=\"desc\" id=\"ytc-desc\" aria-label=\"Video description\"><div class=\"ws\"><span>386,361 views</span><span>Oct 9, 2026</span></div><div class=\"dl\"><p class=\"dr\">Files: <a href=\"https://files.example/dl\" target=\"_blank\""), "{d}");
        assert!(d.contains("</a><button type=\"button\" class=\"fx-blk\" data-act=\"fx-block\" data-arg=\"files.example\">Block files.example</button></p></div></section>"), "{d}");
        assert!(!d.contains("Intro") && !d.contains("#tag") && !d.contains("0:30") && !d.contains("data-act=\"seek\"") && !d.contains("lnk") && !d.contains("clamp"));
        // No resources: views/date only.
        let h = watch_info(&Watch { views: "1 view".into(), desc: vec![r("Only prose", None)], ..Default::default() }, "abcdefghijk");
        assert!(h.contains("<section class=\"desc\" id=\"ytc-desc\" aria-label=\"Video description\"><div class=\"ws\"><span>1 view</span></div></section>"), "{h}");
        // Nothing at all: hidden.
        assert!(watch_info(&Watch::default(), "abcdefghijk").contains("aria-label=\"Video description\" hidden></section>"));
    }

    #[test]
    fn layout_hooks_and_skeleton_geometry() {
        fn hook(out: &mut String, v: &Video) {
            out.push_str("<button class=\"ca\" data-v=\"");
            out.push_str(&v.id);
            out.push_str("\"></button>");
        }
        let v = Video { id: "abcdefghijk".into(), title: "T".into(), ..Default::default() };
        let mut out = String::new();
        cards(&mut out, &[Item::Video(v.clone())], ImgPlan::lazy(Lay::Grid));
        assert!(out.starts_with("<div class=\"c\" data-k=\"abcdefghijk\" data-v=\"abcdefghijk\">") && !out.contains("class=\"ca\""));
        set_card_hook(hook);
        let mut out = String::new();
        cards(&mut out, &[Item::Video(v)], ImgPlan::lazy(Lay::Grid));
        assert!(out.ends_with("<button class=\"ca\" data-v=\"abcdefghijk\"></button></div></div>"), "{out}");
        // The sidebar: the seven links in their fixed order.
        let s = shell(&Route::Home, "/", None);
        assert!(!s.contains("Shorts"), "no Shorts entry");
        assert_eq!(go_href("w"), Some("/playlist?list=WL"));
        assert!(s.contains("data-nav=\"wl\" aria-keyshortcuts=\"g w\""));
        assert!(keys_help().contains("<dt>g w · g p · g l</dt>"));
        let order: Vec<usize> = ["Home", "History", "Watch later", "Subscriptions", "Playlists", "Liked videos"].iter().map(|l| s.find(&format!("</svg>{l}</a>")).unwrap()).collect();
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        assert!(!s.contains("title="), "no tooltips");
        // Skeleton and page start with the same heading row; watch slots exist while loading.
        assert!(skeleton(&Route::Subs).contains("<div class=\"fh\"><h1>Subscriptions</h1><div id=\"ytc-filters\""));
        let p = page(&feed_head("Subscriptions"), &[], false, "none", ImgPlan::lazy(Lay::Grid));
        assert!(p.contains("<div class=\"fh\"><h1>Subscriptions</h1><div id=\"ytc-filters\""));
        let w = skeleton(&Route::Watch { v: "abcdefghijk".into(), t: 0, list: None });
        for id in [SUBSCRIBE, ACTIONS, COMMENTS] {
            assert!(w.contains(&format!("id=\"{id}\"")), "{id}");
        }
        let full = watch_info(&Watch { title: "T".into(), channel: "C".into(), ..Default::default() }, "abcdefghijk");
        assert!(full.contains("id=\"ytc-sub\"") && full.contains("id=\"ytc-acts\""));
        assert_eq!(grid_cols(2560.0), 5);
        assert_eq!(grid_cols(1400.0), 4);
        assert_eq!(grid_cols(700.0), 2);
        // TV: five cards per row on a 960 CSS px WebView as on a 1920 one (the desktop would give 3 and 5).
        assert_eq!((page_cols(960.0, true), page_cols(1920.0, true), page_cols(960.0, false)), (5, 5, 3));
        assert_eq!(ImgPlan::first(Lay::Grid, page_cols(960.0, true)).high, 5);
        // A batch of 24 after 20 / 23 / 25 cards on a five-column grid: 25 / 27 / 25, every row full.
        assert_eq!([20, 23, 25].map(|n| row_chunk(n, TV_COLS, 24)), [25, 27, 25]);
        assert!((0..40).all(|n| (n + row_chunk(n, TV_COLS, 24)) % TV_COLS == 0));
    }
}
