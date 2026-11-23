//! Fork Rofocale11: perform a pick from the player's Streams (`…` → Streams → addon → stream).
//!
//! This is the application half of the typed effect: the screen only reports an index, this module
//! does the shim write on the caller's thread (the app is the runner, not a machine) and starts a
//! fresh resolve of the playing item. Kept in a fork-owned file so `app/playback.rs` carries only
//! one marked match arm and an upstream merge never has to reconcile this logic.
use plx_base::eventlog::log;

/// Commit `index` on the playing item's shim and restart its playback at the same position.
/// `false` from the shim (a non-Nuvio server, a refused select) is a no-op with a log line, never
/// a change of what is on screen.
pub(crate) fn apply_stream_pick(
    ps: &mut plx_media::route::PlaybackSession,
    pa: &mut plx_media::player::adapter::PlayerAdapter,
    bridge: &mut super::bridge::Bridge,
    present: &mut plx_machine::present::Present,
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
    if !client.nuvio_select(&rk, index) {
        log(&format!("nuvio: stream pick {index} for {rk} refused by the shim"));
        return;
    }
    // The old fork's `SetStream` did exactly this: the shim has already rewritten the item's
    // `Media[]` so the chosen stream is first, and a real stop + fresh resolve is what makes the
    // NEW file play. A routing policy (quality ladder) would be the wrong tool here — there is no
    // cap to move, the stream IS the quality.
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
