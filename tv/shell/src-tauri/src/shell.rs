//! The shell's state and the `ytc` command the page calls (`YTC_SHELL.call`, tv/shim/shim.js):
//!
//! | cmd            | answer                                                                  |
//! |----------------|-------------------------------------------------------------------------|
//! | `auth_token`   | `{"token", "exp"}` (refreshed when due) or `{"token": null}`            |
//! | `auth_start`   | `{"user_code", "verification_url", "expires_in", "interval"}`           |
//! | `auth_poll`    | `{"state": "pending"/"done"/"denied"/"expired"/"error", ...}`           |
//! | `auth_signout` | `{}`                                                                    |
//! | `store_get/set/remove` | chrome.storage.local                                            |
//! | `shell_info`   | shell version, bundle in use, update state, what this shell can do (`caps`: `voice`: a speech recognizer is there) |
//! | `shell_check`  | checks for updates now, then answers like `shell_info`                  |
//! | `shell_install`| downloads, verifies and installs the offered APK (in the background; `shell_info` reports `downloading` with `got`/`total`, then `installing`) |
//! | `voice_start`  | opens Android's speech recognizer: `{"started": bool}`; what it heard arrives through `shell_events` |
//! | `shell_events` | `{"events": [...]}`, taken once: `{"kind":"voice","state":"text","text"}` (or `state` `cancel`/`none`/`error`/`missing`), `{"kind":"search","q"}`, `{"kind":"open","path"}` |
//! | `tel_info`     | speed telemetry: `{"on", "url", "dev", "queued", "sent", "error"}` (`dev`: the bundle has its telemetry variant) |
//! | `tel_set`      | `{"on"?, "url"?}` → like `tel_info`, plus `"reload": true` when the page must load again (the script changed) |
//! | `tel_find`     | looks for a debug center on the network (`--lan`): `{"url"}`                                      |
//! | `ready_put`    | `{"record"}`: the page's ready Home response, kept sealed on disk for the next app start (`lumen_tv_core::readystore`) |
//! | `ready_take`   | `{"record": <string or null>}`: the one kept for this account, younger than 10 min; once         |
//! | `ready_drop`   | deletes it (something personalizing happened, or the page used its own copy)                     |
//! | `tel_send`     | `{"perf": <batch>}` / `{"incident": <report>}` from the shim; kept for the debug center while on, else dropped |
//!
//! Updates: at launch and every 6 hours the latest GitHub release is checked (`lumen_tv_core::release`), sooner after
//! a failed check (5 min, 15 min, 1 h; `update.retry_at` says when); a newer verified bundle is cached and handed to the
//! WebView (`host::bundle_ready`), a newer APK is offered to the page (`shell_info`'s `offer`): the client shows an
//! "Update ready" notice and its About & updates screen installs it (`shell_install`).
//!
//! Shell → page: what the page must hear of (a recognised search, a link another app opened) waits in a short queue,
//! and Kotlin tells the page to come and ask (`shell_events`). Nothing from outside is ever written into a script, and
//! a page that starts later (a cold start from a link) finds it there.
//!
//! Telemetry (`lumen_tv_core::tel`): off unless the owner turns it on (About & updates → Developer; debug builds:
//! `"telemetry": "<url>"` in `files/dev.json`). While on, the page runs the bundle's telemetry variant and its
//! reports go to the debug center at the owner's address, from a thread of their own.

use crate::host;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, OnceLock, RwLock, TryLockError};
use std::time::Duration;
use lumen_tv_core::auth::{Account, Endpoints};
use lumen_tv_core::incoming::{self, Incoming};
use lumen_tv_core::net::Http;
use lumen_tv_core::readystore;
use lumen_tv_core::release::{self, ApkOffer, Bundle, Schedule};
use lumen_tv_core::rules::{Decision, Rules};
use lumen_tv_core::store::Store;
use lumen_tv_core::tel;

