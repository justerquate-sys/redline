//! TV watch drawers reuse the existing watch data and comment state. Opening a drawer is the
//! only TV path that starts comment requests; closing it leaves playback and loaded pages intact.

use super::App;
use crate::{
    actions, bridge,
    parse::Watch,
    render,
    tv::{self, Dir, Rect},
};

const INFO: &str = "ytc-tvinfo";
const INFO_BODY: &str = "ytc-tvinfo-body";
const COMMENTS: &str = render::COMMENTS;
const SAVE: &str = actions::TV_SAVE;
const STOPS: &str = "a[href]:not([tabindex='-1']),button:not([disabled]):not([tabindex='-1']),[tabindex='0']";
// Enter the text before its inline links: a short nested link otherwise beats its tall reader
// in directional scoring. Once inside the reader, Right/Left can enter and leave those links.
const READER_SKIP: &str = ":is(.yct,.tvd):not(:focus-within) a[href]";
const READER_MARKS: &str = ":focus a[href]|:is(.yct,.tvd):has(:focus)";
// Reply continuation buttons hide while loading. They can retain DOM focus until the next frame;
// ancestry alone would mistake that hidden button for a usable focus stop.
const FOCUSED_STOP: &str = ":focus:not([hidden]):not([hidden] *):not(.hide):not(.hide *)";

/// A long focused comment/description is read to its edge before the next arrow changes focus.
fn read_before_move(item: Rect, view: Rect, dir: Dir) -> bool {
    item.h > view.h * 0.7
        && match dir {
            Dir::Down => item.y + item.h > view.y + view.h + 1.0,
            Dir::Up => item.y < view.y - 1.0,
            _ => false,
        }
}

/// Right enters an inline link in the current reader. Prefer the part being read now, rather
/// than jumping back to the first link of a long description. Left returns to the reader.
fn reader_link(rects: &[Rect], marks: &[u8], view: Option<Rect>) -> Option<usize> {
    let links = || marks.iter().enumerate().filter_map(|(i, m)| (*m == 1).then_some(i));
    let Some(view) = view else { return links().next() };
    links().find(|&i| rects[i].y + rects[i].h > view.y && rects[i].y < view.y + view.h).or_else(|| {
        links().min_by(|&a, &b| {
            let distance = |r: Rect| (r.y - (view.y + view.h)).max(view.y - (r.y + r.h)).max(0.0);
            distance(rects[a]).total_cmp(&distance(rects[b]))
        })
    })
}

impl App {
    /// Both a fresh response and Back/Forward restore supply the same details.
    pub(super) fn tv_watch_data(&mut self, w: &Watch, v: &str) {
        if !tv::on() {
            return;
        }
        self.tv.info = render::tv_info(w, v);
        bridge::html(crate::player::ui::TVCH, &render::tv_channel(w));
        if bridge::any("#ytc-tvinfo.on") {
            bridge::html(INFO, &self.tv.info);
            self.acts.tv_info_refresh();
        }
        if bridge::any("#ytc-comments.on") {
            self.tv_comments_load();
        }
        self.tv_drawer_focus();
    }

    pub(super) fn tv_info_open(&mut self) {
        if bridge::any("#ytc-tvinfo.on") {
            self.tv_drawer_focus();
            return;
        }
        bridge::focus_save("info");
        let html = if self.tv.info.is_empty() {
            "<header class=\"tvih\"><button type=\"button\" class=\"tvb tvx\" data-act=\"tv-info-close\">Back to video</button></header><div id=\"ytc-tvinfo-body\" class=\"tvib\"><h2>More info</h2><p class=\"tvd\" tabindex=\"0\">Loading video details…</p></div>"
        } else {
            &self.tv.info
        };
        bridge::html(INFO, html);
        self.acts.tv_info_refresh();
        bridge::attr(INFO, "class", "tvi on");
        bridge::focus_sel(INFO, ".tvd");
        // Long titles/metadata may put the description below the reader. The generic focus helper
        // deliberately prevents scrolling, so bring this initial stop into its own scrolling body.
        let flat = bridge::spatial_rects("#ytc-tvinfo", STOPS, "", "", "");
        if let Some((_, _, Some(cur))) = super::tvglue::split(&flat) {
            bridge::spatial_focus(cur as u32, -1.0);
        }
    }

    pub(super) fn tv_info_close(&mut self) {
        if !bridge::any("#ytc-tvinfo.on") {
            return;
        }
        bridge::attr(INFO, "class", "tvi");
        if !bridge::focus_load("info") {
            bridge::focus("ytc-seek");
        }
        if let Some(p) = self.player.as_mut() {
            p.wake();
        }
    }

