//! A channel page's tabs: one persistent header and tab row, a list per tab.
//!
//! The tabs come from the channel response's own `tabRenderer` list (`parse::channel`), nothing is assumed. A tab
//! change is a navigation (`App::navigate`: history, telemetry, what a page change tears down), except that only the
//! list under the tab row changes: the list on screen is set aside whole (`bridge::park`: its cards, decoded pictures
//! and `FeedState`) and a tab seen before comes back from there in the same task, with its scroll position, no request
//! and no flash. A new tab shows skeletons in its cards' shape and loads through the page prefetch (`load`: a hover or
//! focus prefetch already on its way or in the cache is adopted, the hedge applies), so its answer stays useful when
//! the user has moved on: it lands in the page cache and never touches the tab on screen. Sort chips work the same way
//! inside a tab, through YouTube's own tokens (`SortOpt`). Everything here lives in memory while the channel is open.

use super::*;
use crate::motion;
use crate::parse::{ChannelPage, ChannelTabLink, SortOpt};
use crate::render::{CH_HEAD, CH_MARK, CH_SORT, CH_TABS};

/// Lists set aside per channel (tabs × sorts); the oldest goes first.
const PARKED_MAX: usize = 12;
/// `.chb`'s sticky place under the top bar, and the gap between the header and the tab row (client.css).
const STICK_TOP: f64 = 64.0;
const HEAD_GAP: f64 = 16.0;
const SKELETON_CARDS: usize = 12;

/// A list set aside.
struct TabSave {
    feed: FeedState,
    scroll: f64,
    stats: Stats,
    title: String,
}

#[derive(Default)]
pub(super) struct ChanState {
    /// The channel these belong to (`Route::Channel::path`).
    path: String,
    tabs: Vec<ChannelTabLink>,
    /// Tabs that answered with nothing the client can show: out of the row for this visit.
    hidden: Vec<ChannelTab>,
    parked: Vec<(String, TabSave)>,
    /// The header and tab row are on the page.
    pub(super) shell: bool,
    /// The next page answer is written into the list only (the tab row stays).
    pub(super) inplace: bool,
    /// The sort row as written (no write, and no lost focus or slide, when it stays the same).
    pub(super) row: String,
    /// The tab being left, for the indicator.
    was: ChannelTab,
    /// Where the list a sort replaced is set aside, while that sort loads.
    sort_back: String,
    /// Where the list starts when the tab row is stuck (the scroll a new list opens at, when past it).
    top: f64,
    past_top: bool,
}

impl ChanState {
    pub(super) fn visible(&self) -> Vec<ChannelTabLink> {
        self.tabs.iter().filter(|t| !self.hidden.contains(&t.tab)).cloned().collect()
    }

    fn take(&mut self, key: &str) -> Option<TabSave> {
        let i = self.parked.iter().position(|(k, _)| k == key)?;
        Some(self.parked.remove(i).1)
    }

    fn put(&mut self, key: String, save: TabSave) {
        self.parked.retain(|(k, _)| *k != key);
        self.parked.push((key, save));
        if self.parked.len() > PARKED_MAX {
            let (old, _) = self.parked.remove(0);
            bridge::park_drop(&old);
        }
    }

    fn drop_parked(&mut self) {
        if !self.parked.is_empty() {
            self.parked.clear();
            bridge::park_drop("");
        }
    }
}

/// The tab's cards as the tab wants them. The Shorts tab shows its Shorts whatever the Shorts switch says (the user
/// asked for them by opening it): there they are ordinary cards, with their badge gone since the whole tab is Shorts.
/// True when nothing else may drop them.
pub(super) fn admit_tab(route: &Route, items: &mut [Item]) -> bool {
    let Route::Channel { tab, .. } = route else { return false };
    let kind = match tab {
        ChannelTab::Podcasts => "Podcast",
        ChannelTab::Courses => "Course",
        ChannelTab::Releases => "Release",
        ChannelTab::Shows => "Show",
        _ => "",
    };
    for it in items.iter_mut() {
        match it {
            Item::Video(v) if *tab == ChannelTab::Shorts => {
                v.short = false;
                v.duration.clear();
            }
            // Every card here is this channel's: no byline. (YouTube's first row is "Playlist", linked to the channel.)
            Item::Playlist(p) => {
                p.kind = kind;
                p.channel.clear();
                p.channel_href = None;
            }
            _ => {}
        }
    }
    *tab == ChannelTab::Shorts
}

