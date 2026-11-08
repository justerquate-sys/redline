//! Glue between the app and `features/*`: storage, requests, clicks, keys
//! and re-rendering for the Watch later button, filters, blocked websites,
//! topic feeds, search history and resume positions. A child module of
//! `app` so the hooks in `app.rs` stay one line each.

use super::{App, FeedState, SEARCH_PICK_MS};
use crate::{bridge, motion};
use crate::features::filters::{self, Prefs, Surface};
use crate::features::watch_later::{self as wl, Edit, Outcome, State};
use crate::features::{self, history, home_chips as hc, import, members, progress, search_filters as sf, topics, websites};
use crate::parse::{self, Item};
use crate::render::{self, ImgPlan, Lay, LIST, MAIN, MORE, SUGGEST};
use crate::route::Route;
use crate::util::next_id;
use crate::json::{self, Value};
use std::collections::{HashMap, HashSet};

/// sessionStorage mirror of the filter preferences (tab lifetime).
const PREFS_SESSION: &str = "ytc:fx";
const SAVE_PREFS_MS: u32 = 400;
const SAVE_PROGRESS_MS: u32 = 3000;
/// Chunks in a row that the filters hid completely before automatic loading pauses.
const BLANK_LIMIT: u32 = 4;
/// A feed whose cards the Members-only switch hid (Subscriptions, which has no Filters note).
const FILTER_HID: &str = "Your Filters hide the members-only videos here.";
const ANON_OPTS: &str = "{\"credentials\":\"omit\",\"priority\":\"low\"}";
const ANON_HEADERS: &str = "{\"Content-Type\":\"application/json\"}";

// Feed and chip motion (the motion law, `.claude/skills/lumen-motion`): every write below is
// final first; these only animate paint, from pointer actions only.
/// Cards that join an entrance wave, the step between them (≤ 7 steps), its length and rise.
const ENTER_MAX: u32 = 15;
const ENTER_STEP: u32 = 24;
const ENTER_MS: u32 = 320;
const ENTER_DY: f64 = 12.0;
/// Text that replaces a skeleton's bars rises less (it stays inside its card's paint box).
const ENTER_TEXT_DY: f64 = 8.0;
/// The chip row's items for reflow FLIPs (pill copies and ghosts excluded), the editor's YouTube-topic rows.
const CHIP_ITEMS: &str = ".fx-tc>:not([data-ghost]),.fx-ed";
const NL_ITEMS: &str = ".fx-nl li";
/// A removed chip's copy shrinks out this fast (exits are quicker than moves).
const FLIP_OUT_MS: u32 = 160;
/// The Filters summary's new text slides in.
const NUDGE_MS: u32 = 200;
/// The class of the selection pill's copy (client.css `.fx-pg`).
const PILL_CLS: &str = "fx-pg";

enum Req {
    Wl { v: String, edit: Option<Edit> },
    Check { v: String },
    Topic { job: topics::Job, sel: usize },
    /// A YouTube Home chip's feed (`seq` = `Fx::ngen` when sent).
    Chip { seq: u32 },
}

enum Timer {
    SavePrefs,
    SaveProgress,
    /// Show held cards whose check takes too long.
    Hold,
}

#[derive(Default)]
struct Sug {
    typed: String,
    hist: Vec<String>,
    sugg: Vec<String>,
    sel: Option<usize>,
    focused: bool,
    /// The dropdown's rows settle in (a pointer opened it); typing and keys change it instantly.
    anim: bool,
    /// `#ytc-search` carries `now` (its focus morph is off: the last focus change came from a key).
    form_now: bool,
}

pub struct Fx {
    pub prefs: Prefs,
    touched: HashSet<&'static str>,
    order: filters::Order,
    hist: history::History,
    hist_loaded: bool,
    prog_loaded: bool,
    prog_dirty: bool,
    rules_loaded: bool,
    topics: Vec<String>,
    topics_loaded: bool,
    sel: Option<usize>,
    session: Option<topics::Session>,
    /// The native Home feed while a topic is shown ("All" restores it).
    stash: Option<FeedState>,
    parked: bool,
    topic_want: bool,
    editor_msg: String,
    /// The import dialog is open; the YouTube topic whose name field has focus (editor row).
    pub(super) imp_open: bool,
    ren: Option<usize>,
    /// The + Topic editor is pinned open after a pointer action in it (a re-render can move it
    /// out from under a still pointer); real movement outside it unpins it (`ed_in`: the
    /// pointer's hover zone is the editor).
    pub(super) ed_pin: bool,
    pub(super) ed_in: bool,
    checker: websites::Checker,
    hold: websites::Hold,
    hold_timer: bool,
    reqs: HashMap<u32, Req>,
    timers: HashMap<u32, Timer>,
    save_timer: bool,
    prog_timer: bool,
    sug: Sug,
    blank_streak: u32,
    /// Videos whose position was recorded in this tab (their bars may be stale in restored pages).
    recent: HashSet<String>,
    check_bars: bool,
    /// What the note shows now (written only when it changes).
    note_html: String,
    /// Last Watch later failure (HTTP status and YouTube's text), for the debug overlay.
    pub last_wl_error: String,
    /// YouTube's search-filter groups per search URL (memory, last few pages).
    sgroups: Vec<(String, std::rc::Rc<Vec<sf::Group>>)>,
    slearn: sf::Learned,
    /// Control to focus again after a keyboard-activated filter re-query.
    refocus: Option<String>,
    /// YouTube's own Home chips (latest response; memory only) and the user's order/hide.
    nbar: Vec<hc::Chip>,
    nall: Option<String>,
    nprefs: hc::Prefs,
    /// The selected YouTube chip (key) and the native All feed it replaced.
    nsel: Option<String>,
    nstash: Option<FeedState>,
    nparked: bool,
    ngen: u32,
    /// The click being handled came from the keyboard (set by `app.rs` around `fx_click`): no motion.
    pub(super) kbd: bool,
    /// The next card list into an empty grid enters with a wave (a pointer-picked chip or topic).
    enter: bool,
    /// This navigation started from a submitted search (keyboard-like: no entrance wave); `calm_next`
    /// is set by the submit, taken by `fx_leave`.
    calm: bool,
    calm_next: bool,
    /// Watch-later buttons saved by a pointer click whose bookmark pops (until the save settles).
    wl_pop: HashSet<String>,
    /// The Filters button's summary as painted.
    sum: String,
}

const SEARCH_GROUPS: usize = 8;

impl Fx {
    pub fn new(signed_in: bool) -> Fx {
        features::shared(|s| s.signed_in = signed_in);
        let prefs = Prefs::from_mirror(&bridge::session_get(PREFS_SESSION)).unwrap_or_default();
        members::load(&bridge::session_get(members::SESSION_KEY));
        Fx {
            prefs,
            touched: HashSet::new(),
            order: Default::default(),
            hist: Default::default(),
            hist_loaded: false,
            prog_loaded: false,
            prog_dirty: false,
            rules_loaded: false,
            topics: Vec::new(),
            topics_loaded: false,
            sel: None,
            session: None,
            stash: None,
            parked: false,
            topic_want: false,
            editor_msg: String::new(),
            imp_open: false,
            ren: None,
            ed_pin: false,
            ed_in: false,
            checker: Default::default(),
            hold: Default::default(),
            hold_timer: false,
            reqs: HashMap::new(),
            timers: HashMap::new(),
            save_timer: false,
            prog_timer: false,
            sug: Sug::default(),
            blank_streak: 0,
            last_wl_error: String::new(),
            recent: HashSet::new(),
            check_bars: false,
            note_html: String::new(),
            sgroups: Vec::new(),
            slearn: Default::default(),
            refocus: None,
            nbar: Vec::new(),
            nall: None,
            nprefs: Default::default(),
            nsel: None,
            nstash: None,
            nparked: false,
            ngen: 0,
            kbd: false,
            enter: false,
            calm: false,
            calm_next: false,
            wl_pop: HashSet::new(),
            sum: String::new(),
        }
    }

    pub fn boot() {
        for k in filters::KEYS {
            bridge::storage_get(k);
        }
        for k in [history::KEY, progress::KEY, websites::KEY, topics::KEY, hc::KEY] {
            bridge::storage_get(k);
        }
    }

    pub fn topic_on(&self) -> bool {
        self.sel.is_some() && self.session.is_some() && !self.parked
    }

    /// A YouTube Home chip's feed is shown (not the native All feed).
    pub fn chip_on(&self) -> bool {
        self.nsel.is_some() && !self.nparked
    }

    pub fn topic_more(&self) -> bool {
        self.topic_on() && self.session.as_ref().is_some_and(|s| s.more())
    }
}

fn rules_empty() -> bool {
    features::shared(|s| s.rules.domains.is_empty())
}

impl App {
    // ------------------------------------------------------------- basics

    pub(super) fn fx_surface(&self) -> Surface {
        match self.route {
            Route::Home => Surface::Home,
            Route::Search { .. } => Surface::Search,
            Route::Watch { .. } => Surface::Related,
            Route::Subs => Surface::Subs,
            _ => Surface::Other,
        }
    }

    /// The items of `items` that stay visible with the current filters.
    pub(super) fn fx_visible(&self, items: &[Item]) -> Vec<Item> {
        self.fx_visible_as(items, &self.fx.prefs)
    }

    fn fx_visible_as(&self, items: &[Item], prefs: &Prefs) -> Vec<Item> {
        let surf = self.fx_surface();
        let any_blocked = !rules_empty();
        let wl = matches!(&self.route, Route::Playlist { list } if list == "WL");
        if !wl && !filters::active(surf, prefs, any_blocked) {
            return items.to_vec();
        }
        let checker = &self.fx.checker;
        // A card that turned out to be members-only only when it was clicked goes by the switch too.
        let hide_learned = !prefs.show_members && surf != Surface::Other;
        features::shared(|s| {
            let rules = &s.rules;
            let blocked = |id: &str| checker.verdict(id, rules) == Some(websites::Verdict::Blocked);
            items.iter().filter(|it| (!wl || !s.saves.hidden(it.key())) && filters::keep(it, surf, prefs, &blocked) && !(hide_learned && s.members.has(it.key()))).cloned().collect()
        })
    }

    fn fx_counts(&self) -> (usize, usize) {
        let vis = self.fx_visible(&self.feed.shown).len();
        let all = self.counted(&self.feed.shown) + self.counted(&self.feed.buf);
        let vis_all = vis + self.fx_visible(&self.feed.buf).len();
        (all - vis_all, vis)
    }

