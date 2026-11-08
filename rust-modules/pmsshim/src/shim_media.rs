//! Streams → PMS `Media[]`. One Media per playable stream, best first, because the front end plays
//! `Media[0]` (it has no version picker); `/nuvio/streams` and `/nuvio/select` let a picker reorder.

use plx_stremio::classify::Hdr;
use plx_stremio::classify::StreamInfo;
use plx_stremio::model::Stream;
use serde_json::{json, Value};

/// Streams per item exposed as Media. Torrentio can return 150; nobody walks past this many.
pub const MAX_STREAMS: usize = 200;

/// `part id = ratingKey * PART_STRIDE + stream index`.
pub const PART_STRIDE: i64 = 1000;

pub fn classify(s: &Stream, runtime_secs: Option<u32>) -> StreamInfo {
    plx_stremio::classify::classify_stream(s, runtime_secs)
}

/// A torrent the streaming server must pick the file of (`/create`) — resolved when it is played,
/// never while listing. The marker keeps it rankable and identifiable until then.
pub const TORRENT_SCHEME: &str = "torrent:";

/// The URL the TV should open, a [`TORRENT_SCHEME`] marker for a torrent still to resolve, or None
/// when this stream cannot play here (YouTube, external links, a torrent with no streaming server).
pub fn playable_url(s: &Stream, torrent_server: Option<&str>) -> Option<String> {
    playable_url_with(s, torrent_server, plx_stremio::sbengine::available())
}

/// [`playable_url`] with the engine's availability passed in, so the branch this exists for — a
/// torrent must become a marker, never a Node-style URL, when the engine will serve it — is
/// testable on a host that cannot load the ARM library.
fn playable_url_with(s: &Stream, torrent_server: Option<&str>, engine_available: bool) -> Option<String> {
    // Debrid/HTTP links play as they are, engine or not.
    if let Some(u) = plx_stremio::torrent::direct_url(s) {
        return Some(u.to_string());
    }
    // The in-process engine resolves a torrent at open time (`open_url`). Handing the player the
    // Node-style `/{hash}/{idx}` URL would bypass it and connect to a server this build does not
    // start — the failure mode is `curlio: transport failed rc=7 — could not connect` on 11470.
    // `torrent_server` still gates that torrents are offered at all.
    if !engine_available {
        if let Some(u) = plx_stremio::torrent::playable_url(s, torrent_server) {
            return Some(u);
        }
    }
    let hash = plx_stremio::torrent::info_hash(s)?;
    torrent_server.filter(|t| !t.trim().is_empty())?;
    Some(format!("{TORRENT_SCHEME}{hash}"))
}

pub struct Classified {
    pub stream: Stream,
    pub info: StreamInfo,
    pub url: String,
}

fn hdr_rank(h: Hdr) -> u8 {
    match h {
        Hdr::DolbyVision { .. } => 3,
        Hdr::Hdr10Plus | Hdr::Hdr10 => 2,
        Hdr::Hlg => 1,
        Hdr::Sdr => 0,
    }
}

/// Keep the playable streams, classify, and order best first: debrid-cached/direct HTTP before
/// torrents, then the [`StreamInfo::rank`] key — resolution, then seeders, then HDR, then size
/// (bigger is higher bitrate for the same title). Seeders matters only among torrents: a cached
/// stream has no seeder count to compare, and in any case the direct flag floats it ahead outright.
/// `audio_ok`: the TV's audio codecs; a stream whose label names a codec outside it (DTS, TrueHD on
/// a CX) ranks after the playable ones of its resolution. Empty = unknown.
pub fn rank(streams: &[Stream], runtime_secs: Option<u32>, torrent_server: Option<&str>, audio_ok: &[String]) -> Vec<Classified> {
    let mut v: Vec<Classified> = streams
        .iter()
        .filter_map(|s| {
            let url = playable_url(s, torrent_server)?;
            Some(Classified { info: classify(s, runtime_secs), stream: s.clone(), url })
        })
        .collect();
    v.sort_by(|a, b| {
        let key = |c: &Classified| {
            let direct = c.stream.url.is_some() || c.info.cached;
            let audio_bad = !audio_ok.is_empty() && c.info.audio_codec.as_ref().is_some_and(|a| !audio_ok.iter().any(|x| x == a));
            (
                std::cmp::Reverse(direct),
                std::cmp::Reverse(c.info.height.unwrap_or(0)),
                audio_bad,
                std::cmp::Reverse(c.info.seeders.unwrap_or(0)),
                std::cmp::Reverse(hdr_rank(c.info.hdr)),
                std::cmp::Reverse(c.info.size_bytes.unwrap_or(0)),
            )
        };
        key(a).cmp(&key(b))
    });
    v.truncate(MAX_STREAMS);
    v
}

