//! What a stream is, read from its name/title/description/filename the way Nuvio's stream badges
//! do (Torrentio/AIOStreams/MediaFusion formats). CONTRACT: [`StreamInfo`] and [`classify_stream`].
//!
//! The token rules are Nuvio's (`js/core/debrid/debridStreamPresentationHelpers-0{1,2}*.js`,
//! `streamResolution.js`, `streamTextSizeParser.js`): a token matches only between non-alphanumeric
//! boundaries (`hasToken`), resolution is read per field in priority order (the parsed field wins
//! over noise like "4K remaster" in a title), sizes are binary units. Added here: the codec /
//! container / cached / seeders facts the PMS shim needs for a Media entry.

use crate::model::Stream;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hdr {
    #[default]
    Sdr,
    Hdr10,
    Hdr10Plus,
    Hlg,
    /// Dolby Vision; `with_hdr10_fallback` when the label also says HDR/HDR10 (profile 7/8 hybrids).
    DolbyVision { with_hdr10_fallback: bool },
}

impl Hdr {
    pub fn label(self) -> &'static str {
        match self {
            Hdr::Sdr => "",
            Hdr::Hdr10 => "HDR10",
            Hdr::Hdr10Plus => "HDR10+",
            Hdr::Hlg => "HLG",
            Hdr::DolbyVision { with_hdr10_fallback: true } => "DV HDR",
            Hdr::DolbyVision { with_hdr10_fallback: false } => "DV",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StreamInfo {
    /// Vertical resolution: 2160, 1440, 1080, 720, 576, 480.
    pub height: Option<u32>,
    pub hdr: Hdr,
    /// `"hevc"`, `"h264"`, `"av1"`, `"vp9"`, ...
    pub video_codec: Option<String>,
    /// `"eac3"`, `"truehd"`, `"dts"`, `"ac3"`, `"aac"`, ...
    pub audio_codec: Option<String>,
    pub atmos: bool,
    /// `"mkv"`, `"mp4"`, ...
    pub container: Option<String>,
    pub size_bytes: Option<u64>,
    /// Estimated from size and runtime when the caller supplies runtime; else None.
    pub bitrate_kbps: Option<u32>,
    pub seeders: Option<u32>,
    /// Debrid-cached / instant (`[RD+]`, `⚡`, ...).
    pub cached: bool,
    /// `"BluRay REMUX"`, `"WEB-DL"`, ...
    pub source: Option<String>,
    /// Human one-liner for the source list, e.g. `"4K DV HEVC · 58.2 GB · Torrentio RD+"`.
    pub label: String,
    /// The file name when known.
    pub filename: Option<String>,
}

impl StreamInfo {
    /// `"4K"`, `"1080p"`, … or `""`.
    pub fn resolution_label(&self) -> String {
        match self.height {
            Some(2160) => "4K".into(),
            Some(h) => format!("{h}p"),
            None => String::new(),
        }
    }

    /// A sort key: higher is better (resolution, then DV/HDR, then cached, then size).
    pub fn rank(&self) -> (u32, u8, bool, u64) {
        let hdr = match self.hdr {
            Hdr::DolbyVision { .. } => 3,
            Hdr::Hdr10Plus => 2,
            Hdr::Hdr10 | Hdr::Hlg => 1,
            Hdr::Sdr => 0,
        };
        (self.height.unwrap_or(0), hdr, self.cached, self.size_bytes.unwrap_or(0))
    }
}

// --- text normalisation -------------------------------------------------------------------------

/// Fold the decorative Unicode AIOStreams/other formatters use into plain ASCII before matching:
/// zero-width joiners and BOMs vanish (`Web\u{200d}-\u{200d}dl` → `Web-dl`), no-break spaces become
/// spaces, small capitals / super- and subscripts become their letters and digits
/// (`79.4 ᴹᵇᵖˢ` → `79.4 Mbps`, `s₀₁·ᴇ₀₁` → `s01·e01`).
pub fn fold(text: &str) -> String {
    const MAP: &[(char, char)] = &[
        ('ᴀ', 'a'), ('ʙ', 'b'), ('ᴄ', 'c'), ('ᴅ', 'd'), ('ᴇ', 'e'), ('ꜰ', 'f'), ('ɢ', 'g'), ('ʜ', 'h'),
        ('ɪ', 'i'), ('ᴊ', 'j'), ('ᴋ', 'k'), ('ʟ', 'l'), ('ᴍ', 'm'), ('ɴ', 'n'), ('ᴏ', 'o'), ('ᴘ', 'p'),
        ('ǫ', 'q'), ('ʀ', 'r'), ('ꜱ', 's'), ('ᴛ', 't'), ('ᴜ', 'u'), ('ᴠ', 'v'), ('ᴡ', 'w'), ('ʏ', 'y'),
        ('ᴢ', 'z'), ('ᴬ', 'A'), ('ᴮ', 'B'), ('ᴰ', 'D'), ('ᴱ', 'E'), ('ᴳ', 'G'), ('ᴴ', 'H'), ('ᴵ', 'I'),
        ('ᴶ', 'J'), ('ᴷ', 'K'), ('ᴸ', 'L'), ('ᴹ', 'M'), ('ᴺ', 'N'), ('ᴼ', 'O'), ('ᴾ', 'P'), ('ᴿ', 'R'),
        ('ᵀ', 'T'), ('ᵁ', 'U'), ('ⱽ', 'V'), ('ᵂ', 'W'), ('ᵃ', 'a'), ('ᵇ', 'b'), ('ᶜ', 'c'), ('ᵈ', 'd'),
        ('ᵉ', 'e'), ('ᶠ', 'f'), ('ᵍ', 'g'), ('ʰ', 'h'), ('ⁱ', 'i'), ('ʲ', 'j'), ('ᵏ', 'k'), ('ˡ', 'l'),
        ('ᵐ', 'm'), ('ⁿ', 'n'), ('ᵒ', 'o'), ('ᵖ', 'p'), ('ʳ', 'r'), ('ˢ', 's'), ('ᵗ', 't'), ('ᵘ', 'u'),
        ('ᵛ', 'v'), ('ʷ', 'w'), ('ˣ', 'x'), ('ʸ', 'y'), ('ᶻ', 'z'), ('⁰', '0'), ('¹', '1'), ('²', '2'),
        ('³', '3'), ('⁴', '4'), ('⁵', '5'), ('⁶', '6'), ('⁷', '7'), ('⁸', '8'), ('⁹', '9'), ('₀', '0'),
        ('₁', '1'), ('₂', '2'), ('₃', '3'), ('₄', '4'), ('₅', '5'), ('₆', '6'), ('₇', '7'), ('₈', '8'),
        ('₉', '9'), ('\u{a0}', ' '), ('\u{2007}', ' '), ('\u{202f}', ' '),
    ];
    text.chars()
        .filter(|c| !matches!(c, '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{fe0e}'))
        .map(|c| MAP.iter().find(|(k, _)| *k == c).map(|(_, v)| *v).unwrap_or(c))
        .collect()
}

/// An explicitly stated bitrate: `79.4 Mbps`, `25 Mb/s`, `8000 kbps`.
pub fn bitrate_from_text(text: &str) -> Option<u32> {
    let l = text.to_lowercase();
    for (unit, mult) in [("mbps", 1000.0), ("mb/s", 1000.0), ("mbit/s", 1000.0), ("kbps", 1.0), ("kb/s", 1.0)] {
        for (pos, _) in l.match_indices(unit) {
            if !boundary(l[pos + unit.len()..].chars().next()) {
                continue;
            }
            let head = l[..pos].trim_end();
            let num: String = head.chars().rev().take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',').collect::<Vec<_>>().into_iter().rev().collect();
            if let Ok(n) = num.replace(',', ".").parse::<f64>() {
                if n > 0.0 {
                    return Some((n * mult) as u32);
                }
            }
        }
    }
    None
}

// --- token matching -----------------------------------------------------------------------------

fn boundary(c: Option<char>) -> bool {
    !c.is_some_and(|c| c.is_ascii_alphanumeric())
}

/// Nuvio's `hasToken`: `token` (lowercase) occurs in `text` (lowercase) between non-alphanumeric
/// boundaries.
pub fn has_token(text: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    text.match_indices(token).any(|(pos, m)| {
        boundary(text[..pos].chars().next_back()) && boundary(text[pos + m.len()..].chars().next())
    })
}

fn any_token(text: &str, tokens: &[&str]) -> bool {
    tokens.iter().any(|t| has_token(text, t))
}

/// Nuvio's `resolutionFromText`, as a height.
pub fn resolution_from_text(text: &str) -> Option<u32> {
    let t = text.to_lowercase();
    if any_token(&t, &["2160p", "2160", "4k", "uhd"]) {
        return Some(2160);
    }
    if any_token(&t, &["1440p", "1440", "2k"]) {
        return Some(1440);
    }
    if any_token(&t, &["1080p", "1080", "fhd", "1080i"]) {
        return Some(1080);
    }
    if any_token(&t, &["720p", "720", "hd"]) {
        return Some(720);
    }
    if any_token(&t, &["576p", "576"]) {
        return Some(576);
    }
    if any_token(&t, &["480p", "480", "sd"]) {
        return Some(480);
    }
    if any_token(&t, &["360p", "360"]) {
        return Some(360);
    }
    // Any other `<n>p` token (`406p`, `540p`) within plausible video heights.
    t.split(|c: char| !c.is_ascii_alphanumeric())
        .filter_map(|w| w.strip_suffix('p')?.parse::<u32>().ok())
        .find(|h| (240..=4320).contains(h))
}

/// Nuvio's `qualityFromText`, as its display label.
pub fn source_from_text(text: &str) -> Option<&'static str> {
    let v = text.to_lowercase();
    if v.contains("remux") {
        return Some("BluRay REMUX");
    }
    if v.contains("blu-ray") || v.contains("bluray") || v.contains("bdrip") || v.contains("brrip") {
        return Some("BluRay");
    }
    if v.contains("web-dl") || v.contains("webdl") {
        return Some("WEB-DL");
    }
    if v.contains("webrip") || v.contains("web-rip") {
        return Some("WEBRip");
    }
    if v.contains("hdrip") {
        return Some("HDRip");
    }
    if v.contains("hd-rip") || v.contains("hcrip") {
        return Some("HC HD-Rip");
    }
    if v.contains("dvdrip") {
        return Some("DVDRip");
    }
    if v.contains("hdtv") {
        return Some("HDTV");
    }
    for (tok, label) in [("cam", "CAM"), ("camrip", "CAM"), ("hdcam", "CAM"), ("telesync", "TS"), ("hdts", "TS"), ("tc", "TC"), ("scr", "SCR")] {
        if has_token(&v, tok) {
            return Some(label);
        }
    }
    None
}

