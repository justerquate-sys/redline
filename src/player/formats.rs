//! Adaptive formats from an InnerTube `player` response and the quality
//! decisions on them: the per-rung ladder (codec preference), the start rung,
//! the size cap and a simple throughput-based ABR. Pure; `supported` is the
//! caller's `MediaSource.isTypeSupported`.

use crate::json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Vp9,
    Av1,
    H264,
    Opus,
    Aac,
    Other,
}

impl Codec {
    pub fn name(self) -> &'static str {
        match self {
            Codec::Vp9 => "VP9",
            Codec::Av1 => "AV1",
            Codec::H264 => "H.264",
            Codec::Opus => "Opus",
            Codec::Aac => "AAC",
            Codec::Other => "?",
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Codec::Vp9 => "vp9",
            Codec::Av1 => "av1",
            Codec::H264 => "h264",
            _ => "",
        }
    }
    pub fn from_key(s: &str) -> Option<Codec> {
        match s {
            "vp9" => Some(Codec::Vp9),
            "av1" => Some(Codec::Av1),
            "h264" => Some(Codec::H264),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Format {
    pub itag: u32,
    pub url: String,
    /// `video/webm; codecs="vp9"` (as given; usable for `isTypeSupported`/`addSourceBuffer`).
    pub mime: String,
    pub codec: Option<Codec>,
    pub video: bool,
    pub webm: bool,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub label: String,
    /// Nominal rung: the number in `qualityLabel` (1080 for 1920×1080 and 1080×1920).
    pub p: u32,
    pub bitrate: u64,
    pub init_end: u64,
    pub index_end: u64,
    pub len: u64,
    pub dur: f64,
    pub hdr: bool,
    /// HDR with the HLG transfer (else PQ, SMPTE ST 2084).
    pub hlg: bool,
    /// A DRC (dynamic range compressed) audio variant per its `xtags`. Other
    /// xtags (dub language/content tags) are normal: on auto-dubbed videos
    /// every audio format, the original included, carries them.
    pub xtags: bool,
    /// Non-default dubbed audio track.
    pub alt_audio: bool,
    /// `audioTrack.id` (e.g. `de.3`) and `displayName`; empty when the video
    /// has a single audio track.
    pub audio_id: String,
    pub audio_name: String,
}

/// `xtags` is base64(url) protobuf of key/value strings; DRC variants carry "drc".
fn xtags_drc(tags: &str) -> bool {
    let mut bytes = Vec::with_capacity(tags.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in tags.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => continue,
        } as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            bytes.push((acc >> bits) as u8);
        }
    }
    bytes.windows(3).any(|w| w == b"drc")
}

fn num(v: &Value) -> u64 {
    match v {
        Value::String(s) => s.parse().unwrap_or(0),
        Value::Number(_) => v.as_u64().unwrap_or(0),
        _ => 0,
    }
}

pub fn codec_of(mime: &str) -> Codec {
    let c = mime.split("codecs=").nth(1).unwrap_or("").trim_matches(|c| c == '"' || c == ' ');
    if c.starts_with("vp9") || c.starts_with("vp09") {
        Codec::Vp9
    } else if c.starts_with("av01") {
        Codec::Av1
    } else if c.starts_with("avc1") {
        Codec::H264
    } else if c.starts_with("opus") {
        Codec::Opus
    } else if c.starts_with("mp4a") {
        Codec::Aac
    } else {
        Codec::Other
    }
}

fn label_p(label: &str, w: u32, h: u32) -> u32 {
    let digits: String = label.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or_else(|_| w.min(h))
}