fn res_class(h: Option<u32>) -> &'static str {
    match h {
        Some(h) if h >= 1800 => "4k",
        Some(h) if h >= 1000 => "1080",
        Some(h) if h >= 700 => "720",
        Some(h) if h >= 560 => "576",
        Some(_) => "sd",
        None => "",
    }
}

fn container_of(c: &Classified) -> String {
    if let Some(x) = &c.info.container {
        return x.clone();
    }
    let name = c.info.filename.clone().or_else(|| c.stream.behavior_hints.filename.clone()).unwrap_or_else(|| c.url.clone());
    let path = name.split(['?', '#']).next().unwrap_or("").to_ascii_lowercase();
    for ext in ["mkv", "mp4", "m4v", "webm", "avi", "ts", "m2ts", "mov"] {
        if path.ends_with(&format!(".{ext}")) {
            return if ext == "m4v" { "mp4".into() } else { ext.into() };
        }
    }
    if path.ends_with(".m3u8") || path.contains("/hls") {
        return "hls".into();
    }
    // Torrents and debrid links without a file name are overwhelmingly Matroska.
    "mkv".into()
}

/// A subtitle the shim serves at `/library/streams/{id}`.
pub struct SubRef {
    pub id: i64,
    pub lang: String,
    /// The release name, when the provider named one. PMS carries it in the stream's `title`,
    /// and the player's Search page uses it to recognise a candidate the item already has.
    pub title: String,
    /// Plex's `default`/`selected` for this track: the one the viewer's account language asks for
    /// (see [`apply_subtitle_preference`]). A sidecar carries no container ordinal, so this pair is
    /// the ONLY way the app can start with it on — `metadata::server_selected_sidecar` reads it.
    pub selected: bool,
}

/// Marks which of `subs` the viewer's account language picks, if any. `pref` is the account's
/// `defaultSubtitleLanguage` (the picker's code). An exact tag wins over a folded one, so a
/// `pt-BR` preference takes the `pt-BR` track rather than the first `pt` one. With no preference
/// at all the first sidecar is selected (a signed-out install still shows subtitles); a preference
/// that matches nothing claims nothing, leaving the file's own order alone.
///
/// Forced tracks cannot be ranked on: Stremio subtitle objects carry no forced flag and [`SubRef`]
/// has nowhere to keep one. `defaultSubtitleForced` is a mode, not a per-track attribute.
pub fn apply_subtitle_preference(subs: &mut [SubRef], pref: Option<&str>) {
    let hit = match pref.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => subs
            .iter()
            .position(|s| s.lang.trim().eq_ignore_ascii_case(p))
            .or_else(|| {
                let want = lang_key(p);
                subs.iter().position(|s| !want.is_empty() && lang_key(&s.lang) == want)
            }),
        None => (!subs.is_empty()).then_some(0),
    };
    for (i, s) in subs.iter_mut().enumerate() {
        s.selected = hit == Some(i);
    }
}