    /// A new feed was set (first response, snapshot, topic): native order and arrangement.
    pub(super) fn fx_new_feed(&mut self) {
        let mut all = self.feed.shown.clone();
        all.extend(self.feed.buf.iter().cloned());
        self.fx.order.reset(&all);
        self.fx.blank_streak = 0;
        if self.fx.prefs.popular && self.route == Route::Home {
            filters::arrange(&mut self.feed.shown, &mut self.feed.buf, &self.fx.order, true);
        }
        self.fx_known_saved(&all);
    }

    /// Continuation items were added to the buffer.
    pub(super) fn fx_more_items(&mut self, new: &[Item]) {
        self.fx.order.add(new);
        if self.fx.prefs.popular && self.route == Route::Home {
            filters::sort_popular(&mut self.feed.buf, &self.fx.order);
        }
        self.fx_known_saved(new);
    }

    /// Cards on the Watch later playlist are saved by definition.
    fn fx_known_saved(&mut self, items: &[Item]) {
        if matches!(&self.route, Route::Playlist { list } if list == "WL") {
            let ids: Vec<String> = items.iter().filter_map(|i| if let Item::Video(v) = i { Some(v.id.clone()) } else { None }).collect();
            features::shared(|s| s.saves.known_saved(ids));
        }
    }

    /// A chunk was rendered: `shown` of `total` cards were visible.
    pub(super) fn fx_chunk(&mut self, total: usize, shown: usize) {
        if total > 0 && shown == 0 {
            self.fx.blank_streak += 1;
        } else if shown > 0 {
            self.fx.blank_streak = 0;
        }
        self.fx_queue_checks();
        self.fx_note();
    }

    /// Re-renders the card list from memory (filters/order/verdicts changed).
    pub(super) fn fx_refilter(&mut self, reorder: bool) {
        if reorder && self.route == Route::Home {
            filters::arrange(&mut self.feed.shown, &mut self.feed.buf, &self.fx.order, self.fx.prefs.popular);
        }
        if !self.rendered || (self.feed.head.is_none() && !matches!(self.route, Route::Watch { .. })) {
            return;
        }
        let vis = self.fx_visible(&self.feed.shown);
        let mut s = String::with_capacity(vis.len() * 1000);
        render::cards(&mut s, &vis, ImgPlan::first(self.lay(), self.cols));
        bridge::html(LIST, &s);
        if !vis.is_empty() {
            bridge::remove_all(".feed>.empty,#ytc-more>.empty");
        }
        self.fade_thumbs();
        // Cards still waiting for their check stay hidden in the new markup.
        let held = self.fx.hold.ids();
        if !held.is_empty() {
            bridge::class_all(&websites::card_selector(LIST, &held), "fxh", true);
        }
        self.fx.blank_streak = 0;
        if self.has_more() {
            bridge::attr(MORE, "class", "more");
            bridge::html(MORE, "");
            bridge::observe(MORE);
        }
        self.fx_note();
    }

    // -------------------------------------------------------------- paint

    /// Every painted page (fresh, snapshot, memory): the bar, checks, topic state.
    pub(super) fn fx_painted(&mut self) {
        if matches!(&self.route, Route::Playlist { list } if list == "WL") && features::shared(|s| self.feed.shown.iter().any(|i| s.saves.hidden(i.key()))) {
            self.fx_wl_redraw();
        }
        if self.fx.parked && self.route == Route::Home {
            self.fx_topic_reset();
        }
        if self.fx.nparked && self.route == Route::Home {
            self.fx_chip_reset();
        }
        if (std::mem::take(&mut self.fx.check_bars) && self.feed.shown.iter().any(|i| self.fx.recent.contains(i.key()))) || self.fx_members_stale() {
            self.fx_refilter(false);
        }
        self.fx_bar();
        self.fx_queue_checks();
    }

    /// A page restored from memory was rendered before the player's verdict on one of its cards: it
    /// needs the label (or, with the switch off, to lose the card), or to lose a label it no longer has.
    fn fx_members_stale(&self) -> bool {
        let hide = !self.fx.prefs.show_members && self.fx_surface() != Surface::Other;
        self.feed.shown.iter().any(|i| {
            let Item::Video(v) = i else { return false };
            if v.members {
                return false;
            }
            let card = format!("#{LIST} .c[data-v=\"{}\"]", v.id);
            if members::has(&v.id) {
                bridge::any(&card) && (hide || !bridge::any(&format!("{card} .mo")))
            } else {
                members::forgotten(&v.id) && bridge::any(&format!("{card} .mo"))
            }
        })
    }

    /// The player's verdict (or a prefetch's) that `v` is for a channel's members, its card perhaps unlabelled:
    /// labelled from now on, by the switch, never next. A card on the page changes in place: no list
    /// re-render under a pointer, since a prefetch of a card in view may be what found out.
    pub(super) fn members_learn(&mut self, v: &str) {
        let new = !members::has(v);
        members::learn(v);
        bridge::session_set(members::SESSION_KEY, &members::mirror());
        let Some(Item::Video(c)) = self.feed.shown.iter().find(|i| i.key() == v).cloned() else { return };
        if !new || c.members {
            return;
        }
        let card = format!("#{LIST} .c[data-v=\"{v}\"]");
        if !self.fx.prefs.show_members && self.fx_surface() != Surface::Other {
            bridge::remove_all(&card);
            self.fx_note();
        } else if !bridge::any(&format!("{card} .mo")) {
            let mut html = String::new();
            features::card_badge(&mut html, &c);
            bridge::append_all(&format!("{card} .th"), &html);
        }
    }

    /// It played after all (a member's route, early access gone public).
    pub(super) fn members_played(&mut self, v: &str) {
        if members::forget(v) {
            bridge::session_set(members::SESSION_KEY, &members::mirror());
            if !self.feed.shown.iter().any(|i| matches!(i, Item::Video(c) if c.id == v && c.members)) {
                bridge::remove_all(&format!("#{LIST} .c[data-v=\"{v}\"] .mo"));
            }
        }
    }

    /// The Members-only switch is really what emptied the page: with it on, a card would show.
    fn fx_members_hid(&self) -> bool {
        if self.fx.prefs.show_members {
            return false;
        }
        let on = Prefs { show_members: true, ..self.fx.prefs.clone() };
        !self.fx_visible_as(&self.feed.shown, &on).is_empty() || !self.fx_visible_as(&self.feed.buf, &on).is_empty()
    }

    /// The line of a feed with no visible card: `empty` only when nothing arrived. Cards the filters hid and
    /// more on the way say nothing (more may fill the page); Home and Search have their note. On Subscriptions
    /// the switch is the only filter.
    pub(super) fn feed_empty(&self, empty: &str) -> String {
        if self.feed.shown.is_empty() && self.feed.buf.is_empty() {
            return empty.to_string();
        }
        match self.fx_surface() {
            Surface::Home | Surface::Search => String::new(),
            Surface::Subs if !self.has_more() => FILTER_HID.into(),
            Surface::Subs => String::new(),
            _ => empty.to_string(),
        }
    }

    /// Subscriptions (no Filters note there): the last page came and the filters hid every card.
    pub(super) fn fx_all_hidden(&mut self) {
        if self.fx_surface() == Surface::Subs && !self.has_more() && !self.feed.shown.is_empty() && self.fx_visible(&self.feed.shown).is_empty() {
            bridge::html(MORE, &format!("<p class=\"empty\">{FILTER_HID}</p>"));
        }
    }

    pub(super) fn fx_bar(&mut self) {
        let surf = self.fx_surface();
        if !matches!(surf, Surface::Home | Surface::Search) || self.feed.head.is_none() {
            return;
        }
        self.fx.ed_pin = false;
        let mut html = String::from("<div class=\"fx-top\">");
        if surf == Surface::Home {
            let rules = features::shared(|s| s.rules.domains.clone());
            let shown = hc::arrange(&self.fx.nbar, &self.fx.nprefs);
            let native = features::Native { chips: &shown, sel: self.fx.nsel.as_deref().filter(|_| !self.fx.topic_on()) };
            html.push_str(&features::chips(&self.fx.topics, if self.fx.topic_on() { self.fx.sel } else { None }, &native, &rules, &self.fx.editor_msg));
        }
        let search = match &self.route {
            Route::Search { q, sp } => {
                let groups = self.fx.sgroups.iter().find(|(u, _)| *u == self.url).map(|e| e.1.clone());
                groups.map(|g| (q.clone(), sf::view(&g, sp.as_deref(), &mut self.fx.slearn)))
            }
            _ => None,
        };
        if let Some((q, v)) = &search {
            html.push_str(&features::search_chips(q, v));
        }
        // Filters live in the header, left of the bell (no extra row above the videos).
        bridge::html(crate::render::HFX, &features::panel(&self.fx.prefs, surf, search.as_ref().map(|(q, v)| (q.as_str(), v))));
        self.fx.sum = features::summary(&self.fx.prefs, surf);
        html.push_str("</div><div id=\"");
        html.push_str(features::NOTE);
        html.push_str("\" class=\"fx-note\" role=\"status\"></div>");
        bridge::html(features::BAR, &html);
        // Wheel scrolling, and the re-rendered chip row keeps its sideways position.
        bridge::wheel_x(features::CHIPS);
        // A pointer slide of the selection pill still running goes on over the new row (a chip's feed
        // answered mid-slide); a keyboard click stopped it already.
        if !self.fx.kbd {
            bridge::pill_carry(features::CHIPS);
        }
        self.fx.note_html.clear();
        self.fx_note();
        if let Some(id) = self.fx.refocus.take() {
            // A hidden panel refuses focus: open it for the call; :focus-visible keeps it open.
            bridge::attr("ytc-fx-f", "class", "fx-f open");
            bridge::focus(&id);
            bridge::attr("ytc-fx-f", "class", "fx-f");
        }
    }

    fn fx_note(&mut self) {
        let surf = self.fx_surface();
        if !matches!(surf, Surface::Home | Surface::Search) {
            return;
        }
        let (hidden, visible) = self.fx_counts();
        let mut html = features::note(hidden, visible, &self.fx.prefs, surf);
        if hidden > 0 && visible == 0 && self.fx_members_hid() {
            html = FILTER_HID.to_string();
        }
        if let Some(s) = self.fx.session.as_ref().filter(|_| self.fx.topic_on()) {
            if self.feed.shown.is_empty() && self.feed.buf.is_empty() {
                // Nothing looks like loading; only a final outcome is stated.
                let what = if s.more() {
                    String::new()
                } else if s.failures > 0 {
                    "YouTube didn't return recommendations for this topic. Try again later.".to_string()
                } else {
                    format!("No matching videos found for {}.", s.topic.topic)
                };
                html = crate::util::esc(&what);
            }
        }
        if html != self.fx.note_html {
            bridge::html(features::NOTE, &html);
            self.fx.note_html = html;
        }
    }

