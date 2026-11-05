//! FORK (RichiL10): the PCM audio lane, and the declarations the Load makes for the fork's
//! audio lanes.
//!
//! LG's buffer-feed pipeline accepts `codec.audio = "PCM"` with a `contents.pcmInfo` block
//! (format, channelMode, layout, sampleRate, bitsPerSample: read off `libpf-1.0.so` on the B4,
//! and the same block moonlight-tv's webOS driver plays with). The fork feeds three kinds of
//! source through it:
//!
//! * **FLAC and ALAC**, decoded HERE with the bundled FFmpeg's own decoders
//!   (`ci/build-ffmpeg.sh`). They leave the app bit-exact at or below 48 kHz: the decoder's
//!   MSB-aligned S32 output is written at the declared width, so a 24-bit source declared S32LE or
//!   S24LE loses nothing. A stereo source above 48 kHz is not declared at its own rate — libpf's
//!   PCM table names no rate above 48 kHz on any image from 2018 to 2025 — so it is resampled here
//!   to 48 kHz (or 44.1 kHz when the source is a multiple of it) by a Kaiser-windowed sinc
//!   ([`Downsampler`]), and the Load declares that rate.
//! * **Native PCM tracks** (Matroska `A_PCM/*`), converted to the declared width/endianness.
//! * **MP3 and Vorbis** (only with the `lossy` fork knob on): decoded here for a set whose own
//!   pipeline lacks them, downmixed to stereo ([`crate::speed_audio::downmix`]) and declared S16
//!   at the source rate (or a whole-ratio reduction of it, like the lossless lane). The knob is
//!   off by default — this set's own table names MPEG and Vorbis, so it plays them compressed.
//!
//! The Load payload is composed from the ROUTE's metadata before this demuxer opens the stream,
//! so the format is declared first ([`PcmSpec`], published by the engine through [`declare`])
//! and this lane's whole job is to emit exactly that: the source's own rate and channel count
//! (nothing here resamples or remixes; a stream that disagrees with its declaration is refused
//! loudly instead of being played at the wrong pitch or with the wrong speakers) at the declared
//! sample width.

use super::*;

/// The sample width the lane emits — always interleaved and little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcmWidth {
    S16,
    S24,
    S32,
}

impl PcmWidth {
    pub fn bytes(self) -> usize {
        match self {
            PcmWidth::S16 => 2,
            PcmWidth::S24 => 3,
            PcmWidth::S32 => 4,
        }
    }
    /// `pcmInfo.format` — spellings from libpf's own table (S16LE, S24LE, S32LE).
    pub fn format(self) -> &'static str {
        match self {
            PcmWidth::S16 => "S16LE",
            PcmWidth::S24 => "S24LE",
            PcmWidth::S32 => "S32LE",
        }
    }
    pub fn bits(self) -> u32 {
        self.bytes() as u32 * 8
    }
}

/// What the Load declared for the PCM lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PcmSpec {
    pub rate: u32,
    pub channels: u32,
    pub width: PcmWidth,
}

impl PcmSpec {
    /// The declaration for a PCM-lane track from the route's metadata (PMS `samplingRate`,
    /// `channels`, `bitDepth`). Sources up to 16 bit are declared S16LE (exact); deeper or
    /// unknown depths follow the `pcm_hi` knob: `s32` (default), `s24`, or `s16`. `None` when
    /// the rate is unknown or the channel count is not one the lane can declare.
    pub fn for_track(rate: i64, channels: i64, bit_depth: i64, pcm_hi: Option<&str>) -> Option<PcmSpec> {
        if rate <= 0 || !plx_platform::devcaps::pcm_lane_channels_ok(channels) {
            return None;
        }
        // A stereo source above the PCM table's top is resampled here, and the Load declares the
        // rate it actually receives (`downsample_target`). A surround hi-res track keeps its own
        // rate: the lane does not resample or remix it, and the route's encode lane / the server
        // decide its fate.
        let resampled = (channels <= 2).then(|| downsample_target(rate as u32)).flatten();
        let width = if resampled.is_some() {
            PcmWidth::S32
        } else if (1..=16).contains(&bit_depth) {
            PcmWidth::S16
        } else {
            match pcm_hi {
                Some("s16") => PcmWidth::S16,
                Some("s24") => PcmWidth::S24,
                _ => PcmWidth::S32,
            }
        };
        Some(PcmSpec {
            rate: resampled.unwrap_or(u32::try_from(rate).ok()?),
            channels: channels as u32,
            width,
        })
    }

    /// The declaration for an app-decoded MP3/Vorbis track (`lossy=on`): always stereo at S16.
    /// The source keeps its own rate up to 48 kHz; above it the lane's decimator takes a whole
    /// ratio and the Load declares that (`downsample_target`). `None` for a rate that is neither:
    /// the route then leaves the track to the server rather than declare a rate the lane cannot
    /// produce.
    pub fn for_lossy(rate: i64) -> Option<PcmSpec> {
        if rate <= 0 {
            return None;
        }
        let rate = u32::try_from(rate).ok()?;
        let rate = if rate <= 48_000 { rate } else { downsample_target(rate)? };
        Some(PcmSpec { rate, channels: 2, width: PcmWidth::S16 })
    }

