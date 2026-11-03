//! One fetch per key at a time, with everyone else waiting for its answer.
//!
//! A check-then-fetch cache with the lock released during the fetch (the shape both the shim's
//! addon-meta cache and the TMDB response cache use) lets two callers miss together and both go to
//! the network. That is exactly what the detail prefetch and the click it is warming produce: the
//! focus dwell starts a fetch, the click starts the same one, and the second caller pays for a
//! download the first is already making.
//!
//! [`Flight::claim`] gives one caller the turn for a key ([`Claim`]); every other caller blocks
//! until that turn ends and then re-reads its cache. The cache itself stays the caller's: this
//! module only serialises the fetch, so a failed fetch (or a panicking one, through `Drop`) simply
//! releases the next caller to try.
//!
//! Callers must re-check their cache after `None` — the finished fetch may have produced it, and
//! may not have. The usual shape is a loop:
//!
//! ```ignore
//! loop {
//!     if let Some(hit) = cache.read(&key) { return hit; }
//!     let Some(_turn) = flight.claim(&key) else { continue };
//!     let value = fetch()?;      // dropping `_turn` releases the waiters
//!     cache.write(key, value.clone());
//!     return value;
//! }
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};

/// The keyed single-flight registry. One per cache it fronts.
#[derive(Default, Debug)]
pub struct Flight {
    gates: Mutex<HashMap<String, Arc<Gate>>>,
}

#[derive(Default, Debug)]
struct Gate {
    done: Mutex<bool>,
    cv: Condvar,
}

/// The turn granted to one caller for one key. Dropping it — normally, by a panic, or after a
/// failed fetch — releases every waiter, who then re-checks its cache.
pub struct Claim<'a> {
    flight: &'a Flight,
    key: String,
    gate: Arc<Gate>,
}

impl Flight {
    /// Take the turn for `key`, or wait for the caller that holds it and return `None`.
    ///
    /// `Some` means this caller must perform the fetch and keep the [`Claim`] alive until the
    /// result is stored (a block scoped around both is enough). `None` means a fetch for the same
    /// key finished while this caller waited: re-read the cache and, if it is still empty, call
    /// `claim` again — the failed or empty fetch has released the key.
    ///
    /// Never claim a key while this thread already holds its [`Claim`]: the second call waits for
    /// the first, which only ends when this call returns.
    pub fn claim(&self, key: &str) -> Option<Claim<'_>> {
        let gate = {
            let mut gates = self.gates.lock().unwrap_or_else(|e| e.into_inner());
            match gates.get(key) {
                Some(gate) => Arc::clone(gate),
                None => {
                    let gate = Arc::new(Gate::default());
                    gates.insert(key.to_string(), Arc::clone(&gate));
                    return Some(Claim {
                        flight: self,
                        key: key.to_string(),
                        gate,
                    });
                }
            }
        };
        // The map lock is released before waiting: the leader removes its own gate from the map
        // on the way out and must never need this lock while a waiter holds it.
        let mut done = gate.done.lock().unwrap_or_else(|e| e.into_inner());
        while !*done {
            done = gate.cv.wait(done).unwrap_or_else(|e| e.into_inner());
        }
        None
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        // Remove first, so the next caller can become the leader as soon as the turn ends; the
        // waiters below only ever look at this gate, never at the map.
        self.flight
            .gates
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
        let mut done = self.gate.done.lock().unwrap_or_else(|e| e.into_inner());
        *done = true;
        self.gate.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testrace::race;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::OnceLock;
    use std::time::Duration;

    /// Four callers released together: exactly one is handed the turn, and the others return only
    /// after it ends.
    #[test]
    fn one_caller_leads_the_key_and_the_rest_wait_for_it() {
        static FLIGHT: OnceLock<Flight> = OnceLock::new();
        static LEADS: AtomicUsize = AtomicUsize::new(0);
        static WOKEN_AFTER_LEAD: AtomicUsize = AtomicUsize::new(0);
        let flight = FLIGHT.get_or_init(Flight::default);
        race(4, move |_| {
            match flight.claim("same-key") {
                Some(_turn) => {
                    // Hold the turn until the others have arrived at the gate, so this really is
                    // the overlap a prefetch and a click produce rather than four serial claims.
                    std::thread::sleep(Duration::from_millis(50));
                    LEADS.fetch_add(1, Ordering::SeqCst);
                }
                None => {
                    assert_eq!(LEADS.load(Ordering::SeqCst), 1, "a waiter returned before the lead finished");
                    WOKEN_AFTER_LEAD.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        assert_eq!(LEADS.load(Ordering::SeqCst), 1);
        assert_eq!(WOKEN_AFTER_LEAD.load(Ordering::SeqCst), 3);
    }

    /// A lead that finds nothing (an addon refused the read) releases the key instead of leaving
    /// its waiters parked until the process ends.
    #[test]
    fn a_failed_fetch_releases_the_key_to_the_next_caller() {
        static FLIGHT: OnceLock<Flight> = OnceLock::new();
        let flight = FLIGHT.get_or_init(Flight::default);
        let key = "refused";
        let (arrived, waiter_started) = std::sync::mpsc::channel();
        let (made, first_lead) = std::sync::mpsc::channel();
        let lead = flight.claim(key).expect("the test is the first caller");
        std::thread::scope(|scope| {
            scope.spawn(move || {
                arrived.send(()).unwrap();
                // `None` is the first (finished, empty) answer; the loop's second claim is ours.
                assert!(flight.claim(key).is_none(), "a waiter must not lead while a fetch is in flight");
                let second = flight.claim(key).expect("the abandoned key is free again");
                made.send(()).unwrap();
                drop(second);
            });
            waiter_started.recv().unwrap();
            // Give the waiter time to reach the gate before the lead gives up.
            std::thread::sleep(Duration::from_millis(50));
            drop(lead);
            first_lead.recv_timeout(Duration::from_secs(10)).expect("the waiter never took the released key");
        });
    }

    /// Distinct keys do not wait on each other — the prefetch of one card must never serialize
    /// behind another card's fetch.
    #[test]
    fn distinct_keys_lead_at_the_same_time() {
        static FLIGHT: OnceLock<Flight> = OnceLock::new();
        let flight = FLIGHT.get_or_init(Flight::default);
        let first = flight.claim("a").expect("key a is free");
        let second = flight.claim("b").expect("key b leads while a is still held");
        drop(first);
        assert!(flight.claim("a").is_some(), "a finished key can be claimed again");
        drop(second);
    }
}
