//! The one HTTP door for this crate: GET/POST JSON through `plx_net::net` (libcurl, bound at run
//! time, on the TV and on the host). Every addon and Nuvio-account request goes through here.

use crate::model::{Error, Result};
use plx_net::net;

/// Body ceiling for one addon answer. A Torrentio/AIOStreams stream list for a popular title is a
/// few hundred KB; 16 MB bounds a misbehaving addon without truncating a real one.
const MAX_BODY: usize = 16 * 1024 * 1024;

/// Deadlines for addon requests: connect fast, allow slow scrapers to finish.
pub const ADDON: net::Timeouts = net::Timeouts {
    connect_s: 8,
    total_s: 30,
    total_ms: 0,
    low_speed_bps: 0,
    low_speed_s: 0,
};

/// Deadlines for small control-plane calls (manifest, account).
pub const CONTROL: net::Timeouts = net::API;

/// `ADDON` with the whole-request ceiling cut to `ms` (a caller's remaining deadline).
pub fn within_ms(ms: u64) -> net::Timeouts {
    let ms = ms.clamp(500, 120_000) as std::os::raw::c_long;
    net::Timeouts { connect_s: 8.min((ms / 1000).max(1)), total_s: 0, total_ms: ms, low_speed_bps: 0, low_speed_s: 0 }
}

/// Bind libcurl once. Idempotent and cheap after the first call; production boot also calls it.
pub fn ready() -> bool {
    net::global_init() && net::available()
}

/// Whether several requests may run on parallel threads (libcurl's TLS backend is thread-safe on
/// this firmware). When false, callers serialise.
pub fn parallel_ok() -> bool {
    ready() && net::threaded_tls_ready()
}

pub fn get_json(url: &str, t: net::Timeouts) -> Result<serde_json::Value> {
    request_json(url, "GET", &[], None, t)
}

pub fn get_typed<T: serde::de::DeserializeOwned>(url: &str, t: net::Timeouts) -> Result<T> {
    let body = request_raw(url, "GET", &[], None, t)?;
    serde_json::from_slice(&body).map_err(|e| Error::Parse(e.to_string()))
}

pub fn post_json(url: &str, headers: &[String], body: &serde_json::Value, t: net::Timeouts) -> Result<serde_json::Value> {
    let bytes = serde_json::to_vec(body).map_err(|e| Error::Parse(e.to_string()))?;
    request_json(url, "POST", headers, Some(&bytes), t)
}

pub fn request_json(
    url: &str,
    verb: &str,
    headers: &[String],
    body: Option<&[u8]>,
    t: net::Timeouts,
) -> Result<serde_json::Value> {
    let bytes = request_raw(url, verb, headers, body, t)?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|e| Error::Parse(e.to_string()))
}

/// Raw request. Non-2xx is `Error::Status`; transport failure `Error::Network`.
pub fn request_raw(url: &str, verb: &str, headers: &[String], body: Option<&[u8]>, t: net::Timeouts) -> Result<Vec<u8>> {
    request_status(url, verb, headers, body, t).and_then(|(status, bytes)| {
        if (200..300).contains(&status) {
            Ok(bytes)
        } else {
            Err(Error::Status(status))
        }
    })
}

/// How many redirects one request may follow. Addon manifests and configured URLs do one hop (a
/// bare name upgrading to `https`, an addon's shortlink resolving); five is past every shape we
/// have measured and still bounds a loop. `net::https_get_public` (the QR fetch) uses the same
/// number, and the two are deliberately independent: that one lets libcurl follow, this one does not.
const MAX_REDIRECTS: usize = 5;

/// Header names whose value must never reach a host the caller did not name. `apikey` is Supabase's
/// anon key: it ships in the binary and is not a user secret, but a redirect is still no reason to
/// hand it to somebody else's server.
const CREDENTIAL_HEADERS: &[&str] = &["authorization", "cookie", "proxy-authorization", "x-plex-token", "apikey"];

/// The header's name, lowercased, without its value.
fn header_name(line: &str) -> String {
    line.split(':').next().unwrap_or("").trim().to_ascii_lowercase()
}

fn is_credential(line: &str) -> bool {
    let name = header_name(line);
    CREDENTIAL_HEADERS.iter().any(|credential| *credential == name)
}

/// Whether a response status is a redirect this transport follows.
fn is_redirect(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

/// Scheme, host and port of an absolute `http(s)` URL, normalized so two spellings of the same
/// origin compare equal (case-insensitive host, the default port filled in).
fn origin(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?; // drop any userinfo, which we never send
    let default = if scheme == "https" { 443 } else { 80 };
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, tail) = bracketed.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(port) => port.parse::<u16>().ok()?,
            None => default,
        };
        (format!("[{host}]"), port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_string(), port.parse::<u16>().ok()?),
            None => (authority.to_string(), default),
        }
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme, host.to_ascii_lowercase(), port))
}

