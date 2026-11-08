//! What this television can DECODE — the device's own codec table, read once at boot.
//!
//! # Why the app asks the device instead of knowing
//!
//! The capability profile used to be a set of constants describing the author's television: HEVC
//! yes, 4K yes, AAC/AC3/EAC3 yes. Every one of those claims is true on the 49SM9000PLA and was
//! asserted to every server as if it were true of webOS. That is the bug class issue #22 named
//! and `docs/plex-pass-audit.md` enumerates — **a claim true on the development environment,
//! asserted as universal** — and the decode-capability assertion was its last standing member
//! (the audit's closing section): a webOS set whose SoC lacks HEVC would be offered, and would
//! direct-play, streams it cannot decode, and nothing here could catch it because the only
//! device this project owns decodes everything the constants claimed.
//!
//! # Where it comes from
//!
//! `/etc/umediaserver/device_codec_capability_config.json` — published by the platform for its
//! own media stack, verified present on the dev set (webOS 4.5): per-codec rows with max
//! width/height/framerate/bitrate for video, channel counts for audio. Read once at boot, the
//! same shape as `webos::probe` (one file read, can never fail the boot, loud line either way).
//!
//! Two facts about the table shape the parse:
//!
//! * **Duplicate names are one codec.** The dev set lists BOTH `"H.265"` (4096x2304@120) and
//!   `"HEVC"` (4096x2176@60) — same decoder, two rows, different numbers. Rows naming the same
//!   codec are merged by MIN on every axis we consume, because over-claiming is the exact
//!   failure mode this module exists to end.
//! * **It cannot express bit depth.** No row carries one, so the profile's `bitDepth=10` claim
//!   stays the constant it always was (see `plex/transcoder.rs::profile_for`) — the table can
//!   neither confirm nor deny it.
//!
//! Parsed with serde_json (already a dependency of the plex layer) rather than `webos.rs`'s
//! hand scanner: this file is nested arrays of objects, not a flat string map. Any parse
//! failure — or a shape we misread — lands on [`Caps::assumed`], never on a refused boot.
//!
//! # The fallback is the old behavior, verbatim
//!
//! When the file is unreadable or unparseable, [`Caps::assumed`] restores EXACTLY the constants
//! that shipped before this module existed — the 49SM9000PLA values, the only set ever measured
//! before this module. Misreading the table must degrade to yesterday's behavior, not invent a
//! weaker television: the issue-#22 lesson cuts both ways, and a client that suddenly transcodes
//! everything on a healthy 4K panel is as wrong as one that direct-plays HEVC to a SoC without it.
use std::borrow::Cow;
use std::sync::OnceLock;

pub mod dv;

/// The AUDIO codec set the buffer-feed PIPELINE decodes. This is the software half of a
/// two-sided test — what our demuxer/payload path can feed, before asking whether this
/// particular SoC can decode it. The live set is [`Caps::audio`] (this list ∩ the device's own
/// codec table), and the ONE-definition rule lives with it: the Normal routing uses
/// `plex::is_dp_audio_track` for membership and channel bounds, shared with the device profile.
/// Forced mode instead uses the implemented software feed formats and its separate profile,
/// without conservative device bounds.
///
/// Defined here, not in `plex`, because this module is the one that intersects it with the
/// device's own table and `plex` sits above the platform layer; `plex` re-exports it
/// (`plex::DP_AUDIO_CODECS`) for the profile string and route's plan.
///
/// FORK (RichiL10): `flac`, `pcm` and `opus` are the fork's own feed lanes. FLAC is decoded in
/// the app and leaves it as PCM, native PCM tracks are converted to the declared sample width,
/// and Opus is fed as-is with its `opusInfo` (`player::engine::with_audio_info`). Each is admitted
/// only with a MEASURED channel row in the device table, like DTS.
///
/// What a set announces is this list intersected with ITS table, with the table's channel counts; a
/// combination measured broken on a firmware family is taken off by [`EXCLUSIONS`], not here.
pub const DP_AUDIO_CODECS: &str = "aac,ac3,eac3,dts,flac,alac,pcm,opus";

/// Auto's channel ceiling for Opus, whatever a table row claims: stereo plays cleanly on every set
/// measured, while 5.1 stalls on a C1 (webOS 6) once packets pass ~1500 bytes, with nothing in the
/// log; the surround stall watchdog (`player::native_stall`) is the second half of that evidence.
/// Wider tracks stay with the server; Force Direct Play applies no table ceiling, as for every
/// codec. (A 5.1 Opus film did play through on a C5, webOS 10.3, so this can loosen per model.)
const OPUS_AUTO_MAX_CHANNELS: u32 = 2;

/// FORK (RichiL10): the lanes that are admitted only with a measured channel ceiling.
const MEASURED_ONLY: [&str; 5] = ["dts", "flac", "alac", "pcm", "opus"];

/// **The audio codecs every webOS TV decodes**, and so the ones no `gstcool.conf` can take away
/// ([`apply_gst_decoders`], upstream #1096 / PR #1133); the assumed fallback states exactly them
/// ([`Caps::assumed`]). The device table stays the authority on a set without one of them.
pub const BASELINE_AUDIO: &[&str] = &["aac", "ac3", "eac3"];

/// FORK (RichiL10): the PCM lane's channel layouts — the `pcmInfo.channelMode` strings the
/// television's pipeline is known to accept (`mono`, `stereo`, `6-channel`).
pub fn pcm_lane_channels_ok(channels: i64) -> bool {
    matches!(channels, 1 | 2 | 6)
}

/// FORK (RichiL10): can the PCM lane declare an MP3/Vorbis source at this rate? The sink knows
/// the source's own rate up to 48 kHz, and a whole-ratio reduction above it (the lane's
/// `Downsampler` takes only integer ratios, exactly as for the lossless lane).
pub fn lossy_rate_ok(rate: i64) -> bool {
    rate <= 0 || rate <= 48_000 || rate % 44_100 == 0 || rate % 48_000 == 0
}

/// FORK (RichiL10): is a DTS track ABOVE the device's DTS ceiling direct-playable? The demuxer
/// always feeds the DTS core and drops the DTS-HD extension (`ff::dts_core_packet`), and the
/// core of a 6.1/7.1 DTS-HD track is itself 5.1 (6.1 with the XCh extension inside it), which
/// is all an optical output can carry anyway. `dts_core=never` restores the old ceiling.
fn dts_over_ceiling_for(knob: Option<&str>) -> bool {
    knob != Some("never")
}

pub fn dts_over_ceiling_admitted() -> bool {
    dts_over_ceiling_for(plx_base::forkknobs::get("dts_core").as_deref())
}

/// FORK (RichiL10): the only sample rate the AC3 encode lane (`ff::enc`) takes. The bundled
/// FFmpeg has no resampler (`--disable-swresample`), and 48 kHz keeps the television's AC3 path
/// on the rate it always plays; any other rate stays with the server.
pub const ENC_LANE_RATE: i64 = 48_000;

/// FORK (RichiL10): the encode lane's `encode` knob: `ac3` (default) or `off`, which is build 3's
/// behaviour exactly (those tracks are converted by the server).
fn encode_lane_for(knob: Option<&str>) -> bool {
    knob != Some("off")
}

pub fn encode_lane_on() -> bool {
    encode_lane_for(plx_base::forkknobs::get("encode").as_deref())
}

/// FORK (RichiL10): which of the fork's feed lanes a track takes, when the device table alone
/// does not decide it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioLane {
    /// FLAC / native PCM inside the PCM lane's admitted channel counts: lossless PCM.
    Pcm,
    /// FLAC / native PCM above them, and TrueHD/MLP: decoded, folded to 5.1(side) and encoded to
    /// AC3 640 kbps in-app (`ff_enc.rs`), fed as AC3.
    Ac3Encode,
}

/// FORK (RichiL10): the key [`DP_AUDIO_CODECS`] and [`Caps::audio`] know a codec by. PMS reports
/// every DTS flavour as `dca` (libavcodec's old decoder name), `dca-ma`, `dca-hra` and friends on
/// a Media whose track is DTS-HD, while this set, the television's codec table ("DTS") and the
/// bundled FFmpeg (`dts`) all say `dts`. Without the fold a DTS track off a real server never
/// matched the set and was always converted by the server. Anything else passes through unchanged.
pub fn dp_audio_key(codec: &str) -> &str {
    if codec.eq_ignore_ascii_case("dca") || codec.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("dca-")) {
        "dts"
    } else {
        codec
    }
}

const CAPS_TABLE: &str = "/etc/umediaserver/device_codec_capability_config.json";

/// One native-audio combination that is **measured broken** on a firmware family, and so is not
/// announced for it although the family's own codec table lists it. An excluded track is
/// converted exactly as one the set never listed: by the server.
///
/// The key is the firmware codename (`tv::device::Info::codename`, nyx's `webos_release_codename`),
/// the one identity the app reads reliably on every set: it groups the sets that run the same
/// build of the media stack, which is what decides the decoder's behaviour, where the retail model
/// name (`product_id`, absent on some firmware) and the SoC class would split one stack or merge two.
/// A row says `max_channels` native channels of `codec`, `0` meaning the codec is not announced at
/// all. A set no row names announces what its table lists; a row only takes away, so an unmeasured
/// family is never promised less than the firmware says.
pub struct Exclusion {
    pub codename: &'static str,
    pub codec: &'static str,
    pub max_channels: u32,
    pub evidence: &'static str,
}

pub const EXCLUSIONS: &[Exclusion] = &[
    // LG C1 (OLED55C14LB), webOS 6.5.3, measured 2026-10-21 and 2026-10-23.
    Exclusion {
        codename: "kisscurl-koli",
        codec: "opus",
        max_channels: 2,
        evidence: "6-channel Opus stalls the pipeline (Load returns, nothing is accepted, no picture) once \
                   packets pass 1500 bytes: CBR files with 1500 B packets play, 1536 B CBR and VBR noise \
                   (2264-3411 B) stall after ~190 AUs; stereo Opus (<=635 B) is clean. Emby topic 132334 \
                   reports corrupted 5.1 Opus on a 2021 webOS 6 LG (43UP78006LB).",
    },
    Exclusion {
        codename: "kisscurl-koli",
        codec: "vorbis",
        max_channels: 0,
        evidence: "the pipeline refuses the first buffers with type=18 num=200 'Audio Codec Not \
                   Supported' (stereo 44.1/48 kHz and 5.1, with the three header packets fed in-band); \
                   libpf's payload has no key for Vorbis headers.",
    },
];

/// Bound the native channels of `codec` in `caps` to `max` (`0`: the codec is not announced at all),
/// never raising what the table said. Returns what was applied as `codec<=N` / `codec=none`, or
/// nothing when the set does not announce the codec.
///
/// FORK (Nuvio Native): `vorbis` and `mp3` are APP-decoded here (the PCM lane, `lossy=on`) and only
/// ever appear in [`Caps::audio`] because of that lane, so an exclusion row naming them would take
/// away a lane the television's pipeline never sees. They are left alone and the row is inert,
/// which is upstream's own answer for a set whose pipeline list does not feed Vorbis.
fn limit_codec(caps: &mut Caps, codec: &str, max: u32) -> Option<String> {
    let id = audio_codec_id(codec).into_owned();
    if !caps.audio_has(&id) || matches!(id.as_str(), "vorbis" | "mp3") {
        return None;
    }
    if max == 0 {
        caps.audio = caps.audio.split(',').filter(|c| *c != id).collect::<Vec<_>>().join(",");
        caps.audio_channels.remove(&id);
        // A measured removal holds in Force Direct Play too: `0` says "nothing native".
        caps.native_ceiling.insert(id, 0);
        return Some(format!("{codec}=none"));
    }
    let limit = caps.audio_channels.get(&id).copied().filter(|l| *l > 0).map_or(max, |l| l.min(max));
    caps.audio_channels.insert(id.clone(), limit);
    // Force Direct Play applies no table ceiling, but it does honour a measured one.
    let native = caps.native_ceiling.get(&id).copied().map_or(max, |l| l.min(max));
    caps.native_ceiling.insert(id, native);
    Some(format!("{codec}<={limit}"))
}

/// Take the exclusions for `codename` off `caps`, returning what was applied as `codec<=N` or
/// `codec=none`. `ignore` (the debug flag `/tmp/plxnative-nativeaudiowide`) applies none, so a
/// combination the table keeps off the pipeline can be tried on a set.
pub fn apply_exclusions(caps: &mut Caps, codename: &str, ignore: bool, table: &[Exclusion]) -> Vec<String> {
    if ignore || codename.is_empty() {
        return Vec::new();
    }
    table
        .iter()
        .filter(|e| e.codename.eq_ignore_ascii_case(codename))
        .filter_map(|e| limit_codec(caps, e.codec, e.max_channels))
        .collect()
}

/// A native audio combination this install **watched the pipeline refuse**, remembered so the next
/// play goes straight to conversion. The install preference `native_audio_refused`
/// (`plex::session`) is the one list of them; this is the shape it hands over
/// ([`project_stored_refusals`]) and receives back ([`learn_refusal`]). `channels` is the refused
/// track's channel count.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Refusal {
    pub firmware: String,
    pub codec: String,
    pub channels: u32,
    /// The firmware BUILD the refusal was watched on (`tv::device::Info::build`; empty for an entry
    /// stored before builds were recorded). A different build is a different decoder, so a refusal
    /// is evidence only on the build it was watched on ([`Refusal::watched_on`]).
    pub build: String,
}

impl Refusal {
    /// Whether this refusal says anything about a set running `build`. An LG update replaces the
    /// decoder a refusal was watched on, so another build's entry (and an older record's, which
    /// never named one) is not evidence and the new firmware gets a fresh native try. An unknown
    /// running build (no nyx file) judges nothing, so everything counts.
    pub fn watched_on(&self, build: &str) -> bool {
        build.is_empty() || self.build == build
    }
}

