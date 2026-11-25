//! Fork Rofocale11: the shim's subtitle CANDIDATE cache.
//
// The player asks for subtitles on every detail open and again from the Search page; the automatic
// lane and the builtin OpenSubtitles provider both fan out over the network. Keeping the candidates
// in the one redb store saves that fan-out on a reopen, but only if the cache knows what it is
// storing. The old `Vec`-returning backend call could not tell an addon that timed out from one
// that answered `{"subtitles":[]}`; persisting that answer would make a provider outage look like
// "this title has no subtitles" for the whole TTL. So the three states are kept apart:
//
// - a successful non-empty answer — cached for [`CANDIDATE_TTL`];
// - a real empty answer — cached for the much shorter [`EMPTY_TTL`], because a newly installed
//   addon can change it;
// - a provider fault — NEVER cached. Only a short in-memory [`FAULT_BACKOFF`] stops a dead host
//   being asked on every poll, and an explicit Retry (or the backoff) asks it again.
//
// This is a fork-owned file; the only edit to `shim_pms.rs` is the field, the routed call sites and
// the `addons_changed()` clear.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use plx_stremio::model::{Fetch, Subtitle};

/// Candidates are stable for a long time (release names and languages do not change).
pub const CANDIDATE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
/// "No subtitles" is real information, but it is not permanent — a new addon can supply them.
pub const EMPTY_TTL: Duration = Duration::from_secs(10 * 60);
/// A fault is not an answer. Keep the backoff in memory only; it must not survive a restart as a
/// reason to refuse to ask a now-healthy provider.
pub const FAULT_BACKOFF: Duration = Duration::from_secs(30);

const MAX_ROWS: usize = 512;
const MAX_CANDIDATES: usize = 80;

#[derive(serde::Serialize, serde::Deserialize)]
struct Answer {
    empty: bool,
    subs: Vec<Subtitle>,
}

#[derive(Default)]
struct Inner {
    rows: HashMap<String, (Instant, bool, Arc<Vec<Subtitle>>)>,
    faults: HashMap<String, Instant>,
}

/// One cache for the whole shim. Rows are shared as `Arc<Vec<_>>` so a reader never copies a list
/// under the lock; the redb table does the persistence and applies the 50 MiB budget's shedding.
pub struct SubtitleCache {
    inner: Mutex<Inner>,
    flight: crate::shim_flight::Flight,
}

impl Default for SubtitleCache {
    fn default() -> Self {
        SubtitleCache { inner: Mutex::new(Inner::default()), flight: crate::shim_flight::Flight::default() }
    }
}

impl SubtitleCache {
    pub fn open() -> Arc<SubtitleCache> {
        Arc::new(SubtitleCache::default())
    }

    /// The redb key for one item. The type is part of it: a movie and a show can share an id.
    pub fn key(type_: &str, id: &str) -> String {
        format!("{type_}\u{0}{id}")
    }

    fn ttl(empty: bool) -> Duration {
        if empty { EMPTY_TTL } else { CANDIDATE_TTL }
    }

    /// The process-wide redb store is shared by every unit test in this crate, so a row one test
    /// writes would silently answer another test's shim. Unit tests therefore keep the cache in
    /// memory only; the production build reads and writes the durable table.
    fn durable_get(&self, key: &str) -> Option<Vec<u8>> {
        #[cfg(test)]
        {
            let _ = key;
            None
        }
        #[cfg(not(test))]
        {
            plx_nuvio_store::get()?.cache_get(plx_nuvio_store::NuvioCache::Subtitles, key)
        }
    }

    fn durable_put(&self, key: &str, bytes: &[u8], ttl: Duration) {
        #[cfg(test)]
        {
            let _ = (key, bytes, ttl);
        }
        #[cfg(not(test))]
        {
            if let Some(store) = plx_nuvio_store::get() {
                let _ = store.cache_put(plx_nuvio_store::NuvioCache::Subtitles, key, ttl, bytes);
            }
        }
    }

