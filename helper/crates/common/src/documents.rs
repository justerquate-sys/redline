use crate::{Error, MODEL, Result};
use chrono::NaiveDate;
use regex::Regex;
use serde_json::{Value, json};
use std::{path::Path, sync::LazyLock};
pub fn publication_date(value: &Value) -> String {
    let Some(v) = value.as_str() else {
        return String::new();
    };
    let v = v.trim();
    if !(v.len() == 8 && v.bytes().all(|b| b.is_ascii_digit())
        || v.len() == 10
            && v.as_bytes()[4] == b'-'
            && v.as_bytes()[7] == b'-'
            && v.bytes()
                .enumerate()
                .all(|(i, b)| [4, 7].contains(&i) || b.is_ascii_digit()))
    {
        return String::new();
    }
    let parsed = NaiveDate::parse_from_str(v, if v.len() == 8 { "%Y%m%d" } else { "%Y-%m-%d" });
    parsed
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}
fn is_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{000b}'
            | '\u{000c}'
            | '\u{001c}'
            | '\u{001d}'
            | '\u{001e}'
            | '\u{0085}'
            | '\u{2028}'
            | '\u{2029}'
    )
}
fn is_word(c: char) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\w$").expect("constant regex"));
    RE.is_match(&c.to_string())
}
fn link_line(line: &str) -> bool {
    if line.trim().is_empty() || line.chars().any(is_break) {
        return false;
    }
    static WEB: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)https?://[^\s<>]+|www\.[^\s<>]+").expect("constant regex")
    });
    if WEB.find_iter(line).any(|m| {
        line[..m.start()]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word(c) && c != '@')
    }) {
        return true;
    }
    static DOMAIN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:[\p{L}\p{N}](?:[\w-]{0,61}[\p{L}\p{N}])?\.)+(?:[\p{L}]{2,63}|xn--[a-zA-Z0-9-]{2,59})").expect("constant regex")
    });
    for m in DOMAIN.find_iter(line) {
        let before = &line[..m.start()];
        let after = &line[m.end()..];
        if after.chars().next().is_some_and(|c| is_word(c) || c == '-')
            || after
                .strip_prefix('.')
                .and_then(|v| v.chars().next())
                .is_some_and(|c| is_word(c) || c == '-')
        {
            continue;
        }
        if let Some(local) = before.strip_suffix('@') {
            let email = local
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_alphanumeric() || ".!#$%&'*+/=?^_`{|}~-".contains(*c))
                .count();
            if email > 0 {
                let prefix = local.chars().count() - email;
                if local
                    .chars()
                    .nth(prefix.saturating_sub(1))
                    .is_none_or(|c| prefix == 0 || !is_word(c) && c != '@')
                {
                    return true;
                }
            }
        }
        if before
            .chars()
            .next_back()
            .is_some_and(|c| is_word(c) || "@#.-".contains(c))
        {
            continue;
        }
        let suffix = m.as_str().rsplit('.').next().unwrap_or("");
        if let Ok(ascii) = idna::domain_to_ascii(suffix)
            && BARE_SUFFIXES
                .split_whitespace()
                .any(|s| s == ascii.to_lowercase())
        {
            return true;
        }
    }
    false
}
const BARE_SUFFIXES: &str = "ac academy ad ae aero af ag agency ai al am ao app aq ar art as at au audio aw ax az ba bb bd be bf bg bh bi biz bj bl blog bm bn bo bq br bs bt bv bw by bz ca cc cd cf cg ch ci ck cl cloud club cm cn co com coop courses cr cu cv cw cx cy cz de design dev digital dj dk dm do dz ec edu ee eg eh er es et eu fi fj fk fm fo fr ga game games gb gd ge gf gg gh gi gl gm gn gov gp gq gr gs gt gu gw gy hk hm hn hr ht hu id ie il im in info int io iq ir is it je jm jo jp ke kg kh ki km kn kp kr kw ky kz la lb lc li link live lk lr ls lt lu lv ly ma mc me media mg mh mil mk ml mm mn mo mp mq mr ms mt mu museum music mv mw mx my mz na name nc ne net news nf ng ni nl no np nr nu nz om online org pa pe pf pg ph pk pl pm pn pr pro ps pt pw qa re ro rs ru rw sa sb sc sd se sg shop si site sj sk sl sm sn so social space sr ss st store studio su sv sx sy sz tc td tech tf tg th tj tk tl tm tn to tools tr tt tv tw tz ua ug uk us uy uz va vc ve vg vi video vn vu website wf world ws xn--fiqs8s xn--fiqz9s xn--j1amh xn--p1ai xyz ye yt za zm zw";
pub fn extract_description_links(value: &Value) -> Vec<String> {
    value
        .as_str()
        .map(|text| {
            text.split(is_break)
                .filter(|s| link_line(s))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
pub fn normalize_description_links(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(Value::as_str)
                .filter(|s| link_line(s))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
pub fn build_document(job: &Value, chunks: &[Value], complete: bool) -> Value {
    let m = &job["metadata"];
    let text = chunks
        .iter()
        .filter_map(|v| v["text"].as_str())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut doc = json!({"schema_version":1,"title":job["title"].as_str().or(job["video_id"].as_str()).unwrap_or("YouTube transcript"),"source_url":job["url"],"video_id":job["video_id"],"channel":m["channel"].as_str().unwrap_or(""),"published_date":publication_date(&m["published_date"]),"video_duration_seconds":m["duration_seconds"],"audio_duration_seconds":m["audio_duration_seconds"],"transcribed_at":crate::now(),"provider":"Azure Speech","model":MODEL,"language_handling":"Automatic detection; original language retained","transcription_style":"verbatim","diarization":false,"complete":complete,"completed_chunks":chunks.len(),"total_chunks":job["total_chunks"].as_u64().unwrap_or(chunks.len() as u64),"timing_note":"Chunk boundaries are exact decoded-audio positions. Segment timestamps, when returned, are Azure estimates; no word timing is inferred.","content_note":"Automatic transcription of the full audio, not a summary. Recognition errors and words at chunk boundaries may occur. Video images and on-screen text are not included.","full_text":text,"chunks":chunks});
    let links = normalize_description_links(&m["description_links"]);
    if !links.is_empty() {
        doc["description_links"] = json!(links);
    }
    if let Some(speed) = m["audio_speed"].as_f64() {
        doc["audio_speed"] = json!(speed);
    }
    doc
}
fn line(value: Option<&str>) -> String {
    value
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn export_filename(title: &str, _video_id: &str, format: &str) -> String {
    let cleaned: String = title
        .chars()
        .filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c))
        .collect();
    let cleaned = line(Some(cleaned.trim_matches([' ', '.'])));
    let mut out = String::new();
    for c in cleaned.chars().take(110) {
        if out.len() + c.len_utf8() > 180 {
            break;
        }
        out.push(c);
    }
    let trimmed = out.trim_end_matches([' ', '.']);
    format!(
        "{}.{}",
        if trimmed.is_empty() {
            "YouTube transcript"
        } else {
            trimmed
        },
        format
    )
}
pub fn render_document(doc: &Value, format: &str) -> Result<String> {
    if !["txt", "md", "json"].contains(&format) {
        return Err(Error::bad_request("Choose Markdown, text, or JSON."));
    }
    let title = line(doc["title"].as_str());
    let channel = line(doc["channel"].as_str());
    let published = publication_date(&doc["published_date"]);
    let source = doc["source_url"].as_str().unwrap_or("").trim();
    let fallback = doc["chunks"]
        .as_array()
        .map(|c| {
            c.iter()
                .filter_map(|v| v["text"].as_str())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();
    let text = doc["full_text"].as_str().unwrap_or(&fallback);
    let partial = doc["complete"] != true;
    let links = normalize_description_links(&doc["description_links"]);
    if format == "json" {
        let mut result = json!({"title":title,"channel":channel,"published_date":if published.is_empty(){Value::Null}else{json!(published)},"source_url":source,"full_text":text});
        if !links.is_empty() {
            result["description_links"] = json!(links);
        }
        if partial {
            result["complete"] = json!(false);
            result["notice"] = json!("PARTIAL — transcription did not finish.");
        }
        return Ok(serde_json::to_string_pretty(&result)? + "\n");
    }
    let mut lines = vec![
        if format == "md" {
            format!("# {title}")
        } else {
            title
        },
        String::new(),
    ];
    if !channel.is_empty() {
        lines.push(format!("Channel: {channel}"));
    }
    lines.push(format!(
        "Published: {}",
        if published.is_empty() {
            "Unknown"
        } else {
            &published
        }
    ));
    if !source.is_empty() {
        lines.push(format!("Source: {source}"));
    }
    if partial {
        lines.extend([
            String::new(),
            "PARTIAL — transcription did not finish.".into(),
        ]);
    }
    if !links.is_empty() {
        lines.extend([
            String::new(),
            "Description links (creator-provided):".into(),
        ]);
        lines.extend(links);
        lines.extend([String::new(), "Transcript:".into()]);
    }
    lines.extend([String::new(), text.to_string()]);
    Ok(lines.join("\n").trim_end().to_string() + "\n")
}
pub fn write_exports(directory: &Path, document: &Value) -> Result<()> {
    // Render everything before touching disk. JSON is the authoritative
    // completion record and becomes visible only after both readable formats.
    let markdown = render_document(document, "md")?;
    let text = render_document(document, "txt")?;
    let json = serde_json::to_vec_pretty(document)?;
    crate::atomic_batch(
        directory,
        &[
            ("transcript.md", markdown.as_bytes()),
            ("transcript.txt", text.as_bytes()),
            ("transcript.json", &json),
        ],
    )
}
