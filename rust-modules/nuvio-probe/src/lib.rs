//! **What a third-party stream actually is** — NuvioNative's answer to a server that cannot tell us.
//!
//! A Plex server reads every part with its own scanner and reports codecs, tracks and Dolby Vision
//! configuration; the route and the Load payload trust those fields. A Stremio stream comes with
//! nothing but a label (`"4K DV HDR10 … DTS-HD"`) that may be wrong, so the PMS shim asks this crate
//! to OPEN the selected stream with the bundled libavformat — over the same libcurl byte-range
//! transport the demuxer uses, redirects, TLS fallback and all — and reports the real container,
//! video codec/raster/HDR signalling/Dolby Vision record, and every audio and subtitle track in
//! file order. The shim turns that into PMS `Media`/`Stream` fields, so every decision downstream
//! (direct play, the Load codecs, audio track choice, the DV presentation, the embedded-subtitle
//! menu) is made on what the file IS.
//!
//! Cost: one HTTP open plus libavformat's probe window (`find_stream_info`, a few MB at most for
//! Matroska; an MP4 with its index at the end costs one ranged seek). Bounded by `deadline`.
//!
//! # why this is its own crate
//!
//! This used to be `plx_media::ff::probe` (`media/src/ff_probe.rs`), reached through `super::*`.
//! That made it borrow upstream's `ff.rs` names and carry a SECOND `dynlib!` table with its own
//! filename candidates — the two tables disagreed in the Linux simulator, so media reported FFmpeg
//! bound while the probe refused every stream. It now speaks to `plx_media_codecs::ffi` directly:
//! one ABI table, one set of candidate names, and no edit to an upstream-owned file on merge.
//!
//! Callable from any thread once `plx_media_codecs::ffi::boot()` has run; it self-boots as a
//! fallback for standalone use, and refuses (`Err`) when the bundled FFmpeg is absent or not the
//! ABI this build expects — exactly as the demuxer does.

use plx_media_codecs::ffi::*;
use std::os::raw::{c_char, c_int, c_void};

/// `AVStream.disposition` and `AVStream.avg_frame_rate` — derived from FFmpeg 9's AVStream layout
/// beside the offsets in `plx_media_codecs::ffi` (`ci/ffabi-assert.c` holds all of them, including
/// these two, against the very headers the bundled libraries were built from). They stay local
/// because upstream reads neither field: adding them to the shared table would be a fork edit in
/// an upstream-owned file, which is the whole cost this crate exists to avoid.
#[cfg(target_pointer_width = "32")]
const OFF_STREAM_DISPOSITION: usize = 56;
#[cfg(target_pointer_width = "64")]
const OFF_STREAM_DISPOSITION: usize = 64;
#[cfg(target_pointer_width = "32")]
const OFF_STREAM_AVG_FRAME_RATE: usize = 76;
#[cfg(target_pointer_width = "64")]
const OFF_STREAM_AVG_FRAME_RATE: usize = 88;
const AV_DISPOSITION_DEFAULT: c_int = 1 << 0;
const AV_DISPOSITION_FORCED: c_int = 1 << 6;
/// AVSTREAM attached picture (cover art) is a VIDEO stream; skip it.
const AV_DISPOSITION_ATTACHED_PIC: c_int = 1 << 10;

// The seek constants FFmpeg's AVIO callbacks use. `libc` already defines the SEEK_* trio; only
// AVSEEK_SIZE is FFmpeg's own. Kept local for the same reason as the two offsets above.
const AVSEEK_SIZE: c_int = 0x10000;

// Fork Rofocale11: a container chapter is the only credits signal that can come from the media
// itself (addons and debrid links carry no segment metadata). These offsets are derived from the
// bundled FFmpeg 9.1 headers — host `nb_chapters` 72 / `chapters` 80, ARM 40 / 44 (the 32-bit
// numbers follow from AAPCS and are cross-checked against the known start_time 56 / duration 64).
// Kept local for the same reason as the stream offsets above: no edit to upstream's ABI table.
#[cfg(target_pointer_width = "32")]
const OFF_FMT_NB_CHAPTERS: usize = 40;
#[cfg(target_pointer_width = "32")]
const OFF_FMT_CHAPTERS: usize = 44;
#[cfg(target_pointer_width = "64")]
const OFF_FMT_NB_CHAPTERS: usize = 72;
#[cfg(target_pointer_width = "64")]
const OFF_FMT_CHAPTERS: usize = 80;