    /// A live answer, from memory or from the durable table. A row past its TTL is dropped, not
    /// returned — the same rule `plx_nuvio_store::cache_get` applies to the on-disk envelope.
    pub fn get(&self, key: &str) -> Option<Arc<Vec<Subtitle>>> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, empty, rows)) = g.rows.get(key) {
            if at.elapsed() < Self::ttl(*empty) {
                return Some(Arc::clone(rows));
            }
            g.rows.remove(key);
        }
        let bytes = self.durable_get(key)?;
        let answer: Answer = serde_json::from_slice(&bytes).ok()?;
        let rows = if answer.empty { Arc::new(Vec::new()) } else { Arc::new(answer.subs) };
        if g.rows.len() >= MAX_ROWS {
            if let Some(oldest) = g.rows.iter().min_by_key(|(_, (at, _, _))| *at).map(|(k, _)| k.clone()) {
                g.rows.remove(&oldest);
            }
        }
        g.rows.insert(key.to_string(), (Instant::now(), answer.empty, Arc::clone(&rows)));
        Some(rows)
    }

    /// Is this key inside its short post-fault backoff? A `true` means "do not ask again yet",
    /// never "the answer is empty".
    pub fn faulted(&self, key: &str) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
            .faults.get(key).is_some_and(|at| at.elapsed() < FAULT_BACKOFF)
    }

    fn note_fault(&self, key: &str) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).faults.insert(key.to_string(), Instant::now());
    }

    fn commit(&self, key: &str, fetched: &Fetch<Subtitle>) {
        let empty = fetched.rows.is_empty();
        let answer = Answer { empty, subs: fetched.rows.iter().take(MAX_CANDIDATES).cloned().collect() };
        if let Ok(bytes) = serde_json::to_vec(&answer) {
            self.durable_put(key, &bytes, Self::ttl(empty));
        }
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.faults.remove(key);
        if g.rows.len() >= MAX_ROWS {
            if let Some(oldest) = g.rows.iter().min_by_key(|(_, (at, _, _))| *at).map(|(k, _)| k.clone()) {
                g.rows.remove(&oldest);
            }
        }
        g.rows.insert(key.to_string(), (Instant::now(), empty, Arc::new(answer.subs)));
    }

    /// Drop everything that depends on the addon set (Settings change, profile switch). Faults go
    /// too: a provider that was down under the old set must be asked at once under the new one.
    pub fn clear(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).faults.clear();
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).rows.clear();
        plx_nuvio_store::clear_table(plx_nuvio_store::Table::SubtitleCache);
    }
}

/// Read through the cache, or make exactly one fetch and cache it. A failed fetch is returned to
/// the caller with its rows (partial answers are useful) but sets only the in-memory backoff;
/// nothing about the outage is persisted. `Flight` keeps a miss from being fetched twice when the
/// automatic lane and the Search page ask for the same title together.
pub fn fetch_cached(
    cache: &Arc<SubtitleCache>,
    key: &str,
    fetch: impl FnOnce() -> Fetch<Subtitle>,
) -> Fetch<Subtitle> {
    loop {
        if let Some(rows) = cache.get(key) {
            return Fetch::ok((*rows).clone(), 1, 1);
        }
        if cache.faulted(key) {
            return Fetch::with_fault(Vec::new(), 0, 1);
        }
        let Some(_turn) = cache.flight.claim(key) else { continue };
        let fetched = fetch();
        if !fetched.faulted {
            cache.commit(key, &fetched);
            return fetched;
        }
        cache.note_fault(key);
        return fetched;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(lang: &str) -> Subtitle {
        Subtitle { url: format!("https://example.test/{lang}.srt"), lang: lang.into(), ..Default::default() }
    }

    #[test]
    fn an_empty_answer_is_cached_but_a_fault_is_not() {
        let cache = SubtitleCache::open();
        let key = SubtitleCache::key("movie", "tt-empty");
        let first = fetch_cached(&cache, &key, || Fetch::ok(Vec::new(), 1, 1));
        assert!(!first.faulted && first.rows.is_empty());
        let mut calls = 0;
        let second = fetch_cached(&cache, &key, || { calls += 1; Fetch::ok(vec![row("en")], 1, 1) });
        assert_eq!(calls, 0, "a real empty answer is cached for a short while");
        assert!(second.rows.is_empty());

        let fault_key = SubtitleCache::key("movie", "tt-fault");
        let fault = fetch_cached(&cache, &fault_key, || Fetch::with_fault(Vec::new(), 0, 1));
        assert!(fault.faulted);
        // Inside the backoff, no provider call is made; the returned state is still a FAULT.
        let backed_off = fetch_cached(&cache, &fault_key, || { calls += 1; Fetch::ok(vec![row("en")], 1, 1) });
        assert!(backed_off.faulted && calls == 0);
        cache.inner.lock().unwrap().faults.remove(&fault_key);
        let recovered = fetch_cached(&cache, &fault_key, || Fetch::ok(vec![row("en")], 1, 1));
        assert!(!recovered.faulted && recovered.rows.len() == 1);
    }
}