    /// `pcmInfo.channelMode`.
    pub fn channel_mode(self) -> &'static str {
        match self.channels {
            1 => "mono",
            6 => "6-channel",
            _ => "stereo",
        }
    }
}

/// The rate a stereo source above the PCM table's top is resampled to: the table's own top in the
/// source's family (44.1 kHz for a multiple of 44.1, else 48 kHz). `None` when the source is at or
/// below 48 kHz, or when the ratio is not a whole number (such a track keeps its declared rate).
pub fn downsample_target(rate: u32) -> Option<u32> {
    if rate <= 48_000 {
        return None;
    }
    if rate % 44_100 == 0 {
        Some(44_100)
    } else if rate % 48_000 == 0 {
        Some(48_000)
    } else {
        None
    }
}

/// The Load's PCM declaration for the session being started; `None` when the Load declared
/// another codec. Written by the engine BEFORE the demux thread exists, read once by it at open.
static DECLARED: std::sync::Mutex<Option<PcmSpec>> = std::sync::Mutex::new(None);

pub fn declare(spec: Option<PcmSpec>) {
    *DECLARED.lock().unwrap_or_else(|e| e.into_inner()) = spec;
}

pub(super) fn declared() -> Option<PcmSpec> {
    *DECLARED.lock().unwrap_or_else(|e| e.into_inner())
}

/// A source sample layout the lane reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SrcFmt {
    S16Le,
    S16Be,
    S24Le,
    S24Be,
    S32Le,
    S32Be,
    F32Le,
    F32Be,
}

impl SrcFmt {
    // FORK (RichiL10): `bytes`/`read` are shared with the AC3 encode lane (`ff_enc.rs`).
    pub(super) fn bytes(self) -> usize {
        match self {
            SrcFmt::S16Le | SrcFmt::S16Be => 2,
            SrcFmt::S24Le | SrcFmt::S24Be => 3,
            _ => 4,
        }
    }

    /// The Matroska/MOV PCM flavours FFmpeg names (`avcodec_get_name`, stable across majors).
    pub(super) fn from_pcm_codec(name: &str) -> Option<SrcFmt> {
        Some(match name {
            "pcm_s16le" => SrcFmt::S16Le,
            "pcm_s16be" => SrcFmt::S16Be,
            "pcm_s24le" => SrcFmt::S24Le,
            "pcm_s24be" => SrcFmt::S24Be,
            "pcm_s32le" => SrcFmt::S32Le,
            "pcm_s32be" => SrcFmt::S32Be,
            "pcm_f32le" => SrcFmt::F32Le,
            "pcm_f32be" => SrcFmt::F32Be,
            _ => return None,
        })
    }

