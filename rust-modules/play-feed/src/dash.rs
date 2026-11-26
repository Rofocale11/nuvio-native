//! DASH (trailer) demux: Rust engine over the shim's static SegmentBase MPD.
//!
//! # why this is a new file
//!
//! Ported from the old fork's `media/src/ff.rs`. In this fork the physical FFmpeg demux lives in
//! `plx_play_feed`, and the strict MPD parser in the fork-owned crate `plx_nuvio_dash`. This module
//! is a child of `ff` (declared with `#[path = "dash.rs"] mod dash;` there), so it borrows the
//! demuxer's existing AVIO transport, packet framing and helper functions without making a single
//! one of them `pub` — the only edits to upstream `ff.rs` are that declaration and the `.mpd`
//! routing branch in `demux`.
//!
//! The bundled FFmpeg has no `dash` demuxer, so `.mpd` can never open via the progressive path.
//! `plx_nuvio_dash::parse_manifest` validates the one shape the shim generates (two AdaptationSets,
//! SegmentBase, https or loopback-proxy BaseURLs) and each track is then opened as a seekable
//! progressive fMP4 source through the bundled `mov` demuxer — two independent format contexts,
//! packets alternated onto the two AU lanes. No ABR for DASH v1: trailers are direct-play.

use super::*;
use plx_playback_contract::demux::DemuxMedia;

/// Fetch the MPD text from the shim loopback. The shim serves trailer DASH Parts with 200
/// regardless of token, so when no credential is captured the fetch falls back to an
/// unauthenticated loopback GET (1 MiB cap, UTF-8).
fn dash_fetch_manifest(
    origin: &Origin,
    path: &str,
    aq: *mut AuQueue,
    hs: *mut HttpStream,
    media: &mut dyn DemuxMedia,
    observer: Arc<dyn DemuxObserver>,
    transport: &DemuxTransport,
) -> Result<String, HlsExit> {
    const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
    media.record("dash: fetching trailer manifest");
    let resource = crate::hls::Resource::new(origin.clone(), path)
        .map_err(|_| HlsExit::Failed("invalid DASH manifest URL"))?;
    // The shim serves the MPD over the Engine's plaintext loopback socket, so the HLS opener needs
    // its real `hs` — a null here would be a null deref in the redirect/socket path.
    let mut net = HlsNet {
        hs,
        curl: None,
        range: Arc::clone(&transport.range),
        credential_gate: transport.credential_gate,
        observer: Arc::clone(&observer),
        // Upstream's Live TV segment carry is never armed by a trailer DASH playlist.
        carry: None,
    };
    let (mut src, size, _landed) = if let Ok(auth) = crate::hls::InheritedAuth::capture(&resource) {
        let request_path = auth
            .request_path(&resource)
            .map_err(|_| HlsExit::Failed("DASH manifest credential rejected"))?;
        hls_open_source(
            &resource,
            &request_path,
            aq,
            &mut net,
            None,
            &mut plx_base::checkpoint::NoCheckpoint,
        )?
    } else {
        // No credential on the Part URL (the shim's trailer Part answers 200 anyway): the same
        // loopback open without a token.
        let request_path = resource.path.clone();
        hls_open_source(
            &resource,
            &request_path,
            aq,
            &mut net,
            None,
            &mut plx_base::checkpoint::NoCheckpoint,
        )?
    };
    if size > MAX_MANIFEST_BYTES as i64 {
        return Err(HlsExit::Failed("DASH manifest exceeds size cap"));
    }
    let mut body = Vec::with_capacity(if size > 0 { size as usize } else { 4096 });
    let mut chunk = [0u8; 16 * 1024];
    loop {
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        let n = hls_source_read(&mut src, aq, &mut chunk, None, observer.as_ref())?;
        if n == 0 {
            break;
        }
        if body.len().saturating_add(n) > MAX_MANIFEST_BYTES {
            return Err(HlsExit::Failed("DASH manifest exceeds size cap"));
        }
        body.extend_from_slice(&chunk[..n]);
    }
    if !hls_body_complete(size, body.len() as i64) {
        return Err(HlsExit::Failed("DASH manifest body ended before Content-Length"));
    }
    String::from_utf8(body).map_err(|_| HlsExit::Failed("DASH manifest is not UTF-8"))
}

