//! Videos known to be for a channel's members, beyond a card's own badge (`parse::Video::members`): the
//! labelled cards this tab has seen (so a card or autoplay target without the data is known too) and the
//! videos the player's verdict named when their card wasn't labelled. Labels the card from then on, goes
//! by the Members-only switch, never plays next (`upnext::next`). Nothing leaves the device: memory, and
//! the verdicts in this tab's `sessionStorage` (a reload keeps them, the tab's end forgets them). A video
//! that plays after all (a member's route, an early-access video gone public) is forgotten at once.

use super::shared;
use crate::util::is_video_id;
use std::collections::HashSet;

/// The label.
pub const LABEL: &str = "Members only";
/// `sessionStorage`: the learned ids, comma-separated.
pub const SESSION_KEY: &str = "ytc:members";
/// The newest verdicts kept.
const LEARNED_MAX: usize = 100;
/// Labelled cards remembered (starts over beyond this, as the app's own card memory does).
const SEEN_MAX: usize = 2000;

#[derive(Default)]
pub struct Known {
    /// Oldest first.
    learned: Vec<String>,
    seen: HashSet<String>,
    /// Played after all: a page kept in memory may still show the label.
    gone: Vec<String>,
}

impl Known {
    pub fn has(&self, id: &str) -> bool {
        self.seen.contains(id) || self.learned.iter().any(|x| x == id)
    }
}

pub fn has(id: &str) -> bool {
    shared(|s| s.members.has(id))
}

/// A labelled card arrived.
pub fn see(id: &str) {
    shared(|s| {
        if s.members.seen.len() >= SEEN_MAX {
            s.members.seen.clear();
        }
        s.members.seen.insert(id.to_string());
    });
}

/// The player said so.
pub fn learn(id: &str) {
    if !is_video_id(id) {
        return;
    }
    shared(|s| {
        let l = &mut s.members.learned;
        l.retain(|x| x != id);
        l.push(id.to_string());
        s.members.gone.retain(|x| x != id);
        if l.len() > LEARNED_MAX {
            let extra = l.len() - LEARNED_MAX;
            l.drain(..extra);
        }
    });
}

/// It played. True when it was known.
pub fn forget(id: &str) -> bool {
    shared(|s| {
        let known = s.members.has(id);
        s.members.learned.retain(|x| x != id);
        s.members.seen.remove(id);
        if known {
            let g = &mut s.members.gone;
            g.push(id.to_string());
            if g.len() > LEARNED_MAX {
                g.remove(0);
            }
        }
        known
    })
}

/// Forgotten since (`forget`).
pub fn forgotten(id: &str) -> bool {
    shared(|s| s.members.gone.iter().any(|x| x == id))
}

/// The learned ids for `sessionStorage`.
pub fn mirror() -> String {
    shared(|s| s.members.learned.join(","))
}

/// Reads `mirror`'s text (anything that isn't an id is dropped).
pub fn load(text: &str) {
    for id in text.split(',') {
        learn(id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learned_ids_are_bounded_and_mirrored() {
        assert!(!has("aaaaaaaaaaa"));
        learn("aaaaaaaaaaa");
        learn("aaaaaaaaaaa");
        learn("bad\"id");
        see("bbbbbbbbbbb");
        assert!(has("aaaaaaaaaaa") && has("bbbbbbbbbbb"));
        assert_eq!(mirror(), "aaaaaaaaaaa", "labelled cards aren't kept past the tab's memory; garbage never");
        assert!(forget("bbbbbbbbbbb"));
        assert!(!forget("bbbbbbbbbbb"));
        assert!(forgotten("bbbbbbbbbbb"));
        learn("bbbbbbbbbbb");
        assert!(!forgotten("bbbbbbbbbbb"), "known again");
        forget("bbbbbbbbbbb");
        assert!(forget("aaaaaaaaaaa"));
        assert_eq!(mirror(), "");
        for i in 0..LEARNED_MAX + 5 {
            learn(&format!("id{i:09}"));
        }
        let m = mirror();
        assert_eq!(m.split(',').count(), LEARNED_MAX);
        assert!(!has("id000000000") && has(&format!("id{:09}", LEARNED_MAX + 4)), "the oldest go");
        // What a reload reads back.
        let saved = m.clone();
        shared(|s| s.members = Known::default());
        load(&saved);
        load("bad\"id,,x");
        assert_eq!(mirror(), saved);
        shared(|s| s.members = Known::default());
    }
}
