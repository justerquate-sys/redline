mod mock;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use lumen_tv_core::auth::{Account, Endpoints};
use lumen_tv_core::net::Http;
use lumen_tv_core::release::{self, Bundle, Manifest};
use lumen_tv_core::rules::{Decision, Rules};

fn http() -> Http {
    Http::new(Duration::from_secs(5))
}

/// The device sign-in against a mocked Google: code, two pending polls (one asking to slow down),
/// the token exchange, refresh, revocation (invalid_grant signs out), sign-out.
#[test]
fn device_sign_in_and_token_exchange() {
    let polls = Arc::new(AtomicU32::new(0));
    let p2 = polls.clone();
    let m = mock::start(Arc::new(move |_m, path, body| {
        let b: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let j = |s: u16, v: Value| (s, "application/json".to_string(), v.to_string().into_bytes());
        match path {
            "/device/code" => {
                assert_eq!(b["client_id"], lumen_tv_core::auth::CLIENT_ID);
                assert!(b["scope"].as_str().unwrap().contains("gdata.youtube.com"));
                assert_eq!(b["device_id"].as_str().unwrap().len(), 36);
                j(
                    200,
                    json!({"device_code": "DEV-123", "user_code": "ABC-DEF-GHI", "expires_in": 1800, "interval": 5, "verification_url": "https://www.google.com/device"}),
                )
            }
            "/token" if b["grant_type"] == "http://oauth.net/grant_type/device/1.0" => {
                assert_eq!(b["code"], "DEV-123");
                assert_eq!(b["client_secret"], lumen_tv_core::auth::CLIENT_SECRET);
                match p2.fetch_add(1, Ordering::SeqCst) {
                    0 => j(428, json!({"error": "authorization_pending", "error_description": "Precondition Required"})),
                    1 => j(403, json!({"error": "slow_down"})),
                    _ => j(
                        200,
                        json!({"access_token": "ya29.first-access-token_0123456789", "expires_in": 3599, "refresh_token": "1//refresh-token", "scope": "x", "token_type": "Bearer"}),
                    ),
                }
            }
            "/token" if b["grant_type"] == "refresh_token" => {
                if b["refresh_token"] == "1//refresh-token" {
                    j(200, json!({"access_token": "ya29.second-access-token_0123456789", "expires_in": 3599}))
                } else {
                    j(400, json!({"error": "invalid_grant"}))
                }
            }
            p if p.starts_with("/revoke?token=") => j(200, json!({})),
            _ => j(404, json!({"error": "not found"})),
        }
    }));
    let saved: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let s2 = saved.clone();
    let mut acct = Account::new(
        Endpoints { base: m.base.clone() },
        lumen_tv_core::auth::new_device_id(),
        None,
        Box::new(move |r| s2.lock().unwrap().push(r.map(String::from))),
    );
    let h = http();
    assert_eq!(acct.token(&h, 0.0).unwrap(), None, "signed out: no token, no request");
    let start: Value = serde_json::from_str(&acct.start(&h, 0.0).unwrap()).unwrap();
    assert_eq!(start["user_code"], "ABC-DEF-GHI");
    assert_eq!(start["verification_url"], "https://www.google.com/device");
    assert!(start.get("device_code").is_none(), "the device code stays in the shell");
    let p = |a: &mut Account| serde_json::from_str::<Value>(&a.poll(&h, 1000.0)).unwrap();
    assert_eq!(p(&mut acct), json!({"state": "pending"}));
    assert_eq!(p(&mut acct), json!({"state": "pending", "slow": true}));
    let done = p(&mut acct);
    assert_eq!(done["state"], "done");
    assert_eq!(done["token"], "ya29.first-access-token_0123456789");
    assert_eq!(done["exp"], 1000.0 + 3_599_000.0);
    assert_eq!(saved.lock().unwrap().as_slice(), &[Some("1//refresh-token".to_string())]);
    assert!(acct.signed_in());
    // Still valid: answered from memory.
    let n = m.log.lock().unwrap().len();
    assert_eq!(acct.token(&h, 2000.0).unwrap().unwrap().0, "ya29.first-access-token_0123456789");
    assert_eq!(m.log.lock().unwrap().len(), n);
    // Close to expiry: refreshed.
    let t = acct.token(&h, 3_400_000.0).unwrap().unwrap();
    assert_eq!(t.0, "ya29.second-access-token_0123456789");
    // A revoked grant signs out.
    let mut gone = Account::new(Endpoints { base: m.base.clone() }, "d".into(), Some("1//revoked".into()), Box::new(|_| {}));
    assert_eq!(gone.token(&h, 0.0).unwrap(), None);
    assert!(!gone.signed_in());
    // Sign out revokes and forgets.
    acct.sign_out(&h);
    assert!(!acct.signed_in());
    assert_eq!(saved.lock().unwrap().last().unwrap(), &None);
    assert!(m.log.lock().unwrap().iter().any(|(_, p, _)| p == "/revoke?token=1//refresh-token"));
}

#[test]
fn poll_outcomes() {
    let m = mock::start(Arc::new(|_m, path, _b| {
        let e = if path == "/device/code" {
            json!({"device_code": "D", "user_code": "U-1234", "verification_url": "https://www.google.com/device"})
        } else {
            json!({"error": "access_denied"})
        };
        (
            if path == "/device/code" { 200 } else { 403 },
            "application/json".into(),
            e.to_string().into_bytes(),
        )
    }));
    let h = http();
    let mut a = Account::new(Endpoints { base: m.base.clone() }, "d".into(), None, Box::new(|_| {}));
    a.start(&h, 0.0).unwrap();
    assert_eq!(a.poll(&h, 1.0), json!({"state": "denied"}).to_string());
    assert_eq!(a.poll(&h, 2.0), json!({"state": "expired"}).to_string(), "no running sign-in");
    a.start(&h, 0.0).unwrap();
    assert_eq!(a.poll(&h, 1_800_001.0), json!({"state": "expired"}).to_string(), "the code ran out");
    // Network down: an error state, the sign-in keeps running.
    let mut b = Account::new(
        Endpoints {
            base: "http://127.0.0.1:9".into(),
        },
        "d".into(),
        None,
        Box::new(|_| {}),
    );
    assert!(b.start(&h, 0.0).is_err());
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32])
}