    pub(super) fn tv_save_open(&mut self) {
        if !bridge::any("#ytc-tvsave.on") {
            bridge::focus_save("save");
        }
        bridge::attr(SAVE, "class", "tvi on");
        bridge::focus_sel(SAVE, "[data-act=tv-save-close]");
        self.acts.tv_save_open(&actions::Net { cfg: &self.cfg, hl: &self.hl, url: &self.url });
        self.tv_drawer_focus();
    }

    pub(super) fn tv_save_close(&mut self) {
        if !bridge::any("#ytc-tvsave.on") {
            return;
        }
        self.acts.tv_save_close();
        bridge::attr(SAVE, "class", "tvi");
        if !bridge::focus_load("save") {
            bridge::focus("ytc-seek");
        }
        if let Some(p) = self.player.as_mut() {
            p.wake();
        }
    }

    pub(super) fn tv_comments_open(&mut self) {
        if bridge::any("#ytc-comments.on") {
            self.tv_drawer_focus();
            return;
        }
        bridge::focus_save("comments");
        bridge::attr(COMMENTS, "class", "cmts tvcm on");
        // The header stays put while pages and replies change inside the scrolling body.
        bridge::focus_sel(COMMENTS, ".yc-sort button.on,[data-act=tv-comments-close]");
        self.tv_comments_load();
    }

    fn tv_comments_load(&mut self) {
        self.acts.tv_comments_open(&actions::Net {
            cfg: &self.cfg,
            hl: &self.hl,
            url: &self.url,
        });
    }

    pub(super) fn tv_comments_close(&mut self) {
        if !bridge::any("#ytc-comments.on") {
            return;
        }
        bridge::attr(COMMENTS, "class", "cmts tvcm");
        if !bridge::focus_load("comments") {
            bridge::focus("ytc-seek");
        }
        if let Some(p) = self.player.as_mut() {
            p.wake();
        }
    }

    /// Called before saving/leaving a route. No open drawer leaks into another page or its snapshot.
    pub(super) fn tv_drawers_reset(&mut self) {
        if !tv::on() {
            return;
        }
        bridge::attr(COMMENTS, "class", "cmts tvcm");
        bridge::attr(INFO, "class", "tvi");
        bridge::html(INFO, "");
        self.tv.info.clear();
        bridge::attr(SAVE, "class", "tvi");
        bridge::html(actions::TV_SAVE_BODY, "");
    }

    pub(super) fn tv_drawer_back(&mut self) -> bool {
        if bridge::any("#ytc-comments.on") {
            self.tv_comments_close();
            true
        } else if bridge::any("#ytc-tvinfo.on") {
            self.tv_info_close();
            true
        } else if bridge::any("#ytc-tvsave.on") {
            self.tv_save_close();
            true
        } else {
            false
        }
    }

    fn tv_drawer(&self) -> Option<(&'static str, &'static str)> {
        if !tv::on() || bridge::any("#ytc-tvsign.on,#ytc-tvabout.on,#ytc[data-tvq='1']") {
            None
        } else if bridge::any("#ytc-comments.on") {
            Some((COMMENTS, actions::TV_CM_BODY))
        } else if bridge::any("#ytc-tvinfo.on") {
            Some((INFO, INFO_BODY))
        } else if bridge::any("#ytc-tvsave.on") {
            Some((SAVE, actions::TV_SAVE_BODY))
        } else {
            None
        }
    }

    /// Async comment frames/replies may replace the focused row. Keep the remote in its open drawer.
    pub(super) fn tv_drawer_focus(&mut self) -> bool {
        let Some((id, _)) = self.tv_drawer() else { return false };
        if !bridge::any(&format!("#{id} {FOCUSED_STOP}")) {
            bridge::focus_sel(id, "[data-act=tv-comments-close],[data-act=tv-info-close],[data-act=tv-save-close]");
        }
        true
    }

