//! Watch-page actions (like/dislike, subscribe, save to playlist, share,
//! chapters), comments, and the top bar's notifications and account menu.
//!
//! The app hands this module the parsed watch page, its clicks (`a-…`
//! actions), `data-hov` zones (Save chooser via `MenuCtl`), outside presses, timers, fetch responses and the
//! comments sentinel; everything else stays in `app.rs`. Actions are
//! optimistic: the control changes at once, at most one request per target is
//! in flight (`toggle.rs`), and a failure rolls back and says why. State lives
//! in memory only (nothing is persisted).

pub mod comments;
pub mod notify;
pub mod req;
pub mod save;
pub mod toggle;
pub mod watch;

use crate::bridge;
use crate::config::{Auth, Config};
use crate::parse::{Watch, text};
use crate::util::next_id;
use crate::json::Value;
use std::collections::HashMap;
use toggle::Toggle;
use crate::player::ui::{Do, MenuCtl};
use watch::{Acts, Like};

pub const SUB: &str = crate::render::SUBSCRIBE;
pub const ACT: &str = crate::render::ACTIONS;
pub const MSG: &str = "ytc-act-msg";
pub const CHAP: &str = "ytc-chap";
pub const CM: &str = "ytc-cm";
pub const CM_LIST: &str = "ytc-cm-list";
pub const CM_MORE: &str = "ytc-cm-more";
pub const ACCT: &str = "ytc-acct";
pub const TV_CM_BODY: &str = "ytc-tvcm-body";
const TV_SUB: &str = "ytc-tvsub";
const TV_SUB_B: &str = "ytc-tvsub-b";
const TV_MSG: &str = "ytc-tvact-msg";
/// TV: the notice over the picture or page (a refused action says why, whatever has the focus).
pub const TV_NOTE: &str = "ytc-tvnote";
/// TV: Subscribe in the player's row (its button id and its focused label), and on the channel page.
const TV_ROW_B: &str = "ytc-tvsubr-b";
const TV_ROW_L: &str = "ytc-tvsubr-l";
pub const TV_CH_SUB: &str = "ytc-chsub";
/// TV: the Save panel and its scrolling body.
pub const TV_SAVE: &str = "ytc-tvsave";
pub const TV_SAVE_BODY: &str = "ytc-tvsave-body";

/// Filled into `render::COMMENTS` (outside `#ytc-info`): chapters, then comments.
const BELOW: &str = "<div id=\"ytc-chap\"></div><div id=\"ytc-cm\" class=\"yc\" aria-label=\"Comments\"></div>";
const MSG_MS: u32 = 6000;
/// The status message folds back into its corner this fast, then empties.
const MSG_OUT_MS: u32 = 200;
/// "Copied" stays this long, then the Copy label morphs back.
const COPIED_MS: u32 = 1800;
const CONFIRM_MS: u32 = 3000;
/// TV: a press this soon after "Unsubscribe?" was shown cannot have read it.
const TV_CONFIRM_QUIET_MS: f64 = 400.0;
/// Comments start observing their sentinel when the main thread is idle (so
/// they never compete with the player's first requests).
const COMMENTS_IDLE_MS: u32 = 1500;
const STASH: usize = 4;
const LISTS: usize = 20;
/// The quiet membership check waits until the watch page has settled (player first).
const SAVE_PROBE_MS: u32 = 1200;
const SAVE_LABEL: &str = "ytc-save-l";
const NOTIF_FRESH_MS: f64 = 60_000.0;
const SIGN_IN: &str = "https://accounts.google.com/ServiceLogin?service=youtube&amp;passive=true&amp;continue=https%3A%2F%2Fwww.youtube.com%2F";

// ------------------------------------------------------------ JSON helpers

pub(crate) fn at<'a>(v: &'a Value<'a>, path: &[&str]) -> Option<&'a Value<'a>> {
    let mut cur = v;
    for p in path {
        cur = match cur {
            Value::Object(m) => m.get(*p)?,
            Value::Array(a) => a.get(p.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

pub(crate) fn st<'a>(v: &'a Value<'a>, path: &[&str]) -> Option<&'a str> {
    at(v, path)?.as_str()
}

pub(crate) fn arr<'a>(v: &'a Value<'a>, path: &[&str]) -> &'a [Value<'a>] {
    at(v, path).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

pub(crate) fn text_at(v: &Value, path: &[&str]) -> Option<String> {
    at(v, path).and_then(text)
}

/// Calls `f(key, value)` for every object member, depth-first (bounded depth).
pub(crate) fn walk<'a>(v: &'a Value<'a>, f: &mut impl FnMut(&str, &'a Value<'a>)) {
    fn go<'a>(v: &'a Value<'a>, f: &mut impl FnMut(&str, &'a Value<'a>), depth: u32) {
        if depth > 64 {
            return;
        }
        match v {
            Value::Object(m) => {
                for (k, x) in m {
                    f(k, x);
                    go(x, f, depth + 1);
                }
            }
            Value::Array(a) => a.iter().for_each(|x| go(x, f, depth + 1)),
            _ => {}
        }
    }
    go(v, f, 0)
}

/// The first value under key `k` anywhere in `v`.
pub(crate) fn find<'a>(v: &'a Value<'a>, k: &str) -> Option<&'a Value<'a>> {
    let mut out = None;
    walk(v, &mut |key, x| {
        if out.is_none() && key == k {
            out = Some(x);
        }
    });
    out
}

/// Every value under key `k`, in document order.
pub(crate) fn find_all<'a>(v: &'a Value<'a>, k: &str) -> Vec<&'a Value<'a>> {
    let mut out = Vec::new();
    walk(v, &mut |key, x| {
        if key == k {
            out.push(x);
        }
    });
    out
}

// ------------------------------------------------------------------- state

/// What the app needs to build requests (config, language, page URL).
pub struct Net<'a> {
    pub cfg: &'a Config,
    pub hl: &'a str,
    pub url: &'a str,
}

/// Result of an `a-…` click.
#[derive(Debug, PartialEq)]
pub enum Click {
    /// Done; prevent the default.
    Handled,
    /// Let the app follow the link (`href`) as usual.
    Pass,
    /// Not ours / nothing to do.
    Ignored,
}

enum Job {
    Like { v: String },
    Sub { ch: String },
    Comments { epoch: u32 },
    Replies { epoch: u32, slot: usize },
    /// `probe`: the quiet membership check after a watch page opened (fills the bookmark).
    Playlists { v: String, probe: bool },
    /// TV: what the account says about this video (`v`) or channel (`ch`, on its page).
    TvState { v: Option<String>, ch: Option<String> },
    Edit { v: String, pl: String },
    Create { v: String, title: String },
    Notifs,
    Record,
    Account,
}

enum Tm {
    /// `MenuCtl` timer for the Save chooser (its generation).
    Save(u32),
    Msg(u32),
    /// The message finished folding away: empty it (its generation).
    MsgGone(u32),
    /// The Copy button returns from "Copied" (its generation).
    Copied(u32),
    Confirm(u32),
    Comments(u32),
    /// Ask the video's playlist membership once the watch page has settled (its generation).
    SaveProbe(u32),
    /// TV: ask what the account says about this video once the watch page has settled (its generation).
    TvStateProbe(u32),
}

struct Rep {
    token: String,
    loaded: bool,
    /// A "Show more replies" slot (its button goes once used).
    more: bool,
}

/// Comments of one video.
#[derive(Default)]
struct Cm {
    v: String,
    epoch: u32,
    sort: usize,
    sorts: Vec<(String, String)>,
    count: String,
    /// Token of the first page of the current sort.
    first: Option<String>,
    cont: Option<String>,
    started: bool,
    busy: bool,
    shown: usize,
    /// Reply tokens by slot (`ytc-rp-{slot}`, button `ytc-rb-{slot}`).
    tokens: Vec<Rep>,
}

