//! Redline debug center: a tiny local collector for Redline incident
//! reports and speed telemetry. Listens on 127.0.0.1 only, unless started with `--lan`: then
//! the owner's TV (Redline for TV with telemetry turned on) can deliver its reports over the
//! network too, and a UDP probe (`LUMEN-DC?` on the same port) is answered so the TV finds it;
//! peers other than this computer may only post reports, never read them. The extension's
//! background worker POSTs each incident (player fallback, InnerTube error, Rust
//! panic, boot failure) and each telemetry batch (`kind: "perf"`, `schema: 2`) to
//! `/incident`; reports are appended, one JSON object per line, to the incidents
//! file so they can be investigated. `GET /` shows the latest incidents, `GET /perf`
//! the speed dashboard, `GET /perf.json` raw telemetry rows (see `perf.rs`).
//! Standard library only.

mod json;
mod perf;
mod timeline;

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const ADDR: &str = "127.0.0.1:47321";
const MAX_BODY: usize = 512 * 1024;

/// The discovery probe of Redline for TV and the answer's start (`tv/core/src/tel.rs`).
const PROBE: &[u8] = b"LUMEN-DC?";

/// Where to listen and the incidents file: `[--lan] [file]`. 127.0.0.1 unless `--lan` (or
/// `YTC_DEBUG_CENTER_LAN=1`) asks for every interface. `YTC_DEBUG_CENTER_ADDR`: another 127.0.0.1 port
/// (tests); the extension only talks to ADDR.
fn config(args: &[String], env_addr: Option<String>, env_lan: Option<String>) -> (String, bool, Option<PathBuf>) {
    let lan = args.iter().any(|a| a == "--lan") || env_lan.as_deref() == Some("1");
    let file = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);
    let local = env_addr.filter(|a| a.starts_with("127.0.0.1:")).unwrap_or_else(|| ADDR.to_string());
    let addr = if lan { local.replacen("127.0.0.1", "0.0.0.0", 1) } else { local };
    (addr, lan, file)
}

/// What a peer may ask: this computer anything; another device on the network (`--lan`) only to deliver a
/// report (and the preflight / health check), never to read the reports.
fn allowed(peer_local: bool, method: &str, route: &str) -> bool {
    peer_local || matches!((method, route), ("POST", "/incident") | ("OPTIONS", _) | ("GET", "/health"))
}

/// Only devices on the owner's network are answered at all (private ranges, link-local, Tailscale).
fn on_network(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_loopback() || v.is_link_local() || (v.octets()[0] == 100 && (64..128).contains(&v.octets()[1])),
        IpAddr::V6(v) => v.is_loopback() || v.to_ipv4_mapped().is_some_and(|m| m.is_private() || m.is_loopback()),
    }
}

/// This computer's address on the network (what to type on the TV), without sending anything.
fn lan_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip()).filter(|ip| !ip.is_unspecified())
}

/// Answers the TV's discovery probe with `LUMEN-DC <port>` (its sender address is then the collector).
fn answer_probes(sock: UdpSocket, port: u16) {
    let mut buf = [0u8; 64];
    loop {
        let Ok((n, peer)) = sock.recv_from(&mut buf) else { continue };
        if &buf[..n] == PROBE && on_network(peer.ip()) {
            let _ = sock.send_to(format!("LUMEN-DC {port}").as_bytes(), peer);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (addr, lan, file) = config(&args, std::env::var("YTC_DEBUG_CENTER_ADDR").ok(), std::env::var("YTC_DEBUG_CENTER_LAN").ok());
    let file = file.unwrap_or_else(default_file);
    if let Some(dir) = file.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| panic!("cannot listen on {addr}: {e}"));
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(47321);
    if let Ok(u) = UdpSocket::bind(&addr) {
        std::thread::spawn(move || answer_probes(u, port));
    }
    eprintln!("Redline debug center on http://127.0.0.1:{port}/ → {}", file.display());
    if lan {
        let ip = lan_ip().map_or_else(|| "<this computer's address>".to_string(), |ip| ip.to_string());
        eprintln!("Also listening on the network (--lan): on the TV, About & updates → Developer → {ip}:{port} (or Find). Devices on the network can only deliver reports.");
    }
    // One thread per connection: a long dashboard read never holds up an incoming report.
    for stream in listener.incoming().flatten() {
        let file = file.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, &file) {
                eprintln!("request failed: {e}");
            }
        });
    }
}

/// Where incidents go without an argument: the user's data folder, outside any checkout.
/// macOS `~/Library/Application Support/Lumen/incidents.ndjson`, Windows
/// `%APPDATA%\Lumen\incidents.ndjson`, Linux and others `$XDG_DATA_HOME/lumen/incidents.ndjson`
/// (default `~/.local/share/lumen/incidents.ndjson`); `incidents.ndjson` here when none is known. (The folders keep
/// the name from before the rename to Redline, so data already collected stays where it is.)
fn default_file() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let dir = if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/Lumen"))
    } else if cfg!(windows) {
        env("APPDATA").map(|a| a.join("Lumen"))
    } else {
        env("XDG_DATA_HOME").or_else(|| env("HOME").map(|h| h.join(".local/share"))).map(|d| d.join("lumen"))
    };
    dir.map_or_else(|| PathBuf::from("incidents.ndjson"), |d| d.join("incidents.ndjson"))
}