/// `streamingData.adaptiveFormats` → formats with a direct URL. VOD formats
/// carry byte ranges; live ones (fetched by sequence number) have none, so
/// `init_end`/`index_end` stay 0 and only the live rules (`live_ok`) take them.
pub fn parse(player: &Value) -> Vec<Format> {
    let Some(list) = player.pointer("/streamingData/adaptiveFormats").and_then(Value::as_array) else { return Vec::new() };
    let mut out = Vec::new();
    for f in list {
        let url = f.get("url").and_then(Value::as_str).unwrap_or("");
        let mime = f.get("mimeType").and_then(Value::as_str).unwrap_or("");
        if !url.starts_with("https://") || mime.is_empty() {
            continue;
        }
        let (init, index) = (f.get("initRange").unwrap_or(&Value::Null), f.get("indexRange").unwrap_or(&Value::Null));
        let video = mime.starts_with("video/");
        if !video && !mime.starts_with("audio/") {
            continue;
        }
        let width = num(f.get("width").unwrap_or(&Value::Null)) as u32;
        let height = num(f.get("height").unwrap_or(&Value::Null)) as u32;
        let label = f.get("qualityLabel").and_then(Value::as_str).unwrap_or("").to_string();
        let codec = codec_of(mime);
        out.push(Format {
            itag: num(&f["itag"]) as u32,
            url: url.to_string(),
            mime: mime.to_string(),
            codec: Some(codec),
            video,
            webm: mime.contains("/webm"),
            width,
            height,
            fps: num(f.get("fps").unwrap_or(&Value::Null)) as u32,
            p: if video { label_p(&label, width, height) } else { 0 },
            hdr: label.contains("HDR") || f.pointer("/colorInfo/transferCharacteristics").and_then(Value::as_str).is_some_and(|t| t.contains("SMPTEST2084") || t.contains("HLG")),
            hlg: f.pointer("/colorInfo/transferCharacteristics").and_then(Value::as_str).is_some_and(|t| t.contains("HLG")),
            label,
            bitrate: num(&f["bitrate"]),
            init_end: num(init.get("end").unwrap_or(&Value::Null)),
            index_end: num(index.get("end").unwrap_or(&Value::Null)),
            len: num(f.get("contentLength").unwrap_or(&Value::Null)),
            dur: num(f.get("approxDurationMs").unwrap_or(&Value::Null)) as f64 / 1000.0,
            xtags: f.get("xtags").and_then(Value::as_str).is_some_and(xtags_drc),
            alt_audio: f.pointer("/audioTrack/audioIsDefault").and_then(Value::as_bool) == Some(false),
            audio_id: f.pointer("/audioTrack/id").and_then(Value::as_str).unwrap_or("").to_string(),
            audio_name: f.pointer("/audioTrack/displayName").and_then(Value::as_str).unwrap_or("").to_string(),
        });
    }
    out
}

/// Default codec order: VP9 (hardware decode on Apple Silicon, small files),
/// then AV1, then H.264. Each is only used where `isTypeSupported` agrees.
pub const DEFAULT_ORDER: [Codec; 3] = [Codec::Vp9, Codec::Av1, Codec::H264];

/// Codec order with `preferred` moved to the front.
pub fn order(preferred: Option<Codec>) -> Vec<Codec> {
    let mut o: Vec<Codec> = DEFAULT_ORDER.to_vec();
    if let Some(p) = preferred {
        o.retain(|c| *c != p);
        o.insert(0, p);
    }
    o
}

/// Byte-range playable; `strict` also skips DRC audio and non-default dubs.
fn usable(f: &Format, strict: bool) -> bool {
    f.init_end > 0 && f.index_end > f.init_end && f.len > f.index_end && (!strict || !f.xtags && !f.alt_audio)
}

/// A live format: fetched by sequence number as fragmented MP4 (no ranges).
pub fn live_ok(f: &Format) -> bool {
    f.init_end == 0 && f.index_end == 0 && !f.webm && !f.xtags && !f.alt_audio
}

/// One video format per rung (highest first), best codec per rung by
/// `codecs` order; SDR preferred over HDR at the same rung.
pub fn ladder<'a>(formats: &'a [Format], codecs: &[Codec], supported: &dyn Fn(&str) -> bool) -> Vec<&'a Format> {
    ladder_with(formats, codecs, supported, &|_| None)
}

/// Like `ladder`, using `MediaCapabilities.decodingInfo` results per itag
/// (`Some((smooth, power_efficient))`): per rung the highest frame rate wins,
/// then formats the browser decodes smoothly, then power-efficient (hardware)
/// decoding, then the codec order and bitrate. Rungs the browser reports as
/// not smooth are dropped while a smooth rung remains below.
pub fn ladder_with<'a>(formats: &'a [Format], codecs: &[Codec], supported: &dyn Fn(&str) -> bool, caps: &dyn Fn(u32) -> Option<(bool, bool)>) -> Vec<&'a Format> {
    ladder_by(formats, codecs, supported, &|f| usable(f, false), caps)
}

/// The ladder of a live stream's formats.
pub fn live_ladder<'a>(formats: &'a [Format], codecs: &[Codec], supported: &dyn Fn(&str) -> bool) -> Vec<&'a Format> {
    ladder_by(formats, codecs, supported, &live_ok, &|_| None)
}

/// Best live audio (AAC for YouTube live; Opus if ever offered).
pub fn live_audio<'a>(formats: &'a [Format], supported: &dyn Fn(&str) -> bool) -> Option<&'a Format> {
    let rank = |f: &Format| match f.codec {
        Some(Codec::Opus) => 0,
        Some(Codec::Aac) => 1,
        _ => 9,
    };
    formats.iter().filter(|f| !f.video && live_ok(f) && rank(f) < 9 && supported(&f.mime)).min_by(|a, b| rank(a).cmp(&rank(b)).then(b.bitrate.cmp(&a.bitrate)))
}