fn manifest(seq: u64, files: &[(&str, &[u8])], min_shell: u32, apk: Option<(&str, &[u8], u32)>) -> Vec<u8> {
    let mut f = serde_json::Map::new();
    for (n, b) in files {
        f.insert(n.to_string(), json!({"sha256": release::sha256_hex(b), "size": b.len()}));
    }
    let mut m = json!({"schema": 1, "seq": seq, "client": "0.8.6 (abc)", "min_shell": min_shell, "files": f});
    if let Some((name, b, code)) = apk {
        m["apk"] = json!({"version_code": code, "version": "0.2.0", "file": name, "sha256": release::sha256_hex(b), "size": b.len()});
    }
    serde_json::to_vec_pretty(&m).unwrap()
}

fn sign(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(key().sign(b).to_bytes())
}

#[test]
fn manifest_signature() {
    let pk = key().verifying_key().to_bytes();
    let files: [(&str, &[u8]); 4] = [("content.js", b"js"), ("client.css", b"css"), ("native.css", b"n"), ("rules.json", b"[]")];
    let m = manifest(5, &files, 1, None);
    let ok = release::verify_manifest(&m, &sign(&m), &pk).unwrap();
    assert_eq!(ok.seq, 5);
    // Tampered manifest, wrong key, garbage signature: refused.
    let mut bad = m.clone();
    bad[10] ^= 1;
    assert!(release::verify_manifest(&bad, &sign(&m), &pk).is_err());
    let other = SigningKey::from_bytes(&[8u8; 32]).verifying_key().to_bytes();
    assert!(release::verify_manifest(&m, &sign(&m), &other).is_err());
    assert!(release::verify_manifest(&m, "!!", &pk).is_err());
    // A manifest without all files is refused even when signed.
    let partial = manifest(5, &files[..3], 1, None);
    assert!(release::verify_manifest(&partial, &sign(&partial), &pk).is_err());
    assert!(release::check_file(&ok, "content.js", b"js").is_ok());
    assert!(release::check_file(&ok, "content.js", b"jS").is_err());
}