/// Take the learned refusals for `codename` off `caps`, as [`apply_exclusions`] does. A refusal of a
/// stereo (or mono) track takes the codec away altogether; a refusal of a surround track keeps the
/// codec to stereo, which is what the static rows do for the same measurements. `ignore` is the debug
/// flag again.
pub fn apply_learned(caps: &mut Caps, codename: &str, ignore: bool, learned: &[Refusal]) -> Vec<String> {
    if ignore || codename.is_empty() {
        return Vec::new();
    }
    learned
        .iter()
        .filter(|r| r.firmware.eq_ignore_ascii_case(codename))
        .filter_map(|r| limit_codec(caps, &r.codec, if r.channels <= 2 { 0 } else { 2 }))
        .collect()
}

/// gst's rank file: which decoder elements the firmware leaves switched on.
const GST_CONF: &str = "/etc/gst/gstcool.conf";

/// For each audio codec the pipeline can be fed, the gst decoder elements that can decode it: LG's
/// hardware `<codec>_audiodec` FIRST (the one [`apply_gst_decoders`] needs listed off before it drops
/// a codec), then the software decoders (`avdec_dca` is DTS's, which webosbrew's `dts_restore`
/// re-ranks). Codec ids are [`audio_codec_id`]'s.
const DECODER_ELEMENTS: &[(&str, &[&str])] = &[
    ("aac", &["aac_audiodec", "avdec_aac", "faad"]),
    ("ac3", &["ac3_audiodec", "avdec_ac3"]),
    ("eac3", &["ac3_audiodec", "avdec_eac3"]),
    ("dts", &["dts_audiodec", "avdec_dca"]),
    ("opus", &["opus_audiodec", "opusdec", "avdec_opus"]),
    ("vorbis", &["vorbis_audiodec", "vorbisdec", "ivorbisdec"]),
];

/// `[rank]` and `[sw_decoder]` of `gstcool.conf` as `element -> rank` (the later line wins). Other
/// sections, comments and lines that are not `name=integer` are ignored.
pub fn parse_gst_ranks(text: &str) -> std::collections::BTreeMap<String, i64> {
    let mut ranks = std::collections::BTreeMap::new();
    let mut wanted = false;
    for line in text.lines() {
        let line = line.trim();
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            wanted = matches!(section.trim(), "rank" | "sw_decoder");
        } else if wanted {
            if let Some((name, value)) = line.split_once('=') {
                if let Ok(rank) = value.trim().parse::<i64>() {
                    ranks.insert(name.trim().to_string(), rank);
                }
            }
        }
    }
    ranks
}

/// Drop from `caps` the audio codecs that `gstcool.conf` **positively lists as switched off**, and
/// say what happened as `(dropped, kept because the file never lists their hardware decoder)`.
///
/// The device table says what the hardware block can do; this file says which gst decoder elements
/// the firmware left switched on. **A missing line is never evidence**: it means the default rank.
/// The software decoders (`avdec_*`, `faad`) sit at rank 0 in every conf read, because the pipeline
/// uses LG's hardware elements, so they say nothing on their own (an LG B4 on webOS 10.3.1 lists only
/// them and no `*_audiodec` line at all, and plays AAC, AC3, EAC3 and DTS: upstream #1096). A codec
/// is dropped only when its OWN hardware element (`DECODER_ELEMENTS`' first, e.g. `dts_audiodec`) is
/// LISTED at rank `<= 0` and every other listed element of it is `<= 0` too (so `dts_restore` giving
/// `avdec_dca` a rank again saves DTS), and never when it is in [`BASELINE_AUDIO`]. An empty or
/// unreadable file therefore changes nothing. The channel ceiling goes with the codec, so Force
/// Direct Play's "DTS needs a surviving table row" rule sees the same answer.
///
/// **The first version of this rule was wrong and shipped once** (upstream package 12, #1096): it
/// dropped every codec whose listed decoders were all rank 0, which on a set that lists only the
/// software decoders took away `aac`, `ac3`, `eac3` and `dts` — measured on the owner's B4. On this
/// fork that is worse than upstream, because the server cannot convert: the AC3 encode lane turns
/// off with the `ac3` row and every non-direct-playable track becomes a failed route.
pub fn apply_gst_decoders(caps: &mut Caps, conf: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let ranks = parse_gst_ranks(conf);
    let (mut dropped, mut unlisted) = (Vec::new(), Vec::new());
    for (codec, elements) in DECODER_ELEMENTS {
        // FORK (Nuvio Native): app-decoded codecs never reach the pipeline natively.
        if matches!(*codec, "vorbis" | "mp3") {
            continue;
        }
        if !caps.audio_has(codec) {
            continue;
        }
        let Some(&hardware) = ranks.get(elements[0]) else {
            unlisted.push(*codec);
            continue;
        };
        let listed_off = elements.iter().filter_map(|e| ranks.get(*e)).all(|r| *r <= 0);
        if hardware <= 0 && listed_off && !BASELINE_AUDIO.contains(codec) {
            caps.audio = caps.audio.split(',').filter(|c| *c != *codec).collect::<Vec<_>>().join(",");
            caps.audio_channels.remove(*codec);
            caps.native_ceiling.insert((*codec).to_string(), 0);
            dropped.push(*codec);
        }
    }
    (dropped, unlisted)
}

/// The decode-capability snapshot the playback stack derives from. `OnceLock` for the same
/// reason as `tv::device::Info`: written exactly once, at boot, then read on every play decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    /// The SoC decodes HEVC. Gates the `hevc` arm of route.rs's direct-play test and the
    /// hevc entries in BOTH profile codec lists (h264 needs no flag — every webOS SoC has it).
    pub hevc: bool,
    /// The video width/height upper bound — the per-axis MIN across the merged H.264 and HEVC
    /// rows, because both consumers apply one bound to EVERY codec at once: the profile's
    /// `*`-scoped limitation (a 1080p-only SoC must tell PMS 1920x1088 so 4K sources transcode
    /// down) and route.rs's local direct-play resolution gate. Taking the HEVC row alone
    /// whenever one existed — as this first shipped — assumed the dev set's shape ("HEVC is the
    /// tighter of the two decoders") as if it were universal: on a SoC with a 4K HEVC decoder
    /// beside 1080p-limited AVC (a real SoC shape) it advertised a 4K bound for h264, and PMS
    /// would direct-play 4K H.264 onto a decoder that cannot handle it — issue #22's over-claim
    /// class, reintroduced by the module built to end it. The dev set's bound is unchanged:
    /// min((4096,2304), (4096,2176)) = (4096,2176).
    pub hevc_max: (u32, u32),
    /// **Per-codec `(maxWidth, maxHeight, maxFrameRate)` as the table states them**, H.264 and HEVC
    /// respectively, duplicate rows MIN-merged per axis, `0` on any axis the table did not say.
    /// Added 2026-09-03 for ONE consumer — `engine::sink_envelope`, the Starfish Load's
    /// `adaptiveStreaming` ceiling — and clamped there only when [`measured`] is true: the assumed
    /// fallback is `(0, 0, 0)` on purpose, so an unreadable table never clamps anything. The
    /// frame rate is the axis `docs/webos10-resource-allocation.md` names as the likely
    /// discriminator on webOS 10 (the raster was not: that set's own table claims 4096x2176 for
    /// H.264 too); this module used to drop it by design, and `hevc_max` above still folds both
    /// codecs into one raster bound for the PMS profile and the direct-play gate, unchanged.
    pub h264_row: (u32, u32, u32),
    pub hevc_row: (u32, u32, u32),
    /// The device table lists a VP9 decoder row. False in `assumed()` and on every table without
    /// the row, which keeps VP9 a server job there. Until VP9 direct play this was a diagnostic
    /// only and `assumed()` claimed it; now it gates the feed, so an unreadable table must not.
    pub vp9: bool,
    /// The VP9 row's (max width, max height, ceil max frame rate), MIN-merged across duplicate
    /// rows; 0 = not stated. Read only when `vp9`.
    pub vp9_row: (u32, u32, u32),
    /// The deepest VP9 bit depth the profile claims: 10 where a 10-bit VP9 stream has been measured
    /// to decode with the right levels AND to reach HDR when its container says PQ, 8 everywhere
    /// else (the table has no bit-depth axis, so this is device evidence, not table data). Only the
    /// webOS 4 family (ACB video path) is measured, so [`vp9_bit_depth_for`] is the one place the
    /// 10 comes from; the assumed profile and a table read on any other set say 8.
    pub vp9_bit_depth: u32,
    /// The device table lists an AV1 decoder row. False in `assumed()` and on every table without
    /// the row, which keeps AV1 a server transcode there. Issue #593.
    pub av1: bool,
    /// The AV1 row's (max width, max height, ceil max frame rate), MIN-merged across duplicate
    /// rows; 0 = not stated. Read only when `av1`.
    pub av1_row: (u32, u32, u32),
    /// The direct-playable AUDIO subset: [`DP_AUDIO_CODECS`] (what the pipeline decodes)
    /// intersected with the table's audio rows, in `DP_AUDIO_CODECS`'s own URL form/order.
    /// Normal routing reads this and the channel ceilings through `plex::is_dp_audio_track`,
    /// as does its PMS profile. Forced mode starts from the pipeline's feed set with no channel
    /// ceiling and admits DTS only with a DTS row in `audio_channels` (`Caps::forced_feeds_audio`).
    pub audio: String,
    /// Nonzero, per-codec channel ceilings from the device table, duplicate rows MIN-merged.
    /// Missing legacy limits remain unknown; DTS is admitted only with a measured limit.
    pub audio_channels: std::collections::BTreeMap<String, u32>,
    /// Per-codec channel ceilings that were **measured** on this firmware ([`EXCLUSIONS`]) or
    /// learned from a refusal, as opposed to the table's. Force Direct Play applies no table
    /// ceiling but does honour these ([`Caps::forced_feeds_audio_track`]).
    pub native_ceiling: std::collections::BTreeMap<String, u32>,
}

impl Caps {
    /// The 49SM9000PLA values — the only set ever measured before this module. Byte-for-byte,
    /// these reproduce the profile the app has always sent (the transcoder test pins that), so
    /// an unreadable table changes nothing for any device that was working yesterday.
    pub fn assumed() -> Caps {
        Caps {
            hevc: true,
            hevc_max: (3840, 2176),
            h264_row: (0, 0, 0),
            hevc_row: (0, 0, 0),
            vp9: false,
            vp9_row: (0, 0, 0),
            vp9_bit_depth: 8,
            av1: false,
            av1_row: (0, 0, 0),
            audio: BASELINE_AUDIO.join(","),
            audio_channels: Default::default(),
            native_ceiling: Default::default(),
        }
    }

    /// Membership test on [`Caps::audio`]. `codec` is any spelling of the name
    /// ([`audio_codec_id`]): PMS says `dca` where the table and the pipeline say `dts`.
    pub fn audio_has(&self, codec: &str) -> bool {
        let id = audio_codec_id(codec);
        self.audio.split(',').any(|c| c == id)
    }

    /// Local counterpart of the profile's codec-scoped audio.channels limitations.
    pub fn audio_supports(&self, codec: &str, channels: i64) -> bool {
        self.audio_supports_at(codec, channels, 0)
    }

    /// FORK (RichiL10): [`Caps::audio_supports`] with the track's sample rate (Hz, 0 = not
    /// known), which the AC3 encode lane needs: it takes 48 kHz only. An unknown rate is admitted
    /// here, and the Load (which declares from the track's PMS facts) refuses it if it is wrong.
    pub fn audio_supports_at(&self, codec: &str, channels: i64, rate: i64) -> bool {
        self.audio_supports_lanes(codec, channels, rate, dts_over_ceiling_admitted(), encode_lane_on())
    }

    /// [`Caps::audio_supports`] with the `dts_core` knob's answer passed in and the encode lane
    /// off (pure, for tests): the answers before the encode lane existed.
    #[cfg(test)]
    fn audio_supports_with(&self, codec: &str, channels: i64, dts_over_ceiling: bool) -> bool {
        self.audio_supports_lanes(codec, channels, 0, dts_over_ceiling, false)
    }

    /// The whole rule, every knob passed in (pure).
    fn audio_supports_lanes(&self, codec: &str, channels: i64, rate: i64, dts_over_ceiling: bool, encode: bool) -> bool {
        let codec = dp_audio_key(codec);
        // FORK (RichiL10): a track the encode lane takes is direct-playable at 48 kHz.
        if self.enc_lane_candidate(codec, channels, encode) {
            return rate == 0 || rate == ENC_LANE_RATE;
        }
        if !self.audio_has(codec) {
            return false;
        }
        // FORK (RichiL10): the PCM lane can only declare mono, stereo or 6-channel.
        if matches!(codec, "flac" | "alac" | "pcm") && !pcm_lane_channels_ok(channels) {
            return false;
        }
        // FORK (RichiL10): MP3/Vorbis are declared stereo at a rate the PCM sink knows: the
        // source's own rate up to 48 kHz, or a whole-ratio reduction of it (`lossy_rate_ok`).
        if matches!(codec, "mp3" | "vorbis") && !lossy_rate_ok(rate) {
            return false;
        }
        match self.audio_channels.get(codec) {
            // FORK (RichiL10): DTS above the ceiling plays as its core (see dts_over_ceiling_for).
            Some(&limit) if codec == "dts" && channels > i64::from(limit) => dts_over_ceiling,
            Some(&limit) => {
                (channels > 0 || !MEASURED_ONLY.contains(&codec)) && channels <= i64::from(limit)
            }
            None => !MEASURED_ONLY.contains(&codec),
        }
    }

    /// FORK (RichiL10): can this television take the encode lane's OUTPUT at all? AC3 5.1, from
    /// a MEASURED AC3 row of at least six channels (like every other fork lane, never assumed).
    fn enc_sink_ok(&self) -> bool {
        self.audio_has("ac3") && self.audio_channels.get("ac3").is_some_and(|&l| l >= 6)
    }

    /// FORK (RichiL10): the PCM lane's own admission for a FLAC / native PCM track: the codec is
    /// admitted, the channel count is one the lane declares, and it fits the device's row.
    fn pcm_lane_admits(&self, codec: &str, channels: i64) -> bool {
        self.audio_has(codec)
            && pcm_lane_channels_ok(channels)
            && self.audio_channels.get(codec).is_some_and(|&l| channels <= i64::from(l))
    }