fn hdr_from_text(t: &str) -> Hdr {
    let dv = any_token(t, &["dv", "dovi", "dolby vision", "dolbyvision", "dolby.vision", "dolby-vision", "dolby_vision"]);
    let hdr10p = t.contains("hdr10+") || t.contains("hdr10plus") || t.contains("hdr10 plus");
    let hdr = hdr10p || any_token(t, &["hdr", "hdr10", "hlg"]);
    if dv {
        return Hdr::DolbyVision { with_hdr10_fallback: hdr };
    }
    if hdr10p {
        return Hdr::Hdr10Plus;
    }
    if has_token(t, "hlg") && !any_token(t, &["hdr", "hdr10"]) {
        return Hdr::Hlg;
    }
    if hdr {
        return Hdr::Hdr10;
    }
    Hdr::Sdr
}

fn video_codec_from_text(t: &str) -> Option<&'static str> {
    if has_token(t, "av1") {
        return Some("av1");
    }
    if any_token(t, &["hevc", "h265", "x265", "h.265", "h 265"]) {
        return Some("hevc");
    }
    if any_token(t, &["avc", "h264", "x264", "h.264", "h 264"]) {
        return Some("h264");
    }
    if has_token(t, "vp9") {
        return Some("vp9");
    }
    if any_token(t, &["xvid", "divx"]) {
        return Some("mpeg4");
    }
    None
}

