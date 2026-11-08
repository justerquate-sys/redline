//! What the system and other apps hand the app: Android calls the activity with a search
//! (`android.intent.action.SEARCH`, `com.google.android.gms.actions.SEARCH_ACTION`, extra `query`) or a link
//! (`android.intent.action.VIEW`). The activity is exported, so any app on the TV can send anything: nothing from
//! here reaches the WebView as it came. A link is read into its parts (a video id, a list, a channel or a handle, each
//! checked), and the page gets a path rebuilt from those parts; anything else is dropped.

/// A query as long as the search route takes (`src/route.rs`).
const MAX_QUERY: usize = 500;

#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Search(String),
    /// A path on www.youtube.com, built here from checked parts.
    Open(String),
}

/// `action`, `data` (the intent's URI) and `query` (its extra) as Android gave them; empty when absent.
pub fn parse(action: &str, data: &str, query: &str) -> Option<Incoming> {
    match action {
        "android.intent.action.SEARCH" | "com.google.android.gms.actions.SEARCH_ACTION" => clean_query(query).map(Incoming::Search),
        "android.intent.action.VIEW" => youtube_path(data).map(Incoming::Open),
        _ => None,
    }
}

/// Query text: no control characters, bounded, not empty.
pub fn clean_query(q: &str) -> Option<String> {
    let q: String = q.chars().filter(|c| !c.is_control()).take(MAX_QUERY).collect();
    let q = q.trim();
    (!q.is_empty()).then(|| q.to_string())
}