/// The release key (`tv/keys/release.pub`): only bundles and APKs it signed are used.
const RELEASE_KEY: &str = include_str!("../../../keys/release.pub");
/// Where releases come from (`LUMEN_TV_REPO=owner/name` at build time). The repository was renamed from
/// justerquate-sys/lumen; apps built before 0.5.0 ask for that name, and GitHub's redirect for renamed repositories
/// (followed by `Http`: ureq, up to 10 redirects) brings them to the same releases.
const REPO: &str = match option_env!("LUMEN_TV_REPO") {
    Some(r) => r,
    None => "justerquate-sys/redline",
};
/// The APK download: minutes on a slow TV, but a route that does not answer gives way to the other one soon.
const APK_TOTAL: Duration = Duration::from_secs(20 * 60);
const APK_ANSWER: Duration = Duration::from_secs(30);
/// Events waiting for the page: the newest few (a page too old to ask for them must not grow this).
const EVENTS_KEPT: usize = 4;

/// The client bundle built into this APK (`./build.sh --tv` writes it).
fn builtin() -> Bundle {
    let mut files = std::collections::BTreeMap::new();
    files.insert("content.js".into(), include_str!("../bundle/content.js").into());
    files.insert("client.css".into(), include_str!("../bundle/client.css").into());
    files.insert("native.css".into(), include_str!("../bundle/native.css").into());
    files.insert("rules.json".into(), include_str!("../bundle/rules.json").into());
    let dev = include_str!(env!("LUMEN_TV_DEV_BUNDLE"));
    if !dev.is_empty() {
        files.insert(release::DEV_FILE.into(), dev.into());
    }
    let mut bin = std::collections::BTreeMap::new();
    for (name, bytes) in [(release::WASM_FILE, &include_bytes!(env!("LUMEN_TV_WASM"))[..]), (release::DEV_WASM_FILE, &include_bytes!(env!("LUMEN_TV_DEV_WASM"))[..])] {
        if !bytes.is_empty() {
            bin.insert(name.to_string(), bytes.to_vec());
        }
    }
    Bundle {
        seq: include_str!("../bundle/seq").trim().parse().unwrap_or(0),
        client: include_str!("../bundle/client").trim().into(),
        source: "builtin",
        files,
        bin,
    }
}

pub struct Shell {
    dir: PathBuf,
    key: [u8; 32],
    http: Http,
    apk_http: Http,
    api: String,
    account: Mutex<Account>,
    store: Mutex<Store>,
    bundle: RwLock<(Bundle, Rules)>,
    offer: Mutex<Option<ApkOffer>>,
    status: Mutex<Value>,
    /// One check at a time (the update thread, the About & updates screen).
    checking: Mutex<()>,
    schedule: Mutex<Schedule>,
    events: Mutex<VecDeque<Value>>,
    tel: Mutex<Tel>,
    tel_wake: Condvar,
    tel_http: Http,
    ready: Mutex<ReadySlot>,
}

/// The ready Home response kept from the last run: read (and deleted from disk) once, at launch.
#[derive(Default)]
struct ReadySlot {
    loaded: bool,
    record: Option<String>,
}

/// Telemetry: the owner's choice, the reports waiting, what happened to the last delivery.
#[derive(Default)]
struct Tel {
    settings: tel::Settings,
    /// Set by `files/dev.json` (debug builds): not written to `telemetry.json`.
    forced: bool,
    queue: tel::Queue,
    sent: u64,
    error: Option<String>,
    retry_at: f64,
}

static SHELL: OnceLock<Shell> = OnceLock::new();

pub fn get() -> Option<&'static Shell> {
    SHELL.get()
}

/// Version name and Android versionCode (Tauri's: major·10⁶ + minor·10³ + patch).
pub fn version() -> (&'static str, u32) {
    let v = env!("CARGO_PKG_VERSION");
    let n: Vec<u32> = v.split('.').map(|x| x.parse().unwrap_or(0)).collect();
    (v, n.first().unwrap_or(&0) * 1_000_000 + n.get(1).unwrap_or(&0) * 1000 + n.get(2).unwrap_or(&0))
}

fn now() -> f64 {
    lumen_tv_core::now_ms()
}