#[derive(Default)]
pub struct Actions {
    jobs: HashMap<u32, Job>,
    timers: HashMap<u32, Tm>,
    /// Current watch video ("" elsewhere).
    v: String,
    acts: Acts,
    likes: HashMap<String, Toggle<Like>>,
    subs: HashMap<String, Toggle<bool>>,
    confirm: u32,
    confirm_at: f64,
    share_open: bool,
    share_t: bool,
    share_at: u32,
    /// Save chooser: hover intent/grace and click/keyboard ownership (the
    /// player's reusable controller; menu name "save").
    save_ctl: MenuCtl,
    save_loading: bool,
    /// Generation of the scheduled membership probe (`Tm::SaveProbe`).
    probe_gen: u32,
    /// TV: the Save panel is open; the channel page is on screen (`acts` stands for its channel); the generation of the
    /// scheduled account check, and what was checked already (bounded, in memory).
    tv_save: bool,
    tv_channel: bool,
    tv_state_gen: u32,
    /// TV: the videos and channels the user changed here: what this device did is newer than any quiet answer.
    tv_changed: std::collections::HashSet<String>,
    tv_checked: std::collections::HashSet<String>,
    save_err: Option<String>,
    lists: Vec<(String, Vec<(save::Playlist, Toggle<bool>)>)>,
    cm: Option<Cm>,
    stash: Vec<Cm>,
    epoch: u32,
    slot_on: bool,
    count: u32,
    avatar: Option<String>,
    nt_open: bool,
    nt: notify::Menu,
    nt_loading: bool,
    nt_err: Option<String>,
    nt_at: f64,
    ac_open: bool,
    account: Option<notify::Account>,
    ac_loading: bool,
    /// Generations of the status message and of the Copy confirmation (own
    /// counters: other `next_gen` users must not strand them).
    msg_gen: u32,
    copy_gen: u32,
    /// Alternates the badge pulse / bell wiggle class so each new count replays it.
    ring: bool,
    /// The input being handled came from the keyboard: popovers open/close
    /// without motion (the motion law: keyboard actions are instant).
    kb: bool,
    /// Unknown comment items (debug overlay).
    pub unknown: crate::parse::Stats,
    /// Failed actions for the debug center: (kind, reason). Drained by the app.
    pub incidents: Vec<(&'static str, String)>,
}

fn signed_in() -> bool {
    crate::tv::auth().signed_in()
}

impl Actions {
    pub fn owns_fetch(&self, id: u32) -> bool {
        self.jobs.contains_key(&id)
    }

    pub fn owns_timer(&self, id: u32) -> bool {
        self.timers.contains_key(&id)
    }

    fn post(&mut self, net: &Net, (ep, body): req::Body, job: Job) -> u32 {
        let id = next_id();
        let mut auth = crate::tv::auth();
        // TV: comment pages continue the anonymous WEB `next` (their tokens are WEB tokens).
        if auth.bearer.is_some() && matches!(job, Job::Comments { .. } | Job::Replies { .. }) {
            auth = Auth::default();
        }
        let (body, headers) = crate::tv::finish(net.cfg, &auth, req::with_context(body, net.cfg, net.hl, net.url), (bridge::epoch() / 1000.0) as u64);
        if crate::ready::personalizes(ep) {
            // A like, subscription or playlist change: YouTube's next Home differs from the ready one.
            crate::ready::taint();
        }
        bridge::fetch(id, "POST", &req::url(ep), &body, &headers);
        self.jobs.insert(id, job);
        id
    }

    fn timer(&mut self, t: Tm, ms: u32) -> u32 {
        let id = next_id();
        self.timers.insert(id, t);
        bridge::timer(id, ms);
        id
    }

    fn next_gen(&mut self) -> u32 {
        self.epoch += 1;
        self.epoch
    }

    /// A short status next to the action row (cleared after a few seconds).
    /// `html` must already be escaped.
    fn say(&mut self, html: &str) {
        bridge::attr(MSG, "class", "ya-msg");
        bridge::html(MSG, html);
        if crate::tv::on() {
            bridge::html(TV_MSG, html);
            bridge::html(TV_NOTE, html);
        }
        self.msg_gen += 1;
        let g = self.msg_gen;
        self.timer(Tm::Msg(g), MSG_MS);
    }

    /// Records a failure for the debug center (no tokens or cookies in `reason`).
    fn report(&mut self, kind: &'static str, reason: String) {
        if self.incidents.len() < 20 {
            self.incidents.push((kind, reason));
        }
    }

    fn say_text(&mut self, s: &str) {
        self.say(&crate::util::esc(s));
    }

    fn need_sign_in(&mut self, what: &str) -> bool {
        if signed_in() {
            return false;
        }
        if crate::tv::on() {
            self.say(&format!("Sign in to {what} <button type=\"button\" class=\"tvb\" data-act=\"tv-signin\">Sign in with your phone</button>"));
            bridge::text(TV_NOTE, &format!("Sign in to {what}"));
        } else {
            self.say(&format!("Sign in to {what}. <a href=\"{SIGN_IN}\">Sign in</a>"));
        }
        true
    }

    // ------------------------------------------------------------ top bar

    /// Shell is in the DOM: show the bell and avatar when signed in.
    pub fn boot(&mut self) {
        // TV: the top-bar slot is the TV account control (app/tvglue.rs); no bell (its WEB endpoints
        // do not take the TV token).
        if signed_in() && !crate::tv::on() {
            self.slot_on = true;
            self.render_slot();
        }
    }

    /// Bell count and avatar from a page response's `topbar`.
    pub fn learn(&mut self, json: &Value) {
        self.learn_bar(notify::topbar(json));
    }

    /// Bell count and avatar (a page response's `topbar`, or a ready Home response's copy of it).
    pub fn learn_bar(&mut self, t: Option<notify::Topbar>) {
        let Some(t) = t else { return };
        if t.signed_out {
            return;
        }
        let count = if self.nt_open { 0 } else { t.count.unwrap_or(self.count) };
        let avatar = t.avatar.or_else(|| self.avatar.clone());
        if count != self.count || avatar != self.avatar || !self.slot_on {
            let in_place = self.slot_on && avatar == self.avatar;
            let old = self.count;
            self.count = count;
            self.avatar = avatar;
            self.slot_on = true;
            if in_place {
                self.badge(old);
            } else {
                self.render_slot();
            }
        }
    }

    /// The unread count changed under a rendered bell: the badge morphs in place
    /// (grows in from nothing, pulses on a new number, shrinks away at zero; the
    /// number itself is final at once) and the bell rings once when it went up.
    fn badge(&mut self, old: u32) {
        let n = self.count;
        self.ring = !self.ring;
        if n > 0 {
            bridge::text(notify::NT_N, &notify::count_label(n));
        }
        let cls = match (old, n) {
            (_, 0) => "yac-n z",
            (0, _) => "yac-n",
            _ if self.ring => "yac-n p1",
            _ => "yac-n p2",
        };
        bridge::attr(notify::NT_N, "class", cls);
        if n > old {
            bridge::attr(notify::NT_B, "class", if self.ring { "yac-b r1" } else { "yac-b r2" });
        }
        bridge::attr(notify::NT_B, "aria-label", &notify::bell_label(n));
    }

    fn nt_html(&self) -> String {
        notify::list_html(&self.nt.items, self.nt.ctoken.is_some(), self.nt.empty.as_deref(), self.nt_loading, self.nt_err.as_deref())
    }

    fn render_slot(&self) {
        if !self.slot_on {
            return;
        }
        let nt = self.nt_open.then(|| self.nt_html());
        let ac = self.ac_open.then(|| self.ac_html());
        bridge::html(ACCT, &notify::slot_html(self.count, self.avatar.as_deref(), nt.as_deref(), ac.as_deref()));
    }

    fn ac_html(&self) -> String {
        let mut s = notify::account_html(self.account.as_ref());
        if self.ac_loading && self.account.is_none() {
            s.insert_str(0, "<div class=\"ya-load\"></div>");
        }
        s
    }

    fn load_notifs(&mut self, net: &Net, more: bool) {
        if self.nt_loading {
            return;
        }
        self.nt_loading = true;
        self.nt_err = None;
        let token = if more { self.nt.ctoken.clone() } else { None };
        if !more {
            self.nt = notify::Menu::default();
        }
        self.post(net, req::notifications(token.as_deref()), Job::Notifs);
    }

    // --------------------------------------------------------- watch page