fn is_video_id(s: &str) -> bool {
    s.len() == 11 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_list_id(s: &str) -> bool {
    (2..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_channel_id(s: &str) -> bool {
    s.len() == 24 && s.starts_with("UC") && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A handle or custom channel name (`/@name`, `/c/name`, `/user/name`).
fn is_name(s: &str) -> bool {
    (1..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// A start time: `90`, `90s`, `1m30s`, `1h2m3s` → seconds.
fn seconds(t: &str) -> Option<u32> {
    if t.is_empty() || t.len() > 12 {
        return None;
    }
    let (mut total, mut n, mut digits) = (0u32, 0u32, false);
    for c in t.chars() {
        match c {
            '0'..='9' => {
                n = n.checked_mul(10)?.checked_add(c as u32 - '0' as u32)?;
                digits = true;
            }
            'h' | 'm' | 's' if digits => {
                let unit = match c {
                    'h' => 3600,
                    'm' => 60,
                    _ => 1,
                };
                total = total.checked_add(n.checked_mul(unit)?)?;
                n = 0;
                digits = false;
            }
            _ => return None,
        }
    }
    total.checked_add(n)
}

fn query_value<'a>(query: &'a str, name: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('='))
}

fn watch(video: &str, query: &str) -> Option<String> {
    if !is_video_id(video) {
        return None;
    }
    let mut path = format!("/watch?v={video}");
    if let Some(list) = query_value(query, "list").filter(|l| is_list_id(l)) {
        path.push_str("&list=");
        path.push_str(list);
    }
    if let Some(t) = query_value(query, "t").or_else(|| query_value(query, "start")).and_then(seconds).filter(|t| *t > 0) {
        path.push_str(&format!("&t={t}s"));
    }
    Some(path)
}

/// The path on www.youtube.com for a video, list or channel link. Only `https` on `youtube.com`, `www.youtube.com`,
/// `m.youtube.com` and `youtu.be`, no user name or port, and only these shapes: `/watch?v=`, `/shorts/`, `/live/`,
/// `/embed/`, `/v/`, `youtu.be/<id>`, `/playlist?list=`, `/channel/UC…`, `/@handle`, `/c/<name>`, `/user/<name>`.
pub fn youtube_path(url: &str) -> Option<String> {
    if url.len() > 2048 || url.bytes().any(|b| b <= b' ' || b == 0x7f || b == b'\\') {
        return None;
    }
    let scheme = url.get(..8)?;
    if !scheme.eq_ignore_ascii_case("https://") {
        return None;
    }
    let rest = &url[8..];
    let rest = rest.split('#').next().unwrap_or("");
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (host, tail) = rest.split_at(end);
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    let mut parts = path.trim_start_matches('/').trim_end_matches('/').split('/');
    let first = parts.next().unwrap_or("");
    let second = parts.next().unwrap_or("");
    if path.contains("//") {
        return None;
    }
    match host.to_ascii_lowercase().as_str() {
        "youtu.be" if parts.next().is_none() && second.is_empty() => watch(first, query),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" => match first {
            "watch" if second.is_empty() => watch(query_value(query, "v")?, query),
            "shorts" | "live" | "embed" | "v" => watch(second, query),
            "playlist" if second.is_empty() => query_value(query, "list").filter(|l| is_list_id(l)).map(|l| format!("/playlist?list={l}")),
            "channel" if is_channel_id(second) => Some(format!("/channel/{second}")),
            "c" | "user" if is_name(second) => Some(format!("/{first}/{second}")),
            f if f.starts_with('@') && is_name(&f[1..]) => Some(format!("/{f}")),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const V: &str = "dQw4w9WgXcQ";

    #[test]
    fn searches_from_the_system() {
        let s = |a, q| parse(a, "", q);
        assert_eq!(s("android.intent.action.SEARCH", "lofi"), Some(Incoming::Search("lofi".into())));
        assert_eq!(s("com.google.android.gms.actions.SEARCH_ACTION", "  rain\n\tsounds "), Some(Incoming::Search("rainsounds".into())));
        assert_eq!(s("android.intent.action.SEARCH", " \n "), None);
        assert_eq!(s("android.intent.action.MAIN", "lofi"), None);
        assert_eq!(clean_query(&"音".repeat(600)).unwrap().chars().count(), MAX_QUERY);
        assert_eq!(s("android.intent.action.SEARCH", "<script>\"'"), Some(Incoming::Search("<script>\"'".into())), "text stays text: the page escapes it and encodes the route");
    }

    #[test]
    fn links_become_rebuilt_paths() {
        let p = |u: &str| parse("android.intent.action.VIEW", u, "");
        let open = |s: String| Some(Incoming::Open(s));
        assert_eq!(p(&format!("https://www.youtube.com/watch?v={V}")), open(format!("/watch?v={V}")));
        assert_eq!(p(&format!("https://youtube.com/watch?feature=share&v={V}&si=abc#x")), open(format!("/watch?v={V}")));
        assert_eq!(p(&format!("HTTPS://M.YOUTUBE.COM/watch?v={V}&list=PLabc_-1&t=1m5s")), open(format!("/watch?v={V}&list=PLabc_-1&t=65s")));
        assert_eq!(p(&format!("https://youtu.be/{V}?t=90")), open(format!("/watch?v={V}&t=90s")));
        for shape in ["shorts", "live", "embed", "v"] {
            assert_eq!(p(&format!("https://www.youtube.com/{shape}/{V}/")), open(format!("/watch?v={V}")));
        }
        assert_eq!(p("https://www.youtube.com/playlist?list=PLabc"), open("/playlist?list=PLabc".into()));
        assert_eq!(p("https://www.youtube.com/channel/UCaZO-66UcqyVT9btiqdDryA"), open("/channel/UCaZO-66UcqyVT9btiqdDryA".into()));
        assert_eq!(p("https://www.youtube.com/@monkey.dev_1/videos"), open("/@monkey.dev_1".into()));
        assert_eq!(p("https://www.youtube.com/c/somebody"), open("/c/somebody".into()));
        assert_eq!(p("https://www.youtube.com/user/somebody"), open("/user/somebody".into()));
    }

    #[test]
    fn anything_else_is_dropped() {
        let p = |u: &str| youtube_path(u);
        for bad in [
            // Other schemes and hosts.
            format!("http://www.youtube.com/watch?v={V}"),
            format!("javascript:alert(1)//https://www.youtube.com/watch?v={V}"),
            format!("file:///watch?v={V}"),
            format!("https://evil.example/watch?v={V}"),
            format!("https://www.youtube.com.evil.example/watch?v={V}"),
            format!("https://notyoutube.com/watch?v={V}"),
            format!("https://music.youtube.com/watch?v={V}"),
            // A host hiding behind a user name, a port or a backslash.
            format!("https://www.youtube.com@evil.example/watch?v={V}"),
            format!("https://evil.example@www.youtube.com/watch?v={V}"),
            format!("https://www.youtube.com:8443/watch?v={V}"),
            format!("https://evil.example\\@www.youtube.com/watch?v={V}"),
            // Bad ids and shapes.
            "https://www.youtube.com/watch?v=short".to_string(),
            "https://www.youtube.com/watch?v=%3Cscript%3E12".to_string(),
            "https://www.youtube.com/watch?v=<script>abcd".to_string(),
            "https://www.youtube.com/watch".to_string(),
            format!("https://www.youtube.com/watch/extra?v={V}"),
            format!("https://youtu.be/{V}/extra"),
            "https://youtu.be/".to_string(),
            "https://www.youtube.com/".to_string(),
            "https://www.youtube.com/feed/history".to_string(),
            "https://www.youtube.com/account".to_string(),
            "https://www.youtube.com/playlist?list=a'b".to_string(),
            "https://www.youtube.com/channel/notachannel".to_string(),
            "https://www.youtube.com/@".to_string(),
            "https://www.youtube.com/@a%2F..%2F..%2Faccount".to_string(),
            "https://www.youtube.com/@a%2Fb".to_string(),
            format!("https://www.youtube.com//watch?v={V}"),
            format!("https://www.youtube.com/watch?v={V} "),
            String::new(),
            "https".to_string(),
        ] {
            assert_eq!(p(&bad), None, "{bad}");
        }
        // Extras that do not check out are left out, the video still opens.
        assert_eq!(p(&format!("https://www.youtube.com/watch?v={V}&list=a'b&t=x")), Some(format!("/watch?v={V}")));
        assert_eq!(p(&format!("https://www.youtube.com/watch?v={V}&t=99999999999")), Some(format!("/watch?v={V}")));
        assert_eq!(parse("android.intent.action.VIEW", "https://evil.example/", "lofi"), None);
    }

    #[test]
    fn start_times() {
        assert_eq!(seconds("90"), Some(90));
        assert_eq!(seconds("90s"), Some(90));
        assert_eq!(seconds("1h2m3s"), Some(3723));
        assert_eq!(seconds("m"), None);
        assert_eq!(seconds("1x"), None);
        assert_eq!(seconds(""), None);
    }
}
