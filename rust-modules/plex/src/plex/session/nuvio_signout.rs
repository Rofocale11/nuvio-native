//! Fork Rofocale11: the Nuvio half of a session sign-out.
//!
//! Upstream's `clear_for_erase` revokes the Plex credentials and sweeps their files. Nuvio also
//! keeps TV-side profile PIN verifiers in the one redb file, and the documented recovery for a
//! forgotten PIN is "Sign out, then sign back in with the phone". Before this hook the verifiers
//! survived sign-out, so that path did not exist and a locked-out viewer stayed locked out.
//!
//! This is a child module of `session` so the edit to the upstream file stays a `mod` declaration
//! and one routed call. It never lifts a second store or a second PIN count into this layer.

use plx_nuvio_store::{Store, Table};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

/// Test-only witness that the routed call actually fired; the real effect lands in redb.
#[cfg(test)]
static CLEAR_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Delete every TV PIN verifier in this store. The keys are `profile-N`, so walking the table
/// covers every slot without duplicating a count this crate cannot import from `plx_stremio`.
fn clear_in(store: &Store) -> usize {
    let keys: Vec<String> = store.list(Table::ProfilePins).into_iter().map(|(key, _)| key).collect();
    let mut removed = 0;
    for key in keys {
        if store.remove(Table::ProfilePins, &key).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Production door, called from `clear_for_erase` only after the canonical credential clear is
/// known durable. A failed sign-out must never let a remote-holder drop a profile lock; and a
/// synthetic boot's private store is the only store `get()` can see here, so a real install's
/// verifiers cannot be cleared from one.
pub(super) fn clear_profile_pins() -> usize {
    #[cfg(test)]
    CLEAR_CALLS.fetch_add(1, Ordering::SeqCst);
    let Some(store) = plx_nuvio_store::get() else { return 0 };
    let removed = clear_in(&store);
    if removed > 0 {
        plx_base::eventlog::log("nuvio: TV PINs cleared on sign-out");
    }
    removed
}

#[cfg(test)]
pub(super) fn clear_calls_for_test() -> usize {
    CLEAR_CALLS.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hook_clears_every_pin_slot() {
        let dir = std::env::temp_dir().join(format!("nuvio-signout-pins-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open_at(&dir.join("t.redb")).unwrap();
        for index in 1..=6 {
            let verifier = plx_nuvio_store::pins::PinVerifier::new("1234").unwrap();
            plx_nuvio_store::pins::put(&store, index, &verifier).unwrap();
        }
        assert_eq!(store.list(Table::ProfilePins).len(), 6);
        assert_eq!(clear_in(&store), 6);
        assert!(store.list(Table::ProfilePins).is_empty(), "no verifier may survive");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_out_routes_through_the_hook() {
        let _serial = plx_base::testlock::serial();
        let _session = super::super::test_support::TempSession::new("signout-tv-pins");
        let before = clear_calls_for_test();
        let report = super::super::clear_for_erase(false, None);
        assert!(matches!(report.outcome, super::super::ClearOutcome::Durable { .. }));
        assert_eq!(clear_calls_for_test(), before + 1, "sign-out must clear the TV PINs");
    }
}
