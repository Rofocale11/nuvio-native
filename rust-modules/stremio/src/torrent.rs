//! Torrent (`infoHash`) streams, played the way Nuvio's webOS app plays them: through a
//! Stremio-compatible streaming server ("EngineFS", the `server.js` Stremio ships).
//!
//! On the TV, Nuvio's companion service `space.nuvio.webos.service` (Luna `ping` starts it) runs
//! that server as `media-http.cjs` on the first free port of `127.0.0.1:2710..=2714`; Nuvio then
//! talks plain HTTP to it (`js/core/p2p/webOsEngineFsResolver*.js`):
//!
//! * `POST {server}/{infoHash}/create` with `{"torrent":{"infoHash"},"peerSearch":{"sources",
//!   "min":40,"max":200},"guessFileIdx":{season,episode}|false}` — skipped when the stream names its
//!   `fileIdx` and carries no tracker sources;
//! * then plays `GET {server}/{infoHash}/{fileIdx}[?tr=<source>…]`.
//!
//! A desktop Stremio's server (`http://<lan-ip>:11470`) speaks the same API. P2P is OFF by
//! default in Nuvio (`torrentSettings.p2pEnabled`); debrid-resolved links from Torrentio /
//! AIOStreams are plain HTTPS `url`s and need none of this.

use serde_json::{json, Value};

use crate::addon_http as http;
use crate::model::{Error, Result, Stream};

/// The ports Nuvio's companion service tries, in order (`services/webos/src/constants.js`).
pub const NUVIO_ENGINEFS_PORTS: [u16; 5] = [2710, 2711, 2712, 2713, 2714];