/// An absolute URL for `location`, given the URL that named it. Relative targets resolve the way a
/// browser resolves them (`/path`, `path`, `//host/path`), and anything that is not `http(s)` — a
/// `file:` or a `data:` — is refused, because this transport only speaks to servers.
fn resolve_location(base: &str, location: &str) -> Option<String> {
    let location = location.trim();
    if location.is_empty() {
        return None;
    }
    let lower = location.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Some(location.to_string());
    }
    if location.starts_with("//") {
        let scheme = base.split_once("://").map(|(scheme, _)| scheme)?;
        return Some(format!("{scheme}:{location}"));
    }
    // Anything else that names a scheme (`file:`, `data:`, `ftp:`) is not a server we speak to.
    if location[..location.find(['/', '?', '#']).unwrap_or(location.len())].contains(':') {
        return None;
    }
    let (scheme, rest) = base.split_once("://")?;
    if location.starts_with('/') {
        let authority = rest.split(['/', '?', '#']).next()?;
        return Some(format!("{scheme}://{authority}{location}"));
    }
    // A relative path replaces the last segment of the base's path; a base with no path at all
    // resolves against `/`.
    let without_fragment = base.split('#').next().unwrap_or(base);
    let authority = rest.split(['/', '?', '#']).next()?;
    let path = &without_fragment[scheme.len() + 3 + authority.len()..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    let base_dir = match path.rfind('/') {
        Some(slash) => &path[..=slash],
        None => "/",
    };
    Some(format!("{scheme}://{authority}{base_dir}{location}"))
}

/// What one hop of a redirect chain decides. The decision is a value so that the policy — which
/// headers may travel, which method and body — is graded on its own, without a server.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    Follow { target: String, keep_credentials: bool, bodiless_get: bool },
    Stop(&'static str),
}

/// The next hop of a chain, or why the chain ends here. `Stop` is not an error: the caller receives
/// the 3xx answer itself (its status check reports it) rather than a fabricated success.
fn step(current: &str, status: u16, location: Option<&str>, method: &str) -> Step {
    if !is_redirect(status) {
        return Step::Stop("not a redirect");
    }
    let Some(location) = location else { return Step::Stop("redirect without a target") };
    let Some(target) = resolve_location(current, location) else {
        return Step::Stop("not an http(s) target");
    };
    let (here, there) = (origin(current), origin(&target));
    let (Some(here), Some(there)) = (here, there) else { return Step::Stop("unparsable origin") };
    if here.0 == "https" && there.0 == "http" {
        return Step::Stop("https may not downgrade to http");
    }
    let bodiless_get = status == 303
        || (matches!(status, 301 | 302) && method != "GET" && method != "HEAD");
    Step::Follow { target, keep_credentials: here == there, bodiless_get }
}

/// Raw request returning the status whatever it is (for callers that read error bodies).
///
/// **Redirects are followed here, not by libcurl.** The caller's header block travels to every hop
/// otherwise: `plx_net` refuses that combination outright (libcurl on this firmware replays custom
/// headers to whatever host a `Location:` names), and both an addon URL and the Nuvio backend are
/// reached with credentials — a bearer token for the account, `apikey`/`Authorization` for every
/// backend call. So each hop is one `follow_redirects = false` request, and this loop decides what
/// may travel to the next one: the credential headers are dropped when the origin changes, an
/// `https` request never downgrades to `http`, a 303 (and a 301/302 answering a POST) becomes a
/// bodiless GET, and `MAX_REDIRECTS` bounds the chain. A chain that ends in a redirect (a loop, or
/// a header we cannot follow) is returned as that 3xx answer, which the caller's status check
/// reports — never as a success.
pub fn request_status(
    url: &str,
    verb: &str,
    headers: &[String],
    body: Option<&[u8]>,
    t: net::Timeouts,
) -> Result<(u16, Vec<u8>)> {
    if !ready() {
        return Err(Error::Network);
    }
    let mut hdrs: Vec<String> = Vec::with_capacity(headers.len() + 2);
    if !headers.iter().any(|h| h.to_ascii_lowercase().starts_with("accept:")) {
        hdrs.push("Accept: application/json".to_string());
    }
    if body.is_some() && !headers.iter().any(|h| h.to_ascii_lowercase().starts_with("content-type:")) {
        hdrs.push("Content-Type: application/json".to_string());
    }
    hdrs.extend(headers.iter().cloned());
    let mut current = url.to_string();
    let mut method = verb.to_string();
    let mut payload = body.map(|bytes| bytes.to_vec());
    let mut hops = 0;
    loop {
        let resp = match net::request_result(&current, &hdrs, &method, payload.as_deref(), t, false,
                                             Some(MAX_BODY), None) {
            Ok(resp) => resp,
            Err(_) => return Err(Error::Network),
        };
        let (target, keep_credentials, bodiless_get) =
            match step(&current, resp.status, resp.location.as_deref(), &method) {
                Step::Follow { target, keep_credentials, bodiless_get } => (target, keep_credentials, bodiless_get),
                Step::Stop(reason) => {
                    if is_redirect(resp.status) {
                        plx_base::eventlog::log(&format!("stremio: redirect not followed — {reason}"));
                    }
                    return Ok((resp.status, resp.body));
                }
            };
        if hops == MAX_REDIRECTS {
            plx_base::eventlog::log("stremio: redirect limit reached — answering the last redirect");
            return Ok((resp.status, resp.body));
        }
        if !keep_credentials {
            hdrs.retain(|line| !is_credential(line));
        }
        if bodiless_get {
            method = "GET".to_string();
            payload = None;
            hdrs.retain(|line| header_name(line) != "content-type");
        }
        current = target;
        hops += 1;
    }
}

