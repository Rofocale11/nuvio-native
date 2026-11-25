//! permanent IMDb -> TMDB id lookups (DETAIL-LATENCY Stage 2C).
//!
//! Cinemeta usually sends `moviedb_id`, but not every addon does. The `/find` fallback answers a
//! question that never changes — an IMDb id names one film or one show forever — yet the old code
//! paid for it every 15 minutes when the TMDB response cache expired. This wrapper is a child of
//! the upstream shim's resolver surface: the only edit needed in `shim_pms.rs` is making its
//! existing `TmdbIdLookup` trait visible here, plus wrapping the four real-client call sites.
//!
//! Values in `Table::ImdbTmdb` are plain 8-byte big-endian u64s with no TTL envelope. Negative
//! answers are never stored: a transient `/find` failure must be retried on the next open.

use super::shim_pms::TmdbIdLookup;
use plx_nuvio_store::Table;

pub(crate) struct PersistentIdLookup<'a, L> {
    inner: &'a L,
}

impl<'a, L> PersistentIdLookup<'a, L> {
    pub(crate) fn new(inner: &'a L) -> Self {
        Self { inner }
    }

    /// A permanent row is only meaningful for an IMDb-shaped id. Addon-local ids are not stable
    /// across installs (and a `tmdb:` id needs no lookup at all), so they are never cached.
    fn cached(&self, prefix: &str, id: &str) -> Option<u64> {
        if !id.starts_with("tt") {
            return None;
        }
        let store = plx_nuvio_store::get()?;
        let bytes = store.get(Table::ImdbTmdb, &format!("{prefix}:{id}"))?;
        Some(u64::from_be_bytes(bytes.try_into().ok()?))
    }

    fn persist(&self, prefix: &str, id: &str, tmdb_id: u64) {
        if !id.starts_with("tt") {
            return;
        }
        // A durable store failure must never turn a resolved id into a failed detail open.
        if let Some(store) = plx_nuvio_store::get() {
            let _ = store.put(Table::ImdbTmdb, &format!("{prefix}:{id}"), &tmdb_id.to_be_bytes());
        }
    }
}

impl<L: TmdbIdLookup> TmdbIdLookup for PersistentIdLookup<'_, L> {
    fn movie_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>> {
        if let Some(cached) = self.cached("movie", id) {
            return Ok(Some(cached));
        }
        let found = self.inner.movie_id(id)?;
        if let Some(tmdb_id) = found {
            self.persist("movie", id, tmdb_id);
        }
        Ok(found)
    }

    fn series_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>> {
        if let Some(cached) = self.cached("show", id) {
            return Ok(Some(cached));
        }
        let found = self.inner.series_id(id)?;
        if let Some(tmdb_id) = found {
            self.persist("show", id, tmdb_id);
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The global store is empty in most host tests: install the same one-shim-store fixture the
    /// other persistent-cache tests use. A different test winning the OnceLock is fine — every
    /// key below is unique to this module.
    fn install_test_store() {
        static STORE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        STORE.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("nuvio-tmdb-ids-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let store = plx_nuvio_store::Store::open_at(&dir.join("t.redb")).unwrap();
            let _ = plx_nuvio_store::install(store);
        });
    }

    #[derive(Default)]
    struct CountingLookup {
        movies: AtomicUsize,
        shows: AtomicUsize,
    }

    impl TmdbIdLookup for CountingLookup {
        fn movie_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>> {
            self.movies.fetch_add(1, Ordering::Relaxed);
            Ok(match id {
                "tt9900001" => Some(1001),
                "tt9900002" => Some(1002),
                "tt9900004" => Some(1004),
                "local:9900003" => Some(2003),
                _ => None,
            })
        }
        fn series_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>> {
            self.shows.fetch_add(1, Ordering::Relaxed);
            Ok((id == "tt9900011").then_some(1101))
        }
    }

    #[test]
    fn an_imdb_id_is_looked_up_once_and_a_different_id_again() {
        let _guard = serial();
        install_test_store();
        let lookup = CountingLookup::default();
        let persistent = PersistentIdLookup::new(&lookup);
        assert_eq!(persistent.movie_id("tt9900001").unwrap(), Some(1001));
        assert_eq!(persistent.movie_id("tt9900001").unwrap(), Some(1001));
        assert_eq!(lookup.movies.load(Ordering::Relaxed), 1, "the second answer must come from the durable store");
        assert_eq!(persistent.movie_id("tt9900002").unwrap(), Some(1002));
        assert_eq!(lookup.movies.load(Ordering::Relaxed), 2);
        assert_eq!(persistent.series_id("tt9900011").unwrap(), Some(1101));
        assert_eq!(persistent.series_id("tt9900011").unwrap(), Some(1101));
        assert_eq!(lookup.shows.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_new_wrapper_reads_the_same_permanent_row() {
        let _guard = serial();
        install_test_store();
        let lookup = CountingLookup::default();
        assert_eq!(PersistentIdLookup::new(&lookup).movie_id("tt9900004").unwrap(), Some(1004));
        assert_eq!(PersistentIdLookup::new(&lookup).movie_id("tt9900004").unwrap(), Some(1004));
        assert_eq!(lookup.movies.load(Ordering::Relaxed), 1);
        let Some(store) = plx_nuvio_store::get() else { panic!("the test store is installed") };
        assert_eq!(store.get(Table::ImdbTmdb, "movie:tt9900004").unwrap(), 1004u64.to_be_bytes());
    }

    #[test]
    fn a_non_imdb_id_is_never_cached() {
        let _guard = serial();
        install_test_store();
        let lookup = CountingLookup::default();
        let persistent = PersistentIdLookup::new(&lookup);
        assert_eq!(persistent.movie_id("local:9900003").unwrap(), Some(2003));
        assert_eq!(persistent.movie_id("local:9900003").unwrap(), Some(2003));
        assert_eq!(lookup.movies.load(Ordering::Relaxed), 2, "addon-local ids must not become permanent rows");
        let Some(store) = plx_nuvio_store::get() else { panic!("the test store is installed") };
        assert_eq!(store.get(Table::ImdbTmdb, "movie:local:9900003"), None);
    }
}