/// A fake GitHub release served locally: a newer bundle is downloaded, verified, cached and chosen;
/// the APK is offered; a tampered file or a bundle needing a newer shell is not installed.
/// The repository was renamed (justerquate-sys/lumen → justerquate-sys/redline). Apps built before have the old
/// name compiled in: GitHub answers the old API path with a 301 to `/repositories/<id>/…` and the assets'
/// `browser_download_url` carry the new name, answered with a 302 to the file's storage. The shell's `Http`
/// follows both, so an installed app keeps finding, verifying and downloading updates.
#[test]
fn update_through_a_renamed_repository() {
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("content.js", b"console.log('v3')".to_vec()),
        ("client.css", b"a{}".to_vec()),
        ("native.css", b"b{}".to_vec()),
        ("rules.json", b"[]".to_vec()),
    ];
    let apk = b"PK-renamed".to_vec();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let man = manifest(300, &refs, 1, Some(("lumen-tv-0.5.0.apk", &apk, 5000)));
    let base = Arc::new(Mutex::new(String::new()));
    let b2 = base.clone();
    let m = mock::start(Arc::new(move |_m, path, _b| {
        let base = b2.lock().unwrap().clone();
        let names = ["lumen-tv-manifest.json", "lumen-tv-manifest.json.sig", "content.js", "client.css", "native.css", "rules.json", "lumen-tv-0.5.0.apk"];
        match path {
            "/repos/justerquate-sys/lumen/releases?per_page=10" => (301, format!("{base}/repositories/42/releases?per_page=10"), b"{\"message\":\"Moved Permanently\"}".to_vec()),
            "/repositories/42/releases?per_page=10" => {
                let assets: Vec<Value> = names
                    .iter()
                    .map(|n| json!({"name": n, "browser_download_url": format!("{base}/justerquate-sys/redline/releases/download/lumen-tv-0.5.0-300/{n}")}))
                    .collect();
                (200, "application/json".into(), json!([{"tag_name": "lumen-tv-0.5.0-300", "assets": assets}]).to_string().into_bytes())
            }
            p if p.starts_with("/justerquate-sys/redline/releases/download/") => (302, format!("{base}/objects/{}", p.rsplit('/').next().unwrap()), Vec::new()),
            "/objects/lumen-tv-manifest.json" => (200, "application/json".into(), man.clone()),
            "/objects/lumen-tv-manifest.json.sig" => (200, "text/plain".into(), sign(&man).into_bytes()),
            "/objects/lumen-tv-0.5.0.apk" => (200, "application/octet-stream".into(), apk.clone()),
            p => match files.iter().find(|(f, _)| p == format!("/objects/{f}")) {
                Some((_, b)) => (200, "text/plain".into(), b.clone()),
                None => (404, "text/plain".into(), Vec::new()),
            },
        }
    }));
    *base.lock().unwrap() = m.base.clone();
    // What a 0.4.0 shell builds from its compiled-in REPO.
    let api = format!("{}/repos/justerquate-sys/lumen/releases?per_page=10", m.base);
    let pk = key().verifying_key().to_bytes();
    let dir = std::env::temp_dir().join(format!("lumen-tv-renamed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = release::check(&http(), &api, &pk, &dir.join("bundle"), 100, 4000).unwrap();
    assert_eq!(out.bundle.expect("bundle installed through the redirects").file("content.js"), "console.log('v3')");
    let offer = out.apk.expect("APK offered");
    let p = release::download_apk(&http(), &offer, &dir).unwrap();
    assert_eq!(std::fs::read(p).unwrap(), b"PK-renamed");
    let log = m.log.lock().unwrap();
    assert!(log.iter().any(|(_, p, _)| p == "/repos/justerquate-sys/lumen/releases?per_page=10") && log.iter().any(|(_, p, _)| p.starts_with("/objects/")));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn update_from_a_fake_release() {
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("content.js", b"console.log('v2')".to_vec()),
        ("client.css", b"a{}".to_vec()),
        ("native.css", b"b{}".to_vec()),
        ("rules.json", b"[]".to_vec()),
    ];
    let apk = b"PK-fake-apk".to_vec();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let state = Arc::new(Mutex::new((manifest(200, &refs, 1, Some(("lumen-tv.apk", &apk, 2))), files.clone(), false)));
    let st = state.clone();
    let base = Arc::new(Mutex::new(String::new()));
    let b2 = base.clone();
    let m = mock::start(Arc::new(move |_m, path, _b| {
        let s = st.lock().unwrap();
        let base = b2.lock().unwrap().clone();
        let names = [
            "lumen-tv-manifest.json",
            "lumen-tv-manifest.json.sig",
            "content.js",
            "client.css",
            "native.css",
            "rules.json",
            "lumen-tv.apk",
        ];
        match path {
            "/repos/o/r/releases" => {
                let assets: Vec<Value> = names
                    .iter()
                    .map(|n| json!({"name": n, "browser_download_url": format!("{base}/dl/{n}")}))
                    .collect();
                // Newest first: a desktop-only release (no TV files) is skipped.
                let desktop = json!({"tag_name": "v0.9.0", "assets": [{"name": "lumen-0.9.0.zip", "browser_download_url": format!("{base}/dl/x.zip")}]});
                (
                    200,
                    "application/json".into(),
                    json!([desktop, {"tag_name": "tv-200", "assets": assets}]).to_string().into_bytes(),
                )
            }
            "/dl/lumen-tv-manifest.json" => (200, "application/json".into(), s.0.clone()),
            "/dl/lumen-tv-manifest.json.sig" => (200, "text/plain".into(), sign(&s.0).into_bytes()),
            "/dl/lumen-tv.apk" => (200, "application/octet-stream".into(), apk.clone()),
            p => {
                let n = p.trim_start_matches("/dl/");
                let mut b = s.1.iter().find(|(f, _)| *f == n).map(|x| x.1.clone()).unwrap_or_default();
                if s.2 && n == "content.js" {
                    b.push(b'!');
                }
                (if b.is_empty() { 404 } else { 200 }, "text/plain".into(), b)
            }
        }
    }));
    *base.lock().unwrap() = m.base.clone();
    let api = format!("{}/repos/o/r/releases", m.base);
    let pk = key().verifying_key().to_bytes();
    let dir = std::env::temp_dir().join(format!("lumen-tv-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("bundle");
    let h = http();
    let builtin = Bundle {
        seq: 100,
        client: "builtin".into(),
        source: "builtin",
        ..Default::default()
    };

    // A tampered file: nothing installed, the cache stays empty.
    state.lock().unwrap().2 = true;
    assert!(release::check(&h, &api, &pk, &cache, 100, 1).unwrap_err().reason.contains("hash mismatch"));
    assert!(release::load_cached(&cache, &pk).is_none());
    state.lock().unwrap().2 = false;

    let out = release::check(&h, &api, &pk, &cache, 100, 1).unwrap();
    let b = out.bundle.expect("installed");
    assert_eq!((b.seq, b.file("content.js")), (200, "console.log('v2')"));
    let offer = out.apk.expect("APK offered");
    assert_eq!(offer.apk.version_code, 2);
    let p = release::download_apk(&h, &offer, &dir).unwrap();
    assert_eq!(std::fs::read(p).unwrap(), b"PK-fake-apk");
    // Cached, re-verified on load, newer than the built-in copy.
    let c = release::load_cached(&cache, &pk).expect("cached");
    assert_eq!(release::choose(Some(c.clone()), builtin.clone()).seq, 200);
    // A newer APK's built-in copy beats an older cache.
    assert_eq!(release::choose(Some(c), Bundle { seq: 300, ..builtin.clone() }).source, "builtin");
    // Up to date: nothing downloaded again.
    assert!(release::check(&h, &api, &pk, &cache, 200, 2).unwrap().bundle.is_none());
    // A cached file changed on disk: the cache is ignored.
    std::fs::write(cache.join("content.js"), b"evil").unwrap();
    assert!(release::load_cached(&cache, &pk).is_none());
    // A bundle that needs a newer shell is not installed.
    state.lock().unwrap().0 = manifest(400, &refs, 9, None);
    let out = release::check(&h, &api, &pk, &cache, 200, 2).unwrap();
    assert!(out.needs_shell && out.bundle.is_none());
    let _ = std::fs::remove_dir_all(&dir);
    let _: Option<Manifest> = None;
}

#[test]
fn the_clients_rules() {
    let r = Rules::parse(include_str!("../../../extension/rules.json")).unwrap();
    let yt = "https://www.youtube.com/";
    assert_eq!(r.decide("https://www.youtube.com/youtubei/v1/log_event?alt=json", false, yt), Decision::Block);
    assert_eq!(r.decide("https://www.youtube.com/pagead/viewthroughconversion/1", false, yt), Decision::Block);
    assert_eq!(r.decide("https://www.youtube.com/csi_204?v=3", false, yt), Decision::Block);
    assert_eq!(r.decide("https://googleads.g.doubleclick.net/pagead/id", false, yt), Decision::Block);
    assert_eq!(
        r.decide("https://doubleclick.net/x", false, "https://example.com/"),
        Decision::Pass,
        "only from YouTube"
    );
    assert_eq!(r.decide("https://www.youtube.com/youtubei/v1/browse", false, yt), Decision::Pass);
    assert_eq!(
        r.decide("https://rr3---sn-4g5e6nzz.googlevideo.com/videoplayback?itag=243&range=0-100", false, yt),
        Decision::Cors
    );
    assert_eq!(r.decide("https://www.youtube.com/watch?v=x", true, yt), Decision::Pass);
    assert_eq!(r.decide("https://i.ytimg.com/vi/x/hq.jpg", false, yt), Decision::Pass);
    // Native fallback pages: their own requests are allowed, the blocks (higher priority) still hold.
    let native = "https://www.youtube.com/watch?v=x&yt-native=1";
    assert_eq!(r.decide("https://www.youtube.com/youtubei/v1/log_event", false, native), Decision::Block);
    assert_eq!(r.decide("https://www.youtube.com/youtubei/v1/player", false, native), Decision::Pass);
    assert_eq!(
        r.decide("https://rr1---sn-x.googlevideo.com/videoplayback?a=1", false, native),
        Decision::Cors,
        "the header rule (priority 3) outranks allowAllRequests (2)"
    );
    assert!(lumen_tv_core::rules::url_filter("||doubleclick.net^", "https://ad.doubleclick.net/"));
    assert!(!lumen_tv_core::rules::url_filter("||doubleclick.net^", "https://notdoubleclick.net/"));
    assert!(lumen_tv_core::rules::url_filter(
        "|https://www.youtube.com/embed/",
        "https://www.youtube.com/embed/abc"
    ));
    assert!(!lumen_tv_core::rules::url_filter(
        "|https://www.youtube.com/embed/",
        "https://x.com/?https://www.youtube.com/embed/"
    ));
}

#[test]
fn storage_shim() {
    let p = std::env::temp_dir().join(format!("ytc-store-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let mut s = lumen_tv_core::store::Store::open(p.clone());
    s.set(&json!({"a": 1, "b": {"x": [1, 2]}})).unwrap();
    assert_eq!(s.get(&json!("a")), json!({"a": 1}));
    assert_eq!(s.get(&json!(["a", "zz"])), json!({"a": 1}));
    assert_eq!(s.get(&json!({"zz": 5, "a": 0})), json!({"zz": 5, "a": 1}), "defaults");
    let s2 = lumen_tv_core::store::Store::open(p.clone());
    assert_eq!(s2.get(&Value::Null), json!({"a": 1, "b": {"x": [1, 2]}}), "persisted");
    s.remove(&json!("a")).unwrap();
    assert_eq!(lumen_tv_core::store::Store::open(p.clone()).get(&Value::Null), json!({"b": {"x": [1, 2]}}));
    let _ = std::fs::remove_file(&p);
}

/// A release that carries the telemetry variant (`content-dev.js`): verified, cached and loaded with the bundle;
/// a cached variant that no longer verifies is left out while the bundle stays usable.
#[test]
fn the_telemetry_variant_rides_along() {
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("content.js", b"public()".to_vec()),
        ("client.css", b"a{}".to_vec()),
        ("native.css", b"b{}".to_vec()),
        ("rules.json", b"[]".to_vec()),
        (release::DEV_FILE, b"dev()".to_vec()),
    ];
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let man = manifest(300, &refs, 1, None);
    let base = Arc::new(Mutex::new(String::new()));
    let b2 = base.clone();
    let f2 = files.clone();
    let m = mock::start(Arc::new(move |_m, path, _b| {
        let base = b2.lock().unwrap().clone();
        match path {
            "/repos/o/r/releases" => {
                let names = ["lumen-tv-manifest.json", "lumen-tv-manifest.json.sig", "content.js", "client.css", "native.css", "rules.json", release::DEV_FILE];
                let assets: Vec<Value> = names.iter().map(|n| json!({"name": n, "browser_download_url": format!("{base}/dl/{n}")})).collect();
                (200, "application/json".into(), json!([{"tag_name": "tv-300", "assets": assets}]).to_string().into_bytes())
            }
            "/dl/lumen-tv-manifest.json" => (200, "application/json".into(), man.clone()),
            "/dl/lumen-tv-manifest.json.sig" => (200, "text/plain".into(), sign(&man).into_bytes()),
            p => {
                let n = p.trim_start_matches("/dl/");
                let b = f2.iter().find(|(f, _)| *f == n).map(|x| x.1.clone()).unwrap_or_default();
                (if b.is_empty() { 404 } else { 200 }, "text/plain".into(), b)
            }
        }
    }));
    *base.lock().unwrap() = m.base.clone();
    let pk = key().verifying_key().to_bytes();
    let dir = std::env::temp_dir().join(format!("lumen-tv-dev-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("bundle");
    let out = release::check(&http(), &format!("{}/repos/o/r/releases", m.base), &pk, &cache, 100, 1).unwrap();
    assert_eq!(out.bundle.unwrap().file(release::DEV_FILE), "dev()");
    assert_eq!(release::load_cached(&cache, &pk).unwrap().file(release::DEV_FILE), "dev()");
    std::fs::write(cache.join(release::DEV_FILE), b"evil()").unwrap();
    let b = release::load_cached(&cache, &pk).expect("the bundle stays");
    assert_eq!((b.file("content.js"), b.file(release::DEV_FILE)), ("public()", ""));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Telemetry reports go to the debug center as `POST <url>/incident`, stamped with the build and the platform;
/// a collector that refuses keeps the report queued.
#[test]
fn telemetry_reaches_the_debug_center() {
    use lumen_tv_core::tel;
    let refuse = Arc::new(Mutex::new(false));
    let r2 = refuse.clone();
    let m = mock::start(Arc::new(move |_m, _p, _b| if *r2.lock().unwrap() { (503, "text/plain".into(), b"no".to_vec()) } else { (200, "text/plain".into(), b"stored".to_vec()) }));
    let mut q = tel::Queue::default();
    q.push(true, tel::stamp(r#"{"kind":"perf","schema":2,"events":[]}"#, "0.10.0 (x) (TV 0.2.0, builtin)").unwrap());
    let url = m.base.clone();
    let item = q.front().unwrap().to_string();
    tel::post(&http(), &url, &item).unwrap();
    q.delivered(&item);
    assert!(q.is_empty());
    let log = m.log.lock().unwrap().clone();
    assert_eq!((log[0].0.as_str(), log[0].1.as_str()), ("POST", "/incident"));
    let v: Value = serde_json::from_str(&log[0].2).unwrap();
    assert_eq!((v["platform"].as_str(), v["build"].as_str(), v["kind"].as_str()), (Some("tv"), Some("0.10.0 (x) (TV 0.2.0, builtin)"), Some("perf")));
    *refuse.lock().unwrap() = true;
    assert!(tel::post(&http(), &url, &item).unwrap_err().contains("503"));
}

/// The ready Home response is kept for one account: the fingerprint names it without the token.
#[test]
fn account_fingerprint() {
    let ep = || Endpoints { base: "http://127.0.0.1:9".into() };
    let a = Account::new(ep(), "d".into(), Some("1//refresh-a".into()), Box::new(|_| {}));
    let b = Account::new(ep(), "d".into(), Some("1//refresh-b".into()), Box::new(|_| {}));
    let out = Account::new(ep(), "d".into(), None, Box::new(|_| {}));
    assert!(a.fingerprint().starts_with("in:") && a.fingerprint().len() == 19 && !a.fingerprint().contains("refresh"));
    assert_ne!(a.fingerprint(), b.fingerprint());
    assert_eq!(a.fingerprint(), Account::new(ep(), "other".into(), Some("1//refresh-a".into()), Box::new(|_| {})).fingerprint());
    assert_eq!(out.fingerprint(), "out");
}

/// Format 2 (the Wasm as a file the shell serves): its release needs shell 0.3.0 (`min_shell` 3000). A 0.2.0 shell
/// keeps the bundle it runs and is offered the APK; a 0.3.0 shell installs it with its Wasm (binary, verified, cached).
#[test]
fn format_two_needs_the_new_app() {
    let wasm: Vec<u8> = vec![0, 97, 115, 109, 1, 0, 0, 0, 0xff, 0xfe];
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("content.js", b"loader('/__lumen/client.wasm')".to_vec()),
        ("client.css", b"a{}".to_vec()),
        ("native.css", b"b{}".to_vec()),
        ("rules.json", b"[]".to_vec()),
        (release::WASM_FILE, wasm.clone()),
    ];
    let apk = b"PK-0.3.0".to_vec();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let man = manifest(500, &refs, 3000, Some(("lumen-tv-0.3.0.apk", &apk, 3000)));
    let base = Arc::new(Mutex::new(String::new()));
    let tamper = Arc::new(Mutex::new(false));
    let (b2, t2, f2) = (base.clone(), tamper.clone(), files.clone());
    let m = mock::start(Arc::new(move |_m, path, _b| {
        let base = b2.lock().unwrap().clone();
        match path {
            "/repos/o/r/releases" => {
                let names = ["lumen-tv-manifest.json", "lumen-tv-manifest.json.sig", "content.js", "client.css", "native.css", "rules.json", release::WASM_FILE, "lumen-tv-0.3.0.apk"];
                let assets: Vec<Value> = names.iter().map(|n| json!({"name": n, "browser_download_url": format!("{base}/dl/{n}")})).collect();
                (200, "application/json".into(), json!([{"tag_name": "tv-500", "assets": assets}]).to_string().into_bytes())
            }
            "/dl/lumen-tv-manifest.json" => (200, "application/json".into(), man.clone()),
            "/dl/lumen-tv-manifest.json.sig" => (200, "text/plain".into(), sign(&man).into_bytes()),
            "/dl/lumen-tv-0.3.0.apk" => (200, "application/octet-stream".into(), apk.clone()),
            p => {
                let n = p.trim_start_matches("/dl/");
                let mut b = f2.iter().find(|(f, _)| *f == n).map(|x| x.1.clone()).unwrap_or_default();
                if *t2.lock().unwrap() && n == release::WASM_FILE {
                    b[9] ^= 1;
                }
                (if b.is_empty() { 404 } else { 200 }, "application/octet-stream".into(), b)
            }
        }
    }));
    *base.lock().unwrap() = m.base.clone();
    let api = format!("{}/repos/o/r/releases", m.base);
    let pk = key().verifying_key().to_bytes();
    let dir = std::env::temp_dir().join(format!("lumen-tv-f2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache = dir.join("bundle");
    // A 0.2.0 app (version code 2000): nothing installed, it keeps what it runs, the new app is offered.
    let old = release::check(&http(), &api, &pk, &cache, 100, 2000).unwrap();
    assert!(old.needs_shell && old.bundle.is_none() && !cache.exists());
    assert_eq!(old.apk.expect("the APK is offered").apk.version_code, 3000);
    // A tampered Wasm: refused.
    *tamper.lock().unwrap() = true;
    assert!(release::check(&http(), &api, &pk, &cache, 100, 3000).unwrap_err().reason.contains("hash mismatch"));
    *tamper.lock().unwrap() = false;
    // The 0.3.0 app: installed, the Wasm kept as bytes, cached and re-verified.
    let new = release::check(&http(), &api, &pk, &cache, 100, 3000).unwrap();
    let b = new.bundle.expect("installed");
    assert_eq!(b.bin_file(release::WASM_FILE), &wasm[..]);
    assert!(new.apk.is_none(), "this app is the offered one");
    let c = release::load_cached(&cache, &pk).expect("cached");
    assert_eq!(c.bin_file(release::WASM_FILE), &wasm[..]);
    // The cached Wasm damaged on disk: the whole bundle is refused (content.js cannot run without it).
    std::fs::write(cache.join(release::WASM_FILE), b"x").unwrap();
    assert!(release::load_cached(&cache, &pk).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- The two routes to a release's files (GitHub's website answering 504 while its API works, 8 Nov 2026).

/// What a route answers for a file (None = it serves the file): status, extra header lines for the mock.
type Answering = Arc<dyn Fn(&str) -> Option<(u16, &'static str)> + Send + Sync>;

fn always(status: u16) -> Answering {
    Arc::new(move |_| Some((status, "")))
}

fn serves() -> Answering {
    Arc::new(|_| None)
}

/// A fake GitHub: `/repos/o/r/releases`, `/dl/<file>` (the website), `/api/<file>` (the release API's asset route).
/// The APK is offered to a shell older than it.
struct FakeGitHub {
    m: mock::Mock,
    api: String,
    dir: std::path::PathBuf,
    pk: [u8; 32],
}

const APK: &[u8] = b"PK-fake-apk-with-some-length";

fn route_files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("content.js", b"console.log('v2')".to_vec()),
        ("client.css", b"a{}".to_vec()),
        ("native.css", b"b{}".to_vec()),
        ("rules.json", b"[]".to_vec()),
    ]
}

/// `bad_api`: what the API route delivers wrongly ("sig": a signature of other bytes, a file name: changed bytes).
fn fake_github(tag: &str, web: Answering, api: Answering, bad_api: &'static str) -> FakeGitHub {
    let files = route_files();
    let refs: Vec<(&str, &[u8])> = files.iter().map(|(n, b)| (*n, b.as_slice())).collect();
    let man = manifest(200, &refs, 1, Some(("lumen-tv-0.2.0.apk", APK, 2)));
    let base = Arc::new(Mutex::new(String::new()));
    let b2 = base.clone();
    let m = mock::start(Arc::new(move |_m, path, _b| {
        let b = b2.lock().unwrap().clone();
        if path == "/repos/o/r/releases" {
            let mut names = vec!["lumen-tv-manifest.json", "lumen-tv-manifest.json.sig", "lumen-tv-0.2.0.apk"];
            names.extend(files.iter().map(|f| f.0));
            let assets: Vec<Value> = names.iter().map(|n| json!({"name": n, "url": format!("{b}/api/{n}"), "browser_download_url": format!("{b}/dl/{n}")})).collect();
            return (200, "application/json".to_string(), json!([{"tag_name": "tv-200", "assets": assets}]).to_string().into_bytes());
        }
        let (route, name) = path[1..].split_once('/').unwrap_or(("", ""));
        let fail = if route == "api" { api(name) } else { web(name) };
        if let Some((status, headers)) = fail {
            // (The mock writes the content type as it is: an extra header line goes along with it.)
            return (status, format!("text/plain{headers}"), b"not this time".to_vec());
        }
        let wrong = route == "api" && bad_api == name;
        let wrong_sig = route == "api" && bad_api == "sig" && name == release::SIG;
        let mut bytes = match name {
            release::MANIFEST => man.clone(),
            release::SIG if wrong_sig => sign(b"other bytes").into_bytes(),
            release::SIG => sign(&man).into_bytes(),
            "lumen-tv-0.2.0.apk" => APK.to_vec(),
            n => files.iter().find(|(f, _)| *f == n).map(|x| x.1.clone()).unwrap_or_default(),
        };
        if wrong {
            bytes.push(b'!');
        }
        (if bytes.is_empty() { 404 } else { 200 }, "application/octet-stream".to_string(), bytes)
    }));
    *base.lock().unwrap() = m.base.clone();
    let dir = std::env::temp_dir().join(format!("lumen-tv-routes-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let api = format!("{}/repos/o/r/releases", m.base);
    FakeGitHub { m, api, dir, pk: key().verifying_key().to_bytes() }
}

impl FakeGitHub {
    fn check(&self) -> Result<release::Outcome, release::Failure> {
        release::check(&http(), &self.api, &self.pk, &self.dir.join("bundle"), 100, 1)
    }

    /// Paths asked for, in order.
    fn asked(&self) -> Vec<String> {
        self.m.log.lock().unwrap().iter().map(|l| l.1.clone()).collect()
    }

    fn count(&self, prefix: &str) -> usize {
        self.asked().iter().filter(|p| p.starts_with(prefix)).count()
    }

    fn cached(&self) -> Option<Bundle> {
        release::load_cached(&self.dir.join("bundle"), &self.pk)
    }
}

impl Drop for FakeGitHub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The normal case: one API request (the lookup), every file from the website: what a check always cost.
#[test]
fn a_normal_check_costs_one_api_request() {
    let g = fake_github("normal", serves(), always(500), "");
    let out = g.check().unwrap();
    assert_eq!(out.bundle.unwrap().seq, 200);
    assert_eq!(g.count("/repos/"), 1);
    assert_eq!(g.count("/api/"), 0, "the API's rate limit is left alone");
    assert_eq!(g.count("/dl/"), 6);
    assert!(g.m.heads.lock().unwrap().iter().all(|(_, h)| !h.contains("authorization")), "no auth, ever");
    // The offer carries both routes.
    let offer = out.apk.unwrap();
    assert!(offer.url.ends_with("/dl/lumen-tv-0.2.0.apk") && offer.api_url.unwrap().ends_with("/api/lumen-tv-0.2.0.apk"));
}

/// The first route answers 504: the API route delivers, and after the first failure it comes first for the rest of
/// the check, so the failing route costs its 11 s once.
#[test]
fn a_failing_website_falls_over_to_the_api_once() {
    let g = fake_github("fallback", always(504), serves(), "");
    let b = g.check().unwrap().bundle.expect("installed through the API route");
    assert_eq!((b.seq, b.file("content.js")), (200, "console.log('v2')"));
    assert!(g.cached().is_some_and(|c| c.seq == 200), "verified and cached like any other");
    assert_eq!(g.asked()[..3], ["/repos/o/r/releases".to_string(), format!("/dl/{}", release::MANIFEST), format!("/api/{}", release::MANIFEST)]);
    assert_eq!(g.count("/dl/"), 1, "the failing route is asked once in this check");
    assert_eq!(g.count("/api/"), 6, "manifest, signature, four files: each once");
    let heads = g.m.heads.lock().unwrap();
    for (path, h) in heads.iter().filter(|(p, _)| p.starts_with("/api/")) {
        assert!(h.contains("accept: application/octet-stream"), "{path}: {h}");
    }
    assert!(heads.iter().all(|(_, h)| !h.contains("authorization")), "no auth, ever");
    drop(heads);
    // The next check starts with the website again (it may be back), then up to date: 3 API requests at most.
    let g2 = g.m.log.lock().unwrap().len();
    release::check(&http(), &g.api, &g.pk, &g.dir.join("bundle"), 200, 2).unwrap();
    let asked = g.asked();
    assert_eq!(asked[g2..], ["/repos/o/r/releases".to_string(), format!("/dl/{}", release::MANIFEST), format!("/api/{}", release::MANIFEST), format!("/api/{}", release::SIG)]);
}

/// The first route's failure can be anything of its kind: a 429 from the website, a connection that fails.
#[test]
fn a_rate_limited_or_unreachable_first_route_falls_over_too() {
    let g = fake_github("429", always(429), serves(), "");
    assert!(g.check().unwrap().bundle.is_some());
    assert_eq!(g.count("/dl/"), 1);
    // A closed port for the website's URLs.
    let g = fake_github("refused", serves(), serves(), "");
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let asset = release::Asset { web: format!("http://{closed}/x"), api: Some(format!("{}/api/lumen-tv-0.2.0.apk", g.m.base)) };
    let offer = release::ApkOffer { apk: apk_entry(), url: asset.web, api_url: asset.api };
    let p = release::download_apk(&http(), &offer, &g.dir).unwrap();
    assert_eq!(std::fs::read(p).unwrap(), APK);
    // Nothing reachable: said in words, nothing written.
    let offer = release::ApkOffer { apk: apk_entry(), url: format!("http://{closed}/x"), api_url: Some(format!("http://{closed}/y")) };
    let e = release::download_apk(&http(), &offer, &g.dir.join("none")).unwrap_err();
    assert!(e.starts_with("No connection to GitHub (APK: download "), "{e}");
    assert!(!e.contains("127.0.0.1"), "no addresses in the reason: {e}");
    assert!(!g.dir.join("none").exists());
}

fn apk_entry() -> release::Apk {
    release::Apk { version_code: 2, version: "0.2.0".into(), file: "lumen-tv-0.2.0.apk".into(), sha256: release::sha256_hex(APK), size: APK.len() as u64 }
}

/// Both routes fail: a reason in words with the technical part, the check stops there (no more requests), and the
/// bundle already cached stays.
#[test]
fn both_routes_failing_says_so_in_words() {
    let g = fake_github("both", always(504), always(502), "");
    let e = g.check().unwrap_err();
    assert_eq!(e.reason, format!("GitHub isn't answering ({}: download HTTP 504, API HTTP 502)", release::MANIFEST));
    assert_eq!(e.wait_secs, 0);
    assert_eq!(g.asked().len(), 3, "the lookup and one try per route");
    assert!(g.cached().is_none());
}

/// A route that flips between working and not working cannot drag a check out: at most MAX_ROUTE_FAILURES failed tries.
#[test]
fn a_flapping_route_ends_the_check_early() {
    let names = [release::MANIFEST, release::SIG, "content.js", "client.css", "native.css", "rules.json"];
    let web: Answering = Arc::new(move |n| (names.iter().position(|x| *x == n).unwrap() % 2 == 0).then_some((504, "")));
    let api: Answering = Arc::new(move |n| (names.iter().position(|x| *x == n).unwrap() % 2 == 1).then_some((503, "")));
    let g = fake_github("flap", web, api, "");
    let e = g.check().unwrap_err();
    assert!(e.reason.starts_with("GitHub isn't answering"), "{}", e.reason);
    // Manifest, signature and content.js arrived on their second try; client.css's failed try was the fourth.
    assert_eq!(g.count("/dl/") + g.count("/api/"), 3 + release::MAX_ROUTE_FAILURES as usize, "{:?}", g.asked());
    assert!(g.cached().is_none());
}

/// A file that is simply not there is not an outage: the other route is not asked.
#[test]
fn a_missing_file_does_not_try_the_other_route() {
    let g = fake_github("404", always(404), serves(), "");
    assert_eq!(g.check().unwrap_err().reason, format!("{}: HTTP 404", release::MANIFEST));
    assert_eq!(g.count("/api/"), 0);
}

/// The fallback route is no way around the signature or the hashes.
#[test]
fn the_fallback_route_passes_the_same_checks() {
    let g = fake_github("badsig", always(504), serves(), "sig");
    let e = g.check().unwrap_err();
    assert!(e.reason.contains("signature"), "{}", e.reason);
    assert!(g.cached().is_none(), "nothing installed");
    assert!(!g.dir.join("bundle.new").join("content.js").exists(), "no file was even fetched");

    let g = fake_github("badman", always(504), serves(), release::MANIFEST);
    assert!(g.check().unwrap_err().reason.contains("signature"));
    assert!(g.cached().is_none());

    let g = fake_github("badfile", always(504), serves(), "client.css");
    let e = g.check().unwrap_err();
    assert_eq!(e.reason, "client.css: hash mismatch");
    assert!(g.cached().is_none());

    // The APK: the hash in the signed manifest decides, whichever route delivered it.
    let g = fake_github("badapk", always(504), serves(), "lumen-tv-0.2.0.apk");
    let offer = g.check().unwrap().apk.unwrap();
    let e = release::download_apk(&http(), &offer, &g.dir.join("apk")).unwrap_err();
    assert_eq!(e, "APK: hash mismatch");
    assert!(!g.dir.join("apk").join("update.apk").exists());
}

/// The APK download's second route, with its progress starting over and ending at the signed size.
#[test]
fn the_apk_falls_over_with_its_progress() {
    let g = fake_github("apk", always(504), serves(), "");
    let offer = g.check().unwrap().apk.unwrap();
    let mut seen = Vec::new();
    let p = release::download_apk_progress(&http(), &offer, &g.dir, &mut |got, total| seen.push((got, total))).unwrap();
    assert_eq!(std::fs::read(p).unwrap(), APK);
    let size = APK.len() as u64;
    assert_eq!(seen.first(), Some(&(0, size)));
    assert_eq!(seen.last(), Some(&(size, size)));
    assert!(seen.iter().all(|(got, total)| got <= total && *total == size), "the error answer's body is no progress: {seen:?}");
    assert!(g.m.heads.lock().unwrap().iter().any(|(p, h)| p == "/api/lumen-tv-0.2.0.apk" && h.contains("accept: application/octet-stream")));
    // Both failing.
    let g2 = fake_github("apk2", serves(), serves(), "");
    let bad = release::ApkOffer { apk: apk_entry(), url: format!("{}/dl/gone", g.m.base), api_url: Some(format!("{}/dl/gone", g2.m.base)) };
    let e = release::download_apk(&http(), &bad, &g.dir.join("x")).unwrap_err();
    assert!(e.starts_with("GitHub isn't answering (APK: download HTTP 504, API HTTP 404"), "{e}");
}

/// GitHub's rate limit: a readable reason, how long it lasts, and what the next automatic check makes of it.
#[test]
fn a_rate_limit_is_said_in_words() {
    let reset = (lumen_tv_core::now_ms() / 1000.0) as u64 + 600;
    let header: &'static str = Box::leak(format!("\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {reset}").into_boxed_str());
    // The lookup itself (it has no second route).
    let m = mock::start(Arc::new(move |_, _, _| (403, format!("application/json{header}"), br#"{"message":"API rate limit exceeded"}"#.to_vec())));
    let pk = key().verifying_key().to_bytes();
    let e = release::check(&http(), &format!("{}/repos/o/r/releases", m.base), &pk, std::path::Path::new("/nonexistent"), 100, 1).unwrap_err();
    assert!(e.reason.starts_with("GitHub's rate limit was reached (release lookup: HTTP 403, resets in "), "{}", e.reason);
    assert!((595..=600).contains(&e.wait_secs), "{}", e.wait_secs);
    assert!(release::next_check_ms(1, e.wait_secs) >= 595_000.0, "not before it is over");
    // A TV whose clock is days behind: the reset is within the hour whatever it thinks.
    let far: &'static str = Box::leak(format!("\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Reset: {}", reset + 500_000).into_boxed_str());
    let g = fake_github("clock", always(504), Arc::new(move |_| Some((403, far))), "");
    let e = g.check().unwrap_err();
    assert_eq!(e.wait_secs, 3600);
    assert!(e.reason.contains("resets in 60 min"), "{}", e.reason);
    // Website down, API out of requests.
    let api: Answering = Arc::new(move |_| Some((403, header)));
    let g = fake_github("limit", always(504), api, "");
    let e = g.check().unwrap_err();
    assert!(e.reason.starts_with(&format!("GitHub's rate limit was reached ({}: download HTTP 504, API HTTP 403, resets in ", release::MANIFEST)), "{}", e.reason);
    // Another way of saying it: 429 with Retry-After.
    let api: Answering = Arc::new(|_| Some((429, "\r\nRetry-After: 120")));
    let g = fake_github("after", always(504), api, "");
    let e = g.check().unwrap_err();
    assert_eq!(e.wait_secs, 120);
    assert!(e.reason.contains("resets in 2 min"), "{}", e.reason);
    // A 403 that is no rate limit stays what it is, and sends nobody to the other route.
    let g = fake_github("forbidden", always(403), serves(), "");
    assert_eq!(g.check().unwrap_err().reason, format!("{}: HTTP 403", release::MANIFEST));
    assert_eq!(g.count("/api/"), 0);
    // A rate-limited API after the website failed once: back to the website for the rest.
    let flip = Arc::new(AtomicU32::new(0));
    let f2 = flip.clone();
    let web: Answering = Arc::new(move |_| (f2.fetch_add(1, Ordering::SeqCst) == 0).then_some((504, "")));
    let api: Answering = Arc::new(move |n| (n != release::MANIFEST).then_some((403, header)));
    let g = fake_github("back", web, api, "");
    assert!(g.check().unwrap().bundle.is_some());
    assert_eq!((g.count("/api/"), g.count("/dl/")), (2, 6), "{:?}", g.asked());
}

/// When the automatic check runs next.
#[test]
fn failed_checks_retry_sooner_then_return_to_the_schedule() {
    const MIN: f64 = 60_000.0;
    let at = |fails| release::next_check_ms(fails, 0) / MIN;
    assert_eq!((at(0), at(1), at(2), at(3), at(4), at(9)), (360.0, 5.0, 15.0, 60.0, 360.0, 360.0));
    assert_eq!(release::next_check_ms(1, 1800) / MIN, 30.0, "a rate limit outlasts the step");
    assert_eq!(release::next_check_ms(3, 1800) / MIN, 60.0);
    assert_eq!(release::next_check_ms(1, 86_400) / MIN, 120.0, "at most 2 h, whatever a header says");
}

/// A route that sends part of the APK and then nothing: the other route gets its turn long before the 20-minute limit.
#[test]
fn a_stalled_apk_body_falls_over_to_the_other_route() {
    use std::io::{Read, Write};
    let g = fake_github("stall", serves(), serves(), "");
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let stalled = format!("http://{}/x", l.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let _ = s.read(&mut [0u8; 2048]);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\nPK-", APK.len());
            let _ = s.flush();
            held.push(s);
        }
    });
    let offer = release::ApkOffer { apk: apk_entry(), url: stalled.clone(), api_url: Some(format!("{}/api/lumen-tv-0.2.0.apk", g.m.base)) };
    let h = Http::download(Duration::from_secs(120), Duration::from_millis(400));
    let started = std::time::Instant::now();
    let mut seen = Vec::new();
    let p = release::download_apk_progress(&h, &offer, &g.dir, &mut |got, _| seen.push(got)).unwrap();
    assert_eq!(std::fs::read(p).unwrap(), APK);
    assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
    assert_eq!(seen.first(), Some(&0));
    assert!(seen.contains(&3), "the part it did send counted: {seen:?}");
    assert_eq!(seen.iter().filter(|g| **g == 0).count(), 2, "the count starts over: {seen:?}");
    // Both routes stalling: said in words, not hanging.
    let offer = release::ApkOffer { apk: apk_entry(), url: stalled.clone(), api_url: Some(stalled) };
    let e = release::download_apk(&h, &offer, &g.dir.join("none")).unwrap_err();
    assert!(e.starts_with("No connection to GitHub (APK: download timeout: no data"), "{e}");
}

/// The update thread's one due time: only automatic failures back off, a press reuses a fresh check and never pushes the
/// next check out, and a clock corrected backwards does not stall it.
#[test]
fn the_schedule_of_automatic_checks() {
    use release::{CHECK_EVERY_MS, Schedule};
    const MIN: f64 = 60_000.0;
    let t0 = 1_000_000_000_000.0;
    let mut s = Schedule::default();
    assert!(!s.recent(t0), "nothing to reuse yet");
    // Automatic failures: 5, 15, 60 min, then the schedule.
    for (i, step) in [5.0, 15.0, 60.0, 360.0].into_iter().enumerate() {
        assert_eq!(s.finished(t0, true, Some(0)), t0 + step * MIN, "failure {}", i + 1);
    }
    assert_eq!(s.fails, 4);
    // Any success: back to the schedule, the back-off forgotten.
    assert_eq!(s.finished(t0, false, None), t0 + CHECK_EVERY_MS);
    assert_eq!(s.fails, 0);
    // Four failed presses from a healthy schedule: no back-off, and the next automatic check 5 min off, not 6 h.
    let mut s = Schedule { next_at: t0 + CHECK_EVERY_MS, ..Default::default() };
    for press in 0..4 {
        let at = t0 + press as f64 * MIN;
        assert_eq!(s.finished(at, false, Some(0)), t0 + 5.0 * MIN, "press {press}");
    }
    assert_eq!(s.fails, 0);
    // A press never pushes a retry out, even at the end of the back-off.
    let mut s = Schedule { fails: 1, next_at: t0 + 3.0 * MIN, ..Default::default() };
    assert_eq!(s.finished(t0, false, Some(0)), t0 + 3.0 * MIN);
    assert_eq!(s.fails, 1);
    // A rate limit a press learned of is waited out, by the automatic check too (at most 2 h).
    let mut s = Schedule { next_at: t0 + MIN, ..Default::default() };
    assert_eq!(s.finished(t0, false, Some(1800)), t0 + 30.0 * MIN);
    let mut s = Schedule { next_at: t0 + MIN, ..Default::default() };
    assert_eq!(s.finished(t0, false, Some(99_999)), t0 + 120.0 * MIN);
    // A check of any kind is reusable for a press for 60 s.
    let mut s = Schedule::default();
    s.finished(t0, true, None);
    assert!(s.recent(t0 + 59_000.0) && !s.recent(t0 + 61_000.0));
    // Due time: the plain case; past due; a clock corrected back by a year.
    let mut s = Schedule { next_at: t0 + 5.0 * MIN, ..Default::default() };
    assert_eq!(s.due_in(t0), 5.0 * MIN);
    assert!(s.due_in(t0 + 6.0 * MIN) < 0.0);
    let back = t0 - 365.0 * 24.0 * 60.0 * MIN;
    assert_eq!(s.due_in(back), CHECK_EVERY_MS);
    assert_eq!(s.next_at, back + CHECK_EVERY_MS, "and it stays fixed");
    assert!(!s.recent(back), "nor does a jump count as recent");
}

/// By hand and rarely, against the real GitHub (`cargo test --locked --test core -- --ignored real_github`): the API
/// route, without the website, delivers the signed files of the latest release, and the compiled-in key accepts them.
#[test]
#[ignore]
fn real_github_delivers_the_signed_manifest_through_the_api() {
    let h = Http::new(Duration::from_secs(30));
    let rel = h.get("https://api.github.com/repos/justerquate-sys/redline/releases?per_page=10", &[("Accept", "application/vnd.github+json")]).unwrap();
    assert_eq!(rel.status, 200);
    let assets = release::assets(&rel.json().unwrap());
    let get = |name: &str| {
        let a = h.get(assets[name].api.as_ref().expect("the asset's API url"), &[("Accept", "application/octet-stream")]).unwrap();
        assert_eq!(a.status, 200, "{name}");
        a.body
    };
    let key = release::parse_key(include_str!("../../keys/release.pub")).unwrap();
    let m = release::verify_manifest(&get(release::MANIFEST), &String::from_utf8(get(release::SIG)).unwrap(), &key).unwrap();
    release::check_file(&m, "rules.json", &get("rules.json")).unwrap();
}
