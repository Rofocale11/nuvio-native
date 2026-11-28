//! Durable catalog pages for the shim's Home/Library shelves — stale-while-revalidate.
//!
//! The catalog rows used to live only in [`crate::shim_pms`]'s in-memory `catalogs` map, so every
//! TV relaunch re-fetched the first page of every addon catalog (the ~15 s cold launch). This
//! module keeps the last successful fill in the one redb store. A relaunch with an unchanged addon
//! set paints Home/Library from those rows immediately, then revalidates them in the background.
//!
//! [`CATALOG_TTL`] is a FRESHNESS hint, not an expiry: a row older than it is still served, and its
//! reader starts a background revalidation. That is what keeps a launch fast even after a long gap
//! — an addon publishes no "content changed" signal, so the only alternative is to ask it before
//! painting, which is the slow launch this module removed. The durable envelope uses a much longer
//! window so budget maintenance cannot throw away a still-useful row; the row's own `at` travels
//! with it, so a stale row cannot masquerade as fresh after a restart.
//!
//! Configuration changes are detected exactly by the caller: the key is namespaced by the addon
//! fingerprint AND the catalog declarations from the installed manifests. Adding, removing,
//! disabling or re-pointing an addon — or a manifest that adds a catalog or changes its genres —
//! produces a different namespace, so old rows are simply missed. A provider fault is never
//! written; the caller's existing 30 s backoff governs retries.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use plx_stremio::model::MetaPreview;

/// A test-owned store: the ordinary tests must not touch the install's real redb, but a restart
/// test has to exercise the real durable path. A thread-local keeps that one case isolated.
#[cfg(test)]
mod test_store {
    use std::cell::RefCell;
    use std::sync::Arc;
    thread_local! {
        pub static STORE: RefCell<Option<Arc<plx_nuvio_store::Store>>> = const { RefCell::new(None) };
    }
}

/// When a row counts as fresh. Past this age a reader still serves it but revalidates it.
pub const CATALOG_TTL: Duration = Duration::from_secs(15 * 60);

/// The redb envelope's own window. Longer than `CATALOG_TTL` on purpose: an old row is still the
/// best answer we have until the revalidation lands, so maintenance must not purge it underneath
/// the reader. The row carries its real fetch time in `CachedCatalog::at`.
const CATALOG_DURABLE_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedCatalog {
    pub rows: Vec<MetaPreview>,
    pub exhausted: bool,
    /// Unix seconds when the rows were fetched. Travels with the durable row so a restart keeps
    /// the age honest: a stale row must be recognised and revalidated, not treated as fresh.
    pub at: i64,
}

#[derive(Default)]
struct Inner {
    rows: HashMap<String, CachedCatalog>,
}

pub struct CatalogCache {
    inner: Mutex<Inner>,
}

impl Default for CatalogCache {
    fn default() -> Self {
        CatalogCache { inner: Mutex::new(Inner::default()) }
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The store wraps cached values in an 8-byte big-endian expiry. Read the payload with or without
/// that prefix so a row written by either format can still be served.
fn decode_answer(bytes: &[u8]) -> Option<CachedCatalog> {
    serde_json::from_slice::<CachedCatalog>(bytes)
        .ok()
        .or_else(|| serde_json::from_slice::<CachedCatalog>(bytes.get(8..)?).ok())
}

impl CatalogCache {
    pub fn open() -> CatalogCache {
        CatalogCache::default()
    }

    /// The key includes the namespace so a configuration change cannot read stale rows, and the
    /// genre because a genre-filtered fill stores a different set.
    pub fn key(namespace: &str, catalog: &str, genre: Option<&str>) -> String {
        format!("{namespace}\u{0}{catalog}\u{0}{}", genre.unwrap_or(""))
    }

    /// Read the raw table, NOT `cache_get`: the envelope's expiry is a maintenance hint, not a
    /// reason to blank a shelf. Freshness is the caller's decision, made from `CachedCatalog::at`.
    fn durable_get(&self, key: &str) -> Option<Vec<u8>> {
        #[cfg(test)]
        {
            return test_store::STORE.with(|s| {
                s.borrow().as_ref().and_then(|store| {
                    store.get(plx_nuvio_store::Table::CatalogCache, key)
                })
            });
        }
        #[cfg(not(test))]
        {
            plx_nuvio_store::get()?.get(plx_nuvio_store::Table::CatalogCache, key)
        }
    }

    fn durable_put(&self, key: &str, bytes: &[u8]) {
        #[cfg(test)]
        {
            test_store::STORE.with(|s| {
                if let Some(store) = s.borrow().as_ref() {
                    let _ = store.cache_put(
                        plx_nuvio_store::NuvioCache::Catalogs, key, CATALOG_DURABLE_TTL, bytes,
                    );
                }
            });
            return;
        }
        #[cfg(not(test))]
        {
            if let Some(store) = plx_nuvio_store::get() {
                let _ = store.cache_put(
                    plx_nuvio_store::NuvioCache::Catalogs, key, CATALOG_DURABLE_TTL, bytes,
                );
            }
        }
    }

    /// The last known rows, from memory or from the durable table. A row past [`CATALOG_TTL`] is
    /// still returned — the caller uses `CachedCatalog::at` to decide whether to revalidate it.
    /// Missing rows (first launch, addon change, or a cleared table) still miss.
    pub fn get(&self, key: &str) -> Option<CachedCatalog> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(answer) = g.rows.get(key) {
            return Some(answer.clone());
        }
        let bytes = self.durable_get(key)?;
        let answer = decode_answer(&bytes)?;
        g.rows.insert(key.to_string(), answer.clone());
        Some(answer)
    }

    /// Store a successful fill. Faults are the caller's business and never reach this method.
    pub fn put(&self, key: &str, rows: Vec<MetaPreview>, exhausted: bool) {
        let answer = CachedCatalog { rows, exhausted, at: now_secs() };
        if let Ok(bytes) = serde_json::to_vec(&answer) {
            self.durable_put(key, &bytes);
        }
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
            .rows.insert(key.to_string(), answer);
    }

    /// Mark a row stale without dropping it. Used by the explicit Sync action: the rows stay
    /// painted, but the next reader sees an old `at` and revalidates them from the addon.
    pub fn mark_stale(&self, key: &str) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let old = now_secs() - CATALOG_TTL.as_secs() as i64 - 1;
        if let Some(answer) = g.rows.get_mut(key) {
            answer.at = old;
            if let Ok(bytes) = serde_json::to_vec(&*answer) {
                self.durable_put(key, &bytes);
            }
            return;
        }
        if let Some(mut answer) = self.durable_get(key).and_then(|bytes| decode_answer(&bytes)) {
            answer.at = old;
            if let Ok(bytes) = serde_json::to_vec(&answer) {
                self.durable_put(key, &bytes);
            }
            g.rows.insert(key.to_string(), answer);
        }
    }

