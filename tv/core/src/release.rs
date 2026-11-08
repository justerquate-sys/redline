//! Client updates from GitHub Releases.
//!
//! A release carries the shared client's files (`content.js`, `client.css`, `native.css`,
//! `rules.json`), the APK,
//! and `lumen-tv-manifest.json` + `lumen-tv-manifest.json.sig`: the SHA-256 and size of every file, a sequence
//! number (`seq`, release time in seconds: higher = newer) and the oldest shell able to run the
//! bundle (`min_shell`), signed with the release key (ed25519; its public key is compiled into the
//! app, `tv/keys/release.pub`). Nothing is used unless the signature and every hash check out.
//!
//! The app runs the newest verified bundle it has: the cached download or the copy built into the
//! APK (a new APK brings a newer built-in copy, so a stale cache never wins over it). Offline it
//! runs what it has.
//!
//! Every release file has two routes (GitHub's website can answer 504 for a repository while its API is fine,
//! 8 Nov 2026): the asset's `browser_download_url` and the release API's asset `url` (`Accept:
//! application/octet-stream`, redirects followed, no auth). Whichever route delivered the bytes, they pass the same
//! signature and hash checks. See [`Routes`] for how one check uses them, and what it costs.

use crate::net::Http;
use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "lumen-tv-manifest.json";
pub const SIG: &str = "lumen-tv-manifest.json.sig";
/// The bundle's files (all required).
pub const FILES: [&str; 4] = ["content.js", "client.css", "native.css", "rules.json"];
/// The bundle's development variant of `content.js` (speed telemetry, `./build.sh --tv --dev`): optional, signed
/// like the others when the release carries it; the app runs it only while its owner turned telemetry on
/// (`crate::tel`). Apps from before it ignore it.
pub const DEV_FILE: &str = "content-dev.js";
/// Format 2 (shell 0.3.0+): the client's Wasm as a file of its own, served to the page by the shell
/// (`/__lumen/client.wasm`) instead of travelling inside `content.js`; and the telemetry variant's. Binary, optional:
/// a release that has them needs a shell that serves them (its manifest's `min_shell`).
pub const WASM_FILE: &str = "client.wasm";
pub const DEV_WASM_FILE: &str = "client-dev.wasm";
const BIN_FILES: [&str; 2] = [WASM_FILE, DEV_WASM_FILE];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FileEntry {
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Apk {
    pub version_code: u32,
    pub version: String,
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub schema: u32,
    pub seq: u64,
    /// Client version and build id (`<version> (<bundle hash>)`, written by `browser/build.cjs --target=tv`).
    pub client: String,
    pub min_shell: u32,
    pub files: BTreeMap<String, FileEntry>,
    #[serde(default)]
    pub apk: Option<Apk>,
}

pub fn sha256_hex(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

/// The manifest, if `sig` (base64 ed25519 over the exact bytes) is the release key's.
pub fn verify_manifest(bytes: &[u8], sig: &str, key: &[u8; 32]) -> Result<Manifest, String> {
    let key = VerifyingKey::from_bytes(key).map_err(|e| format!("bad key: {e}"))?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(sig.trim())
        .map_err(|_| "signature is not base64".to_string())?;
    let sig = Signature::from_slice(&raw).map_err(|_| "bad signature length".to_string())?;
    key.verify(bytes, &sig).map_err(|_| "signature does not match the release key".to_string())?;
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("manifest: {e}"))?;
    if m.schema != 1 {
        return Err(format!("unknown manifest schema {}", m.schema));
    }
    for f in FILES {
        if !m.files.contains_key(f) {
            return Err(format!("manifest lacks {f}"));
        }
    }
    Ok(m)
}

/// A bundle's file checked against the manifest.
pub fn check_file(m: &Manifest, name: &str, bytes: &[u8]) -> Result<(), String> {
    let e = m.files.get(name).ok_or_else(|| format!("{name} not in the manifest"))?;
    if bytes.len() as u64 != e.size || sha256_hex(bytes) != e.sha256 {
        return Err(format!("{name}: hash mismatch"));
    }
    Ok(())
}

/// A verified client bundle.
#[derive(Clone, Debug, Default)]
pub struct Bundle {
    pub seq: u64,
    pub client: String,
    /// "builtin" or "release"
    pub source: &'static str,
    pub files: BTreeMap<String, String>,
    /// The binary files (format 2: `client.wasm`, `client-dev.wasm`).
    pub bin: BTreeMap<String, Vec<u8>>,
}

impl Bundle {
    pub fn file(&self, name: &str) -> &str {
        self.files.get(name).map(String::as_str).unwrap_or("")
    }

    pub fn bin_file(&self, name: &str) -> &[u8] {
        self.bin.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The telemetry variant can run: its script, and in format 2 its own Wasm (the two are built together).
    pub fn has_dev(&self) -> bool {
        !self.file(DEV_FILE).is_empty() && (self.bin_file(WASM_FILE).is_empty() || !self.bin_file(DEV_WASM_FILE).is_empty())
    }
}

/// The bundle cached at `dir` (re-verified: signature and hashes), if any.
pub fn load_cached(dir: &Path, key: &[u8; 32]) -> Option<Bundle> {
    let m = std::fs::read(dir.join(MANIFEST)).ok()?;
    let sig = std::fs::read_to_string(dir.join(SIG)).ok()?;
    let m = verify_manifest(&m, &sig, key).ok()?;
    let mut files = BTreeMap::new();
    for f in FILES {
        let b = std::fs::read(dir.join(f)).ok()?;
        check_file(&m, f, &b).ok()?;
        files.insert(f.to_string(), String::from_utf8(b).ok()?);
    }
    // (A development variant that is missing or does not verify is left out; the bundle stays usable.)
    if m.files.contains_key(DEV_FILE) {
        if let Some(t) = std::fs::read(dir.join(DEV_FILE)).ok().filter(|b| check_file(&m, DEV_FILE, b).is_ok()).and_then(|b| String::from_utf8(b).ok()) {
            files.insert(DEV_FILE.to_string(), t);
        }
    }
    // Format 2: the Wasm the page asks for must be there and verify (its telemetry twin may be missing).
    let mut bin = BTreeMap::new();
    for f in BIN_FILES.into_iter().filter(|f| m.files.contains_key(*f)) {
        match std::fs::read(dir.join(f)).ok().filter(|b| check_file(&m, f, b).is_ok()) {
            Some(b) => {
                bin.insert(f.to_string(), b);
            }
            None if f == WASM_FILE => return None,
            None => {}
        }
    }
    Some(Bundle {
        seq: m.seq,
        client: m.client,
        source: "release",
        files,
        bin,
    })
}

/// The newer of the cached and the built-in bundle.
pub fn choose(cached: Option<Bundle>, builtin: Bundle) -> Bundle {
    match cached {
        Some(c) if c.seq > builtin.seq => c,
        _ => builtin,
    }
}

/// The two routes to one release file.
#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    /// `browser_download_url` (github.com).
    pub web: String,
    /// The release API's asset `url` (api.github.com, counts against its rate limit), when the answer has one.
    pub api: Option<String>,
}

/// The files of the newest GitHub release that carries the TV files: `GET
/// /repos/{owner}/{repo}/releases` lists them newest first (a release without them, e.g. a
/// desktop-only one, is skipped); a single release object is read as it is.
pub fn assets(answer: &serde_json::Value) -> BTreeMap<String, Asset> {
    let release = match answer.as_array() {
        Some(list) => match list
            .iter()
            .filter(|r| r.get("draft").and_then(|d| d.as_bool()) != Some(true))
            .find(|r| assets_of(r).contains_key(MANIFEST))
        {
            Some(r) => r,
            None => return BTreeMap::new(),
        },
        None => answer,
    };
    assets_of(release)
}

fn assets_of(release: &serde_json::Value) -> BTreeMap<String, Asset> {
    release
        .get("assets")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let web = a.get("browser_download_url")?.as_str()?.to_string();
            let api = a.get("url").and_then(|u| u.as_str()).map(String::from);
            Some((a.get("name")?.as_str()?.to_string(), Asset { web, api }))
        })
        .collect()
}

