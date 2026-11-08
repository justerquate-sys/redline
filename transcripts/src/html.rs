//! HTML strings: every value that did not come from this crate goes through `esc`.

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// One icon set: 16 px line icons on a 24-unit box, stroke from the text role (client.css `.tx-i`).
pub fn icon(d: &str) -> String {
    format!("<svg class=\"tx-i\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"{d}\"/></svg>")
}

pub const I_FOLDER: &str = "M3 18V6a2 2 0 0 1 2-2h4.5a2 2 0 0 1 1.7 1l.9 1.5a2 2 0 0 0 1.7 1H19a2 2 0 0 1 2 2V18a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z";
pub const I_FOLDERS: &str = "M6 5V4a1 1 0 0 1 1-1h4l1.5 2H19a2 2 0 0 1 2 2v7M3 19V9a2 2 0 0 1 2-2h4l2 2.5h6a2 2 0 0 1 2 2V19a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z";
pub const I_COPY: &str = "M9 9h10a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H9a1 1 0 0 1-1-1V10a1 1 0 0 1 1-1ZM16 9V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3";
pub const I_CHECK: &str = "m5 12.5 4.5 4.5L19 7.5";
pub const I_ALERT: &str = "M12 7.5v5.5M12 16.5v.01M12 3.5a8.5 8.5 0 1 0 0 17 8.5 8.5 0 0 0 0-17Z";
pub const I_PLUS: &str = "M12 5v14M5 12h14";
pub const I_STOP: &str = "M7 7h10v10H7Z";
pub const I_OPEN: &str = "M8 16 18 6M9 6h9v9";
pub const I_BACK: &str = "m14 6-6 6 6 6";

pub const I_REFRESH: &str = "M20 12a8 8 0 1 1-2.5-5.8M20 4v4.5h-4.5";
pub const I_SETTINGS: &str = "M4 7.5h9m4 0h3M4 16.5h3m4 0h9M15 5.5v4M9 14.5v4";

#[cfg(test)]
mod tests {
    #[test]
    fn hostile_text_is_inert() {
        assert_eq!(super::esc("<img src=x onerror=\"a\">'&"), "&lt;img src=x onerror=&quot;a&quot;&gt;&#39;&amp;");
    }
}
