//! Captions: tracks from the `player` response and YouTube timed text in
//! `json3` (`baseUrl&fmt=json3`), rendered by our own overlay; the caption
//! language preference (`prefer`) and per-video deliberate choices.

use crate::json::{self, Value};
use std::cell::RefCell;
use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub url: String,
    pub lang: String,
    pub name: String,
    pub asr: bool,
    /// YouTube offers machine translations of this track (`isTranslatable`).
    pub translatable: bool,
    /// A translation entry we added (`&tlang=`): its target language.
    pub tlang: Option<String>,
}

/// The tracks of one `player` response, plus YouTube's default choice.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct List {
    pub tracks: Vec<Track>,
    /// `audioTracks[defaultAudioTrackIndex].defaultCaptionTrackIndex`.
    pub default: Option<usize>,
}

fn primary(lang: &str) -> &str {
    lang.split(['-', '_']).next().unwrap_or(lang)
}

/// Tracks plus, when no English track exists and YouTube can translate a
/// source-language track into English, one "English (translated)" entry.
pub fn list(player: &Value) -> List {
    let r = player.pointer("/captions/playerCaptionsTracklistRenderer");
    let mut tracks = tracks(player);
    let ai = r.and_then(|r| r.get("defaultAudioTrackIndex")).and_then(Value::as_u64).unwrap_or(0) as usize;
    let default = r.and_then(|r| r.pointer(&format!("/audioTracks/{ai}/defaultCaptionTrackIndex"))).and_then(Value::as_u64).map(|i| i as usize).filter(|i| *i < tracks.len());
    let to_en = r.and_then(|r| r.get("translationLanguages")).and_then(Value::as_array).is_some_and(|l| l.iter().any(|t| t.get("languageCode").and_then(Value::as_str) == Some("en")));
    if to_en && !tracks.iter().any(|t| primary(&t.lang) == "en") {
        if let Some(src) = source(&tracks) {
            let base = tracks.iter().position(|t| !t.asr && t.translatable && primary(&t.lang) == src).or_else(|| tracks.iter().position(|t| t.asr && t.translatable && primary(&t.lang) == src));
            if let Some(b) = base {
                let mut t = tracks[b].clone();
                t.url = format!("{}&tlang=en", t.url);
                t.name = "English (translated)".into();
                t.lang = "en".into();
                t.tlang = Some("en".into());
                tracks.push(t);
            }
        }
    }
    List { tracks, default }
}

/// TV: how many languages Auto-translate offers.
const TARGETS_MAX: usize = 8;
/// TV: what fills Auto-translate after the device's language, Russian and English, when YouTube offers it.
const TARGETS_MORE: [&str; 9] = ["es", "de", "fr", "pt", "uk", "it", "tr", "ja", "ko"];

/// The Auto-translate list from what YouTube offers (`languages`: code, name): the device's language, Russian,
/// English, then the common ones, each once.
fn targets(languages: &[(String, String)], locale: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let device = locale.replace('_', "-");
    let wanted = [device.as_str(), "ru", "en"].into_iter().chain(TARGETS_MORE);
    for w in wanted {
        if w.is_empty() {
            continue;
        }
        let found = languages.iter().find(|(c, _)| c.eq_ignore_ascii_case(w)).or_else(|| languages.iter().find(|(c, _)| primary(c).eq_ignore_ascii_case(primary(w))));
        if let Some(l) = found {
            if !out.iter().any(|(c, _)| primary(c).eq_ignore_ascii_case(primary(&l.0))) {
                out.push(l.clone());
            }
        }
    }
    out
}

/// The track YouTube translates from: the spoken language's, human before ASR, else YouTube's default, else any.
fn translation_base(l: &List) -> Option<usize> {
    let ts = &l.tracks;
    let spoken = source(ts);
    let of = |asr: bool| ts.iter().position(|t| t.translatable && t.asr == asr && spoken.is_some_and(|s| primary(&t.lang) == s));
    of(false).or_else(|| of(true)).or_else(|| l.default.filter(|d| ts.get(*d).is_some_and(|t| t.translatable))).or_else(|| ts.iter().position(|t| t.translatable))
}