    /// One sample as a full-scale (MSB-aligned) 32-bit value.
    #[inline]
    pub(super) fn read(self, b: &[u8]) -> i32 {
        match self {
            SrcFmt::S16Le => i32::from(i16::from_le_bytes([b[0], b[1]])) << 16,
            SrcFmt::S16Be => i32::from(i16::from_be_bytes([b[0], b[1]])) << 16,
            SrcFmt::S24Le => i32::from_le_bytes([0, b[0], b[1], b[2]]),
            SrcFmt::S24Be => i32::from_be_bytes([b[0], b[1], b[2], 0]),
            SrcFmt::S32Le => i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            SrcFmt::S32Be => i32::from_be_bytes([b[0], b[1], b[2], b[3]]),
            SrcFmt::F32Le => float_to_s32(f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            SrcFmt::F32Be => float_to_s32(f32::from_be_bytes([b[0], b[1], b[2], b[3]])),
        }
    }
}

#[inline]
fn float_to_s32(f: f32) -> i32 {
    if f.is_nan() {
        return 0;
    }
    let v = f64::from(f.clamp(-1.0, 1.0)) * 2_147_483_647.0;
    v.round() as i32
}

/// One full-scale sample written at `out`'s width, little-endian. Narrowing truncates, which is
/// exact for a source no deeper than the target (the only case the declaration produces unless
/// the `pcm_hi=s16` knob deliberately asks for 16 bit).
#[inline]
fn write(v: i32, out: PcmWidth, dst: &mut Vec<u8>) {
    let b = v.to_le_bytes();
    match out {
        PcmWidth::S16 => dst.extend_from_slice(&b[2..4]),
        PcmWidth::S24 => dst.extend_from_slice(&b[1..4]),
        PcmWidth::S32 => dst.extend_from_slice(&b),
    }
}

/// Interleaved source samples to interleaved little-endian `out` samples, appended to `dst`.
/// A trailing partial sample is ignored.
pub(super) fn convert_interleaved(src: &[u8], fmt: SrcFmt, out: PcmWidth, dst: &mut Vec<u8>) {
    let n = fmt.bytes();
    dst.reserve(src.len() / n * out.bytes());
    for s in src.chunks_exact(n) {
        write(fmt.read(s), out, dst);
    }
}

/// Planar source samples (one slice per channel, `samples` each) interleaved into `dst`.
pub(super) fn convert_planar(planes: &[&[u8]], samples: usize, fmt: SrcFmt, out: PcmWidth, dst: &mut Vec<u8>) {
    let n = fmt.bytes();
    dst.reserve(samples * planes.len() * out.bytes());
    for i in 0..samples {
        for p in planes {
            if let Some(s) = p.get(i * n..i * n + n) {
                write(fmt.read(s), out, dst);
            }
        }
    }
}

// AVSampleFormat values (libavutil/samplefmt.h; unchanged since FFmpeg 1.0, and asserted in
// ci/ffabi-assert.c for the bundled 9.0).
const AV_SAMPLE_FMT_S16: c_int = 1;
const AV_SAMPLE_FMT_S32: c_int = 2;
const AV_SAMPLE_FMT_S16P: c_int = 6;
const AV_SAMPLE_FMT_S32P: c_int = 7;
/// AVERROR(EAGAIN) on Linux and the macOS host (EAGAIN is 11 on Linux, 35 on Darwin).
#[cfg(target_os = "macos")]
const AVERROR_EAGAIN: c_int = -35;
#[cfg(not(target_os = "macos"))]
const AVERROR_EAGAIN: c_int = -11;

// AVFrame fields the decode path reads, beside the encoder's OFF_FRAME_* table and asserted in
// ci/ffabi-assert.c on both pointer widths: `data` (+0), `nb_samples` (+76 / +112) and `format`
// (+80 / +116).
const OFF_AFRAME_DATA: usize = 0;
#[cfg(target_pointer_width = "32")]
const OFF_AFRAME_NB_SAMPLES: usize = 76;
#[cfg(target_pointer_width = "32")]
const OFF_AFRAME_FORMAT: usize = 80;
#[cfg(target_pointer_width = "32")]
const OFF_AFRAME_SAMPLE_RATE: usize = 132;
#[cfg(target_pointer_width = "32")]
const OFF_AFRAME_NB_CHANNELS: usize = 256 + 4;
#[cfg(target_pointer_width = "32")]
const OFF_AFRAME_EXTENDED_DATA: usize = 64;
#[cfg(target_pointer_width = "64")]
const OFF_AFRAME_NB_SAMPLES: usize = 112;
#[cfg(target_pointer_width = "64")]
const OFF_AFRAME_FORMAT: usize = 116;
#[cfg(target_pointer_width = "64")]
const OFF_AFRAME_SAMPLE_RATE: usize = 180;
#[cfg(target_pointer_width = "64")]
const OFF_AFRAME_NB_CHANNELS: usize = 384 + 4;
#[cfg(target_pointer_width = "64")]
const OFF_AFRAME_EXTENDED_DATA: usize = 96;

#[inline]
unsafe fn frame_i32(frame: *const c_void, off: usize) -> i32 {
    *((frame as *const u8).add(off) as *const i32)
}

#[inline]
unsafe fn frame_plane(frame: *const c_void, i: usize) -> *const u8 {
    *((frame as *const u8).add(OFF_AFRAME_DATA) as *const *const u8).add(i)
}

/// The bundled FFmpeg's lossless decoder (FLAC or ALAC), owned for one demux run.
struct Decoder {
    ctx: *mut AVCodecContext,
    frame: *mut c_void,
    name: String,
}

impl Decoder {
    unsafe fn open(acp: *const AVCodecParameters, name: &str) -> Result<Decoder, String> {
        let codec = avcodec_find_decoder((*acp).codec_id);
        if codec.is_null() {
            return Err(format!("the bundled FFmpeg has no {name} decoder"));
        }
        let mut ctx = avcodec_alloc_context3(codec);
        if ctx.is_null() {
            return Err("avcodec_alloc_context3 failed".into());
        }
        if avcodec_parameters_to_context(ctx, acp) < 0 || avcodec_open2(ctx, codec, std::ptr::null_mut()) < 0 {
            avcodec_free_context(&mut ctx);
            return Err(format!("the {name} decoder refused the stream parameters"));
        }
        let frame = av_frame_alloc();
        if frame.is_null() {
            avcodec_free_context(&mut ctx);
            return Err("av_frame_alloc failed".into());
        }
        Ok(Decoder { ctx, frame, name: name.to_string() })
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe {
            av_frame_free(&mut self.frame);
            avcodec_free_context(&mut self.ctx);
        }
    }
}

enum Source {
    Decoded(Decoder),
    /// MP3/Vorbis behind the `lossy` knob: decoded, downmixed to stereo and resampled to the
    /// declared rate (see [`decode_lossy`]).
    Lossy(Decoder),
    Raw(SrcFmt),
}

/// One audio stream fed through the PCM lane.
pub(super) struct PcmLane {
    src: Source,
    spec: PcmSpec,
    /// `Some` when the source rate is above the PCM table's top and the lane resamples it (the
    /// Load declares `spec.rate`, not the file's rate).
    down: Option<Downsampler>,
    buf: Vec<u8>,
    errors: u32,
}

impl PcmLane {
    /// `Ok(None)`: the stream is not a PCM-lane codec. `Err(reason)`: it is one, but cannot be
    /// fed as the Load declared it (the caller refuses the audio feed and says why).
    pub(super) unsafe fn open(acp: *const AVCodecParameters, spec: Option<PcmSpec>) -> Result<Option<PcmLane>, String> {
        let name = std::ffi::CStr::from_ptr(avcodec_get_name((*acp).codec_id)).to_string_lossy().into_owned();
        let raw = SrcFmt::from_pcm_codec(&name);
        // The knob's answer is already baked into the route's declaration (`devcaps` read it at
        // boot): a declared PCM spec on an MP3/Vorbis stream is this lane's, and no spec means the
        // route left the track to the pipeline, which is fed compressed as before.
        let lossy = matches!(name.as_str(), "mp3" | "vorbis") && spec.is_some();
        if !matches!(name.as_str(), "flac" | "alac") && raw.is_none() && !lossy {
            return Ok(None);
        }
        let Some(spec) = spec else {
            return Err(format!("{name}: the Load declared no PCM format for it"));
        };
        let (channels, rate) = ((*acp).ch_layout.nb_channels, (*acp).sample_rate as u32);
        if lossy {
            // The lane always declares the stereo it downmixes to, whatever the source carries.
            if spec.channels != 2 {
                return Err(format!("{name}: the Load declared {}ch, the lossy lane feeds 2", spec.channels));
            }
            let down = if rate == spec.rate {
                None
            } else if downsample_target(rate) == Some(spec.rate) {
                Some(Downsampler::new(rate / spec.rate, 2))
            } else {
                return Err(format!("{name} {rate} Hz cannot be declared at {} Hz", spec.rate));
            };
            let dec = Decoder::open(acp, &name)?;
            crate::player::log(&format!(
                "ff: audio lane {name} {channels}ch {rate} Hz decoded in-app -> PCM {} 2ch {} Hz",
                spec.width.format(),
                spec.rate
            ));
            return Ok(Some(PcmLane { src: Source::Lossy(dec), spec, down, buf: Vec::new(), errors: 0 }));
        }
        if channels as u32 != spec.channels {
            return Err(format!(
                "{name} {channels}ch does not match the declared {}ch",
                spec.channels
            ));
        }
        let down = if rate == spec.rate {
            None
        } else if channels <= 2 && downsample_target(rate) == Some(spec.rate) {
            Some(Downsampler::new(rate / spec.rate, channels as usize))
        } else {
            return Err(format!(
                "{name} {rate} Hz does not match the declared {} Hz",
                spec.rate
            ));
        };
        let src = match raw {
            Some(fmt) => Source::Raw(fmt),
            None => Source::Decoded(Decoder::open(acp, &name)?),
        };
        let what = match &src {
            Source::Decoded(_) | Source::Lossy(_) => format!("{name} decoded in-app"),
            Source::Raw(_) => format!("PCM {name}"),
        };
        crate::player::log(&format!(
            "ff: audio lane {what}{} -> PCM {} {}ch {} Hz",
            down.as_ref().map_or(String::new(), |d| format!(" ({}:1 resample from {rate} Hz)", d.factor)),
            spec.width.format(),
            spec.channels,
            spec.rate
        ));
        Ok(Some(PcmLane { src, spec, down, buf: Vec::new(), errors: 0 }))
    }

