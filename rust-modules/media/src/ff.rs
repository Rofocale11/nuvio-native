//! The codecs' FFmpeg bindings media's own code reaches (`plx_media_codecs`), and the integration
//! fixtures that drive the physical Feed (`plx_play_feed`) through the player session.
// Fork Rofocale11: the ABI structs and the whole retained low-level core moved to the fork-owned
// `plx_nuvio_ffmpeg` crate, which re-exports `plx_media_codecs::ffi` itself. The single glob below
// is the facade the upstream test modules' `use super::*` sees; a second glob here would be an
// ambiguous re-export. `hidden_glob_reexports` stays for the local `c_int` import.
#![allow(hidden_glob_reexports)]
// Fork Rofocale11: the subtitle/subside children are exercised by the test modules below but are
// not named by production code after the demuxer moved to plx_play_feed; upstream's file carried
// this allow for the same reason.
#![allow(dead_code)]
// Fork Rofocale11: upstream's `SpeedDecoder` re-export is kept; `Venc` now comes from the fork
// crate's glob (the fork's encoder, not media_codecs', is the one this facade has always exposed).
pub use plx_media_codecs::SpeedDecoder;
// Fork Rofocale11: the upstream test files in this module use `use super::*` and expect these two
// standard types from the facade header, where upstream kept them under cfg(test). The fork's
// non-test ABI/probe core no longer needs them (the probe spells out std::ffi::CString::new), so
// the cfg(test) gate keeps `cargo check` free of unused-import errors while the tests compile.
#[cfg(test)]
use plx_net::stream::HttpStream;
#[cfg(test)]
use std::ffi::CString;
#[cfg(test)]
use plx_play_feed::ff_test_support::*;
#[cfg(test)]
use plx_play_feed::{demux, DemuxTransport};
#[cfg(test)]
use plx_media_codecs::subtitle_extract::pts_ns;

// The integration fixtures remain in media because they exercise its player
// session and route adapter. Their physical vocabulary is imported from the
// lower contracts and the same standard types the former local module used.
#[cfg(test)]
use plx_play_feed::aq::AuQueue;
#[cfg(test)]
use plx_playback_contract::adaptive::TrialClock;
#[cfg(test)]
use plx_playback_contract::demux::{DemuxCallbackEffect, DemuxObserver, DemuxSession};
#[cfg(test)]
use plx_play_feed::SendPtr;
#[cfg(test)]
use plx_net::net::origin::Origin;
// `ff_subs.rs` and the retained test modules spell C/atomic types through `use super::*`; in the
// shipped build those names came from the fork crate's glob, so the explicit imports are test-only.
use std::os::raw::c_int;
#[cfg(test)]
use std::os::raw::c_void;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

// Fork Rofocale11: the low-level FFmpeg core (runtime binding table, verified offsets, Dolby
// Vision side-data reader, dev capture encoder, ABI gate and ffprobe smoke test) now lives in the
// fork-owned `plx_nuvio_ffmpeg` crate below this one. Upstream owns this file and deleted that core
// when it moved the demuxer into plx_play_feed; keeping 1300 fork lines here made every upstream
// edit of ff.rs a conflict. The glob preserves the `use super::*` vocabulary of the test modules
// below, and the media tests keep exercising the same symbols from the same ABI table.
pub use plx_nuvio_ffmpeg::*;

/// Boot smoke test + optional ABI probe. Called once at startup.
///
/// Fork Rofocale11: only the `curlio` leg stays here — it is the one call that names an upward
/// layer (media's own HTTPS transport); everything FFmpeg now boots in `plx_nuvio_ffmpeg`.
pub fn boot() {
    crate::curlio::boot();
    plx_nuvio_ffmpeg::boot();
}

#[path = "ff_subs.rs"]
mod subs;
#[path = "ff_subside.rs"]
pub(crate) mod subside;
// Fork Rofocale11: the addon/debrid stream prober moved to the fork-owned `plx_nuvio_probe` crate
// (src/app/nuvio.rs calls it directly). It used to be `#[path = "ff_probe.rs"] pub mod probe;` here,
// borrowing this file's names and a second dynlib! table. Removing the declaration on merge is all
// that is left of the fork in this upstream-owned file for the probe.
#[cfg(test)]
use std::sync::Arc;

// ---------------------------------------------------------------------------------------
#[cfg(test)]
#[path = "ff_test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
#[path = "ff_abr_reject_tests.rs"]
mod abr_reject_tests;

#[cfg(test)]
#[path = "ff_source_teardown_tests.rs"]
mod source_teardown_tests;

#[cfg(test)]
#[path = "ff_hls_reserve_tests.rs"]
mod hls_reserve_tests;

#[cfg(test)]
#[path = "ff_stall_guard_tests.rs"]
mod stall_guard_tests;

#[cfg(test)]
#[path = "ff_image_subtitle_tests.rs"]
pub(crate) mod image_subtitle_tests;

#[cfg(test)]
#[path = "ff_subtitle_charset_tests.rs"]
mod subtitle_charset_tests;

#[cfg(test)]
#[path = "ff_redirect_tests.rs"]
mod redirect_tests;

#[cfg(test)]
#[path = "ff_pcm_decode_tests.rs"]
mod pcm_decode_tests;

#[cfg(test)]
#[path = "ff_late_audio_tests.rs"]
mod late_audio_tests;

#[cfg(test)]
#[path = "ff_eof_seek_tests.rs"]
mod eof_seek_tests;
#[cfg(test)]
#[path = "ff_body_heal_tests.rs"]
mod body_heal_tests;

#[cfg(test)]
#[path = "ff_subs_tests.rs"]
mod subs_tests;