fn normalize_hash(s: &str) -> Option<String> {
    let h = s.trim().to_ascii_lowercase();
    (h.len() == 40 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

fn hash_from_magnet(m: &str) -> Option<String> {
    let lower = m.to_ascii_lowercase();
    let pos = lower.find("xt=urn:btih:")?;
    normalize_hash(lower[pos + 12..].get(..40)?)
}

fn is_magnet(s: &str) -> bool {
    s.trim().to_ascii_lowercase().starts_with("magnet:")
}

/// The stream's info hash: `infoHash`, else one in a magnet `url`/`externalUrl`.
pub fn info_hash(stream: &Stream) -> Option<String> {
    stream
        .info_hash
        .as_deref()
        .and_then(normalize_hash)
        .or_else(|| stream.url.as_deref().filter(|u| is_magnet(u)).and_then(hash_from_magnet))
        .or_else(|| stream.external_url.as_deref().filter(|u| is_magnet(u)).and_then(hash_from_magnet))
}

/// A direct HTTP(S) URL the player can open as is: `url`, else (Nuvio's `getStreamUrl`) a
/// non-magnet `externalUrl` when the stream is not a torrent.
pub fn direct_url(stream: &Stream) -> Option<&str> {
    let is_http = |u: &&str| {
        let l = u.trim().to_ascii_lowercase();
        l.starts_with("http://") || l.starts_with("https://")
    };
    stream.url.as_deref().filter(is_http).or_else(|| {
        stream.external_url.as_deref().filter(is_http).filter(|_| stream.info_hash.is_none() && stream.url.is_none())
    })
}

/// Tracker/DHT peer sources in the `tracker:…`/`dht:…` form the server's `peerSearch` takes,
/// `dht:<hash>` first (Nuvio's `buildNuvioPeerSearchSources`). Empty when the stream names none.
pub fn peer_sources(stream: &Stream, hash: &str) -> Vec<String> {
    let mut trackers: Vec<String> = Vec::new();
    if let Some(m) = stream.url.as_deref().filter(|u| is_magnet(u)) {
        for part in m.split(['?', '&']) {
            if let Some(tr) = part.strip_prefix("tr=") {
                trackers.push(percent_decode(tr));
            }
        }
    }
    trackers.extend(stream.sources.iter().cloned());
    if trackers.iter().all(|s| s.trim().is_empty()) {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    let mut add = |raw: &str| {
        let raw = raw.trim();
        if raw.is_empty() {
            return;
        }
        let l = raw.to_ascii_lowercase();
        let s = if l.starts_with("tracker:") || l.starts_with("dht:") { raw.to_string() } else { format!("tracker:{raw}") };
        if !out.iter().any(|x| x.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    };
    add(&format!("dht:{hash}"));
    for t in &trackers {
        add(t);
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn server_root(server: &str) -> String {
    server.trim().trim_end_matches('/').to_string()
}

fn playback_url(server: &str, hash: &str, file_idx: u32, sources: &[String]) -> String {
    let mut url = format!("{}/{}/{}", server_root(server), hash, file_idx);
    if !sources.is_empty() {
        let q: Vec<String> = sources.iter().map(|s| format!("tr={}", http::encode_component(s))).collect();
        url.push('?');
        url.push_str(&q.join("&"));
    }
    url
}

/// What to hand the player for this stream, without network: its direct URL, or — for a torrent
/// whose `fileIdx` is known — the streaming-server URL. `None` for a torrent with no server, or
/// with no `fileIdx` (use [`resolve`], which asks the server to pick the file), and for YouTube /
/// external-app streams.
pub fn playable_url(stream: &Stream, streaming_server: Option<&str>) -> Option<String> {
    if let Some(u) = direct_url(stream) {
        return Some(u.to_string());
    }
    let hash = info_hash(stream)?;
    let server = streaming_server.filter(|s| !s.trim().is_empty())?;
    let idx = stream.file_idx?;
    Some(playback_url(server, &hash, idx, &peer_sources(stream, &hash)))
}

/// Resolve a torrent stream on a streaming server: `/create` when needed (no explicit `fileIdx`,
/// or tracker sources to hand over), then the playback URL. `season`/`episode` let the server
/// guess the episode file in a season pack.
pub fn resolve(server: &str, stream: &Stream, season: Option<u32>, episode: Option<u32>) -> Result<String> {
    if let Some(u) = direct_url(stream) {
        return Ok(u.to_string());
    }
    let hash = info_hash(stream).ok_or(Error::NoAddon)?;
    let sources = peer_sources(stream, &hash);
    if let (Some(idx), true) = (stream.file_idx, sources.is_empty()) {
        return Ok(playback_url(server, &hash, idx, &sources));
    }
    let mut guess = serde_json::Map::new();
    if let Some(s) = season {
        guess.insert("season".into(), json!(s));
    }
    if let Some(e) = episode {
        guess.insert("episode".into(), json!(e));
    }
    let mut body = json!({"torrent": {"infoHash": hash}});
    if !sources.is_empty() {
        body["peerSearch"] = json!({"sources": sources, "min": 40, "max": 200});
    }
    body["guessFileIdx"] = if stream.file_idx.is_some() { Value::Bool(false) } else { Value::Object(guess) };
    let url = format!("{}/{}/create", server_root(server), http::encode_component(&hash));
    let created = http::post_json(&url, &[], &body, http::within_ms(30_000))?;
    let idx = stream
        .file_idx
        .or_else(|| {
            ["fileIdx", "fileIndex", "idx", "index", "guessedFileIdx"]
                .iter()
                .find_map(|k| created.get(*k).and_then(Value::as_i64).filter(|n| *n >= 0).map(|n| n as u32))
        })
        .ok_or_else(|| Error::Parse("streaming server could not pick a file".into()))?;
    Ok(playback_url(server, &hash, idx, &sources))
}

/// Find a running Stremio-compatible streaming server on this device (Nuvio's companion, once its
/// Luna service has started it): the first `127.0.0.1:{2710..2714}` answering `/settings`.
pub fn probe_local_server() -> Option<String> {
    for port in NUVIO_ENGINEFS_PORTS {
        let base = format!("http://127.0.0.1:{port}");
        let t = plx_net::net::Timeouts { connect_s: 1, total_s: 2, total_ms: 0, low_speed_bps: 0, low_speed_s: 0 };
        if http::request_status(&format!("{base}/settings"), "GET", &[], None, t).is_ok_and(|(s, _)| (200..300).contains(&s)) {
            return Some(base);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn direct_links_pass_through() {
        let s = Stream { url: Some("https://x/y.mkv".into()), ..Default::default() };
        assert_eq!(playable_url(&s, None).as_deref(), Some("https://x/y.mkv"));
    }

    #[test]
    fn torrents_need_a_server_and_a_file() {
        let mut s = Stream { info_hash: Some(H.to_uppercase()), file_idx: Some(3), ..Default::default() };
        assert_eq!(playable_url(&s, None), None);
        assert_eq!(playable_url(&s, Some("http://127.0.0.1:2710/")).as_deref(), Some(&*format!("http://127.0.0.1:2710/{H}/3")));
        s.sources = vec!["tracker:udp://t.example:80/announce".into()];
        assert_eq!(
            playable_url(&s, Some("http://s")).as_deref(),
            Some(&*format!("http://s/{H}/3?tr=dht%3A{H}&tr=tracker%3Audp%3A%2F%2Ft.example%3A80%2Fannounce"))
        );
        s.file_idx = None;
        assert_eq!(playable_url(&s, Some("http://s")), None);
    }

    #[test]
    fn magnets() {
        let s = Stream { url: Some(format!("magnet:?xt=urn:btih:{}&tr=udp%3A%2F%2Fa%3A1", H.to_uppercase())), ..Default::default() };
        assert_eq!(info_hash(&s).as_deref(), Some(H));
        assert!(direct_url(&s).is_none());
        assert_eq!(peer_sources(&s, H), vec![format!("dht:{H}"), "tracker:udp://a:1".to_string()]);
    }
}