    /// One demuxed packet to the declared PCM. Empty when the packet produced no samples (a
    /// decode error, logged a few times then counted silently).
    pub(super) unsafe fn convert(&mut self, pkt: *const AVPacket) -> &[u8] {
        self.buf.clear();
        let out = self.spec.width;
        let data: &[u8] = if (*pkt).data.is_null() || (*pkt).size <= 0 {
            &[]
        } else {
            std::slice::from_raw_parts((*pkt).data, (*pkt).size as usize)
        };
        match &mut self.src {
            Source::Raw(fmt) => convert_interleaved(data, *fmt, out, &mut self.buf),
            Source::Lossy(dec) => {
                decode_lossy(dec, self.down.as_mut(), out, &mut self.buf, pkt, &mut self.errors)
            }
            Source::Decoded(dec) => {
                let codec = dec.name.clone();
                let ret = avcodec_send_packet(dec.ctx, pkt);
                if ret < 0 && ret != AVERROR_EAGAIN {
                    self.errors += 1;
                    if self.errors <= 3 {
                        crate::player::log(&format!("ff: {codec} decode error {ret} (packet skipped)"));
                    }
                    return &self.buf;
                }
                let channels = self.spec.channels as usize;
                loop {
                    let r = avcodec_receive_frame(dec.ctx, dec.frame);
                    if r == AVERROR_EAGAIN || r == AVERROR_EOF {
                        break;
                    }
                    if r < 0 {
                        self.errors += 1;
                        if self.errors <= 3 {
                            crate::player::log(&format!("ff: {codec} receive_frame error {r}"));
                        }
                        break;
                    }
                    let samples = frame_i32(dec.frame, OFF_AFRAME_NB_SAMPLES).max(0) as usize;
                    let format = frame_i32(dec.frame, OFF_AFRAME_FORMAT);
                    let (fmt, planar) = match format {
                        AV_SAMPLE_FMT_S16 => (SrcFmt::S16Le, false),
                        AV_SAMPLE_FMT_S32 => (SrcFmt::S32Le, false),
                        AV_SAMPLE_FMT_S16P => (SrcFmt::S16Le, true),
                        AV_SAMPLE_FMT_S32P => (SrcFmt::S32Le, true),
                        other => {
                            self.errors += 1;
                            if self.errors <= 3 {
                                crate::player::log(&format!("ff: {codec} decoder gave sample format {other}; frame skipped"));
                            }
                            av_frame_unref(dec.frame);
                            continue;
                        }
                    };
                    let n = fmt.bytes();
                    if let Some(down) = &mut self.down {
                        let planes: Vec<&[u8]> = (0..channels.min(8))
                            .map(|c| {
                                let p = frame_plane(dec.frame, if planar { c } else { 0 });
                                let len = if planar { samples * n } else { samples * channels * n };
                                if p.is_null() { &[][..] } else { std::slice::from_raw_parts(p, len) }
                            })
                            .collect();
                        down.push_frame(&planes, samples, fmt, planar, out, &mut self.buf);
                    } else if planar {
                        let planes: Vec<&[u8]> = (0..channels.min(8))
                            .map(|c| {
                                let p = frame_plane(dec.frame, c);
                                if p.is_null() { &[][..] } else { std::slice::from_raw_parts(p, samples * n) }
                            })
                            .collect();
                        convert_planar(&planes, samples, fmt, out, &mut self.buf);
                    } else {
                        let p = frame_plane(dec.frame, 0);
                        if !p.is_null() {
                            let src = std::slice::from_raw_parts(p, samples * channels * n);
                            convert_interleaved(src, fmt, out, &mut self.buf);
                        }
                    }
                    av_frame_unref(dec.frame);
                }
            }
        }
        &self.buf
    }