/// Why a check or download failed: `reason` says it in words with the technical part in brackets (no URLs), `wait_secs`
/// is how long GitHub asked to be left alone (a rate limit), 0 when it did not.
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub reason: String,
    pub wait_secs: u64,
}

impl From<String> for Failure {
    fn from(reason: String) -> Failure {
        Failure { reason, wait_secs: 0 }
    }
}

impl From<&str> for Failure {
    fn from(reason: &str) -> Failure {
        reason.to_string().into()
    }
}

/// Route failures one check accepts before it gives up (and tries again later). Only a route that flips between
/// working and not working gets this far; otherwise the first file settles which route the rest use.
pub const MAX_ROUTE_FAILURES: u32 = 4;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Via {
    Web,
    Api,
}

impl Via {
    fn other(self) -> Via {
        if self == Via::Web { Via::Api } else { Via::Web }
    }

    fn name(self) -> &'static str {
        if self == Via::Web { "download" } else { "API" }
    }
}

/// One failed try.
struct Miss {
    what: String,
    status: Option<u16>,
    /// A rate limit: how long it lasts (0 = not said).
    limit: Option<u64>,
    /// Another route may help: a 5xx, a rate limit, a transport error. (A 404 does not: the file is not there.)
    other_route: bool,
}

/// The seconds a rate-limit answer asks to wait, or None when `a` is not one: 429, or GitHub's 403 with its signs
/// (no requests left, `Retry-After`, or "rate limit" in the message).
fn rate_limit(a: &crate::net::Answer) -> Option<u64> {
    let signs = a.limit_remaining == Some(0) || a.retry_after.is_some() || String::from_utf8_lossy(&a.body).to_ascii_lowercase().contains("rate limit");
    if a.status != 429 && !(a.status == 403 && signs) {
        return None;
    }
    // (The limit starts over within the hour: a reset further off than that is this TV's clock.)
    let now = (crate::now_ms() / 1000.0) as u64;
    Some(a.retry_after.or(a.limit_reset.map(|r| r.saturating_sub(now).min(3600))).unwrap_or(0))
}