/// FFmpeg 9's `AVChapter`. Both ABIs place every field at the same offset (all are 8-byte
/// aligned), so one mirror serves the TV and the simulator.
#[repr(C)]
struct AvChapter {
    id: i64,
    time_base: AVRational,
    start: i64,
    end: i64,
    metadata: *mut AVDictionary,
}

/// the media layer's `curlio::PlexAuthorization` bound to the shared byte-range
/// source. Duplicated here rather than imported from `plx_media` because that would make this
/// low-level crate depend on the orchestration layer above it (and invert `ci/module-layers.ini`).
/// The policy call is identical: a request that carries a credential reaches the wire only when
/// the Plex grant says its origin may carry one — including every redirect hop of an addon URL.
struct ProbeAuthorization;
impl plx_net::range_io::UrlAuthorizer for ProbeAuthorization {
    fn allows(url: &str) -> bool {
        let (origin, path) = plx_net::net::origin::split(url);
        plx_plex::http::credential_transport_allowed(&origin, path, &[])
    }
}
type ProbeSource = plx_net::range_io::CurlSource<ProbeAuthorization>;

/// One probed video track.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbedVideo {
    /// libavcodec's name: `hevc`, `h264`, `av1`, `vp9`, `mpeg2video`, …
    pub codec: String,
    pub width: i32,
    pub height: i32,
    /// Frames per second (average), 0.0 when the container does not say.
    pub fps: f64,
    /// 8, 10 or 12; 0 when unknown.
    pub bit_depth: i32,
    /// libavcodec profile number (HEVC Main 10 = 2).
    pub profile: i32,
    /// AVCOL_* raw values: trc 16 = PQ (HDR10), 18 = HLG; primaries 9 = BT.2020; space 9 = BT.2020nc.
    pub color_trc: i32,
    pub color_primaries: i32,
    pub color_space: i32,
    /// The `dvcC`/`dvvC` configuration record, when the file carries one.
    pub dovi: Option<ProbedDovi>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbedDovi {
    pub profile: u8,
    pub level: u8,
    pub bl_compat: u8,
    pub rpu: bool,
    pub el: bool,
    pub bl: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbedAudio {
    /// `eac3`, `ac3`, `aac`, `truehd`, `dts`, `flac`, `opus`, …
    pub codec: String,
    /// libavcodec profile number: DTS-HD MA = 60, E-AC-3/TrueHD Atmos = 30.
    pub profile: i32,
    pub channels: i32,
    pub sample_rate: i32,
    /// ISO 639-2 from the container (`eng`), empty when absent.
    pub language: String,
    pub title: String,
    pub default: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbedSubtitle {
    /// `subrip`, `ass`, `hdmv_pgs_subtitle`, `dvd_subtitle`, `mov_text`, `webvtt`, …
    pub codec: String,
    pub language: String,
    pub title: String,
    pub default: bool,
    pub forced: bool,
}

/// One container chapter, in file order. `title` is the atom's `title` tag when it has one.
/// Fork Rofocale11: read from the file we are already probing, so a release with named chapters
/// can tell us where its credits start without a network dependency or a server we do not have.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbedChapter {
    pub start_ms: i64,
    pub end_ms: i64,
    pub title: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaProbe {
    /// libavformat's short name list, e.g. `matroska,webm` or `mov,mp4,m4a,3gp,3g2,mj2`.
    pub format: String,
    pub duration_ms: i64,
    /// Content-Length of the effective URL, -1 when the server did not say.
    pub size: i64,
    pub video: Option<ProbedVideo>,
    pub audio: Vec<ProbedAudio>,
    pub subtitles: Vec<ProbedSubtitle>,
    /// Fork Rofocale11: container chapters, including a named "Credits"/"End Credits" atom when
    /// the release author put one there. Empty for the common torrent/debrid rip.
    pub chapters: Vec<ProbedChapter>,
}

impl MediaProbe {
    /// The PMS `container` spelling: `mkv`, `mp4`, `mpegts`, `avi`, `webm`, …
    pub fn container(&self) -> &'static str {
        let f = self.format.as_str();
        if f.starts_with("matroska") {
            "mkv"
        } else if f.starts_with("mov") {
            "mp4"
        } else if f == "mpegts" {
            "mpegts"
        } else if f == "avi" {
            "avi"
        } else if f == "flv" {
            "flv"
        } else {
            "unknown"
        }
    }
}

/// The probe's AVIO state: one curl source, the logical offset and the deadline.
struct ProbeIo {
    src: Box<ProbeSource>,
    off: i64,
    deadline: std::time::Instant,
}

extern "C" fn probe_read(op: *mut c_void, dst: *mut u8, n: c_int) -> c_int {
    unsafe {
        let s = &mut *(op as *mut ProbeIo);
        if std::time::Instant::now() >= s.deadline {
            return AVERROR_EOF;
        }
        let buf = std::slice::from_raw_parts_mut(dst, n.max(0) as usize);
        let r = s.src.read_until(
            buf,
            Some(s.deadline),
            &mut plx_base::checkpoint::NoCheckpoint,
        );
        if r > 0 {
            s.off += r as i64;
            r
        } else {
            AVERROR_EOF
        }
    }
}

extern "C" fn probe_seek(op: *mut c_void, offset: i64, whence: c_int) -> i64 {
    unsafe {
        let s = &mut *(op as *mut ProbeIo);
        let size = s.src.size();
        if whence == AVSEEK_SIZE {
            return size;
        }
        let target = match whence & !0x20000 {
            // AVSEEK_FORCE is a hint we do not need.
            libc::SEEK_SET => offset,
            libc::SEEK_CUR => s.off + offset,
            libc::SEEK_END if size >= 0 => size + offset,
            _ => return -1,
        };
        if target < 0 || std::time::Instant::now() >= s.deadline {
            return -1;
        }
        if target == s.off {
            return target;
        }
        if s.src.seek(target) {
            s.off = target;
            target
        } else {
            -1
        }
    }
}

unsafe fn dict_str(dict: *const AVDictionary, key: &std::ffi::CStr) -> String {
    if dict.is_null() {
        return String::new();
    }
    let e = av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0);
    if e.is_null() || (*e).value.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr((*e).value)
        .to_string_lossy()
        .trim()
        .to_string()
}

