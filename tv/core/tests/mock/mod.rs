//! A tiny HTTP/1.1 server for the tests: each request is answered by `route(method, path, body)`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

pub type Route = Arc<dyn Fn(&str, &str, &str) -> (u16, String, Vec<u8>) + Send + Sync>;

#[allow(dead_code)]
pub struct Mock {
    pub base: String,
    pub log: Arc<Mutex<Vec<(String, String, String)>>>,
    /// (path, the request's header lines in lower case)
    pub heads: Arc<Mutex<Vec<(String, String)>>>,
}

pub fn start(route: Route) -> Mock {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", l.local_addr().unwrap());
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let heads2 = heads.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let route = route.clone();
            let log = log2.clone();
            let heads = heads2.clone();
            std::thread::spawn(move || {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                if r.read_line(&mut line).is_err() {
                    return;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut len = 0usize;
                let mut lines = String::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).is_err() || h == "\r\n" || h.is_empty() {
                        break;
                    }
                    lines.push_str(&h.to_ascii_lowercase());
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                heads.lock().unwrap().push((path.clone(), lines));
                let mut body = vec![0u8; len];
                let _ = r.read_exact(&mut body);
                let body = String::from_utf8_lossy(&body).to_string();
                log.lock().unwrap().push((method.clone(), path.clone(), body.clone()));
                let (status, ctype, out) = route(&method, &path, &body);
                // A redirect (3xx): the route's second value is the Location.
                let head = if (300..400).contains(&status) {
                    format!("HTTP/1.1 {status} X\r\nLocation: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len())
                } else {
                    format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        out.len()
                    )
                };
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(&out);
            });
        }
    });
    Mock { base, log, heads }
}
