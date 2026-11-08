//! The TV search keyboard. Its labels and bounded edits are Rust-owned; the platform keyboard is
//! not required, so the same remote path also works in older TV shells.

use crate::util::esc_into;

pub const MAX_CHARS: usize = 500;
pub const ID: &str = "ytc-tvk";
pub const QUERY: &str = "ytc-tvquery";
pub const LABEL: &str = "ytc-tvquery-text";
/// The mic control beside the query; shown only while the shell has a recognizer (`html[data-ytc-tvmic]`), and never on
/// the desktop extension (whose own `#ytc-mic` is another thing altogether).
pub const MIC: &str = "ytc-tvmic";
pub const MIC_ROOT: &str = "data-ytc-tvmic";
const LETTERS: &str = "abcdefghijklmnopqrstuvwxyz'-.,";
const SYMBOLS: &str = "0123456789@#&+!?/():;\"=_[]%€$£";

#[derive(Default)]
pub struct State {
    pub symbols: bool,
    /// Actual typing makes Enter submit; a D-pad move returns Enter to native button activation.
    pub typed: bool,
}

pub enum Edit<'a> {
    Insert(&'a str),
    Delete,
    Clear,
}

/// Edits by Unicode scalar, never by byte offsets. Existing queries and pasted text are bounded
/// before editing, just like the search route; control characters are not query text.
pub fn edit(query: &str, edit: Edit<'_>) -> String {
    let mut out: String = query
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_CHARS)
        .collect();
    match edit {
        Edit::Insert(s) => {
            let left = MAX_CHARS.saturating_sub(out.chars().count());
            out.extend(s.chars().filter(|c| !c.is_control()).take(left));
        }
        Edit::Delete => {
            out.pop();
        }
        Edit::Clear => out.clear(),
    }
    out
}

pub fn printable(key: &str) -> bool {
    let mut c = key.chars();
    c.next().is_some_and(|c| !c.is_control()) && c.next().is_none()
}

/// Tab stays inside the visible Search controls and wraps in document order. The bridge already
/// excluded hidden controls; missing focus starts at the edge for the requested direction.
pub fn tab_stop(count: usize, current: Option<usize>, reverse: bool) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let Some(current) = current.filter(|&i| i < count) else {
        return Some(if reverse { count - 1 } else { 0 });
    };
    Some(if reverse {
        if current == 0 { count - 1 } else { current - 1 }
    } else if current + 1 == count {
        0
    } else {
        current + 1
    })
}

fn key(out: &mut String, act: &str, arg: &str, label: &str, class: &str) {
    out.push_str("<button type=\"button\" class=\"tvk-key ");
    out.push_str(class);
    out.push_str("\" data-act=\"");
    out.push_str(act);
    out.push_str("\" data-arg=\"");
    esc_into(out, arg);
    out.push_str("\">");
    esc_into(out, label);
    out.push_str("</button>");
}

pub fn keys(symbols: bool) -> String {
    let mut out = String::with_capacity(4500);
    out.push_str("<div class=\"tvk-keys\" role=\"group\" aria-label=\"Search characters\">");
    for c in if symbols { SYMBOLS } else { LETTERS }.chars() {
        let s = c.to_string();
        key(&mut out, "tv-kb-key", &s, &s, "");
    }
    out.push_str("</div><div class=\"tvk-tools\" role=\"group\" aria-label=\"Search editing\">");
    key(&mut out, "tv-kb-key", " ", "Space", "tvk-space");
    key(&mut out, "tv-kb-delete", "", "Delete", "");
    key(&mut out, "tv-kb-clear", "", "Clear", "");
    key(
        &mut out,
        "tv-kb-layout",
        "",
        if symbols { "ABC" } else { "123 & symbols" },
        "tvk-layout",
    );
    key(&mut out, "tv-kb-search", "", "Search", "tvk-search");
    out.push_str("</div><p class=\"tvk-hint\">Arrows move · OK selects · Back closes keyboard</p>");
    out
}

pub fn shell() -> String {
    format!(
        "<div id=\"{ID}\" class=\"tvk\" role=\"group\" aria-label=\"On-screen keyboard\" aria-hidden=\"true\">{}</div>",
        keys(false)
    )
}

pub fn query(q: &str) -> String {
    let mut out = format!(
        "<button id=\"{QUERY}\" class=\"tvquery\" type=\"button\" data-act=\"tv-search\" data-focus=\"q\" aria-label=\"Edit search\" aria-controls=\"{ID}\" aria-expanded=\"false\"><span id=\"{LABEL}\">"
    );
    esc_into(&mut out, if q.is_empty() { "Search videos" } else { q });
    out.push_str("</span><span class=\"tvquery-edit\" aria-hidden=\"true\">Edit</span></button>");
    out.push_str(&format!("<button id=\"{MIC}\" class=\"tvmic\" type=\"button\" data-act=\"tv-mic\" aria-label=\"Voice search\"><svg viewBox=\"0 0 24 24\" aria-hidden=\"true\"><rect x=\"9\" y=\"3\" width=\"6\" height=\"11.5\" rx=\"3\"/><path d=\"M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21\"/></svg></button>"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_are_unicode_safe_and_bounded() {
        assert_eq!(edit("café 🦀", Edit::Delete), "café ");
        assert_eq!(edit("東京", Edit::Insert("音楽")), "東京音楽");
        assert_eq!(edit("x", Edit::Insert("\n\t y\0")), "x y");
        assert_eq!(edit("", Edit::Delete), "");
        assert_eq!(edit("abc", Edit::Clear), "");
        let full = "音".repeat(MAX_CHARS);
        assert_eq!(edit(&full, Edit::Insert("x")), full);
        assert_eq!(
            edit(&"a".repeat(MAX_CHARS + 3), Edit::Delete)
                .chars()
                .count(),
            MAX_CHARS - 1
        );
    }

    #[test]
    fn layouts_have_six_columns_and_escape_symbols() {
        assert_eq!(LETTERS.chars().count(), 30);
        assert_eq!(SYMBOLS.chars().count(), 30);
        let html = keys(true);
        assert_eq!(html.matches("data-act=\"tv-kb-key\"").count(), 31);
        assert!(html.contains("data-arg=\"&amp;\""));
        assert!(html.contains("data-arg=\"&quot;\""));
        assert!(query("<tag>").contains("&lt;tag&gt;"));
        assert!(shell().contains("aria-hidden=\"true\""));
    }

    #[test]
    fn physical_text_excludes_named_keys_and_controls() {
        for key in [" ", "A", "é", "音", "🦀"] {
            assert!(printable(key));
        }
        for key in ["", "Enter", "Backspace", "ArrowDown", "\n", "\0"] {
            assert!(!printable(key));
        }
    }

    #[test]
    fn tab_wraps_without_leaving_search() {
        assert_eq!(tab_stop(0, None, false), None);
        assert_eq!(tab_stop(1, Some(0), false), Some(0));
        assert_eq!(tab_stop(1, Some(0), true), Some(0));
        assert_eq!(tab_stop(4, Some(3), false), Some(0));
        assert_eq!(tab_stop(4, Some(0), true), Some(3));
        assert_eq!(tab_stop(4, Some(1), false), Some(2));
        assert_eq!(tab_stop(4, Some(1), true), Some(0));
        assert_eq!(tab_stop(4, None, true), Some(3));
        assert_eq!(tab_stop(4, Some(99), false), Some(0));
    }
}