    /// A watch page's info was (re)rendered: fill the slots.
    pub fn on_watch(&mut self, v: &str, w: &Watch) {
        self.close_pops();
        self.v = v.to_string();
        self.acts = w.acts.clone();
        self.confirm = 0;
        self.share_t = false;
        self.seed();
        self.render_watch_slots();
        self.save_probe();
        self.tv_state_probe();
        if self.tv_save {
            self.tv_save_paint();
        }
        // `#ytc-comments` survives an info re-render of the same video.
        if self.cm.as_ref().is_none_or(|c| c.v != v) {
            self.start_comments();
        }
    }

    /// Back/forward restored a watch page's HTML from memory.
    pub fn on_restore(&mut self, v: &str, w: Option<&Watch>) {
        self.close_pops();
        self.v = v.to_string();
        if let Some(w) = w {
            self.acts = w.acts.clone();
        }
        self.seed();
        self.render_watch_slots();
        self.save_probe();
        self.tv_state_probe();
        match self.stash.iter().position(|c| c.v == v) {
            Some(i) => {
                let cm = self.stash.remove(i);
                let observe = !cm.started || (cm.cont.is_some() && !cm.busy);
                self.cm = Some(Cm { busy: false, ..cm });
                if observe {
                    bridge::observe(CM_MORE);
                }
            }
            None => self.start_comments(),
        }
    }

    /// Leaving the current page.
    pub fn leave(&mut self) {
        self.close_pops();
        if let Some(cm) = self.cm.take() {
            self.stash.retain(|c| c.v != cm.v);
            self.stash.push(cm);
            if self.stash.len() > STASH {
                self.stash.remove(0);
            }
        }
        self.v.clear();
        self.tv_save = false;
        self.tv_channel = false;
    }

    fn seed(&mut self) {
        let v = self.v.clone();
        if let Some(l) = self.acts.like {
            self.likes.entry(v).or_insert_with(|| Toggle::new(l));
        }
        if let Some(ch) = self.acts.channel_id.clone() {
            let s = self.acts.subscribed.unwrap_or(false);
            self.subs.entry(ch).or_insert_with(|| Toggle::new(s));
        }
        if self.likes.len() > 500 {
            self.likes.retain(|_, t| t.busy());
        }
    }

    fn render_watch_slots(&mut self) {
        self.render_sub();
        let like = self.like_state().map(|l| (l, self.like_count(l)));
        let row = watch::row_html(like.as_ref().map(|(l, c)| (*l, c.as_str())), &self.share_html(), &self.save_html());
        bridge::html(ACT, &row);
        if crate::tv::on() {
            self.update_like();
            self.tv_save_mark();
        }
    }

    fn save_open(&self) -> bool {
        self.save_ctl.open.is_some()
    }

    /// In any playlist (shown, optimistic) as far as known.
    fn save_saved(&self) -> bool {
        self.save_rows().is_some_and(|r| save::any_saved(&r))
    }

    /// The Save button's class and label (open, saved; a keyboard change has no motion).
    fn paint_save_btn(&self) {
        if self.v.is_empty() {
            return;
        }
        let saved = self.save_saved();
        bridge::attr(save::SAVE_B, "class", &save::btn_class(self.save_open(), saved, self.kb));
        bridge::text(SAVE_LABEL, save::label(saved));
        self.tv_save_mark();
    }

    /// Signed in: ask the playlist membership once the page has settled, so the bookmark shows
    /// whether the video is saved without opening the chooser (once per video, kept in memory).
    fn save_probe(&mut self) {
        if !signed_in() || self.save_rows().is_some() {
            return;
        }
        self.probe_gen += 1;
        let g = self.probe_gen;
        self.timer(Tm::SaveProbe(g), SAVE_PROBE_MS);
    }

    /// TV: the watch page's `next` is an anonymous call, so it knows nothing of the account: once the page has settled,
    /// ask as the account (once per video in this session; a player's requests go first).
    fn tv_state_probe(&mut self) {
        if !crate::tv::on() || !signed_in() || self.tv_checked.contains(&self.v) {
            return;
        }
        self.tv_state_gen += 1;
        let g = self.tv_state_gen;
        self.timer(Tm::TvStateProbe(g), SAVE_PROBE_MS);
    }

    fn tv_changed(&mut self, key: &str) {
        if crate::tv::on() {
            if self.tv_changed.len() > 500 {
                self.tv_changed.clear();
            }
            self.tv_changed.insert(key.to_string());
        }
    }

    fn tv_mark_checked(&mut self, key: String) {
        if self.tv_checked.len() > 500 {
            self.tv_checked.clear();
        }
        self.tv_checked.insert(key);
    }

    /// TV: the channel page was painted (also from memory): its Subscribe button, under the header. Signed out it goes to
    /// Sign in; signed in the state is the one every surface shares, and the account is asked once.
    pub fn tv_channel(&mut self, net: &Net, ch: &str, title: &str) {
        self.acts = Acts { channel_id: Some(ch.to_string()), channel: title.to_string(), subscribed: Some(false), ..Acts::default() };
        self.tv_channel = true;
        self.confirm = 0;
        self.subs.entry(ch.to_string()).or_insert_with(|| Toggle::new(false));
        if !signed_in() {
            bridge::html(TV_CH_SUB, "<button type=\"button\" class=\"tvb\" data-act=\"tv-signin\">Subscribe</button>");
            return;
        }
        if bridge::any(&format!("#{TV_CH_SUB} #{}", watch::SUB_B)) {
            // (A page restored from memory carries the button as it was.)
            self.update_sub();
        } else {
            let t = self.subs[ch];
            bridge::html(TV_CH_SUB, &watch::sub_html(t.shown(), t.busy(), false, title));
        }
        if !self.tv_checked.contains(ch) {
            self.tv_mark_checked(ch.to_string());
            self.post(net, req::account_channel(ch), Job::TvState { v: None, ch: Some(ch.to_string()) });
        }
    }

    /// TV: the Save panel opens: its rows at once when known (the quiet check usually ran), else they load. The Watch
    /// later row follows the player's Watch later action, which may have changed since.
    pub fn tv_save_open(&mut self, net: &Net) {
        self.tv_save = true;
        self.save_err = None;
        // Pressed before the page's data came: it waits as Loading, the page's quiet check fills it (`on_watch`).
        if self.v.is_empty() {
            self.tv_save_paint();
            return;
        }
        let v = self.v.clone();
        let wl = crate::features::shared(|s| s.saves.probed.contains_key(&v).then(|| s.saves.get(&v) == crate::features::watch_later::State::Saved));
        if let (Some(on), Some(l)) = (wl, self.list_mut(&v)) {
            if let Some((_, t)) = l.iter_mut().find(|(p, _)| p.id == "WL") {
                t.learn(on);
            }
        }
        if self.save_rows().is_none() && !self.save_loading {
            self.save_loading = true;
            self.post(net, req::playlists(&v), Job::Playlists { v, probe: false });
        }
        self.tv_save_paint();
    }

    pub fn tv_save_close(&mut self) {
        self.tv_save = false;
    }

    /// The panel's content; its first playlist takes the focus (the arrows then walk them).
    fn tv_save_paint(&self) {
        let rows = self.save_rows();
        let body = match (&rows, &self.save_err) {
            (Some(r), _) => save::Body::List(r),
            (None, Some(e)) => save::Body::Failed(e),
            (None, None) => save::Body::Loading,
        };
        bridge::html(TV_SAVE_BODY, &save::tv_inner(body));
        bridge::focus_sel(TV_SAVE, ".ya-opt");
    }

    fn like_state(&self) -> Option<Like> {
        self.likes.get(&self.v).map(Toggle::shown).filter(|_| self.acts.like.is_some())
    }

    fn like_count(&self, l: Like) -> String {
        watch::count_text(if l == Like::Like { &self.acts.liked_label } else { &self.acts.like_label }).to_string()
    }

