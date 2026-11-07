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
    assert!(release::check(&h, &api, &pk, &cache, 100, 1).unwrap_err().contains("hash mismatch"));
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
    assert!(release::check(&http(), &api, &pk, &cache, 100, 3000).unwrap_err().contains("hash mismatch"));
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