fn miss(via: Via, r: Result<crate::net::Answer, String>) -> Result<Vec<u8>, Miss> {
    let a = match r {
        Ok(a) if a.status == 200 => return Ok(a.body),
        Ok(a) => a,
        Err(e) => return Err(Miss { what: format!("{} {e}", via.name()), status: None, limit: None, other_route: true }),
    };
    let limit = rate_limit(&a);
    Err(Miss { what: format!("{} HTTP {}", via.name(), a.status), status: Some(a.status), limit, other_route: limit.is_some() || a.status >= 500 })
}

/// All the tries for `name` failed: in words, with the technical part in brackets.
fn describe(name: &str, misses: &[Miss]) -> Failure {
    let parts = misses.iter().map(|m| m.what.as_str()).collect::<Vec<_>>().join(", ");
    if let Some(wait) = misses.iter().filter_map(|m| m.limit).max() {
        let resets = if wait > 0 { format!(", resets in {} min", wait.div_ceil(60)) } else { String::new() };
        return Failure { reason: format!("GitHub's rate limit was reached ({name}: {parts}{resets})"), wait_secs: wait };
    }
    if misses.iter().any(|m| m.status.is_some_and(|s| s >= 500)) {
        format!("GitHub isn't answering ({name}: {parts})").into()
    } else if misses.iter().all(|m| m.status.is_none()) {
        format!("No connection to GitHub ({name}: {parts})").into()
    } else if let [only] = misses {
        // One route, and the file is not there: "<name>: HTTP 404".
        format!("{name}: HTTP {}", only.status.unwrap_or(0)).into()
    } else {
        format!("{name}: {parts}").into()
    }
}