    fn update_like(&self) {
        let Some(l) = self.like_state() else { return };
        let busy = self.likes.get(&self.v).is_some_and(Toggle::busy);
        bridge::attr("ytc-like", "class", &format!("{}{}", watch::like_class(l == Like::Like, false), if busy { " busy" } else { "" }));
        bridge::attr("ytc-like", "aria-pressed", if l == Like::Like { "true" } else { "false" });
        bridge::attr("ytc-dis", "class", watch::like_class(l == Like::Dislike, true));
        bridge::attr("ytc-dis", "aria-pressed", if l == Like::Dislike { "true" } else { "false" });
        bridge::text("ytc-like-n", &self.like_count(l));
        if crate::tv::on() {
            bridge::attr(crate::player::ui::TVLK, "aria-pressed", if l == Like::Like { "true" } else { "false" });
            bridge::attr(crate::player::ui::TVDL, "aria-pressed", if l == Like::Dislike { "true" } else { "false" });
            bridge::text(crate::player::ui::TVLK_L, if l == Like::Like { "Liked" } else { "Like" });
            bridge::text(crate::player::ui::TVDL_L, if l == Like::Dislike { "Disliked" } else { "Dislike" });
        }
    }

    fn render_sub(&self) {
        let html = match self.acts.channel_id.as_ref().and_then(|c| self.subs.get(c)) {
            Some(t) => watch::sub_html(t.shown(), t.busy(), self.confirm != 0, &self.acts.channel),
            None => String::new(),
        };
        bridge::html(SUB, &html);
        self.tv_info_refresh();
    }

    /// More info reuses the watch page's subscription state without duplicating the desktop ids.
    pub fn tv_info_refresh(&self) {
        if !crate::tv::on() {
            return;
        }
        let html = match self.acts.channel_id.as_ref().and_then(|c| self.subs.get(c)) {
            Some(t) if signed_in() => {
                let (cls, label, aria) = watch::sub_view(t.shown(), t.busy(), self.confirm != 0, &self.acts.channel);
                format!("<button type=\"button\" id=\"{TV_SUB_B}\" class=\"{cls}\" data-act=\"a-sub\" aria-pressed=\"{}\" aria-label=\"{}\">{label}</button>", t.shown(), crate::util::esc(&aria))
            }
            _ => String::new(),
        };
        bridge::html(TV_SUB, &html);
        let row = match self.acts.channel_id.as_ref().and_then(|c| self.subs.get(c)) {
            Some(t) if signed_in() => watch::tv_row_sub(TV_ROW_B, TV_ROW_L, t.shown(), t.busy(), self.confirm != 0, &self.acts.channel),
            _ => String::new(),
        };
        bridge::html(crate::player::ui::TVSUB, &row);
    }

    /// TV: a player was mounted (again, after a retry): the row's account actions from what is known.
    pub fn tv_row_refresh(&self) {
        self.tv_info_refresh();
        self.update_like();
        self.tv_save_mark();
    }

    /// TV: the Save action carries the bookmark's state.
    fn tv_save_mark(&self) {
        if crate::tv::on() {
            bridge::class_all(&format!("#{}", crate::player::ui::TVSV), "on", self.save_saved());
        }
    }

    /// A Subscribe state change: the same button, new attributes (its colors
    /// cross-fade in CSS; a re-render would snap and drop hover).
    fn update_sub(&self) {
        let Some(t) = self.acts.channel_id.as_ref().and_then(|c| self.subs.get(c)) else { return };
        let (cls, label, aria) = watch::sub_view(t.shown(), t.busy(), self.confirm != 0, &self.acts.channel);
        bridge::attr(watch::SUB_B, "class", &cls);
        bridge::attr(watch::SUB_B, "aria-pressed", if t.shown() { "true" } else { "false" });
        bridge::attr(watch::SUB_B, "aria-label", &aria);
        bridge::text(watch::SUB_B, label);
        if crate::tv::on() {
            bridge::attr(TV_SUB_B, "class", &cls);
            bridge::attr(TV_SUB_B, "aria-pressed", if t.shown() { "true" } else { "false" });
            bridge::attr(TV_SUB_B, "aria-label", &aria);
            bridge::text(TV_SUB_B, label);
            let (row, _) = watch::tv_row_sub_view(t.shown(), t.busy(), self.confirm != 0);
            bridge::attr(TV_ROW_B, "class", &row);
            bridge::attr(TV_ROW_B, "aria-pressed", if t.shown() { "true" } else { "false" });
            bridge::attr(TV_ROW_B, "aria-label", &aria);
            bridge::text(TV_ROW_L, label);
        }
    }

    /// Opens or closes a persistent popover by class (client.css grows it from
    /// its trigger and shrinks it back; instant from the keyboard).
    /// A pointer open or close morphs it out of / back into the trigger's shape
    /// (`morph_vars`; an open measures in `morph_from`, before its content is
    /// written), and `in` lets its rows settle in (client.css).
    fn pop(&self, btn: &str, pop: &str, base: &str, open: bool) {
        let mut cls = notify::pop_class(base, open, self.kb);
        if !self.kb {
            if open {
                cls.push_str(" in");
            } else {
                bridge::morph_vars(btn, pop);
            }
        }
        bridge::attr(pop, "class", &cls);
        bridge::attr(btn, "aria-expanded", if open { "true" } else { "false" });
    }

    /// A pointer open measures its trigger first: the read happens before the
    /// panel's new content exists, so that content's first style is computed
    /// with the panel already `open in` (its rows' entrance applies) and the
    /// layout read costs nothing extra.
    fn morph_from(&self, btn: &str, pop: &str) {
        if !self.kb {
            bridge::morph_vars(btn, pop);
        }
    }

    fn share_url(&self) -> String {
        watch::share_url(&self.v, self.share_t.then_some(self.share_at))
    }

    fn share_html(&self) -> String {
        watch::share_html(self.share_open, &self.share_url(), Some(self.share_at), self.share_t)
    }

    fn save_rows(&self) -> Option<Vec<(save::Playlist, bool, bool)>> {
        let (_, l) = self.lists.iter().find(|(k, _)| *k == self.v)?;
        Some(l.iter().map(|(p, t)| (p.clone(), t.shown(), t.busy())).collect())
    }

    fn save_html(&self) -> String {
        let rows = self.save_rows();
        let body = match (&rows, &self.save_err) {
            (Some(r), _) => save::Body::List(r),
            (None, Some(e)) => save::Body::Failed(e),
            (None, None) => save::Body::Loading,
        };
        save::html(self.save_open(), self.save_saved(), body, "")
    }

    fn save_body(&self) -> String {
        let rows = self.save_rows();
        let body = match (&rows, &self.save_err) {
            (Some(r), _) => save::Body::List(r),
            (None, Some(e)) => save::Body::Failed(e),
            (None, None) => save::Body::Loading,
        };
        save::inner(body, "")
    }

    /// The chooser's state: content rewritten while open, then the class flip
    /// (closing keeps the content for the exit motion).
    fn render_save(&self) {
        if self.v.is_empty() {
            return;
        }
        let open = self.save_open();
        if open {
            self.morph_from(save::SAVE_B, save::SAVE_P);
            bridge::html(save::SAVE_P, &self.save_body());
        }
        self.paint_save_btn();
        self.pop(save::SAVE_B, save::SAVE_P, "ya-pop", open);
    }

    /// New content for the open chooser (playlists arrived, one was created): no
    /// open/close. `settle`: the rows settle in as they arrive (first content);
    /// otherwise the rows already shown stay still.
    fn save_refresh(&self, settle: bool) {
        if self.v.is_empty() || !self.save_open() {
            return;
        }
        if !settle {
            bridge::attr(save::SAVE_P, "class", &notify::pop_class("ya-pop", true, false));
        }
        bridge::html(save::SAVE_P, &self.save_body());
    }

    fn send_like(&mut self, net: &Net, v: &str, to: Like) {
        let p = match to {
            Like::Like => self.acts.like_params.clone(),
            Like::Dislike => self.acts.dislike_params.clone(),
            Like::None => self.acts.removelike_params.clone(),
        }
        .filter(|_| self.v == v);
        let id = self.post(net, req::like(v, to, p.as_deref()), Job::Like { v: v.to_string() });
        if let Some(t) = self.likes.get_mut(v) {
            t.sent(id, to);
        }
    }

    fn send_sub(&mut self, net: &Net, ch: &str, on: bool) {
        let p = if on { self.acts.sub_params.clone() } else { self.acts.unsub_params.clone() };
        let p = p.filter(|_| self.acts.channel_id.as_deref() == Some(ch));
        let id = self.post(net, req::subscribe(ch, on, p.as_deref()), Job::Sub { ch: ch.to_string() });
        if let Some(t) = self.subs.get_mut(ch) {
            t.sent(id, on);
        }
    }