    /// FORK (RichiL10): would this track take the AC3 encode lane, its sample rate aside?
    ///
    /// | track | lane |
    /// |---|---|
    /// | FLAC/PCM inside the PCM lane's admitted channel counts | PCM lane (lossless) |
    /// | FLAC/PCM with 3 to 8 channels the PCM lane does not admit | encode lane |
    /// | TrueHD/MLP, any channel count | encode lane |
    /// | DTS, any channel count | encode lane |
    ///
    /// FLAC/PCM follow their own admission (`flac=off` hands them back to the server too); TrueHD
    /// and MLP are not in any television's table and need only the AC3 sink.
    ///
    /// **DTS is the fork's oldest lane tenant and the merge dropped it** (2026-11-01): upstream's
    /// newer policy direct-plays the DTS core and encodes nothing, which is the right answer on
    /// the set it was measured on. On the dev set (webOS 10, LG's table lists a `dts` row) a
    /// native DTS Load is accepted and then never configures the audio sink — `smp_cb type=8
    /// audio/x-dts` arrives, no `type=7` out-channels ever does, no `FRAMEREADY` tick is produced,
    /// and with `audioSync` the video sink slaves to that dead audio clock: black screen, audio
    /// queue filling and then stalled at ~2 s. That is exactly the failure this lane exists to
    /// avoid, so DTS comes back to the lane.
    pub fn enc_lane_candidate(&self, codec: &str, channels: i64, encode: bool) -> bool {
        if !encode || !self.enc_sink_ok() {
            return false;
        }
        match dp_audio_key(codec) {
            "truehd" | "mlp" => channels >= 0,
            "dts" => true,
            // FORK (Nuvio Native): a surround Opus track. Stereo Opus is the one width measured
            // clean on LG's own decoder on every set, and it stays native; anything wider is
            // decoded by the bundled FFmpeg and folded to 5.1 AC3 here, because the alternative on
            // this product is silence (a C1 stalls above 1500 B packets, and a set whose decoder
            // refuses it silently gets no picture at all) and the loopback shim has no transcoder
            // to fall back on. 48 kHz only, which is all Opus ever carries.
            "opus" => channels > 2,
            c @ ("flac" | "alac" | "pcm") => {
                self.audio_has(c) && (3..=8).contains(&channels) && !self.pcm_lane_admits(c, channels)
            }
            _ => false,
        }
    }

    /// FORK (RichiL10): the fork lane for a track of `rate` Hz (0 = not known), or `None` for a
    /// codec the television decodes itself or a track no lane takes. Pure; `encode` is the knob.
    pub fn audio_lane(&self, codec: &str, channels: i64, rate: i64, encode: bool) -> Option<AudioLane> {
        if self.enc_lane_candidate(codec, channels, encode) {
            return (rate == 0 || rate == ENC_LANE_RATE).then_some(AudioLane::Ac3Encode);
        }
        let codec = dp_audio_key(codec);
        if matches!(codec, "mp3" | "vorbis") {
            return (self.audio_has(codec) && lossy_rate_ok(rate)).then_some(AudioLane::Pcm);
        }
        (matches!(codec, "flac" | "alac" | "pcm") && self.pcm_lane_admits(codec, channels)).then_some(AudioLane::Pcm)
    }

    /// FORK (RichiL10): the codecs the encode lane adds to the PMS direct-play profile beyond
    /// [`Caps::audio`]. TrueHD/MLP are in no television table, and DTS is admitted whether or not
    /// the set lists a `dts` row — the lane is the only path this fork has verified for it. Empty
    /// when the lane is off.
    pub fn enc_lane_profile_codecs(&self, encode: bool) -> &'static [&'static str] {
        if encode && self.enc_sink_ok() {
            // FORK (Nuvio Native): `opus` joins them at 6 channels — the lane is what makes a
            // surround Opus track audible on a set whose own decoder stalls or refuses it.
            &["truehd", "mlp", "dts", "opus"]
        } else {
            &[]
        }
    }

    /// FORK (RichiL10): the `audio.channels` upper bound the PMS profile states for `codec`, or
    /// None for no limitation. The device row, except DTS while its above-ceiling tracks are
    /// admitted: then PMS must be allowed to direct-play a 7.1 DTS-HD track (8 channels), because
    /// the app feeds its core.
    pub fn profile_channel_limit(&self, codec: &str) -> Option<u32> {
        self.profile_channel_limit_with(codec, encode_lane_on())
    }

    /// [`Caps::profile_channel_limit`] with the `encode` knob passed in: while the encode lane is
    /// on, FLAC and PCM (when admitted at all) and TrueHD/MLP/DTS are stated up to 8 channels, so
    /// MDE answers direct play for the tracks the app now converts itself.
    pub fn profile_channel_limit_with(&self, codec: &str, encode: bool) -> Option<u32> {
        let codec = dp_audio_key(codec);
        // FORK (RichiL10): an app-decoded MP3/Vorbis track plays downmixed to stereo, so the
        // profile must not refuse the 5.1 Vorbis the television would have converted itself.
        if matches!(codec, "mp3" | "vorbis") && self.audio_has(codec) {
            return Some(8);
        }
        if encode && self.enc_sink_ok() {
            if matches!(codec, "truehd" | "mlp" | "dts" | "opus")
                || (matches!(codec, "flac" | "alac" | "pcm") && self.audio_has(codec))
            {
                return Some(8);
            }
        }
        let limit = self.audio_channels.get(codec).copied()?;
        if codec == "dts" && dts_over_ceiling_admitted() {
            Some(limit.max(8))
        } else {
            Some(limit)
        }
    }

    /// Force Direct Play's audio gate: the pipeline's feed formats ([`DP_AUDIO_CODECS`]), with
    /// no channel ceiling. DTS additionally needs the evidence Auto asks for
    /// ([`Caps::forced_admits_dts`]). The planner and the forced profile both read this.
    pub fn forced_feeds_audio(&self, codec: &str) -> bool {
        let id = audio_codec_id(codec);
        // A codec a measurement took away (an exclusion row, a learned refusal) is not forced in
        // either: the pipeline was watched refusing it on THIS firmware.
        if self.native_ceiling.get(id.as_ref()) == Some(&0) {
            return false;
        }
        if id == "dts" {
            return self.forced_admits_dts();
        }
        DP_AUDIO_CODECS.split(',').any(|c| c == id)
    }

    /// [`Caps::forced_feeds_audio`] for one TRACK: Force Direct Play applies no table ceiling, but a
    /// ceiling measured on this firmware ([`EXCLUSIONS`], or a refusal learned from it) holds, so a
    /// 5.1 Opus track that stalls a C1's pipeline is not forced into it. `channels` 0 = not known.
    pub fn forced_feeds_audio_track(&self, codec: &str, channels: i64) -> bool {
        if !self.forced_feeds_audio(codec) {
            return false;
        }
        match self.native_ceiling.get(audio_codec_id(codec).as_ref()) {
            Some(&0) => false,
            Some(&max) => channels <= i64::from(max),
            None => true,
        }
    }

    /// The Forced-mode DTS rule, one predicate so the owner can flip it: the table has a DTS
    /// row with a stated channel count (an absent decoder would hang the Load like #584 instead
    /// of refusing). The channel ceiling itself is not applied.
    pub fn forced_admits_dts(&self) -> bool {
        self.audio_channels.contains_key("dts")
    }

    /// The transcode-target chain's HEAD — the codec the profile asks PMS to ENCODE when a
    /// source must re-encode: `hevc` where the SoC decodes it (keeps 4K + HDR10 through a
    /// transcode), else `h264`. ONE definition on purpose: route.rs's /decision-unreachable
    /// Load-payload guess and retranscode's codec record must name the codec the profile
    /// actually asked the server to produce, and as three hand-copied `if caps().hevc`
    /// expressions they could drift — a Load payload naming a codec the profile never requested
    /// is exactly the payload/output mismatch of docs/plex-pass-audit.md row 1 (issue #22).
    /// `transcoder.rs::profile_for` builds the target list's head from this, and its test pins
    /// the agreement.
    pub fn encode_vcodec(&self) -> &'static str {
        if self.hevc {
            "hevc"
        } else {
            "h264"
        }
    }
}

/// Did [`probe`] READ the table, or is [`caps`] returning [`Caps::assumed`]?
///
/// The two are indistinguishable from the values alone — on the 49SM9000PLA they are equal by
/// construction, and on every other set the fallback silently claims that set's profile. The event
/// log says which happened, in one line at boot, and that was enough while a log was the only
/// consumer. It stopped being enough when the diagnostics read-out started printing these values:
/// its output format is a PHOTOGRAPH, from a television nobody here owns, and a panel that prints
/// the dev set's decoder profile as if it had measured the reporter's is the [[silent-instrument-
/// trap]] shape exactly — an instrument that cannot say it did not measure.
///
/// **`false` when `probe` has not run**, which covers host tests as well as a boot that never got
/// here: "not measured" is the honest reading of both.
static MEASURED: OnceLock<bool> = OnceLock::new();

/// What this device decodes. Falls back to [`Caps::assumed`] if [`probe`] has not run — the
/// boot calls it first, so that path exists for host tests, not the TV. The answer can change
/// afterwards, only by losing native audio the pipeline was watched refusing
/// ([`learn_refusal`], [`project_stored_refusals`]), so a decision that must hold across a whole
/// attempt reads it once.
pub fn caps() -> std::sync::Arc<Caps> {
    with_layers(|l| l.caps.read().unwrap_or_else(|e| e.into_inner()).clone())
        .unwrap_or_else(|| ASSUMED.get_or_init(|| std::sync::Arc::new(Caps::assumed())).clone())
}

/// `nativeaudiowide` ignores the learned refusals as it ignores the static rows; `nativeaudiolearn`
/// beside it keeps the learned layer (and the learning) on.
fn ignores_learned(wide: bool, learn: bool) -> bool {
    wide && !learn
}

/// This set's audio capability answer and what derives it. [`probe`] builds it once: the table,
/// minus the codecs gstcool.conf shows nothing can decode, minus [`EXCLUSIONS`]. The one thing that
/// moves afterwards is [`Layers::caps`], and only one way: it LOSES native audio as refusals arrive
/// ([`Layers::fold`]). The list of refusals is not kept here. The install preference
/// `native_audio_refused` (`plex::session`) owns it, and this holds the answer derived from it.
struct Layers {
    /// What [`caps`] answers. Replaced, never edited, so a reader holding the `Arc` keeps one
    /// consistent snapshot and nothing is leaked for it.
    caps: std::sync::RwLock<std::sync::Arc<Caps>>,
    codename: String,
    /// The running firmware build (`tv::device::Info::build`; empty when the set did not say).
    build: String,
    wide: bool,
    /// `plxnative-nativeaudiolearn` beside `wide`: the learned refusals still apply and are still
    /// learned, so the learn-and-convert path can be shown on a set the exclusions would keep off it.
    learn: bool,
    /// `plxnative-nativeaudioforget`: the stored refusals are not applied this run, so a set can be
    /// shown learning from nothing. A refusal learned during the run still applies at once.
    forget: bool,
}

impl Layers {
    fn new(caps: Caps, codename: String, build: String, wide: bool, learn: bool, forget: bool) -> Layers {
        Layers { caps: std::sync::RwLock::new(std::sync::Arc::new(caps)), codename, build, wide, learn, forget }
    }

    /// Take away from the answer whatever `refusals` (this firmware's, this build's) say the pipeline
    /// cannot take, and report whether that took anything. Monotone and idempotent: it only ever
    /// removes, so a list that is behind (a record read before a refusal's write landed) cannot give
    /// back what an earlier call took, and applying the same list twice is a no-op.
    fn fold(&self, refusals: &[Refusal]) -> bool {
        let current: Vec<Refusal> = refusals.iter().filter(|r| r.watched_on(&self.build)).cloned().collect();
        // The write lock spans the read-modify-write so two folds cannot lose each other's removal.
        let mut answer = self.caps.write().unwrap_or_else(|e| e.into_inner());
        let mut next = Caps::clone(&answer);
        let applied = apply_learned(&mut next, &self.codename, ignores_learned(self.wide, self.learn), &current);
        if next == **answer {
            return false;
        }
        plx_base::eventlog::log(&format!(
            "devcaps: firmware={:?} learned refusals: {} -> audio={}",
            self.codename,
            applied.join(" "),
            next.audio
        ));
        *answer = std::sync::Arc::new(next);
        true
    }
}

/// The boot answer, set once by [`probe`].
static BOOT: OnceLock<Layers> = OnceLock::new();
static ASSUMED: OnceLock<std::sync::Arc<Caps>> = OnceLock::new();

/// Run `f` on this set's layers: the test thread's own when a test installed one, else the boot's.
/// `None` before [`probe`] has run (every host test that installed nothing).
fn with_layers<R>(f: impl FnOnce(&Layers) -> R) -> Option<R> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(layers) = THREAD_LAYERS.with(|slot| slot.borrow().clone()) {
        return Some(f(&layers));
    }
    BOOT.get().map(f)
}

/// Apply the refusals the install's stored record holds to the answer ([`Layers::fold`]). The plex
/// session calls it with every record it installs, under its cache lock, so it does no IO and takes
/// no lock the session could hold. `plxnative-nativeaudioforget` applies none. True when it took
/// something away.
pub fn project_stored_refusals(stored: &[Refusal]) -> bool {
    with_layers(|l| !l.forget && l.fold(stored)).unwrap_or(false)
}

/// The pipeline refused `codec` with `channels` channels on this firmware, fed natively: take it
/// off the answer at once (the recovery that follows plans against the new answer) and return the
/// entry for the caller to persist. `None` when this set cannot learn (no firmware name, the debug
/// flag, no [`probe`]), or the combination changes nothing: the codec is not announced to that
/// many channels any more, so the same play converting it would not be this refusal.
pub fn learn_refusal(codec: &str, channels: u32) -> Option<Refusal> {
    with_layers(|l| {
        if ignores_learned(l.wide, l.learn) || l.codename.is_empty() {
            return None;
        }
        // The pipeline's spelling (PMS says `dca` where the table and the pipeline say `dts`).
        let refusal = Refusal {
            firmware: l.codename.clone(),
            codec: audio_codec_id(codec).into_owned(),
            channels,
            build: l.build.clone(),
        };
        l.fold(std::slice::from_ref(&refusal)).then_some(refusal)
    })
    .flatten()
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static THREAD_LAYERS: std::cell::RefCell<Option<std::sync::Arc<Layers>>> = const { std::cell::RefCell::new(None) };
}

