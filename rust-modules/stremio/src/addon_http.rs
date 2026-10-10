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

/// Raw request returning the status whatever it is (for callers that read error bodies).
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
    match net::request_result(url, &hdrs, verb, body, t, true, Some(MAX_BODY), None) {
        Ok(resp) => Ok((resp.status, resp.body)),
        Err(_) => Err(Error::Network),
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
}