    // ------------------------------------------------------------ storage

    pub(super) fn fx_storage(&mut self, key: &str, json: &str) -> bool {
        let v = json::parse_str(json).unwrap_or_default();
        let now = bridge::epoch();
        if let Some(k) = filters::KEYS.iter().find(|k| **k == key) {
            if !self.fx.touched.contains(k) && !v.is_null() {
                let before = self.fx.prefs.clone();
                self.fx.prefs.apply(key, &v);
                if before != self.fx.prefs {
                    bridge::session_set(PREFS_SESSION, &self.fx.prefs.mirror());
                    self.fx_refilter(before.popular != self.fx.prefs.popular);
                    self.fx_bar();
                }
            }
            return true;
        }
        match key {
            history::KEY => {
                let mut stored = history::History::from_json(&v, now);
                // Searches made before storage answered are kept (newest first).
                for (q, at) in self.fx.hist.entries.iter().rev() {
                    if stored.entries.first().is_none_or(|e| e.1 <= *at) {
                        stored.record(q, *at);
                    }
                }
                let changed = !self.fx.hist.entries.is_empty();
                self.fx.hist = stored;
                self.fx.hist_loaded = true;
                if changed {
                    bridge::storage_set(history::KEY, &self.fx.hist.to_json());
                }
            }
            progress::KEY => {
                let stored = progress::Store::from_json(&v, now);
                let mine = features::shared(|s| std::mem::replace(&mut s.progress, stored));
                let any_mine = !mine.map.is_empty();
                features::shared(|s| {
                    for (id, e) in mine.map {
                        if s.progress.map.get(&id).is_none_or(|o| o.updated_at <= e.updated_at) {
                            s.progress.map.insert(id, e);
                        }
                    }
                });
                self.fx.prog_loaded = true;
                if any_mine {
                    self.fx_save_progress();
                }
                let shows_local = features::shared(|s| self.feed.shown.iter().any(|i| s.progress.map.contains_key(i.key())));
                if shows_local {
                    self.fx_refilter(false);
                }
            }
            websites::KEY => {
                if !self.fx.rules_loaded {
                    self.fx.rules_loaded = true;
                    let r = websites::Rules::from_json(&v);
                    if !r.domains.is_empty() {
                        features::shared(|s| s.rules = r);
                        self.fx_rules_changed();
                    }
                    self.fx_rules_known();
                }
            }
            hc::KEY => {
                self.fx.nprefs = hc::Prefs::from_json(&v);
                if self.route == Route::Home {
                    self.fx_bar();
                }
            }
            topics::KEY => {
                self.fx.topics_loaded = true;
                self.fx.topics = topics::topics_from_json(&v);
                if self.route == Route::Home {
                    self.fx_bar();
                }
            }
            _ => return false,
        }
        true
    }

    fn fx_set_pref(&mut self, key: &'static str, reorder: bool) {
        self.fx.touched.insert(key);
        bridge::session_set(PREFS_SESSION, &self.fx.prefs.mirror());
        if !self.fx.save_timer {
            self.fx.save_timer = true;
            let id = next_id();
            self.fx.timers.insert(id, Timer::SavePrefs);
            bridge::timer(id, SAVE_PREFS_MS);
        }
        self.fx_refilter(reorder);
    }

    fn fx_save_prefs(&mut self) {
        for k in self.fx.touched.iter() {
            bridge::storage_set(k, &self.fx.prefs.stored(k));
        }
    }

    fn fx_save_progress(&mut self) {
        if !self.fx.prog_loaded {
            // Never overwrite the stored records with this tab's few: wait for the read.
            self.fx.prog_dirty = true;
            return;
        }
        self.fx.prog_dirty = false;
        let json = features::shared(|s| s.progress.to_json());
        bridge::storage_set(progress::KEY, &json);
    }

    /// Page hidden or left: write pending preference/progress saves now.
    pub(super) fn fx_flush(&mut self) {
        if self.fx.save_timer {
            self.fx.save_timer = false;
            self.fx.timers.retain(|_, t| !matches!(t, Timer::SavePrefs));
            self.fx_save_prefs();
        }
        if self.fx.prog_dirty {
            self.fx_save_progress();
        }
    }

    pub(super) fn fx_timer(&mut self, id: u32) -> bool {
        match self.fx.timers.remove(&id) {
            Some(Timer::SavePrefs) => {
                self.fx.save_timer = false;
                self.fx_save_prefs();
                true
            }
            Some(Timer::SaveProgress) => {
                self.fx.prog_timer = false;
                if self.fx.prog_dirty {
                    self.fx_save_progress();
                }
                true
            }
            Some(Timer::Hold) => {
                self.fx.hold_timer = false;
                let late = self.fx.hold.expire(bridge::now());
                Self::fx_reveal(&late);
                self.fx_hold_timer();
                true
            }
            None => false,
        }
    }

    // ----------------------------------------------------- search filters

    /// A search response: keep its filter groups for this URL's bar.
    pub(super) fn fx_search_response(&mut self, json: &Value) {
        let groups = sf::parse(json);
        let url = self.url.clone();
        self.fx.sgroups.retain(|(u, _)| *u != url);
        if !groups.is_empty() {
            self.fx.sgroups.push((url, std::rc::Rc::new(groups)));
            if self.fx.sgroups.len() > SEARCH_GROUPS {
                self.fx.sgroups.remove(0);
            }
        }
    }

    /// A filter option or chip (`<a href>` to the same query with YouTube's
    /// `params`): re-query at once (a hover already prefetched it).
    pub(super) fn fx_search_filter(&mut self, href: &str, id: &str, kbd: bool) -> bool {
        let Some(route @ Route::Search { .. }) = crate::route::parse(href) else { return false };
        self.fx.refocus = (kbd && id.starts_with("ytc-fx-s")).then(|| id.to_string());
        let url = route.url();
        self.navigate(route, url, super::Mode::Push);
        true
    }

    // ------------------------------------------------------------- clicks

    /// A click on a feature control. Pointer clicks animate the change they make (the change itself
    /// is written first, exactly as for the keyboard): the chip row's selection pill slides to the new
    /// chip, a removed/moved/hidden chip reflows its neighbours, the Filters segments' pill slides.
    pub(super) fn fx_click(&mut self, act: &str, arg: &str) -> bool {
        let kbd = self.fx.kbd || crate::tv::on();
        // Movement (pill slides, reflows) is off for the keyboard and under reduced motion.
        let still = kbd || (matches!(act, "fx-topic" | "fx-nchip" | "fx-tdel" | "fx-nup" | "fx-ndown" | "fx-nhide" | "fx-dur" | "fx-order") && bridge::reduced_motion());
        let chip_before = matches!(act, "fx-topic" | "fx-nchip").then(|| self.fx_chip_sel_id());
        let reflow = !still && matches!(act, "fx-tdel" | "fx-nup" | "fx-ndown" | "fx-nhide");
        if reflow {
            bridge::flip_mark(features::BAR, CHIP_ITEMS);
            bridge::flip_mark(features::BAR, NL_ITEMS);
        }
        if still && chip_before.is_some() {
            // A keyboard selection is instant: stop a pointer slide that may still run.
            bridge::pill_slide(features::CHIPS, "", "", "", "", 0);
        }
        let prefs_before = self.fx.prefs.clone();
        let handled = self.fx_click_inner(act, arg, kbd);
        self.fx.kbd = false;
        if !handled {
            return false;
        }
        let (curve, ms) = motion::ui();
        if let Some(old) = chip_before.filter(|_| !still) {
            let new = self.fx_chip_sel_id();
            if new != old {
                bridge::pill_slide(features::CHIPS, &old, &new, PILL_CLS, curve, ms);
            }
        }
        if reflow {
            bridge::flip_play(features::BAR, CHIP_ITEMS, curve, ms, FLIP_OUT_MS);
            bridge::flip_play(features::BAR, NL_ITEMS, curve, ms, FLIP_OUT_MS);
        }
        if matches!(act, "fx-dur" | "fx-order") {
            let p = self.fx.prefs.clone();
            for (row, a, b, act) in [
                (features::DUR_ROW, prefs_before.min_minutes.to_string(), p.min_minutes.to_string(), "fx-dur"),
                (features::ORDER_ROW, order_arg(prefs_before.popular).to_string(), order_arg(p.popular).to_string(), "fx-order"),
            ] {
                if still {
                    bridge::pill_slide(row, "", "", "", "", 0);
                } else if a != b {
                    bridge::pill_slide(row, &features::control_id(act, &a), &features::control_id(act, &b), PILL_CLS, curve, ms);
                }
            }
        }
        true
    }

    /// The id of the chip the row shows selected (mirrors `features::chips`' pressed state).
    fn fx_chip_sel_id(&self) -> String {
        let topic = if self.fx.topic_on() { self.fx.sel } else { None };
        match (topic, self.fx.nsel.as_deref().filter(|_| !self.fx.topic_on())) {
            (Some(i), _) => features::control_id("fx-topic", &i.to_string()),
            (None, Some(k)) => features::control_id("fx-nchip", k),
            _ => features::control_id("fx-topic", "-1"),
        }
    }

    /// A card list was written into the grid (`skeleton`: it replaced the skeleton; `fresh_list`: the
    /// grid was empty before). A fresh pointer-initiated load (never the first page load, the keyboard,
    /// a submitted search, back/forward, a restore or a snapshot repaint: those never show a skeleton
    /// here or are `calm`) lets the text replace the skeleton bars in a short wave while the
    /// thumbnails' boxes stay; a pointer-picked chip or topic brings its first cards in as a wave.
    /// Opacity/transform only, no layout read, nothing waits: the cards are clickable at once.
    pub(super) fn fx_entrance(&mut self, skeleton: bool, fresh_list: bool) {
        let armed = std::mem::take(&mut self.fx.enter) && fresh_list;
        let fresh = skeleton && self.nav > 1 && !self.mo.calm && !self.fx.calm;
        if crate::tv::on() || !(armed || fresh) || self.lay() != Lay::Grid {
            return;
        }
        let still = bridge::reduced_motion();
        let sel = ":scope>.c:not(.fxh)";
        if armed {
            bridge::stagger(LIST, sel, "", ENTER_MAX, ENTER_STEP, if still { 0.0 } else { ENTER_DY }, motion::E_OUT, ENTER_MS);
        } else {
            bridge::stagger(LIST, sel, ".d", ENTER_MAX, ENTER_STEP, if still { 0.0 } else { ENTER_TEXT_DY }, motion::E_OUT, ENTER_MS);
            bridge::stagger(LIST, sel, ".th>img", ENTER_MAX, ENTER_STEP, 0.0, motion::E_OUT, ENTER_MS);
        }
    }