/// Open one track's BaseURL (the `http://127.0.0.1` trailer-proxy URL, or the raw https
/// googlevideo URL when the proxy is unavailable) as an owned byte-range source through the same
/// credential-bound transport the rest of the feed uses. Never logs the URL body; only the
/// transport error.
fn dash_open_track(
    label: &str,
    url: &str,
    aq: *mut AuQueue,
    media: &mut dyn DemuxMedia,
    observer: &dyn DemuxObserver,
    transport: &DemuxTransport,
) -> Result<(Src, i64), HlsExit> {
    let reservation = transport.range.reserve_open().map_err(|e| {
        media.record(&format!("dash: {label} track reservation failed: {e:?}"));
        if e == range_io::OpenErr::Aborted {
            HlsExit::Aborted
        } else {
            HlsExit::Failed("DASH track reservation failed")
        }
    })?;
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        return Err(HlsExit::Aborted);
    }
    let cs = transport
        .range
        .open_reserved_checked(url, 0, reservation, None, &mut plx_base::checkpoint::NoCheckpoint)
        .map_err(|e| {
            if let range_io::OpenErr::Status(st) = e {
                observer.publish(DemuxCallbackEffect::HttpStatus(st));
            }
            media.record(&format!("dash: {label} track open failed: {e:?}"));
            if e == range_io::OpenErr::Aborted {
                HlsExit::Aborted
            } else {
                HlsExit::Failed("DASH track open failed")
            }
        })?;
    let size = cs.size();
    observer.publish(DemuxCallbackEffect::FileSize(size));
    Ok((Src::Curl(cs), size))
}

/// A seekable progressive fMP4 input (init + segments contiguous): the same AVIO construction as
/// the progressive open, with `seek_cb` so the `mov` demuxer can reach the `sidx`/moov like any
/// seekable MP4.
unsafe fn dash_input(
    src: Src,
    size: i64,
    aq: *mut AuQueue,
    observer: Arc<dyn DemuxObserver>,
    transport: &DemuxTransport,
) -> Result<HlsInput, HlsExit> {
    let reserve_deadline = ReserveDeadlineState::new(None, false, Arc::clone(&observer));
    let mut input = HlsInput {
        state: Box::new(AvioState {
            src,
            range: Arc::clone(&transport.range),
            credential_gate: transport.credential_gate,
            observer,
            aq,
            off: 0,
            size,
            io_failed: false,
            body_active_us: 0,
            body_bytes: 0,
            first_byte_at: None,
            reserve_deadline,
            transport_watchdog: None,
            acquisition: None,
            bounce: Vec::new(),
            bounce_pos: 0,
            parked_at_end: false,
            body_silence: BODY_SILENCE,
            heal: HealBudget::default(),
        }),
        avio: std::ptr::null_mut(),
        fmt: std::ptr::null_mut(),
        pkt: std::ptr::null_mut(),
    };
    let buf = av_malloc(65536) as *mut u8;
    if buf.is_null() {
        return Err(HlsExit::Failed("DASH AVIO buffer allocation failed"));
    }
    input.avio = avio_alloc_context(
        buf,
        65536,
        0,
        &mut *input.state as *mut AvioState as *mut c_void,
        Some(read_cb),
        None,
        Some(seek_cb),
    );
    if input.avio.is_null() {
        free_ptr(buf as *mut c_void);
        return Err(HlsExit::Failed("DASH AVIO allocation failed"));
    }
    input.fmt = avformat_alloc_context();
    if input.fmt.is_null() {
        return Err(HlsExit::Failed("DASH format allocation failed"));
    }
    (*input.fmt).pb = input.avio;
    let opened = avformat_open_input(
        &mut input.fmt,
        std::ptr::null(),
        std::ptr::null_mut(),
        std::ptr::null_mut(),
    );
    if crate::aq::aq_is_aborted(aq) {
        return Err(HlsExit::Aborted);
    }
    if opened < 0 || input.fmt.is_null() {
        plx_base::eventlog::log(&open_input_failure_note(opened, crate::aq::aq_is_aborted(aq)));
        return Err(HlsExit::Failed("DASH track open/probe failed"));
    }
    input.state.io_failed = false;
    if avformat_find_stream_info(input.fmt, std::ptr::null_mut()) < 0 {
        return Err(HlsExit::Failed("DASH track stream discovery failed"));
    }
    input.state.io_failed = false;
    input.pkt = av_packet_alloc();
    if input.pkt.is_null() {
        return Err(HlsExit::Failed("DASH packet allocation failed"));
    }
    Ok(input)
}

