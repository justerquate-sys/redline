//! Search text entry with the TV remote. The query is a button and the backing input is hidden on
//! TV, so existing shells cannot swallow OK to open an unavailable system keyboard. Suggestions,
//! history and submission all use the same app paths as typing in the desktop field.

use super::App;
use crate::tv::keyboard::{self, Edit, ID, LABEL, QUERY};
use crate::{bridge, tv};

impl App {
    pub(super) fn tv_keyboard_open(&mut self) {
        self.tv.keyboard.typed = false;
        bridge::attr(ID, "aria-hidden", "false");
        bridge::attr(ID, "class", "tvk on");
        bridge::attr("ytc", "data-tvk", "1");
        bridge::attr(QUERY, "aria-expanded", "true");
        self.tv_keyboard_label();
        self.on_focus("q", "", true);
        bridge::focus_sel(ID, ".tvk-keys button");
    }

    pub(super) fn tv_keyboard_label(&self) {
        if tv::on() {
            let query = bridge::input_value("ytc-q");
            bridge::html(
                LABEL,
                &crate::util::esc(if query.is_empty() {
                    "Search videos"
                } else {
                    &query
                }),
            );
            bridge::attr(
                QUERY,
                "aria-label",
                if query.is_empty() {
                    "Search videos"
                } else {
                    "Edit search"
                },
            );
        }
    }

    pub(super) fn tv_keyboard_hide(&mut self) {
        self.tv.keyboard.typed = false;
        bridge::attr(ID, "class", "tvk");
        bridge::attr(ID, "aria-hidden", "true");
        bridge::attr("ytc", "data-tvk", "");
        bridge::attr(QUERY, "aria-expanded", "false");
    }

    /// History or another route can leave Search without submitting it. Its fixed header survives
    /// page swaps, so close it before the route is saved and let the destination choose its focus.
    pub(super) fn tv_keyboard_leave(&mut self) {
        if self.tv_search_on() {
            self.tv_search_close(false);
            bridge::blur();
        }
    }

    pub(super) fn tv_keyboard_back(&mut self) -> bool {
        if !self.tv_search_on() || !bridge::any("#ytc[data-tvk='1']") {
            return false;
        }
        self.tv_keyboard_hide();
        bridge::focus(QUERY);
        true
    }

    fn tv_keyboard_edit(&mut self, edit: Edit<'_>) {
        let query = keyboard::edit(&bridge::input_value("ytc-q"), edit);
        bridge::value("ytc-q", &query);
        self.tv_keyboard_label();
        self.on_input("q", &query, "insertText");
    }

    pub(super) fn tv_keyboard_click(&mut self, act: &str, arg: &str) -> bool {
        if !tv::on() || !self.tv_search_on() {
            return false;
        }
        self.tv.keyboard.typed = false;
        match act {
            "tv-kb-key" if keyboard::printable(arg) => self.tv_keyboard_edit(Edit::Insert(arg)),
            "tv-kb-delete" => self.tv_keyboard_edit(Edit::Delete),
            "tv-kb-clear" => self.tv_keyboard_edit(Edit::Clear),
            "tv-kb-layout" => {
                self.tv.keyboard.symbols = !self.tv.keyboard.symbols;
                bridge::html(ID, &keyboard::keys(self.tv.keyboard.symbols));
                bridge::focus_sel(ID, "[data-act=tv-kb-layout]");
            }
            "tv-kb-search" => {
                let query = bridge::input_value("ytc-q");
                self.on_submit("ytc-search", &query);
            }
            _ => return false,
        }
        true
    }

    /// Printable physical keys still enter text while the remote's buttons have focus. Arrows end
    /// physical typing, making OK select the next focused key. Enter after actual typing submits.
    pub(super) fn tv_keyboard_key(&mut self, key: &str, editable: bool) -> Option<bool> {
        if !self.tv_search_on() || editable {
            return None;
        }
        if let Some(dir) = tv::dir(key) {
            self.tv.keyboard.typed = false;
            if dir == tv::Dir::Down && bridge::any("#ytc-tvquery:focus,#ytc-tvmic:focus") {
                if bridge::any("#ytc[data-tvk='1']") {
                    bridge::focus_sel(ID, ".tvk-keys button");
                } else {
                    bridge::focus_sel(crate::render::SUGGEST, ".sg a");
                }
                return Some(true);
            }
            self.tv_move_in("#ytc-top", dir);
            return Some(true);
        }
        match key {
            "Tab" | "Shift+Tab" => {
                self.tv.keyboard.typed = false;
                let flat = bridge::spatial_rects(
                    "#ytc-top",
                    "button:not([disabled]):not([tabindex='-1']),a[href]:not([tabindex='-1'])",
                    "",
                    "",
                    "",
                );
                if let Some((rects, _, current)) = super::tvglue::split(&flat) {
                    if let Some(next) = keyboard::tab_stop(rects.len(), current, key == "Shift+Tab")
                    {
                        bridge::spatial_focus(next as u32, -1.0);
                    }
                }
                Some(true)
            }
            "Escape" | "BrowserBack" => {
                if !self.tv_keyboard_back() {
                    self.tv_search_close(true);
                }
                Some(true)
            }
            "Enter" if self.tv.keyboard.typed => {
                let query = bridge::input_value("ytc-q");
                self.on_submit("ytc-search", &query);
                Some(true)
            }
            "Enter" => Some(false),
            "Backspace" | "Delete" => {
                self.tv.keyboard.typed = true;
                self.tv_keyboard_edit(Edit::Delete);
                Some(true)
            }
            k if keyboard::printable(k) => {
                self.tv.keyboard.typed = true;
                self.tv_keyboard_edit(Edit::Insert(k));
                Some(true)
            }
            _ => None,
        }
    }
}