fn audio_from_text(t: &str) -> (Option<&'static str>, bool) {
    let atmos = has_token(t, "atmos");
    let codec = if t.contains("truehd") || t.contains("true hd") || t.contains("true-hd") {
        Some("truehd")
    } else if t.contains("dts:x") || t.contains("dtsx") || t.contains("dts-x") || t.contains("dts-hd") || t.contains("dtshd") || has_token(t, "dts") {
        Some("dts")
    } else if t.contains("dd+") || t.contains("ddp") || t.contains("eac3") || t.contains("e-ac-3") || t.contains("e-ac3") || t.contains("dolby digital plus") {
        Some("eac3")
    } else if has_token(t, "dd") || has_token(t, "dd5.1") || t.contains("ac3") || t.contains("dolby digital") {
        Some("ac3")
    } else if has_token(t, "flac") {
        Some("flac")
    } else if has_token(t, "opus") {
        Some("opus")
    } else if has_token(t, "aac") || t.contains("aac2.0") || t.contains("aac5.1") {
        Some("aac")
    } else if has_token(t, "mp3") {
        Some("mp3")
    } else {
        None
    };
    // Atmos rides on TrueHD or E-AC-3; without a stated codec assume E-AC-3 (streaming Atmos).
    (codec.or(if atmos { Some("eac3") } else { None }), atmos)
}