/// How one check reaches a release's files, at most one failed try per route and file:
///
/// - Normal case: every file from `browser_download_url`, which does not touch the API's rate limit (60 requests an
///   hour per IP unauthenticated). A check costs one API request, the release lookup, as it always did.
/// - A try that fails with a 5xx, a 429/rate-limit answer or a transport error goes straight to the other route (no
///   retry of the same route: a failing website takes about 11 s per file and a release has about 10), and that
///   other route comes first for the rest of this check. The next check starts with the download route again.
/// - Worst case in API requests: the lookup and one per file, 10 for a release of 9 files; 3 when already up to date
///   (lookup, manifest, signature). Worst case in all requests: those plus [`MAX_ROUTE_FAILURES`].
#[derive(Debug)]
struct Routes {
    first: Via,
    failures: u32,
}

impl Routes {
    fn new() -> Routes {
        Routes { first: Via::Web, failures: 0 }
    }

    /// `name`'s bytes, through `get(url, headers)`.
    fn fetch(&mut self, name: &str, asset: &Asset, get: &mut dyn FnMut(&str, &[(&str, &str)]) -> Result<crate::net::Answer, String>) -> Result<Vec<u8>, Failure> {
        let mut via = if asset.api.is_some() { self.first } else { Via::Web };
        let mut misses: Vec<Miss> = Vec::new();
        loop {
            let got = match (via, &asset.api) {
                (Via::Api, Some(url)) => get(url, &[("Accept", "application/octet-stream")]),
                _ => get(&asset.web, &[]),
            };
            let m = match miss(via, got) {
                Ok(bytes) => return Ok(bytes),
                Err(m) => m,
            };
            let other_route = m.other_route;
            misses.push(m);
            if !other_route {
                return Err(describe(name, &misses));
            }
            self.failures += 1;
            if asset.api.is_some() {
                self.first = via.other();
            }
            if misses.len() == 2 || asset.api.is_none() || self.failures >= MAX_ROUTE_FAILURES {
                return Err(describe(name, &misses));
            }
            via = via.other();
        }
    }
}

/// The automatic checks, without a failure: every 6 hours.
pub const CHECK_EVERY_MS: f64 = 6.0 * 3_600_000.0;

/// Milliseconds until the next automatic check after `fails` failed checks in a row (0 = the last one worked): sooner
/// after a failure (5 min, 15 min, 1 h), then back to the schedule; never before a rate limit's `wait_secs` is over
/// (at most 2 h: a wrong header must not stop the updates).
pub fn next_check_ms(fails: u32, wait_secs: u64) -> f64 {
    let step = match fails {
        1 => 5.0 * 60_000.0,
        2 => 15.0 * 60_000.0,
        3 => 3_600_000.0,
        _ => CHECK_EVERY_MS,
    };
    step.max(wait_secs.min(2 * 3600) as f64 * 1000.0)
}

/// A newer shell (APK) the release offers.
#[derive(Clone, Debug, PartialEq)]
pub struct ApkOffer {
    pub apk: Apk,
    pub url: String,
    /// The second route (see [`Asset`]).
    pub api_url: Option<String>,
}

#[derive(Debug, Default)]
pub struct Outcome {
    /// A newer bundle, verified and now in the cache (applies on the next page load).
    pub bundle: Option<Bundle>,
    /// A newer APK (the owner is asked; installing needs their OK on the TV).
    pub apk: Option<ApkOffer>,
    /// The release's bundle needs a newer shell than this one.
    pub needs_shell: bool,
}