/// Everything the DASH demux borrows from the feed's single `demux` call. Grouped so the call
/// site stays small and the lifetimes of the session's media port and observer are explicit.
pub(super) struct DashContext<'a> {
    pub(super) origin: &'a Origin,
    pub(super) path: &'a str,
    pub(super) acodec: &'a str,
    pub(super) aq: *mut AuQueue,
    pub(super) aqa: *mut AuQueue,
    pub(super) hs: *mut HttpStream,
    pub(super) media: &'a mut dyn DemuxMedia,
    pub(super) observer: Arc<dyn DemuxObserver>,
    pub(super) transport: &'a DemuxTransport,
}

pub(super) fn dash_demux(ctx: DashContext<'_>) -> Result<(), HlsExit> {
    let DashContext { origin, path, acodec, aq, aqa, hs, media, observer, transport } = ctx;
    media.record("dash: demux start");
    if unsafe { crate::aq::aq_is_aborted(aq) } {
        return Err(HlsExit::Aborted);
    }
    let xml = dash_fetch_manifest(origin, path, aq, hs, media, Arc::clone(&observer), transport)?;
    let manifest = plx_nuvio_dash::parse_manifest(&xml).map_err(|e| {
        media.record(&format!("dash: manifest rejected: {e}"));
        HlsExit::Failed("DASH manifest rejected")
    })?;
    media.record(&format!(
        "dash: manifest video={} {}x{} audio={} dur_ms={:?}",
        manifest.video.mime,
        manifest.video.width,
        manifest.video.height,
        manifest.audio.mime,
        manifest.duration_ms
    ));
    if let Some(ms) = manifest.duration_ms {
        media.publish(DemuxEffect::DurationNs(ms.saturating_mul(1_000_000)));
    }
    let (vsrc, vsize) = dash_open_track("video", &manifest.video.base_url, aq, media, observer.as_ref(), transport)?;
    let (asrc, asize) = dash_open_track("audio", &manifest.audio.base_url, aq, media, observer.as_ref(), transport)?;

    let mut vinput = unsafe { dash_input(vsrc, vsize, aq, Arc::clone(&observer), transport) }?;
    let mut ainput = unsafe { dash_input(asrc, asize, aq, Arc::clone(&observer), transport) }?;

    let vstreams = unsafe { (*vinput.fmt).streams };
    let vi = unsafe {
        av_find_best_stream(vinput.fmt, AVMEDIA_TYPE_VIDEO, -1, -1, std::ptr::null_mut(), 0)
    };
    if vi < 0 {
        media.record("dash: video track has no video stream");
        return Err(HlsExit::Failed("DASH video track has no video stream"));
    }
    let vst = unsafe { *vstreams.add(vi as usize) };
    let vcp = unsafe { stream_codecpar(vst) };
    let astreams = unsafe { (*ainput.fmt).streams };
    let ai = unsafe {
        audio_stream_matching(ainput.fmt, acodec).unwrap_or_else(|| {
            av_find_best_stream(ainput.fmt, AVMEDIA_TYPE_AUDIO, -1, -1, std::ptr::null_mut(), 0)
        })
    };
    if ai < 0 {
        media.record("dash: audio track has no audio stream");
        return Err(HlsExit::Failed("DASH audio track has no audio stream"));
    }
    let ast = unsafe { *astreams.add(ai as usize) };
    let acp = unsafe { stream_codecpar(ast) };
    let aac_adts = if unsafe { (*acp).codec_id } == AV_CODEC_ID_AAC {
        unsafe { adts_params(acp) }
    } else {
        None
    };

    let Some(framing) = VideoFraming::of_codec_id(unsafe { (*vcp).codec_id }) else {
        let cname = unsafe {
            std::ffi::CStr::from_ptr(avcodec_get_name((*vcp).codec_id)).to_string_lossy()
        };
        media.record(&format!(
            "dash: no framing for video codec {cname} (codec_id={}); refusing the stream",
            unsafe { (*vcp).codec_id }
        ));
        return Err(HlsExit::Failed("DASH video codec has no framing"));
    };
    let is_hevc = framing == VideoFraming::AnnexB { hevc: true };
    let is_av1 = framing == VideoFraming::Av1;
    let is_vp9 = framing == VideoFraming::Vp9;
    let (param_blob, nal_len_size) = if is_av1 || is_vp9 {
        (Vec::new(), 0)
    } else {
        unsafe {
            parse_extradata(
                (*vcp).extradata,
                (*vcp).extradata_size.max(0) as usize,
                is_hevc,
            )
        }
    };
    let av1_ed: &[u8] = if unsafe { (*vcp).extradata.is_null() } || unsafe { (*vcp).extradata_size } <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts((*vcp).extradata, (*vcp).extradata_size as usize) }
    };
    let av1_cfg = if is_av1 { parse_av1c(av1_ed) } else { Vec::new() };
    let av1_framing = if is_av1 {
        Av1Framing::from_trigger(plx_base::devtrig::read("av1obu").as_deref())
    } else {
        Av1Framing::DEFAULT
    };
    media.publish(DemuxEffect::ContainerHdr(if is_vp9 {
        Some(unsafe { container_hdr(vcp) })
    } else {
        None
    }));
    let dur = unsafe { fmt_duration(vinput.fmt) };
    if dur > 0 && manifest.duration_ms.is_none() {
        media.publish(DemuxEffect::DurationNs(dur.saturating_mul(1000)));
    }
    media.publish(DemuxEffect::VideoRaster {
        width: unsafe { (*vcp).width },
        height: unsafe { (*vcp).height },
    });
    let cname = unsafe {
        std::ffi::CStr::from_ptr(avcodec_get_name((*vcp).codec_id)).to_string_lossy()
    };
    media.record(&format!(
        "dash: v=#{vi} codec={cname} codec_id={} {}x{} a=#{ai} dur_ns={}",
        unsafe { (*vcp).codec_id },
        unsafe { (*vcp).width },
        unsafe { (*vcp).height },
        observer.duration_ns().unwrap_or(0)
    ));

    let mut aubuf: Vec<u8> = Vec::with_capacity(4 * 1024 * 1024);
    let mut dv_area = DvActiveArea::default();
    let mut video_done = false;
    let mut audio_done = false;
    // Media time of the last video AU pushed. The audio lane is paced against THIS, not against
    // a wall clock or a byte count: the two fMP4 tracks have different AU rates (~24 frames/s of
    // 4K VP9 vs ~43 AAC frames/s), so one audio AU per video frame let audio media time fall
    // further behind every second until Starfish's audio clock ran out of data and went silent.
    let mut latest_video_pts_ns = 0i64;
    while !video_done || !audio_done {
        if unsafe { crate::aq::aq_is_aborted(aq) } {
            return Err(HlsExit::Aborted);
        }
        if !video_done {
            // Read one relevant video packet.
            loop {
                let r = unsafe { av_read_frame(vinput.fmt, vinput.pkt) };
                if unsafe { crate::aq::aq_is_aborted(aq) } {
                    return Err(HlsExit::Aborted);
                }
                if r < 0 {
                    let _ = frame_read_failed(&mut vinput.state, r);
                    if vinput.state.io_failed {
                        return Err(HlsExit::Failed("DASH video track transport failed"));
                    }
                    video_done = true;
                    break;
                }
                if unsafe { (*vinput.pkt).stream_index } != vi {
                    unsafe { av_packet_unref(vinput.pkt) };
                    continue;
                }
                let pkt_bytes: &[u8] = if unsafe { (*vinput.pkt).data.is_null() } {
                    &[]
                } else {
                    unsafe {
                        std::slice::from_raw_parts(
                            (*vinput.pkt).data,
                            (*vinput.pkt).size.max(0) as usize,
                        )
                    }
                };
                let kf = if is_av1 {
                    Keyframe::of(av1_packet_to_tu(pkt_bytes, unsafe { (*vinput.pkt).flags } & AV_PKT_FLAG_KEY != 0, &av1_cfg, av1_framing, &mut aubuf))
                } else if is_vp9 {
                    Keyframe::of(vp9_packet_to_frame(pkt_bytes, unsafe { (*vinput.pkt).flags } & AV_PKT_FLAG_KEY != 0, &mut aubuf))
                } else {
                    unsafe {
                        packet_to_annexb(
                            (*vinput.pkt).data,
                            (*vinput.pkt).size.max(0) as usize,
                            nal_len_size,
                            is_hevc,
                            false,
                            &mut dv_area,
                            (*vinput.pkt).flags & AV_PKT_FLAG_KEY != 0,
                            &param_blob,
                            &mut aubuf,
                        )
                    }
                };
                let is_key = kf.is_key();
                let pts = unsafe { pts_ns(vinput.pkt, vst) };
                let pushed = crate::aq::aq_push_with_drain(
                    aq,
                    aubuf.as_ptr(),
                    aubuf.len() as c_int,
                    pts,
                    if is_key { 1 } else { 0 },
                    1,
                    || vinput.state.drain_wire(),
                );
                unsafe { av_packet_unref(vinput.pkt) };
                if pushed != 0 {
                    return Err(HlsExit::Aborted);
                }
                PUSHED_ANY.store(true, Ordering::Relaxed);
                latest_video_pts_ns = pts;
                media.publish(DemuxEffect::VideoTailNs(pts));
                break;
            }
        }
        if !audio_done && FEED_AUDIO.load(Ordering::Relaxed) {
            // Read as many audio AUs as it takes to reach the video lane's media time (bounded),
            // not exactly one. The old fork's 1:1 loop made audio fall ~16 s behind a 4K trailer.
            let mut pushed_this_tick = 0usize;
            loop {
                let r = unsafe { av_read_frame(ainput.fmt, ainput.pkt) };
                if unsafe { crate::aq::aq_is_aborted(aq) } {
                    return Err(HlsExit::Aborted);
                }
                if r < 0 {
                    let _ = frame_read_failed(&mut ainput.state, r);
                    if ainput.state.io_failed {
                        return Err(HlsExit::Failed("DASH audio track transport failed"));
                    }
                    audio_done = true;
                    break;
                }
                if unsafe { (*ainput.pkt).stream_index } != ai {
                    unsafe { av_packet_unref(ainput.pkt) };
                    continue;
                }
                let pts = unsafe { pts_ns(ainput.pkt, ast) };
                let pushed = if let Some((freq_idx, chan_cfg)) = aac_adts {
                    let plen = unsafe { (*ainput.pkt).size.max(0) as usize };
                    let mut framed = Vec::with_capacity(7 + plen);
                    framed.extend_from_slice(&adts_header(freq_idx, chan_cfg, plen));
                    framed.extend_from_slice(unsafe {
                        std::slice::from_raw_parts((*ainput.pkt).data, plen)
                    });
                    crate::aq::aq_push_with_drain(
                        aqa,
                        framed.as_ptr(),
                        framed.len() as c_int,
                        pts,
                        1,
                        2,
                        || ainput.state.drain_wire(),
                    )
                } else {
                    crate::aq::aq_push_with_drain(
                        aqa,
                        unsafe { (*ainput.pkt).data },
                        unsafe { (*ainput.pkt).size },
                        pts,
                        1,
                        2,
                        || ainput.state.drain_wire(),
                    )
                };
                unsafe { av_packet_unref(ainput.pkt) };
                if pushed != 0 {
                    return Err(HlsExit::Aborted);
                }
                media.publish(DemuxEffect::AudioTailNs(pts));
                pushed_this_tick += 1;
                if !audio_needs_catch_up(pts, latest_video_pts_ns)
                    || pushed_this_tick >= MAX_AUDIO_AUS_PER_TICK
                {
                    break;
                }
            }
        } else if !audio_done {
            audio_done = true;
        }
    }
    media.record("dash: demux ended");
    let _ = hs; // the engine's plaintext handle is unused for the shim's loopback/HTTPS tracks
    Ok(())
}