/// Make [`caps`] answer `caps` on THIS test thread until the guard drops (the
/// `i18n::language_on_this_thread_for_test` precedent). Tests run in parallel and the
/// process-wide answer is write-once, so a thread-local door is how one test plays a set whose
/// table lists a DTS decoder. The set has no firmware name, so it learns nothing
/// ([`firmware_on_this_thread_for_test`] is the one that does).
#[cfg(any(test, feature = "test-support"))]
pub fn caps_on_this_thread_for_test(caps: Caps) -> CapsGuard {
    firmware_on_this_thread_for_test("", "", caps)
}

/// [`caps_on_this_thread_for_test`] for a set that names its firmware and build, so the thread
/// can learn a refusal ([`learn_refusal`]) and be handed a stored record
/// ([`project_stored_refusals`]) as a television would.
#[cfg(any(test, feature = "test-support"))]
pub fn firmware_on_this_thread_for_test(codename: &str, build: &str, caps: Caps) -> CapsGuard {
    let layers = Layers::new(caps, codename.to_owned(), build.to_owned(), false, false, false);
    CapsGuard(THREAD_LAYERS.with(|slot| slot.replace(Some(std::sync::Arc::new(layers)))))
}

#[cfg(any(test, feature = "test-support"))]
pub struct CapsGuard(Option<std::sync::Arc<Layers>>);

#[cfg(any(test, feature = "test-support"))]
impl Drop for CapsGuard {
    fn drop(&mut self) {
        THREAD_LAYERS.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

/// The one canonical spelling of an AUDIO codec name, for every comparison between vocabularies:
/// lowercase, PMS's `dca` is the pipeline's `dts`, and `ec-3` is `eac3`. Applied at membership
/// ([`Caps::audio_has`]) and at the Load payload; inbound data stays raw (track memory compares
/// the raw string).
pub fn audio_codec_id(codec: &str) -> Cow<'_, str> {
    if codec.bytes().any(|b| b.is_ascii_uppercase()) {
        return Cow::Owned(audio_codec_id(&codec.to_ascii_lowercase()).into_owned());
    }
    match codec {
        "dca" => Cow::Borrowed("dts"),
        // FORK (RichiL10): DTS-HD flavours (`dca-ma`, `dca-hra`, ...) are DTS too (`dp_audio_key`).
        c if c.starts_with("dca-") => Cow::Borrowed("dts"),
        "ec-3" => Cow::Borrowed("eac3"),
        _ => Cow::Borrowed(codec),
    }
}

/// One codec name, canonicalized: the table writes `"H.264"`, `"H.265"`, `"HEVC"`, `"EAC3"`;
/// dropping dots/dashes and case folds the spelling variants ("H265", "E-AC3") onto one key
/// so a firmware's formatting choice cannot un-recognize a codec.
fn canon(name: &str) -> String {
    name.chars()
        .filter(|c| !matches!(c, '.' | '-'))
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Per-axis conservative merge: min of the nonzero values, 0 (= the row didn't say) only when
/// neither row said. A row missing its dimensions must not zero out one that has them.
fn min_nz(a: u32, b: u32) -> u32 {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (a, b) => a.min(b),
    }
}

/// The table's shape, structurally: unknown fields (maxBitRate, the license blurb) are
/// ignored by serde, and every field is defaulted so one malformed row degrades to "row said
/// nothing" instead of failing the whole parse. `maxFrameRate` was in that ignored list until
/// 2026-09-03; it is read now for the per-codec rows and nothing else.
#[derive(serde::Deserialize)]
struct Table {
    #[serde(default, rename = "videoCodecs")]
    video_codecs: Vec<VideoRow>,
    #[serde(default, rename = "audioCodecs")]
    audio_codecs: Vec<AudioRow>,
}
#[derive(serde::Deserialize)]
struct VideoRow {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "maxWidth")]
    max_width: u32,
    #[serde(default, rename = "maxHeight")]
    max_height: u32,
    /// A float on the wire is tolerated and rounded UP (59.94 → 60): this is a ceiling the
    /// stream has to fit under, and flooring it would clamp a 59.94 stream to 59. The table on
    /// the dev set writes integers.
    #[serde(default, rename = "maxFrameRate")]
    max_frame_rate: f64,
}
#[derive(serde::Deserialize)]
struct AudioRow {
    #[serde(default)]
    name: String,
    #[serde(default)]
    channels: u32,
}

/// Everything [`probe`] extracts, as a pure function of the file's text — testable without a
/// filesystem. `None` means "this is not a codec table we understand" — bad JSON, no video rows,
/// no H.264 row (every webOS SoC decodes it), or no stated width/height bound — and the caller
/// falls back to [`Caps::assumed`] WHOLE: a misread must never splice assumed numbers into
/// table-derived ones (that is how "(device table)" once logged invented values).
fn parse(s: &str) -> Option<Caps> {
    let t: Table = serde_json::from_str(s).ok()?;
    if t.video_codecs.is_empty() {
        // A table that names no video codec is a shape we misread, not a TV that decodes
        // nothing — fall back whole rather than deriving a client that transcodes everything.
        return None;
    }
    let (mut h264, mut hevc, mut vp9, mut av1) = (false, false, false, false);
    let (mut hevc_wh, mut h264_wh, mut vp9_wh, mut av1_wh) =
        ((0u32, 0u32), (0u32, 0u32), (0u32, 0u32), (0u32, 0u32));
    let (mut hevc_fps, mut h264_fps, mut vp9_fps, mut av1_fps) = (0u32, 0u32, 0u32, 0u32);
    for row in &t.video_codecs {
        let wh = (row.max_width, row.max_height);
        let fps = if row.max_frame_rate.is_finite() && row.max_frame_rate > 0.0 {
            row.max_frame_rate.ceil() as u32
        } else {
            0
        };
        match canon(&row.name).as_str() {
            // "H.265" and "HEVC" are duplicate rows for one decoder — MIN on every axis.
            "h265" | "hevc" => {
                hevc = true;
                hevc_wh = (min_nz(hevc_wh.0, wh.0), min_nz(hevc_wh.1, wh.1));
                hevc_fps = min_nz(hevc_fps, fps);
            }
            "h264" => {
                h264 = true;
                h264_wh = (min_nz(h264_wh.0, wh.0), min_nz(h264_wh.1, wh.1));
                h264_fps = min_nz(h264_fps, fps);
            }
            // VP9 and AV1 are capability bits with their own bounds. Neither feeds `hevc_max`, the
            // H.264 guard or the audio logic: those stay exactly as the table without them
            // computes them.
            "vp9" => {
                vp9 = true;
                vp9_wh = (min_nz(vp9_wh.0, wh.0), min_nz(vp9_wh.1, wh.1));
                vp9_fps = min_nz(vp9_fps, fps);
            }
            "av1" => {
                av1 = true;
                av1_wh = (min_nz(av1_wh.0, wh.0), min_nz(av1_wh.1, wh.1));
                av1_fps = min_nz(av1_fps, fps);
            }
            _ => {}
        }
    }
    // Every webOS SoC decodes H.264, so a table without the row (VP9-only, say) is a misread of
    // the file, not a TV without the codec — and the reject must be WHOLE, like the empty-list
    // guard above: `transcoder.rs`'s profile doc promises exactly that, yet this used to splice
    // `assumed` axes into table-derived ones instead, so probe() logged invented numbers under
    // the "(device table)" provenance tag.
    if !h264 {
        return None;
    }
    // The bound BOTH consumers apply to every codec at once — the per-axis MIN across the two
    // decoders' merged rows, so neither decoder is over-claimed (see the field doc).
    let hevc_max = (min_nz(h264_wh.0, hevc_wh.0), min_nz(h264_wh.1, hevc_wh.1));
    if hevc_max.0 == 0 || hevc_max.1 == 0 {
        // Rows present but no stated bound on some axis: the same misread rule — fall back
        // whole rather than publish half a table's numbers as the device's.
        return None;
    }
    // DP_AUDIO_CODECS ∩ the table, keeping DP_AUDIO_CODECS's order so the profile string stays
    // stable across firmwares that merely reorder their rows.
    let mut audio_channels = std::collections::BTreeMap::new();
    for row in &t.audio_codecs {
        if row.channels > 0 {
            let codec = canon(&row.name);
            let old = audio_channels.get(&codec).copied().unwrap_or(0);
            audio_channels.insert(codec, min_nz(old, row.channels));
        }
    }
    // FORK (RichiL10): the PCM lane's ceiling is the tighter of the table's PCM and LPCM rows,
    // and decoded FLAC leaves the app as PCM, so FLAC is bounded by its own row AND that one.
    // A lane without both bounds is not admitted (MEASURED_ONLY below).
    let pcm_ceiling = min_nz(
        audio_channels.get("pcm").copied().unwrap_or(0),
        audio_channels.get("lpcm").copied().unwrap_or(0),
    );
    if pcm_ceiling > 0 {
        audio_channels.insert("pcm".to_string(), pcm_ceiling);
    } else {
        audio_channels.remove("pcm");
    }
    match audio_channels.get("flac").copied() {
        Some(flac) if pcm_ceiling > 0 => {
            audio_channels.insert("flac".to_string(), flac.min(pcm_ceiling));
        }
        _ => {
            audio_channels.remove("flac");
        }
    }
    // FORK (RichiL10): ALAC is decoded by the same bundled FFmpeg and leaves the app as the same
    // PCM the FLAC row has already proven the sink takes; the table's own audio rows need not name
    // it (no firmware inventory has an ALAC row). A set with no FLAC row does not admit it either.
    if let Some(&flac) = audio_channels.get("flac") {
        audio_channels.insert("alac".to_string(), flac);
    }
    let table_audio: Vec<String> = t.audio_codecs.iter().map(|r| canon(&r.name)).collect();
    let audio: Vec<&str> = DP_AUDIO_CODECS
        .split(',')
        .filter(|c| table_audio.iter().any(|t| t == c) || (*c == "alac" && audio_channels.contains_key("alac")))
        .filter(|c| !MEASURED_ONLY.contains(c) || audio_channels.contains_key(*c))
        .collect();
    let audio = if audio.is_empty() {
        // Every webOS SoC decodes AAC; a table naming none of the three is evidence about our
        // reading of it, not about the television. Same reasoning as the empty-video guard.
        Caps::assumed().audio
    } else {
        audio.join(",")
    };
    // Auto's Opus ceiling, whatever the row claims: stereo is the only width measured clean on
    // every set (`OPUS_AUTO_MAX_CHANNELS`); Force Direct Play still applies no table ceiling.
    //
    // **Only when the encode lane can take the wider track** (a measured AC3 row of six channels).
    // Taking the width away with no lane to serve it and no server to convert it is how a silent
    // track became a failed route: the plan stops direct-playing, asks for an audio conversion, and
    // the loopback shim answers that with nothing (measured on the owner's B4/papikonda, webOS
    // 10.3.1, 2026-11-08: `decision ... DIRECT PLAY`, then a `start.mkv` that demuxes to no access
    // unit, then the error read-out). With the lane the ceiling costs nothing — the surround track
    // is decoded in-app and fed as AC3 5.1, which is what the lane exists for.
    let enc_lane = audio.split(',').any(|c| c == "ac3") && audio_channels.get("ac3").is_some_and(|&l| l >= 6);
    if enc_lane && audio.split(',').any(|c| c == "opus") {
        let limit = audio_channels.entry("opus".to_owned()).or_default();
        *limit = min_nz(*limit, OPUS_AUTO_MAX_CHANNELS);
    }
    Some(Caps {
        hevc,
        hevc_max,
        h264_row: (h264_wh.0, h264_wh.1, h264_fps),
        hevc_row: if hevc {
            (hevc_wh.0, hevc_wh.1, hevc_fps)
        } else {
            (0, 0, 0)
        },
        vp9,
        vp9_row: (vp9_wh.0, vp9_wh.1, vp9_fps),
        vp9_bit_depth: 8,
        av1,
        av1_row: (av1_wh.0, av1_wh.1, av1_fps),
        audio,
        audio_channels,
        native_ceiling: Default::default(),
    })
}

/// The VP9 bit depth to claim on a set running webOS `major` (0 = unknown). 10 on webOS 1 to 4, the
/// ACB video path, where the dev set (4.x) decoded a 10-bit Profile 2 stream with correct levels and a
/// PQ-tagged one switched the panel to HDR through `player::vp9_hdr`. 8 on every other set: on webOS 5+
/// there is no ACB, so `vp9_hdr` sends nothing, and whether a PQ VP9 stream reaches HDR there is
/// unknown, which means a 10-bit file might direct-play with washed-out colour where the server
/// would have converted it correctly.
pub fn vp9_bit_depth_for(major: u32) -> u32 {
    if (1..=4).contains(&major) {
        10
    } else {
        8
    }
}