/// TV: the tracks, then one entry per Auto-translate language that has no track of its own (`tlang`; the entries
/// come last, so `List::default` and the track indices stay valid).
pub fn list_tv(player: &Value, locale: &str) -> List {
    let mut l = list(player);
    l.tracks.retain(|t| t.tlang.is_none());
    let languages: Vec<(String, String)> = player
        .pointer("/captions/playerCaptionsTracklistRenderer/translationLanguages")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    let code = t.get("languageCode").and_then(Value::as_str).filter(|c| crate::config::is_lang_tag(c))?;
                    let name = t.get("languageName").and_then(crate::parse::text).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| code.to_string());
                    Some((code.to_string(), name))
                })
                .collect()
        })
        .unwrap_or_default();
    let Some(base) = translation_base(&l).map(|b| l.tracks[b].clone()) else { return l };
    let own: Vec<String> = l.tracks.iter().map(|t| primary(&t.lang).to_ascii_lowercase()).collect();
    for (code, name) in targets(&languages, locale).into_iter().filter(|(c, _)| !own.contains(&primary(c).to_ascii_lowercase())).take(TARGETS_MAX) {
        l.tracks.push(Track { url: format!("{}&tlang={code}", base.url), lang: code.clone(), name, asr: base.asr, translatable: false, tlang: Some(code) });
    }
    l
}

/// The first Auto-translate entry (`list_tv`), where the panel's heading goes.
pub fn first_translated(l: &List) -> Option<usize> {
    l.tracks.iter().position(|t| t.tlang.is_some())
}

/// TV: the remembered Auto-translate language in `l`: a track of its own (human first) beats a translation.
pub fn prefer_lang(l: &List, lang: &str) -> Option<usize> {
    let is = |t: &Track| primary(&t.lang).eq_ignore_ascii_case(primary(lang));
    let ts = &l.tracks;
    ts.iter().position(|t| t.tlang.is_none() && !t.asr && is(t)).or_else(|| ts.iter().position(|t| t.tlang.is_none() && is(t))).or_else(|| ts.iter().position(|t| t.tlang.is_some() && is(t)))
}

/// The video's spoken language from YouTube's speech recognition (the ASR
/// track's language): the only evidence we use. None when there is no ASR
/// track or they disagree.
pub fn source(tracks: &[Track]) -> Option<&str> {
    let mut asr = tracks.iter().filter(|t| t.asr && t.tlang.is_none()).map(|t| primary(&t.lang));
    let first = asr.next()?;
    asr.all(|l| l == first).then_some(first).filter(|l| !l.is_empty())
}

/// Caption language: Russian source → Russian, English
/// source → English, any other source → an offered English track, else
/// YouTube's English translation of a source-language track; human before
/// ASR in the wanted language. Unknown source (or nothing fitting) keeps
/// YouTube's default track (then the old `pick`).
pub fn prefer(l: &List, hl: &str) -> Option<usize> {
    let ts = &l.tracks;
    let fallback = || l.default.or_else(|| pick(ts, hl));
    let Some(src) = source(ts) else { return fallback() };
    let want = if src == "ru" || src == "en" { src } else { "en" };
    let is = |t: &Track, asr: bool| primary(&t.lang) == want && t.asr == asr && t.tlang.is_none();
    let exact = |asr: bool| ts.iter().position(|t| is(t, asr) && t.lang == want).or_else(|| ts.iter().position(|t| is(t, asr)));
    exact(false).or_else(|| exact(true)).or_else(|| ts.iter().position(|t| t.tlang.as_deref() == Some(want))).or_else(fallback)
}

/// A deliberate language choice, remembered per video for this session (in
/// memory, 100 videos: a deliberate per-video language choice is respected, and
/// a different video goes back to the fixed preference; no stored records).
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub lang: String,
    pub asr: bool,
    pub translated: bool,
}

thread_local! {
    static CHOSEN: RefCell<VecDeque<(String, Choice)>> = const { RefCell::new(VecDeque::new()) };
}

pub fn remember(video: &str, t: &Track) {
    let c = Choice { lang: t.lang.clone(), asr: t.asr, translated: t.tlang.is_some() };
    CHOSEN.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|(v, _)| v != video);
        m.push_back((video.to_string(), c));
        while m.len() > 100 {
            m.pop_front();
        }
    });
}

