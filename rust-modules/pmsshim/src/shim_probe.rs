//! What a stream really holds, from the app's bundled FFmpeg (`plx_media::ff::probe::probe_url`),
//! injected at boot as a [`Prober`] so this crate does not depend on the media layer. Field names
//! mirror `plx_media::ff_probe::MediaProbe` one for one; the app's adapter is a copy plus the two
//! `*_decodable` verdicts it takes from the TV's codec table (`devcaps`).
//!
//! `docs/nuvio/PLAYBACK.md` is the contract: probe at decision time, cache per URL, emit the real
//! fields, and refuse (never "transcode") a stream the TV cannot decode.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct Dovi {
    pub profile: u8,
    pub level: u8,
    pub bl_compat: u8,
    pub rpu: bool,
    pub el: bool,
    pub bl: bool,
}

#[derive(Clone, Debug, Default)]
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

#[derive(Clone, Debug, Default)]
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

#[derive(Clone, Debug, Default)]
pub struct Sub {
    pub codec: String,
    pub language: String,
    pub title: String,
    pub default: bool,
    pub forced: bool,
}

#[derive(Clone, Debug, Default)]
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

pub const PROBE_DEADLINE: Duration = Duration::from_secs(45);
const PROBE_TTL: Duration = Duration::from_secs(30 * 60);

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
    format!("{scheme}://{authority}{path}")
}

#[derive(Default)]
pub struct ProbeCache {
    prober: Mutex<Option<Prober>>,
    done: Mutex<HashMap<String, (Instant, Result<ProbeInfo, String>)>>,
    /// One probe per URL at a time: a second asker waits on the first.
    inflight: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ProbeCache {
    pub fn set_prober(&self, p: Prober) {
        *self.prober.lock().unwrap_or_else(|e| e.into_inner()) = Some(p);
    }

    pub fn has_prober(&self) -> bool {
        self.prober.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    /// A cached answer, without probing.
    pub fn cached(&self, url: &str) -> Option<Result<ProbeInfo, String>> {
        let key = cache_key(url);
        let g = self.done.lock().unwrap_or_else(|e| e.into_inner());
        g.get(&key).filter(|(at, _)| at.elapsed() < PROBE_TTL).map(|(_, r)| r.clone())
    }

    /// Probe (or wait for the probe already running). `None` when no prober is installed (host
    /// runs of the bin, tests): the caller keeps the label guesses.
    ///
    /// The `cached`/`done`/`inflight` maps are keyed by [`cache_key`] (host+path,
    /// no query), but the prober is invoked with the caller's original `url`
    /// byte-identical \u2014 the first URL to ask determines the probe URL, and
    /// later re-signed URLs for the same file reuse its result.
    pub fn probe(&self, url: &str) -> Option<Result<ProbeInfo, String>> {
        let key = cache_key(url);
        if let Some(r) = self.cached(url) {
            return Some(r);
        }
        let prober = self.prober.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        let gate = self.inflight.lock().unwrap_or_else(|e| e.into_inner()).entry(key.clone()).or_default().clone();
        let _held = gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = self.cached(url) {
            return Some(r);
        }
        let r = prober(url, PROBE_DEADLINE);
        let mut g = self.done.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() > 300 {
            g.retain(|_, (at, _)| at.elapsed() < PROBE_TTL);
        }
        g.insert(key.clone(), (Instant::now(), r.clone()));
        self.inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        Some(r)
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