    fn fx_click_inner(&mut self, act: &str, arg: &str, kbd: bool) -> bool {
        match act {
            "fx-dur" => {
                let m = arg.parse::<u32>().unwrap_or(0).min(filters::MAX_MINUTES);
                self.fx.prefs.min_minutes = m;
                self.fx_set_pref(filters::KEY_DURATION, false);
                self.fx_panel_sync(!kbd);
            }
            "fx-order" => {
                self.fx.prefs.popular = arg == "popular";
                self.fx_set_pref(filters::KEY_ORDER, true);
                self.fx_panel_sync(!kbd);
            }
            "fx-music" => {
                self.fx.prefs.show_music = !self.fx.prefs.show_music;
                self.fx_set_pref(filters::KEY_MUSIC, false);
                self.fx_panel_sync(!kbd);
            }
            "fx-live" => {
                self.fx.prefs.show_live = !self.fx.prefs.show_live;
                self.fx_set_pref(filters::KEY_LIVE, false);
                self.fx_panel_sync(!kbd);
            }
            "fx-shorts" => {
                self.fx.prefs.show_shorts = !self.fx.prefs.show_shorts;
                self.fx_set_pref(filters::KEY_SHORTS, false);
                self.fx_panel_sync(!kbd);
            }
            "fx-members" => {
                self.fx.prefs.show_members = !self.fx.prefs.show_members;
                self.fx_set_pref(filters::KEY_MEMBERS, false);
                self.fx_panel_sync(!kbd);
            }
            "fx-pl" => {
                self.fx.prefs.playlists_only = !self.fx.prefs.playlists_only;
                self.fx_set_pref(filters::KEY_PLAYLISTS, false);
                self.fx_panel_sync(!kbd);
            }
            "fx-open" => {
                if arg.starts_with("ytc-fx-") {
                    bridge::toggle(arg, "open");
                }
            }
            "fx-topic" => {
                match arg.parse::<usize>().ok().filter(|i| *i < self.fx.topics.len()) {
                    Some(i) => self.fx_topic_select(i),
                    None if self.fx.nsel.is_some() && !self.fx.topic_on() => self.fx_chip_all(),
                    None => self.fx_topic_all(),
                }
            }
            "fx-nchip" => self.fx_chip_select(arg),
            "fx-imp" => self.fx_import_open(true, kbd),
            "fx-imp-x" => self.fx_import_open(false, kbd),
            "fx-imp-copy" => {
                let ok = bridge::copy(features::IMPORT_CMD);
                bridge::text(features::IMPORT_MSG, if ok { "Copied. Run it in the old extension's console, then paste below." } else { "Select the line and copy it (⌘C)." });
                bridge::attr(features::IMPORT_MSG, "class", "imp-m");
            }
            "fx-imp-go" => self.fx_import(),
            "fx-nup" | "fx-ndown" | "fx-nhide" => {
                let p = if act == "fx-nhide" { hc::toggled(&self.fx.nbar, &self.fx.nprefs, arg) } else { hc::moved(&self.fx.nbar, &self.fx.nprefs, arg, if act == "fx-nup" { -1 } else { 1 }) };
                self.fx.nprefs = p;
                bridge::storage_set(hc::KEY, &self.fx.nprefs.to_json());
                if act == "fx-nhide" && self.fx.nsel.as_deref() == Some(arg) && hc::is_hidden(&self.fx.nprefs, arg) && self.fx.chip_on() {
                    self.fx_chip_all();
                }
                self.fx_bar_editor(&features::control_id(act, arg));
            }
            "fx-tdel" => {
                if let Some(i) = arg.parse::<usize>().ok().filter(|i| *i < self.fx.topics.len()) {
                    self.fx.topics.remove(i);
                    bridge::storage_set(topics::KEY, &topics::topics_json(&self.fx.topics));
                    match self.fx.sel {
                        Some(s) if s == i => {
                            self.fx_topic_all();
                        }
                        Some(s) if s > i => self.fx.sel = Some(s - 1),
                        _ => {}
                    }
                    self.fx_bar();
                    bridge::focus("ytc-fx-ed-btn");
                }
            }
            "fx-block" | "fx-unblock" => {
                let Some(host) = websites::arg_domain(arg) else { return true };
                let changed = features::shared(|s| {
                    if act == "fx-block" {
                        s.rules.add(&host).is_ok()
                    } else {
                        let n = s.rules.domains.len();
                        s.rules.domains.retain(|d| !websites::covers(d, &host));
                        n != s.rules.domains.len()
                    }
                });
                if changed {
                    // The re-rendered Block control morphs into "Blocked · Undo" (client.css, pointer only).
                    if !kbd {
                        bridge::attr(render::INFO, "data-fxb", "1");
                    }
                    self.fx_rules_saved();
                }
            }
            "fx-wl" => self.fx_wl_click(arg, kbd),
            "fx-qx" => {
                // The clear control: the field empties and keeps focus; recent searches settle back in.
                bridge::value("ytc-q", "");
                self.suggest_q.clear();
                if !self.fx.sug.focused {
                    bridge::focus("ytc-q");
                }
                let s = &mut self.fx.sug;
                s.typed.clear();
                s.sugg.clear();
                s.sel = None;
                s.focused = true;
                s.anim = true;
                self.fx_sug_render("");
            }
            "fx-q" => {
                self.on_submit("", arg);
            }
            "fx-hdel" => {
                self.fx.sug.anim = false;
                if self.fx.hist.remove(arg) && self.fx.hist_loaded {
                    bridge::storage_set(history::KEY, &self.fx.hist.to_json());
                }
                let typed = self.fx.sug.typed.clone();
                self.fx.sug.sel = None;
                self.fx_sug_render(&typed);
            }
            "fx-more" => {
                self.fx.blank_streak = 0;
                bridge::html(MORE, "");
                self.want = true;
                self.pump();
            }
            _ => return false,
        }
        true
    }

    /// Updates the Filters panel's control states in place (no re-render:
    /// hover and keyboard focus stay where they are).
    /// `anim`: a pointer changed it (the summary's new text slides in).
    fn fx_panel_sync(&mut self, anim: bool) {
        let p = self.fx.prefs.clone();
        let surf = self.fx_surface();
        let tf = |b: bool| if b { "true" } else { "false" };
        for m in filters::PRESETS {
            bridge::attr(&features::control_id("fx-dur", &m.to_string()), "aria-pressed", tf(p.min_minutes == m));
        }
        bridge::text("ytc-fx-min", &if filters::PRESETS.contains(&p.min_minutes) { String::new() } else { format!("{}+ min", p.min_minutes) });
        bridge::value(features::RANGE, &p.min_minutes.to_string());
        bridge::css_var(features::RANGE, "--v", &features::range_fill(p.min_minutes));
        bridge::attr(&features::control_id("fx-order", "recommended"), "aria-pressed", tf(!p.popular));
        bridge::attr(&features::control_id("fx-order", "popular"), "aria-pressed", tf(p.popular));
        bridge::attr(&features::control_id("fx-music", ""), "aria-checked", tf(p.show_music));
        bridge::attr(&features::control_id("fx-live", ""), "aria-checked", tf(p.show_live));
        bridge::attr(&features::control_id("fx-shorts", ""), "aria-checked", tf(p.show_shorts));
        bridge::attr(&features::control_id("fx-members", ""), "aria-checked", tf(p.show_members));
        bridge::attr(&features::control_id("fx-pl", ""), "aria-checked", tf(p.playlists_only));
        let sum = features::summary(&p, surf);
        bridge::text("ytc-fx-sum", &sum);
        if anim && !sum.is_empty() && sum != self.fx.sum {
            bridge::nudge("ytc-fx-sum", if bridge::reduced_motion() { 0.0 } else { 5.0 }, motion::E_OUT, NUDGE_MS);
        }
        self.fx.sum = sum;
    }

    /// The `i`-th YouTube topic's name field shows its saved name.
    fn fx_ren_revert(&self, i: usize) {
        let shown = hc::arrange(&self.fx.nbar, &self.fx.nprefs);
        if let Some(c) = shown.get(i) {
            bridge::value(&features::ren_input(&i.to_string()), &c.label);
        }
    }

    // ------------------------------------------------------- import dialog

    /// Opens/closes the import dialog (a persistent element; the class flip animates it, a
    /// keyboard open/close is instant). Focus goes to the paste box, and back to the editor's link.
    pub(super) fn fx_import_open(&mut self, open: bool, kbd: bool) {
        if open && !self.fx.imp_open {
            bridge::html(features::IMPORT, &features::import_dialog());
        }
        self.fx.imp_open = open;
        let now = if kbd { " now" } else { "" };
        bridge::attr(features::IMPORT, "class", &format!("imp{}{now}", if open { " on" } else { "" }));
        if open {
            bridge::focus(features::IMPORT_IN);
        } else if self.route == Route::Home {
            bridge::focus("ytc-fx-imp");
        }
    }