fn ladder_by<'a>(formats: &'a [Format], codecs: &[Codec], supported: &dyn Fn(&str) -> bool, ok: &dyn Fn(&Format) -> bool, caps: &dyn Fn(u32) -> Option<(bool, bool)>) -> Vec<&'a Format> {
    let rank = |f: &Format| f.codec.and_then(|c| codecs.iter().position(|x| *x == c)).unwrap_or(usize::MAX);
    let smooth = |f: &Format| caps(f.itag).is_none_or(|c| c.0);
    let eff = |f: &Format| caps(f.itag).is_some_and(|c| c.1);
    let mut cands: Vec<&Format> = formats.iter().filter(|f| f.video && ok(f) && rank(f) != usize::MAX && supported(&f.mime)).collect();
    if cands.iter().any(|f| smooth(f)) {
        cands.retain(|f| smooth(f));
    }
    cands.sort_by(|a, b| b.p.cmp(&a.p).then(b.fps.cmp(&a.fps)).then(a.hdr.cmp(&b.hdr)).then(eff(b).cmp(&eff(a))).then(rank(a).cmp(&rank(b))).then(b.bitrate.cmp(&a.bitrate)));
    let mut out: Vec<&Format> = Vec::new();
    for f in cands {
        if out.last().is_none_or(|l| l.p != f.p) {
            out.push(f);
        }
    }
    out
}

/// decodingInfo answers with HDR formats counted as not smooth when the screen is not HDR
/// (`hdr_screen`): the SDR format of a rung then wins. An SDR screen gains nothing from HDR, and
/// decoders answer "smooth" for HDR they cannot show (live 3 Nov 2026, Android TV emulator: 1440p60
/// HDR AV1 "smooth" even when asked with its transfer function, then no picture).
pub fn caps_for_screen<'a>(formats: &'a [Format], hdr_screen: bool, caps: &'a dyn Fn(u32) -> Option<(bool, bool)>) -> impl Fn(u32) -> Option<(bool, bool)> + 'a {
    move |i| {
        if !hdr_screen && formats.iter().any(|f| f.itag == i && f.video && f.hdr) {
            return Some((false, false));
        }
        caps(i)
    }
}

/// Best audio: Opus, then AAC; highest bitrate; default track only.
pub fn audio<'a>(formats: &'a [Format], supported: &dyn Fn(&str) -> bool) -> Option<&'a Format> {
    let rank = |f: &Format| match f.codec {
        Some(Codec::Opus) => 0,
        Some(Codec::Aac) => 1,
        _ => 9,
    };
    let best = |strict: bool| {
        formats
            .iter()
            .filter(|f| !f.video && usable(f, strict) && rank(f) < 9 && supported(&f.mime))
            .min_by(|a, b| rank(a).cmp(&rank(b)).then(b.bitrate.cmp(&a.bitrate)))
    };
    // Prefer the default (original) track without DRC; otherwise take any.
    best(true).or_else(|| best(false))
}

/// Distinct playable audio tracks `(id, name)`, default track first. Empty
/// or one entry = nothing to choose.
pub fn audio_tracks(formats: &[Format], supported: &dyn Fn(&str) -> bool) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut ok: Vec<&Format> = formats.iter().filter(|f| !f.video && !f.audio_id.is_empty() && usable(f, false) && !f.xtags && supported(&f.mime)).collect();
    ok.sort_by_key(|f| f.alt_audio);
    for f in ok {
        if !out.iter().any(|(id, _)| *id == f.audio_id) {
            let name = if f.audio_name.is_empty() { f.audio_id.clone() } else { f.audio_name.clone() };
            out.push((f.audio_id.clone(), name));
        }
    }
    out
}

/// Best audio format of one track (`audio` rules, restricted to `id`).
pub fn audio_in<'a>(formats: &'a [Format], id: &str, supported: &dyn Fn(&str) -> bool) -> Option<&'a Format> {
    let rank = |f: &Format| match f.codec {
        Some(Codec::Opus) => 0,
        Some(Codec::Aac) => 1,
        _ => 9,
    };
    formats
        .iter()
        .filter(|f| !f.video && f.audio_id == id && usable(f, false) && !f.xtags && rank(f) < 9 && supported(&f.mime))
        .min_by(|a, b| rank(a).cmp(&rank(b)).then(b.bitrate.cmp(&a.bitrate)))
}

/// The language of an audio track's id (`en-US.4` → `en-US`).
pub fn track_lang(id: &str) -> &str {
    id.split('.').next().unwrap_or(id)
}