    /// After a seek: neither the decoder nor the resampler may carry state across the gap.
    pub(super) unsafe fn flush(&mut self) {
        if let Source::Decoded(dec) | Source::Lossy(dec) = &mut self.src {
            avcodec_flush_buffers(dec.ctx);
        }
        if let Some(down) = &mut self.down {
            down.reset();
        }
    }
}

/// Decode one packet of an MP3/Vorbis stream, downmix every frame to stereo and write it to
/// `dst` as the declared PCM. `down` is the whole-ratio decimator for a source above 48 kHz.
unsafe fn decode_lossy(
    dec: &mut Decoder,
    down: Option<&mut Downsampler>,
    out: PcmWidth,
    dst: &mut Vec<u8>,
    pkt: *const AVPacket,
    errors: &mut u32,
) {
    let ret = avcodec_send_packet(dec.ctx, pkt);
    if ret < 0 && ret != AVERROR_EAGAIN {
        *errors += 1;
        if *errors <= 3 {
            crate::player::log(&format!("ff: {} decode error {ret} (packet skipped)", dec.name));
        }
        return;
    }
    let mut down = down;
    loop {
        let r = avcodec_receive_frame(dec.ctx, dec.frame);
        if r == AVERROR_EAGAIN || r == AVERROR_EOF {
            break;
        }
        if r < 0 {
            *errors += 1;
            if *errors <= 3 {
                crate::player::log(&format!("ff: {} receive_frame error {r}", dec.name));
            }
            break;
        }
        let nb = frame_i32(dec.frame, OFF_AFRAME_NB_SAMPLES).max(0) as usize;
        let format = frame_i32(dec.frame, OFF_AFRAME_FORMAT);
        let channels = frame_i32(dec.frame, OFF_AFRAME_NB_CHANNELS).max(0) as usize;
        let ext = *((dec.frame as *const u8).add(OFF_AFRAME_EXTENDED_DATA) as *const *const *const u8);
        if let Some((sf, planar)) = crate::speed_audio::SampleFormat::from_av(format) {
            if nb > 0 && channels > 0 && !ext.is_null() {
                let width = sf.bytes();
                let planes: Vec<&[u8]> = if planar {
                    (0..channels).map(|c| std::slice::from_raw_parts(*ext.add(c), nb * width)).collect()
                } else {
                    vec![std::slice::from_raw_parts(*ext, nb * width * channels)]
                };
                let stereo = crate::speed_audio::downmix(sf, planar, channels, nb, &planes);
                match down.as_mut() {
                    Some(d) => d.push_stereo(&stereo, out, dst),
                    None => {
                        for f in &stereo {
                            write_from_f32(f[0], out, dst);
                            write_from_f32(f[1], out, dst);
                        }
                    }
                }
            }
        }
        av_frame_unref(dec.frame);
    }
}

/// A Kaiser-windowed sinc low-pass followed by whole-number decimation, per channel, stateful
/// across packets. Used only for a stereo lossless source above the PCM table's top (96, 88.2,
/// 192, 176.4 kHz and their multiples): the cutoff sits at the OUTPUT Nyquist, so nothing above it
/// folds back into the audible band, and the ratio is an integer (`downsample_target` only accepts
/// one), which makes the kernel one phase and the filter a plain FIR.
struct Downsampler {
    factor: usize,
    /// Zero crossings kept on each side of the centre; the filter is `2 * half + 1` taps.
    half: usize,
    taps: Vec<f32>,
    /// Per-channel input from global frame `start` on; trimmed as outputs consume it.
    win: Vec<Vec<f32>>,
    start: i64,
    /// Global index of the next output frame.
    next: i64,
}

impl Downsampler {
    fn new(factor: u32, channels: usize) -> Downsampler {
        let factor = factor.max(2) as usize;
        let half = 32 * factor;
        Downsampler {
            factor,
            half,
            taps: kaiser_sinc_taps(factor, half),
            win: vec![Vec::new(); channels],
            start: 0,
            // The filter needs `half` frames of run-up: the first output whose window is fully
            // inside the stream is the one at or after that boundary.
            next: half.div_ceil(factor) as i64,
        }
    }