/// More audio AUs are needed to bring the audio lane's media time up to the video lane's. Kept
/// pure so the pacing rule — the thing that made trailer audio cut out — is directly testable
/// without a demux or a television.
fn audio_needs_catch_up(audio_pts_ns: i64, video_pts_ns: i64) -> bool {
    video_pts_ns > 0 && audio_pts_ns >= 0 && audio_pts_ns < video_pts_ns
}

/// One demux iteration never pushes more than this many audio AUs, even if the audio track's pts
/// are far behind: a broken manifest must not turn this into an unbounded read loop on the demux
/// thread. The real trailer needs about two per video frame.
const MAX_AUDIO_AUS_PER_TICK: usize = 12;

#[cfg(test)]
mod pacing_tests {
    use super::*;

    #[test]
    fn audio_lagging_video_is_caught_up() {
        // 4K VP9 frame = 42 ms, AAC AU = 23.2 ms: two AUs close the gap.
        assert!(audio_needs_catch_up(23_219_955, 42_000_000));
        assert!(!audio_needs_catch_up(46_439_909, 42_000_000));
    }

    #[test]
    fn audio_ahead_or_before_the_timeline_is_not_caught_up() {
        assert!(!audio_needs_catch_up(50_000_000, 42_000_000), "audio already ahead");
        assert!(!audio_needs_catch_up(23_219_955, 0), "no video pushed yet");
        assert!(!audio_needs_catch_up(-1, 42_000_000), "unknown audio pts");
    }

    #[test]
    fn the_catch_up_budget_is_bounded() {
        assert_eq!(MAX_AUDIO_AUS_PER_TICK, 12);
    }
}