impl App {
    /// The tab's `browse` params, when this channel's tab list is known.
    pub(super) fn chan_params(&self, path: &str, tab: ChannelTab) -> Option<String> {
        if self.chan.path != path {
            return None;
        }
        self.chan.tabs.iter().find(|t| t.tab == tab).map(|t| t.params.clone())
    }

    /// `tab` of this channel is set aside: opening it needs no request.
    pub(super) fn chan_loaded(&self, path: &str, tab: ChannelTab) -> bool {
        self.chan.path == path && self.chan.parked.iter().any(|(k, _)| k == tab.slug())
    }

    /// `route` is another tab of the channel on the page.
    pub(super) fn chan_same(&self, route: &Route) -> bool {
        self.chan.shell && matches!((&self.route, route), (Route::Channel { path: a, .. }, Route::Channel { path: b, .. }) if a == b)
    }

    /// Going somewhere that is not this channel's tab. A new visit starts over; a video opened from here keeps the
    /// lists for the way back; anywhere else lets them go (a TV has little memory to spare).
    pub(super) fn chan_left(&mut self, to: &Route, mode: Mode) {
        self.chan.shell = false;
        self.chan.inplace = false;
        match to {
            Route::Channel { path, .. } => {
                if *path != self.chan.path || mode == Mode::Push {
                    self.chan.drop_parked();
                    self.chan.hidden.clear();
                    if *path != self.chan.path {
                        self.chan.tabs.clear();
                    }
                    self.chan.path = path.clone();
                }
            }
            Route::Watch { .. } => {}
            _ => self.chan.drop_parked(),
        }
    }

    /// The page shows `feed.head`: the tab row is there when it has tabs.
    pub(super) fn chan_sync(&mut self) {
        self.chan.inplace = false;
        let Some(nav) = self.feed.head.as_ref().and_then(Head::nav) else {
            self.chan.shell = false;
            return;
        };
        if nav.path != self.chan.path {
            self.chan.drop_parked();
            self.chan.hidden.clear();
            self.chan.path = nav.path.clone();
        }
        self.chan.shell = !nav.tabs.is_empty();
        if !nav.tabs.is_empty() {
            self.chan.tabs = nav.tabs.clone();
        }
        self.chan.row = render::channel_sorts(&nav.sorts);
    }

    /// The tab on screen is left: its list is set aside whole. A list still loading has nothing to keep, and a failed
    /// tab (no head) asks again when it comes back. (No snapshot here: walking a channel's tabs must not push other
    /// pages' snapshots out; `on_hide` and `save_current` keep the one on screen.)
    pub(super) fn chan_leave_tab(&mut self) {
        let Route::Channel { tab, .. } = self.route else { return };
        self.chan.was = tab;
        self.tv_tab_opened();
        let y = bridge::scroll_y();
        self.chan.top = y.min(self.chan_list_top(y));
        self.chan.past_top = self.chan.top < y;
        if self.rendered && self.feed.head.is_some() {
            self.chan_park(tab.slug().to_string());
        }
    }

    /// Where the page stands with the header just gone and the tab row stuck under the top bar. (TV: no sticky row,
    /// a new list opens at the top.)
    fn chan_list_top(&self, y: f64) -> f64 {
        if crate::tv::on() {
            return 0.0;
        }
        match bridge::rect(CH_HEAD).as_slice() {
            [_, top, _, h] => (y + top + h + HEAD_GAP - STICK_TOP).max(0.0),
            _ => 0.0,
        }
    }