    /// Import: parse + validate + merge (pure, `features::import`), then write every record and
    /// apply it live (no reload): filters re-filter, chips/topics re-render, the player takes the
    /// caption settings.
    fn fx_import(&mut self) {
        let say = |ok: bool, m: &str| {
            bridge::text(features::IMPORT_MSG, m);
            bridge::attr(features::IMPORT_MSG, "class", if ok { "imp-m ok" } else { "imp-m er" });
        };
        if !(self.fx.hist_loaded && self.fx.prog_loaded && self.fx.rules_loaded && self.fx.topics_loaded && self.cfg_ready) {
            say(false, "Your data here is still loading. Try again in a moment.");
            return;
        }
        let text = bridge::read_value(features::IMPORT_IN);
        let (prog, rules) = features::shared(|s| (Some(progress::Store { map: s.progress.map.clone() }), s.rules.clone()));
        let prog = prog.unwrap_or_default();
        let cur = import::Current { hist: &self.fx.hist, progress: &prog, rules: &rules, topics: &self.fx.topics, native: &self.fx.nprefs, filters: &self.fx.prefs };
        let plan = match import::plan(&text, &cur, bridge::epoch()) {
            Ok(p) => p,
            Err(e) => return say(false, &e),
        };
        if let Some(h) = plan.hist {
            self.fx.hist = h;
            bridge::storage_set(history::KEY, &self.fx.hist.to_json());
        }
        if let Some(p) = plan.progress {
            features::shared(|s| s.progress = p);
            self.fx_save_progress();
        }
        if let Some(t) = plan.topics {
            self.fx.topics = t;
            bridge::storage_set(topics::KEY, &topics::topics_json(&self.fx.topics));
        }
        if let Some(n) = plan.native {
            self.fx.nprefs = n;
            bridge::storage_set(hc::KEY, &self.fx.nprefs.to_json());
        }
        let mut reorder = false;
        if let Some((fp, keys)) = plan.filters {
            reorder = fp.popular != self.fx.prefs.popular;
            self.fx.prefs = fp;
            for k in keys {
                self.fx.touched.insert(k);
                bridge::storage_set(k, &self.fx.prefs.stored(k));
            }
            bridge::session_set(PREFS_SESSION, &self.fx.prefs.mirror());
        }
        if plan.cc_style.is_some() || plan.captions.is_some() {
            if let Some(c) = plan.cc_style {
                self.cfg.prefs.cc_style = Some(c);
            }
            if let Some(on) = plan.captions {
                self.cfg.prefs.captions = on;
            }
            self.save_cfg();
            let prefs = self.cfg.prefs.clone();
            if let Some(p) = self.player.as_mut() {
                p.set_prefs(prefs);
            }
        }
        if let Some(r) = plan.rules {
            features::shared(|s| s.rules = r);
            // Saves, re-filters and re-renders the bar.
            self.fx_rules_saved();
            if reorder {
                self.fx_refilter(true);
            }
        } else {
            self.fx_refilter(reorder);
            self.fx_bar();
        }
        say(true, &plan.summary);
        bridge::value(features::IMPORT_IN, "");
    }

    /// Re-renders the bar with the topic/website editor focused (its input).
    fn fx_bar_editor(&mut self, input: &str) {
        self.fx_bar();
        // A hidden popover refuses focus: open it for the call. It stays pinned (hiding a chip
        // shifts it under a still pointer) until the pointer leaves it; :focus-visible keeps
        // it open for the keyboard.
        bridge::attr("ytc-fx-ed", "class", "fx-ed open");
        bridge::focus(input);
        self.fx.ed_pin = true;
    }

    pub(super) fn fx_submit(&mut self, form: &str, value: &str) -> bool {
        // (Typed and submitted: keyboard-like, no motion.)
        self.fx.kbd = true;
        let r = self.fx_submit_inner(form, value);
        self.fx.kbd = false;
        r
    }

    fn fx_submit_inner(&mut self, form: &str, value: &str) -> bool {
        match form {
            "fx-topic-form" => match topics::add_topic(&mut self.fx.topics, value) {
                Ok(i) => {
                    self.fx.editor_msg.clear();
                    bridge::storage_set(topics::KEY, &topics::topics_json(&self.fx.topics));
                    self.fx_topic_select(i);
                }
                Err(e) => {
                    self.fx.editor_msg = e.into();
                    self.fx_bar_editor("fx-topic-in");
                }
            },
            f if f.starts_with("fx-ren-") => {
                let Some(i) = f["fx-ren-".len()..].parse::<usize>().ok() else { return false };
                let shown = hc::arrange(&self.fx.nbar, &self.fx.nprefs);
                let Some(key) = shown.get(i).map(|c| c.key.clone()) else { return true };
                self.fx.nprefs = hc::renamed(&self.fx.nbar, &self.fx.nprefs, &key, value);
                bridge::storage_set(hc::KEY, &self.fx.nprefs.to_json());
                // The chip shows the new name at once; the editor stays open on the same row. (The
                // focus event of that re-focus fires inside this call and cannot reach the app.)
                self.fx_bar_editor(&features::ren_input(&i.to_string()));
                self.fx.ren = Some(i);
            }
            "fx-site-form" => {
                let r = features::shared(|s| Some(s.rules.add(value))).unwrap_or(Err("Try again."));
                match r {
                    Ok(d) => {
                        self.fx.editor_msg = format!("Blocked {d}.");
                        self.fx_rules_saved();
                    }
                    Err(e) => self.fx.editor_msg = e.into(),
                }
                self.fx_bar_editor("fx-site-in");
            }
            _ => return false,
        }
        true
    }

    pub(super) fn fx_input(&mut self, name: &str, value: &str) -> bool {
        if name != "fx-dur" {
            return false;
        }
        let m = value.parse::<f64>().map(|x| (x.round().max(0.0) as u32).min(filters::MAX_MINUTES)).unwrap_or(0);
        self.fx.prefs.min_minutes = m;
        self.fx_panel_sync(false);
        self.fx_set_pref(filters::KEY_DURATION, false);
        true
    }

    // --------------------------------------------------- blocked websites

    fn fx_rules_saved(&mut self) {
        let json = features::shared(|s| s.rules.to_json());
        bridge::storage_set(websites::KEY, &json);
        self.fx_rules_changed();
    }

    /// Rules changed: verdicts are re-evaluated from remembered hosts, the
    /// list and the watch description are re-rendered, new checks queued.
    fn fx_rules_changed(&mut self) {
        if let (Route::Watch { v, .. }, Some(key)) = (self.route.clone(), self.route_wkey()) {
            if let Some(w) = self.watch_cache.iter().find(|(k, _)| *k == key).map(|e| e.1.clone()) {
                bridge::html(render::INFO, &render::watch_info(&w, &v));
            }
        }
        self.fx_refilter(false);
        self.fx_bar();
        self.fx_queue_checks();
    }

    /// A parsed watch response: remember its description's hosts (never the text).
    pub(super) fn fx_learn_watch(&mut self, w: &parse::Watch) {
        if !w.id.is_empty() {
            self.fx.checker.learn(&w.id, websites::description_hosts(&w.desc));
        }
    }

    fn fx_checked_surface(&self) -> bool {
        !rules_empty() && matches!(self.fx_surface(), Surface::Home | Surface::Search | Surface::Related)
    }

    /// Checks for the cards on the page, then one chunk ahead (the next
    /// batch to append), so scrolling rarely has to wait for a verdict.
    fn fx_queue_checks(&mut self) {
        if !self.fx_checked_surface() {
            return;
        }
        let ahead = self.feed.buf.iter().take(super::CHUNK);
        let hide_shorts = filters::short_hidden(self.fx_surface(), &self.fx.prefs);
        let ids: Vec<String> = self.feed.shown.iter().chain(ahead).filter_map(|i| if let Item::Video(v) = i { (!(v.short && hide_shorts)).then(|| v.id.clone()) } else { None }).collect();
        self.fx.checker.want(ids);
        self.fx_check_pump();
    }

    /// The cards of a batch about to be appended that have no verdict yet
    /// (all of them while the rules are still loading after a page load).
    pub(super) fn fx_hold_pick(&self, vis: &[Item]) -> Vec<String> {
        let pending = !self.fx.rules_loaded && matches!(self.fx_surface(), Surface::Home | Surface::Search | Surface::Related);
        if !pending && !self.fx_checked_surface() {
            return Vec::new();
        }
        if pending {
            return vis.iter().filter_map(|i| if let Item::Video(v) = i { Some(v.id.clone()) } else { None }).collect();
        }
        let ids: Vec<String> = vis.iter().filter_map(|i| if let Item::Video(v) = i { Some(v.id.clone()) } else { None }).collect();
        websites::Hold::pick(&ids, &|id| self.fx.checker.known(id))
    }

    /// Hides the just-appended unverified cards (same task as the append, so
    /// they never paint) until their check answers, at most `HOLD_MS`.
    pub(super) fn fx_hold(&mut self, ids: Vec<String>) {
        if !ids.is_empty() {
            bridge::class_all(&websites::card_selector(LIST, &ids), "fxh", true);
            self.fx.hold.add(&ids, bridge::now());
        }
        if !self.fx.rules_loaded || self.fx.hold.len() == 0 {
            // (Rules still loading: `fx_rules_known` continues; nothing to request yet.)
            self.fx_hold_timer();
            return;
        }
        let held = self.fx.hold.ids();
        self.fx.checker.want(held.iter().cloned());
        self.fx.checker.prioritize(&held);
        self.fx_check_pump();
        self.fx_hold_timer();
    }

    /// The rules arrived from storage: with none, cards held meanwhile are
    /// shown; otherwise the ones with a verdict settle and the rest are checked.
    fn fx_rules_known(&mut self) {
        if self.fx.hold.len() == 0 {
            return;
        }
        let ids = self.fx.hold.ids();
        let now = bridge::now();
        if rules_empty() {
            for id in &ids {
                self.fx.hold.settle(id, now, false);
            }
            Self::fx_reveal(&ids);
            return;
        }
        let mut show = Vec::new();
        for id in &ids {
            let verdict = features::shared(|s| self.fx.checker.verdict(id, &s.rules));
            if let Some(v) = verdict {
                let blocked = v == websites::Verdict::Blocked;
                self.fx.hold.settle(id, now, blocked);
                if blocked {
                    bridge::remove_all(&format!("#{LIST} .c[data-v=\"{id}\"]"));
                } else {
                    show.push(id.clone());
                }
            }
        }
        Self::fx_reveal(&show);
        self.fx_hold(Vec::new());
    }

    fn fx_hold_timer(&mut self) {
        if self.fx.hold_timer || self.fx.hold.len() == 0 {
            return;
        }
        self.fx.hold_timer = true;
        let id = next_id();
        self.fx.timers.insert(id, Timer::Hold);
        bridge::timer(id, 250);
    }

    fn fx_reveal(ids: &[String]) {
        if !ids.is_empty() {
            bridge::class_all(&websites::card_selector(LIST, ids), "fxh", false);
        }
    }