/// The remembered choice for `video`, located in `l` again.
pub fn chosen(video: &str, l: &List) -> Option<usize> {
    let c = CHOSEN.with(|m| m.borrow().iter().find(|(v, _)| v == video).map(|e| e.1.clone()))?;
    l.tracks.iter().position(|t| t.lang == c.lang && t.asr == c.asr && t.tlang.is_some() == c.translated)
}

pub fn tracks(player: &Value) -> Vec<Track> {
    let Some(list) = player.pointer("/captions/playerCaptionsTracklistRenderer/captionTracks").and_then(Value::as_array) else { return Vec::new() };
    list.iter()
        .filter_map(|t| {
            let url = t.get("baseUrl").and_then(Value::as_str)?;
            let url = if url.starts_with('/') { format!("https://www.youtube.com{url}") } else { url.to_string() };
            if !url.starts_with("https://www.youtube.com/api/timedtext?") || url.bytes().any(|b| b <= b' ' || b == b'"') {
                return None;
            }
            let name = t.pointer("/name/simpleText").and_then(Value::as_str).map(str::to_string).or_else(|| {
                t.pointer("/name/runs").and_then(Value::as_array).map(|r| r.iter().filter_map(|x| x.get("text").and_then(Value::as_str)).collect())
            });
            Some(Track {
                url,
                lang: t.get("languageCode").and_then(Value::as_str).unwrap_or("").to_string(),
                name: name.unwrap_or_default(),
                asr: t.get("kind").and_then(Value::as_str) == Some("asr") || t.get("vssId").and_then(Value::as_str).is_some_and(|v| v.starts_with("a.")),
                translatable: t.get("isTranslatable").and_then(Value::as_bool) == Some(true),
                tlang: None,
            })
        })
        .collect()
}

/// Preferred track: a manual one in the UI language, any manual one, an
/// automatic one in the UI language, else the first.
pub fn pick(tracks: &[Track], hl: &str) -> Option<usize> {
    let lang = |t: &Track| t.lang == hl || t.lang.split('-').next() == Some(hl);
    tracks
        .iter()
        .position(|t| !t.asr && lang(t))
        .or_else(|| tracks.iter().position(|t| !t.asr))
        .or_else(|| tracks.iter().position(|t| t.asr && lang(t)))
        .or(if tracks.is_empty() { None } else { Some(0) })
}