    fn send_edit(&mut self, net: &Net, v: &str, pl: &str, add: bool) {
        let id = self.post(net, req::edit_playlist(pl, v, add), Job::Edit { v: v.to_string(), pl: pl.to_string() });
        if let Some(t) = self.list_mut(v).and_then(|l| l.iter_mut().find(|(p, _)| p.id == pl)).map(|(_, t)| t) {
            t.sent(id, add);
        }
    }

    fn list_mut(&mut self, v: &str) -> Option<&mut Vec<(save::Playlist, Toggle<bool>)>> {
        self.lists.iter_mut().find(|(k, _)| k == v).map(|(_, l)| l)
    }

    fn update_pl(&self, i: usize) {
        let Some((_, l)) = self.lists.iter().find(|(k, _)| *k == self.v) else { return };
        let Some((_, t)) = l.get(i) else { return };
        let id = format!("ytc-pl-{i}");
        // A key toggles the box at once; a click draws the check in.
        let now = if self.kb { " now" } else { "" };
        bridge::attr(&id, "class", &format!("{}{now}", save::row_class(t.shown(), t.busy())));
        bridge::attr(&id, "aria-checked", if t.shown() { "true" } else { "false" });
        // The bookmark follows the chooser at once (optimistic), and back on a rollback.
        self.paint_save_btn();
    }

    // -------------------------------------------------------------- popups

    /// Applies a `MenuCtl` decision for the Save chooser.
    fn save_do(&mut self, net: &Net, d: Do, focus: bool) {
        match d {
            Do::None => {}
            Do::Wait(ms) => {
                let g = self.save_ctl.generation;
                self.timer(Tm::Save(g), ms);
            }
            Do::Open => {
                self.close_except("save");
                if self.save_rows().is_none() && !self.save_loading {
                    self.save_loading = true;
                    self.save_err = None;
                    let v = self.v.clone();
                    self.post(net, req::playlists(&v), Job::Playlists { v, probe: false });
                }
                self.render_save();
                if focus {
                    bridge::focus_in("ytc-save-p", 0);
                }
            }
            Do::Close => self.render_save(),
        }
    }

    /// Closes every open popup; true if one was open.
    fn close_pops(&mut self) -> bool {
        self.close_except("")
    }

    fn close_except(&mut self, keep: &str) -> bool {
        let mut any = false;
        if self.save_open() && keep != "save" {
            self.save_ctl.close();
            self.render_save();
            any = true;
        }
        if self.share_open && keep != "share" {
            self.share_open = false;
            if !self.v.is_empty() {
                self.pop(watch::SHARE_B, watch::SHARE_P, "ya-pop", false);
            }
            any = true;
        }
        if self.nt_open && keep != "nt" {
            self.nt_open = false;
            self.pop(notify::NT_B, notify::NT_P, notify::NT_CLASS, false);
            any = true;
        }
        if self.ac_open && keep != "ac" {
            self.ac_open = false;
            self.pop(notify::AC_B, notify::AC_P, notify::AC_CLASS, false);
            any = true;
        }
        any
    }

    /// Pointer pressed somewhere (`pop`: the `data-pop` it is inside, or "").
    pub fn on_press(&mut self, pop: &str) {
        self.kb = false;
        self.close_except(pop);
    }

    /// The pointer entered a `data-hov` zone: `sv` (the Save button), `svm`
    /// (its chooser) or anything else. Hover only opens and closes.
    pub fn on_hov(&mut self, net: &Net, zone: &str, held: bool) {
        self.kb = false;
        let z = match zone {
            "sv" if !self.v.is_empty() && signed_in() => ("tb", "save"),
            "svm" => ("menu", ""),
            _ => ("", ""),
        };
        let d = self.save_ctl.hover(z.0, z.1, held);
        self.save_do(net, d, false);
    }

    pub fn on_key(&mut self, key: &str) -> bool {
        self.kb = true;
        match key {
            "Escape" => {
                let save = self.save_open();
                let any = self.close_pops();
                if save {
                    // The closed chooser stays in the DOM: focus the button itself.
                    bridge::focus(save::SAVE_B);
                }
                any
            }
            "ArrowDown" | "ArrowUp" if self.save_open() => bridge::focus_grid("ytc-save-p", ".ya-opt", if key == "ArrowDown" { 1 } else { -1 }),
            _ => false,
        }
    }

    pub fn on_timer(&mut self, net: &Net, id: u32) {
        self.kb = false;
        match self.timers.remove(&id) {
            Some(Tm::SaveProbe(g)) => {
                if g == self.probe_gen && !self.v.is_empty() && !self.save_loading && self.save_rows().is_none() && signed_in() {
                    self.save_loading = true;
                    let v = self.v.clone();
                    self.post(net, req::playlists(&v), Job::Playlists { v, probe: true });
                }
            }
            Some(Tm::TvStateProbe(g)) => {
                if g == self.tv_state_gen && !self.v.is_empty() && signed_in() && !self.tv_checked.contains(&self.v) {
                    let v = self.v.clone();
                    self.tv_mark_checked(v.clone());
                    let ch = self.acts.channel_id.clone();
                    self.post(net, req::account_video(&v), Job::TvState { v: Some(v), ch });
                }
            }
            Some(Tm::Save(g)) => {
                let d = self.save_ctl.timer(g);
                self.save_do(net, d, false);
            }
            Some(Tm::Msg(g)) => {
                if g == self.msg_gen {
                    // Folds back into its corner (client.css), then empties.
                    bridge::attr(MSG, "class", "ya-msg out");
                    self.timer(Tm::MsgGone(g), MSG_OUT_MS);
                }
            }
            Some(Tm::MsgGone(g)) => {
                if g == self.msg_gen {
                    bridge::html(MSG, "");
                    bridge::attr(MSG, "class", "ya-msg");
                    if crate::tv::on() {
                        bridge::html(TV_MSG, "");
                        bridge::html(TV_NOTE, "");
                    }
                }
            }
            Some(Tm::Copied(g)) => {
                if g == self.copy_gen {
                    bridge::attr("ytc-copy", "class", "ya-cp");
                    bridge::attr("ytc-copy", "aria-label", "Copy link");
                }
            }
            Some(Tm::Confirm(g)) => {
                if g == self.confirm {
                    self.confirm = 0;
                    self.update_sub();
                }
            }
            Some(Tm::Comments(g)) => {
                if self.cm.as_ref().is_some_and(|c| c.epoch == g) {
                    bridge::observe(CM_MORE);
                }
            }
            None => {}
        }
    }

    // -------------------------------------------------------------- clicks