    fn reset(&mut self) {
        for w in &mut self.win {
            w.clear();
        }
        self.start = 0;
        self.next = self.half.div_ceil(self.factor) as i64;
    }

    /// Append decoded stereo frames (as the lossy lane's downmix leaves them) and write every
    /// output frame they complete. The S16 packing is lossless for the lossy sources this lane
    /// takes, which declare S16 anyway.
    fn push_stereo(&mut self, frames: &[[f32; 2]], out: PcmWidth, dst: &mut Vec<u8>) {
        let mut bytes = Vec::with_capacity(frames.len() * 4);
        for f in frames {
            let l = (f[0].clamp(-1.0, 1.0) * 32_767.0).round() as i16;
            let r = (f[1].clamp(-1.0, 1.0) * 32_767.0).round() as i16;
            bytes.extend_from_slice(&l.to_le_bytes());
            bytes.extend_from_slice(&r.to_le_bytes());
        }
        self.push_frame(&[&bytes], frames.len(), SrcFmt::S16Le, false, out, dst);
    }

    /// Append one decoded frame (interleaved or planar integer samples) and write every output
    /// frame it completes, at `out`'s width, to `dst`.
    fn push_frame(&mut self, planes: &[&[u8]], samples: usize, fmt: SrcFmt, planar: bool, out: PcmWidth, dst: &mut Vec<u8>) {
        let width = fmt.bytes();
        let channels = self.win.len();
        for c in 0..channels {
            for i in 0..samples {
                let at = if planar { i * width } else { (i * channels + c) * width };
                let s = &planes[c][at..at + width];
                self.win[c].push(fmt.read(s) as f32 / 2_147_483_648.0);
            }
        }
        // An output frame n needs input [n*factor - half, n*factor + half].
        loop {
            let last = self.start + self.win[0].len() as i64 - 1;
            let need = self.next * self.factor as i64 + self.half as i64;
            if last < need {
                break;
            }
            let base = (self.next * self.factor as i64 - self.half as i64 - self.start) as usize;
            for c in 0..channels {
                let mut acc = 0.0f32;
                for (k, &h) in self.taps.iter().enumerate() {
                    acc += h * self.win[c][base + k];
                }
                write_from_f32(acc, out, dst);
            }
            self.next += 1;
        }
        // Drop what no future output can read: the oldest index still needed is next*factor - half.
        let keep_from = self.next * self.factor as i64 - self.half as i64;
        if keep_from > self.start {
            let drop = (keep_from - self.start) as usize;
            let drop = drop.min(self.win.first().map_or(0, Vec::len));
            for w in &mut self.win {
                w.drain(..drop);
            }
            self.start += drop as i64;
        }
    }
}

/// The filter's taps, normalized to unity gain at 0 Hz: `sin(pi*x/factor)/(pi*x/factor)` (cutoff at
/// the output Nyquist) under a Kaiser window with `beta = 12.8` (about 120 dB of stop band, the
/// value upstream's own resampler measured).
fn kaiser_sinc_taps(factor: usize, half: usize) -> Vec<f32> {
    let n = 2 * half + 1;
    let beta = 12.8f64;
    let i0b = bessel_i0(beta);
    let mut taps: Vec<f64> = (0..n)
        .map(|k| {
            let x = (k as f64 - half as f64) / factor as f64;
            let sinc = if x == 0.0 { 1.0 } else { (std::f64::consts::PI * x).sin() / (std::f64::consts::PI * x) };
            let r = 2.0 * k as f64 / (n - 1) as f64 - 1.0;
            let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
            sinc * w
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    if sum != 0.0 {
        for t in &mut taps {
            *t /= sum;
        }
    }
    taps.into_iter().map(|t| t as f32).collect()
}

/// The zeroth-order modified Bessel function of the first kind, by its power series (converges in a
/// few dozen terms for the betas a window uses).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0f64, 1.0f64);
    let q = x * x / 4.0;
    for k in 1..64 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-15 {
            break;
        }
    }
    sum
}

/// One full-scale float sample to the declared width, saturating (a filter overshoot is clipped,
/// never wrapped).
fn write_from_f32(v: f32, out: PcmWidth, dst: &mut Vec<u8>) {
    let s = (v.clamp(-1.0, 1.0) as f64 * 2_147_483_647.0).round() as i32;
    write(s, out, dst);
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_from_track_metadata() {
        let s = PcmSpec::for_track(48_000, 2, 24, None).unwrap();
        assert_eq!((s.rate, s.channels, s.width), (48_000, 2, PcmWidth::S32));
        assert_eq!(s.channel_mode(), "stereo");
        assert_eq!(PcmSpec::for_track(44_100, 2, 16, None).unwrap().width, PcmWidth::S16);
        assert_eq!(PcmSpec::for_track(48_000, 2, 0, None).unwrap().width, PcmWidth::S32);
        assert_eq!(PcmSpec::for_track(48_000, 2, 24, Some("s24")).unwrap().width, PcmWidth::S24);
        assert_eq!(PcmSpec::for_track(48_000, 2, 24, Some("s16")).unwrap().width, PcmWidth::S16);
        assert_eq!(PcmSpec::for_track(48_000, 1, 16, None).unwrap().channel_mode(), "mono");
        assert_eq!(PcmSpec::for_track(48_000, 6, 24, None).unwrap().channel_mode(), "6-channel");
        assert!(PcmSpec::for_track(0, 2, 16, None).is_none());
        assert!(PcmSpec::for_track(48_000, 8, 24, None).is_none());
        assert!(PcmSpec::for_track(48_000, 0, 24, None).is_none());
    }

    /// A stereo lossless source above the PCM table's top is declared at the table's top and
    /// resampled by the lane; a surround one keeps its rate (the lane never remixes).
    #[test]
    fn hi_res_stereo_is_declared_at_the_pcm_tables_top() {
        assert_eq!(downsample_target(48_000), None);
        assert_eq!(downsample_target(44_100), None);
        assert_eq!(downsample_target(96_000), Some(48_000));
        assert_eq!(downsample_target(192_000), Some(48_000));
        assert_eq!(downsample_target(88_200), Some(44_100));
        assert_eq!(downsample_target(176_400), Some(44_100));
        assert_eq!(downsample_target(50_000), None, "not a whole ratio: left as declared");
        let s = PcmSpec::for_track(96_000, 2, 24, None).unwrap();
        assert_eq!((s.rate, s.width), (48_000, PcmWidth::S32));
        let s = PcmSpec::for_track(88_200, 1, 16, None).unwrap();
        assert_eq!((s.rate, s.width), (44_100, PcmWidth::S32));
        assert_eq!(PcmSpec::for_track(96_000, 6, 24, None).unwrap().rate, 96_000);
    }

    /// An app-decoded MP3/Vorbis track is always declared stereo S16, at its own rate up to 48 kHz
    /// and at the decimator's whole-ratio top above it.
    #[test]
    fn lossy_spec_is_stereo_s16_at_a_rate_the_sink_knows() {
        let s = PcmSpec::for_lossy(44_100).unwrap();
        assert_eq!((s.rate, s.channels, s.width), (44_100, 2, PcmWidth::S16));
        assert_eq!(s.channel_mode(), "stereo");
        assert_eq!(PcmSpec::for_lossy(48_000).unwrap().rate, 48_000);
        assert_eq!(PcmSpec::for_lossy(96_000).unwrap().rate, 48_000);
        assert_eq!(PcmSpec::for_lossy(88_200).unwrap().rate, 44_100);
        assert_eq!(PcmSpec::for_lossy(50_000), None, "not a whole ratio: the server keeps it");
        assert_eq!(PcmSpec::for_lossy(0), None);
    }

    /// The decimator keeps a constant level (unity gain at DC), consumes `factor` input frames per
    /// output frame, and leaves the leading half-filter of run-up unconverted.
    #[test]
    fn the_decimator_halves_the_rate_at_unity_gain() {
        let mut down = Downsampler::new(2, 1);
        let mut out = Vec::new();
        // 0.5 full scale, as S16 packed mono
        let frame: Vec<u8> = 16_384i16.to_le_bytes().to_vec();
        for _ in 0..9_600 {
            down.push_frame(&[&frame], 1, SrcFmt::S16Le, false, PcmWidth::S32, &mut out);
        }
        let got: Vec<f32> = out
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0)
            .collect();
        assert!(got.len() >= 4_600 && got.len() <= 4_800, "about half the 9,600 input frames: {}", got.len());
        for v in &got[1..] {
            assert!((v - 0.5).abs() < 1e-3, "unity gain at DC, got {v}");
        }
    }

