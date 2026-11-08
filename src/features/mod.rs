//! The extension's features rebuilt natively in the client: Watch later
//! button, Home/Search filters, blocked websites, custom topic feeds, search
//! history and resume positions/progress bars. Pure logic lives in the
//! submodules (unit tested); `app/fxglue.rs` wires them to the app; this
//! file holds the state shared with `render.rs` and the markup of the
//! filter bar.
//!
//! Stored (chrome.storage.local, these keys only): the seven filter
//! preferences (incl. the Shorts switch, `shortsInFeeds`), `yourTubeHistoryV1`, `yourTubeProgressV1`,
//! `yourTubeBlockedWebsitesV1`, `yourTubeRecommendationTopicsV1`. Never
//! stored: descriptions, recommendation payloads, continuation tokens,
//! filter verdicts. Voice search (`voice`) adds `ytcVoiceKeyV1` (the user's Deepgram key), whose value only
//! the background worker reads.

pub mod filters;
pub mod history;
pub mod home_chips;
pub mod import;
pub mod members;
pub mod progress;
pub mod search_filters;
pub mod topics;
pub mod upnext;
pub mod voice;
pub mod watch_later;
pub mod websites;

use crate::parse::Video;
use crate::util::esc_into;
use filters::{PRESETS, Prefs, Surface};
use std::cell::RefCell;

/// State that card/description rendering needs (read by `render.rs`).
#[derive(Default)]
pub struct Shared {
    pub signed_in: bool,
    pub progress: progress::Store,
    pub saves: watch_later::Saves,
    pub rules: websites::Rules,
    pub members: members::Known,
}

thread_local! {
    static SHARED: RefCell<Shared> = RefCell::new(Shared::default());
}

/// Runs `f` on the shared state (skipped if it is already borrowed).
pub fn shared<R: Default>(f: impl FnOnce(&mut Shared) -> R) -> R {
    SHARED.with(|c| c.try_borrow_mut().map(|mut s| f(&mut s)).unwrap_or_default())
}

// ------------------------------------------------------------ render hooks

/// Card thumbnail progress bar: the local resume record when there is one,
/// else YouTube's own value (`fallback`).
pub fn card_bar(out: &mut String, v: &Video) {
    let local = shared(|s| s.progress.bar(&v.id));
    match local {
        Some((pct, finished)) => {
            if finished && pct < 100 {
                // A faint full track keeps the earlier finish visible under a rewatch.
                out.push_str("<i class=\"pg trk\"></i>");
            }
            out.push_str(&format!("<i class=\"pg own\" style=\"width:{pct}%\"></i>"));
        }
        None if v.progress > 0 => out.push_str(&format!("<i class=\"pg\" style=\"width:{}%\"></i>", v.progress)),
        None => {}
    }
}

/// The thumbnail's "Members only" label (the duration's own row, at the other end). Beside a long duration
/// on a small thumbnail it reads "Members" (`dl`, client.css).
pub fn card_badge(out: &mut String, v: &Video) {
    if v.members || members::has(&v.id) {
        out.push_str(if v.duration.len() > 7 { "<b class=\"mo dl\">Members<span> only</span></b>" } else { "<b class=\"mo\">Members only</b>" });
    }
}

/// Card actions slot (inside `.d`): the Watch later button when signed in.
pub fn card_actions(out: &mut String, v: &Video) {
    let state = shared(|s| s.signed_in.then(|| s.saves.get(&v.id)));
    if let Some(st) = state {
        watch_later::button(out, &v.id, &st);
    }
}

/// The Watch later button for video `id` (signed in only; the TV player's Save button).
pub fn wl_button(out: &mut String, id: &str) {
    let state = shared(|s| s.signed_in.then(|| s.saves.get(id)));
    if let Some(st) = state {
        watch_later::button(out, id, &st);
    }
}

/// Block control beside an external description link.
pub fn link_control(out: &mut String, url: &str) {
    SHARED.with(|c| {
        if let Ok(s) = c.try_borrow() {
            websites::link_control(out, url, &s.rules);
        }
    });
}

// ------------------------------------------------------------ filter bar

