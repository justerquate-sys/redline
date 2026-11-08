//! Browser-toolbar controls. Decisions, markup and preference edits stay in Rust.

use crate::{appearance::Patch, bridge, config::{self, Config}, player::sponsor, util::next_id};
use std::cell::{Cell, RefCell};

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static STATE: RefCell<Popup> = RefCell::new(Popup::default());
}

#[derive(Default)]
struct Popup {
    raw: String,
    pending: Patch,
    writing: Option<(u32, Patch)>,
    reading: bool,
    error: bool,
    counts: Option<Vec<u64>>,
    undo: bool,
    message: u32,
    #[cfg(feature = "transcribe")]
    export: u32,
}

#[cfg(feature = "transcribe")]
const EXPORT_QUERY: &str = "?file=export";
#[cfg(feature = "transcribe")]
const EXPORT_FILE: &str = "redline-settings.json";
/// Never in a settings file: the pairing token (a worker on any other machine pairs anew) and the Transcripts switch (the
/// worker sets it once the helper says Azure is configured).
#[cfg(feature = "transcribe")]
const EXPORT_SKIP: &str = r#"["pairingToken","ytcTranscriptsV1"]"#;

pub fn active() -> bool { ACTIVE.with(Cell::get) }

fn with(f: impl FnOnce(&mut Popup)) { STATE.with(|s| { if let Ok(mut s) = s.try_borrow_mut() { f(&mut s); } }); }

pub fn boot() {
    ACTIVE.with(|a| a.set(true));
    // The export's own tab: a toolbar popup closes when a save dialog takes the focus, and its download with it.
    #[cfg(feature = "transcribe")]
    if bridge::href().contains(EXPORT_QUERY) {
        return bridge::doc(&export_page());
    }
    bridge::doc(&html());
    bridge::storage_watch(config::STORAGE_KEY);
    #[cfg(feature = "transcribe")]
    bridge::storage_get(crate::features::transcribe::KEY);
    with(|s| s.read());
    refresh_status();
    bridge::timer(1, 1000);
}

impl Popup {
    fn view(&self) -> Config {
        let written = self.writing.map(|(_, p)| p.apply(&self.raw)).unwrap_or_else(|| self.raw.clone());
        Config::from_json(&self.pending.apply(&written))
    }

    fn read(&mut self) {
        if !self.reading && self.writing.is_none() {
            self.reading = true;
            bridge::storage_get(config::STORAGE_KEY);
        }
    }

    fn sync(&self) {
        let cfg = self.view();
        bridge::attr("popup-sponsor", "aria-checked", if cfg.prefs.sb_on { "true" } else { "false" });
        for cat in sponsor::SKIPPABLE {
            let id = format!("popup-cat-{}", cat.key());
            bridge::attr(&id, "aria-pressed", if cfg.prefs.sb_skip & cat.bit() != 0 { "true" } else { "false" });
            let count = self.counts.as_ref().and_then(|c| c.get(cat.idx())).copied().unwrap_or(0);
            bridge::text(&format!("{id}-count"), &if count > 0 { count.to_string() } else { String::new() });
        }
        bridge::attr("popup-undo", "data-visible", if self.undo { "true" } else { "false" });
        bridge::text("popup-status", if self.error { "Couldn’t save preferences. Try again." } else { "" });
    }
}

pub fn on_click(act: &str, arg: &str, keyboard: bool) -> bool {
    bridge::root_attr("data-ytc-popup-input", Some(if keyboard { "keyboard" } else { "pointer" }.into()));
    #[cfg(feature = "transcribe")]
    if act == "popup-transcripts-setup" {
        bridge::open_options();
        return true;
    }
    if act == "popup-undo" {
        message("sponsor-undo");
        return true;
    }
    #[cfg(feature = "transcribe")]
    if act == "popup-export" {
        bridge::open_tab(&format!("popup.html{EXPORT_QUERY}"));
        return true;
    }
    #[cfg(feature = "transcribe")]
    if act == "popup-export-save" {
        let id = next_id();
        with(|s| s.export = id);
        bridge::storage_export(id, EXPORT_FILE, EXPORT_SKIP);
        return true;
    }
    let mut handled = false;
    with(|s| {
        let cfg = s.view();
        let patch = match act {
            "sponsor-toggle" => Patch { sponsor_on: Some(!cfg.prefs.sb_on), ..Default::default() },
            "sponsor-category" => match sponsor::Cat::from_key(arg).filter(|c| *c != sponsor::Cat::Highlight) {
                Some(c) if cfg.prefs.sb_skip & c.bit() == 0 => Patch { skip_on: c.bit(), ..Default::default() },
                Some(c) => Patch { skip_off: c.bit(), ..Default::default() },
                None => return,
            },
            _ => return,
        };
        handled = true;
        s.error = false;
        s.pending.merge(patch);
        s.sync();
        s.read();
    });
    handled
}