    /// A 1 kHz tone at 96 kHz comes out of the 2:1 decimator at 48 kHz still a 1 kHz tone at the
    /// same level: the filter is a low-pass, not a rate label.
    #[test]
    fn the_decimator_keeps_a_passband_tone() {
        let mut down = Downsampler::new(2, 1);
        let mut out = Vec::new();
        let n = 9_600usize;
        for i in 0..n {
            let v = (2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / 96_000.0).sin() * 0.5;
            let s = (v * 32_767.0) as i16;
            let bytes = s.to_le_bytes().to_vec();
            down.push_frame(&[&bytes], 1, SrcFmt::S16Le, false, PcmWidth::S32, &mut out);
        }
        let got: Vec<f32> = out
            .chunks_exact(4)
            .map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0)
            .collect();
        // Compare the second half (past the filter's run-up) with the reference tone at 48 kHz.
        let start = got.len() / 2;
        let want = |i: usize| (2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / 48_000.0).sin() * 0.5;
        // Align phase by picking the offset (the filter's delay) that fits best.
        let best = (0..40)
            .map(|off| {
                let err: f32 = (start..got.len())
                    .map(|i| (got[i] - want(i + off)).abs())
                    .sum::<f32>()
                    / (got.len() - start) as f32;
                (err, off)
            })
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
            .unwrap();
        assert!(best.0 < 0.01, "the tone survives the filter (mean error {})", best.0);
    }

    #[test]
    fn flac_s32_msb_aligned_output_is_written_bit_exact() {
        // FFmpeg's flac decoder hands 24-bit audio over as S32 shifted up by 8.
        let samples: [i32; 4] = [0x7fff_ff00, i32::MIN, 0x0012_3400, -0x0000_0100];
        let src: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut s32 = Vec::new();
        convert_interleaved(&src, SrcFmt::S32Le, PcmWidth::S32, &mut s32);
        assert_eq!(s32, src);
        let mut s24 = Vec::new();
        convert_interleaved(&src, SrcFmt::S32Le, PcmWidth::S24, &mut s24);
        assert_eq!(s24, [0xff, 0xff, 0x7f, 0x00, 0x00, 0x80, 0x34, 0x12, 0x00, 0xff, 0xff, 0xff]);
        let mut s16 = Vec::new();
        convert_interleaved(&src, SrcFmt::S32Le, PcmWidth::S16, &mut s16);
        assert_eq!(s16, [0xff, 0x7f, 0x00, 0x80, 0x12, 0x00, 0xff, 0xff]);
    }

    #[test]
    fn sixteen_bit_sources_round_trip_at_every_width() {
        let samples: [i16; 3] = [1, -2, i16::MAX];
        let le: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let be: Vec<u8> = samples.iter().flat_map(|v| v.to_be_bytes()).collect();
        let mut a = Vec::new();
        convert_interleaved(&le, SrcFmt::S16Le, PcmWidth::S16, &mut a);
        assert_eq!(a, le);
        let mut b = Vec::new();
        convert_interleaved(&be, SrcFmt::S16Be, PcmWidth::S16, &mut b);
        assert_eq!(b, le);
        let mut c = Vec::new();
        convert_interleaved(&le, SrcFmt::S16Le, PcmWidth::S32, &mut c);
        let back: Vec<i32> = c.chunks_exact(4).map(|x| i32::from_le_bytes([x[0], x[1], x[2], x[3]])).collect();
        assert_eq!(back, [1 << 16, -2 << 16, i32::from(i16::MAX) << 16]);
    }

    #[test]
    fn packed_24_bit_and_float_sources() {
        let s24le = [0x56, 0x34, 0x12, 0xff, 0xff, 0xff];
        let mut out = Vec::new();
        convert_interleaved(&s24le, SrcFmt::S24Le, PcmWidth::S24, &mut out);
        assert_eq!(out, s24le);
        let s24be = [0x12, 0x34, 0x56, 0xff, 0xff, 0xff];
        let mut out = Vec::new();
        convert_interleaved(&s24be, SrcFmt::S24Be, PcmWidth::S24, &mut out);
        assert_eq!(out, s24le);
        let f: Vec<u8> = [1.0f32, -1.0, 0.0, 2.0].iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut out = Vec::new();
        convert_interleaved(&f, SrcFmt::F32Le, PcmWidth::S16, &mut out);
        assert_eq!(out, [0xff, 0x7f, 0x00, 0x80, 0x00, 0x00, 0xff, 0x7f]);
    }

    #[test]
    fn planar_frames_are_interleaved() {
        let l: Vec<u8> = [1i16, 3].iter().flat_map(|v| v.to_le_bytes()).collect();
        let r: Vec<u8> = [2i16, 4].iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut out = Vec::new();
        convert_planar(&[&l, &r], 2, SrcFmt::S16Le, PcmWidth::S16, &mut out);
        let got: Vec<i16> = out.chunks_exact(2).map(|x| i16::from_le_bytes([x[0], x[1]])).collect();
        assert_eq!(got, [1, 2, 3, 4]);
    }

    #[test]
    fn pcm_codec_names_map_to_layouts() {
        assert_eq!(SrcFmt::from_pcm_codec("pcm_s24le"), Some(SrcFmt::S24Le));
        assert_eq!(SrcFmt::from_pcm_codec("pcm_s16be"), Some(SrcFmt::S16Be));
        assert_eq!(SrcFmt::from_pcm_codec("flac"), None);
        assert_eq!(SrcFmt::from_pcm_codec("pcm_bluray"), None);
    }
}