    fn chan_park(&mut self, key: String) {
        // (The position first: without its cards the page is shorter.)
        let scroll = bridge::scroll_y();
        bridge::park(LIST, &key);
        let save = TabSave { feed: std::mem::take(&mut self.feed), scroll, stats: std::mem::take(&mut self.stats), title: self.title.clone() };
        self.chan.put(key, save);
    }

    /// Writes a sort row unless it is what the page already shows. Focus on a chip stays on it.
    fn chan_row_write(&mut self, sorts: &[SortOpt]) {
        let row = render::channel_sorts(sorts);
        if row == self.chan.row {
            return;
        }
        let focus = bridge::focus_id();
        bridge::html(CH_SORT, &row);
        self.chan.row = row;
        if focus.starts_with("ytc-cs-") {
            bridge::focus(&focus);
        }
        bridge::pill_carry(CH_SORT);
    }

    /// The route is now another tab of this channel.
    pub(super) fn chan_enter(&mut self, mode: Mode) {
        let Route::Channel { tab, .. } = self.route else { return };
        self.chan_mark(self.chan.was, tab, mode == Mode::Push && !self.keyed());
        bridge::attr(LIST, "class", render::grid_class(tab));
        self.cols = render::lay_cols(self.lay(), bridge::width(), crate::tv::on());
        // (TV: pressing OK on a tab keeps the focus on the row, which does not stick: the list opens at its top.)
        let own_scroll = !(crate::tv::on() && mode == Mode::Push);
        if self.chan.parked.iter().any(|(k, _)| k == tab.slug()) {
            crate::telemetry::nav_src("memory");
            if !own_scroll {
                bridge::scroll_to(0.0);
            }
        }
        if self.chan_restore(tab.slug(), own_scroll) {
            return;
        }
        // Skeletons in the cards' shape, unless the answer is already here (written in this same task).
        if !self.page_cached(&self.route.url()) {
            // (`row` holds what the page shows: an answer with no sort choices has to clear these.)
            let skeleton = render::sort_skeleton(tab);
            bridge::html(CH_SORT, &skeleton);
            self.chan.row = skeleton;
            bridge::html(LIST, &render::channel_skeleton(SKELETON_CARDS));
            bridge::attr(MORE, "class", "more done");
        }
        self.chan.inplace = true;
        // (TV: the Subscribe button stays usable while the list loads.)
        if crate::tv::on() {
            self.tv_channel_paint();
        }
        // As a page prefetch: left before it answers, the answer still lands in the page cache for coming back.
        let url = self.route.url();
        self.prefetch_page_now(&url);
        self.load();
        if self.chan.past_top {
            bridge::scroll_to(self.chan.top);
        }
    }

    /// Brings back the list set aside under `key`, if any. `own_scroll`: at its own position (a tab); else where
    /// the page stands (a sort, whose chips are under the pointer).
    fn chan_restore(&mut self, key: &str, own_scroll: bool) -> bool {
        let Some(s) = self.chan.take(key) else { return false };
        if !bridge::unpark(LIST, key) {
            return false;
        }
        self.chan.inplace = false;
        self.feed = s.feed;
        self.stats = s.stats;
        let sorts = self.feed.head.as_ref().and_then(Head::nav).map(|n| n.sorts.clone()).unwrap_or_default();
        self.chan_row_write(&sorts);
        bridge::attr(MORE, "class", if self.has_more() { "more" } else { "more done" });
        if own_scroll {
            self.restore_scroll(s.scroll);
        }
        self.fx_new_feed();
        self.fx_restored();
        self.set_title(&s.title);
        self.mark("restored");
        self.after_paint();
        true
    }

    /// The selected tab changes at once; the indicator slides on a pointer press, jumps otherwise.
    fn chan_mark(&self, from: ChannelTab, to: ChannelTab, slide: bool) {
        let (old, new) = (render::ch_tab_id(from), render::ch_tab_id(to));
        if from != to {
            bridge::attr(&old, "aria-current", "false");
        }
        bridge::attr(&new, "aria-current", "page");
        if crate::tv::on() {
            return;
        }
        self.chan_tab_in_view();
        if slide && from != to {
            bridge::mark_slide(CH_TABS, &old, "u", CH_MARK, false);
            bridge::mark_slide(CH_TABS, &new, "u", CH_MARK, true);
        } else if bridge::any("#ytc-cht[data-mk]") {
            bridge::mark_slide(CH_TABS, &new, "u", CH_MARK, false);
        }
    }