const CONTAINERS: &[&str] = &["mkv", "mp4", "m4v", "avi", "webm", "mov", "ts", "m2ts", "mpg", "mpeg", "wmv", "flv", "m3u8"];

fn container_of_name(name: &str) -> Option<&'static str> {
    let base = name.split(['?', '#']).next().unwrap_or(name);
    let ext = base.rsplit_once('.')?.1.to_ascii_lowercase();
    CONTAINERS.iter().copied().find(|c| *c == ext).map(|c| if c == "m4v" { "mp4" } else { c })
}

/// Nuvio's `sizeBytesFromText`: the first `<number> <TB|GB|MB|KB>` (binary units, `,` decimal ok).
pub fn size_from_text(text: &str) -> Option<u64> {
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() || (i > 0 && (b[i - 1].is_ascii_digit() || b[i - 1] == b'.' || b[i - 1] == b',')) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i + 1 < b.len() && (b[i] == b'.' || b[i] == b',') && b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        let number = text[start..i].replace(',', ".");
        let mut j = i;
        while j < b.len() && b[j] == b' ' {
            j += 1;
        }
        if j + 2 <= b.len() && b[j].is_ascii_alphabetic() && b[j + 1].is_ascii_alphabetic() {
            let unit = text[j..j + 2].to_ascii_uppercase();
            let after = b.get(j + 2).copied();
            let word_end = !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_');
            let mult: Option<u64> = match unit.as_str() {
                "TB" => Some(1 << 40),
                "GB" => Some(1 << 30),
                "MB" => Some(1 << 20),
                "KB" => Some(1 << 10),
                _ => None,
            };
            if let (Some(m), true, Ok(n)) = (mult, word_end, number.parse::<f64>()) {
                let bytes = (n * m as f64) as u64;
                if bytes > 0 {
                    return Some(bytes);
                }
            }
        }
    }
    None
}

/// Seeders/peers: Torrentio `👤 12`, AIOStreams/MediaFusion `👥 12` / `👤 12`, or `seeders: 12`.
fn seeders_from_text(text: &str) -> Option<u32> {
    for marker in ["👤", "👥", "seeders:", "seeders", "seeds:", "seeds"] {
        let lower = text.to_lowercase();
        if let Some(pos) = lower.find(marker) {
            let rest = lower[pos + marker.len()..].trim_start_matches([' ', ':', '\u{fe0f}']);
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = digits.parse() {
                return Some(n);
            }
        }
    }
    None
}

