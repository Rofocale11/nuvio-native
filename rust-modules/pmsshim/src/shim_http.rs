//! A deliberately small HTTP/1.1 server on `std::net`: one thread per connection, keep-alive,
//! `Content-Length` bodies only (the client never sends chunked requests). Loopback only — the
//! listener is bound to 127.0.0.1 by [`crate::start`] and the bin's default.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Connections served at once. The app opens a handful (control plane, poster workers, the media
/// plane's part probe); anything past this is refused rather than queued without bound.
/// A connection runs addon/account requests (libcurl, JSON, QR encoding) inline: 256 KiB overflowed
/// on the Nuvio sign-in path and aborted the process, so 1 MiB (virtual; only touched pages are
/// resident).
const MAX_CONNECTIONS: usize = 48;
const IDLE: Duration = Duration::from_secs(30);
const MAX_HEAD: usize = 64 * 1024;
const MAX_BODY: usize = 1024 * 1024;

pub struct Request {
    pub method: String,
    /// Path without the query, percent-decoded.
    pub path: String,
    /// Query parameters, percent-decoded; the LAST of a repeated key wins (as PMS reads them).
    pub query: HashMap<String, String>,
    /// Header names lower-cased.
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn q(&self, k: &str) -> Option<&str> {
        self.query.get(k).map(String::as_str)
    }
    /// A query parameter, else the same-named header (PMS accepts paging either way).
    pub fn q_or_header(&self, k: &str) -> Option<&str> {
        self.q(k).or_else(|| self.headers.get(&k.to_ascii_lowercase()).map(String::as_str))
    }
}

pub struct Response {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn json(v: &serde_json::Value) -> Response {
        Response::json_status(200, v)
    }
    pub fn json_status(status: u16, v: &serde_json::Value) -> Response {
        Response { status, content_type: "application/json", headers: Vec::new(), body: serde_json::to_vec(v).unwrap_or_default() }
    }
    pub fn bytes(status: u16, content_type: &'static str, body: Vec<u8>) -> Response {
        Response { status, content_type, headers: Vec::new(), body }
    }
    pub fn redirect(location: &str) -> Response {
        Response { status: 302, content_type: "text/plain", headers: vec![("Location".into(), location.into())], body: Vec::new() }
    }
    pub fn not_found() -> Response {
        Response::bytes(404, "text/plain", b"not found".to_vec())
    }
}

pub type Handler = dyn Fn(&Request) -> Response + Send + Sync;

/// Serve forever on `listener` (call from a dedicated thread).
pub fn serve(listener: TcpListener, handler: Arc<Handler>) {
    let live = Arc::new(AtomicUsize::new(0));
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        if live.load(Ordering::Relaxed) >= MAX_CONNECTIONS {
            drop(stream);
            continue;
        }
        live.fetch_add(1, Ordering::Relaxed);
        let (h, live2) = (handler.clone(), live.clone());
        let spawned = std::thread::Builder::new().name("pmsshim-conn".into()).stack_size(1024 * 1024).spawn(move || {
            connection(stream, &*h);
            live2.fetch_sub(1, Ordering::Relaxed);
        });
        if spawned.is_err() {
            live.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn connection(stream: TcpStream, handler: &Handler) {
    let _ = stream.set_read_timeout(Some(IDLE));
    let _ = stream.set_nodelay(true);
    let Ok(mut out) = stream.try_clone() else { return };
    let mut rd = BufReader::new(stream);
    loop {
        let Some((req, keep_alive)) = read_request(&mut rd) else { return };
        let head_only = req.method == "HEAD";
        // A bug in one route answers 500 rather than taking the connection (and, under an abort
        // strategy, the app) with it.
        let resp = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(&req)))
            .unwrap_or_else(|_| Response::bytes(500, "text/plain", b"internal error".to_vec()));
        if write_response(&mut out, &resp, head_only, keep_alive).is_err() || !keep_alive {
            return;
        }
    }
}

fn read_request(rd: &mut BufReader<TcpStream>) -> Option<(Request, bool)> {
    let mut line = String::new();
    let mut total = 0usize;
    // Skip stray CRLFs between pipelined requests.
    loop {
        line.clear();
        let n = rd.read_line(&mut line).ok()?;
        if n == 0 {
            return None;
        }
        total += n;
        if !line.trim().is_empty() {
            break;
        }
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        let n = rd.read_line(&mut h).ok()?;
        total += n;
        if n == 0 || total > MAX_HEAD {
            return None;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let len: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; len];
    rd.read_exact(&mut body).ok()?;
    let conn = headers.get("connection").map(|v| v.to_ascii_lowercase());
    let keep_alive = match conn.as_deref() {
        Some("close") => false,
        Some("keep-alive") => true,
        _ => version == "HTTP/1.1",
    };
    let (raw_path, raw_query) = target.split_once('?').unwrap_or((&target, ""));
    Some((
        Request { method, path: pct_decode(raw_path), query: parse_query(raw_query), headers, body },
        keep_alive,
    ))
}

fn write_response(out: &mut TcpStream, r: &Response, head_only: bool, keep_alive: bool) -> std::io::Result<()> {
    let reason = match r.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        206 => "Partial Content",
        302 => "Found",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: {}\r\n",
        r.status,
        reason,
        r.content_type,
        r.body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    for (k, v) in &r.headers {
        head.push_str(k);
        head.push_str(": ");
        head.push_str(v);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    out.write_all(head.as_bytes())?;
    if !head_only {
        out.write_all(&r.body)?;
    }
    out.flush()
}

pub fn parse_query(q: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for kv in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        m.insert(pct_decode(&k.replace('+', " ")), pct_decode(&v.replace('+', " ")));
    }
    m
}

pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn pct_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 3);
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) {
            o.push(c as char);
        } else {
            o.push_str(&format!("%{c:02X}"));
        }
    }
    o
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decode_roundtrip() {
        assert_eq!(pct_decode("a%2Fb%3Fc"), "a/b?c");
        assert_eq!(pct_decode("%zz"), "%zz");
        assert_eq!(pct_decode("x%4"), "x%4");
        assert_eq!(pct_decode(&pct_encode("https://x.y/p?q=1&r")), "https://x.y/p?q=1&r");
        let q = parse_query("url=%2Flibrary%2Fmetadata%2F42&width=250&query=star+wars");
        assert_eq!(q["url"], "/library/metadata/42");
        assert_eq!(q["query"], "star wars");
    }
}