unsafe fn dict_value(s: *mut AVStream, key: &std::ffi::CStr) -> String {
    let dict = *((s as *const u8).add(OFF_STREAM_METADATA) as *const *const AVDictionary);
    dict_str(dict, key)
}

/// A chapter's timestamp is in its own time base, not AV_TIME_BASE. Convert to milliseconds with
/// saturating arithmetic: a malformed atom must not wrap into a plausible-looking chapter.
fn chapter_ms(value: i64, tb: AVRational) -> i64 {
    if tb.num <= 0 || tb.den <= 0 {
        return -1;
    }
    value
        .saturating_mul(tb.num as i64)
        .saturating_mul(1000)
        .saturating_div(tb.den as i64)
}

/// The container's own name for this track — `title` (Matroska and most formats), else `name`
/// (MP4's per-track `udta` name box, which is what FFmpeg's mov demuxer exposes it as).
///
/// Deliberately NOT `handler_name`: MP4 sets it to a constant per media type, so it is the same
/// string on every track and reading it would give the picker nine identical sub-lines.
unsafe fn stream_name(s: *mut AVStream) -> String {
    let dict = *((s as *const u8).add(OFF_STREAM_METADATA) as *const *const AVDictionary);
    if dict.is_null() {
        return String::new();
    }
    for key in [c"title", c"name"] {
        let e = av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0);
        if e.is_null() || (*e).value.is_null() {
            continue;
        }
        let v = std::ffi::CStr::from_ptr((*e).value)
            .to_string_lossy()
            .trim()
            .to_string();
        if !v.is_empty() {
            return v;
        }
    }
    String::new()
}