    pub(super) fn fx_debug(&self) -> String {
        let h = self.fx.hold.stats;
        if h.held == 0 && self.fx.checker.len() == 0 {
            return String::new();
        }
        format!(
            "websites · {} verdicts · held back {} (shown {}, blocked {}, timed out {}, waiting {}) · wait avg {:.0} ms max {:.0} ms\n",
            self.fx.checker.len(),
            h.held,
            h.shown,
            h.blocked,
            h.timed_out,
            self.fx.hold.len(),
            if h.held > 0 { h.wait_sum_ms / (h.shown + h.blocked + h.timed_out).max(1) as f64 } else { 0.0 },
            h.wait_max_ms
        )
    }

    fn fx_check_pump(&mut self) {
        // Nothing blocked: no description checks at all (also stops a queue that
        // a just-finished check would otherwise keep draining after Undo).
        if features::shared(|s| s.rules.domains.is_empty()) {
            self.fx.checker.clear_queue();
            return;
        }
        while let Some(v) = self.fx.checker.next_job() {
            let id = next_id();
            bridge::fetch_opts(id, "POST", &websites::check_url(), &websites::check_body(&self.cfg.version, &v), ANON_HEADERS, ANON_OPTS);
            self.fx.checker.inflight.insert(id, v.clone());
            self.fx.reqs.insert(id, Req::Check { v });
        }
    }

    // ------------------------------------------------------- watch later

    /// `pop`: a pointer save (the bookmark pops as it fills; the class stays through the confirmation
    /// so the running pop is not cut).
    fn fx_wl_paint(v: &str, st: &State, pop: bool) {
        let (class, label, title) = wl::view(st);
        let id = wl::btn_id(v);
        bridge::attr(&id, "class", &if pop { format!("{class} pop") } else { class.to_string() });
        bridge::attr(&id, "aria-label", &label);
        bridge::attr(&id, "title", &title);
        bridge::attr(&id, "aria-busy", if matches!(st, State::Saving(_)) { "true" } else { "false" });
    }

    fn fx_wl_send(&mut self, v: &str, edit: Option<Edit>) {
        self.auth = crate::tv::auth();
        let (ep, body) = match edit {
            Some(e) => (wl::EDIT, wl::edit_body(&self.cfg, v, e)),
            None => (wl::MEMBERSHIP, wl::membership_body(&self.cfg, v)),
        };
        let (body, headers) = crate::tv::finish(&self.cfg, &self.auth, body, (bridge::epoch() / 1000.0) as u64);
        let id = next_id();
        if edit.is_some() {
            // Saving to Watch later changes what YouTube recommends next.
            crate::ready::taint();
        }
        bridge::fetch_opts(id, "POST", &wl::url(ep), &body, &headers, if edit.is_some() { "{}" } else { "{\"priority\":\"low\"}" });
        self.fx.reqs.insert(id, Req::Wl { v: v.to_string(), edit });
    }

    fn fx_wl_click(&mut self, v: &str, kbd: bool) {
        if !crate::util::is_video_id(v) {
            return;
        }
        let Some(edit) = features::shared(|s| Some(s.saves.click(v))).flatten() else { return };
        let library = matches!(&self.route, Route::Playlist { list } if list == "WL") && edit == Edit::Remove;
        let focus = if library && kbd {
            let items = self.fx_visible(&self.feed.shown);
            items.iter().position(|i| i.key() == v).and_then(|at| items.get(at + 1).or_else(|| at.checked_sub(1).and_then(|n| items.get(n)))).map(|i| wl::btn_id(i.key()))
        } else { None };
        // Only surviving cards move. The removed card itself is gone on this frame.
        let reflow = library && !kbd && !bridge::reduced_motion();
        let selector = format!(".c[data-v]:not([data-v=\"{v}\"])");
        if reflow { bridge::flip_mark(LIST, &selector); }
        let st = State::Saving(edit);
        features::shared(|s| s.saves.state.insert(v.to_string(), st.clone()));
        let pop = edit == Edit::Add && !kbd;
        if pop {
            self.fx.wl_pop.insert(v.to_string());
        } else {
            self.fx.wl_pop.remove(v);
        }
        Self::fx_wl_paint(v, &st, pop);
        if library {
            self.fx_wl_redraw();
            if reflow {
                let (curve, ms) = motion::ui();
                bridge::flip_play(LIST, &selector, curve, ms, 0);
            }
            if kbd { bridge::focus(focus.as_deref().unwrap_or("ytc-nav-wl")); }
        }
        if !self.auth.signed_in() {
            let out = Outcome::Failed { reason: "Sign in required".into(), detail: String::new() };
            self.fx_wl_settle(v, edit, &out);
            return;
        }
        self.fx_wl_send(v, Some(edit));
    }

    pub(super) fn fx_wl_settle(&mut self, v: &str, edit: Edit, out: &Outcome) {
        let st = features::shared(|s| {
            s.saves.settle(v, edit, out);
            s.saves.probed.insert(v.to_string(), true);
            Some(s.saves.get(v))
        });
        let pop = self.fx.wl_pop.remove(v) && st == Some(State::Saved);
        if let Some(st) = st {
            Self::fx_wl_paint(v, &st, pop);
        }
        let library = matches!(&self.route, Route::Playlist { list } if list == "WL");
        if matches!(out, Outcome::Failed { .. }) {
            if edit == Edit::Remove {
                self.fx_wl_invalidate();
                if library { self.fx_wl_redraw(); }
            }
        } else {
            if library && edit == Edit::Remove {
                self.feed.shown.retain(|i| i.key() != v);
                self.feed.buf.retain(|i| i.key() != v);
                self.mark_items();
            }
            self.fx_wl_invalidate();
        }
        if let Outcome::Failed { reason, detail } = out {
            self.fx.last_wl_error = format!("{edit:?} {v}: {detail}");
            let what = if edit == Edit::Add { "save to" } else { "remove from" };
            self.incident("watch-later", &format!("{what} Watch later failed for {v}: {reason} ({detail})"), None);
        }
    }

    /// Membership changes invalidate only Watch later's old page copies. This also handles a
    /// removal that finishes while the user is elsewhere; old in-flight bodies stay filtered.
    pub(super) fn fx_wl_invalidate(&mut self) {
        self.saved.retain(|s| s.url != wl::PAGE_URL);
        self.page_cache.retain(|(u, _, _)| u != wl::PAGE_URL);
        self.snap_at.remove(wl::PAGE_URL);
        bridge::session_del(&crate::snap::key(wl::PAGE_URL));
        let pending = self.page_prefetching.remove(wl::PAGE_URL);
        if let Some(id) = pending { self.pending.remove(&id); self.abort_req(id); }
        if self.await_page.as_deref() == Some(wl::PAGE_URL) {
            self.await_page = None;
            if matches!(&self.route, Route::Playlist { list } if list == "WL") && !self.rendered { self.load(); }
        }
    }

    pub(super) fn fx_wl_redraw(&mut self) {
        let focus = bridge::focus_id();
        self.fx_refilter(false);
        if self.fx_visible(&self.feed.shown).is_empty() && !self.has_more() {
            bridge::html(LIST, "<p class=\"empty\" role=\"status\">This playlist is empty.</p>");
        }
        if focus.starts_with("ytc-wl-") { bridge::focus(&focus); }
    }

    /// First hover/focus of a card's button: ask whether it is already saved.
    fn fx_wl_probe(&mut self, v: &str) {
        if !crate::util::is_video_id(v) || !self.auth.signed_in() {
            return;
        }
        let ask = features::shared(|s| {
            let ask = !s.saves.probed.contains_key(v) && s.saves.get(v) == State::Idle;
            if ask {
                s.saves.probed.insert(v.to_string(), false);
            }
            ask
        });
        if ask {
            self.fx_wl_send(v, None);
        }
    }

    /// The pointer entered a card's Watch later button (`data-hov="wl"`).
    pub(super) fn fx_wl_hover(&mut self, v: &str) {
        self.fx_wl_probe(v);
    }

    pub fn on_focus(&mut self, name: &str, arg: &str, on: bool) {
        match name {
            "vk" => self.voice.key_focus = on,
            "wl" if on => self.fx_wl_probe(arg),
            // The chapter panel under the description (the player drives it).
            "chaps" => {
                let kb = self.keyed();
                if let Some(p) = self.player.as_mut() {
                    p.chap_focus(on, kb);
                }
            }
            "fx-ren" => {
                let i = arg.parse::<usize>().ok();
                if on {
                    self.fx.ren = i;
                } else if self.fx.ren == i {
                    // Left without Enter: the field shows the saved name again (only Enter saves).
                    self.fx.ren = None;
                    if let Some(i) = i {
                        self.fx_ren_revert(i);
                    }
                }
            }
            "q" => {
                if crate::tv::on() {
                    self.tv_search_focus(on);
                    // TV: the list is part of the search screen; the focus moving into it keeps it.
                    if !on && self.tv_search_on() {
                        return;
                    }
                }
                self.fx.sug.focused = on;
                let kb = self.keyed();
                self.fx_form_now(kb);
                if on {
                    self.fx.sug.anim = !kb;
                    let typed = if crate::tv::on() {
                        // The TV query control is a button backed by this input. Empty is a real
                        // edit too: returning from its keyboard must not revive the route's query.
                        bridge::input_value("ytc-q")
                    } else if self.fx.sug.typed.is_empty() {
                        if let Route::Search { q, .. } = &self.route { q.clone() } else { String::new() }
                    } else {
                        self.fx.sug.typed.clone()
                    };
                    self.fx.sug.typed = typed.clone();
                    self.fx_sug_render(&typed);
                } else {
                    self.fx_sug_close(!kb);
                }
            }
            _ => {}
        }
    }

    // ---------------------------------------------------- search dropdown

    /// `#ytc-search.now` turns the field's focus morph off (client.css). Set on
    /// every key press and cleared on every pointer press, i.e. before focus
    /// moves: the browser computes the field's focused style while it moves,
    /// so a class flipped in the focus handler would come one style too late.
    pub(super) fn fx_form_now(&mut self, now: bool) {
        if self.fx.sug.form_now != now {
            self.fx.sug.form_now = now;
            bridge::attr("ytc-search", "class", if now { "now" } else { "" });
        }
    }

