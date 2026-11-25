//! durable catalog pages for the shim's Home/Library shelves.
//!
//! The catalog rows used to live only in [`crate::shim_pms`]'s in-memory `catalogs` map, so every
//! TV relaunch re-fetched the first page of every addon catalog (the ~15 s cold launch). This
//! module keeps the same 15-minute window in the one redb store. A relaunch with an unchanged
//! addon set reads the rows instead of asking the addons; the window bounds how stale a catalog's
//! *content* may get, because a Stremio addon publishes no change signal we could watch.
//!
//! Configuration changes are detected exactly by the caller: the key is namespaced by the addon
//! fingerprint AND the catalog declarations from the installed manifests. Adding, removing,
//! disabling or re-pointing an addon — or a manifest that adds a catalog or changes its genres —
//! produces a different namespace, so old rows are simply missed. A provider fault is never
//! written; the caller's existing 30 s backoff governs retries.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use plx_stremio::model::MetaPreview;

/// The same freshness bound the in-memory catalog cache already used.
pub const CATALOG_TTL: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct CachedCatalog {
    pub rows: Vec<MetaPreview>,
    pub exhausted: bool,
    /// Unix seconds when the rows were fetched. Keeps the in-memory age honest after a restart:
    /// a row written 14 minutes ago must not get a fresh 15 minutes just because it was read.
    pub at: i64,
}

#[derive(Default)]
struct Inner {
    rows: HashMap<String, (Instant, CachedCatalog)>,
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

impl CatalogCache {
    pub fn open() -> CatalogCache {
        CatalogCache::default()
    }

    /// The key includes the namespace so a configuration change cannot read stale rows, and the
    /// genre because a genre-filtered fill stores a different set.
    pub fn key(namespace: &str, catalog: &str, genre: Option<&str>) -> String {
        format!("{namespace}\u{0}{catalog}\u{0}{}", genre.unwrap_or(""))
    }

    fn durable_get(&self, key: &str) -> Option<Vec<u8>> {
        #[cfg(test)]
        {
            let _ = key;
            None
        }
        #[cfg(not(test))]
        {
            plx_nuvio_store::get()?.cache_get(plx_nuvio_store::NuvioCache::Catalogs, key)
        }
    }

    fn durable_put(&self, key: &str, bytes: &[u8]) {
        #[cfg(test)]
        {
            let _ = (key, bytes);
        }
        #[cfg(not(test))]
        {
            if let Some(store) = plx_nuvio_store::get() {
                let _ = store.cache_put(plx_nuvio_store::NuvioCache::Catalogs, key, CATALOG_TTL, bytes);
            }
        }
    }

    /// A live answer, from memory or from the durable table. An answer past its window is dropped,
    /// never served — the next read re-fetches it from the addon.
    pub fn get(&self, key: &str) -> Option<CachedCatalog> {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, answer)) = g.rows.get(key) {
            if at.elapsed() < CATALOG_TTL {
                return Some(answer.clone());
            }
            g.rows.remove(key);
        }
        let bytes = self.durable_get(key)?;
        let answer: CachedCatalog = serde_json::from_slice(&bytes).ok()?;
        let age = Duration::from_secs(now_secs().saturating_sub(answer.at) as u64);
        if age >= CATALOG_TTL {
            return None;
        }
        let started = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
        g.rows.insert(key.to_string(), (started, answer.clone()));
        Some(answer)
    }

    /// Store a successful fill. Faults are the caller's business and never reach this method.
    pub fn put(&self, key: &str, rows: Vec<MetaPreview>, exhausted: bool) {
        let answer = CachedCatalog { rows, exhausted, at: now_secs() };
        if let Ok(bytes) = serde_json::to_vec(&answer) {
            self.durable_put(key, &bytes);
        }
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
            .rows.insert(key.to_string(), (Instant::now(), answer));
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
}