    /// An addon-set change makes every row's namespace stale. Drop the durable rows too, so the
    /// 50 MiB budget is not spent on answers that can never be read again.
    pub fn clear(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).rows.clear();
        plx_nuvio_store::clear_table(plx_nuvio_store::Table::CatalogCache);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview(id: &str) -> MetaPreview {
        MetaPreview { id: id.into(), type_: "movie".into(), name: id.into(), ..Default::default() }
    }

    #[test]
    fn a_seeded_catalog_is_served_without_touching_the_addon() {
        let cache = CatalogCache::open();
        let key = CatalogCache::key("ns-1", "addon/movie/top", None);
        assert!(cache.get(&key).is_none());
        cache.put(&key, vec![preview("tt1"), preview("tt2")], true);
        let got = cache.get(&key).expect("the seeded answer survives the read");
        assert_eq!(got.rows.len(), 2);
        assert!(got.exhausted);
        assert_eq!(got.rows[0].id, "tt1");
    }

    #[test]
    fn the_namespace_and_genre_are_part_of_the_key() {
        let a = CatalogCache::key("ns-1", "cat", None);
        let b = CatalogCache::key("ns-2", "cat", None);
        let c = CatalogCache::key("ns-1", "cat", Some("horror"));
        assert_ne!(a, b, "an addon-set change must miss the old rows");
        assert_ne!(a, c, "a genre-filtered fill is a different answer");
    }

    /// The shim test at `shim_pms.rs`'s `a_relaunch_reads_catalog_pages_from_the_durable_window`
    /// only clears the old in-memory map; the new cache still answers from its own memory. This
    /// one closes and reopens the actual redb so a warm shelf after a real TV relaunch is covered.
    #[test]
    fn a_relaunch_reopens_the_store_and_reads_the_catalog_page() {
        let dir = std::env::temp_dir().join(format!("nuvio-catalog-restart-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("t.redb");
        let key = CatalogCache::key("restart-ns", "addon/movie/top", None);

        let store = plx_nuvio_store::Store::open_at(&path).unwrap();
        test_store::STORE.with(|s| *s.borrow_mut() = Some(store.clone()));
        let before = CatalogCache::open();
        before.put(&key, vec![preview("tt-persisted")], true);
        drop(before);
        drop(store);
        test_store::STORE.with(|s| *s.borrow_mut() = None);

        let reopened_store = plx_nuvio_store::Store::open_at(&path).unwrap();
        test_store::STORE.with(|s| *s.borrow_mut() = Some(reopened_store.clone()));
        let after = CatalogCache::open();
        let got = after.get(&key).expect("the shelf survived a full store reopen");
        assert_eq!(got.rows[0].id, "tt-persisted");
        assert!(got.exhausted);

        // A row past its freshness window is STILL served — the caller revalidates it in the
        // background instead of blanking the shelf while an addon answers.
        let stale_key = CatalogCache::key("restart-ns", "addon/movie/old", None);
        let stale = CachedCatalog {
            rows: vec![preview("tt-old")],
            exhausted: true,
            at: now_secs() - CATALOG_TTL.as_secs() as i64 - 60,
        };
        reopened_store.cache_put(
            plx_nuvio_store::NuvioCache::Catalogs,
            &stale_key,
            CATALOG_DURABLE_TTL,
            serde_json::to_vec(&stale).unwrap().as_slice(),
        ).unwrap();
        let got = after.get(&stale_key).expect("a stale row is served, not dropped");
        assert_eq!(got.rows[0].id, "tt-old");
        assert!(now_secs().saturating_sub(got.at) >= CATALOG_TTL.as_secs() as i64);

        // An explicit Sync marks the row stale without dropping it: it still answers, but with
        // an old timestamp so the shim's reader revalidates it from the addon.
        after.mark_stale(&key);
        let marked = after.get(&key).expect("stale marking must never blank a shelf");
        assert_eq!(marked.rows[0].id, "tt-persisted");
        assert!(now_secs().saturating_sub(marked.at) >= CATALOG_TTL.as_secs() as i64);

        test_store::STORE.with(|s| *s.borrow_mut() = None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