    /// Scope all arrows to the drawer; ordinary button/link activation stays native. A key in a
    /// reader must never fall through to the player (OK on comment text is not play/pause).
    pub(super) fn tv_drawer_key(&mut self, key: &str, dir: Option<Dir>) -> Option<bool> {
        let (id, scroller) = self.tv_drawer()?;
        if key == "Escape" {
            return Some(self.tv_drawer_back());
        }
        self.tv_drawer_focus();
        let Some(dir) = dir else {
            return Some(matches!(key, "Enter" | " ") && !bridge::any(&format!("#{id} :is(a,button):focus")));
        };
        let flat = bridge::spatial_rects(&format!("#{id}"), STOPS, READER_SKIP, READER_MARKS, "");
        let Some((rects, marks, Some(cur))) = super::tvglue::split(&flat) else {
            return Some(true);
        };
        let bounds = bridge::rect(scroller);
        let view = (bounds.len() == 4).then(|| Rect { x: bounds[0], y: bounds[1], w: bounds[2], h: bounds[3] });
        // Spatial edge comparisons cannot enter a link wholly inside a focused text box.
        let inside = match dir {
            Dir::Right => reader_link(&rects, &marks, view),
            Dir::Left => marks.iter().position(|m| *m == 2),
            _ => None,
        };
        if let Some(i) = inside {
            bridge::spatial_focus(i as u32, -1.0);
            return Some(true);
        }
        if let Some(view) = view.filter(|_| bridge::any(&format!("#{id} :is(.yct,.tvd):focus"))) {
            if read_before_move(rects[cur], view, dir) {
                bridge::scroll_box(scroller, if dir == Dir::Up { -0.55 } else { 0.55 });
                return Some(true);
            }
        }
        // More info: the first ↑ from the description reaches View channel, whatever sits beside it in that row.
        if dir == Dir::Up && bridge::any("#ytc-tvinfo .tvd:focus") {
            let flat = bridge::spatial_rects(&format!("#{id}"), STOPS, READER_SKIP, ".tvia a", "");
            if let Some(i) = super::tvglue::split(&flat).and_then(|(_, marks, _)| marks.iter().position(|m| *m == 1)) {
                bridge::spatial_focus(i as u32, -1.0);
                return Some(true);
            }
        }
        if let Some(i) = tv::pick(rects[cur], &rects, dir, Some(cur)) {
            bridge::spatial_focus(i as u32, -1.0);
        } else if matches!(dir, Dir::Up | Dir::Down) {
            bridge::scroll_box(scroller, if dir == Dir::Up { -0.3 } else { 0.3 });
        }
        Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_readers_scroll_before_focus_leaves() {
        let view = Rect {
            x: 0.0,
            y: 100.0,
            w: 400.0,
            h: 300.0,
        };
        let long = Rect {
            x: 10.0,
            y: 120.0,
            w: 380.0,
            h: 600.0,
        };
        assert!(read_before_move(long, view, Dir::Down));
        assert!(!read_before_move(long, view, Dir::Up));
        assert!(!read_before_move(long, view, Dir::Right));
        let bottom = Rect { y: -200.0, ..long };
        assert!(!read_before_move(bottom, view, Dir::Down));
        assert!(read_before_move(bottom, view, Dir::Up));
        assert!(!read_before_move(Rect { h: 80.0, y: 380.0, ..long }, view, Dir::Down));
    }

    #[test]
    fn comment_text_keeps_its_reply_action_in_the_directional_path() {
        let thread = Rect { x: 0.0, y: 0.0, w: 400.0, h: 1600.0 };
        let text = Rect { x: 48.0, y: 40.0, w: 344.0, h: 600.0 };
        let reply = Rect { x: 48.0, y: 656.0, w: 120.0, h: 40.0 };
        let nested = Rect { x: 80.0, y: 720.0, w: 310.0, h: 700.0 };
        let after = Rect { x: 48.0, y: 1640.0, w: 344.0, h: 80.0 };
        let candidates = [text, reply, nested, after];
        // A focus stop wrapping the whole thread skips all contained reply controls.
        assert_eq!(tv::pick(thread, &candidates, Dir::Down, None), Some(3));
        assert_eq!(tv::pick(text, &candidates, Dir::Down, Some(0)), Some(1));
        assert_eq!(tv::pick(reply, &candidates, Dir::Down, Some(1)), Some(2));
        assert_eq!(tv::pick(nested, &candidates, Dir::Up, Some(2)), Some(1));
        assert_eq!(tv::pick(reply, &candidates, Dir::Up, Some(1)), Some(0));
    }

    #[test]
    fn entering_inline_links_prefers_the_visible_reading_position() {
        let view = Rect { x: 0.0, y: 100.0, w: 400.0, h: 300.0 };
        let rects = [Rect { y: -300.0, h: 20.0, ..view }, Rect { y: 180.0, h: 20.0, ..view }, Rect { y: 700.0, h: 20.0, ..view }];
        assert_eq!(reader_link(&rects, &[1, 1, 1], Some(view)), Some(1));
        assert_eq!(reader_link(&rects, &[1, 0, 1], Some(view)), Some(2));
        assert_eq!(reader_link(&rects, &[0, 0, 0], Some(view)), None);
    }
}