/// ISO-639-2/B and /T codes → the 639-1 code the picker uses. **A table, never a truncation**:
/// `kan` is Kannada (`kn`), not Georgian (`ka`); `spa` is Spanish (`es`), not `sp`; `jpn` is
/// Japanese (`ja`), not `jp`. A code this table does not name is compared verbatim, so an
/// unknown tag is a false negative and never a false match.
const CODE3: &[(&str, &str)] = &[
    ("afr", "af"), ("alb", "sq"), ("amh", "am"), ("ara", "ar"), ("arm", "hy"), ("aze", "az"),
    ("baq", "eu"), ("bel", "be"), ("ben", "bn"), ("bos", "bs"), ("bul", "bg"), ("bur", "my"),
    ("cat", "ca"), ("chi", "zh"), ("cze", "cs"), ("dan", "da"), ("dut", "nl"), ("ell", "el"),
    ("eng", "en"), ("est", "et"), ("eus", "eu"), ("fas", "fa"), ("fin", "fi"), ("fra", "fr"),
    ("fre", "fr"), ("geo", "ka"), ("ger", "de"), ("gle", "ga"), ("glg", "gl"), ("gre", "el"),
    ("guj", "gu"), ("heb", "he"), ("hin", "hi"), ("hrv", "hr"), ("hun", "hu"), ("hye", "hy"),
    ("ice", "is"), ("ind", "id"), ("isl", "is"), ("ita", "it"), ("jav", "jv"), ("jpn", "ja"),
    ("kan", "kn"), ("kat", "ka"), ("kaz", "kk"), ("khm", "km"), ("kor", "ko"), ("kur", "ku"),
    ("lao", "lo"), ("lat", "la"), ("lav", "lv"), ("lit", "lt"), ("mac", "mk"), ("mal", "ml"),
    ("mao", "mi"), ("mar", "mr"), ("may", "ms"), ("mkd", "mk"), ("mlt", "mt"), ("mon", "mn"),
    ("mri", "mi"), ("msa", "ms"), ("mya", "my"), ("nep", "ne"), ("nld", "nl"), ("nor", "no"),
    ("oci", "oc"), ("orm", "om"), ("pan", "pa"), ("per", "fa"), ("pol", "pl"), ("por", "pt"),
    ("pus", "ps"), ("ron", "ro"), ("rum", "ro"), ("rus", "ru"), ("san", "sa"), ("sin", "si"),
    ("slo", "sk"), ("slk", "sk"), ("slv", "sl"), ("snd", "sd"), ("som", "so"), ("spa", "es"),
    ("sqi", "sq"), ("srp", "sr"), ("sun", "su"), ("swa", "sw"), ("swe", "sv"), ("tam", "ta"),
    ("tel", "te"), ("tgk", "tg"), ("tgl", "tl"), ("tha", "th"), ("tib", "bo"), ("tur", "tr"),
    ("tuk", "tk"), ("ukr", "uk"), ("urd", "ur"), ("uzb", "uz"), ("vie", "vi"), ("wel", "cy"),
    ("yid", "yi"), ("yor", "yo"), ("zho", "zh"), ("zul", "zu"),
    // The /T (terminology) spellings of the same languages, plus a few common ones the first
    // block did not name.
    ("bod", "bo"), ("cym", "cy"), ("deu", "de"), ("epo", "eo"), ("ewe", "ee"), ("kal", "kl"),
    ("kir", "ky"), ("lug", "lg"), ("nde", "nd"), ("nno", "nn"), ("nob", "nb"), ("nya", "ny"),
    ("ori", "or"), ("oss", "os"), ("pli", "pi"), ("sag", "sg"), ("smo", "sm"), ("sot", "st"),
    ("ssw", "ss"), ("tsn", "tn"), ("tso", "ts"), ("xho", "xh"), ("zap", "za"), ("zha", "za"),
    // Spellings OpenSubtitles v3 actually sends for languages the picker distinguishes no
    // further: `pob` is Portuguese (Brazil), `spl` Spanish (Latin America), `scc` Serbian,
    // `zht`/`zhe` Chinese. `tat` is Tatar, which the picker offers as `tt`.
    ("pob", "pt"), ("spl", "es"), ("scc", "sr"), ("zht", "zh"), ("zhe", "zh"), ("mne", "sr"),
    ("tat", "tt"),
];