/// Checks the latest release at `api` and installs a newer bundle into `cache` (written to a
/// sibling directory first, swapped in only when complete and verified).
pub fn check(http: &Http, api: &str, key: &[u8; 32], cache: &Path, current_seq: u64, shell_code: u32) -> Result<Outcome, Failure> {
    // (The lookup has no second route: there is no list of releases without the API.)
    let rel = http.get(api, &[("Accept", "application/vnd.github+json")]).map_err(|e| format!("No connection to GitHub (release lookup: {e})"))?;
    if rel.status != 200 {
        return Err(describe("release lookup", &[Miss { what: format!("HTTP {}", rel.status), status: Some(rel.status), limit: rate_limit(&rel), other_route: false }]));
    }
    let urls = assets(&rel.json().ok_or("release lookup: not JSON")?);
    let routes = std::cell::RefCell::new(Routes::new());
    let get = |name: &str| -> Result<Vec<u8>, Failure> {
        let asset = urls.get(name).ok_or_else(|| format!("release has no {name}"))?;
        routes.borrow_mut().fetch(name, asset, &mut |url, headers| http.get(url, headers))
    };
    let mbytes = get(MANIFEST)?;
    let sig = String::from_utf8(get(SIG)?).map_err(|_| "signature is not text")?;
    let m = verify_manifest(&mbytes, &sig, key)?;
    let mut out = Outcome::default();
    if let Some(apk) = &m.apk {
        if apk.version_code > shell_code {
            if let Some(a) = urls.get(&apk.file) {
                out.apk = Some(ApkOffer {
                    apk: apk.clone(),
                    url: a.web.clone(),
                    api_url: a.api.clone(),
                });
            }
        }
    }
    if m.seq <= current_seq {
        return Ok(out);
    }
    if m.min_shell > shell_code {
        out.needs_shell = true;
        return Ok(out);
    }
    let stage = sibling(cache, "new");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| e.to_string())?;
    let mut files = BTreeMap::new();
    let dev = m.files.contains_key(DEV_FILE).then_some(DEV_FILE);
    for f in FILES.into_iter().chain(dev) {
        let b = get(f)?;
        check_file(&m, f, &b)?;
        std::fs::write(stage.join(f), &b).map_err(|e| e.to_string())?;
        files.insert(f.to_string(), String::from_utf8(b).map_err(|_| format!("{f} is not UTF-8"))?);
    }
    let mut bin = BTreeMap::new();
    for f in BIN_FILES.into_iter().filter(|f| m.files.contains_key(*f)) {
        let b = get(f)?;
        check_file(&m, f, &b)?;
        std::fs::write(stage.join(f), &b).map_err(|e| e.to_string())?;
        bin.insert(f.to_string(), b);
    }
    std::fs::write(stage.join(MANIFEST), &mbytes).map_err(|e| e.to_string())?;
    std::fs::write(stage.join(SIG), sig.as_bytes()).map_err(|e| e.to_string())?;
    let old = sibling(cache, "old");
    let _ = std::fs::remove_dir_all(&old);
    if cache.exists() {
        std::fs::rename(cache, &old).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&stage, cache).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&old);
    out.bundle = Some(Bundle {
        seq: m.seq,
        client: m.client,
        source: "release",
        files,
        bin,
    });
    Ok(out)
}

/// Downloads an offered APK to `dir` and checks its hash; returns its path.
pub fn download_apk(http: &Http, offer: &ApkOffer, dir: &Path) -> Result<PathBuf, String> {
    download_apk_progress(http, offer, dir, &mut |_, _| {})
}

/// `download_apk` reporting `on(got, total)` while it downloads (total from the signed manifest). When the other route
/// has to take over, the count starts again at 0. The hash check is the same for both routes.
pub fn download_apk_progress(http: &Http, offer: &ApkOffer, dir: &Path, on: &mut dyn FnMut(u64, u64)) -> Result<PathBuf, String> {
    let size = offer.apk.size;
    let asset = Asset { web: offer.url.clone(), api: offer.api_url.clone() };
    let body = Routes::new()
        .fetch("APK", &asset, &mut |url, headers| {
            on(0, size);
            http.get_progress(url, headers, &mut |got, total| on(got, if size > 0 { size } else { total }))
        })
        .map_err(|f| f.reason)?;
    if body.len() as u64 != offer.apk.size || sha256_hex(&body) != offer.apk.sha256 {
        return Err("APK: hash mismatch".into());
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let p = dir.join("update.apk");
    crate::write_atomic(&p, &body).map_err(|e| e.to_string())?;
    Ok(p)
}

fn sibling(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(format!(".{suffix}"));
    PathBuf::from(s)
}

/// The public key from its base64 text form (`tv/keys/release.pub`).
pub fn parse_key(b64: &str) -> Result<[u8; 32], String> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|_| "key is not base64".to_string())?;
    raw.try_into().map_err(|_| "key is not 32 bytes".to_string())
}