    /// A narrow window: the row scrolls sideways to its selected tab. (TV: the row does not scroll.)
    pub(super) fn chan_tab_in_view(&self) {
        if self.chan.shell && !crate::tv::on() {
            bridge::scroll_keep_x(render::CH_ROW, "[aria-current=page]");
        }
    }

    /// Writes the feed on screen under the tab row (the page's own answer, a sort's).
    pub(super) fn chan_write_list(&mut self, empty: &str) {
        let sorts = self.feed.head.as_ref().and_then(Head::nav).map(|n| n.sorts.clone()).unwrap_or_default();
        self.chan_row_write(&sorts);
        let vis = self.fx_visible(&self.feed.shown);
        let mut s = String::with_capacity(vis.len() * 1000);
        render::cards(&mut s, &vis, ImgPlan::first(self.lay(), self.cols));
        if vis.is_empty() {
            s.push_str("<p class=\"empty\">");
            crate::util::esc_into(&mut s, empty);
            s.push_str("</p>");
        }
        bridge::html(LIST, &s);
        bridge::attr(MORE, "class", if self.has_more() { "more" } else { "more done" });
    }

    /// A channel answer for `want`: learns the channel's tabs and decides which tab this is. `None`: another tab was
    /// asked for instead (YouTube answered with its Home because this channel has no such tab, or the tab holds
    /// nothing the client can show).
    pub(super) fn chan_answered(&mut self, path: &str, want: ChannelTab, c: &ChannelPage) -> Option<ChannelTab> {
        if self.chan.path != path {
            self.chan.drop_parked();
            self.chan.hidden.clear();
            self.chan.path = path.to_string();
        }
        if !c.tabs.is_empty() {
            self.chan.tabs = c.tabs.clone();
        }
        let tabs = self.chan.visible();
        let Some(selected) = c.selected.as_deref() else { return Some(want) };
        match ChannelTab::from_slug(selected) {
            Some(tab) if tab == want => {
                // An empty page in a tab YouTube lists: its content is of a kind the client cannot show. Out of the row.
                if c.feed.items.is_empty() && c.feed.cont.is_none() && c.feed.alert.is_none() && tabs.len() > 1 {
                    self.chan.hidden.push(tab);
                    bridge::remove_all(&format!("#{}", render::ch_tab_id(tab)));
                    if let Some(go) = self.chan.visible().first().map(|t| t.tab) {
                        self.chan_redirect(go);
                        return None;
                    }
                }
                Some(want)
            }
            // YouTube chose another tab it lists: that tab is the page, same answer.
            Some(tab) => {
                self.chan_retarget(tab);
                Some(tab)
            }
            None => match tabs.first().map(|t| t.tab) {
                // Its Home, because this channel has no such tab: the first tab it has, same history entry.
                Some(go) if go != want => {
                    self.chan_redirect(go);
                    None
                }
                // No tab the client renders at all: what YouTube sent, without a tab row.
                _ => Some(want),
            },
        }
    }

    /// The route and this history entry become `tab`.
    fn chan_retarget(&mut self, tab: ChannelTab) {
        let Route::Channel { key, path, tab: was } = self.route.clone() else { return };
        self.url = format!("{path}/{}", tab.slug());
        bridge::replace(&self.url);
        self.route = Route::Channel { key, path, tab };
        if self.chan.shell {
            self.chan_mark(was, tab, false);
            bridge::attr(LIST, "class", render::grid_class(tab));
        }
    }

    fn chan_redirect(&mut self, tab: ChannelTab) {
        self.chan_retarget(tab);
        if self.chan.shell {
            if self.chan_restore(tab.slug(), true) {
                return;
            }
            self.chan.inplace = true;
        }
        self.load();
    }