/// TV: the track of the remembered language among `tracks` (`audio_tracks`): that exact tag, else the same language
/// (`es-419` for `es-US`). Only where there is a choice; none of it → None (YouTube's default track plays).
pub fn track_for_lang<'a>(tracks: &'a [(String, String)], lang: &str) -> Option<&'a str> {
    if tracks.len() < 2 || lang.is_empty() {
        return None;
    }
    let primary = |s: &'a str| s.split('-').next().unwrap_or(s);
    let want = lang.split('-').next().unwrap_or(lang);
    tracks.iter().find(|(id, _)| track_lang(id).eq_ignore_ascii_case(lang)).or_else(|| tracks.iter().find(|(id, _)| primary(track_lang(id)).eq_ignore_ascii_case(want))).map(|(id, _)| id.as_str())
}

/// TV: what choosing track `id` leaves remembered for the next videos: a dub's language; nothing for an original.
pub fn remembered_lang(id: &str, dub: bool) -> String {
    if dub { track_lang(id).chars().take(16).collect() } else { String::new() }
}

/// TV: the track this video starts on instead of YouTube's default: the one chosen for it (`sel`), else the remembered
/// language where the video has it.
pub fn tv_audio<'a>(formats: &'a [Format], sel: &str, lang: &str, supported: &dyn Fn(&str) -> bool) -> Option<&'a Format> {
    let chosen = if sel.is_empty() { None } else { audio_in(formats, sel, supported) };
    chosen.or_else(|| {
        let tracks = audio_tracks(formats, supported);
        track_for_lang(&tracks, lang).and_then(|id| audio_in(formats, id, supported))
    })
}

