//! **What a third-party stream actually is** — NuvioNative's answer to a server that cannot tell us.
//!
//! A Plex server reads every part with its own scanner and reports codecs, tracks and Dolby Vision
//! configuration; the route (`crate::route`) and the Load payload trust those fields. A Stremio stream
//! comes with nothing but a label (`"4K DV HDR10 … DTS-HD"`) that may be wrong, so the PMS shim asks
//! this module to OPEN the selected stream with the bundled libavformat — over the same libcurl
//! transport the demuxer reads with (`crate::curlio`), redirects, TLS fallback and all — and reports
//! the real container, video codec/raster/HDR signalling/Dolby Vision record, and every audio and
//! subtitle track in file order. The shim turns that into PMS `Media`/`Stream` fields, so every
//! decision downstream (direct play, the Load codecs, audio track choice, the DV presentation, the
//! embedded-subtitle menu) is made on what the file IS.
//!
//! Cost: one HTTP open plus libavformat's probe window (`find_stream_info`, a few MB at most for
//! Matroska; an MP4 with its index at the end costs one ranged seek). Bounded by `deadline`.
//!
//! Callable from any thread once [`super::boot`] has run; refuses (`Err`) when the bundled FFmpeg is
//! absent or not the ABI this build expects, exactly as the demuxer does.

use super::*;

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

/// `AVStream.disposition` and `AVStream.avg_frame_rate` — derived from FFmpeg 9's AVStream layout
/// beside the offsets above (`ci/ffabi-assert.c` holds them).
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
const AVMEDIA_TYPE_VIDEO: c_int = 0;
const AVMEDIA_TYPE_AUDIO: c_int = 1;
const AVMEDIA_TYPE_SUBTITLE: c_int = 3;
/// AVSTREAM attached picture (cover art) is a VIDEO stream; skip it.
const AV_DISPOSITION_ATTACHED_PIC: c_int = 1 << 10;

/// The probe's AVIO state: one curl source, the logical offset and the deadline.
struct ProbeIo {
    src: Box<crate::curlio::CurlSource>,
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
        let r = s.src.read_until(buf, Some(s.deadline), &mut plx_base::checkpoint::NoCheckpoint);
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
            SEEK_SET => offset,
            SEEK_CUR => s.off + offset,
            SEEK_END if size >= 0 => size + offset,
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

unsafe fn dict_value(s: *mut AVStream, key: &std::ffi::CStr) -> String {
    let dict = *((s as *const u8).add(OFF_STREAM_METADATA) as *const *const AVDictionary);
    if dict.is_null() {
        return String::new();
    }
    let e = av_dict_get(dict, key.as_ptr(), std::ptr::null(), 0);
    if e.is_null() || (*e).value.is_null() {
        return String::new();
    }
    std::ffi::CStr::from_ptr((*e).value).to_string_lossy().trim().to_string()
}

unsafe fn codec_name(id: c_int) -> String {
    std::ffi::CStr::from_ptr(avcodec_get_name(id)).to_string_lossy().into_owned()
}

/// Open `url` and report what it holds. `Err` carries a short reason for the log (never the URL,
/// whose query may be a signature).
pub fn probe_url(url: &str, deadline: std::time::Duration) -> Result<MediaProbe, String> {
    if !ABI_OK.load(Ordering::Relaxed) {
        return Err("bundled FFmpeg unavailable".into());
    }
    ensure_registered();
    let until = std::time::Instant::now() + deadline;
    // A private transfer: never the player's ACTIVE one, so a probe beside a playing stream cannot
    // be woken by (or wake) the movie's teardown. Its abort handle is not needed — the deadline
    // bounds every read.
    let (src, _abort) = crate::curlio::CurlSource::open_private(url, 0).map_err(|e| format!("open failed: {e:?}"))?;
    let size = src.size();
    let mut io = Box::new(ProbeIo { src, off: 0, deadline: until });
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
        let r = avformat_open_input(&mut fmt, std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
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
                    std::ffi::CStr::from_ptr(*ifmt).to_string_lossy().into_owned()
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
                AVMEDIA_TYPE_VIDEO if out.video.is_none() && disposition & AV_DISPOSITION_ATTACHED_PIC == 0 => {
                    let fr = *((st as *const u8).add(OFF_STREAM_AVG_FRAME_RATE) as *const AVRational);
                    let fr = if fr.num > 0 && fr.den > 0 { fr } else { (*cp).framerate };
                    let hdr = matches!((*cp).color_trc, 16 | 18);
                    out.video = Some(ProbedVideo {
                        codec: codec_name((*cp).codec_id),
                        width: (*cp).width,
                        height: (*cp).height,
                        fps: if fr.num > 0 && fr.den > 0 { fr.num as f64 / fr.den as f64 } else { 0.0 },
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
        avformat_close_input(&mut fmt);
        free_avio(avio);
        drop(io);
        Ok(out)
    }
}

/// Which audio track a television without a DTS decoder should start on: the container's default
/// when it is decodable, else the first decodable one, else `None` (nothing playable — the caller
/// says so rather than declaring a codec the pipeline cannot open). `decodable` is the device's
/// own rule (`crate::route` / the codec table); index is the AUDIO ordinal in file order.
pub fn preferred_audio(tracks: &[ProbedAudio], decodable: impl Fn(&ProbedAudio) -> bool) -> Option<usize> {
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
        ProbedAudio { codec: codec.into(), default, ..ProbedAudio::default() }
    }

    #[test]
    fn a_dts_default_falls_back_to_the_first_track_the_set_decodes() {
        let no_dts = |t: &ProbedAudio| t.codec != "dts";
        assert_eq!(preferred_audio(&[a("dts", true), a("ac3", false), a("eac3", false)], no_dts), Some(1));
        assert_eq!(preferred_audio(&[a("aac", false), a("eac3", true)], no_dts), Some(1));
        assert_eq!(preferred_audio(&[a("dts", true)], no_dts), None);
        assert_eq!(preferred_audio(&[a("eac3", false), a("dts", false)], no_dts), Some(0));
        assert_eq!(preferred_audio(&[], no_dts), None);
    }

    #[test]
    fn the_container_spelling_is_the_pms_one() {
        let p = |f: &str| MediaProbe { format: f.into(), ..MediaProbe::default() };
        assert_eq!(p("matroska,webm").container(), "mkv");
        assert_eq!(p("mov,mp4,m4a,3gp,3g2,mj2").container(), "mp4");
        assert_eq!(p("mpegts").container(), "mpegts");
    }

    /// Live: `PLX_PROBE_URL=<media url> PLX_FFMPEG_DIR=<dir with libav*-plx dylibs> cargo test
    /// -p plx_media --lib live_probe -- --nocapture`. Skipped without both.
    #[test]
    fn live_probe() {
        let (Ok(url), Ok(_dir)) = (std::env::var("PLX_PROBE_URL"), std::env::var("PLX_FFMPEG_DIR")) else {
            return;
        };
        plx_net::net::global_init();
        super::super::boot();
        let p = probe_url(&url, std::time::Duration::from_secs(30));
        eprintln!("{p:#?}");
        assert!(p.is_ok());
    }
}
