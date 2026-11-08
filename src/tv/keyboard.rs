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

/// One keyboard layout, as data: another alphabet is one more row in `LAYOUTS`.
pub struct Layout {
    pub id: &'static str,
    /// What the language key says while this layout comes next.
    pub name: &'static str,
    /// The keys in reading order. Every layout fills the same grid (six columns × six rows, `KEYS`), so the
    /// editing keys under it never move when the layout changes.
    pub keys: &'static str,
    /// Device languages that start on this layout.
    pub locales: &'static [&'static str],
    /// In everyone's cycle; otherwise only on a device in one of `locales`.
    pub always: bool,
}

pub const KEYS: usize = 36;

/// The cycle's order. a–z also carries the German and Spanish letters; Russian and Ukrainian follow ЙЦУКЕН row by row.
pub const LAYOUTS: &[Layout] = &[
    Layout { id: "abc", name: "ABC", keys: "abcdefghijklmnopqrstuvwxyz'-.,äöüßñé", locales: &[], always: true },
    Layout { id: "ru", name: "Русский", keys: "йцукенгшщзхъфывапролджэячсмитьбюё-'.", locales: &["ru"], always: true },
    Layout { id: "uk", name: "Українська", keys: "йцукенгшщзхїфівапролджєячсмитьбюґ'-.", locales: &["uk"], always: false },
    Layout { id: "sym", name: "123 & symbols", keys: "0123456789@#&+!?/():;\"=_[]%€$£*'~№«»", locales: &[], always: true },
];

fn primary(locale: &str) -> String {
    locale.split(['-', '_']).next().unwrap_or("").to_ascii_lowercase()
}

fn layout(id: &str) -> &'static Layout {
    LAYOUTS.iter().find(|l| l.id == id).unwrap_or(&LAYOUTS[0])
}

/// The layouts the language key walks on a device in `locale`.
pub fn cycle(locale: &str) -> Vec<&'static Layout> {
    let lang = primary(locale);
    LAYOUTS.iter().filter(|l| l.always || l.locales.contains(&lang.as_str())).collect()
}

/// The layout after `id`, wrapping around.
pub fn next(id: &str, locale: &str) -> &'static str {
    let c = cycle(locale);
    let at = c.iter().position(|l| l.id == id);
    c[at.map_or(0, |i| (i + 1) % c.len())].id
}

/// The layout to start on: the one remembered, else the device language's, else a–z.
pub fn initial(stored: &str, locale: &str) -> &'static str {
    let lang = primary(locale);
    let c = cycle(locale);
    c.iter().find(|l| l.id == stored).or_else(|| c.iter().find(|l| l.locales.contains(&lang.as_str()))).map_or(LAYOUTS[0].id, |l| l.id)
}

#[derive(Default)]
pub struct State {
    /// The layout on screen; empty until the keyboard first opens (`initial`).
    pub layout: &'static str,
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

pub fn keys(id: &str, locale: &str) -> String {
    let mut out = String::with_capacity(5400);
    out.push_str("<div class=\"tvk-keys\" role=\"group\" aria-label=\"Search characters\">");
    for c in layout(id).keys.chars() {
        let s = c.to_string();
        key(&mut out, "tv-kb-key", &s, &s, "");
    }
    out.push_str("</div><div class=\"tvk-tools\" role=\"group\" aria-label=\"Search editing\">");
    key(&mut out, "tv-kb-key", " ", "Space", "tvk-space");
    key(&mut out, "tv-kb-delete", "", "Delete", "");
    key(&mut out, "tv-kb-clear", "", "Clear", "");
    key(&mut out, "tv-kb-layout", "", layout(next(id, locale)).name, "tvk-layout");
    key(&mut out, "tv-kb-search", "", "Search", "tvk-search");
    out.push_str("</div><p class=\"tvk-hint\">Arrows move · OK selects · Back closes keyboard</p>");
    out
}

pub fn shell() -> String {
    format!(
        "<div id=\"{ID}\" class=\"tvk\" role=\"group\" aria-label=\"On-screen keyboard\" aria-hidden=\"true\">{}</div>",
        keys(LAYOUTS[0].id, "")
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
        for l in LAYOUTS {
            assert_eq!(l.keys.chars().count(), KEYS, "{}: six columns, six rows", l.id);
            let mut seen: Vec<char> = l.keys.chars().collect();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), KEYS, "{}: no key twice", l.id);
        }
        let html = keys("sym", "en");
        assert_eq!(html.matches("data-act=\"tv-kb-key\"").count(), KEYS + 1);
        assert!(html.contains("data-arg=\"&amp;\""));
        assert!(html.contains("data-arg=\"&quot;\""));
        assert!(query("<tag>").contains("&lt;tag&gt;"));
        assert!(shell().contains("aria-hidden=\"true\""));
    }

    #[test]
    fn every_alphabet_is_whole() {
        let has = |id: &str, letters: &str| letters.chars().all(|c| layout(id).keys.contains(c));
        assert!(has("abc", "abcdefghijklmnopqrstuvwxyz"));
        let ru = "абвгдеёжзийклмнопрстуфхцчшщъыьэюя";
        assert_eq!(ru.chars().count(), 33);
        assert!(has("ru", ru));
        assert!(layout("ru").keys.starts_with("йцукен"), "ЙЦУКЕН order");
        assert!(has("uk", "абвгґдеєжзиіїйклмнопрстуфхцчшщьюя"));
        assert!(has("abc", "äöüßñé"));
    }

    #[test]
    fn the_language_key_cycles_and_the_device_language_picks_the_start() {
        let ids = |l: &str| cycle(l).iter().map(|x| x.id).collect::<Vec<_>>();
        assert_eq!(ids("en-US"), ["abc", "ru", "sym"]);
        assert_eq!(ids("ru-RU"), ["abc", "ru", "sym"]);
        assert_eq!(ids("uk"), ["abc", "ru", "uk", "sym"], "Ukrainian on a Ukrainian device only");
        assert_eq!(next("abc", "en"), "ru");
        assert_eq!(next("ru", "en"), "sym");
        assert_eq!(next("sym", "en"), "abc");
        assert_eq!(next("ru", "uk-UA"), "uk");
        assert_eq!(next("uk", "en"), "abc", "a layout outside the cycle goes back to its start");
        assert_eq!(initial("", "ru-RU"), "ru");
        assert_eq!(initial("", "ru_RU"), "ru");
        assert_eq!(initial("", "uk-UA"), "uk");
        assert_eq!(initial("", "en-US"), "abc");
        assert_eq!(initial("", ""), "abc");
        assert_eq!(initial("sym", "ru-RU"), "sym", "remembered beats the device language");
        assert_eq!(initial("abc", "ru-RU"), "abc");
        assert_eq!(initial("bogus", "ru-RU"), "ru");
        assert_eq!(initial("uk", "en-US"), "abc", "a remembered layout outside this device's cycle");
        // The language key names what comes next.
        assert!(keys("abc", "en").contains(">Русский</button>"));
        assert!(keys("ru", "en").contains(">123 &amp; symbols</button>"));
        assert!(keys("sym", "en").contains(">ABC</button>"));
        assert!(keys("ru", "en").contains("data-arg=\"ё\""));
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