    /// The tab on screen pressed again: its first cards.
    pub(super) fn chan_top(&mut self) {
        let y = bridge::scroll_y();
        bridge::scroll_to(y.min(self.chan_list_top(y)));
    }

    /// A sort chip. The chip row and the pill answer at once; the list comes from memory or YouTube's own token.
    pub(super) fn chan_sort(&mut self, i: usize, kbd: bool) -> bool {
        let Route::Channel { tab, .. } = self.route else { return false };
        let Some(mut head) = self.feed.head.clone() else { return false };
        let Some(nav) = head.nav_mut() else { return false };
        let Some(opt) = nav.sorts.get(i).cloned() else { return true };
        let old = nav.sorts.iter().position(|o| o.selected).unwrap_or(0);
        if opt.selected || (opt.token.is_none() && opt.params.is_none()) {
            return true;
        }
        // Another sort still loading: this press replaces it, and the list to fall back on stays the last one shown.
        let loading = !self.rendered;
        let back = if loading { std::mem::take(&mut self.chan.sort_back) } else { format!("{}#{}", tab.slug(), nav.sorts[old].label) };
        for (k, o) in nav.sorts.iter_mut().enumerate() {
            o.selected = k == i;
        }
        let sorts = nav.sorts.clone();
        let next = format!("{}#{}", tab.slug(), opt.label);
        // The state first, then the motion on top of it.
        self.chan_row_write(&sorts);
        if kbd || crate::tv::on() || bridge::reduced_motion() {
            bridge::pill_slide(CH_SORT, "", "", "", "", 0);
        } else {
            let (curve, ms) = motion::ui();
            bridge::pill_slide(CH_SORT, &render::ch_sort_id(old), &render::ch_sort_id(i), "fx-pg", curve, ms);
        }
        let y = bridge::scroll_y();
        let y = y.min(self.chan_list_top(y));
        self.abort_nav_requests();
        self.nav += 1;
        self.generation += 1;
        self.cont_busy = false;
        self.want = false;
        self.swr = false;
        // A refresh of the snapshot on its way or held behind the pill is of the list this press replaces.
        self.await_page = None;
        if self.held.take().is_some() {
            bridge::html(PILL, "");
        }
        if !loading {
            self.chan_park(back.clone());
        }
        self.chan.sort_back.clone_from(&back);
        self.feed = FeedState { cont_ep: "browse", head: Some(head), ..Default::default() };
        if self.chan_restore(&next, false) {
            bridge::scroll_to(y);
            return true;
        }
        self.rendered = false;
        bridge::html(LIST, &render::channel_skeleton(SKELETON_CARDS));
        bridge::attr(MORE, "class", "more done");
        bridge::scroll_to(y);
        let req = match (opt.token, opt.params, self.page_req(&self.route)) {
            (Some(token), _, _) => Req::Cont { ep: "browse", token },
            (None, Some(params), Some(Req::Browse { id, .. })) => Req::Browse { id, params: Some(params) },
            _ => {
                self.chan_sort_back(&back);
                return true;
            }
        };
        let (nav, generation) = (self.nav, self.generation);
        self.send(req, Purpose::ChanSort { nav, generation, back }, false);
        true
    }

    /// A new sort's answer: a reload of the list (the chip bar's token) or a whole page (Playlists' menu).
    pub(super) fn chan_sort_body(&mut self, status: u32, body: &[u8], back: &str) {
        let Some(json) = (status == 200).then(|| json::parse(body)).flatten() else {
            self.chan_sort_back(back);
            return;
        };
        self.learn(&json);
        let (feed, sorts) = if json.get("contents").is_some() {
            let c = parse::channel(&json);
            (c.feed, c.sorts)
        } else {
            (parse::continuation(&json), parse::channel_sorts(&json))
        };
        let Some(mut head) = self.feed.head.clone() else { return };
        if let Some(nav) = head.nav_mut().filter(|_| !sorts.is_empty()) {
            // A choice that came without its token keeps the one it had.
            let old = std::mem::replace(&mut nav.sorts, sorts);
            for o in nav.sorts.iter_mut().filter(|o| o.token.is_none() && o.params.is_none()) {
                if let Some(k) = old.iter().find(|k| k.label == o.label) {
                    o.token.clone_from(&k.token);
                    o.params.clone_from(&k.params);
                }
            }
        }
        self.chan.inplace = true;
        self.show_feed(head, feed, "browse", "Nothing here yet.", Vec::new());
        self.mark("rendered");
        self.after_paint();
    }