/// Read the table once and log what was derived. Called at boot right after `webos::probe`,
/// before anything can fail; safe when the file does not exist (older/odd firmware → assumed).
pub fn probe() {
    let mut measured = false;
    let caps = match std::fs::read_to_string(CAPS_TABLE) {
        Ok(s) => match parse(&s) {
            Some(c) => {
                measured = true;
                let av1_row = if c.av1 {
                    format!(" av1={}x{}@{}", c.av1_row.0, c.av1_row.1, c.av1_row.2)
                } else {
                    String::new()
                };
                let vp9_row = if c.vp9 {
                    format!(" vp9={}x{}@{}", c.vp9_row.0, c.vp9_row.1, c.vp9_row.2)
                } else {
                    String::new()
                };
                let mut c = c;
                c.vp9_bit_depth = vp9_bit_depth_for(crate::tv::device::info().major);
                // Before the exclusions: a codec nothing can decode is not a candidate to limit.
                let (gst_dropped, gst_unlisted) = match std::fs::read_to_string(GST_CONF) {
                    Ok(conf) => apply_gst_decoders(&mut c, &conf),
                    Err(_) => (Vec::new(), Vec::new()),
                };
                plx_base::eventlog::log(&format!(
                    "devcaps: gstcool dropped={gst_dropped:?} kept-unlisted={gst_unlisted:?}"
                ));
                plx_base::eventlog::log(&format!(
                    "devcaps: hevc={} {}x{} vp9={} av1={} audio={} rows: h264={}x{}@{} hevc={}x{}@{}{vp9_row}{av1_row} vp9bits={} (device table)",
                    c.hevc,
                    c.hevc_max.0,
                    c.hevc_max.1,
                    c.vp9,
                    c.av1,
                    c.audio,
                    c.h264_row.0,
                    c.h264_row.1,
                    c.h264_row.2,
                    c.hevc_row.0,
                    c.hevc_row.1,
                    c.hevc_row.2,
                    c.vp9_bit_depth
                ));
                c
            }
            None => {
                plx_base::eventlog::log(&format!(
                    "devcaps: {CAPS_TABLE} unparseable — assuming the 49SM9000PLA profile"
                ));
                Caps::assumed()
            }
        },
        Err(e) => {
            plx_base::eventlog::log(&format!(
                "devcaps: {CAPS_TABLE} unreadable ({e}) — assuming the 49SM9000PLA profile"
            ));
            Caps::assumed()
        }
    };
    let _ = MEASURED.set(measured);
    let caps = with_fork_knobs(
        caps,
        plx_base::forkknobs::get("flac").as_deref(),
        plx_base::forkknobs::get("opus").as_deref(),
        plx_base::forkknobs::get("lossy").as_deref(),
    );
    let info = crate::tv::device::info();
    let (wide, learn, forget) = (
        plx_base::devtrig::flag("nativeaudiowide"),
        plx_base::devtrig::flag("nativeaudiolearn"),
        plx_base::devtrig::flag("nativeaudioforget"),
    );
    // The layers in order: the table, minus gstcool.conf's no-decoder codecs (above), minus
    // [`EXCLUSIONS`]. The refusals this install learned come after, from the stored record and from
    // the pipeline itself ([`Layers::fold`]). The debug flags skip the last two.
    let mut boot = caps.clone();
    let excluded = apply_exclusions(&mut boot, &info.codename, wide, EXCLUSIONS);
    plx_base::eventlog::log(&format!(
        "devcaps: firmware={:?} build={:?} audio layers: table+gst={} | exclusions: {} | -> audio={}{}{}",
        info.codename,
        info.build,
        caps.audio,
        if excluded.is_empty() { "none".to_string() } else { excluded.join(" ") },
        boot.audio,
        match (wide, learn) {
            (true, true) => " (nativeaudiowide: exclusions ignored; nativeaudiolearn: refusals still learned)",
            (true, false) => " (nativeaudiowide: exclusions and learned entries ignored)",
            _ => "",
        },
        if forget { " (nativeaudioforget: stored refusals ignored this run)" } else { "" },
    ));
    let _ = BOOT.set(Layers::new(boot, info.codename.clone(), info.build.clone(), wide, learn, forget));
}

/// FORK (RichiL10): the `lossy` knob. Off by default: this set's own table names MPEG and
/// Vorbis, so its pipeline plays them compressed. A set whose table lacks a row (or whose
/// pipeline refuses the track) turns the knob on and the app decodes them to stereo PCM.
pub fn lossy_for(knob: Option<&str>) -> bool {
    matches!(knob, Some("on"))
}

/// FORK (RichiL10): `flac=off` / `opus=off` in the fork's knob file hand those tracks back to the
/// server (read once at boot, like the table). Pure, so the rule is host-testable.
///
/// ALAC rides the same lossless decode lane and is admitted off the FLAC row's ceiling, so
/// `flac=off` turns it off too: a knob that quietly left half the lane on would make the two
/// codecs take different routes with no visible reason.
fn with_fork_knobs(mut caps: Caps, flac: Option<&str>, opus: Option<&str>, lossy: Option<&str>) -> Caps {
    let drop = |c: &str| {
        (matches!(c, "flac" | "alac") && flac == Some("off")) || (c == "opus" && opus == Some("off"))
    };
    let kept: Vec<&str> = caps.audio.split(',').filter(|c| !drop(c)).collect();
    if kept.len() != caps.audio.split(',').count() {
        caps.audio = kept.join(",");
        plx_base::eventlog::log(&format!("devcaps: fork knobs -> audio={}", caps.audio));
    }
    // FORK (RichiL10): `lossy=on` admits MP3 and Vorbis to the app-decode lane. The table's own
    // rows are not needed: a set with no row is exactly the set the knob is for, and the sink the
    // lane actually feeds is the PCM one (already enforced when the table parsed). The channel
    // bound is the pipeline's own feed limit (8) for the same reason — the lane downmixes whatever
    // arrives to stereo, so a source wider than the table's row is still playable here.
    if lossy_for(lossy) && caps.audio_has("pcm") {
        for key in ["mp3", "vorbis"] {
            if !caps.audio.split(',').any(|c| c == key) {
                caps.audio.push(',');
                caps.audio.push_str(key);
            }
            caps.audio_channels.insert(key.to_string(), 8);
        }
        plx_base::eventlog::log(&format!("devcaps: lossy lane -> audio={}", caps.audio));
    }
    let lane = |c: &str| caps.audio_channels.get(c).map_or(0, |v| *v);
    let enc = encode_lane_on() && !caps.enc_lane_profile_codecs(true).is_empty();
    if caps.audio_has("flac") || caps.audio_has("pcm") || caps.audio_has("opus") || enc {
        plx_base::eventlog::log(&format!(
            "devcaps: fork lanes flac<={} alac<={} pcm<={} opus<={} dts<={} (dts above it: {}) {}",
            lane("flac"),
            lane("alac"),
            lane("pcm"),
            lane("opus"),
            lane("dts"),
            if dts_over_ceiling_admitted() { "core" } else { "server" },
            // FORK (RichiL10): the AC3 encode lane (`ff_enc.rs`).
            if enc { "enc ac3<=8 (flac,pcm,truehd)" } else { "enc off" },
        ));
    }
    caps
}