/// Full display names an addon may use instead of a code (`"Indonesian"`, `"English"`). Only the
/// languages the picker offers are listed; anything else is compared verbatim.
const NAMES: &[(&str, &str)] = &[
    ("arabic", "ar"), ("bahasa indonesia", "id"), ("bahasa melayu", "ms"), ("bengali", "bn"), ("bulgarian", "bg"), ("cantonese", "zh"),
    ("chinese", "zh"), ("croatian", "hr"), ("czech", "cs"), ("danish", "da"),
    ("dutch", "nl"), ("english", "en"), ("farsi", "fa"), ("filipino", "fil"),
    ("finnish", "fi"), ("french", "fr"), ("german", "de"), ("greek", "el"),
    ("hebrew", "he"), ("hindi", "hi"), ("hungarian", "hu"), ("indonesian", "id"),
    ("italian", "it"), ("japanese", "ja"), ("korean", "ko"), ("malay", "ms"),
    ("norwegian", "no"), ("persian", "fa"), ("polish", "pl"), ("portuguese", "pt"),
    ("romanian", "ro"), ("russian", "ru"), ("serbian", "sr"), ("slovak", "sk"),
    ("slovenian", "sl"), ("spanish", "es"), ("swedish", "sv"), ("tagalog", "tl"),
    ("tamil", "ta"), ("telugu", "te"), ("thai", "th"), ("turkish", "tr"),
    ("ukrainian", "uk"), ("urdu", "ur"), ("vietnamese", "vi"),
];

/// The language one tag names, folded to the picker's code. Mirrors `plx_data::metadata::lang_key`
/// (the shim does not depend on the data crate). Only the primary subtag counts.
pub(crate) fn lang_key(lang: &str) -> String {
    let s = lang.trim().to_ascii_lowercase();
    // A display name may contain a space ("Bahasa Indonesia"), so it is checked WHOLE before the
    // primary-subtag split below.
    if let Some((_, to)) = NAMES.iter().find(|(name, _)| *name == s) {
        return (*to).to_string();
    }
    let code = s.split(['-', '_', ' ']).next().unwrap_or("");
    if code.is_empty() {
        return String::new();
    }
    let folded = if code.len() > 3 {
        NAMES.iter().find(|(name, _)| *name == code).map(|(_, to)| *to)
    } else {
        CODE3.iter().find(|(from, _)| *from == code).map(|(_, to)| *to)
    };
    folded.unwrap_or(code).to_string()
}