/// Debrid-cached markers: `[RD+]`, `[AD+]`, `RD⚡`, `⚡`, `instant`, `cached`.
fn cached_from_text(name: &str, all: &str) -> bool {
    if all.contains('⚡') {
        return true;
    }
    // `[XX+]` service tags (Torrentio).
    let b = name.as_bytes();
    for (i, _) in name.match_indices('[') {
        let tail = &b[i + 1..];
        let code_len = tail.iter().take_while(|c| c.is_ascii_alphabetic()).count();
        if (2..=3).contains(&code_len) && tail.get(code_len) == Some(&b'+') && tail.get(code_len + 1) == Some(&b']') {
            return true;
        }
    }
    let l = all.to_lowercase();
    (has_token(&l, "cached") && !l.contains("uncached") && !l.contains("not cached")) || has_token(&l, "instant")
}

/// The debrid service code shown in Torrentio-style names (`RD`, `AD`, `PM`, `TB`, `DL`, `ED`, `OC`, `PK`).
fn debrid_tag(name: &str) -> Option<String> {
    for svc in ["RD", "AD", "PM", "TB", "DL", "ED", "OC", "PK", "EN"] {
        for pat in [format!("[{svc}+]"), format!("[{svc}⚡]"), format!("[{svc} ⚡]"), format!("{svc}⚡"), format!("[{svc}]")] {
            if name.contains(&pat) {
                return Some(svc.to_string());
            }
        }
    }
    None
}

/// The file or release name in a Torrentio/MediaFusion/Comet text block: a line ending in a
/// container extension (Torrentio puts the episode file of a season pack on line 2), else a first
/// line that is a spaceless dotted release name (`Movie.2019.2160p.BluRay.x265-GRP`).
fn first_line_filename(s: &str) -> Option<String> {
    let clean = |l: &str| l.trim().trim_start_matches(|c: char| !c.is_ascii_alphanumeric() && c != '[').trim().to_string();
    if let Some(l) = s.lines().map(clean).find(|l| !l.is_empty() && container_of_name(l).is_some()) {
        return Some(l);
    }
    let first = clean(s.lines().next()?);
    (!first.is_empty() && !first.contains(' ') && first.matches('.').count() >= 3).then_some(first)
}

fn format_size(bytes: u64) -> String {
    let gb = bytes as f64 / (1u64 << 30) as f64;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / (1u64 << 20) as f64)
    }
}

/// `runtime_secs` lets the classifier estimate bitrate from size.
pub fn classify_stream(stream: &Stream, runtime_secs: Option<u32>) -> StreamInfo {
    let name = fold(stream.name.as_deref().unwrap_or_default());
    let title = fold(stream.title.as_deref().unwrap_or_default());
    let desc = fold(stream.description.as_deref().unwrap_or_default());
    let hint_name = stream.behavior_hints.filename.clone().unwrap_or_default();
    let filename = stream
        .behavior_hints
        .filename
        .clone()
        .filter(|f| !f.trim().is_empty())
        .or_else(|| first_line_filename(&title))
        .or_else(|| first_line_filename(&desc));
    let fname = filename.clone().unwrap_or_default();
    let all = [name.as_str(), title.as_str(), desc.as_str(), hint_name.as_str()].join(" \n ");
    let lower = all.to_lowercase();

    // Resolution: Android/Nuvio field priority — the addon's own quality label (name), then the
    // file name, then the free text.
    let height = [name.as_str(), fname.as_str(), title.as_str(), desc.as_str()]
        .iter()
        .filter(|f| !f.is_empty())
        .find_map(|f| resolution_from_text(f));

    let hdr = hdr_from_text(&lower);
    let video_codec = video_codec_from_text(&lower).map(str::to_string);
    let (audio, atmos) = audio_from_text(&lower);
    let container = container_of_name(&fname)
        .or_else(|| stream.url.as_deref().and_then(container_of_name))
        .map(str::to_string);
    let size_bytes = stream
        .behavior_hints
        .video_size
        .filter(|s| *s > 0)
        .or_else(|| size_from_text(&desc))
        .or_else(|| size_from_text(&title))
        .or_else(|| size_from_text(&name));
    let bitrate_kbps = bitrate_from_text(&all).or(match (size_bytes, runtime_secs) {
        (Some(b), Some(r)) if r > 0 => u32::try_from(b.saturating_mul(8) / u64::from(r) / 1000).ok(),
        _ => None,
    });
    let seeders = seeders_from_text(&all);
    let cached = cached_from_text(&name, &all);
    let source = source_from_text(&lower).map(str::to_string);

    let mut info = StreamInfo {
        height,
        hdr,
        video_codec,
        audio_codec: audio.map(str::to_string),
        atmos,
        container,
        size_bytes,
        bitrate_kbps,
        seeders,
        cached,
        source,
        label: String::new(),
        filename,
    };
    info.label = build_label(&info, stream);
    info
}