pub const BAR: &str = crate::render::FILTERS;
pub const NOTE: &str = "ytc-fx-note";
/// The chip row (a sideways scroller; the selection pill slides inside it).
pub const CHIPS: &str = "ytc-fx-tc";
/// The Filters panel's segmented rows (each has its own sliding selection pill).
pub const DUR_ROW: &str = "ytc-fx-durrow";
pub const ORDER_ROW: &str = "ytc-fx-ordrow";
/// The minimum-length slider (`--v` = its value / max paints the fill).
pub const RANGE: &str = "ytc-fx-range";

/// `--v` for the slider fill at `m` minutes.
pub fn range_fill(m: u32) -> String {
    format!("{:.4}", f64::from(m.min(filters::MAX_MINUTES)) / f64::from(filters::MAX_MINUTES))
}

/// Stable id of a bar control (focus returns to it after a re-render).
pub fn control_id(act: &str, arg: &str) -> String {
    let arg: String = arg.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(24).collect();
    if arg.is_empty() { format!("ytc-{act}") } else { format!("ytc-{act}-{arg}") }
}

fn switch(out: &mut String, act: &str, label: &str, on: bool, hint: &str) {
    out.push_str("<button type=\"button\" class=\"fx-sw\" role=\"switch\" id=\"");
    out.push_str(&control_id(act, ""));
    out.push_str("\" data-act=\"");
    out.push_str(act);
    out.push_str("\" aria-checked=\"");
    out.push_str(if on { "true" } else { "false" });
    out.push_str("\" title=\"");
    esc_into(out, hint);
    out.push_str("\"><span>");
    esc_into(out, label);
    out.push_str("</span><i aria-hidden=\"true\"></i></button>");
}

fn seg(out: &mut String, act: &str, arg: &str, label: &str, on: bool) {
    out.push_str("<button type=\"button\" class=\"fx-ch\" id=\"");
    out.push_str(&control_id(act, arg));
    out.push('"');
    out.push_str(" data-act=\"");
    out.push_str(act);
    out.push_str("\" data-arg=\"");
    esc_into(out, arg);
    out.push_str("\" aria-pressed=\"");
    out.push_str(if on { "true" } else { "false" });
    out.push_str("\">");
    esc_into(out, label);
    out.push_str("</button>");
}

/// Short summary on the Filters button ("20+ min · Popular").
pub fn summary(p: &Prefs, surf: Surface) -> String {
    let mut parts = Vec::new();
    if p.min_minutes > 0 {
        parts.push(format!("{}+ min", p.min_minutes));
    }
    if surf == Surface::Home {
        if p.popular {
            parts.push("Popular".into());
        }
        if p.playlists_only {
            parts.push("Playlists".into());
        }
    }
    parts.join(" · ")
}

/// A search-filter link: YouTube's own `params` for the same query.
fn sf_link(out: &mut String, idp: &str, q: &str, label: &str, target: &search_filters::Target, current: bool, extra: &str) {
    let id = control_id(idp, label);
    out.push_str("<a class=\"fx-ch");
    out.push_str(extra);
    out.push_str("\" id=\"");
    out.push_str(&id);
    out.push_str("\" data-act=\"fx-sf\" data-arg=\"");
    out.push_str(&id);
    out.push_str("\" href=\"");
    esc_into(out, &crate::route::Route::Search { q: q.to_string(), sp: target.clone() }.url());
    out.push('"');
    if current {
        out.push_str(" aria-current=\"true\" title=\"Remove this filter\"");
    }
    out.push('>');
    esc_into(out, label);
}

fn sf_option(out: &mut String, q: &str, o: &search_filters::OptView) {
    match &o.target {
        Some(t) => {
            sf_link(out, "fx-sf", q, &o.label, t, o.selected, "");
            out.push_str("</a>");
        }
        None => {
            out.push_str(if o.selected { "<span class=\"fx-ch\" aria-current=\"true\">" } else { "<span class=\"fx-ch\" aria-disabled=\"true\">" });
            esc_into(out, &o.label);
            out.push_str("</span>");
        }
    }
}