/// The Media block for stream `idx` of item `rk`.
pub fn media_json(rk: i64, idx: usize, c: &Classified, duration_ms: i64, subs: &[SubRef]) -> Value {
    let part_id = rk * PART_STRIDE + idx as i64;
    let height = c.info.height.unwrap_or(0) as i64;
    let width = match height {
        0 => 0,
        h if h >= 2000 => 3840,
        h if h >= 1000 => 1920,
        h if h >= 700 => 1280,
        h => h * 16 / 9,
    };
    // Unknown codec: modern 4K/HDR releases are HEVC; everything else is most likely H.264.
    let vcodec = c.info.video_codec.clone().unwrap_or_else(|| if height >= 2000 || c.info.hdr != Hdr::Sdr { "hevc".into() } else { "h264".into() });
    let acodec = c.info.audio_codec.clone().unwrap_or_else(|| "aac".into());
    let container = container_of(c);
    let kbps = c.info.bitrate_kbps.map(i64::from).or_else(|| {
        let size = c.info.size_bytes? as i64;
        (duration_ms > 0).then(|| size * 8 / duration_ms) // bytes*8/ms = kbit/s
    });
    let ten_bit = c.info.hdr != Hdr::Sdr;
    let mut video = json!({
        "id": part_id * 10 + 1, "streamType": 1, "codec": vcodec, "index": 0,
        "width": width, "height": height, "bitDepth": if ten_bit { 10 } else { 8 },
        "displayTitle": c.info.label, "extendedDisplayTitle": c.info.label,
        "selected": true, "default": true,
    });
    match c.info.hdr {
        Hdr::Hdr10 | Hdr::Hdr10Plus => video["colorTrc"] = json!("smpte2084"),
        Hdr::Hlg => video["colorTrc"] = json!("arib-std-b67"),
        Hdr::DolbyVision { with_hdr10_fallback } => {
            // The label cannot tell the profile; a release that also advertises HDR10 is a
            // profile 8.1 (or a 7 remux, which the TV plays as its 8.1-compatible base), a DV-only
            // web release is profile 5. The demuxer reads the real configuration record.
            let (profile, compat) = if with_hdr10_fallback { (8, 1) } else { (5, 0) };
            video["colorTrc"] = json!("smpte2084");
            video["DOVIPresent"] = json!(true);
            video["DOVIProfile"] = json!(profile);
            video["DOVIBLCompatID"] = json!(compat);
            video["DOVIBLPresent"] = json!(true);
            video["DOVIRPUPresent"] = json!(true);
            video["DOVIELPresent"] = json!(false);
            video["DOVILevel"] = json!(6);
            video["DOVIVersion"] = json!("1.0");
        }
        Hdr::Sdr => {}
    }
    let audio = json!({
        "id": part_id * 10 + 2, "streamType": 2, "codec": acodec, "index": 1,
        "channels": if c.info.atmos || matches!(acodec.as_str(), "eac3" | "ac3" | "truehd" | "dts") { 6 } else { 2 },
        "selected": true, "default": true,
        "displayTitle": acodec.to_uppercase(),
    });
    let mut streams = vec![video, audio];
    for (n, s) in subs.iter().enumerate() {
        streams.push(json!({
            "id": s.id, "streamType": 3, "codec": "srt", "index": 2 + n, "external": true,
            "key": format!("/library/streams/{}.srt", s.id), "language": s.lang, "languageCode": s.lang,
            "title": s.title, "displayTitle": format!("{} (SRT)", s.lang),
            "selected": s.selected, "default": s.selected,
        }));
    }
    let ext = if container == "hls" { "m3u8".to_string() } else { container.clone() };
    json!({
        "id": part_id, "duration": duration_ms, "bitrate": kbps.unwrap_or(0),
        "width": width, "height": height, "videoResolution": res_class(c.info.height),
        "container": container, "videoCodec": vcodec, "audioCodec": acodec,
        "audioChannels": if c.info.atmos { 8 } else { 6 },
        "videoProfile": if ten_bit && vcodec == "hevc" { "main 10" } else { "" },
        "title": c.info.label,
        // The addon that returned this stream, so the picker can group its list by source. Empty
        // only on the placeholder Media that stands in while the addons are still answering.
        "addon_name": c.stream.addon_name,
        // The delivery is the local torrent engine (no addon URL — the shim resolves the hash
        // itself). The diagnostics panel reads this to swap its Plex-model block for live swarm
        // stats; a real PMS never sends it, so it reads false there.
        "p2p": c.stream.url.is_none(),
        "Part": [{
            "id": part_id, "key": format!("/library/parts/{part_id}/0/file.{ext}"),
            "duration": duration_ms, "file": c.info.filename.clone().unwrap_or_default(),
            "size": c.info.size_bytes.unwrap_or(0), "container": container,
            "Stream": streams,
        }],
    })
}