pub fn json3_url(t: &Track) -> String {
    let base: String = t.url.split('&').filter(|p| !p.starts_with("fmt=")).collect::<Vec<_>>().join("&");
    format!("{base}&fmt=json3")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cue {
    pub t0: f64,
    pub t1: f64,
    pub text: String,
}

/// `{"events":[{"tStartMs","dDurationMs","segs":[{"utf8"}]}]}` → cues.
/// Window/style-only events and newline-only appends are skipped.
pub fn parse_json3(bytes: &[u8]) -> Vec<Cue> {
    let Some(v) = json::parse(bytes) else { return Vec::new() };
    let Some(events) = v.get("events").and_then(Value::as_array) else { return Vec::new() };
    let mut out: Vec<Cue> = Vec::new();
    for e in events {
        let Some(segs) = e.get("segs").and_then(Value::as_array) else { continue };
        let text: String = segs.iter().filter_map(|s| s.get("utf8").and_then(Value::as_str)).collect();
        let text = text.trim_matches('\n').to_string();
        if text.trim().is_empty() {
            continue;
        }
        let t0 = e.get("tStartMs").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
        let d = e.get("dDurationMs").and_then(Value::as_f64).unwrap_or(0.0) / 1000.0;
        out.push(Cue { t0, t1: t0 + d.max(0.0), text });
    }
    out.sort_by(|a, b| a.t0.partial_cmp(&b.t0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Text of every cue active at `t`, one per line (roll-up captions overlap).
pub fn active(cues: &[Cue], t: f64) -> String {
    let end = cues.partition_point(|c| c.t0 <= t);
    let mut lines: Vec<&str> = cues[..end].iter().rev().take_while(|c| t - c.t0 < 60.0).filter(|c| t < c.t1).map(|c| c.text.as_str()).collect();
    lines.reverse();
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// The readers take parsed responses; these tests build them with `json!`.
    fn tracks(v: &Value) -> Vec<Track> {
        super::tracks(&crate::json::Value::from_serde(v))
    }
    fn list(v: &Value) -> List {
        super::list(&crate::json::Value::from_serde(v))
    }

    #[test]
    fn track_choice() {
        let p = json!({"captions": {"playerCaptionsTracklistRenderer": {"captionTracks": [
            {"baseUrl": "https://www.youtube.com/api/timedtext?v=x&caps=asr&lang=en", "name": {"runs": [{"text": "English (auto-generated)"}]}, "vssId": "a.en", "languageCode": "en", "kind": "asr"},
            {"baseUrl": "https://www.youtube.com/api/timedtext?v=x&lang=de&fmt=srv3", "name": {"simpleText": "German"}, "vssId": ".de", "languageCode": "de"},
            {"baseUrl": "https://www.youtube.com/api/timedtext?v=x&lang=en-GB", "name": {"simpleText": "English (UK)"}, "vssId": ".en-GB", "languageCode": "en-GB"},
            {"baseUrl": "https://evil.example/api/timedtext?v=x", "languageCode": "fr"}
        ]}}});
        let t = tracks(&p);
        assert_eq!(t.len(), 3);
        assert!(t[0].asr && !t[1].asr);
        assert_eq!(t[0].name, "English (auto-generated)");
        assert_eq!(pick(&t, "en"), Some(2));
        assert_eq!(pick(&t, "de"), Some(1));
        assert_eq!(pick(&t, "ja"), Some(1), "manual track before ASR");
        assert_eq!(pick(&t[..1], "ja"), Some(0));
        assert_eq!(pick(&[], "en"), None);
        assert_eq!(json3_url(&t[1]), "https://www.youtube.com/api/timedtext?v=x&lang=de&fmt=json3");
    }

    fn tr(lang: &str, asr: bool) -> Value {
        json!({"baseUrl": format!("https://www.youtube.com/api/timedtext?v=x&lang={lang}{}", if asr { "&kind=asr" } else { "" }), "name": {"simpleText": lang}, "vssId": format!("{}{lang}", if asr { "a." } else { "." }), "languageCode": lang, "kind": if asr { "asr" } else { "" }, "isTranslatable": true})
    }
    fn player(ts: Vec<Value>, default: Option<usize>, to_en: bool) -> Value {
        let mut r = json!({"captionTracks": ts, "audioTracks": [{"captionTrackIndices": [0]}], "defaultAudioTrackIndex": 0,
            "translationLanguages": if to_en { json!([{"languageCode": "de"}, {"languageCode": "en", "languageName": {"simpleText": "English"}}]) } else { json!([{"languageCode": "de"}]) }});
        if let Some(d) = default {
            r["audioTracks"][0]["defaultCaptionTrackIndex"] = d.into();
        }
        json!({"captions": {"playerCaptionsTracklistRenderer": r}})
    }
    fn pref(p: &Value) -> Option<(String, bool, bool)> {
        let l = list(p);
        prefer(&l, "de").map(|i| (l.tracks[i].lang.clone(), l.tracks[i].asr, l.tracks[i].tlang.is_some()))
    }
    fn t(lang: &str, asr: bool, translated: bool) -> Option<(String, bool, bool)> {
        Some((lang.into(), asr, translated))
    }

    #[test]
    fn language_preference() {
        // Russian source: Russian (human first), never English.
        assert_eq!(pref(&player(vec![tr("en", false), tr("ru", true), tr("ru", false)], Some(0), true)), t("ru", false, false));
        assert_eq!(pref(&player(vec![tr("en", false), tr("ru", true)], Some(0), true)), t("ru", true, false), "Russian ASR before an English track");
        // English source: human English before ASR English (also en-GB).
        assert_eq!(pref(&player(vec![tr("en", true), tr("de", false), tr("en-GB", false)], None, true)), t("en-GB", false, false));
        assert_eq!(pref(&player(vec![tr("de", false), tr("en", true)], Some(0), true)), t("en", true, false));
        // Other source: an offered English track (human, then ASR-free), else a translation.
        assert_eq!(pref(&player(vec![tr("de", true), tr("de", false), tr("en", false)], Some(0), true)), t("en", false, false));
        let p = player(vec![tr("ja", true), tr("ja", false), tr("fr", false)], Some(2), true);
        assert_eq!(pref(&p), t("en", false, true), "English translation of the Japanese source");
        let l = list(&p);
        let en = l.tracks.last().unwrap();
        assert_eq!(en.url, "https://www.youtube.com/api/timedtext?v=x&lang=ja&tlang=en", "human source track as the base");
        assert_eq!(en.name, "English (translated)");
        assert_eq!(json3_url(en), "https://www.youtube.com/api/timedtext?v=x&lang=ja&tlang=en&fmt=json3");
        // ASR-only source is translated from the ASR track.
        let l = list(&player(vec![tr("ko", true)], None, true));
        assert_eq!(l.tracks.len(), 2);
        assert!(l.tracks[1].url.contains("kind=asr&tlang=en"));
        // No English translation offered → YouTube's default track stays.
        assert_eq!(pref(&player(vec![tr("ja", true), tr("fr", false)], Some(1), false)), t("fr", false, false));
        // Unknown source (no ASR evidence) → YouTube's default, else the old pick.
        assert_eq!(pref(&player(vec![tr("fr", false), tr("en", false)], Some(0), true)), t("fr", false, false));
        assert_eq!(pref(&player(vec![tr("fr", false), tr("de", false)], None, true)), t("de", false, false), "pick: UI language de");
        // Conflicting ASR languages → unknown.
        assert_eq!(pref(&player(vec![tr("ru", true), tr("uk", true), tr("uk", false)], Some(2), true)), t("uk", false, false));
        assert_eq!(prefer(&List::default(), "en"), None);
        // An English track exists → no translation entry is added.
        assert_eq!(list(&player(vec![tr("de", true), tr("en", true)], None, true)).tracks.len(), 2);
    }

    fn tv_list(ts: Vec<Value>, default: Option<usize>, langs: &[(&str, &str)], locale: &str) -> List {
        let mut p = player(ts, default, false);
        p["captions"]["playerCaptionsTracklistRenderer"]["translationLanguages"] = langs.iter().map(|(c, n)| json!({"languageCode": c, "languageName": {"simpleText": n}})).collect();
        list_tv(&crate::json::Value::from_serde(&p), locale)
    }

    #[test]
    fn auto_translate_is_led_by_the_device_then_russian_then_english() {
        let all = [("en", "English"), ("de", "German"), ("ru", "Russian"), ("ko", "Korean"), ("hu", "Hungarian"), ("es", "Spanish"), ("fr", "French"), ("pt-BR", "Portuguese"), ("uk", "Ukrainian"), ("it", "Italian"), ("tr", "Turkish"), ("ja", "Japanese")];
        let names = |l: &List| l.tracks.iter().filter(|t| t.tlang.is_some()).map(|t| t.name.clone()).collect::<Vec<_>>();
        let l = tv_list(vec![tr("fi", true), tr("fi", false)], None, &all, "hu-HU");
        assert_eq!(names(&l), ["Hungarian", "Russian", "English", "Spanish", "German", "French", "Portuguese", "Ukrainian"], "short, device first");
        assert_eq!(first_translated(&l), Some(2));
        assert_eq!(l.tracks[2].url, "https://www.youtube.com/api/timedtext?v=x&lang=fi&tlang=hu", "from the spoken language's human track");
        assert_eq!(json3_url(&l.tracks[3]), "https://www.youtube.com/api/timedtext?v=x&lang=fi&tlang=ru&fmt=json3");
        // A language that has a track gets no translation; the device's language counts once.
        let l = tv_list(vec![tr("fi", true), tr("en", false)], None, &all, "ru");
        assert_eq!(names(&l), ["Russian", "Spanish", "German", "French", "Portuguese", "Ukrainian", "Italian", "Turkish"]);
        // Only what YouTube offers; nothing to translate from, no entries.
        assert_eq!(names(&tv_list(vec![tr("fi", true)], None, &[("ru", "Russian")], "en")), ["Russian"]);
        let mut plain = tr("fi", false);
        plain["isTranslatable"] = false.into();
        assert!(names(&tv_list(vec![plain], None, &all, "en")).is_empty());
        // pt-PT on the device takes YouTube's pt-BR; an ASR-only source translates from ASR.
        let l = tv_list(vec![tr("ko", true)], None, &all, "pt-PT");
        assert_eq!(l.tracks[1].tlang.as_deref(), Some("pt-BR"));
        assert!(l.tracks[1].url.contains("kind=asr&tlang=pt-BR"));
        assert!(l.tracks[1].asr);
        // No speech-recognition evidence: YouTube's default track is the base.
        let l = tv_list(vec![tr("de", false), tr("fr", false)], Some(1), &all, "ru");
        assert!(l.tracks[2].url.contains("lang=fr&tlang=ru"));
        // A strange code never reaches a URL.
        let l = tv_list(vec![tr("fi", false)], None, &[("r&u=", "Bad"), ("ru", "Russian")], "en");
        assert_eq!(names(&l), ["Russian"]);
    }

    #[test]
    fn a_remembered_translation_finds_its_language_again() {
        let langs = [("ru", "Russian"), ("en", "English")];
        let l = tv_list(vec![tr("fi", true), tr("fi", false)], None, &langs, "en");
        assert_eq!(l.tracks[prefer_lang(&l, "ru").unwrap()].tlang.as_deref(), Some("ru"));
        let l = tv_list(vec![tr("ru", true), tr("en", false)], None, &langs, "en");
        assert_eq!(prefer_lang(&l, "ru"), Some(0), "its own track, ASR too, before a translation");
        let l = tv_list(vec![tr("ru", true), tr("ru-RU", false)], None, &langs, "en");
        assert_eq!(prefer_lang(&l, "ru"), Some(1), "human first");
        assert_eq!(prefer_lang(&l, "ko"), None);
        assert_eq!(prefer_lang(&List::default(), "ru"), None);
    }

    #[test]
    fn deliberate_choice_per_video() {
        let l = list(&player(vec![tr("ja", true), tr("ja", false)], None, true));
        assert_eq!(chosen("vid00000001", &l), None);
        remember("vid00000001", &l.tracks[0]);
        assert_eq!(chosen("vid00000001", &l), Some(0));
        assert_eq!(chosen("vid00000002", &l), None, "other videos keep the fixed preference");
        remember("vid00000001", &l.tracks[2]);
        assert_eq!(chosen("vid00000001", &l), Some(2), "the translated entry is found again");
        for i in 0..120 {
            remember(&format!("v{i:010}"), &l.tracks[1]);
        }
        assert_eq!(chosen("vid00000001", &l), None, "bounded to 100 videos");
    }

    #[test]
    fn json3() {
        let j = br#"{"wireMagic":"pb3","pens":[{}],"events":[
            {"tStartMs":0,"dDurationMs":5000,"id":1,"wpWinPosId":1,"wsWinStyleId":1},
            {"tStartMs":1000,"dDurationMs":3000,"wWinId":1,"segs":[{"utf8":"hello","acAsrConf":0},{"utf8":" world","tOffsetMs":400}]},
            {"tStartMs":2500,"dDurationMs":2500,"wWinId":1,"segs":[{"utf8":"second line"}]},
            {"tStartMs":3990,"wWinId":1,"aAppend":1,"segs":[{"utf8":"\n"}]},
            {"tStartMs":9000,"dDurationMs":1000,"segs":[{"utf8":"<b>&amp;</b>"}]}]}"#;
        let c = parse_json3(j);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0], Cue { t0: 1.0, t1: 4.0, text: "hello world".into() });
        assert_eq!(active(&c, 0.5), "");
        assert_eq!(active(&c, 1.0), "hello world");
        assert_eq!(active(&c, 3.0), "hello world\nsecond line");
        assert_eq!(active(&c, 4.5), "second line");
        assert_eq!(active(&c, 9.5), "<b>&amp;</b>", "raw text; the overlay sets textContent");
        assert!(parse_json3(b"nope").is_empty());
    }
}