    /// `now`: the player's current time (for "Start at").
    /// `kbd`: activated from the keyboard (Enter/Space).
    pub fn on_click(&mut self, net: &Net, act: &str, arg: &str, now: f64, kbd: bool) -> Click {
        let idx = arg.parse::<usize>().ok();
        self.kb = kbd;
        match act {
            "a-like" | "a-dislike" => {
                let v = self.v.clone();
                if v.is_empty() || self.need_sign_in("rate videos") {
                    return Click::Handled;
                }
                self.tv_changed(&v);
                let me = if act == "a-like" { Like::Like } else { Like::Dislike };
                let Some(t) = self.likes.get_mut(&v) else { return Click::Handled };
                let to = if t.shown() == me { Like::None } else { me };
                if let Some(s) = t.want(to) {
                    self.send_like(net, &v, s);
                }
                self.update_like();
            }
            "a-sub" => {
                let Some(ch) = self.acts.channel_id.clone() else { return Click::Handled };
                if self.need_sign_in("subscribe") {
                    return Click::Handled;
                }
                self.tv_changed(&ch);
                if crate::tv::on() && self.confirm != 0 && bridge::now() - self.confirm_at < TV_CONFIRM_QUIET_MS {
                    return Click::Handled;
                }
                let Some(t) = self.subs.get_mut(&ch) else { return Click::Handled };
                if t.shown() && self.confirm == 0 {
                    // Unsubscribing is deliberate: a second click confirms.
                    let g = self.next_gen();
                    self.confirm = g;
                    self.confirm_at = bridge::now();
                    self.timer(Tm::Confirm(g), CONFIRM_MS);
                } else {
                    self.confirm = 0;
                    let to = !t.shown();
                    if let Some(s) = t.want(to) {
                        self.send_sub(net, &ch, s);
                    }
                }
                self.update_sub();
            }
            "a-save" => {
                if self.v.is_empty() || self.need_sign_in("save videos") {
                    return Click::Handled;
                }
                // A click on a hover-opened chooser pins it; otherwise toggles.
                let d = self.save_ctl.click("save", kbd);
                self.save_do(net, d, kbd);
            }
            "a-pl" => {
                let (Some(i), v) = (idx, self.v.clone()) else { return Click::Handled };
                let Some((pl, to)) = self.list_mut(&v).and_then(|l| l.get_mut(i)).map(|(p, t)| (p.id.clone(), !t.shown())) else { return Click::Handled };
                let send = self.list_mut(&v).and_then(|l| l.get_mut(i)).and_then(|(_, t)| t.want(to));
                if let Some(s) = send {
                    self.send_edit(net, &v, &pl, s);
                }
                self.update_pl(i);
                self.save_ctl.chose(kbd);
                bridge::text("ytc-save-m", "");
            }
            "a-share" => {
                let open = !self.share_open;
                self.close_pops();
                self.share_open = open;
                self.share_at = now.max(0.0) as u32;
                if open {
                    self.morph_from(watch::SHARE_B, watch::SHARE_P);
                    bridge::html(watch::SHARE_P, &watch::share_inner(&self.share_url(), Some(self.share_at), self.share_t));
                }
                self.pop(watch::SHARE_B, watch::SHARE_P, "ya-pop", open);
                if open {
                    bridge::focus("ytc-share-u");
                }
            }
            "a-share-t" => {
                self.share_t = !self.share_t;
                bridge::attr("ytc-share-t", "class", &format!("ya-opt{}{}", if self.share_t { " on" } else { "" }, if kbd { " now" } else { "" }));
                bridge::attr("ytc-share-t", "aria-checked", if self.share_t { "true" } else { "false" });
                bridge::value("ytc-share-u", &self.share_url());
            }
            "a-copy" => {
                let ok = bridge::copy("ytc-share-u");
                // "Copy" morphs into "✓ Copied" (one persistent button), and back after a moment.
                bridge::text("ytc-copied", if ok { "Copied" } else { "Press ⌘C" });
                bridge::attr("ytc-copy", "class", if kbd { "ya-cp done now" } else { "ya-cp done" });
                bridge::attr("ytc-copy", "aria-label", if ok { "Link copied" } else { "Press ⌘C to copy" });
                self.copy_gen += 1;
                let g = self.copy_gen;
                self.timer(Tm::Copied(g), COPIED_MS);
            }
            "a-csort" => {
                if let Some(i) = idx {
                    self.sort_comments(net, i);
                }
            }
            "a-rep" => {
                if let Some(i) = idx {
                    self.replies(net, i);
                }
            }
            "a-cretry" => {
                if let Some(cm) = self.cm.as_mut() {
                    cm.busy = false;
                    bridge::attr(CM_MORE, "class", "more");
                    bridge::html(CM_MORE, "");
                    bridge::observe(CM_MORE);
                }
            }
            "a-nt" => {
                let open = !self.nt_open;
                self.close_pops();
                if open {
                    self.nt_open = true;
                    // Opening the list clears the badge (as on YouTube).
                    self.count = 0;
                    if self.nt.items.is_empty() || bridge::now() - self.nt_at > NOTIF_FRESH_MS || self.nt_err.is_some() {
                        self.load_notifs(net, false);
                    }
                    // The badge shrinks away while the panel grows out of the bell.
                    bridge::attr(notify::NT_N, "class", "yac-n z");
                    bridge::attr(notify::NT_B, "aria-label", "Notifications");
                    self.morph_from(notify::NT_B, notify::NT_P);
                    bridge::html(notify::NT_P, &self.nt_html());
                    self.pop(notify::NT_B, notify::NT_P, notify::NT_CLASS, true);
                }
            }
            "a-ntmore" => {
                // The rows already shown stay still (no second entrance).
                bridge::attr(notify::NT_P, "class", &notify::pop_class(notify::NT_CLASS, true, false));
                let more = self.nt.ctoken.is_some();
                self.load_notifs(net, more);
                bridge::html("ytc-nt-p", &self.nt_html());
            }
            "a-ntc" => {
                if let Some(n) = idx.and_then(|i| self.nt.items.get_mut(i)) {
                    n.unread = false;
                    if let Some(r) = n.record.clone() {
                        self.post(net, req::record_interaction(&r), Job::Record);
                    }
                }
                self.close_pops();
                return Click::Pass;
            }
            "a-ac" => {
                let open = !self.ac_open;
                self.close_pops();
                if open {
                    self.ac_open = true;
                    if self.account.is_none() && !self.ac_loading {
                        self.ac_loading = true;
                        self.post(net, req::account_menu(), Job::Account);
                    }
                    self.morph_from(notify::AC_B, notify::AC_P);
                    bridge::html(notify::AC_P, &self.ac_html());
                    self.pop(notify::AC_B, notify::AC_P, notify::AC_CLASS, true);
                }
            }
            "a-go" => {
                let ok = (arg.starts_with('/') && !arg.starts_with("//")) || arg == "https://myaccount.google.com/";
                if ok && !arg.bytes().any(|b| b <= b' ' || b == b'\\') {
                    self.close_pops();
                    bridge::go(arg);
                }
            }
            _ => return Click::Ignored,
        }
        Click::Handled
    }

    /// Forms with ids starting `ytc-a-`.
    pub fn on_submit(&mut self, net: &Net, form: &str, value: &str) -> bool {
        if form != "ytc-a-newpl" {
            return false;
        }
        let title: String = value.trim().chars().take(150).collect();
        if title.is_empty() || self.v.is_empty() {
            return true;
        }
        bridge::text("ytc-save-m", "Creating…");
        let v = self.v.clone();
        self.post(net, req::create_playlist(&title, &v), Job::Create { v, title });
        true
    }

    // ------------------------------------------------------------ comments

    /// Explicit TV drawer opening: begin the first page, otherwise resume ordinary sentinel paging.
    pub fn tv_comments_open(&mut self, net: &Net) {
        if self.cm.as_ref().is_some_and(|cm| !cm.started) {
            self.on_visible(net, CM_MORE);
        } else if self.cm.as_ref().is_some_and(|cm| cm.cont.is_some() && !cm.busy) {
            bridge::observe(CM_MORE);
        }
    }

    fn start_comments(&mut self) {
        let tv = crate::tv::on();
        bridge::html(if tv { TV_CM_BODY } else { crate::render::COMMENTS }, BELOW);
        bridge::html(CHAP, &watch::chapters_html(&self.v, &self.acts.chapters));
        let a = &self.acts;
        let mut cm = Cm { v: self.v.clone(), epoch: 0, sorts: a.sorts.clone(), count: a.comment_count.clone(), first: a.comments.clone(), ..Default::default() };
        if let Some(off) = a.comments_off.clone() {
            bridge::html(CM, &comments::notice_html(&off, false));
            self.cm = None;
            return;
        }
        if cm.first.is_none() {
            bridge::html(CM, if tv { "<p class=\"yc-note\">Comments are not available for this video</p>" } else { "" });
            self.cm = None;
            return;
        }
        cm.epoch = self.next_gen();
        bridge::html(CM, &comments::frame_html(&cm.count, &cm.sorts, 0));
        bridge::mark_slide(comments::SORT, &comments::sort_id(0), "", "ycsk", false);
        let g = cm.epoch;
        self.cm = Some(cm);
        // TV comments are a deliberate drawer action. Its hidden sentinel must never compete with playback.
        if tv {
            return;
        }
        let id = next_id();
        self.timers.insert(id, Tm::Comments(g));
        bridge::idle(id, COMMENTS_IDLE_MS);
    }

