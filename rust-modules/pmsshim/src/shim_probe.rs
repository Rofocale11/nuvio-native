//! What a stream really holds, from the app's bundled FFmpeg (`plx_media::ff::probe::probe_url`),
//! injected at boot as a [`Prober`] so this crate does not depend on the media layer. Field names
//! mirror `plx_media::ff_probe::MediaProbe` one for one; the app's adapter is a copy plus the two
//! `*_decodable` verdicts it takes from the TV's codec table (`devcaps`).
//!
//! `docs/nuvio/PLAYBACK.md` is the contract: probe at decision time, cache per content (a torrent's
//! info hash + file index — the C engine mints a random `/stream/<token>` per resolve, so a URL key
//! never survives a replay — and per host+path for everything else, with signed query params such as
//! debrid expiry stripped), emit the real fields, and refuse (never "transcode") a stream the TV
//! cannot decode.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Dovi {
    pub profile: u8,
    pub level: u8,
    pub bl_compat: u8,
    pub rpu: bool,
    pub el: bool,
    pub bl: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Video {
    pub codec: String,
    pub width: i32,
    pub height: i32,
    pub fps: f64,
    pub bit_depth: i32,
    pub profile: i32,
    pub color_trc: i32,
    pub color_primaries: i32,
    pub color_space: i32,
    pub dovi: Option<Dovi>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Audio {
    pub codec: String,
    pub profile: i32,
    pub channels: i32,
    /// The file's own sample rate in Hz (`ProbedAudio::sample_rate`), 0 when the probe could not
    /// say. Published as PMS `samplingRate`, which is what the player's fork lanes read: the AC3
    /// encode lane declares at 48 kHz only and the PCM lane states the exact rate in its
    /// `pcmInfo`, so without this field every track off this shim arrived with "rate not told".
    pub sample_rate: i32,
    pub language: String,
    pub title: String,
    pub default: bool,
    /// The TV decodes this track directly (the app's verdict).
    pub decodable: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Sub {
    pub codec: String,
    pub language: String,
    pub title: String,
    pub default: bool,
    pub forced: bool,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct ProbeInfo {
    /// `mkv` / `mp4` / `mpegts` (`MediaProbe::container()`).
    pub container: String,
    pub duration_ms: i64,
    pub size: i64,
    pub video: Option<Video>,
    /// The TV decodes the video track directly (the app's verdict).
    pub video_decodable: bool,
    pub audio: Vec<Audio>,
    pub subtitles: Vec<Sub>,
}

/// `(url, deadline) -> probe`. `Err` = not playable here (unreachable or unsupported container).
pub type Prober = Arc<dyn Fn(&str, Duration) -> Result<ProbeInfo, String> + Send + Sync>;

/// `(url, at, deadline) -> total size`: open `url` at byte `at` and report the resource's total
/// size, `Err` when the server refuses that offset. The DASH gate asks for each track's LAST
/// 64 KiB: googlevideo grants most client identities only the first 60 s of an adaptive track, so
/// it can answer its first window, pass the probe, and then fail every later range — a `Media`
/// that starts and never finishes. Absent on the host bin and in tests, where the gate then trusts
/// the probe alone.
pub type Reacher = Arc<dyn Fn(&str, i64, Duration) -> Result<i64, String> + Send + Sync>;

pub const PROBE_DEADLINE: Duration = Duration::from_secs(45);
/// How long a **URL-keyed** answer is trusted. A signed debrid link can be re-pointed and a 403 is
/// transient, so those verdicts expire. A content key cannot go stale at all (see
/// [`content_key`]) and is kept for the life of the process, then across a relaunch on disk.
const PROBE_TTL: Duration = Duration::from_secs(30 * 60);
/// The real ceiling on the whole cache, oldest dropped first. `PROBE_TTL` alone never bounded it: a
/// session playing many titles keeps every entry fresh. ~4 KB per entry at worst (a 4K remux with
/// 45 subtitle tracks), so this is about 2 MB of RAM, and the same on disk.
const MAX_ENTRIES: usize = 512;
/// The durable side of the cache, beside the swarm state in
/// `p2p_state_candidates()` — `probe-cache.json` (see `ProbeCache::attach`).
pub const PROBE_CACHE_FILE: &str = "probe-cache.json";

/// Cache key for a URL: scheme + host + path, without query or fragment.
///
/// Debrid re-resolves a file to a fresh signed URL on each play
/// (`?Signature=...&Expires=...&token=...`), so the same host+path is the
/// same file and must share one probe entry \u2014 otherwise a second play pays a
/// cold probe. The query is never part of path/file identity for these signed
/// URLs, so it is dropped here. Path bytes (including any credential embedded
/// in the path) are kept byte-identical, as is any userinfo; only scheme/host
/// casing is normalized. The stored `ProbeInfo` and the URL handed to the
/// prober stay byte-identical to the caller's \u2014 only this key is normalized.
/// **Exception — `*.googlevideo.com`:** /videoplayback is shared by many trailers;
/// file identity is `id` + `itag` in the query. The key keeps those so one
/// trailer's 403 cannot poison the edge for 30 min. See the inline note.
fn cache_key(url: &str) -> String {
    let end = url.find(['?', '#']).unwrap_or(url.len());
    let base = &url[..end];
    let Some(sep) = base.find("://") else {
        return base.to_string();
    };
    let scheme = base[..sep].to_ascii_lowercase();
    let rest = &base[sep + 3..];
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // Preserve userinfo (credentials) byte-identical; lowercase only the host.
    let authority = match authority.rfind('@') {
        Some(at) => format!("{}@{}", &authority[..at], authority[at + 1..].to_ascii_lowercase()),
        None => authority.to_ascii_lowercase(),
    };
    let mut key = format!("{scheme}://{authority}{path}");
    // googlevideo: file identity is id+itag, not host+path (same edge serves
    // different trailers). Stripping the query would let one trailer's 403
    // poison all later trailers on that edge for 30 min. Keep id+itag so
    // each deciphered format probes separately; re-signing the same id+itag
    // (new sig) still shares an entry, as with debrid.
    if authority.ends_with(".googlevideo.com") || authority == "googlevideo.com" {
        let qstart = url.find('?').unwrap_or(url.len());
        let q = if qstart < url.len() { &url[qstart + 1..] } else { "" };
        let q = q.split('#').next().unwrap_or(q);
        let mut id = "";
        let mut itag = "";
        for pair in q.split('&') {
            if let Some(v) = pair.strip_prefix("id=") {
                id = v;
            } else if let Some(v) = pair.strip_prefix("itag=") {
                itag = v;
            }
            if !id.is_empty() && !itag.is_empty() {
                break;
            }
        }
        if !id.is_empty() || !itag.is_empty() {
            key.push_str("?id=");
            key.push_str(id);
            key.push_str("&itag=");
            key.push_str(itag);
        }
    }
    key
}

/// The probe-cache key for a stream ranked from `marker`, or `None` when the URL's own key is the
/// right one.
///
/// A `torrent:<hash>` marker names the **content**, so it is the key — with the file, because a
/// season pack holds one probe per episode. The C engine mints a random `/stream/<32 hex>` on
/// every resolve (`libtorrent_stream_bridge.cpp` `register_stream`), so a URL-keyed entry can
/// never be reused: every replay re-probed, and since `remove()` on a source switch leaves no
/// payload, each probe re-read the container header **and the tail cues** from the swarm. On a
/// 4K remux that was 5.7 s and then 10.7 s for the same part, spent before DIRECT PLAY.
///
/// `file_idx` is the stream's own hint, which is also what the engine resolves (`None` = it picks
/// the largest file, deterministically, so `largest` names that choice).
pub fn content_key(marker: &str, file_idx: Option<u32>) -> Option<String> {
    marker.strip_prefix(crate::shim_media::TORRENT_SCHEME)?;
    Some(match file_idx {
        Some(i) => format!("{marker}/{i}"),
        None => format!("{marker}/largest"),
    })
}

/// Whether `key` names content rather than a URL — the one property that decides both the expiry
/// rule and whether the entry is worth writing to disk.
fn content_key_of(key: &str) -> Option<&str> {
    key.strip_prefix(crate::shim_media::TORRENT_SCHEME)
}

#[derive(Default)]
pub struct ProbeCache {
    prober: Mutex<Option<Prober>>,
    done: Mutex<HashMap<String, (Instant, Result<ProbeInfo, String>)>>,
    /// One probe per URL at a time: a second asker waits on the first.
    inflight: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Where content-keyed entries persist. `None` (tests, the host bin, an unwritable device)
    /// keeps the cache in RAM only — persistence is an optimisation, never a requirement.
    disk: Mutex<Option<PathBuf>>,
}

impl ProbeCache {
    pub fn set_prober(&self, p: Prober) {
        *self.prober.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
    }

    pub fn has_prober(&self) -> bool {
        self.prober.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// Make the content-keyed half of the cache durable at `path`, loading what a previous run
    /// wrote. Called once at boot with `p2p_state_candidates()[0]/probe-cache.json`, beside the
    /// swarm state. A torrent's probe is what costs 10 s on a 4K remux (the container header and the
    /// tail cues come back from the swarm), so paying it once per file rather than once per relaunch
    /// is the point. A missing, unreadable or stale-format file just means an empty cache.
    pub fn attach(&self, path: PathBuf) {
        let loaded = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<HashMap<String, ProbeInfo>>(&b).ok())
            .unwrap_or_default();
        if !loaded.is_empty() {
            let mut g = self.done.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            for (k, v) in loaded.into_iter().take(MAX_ENTRIES) {
                if content_key_of(&k).is_some() {
                    g.insert(k, (now, Ok(v)));
                }
            }
            trim(&mut g, now);
        }
        *self.disk.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    }

    /// A cached answer, without probing.
    pub fn cached(&self, url: &str) -> Option<Result<ProbeInfo, String>> {
        self.cached_keyed(&cache_key(url))
    }

    /// A cached answer under an explicit key, without probing. A content key never expires (the
    /// infohash fixes the bytes); a URL key lasts [`PROBE_TTL`].
    pub fn cached_keyed(&self, key: &str) -> Option<Result<ProbeInfo, String>> {
        let g = self.done.lock().unwrap_or_else(|e| e.into_inner());
        g.get(key)
            .filter(|(at, _)| content_key_of(key).is_some() || at.elapsed() < PROBE_TTL)
            .map(|(_, r)| r.clone())
    }

    /// [`probe`](Self::probe) for a stream ranked from `marker`: a torrent is keyed by its
    /// content (`hash` + file), everything else by its URL. `url` is always what the prober opens.
    pub fn probe_for(
        &self,
        marker: &str,
        file_idx: Option<u32>,
        url: &str,
    ) -> Option<Result<ProbeInfo, String>> {
        match content_key(marker, file_idx) {
            Some(key) => self.probe_keyed(&key, url),
            None => self.probe(url),
        }
    }

    /// [`cached`](Self::cached) for a stream ranked from `marker`.
    pub fn cached_for(
        &self,
        marker: &str,
        file_idx: Option<u32>,
        url: &str,
    ) -> Option<Result<ProbeInfo, String>> {
        match content_key(marker, file_idx) {
            Some(key) => self.cached_keyed(&key),
            None => self.cached(url),
        }
    }

    /// Probe (or wait for the probe already running). `None` when no prober is installed (host
    /// runs of the bin, tests): the caller keeps the label guesses.
    ///
    /// The `done`/`inflight` maps are keyed by [`cache_key`] (host+path, no query), but the prober
    /// is invoked with the caller's original `url` byte-identical \u2014 the first URL to ask
    /// determines the probe URL, and later re-signed URLs for the same file reuse its result.
    pub fn probe(&self, url: &str) -> Option<Result<ProbeInfo, String>> {
        self.probe_keyed(&cache_key(url), url)
    }

    /// [`probe`](Self::probe) under an explicit key. The prober still opens the caller's `url`
    /// byte-identical; only the entry it fills is named by `key`.
    pub fn probe_keyed(&self, key: &str, url: &str) -> Option<Result<ProbeInfo, String>> {
        if let Some(r) = self.cached_keyed(key) {
            return Some(r);
        }
        let prober = self.prober.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        let gate = self.inflight.lock().unwrap_or_else(|e| e.into_inner()).entry(key.to_string()).or_default().clone();
        let _held = gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = self.cached_keyed(key) {
            return Some(r);
        }
        let r = prober(url, PROBE_DEADLINE);
        let content = content_key_of(key).is_some();
        // YouTube 403s are transient (IP throttling / edge variance): caching a googlevideo failure
        // would freeze the shelf as empty. A content key never caches a failure either: the same
        // file becomes readable the moment the swarm delivers its header, and freezing that refusal
        // would turn a slow start into a permanent one.
        let cacheable = r.is_ok() || (!content && !url.contains("googlevideo.com"));
        if cacheable {
            let mut g = self.done.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            g.insert(key.to_string(), (now, r.clone()));
            trim(&mut g, now);
            drop(g);
            if content {
                self.save();
            }
        }
        self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
        Some(r)
    }

    /// Push every entry's age back by `d`, so a test can observe both the expiry rules without
    /// waiting 30 minutes.
    #[cfg(test)]
    fn age(&self, d: Duration) {
        let mut g = self.done.lock().unwrap_or_else(|e| e.into_inner());
        for (at, _) in g.values_mut() {
            *at = at.checked_sub(d).unwrap_or(*at);
        }
    }

    /// Write the content-keyed entries out atomically (temp + rename), so a half-written file is
    /// never loaded. Called once per newly probed file, not per request.
    fn save(&self) {
        let Some(path) = self.disk.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return;
        };
        let keep: HashMap<String, ProbeInfo> = {
            let g = self.done.lock().unwrap_or_else(|e| e.into_inner());
            g.iter()
                .filter(|(k, (_, r))| content_key_of(k).is_some() && r.is_ok())
                .filter_map(|(k, (_, r))| r.as_ref().ok().map(|p| (k.clone(), p.clone())))
                .collect()
        };
        let Ok(bytes) = serde_json::to_vec(&keep) else { return };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, &bytes).is_ok() {
            if let Err(e) = std::fs::rename(&tmp, &path) {
                plx_base::eventlog::log(&format!("probe cache: could not save {}: {e}", path.display()));
            }
        }
    }
}

/// Hold at most [`MAX_ENTRIES`], dropping the oldest first. Expiry alone never bounded the map: a
/// session that plays many titles keeps every entry fresh, and a longer TTL would only make that
/// worse.
fn trim(g: &mut HashMap<String, (Instant, Result<ProbeInfo, String>)>, now: Instant) {
    g.retain(|k, (at, _)| content_key_of(k).is_some() || now.duration_since(*at) < PROBE_TTL);
    if g.len() <= MAX_ENTRIES {
        return;
    }
    let mut ages: Vec<(Instant, String)> = g.iter().map(|(k, (at, _))| (*at, k.clone())).collect();
    ages.sort_unstable();
    for (_, k) in ages.into_iter().take(g.len() - MAX_ENTRIES) {
        g.remove(&k);
    }
}

/// Why this probed stream cannot direct-play, or `None` when it can.
pub fn refusal(p: &ProbeInfo) -> Option<String> {
    let Some(v) = &p.video else { return Some("This stream has no video track.".into()) };
    if !p.video_decodable {
        return Some(format!("This TV cannot play {} video.", v.codec.to_uppercase()));
    }
    if !p.audio.is_empty() && !p.audio.iter().any(|a| a.decodable) {
        let codecs: Vec<String> = p.audio.iter().map(|a| audio_name(a)).collect();
        return Some(format!("Audio format not supported on this TV ({}). Pick another stream.", codecs.join(", ")));
    }
    None
}

fn audio_name(a: &Audio) -> String {
    match (a.codec.as_str(), a.profile) {
        ("dts", 60) => "DTS-HD MA".into(),
        ("dts", _) => "DTS".into(),
        ("truehd", _) => "TrueHD".into(),
        ("eac3", _) => "E-AC-3".into(),
        (c, _) => c.to_uppercase(),
    }
}

fn trc_name(t: i32) -> Option<&'static str> {
    match t {
        16 => Some("smpte2084"),
        18 => Some("arib-std-b67"),
        1 => Some("bt709"),
        _ => None,
    }
}