unsafe fn codec_name(id: c_int) -> String {
    std::ffi::CStr::from_ptr(avcodec_get_name(id))
        .to_string_lossy()
        .into_owned()
}

/// The Dolby Vision configuration record attached to this stream, if the demuxer found one.
/// `coded_side_data` is STREAM-level (not per-packet) and both bundled demuxers populate it: `mov`
/// from the `dvcC`/`dvvC` box, matroska from the `DolbyVisionConfiguration` block. Reading it needs
/// no frame decode. Nine plain bytes, so this is a zero-risk read on any target.
unsafe fn dovi_conf(cp: *const AVCodecParameters) -> Option<AVDOVIDecoderConfigurationRecord> {
    let (list, n) = ((*cp).coded_side_data, (*cp).nb_coded_side_data);
    if list.is_null() || n <= 0 {
        return None;
    }
    for i in 0..n as usize {
        let sd = &*list.add(i);
        if sd.type_ != AV_PKT_DATA_DOVI_CONF || sd.data.is_null() {
            continue;
        }
        let bytes = std::slice::from_raw_parts(sd.data, sd.size);
        if bytes.len() < 9 {
            return None;
        }
        return Some(AVDOVIDecoderConfigurationRecord {
            dv_version_major: bytes[0],
            dv_version_minor: bytes[1],
            dv_profile: bytes[2],
            dv_level: bytes[3],
            rpu_present_flag: bytes[4],
            el_present_flag: bytes[5],
            bl_present_flag: bytes[6],
            dv_bl_signal_compatibility_id: bytes[7],
            dv_md_compression: bytes[8],
        });
    }
    None
}