/// Active search filters beside the Filters button: one click removes one.
pub fn search_chips(q: &str, v: &search_filters::View) -> String {
    if v.active.is_empty() {
        return String::new();
    }
    let mut out = String::from("<div class=\"fx-act\" role=\"group\" aria-label=\"Active search filters\">");
    for a in &v.active {
        match &a.remove {
            Some(t) => {
                sf_link(&mut out, "fx-sx", q, &a.label, t, false, " fx-on");
                out.push_str("<i aria-hidden=\"true\">×</i></a>");
            }
            None => {
                out.push_str("<span class=\"fx-ch fx-on\">");
                esc_into(&mut out, &a.label);
                out.push_str("</span>");
            }
        }
    }
    if v.active.len() > 1 || v.active.iter().any(|a| a.remove.is_none()) {
        sf_link(&mut out, "fx-sx", q, "Clear filters", &None, false, " fx-clr");
        out.push_str("</a>");
    }
    out.push_str("</div>");
    out
}

fn min_length(out: &mut String, p: &Prefs) {
    out.push_str("<div class=\"fx-row\" id=\"");
    out.push_str(DUR_ROW);
    out.push_str("\" role=\"group\" aria-label=\"Minimum length (known durations only; Home and Search)\">");
    for m in PRESETS {
        seg(out, "fx-dur", &m.to_string(), &if m == 0 { "Any".to_string() } else { format!("{m}+") }, p.min_minutes == m);
    }
    out.push_str("</div><label class=\"fx-sl\"><input type=\"range\" min=\"0\" max=\"180\" step=\"5\" value=\"");
    out.push_str(&p.min_minutes.to_string());
    out.push_str("\" style=\"--v:");
    out.push_str(&range_fill(p.min_minutes));
    out.push_str("\" id=\"ytc-fx-range\" data-input=\"fx-dur\" aria-label=\"Minimum length in minutes\"><span id=\"ytc-fx-min\">");
    if !PRESETS.contains(&p.min_minutes) {
        out.push_str(&format!("{}+ min", p.min_minutes));
    }
    out.push_str("</span></label>");
}

/// The filter panel (hover/focus opens it; every choice applies at once).
/// On Search, `server` = the query and YouTube's own filter groups: each
/// option re-queries with YouTube's `params`; YouTube's Duration options and
/// the shared minimum length form one section.
pub fn panel(p: &Prefs, surf: Surface, server: Option<(&str, &search_filters::View)>) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str("<div class=\"fx-f\" id=\"ytc-fx-f\"><button type=\"button\" class=\"fx-fb\" aria-haspopup=\"true\" data-act=\"fx-open\" data-arg=\"ytc-fx-f\">Filters");
    out.push_str("<small id=\"ytc-fx-sum\">");
    esc_into(&mut out, &summary(p, surf));
    out.push_str("</small>");
    out.push_str(if surf == Surface::Search { "</button><div class=\"fx-p s\" role=\"group\" aria-label=\"Filters\">" } else { "</button><div class=\"fx-p\" role=\"group\" aria-label=\"Filters\">" });
    let groups = server.map(|(q, v)| (q, v.groups.as_slice())).filter(|(_, g)| !g.is_empty());
    match groups {
        Some((q, groups)) => {
            let mut merged = false;
            for g in groups {
                out.push_str("<div class=\"fx-row\" role=\"group\" aria-label=\"");
                esc_into(&mut out, &g.title);
                out.push_str("\">");
                for o in &g.opts {
                    sf_option(&mut out, q, o);
                }
                out.push_str("</div>");
                if g.duration {
                    merged = true;
                    min_length(&mut out, p);
                }
            }
            if !merged {
                min_length(&mut out, p);
            }
        }
        None => min_length(&mut out, p),
    }
    if surf == Surface::Home {
        out.push_str("<div class=\"fx-row\" id=\"");
        out.push_str(ORDER_ROW);
        out.push_str("\" role=\"group\" aria-label=\"Order of loaded Home videos\">");
        seg(&mut out, "fx-order", "recommended", "Relevance", !p.popular);
        seg(&mut out, "fx-order", "popular", "Popularity", p.popular);
        out.push_str("</div>");
    }
    out.push_str("<div class=\"fx-row\">");
    switch(&mut out, "fx-shorts", "Shorts", p.show_shorts, "Show Shorts on Home, in search results and next to videos");
    switch(&mut out, "fx-music", "Music", p.show_music, "Show videos YouTube labels Music");
    switch(&mut out, "fx-members", "Members-only videos", p.show_members, "Show videos for a channel's members, labelled");
    if surf == Surface::Home {
        switch(&mut out, "fx-live", "Live", p.show_live, "Show live streams on Home and next to videos");
        switch(&mut out, "fx-pl", "Playlists only", p.playlists_only, "Show only playlists from the loaded Home feed");
    }
    out.push_str("</div></div></div>");
    out
}