fn res_class(w: i32, h: i32) -> &'static str {
    if w >= 3200 || h >= 1800 {
        "4k"
    } else if h >= 1000 || w >= 1900 {
        "1080"
    } else if h >= 700 || w >= 1260 {
        "720"
    } else if h >= 560 {
        "576"
    } else {
        "sd"
    }
}

/// Overwrite a guessed `Media` block with what the probe found. Sidecar (addon) subtitle rows
/// already on the part are kept after the embedded ones.
pub fn apply(media: &mut Value, p: &ProbeInfo) {
    let part_id = media["Part"][0]["id"].as_i64().unwrap_or(0);
    let sidecars: Vec<Value> = media["Part"][0]["Stream"]
        .as_array()
        .map(|v| v.iter().filter(|s| s["external"].as_bool() == Some(true)).cloned().collect())
        .unwrap_or_default();
    let mut streams: Vec<Value> = Vec::new();
    let mut index = 0;
    if let Some(v) = &p.video {
        media["videoCodec"] = json!(v.codec);
        media["width"] = json!(v.width);
        media["height"] = json!(v.height);
        media["videoResolution"] = json!(res_class(v.width, v.height));
        media["videoFrameRate"] = json!(if v.fps >= 49.0 { "60p" } else if v.fps >= 29.0 { "NTSC" } else { "24p" });
        media["videoProfile"] = json!(if v.codec == "hevc" && v.bit_depth >= 10 { "main 10" } else { "" });
        let mut s = json!({
            "id": part_id * 10 + 1, "streamType": 1, "codec": v.codec, "index": index,
            "width": v.width, "height": v.height, "frameRate": v.fps, "bitDepth": v.bit_depth,
            "displayTitle": format!("{} ({})", res_class(v.width, v.height).to_uppercase(), v.codec.to_uppercase()),
            "selected": true, "default": true,
        });
        if let Some(t) = trc_name(v.color_trc) {
            s["colorTrc"] = json!(t);
        }
        if v.color_primaries == 9 {
            s["colorPrimaries"] = json!("bt2020");
        }
        if v.color_space == 9 {
            s["colorSpace"] = json!("bt2020nc");
        }
        if let Some(d) = &v.dovi {
            // A profile-7 dual-layer remux with an HDR10 (6) or SDR-compatible (1) base plays as
            // that base: no DOVI keys at all, so the route treats it as plain HEVC (PLAYBACK.md).
            let p7_as_base = d.profile == 7 && matches!(d.bl_compat, 1 | 6);
            if !p7_as_base {
                s["DOVIPresent"] = json!(true);
                s["DOVIProfile"] = json!(d.profile);
                s["DOVILevel"] = json!(d.level);
                s["DOVIBLCompatID"] = json!(d.bl_compat);
                s["DOVIRPUPresent"] = json!(d.rpu);
                s["DOVIELPresent"] = json!(d.el);
                s["DOVIBLPresent"] = json!(d.bl);
                s["DOVIVersion"] = json!("1.0");
            }
        }
        streams.push(s);
        index += 1;
    }
    // Nuvio has no Plex account language preference. Prefer a decodable English track over
    // a release group's foreign-language default. Session track choices still outrank this
    // initial selection in the route. If English is absent, retain the file's usual order.
    let english = |a: &Audio| matches!(a.language.to_ascii_lowercase().as_str(), "en" | "eng" | "en-us" | "en-gb");
    let pick = p.audio.iter().position(|a| english(a) && a.default && a.decodable && !a.title.to_ascii_lowercase().contains("commentary"))
        .or_else(|| p.audio.iter().position(|a| english(a) && a.decodable && !a.title.to_ascii_lowercase().contains("commentary")))
        .or_else(|| p.audio.iter().position(|a| a.default && a.decodable))
        .or_else(|| p.audio.iter().position(|a| a.decodable)).unwrap_or(0);
    for (n, a) in p.audio.iter().enumerate() {
        let atmos = a.profile == 30 && matches!(a.codec.as_str(), "eac3" | "truehd");
        let name = if atmos { format!("{} Atmos", audio_name(a)) } else { audio_name(a) };
        let lang = if a.language.is_empty() { "und".to_string() } else { a.language.clone() };
        streams.push(json!({
            "id": part_id * 10 + 2 + n as i64 * 1000, "streamType": 2, "codec": a.codec, "index": index,
            "channels": a.channels, "samplingRate": a.sample_rate,
            "language": lang, "languageCode": lang,
            "title": if a.title.is_empty() { name.clone() } else { a.title.clone() },
            "displayTitle": format!("{} ({} {}ch)", lang, name, a.channels),
            "selected": n == pick, "default": a.default,
        }));
        if n == pick {
            media["audioCodec"] = json!(a.codec);
            media["audioChannels"] = json!(a.channels);
        }
        index += 1;
    }
    for (n, s) in p.subtitles.iter().enumerate() {
        let lang = if s.language.is_empty() { "und".to_string() } else { s.language.clone() };
        streams.push(json!({
            "id": part_id * 10 + 3 + n as i64 * 1000, "streamType": 3, "codec": s.codec, "index": index,
            "language": lang, "languageCode": lang, "title": s.title,
            "displayTitle": format!("{} ({})", lang, s.codec.to_uppercase()),
            "forced": s.forced, "default": s.default, "selected": false,
        }));
        index += 1;
    }
    for mut s in sidecars {
        s["index"] = json!(index);
        streams.push(s);
        index += 1;
    }
    media["container"] = json!(p.container);
    if p.duration_ms > 0 {
        media["duration"] = json!(p.duration_ms);
        media["Part"][0]["duration"] = json!(p.duration_ms);
        if p.size > 0 {
            media["bitrate"] = json!(p.size * 8 / p.duration_ms);
        }
    }
    if p.size > 0 {
        media["Part"][0]["size"] = json!(p.size);
    }
    media["Part"][0]["container"] = json!(p.container);
    media["Part"][0]["Stream"] = Value::Array(streams);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Two re-signed debrid URLs for the same host+path but different query
    /// strings (`Signature`/`Expires`/`token`) must share one cache entry:
    /// the prober is called once, with the first URL byte-identical, and the
    /// second `probe`/`cached` returns that same `ProbeInfo`. Different
    /// paths, however, stay distinct entries; path bytes (including any
    /// credential embedded in the path) are never stripped.
    #[test]
    fn cache_key_normalizes_query_but_keeps_path() {
        let base = "https://cdn.debrid.example/videos/movie.mkv";
        let a = format!("{base}?Signature=AAA&Expires=111&token=one#frag");
        let b = format!("{base}?Signature=BBB&Expires=222&token=two");
        assert_eq!(cache_key(&a), cache_key(&b));
        assert_eq!(cache_key(&a), base);
        assert_ne!(cache_key(&a), cache_key("https://cdn.debrid.example/videos/other.mkv?Signature=AAA"));
        assert_ne!(
            cache_key("https://cdn.debrid.example/cred/secret-token-1/movie.mkv?x=1"),
            cache_key("https://cdn.debrid.example/cred/secret-token-2/movie.mkv?x=1"),
        );
        // Host casing folds; scheme casing folds; path casing does not.
        assert_eq!(cache_key("HTTPS://CDN.DEBRID.example/videos/m.mkv?x=1"), "https://cdn.debrid.example/videos/m.mkv");
        assert_ne!(cache_key("https://h.example/Videos/m.mkv?x=1"), cache_key("https://h.example/videos/m.mkv?x=1"));
    }

    #[test]
    fn res_signed_urls_for_same_file_probe_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let probe_calls = calls.clone();
        let probe_seen = seen.clone();
        let cache = ProbeCache::default();
        cache.set_prober(Arc::new(move |url: &str, _deadline| {
            probe_calls.fetch_add(1, Ordering::SeqCst);
            probe_seen.lock().unwrap().push(url.to_string());
            Ok(ProbeInfo { container: "mkv".into(), ..Default::default() })
        }));
        let first = "https://cdn.debrid.example/videos/movie.mkv?Signature=AAA&Expires=111";
        let second = "https://cdn.debrid.example/videos/movie.mkv?Signature=BBB&Expires=222&token=two";

        let r1 = cache.probe(first).expect("prober installed");
        let r2 = cache.probe(second).expect("prober installed");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "same host+path must probe once");
        assert_eq!(seen.lock().unwrap().as_slice(), [first.to_string()], "prober gets the first URL byte-identical");
        assert_eq!(r1.expect("ok").container, "mkv");
        assert_eq!(r2.expect("ok").container, "mkv");

        // `cached()` (the read-only path) also hits under the normalized key,
        // and a different path does not.
        assert!(cache.cached(second).is_some());
        assert!(cache.cached("https://cdn.debrid.example/videos/other.mkv?Signature=BBB").is_none());
        let _ = cache.probe("https://cdn.debrid.example/videos/other.mkv?Signature=BBB");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "a different path probes separately");
    }

    /// The C engine mints a random `/stream/<32 hex>` on every resolve, so the same
    /// torrent file arrives under a NEW URL each replay. Keyed by content it probes
    /// once; without that the TV re-read the container header and the tail cues from
    /// the swarm on every replay (5.7 s, then 10.7 s, on one part).
    #[test]
    fn a_new_engine_url_for_the_same_file_reuses_the_probe() {
        let calls = Arc::new(AtomicUsize::new(0));
        let probe_calls = calls.clone();
        let cache = ProbeCache::default();
        cache.set_prober(Arc::new(move |_url: &str, _deadline| {
            probe_calls.fetch_add(1, Ordering::SeqCst);
            Ok(ProbeInfo { container: "mkv".into(), ..Default::default() })
        }));
        let marker = "torrent:9bb5a1b259c6abf5451366b9c0ffee0123456789";
        let first = "http://127.0.0.1:43289/stream/aaaa";
        let second = "http://127.0.0.1:43289/stream/bbbb";

        assert!(cache.probe_for(marker, Some(0), first).is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.probe_for(marker, Some(0), second).is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "same torrent file must probe once");
        assert!(cache.cached_for(marker, Some(0), second).is_some());

        // Another file of the same torrent is its own entry: a season pack has one
        // probe per episode, not one per torrent.
        assert!(cache.probe_for(marker, Some(1), first).is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "each file probes separately");
        // A non-torrent URL keeps the host+path key.
        let h = "https://cdn.debrid.example/videos/m.mkv?Signature=A";
        assert!(cache.probe_for(h, None, h).is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// A content key must not freeze a refusal for the TTL: the same file becomes
    /// readable the moment the swarm delivers its header, and the next resolve has
    /// to reach the prober again. URL keys keep today's behaviour (a refused debrid
    /// link stays refused).
    #[test]
    fn a_content_key_does_not_cache_a_failure() {
        let calls = Arc::new(AtomicUsize::new(0));
        let probe_calls = calls.clone();
        let cache = ProbeCache::default();
        cache.set_prober(Arc::new(move |_url: &str, _deadline| {
            match probe_calls.fetch_add(1, Ordering::SeqCst) {
                0 => Err("not openable yet".into()),
                _ => Ok(ProbeInfo { container: "mkv".into(), ..Default::default() }),
            }
        }));
        let marker = "torrent:1111111111111111111111111111111111111111";
        let url = "http://127.0.0.1:43289/stream/aaaa";
        assert!(cache.probe_for(marker, Some(0), url).expect("prober").is_err());
        assert!(cache.cached_for(marker, Some(0), url).is_none(), "a refusal is not a cached answer");
        assert!(cache.probe_for(marker, Some(0), url).expect("prober").is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "the retry reaches the prober");
    }

    /// A content key cannot go stale, so it must outlive the URL TTL; a URL key must not.
    #[test]
    fn a_content_verdict_never_expires_but_a_url_one_does() {
        let cache = ProbeCache::default();
        cache.set_prober(Arc::new(|_url: &str, _deadline| {
            Ok(ProbeInfo { container: "mkv".into(), ..Default::default() })
        }));
        let marker = "torrent:2222222222222222222222222222222222222222";
        assert!(cache.probe_for(marker, Some(0), "http://127.0.0.1:1/stream/aa").is_some());
        assert!(cache.probe("https://cdn.example/v/m.mkv?Signature=A").is_some());
        cache.age(PROBE_TTL * 3);
        assert!(cache.cached_for(marker, Some(0), "http://127.0.0.1:1/stream/bb").is_some(), "content outlives the TTL");
        assert!(cache.cached("https://cdn.example/v/m.mkv?Signature=B").is_none(), "a URL verdict expires");
    }

    /// The point of `attach`: a reboot used to re-pay the probe (10 s on a 4K remux, because the
    /// payload is tmpfs and the header and tail cues come back from the swarm).
    #[test]
    fn a_content_verdict_survives_a_relaunch() {
        let path = plx_base::testscratch::process_dir("probe-cache-reload").join(PROBE_CACHE_FILE);
        let marker = "torrent:3333333333333333333333333333333333333333";
        let url = "http://127.0.0.1:1/stream/aa";
        {
            let cache = ProbeCache::default();
            cache.attach(path.clone());
            cache.set_prober(Arc::new(|_u: &str, _d| {
                Ok(ProbeInfo { container: "mkv".into(), duration_ms: 7200000, ..Default::default() })
            }));
            assert!(cache.probe_for(marker, Some(0), url).is_some());
            // A URL verdict is not durable state: it is a signed link's answer.
            assert!(cache.probe("https://cdn.example/v/m.mkv?Signature=A").is_some());
        }
        let again = ProbeCache::default();
        again.attach(path.clone());
        let hit = again.cached_for(marker, Some(0), url).expect("restored from disk");
        assert_eq!(hit.expect("ok").duration_ms, 7200000);
        assert!(again.cached("https://cdn.example/v/m.mkv?Signature=B").is_none(), "URL answers stay in RAM");
        // And a prober that must not run proves the hit above came from the file.
        again.set_prober(Arc::new(|_u: &str, _d| panic!("restored entry must not re-probe")));
        assert!(again.probe_for(marker, Some(0), url).is_some());
    }

    /// `PROBE_TTL` alone never bounded the map: a long session keeps every entry fresh, so the
    /// ceiling has to evict by age.
    #[test]
    fn the_cache_drops_the_oldest_past_its_ceiling() {
        let cache = ProbeCache::default();
        cache.set_prober(Arc::new(|_u: &str, _d| {
            Ok(ProbeInfo { container: "mkv".into(), ..Default::default() })
        }));
        for i in 0..(MAX_ENTRIES + 8) {
            let marker = format!("torrent:{i:040x}");
            let _ = cache.probe_for(&marker, Some(0), "http://127.0.0.1:1/stream/aa");
        }
        let len = cache.done.lock().unwrap().len();
        assert_eq!(len, MAX_ENTRIES, "the ceiling holds");
        let oldest = "torrent:0000000000000000000000000000000000000000";
        assert!(cache.cached_for(oldest, Some(0), "http://127.0.0.1:1/stream/aa").is_none(), "oldest evicted");
        let newest = format!("torrent:{:040x}", MAX_ENTRIES + 7);
        assert!(cache.cached_for(&newest, Some(0), "http://127.0.0.1:1/stream/aa").is_some());
    }

    #[test]
    fn english_audio_beats_a_decodable_foreign_default() {
        let mut p = p7(6);
        p.audio = vec![
            Audio { codec: "ac3".into(), language: "rus".into(), default: true, decodable: true, ..Default::default() },
            Audio { codec: "eac3".into(), language: "eng".into(), decodable: true, ..Default::default() },
        ];
        let mut m = json!({"Part": [{"id": 5}]});
        apply(&mut m, &p);
        let s = &m["Part"][0]["Stream"];
        assert_eq!(s[1]["selected"], false);
        assert_eq!(s[2]["selected"], true);
    }

    /// The player's fork lanes read the track's own rate off the Media: the AC3 encode lane
    /// declares at 48 kHz only, and the PCM lane states the exact rate in its `pcmInfo`. The probe
    /// already knows it, so the shim must say it — without `samplingRate` every track arrived with
    /// "rate not told" and the TrueHD Load was refused outright (TV log 2026-11-01).
    #[test]
    fn audio_streams_publish_the_probed_sampling_rate() {
        let mut p = p7(6);
        p.audio[0].channels = 8;
        p.audio[0].sample_rate = 48_000;
        let mut m = json!({"Part": [{"id": 5}]});
        apply(&mut m, &p);
        let audio = &m["Part"][0]["Stream"][1];
        assert_eq!(audio["streamType"], 2);
        assert_eq!(audio["samplingRate"], 48_000);
        assert_eq!(audio["channels"], 8);
    }

    fn p7(bl_compat: u8) -> ProbeInfo {
        ProbeInfo {
            container: "mkv".into(),
            duration_ms: 7_200_000,
            size: 72_000_000_000,
            video: Some(Video { codec: "hevc".into(), width: 3840, height: 2160, fps: 23.976, bit_depth: 10, color_trc: 16, color_primaries: 9, color_space: 9,
                dovi: Some(Dovi { profile: 7, level: 6, bl_compat, rpu: true, el: true, bl: true }), ..Default::default() }),
            video_decodable: true,
            audio: vec![
                Audio { codec: "truehd".into(), profile: 30, channels: 8, language: "eng".into(), default: true, decodable: false, ..Default::default() },
                Audio { codec: "ac3".into(), channels: 6, language: "eng".into(), decodable: true, ..Default::default() },
            ],
            subtitles: vec![Sub { codec: "subrip".into(), language: "eng".into(), ..Default::default() }],
        }
    }

    #[test]
    fn english_commentary_does_not_outrank_english_dialogue() {
        let mut p = p7(6);
        p.audio = vec![
            Audio { codec: "ac3".into(), language: "en".into(), title: "Director commentary".into(), default: true, decodable: true, ..Default::default() },
            Audio { codec: "eac3".into(), language: "en-US".into(), decodable: true, ..Default::default() },
        ];
        let mut m = json!({"Part": [{"id": 5}]}); apply(&mut m, &p);
        assert_eq!(m["Part"][0]["Stream"][2]["selected"], true);
        assert_eq!(m["audioCodec"], "eac3");
    }

    #[test]
    fn p7_with_hdr10_base_drops_dovi_and_selects_decodable_audio() {
        let mut m = json!({"Part": [{"id": 5, "Stream": [{"streamType": 3, "external": true, "key": "/library/streams/9.srt"}]}]});
        apply(&mut m, &p7(6));
        let s = &m["Part"][0]["Stream"];
        assert!(s[0].get("DOVIPresent").is_none());
        assert_eq!(s[0]["colorTrc"], "smpte2084");
        assert_eq!(s[1]["selected"], false);
        assert_eq!(s[2]["selected"], true);
        assert_eq!(s[4]["key"], "/library/streams/9.srt");
        assert_eq!(m["videoResolution"], "4k");
        assert_eq!(m["bitrate"], 80_000);
        assert!(refusal(&p7(6)).is_none());
    }

    #[test]
    fn p8_keeps_dovi_and_undecodable_audio_refuses() {
        let mut p = p7(1);
        p.video.as_mut().unwrap().dovi = Some(Dovi { profile: 8, level: 6, bl_compat: 1, rpu: true, el: false, bl: true });
        let mut m = json!({"Part": [{"id": 5}]});
        apply(&mut m, &p);
        assert_eq!(m["Part"][0]["Stream"][0]["DOVIProfile"], 8);
        p.audio.retain(|a| !a.decodable);
        assert!(refusal(&p).unwrap().contains("TrueHD"));
    }
}