    /// The sort could not be loaded: the list it replaced comes back with its chip.
    fn chan_sort_back(&mut self, back: &str) {
        self.rendered = true;
        self.chan_restore(back, false);
    }

    /// Arrow keys in the tab row and the sort chips: ←/→ walk them, ↓ goes to the cards; ↑ from the first row of
    /// cards comes back to the selected tab.
    pub(super) fn chan_arrow(&mut self, key: &str) -> Option<bool> {
        if !self.chan.shell {
            return None;
        }
        let delta = if key == "ArrowLeft" { -1 } else { 1 };
        if bridge::any("#ytc-cht :focus") {
            return match key {
                "ArrowLeft" | "ArrowRight" => Some(bridge::focus_grid(CH_TABS, "a", delta)),
                "ArrowUp" => Some(true),
                _ => {
                    if bridge::any("#ytc-chs button") {
                        bridge::focus_sel(CH_SORT, "[aria-pressed=true]");
                        return Some(true);
                    }
                    None
                }
            };
        }
        if bridge::any("#ytc-chs :focus") {
            return match key {
                "ArrowLeft" | "ArrowRight" => Some(bridge::focus_grid(CH_SORT, "button", delta)),
                "ArrowUp" => {
                    bridge::focus_sel(CH_TABS, "[aria-current=page]");
                    Some(true)
                }
                _ => None,
            };
        }
        if key == "ArrowUp" && bridge::grid_row(LIST, "a.t") == 0 {
            if bridge::any("#ytc-chs button") {
                bridge::focus_sel(CH_SORT, "[aria-pressed=true]");
            } else {
                bridge::focus_sel(CH_TABS, "[aria-current=page]");
            }
            return Some(true);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{PlaylistCard, Video};

    #[test]
    fn the_shorts_tab_shows_shorts_whatever_the_switch_says() {
        let short = || Item::Video(Video { id: "shortshort1".into(), duration: "Shorts".into(), short: true, ..Default::default() });
        let shorts = crate::route::parse("/@x/shorts").unwrap();
        let mut items = vec![short()];
        assert!(admit_tab(&shorts, &mut items), "nothing else may drop them");
        assert!(!items[0].is_short());
        assert!(matches!(&items[0], Item::Video(v) if v.duration.is_empty()), "no badge: the whole tab is Shorts");
        // Other tabs and pages decide as before.
        let mut items = vec![short()];
        assert!(!admit_tab(&crate::route::parse("/@x/videos").unwrap(), &mut items));
        assert!(items[0].is_short());
        assert!(!admit_tab(&Route::Home, &mut items));
    }

    #[test]
    fn a_tab_names_its_cards() {
        let pl = || Item::Playlist(PlaylistCard { id: "PLx".into(), channel: "Playlist".into(), channel_href: Some("/@x".into()), ..Default::default() });
        for (url, kind) in [("/@x/podcasts", "Podcast"), ("/@x/courses", "Course"), ("/@x/releases", "Release"), ("/@x/shows", "Show"), ("/@x/playlists", "")] {
            let mut items = vec![pl()];
            admit_tab(&crate::route::parse(url).unwrap(), &mut items);
            assert!(matches!(&items[0], Item::Playlist(p) if p.kind == kind && p.channel.is_empty() && p.channel_href.is_none()), "{url}");
            assert!(kind.is_empty() || crate::parse::PLAYLIST_KINDS.contains(&kind));
        }
    }
}