/// YouTube's own Home chips for the bar: in the user's order, hidden ones left out.
pub struct Native<'a> {
    pub chips: &'a [home_chips::Shown],
    pub sel: Option<&'a str>,
}

fn icon_btn(out: &mut String, act: &str, key: &str, label: &str, aria: &str, pressed: Option<bool>) {
    out.push_str("<button type=\"button\" id=\"");
    out.push_str(&control_id(act, key));
    out.push_str("\" data-act=\"");
    out.push_str(act);
    out.push_str("\" data-arg=\"");
    esc_into(out, key);
    out.push_str("\" aria-label=\"");
    esc_into(out, aria);
    out.push('"');
    if let Some(p) = pressed {
        out.push_str(if p { " aria-pressed=\"true\"" } else { " aria-pressed=\"false\"" });
    }
    out.push('>');
    out.push_str(label);
    out.push_str("</button>");
}

/// Topic chips (Home only): All, YouTube's own chips, saved topics, and the + editor.
pub fn chips(topics: &[String], sel: Option<usize>, native: &Native, rules: &[String], msg: &str) -> String {
    let mut out = String::with_capacity(1024);
    // One horizontally scrolling row; the + editor sits outside it (its panel must not be clipped).
    out.push_str("<div class=\"fx-tcw\"><div class=\"fx-tc\" id=\"ytc-fx-tc\" role=\"group\" aria-label=\"Home feeds\">");
    seg(&mut out, "fx-topic", "-1", "All", sel.is_none() && native.sel.is_none());
    for c in native.chips.iter().filter(|c| !c.hidden) {
        seg(&mut out, "fx-nchip", &c.key, &c.label, native.sel == Some(c.key.as_str()));
    }
    for (i, t) in topics.iter().enumerate() {
        // Each custom topic carries its own remove control (shown on hover/focus).
        out.push_str("<span class=\"fx-tw\">");
        seg(&mut out, "fx-topic", &i.to_string(), t, sel == Some(i));
        out.push_str("<button type=\"button\" class=\"fx-tx\" id=\"");
        out.push_str(&control_id("fx-tx", &i.to_string()));
        out.push_str("\" data-act=\"fx-tdel\" data-arg=\"");
        out.push_str(&i.to_string());
        out.push_str("\" aria-label=\"Remove topic ");
        esc_into(&mut out, t);
        out.push_str("\" title=\"Remove topic\">×</button></span>");
    }
    out.push_str("</div><div class=\"fx-ed\" id=\"ytc-fx-ed\" data-k=\"+ed\" data-hov=\"fxed\"><button type=\"button\" id=\"ytc-fx-ed-btn\" class=\"fx-ch fx-plus\" aria-haspopup=\"true\" aria-label=\"Edit topics and blocked websites\" data-act=\"fx-open\" data-arg=\"ytc-fx-ed\">+ Topic</button><div class=\"fx-p\" role=\"group\" aria-label=\"Topics and blocked websites\">");
    out.push_str("<form id=\"fx-topic-form\" class=\"fx-add\"><input id=\"fx-topic-in\" name=\"t\" maxlength=\"160\" placeholder=\"New topic, e.g. Roblox scripting\" autocomplete=\"off\" aria-label=\"New topic\"><button type=\"submit\">Add</button></form>");
    if !topics.is_empty() {
        out.push_str("<ul class=\"fx-list\">");
        for (i, t) in topics.iter().enumerate() {
            out.push_str("<li><span>");
            esc_into(&mut out, t);
            out.push_str("</span><button type=\"button\" data-act=\"fx-tdel\" data-arg=\"");
            out.push_str(&i.to_string());
            out.push_str("\" aria-label=\"Remove topic ");
            esc_into(&mut out, t);
            out.push_str("\">×</button></li>");
        }
        out.push_str("</ul>");
    }
    if !native.chips.is_empty() {
        out.push_str("<div class=\"fx-h\">YouTube topics</div><ul class=\"fx-list fx-nl\">");
        for (i, c) in native.chips.iter().enumerate() {
            // `data-k`: the reflow FLIP's identity for the row (its name is in a field, not text).
            out.push_str(if c.hidden { "<li class=\"off\" data-k=\"" } else { "<li data-k=\"" });
            esc_into(&mut out, &c.key);
            out.push_str("\">");
            // The name is an inline field: Enter saves, Escape cancels, empty restores YouTube's label.
            let i = i.to_string();
            out.push_str("<form id=\"");
            out.push_str(&ren_form(&i));
            out.push_str("\" class=\"fx-ren\"><input id=\"");
            out.push_str(&ren_input(&i));
            out.push_str("\" name=\"n\" maxlength=\"40\" autocomplete=\"off\" spellcheck=\"false\" data-focus=\"fx-ren\" data-arg=\"");
            out.push_str(&i);
            out.push_str("\" value=\"");
            esc_into(&mut out, &c.label);
            out.push_str("\" placeholder=\"");
            esc_into(&mut out, &c.original);
            out.push_str("\" aria-label=\"Rename ");
            esc_into(&mut out, &c.original);
            out.push_str("\" title=\"Rename: Enter saves, Esc cancels, empty restores ");
            esc_into(&mut out, &c.original);
            out.push_str("\"></form>");
            icon_btn(&mut out, "fx-nup", &c.key, "↑", &format!("Move {} earlier", c.label), None);
            icon_btn(&mut out, "fx-ndown", &c.key, "↓", &format!("Move {} later", c.label), None);
            icon_btn(&mut out, "fx-nhide", &c.key, if c.hidden { "Show" } else { "Hide" }, &format!("Hide {}", c.label), Some(c.hidden));
            out.push_str("</li>");
        }
        out.push_str("</ul>");
    }
    out.push_str("<div class=\"fx-h\">Blocked websites</div><form id=\"fx-site-form\" class=\"fx-add\"><input id=\"fx-site-in\" name=\"d\" maxlength=\"253\" placeholder=\"example.com\" autocomplete=\"off\" aria-label=\"Block a website\"><button type=\"submit\">Block</button></form>");
    if !rules.is_empty() {
        out.push_str("<ul class=\"fx-list\">");
        for d in rules {
            out.push_str("<li><span>");
            esc_into(&mut out, d);
            out.push_str("</span><button type=\"button\" data-act=\"fx-unblock\" data-arg=\"");
            esc_into(&mut out, d);
            out.push_str("\" aria-label=\"Unblock ");
            esc_into(&mut out, d);
            out.push_str("\">×</button></li>");
        }
        out.push_str("</ul>");
    }
    if !msg.is_empty() {
        out.push_str("<p class=\"fx-msg\" role=\"status\">");
        esc_into(&mut out, msg);
        out.push_str("</p>");
    }
    // Footer: the one-time import from the old extension (opens its dialog).
    out.push_str("<div class=\"fx-ft\"><button type=\"button\" class=\"lnk2\" id=\"ytc-fx-imp\" data-act=\"fx-imp\" aria-haspopup=\"dialog\">Import data from the old YourTube extension…</button></div>");
    out.push_str("</div></div></div>");
    out
}

