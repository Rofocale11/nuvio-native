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
    if let Some(u) = plx_stremio::torrent::playable_url(s, torrent_server) {
        return Some(u);
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
/// torrents, then resolution, then HDR, then size (bigger is higher bitrate for the same title).
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
            "displayTitle": format!("{} (SRT)", s.lang), "selected": false, "default": false,
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

    #[test]
    fn media_shape() {
        let c = Classified { stream: s("x", Some("https://h/f.mkv?t=1"), None), info: StreamInfo { height: Some(2160), hdr: Hdr::DolbyVision { with_hdr10_fallback: true }, size_bytes: Some(60_000_000_000), ..Default::default() }, url: "https://h/f.mkv?t=1".into() };
        let m = media_json(10_001, 3, &c, 7_200_000, &[SubRef { id: 9, lang: "eng".into() }]);
        assert_eq!(m["videoResolution"], "4k");
        assert_eq!(m["container"], "mkv");
        assert_eq!(m["videoCodec"], "hevc");
        assert_eq!(m["Part"][0]["id"], 10_001_003);
        assert_eq!(m["Part"][0]["key"], "/library/parts/10001003/0/file.mkv");
        assert_eq!(m["Part"][0]["Stream"][0]["DOVIProfile"], 8);
        assert_eq!(m["Part"][0]["Stream"][2]["key"], "/library/streams/9.srt");
        assert_eq!(m["bitrate"], 66_666);
    }
}