/// Whether [`caps`] reflects THIS set's table or the assumed profile — see [`MEASURED`].
pub fn measured() -> bool {
    *MEASURED.get().unwrap_or(&false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dev set's real file, trimmed but formatting-verbatim (the ` : ` spacing, the license
    /// blurb, the fields nothing consumes): the parser has to survive the platform's own
    /// formatting, not a tidied version of it. Full copy captured off the TV 2026-08-10.
    const REAL: &str = r#"{
  "license" : "@@@LICENSE  Copyright (c) 2018 LG Electronics, Inc.  LICENSE@@@",
  "version" : "1.0",
  "videoCodecs" : [
    {
      "name" : "H.264",
      "maxWidth" : 4096,
      "maxHeight" : 2304,
      "maxFrameRate" : 60,
      "maxBitRate" : 50
    },

    {
      "name" : "H.265",
      "maxWidth" : 4096,
      "maxHeight" : 2304,
      "maxFrameRate" : 120,
      "maxBitRate" : 50
    },

    {
      "name" : "HEVC",
      "maxWidth" : 4096,
      "maxHeight" : 2176,
      "maxFrameRate" : 60,
      "maxBitRate" : 50
    },

    {
      "name" : "MPEG2",
      "maxWidth" : 1920,
      "maxHeight" : 1088,
      "maxFrameRate" : 30,
      "maxBitRate" : 40
    },

    {
      "name" : "VP8",
      "maxWidth" : 1920,
      "maxHeight" : 1088,
      "maxFrameRate" : 30,
      "maxBitRate" : 40
    },

    {
      "name" : "VP9",
      "maxWidth" : 4096,
      "maxHeight" : 2304,
      "maxFrameRate" : 60,
      "maxBitRate" : 50
    }
  ],
  "audioCodecs" : [
    {
      "name" : "MPEG",
      "channels" : 2
    },

    {
      "name" : "AAC",
      "channels" : 6
    },

    {
      "name" : "AC3",
      "channels" : 6
    },

    {
      "name" : "EAC3",
      "channels" : 8
    },

    {
      "name" : "DTS",
      "channels" : 6
    },

    {
      "name" : "FLAC",
      "channels" : 6
    }
  ]
}"#;

    #[test]
    fn reads_the_dev_sets_real_table() {
        let c = parse(REAL).expect("the real table parses");
        assert!(c.hevc && c.vp9);
        // the dev set's VP9 row is its own bound, not folded into the shared one
        assert_eq!(c.vp9_row, (4096, 2304, 60));
        // H.265 (4096x2304) merged with HEVC (4096x2176) by min — NOT either row verbatim.
        assert_eq!(c.hevc_max, (4096, 2176));
        // the per-codec rows, frame rate included: H.264 is one row; "H.265"+"HEVC" MIN-merge
        // to 4096x2176@60 (the 120 on the H.265 row loses to the HEVC row's 60)
        assert_eq!(c.h264_row, (4096, 2304, 60));
        assert_eq!(c.hevc_row, (4096, 2176, 60));
        // a table with no AV1 row leaves the AV1 capability off, with no bound stated
        assert!(!c.av1 && c.av1_row == (0, 0, 0));
        // a fractional cap is a CEILING the stream must fit under: 59.94 reads as 60, never 59
        let frac = parse(r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1088,"maxFrameRate":59.94}]}"#).unwrap();
        assert_eq!(frac.h264_row, (1920, 1088, 60));
        // MPEG is in the table but not in the pipeline's decode set, and FLAC (FORK: a PCM-lane
        // codec) needs a PCM row the dev set does not have; the subset keeps DP_AUDIO_CODECS's
        // own order, not the table's.
        assert_eq!(c.audio, "aac,ac3,eac3,dts");
    }

    /// PMS names DTS `dca` (every DTS entry in the profiles shipped with PMS spells it so); the
    /// pipeline and the device table say `dts`. Both spellings must reach the same answer.
    #[test]
    fn pms_spelling_dca_is_dts_for_membership_and_channel_bounds() {
        let caps = parse(REAL).unwrap();
        assert!(caps.audio_supports("dca", 6));
        // FORK (RichiL10): above the 6-channel row only with `dts_core=never`; the fork's
        // default plays the DTS core of a 7.1 DTS-HD track (`dts_over_ceiling_for`).
        assert!(!caps.audio_supports_with("dca", 8, false));
        assert!(caps.audio_supports_with("dca", 8, true));
        assert!(caps.audio_supports("dca", 0), "FORK: the AC3 lane needs no channel count");
        assert!(!caps.audio_supports_lanes("dca", 0, 0, true, false), "lane off: the row needs one");
        assert!(caps.audio_supports("DCA", 6), "case folds too");
        assert!(caps.audio_supports("ec-3", 8), "ec-3 is eac3");
        assert!(!Caps::assumed().audio_has("dca"));
        assert_eq!(audio_codec_id("DCA"), "dts");
        assert_eq!(audio_codec_id("AC3"), "ac3");
    }

    /// Forced mode admits DTS only on table evidence (a DTS row with channels), but skips the
    /// channel ceiling: 7.1 DTS-HD MA is refused in Auto and admitted in Forced.
    #[test]
    fn forced_dts_needs_a_table_row_but_no_channel_ceiling() {
        let caps = parse(REAL).unwrap();
        assert!(caps.forced_feeds_audio("dca"));
        assert!(caps.forced_feeds_audio("dts"));
        assert!(!caps.audio_supports_with("dca", 8, false), "FORK: Auto without the dts_core lane");
        assert!(!Caps::assumed().forced_feeds_audio("dca"));
        assert!(!Caps::assumed().forced_feeds_audio("dts"));
        let no_row = parse(&REAL.replace("DTS", "WMA")).unwrap();
        assert!(!no_row.forced_feeds_audio("dca"));
        for c in ["aac", "ac3", "eac3", "EAC3"] {
            assert!(Caps::assumed().forced_feeds_audio(c), "{c}");
        }
        assert!(!Caps::assumed().forced_feeds_audio("truehd"));
    }

    #[test]
    fn dts_requires_an_explicit_device_row() {
        assert!(parse(REAL).unwrap().audio_has("dts"));
        assert!(!Caps::assumed().audio_has("dts"));
        assert!(!parse(&REAL.replace("DTS", "WMA")).unwrap().audio_has("dts"));
    }

    /// PMS names every DTS track `dca` (`dca-ma` on DTS-HD MA). The gate must give it exactly
    /// the answer it gives `dts` — channel bound included — or a real server's DTS is never
    /// direct-played.
    #[test]
    fn pms_spells_dts_dca_and_the_gate_folds_it() {
        assert_eq!(dp_audio_key("dca"), "dts");
        assert_eq!(dp_audio_key("dca-ma"), "dts");
        assert_eq!(dp_audio_key("DCA"), "dts");
        assert_eq!(dp_audio_key("dts"), "dts");
        for other in ["ac3", "eac3", "aac", "truehd", "dcafoo", ""] {
            assert_eq!(dp_audio_key(other), other);
        }
        let caps = parse(REAL).unwrap();
        for codec in ["dca", "dca-ma"] {
            assert!(caps.audio_has(codec), "{codec}");
            assert!(caps.audio_supports(codec, 6), "{codec}");
            assert!(
                !caps.audio_supports_with(codec, 8, false),
                "{codec}: with dts_core=never the device's 6-channel bound holds"
            );
            assert!(caps.audio_supports_with(codec, 8, true), "{codec}: FORK: above it, the core plays");
            assert!(
                caps.audio_supports(codec, 0),
                "{codec}: FORK: the AC3 lane folds whatever channels the decoder returns, so a \
                 track whose channel count the server did not state still plays"
            );
            assert!(
                !caps.audio_supports_lanes(codec, 0, 0, true, false),
                "{codec}: with the lane off the device row needs a known channel count"
            );
        }
        assert!(!Caps::assumed().audio_has("dca"), "no DTS row, no DTS under any spelling");
    }

    #[test]
    fn audio_limits_retain_and_merge_the_devices_channel_bounds() {
        let caps = parse(REAL).unwrap();
        assert!(caps.audio_supports("dts", 6));
        assert!(!caps.audio_supports_with("dts", 8, false));
        assert!(caps.audio_supports_with("dts", 8, true));
        // FORK: the encode lane needs no channel count; the device row (lane off) does
        assert!(caps.audio_supports("dts", 0));
        assert!(!caps.audio_supports_lanes("dts", 0, 0, true, false));
        assert!(caps.audio_supports("aac", 2));
        assert!(caps.audio_supports("aac", 6));
        assert!(!caps.audio_supports("aac", 8));
        assert!(caps.audio_supports("eac3", 8));
        let merged = parse(&REAL.replace("\"name\" : \"FLAC\"", "\"name\" : \"EAC3\"" )).unwrap();
        assert!(!merged.audio_supports("eac3", 8));
        let unbounded = parse(&REAL.replace("\"channels\" : 6", "\"channels\" : 0")).unwrap();
        assert!(!unbounded.audio_has("dts"));
        assert!(!Caps::assumed().audio_supports("truehd", 6));
    }

    /// The merge is per-AXIS min, not a pick of the smaller row: a table whose duplicate rows
    /// each win one axis must combine into the bound neither row states alone. (The roomier
    /// H.264 row is there because an h264-less table is rejected whole — and it must not win
    /// an axis back from the hevc merge.)
    #[test]
    fn duplicate_rows_merge_by_min_on_every_axis() {
        let c = parse(
            r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":4096,"maxHeight":2304},
                {"name":"H.265","maxWidth":3000,"maxHeight":2304},
                {"name":"HEVC","maxWidth":4096,"maxHeight":2176}
            ]}"#,
        )
        .unwrap();
        assert_eq!(c.hevc_max, (3000, 2176));
    }

    /// The issue-#22 shape the first cut of this module REINTRODUCED: a 4K HEVC decoder beside
    /// 1080p-limited AVC. Bounding at the HEVC rows alone ("HEVC is the tighter decoder" — a
    /// dev-set fact asserted as universal) advertised a `*`-scoped 3840x2160, and PMS would
    /// direct-play 4K H.264 onto the 1080p decoder. The bound is the min across BOTH.
    #[test]
    fn the_bound_is_the_min_across_both_decoders_not_the_hevc_rows_alone() {
        let c = parse(
            r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1088},
                {"name":"HEVC","maxWidth":3840,"maxHeight":2160}
            ]}"#,
        )
        .unwrap();
        assert!(c.hevc);
        assert_eq!(c.hevc_max, (1920, 1088));
    }

    /// A row that omits its dimensions says nothing — it must not zero out the rows that spoke.
    #[test]
    fn a_dimensionless_duplicate_does_not_erase_the_stated_bound() {
        let c = parse(
            r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":4096,"maxHeight":2304},
                {"name":"HEVC","maxWidth":4096,"maxHeight":2176},
                {"name":"H.265"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(c.hevc_max, (4096, 2176));
    }

    /// The issue-#22 case this module exists for: a SoC whose table has no HEVC row. The width/
    /// height bound must then come from the H.264 row — telling PMS 3840 wide on a 1080p-only
    /// decoder would direct-play 4K H.264 onto hardware that cannot play it.
    #[test]
    fn a_soc_without_hevc_bounds_at_its_h264_row() {
        let c = parse(
            r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1088}],
                "audioCodecs":[{"name":"AAC","channels":2}]}"#,
        )
        .unwrap();
        assert!(!c.hevc);
        assert_eq!(c.hevc_max, (1920, 1088));
        assert_eq!(c.audio, "aac");
    }

    /// Codec-name matching survives the spelling variants firmwares use: case, dots, dashes
    /// ("EAC3" here, "E-AC3" elsewhere in LG's own configs).
    #[test]
    fn codec_names_match_case_and_punctuation_insensitively() {
        let c = parse(
            r#"{"videoCodecs":[{"name":"H264","maxWidth":3840,"maxHeight":2160},
                              {"name":"h265","maxWidth":3840,"maxHeight":2160}],
                "audioCodecs":[{"name":"aAc"},{"name":"Ac-3"},{"name":"E-AC3"}]}"#,
        )
        .unwrap();
        assert!(c.hevc);
        assert_eq!(c.audio, "aac,ac3,eac3");
    }

    /// Garbage in must not panic AND must not half-parse: every unusable input lands on None so
    /// probe() falls back to the assumed profile whole — never spliced with it, which is how a
    /// misread once logged assumed numbers under the "(device table)" provenance tag. This runs
    /// during boot.
    #[test]
    fn garbage_and_vacuous_tables_are_none_not_a_panic() {
        for bad in [
            "",
            "not json at all",
            "{",
            "{}",
            r#"{"videoCodecs":[]}"#,
            r#"{"videoCodecs":"nope"}"#,
            // no H.264 row (every webOS SoC decodes it — a misread, not a VP9-only television)
            r#"{"videoCodecs":[{"name":"VP9","maxWidth":3840,"maxHeight":2160}]}"#,
            // rows, but no stated bound on an axis — half a table is not a device profile
            r#"{"videoCodecs":[{"name":"H.264"}]}"#,
            r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920}]}"#,
        ] {
            assert!(parse(bad).is_none(), "input {bad:?}");
        }
    }

    /// A parsed table whose audio rows miss every direct-playable codec falls back to the full
    /// set: that is a misread (every webOS SoC decodes AAC), and misreads must degrade to
    /// yesterday's behavior, not to a client that transcodes all audio.
    #[test]
    fn an_empty_audio_intersection_falls_back_to_the_full_dp_set() {
        let c = parse(
            r#"{"videoCodecs":[{"name":"H.264","maxWidth":1920,"maxHeight":1088}],
                "audioCodecs":[{"name":"DTS"},{"name":"WMA"}]}"#,
        )
        .unwrap();
        assert_eq!(c.audio, "aac,ac3,eac3");
    }

    /// The fallback IS yesterday's constants — the values the app asserted for every device
    /// before this module existed (the profile-string pin in plex/transcoder.rs is the other
    /// half of this contract).
    #[test]
    fn the_assumed_profile_is_todays_constants() {
        let c = Caps::assumed();
        assert!(c.hevc);
        assert_eq!(c.hevc_max, (3840, 2176));
        assert_eq!(c.audio, "aac,ac3,eac3");
    }

    /// An AV1 row is a capability bit with its own bound, and it does not move the shared
    /// bound: `hevc_max` is what the table says without the AV1 row. The row's nested
    /// `restrictions` array and `maxBitRate` are unknown fields to this parser and must parse
    /// through without touching the row's own dimensions. Synthetic table, not a device file.
    #[test]
    fn an_av1_row_sets_the_av1_capability_and_its_bound() {
        let base = r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080,"maxFrameRate":60},
                {"name":"HEVC","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60}
            ]}"#;
        let with_av1 = r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080,"maxFrameRate":60},
                {"name":"HEVC","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60},
                {"name":"AV1","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60,
                 "maxBitRate":50,
                 "restrictions":[{"mode":"x","maxWidth":1920,"maxHeight":1088,"maxFrameRate":60}]}
            ]}"#;
        let c = parse(with_av1).expect("a table with an AV1 row parses");
        assert!(c.av1);
        assert_eq!(c.av1_row, (3840, 2160, 60));
        assert_eq!(c.hevc_max, parse(base).unwrap().hevc_max);
    }

    /// Duplicate AV1 rows MIN-merge per axis like the other decoders' duplicates do: the
    /// roomier 8K row cannot win an axis back from the 4K one.
    #[test]
    fn duplicate_av1_rows_merge_to_the_minimum() {
        let c = parse(
            r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080},
                {"name":"AV1","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60},
                {"name":"AV1","maxWidth":7680,"maxHeight":4320,"maxFrameRate":30}
            ]}"#,
        )
        .unwrap();
        assert!(c.av1);
        assert_eq!(c.av1_row, (3840, 2160, 30));
    }

    /// The AV1 row is not an H.264 row: a table with AV1 and no H.264 is rejected whole, the
    /// same rule every table without H.264 follows, so AV1 can never be the only thing a
    /// misread table claims.
    #[test]
    fn an_av1_only_table_is_still_rejected() {
        assert!(parse(r#"{"videoCodecs":[{"name":"AV1","maxWidth":3840,"maxHeight":2160}]}"#).is_none());
    }

    /// The fallback never claims AV1: an unreadable table, or a table that lacks the row, keeps
    /// AV1 a server transcode, exactly as before this capability existed.
    #[test]
    fn assumed_never_claims_av1() {
        let c = Caps::assumed();
        assert!(!c.av1);
        assert_eq!(c.av1_row, (0, 0, 0));
    }

    /// A VP9 row is a capability bit with its own bound, like AV1's: duplicate rows MIN-merge per
    /// axis, and it never moves the shared bound (`hevc_max`) the other codecs derive from.
    #[test]
    fn a_vp9_row_sets_the_vp9_capability_and_its_bound() {
        let base = r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080,"maxFrameRate":60}
            ]}"#;
        let with_vp9 = r#"{"videoCodecs":[
                {"name":"H.264","maxWidth":1920,"maxHeight":1080,"maxFrameRate":60},
                {"name":"VP9","maxWidth":3840,"maxHeight":2160,"maxFrameRate":60},
                {"name":"VP9","maxWidth":4096,"maxHeight":2304,"maxFrameRate":30}
            ]}"#;
        let c = parse(with_vp9).expect("a table with a VP9 row parses");
        assert!(c.vp9);
        assert_eq!(c.vp9_row, (3840, 2160, 30));
        assert_eq!(c.hevc_max, parse(base).unwrap().hevc_max);
        let without = parse(base).unwrap();
        assert!(!without.vp9 && without.vp9_row == (0, 0, 0));
    }

    /// The fallback never claims VP9 either: VP9 direct play is gated on the table's own row, so
    /// an unreadable table keeps VP9 a server job, as it was before the gate existed.
    #[test]
    fn assumed_never_claims_vp9() {
        let c = Caps::assumed();
        assert!(!c.vp9);
        assert_eq!(c.vp9_row, (0, 0, 0));
    }

    /// 10-bit VP9 is claimed only on the webOS 4 family (the ACB path it was measured on); an unknown
    /// release, webOS 5+ and the assumed profile all say 8.
    #[test]
    fn vp9_bit_depth_is_ten_only_where_it_was_measured() {
        assert_eq!(vp9_bit_depth_for(0), 8, "unknown release");
        for major in 1..=4 {
            assert_eq!(vp9_bit_depth_for(major), 10, "webOS {major}");
        }
        for major in [5, 6, 7, 8, 9, 10, 11, 25] {
            assert_eq!(vp9_bit_depth_for(major), 8, "webOS {major}");
        }
        assert_eq!(Caps::assumed().vp9_bit_depth, 8);
        assert_eq!(parse(REAL).unwrap().vp9_bit_depth, 8, "parse() alone never claims it; probe() sets it");
    }

    /// FORK (RichiL10): the LG OLED B4's audio rows (webOS 10.3.1, read off the set 2026-10-15),
    /// formatting trimmed. FLAC is bounded by the PCM/LPCM rows it leaves the app through.
    const B4: &str = r#"{
  "videoCodecs" : [
    { "name" : "H.264", "maxWidth" : 4096, "maxHeight" : 2176, "maxFrameRate" : 30 },
    { "name" : "HEVC", "maxWidth" : 4096, "maxHeight" : 2176, "maxFrameRate" : 60 }
  ],
  "audioCodecs" : [
    { "name" : "MPEG", "channels" : 2 }, { "name" : "AAC", "channels" : 6 },
    { "name" : "WMA", "channels" : 6 }, { "name" : "PCM", "channels" : 2 },
    { "name" : "LPCM", "channels" : 2 }, { "name" : "AC3", "channels" : 6 },
    { "name" : "EAC3", "channels" : 8 }, { "name" : "DTH", "channels" : 2 },
    { "name" : "DTS", "channels" : 6 }, { "name" : "DTSH", "channels" : 6 },
    { "name" : "DTSE", "channels" : 6 }, { "name" : "DRA", "channels" : 8 },
    { "name" : "Vorbis", "channels" : 6 }, { "name" : "RA6", "channels" : 6 },
    { "name" : "AMR", "channels" : 2 }, { "name" : "FLAC", "channels" : 2 },
    { "name" : "OPUS", "channels" : 6 }
  ]
}"#;

    #[test]
    fn the_b4_table_admits_the_fork_lanes_at_their_measured_ceilings() {
        let c = parse(B4).expect("the B4 table parses");
        assert_eq!(c.audio, "aac,ac3,eac3,dts,flac,alac,pcm,opus");
        for (codec, ch, ok) in [
            ("flac", 1, true), ("flac", 2, true), ("flac", 6, false), ("flac", 8, false),
            ("flac", 0, false), ("pcm", 2, true), ("pcm", 6, false), ("opus", 2, true),
            ("opus", 6, false), ("opus", 8, false), ("opus", 0, false), ("truehd", 8, false),
            ("eac3", 8, true), ("ac3", 6, true),
        ] {
            assert_eq!(c.audio_supports_with(codec, ch, true), ok, "{codec} {ch}ch");
        }
        // Auto's Opus ceiling is stereo on every set (`OPUS_AUTO_MAX_CHANNELS`), whatever the row
        // claims; Force Direct Play applies no table ceiling (`forced_feeds_audio`).
        assert_eq!(c.audio_channels.get("opus"), Some(&OPUS_AUTO_MAX_CHANNELS));
        // DTS-HD 6.1 / 7.1: the core plays unless the knob says never.
        assert!(c.audio_supports_with("dca-ma", 7, true));
        assert!(c.audio_supports_with("dca", 8, true));
        assert!(!c.audio_supports_with("dca-ma", 7, false));
        assert_eq!(c.profile_channel_limit_with("flac", false), Some(2));
        assert_eq!(c.profile_channel_limit_with("pcm", false), Some(2));
        assert_eq!(c.profile_channel_limit_with("opus", false), Some(2));
        assert_eq!(c.profile_channel_limit_with("truehd", false), None);
    }

    /// FORK (RichiL10): the AC3 encode lane on the B4. FLAC/PCM the PCM lane cannot declare and
    /// TrueHD/MLP become direct-playable at 48 kHz; any other rate, and `encode=off`, keep the
    /// answers above.
    #[test]
    fn the_b4_table_admits_the_encode_lane_at_48_khz_only() {
        let c = parse(B4).unwrap();
        let on = |codec: &str, ch: i64, rate: i64| c.audio_supports_lanes(codec, ch, rate, true, true);
        let off = |codec: &str, ch: i64, rate: i64| c.audio_supports_lanes(codec, ch, rate, true, false);
        for (codec, ch) in [("flac", 8), ("flac", 6), ("flac", 3), ("pcm", 8), ("pcm", 6), ("truehd", 8), ("truehd", 6), ("mlp", 2), ("dts", 6), ("dts", 8), ("dca-ma", 8)] {
            assert!(on(codec, ch, 48_000), "{codec} {ch}ch at 48 kHz: encode lane");
            assert!(on(codec, ch, 0), "{codec} {ch}ch, rate not said: admitted, the Load decides");
            assert!(!on(codec, ch, 96_000), "{codec} {ch}ch at 96 kHz stays with the server");
            assert!(!on(codec, ch, 44_100), "{codec} {ch}ch at 44.1 kHz: no resampler");
            if dp_audio_key(codec) != "dts" {
                // DTS is the one lane tenant the device row also admits on its own, so `off`
                // leaves it playable (natively, which is what this fork stopped doing).
                assert!(!off(codec, ch, 48_000), "{codec} {ch}ch with encode=off: the server, as in build 3");
            }
        }
        assert!(off("dts", 6, 48_000), "encode=off: the device row still admits DTS natively");
        // inside the PCM lane nothing changes, whatever the rate
        assert!(on("flac", 2, 96_000) && on("flac", 1, 44_100) && on("pcm", 2, 48_000));
        assert_eq!(c.audio_lane("flac", 2, 96_000, true), Some(AudioLane::Pcm));
        assert_eq!(c.audio_lane("flac", 8, 48_000, true), Some(AudioLane::Ac3Encode));
        assert_eq!(c.audio_lane("truehd", 8, 48_000, true), Some(AudioLane::Ac3Encode));
        assert_eq!(c.audio_lane("truehd", 8, 96_000, true), None);
        assert_eq!(c.audio_lane("flac", 8, 48_000, false), None);
        assert_eq!(c.audio_lane("ac3", 6, 48_000, true), None);
        assert_eq!(c.audio_lane("dca-ma", 8, 48_000, true), Some(AudioLane::Ac3Encode),
            "DTS goes through the AC3 lane: a native DTS Load never configures this set's audio sink");
        assert_eq!(c.audio_lane("dts", 6, 0, true), Some(AudioLane::Ac3Encode), "rate not said: the Load decides");
        // more than 8 channels is no lane at all; neither is a codec the TV decodes itself
        assert!(!on("flac", 9, 48_000) && !on("vorbis", 6, 48_000));
        assert!(on("eac3", 8, 48_000) && on("ac3", 6, 48_000));
        // the profile states 8 for the lane's codecs while it is on, the old rows when it is off
        for codec in ["flac", "pcm", "truehd", "mlp", "dts", "opus"] {
            assert_eq!(c.profile_channel_limit_with(codec, true), Some(8), "{codec}");
        }
        // With the lane off, the set's own row is what the profile states for Opus.
        assert_eq!(c.profile_channel_limit_with("opus", false), Some(2));
        assert_eq!(c.enc_lane_profile_codecs(true), ["truehd", "mlp", "dts", "opus"]);
        assert!(c.enc_lane_profile_codecs(false).is_empty());
        // flac=off hands FLAC back to the server in both lanes
        let no_flac = with_fork_knobs(c.clone(), Some("off"), None, None);
        assert!(!no_flac.audio_supports_lanes("flac", 8, 48_000, true, true));
        assert!(no_flac.audio_supports_lanes("truehd", 8, 48_000, true, true));
        assert!(encode_lane_for(None) && encode_lane_for(Some("ac3")) && !encode_lane_for(Some("off")));
    }

    /// FORK (RichiL10): the encode lane needs a MEASURED AC3 5.1 sink, like every fork lane.
    #[test]
    fn the_encode_lane_needs_a_measured_ac3_sink() {
        let assumed = Caps::assumed();
        assert!(!assumed.audio_supports_lanes("truehd", 8, 48_000, true, true), "no AC3 row: no lane");
        assert!(assumed.enc_lane_profile_codecs(true).is_empty());
        let stereo_ac3 = parse(&B4.replace(r#""AC3", "channels" : 6"#, r#""AC3", "channels" : 2"#)).unwrap();
        assert!(!stereo_ac3.audio_supports_lanes("flac", 8, 48_000, true, true));
        // a wider PCM row moves 6-channel FLAC/PCM back into the lossless lane
        let wide = parse(&B4.replace(r#""PCM", "channels" : 2"#, r#""PCM", "channels" : 6"#)
            .replace(r#""LPCM", "channels" : 2"#, r#""LPCM", "channels" : 6"#))
            .unwrap();
        assert_eq!(wide.audio_lane("pcm", 6, 96_000, true), Some(AudioLane::Pcm));
        assert_eq!(wide.audio_lane("pcm", 8, 48_000, true), Some(AudioLane::Ac3Encode));
    }

    #[test]
    fn a_flac_row_without_a_pcm_row_is_not_a_lane() {
        let c = parse(&B4.replace(r#"{ "name" : "PCM", "channels" : 2 },"#, "")
            .replace(r#"{ "name" : "LPCM", "channels" : 2 },"#, ""))
            .unwrap();
        assert_eq!(c.audio, "aac,ac3,eac3,dts,opus");
        // A six-channel PCM row lets FLAC follow FLAC's own row, not more.
        let wide = parse(&B4.replace(r#""PCM", "channels" : 2"#, r#""PCM", "channels" : 6"#)
            .replace(r#""LPCM", "channels" : 2"#, r#""LPCM", "channels" : 6"#))
            .unwrap();
        assert_eq!(wide.audio_channels.get("flac"), Some(&2));
        assert_eq!(wide.audio_channels.get("pcm"), Some(&6));
    }

    #[test]
    fn fork_knobs_hand_flac_and_opus_back_to_the_server() {
        let c = parse(B4).unwrap();
        assert_eq!(with_fork_knobs(c.clone(), None, None, None).audio, "aac,ac3,eac3,dts,flac,alac,pcm,opus");
        assert_eq!(with_fork_knobs(c.clone(), Some("off"), None, None).audio, "aac,ac3,eac3,dts,pcm,opus");
        assert_eq!(with_fork_knobs(c, Some("pcm"), Some("off"), None).audio, "aac,ac3,eac3,dts,flac,alac,pcm");
        assert!(dts_over_ceiling_for(None));
        assert!(dts_over_ceiling_for(Some("auto")));
        assert!(!dts_over_ceiling_for(Some("never")));
    }

    /// **`lossy=on` admits MP3 and Vorbis to the PCM lane, `lossy` absent changes nothing.** The
    /// lane downmixes to stereo, so the bound announced is the pipeline's feed limit (8) whether
    /// or not the table names a row — and a set whose table names neither still gets them: that
    /// set is the one the knob exists for.
    #[test]
    fn the_lossy_knob_admits_mp3_and_vorbis_to_the_pcm_lane() {
        assert!(!lossy_for(None) && !lossy_for(Some("off")) && lossy_for(Some("on")));
        let plain = with_fork_knobs(parse(B4).unwrap(), None, None, None);
        assert_eq!(plain.audio, "aac,ac3,eac3,dts,flac,alac,pcm,opus");
        assert_eq!(plain.audio_lane("mp3", 2, 44_100, true), None);
        assert_eq!(plain.audio_lane("vorbis", 6, 48_000, true), None);

        let on = with_fork_knobs(parse(B4).unwrap(), None, None, Some("on"));
        assert_eq!(on.audio, "aac,ac3,eac3,dts,flac,alac,pcm,opus,mp3,vorbis");
        assert_eq!(on.audio_lane("mp3", 2, 44_100, true), Some(AudioLane::Pcm));
        assert_eq!(on.audio_lane("vorbis", 6, 48_000, true), Some(AudioLane::Pcm), "a 5.1 Vorbis plays downmixed");
        assert!(on.audio_supports_at("mp3", 2, 44_100));
        assert!(on.audio_supports_at("vorbis", 8, 48_000), "the lane downmixes, so the feed limit is 8");
        assert!(!on.audio_supports_at("mp3", 2, 50_000), "no PCM declaration for a rate the lane cannot feed");
        assert_eq!(on.profile_channel_limit("vorbis"), Some(8));

        // A table with no MPEG/Vorbis row still gets the lane: the sink is what matters.
        let bare = parse(&B4.replace(r#"{ "name" : "Vorbis", "channels" : 6 },"#, "").replace(r#"{ "name" : "MPEG", "channels" : 2 },"#, "")).unwrap();
        let bare_on = with_fork_knobs(bare, None, None, Some("on"));
        assert!(bare_on.audio_has("mp3") && bare_on.audio_has("vorbis"));
        assert_eq!(bare_on.audio_lane("vorbis", 2, 44_100, true), Some(AudioLane::Pcm));
    }

    #[test]
    fn the_learn_switch_keeps_the_learned_layer_under_the_wide_flag() {
        assert!(!ignores_learned(false, false));
        assert!(!ignores_learned(false, true));
        assert!(ignores_learned(true, false));
        assert!(!ignores_learned(true, true));
    }

    /// The fork's Opus-announcing table (the B4 shape): FLAC/PCM rows the PCM lane uses, an
    /// `OPUS` row, and an AC3 6-channel row so the encode lane is live. The dev table (`REAL`) has
    /// no Opus row at all, so a test about Opus needs this one.
    fn native_set() -> Caps {
        parse(B4).unwrap()
    }

    #[test]
    fn an_unknown_firmware_family_announces_what_its_table_lists() {
        for codename in ["", "goldilocks2-grampians", "some-future-build"] {
            let mut c = native_set();
            assert!(apply_exclusions(&mut c, codename, false, EXCLUSIONS).is_empty(), "{codename:?}");
            assert_eq!(c, native_set(), "{codename:?}");
            assert!(c.audio_supports("opus", 2));
            assert!(c.forced_feeds_audio_track("opus", 6), "no row, no ceiling");
        }
    }

    #[test]
    fn the_c1s_firmware_family_loses_the_combinations_measured_broken() {
        // The dev table (`REAL`) lists no Opus row, so the C1's `opus<=2` row has nothing to take
        // there; a set that DOES announce Opus is bounded (the next test).
        let mut c = parse(REAL).unwrap();
        // FORK (Nuvio Native): this table lists no Opus row to bound, so the C1's `opus<=2` row
        // has nothing to take here; on a set that DOES announce Opus it bounds it (next test).
        assert!(apply_exclusions(&mut c, "kisscurl-koli", false, EXCLUSIONS).is_empty());
        assert_eq!(c, parse(REAL).unwrap());
        // The Vorbis row is inert here: vorbis is APP-decoded in this fork (the PCM lane), so an
        // exclusion must never take that lane away (see `limit_codec`).
        let lossy = with_fork_knobs(parse(B4).unwrap(), None, None, Some("on"));
        let mut d = lossy.clone();
        // The Opus row applies; the Vorbis row does NOT touch the app-decoded lane.
        assert_eq!(apply_exclusions(&mut d, "kisscurl-koli", false, EXCLUSIONS), ["opus<=2"]);
        assert_eq!(d.audio, lossy.audio, "the app-decoded Vorbis lane survives the Vorbis row");
        assert!(d.audio_has("vorbis"));
    }

    /// The C1's real rows, applied to a table that announces Opus at six channels (the B4 shape,
    /// the one fork set whose table carries an `OPUS` row).
    #[test]
    fn the_c1s_rows_bound_a_set_that_announces_opus() {
        let mut c = parse(B4).unwrap();
        assert_eq!(apply_exclusions(&mut c, "kisscurl-koli", false, EXCLUSIONS), ["opus<=2"]);
        assert!(c.audio_supports("opus", 2));
        // Six channels are admitted only through the AC3 encode lane on a set that has its sink;
        // the measured row is what the NATIVE path is bounded by (the lane-off answer).
        assert!(!c.audio_supports_with("opus", 6, true));
        assert!(c.audio_supports("opus", 6), "the lane takes the width the row does not");
        // Force Direct Play applies no table ceiling, but it holds a measured one.
        assert!(c.forced_feeds_audio_track("opus", 2) && !c.forced_feeds_audio_track("opus", 6));
        assert!(c.forced_feeds_audio_track("eac3", 8), "a codec without a row keeps its freedom");
        // The rest of the table is untouched.
        assert!(c.audio_supports("eac3", 8) && c.audio_supports("dts", 6) && c.audio_supports("aac", 6));
        // The codename compares without regard to case.
        let mut d = parse(B4).unwrap();
        assert_eq!(apply_exclusions(&mut d, "Kisscurl-Koli", false, EXCLUSIONS).len(), 1);
    }

    #[test]
    fn the_debug_flag_ignores_the_exclusions() {
        let mut c = parse(B4).unwrap();
        assert!(apply_exclusions(&mut c, "kisscurl-koli", true, EXCLUSIONS).is_empty());
        assert_eq!(c, parse(B4).unwrap());
    }

    #[test]
    fn an_exclusion_only_takes_away() {
        // A table row of two channels is not raised to the exclusion's four; a codec the table
        // never listed is not added.
        let table = [
            Exclusion { codename: "x", codec: "opus", max_channels: 4, evidence: "" },
            Exclusion { codename: "x", codec: "truehd", max_channels: 2, evidence: "" },
        ];
        let mut c = parse(&REAL.replace(
            "\"name\" : \"FLAC\"",
            "\"name\" : \"OPUS\", \"channels\" : 2 }, { \"name\" : \"FLAC\"",
        ))
        .unwrap();
        assert_eq!(apply_exclusions(&mut c, "x", false, &table), ["opus<=2"]);
        assert_eq!(c.audio_channels.get("opus"), Some(&2));
        assert!(!c.audio_has("truehd"));
    }

    fn refusal(codec: &str, channels: u32) -> Refusal {
        Refusal { firmware: "kisscurl-koli".into(), codec: codec.into(), channels, build: String::new() }
    }

    #[test]
    fn a_learned_refusal_of_a_stereo_track_takes_the_codec_away_and_a_surround_one_keeps_stereo() {
        let mut c = parse(B4).unwrap();
        let applied = apply_learned(&mut c, "kisscurl-koli", false, &[refusal("opus", 2), refusal("eac3", 8)]);
        assert_eq!(applied, ["opus=none", "eac3<=2"]);
        assert!(!c.audio_has("opus") && !c.forced_feeds_audio("opus"));
        assert!(c.audio_supports("eac3", 2) && !c.audio_supports("eac3", 6));
        assert!(!c.forced_feeds_audio_track("eac3", 6), "Force holds a learned ceiling too");
        // Another firmware's lessons, an unannounced codec and the debug flag do nothing.
        for (name, ignore) in [("goldilocks2-grampians", false), ("", false), ("kisscurl-koli", true)] {
            let mut d = parse(B4).unwrap();
            assert!(apply_learned(&mut d, name, ignore, &[refusal("opus", 2)]).is_empty(), "{name:?} {ignore}");
            assert_eq!(d, parse(B4).unwrap());
        }
        let mut e = Caps::assumed();
        assert!(apply_learned(&mut e, "kisscurl-koli", false, &[refusal("opus", 6)]).is_empty(), "the fallback never announced Opus");
    }

    #[test]
    fn the_layers_stack_in_order_table_gst_exclusions_learned() {
        // gstcool.conf drops DTS; the C1 row bounds Opus to stereo; a learned refusal then removes
        // the codec the row left (stereo Opus).
        let mut c = parse(B4).unwrap();
        assert_eq!(apply_gst_decoders(&mut c, &GST_C1.replace("dts_audiodec=290", "dts_audiodec=0")).0, ["dts"]);
        assert_eq!(apply_exclusions(&mut c, "kisscurl-koli", false, EXCLUSIONS), ["opus<=2"]);
        assert_eq!(apply_learned(&mut c, "kisscurl-koli", false, &[refusal("opus", 2)]), ["opus=none"]);
        assert_eq!(c.audio, "aac,ac3,eac3,flac,alac,pcm");
    }

    fn layers_on(build: &str) -> Layers {
        Layers::new(native_set(), "kisscurl-koli".into(), build.into(), false, false, false)
    }

    fn announced(layers: &Layers, codec: &str, channels: i64) -> bool {
        layers.caps.read().unwrap().audio_supports(codec, channels)
    }

    /// The answer only ever loses native audio: folding is idempotent, a list that is behind (an
    /// older record, an empty one) gives nothing back, and a snapshot a reader already holds is not
    /// touched by the next fold.
    #[test]
    fn folding_refusals_only_ever_takes_native_audio_away() {
        let layers = Layers::new(parse(B4).unwrap(), "kisscurl-koli".into(), "6.5.3-47".into(), false, false, false);
        let before = layers.caps.read().unwrap().clone();
        let stereo_opus = Refusal { build: "6.5.3-47".into(), ..refusal("opus", 2) };
        assert!(layers.fold(&[stereo_opus.clone()]));
        assert!(!announced(&layers, "opus", 2));
        assert!(!layers.fold(&[stereo_opus.clone()]), "the same lesson is not news");
        assert!(!layers.fold(&[]), "an empty record removes nothing");
        assert!(!announced(&layers, "opus", 2), "and gives nothing back");
        assert!(before.audio_has("opus"), "the snapshot a reader held is unchanged");
        // A surround refusal keeps the codec to stereo.
        assert!(layers.fold(&[Refusal { build: "6.5.3-47".into(), ..refusal("eac3", 6) }]));
        assert!(announced(&layers, "eac3", 2) && !announced(&layers, "eac3", 6));
    }

    /// An LG update replaces the decoder a refusal was watched on: another build's entry, and one an
    /// older record never tagged, is no evidence on this build, while a set that did not name its
    /// build judges nothing and takes every entry.
    #[test]
    fn a_refusal_is_evidence_only_on_the_build_it_was_watched_on() {
        let layers = layers_on("6.5.3-47");
        for stale in ["6.5.3-48", ""] {
            assert!(!layers.fold(&[Refusal { build: stale.into(), ..refusal("opus", 2) }]), "{stale:?}");
            assert!(announced(&layers, "opus", 2));
        }
        let blind = layers_on("");
        assert!(blind.fold(&[Refusal { build: "6.5.3-48".into(), ..refusal("opus", 2) }]));
        assert!(!announced(&blind, "opus", 2));
        assert!(refusal("opus", 2).watched_on("") && !refusal("opus", 2).watched_on("6.5.3-47"));
    }

    /// The debug flags: `nativeaudiowide` ignores every refusal, `nativeaudiolearn` beside it keeps
    /// them, and `nativeaudioforget` ignores the stored record but not a refusal learned this run.
    #[test]
    fn the_debug_flags_decide_which_refusals_apply() {
        let fresh = |wide, learn, forget| Layers::new(native_set(), "kisscurl-koli".into(), String::new(), wide, learn, forget);
        assert!(!fresh(true, false, false).fold(&[refusal("opus", 2)]));
        assert!(fresh(true, true, false).fold(&[refusal("opus", 2)]));
        assert!(fresh(false, false, true).fold(&[refusal("opus", 2)]), "learned this run, it applies");
    }

    /// Learning: the thread's set names its firmware, so a refusal changes its answer at once, is
    /// handed back once for the caller to persist, and is not news the second time (a conversion
    /// that still feeds it natively ends in the ordinary failure instead of looping). A set with no
    /// firmware name, or no table read, learns nothing.
    #[test]
    fn a_learned_refusal_changes_the_answer_once() {
        {
            let _set = firmware_on_this_thread_for_test("kisscurl-koli", "6.5.3-47", native_set());
            assert!(caps().audio_supports("opus", 2), "sanity: announced");
            let learned = learn_refusal("Opus", 2).expect("news");
            assert_eq!(learned, Refusal { firmware: "kisscurl-koli".into(), codec: "opus".into(), channels: 2, build: "6.5.3-47".into() });
            assert!(!caps().audio_has("opus"));
            assert_eq!(learn_refusal("opus", 2), None, "known");
            assert_eq!(learn_refusal("wmapro", 6), None, "never announced, nothing to take away");
            assert!(!project_stored_refusals(&[learned]), "the record catching up changes nothing");
        }
        let _nameless = caps_on_this_thread_for_test(native_set());
        assert_eq!(learn_refusal("opus", 2), None, "no firmware name: nothing can be learned about this set");
        assert!(caps().audio_supports("opus", 2));
    }

    /// The C1's real `/etc/gst/gstcool.conf` (webOS 6.5.3, `kisscurl-koli`), captured 2026-10-21;
    /// the license, debug and buffering sections are trimmed.
    const GST_C1: &str = "[buffering]\nmax-size-buffers=0\n\n[rank]\nmpegpsdemux=310\naudio_sink=0\ndecproxy=300\naudiodec=0\naudio_dec=0\nac4_dec=0\nomx_lxvideosink=256\nomx_lxdumpsink=0\nomx_lxvideodec=290\nalsasink=0\npulsesink=0\nmediainfo=64\nosssink=0\noss4sink=0\naac_audiodec=290\nac3_audiodec=290\nac4_audiodec=290\nadpcm_audiodec=290\namr_audiodec=290\ndts_audiodec=290\nflac_audiodec=290\nmpegh_audiodec=290\npcm_audiodec=290\nvorbis_audiodec=290\nwma_audiodec=290\navenc_ac3=0\navenc_eac3=0\n\n[sw_decoder]\navdec_mp3=0\navdec_aac=0\navdec_ac3=0\navdec_dca=0\navdec_alac=0\navdec_amrnb=0\navdec_amrwb=0\navdec_eac3=0\nwavpackdec=0\navdec_wmapro=0\navdec_wmav1=0\navdec_wmav2=0\navdec_wmavoice=0\nalawdec=0\nmulawdec=0\navdec_flac=0\nadpcmdec=0\nfaad=0\ndvdlpcmdec=0\nvorbisdec=0\n\n[sw_video_decoder]\navdec_h264=0\n\n[default_sink]\nvideo=omx_lxvideosink\naudio=audiosink\n";

    #[test]
    fn the_c1s_gstcool_conf_drops_nothing_and_keeps_opus_unlisted() {
        let mut c = native_set();
        let before = c.clone();
        let (dropped, unlisted) = apply_gst_decoders(&mut c, GST_C1);
        assert!(dropped.is_empty(), "{dropped:?}");
        // The file names no Opus decoder at all: no line means the default rank, not unsupported.
        assert_eq!(unlisted, ["opus"]);
        assert_eq!(c, before);
        assert!(c.audio_has("opus") && c.audio_supports("dts", 6));
    }

    /// The owner's LG B4 (`ponytail-papikonda`, webOS 10.3.1) shape, reconstructed from the boot
    /// line the first version of this rule produced on it (2026-11-08): `[rank]` names no
    /// `*_audiodec` hardware element at all and `[sw_decoder]` ranks every software decoder 0.
    /// Under the old "every listed decoder is off" rule that read as `dropped=["aac","ac3","eac3",
    /// "dts"]` — four codecs the set demonstrably plays (upstream #1096).
    const GST_B4_SHAPE: &str = "[rank]\naudio_sink=0\ndecproxy=300\naudiodec=0\nomx_lxvideodec=290\n\n[sw_decoder]\navdec_mp3=0\navdec_aac=0\navdec_ac3=0\navdec_dca=0\navdec_alac=0\navdec_eac3=0\navdec_flac=0\nfaad=0\nvorbisdec=0\n";

    #[test]
    fn a_software_only_rank_file_drops_nothing_and_lists_the_codecs_it_never_names() {
        // A missing line is never evidence: the file lists no hardware decoder element for these,
        // so every one of them stays — including the `ac3` row the encode lane needs for its SINK.
        let mut c = native_set();
        let before = c.clone();
        let (dropped, unlisted) = apply_gst_decoders(&mut c, GST_B4_SHAPE);
        assert!(dropped.is_empty(), "{dropped:?}");
        for codec in ["aac", "ac3", "eac3", "dts", "opus"] {
            assert!(unlisted.contains(&codec), "{codec:?} must be kept-unlisted, saw {unlisted:?}");
        }
        assert_eq!(c, before);
        // The lane's sink survived with the row, so the lane is still live after the boot layer.
        assert!(c.audio_has("ac3") && c.audio_channels.get("ac3").is_some_and(|&l| l >= 6));
        let mut d = before;
        apply_gst_decoders(&mut d, GST_B4_SHAPE);
        assert!(d.enc_sink_ok(), "the B4's AC3 row is what makes the encode lane usable");
    }

    #[test]
    fn a_set_whose_dts_decoder_is_ranked_off_loses_dts_and_its_forced_evidence() {
        // Positive evidence (the PR #1133 "must stay" case): the hardware dts_audiodec is LISTED at
        // rank 0 and the software avdec_dca is off too.
        let conf = GST_C1.replace("dts_audiodec=290", "dts_audiodec=0");
        let mut c = native_set();
        let (dropped, unlisted) = apply_gst_decoders(&mut c, &conf);
        assert_eq!(dropped, ["dts"]);
        assert!(!unlisted.contains(&"dts"));
        assert!(!c.audio_has("dts"));
        // The fork's AC3 encode lane still takes a native DTS track unconditionally (`dts` is the
        // lane's tenant); with the lane off, the table no longer admits it at all.
        assert!(!c.audio_supports_with("dts", 6, true));
        // The table row goes with it, so a rule that asks for a DTS row (Force Direct Play) agrees.
        assert!(!c.audio_channels.contains_key("dts"));
    }

    #[test]
    fn the_baseline_codecs_are_never_taken_away_by_the_rank_file() {
        // Even a conf that lists `aac_audiodec=0` cannot take AAC away: every webOS SoC decodes it,
        // so a zero there is evidence about our reading, not about the television.
        let conf = GST_C1
            .replace("aac_audiodec=290", "aac_audiodec=0")
            .replace("ac3_audiodec=290", "ac3_audiodec=0");
        let mut c = native_set();
        let (dropped, _) = apply_gst_decoders(&mut c, &conf);
        assert!(!dropped.contains(&"aac") && !dropped.contains(&"ac3"), "{dropped:?}");
        assert!(c.audio_has("aac") && c.audio_has("ac3"));
    }

    #[test]
    fn a_surround_opus_track_takes_the_ac3_encode_lane_and_stereo_stays_native() {
        let c = native_set();
        assert!(c.enc_sink_ok(), "the B4 fixture's AC3 row is the lane's sink");
        // Stereo is the width LG's own decoder was measured clean at: it never reaches the lane.
        assert!(!c.enc_lane_candidate("opus", 2, true));
        assert_eq!(c.audio_lane("opus", 2, 48_000, true), None);
        // Surround does, at Opus's one sample rate.
        assert!(c.enc_lane_candidate("opus", 6, true));
        assert_eq!(c.audio_lane("opus", 6, 48_000, true), Some(AudioLane::Ac3Encode));
        assert_eq!(c.audio_lane("opus", 8, 0, true), Some(AudioLane::Ac3Encode), "rate unstated");
        // Another rate is never Opus, and the knob off hands it back to whatever the plan decides.
        assert_eq!(c.audio_lane("opus", 6, 44_100, true), None);
        assert_eq!(c.audio_lane("opus", 6, 48_000, false), None);
        // A set with no AC3 row has no sink, so no lane — and no Opus to feed it either.
        let mut bare = c.clone();
        bare.audio = "opus".into();
        bare.audio_channels.remove("ac3");
        assert!(!bare.enc_lane_candidate("opus", 6, true));
    }
}