/// Open `url` and report what it holds. `Err` carries a short reason for the log (never the URL,
/// whose query may be a signature).
pub fn probe_url(url: &str, deadline: std::time::Duration) -> Result<MediaProbe, String> {
    // One ABI table: this is the same `plx_media_codecs::ffi` the demuxer uses, so there is no
    // second `dynlib!` to drift out of sync and no way for media to report "bound" while the probe
    // refuses. The app boots it at startup; self-boot here only covers standalone use/test.
    if !abi_ok() {
        plx_media_codecs::ffi::boot();
        if !abi_ok() {
            return Err("bundled FFmpeg unavailable or the wrong ABI".into());
        }
    }
    ensure_registered();
    let until = std::time::Instant::now() + deadline;
    // A private transfer: never the player's ACTIVE one, so a probe beside a playing stream cannot
    // be woken by (or wake) the movie's teardown. Its abort handle is not needed — the deadline
    // bounds every read.
    let (src, _abort) =
        ProbeSource::open_private(url, 0).map_err(|e| format!("open failed: {e:?}"))?;
    let size = src.size();
    let mut io = Box::new(ProbeIo {
        src,
        off: 0,
        deadline: until,
    });
    unsafe {
        let buf = av_malloc(65536) as *mut u8;
        if buf.is_null() {
            return Err("av_malloc failed".into());
        }
        let avio = avio_alloc_context(
            buf,
            65536,
            0,
            &mut *io as *mut ProbeIo as *mut c_void,
            Some(probe_read),
            None,
            Some(probe_seek),
        );
        if avio.is_null() {
            free_ptr(buf as *mut c_void);
            return Err("avio_alloc_context failed".into());
        }
        let mut fmt = avformat_alloc_context();
        if fmt.is_null() {
            free_avio(avio);
            return Err("avformat_alloc_context failed".into());
        }
        (*fmt).pb = avio;
        let r = avformat_open_input(
            &mut fmt,
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        if r < 0 || fmt.is_null() {
            free_avio(avio);
            return Err(format!("not a recognised container (r={r})"));
        }
        if avformat_find_stream_info(fmt, std::ptr::null_mut()) < 0 {
            avformat_close_input(&mut fmt);
            free_avio(avio);
            return Err("find_stream_info failed".into());
        }
        let mut out = MediaProbe {
            format: {
                let ifmt = (*fmt).iformat as *const *const c_char;
                if ifmt.is_null() || (*ifmt).is_null() {
                    String::new()
                } else {
                    std::ffi::CStr::from_ptr(*ifmt)
                        .to_string_lossy()
                        .into_owned()
                }
            },
            duration_ms: fmt_duration(fmt).max(0) / 1000,
            size,
            ..MediaProbe::default()
        };
        for i in 0..(*fmt).nb_streams {
            let st = *(*fmt).streams.add(i as usize);
            let cp = stream_codecpar(st);
            let disposition = *((st as *const u8).add(OFF_STREAM_DISPOSITION) as *const c_int);
            let default = disposition & AV_DISPOSITION_DEFAULT != 0;
            match (*cp).codec_type {
                AVMEDIA_TYPE_VIDEO
                    if out.video.is_none() && disposition & AV_DISPOSITION_ATTACHED_PIC == 0 =>
                {
                    let fr =
                        *((st as *const u8).add(OFF_STREAM_AVG_FRAME_RATE) as *const AVRational);
                    let fr = if fr.num > 0 && fr.den > 0 {
                        fr
                    } else {
                        (*cp).framerate
                    };
                    let hdr = matches!((*cp).color_trc, 16 | 18);
                    out.video = Some(ProbedVideo {
                        codec: codec_name((*cp).codec_id),
                        width: (*cp).width,
                        height: (*cp).height,
                        fps: if fr.num > 0 && fr.den > 0 {
                            fr.num as f64 / fr.den as f64
                        } else {
                            0.0
                        },
                        bit_depth: match (*cp).bits_per_raw_sample {
                            b @ 8..=12 => b,
                            _ if hdr || (*cp).profile == 2 || dovi_conf(cp).is_some() => 10,
                            _ => 0,
                        },
                        profile: (*cp).profile,
                        color_trc: (*cp).color_trc,
                        color_primaries: (*cp).color_primaries,
                        color_space: (*cp).color_space,
                        dovi: dovi_conf(cp).map(|d| ProbedDovi {
                            profile: d.dv_profile,
                            level: d.dv_level,
                            bl_compat: d.dv_bl_signal_compatibility_id,
                            rpu: d.rpu_present_flag != 0,
                            el: d.el_present_flag != 0,
                            bl: d.bl_present_flag != 0,
                        }),
                    });
                }
                AVMEDIA_TYPE_AUDIO => out.audio.push(ProbedAudio {
                    codec: codec_name((*cp).codec_id),
                    profile: (*cp).profile,
                    channels: (*cp).ch_layout.nb_channels,
                    sample_rate: (*cp).sample_rate,
                    language: dict_value(st, c"language"),
                    title: stream_name(st),
                    default,
                }),
                AVMEDIA_TYPE_SUBTITLE => out.subtitles.push(ProbedSubtitle {
                    codec: codec_name((*cp).codec_id),
                    language: dict_value(st, c"language"),
                    title: stream_name(st),
                    default,
                    forced: disposition & AV_DISPOSITION_FORCED != 0,
                }),
                _ => {}
            }
        }
        // Fork Rofocale11: collect the file's own chapters. Most torrent/debrid rips have none;
        // when they do, a "Credits" title is the only real credits boundary available to a client
        // whose addons carry no segment metadata. Bounded so a hostile container cannot allocate
        // without limit.
        let nch = *((fmt as *const u8).add(OFF_FMT_NB_CHAPTERS) as *const u32) as usize;
        // AVFormatContext.chapters is `AVChapter **`: one pointer to an array of pointers.
        let chapter_ptrs =
            *((fmt as *const u8).add(OFF_FMT_CHAPTERS) as *const *const *const AvChapter);
        if !chapter_ptrs.is_null() {
            for i in 0..nch.min(256) {
                let p = *chapter_ptrs.add(i);
                if p.is_null() {
                    continue;
                }
                let ch = &*p;
                let mut title = dict_str(ch.metadata, c"title");
                if title.is_empty() {
                    title = dict_str(ch.metadata, c"name");
                }
                out.chapters.push(ProbedChapter {
                    start_ms: chapter_ms(ch.start, ch.time_base),
                    end_ms: chapter_ms(ch.end, ch.time_base),
                    title: title.chars().take(120).collect(),
                });
            }
        }
        if !out.chapters.is_empty() {
            let names: Vec<&str> = out.chapters.iter().map(|c| c.title.as_str()).collect();
            plx_base::eventlog::log(&format!(
                "nuvio: probe chapters={} names={:?}",
                out.chapters.len(),
                &names[..names.len().min(8)]
            ));
        }
        avformat_close_input(&mut fmt);
        free_avio(avio);
        drop(io);
        Ok(out)
    }
}

/// Which audio track a television without a DTS decoder should start on: the container's default
/// when it is decodable, else the first decodable one, else `None` (nothing playable — the caller
/// says so rather than declaring a codec the pipeline cannot open). `decodable` is the device's
/// own rule; index is the AUDIO ordinal in file order.
pub fn preferred_audio(
    tracks: &[ProbedAudio],
    decodable: impl Fn(&ProbedAudio) -> bool,
) -> Option<usize> {
    let default = tracks.iter().position(|t| t.default).unwrap_or(0);
    if tracks.get(default).is_some_and(&decodable) {
        return Some(default);
    }
    tracks.iter().position(decodable)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(codec: &str, default: bool) -> ProbedAudio {
        ProbedAudio {
            codec: codec.into(),
            default,
            ..ProbedAudio::default()
        }
    }

    #[test]
    fn a_dts_default_falls_back_to_the_first_track_the_set_decodes() {
        let no_dts = |t: &ProbedAudio| t.codec != "dts";
        assert_eq!(
            preferred_audio(&[a("dts", true), a("ac3", false), a("eac3", false)], no_dts),
            Some(1)
        );
        assert_eq!(
            preferred_audio(&[a("aac", false), a("eac3", true)], no_dts),
            Some(1)
        );
        assert_eq!(preferred_audio(&[a("dts", true)], no_dts), None);
        assert_eq!(
            preferred_audio(&[a("eac3", false), a("dts", false)], no_dts),
            Some(0)
        );
        assert_eq!(preferred_audio(&[], no_dts), None);
    }

    /// Fork Rofocale11: chapter atoms use their own time base, which is not always milliseconds.
    #[test]
    fn chapter_timestamps_convert_from_the_atoms_time_base() {
        let ms = AVRational { num: 1, den: 1000 };
        assert_eq!(chapter_ms(1_234, ms), 1_234);
        let ts = AVRational { num: 1, den: 90_000 };
        assert_eq!(chapter_ms(90_000 * 42, ts), 42_000);
        assert_eq!(chapter_ms(42, AVRational { num: 0, den: 0 }), -1);
    }

    #[test]
    fn the_container_spelling_is_the_pms_one() {
        let p = |f: &str| MediaProbe {
            format: f.into(),
            ..MediaProbe::default()
        };
        assert_eq!(p("matroska,webm").container(), "mkv");
        assert_eq!(p("mov,mp4,m4a,3gp,3g2,mj2").container(), "mp4");
        assert_eq!(p("mpegts").container(), "mpegts");
    }

    /// Live: `PLX_PROBE_URL=<media url> PLX_FFMPEG_DIR=<dir with libav*-plx dylibs> cargo test
    /// -p plx_nuvio_probe --lib live_probe -- --nocapture`. Skipped without both.
    #[test]
    fn live_probe() {
        let (Ok(url), Ok(_dir)) = (
            std::env::var("PLX_PROBE_URL"),
            std::env::var("PLX_FFMPEG_DIR"),
        ) else {
            return;
        };
        plx_net::net::global_init();
        plx_media_codecs::ffi::boot();
        let p = probe_url(&url, std::time::Duration::from_secs(30));
        eprintln!("{p:#?}");
        assert!(p.is_ok());
    }
}
