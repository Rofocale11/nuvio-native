//! What a stream is, read from its name/title/description/filename the way Nuvio's stream badges
//! do (Torrentio/AIOStreams/MediaFusion formats). CONTRACT: [`StreamInfo`] and [`classify_stream`].

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

/// `runtime_secs` lets the classifier estimate bitrate from size.
pub fn classify_stream(stream: &Stream, runtime_secs: Option<u32>) -> StreamInfo {
    let _ = (stream, runtime_secs);
    todo!()
}