    fn fx_sug_render(&mut self, typed: &str) {
        let s = &mut self.fx.sug;
        s.hist = self.fx.hist.matches(typed, if typed.trim().is_empty() { history::SHOWN } else { 4 });
        if typed.trim().is_empty() {
            s.sugg.clear();
        }
        let rows = history::rows(&s.hist, &s.sugg);
        if !s.focused || rows.is_empty() {
            bridge::html(SUGGEST, "");
            bridge::attr(SUGGEST, "class", "sug");
            bridge::attr("ytc-q", "aria-expanded", "false");
            return;
        }
        if s.sel.is_some_and(|i| i >= rows.len()) {
            s.sel = None;
        }
        bridge::html(SUGGEST, &history::dropdown(&s.hist, &s.sugg, s.sel));
        // Persistent list: `on` unfolds it from the field (pointer); `now` = no motion.
        bridge::attr(SUGGEST, "class", if s.anim { "sug on" } else { "sug on now" });
        bridge::attr("ytc-q", "aria-expanded", "true");
        bridge::attr("ytc-q", "aria-activedescendant", &s.sel.map(|i| format!("ytc-sg{i}")).unwrap_or_default());
    }

    /// `exit`: a pointer closed it, so it folds back into the field with its rows
    /// (kept until the next open rewrites them); otherwise it goes at once.
    /// Voice dictation starts: the dropdown would cover the live transcript.
    pub(super) fn fx_sug_hide(&mut self) {
        if self.fx.sug.focused {
            self.fx_sug_close(false);
        }
    }

    /// TV: the search screen closed (the list goes with it).
    pub(super) fn fx_sug_end(&mut self) {
        self.fx.sug.focused = false;
        self.fx_sug_close(false);
    }

    pub(super) fn fx_sug_close(&mut self, exit: bool) {
        self.fx.sug.sel = None;
        bridge::attr(SUGGEST, "class", "sug");
        if !exit {
            bridge::html(SUGGEST, "");
        }
        bridge::attr("ytc-q", "aria-expanded", "false");
        bridge::attr("ytc-q", "aria-activedescendant", "");
    }

    /// The user typed in the search box.
    pub(super) fn fx_typed(&mut self, value: &str) {
        self.fx.sug.anim = false;
        self.fx.sug.typed = value.to_string();
        self.fx.sug.sel = None;
        self.fx.sug.focused = true;
        self.fx_sug_render(value);
    }

    /// YouTube's suggestions for the typed text arrived.
    pub(super) fn fx_suggestions(&mut self, list: &[String]) {
        self.fx.sug.sugg = list.to_vec();
        let typed = self.fx.sug.typed.clone();
        self.fx_sug_render(&typed);
    }

    /// A search was submitted: remember it.
    pub(super) fn fx_searched(&mut self, q: &str) {
        self.fx.calm_next = true;
        if self.fx.hist.record(q, bridge::epoch()) && self.fx.hist_loaded {
            bridge::storage_set(history::KEY, &self.fx.hist.to_json());
        }
        self.fx.sug.typed = q.to_string();
        self.fx.sug.sugg.clear();
        self.fx_sug_close(false);
    }

    /// Keys while a text field has focus (the search dropdown).
    pub(super) fn fx_key(&mut self, key: &str) -> bool {
        if key == "Escape" {
            if let Some(i) = self.fx.ren {
                // Escape cancels a rename: the saved name returns, focus stays in the field.
                self.fx_ren_revert(i);
                return true;
            }
        }
        let rows = history::rows(&self.fx.sug.hist, &self.fx.sug.sugg);
        let open = self.fx.sug.focused && !rows.is_empty();
        if !open {
            return false;
        }
        self.fx.sug.anim = false;
        match key {
            "ArrowDown" | "ArrowUp" => {
                let n = rows.len();
                let sel = match (self.fx.sug.sel, key == "ArrowDown") {
                    (None, true) => Some(0),
                    (None, false) => Some(n - 1),
                    (Some(i), true) => (i + 1 < n).then_some(i + 1),
                    (Some(i), false) => i.checked_sub(1),
                };
                self.fx.sug.sel = sel;
                bridge::value("ytc-q", sel.map(|i| rows[i].as_str()).unwrap_or(&self.fx.sug.typed));
                let typed = self.fx.sug.typed.clone();
                self.fx_sug_render(&typed);
                // Resting on a row fetches its results ahead (Enter then paints from memory).
                let q: String = sel.map(|i| rows[i].as_str()).unwrap_or(&typed).trim().chars().take(100).collect();
                self.search_warm_soon(&q, SEARCH_PICK_MS);
                true
            }
            "Escape" => {
                if self.fx.sug.sel.is_some() {
                    bridge::value("ytc-q", &self.fx.sug.typed.clone());
                }
                self.fx.sug.focused = false;
                self.fx_sug_close(false);
                true
            }
            "Delete" => {
                let Some(i) = self.fx.sug.sel.filter(|i| *i < self.fx.sug.hist.len()) else { return false };
                let q = rows[i].clone();
                self.fx.hist.remove(&q);
                if self.fx.hist_loaded {
                    bridge::storage_set(history::KEY, &self.fx.hist.to_json());
                }
                self.fx.sug.sel = None;
                bridge::value("ytc-q", &self.fx.sug.typed.clone());
                let typed = self.fx.sug.typed.clone();
                self.fx_sug_render(&typed);
                true
            }
            _ => false,
        }
    }

    // ----------------------------------------------------------- progress

    /// Where to start a watch page without `t=` (a saved partial position).
    pub(super) fn fx_resume(&self, v: &str) -> Option<f64> {
        features::shared(|s| s.progress.resume_at(v))
    }

    /// The player reported a position (`flush` = leaving the page).
    pub(super) fn fx_position(&mut self, v: &str, t: f64, dur: f64, ended: bool, flush: bool) {
        let changed = features::shared(|s| s.progress.record(v, t, dur, ended, bridge::epoch()));
        if !changed {
            return;
        }
        self.fx.prog_dirty = true;
        self.fx.recent.insert(v.to_string());
        if flush || ended {
            self.fx_save_progress();
        } else if !self.fx.prog_timer {
            self.fx.prog_timer = true;
            let id = next_id();
            self.fx.timers.insert(id, Timer::SaveProgress);
            bridge::timer(id, SAVE_PROGRESS_MS);
        }
    }

    // ------------------------------------------------------------- topics

    fn fx_topic_abort(&mut self) {
        let ids: Vec<u32> = self.fx.reqs.iter().filter(|(_, r)| matches!(r, Req::Topic { .. })).map(|(i, _)| *i).collect();
        for id in ids {
            bridge::abort(id);
            if let Some(Req::Topic { job, .. }) = self.fx.reqs.remove(&id) {
                if let Some(s) = self.fx.session.as_mut() {
                    // Not answered: may run again later (it was counted, keep the bound).
                    s.inflight.remove(&id);
                    s.queue.push_front(job);
                    s.reads = 0;
                }
            }
        }
    }

    fn fx_topic_reset(&mut self) {
        self.fx_topic_abort();
        self.fx.session = None;
        self.fx.sel = None;
        self.fx.stash = None;
        self.fx.parked = false;
        self.fx.topic_want = false;
    }

    fn fx_topic_select(&mut self, i: usize) {
        if self.route != Route::Home || !self.rendered {
            return;
        }
        let Some(name) = self.fx.topics.get(i).cloned() else { return };
        let Ok(topic) = topics::create(&name) else { return };
        if self.fx.chip_on() {
            // The topic replaces the native All feed, not the YouTube chip's.
            self.fx_chip_abort();
            if let Some(all) = self.fx.nstash.take() {
                self.feed = all;
            }
            self.fx.nsel = None;
        }
        self.fx_topic_abort();
        // A pending native refresh must not replace the topic page.
        self.abort_nav_requests();
        self.swr = false;
        if self.held.take().is_some() {
            bridge::html(crate::render::PILL, "");
        }
        if !self.fx.topic_on() {
            self.fx.stash = Some(std::mem::take(&mut self.feed));
        }
        let initial: Vec<String> = self.fx.stash.as_ref().map(|f| f.shown.iter().map(|i| i.key().to_string()).collect()).unwrap_or_default();
        self.fx.session = Some(topics::Session::new(topic, &initial));
        self.fx.sel = Some(i);
        self.fx.parked = false;
        self.generation += 1;
        self.cont_busy = false;
        self.want = false;
        self.feed = FeedState { head: Some(render::feed_head("Home")), cont_ep: "browse", ..Default::default() };
        self.fx.order.reset(&[]);
        let head = self.feed.head.clone().expect("set above");
        bridge::html(MAIN, &render::page(&head, &[], true, "", ImgPlan::first(Lay::Grid, self.cols)));
        bridge::scroll_to(0.0);
        self.after_paint();
        self.fx.enter = !self.fx.kbd;
        self.fx_topic_advance(true);
    }

    fn fx_topic_all(&mut self) {
        let was = self.fx.topic_on();
        let stash = self.fx.stash.take();
        self.fx_topic_reset();
        if !was || self.route != Route::Home {
            self.fx_bar();
            return;
        }
        match stash {
            Some(f) => {
                self.generation += 1;
                self.cont_busy = false;
                self.want = false;
                self.feed = f;
                let head = self.feed.head.clone().unwrap_or_else(|| render::feed_head("Home"));
                let vis = self.fx_visible(&self.feed.shown);
                bridge::html(MAIN, &render::page(&head, &vis, self.has_more(), &self.feed_empty("Nothing here yet."), ImgPlan::first(Lay::Grid, self.cols)));
                bridge::scroll_to(0.0);
                self.after_paint();
            }
            None => {
                self.rendered = false;
                self.start_route();
            }
        }
    }

    /// Asks the session for more cards (`want` = the user is waiting for them).
    fn fx_topic_advance(&mut self, want: bool) {
        self.fx.topic_want |= want;
        let Some(sel) = self.fx.sel else { return };
        let Some(s) = self.fx.session.as_mut() else { return };
        let jobs = s.jobs();
        let version = self.cfg.version.clone();
        for job in jobs {
            let id = next_id();
            let body = topics::search_body(&version, &s.topic.query, &job);
            bridge::fetch_opts(id, "POST", topics::SEARCH_URL, &body, ANON_HEADERS, "{\"credentials\":\"omit\"}");
            s.inflight.insert(id, job.clone());
            self.fx.reqs.insert(id, Req::Topic { job, sel });
        }
        if self.fx.topic_want && self.fx.session.as_ref().is_some_and(|s| s.ready()) {
            self.fx_topic_deliver();
        }
        self.fx_note();
    }

    fn fx_topic_deliver(&mut self) {
        let Some(s) = self.fx.session.as_mut() else { return };
        let batch = s.take_batch();
        self.fx.topic_want = false;
        let items: Vec<Item> = batch.into_iter().filter(|v| self.feed.seen.insert(v.id.clone())).map(Item::Video).collect();
        self.remember(&items);
        self.fx.order.add(&items);
        self.feed.buf.extend(items);
        if self.feed.buf.is_empty() {
            if self.has_more() {
                bridge::observe(MORE);
            } else {
                bridge::attr(MORE, "class", "more done");
            }
        } else {
            self.render_chunk();
        }
        self.fx_note();
    }