    /// Top/Newest: the tabs switch at once (persistent buttons, their colors
    /// transition); the shown list stays, dimmed, until the new first page
    /// replaces it, so the section never collapses under the reader.
    fn sort_comments(&mut self, net: &Net, i: usize) {
        let g = self.next_gen();
        let Some(cm) = self.cm.as_mut() else { return };
        let Some((_, token)) = cm.sorts.get(i).cloned() else { return };
        if i == cm.sort && !cm.started {
            return;
        }
        *cm = Cm { v: cm.v.clone(), epoch: g, sort: i, sorts: std::mem::take(&mut cm.sorts), count: std::mem::take(&mut cm.count), first: Some(token), ..Default::default() };
        for k in 0..cm.sorts.len() {
            let id = comments::sort_id(k);
            bridge::attr(&id, "class", if k == i { "on" } else { "" });
            bridge::attr(&id, "aria-pressed", if k == i { "true" } else { "false" });
        }
        // The accent pill springs to the new tab (pointer); a keyboard switch moves it at once.
        bridge::mark_slide(comments::SORT, &comments::sort_id(i), "", "ycsk", !self.kb);
        bridge::attr(CM_LIST, "class", "stale");
        bridge::attr(CM_MORE, "class", "more");
        bridge::html(CM_MORE, "");
        // Load now: the sentinel may sit far below the old list.
        self.on_visible(net, CM_MORE);
    }

    /// The comments sentinel is near the viewport: load the next page.
    pub fn on_visible(&mut self, net: &Net, id: &str) -> bool {
        if id != CM_MORE {
            return false;
        }
        if crate::tv::on() && !bridge::any("#ytc-comments.on") {
            return true;
        }
        let Some(cm) = self.cm.as_mut() else { return true };
        if cm.busy || cm.v != self.v {
            return true;
        }
        let token = if cm.started { cm.cont.clone() } else { cm.first.clone() };
        let Some(token) = token else { return true };
        cm.busy = true;
        let epoch = cm.epoch;
        self.post(net, req::comments(&token), Job::Comments { epoch });
        true
    }

    fn replies(&mut self, net: &Net, slot: usize) {
        let Some(cm) = self.cm.as_mut() else { return };
        let epoch = cm.epoch;
        let Some(rep) = cm.tokens.get_mut(slot) else { return };
        let rp = format!("ytc-rp-{slot}");
        let rb = format!("ytc-rb-{slot}");
        if rep.more {
            bridge::attr(&rb, "hidden", "");
        }
        if rep.loaded {
            let hidden = bridge::toggle(&rp, "hide");
            bridge::attr(&rb, "aria-expanded", if hidden { "false" } else { "true" });
            return;
        }
        rep.loaded = true;
        let token = rep.token.clone();
        bridge::html(&rp, "<div class=\"ya-load\"></div>");
        bridge::attr(&rb, "aria-expanded", "true");
        self.post(net, req::comments(&token), Job::Replies { epoch, slot });
    }

    /// Markup for comments; reply tokens get slots.
    fn comments_html(cm: &mut Cm, list: &[comments::Comment]) -> String {
        let mut s = String::with_capacity(list.len() * 900);
        for c in list {
            let slot = c.replies.as_ref().map(|(_, t)| {
                cm.tokens.push(Rep { token: t.clone(), loaded: false, more: false });
                cm.tokens.len() - 1
            });
            comments::comment_html(&mut s, c, &cm.v, slot);
        }
        s
    }

    fn on_comments(&mut self, epoch: u32, res: Result<Value, String>) {
        let Some(cm) = self.cm.as_mut().filter(|c| c.epoch == epoch) else { return };
        cm.busy = false;
        let page = match res {
            Ok(j) => comments::page(&j, &cm.v),
            Err(r) => {
                let msg = format!("comments {} (sort {}, page {}): {r}", cm.v, cm.sort, if cm.started { "next" } else { "first" });
                bridge::attr(CM_MORE, "class", "yc-note");
                if !cm.started {
                    // A failed new sort: the old list is not its answer.
                    bridge::html(CM_LIST, "");
                    bridge::attr(CM_LIST, "class", "");
                }
                // A request cut off by leaving the page is not a bug.
                if !r.starts_with("Network error") {
                    self.report("comments", msg);
                }
                bridge::html(CM_MORE, &comments::notice_html(&format!("Couldn't load comments. {r}"), true));
                return;
            }
        };
        self.unknown.merge(&page.stats);
        if !cm.started {
            cm.started = true;
            let mut redraw = false;
            if let Some(c) = page.count.clone().filter(|c| *c != cm.count) {
                cm.count = c;
                redraw = true;
            }
            if cm.sorts.is_empty() && !page.sorts.is_empty() {
                cm.sorts = page.sorts.clone();
                redraw = true;
            }
            if redraw {
                bridge::html(CM, &comments::frame_html(&cm.count, &cm.sorts, cm.sort));
                bridge::mark_slide(comments::SORT, &comments::sort_id(cm.sort), "", "ycsk", false);
            }
        }
        let first = cm.shown == 0;
        let html = Self::comments_html(cm, &page.comments);
        if first {
            // Replaces a stale list after Top/Newest (an empty one otherwise).
            bridge::html(CM_LIST, &html);
            bridge::attr(CM_LIST, "class", "");
        } else if !html.is_empty() {
            bridge::append(CM_LIST, &html);
        }
        cm.shown += page.comments.len();
        // A page without comments ends paging (prevents request loops).
        cm.cont = if page.comments.is_empty() { None } else { page.cont };
        if cm.cont.is_some() {
            bridge::observe(CM_MORE);
        } else if cm.shown > 0 {
            // Comment list end: only when top-level paging is complete.
            bridge::attr(CM_MORE, "class", "yc-end");
            bridge::text(CM_MORE, "End");
        } else {
            bridge::attr(CM_MORE, "class", "more done");
            bridge::html(CM_LIST, &comments::notice_html(page.notice.as_deref().unwrap_or("No comments yet."), false));
        }
    }

    fn on_replies(&mut self, epoch: u32, slot: usize, res: Result<Value, String>) {
        let Some(cm) = self.cm.as_mut().filter(|c| c.epoch == epoch) else { return };
        let rp = format!("ytc-rp-{slot}");
        let page = match res {
            Ok(j) => comments::page(&j, &cm.v),
            Err(r) => {
                let msg = format!("replies {}: {r}", cm.v);
                if let Some(t) = cm.tokens.get_mut(slot) {
                    t.loaded = false;
                }
                self.report("comments", msg);
                let reason = format!("Couldn't load replies. {r}");
                let html = if crate::tv::on() { comments::reply_error_html(&reason, slot) } else { comments::notice_html(&reason, false) };
                bridge::html(&rp, &html);
                return;
            }
        };
        self.unknown.merge(&page.stats);
        let mut html = Self::comments_html(cm, &page.comments);
        if let Some(t) = page.cont.filter(|_| !page.comments.is_empty()) {
            cm.tokens.push(Rep { token: t, loaded: false, more: true });
            let j = cm.tokens.len() - 1;
            html.push_str(&format!("<button type=\"button\" id=\"ytc-rb-{j}\" class=\"ycrb\" data-act=\"a-rep\" data-arg=\"{j}\">Show more replies</button><div id=\"ytc-rp-{j}\" class=\"ycr\"></div>"));
        }
        bridge::html(&rp, &html);
    }

    // ----------------------------------------------------------- responses