/// The rename form / field of the `i`-th YouTube topic in the editor.
pub fn ren_form(i: &str) -> String {
    format!("fx-ren-{i}")
}

pub fn ren_input(i: &str) -> String {
    format!("fx-ren-in-{i}")
}

/// The import dialog (`#ytc-imp`): the console line with Copy, the paste box, Import.
pub const IMPORT: &str = "ytc-imp";
pub const IMPORT_IN: &str = "ytc-imp-in";
pub const IMPORT_CMD: &str = "ytc-imp-cmd";
pub const IMPORT_MSG: &str = "ytc-imp-m";

pub fn import_dialog() -> String {
    let mut out = String::from("<div class=\"imp-p\" role=\"dialog\" aria-modal=\"true\" aria-labelledby=\"ytc-imp-h\"><h2 id=\"ytc-imp-h\">Import from the old YourTube extension</h2>");
    out.push_str("<p class=\"imp-t\">Brings over your searches, resume positions, blocked websites, topics, YouTube topic order and names, Home filters and caption settings. It merges with what is here; nothing leaves your browser.</p><ol class=\"imp-o\">");
    out.push_str("<li>Open your browser's extensions page (e.g. <b>chrome://extensions</b>), turn on Developer mode and click <b>service worker</b> on the old YourTube extension.</li>");
    out.push_str("<li>In its console, run this line (it copies the data):<div class=\"imp-c\"><input id=\"");
    out.push_str(IMPORT_CMD);
    out.push_str("\" readonly value=\"");
    esc_into(&mut out, import::EXPORT_SNIPPET);
    out.push_str("\" aria-label=\"Console line\" spellcheck=\"false\"><button type=\"button\" class=\"pill\" data-act=\"fx-imp-copy\">Copy</button></div></li>");
    out.push_str("<li>Paste it here:</li></ol><textarea id=\"");
    out.push_str(IMPORT_IN);
    out.push_str("\" class=\"imp-in\" rows=\"5\" spellcheck=\"false\" autocomplete=\"off\" placeholder=\"{&quot;yourTubeHistoryV1&quot;: …}\" aria-label=\"Old extension data\"></textarea>");
    out.push_str("<p id=\"");
    out.push_str(IMPORT_MSG);
    out.push_str("\" class=\"imp-m\" role=\"status\"></p><div class=\"imp-b\"><button type=\"button\" class=\"lnk2\" data-act=\"fx-imp-x\">Close</button><button type=\"button\" class=\"pill\" data-act=\"fx-imp-go\">Import</button></div></div>");
    out
}