    /// The sentinel is near the viewport. True when handled here.
    pub(super) fn fx_on_visible(&mut self) -> bool {
        if self.fx.topic_on() {
            if !self.feed.buf.is_empty() {
                self.render_chunk();
            } else {
                self.fx_topic_advance(true);
            }
            return true;
        }
        if self.fx.blank_streak >= BLANK_LIMIT && self.feed.buf.is_empty() && self.feed.cont.is_some() && !self.cont_busy {
            // The filters hid everything recently: load more only on request.
            bridge::html(MORE, "<button type=\"button\" class=\"pill\" data-act=\"fx-more\">Load more</button>");
            return true;
        }
        false
    }

    // ---------------------------------------------------------- navigation

    // --------------------------------------------- YouTube's Home chips

    /// A Home `browse` response: its chip bar (tokens stay in memory).
    pub(super) fn fx_home_response(&mut self, json: &Value) {
        if let Some(c) = hc::parse(json, &mut self.fx.nall) {
            hc::adopt(&mut self.fx.nbar, c);
        }
    }

    /// A ready Home response's chips (`fx_home_response` when it was fetched).
    pub(super) fn fx_home_chips(&mut self, chips: Vec<hc::Chip>, all: Option<String>) {
        if all.is_some() {
            self.fx.nall = all;
        }
        if !chips.is_empty() {
            hc::adopt(&mut self.fx.nbar, chips);
        }
    }

    fn fx_chip_abort(&mut self) {
        self.fx.ngen += 1;
        let ids: Vec<u32> = self.fx.reqs.iter().filter(|(_, r)| matches!(r, Req::Chip { .. })).map(|(i, _)| *i).collect();
        for id in ids {
            bridge::abort(id);
            self.fx.reqs.remove(&id);
        }
    }

    fn fx_chip_reset(&mut self) {
        self.fx_chip_abort();
        self.fx.nsel = None;
        self.fx.nstash = None;
        self.fx.nparked = false;
    }

    /// A YouTube chip: its feed through the chip's own continuation token.
    fn fx_chip_select(&mut self, key: &str) {
        if self.route != Route::Home || !self.rendered {
            return;
        }
        let Some(token) = self.fx.nbar.iter().find(|c| c.key == key && !c.token.is_empty()).map(|c| c.token.clone()) else { return };
        if self.fx.chip_on() && self.fx.nsel.as_deref() == Some(key) {
            return;
        }
        if self.fx.topic_on() {
            let all = self.fx.stash.take();
            self.fx_topic_reset();
            if let Some(f) = all {
                self.fx.nstash = Some(f);
            }
        } else if self.fx.nsel.is_none() {
            self.fx.nstash = Some(std::mem::take(&mut self.feed));
        }
        self.fx_chip_abort();
        // A pending native refresh must not replace the chip's page.
        self.abort_nav_requests();
        self.swr = false;
        if self.held.take().is_some() {
            bridge::html(crate::render::PILL, "");
        }
        self.fx.nsel = Some(key.to_string());
        self.generation += 1;
        self.cont_busy = false;
        self.want = false;
        self.feed = FeedState { head: Some(render::feed_head("Home")), cont_ep: "browse", ..Default::default() };
        self.fx.order.reset(&[]);
        let head = self.feed.head.clone().expect("set above");
        bridge::html(MAIN, &render::page(&head, &[], true, "", ImgPlan::first(Lay::Grid, self.cols)));
        bridge::scroll_to(0.0);
        self.after_paint();
        self.fx.enter = !self.fx.kbd;
        bridge::focus(&features::control_id("fx-nchip", key));
        self.auth = crate::tv::auth();
        let req = crate::api::Req::Cont { ep: "browse", token };
        let (body, headers) = crate::tv::finish(&self.cfg, &self.auth, req.body(&self.cfg, &self.hl, &self.url), (bridge::epoch() / 1000.0) as u64);
        let id = next_id();
        bridge::fetch_opts(id, "POST", &req.url(), &body, &headers, "{}");
        self.fx.reqs.insert(id, Req::Chip { seq: self.fx.ngen });
    }

    /// All while a YouTube chip is shown: the native feed again, no request.
    fn fx_chip_all(&mut self) {
        let stash = self.fx.nstash.take();
        self.fx_chip_reset();
        match stash {
            Some(f) if self.route == Route::Home => {
                self.generation += 1;
                self.cont_busy = false;
                self.want = false;
                self.feed = f;
                let head = self.feed.head.clone().unwrap_or_else(|| render::feed_head("Home"));
                let vis = self.fx_visible(&self.feed.shown);
                bridge::html(MAIN, &render::page(&head, &vis, self.has_more(), &self.feed_empty("Nothing here yet."), ImgPlan::first(Lay::Grid, self.cols)));
                bridge::scroll_to(0.0);
                self.after_paint();
            }
            _ => {
                self.rendered = false;
                self.start_route();
            }
        }
        bridge::focus(&features::control_id("fx-topic", "-1"));
    }

    fn fx_chip_feed(&mut self, status: u32, body: &[u8]) {
        let json = (status == 200).then(|| json::parse(body)).flatten();
        let head = render::feed_head("Home");
        match json {
            Some(j) => {
                if let Some(c) = hc::parse(&j, &mut self.fx.nall) {
                    hc::adopt(&mut self.fx.nbar, c);
                }
                let feed = parse::continuation(&j);
                self.show_feed(head, feed, "browse", "YouTube has nothing for this topic right now.", Vec::new());
            }
            None => {
                let what = self.fx.nsel.clone().unwrap_or_default();
                self.incident("home-chip", &format!("YouTube Home chip \"{what}\" failed: HTTP {status}"), None);
                self.show_feed(head, parse::Feed::default(), "browse", "Couldn't load this topic. Pick it again or All.", Vec::new());
            }
        }
        self.after_paint();
    }

    /// Leaving the current page.
    pub(super) fn fx_leave(&mut self) {
        self.fx.calm = std::mem::take(&mut self.fx.calm_next);
        self.fx.enter = false;
        if self.fx.topic_on() {
            self.fx.parked = true;
        }
        if self.fx.chip_on() {
            self.fx.nparked = true;
            self.fx_chip_abort();
        }
        self.fx_topic_abort();
        let checks: Vec<u32> = self.fx.reqs.iter().filter(|(_, r)| matches!(r, Req::Check { .. })).map(|(i, _)| *i).collect();
        for id in checks {
            bridge::abort(id);
            self.fx.reqs.remove(&id);
            self.fx.checker.inflight.remove(&id);
        }
        self.fx.checker.clear_queue();
        self.fx.hold.clear();
        self.fx.sug.focused = false;
        self.fx.sug.typed.clear();
        self.fx_sug_close(false);
        if self.fx.prog_dirty {
            self.fx_save_progress();
        }
    }

    /// Back/forward restored a page from memory (a parked topic feed returns).
    pub(super) fn fx_restored(&mut self) {
        if self.route == Route::Home {
            self.fx.parked = false;
            self.fx.nparked = false;
        }
        self.fx.check_bars = true;
    }

    // ----------------------------------------------------------- responses

    pub(super) fn fx_fetch(&mut self, id: u32, status: u32, body: &[u8]) -> bool {
        let Some(req) = self.fx.reqs.remove(&id) else { return false };
        let ok = status == 200;
        match req {
            Req::Wl { v, edit: Some(e) } => {
                let out = wl::classify(e, status, body);
                self.fx_wl_settle(&v, e, &out);
            }
            Req::Wl { v, edit: None } => {
                let saved = ok.then(|| wl::membership(body)).flatten();
                let st = features::shared(|s| {
                    s.saves.probed.insert(v.clone(), true);
                    if saved == Some(true) && s.saves.get(&v) == State::Idle {
                        s.saves.state.insert(v.clone(), State::Saved);
                        Some(State::Saved)
                    } else {
                        None
                    }
                });
                if let Some(st) = st {
                    Self::fx_wl_paint(&v, &st, false);
                }
            }
            Req::Check { v } => {
                self.fx.checker.inflight.remove(&id);
                let mut blocked = false;
                if let Some(w) = ok.then(|| json::parse(body)).flatten().map(|j| parse::next(&j)) {
                    if let Some(hosts) = websites::hosts_from_next(&w, &v) {
                        self.fx.checker.learn(&v, hosts);
                        blocked = features::shared(|s| self.fx.checker.verdict(&v, &s.rules) == Some(websites::Verdict::Blocked));
                        if blocked {
                            bridge::remove_all(&format!("#{LIST} .c[data-v=\"{v}\"]"));
                            self.fx_note();
                        }
                    }
                }
                // Allowed or unknown (failed check): a held card appears now.
                if self.fx.hold.settle(&v, bridge::now(), blocked) && !blocked {
                    Self::fx_reveal(&[v]);
                }
                self.fx_check_pump();
            }
            Req::Chip { seq } => {
                if seq == self.fx.ngen && self.fx.chip_on() && self.route == Route::Home {
                    self.fx_chip_feed(status, body);
                }
            }
            Req::Topic { job, sel } => {
                if self.fx.sel != Some(sel) {
                    return true;
                }
                let json = ok.then(|| json::parse(body)).flatten();
                let Some(s) = self.fx.session.as_mut() else { return true };
                s.inflight.remove(&id);
                let mut failed: Option<String> = None;
                match (json, &job) {
                    (Some(j), topics::Job::Find(_)) => s.on_results(&job, &parse::search(&j)),
                    (Some(j), topics::Job::More(..)) => s.on_results(&job, &parse::search_continuation(&j)),
                    (None, j) => {
                        s.on_error(j, &format!("HTTP {status}"));
                        failed = Some(format!("topic feed {:?} request failed: HTTP {status} ({})", j, s.status()));
                    }
                }
                let ready = s.ready();
                if let Some(what) = failed {
                    self.incident("topic-feed", &what, None);
                }
                if self.fx.topic_want && ready {
                    self.fx_topic_deliver();
                } else {
                    let want = self.fx.topic_want;
                    self.fx_topic_advance(want);
                }
                self.fx_note();
            }
        }
        true
    }
}

fn order_arg(popular: bool) -> &'static str {
    if popular { "popular" } else { "recommended" }
}
