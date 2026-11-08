//! The Transcripts hooks with the feature compiled out (`./build.sh --tv`): nothing of it is in the TV bundle.

use crate::parse::Video;

pub const PREFIX: &str = "\u{0}";
pub fn boot() {}
pub fn on_storage(_key: &str, _json: &str) -> bool { false }
pub fn on_storage_change(_key: &str, _json: &str) -> bool { false }
pub fn on_timer(_id: u32) -> bool { false }
pub fn on_click(_act: &str, _arg: &str, _mods: u32) -> bool { false }
pub fn on_escape() -> bool { false }
pub fn on_live(_text: &str) {}
pub fn page_changed() {}
pub fn list_mark() -> u64 { 0 }
pub fn list_back(_mark: u64) {}
pub fn card_chip(_out: &mut String, _v: &Video) {}
pub fn playlist_chip(_out: &mut String, _id: &str) {}
pub fn watch_button(_out: &mut String) {}
pub fn channel_slot(_out: &mut String) {}