/// Debug builds only: `files/dev.json` points the release check and the sign-in at local fakes
/// (`{"release_api": "http://10.0.2.2:8765/repos/o/r/releases", "oauth": "http://10.0.2.2:8765"}`).
fn dev(dir: &std::path::Path) -> Value {
    if !cfg!(debug_assertions) {
        return Value::Null;
    }
    std::fs::read(dir.join("dev.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

pub fn init(dir: PathBuf) {
    if SHELL.get().is_some() {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let key = release::parse_key(RELEASE_KEY).expect("release key");
    let dev = dev(&dir);
    // `LUMEN_TV_RELEASE_API` at build time replaces GitHub entirely (test builds against a local fake).
    let api = dev
        .get("release_api")
        .and_then(Value::as_str)
        .map(String::from)
        .or_else(|| option_env!("LUMEN_TV_RELEASE_API").map(String::from))
        .unwrap_or_else(|| format!("https://api.github.com/repos/{REPO}/releases?per_page=10"));
    let ep = Endpoints {
        base: dev.get("oauth").and_then(Value::as_str).unwrap_or(lumen_tv_core::auth::GOOGLE).to_string(),
    };
    // The account: the refresh token sealed by the Keystore (host::seal); a device id per install.
    let device_id = match std::fs::read_to_string(dir.join("device-id")) {
        Ok(s) if s.trim().len() == 36 => s.trim().to_string(),
        _ => {
            let id = lumen_tv_core::auth::new_device_id();
            let _ = std::fs::write(dir.join("device-id"), &id);
            id
        }
    };
    let sealed = dir.join("account.bin");
    let refresh = std::fs::read(&sealed).ok().and_then(|b| host::open(&b)).and_then(|b| String::from_utf8(b).ok());
    let persist = {
        let sealed = sealed.clone();
        Box::new(move |r: Option<&str>| match r.and_then(|r| host::seal(r.as_bytes())) {
            Some(b) => {
                let _ = lumen_tv_core::write_atomic(&sealed, &b);
            }
            None => {
                let _ = std::fs::remove_file(&sealed);
            }
        })
    };
    let account = Account::new(ep, device_id, refresh, persist);
    let bundle = release::choose(release::load_cached(&dir.join("bundle"), &key), builtin());
    let rules = Rules::parse(bundle.file("rules.json")).unwrap_or_else(|_| Rules::parse("[]").unwrap());
    let store = Store::open(dir.join("storage.json"));
    let status = json!({"state": "idle"});
    let mut t = Tel { settings: tel::Settings::load(&dir.join("telemetry.json")), ..Default::default() };
    if let Some(url) = dev.get("telemetry").and_then(Value::as_str).and_then(|u| tel::collector_url(u).ok()) {
        t.settings = tel::Settings { on: true, url };
        t.forced = true;
    }
    let _ = SHELL.set(Shell {
        dir,
        key,
        http: Http::new(Duration::from_secs(30)),
        apk_http: Http::download(APK_TOTAL, APK_ANSWER),
        api,
        account: Mutex::new(account),
        store: Mutex::new(store),
        bundle: RwLock::new((bundle, rules)),
        offer: Mutex::new(None),
        status: Mutex::new(status),
        checking: Mutex::new(()),
        schedule: Mutex::new(Schedule::default()),
        events: Mutex::new(VecDeque::new()),
        tel: Mutex::new(t),
        tel_wake: Condvar::new(),
        tel_http: Http::new(Duration::from_secs(5)),
        ready: Mutex::new(ReadySlot::default()),
    });
    std::thread::spawn(|| {
        if let Some(s) = get() {
            s.tel_loop();
        }
    });
    // Home reload, early start: a new process holds no access token, and the client's first account
    // request (Home) waits for `auth_token`, which would refresh it only then. The refresh starts
    // now, in parallel with the WebView, the document and the client's Wasm; the page's
    // `auth_token` call waits on the same lock and gets the fresh token without a second request.
    std::thread::spawn(|| {
        if let Some(s) = get() {
            // The last run's ready Home response, unsealed before the page asks for it (Keystore work stays off the
            // UI thread and runs while the WebView starts).
            s.ready_load(&mut s.ready.lock().unwrap());
            let _ = s.account.lock().unwrap().token(&s.http, now());
        }
    });
}

impl Shell {
    pub fn script(&self) -> String {
        let on = self.tel.lock().unwrap().settings.on;
        let b = self.bundle.read().unwrap();
        crate::script::compose(&b.0, version().0, on)
    }

    fn ready_path(&self) -> PathBuf {
        self.dir.join(readystore::FILE)
    }

    fn ready_load(&self, slot: &mut ReadySlot) {
        if !std::mem::replace(&mut slot.loaded, true) {
            let fp = self.account.lock().unwrap().fingerprint();
            slot.record = readystore::take(&self.ready_path(), &fp, now(), host::open);
        }
    }

    fn ready_drop(&self) {
        readystore::remove(&self.ready_path());
        let mut g = self.ready.lock().unwrap();
        g.loaded = true;
        g.record = None;
    }

    /// The Wasm the page asks for (`/__lumen/client.wasm`, format 2): the telemetry variant's while telemetry runs.
    pub fn wasm(&self) -> Vec<u8> {
        let on = self.tel.lock().unwrap().settings.on;
        crate::script::wasm(&self.bundle.read().unwrap().0, on).to_vec()
    }

    fn tel_info(&self, reload: bool) -> String {
        let dev = self.bundle.read().unwrap().0.has_dev();
        let t = self.tel.lock().unwrap();
        let mut v = json!({"on": t.settings.on, "url": t.settings.url, "dev": dev, "queued": t.queue.len(), "sent": t.sent, "error": t.error});
        if reload {
            v["reload"] = json!(true);
        }
        v.to_string()
    }

    /// Delivers waiting reports while telemetry is on (one at a time, oldest first; a failure waits a minute).
    fn tel_loop(&self) {
        loop {
            let (url, item) = {
                let mut t = self.tel.lock().unwrap();
                loop {
                    let due = t.settings.on && !t.queue.is_empty();
                    if due && now() >= t.retry_at {
                        break;
                    }
                    let wait = if due { (t.retry_at - now()).clamp(50.0, 60_000.0) } else { 60_000.0 };
                    t = self.tel_wake.wait_timeout(t, Duration::from_millis(wait as u64)).unwrap().0;
                }
                (t.settings.url.clone(), t.queue.front().unwrap_or_default().to_string())
            };
            let r = tel::post(&self.tel_http, &url, &item);
            let mut t = self.tel.lock().unwrap();
            match r {
                Ok(()) => {
                    t.queue.delivered(&item);
                    t.sent += 1;
                    t.error = None;
                }
                Err(e) => {
                    t.error = Some(e);
                    t.retry_at = now() + 60_000.0;
                }
            }
        }
    }

    pub fn decide(&self, url: &str, main_frame: bool, top: &str) -> Decision {
        self.bundle.read().unwrap().1.decide(url, main_frame, top)
    }

    pub fn call(&self, cmd: &str, arg: &str) -> Result<String, String> {
        let arg: Value = serde_json::from_str(arg).unwrap_or(Value::Null);
        match cmd {
            "auth_token" => Ok(lumen_tv_core::auth::token_json(self.account.lock().unwrap().token(&self.http, now())?.as_ref())),
            "auth_start" => self.account.lock().unwrap().start(&self.http, now()),
            "auth_poll" => {
                let r = self.account.lock().unwrap().poll(&self.http, now());
                if serde_json::from_str::<Value>(&r).ok().and_then(|v| v.get("state").and_then(Value::as_str).map(|x| x == "done")).unwrap_or(false) {
                    // Another account (or the first): nothing kept for the one before.
                    self.ready_drop();
                }
                Ok(r)
            }
            "auth_signout" => {
                self.account.lock().unwrap().sign_out(&self.http);
                self.ready_drop();
                Ok("{}".into())
            }
            "ready_put" => {
                let rec = arg.get("record").and_then(Value::as_str).ok_or("no record")?;
                let fp = self.account.lock().unwrap().fingerprint();
                readystore::put(&self.ready_path(), &fp, now(), rec, host::seal).map(|_| "{}".into())
            }
            "ready_take" => {
                let mut g = self.ready.lock().unwrap();
                self.ready_load(&mut g);
                Ok(json!({"record": g.record.take()}).to_string())
            }
            "ready_drop" => {
                self.ready_drop();
                Ok("{}".into())
            }
            "store_get" => Ok(self.store.lock().unwrap().get(&arg).to_string()),
            "store_set" => self.store.lock().unwrap().set(&arg).map(|_| "{}".into()),
            "store_remove" => self.store.lock().unwrap().remove(&arg).map(|_| "{}".into()),
            "shell_info" => {
                let b = self.bundle.read().unwrap();
                let (v, code) = version();
                let offer = self.offer.lock().unwrap().as_ref().map(|o| json!({"version": o.apk.version, "version_code": o.apk.version_code, "size": o.apk.size}));
                Ok(json!({"version": v, "version_code": code, "bundle": {"seq": b.0.seq, "client": b.0.client, "source": b.0.source}, "update": *self.status.lock().unwrap(), "offer": offer, "repo": REPO, "caps": {"voice": host::voice_available()}}).to_string())
            }
            "voice_start" => Ok(json!({"started": host::voice_start()}).to_string()),
            "shell_events" => Ok(json!({"events": self.events.lock().unwrap().drain(..).collect::<Vec<_>>()}).to_string()),
            // Checks for updates now (the client's About & updates screen; tests). No dialog: the screen
            // shows what it found. A check that just ended answers instead (`release::CHECK_REUSE_MS`).
            "shell_check" => {
                self.check(false);
                self.call("shell_info", "null")
            }
            // Installs the offered APK (About & updates → Install update): downloads in the background.
            "shell_install" => {
                if self.offer.lock().unwrap().is_none() {
                    return Err("no app update is offered".into());
                }
                std::thread::spawn(|| {
                    if let Some(s) = get() {
                        s.accept_update();
                    }
                });
                self.call("shell_info", "null")
            }
            // A report from the page (the shim forwards the client's `ytcPerf` / `ytcIncident` messages).
            "tel_send" => {
                let (perf, report) = match (arg.get("perf").and_then(Value::as_str), arg.get("incident").and_then(Value::as_str)) {
                    (Some(p), _) => (true, p),
                    (None, Some(i)) => (false, i),
                    _ => return Ok("{}".into()),
                };
                if !self.tel.lock().unwrap().settings.on {
                    return Ok("{}".into());
                }
                let build = crate::script::build_name(&self.bundle.read().unwrap().0, version().0, true);
                let report = if perf { report.to_string() } else { tel::scrub(report) };
                if let Some(j) = tel::stamp(&report, &build) {
                    self.tel.lock().unwrap().queue.push(perf, j);
                    self.tel_wake.notify_one();
                }
                Ok("{}".into())
            }
            "tel_info" => Ok(self.tel_info(false)),
            // The owner's choice (About & updates → Developer). Turning it on or off changes the page's script: the
            // answer says to load the page again.
            "tel_set" => {
                let mut t = self.tel.lock().unwrap();
                let mut s = t.settings.clone();
                if let Some(u) = arg.get("url").and_then(Value::as_str) {
                    if u.trim().is_empty() {
                        s = tel::Settings::default();
                    } else {
                        s.url = tel::collector_url(u)?;
                    }
                }
                if let Some(on) = arg.get("on").and_then(Value::as_bool) {
                    if on && s.url.is_empty() {
                        return Err("enter the debug center's address first".into());
                    }
                    s.on = on;
                }
                let changed = s.on != t.settings.on;
                if !t.forced {
                    s.save(&self.dir.join("telemetry.json")).map_err(|e| e.to_string())?;
                }
                if !s.on {
                    t.queue.clear();
                }
                t.settings = s;
                t.error = None;
                t.retry_at = 0.0;
                drop(t);
                self.tel_wake.notify_one();
                if changed {
                    host::script_changed();
                }
                Ok(self.tel_info(changed))
            }
            "tel_find" => tel::discover(Duration::from_millis(1500)).map(|url| json!({"url": url}).to_string()),
            _ => Err(format!("unknown command {cmd}")),
        }
    }

    fn push_event(&self, e: Value) {
        let mut q = self.events.lock().unwrap();
        if q.len() >= EVENTS_KEPT {
            q.pop_front();
        }
        q.push_back(e);
    }

    /// An intent the activity received (a search, a link), validated here; anything else is dropped.
    pub fn incoming(&self, action: &str, data: &str, query: &str) {
        match incoming::parse(action, data, query) {
            Some(Incoming::Search(q)) => self.push_event(json!({"kind": "search", "q": q})),
            Some(Incoming::Open(path)) => self.push_event(json!({"kind": "open", "path": path})),
            None => log::info!("incoming: nothing usable in {action}"),
        }
    }

    /// The speech recognizer's answer: `state` is `text` (with `text`), `cancel`, `none`, `error` or `missing`.
    pub fn voice_result(&self, state: &str, text: &str) {
        let e = match (state, incoming::clean_query(text)) {
            ("text", Some(t)) => json!({"kind": "voice", "state": "text", "text": t}),
            ("text", None) | ("none", _) => json!({"kind": "voice", "state": "none"}),
            ("cancel", _) => json!({"kind": "voice", "state": "cancel"}),
            ("missing", _) => json!({"kind": "voice", "state": "missing"}),
            _ => json!({"kind": "voice", "state": "error"}),
        };
        self.push_event(e);
    }

    /// One update check (blocking; `automatic`: the update thread, else the About & updates screen). A check already
    /// running answers for both: this one waits for its end, and adds no requests. So does one that just ended, for a
    /// press of the button.
    pub fn check(&self, automatic: bool) {
        let _one = match self.checking.try_lock() {
            Ok(g) => g,
            Err(TryLockError::Poisoned(e)) => e.into_inner(),
            Err(TryLockError::WouldBlock) => {
                drop(self.checking.lock());
                return;
            }
        };
        if !automatic && self.schedule.lock().unwrap().recent(now()) {
            return;
        }
        let seq = self.bundle.read().unwrap().0.seq;
        let code = version().1;
        *self.status.lock().unwrap() = json!({"state": "checking", "at": now()});
        match release::check(&self.http, &self.api, &self.key, &self.dir.join("bundle"), seq, code) {
            Ok(out) => {
                let mut st = json!({"state": "ok", "at": now(), "needs_shell": out.needs_shell});
                if let Some(b) = out.bundle {
                    log::info!("client bundle {} (seq {}) installed", b.client, b.seq);
                    st["installed"] = json!({"client": b.client, "seq": b.seq});
                    let rules = Rules::parse(b.file("rules.json")).unwrap_or_else(|_| Rules::parse("[]").unwrap());
                    *self.bundle.write().unwrap() = (b, rules);
                    host::bundle_ready();
                }
                if let Some(o) = out.apk {
                    st["apk"] = json!({"version": o.apk.version, "version_code": o.apk.version_code});
                    *self.offer.lock().unwrap() = Some(o.clone());
                }
                self.schedule.lock().unwrap().finished(now(), automatic, None);
                *self.status.lock().unwrap() = st;
            }
            Err(e) => {
                log::warn!("update check: {}", e.reason);
                let retry_at = self.schedule.lock().unwrap().finished(now(), automatic, Some(e.wait_secs));
                *self.status.lock().unwrap() = json!({"state": "failed", "at": now(), "error": e.reason, "retry_at": retry_at});
            }
        }
    }

    /// The update thread's rest: until the next check is due. It looks again every minute: a check from the screen may
    /// have moved it, and a sleeping TV stops this clock, not the wall clock (`due_in` also survives a corrected clock).
    fn until_next_check(&self) {
        loop {
            let left = self.schedule.lock().unwrap().due_in(now());
            if left <= 0.0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(left.min(60_000.0) as u64));
        }
    }

    /// The owner accepted the shell update: download (progress in the status), check, hand to the installer.
    pub fn accept_update(&self) {
        let Some(o) = self.offer.lock().unwrap().clone() else { return };
        let apk = json!({"version": o.apk.version, "version_code": o.apk.version_code});
        let mut last = 0u64;
        let mut on = |got: u64, total: u64| {
            // (A status write per 1 % at most. A count that starts again: the other download route took over.)
            if total == 0 || got == total || got < last || (got - last) * 100 >= total {
                last = got;
                *self.status.lock().unwrap() = json!({"state": "downloading", "at": now(), "got": got, "total": total, "apk": apk});
            }
        };
        on(0, o.apk.size);
        match release::download_apk_progress(&self.apk_http, &o, &self.dir.join("updates"), &mut on) {
            Ok(p) => {
                *self.status.lock().unwrap() = json!({"state": "installing", "at": now(), "apk": apk});
                host::install_apk(&p.to_string_lossy());
            }
            Err(e) => {
                log::warn!("shell update: {e}");
                *self.status.lock().unwrap() = json!({"state": "failed", "at": now(), "error": e});
            }
        }
    }
}

/// The update thread: a check shortly after launch (the first page goes first), then every 6 hours, sooner after a
/// failure (`release::next_check_ms`). One thread and one due time: retries never stack, and none of this touches a
/// running page (a new bundle goes live when the app comes back to the foreground; the notice stays off videos).
pub fn spawn_updates() {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(8));
        while let Some(s) = get() {
            s.check(true);
            s.until_next_check();
        }
    });
}