/// The picker's view of one stream (`/nuvio/streams`).
pub fn summary_json(idx: usize, c: &Classified) -> Value {
    json!({
        "index": idx,
        "label": c.info.label,
        "addon": c.stream.addon_name,
        "name": c.stream.name,
        "description": c.stream.description.clone().or_else(|| c.stream.title.clone()),
        "height": c.info.height,
        "hdr": format!("{:?}", c.info.hdr),
        "videoCodec": c.info.video_codec,
        "audioCodec": c.info.audio_codec,
        "atmos": c.info.atmos,
        "sizeBytes": c.info.size_bytes,
        "bitrateKbps": c.info.bitrate_kbps,
        "seeders": c.info.seeders,
        "cached": c.info.cached,
        "source": c.info.source,
        "torrent": c.stream.url.is_none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(name: &str, url: Option<&str>, hash: Option<&str>) -> Stream {
        Stream { name: Some(name.into()), url: url.map(Into::into), info_hash: hash.map(Into::into), ..Default::default() }
    }

    #[test]
    fn ranks_direct_then_resolution_and_skips_unplayable() {
        let v = vec![
            s("720p", Some("https://a/720.mp4"), None),
            s("4k HDR", None, Some("abcdef0123456789abcdef0123456789abcdef01")),
            s("1080p", Some("https://a/1080.mkv"), None),
            Stream { yt_id: Some("x".into()), ..Default::default() },
        ];
        let r = rank(&v, None, None, &[]);
        assert_eq!(r.len(), 2, "torrent without a server and YouTube are dropped");
        assert_eq!(r[0].url, "https://a/1080.mkv");
        let r = rank(&v, None, Some("http://10.0.0.2:11470/"), &[]);
        assert_eq!(r.len(), 3);
        assert!(r[2].url.starts_with(TORRENT_SCHEME), "no fileIdx: resolved on play");
    }

    /// **A torrent's seeder count outranks its size (and its HDR) once resolution is tied.**
    /// The first [`rank`] kept HDR and size ahead of seeders, so a huge dead torrent beat a
    /// small lively one at the same resolution. Both are pure torrents here (a `torrent:` marker
    /// each, no cached/direct-http flag), at the same 1080p, so the ONLY differences are
    /// seeders/HDR/size, and seeders must decide while the debrid/direct group still leads outright.
    #[test]
    fn ranks_torrents_by_seeders_then_size_after_resolution() {
        let hash = |h: u8| format!("{h:02x}").repeat(20);
        // (name, seeders) — the size is in the name as well so size also differs.
        let mk = |name: &str, desc: &str, h: u8| {
            let mut st = s(name, None, Some(&hash(h)));
            st.description = Some(desc.into());
            st
        };
        let v = vec![
            mk("1080p big dead", "👤 2 💾 20 GB", 1),
            mk("1080p HDR small lively", "👤 950 💾 2 GB", 2),
            mk("1080p mid", "👤 10 💾 8 GB", 3),
            mk("1080p unknown seeders", "💾 99 GB", 4),
            mk("1080p zero seeders", "👤 0 💾 90 GB", 5),
            mk("4k tiny", "👤 1 💾 1 GB", 6),
        ];
        let r = rank(&v, None, Some("http://10.0.0.2:11470/"), &[]);
        assert_eq!(r.len(), 6);
        // 4K with 1 seeder still leads: resolution is the outermost key.
        assert!(r[0].url.ends_with(&hash(6)), "resolution dominates seeders: {:?}", r[0].stream.name);
        // Within the 1080p torrent group: most seeders first, then size.
        assert!(r[1].url.ends_with(&hash(2)), "950 seeders beats a bigger HDR file");
        assert!(r[2].url.ends_with(&hash(3)));
        // 2-seeder torrent beats the unknown/zero-seeder huge files.
        assert!(r[3].url.ends_with(&hash(1)), "2 known seeders beat unknown/zero");
        // Unknown seeders sort as 0, so they fall to size among the zero-seeder group.
        assert!(r[4].url.ends_with(&hash(4)), "unknown seeders treat as 0, then size decides");
        assert!(r[5].url.ends_with(&hash(5)));
    }

    /// **A torrent stream is marked for the engine.** `p2p` is what lets the player's diagnostics
    /// panel swap its Plex-model block for live swarm numbers; an addon URL is not engine-fed, so
    /// it must not claim to be.
    #[test]
    fn media_json_marks_only_engine_fed_streams_as_p2p() {
        let direct = Classified {
            stream: s("direct", Some("https://h/f.mkv"), None),
            info: StreamInfo::default(),
            url: "https://h/f.mkv".into(),
        };
        let torrent = Classified {
            stream: s("torrent", None, Some("abcdef0123456789abcdef0123456789abcdef01")),
            info: StreamInfo::default(),
            url: format!("{TORRENT_SCHEME}abcdef0123456789abcdef0123456789abcdef01"),
        };
        let d = media_json(10_001, 0, &direct, 0, &[]);
        let t = media_json(10_002, 0, &torrent, 0, &[]);
        assert_eq!(d["p2p"], false, "an addon URL plays without the engine");
        assert_eq!(t["p2p"], true, "an info-hash stream is the engine's to fetch");
    }

    #[test]
    fn media_shape() {
        let mut stream = s("x", Some("https://h/f.mkv?t=1"), None);
        stream.addon_name = "Torrentio".into();
        let c = Classified { stream, info: StreamInfo { height: Some(2160), hdr: Hdr::DolbyVision { with_hdr10_fallback: true }, size_bytes: Some(60_000_000_000), filename: Some("Some.Release.2024.2160p.mkv".into()), ..Default::default() }, url: "https://h/f.mkv?t=1".into() };
        let m = media_json(10_001, 3, &c, 7_200_000, &[SubRef { id: 9, lang: "eng".into(), title: "Some.Release".into(), selected: false }]);
        assert_eq!(m["videoResolution"], "4k");
        assert_eq!(m["container"], "mkv");
        assert_eq!(m["addon_name"], "Torrentio", "the picker groups by addon");
        assert_eq!(m["Part"][0]["file"], "Some.Release.2024.2160p.mkv", "the release's own name is what the diagnostics File row shows");
        assert_eq!(m["Part"][0]["size"], 60_000_000_000i64);
        assert_eq!(m["videoCodec"], "hevc");
        assert_eq!(m["Part"][0]["id"], 10_001_003);
        assert_eq!(m["Part"][0]["key"], "/library/parts/10001003/0/file.mkv");
        assert_eq!(m["Part"][0]["Stream"][0]["DOVIProfile"], 8);
        assert_eq!(m["Part"][0]["Stream"][2]["key"], "/library/streams/9.srt");
        assert_eq!(m["Part"][0]["Stream"][2]["title"], "Some.Release", "the release name travels (docs/pms-api.md §8)");
        assert_eq!(m["Part"][0]["Stream"][2]["selected"], false);
        assert_eq!(m["bitrate"], 66_666);
    }

    /// **The account's subtitle language is the one that starts ON.** Plex carries no other way to
    /// say "this sidecar is my pick" — a sidecar has no container ordinal — so `default` and
    /// `selected` on the track ARE the preference the app restores on play.
    #[test]
    fn the_account_language_marks_one_subtitle_selected() {
        let mut subs = [
            SubRef { id: 1, lang: "fre".into(), title: String::new(), selected: false },
            SubRef { id: 2, lang: "English".into(), title: String::new(), selected: false },
            SubRef { id: 3, lang: "ger".into(), title: String::new(), selected: false },
        ];
        apply_subtitle_preference(&mut subs, Some("de"));
        assert_eq!(subs.iter().map(|s| s.selected).collect::<Vec<_>>(), [false, false, true]);
        // No preference at all: the first sidecar is forced on.
        apply_subtitle_preference(&mut subs, None);
        assert_eq!(subs.iter().map(|s| s.selected).collect::<Vec<_>>(), [true, false, false]);
        // No match: nothing is claimed, so the file's own order stands.
        apply_subtitle_preference(&mut subs, Some("pt-BR"));
        assert_eq!(subs.iter().map(|s| s.selected).collect::<Vec<_>>(), [false, false, false]);
    }

    /// The 639-2 spellings an addon actually uses. The old rule truncated any unknown code to two
    /// letters, which turned `spa` into `sp` (never Spanish, `es`), `jpn` into `jp`, and — worst —
    /// `kan` (Kannada) into `ka` (Georgian).
    #[test]
    fn language_codes_compare_across_the_three_spellings_addons_use() {
        for (lang, pref) in [
            ("eng", "en"), ("en", "eng"), ("en-US", "en"), ("English", "en"),
            ("deu", "de"), ("ger", "de"), ("de", "ger"), ("pt-BR", "pt"),
            ("spa", "es"), ("jpn", "ja"), ("swe", "sv"), ("bul", "bg"), ("jav", "jv"),
            ("ind", "id"), ("Indonesian", "id"), ("Bahasa Indonesia", "id"),
            ("may", "ms"), ("msa", "ms"),
            // OpenSubtitles v3 spells Brazilian Portuguese `pob` (the picker has only `pt`),
            // Latin-American Spanish `spl` (the picker has only `es`) and Tatar `tat` (`tt`).
            ("pob", "pt"), ("spl", "es"), ("tat", "tt"),
        ] {
            assert_eq!(lang_key(lang), lang_key(pref), "{lang} vs {pref}");
        }
        // A false MATCH is the dangerous direction: these must stay apart.
        for (a, b) in [("kan", "ka"), ("fil", "fi"), ("haw", "ha"), ("spa", "en"), ("jav", "ja")] {
            assert_ne!(lang_key(a), lang_key(b), "{a} must not fold onto {b}");
        }
    }

    /// An exact tag beats the folded one: a `pt-BR` preference takes the regional track, not
    /// whichever Portuguese track happens to be first.
    #[test]
    fn an_exact_language_tag_outranks_a_folded_one() {
        let mut subs = [
            SubRef { id: 1, lang: "pt".into(), title: String::new(), selected: false },
            SubRef { id: 2, lang: "pt-BR".into(), title: String::new(), selected: false },
        ];
        apply_subtitle_preference(&mut subs, Some("pt-BR"));
        assert_eq!(subs.iter().map(|s| s.selected).collect::<Vec<_>>(), [false, true]);
        apply_subtitle_preference(&mut subs, Some("pt"));
        assert_eq!(subs.iter().map(|s| s.selected).collect::<Vec<_>>(), [true, false]);
    }

    /// With the C engine loaded, a torrent with a known file index must still become the
    /// `torrent:` marker: the Node-style `/{hash}/{idx}` URL is not dialled (no Node is spawned)
    /// and produced `curlio: transport failed rc=7 — could not connect` on the television.
    #[test]
    fn an_engine_served_torrent_is_a_marker_even_with_a_file_index() {
        let s = Stream {
            info_hash: Some("0123456789abcdef0123456789abcdef01234567".into()),
            file_idx: Some(3),
            ..Default::default()
        };
        let server = Some("http://127.0.0.1:11470");
        let marker = format!("{TORRENT_SCHEME}0123456789abcdef0123456789abcdef01234567");
        assert_eq!(playable_url_with(&s, server, true).as_deref(), Some(marker.as_str()));
        let node_url = playable_url_with(&s, server, false).expect("server-backed torrent");
        assert!(node_url.starts_with("http://127.0.0.1:11470/"), "got {node_url}");
    }

    /// A debrid/HTTP stream is never a marker, engine or not.
    #[test]
    fn a_direct_http_stream_stays_direct_with_the_engine() {
        let s = Stream {
            url: Some("https://example.invalid/movie.mkv".into()),
            ..Default::default()
        };
        assert_eq!(
            playable_url_with(&s, Some("http://127.0.0.1:11470"), true).as_deref(),
            Some("https://example.invalid/movie.mkv")
        );
    }
}