/// `encodeURIComponent` semantics, with `+` never produced (space is `%20`). Matches what Nuvio
/// sends for ids and extra values.
pub fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let unreserved = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if unreserved {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_encode_uri_component() {
        assert_eq!(encode_component("tt0111161:1:2"), "tt0111161%3A1%3A2");
        assert_eq!(encode_component("Sci-Fi & Fantasy"), "Sci-Fi%20%26%20Fantasy");
        assert_eq!(encode_component("a/b?c=d"), "a%2Fb%3Fc%3Dd");
        assert_eq!(encode_component("héllo"), "h%C3%A9llo");
        assert_eq!(encode_component("(x)!~*'"), "(x)!~*'");
    }

    #[test]
    fn a_location_resolves_the_way_a_browser_resolves_it() {
        let base = "https://addon.example/stremio/abc/manifest.json?x=1";
        assert_eq!(resolve_location(base, "https://other.example/m.json").as_deref(),
                   Some("https://other.example/m.json"));
        assert_eq!(resolve_location(base, "  /next.json  ").as_deref(),
                   Some("https://addon.example/next.json"));
        assert_eq!(resolve_location(base, "//other.example/m.json").as_deref(),
                   Some("https://other.example/m.json"));
        assert_eq!(resolve_location(base, "sibling.json").as_deref(),
                   Some("https://addon.example/stremio/abc/sibling.json"));
        assert_eq!(resolve_location("https://addon.example", "root.json").as_deref(),
                   Some("https://addon.example/root.json"));
        // Only servers: a redirect to a local file, a data URL or no target at all is not followed.
        for refused in ["file:///etc/passwd", "data:text/plain,hi", "ftp://host/x", ""] {
            assert_eq!(resolve_location(base, refused), None, "{refused}");
        }
    }

    #[test]
    fn an_origin_is_scheme_host_and_port() {
        assert_eq!(origin("https://Addon.Example/x"), Some(("https".into(), "addon.example".into(), 443)));
        assert_eq!(origin("http://addon.example:80/x"), Some(("http".into(), "addon.example".into(), 80)));
        assert_eq!(origin("https://addon.example:8443"), Some(("https".into(), "addon.example".into(), 8443)));
        assert_eq!(origin("https://user:pw@addon.example/x"), Some(("https".into(), "addon.example".into(), 443)));
        assert_eq!(origin("https://[::1]:9000/x"), Some(("https".into(), "[::1]".into(), 9000)));
        assert_eq!(origin("https://[::1]/x"), Some(("https".into(), "[::1]".into(), 443)));
        for refused in ["file:///x", "addon.example/x", "https://", "https://host:notaport/"] {
            assert_eq!(origin(refused), None, "{refused}");
        }
    }

    #[test]
    fn a_credential_header_is_named_not_guessed() {
        for credential in ["Authorization: Bearer t", "authorization: t", "Cookie: a=b",
                           "apikey: anon", "X-Plex-Token: t", "Proxy-Authorization: Basic x"] {
            assert!(is_credential(credential), "{credential}");
        }
        // What an addon answer needs, and what a redirect may carry to another host.
        for harmless in ["Accept: application/json", "Content-Type: application/json", "If-None-Match: \"x\""] {
            assert!(!is_credential(harmless), "{harmless}");
        }
    }

    #[test]
    fn only_a_redirect_status_is_followed() {
        for status in [301u16, 302, 303, 307, 308] {
            assert!(is_redirect(status), "{status}");
        }
        for status in [200u16, 204, 304, 400, 503] {
            assert!(!is_redirect(status), "{status}");
        }
    }

    /// One request to a real loopback server, so the redirect policy is graded on the wire rather
    /// than on its helpers alone. `reply` answers the first request with `first`; the server then
    /// answers every later request with `second` and reports the header block it saw there.
    fn exchange(first: String, second: &'static str) -> (u16, Vec<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (index, reply) in [first.clone(), second.to_string()].into_iter().enumerate() {
                let (mut socket, _) = listener.accept().expect("a connection");
                let mut raw = Vec::new();
                let mut chunk = [0u8; 1024];
                while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).expect("a request");
                    if n == 0 { break; }
                    raw.extend_from_slice(&chunk[..n]);
                }
                if index == 1 {
                    let text = String::from_utf8_lossy(&raw).to_string();
                    seen = text.lines().map(|line| line.trim_end().to_string()).collect();
                }
                let _ = socket.write_all(reply.replace("{{PORT}}", &port.to_string()).as_bytes());
                let _ = socket.flush();
            }
            seen
        });
        let url = format!("http://127.0.0.1:{port}/manifest.json");
        let headers = vec!["apikey: anon".to_string(), "Authorization: Bearer token".to_string()];
        let (status, _) = request_status(&url, "POST", &headers, Some(b"{}"), CONTROL).expect("an answer");
        (status, server.join().expect("the server"))
    }

    fn ok_reply() -> &'static str {
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}"
    }

    #[test]
    fn a_redirect_on_the_same_origin_keeps_the_callers_headers() {
        // Same port, so same origin: the hop is the same server and the credentials stay.
        let (status, seen) = exchange(format!(
            "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{{{{PORT}}}}/retry.json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            ok_reply());
        assert_eq!(status, 200);
        assert!(seen.iter().any(|line| line.eq_ignore_ascii_case("authorization: Bearer token")), "{seen:?}");
    }

    #[test]
    fn a_redirect_to_another_origin_drops_the_credentials_and_the_body() {
        // A 303 to another port is another origin AND a method change, so the credentials and the
        // POST body are gone from the hop while `Accept` (harmless) travels.
        let (status, seen) = exchange(format!(
            "HTTP/1.1 303 See Other\r\nLocation: http://localhost:{{{{PORT}}}}/next.json\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
            ok_reply());
        assert_eq!(status, 200);
        assert!(seen[0].starts_with("GET /next.json"), "{seen:?}");
        for gone in ["authorization:", "apikey:", "content-type:"] {
            assert!(!seen.iter().any(|line| line.to_ascii_lowercase().starts_with(gone)), "{gone} in {seen:?}");
        }
        assert!(seen.iter().any(|line| line.to_ascii_lowercase().starts_with("accept:")), "{seen:?}");
    }

    #[test]
    fn the_next_hop_decides_what_may_travel() {
        let base = "https://addon.example/manifest.json";
        // Same origin: credentials stay, the method does not change on a 307.
        assert_eq!(step(base, 307, Some("/retry"), "POST"),
                   Step::Follow { target: "https://addon.example/retry".into(),
                                  keep_credentials: true, bodiless_get: false });
        // Another host: the credentials go, and a 302 answering a POST becomes a bodiless GET.
        assert_eq!(step(base, 302, Some("https://other.example/x"), "POST"),
                   Step::Follow { target: "https://other.example/x".into(),
                                  keep_credentials: false, bodiless_get: true });
        // A 303 is always a GET, even on the same origin, and even for a GET it carries no body.
        assert_eq!(step(base, 303, Some("/next"), "GET"),
                   Step::Follow { target: "https://addon.example/next".into(),
                                  keep_credentials: true, bodiless_get: true });
        // The refusals: a downgrade, a non-server target, a redirect with no target, and a status
        // that is not a redirect at all.
        assert_eq!(step(base, 301, Some("http://addon.example/x"), "GET"),
                   Step::Stop("https may not downgrade to http"));
        assert_eq!(step(base, 301, Some("file:///etc/passwd"), "GET"), Step::Stop("not an http(s) target"));
        assert_eq!(step(base, 302, None, "GET"), Step::Stop("redirect without a target"));
        assert_eq!(step(base, 200, Some("/x"), "GET"), Step::Stop("not a redirect"));
    }
}