thread_local! {
    /// TV: a track picked on one video stays that video's for this session, whatever is chosen on others.
    static AUDIO_CHOSEN: std::cell::RefCell<std::collections::VecDeque<(String, String)>> = const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

pub fn remember_audio(video: &str, id: &str) {
    AUDIO_CHOSEN.with(|m| {
        let mut m = m.borrow_mut();
        m.retain(|(v, _)| v != video);
        m.push_back((video.to_string(), id.to_string()));
        while m.len() > 100 {
            m.pop_front();
        }
    });
}

pub fn chosen_audio(video: &str) -> Option<String> {
    AUDIO_CHOSEN.with(|m| m.borrow().iter().find(|(v, _)| v == video).map(|e| e.1.clone()))
}

/// Why nothing was playable, for the debug overlay.
pub fn diagnose(formats: &[Format], supported: &dyn Fn(&str) -> bool) -> String {
    let (v, a): (Vec<&Format>, Vec<&Format>) = formats.iter().partition(|f| f.video);
    let count = |l: &[&Format], p: &dyn Fn(&Format) -> bool| l.iter().filter(|f| p(f)).count();
    format!(
        "video {} (ranges ok {}, supported {}), audio {} (ranges ok {}, supported {}, default {}, drc {})",
        v.len(),
        count(&v, &|f| usable(f, false)),
        count(&v, &|f| supported(&f.mime)),
        a.len(),
        count(&a, &|f| usable(f, false)),
        count(&a, &|f| supported(&f.mime)),
        count(&a, &|f| !f.alt_audio),
        count(&a, &|f| f.xtags)
    )
}

/// Size cap for Auto: the rung that fits the player's height in device pixels,
/// allowing the next rung up when it is within 20 % (a 600 px player gets 720p).
pub fn size_cap(box_h_px: f64) -> u32 {
    (box_h_px.max(144.0) * 1.2) as u32
}

/// Auto ceiling from the display (physical pixels): one rung above the screen,
/// since a higher rung downscaled carries more bitrate and detail (a 1440p
/// screen gets 2160p, 1080p gets 1440p). The player box can only raise it.
pub fn display_cap(screen_h_px: f64, box_h_px: f64) -> u32 {
    ((screen_h_px.max(box_h_px).max(144.0)) * 1.6) as u32
}

/// Seconds to buffer ahead: as much as fits a ~100 MB video budget (Chrome's
/// MSE video quota is ~150 MB), between 30 s and 120 s.
pub fn ahead_secs(video_bps: u64) -> f64 {
    if video_bps == 0 {
        return 30.0;
    }
    (100.0 * 8.0 * 1_000_000.0 / video_bps as f64).clamp(30.0, 120.0)
}

/// Dropped-frame watchdog: over a window of at least 180 frames, more than
/// 8 % dropped means this rung is too heavy for the device.
pub fn too_many_drops(dropped: f64, total: f64) -> bool {
    total >= 180.0 && dropped / total > 0.08
}

/// Index in `ladder` of the highest rung with `p <= limit` (else the lowest).
pub fn pick(ladder: &[&Format], limit: u32) -> Option<usize> {
    if ladder.is_empty() {
        return None;
    }
    Some(ladder.iter().position(|f| f.p <= limit).unwrap_or(ladder.len() - 1))
}

/// Rung for the first segment: at most 360p (fast first frame), same codec as
/// the target when possible.
pub const START_P: u32 = 360;
pub fn start_rung(ladder: &[&Format], target: usize) -> usize {
    let t = ladder[target];
    if t.p <= START_P {
        return target;
    }
    ladder.iter().position(|f| f.p <= START_P && f.codec == t.codec).or_else(|| ladder.iter().position(|f| f.p <= START_P)).unwrap_or(target)
}

/// ABR: the highest rung within `limit` whose bitrate fits 70 % of the
/// measured throughput (bits/s). Unknown throughput → the size-capped rung.
/// Never below 1080p on a slow throughput estimate (1080p is the
/// floor by design); only stalls and dropped frames lower it, via `limit`.
pub const MIN_P: u32 = 1080;
pub fn abr(ladder: &[&Format], limit: u32, bps: Option<f64>) -> Option<usize> {
    let capped = pick(ladder, limit)?;
    let floor = pick(ladder, limit.min(MIN_P))?;
    let Some(bw) = bps else { return Some(capped) };
    let fits = (capped..ladder.len()).find(|&i| (ladder[i].bitrate as f64) <= bw * 0.7).unwrap_or(ladder.len() - 1);
    Some(fits.min(floor))
}

/// Throughput estimate: exponentially weighted moving average of per-request
/// bits/s, ignoring tiny responses (latency-dominated).
#[derive(Clone, Copy, Debug, Default)]
pub struct Throughput {
    pub bps: Option<f64>,
}

impl Throughput {
    pub fn add(&mut self, bytes: usize, ms: f64) {
        if bytes < 64 * 1024 || ms <= 0.0 {
            return;
        }
        let sample = bytes as f64 * 8.0 / (ms / 1000.0);
        self.bps = Some(match self.bps {
            None => sample,
            Some(old) => old * 0.7 + sample * 0.3,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// The readers take parsed responses; these tests build them with `json!`.
    fn parse(v: &Value) -> Vec<Format> {
        super::parse(&crate::json::Value::from_serde(v))
    }

    fn fmt(itag: u32, mime: &str, label: &str, h: u32, bitrate: u64) -> Value {
        json!({"itag": itag, "url": format!("https://r.googlevideo.com/videoplayback?itag={itag}"), "mimeType": mime,
            "qualityLabel": label, "height": h, "width": h * 16 / 9, "bitrate": bitrate, "fps": 30,
            "initRange": {"start": "0", "end": "218"}, "indexRange": {"start": "219", "end": "2298"},
            "contentLength": "5000000", "approxDurationMs": "634566"})
    }

    fn sample() -> Value {
        let mut a = vec![
            fmt(315, "video/webm; codecs=\"vp9\"", "2160p60", 2160, 26_000_000),
            fmt(303, "video/webm; codecs=\"vp9\"", "1080p60", 1080, 4_600_000),
            fmt(299, "video/mp4; codecs=\"avc1.64002A\"", "1080p60", 1080, 6_900_000),
            fmt(399, "video/mp4; codecs=\"av01.0.09M.08\"", "1080p60", 1080, 4_400_000),
            fmt(302, "video/webm; codecs=\"vp9\"", "720p60", 720, 2_700_000),
            fmt(136, "video/mp4; codecs=\"avc1.4d401f\"", "720p", 720, 1_500_000),
            fmt(243, "video/webm; codecs=\"vp9\"", "360p", 360, 570_000),
            fmt(134, "video/mp4; codecs=\"avc1.4D401E\"", "360p", 360, 640_000),
            fmt(278, "video/webm; codecs=\"vp9\"", "144p", 144, 100_000),
            fmt(251, "audio/webm; codecs=\"opus\"", "", 0, 143_000),
            fmt(140, "audio/mp4; codecs=\"mp4a.40.2\"", "", 0, 131_000),
        ];
        let mut drc = fmt(251, "audio/webm; codecs=\"opus\"", "", 0, 150_000);
        drc["xtags"] = "CggKA2RyYxIBMQ".into();
        a.push(drc);
        let mut dub = fmt(250, "audio/webm; codecs=\"opus\"", "", 0, 200_000);
        dub["audioTrack"] = json!({"id": "de.3", "audioIsDefault": false});
        a.push(dub);
        // No index → unusable (e.g. a live/HLS-only entry).
        a.push(json!({"itag": 18, "url": "https://x/", "mimeType": "video/mp4; codecs=\"avc1\"", "qualityLabel": "360p"}));
        json!({"streamingData": {"adaptiveFormats": a}})
    }

    #[test]
    fn live_formats_by_sequence_number() {
        // VISIONOS for a live stream (1 Nov 2026): no ranges, `targetDurationSec`.
        let live = |itag: u32, mime: &str, q: &str, h: u32, br: u64| json!({"itag": itag, "url": format!("https://rr4---sn-x.googlevideo.com/videoplayback?itag={itag}&live=1"), "mimeType": mime, "qualityLabel": q, "width": h * 16 / 9, "height": h, "bitrate": br, "targetDurationSec": 5, "maxDvrDurationSec": 43200});
        let j = json!({"streamingData": {"adaptiveFormats": [
            live(137, "video/mp4; codecs=\"avc1.640028\"", "1080p", 1080, 4_500_000),
            live(134, "video/mp4; codecs=\"avc1.4D401E\"", "360p", 360, 900_000),
            live(160, "video/mp4; codecs=\"avc1.42C00B\"", "144p", 144, 200_000),
            live(140, "audio/mp4; codecs=\"mp4a.40.2\"", "", 0, 128_000),
            live(386, "text/mp4; codecs=\"unknown\"", "", 0, 1)]}});
        let fs = parse(&j);
        let all = |_: &str| true;
        assert!(ladder(&fs, &order(None), &all).is_empty(), "not byte-range playable");
        assert!(audio(&fs, &all).is_none());
        let l = live_ladder(&fs, &order(None), &all);
        assert_eq!(l.iter().map(|f| f.itag).collect::<Vec<_>>(), vec![137, 134, 160]);
        assert_eq!(live_audio(&fs, &all).unwrap().itag, 140);
        assert!(live_ladder(&fs, &order(None), &|m: &str| !m.contains("avc1")).is_empty());
        // VOD formats are never taken as live ones.
        assert!(parse(&sample()).iter().filter(|f| f.itag != 18).all(|f| !live_ok(f)));
    }

    #[test]
    fn a_remembered_language_finds_its_track() {
        let t = |ids: &[&str]| ids.iter().map(|i| (i.to_string(), i.to_string())).collect::<Vec<_>>();
        let dub = t(&["ru.3", "de-DE.3", "en-US.4"]);
        assert_eq!(track_lang("en-US.4"), "en-US");
        assert_eq!(remembered_lang("fr-FR.3", true), "fr-FR", "a dub's language is remembered");
        assert_eq!(remembered_lang("en-US.4", false), "", "an original clears it");
        assert_eq!(track_for_lang(&dub, "ru"), Some("ru.3"));
        assert_eq!(track_for_lang(&dub, "en-US"), Some("en-US.4"));
        assert_eq!(track_for_lang(&dub, "en-GB"), Some("en-US.4"), "the same language");
        assert_eq!(track_for_lang(&dub, "DE"), Some("de-DE.3"));
        assert_eq!(track_for_lang(&t(&["es-419.2", "es.3"]), "es"), Some("es.3"), "the exact tag first");
        assert_eq!(track_for_lang(&dub, "fr"), None, "falls back to the default track");
        assert_eq!(track_for_lang(&dub, ""), None);
        assert_eq!(track_for_lang(&t(&["ru.3"]), "ru"), None, "one track: nothing to choose");
        assert_eq!(track_for_lang(&[], "ru"), None);
        remember_audio("v0000000001", "ru.3");
        remember_audio("v0000000001", "de.3");
        assert_eq!(chosen_audio("v0000000001").as_deref(), Some("de.3"));
        assert_eq!(chosen_audio("v0000000002"), None);
        for i in 0..110 {
            remember_audio(&format!("x{i:010}"), "ru.3");
        }
        assert_eq!(chosen_audio("v0000000001"), None, "bounded");
    }

    #[test]
    fn audio_tracks_for_the_cc_popup() {
        let all = |_: &str| true;
        let mut a = Vec::new();
        let mut orig = fmt(251, "audio/webm; codecs=\"opus\"", "", 0, 140_000);
        orig["audioTrack"] = json!({"id": "en.4", "displayName": "English original", "audioIsDefault": true});
        let mut dub = fmt(251, "audio/webm; codecs=\"opus\"", "", 0, 150_000);
        dub["audioTrack"] = json!({"id": "de.3", "displayName": "German", "audioIsDefault": false});
        let mut dub_aac = fmt(140, "audio/mp4; codecs=\"mp4a.40.2\"", "", 0, 130_000);
        dub_aac["audioTrack"] = json!({"id": "de.3", "displayName": "German", "audioIsDefault": false});
        a.extend([dub, dub_aac, orig]);
        let fs = parse(&json!({"streamingData": {"adaptiveFormats": a}}));
        assert_eq!(audio_tracks(&fs, &all), vec![("en.4".to_string(), "English original".to_string()), ("de.3".into(), "German".into())]);
        let de = audio_in(&fs, "de.3", &all).unwrap();
        assert_eq!((de.itag, de.alt_audio), (251, true), "Opus first within the track");
        assert_eq!(audio_in(&fs, "de.3", &|m: &str| m.contains("mp4")).unwrap().itag, 140);
        assert!(audio_in(&fs, "fr.1", &all).is_none());
        // A single-track video: nothing to choose.
        let one = parse(&json!({"streamingData": {"adaptiveFormats": [fmt(251, "audio/webm; codecs=\"opus\"", "", 0, 1)]}}));
        assert!(audio_tracks(&one, &all).is_empty());
    }

    #[test]
    fn smoothness_rules() {
        let mut v = vec![
            fmt(303, "video/webm; codecs=\"vp9\"", "1080p60", 1080, 4_600_000),
            fmt(248, "video/webm; codecs=\"vp9\"", "1080p", 1080, 2_600_000),
            fmt(399, "video/mp4; codecs=\"av01.0.09M.08\"", "1080p60", 1080, 4_400_000),
            fmt(315, "video/webm; codecs=\"vp9\"", "2160p60", 2160, 26_000_000),
        ];
        v[1]["fps"] = 30.into();
        v[0]["fps"] = 60.into();
        v[2]["fps"] = 60.into();
        v[3]["fps"] = 60.into();
        let fs = parse(&json!({"streamingData": {"adaptiveFormats": v}}));
        let all = |_: &str| true;
        // 60 fps beats 30 fps at the same rung.
        assert_eq!(ladder(&fs, &order(None), &all).iter().map(|f| f.itag).collect::<Vec<_>>(), vec![315, 303]);
        // Power-efficient (hardware) AV1 beats software VP9; a non-smooth 4K rung is dropped.
        let caps = |i: u32| match i { 399 => Some((true, true)), 303 => Some((true, false)), 315 => Some((false, false)), _ => None };
        assert_eq!(ladder_with(&fs, &order(None), &all, &caps).iter().map(|f| f.itag).collect::<Vec<_>>(), vec![399]);
        assert_eq!(display_cap(1440.0, 600.0), 2304);
        assert_eq!(display_cap(1964.0, 900.0), 3142);
        assert_eq!(ahead_secs(4_600_000), 120.0);
        assert!((ahead_secs(26_000_000) - 30.77).abs() < 0.1);
        assert!(too_many_drops(20.0, 200.0) && !too_many_drops(10.0, 200.0) && !too_many_drops(50.0, 150.0));
    }

    #[test]
    fn dubbed_videos_keep_their_original_audio() {
        // Live 1 Nov 2026: on auto-dubbed videos every audio format has xtags
        // (language/content tags); only "drc" variants are to be avoided.
        let mut a = vec![fmt(303, "video/webm; codecs=\"vp9\"", "1080p60", 1080, 4_600_000)];
        a[0]["xtags"] = "Cg8KBHZpc28SB2RlZmF1bHQ".into();
        let mut orig = fmt(251, "audio/webm; codecs=\"opus\"", "", 0, 140_000);
        orig["xtags"] = "ChMKBWFjb250EgpvcmlnaW5hbAoKCgRsYW5nEgJlbg".into();
        orig["audioTrack"] = json!({"id": "en.4", "audioIsDefault": true});
        let mut dub = fmt(251, "audio/webm; codecs=\"opus\"", "", 0, 150_000);
        dub["xtags"] = "ChEKBWFjb250EghkdWJiZWQKCgoEbGFuZxICZGU".into();
        dub["audioTrack"] = json!({"id": "de.3", "audioIsDefault": false});
        let mut drc = fmt(251, "audio/webm; codecs=\"opus\"", "", 0, 160_000);
        drc["xtags"] = "CggKA2RyYxIBMQ".into();
        a.extend([orig, dub, drc]);
        let fs = parse(&json!({"streamingData": {"adaptiveFormats": a}}));
        let all = |_: &str| true;
        assert_eq!(ladder(&fs, &order(None), &all).len(), 1, "video xtags do not exclude video");
        assert_eq!(audio(&fs, &all).unwrap().bitrate, 140_000, "original track, not the dub or DRC");
        assert!(diagnose(&fs, &all).contains("drc 1"));
    }

    #[test]
    fn ladder_prefers_codec_and_respects_support() {
        let fs = parse(&sample());
        assert_eq!(fs.len(), 14, "the range-less entry is parsed, but only live rules take it");
        let all = |_: &str| true;
        let l = ladder(&fs, &order(None), &all);
        assert!(l.iter().all(|f| f.itag != 18));
        assert_eq!(l.iter().map(|f| f.itag).collect::<Vec<_>>(), vec![315, 303, 302, 243, 278]);
        let l = ladder(&fs, &order(Some(Codec::H264)), &all);
        assert_eq!(l.iter().map(|f| f.itag).collect::<Vec<_>>(), vec![315, 299, 136, 134, 278]);
        // No VP9 support → AV1 where available, else H.264.
        let no_vp9 = |m: &str| !m.contains("vp9");
        let l = ladder(&fs, &order(None), &no_vp9);
        assert_eq!(l.iter().map(|f| f.itag).collect::<Vec<_>>(), vec![399, 136, 134]);
        // Audio: best Opus that is not DRC or a dub; AAC when Opus is unsupported.
        assert_eq!(audio(&fs, &all).unwrap().itag, 251);
        assert_eq!(audio(&fs, &all).unwrap().bitrate, 143_000);
        assert_eq!(audio(&fs, &|m: &str| !m.contains("opus")).unwrap().itag, 140);
    }

    #[test]
    fn hdr_only_on_an_hdr_screen() {
        let f = parse(&json!({"streamingData": {"adaptiveFormats": [
            fmt(700, "video/mp4; codecs=\"av01.0.12M.10.0.110.09.16.09.0\"", "1440p60 HDR", 1440, 9_000_000),
            fmt(299, "video/mp4; codecs=\"avc1.64002A\"", "1080p60", 1080, 6_900_000),
            fmt(699, "video/mp4; codecs=\"av01.0.09M.10.0.110.09.16.09.0\"", "1080p60 HDR", 1080, 5_000_000),
        ]}}));
        let all = |_: &str| true;
        let none = |_: u32| None;
        let sdr = caps_for_screen(&f, false, &none);
        let l = ladder_with(&f, &DEFAULT_ORDER, &all, &sdr);
        assert_eq!(l.iter().map(|x| x.itag).collect::<Vec<_>>(), vec![299], "SDR screen: the HDR rungs go, 1080p SDR stays");
        let hdr = caps_for_screen(&f, true, &none);
        assert_eq!(ladder_with(&f, &DEFAULT_ORDER, &all, &hdr)[0].itag, 700, "HDR screen keeps HDR");
    }

    #[test]
    fn caps_start_rung_and_abr() {
        let fs = parse(&sample());
        let l = ladder(&fs, &order(None), &|_: &str| true);
        // 1080 device px tall player → 1080p; 534 px → 720p is within 20 %? 534*1.2=640 → 360p.
        assert_eq!(l[pick(&l, size_cap(1080.0)).unwrap()].p, 1080);
        assert_eq!(l[pick(&l, size_cap(600.0)).unwrap()].p, 720);
        assert_eq!(l[pick(&l, size_cap(534.0)).unwrap()].p, 360);
        assert_eq!(l[pick(&l, 100).unwrap()].p, 144, "nothing fits → lowest");
        let target = pick(&l, 1080).unwrap();
        assert_eq!(l[start_rung(&l, target)].itag, 243);
        assert_eq!(start_rung(&l, 3), 3);
        // ABR never drops below 1080p on throughput alone (the design floor)...
        assert_eq!(l[abr(&l, 1080, Some(5_000_000.0)).unwrap()].p, 1080);
        assert_eq!(l[abr(&l, 1080, Some(50_000_000.0)).unwrap()].p, 1080);
        assert_eq!(l[abr(&l, 1080, Some(10_000.0)).unwrap()].p, 1080);
        // ...but a lower size/stall cap still applies.
        assert_eq!(l[abr(&l, 720, Some(10_000.0)).unwrap()].p, 720);
        assert_eq!(l[abr(&l, 1080, None).unwrap()].p, 1080);
        let mut t = Throughput::default();
        t.add(1000, 1.0);
        assert!(t.bps.is_none(), "tiny responses ignored");
        t.add(1_000_000, 1000.0);
        t.add(1_000_000, 500.0);
        assert_eq!(t.bps.unwrap().round(), (8e6 * 0.7 + 16e6 * 0.3_f64).round());
    }

    #[test]
    fn vertical_and_hdr() {
        let mut v = fmt(617, "video/webm; codecs=\"vp09.02.51.10.01.09.16.09.00\"", "1080p HDR", 1920, 9_000_000);
        v["width"] = 1080.into();
        let sdr = fmt(248, "video/webm; codecs=\"vp9\"", "1080p", 1920, 3_000_000);
        let fs = parse(&json!({"streamingData": {"adaptiveFormats": [v, sdr]}}));
        assert_eq!(fs[0].p, 1080);
        assert!(fs[0].hdr && !fs[1].hdr);
        let l = ladder(&fs, &order(None), &|_: &str| true);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].itag, 248);
    }
}