/// Transcripts in the popup: for someone who set the feature up, its section (the extension pages' Wasm, loaded only
/// then); for anyone else one quiet row leading to settings. Neither talks to the helper from here.
#[cfg(feature = "transcribe")]
fn transcripts_section(raw: &str) {
    let on = crate::json::parse_str(raw).as_ref().and_then(|v| v.get("on")).and_then(crate::json::Value::as_bool).unwrap_or(false);
    if on {
        bridge::root_attr("data-tr-page", Some("popup".into()));
        bridge::load_script("transcripts-core.js");
    } else {
        bridge::html("popup-tr", r#"<button type="button" id="popup-tr-setup" data-act="popup-transcripts-setup"><span>Transcripts</span><small>Set up</small></button>"#);
    }
}

pub fn on_storage(key: &str, raw: &str) {
    #[cfg(feature = "transcribe")]
    if key == crate::features::transcribe::KEY { return transcripts_section(raw); }
    if key != config::STORAGE_KEY { return; }
    with(|s| {
        s.raw = raw.into();
        s.reading = false;
        if !s.pending.empty() && s.writing.is_none() {
            let patch = std::mem::take(&mut s.pending);
            let id = next_id();
            s.writing = Some((id, patch));
            bridge::storage_write(id, config::STORAGE_KEY, &patch.apply(&s.raw));
        }
        s.sync();
        // The controls now show the stored preferences (the markup's defaults until then).
        bridge::attr("ytc-popup", "data-ready", "true");
    });
}

pub fn on_storage_change(key: &str, raw: &str) {
    if key == config::STORAGE_KEY { with(|s| { s.raw = raw.into(); s.sync(); }); }
}

pub fn on_store_result(id: u32, ok: bool) {
    #[cfg(feature = "transcribe")]
    if with_export(id) {
        return bridge::text("popup-status", if ok { "Saved to your downloads. Keep it private." } else { "Couldn’t read the settings. Try again." });
    }
    with(|s| {
        let Some((pending_id, patch)) = s.writing else { return };
        if id != pending_id { return; }
        s.writing = None;
        if ok { s.raw = patch.apply(&s.raw); }
        s.error = !ok;
        s.sync();
        if !s.pending.empty() { s.read(); }
    });
    refresh_status();
}

fn message(name: &str) {
    let id = next_id();
    with(|s| s.message = id);
    bridge::active_message(id, name);
}

fn refresh_status() { message("sponsor-status"); }

pub fn on_message_result(id: u32, raw: &str) {
    let v = crate::json::parse_str(raw);
    with(|s| {
        if s.message != id { return; }
        s.counts = v.as_ref().and_then(|v| v.get("counts")).and_then(crate::json::Value::as_array)
            .filter(|a| a.len() == sponsor::SKIPPABLE.len()).map(|a| a.iter().map(|x| x.as_u64().unwrap_or(0).min(64)).collect());
        s.undo = v.as_ref().and_then(|v| v.get("undo")).and_then(crate::json::Value::as_bool).unwrap_or(false);
        s.sync();
    });
}

pub fn on_timer(id: u32) {
    if id == 1 {
        if bridge::visible() { refresh_status(); }
        bridge::timer(1, 1000);
    }
}

#[cfg(feature = "transcribe")]
fn with_export(id: u32) -> bool {
    let mut mine = false;
    with(|s| mine = s.export == id);
    mine
}

#[cfg(feature = "transcribe")]
fn export_page() -> String {
    let name = crate::render::NAME;
    format!(r#"<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Export all settings</title><link rel="stylesheet" href="client.css"><link rel="stylesheet" href="popup.css"></head><body id="ytc" class="popup popup-file"><main id="ytc-popup" data-ready="true"><header>{mark}<span>{name}</span></header><h1>Export all settings</h1><p class="popup-note">One file with everything {name} keeps in this browser: preferences, searches, resume positions, topics, blocked websites and filters.</p><p class="popup-note"><b>It also holds your Deepgram key, if you saved one.</b> Keep the file private, and delete it once you have imported it.</p><button type="button" id="popup-export-save" data-act="popup-export-save">Save file</button><p id="popup-status" role="status" aria-live="polite"></p></main></body>"#, mark = crate::render::POPUP_MARK)
}

fn html() -> String {
    let name = crate::render::NAME;
    let mut s = format!(r#"<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{name}</title><link rel="stylesheet" href="client.css"><link rel="stylesheet" href="popup.css"></head><body id="ytc" class="popup"><main id="ytc-popup"><header>{mark}<span>{name}</span></header>"#, mark = crate::render::POPUP_MARK);
    s.push_str(r#"<section aria-labelledby="sponsor-heading"><button type="button" id="popup-sponsor" class="popup-sponsor" data-act="sponsor-toggle" role="switch" aria-checked="true" aria-labelledby="sponsor-heading"><span><b id="sponsor-heading">SponsorSkip</b><small>Skip selected segments automatically</small></span><i class="popup-switch" aria-hidden="true"></i></button><details><summary>Skip categories</summary><div class="popup-categories" role="group" aria-label="Skip categories">"#);
    for c in sponsor::SKIPPABLE {
        let key = c.key();
        s.push_str(&format!(r#"<button type="button" id="popup-cat-{key}" data-act="sponsor-category" data-arg="{key}" aria-pressed="false"><i aria-hidden="true"><svg viewBox="0 0 16 16"><path d="m3 8 3 3 7-7"/></svg></i><span>{}</span><small id="popup-cat-{key}-count"></small></button>"#, c.label()));
    }
    s.push_str(r#"</div></details><button type="button" id="popup-undo" data-act="popup-undo" data-visible="false">Undo last skip</button><p class="popup-source">Segment data from SponsorBlock</p></section>"#);
    #[cfg(feature = "transcribe")]
    s.push_str(r#"<section id="popup-tr" aria-label="Transcripts"></section><button type="button" id="popup-export" data-act="popup-export">Export all settings</button>"#);
    s.push_str(r#"<p id="popup-status" role="status" aria-live="polite"></p></main></body>"#);
    s
}