fn build_label(info: &StreamInfo, stream: &Stream) -> String {
    let mut head: Vec<String> = Vec::new();
    let res = info.resolution_label();
    if !res.is_empty() {
        head.push(res);
    }
    if !info.hdr.label().is_empty() {
        head.push(info.hdr.label().to_string());
    }
    if let Some(c) = &info.video_codec {
        head.push(match c.as_str() {
            "hevc" => "HEVC".into(),
            "h264" => "AVC".into(),
            other => other.to_uppercase(),
        });
    }
    if let Some(s) = &info.source {
        head.push(s.clone());
    }
    if info.atmos {
        head.push("Atmos".into());
    } else if let Some(a) = &info.audio_codec {
        head.push(match a.as_str() {
            "truehd" => "TrueHD".into(),
            "eac3" => "DD+".into(),
            "ac3" => "DD".into(),
            other => other.to_uppercase(),
        });
    }
    let mut parts: Vec<String> = Vec::new();
    if !head.is_empty() {
        parts.push(head.join(" "));
    }
    if let Some(b) = info.size_bytes {
        parts.push(format_size(b));
    }
    if let Some(s) = info.seeders {
        if stream.info_hash.is_some() && stream.url.is_none() {
            parts.push(format!("{s} peers"));
        }
    }
    let mut origin = if stream.addon_name.is_empty() {
        stream.name.as_deref().and_then(|n| n.lines().next()).map(|l| l.trim().to_string()).unwrap_or_default()
    } else {
        stream.addon_name.clone()
    };
    let all = fold(&[stream.name.as_deref().unwrap_or(""), stream.description.as_deref().unwrap_or(""), stream.title.as_deref().unwrap_or("")].join("\n"));
    if let Some(tag) = debrid_tag(&all) {
        origin = format!("{origin} {tag}{}", if info.cached { "+" } else { "" }).trim().to_string();
    } else if info.cached {
        origin = format!("{origin} ⚡").trim().to_string();
    }
    if !origin.is_empty() {
        parts.push(origin);
    }
    if parts.is_empty() {
        return stream.name.clone().unwrap_or_else(|| "Stream".into()).replace('\n', " ");
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Stream, StreamBehaviorHints};

    fn s(name: &str, title: &str, desc: &str) -> Stream {
        Stream {
            name: Some(name.into()).filter(|x: &String| !x.is_empty()),
            title: Some(title.into()).filter(|x: &String| !x.is_empty()),
            description: Some(desc.into()).filter(|x: &String| !x.is_empty()),
            ..Default::default()
        }
    }

    #[test]
    fn tokens_respect_boundaries() {
        assert!(has_token("movie.2160p.hdr.mkv", "hdr"));
        assert!(!has_token("hdrip", "hdr"));
        assert!(has_token("dd+ 5.1", "dd"));
        assert!(!has_token("addon", "dd"));
        assert!(has_token("a 7.1 b", "7.1"));
        assert!(!has_token("dvdrip", "dv"));
    }

    #[test]
    fn torrentio_debrid() {
        let mut st = s(
            "[RD+] Torrentio\n4k DV | HDR",
            "The.Shawshank.Redemption.1994.2160p.UHD.BluRay.x265.10bit.HDR.DV.TrueHD.7.1.Atmos-FGT\n👤 152 💾 58.2 GB ⚙️ RARBG",
            "",
        );
        st.addon_name = "Torrentio".into();
        st.url = Some("https://torrentio.strem.fun/resolve/realdebrid/x/abc/null/0/The.Shawshank.Redemption.1994.2160p.mkv".into());
        let i = classify_stream(&st, Some(142 * 60));
        assert_eq!(i.height, Some(2160));
        assert_eq!(i.hdr, Hdr::DolbyVision { with_hdr10_fallback: true });
        assert_eq!(i.video_codec.as_deref(), Some("hevc"));
        assert_eq!(i.audio_codec.as_deref(), Some("truehd"));
        assert!(i.atmos);
        assert_eq!(i.container.as_deref(), Some("mkv"));
        assert_eq!(i.size_bytes, Some((58.2 * (1u64 << 30) as f64) as u64));
        assert_eq!(i.seeders, Some(152));
        assert!(i.cached);
        assert_eq!(i.source.as_deref(), Some("BluRay"));
        assert!(i.bitrate_kbps.unwrap() > 50_000 && i.bitrate_kbps.unwrap() < 65_000, "{:?}", i.bitrate_kbps);
        assert_eq!(i.filename.as_deref(), Some("The.Shawshank.Redemption.1994.2160p.UHD.BluRay.x265.10bit.HDR.DV.TrueHD.7.1.Atmos-FGT"));
        assert_eq!(i.label, "4K DV HDR HEVC BluRay Atmos · 58.2 GB · Torrentio RD+");
    }

    #[test]
    fn torrentio_p2p_1080() {
        let mut st = s("Torrentio\n1080p", "Game.of.Thrones.S01E01.1080p.BluRay.x264-REWARD.mkv\n👤 45 💾 1.81 GB ⚙️ ThePirateBay", "");
        st.info_hash = Some("0123456789abcdef0123456789abcdef01234567".into());
        st.addon_name = "Torrentio".into();
        let i = classify_stream(&st, None);
        assert_eq!(i.height, Some(1080));
        assert_eq!(i.hdr, Hdr::Sdr);
        assert_eq!(i.video_codec.as_deref(), Some("h264"));
        assert!(!i.cached);
        assert_eq!(i.container.as_deref(), Some("mkv"));
        assert_eq!(i.label, "1080p AVC BluRay · 1.8 GB · 45 peers · Torrentio");
    }

    #[test]
    fn aiostreams() {
        let mut st = s(
            "[RD⚡] AIOStreams 4K",
            "",
            "🎥 BluRay REMUX 🎞️ HEVC\n📺 DV HDR10\n🎧 Atmos | TrueHD\n📦 61.3 GB 🔍 Torrentio\n📁 Dune.Part.Two.2024.2160p.BluRay.REMUX.mkv",
        );
        st.addon_name = "AIOStreams".into();
        let i = classify_stream(&st, None);
        assert_eq!(i.height, Some(2160));
        assert_eq!(i.hdr, Hdr::DolbyVision { with_hdr10_fallback: true });
        assert_eq!(i.source.as_deref(), Some("BluRay REMUX"));
        assert!(i.cached);
        assert!(i.label.starts_with("4K DV HDR HEVC BluRay REMUX Atmos · 61.3 GB · AIOStreams RD"), "{}", i.label);
    }

    #[test]
    fn mediafusion() {
        let st = s(
            "MediaFusion | RD ⚡️ 4K",
            "",
            "📂 Oppenheimer.2023.2160p.WEB-DL.DDP5.1.Atmos.HDR10+.H.265-FLUX.mkv\n💾 22.4 GB 👤 88\n🔗 Torrentio",
        );
        let i = classify_stream(&st, None);
        assert_eq!(i.height, Some(2160));
        assert_eq!(i.hdr, Hdr::Hdr10Plus);
        assert_eq!(i.video_codec.as_deref(), Some("hevc"));
        assert_eq!(i.audio_codec.as_deref(), Some("eac3"));
        assert!(i.atmos && i.cached);
        assert_eq!(i.source.as_deref(), Some("WEB-DL"));
        assert_eq!(i.seeders, Some(88));
        assert_eq!(i.size_bytes, Some((22.4 * (1u64 << 30) as f64) as u64));
    }

    #[test]
    fn comet() {
        let st = s("[TB⚡] Comet 1080p", "", "Severance.S02E01.1080p.ATVP.WEB-DL.DDP5.1.H.264-NTb.mkv\n💿 WEB-DL|AVC\n💾 3.2 GB 🔎 Torrentio");
        let i = classify_stream(&st, None);
        assert_eq!(i.height, Some(1080));
        assert_eq!(i.video_codec.as_deref(), Some("h264"));
        assert_eq!(i.audio_codec.as_deref(), Some("eac3"));
        assert!(i.cached);
        assert!(i.label.contains("TB"));
    }

    #[test]
    fn behavior_hints_win_and_field_priority() {
        let st = Stream {
            name: Some("Provider\n1080p".into()),
            title: Some("Some.Movie.4K.Remaster.Collection".into()),
            behavior_hints: StreamBehaviorHints { video_size: Some(1234), filename: Some("x.mp4".into()), ..Default::default() },
            ..Default::default()
        };
        let i = classify_stream(&st, None);
        assert_eq!(i.height, Some(1080));
        assert_eq!(i.size_bytes, Some(1234));
        assert_eq!(i.container.as_deref(), Some("mp4"));
    }

    #[test]
    fn sizes() {
        assert_eq!(size_from_text("👤 12 💾 1.81 GB ⚙️ TPB"), Some((1.81 * (1u64 << 30) as f64) as u64));
        assert_eq!(size_from_text("size 700MB"), Some(700 << 20));
        assert_eq!(size_from_text("1,5 GB"), Some((1.5 * (1u64 << 30) as f64) as u64));
        assert_eq!(size_from_text("no size here 1080p"), None);
        assert_eq!(size_from_text("2 GBP"), None);
    }

    #[test]
    fn aiostreams_custom_template() {
        // The shape of a user's custom AIOStreams formatter: decorative Unicode throughout.
        let mut st = s(
            "\u{a0}\u{a0}4K\u{a0}\u{200d}\u{200d}⚡\u{200d}\n\u{a0}〈Web\u{200d}-\u{200d}dl〉\u{200d}\n\u{a0}★★★★☆",
            "",
            "✎  Some Show  s₀₁·ᴇ₀₁\n▣  HEVC ✦ DV · HDR10+\n♬  Atmos · DD+  ♯ 5.1\n◈ 27.2 / 54.3 GB · 25 ᴹᵇᵖˢ · 1d\n⛉ [TB] ComeTorz · GRP\n✓ ᴇɴ · sᴜʙ (ᴇɴ)",
        );
        st.addon_name = "SELection".into();
        st.behavior_hints.video_size = Some(29_205_777_000);
        let i = classify_stream(&st, None);
        assert_eq!(i.height, Some(2160));
        assert_eq!(i.hdr, Hdr::DolbyVision { with_hdr10_fallback: true });
        assert_eq!(i.video_codec.as_deref(), Some("hevc"));
        assert_eq!(i.audio_codec.as_deref(), Some("eac3"));
        assert!(i.atmos && i.cached);
        assert_eq!(i.source.as_deref(), Some("WEB-DL"));
        assert_eq!(i.bitrate_kbps, Some(25_000));
        assert_eq!(i.size_bytes, Some(29_205_777_000));
        assert_eq!(i.label, "4K DV HDR HEVC WEB-DL Atmos · 27.2 GB · SELection TB+");
        assert_eq!(fold("79.4 ᴹᵇᵖˢ"), "79.4 Mbps");
    }

    #[test]
    fn season_pack_file_line_and_odd_heights() {
        let st = s("Torrentio\n406p", "Game Of Thrones Complete Season 1 406p mkv + Subs\nGame Of Thrones S01E01 Winter Is Coming.mkv\n👤 7 💾 341.45 MB", "");
        let i = classify_stream(&st, None);
        assert_eq!(i.height, Some(406));
        assert_eq!(i.filename.as_deref(), Some("Game Of Thrones S01E01 Winter Is Coming.mkv"));
        assert_eq!(i.container.as_deref(), Some("mkv"));
        assert_eq!(resolution_from_text("x 5000p"), None);
    }

    #[test]
    fn plain_direct_link() {
        let st = Stream { url: Some("https://cdn.example/v.m3u8".into()), name: Some("Direct".into()), ..Default::default() };
        let i = classify_stream(&st, None);
        assert_eq!(i.height, None);
        assert_eq!(i.container.as_deref(), Some("m3u8"));
        assert_eq!(i.label, "Direct");
    }
}