    /// Returns a confirmed Watch later membership change for the app's shared card state/cache.
    pub fn on_fetch(&mut self, net: &Net, id: u32, status: u32, body: &[u8]) -> Option<(String, bool)> {
        let Some(job) = self.jobs.remove(&id) else { return None };
        let res = req::outcome(status, body);
        let mut watch_later = None;
        match job {
            Job::Like { v } => {
                let ok = res.is_ok();
                let next = self.likes.get_mut(&v).and_then(|t| t.done(id, ok));
                if let Some(n) = next {
                    self.send_like(net, &v, n);
                }
                if let Err(r) = &res {
                    self.report("action-like", format!("rating {v}: {r}"));
                }
                if v == self.v {
                    self.update_like();
                    if let Err(r) = res {
                        self.say_text(&format!("Rating not saved. {r}"));
                    }
                }
            }
            Job::Sub { ch } => {
                let on = self.subs.get(&ch).and_then(|t| t.inflight).map(|x| x.1).unwrap_or(true);
                let ok = res.as_ref().is_ok_and(|j| req::subscribe_ok(j, on));
                let next = self.subs.get_mut(&ch).and_then(|t| t.done(id, ok));
                if let Some(n) = next {
                    self.send_sub(net, &ch, n);
                }
                if !ok {
                    let r = res.as_ref().err().cloned().unwrap_or_else(|| req::failure(299, None));
                    self.report("action-subscribe", format!("{} {ch}: {r}", if on { "subscribe" } else { "unsubscribe" }));
                }
                if self.acts.channel_id.as_deref() == Some(ch.as_str()) && (!self.v.is_empty() || self.tv_channel) {
                    self.update_sub();
                    if !ok {
                        let r = res.err().unwrap_or_else(|| req::failure(299, None));
                        self.say_text(&format!("{} not saved. {r}", if on { "Subscription" } else { "Unsubscribe" }));
                    }
                }
            }
            // A quiet check: a failure or a shape it cannot read changes nothing.
            Job::TvState { v, ch } => {
                if let Ok(j) = res {
                    let (like, sub) = watch::account_state(&j, ch.as_deref());
                    if let (Some(v), Some(l)) = (v.as_ref().filter(|v| !self.tv_changed.contains(*v)), like) {
                        if self.likes.get_mut(v).is_some_and(|t| t.learn(l)) && *v == self.v {
                            self.update_like();
                        }
                    }
                    if let (Some(c), Some(s)) = (ch.as_ref().filter(|c| !self.tv_changed.contains(*c)), sub) {
                        if self.subs.get_mut(c).is_some_and(|t| t.learn(s)) && self.acts.channel_id.as_deref() == Some(c.as_str()) && (!self.v.is_empty() || self.tv_channel) {
                            self.update_sub();
                        }
                    }
                }
            }
            Job::Playlists { v, probe } => {
                self.save_loading = false;
                // (TV: the panel opened while the quiet check was out: it waits for this answer and reports it.)
                let probe = probe && !(self.tv_save && v == self.v);
                match res.map(|j| save::parse(&j)) {
                    Ok(Some(list)) => {
                        self.lists.retain(|(k, _)| *k != v);
                        self.lists.push((v.clone(), list.into_iter().map(|p| {
                            let t = Toggle::new(p.contains);
                            (p, t)
                        }).collect()));
                        if self.lists.len() > LISTS {
                            self.lists.remove(0);
                        }
                    }
                    // A failed quiet probe stays quiet: opening the chooser asks again and reports.
                    Ok(None) if probe => {}
                    Err(_) if probe => {}
                    Ok(None) => {
                        self.save_err = Some("Couldn't read your playlists.".into());
                        self.report("action-save", format!("get_add_to_playlist {v}: unrecognized response"));
                    }
                    Err(r) => {
                        self.report("action-save", format!("get_add_to_playlist {v}: {r}"));
                        self.save_err = Some(r);
                    }
                }
                if v == self.v {
                    self.paint_save_btn();
                }
                if v == self.v && self.save_open() {
                    self.save_refresh(true);
                    if self.save_ctl.src == crate::player::ui::Src::Key {
                        bridge::focus_in("ytc-save-p", 0);
                    }
                }
                if v == self.v && self.tv_save {
                    self.tv_save_paint();
                }
                // (The panel opened on this video while the answer for another was out: now its own list.)
                if self.tv_save && v != self.v && !self.v.is_empty() && !self.save_loading && self.save_rows().is_none() {
                    self.save_loading = true;
                    self.save_err = None;
                    let cur = self.v.clone();
                    self.post(net, req::playlists(&cur), Job::Playlists { v: cur, probe: false });
                }
            }
            Job::Edit { v, pl } => {
                let add = self.list_mut(&v).and_then(|l| l.iter().find(|(p, _)| p.id == pl)).and_then(|(_, t)| t.inflight).map(|x| x.1).unwrap_or(true);
                let ok = res.as_ref().is_ok_and(req::edit_ok);
                if ok && pl == "WL" { watch_later = Some((v.clone(), add)); }
                let next = self.list_mut(&v).and_then(|l| l.iter_mut().find(|(p, _)| p.id == pl)).and_then(|(_, t)| t.done(id, ok));
                if let Some(n) = next {
                    self.send_edit(net, &v, &pl, n);
                }
                if !ok {
                    let r = res.as_ref().err().cloned().unwrap_or_else(|| req::failure(299, None));
                    self.report("action-save", format!("edit_playlist {} {v}: {r}", if add { "add" } else { "remove" }));
                }
                if v == self.v && (self.save_open() || self.tv_save) {
                    if let Some(i) = self.lists.iter().find(|(k, _)| *k == v).and_then(|(_, l)| l.iter().position(|(p, _)| p.id == pl)) {
                        self.update_pl(i);
                    }
                    if !ok {
                        let r = res.err().unwrap_or_else(|| req::failure(299, None));
                        bridge::text("ytc-save-m", &format!("{} not saved. {r}", if add { "Adding" } else { "Removing" }));
                    }
                }
            }
            Job::Create { v, title } => {
                let pid = res.as_ref().ok().and_then(|j| st(j, &["playlistId"])).filter(|p| crate::util::is_safe_id(p)).map(String::from);
                match pid {
                    Some(id) => {
                        if let Some(l) = self.list_mut(&v) {
                            l.insert(0, (save::Playlist { id, title, privacy: "Private".into(), contains: true }, Toggle::new(true)));
                        }
                        if v == self.v {
                            self.paint_save_btn();
                        }
                        if v == self.v && self.save_open() {
                            self.save_refresh(false);
                            bridge::text("ytc-save-m", "Saved to the new playlist.");
                        }
                    }
                    None => {
                        let r = res.err().unwrap_or_else(|| req::failure(299, None));
                        self.report("action-save", format!("playlist/create {v}: {r}"));
                        bridge::text("ytc-save-m", &format!("Playlist not created. {r}"));
                    }
                }
            }
            Job::Comments { epoch } => self.on_comments(epoch, res),
            Job::Replies { epoch, slot } => self.on_replies(epoch, slot, res),
            Job::Notifs => {
                self.nt_loading = false;
                match res {
                    Ok(j) => {
                        let m = notify::menu(&j);
                        self.nt.items.extend(m.items);
                        self.nt.ctoken = m.ctoken;
                        if self.nt.items.is_empty() {
                            self.nt.empty = m.empty;
                        }
                        self.nt_at = bridge::now();
                    }
                    Err(r) => {
                        self.report("notifications", format!("get_notification_menu: {r}"));
                        self.nt_err = Some(r);
                    }
                }
                if self.nt_open {
                    bridge::html("ytc-nt-p", &self.nt_html());
                }
            }
            Job::Record => {}
            Job::Account => {
                self.ac_loading = false;
                if let Err(r) = &res {
                    self.report("account", format!("account_menu: {r}"));
                }
                let mut avatar = false;
                if let Ok(j) = res {
                    self.account = notify::account(&j);
                    if let Some(p) = self.account.as_ref().and_then(|a| a.photo.clone()).filter(|_| self.avatar.is_none()) {
                        self.avatar = Some(p);
                        avatar = true;
                    }
                }
                if avatar {
                    self.render_slot();
                } else if self.ac_open {
                    bridge::html(notify::AC_P, &self.ac_html());
                }
            }
        }
        watch_later
    }

    pub fn debug(&self) -> String {
        let cm = match &self.cm {
            Some(c) => format!("comments {} shown · sort {} · {}", c.shown, c.sort, if c.cont.is_some() { "more" } else if c.started { "end" } else { "not loaded" }),
            None => "comments -".into(),
        };
        format!("acts   {cm} · requests {} · unknown items {}\n", self.jobs.len(), self.unknown.unknown_total())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::json::Value;
    pub fn fixture(name: &str) -> Value<'static> {
        crate::util::test_fixture(name)
    }
    #[test]
    fn helpers() {
        let j = crate::json::jv!({"a": [{"b": {"k": 1}}, {"k": 2}], "k": 3});
        assert_eq!(super::find(&j, "k"), Some(&crate::json::jv!(1)), "depth-first document order");
        assert_eq!(super::find_all(&j, "k").len(), 3);
        assert_eq!(super::st(&j, &["a", "0"]), None);
    }
}