fn handle(mut stream: TcpStream, file: &Path) -> std::io::Result<()> {
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let peer = stream.peer_addr().map(|a| a.ip()).ok();
    if !peer.is_some_and(on_network) {
        return Ok(());
    }
    let peer_local = peer.is_some_and(|ip| ip.is_loopback());
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let mut length = 0usize;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header == "\r\n" || header == "\n" {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
    }
    let route = path.split('?').next().unwrap_or("/");
    if !allowed(peer_local, method, route) {
        return respond(&mut stream, "403 Forbidden", "text/plain", b"reports are read on the debug center's own computer");
    }
    match (method, route) {
        ("OPTIONS", _) => respond(&mut stream, "204 No Content", "text/plain", b""),
        ("GET", "/perf") => {
            let page = perf::dashboard(file, &perf::query(path));
            respond(&mut stream, "200 OK", "text/html; charset=utf-8", page.as_bytes())
        }
        ("GET", "/timeline") => {
            let page = timeline::page(file, &perf::query(path));
            respond(&mut stream, "200 OK", "text/html; charset=utf-8", page.as_bytes())
        }
        ("GET", "/perf.json") => {
            let rows = perf::rows_json(file, &perf::query(path));
            respond(&mut stream, "200 OK", "application/json", rows.as_bytes())
        }
        ("GET", "/health") => respond(&mut stream, "200 OK", "text/plain", b"ok"),
        ("POST", "/incident") => {
            if length == 0 || length > MAX_BODY {
                return respond(&mut stream, "413 Payload Too Large", "text/plain", b"size");
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body)?;
            let text = String::from_utf8_lossy(&body);
            let one_line = text.replace(['\n', '\r'], " ");
            let received = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
            let mut out = OpenOptions::new().create(true).append(true).open(file)?;
            // One write per line (appends from parallel connections never interleave).
            out.write_all(format!("{{\"received\":{received},\"report\":{one_line}}}\n").as_bytes())?;
            eprintln!("{} stored ({length} bytes)", if one_line.contains("\"schema\":2") { "telemetry batch" } else { "incident" });
            respond(&mut stream, "200 OK", "text/plain", b"stored")
        }
        ("GET", "/") => {
            let page = overview(file);
            respond(&mut stream, "200 OK", "text/html; charset=utf-8", page.as_bytes())
        }
        _ => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: POST, GET, OPTIONS\r\nAccess-Control-Allow-Headers: content-type\r\nAccess-Control-Allow-Private-Network: true\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Latest 30 incidents (telemetry batches only counted), newest first, raw JSON per entry.
/// The file is streamed: only the last 30 incident lines are kept.
fn overview(file: &Path) -> String {
    let mut last: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let (mut incidents, mut batches) = (0usize, 0usize);
    if let Ok(f) = fs::File::open(file) {
        for line in BufReader::with_capacity(1 << 20, f).lines().map_while(Result::ok) {
            if line.contains("\"schema\":2") && line.contains("\"kind\":\"perf\"") {
                batches += 1;
                continue;
            }
            incidents += 1;
            last.push_back(line);
            if last.len() > 30 {
                last.pop_front();
            }
        }
    }
    let mut page = String::from("<!doctype html><meta charset=utf-8><title>Redline debug center</title><style>body{background:#111;color:#ddd;font:13px ui-monospace,monospace;margin:20px}h1{font:600 18px system-ui;color:#b7ebca}a{color:#9cc8ff}pre{white-space:pre-wrap;word-break:break-all;background:#1a1a1a;padding:10px;border-radius:8px}</style>");
    page.push_str(&format!("<h1>Redline debug center</h1><p>{incidents} incident(s) and {batches} telemetry batch(es) stored in {} · <a href=/perf>speed dashboard</a></p>", escape(&file.display().to_string())));
    for l in last.iter().rev() {
        page.push_str("<pre>");
        page.push_str(&escape(l));
        page.push_str("</pre>");
    }
    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_computer_only_unless_lan() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(config(&a(&[]), None, None), ("127.0.0.1:47321".to_string(), false, None));
        assert_eq!(config(&a(&["/tmp/x.ndjson"]), Some("127.0.0.1:5000".into()), None), ("127.0.0.1:5000".to_string(), false, Some("/tmp/x.ndjson".into())));
        assert_eq!(config(&a(&["0.0.0.0:1"]), Some("0.0.0.0:1".into()), Some("0".into())).0, "127.0.0.1:47321", "only a 127.0.0.1 port can be chosen");
        assert_eq!(config(&a(&["--lan", "/tmp/x"]), None, None), ("0.0.0.0:47321".to_string(), true, Some("/tmp/x".into())));
        assert_eq!(config(&a(&[]), None, Some("1".into())).0, "0.0.0.0:47321");
    }

    #[test]
    fn the_network_may_only_deliver() {
        assert!(allowed(true, "GET", "/perf") && allowed(true, "GET", "/"));
        assert!(allowed(false, "POST", "/incident") && allowed(false, "OPTIONS", "/incident") && allowed(false, "GET", "/health"));
        for r in ["/", "/perf", "/perf.json", "/timeline"] {
            assert!(!allowed(false, "GET", r), "{r}");
        }
        assert!(on_network("192.168.1.20".parse().unwrap()) && on_network("10.0.2.15".parse().unwrap()) && on_network("127.0.0.1".parse().unwrap()));
        assert!(!on_network("203.0.113.7".parse().unwrap()) && !on_network("8.8.8.8".parse().unwrap()));
    }

    #[test]
    fn answers_the_tvs_probe() {
        let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
        let at = srv.local_addr().unwrap();
        std::thread::spawn(move || answer_probes(srv, 47999));
        let c = UdpSocket::bind("127.0.0.1:0").unwrap();
        c.set_read_timeout(Some(std::time::Duration::from_secs(2))).unwrap();
        c.send_to(b"hello", at).unwrap();
        c.send_to(PROBE, at).unwrap();
        let mut b = [0u8; 32];
        let (n, from) = c.recv_from(&mut b).unwrap();
        assert_eq!((&b[..n], from), (&b"LUMEN-DC 47999"[..], at));
    }
}