/// The note under the bar: how many loaded cards the filters hide, with resets.
pub fn note(hidden: usize, visible: usize, p: &Prefs, surf: Surface) -> String {
    // Filters work silently; only an empty result explains itself.
    if hidden == 0 || visible > 0 {
        return String::new();
    }
    let mut out = String::from("No videos match your filters");
    if p.min_minutes > 0 && matches!(surf, Surface::Home | Surface::Search) {
        out.push_str(" · <button type=\"button\" class=\"lnk2\" data-act=\"fx-dur\" data-arg=\"0\">Any length</button>");
    }
    if p.playlists_only && surf == Surface::Home {
        out.push_str(" · <button type=\"button\" class=\"lnk2\" data-act=\"fx-pl\">Show all</button>");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_markup() {
        let p = Prefs { min_minutes: 25, popular: true, ..Default::default() };
        let h = panel(&p, Surface::Home, None);
        assert!(h.contains("25+ min · Popular"));
        assert!(h.contains("data-act=\"fx-live\""));
        assert!(h.contains("aria-pressed=\"true\">Popularity"));
        let s = panel(&p, Surface::Search, None);
        assert!(!s.contains("fx-live") && !s.contains("fx-order") && !s.contains("fx-pl"), "Search: duration + Shorts + Music only");
        // The Shorts switch on Home and Search, first in the switch row, off by default.
        for h in [&h, &s] {
            let sw = h.find("data-act=\"fx-shorts\" aria-checked=\"false\"").expect("Shorts switch");
            assert!(sw < h.find("data-act=\"fx-music\"").unwrap(), "Shorts before Music");
        }
        let on = panel(&Prefs { show_shorts: true, ..Default::default() }, Surface::Home, None);
        assert!(on.contains("id=\"ytc-fx-shorts\" data-act=\"fx-shorts\" aria-checked=\"true\""));
        // YouTube's own groups on Search, Duration merged with the shared length row.
        let groups = search_filters::parse(&crate::util::test_fixture("search_filtered.json"));
        let v = search_filters::view(&groups, Some("EgIIBQ%3D%3D"), &mut Default::default());
        let s = panel(&p, Surface::Search, Some(("a<b", &v)));
        assert_eq!(s.matches("data-act=\"fx-dur\"").count(), 5, "one length row, not two");
        let dur = s.find("Over 20 minutes").unwrap();
        assert!(s[dur..].find("data-act=\"fx-dur\"").unwrap() < s[dur..].find("Upload date").unwrap(), "local length right after YouTube's Duration");
        assert!(s.contains("href=\"/results?search_query=a%3Cb&amp;sp=EgQIBRAB\""), "escaped query, YouTube's params");
        assert!(s.contains("<span class=\"fx-ch\" aria-disabled=\"true\">Channels</span>"));
        assert!(s.contains("aria-current=\"true\" title=\"Remove this filter\">This year</a>") && s.contains("href=\"/results?search_query=a%3Cb\" aria-current"), "selected → removal link");
        let c = search_chips("a<b", &v);
        assert!(c.contains("This year<i") && !c.contains("Clear filters"));
        let none = Native { chips: &[], sel: None };
        let c = chips(&["<b>x</b>".into()], Some(0), &none, &["example.com".into()], "");
        assert!(!c.contains("<b>x"));
        assert!(!c.contains("YouTube topics"));
        let shown = vec![
            home_chips::Shown { key: "gaming".into(), label: "Games".into(), original: "Gaming".into(), hidden: false },
            home_chips::Shown { key: "<i>".into(), label: "<i>x".into(), original: "<i>x".into(), hidden: true },
        ];
        let n = chips(&[], None, &Native { chips: &shown, sel: Some("gaming") }, &[], "");
        assert!(n.contains("data-act=\"fx-nchip\" data-arg=\"gaming\" aria-pressed=\"true\">Games"), "the bar shows the user's name");
        assert!(n.contains("data-arg=\"-1\" aria-pressed=\"false\">All"), "All not pressed while a YouTube chip is");
        assert_eq!(n.matches("data-act=\"fx-nchip\"").count(), 1, "hidden chip not in the bar");
        assert!(n.contains("<li class=\"off\" data-k=\"&lt;i&gt;\"><form id=\"fx-ren-1\"") && n.contains("value=\"&lt;i&gt;x\"") && !n.contains("<i>x"), "editor lists hidden ones, escaped");
        assert!(n.contains("id=\"fx-ren-in-0\"") && n.contains("value=\"Games\" placeholder=\"Gaming\" aria-label=\"Rename Gaming\""), "inline rename field");
        assert!(n.contains("data-act=\"fx-imp\""), "import link in the editor's footer");
        let d = import_dialog();
        assert!(d.contains("value=\"chrome.storage.local.get(null, d =&gt; copy(JSON.stringify(d)))\"") && d.contains("data-act=\"fx-imp-go\""));
        assert!(c.contains("data-arg=\"0\" aria-pressed=\"true\""));
        assert!(c.contains("Unblock example.com"));
        assert!(note(0, 5, &p, Surface::Home).is_empty());
        assert!(note(3, 0, &p, Surface::Home).contains("Any length"));
    }

    #[test]
    fn card_hooks() {
        let v = Video { id: "abcdefghijk".into(), progress: 40, ..Default::default() };
        let mut out = String::new();
        card_bar(&mut out, &v);
        assert_eq!(out, "<i class=\"pg\" style=\"width:40%\"></i>");
        shared(|s| {
            s.progress.record("abcdefghijk", 300.0, 600.0, false, 1.0);
            s.signed_in = true;
        });
        out.clear();
        card_bar(&mut out, &v);
        assert_eq!(out, "<i class=\"pg own\" style=\"width:50%\"></i>", "local record wins");
        out.clear();
        card_actions(&mut out, &v);
        assert!(out.contains("ytc-wl-abcdefghijk"));
        shared(|s| s.signed_in = false);
        out.clear();
        card_actions(&mut out, &v);
        assert!(out.is_empty(), "no Watch later button signed out");
    }
}
