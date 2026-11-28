//! Perform a pick from the player's Streams (`…` → Streams → addon → stream).
//!
//! Fork Rofocale11: the shim write is HTTP and MUST NOT run on the frame thread. The Detail
//! picker already sends `NuvioReq::Select` as a typed effect; the player's `…` menu used to call
//! `client.nuvio_select()` inline in `apply_more_action`, blocking up to 25 s (`net::API`) with
//! the HUD frozen. The call now runs on an application worker and its answer returns through a
//! small mailbox that the frame pump drains, where the fresh resolve is still performed.
//!
//! Kept in a fork-owned file so `app/playback.rs` carries only one marked arm and an upstream
//! merge never has to reconcile this logic.

use plx_base::eventlog::log;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

struct PickResult {
    ok: bool,
    rk: String,
    index: usize,
}

static RESULTS: Mutex<Vec<PickResult>> = Mutex::new(Vec::new());
static PENDING: AtomicBool = AtomicBool::new(false);

/// Commit `index` on the playing item's shim, then restart its playback at the same position.
/// The HTTP half leaves this function on a worker; the reload happens in `drain_pick_results`.
pub(crate) fn apply_stream_pick(
    ps: &mut plx_media::route::PlaybackSession,
    _pa: &mut plx_media::player::adapter::PlayerAdapter,
    _bridge: &mut super::bridge::Bridge,
    _present: &mut plx_machine::present::Present,
    index: usize,
) {
    let sid = plx_media::route::cur_sid(ps);
    let rk = plx_media::route::cur_rk(ps);
    let Some(client) = plx_plex::plex::client_for(sid) else {
        log("nuvio: stream pick refused — no server for the playing item");
        return;
    };
    if !client.is_nuvio() {
        return;
    }
    if PENDING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_err() {
        log("nuvio: stream pick refused — a pick is already in flight");
        return;
    }
    let k = rk.clone();
    plx_base::eventlog::log("nuvio: stream pick on worker");
    let queued = plx_base::task::spawn("nuvio-stream-pick", move || {
        let ok = client.nuvio_select(&k, index);
        if let Ok(mut results) = RESULTS.lock() {
            results.push(PickResult { ok, rk: k, index });
        }
        plx_machine::present::wake_from_worker();
    });
    if queued.is_none() {
        PENDING.store(false, Ordering::Release);
        log("nuvio: stream pick refused — no worker thread available");
    }
}

/// Frame pump: apply a worker's answered pick. No IO here — only a mutex drain and the same
/// route reload the old inline path performed. A result for an item that is no longer playing is
/// dropped, so leaving the player mid-request cannot start playback again.
pub(crate) fn drain_pick_results(
    ps: &mut plx_media::route::PlaybackSession,
    pa: &mut plx_media::player::adapter::PlayerAdapter,
    bridge: &mut super::bridge::Bridge,
    present: &mut plx_machine::present::Present,
) {
    let results: Vec<PickResult> = {
        let mut results = RESULTS.lock().unwrap_or_else(|e| e.into_inner());
        results.drain(..).collect()
    };
    if results.is_empty() {
        return;
    }
    PENDING.store(false, Ordering::Release);
    let playing = plx_media::route::cur_rk(ps);
    for result in results {
        if result.rk != playing {
            log(&format!("nuvio: stream pick {} for {} ignored — item no longer playing",
                result.index, result.rk));
            continue;
        }
        if !result.ok {
            log(&format!("nuvio: stream pick {} for {} refused by the shim",
                result.index, result.rk));
            continue;
        }
        // The old fork's `SetStream` did exactly this: the shim has already rewritten the item's
        // `Media[]` so the chosen stream is first, and a real stop + fresh resolve is what makes
        // the NEW file play. A routing policy (quality ladder) would be the wrong tool here —
        // there is no cap to move, the stream IS the quality.
        if super::playback::retry_failed_playback(
            ps,
            pa,
            bridge.metadata_mut(),
            None,
            plx_media::player::report::AttemptOrigin::QualityChange,
        ) {
            present.note_in_scope(
                plx_machine::present::Scope::Page,
                plx_machine::present::PresentEvent::Damage(plx_machine::present::Provenance::Input),
            );
        }
    }
}
