//! NuvioNative's one durable store: a single `redb` file in the shim's data directory.
//!
//! Every record the fork owns lives here, and the upstream persistence layers reach the same
//! file through the in-process storage backend in `plx_os::storage::redb_backend`. There is no
//! second database and no DB8 record on the Nuvio path.
//!
//! This crate deliberately knows no application schema: a value is an opaque byte string. The
//! owner of a table decides its encoding, just as the owner of a DB8 object used to. All IO is
//! explicit — callers run it on a worker, never in a machine's `step`.

use plx_base::paths;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The one file name. Kept here so boot, the shim and the in-process backend cannot disagree.
pub const FILE_NAME: &str = "nuvio.redb";

pub const HELPER_OBJECTS: TableDefinition<&str, &[u8]> = TableDefinition::new("helper_objects");
pub const HELPER_KEYS: TableDefinition<&str, &[u8]> = TableDefinition::new("helper_keys");
pub const ADDONS: TableDefinition<&str, &[u8]> = TableDefinition::new("addons");
pub const LOCAL_SETTINGS: TableDefinition<&str, &[u8]> = TableDefinition::new("local_settings");
pub const WATCH: TableDefinition<&str, &[u8]> = TableDefinition::new("watch");
pub const KEYS: TableDefinition<&str, &[u8]> = TableDefinition::new("keys");
pub const ACCOUNT_SESSION: TableDefinition<&str, &[u8]> = TableDefinition::new("account_session");
pub const ACCOUNT_PREFS: TableDefinition<&str, &[u8]> = TableDefinition::new("account_prefs");
pub const INSTALL_PREFS: TableDefinition<&str, &[u8]> = TableDefinition::new("install_prefs");
// Fork Rofocale11: TMDB enrichment answers are durable app records, so they live in this one file
// beside the key table and watch state — never in a second JSONL cache beside it.
pub const META_CACHE: TableDefinition<&str, &[u8]> = TableDefinition::new("meta_cache");
pub const ENRICH_CACHE: TableDefinition<&str, &[u8]> = TableDefinition::new("enrich_cache");
pub const IMDB_IDS: TableDefinition<&str, &[u8]> = TableDefinition::new("imdb_ids");
// Fork Rofocale11: the IMDb -> TMDB id mapping never goes stale, so it is kept as a plain 8-byte
// big-endian u64 with NO TTL envelope (unlike `cache_put`). A `/find` answer is paid for once per
// title, not once per 15-minute cache window.
pub const IMDB_TMDB: TableDefinition<&str, &[u8]> = TableDefinition::new("imdb_tmdb");

/// The tables of the single file. A new feature that needs durable state adds one here rather
/// than a second file or a new DB8 kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Table {
    /// The in-process storage backend's DB8-shaped objects (session, consent, addons).
    HelperObjects,
    /// Keymanager key material used by that backend's credential envelopes.
    HelperKeys,
    Addons,
    LocalSettings,
    Watch,
    Keys,
    AccountSession,
    AccountPrefs,
    InstallPrefs,
    MetaCache,
    EnrichCache,
    ImdbIds,
    /// Fork Rofocale11: permanent IMDb -> TMDB id mapping (plain BE u64, no TTL envelope).
    ImdbTmdb,
}

/// Fork Rofocale11: the TTL-aware metadata caches. `cache_get` returns `None` on a miss or an
/// expired row, so callers never parse a stale envelope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NuvioCache {
    Meta,
    Enrich,
    Imdb,
}

impl Table {
    fn def(self) -> TableDefinition<'static, &'static str, &'static [u8]> {
        match self {
            Self::HelperObjects => HELPER_OBJECTS,
            Self::HelperKeys => HELPER_KEYS,
            Self::Addons => ADDONS,
            Self::LocalSettings => LOCAL_SETTINGS,
            Self::Watch => WATCH,
            Self::Keys => KEYS,
            Self::AccountSession => ACCOUNT_SESSION,
            Self::AccountPrefs => ACCOUNT_PREFS,
            Self::InstallPrefs => INSTALL_PREFS,
            Self::MetaCache => META_CACHE,
            Self::EnrichCache => ENRICH_CACHE,
            Self::ImdbIds => IMDB_IDS,
            Self::ImdbTmdb => IMDB_TMDB,
        }
    }
}

/// A handle on the one file. `redb`'s database is `Sync`; every caller opens its own read or
/// write transaction, so no application lock is needed around it.
pub struct Store {
    db: Database,
}

impl Store {
    pub fn open_at(path: &Path) -> Result<Arc<Self>, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create store dir: {e}"))?;
        }
        let db = Database::create(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        Ok(Arc::new(Self { db }))
    }

    /// The shipping path: the shim's data directory under the fork's data root.
    pub fn open() -> Result<Arc<Self>, String> {
        Self::open_at(&paths::nuvio_store_path())
    }

    /// Fork Rofocale11: read a cached value that has not passed its TTL. The envelope is an
    /// 8-byte big-endian expiry (Unix millis) followed by the owner's opaque bytes.
    pub fn cache_get(&self, kind: NuvioCache, key: &str) -> Option<Vec<u8>> {
        let table = match kind {
            NuvioCache::Meta => Table::MetaCache,
            NuvioCache::Enrich => Table::EnrichCache,
            NuvioCache::Imdb => Table::ImdbIds,
        };
        let bytes = self.get(table, key)?;
        let (expiry, value) = bytes.split_at_checked(8)?;
        let expiry = u64::from_be_bytes(expiry.try_into().ok()?);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
        (now < expiry).then(|| value.to_vec())
    }

    pub fn cache_put(&self, kind: NuvioCache, key: &str, ttl: Duration, value: &[u8]) -> Result<(), String> {
        let table = match kind {
            NuvioCache::Meta => Table::MetaCache,
            NuvioCache::Enrich => Table::EnrichCache,
            NuvioCache::Imdb => Table::ImdbIds,
        };
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_millis() as u64;
        let expiry = now.saturating_add(ttl.as_millis() as u64).to_be_bytes();
        let mut envelope = Vec::with_capacity(8 + value.len());
        envelope.extend_from_slice(&expiry);
        envelope.extend_from_slice(value);
        self.put(table, key, &envelope)
    }

    pub fn get(&self, table: Table, key: &str) -> Option<Vec<u8>> {
        let read = self.db.begin_read().ok()?;
        let t = read.open_table(table.def()).ok()?;
        let value = t.get(key).ok()??;
        Some(value.value().to_vec())
    }

    pub fn put(&self, table: Table, key: &str, value: &[u8]) -> Result<(), String> {
        let write = self.db.begin_write().map_err(|e| e.to_string())?;
        {
            let mut t = write.open_table(table.def()).map_err(|e| e.to_string())?;
            t.insert(key, value).map_err(|e| e.to_string())?;
        }
        write.commit().map_err(|e| e.to_string())
    }

    pub fn remove(&self, table: Table, key: &str) -> Result<(), String> {
        let write = self.db.begin_write().map_err(|e| e.to_string())?;
        {
            let mut t = write.open_table(table.def()).map_err(|e| e.to_string())?;
            t.remove(key).map_err(|e| e.to_string())?;
        }
        write.commit().map_err(|e| e.to_string())
    }

    /// Every row of a table, in key order. Used by the watches table's restart rebuild.
    pub fn list(&self, table: Table) -> Vec<(String, Vec<u8>)> {
        let Ok(read) = self.db.begin_read() else { return Vec::new() };
        let Ok(t) = read.open_table(table.def()) else { return Vec::new() };
        let Ok(iter) = t.iter() else { return Vec::new() };
        iter
            .flatten()
            .map(|(k, v)| (k.value().to_string(), v.value().to_vec()))
            .collect()
    }
}

/// Fork Rofocale11: drop every row of a table. The addon-set change invalidates the meta and
/// enrichment caches wholesale; this keeps that operation inside the one store.
pub fn clear_table(table: Table) {
    let Some(store) = get() else { return };
    let Ok(write) = store.db.begin_write() else { return };
    {
        if let Ok(mut t) = write.open_table(table.def()) {
            let keys: Vec<String> = match t.iter() {
                Ok(iter) => iter.flatten().map(|(k, _)| k.value().to_string()).collect(),
                Err(_) => Vec::new(),
            };
            for k in keys {
                let _ = t.remove(k.as_str());
            }
        }
    }
    let _ = write.commit();
}

/// The process-wide handle, installed once before any persistence call. Host tests that do not
/// boot the app use `open_at` directly and leave this empty.
static STORE: OnceLock<Arc<Store>> = OnceLock::new();

pub fn install(store: Arc<Store>) -> Result<(), Arc<Store>> {
    STORE.set(store)
}

pub fn get() -> Option<&'static Store> {
    STORE.get().map(|s| s.as_ref())
}

/// The owning handle, for components that need to keep the store alive (`redb_backend`).
pub fn get_arc() -> Option<Arc<Store>> {
    STORE.get().cloned()
}

/// Open the shipping file and publish it. Called once from Nuvio boot, before the shim starts, and
/// lazily from the in-process storage backend if a persistence call arrives first.
pub fn install_default() -> Result<Arc<Store>, String> {
    if let Some(store) = get_arc() {
        return Ok(store);
    }
    let store = Store::open()?;
    let _ = install(store.clone());
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Arc<Store> {
        static STORE: OnceLock<Arc<Store>> = OnceLock::new();
        STORE.get_or_init(|| {
            // Fork Rofocale11: the shared process_dir registers an atexit cleanup, so the cache
            // fixture no longer leaves a directory behind for cargo-test-parallel's TMPDIR check.
            let dir = plx_base::testscratch::process_dir("nuvio-cache-test");
            Store::open_at(&dir.join("cache.redb")).unwrap()
        }).clone()
    }

    #[test]
    fn cache_rows_round_trip_and_expire() {
        let store = store();
        store.cache_put(NuvioCache::Enrich, "movie:tt1", Duration::from_secs(60), b"payload").unwrap();
        assert_eq!(store.cache_get(NuvioCache::Enrich, "movie:tt1").unwrap(), b"payload");
        store.cache_put(NuvioCache::Enrich, "movie:tt1", Duration::ZERO, b"payload").unwrap();
        assert_eq!(store.cache_get(NuvioCache::Enrich, "movie:tt1"), None);
    }

    #[test]
    fn cache_kinds_and_keys_are_isolated() {
        let store = store();
        store.cache_put(NuvioCache::Imdb, "movie:tt2", Duration::from_secs(60), b"tt0133093").unwrap();
        assert_eq!(store.cache_get(NuvioCache::Imdb, "movie:tt2").unwrap(), b"tt0133093");
        store.cache_put(NuvioCache::Imdb, "movie:tt3", Duration::from_secs(60), b"tt0133093").unwrap();
        assert_eq!(store.cache_get(NuvioCache::Imdb, "show:tt3"), None);
        assert_eq!(store.cache_get(NuvioCache::Meta, "movie:tt3"), None);
        store.cache_put(NuvioCache::Meta, "movie:tt3", Duration::from_secs(60), b"meta").unwrap();
        assert_eq!(store.cache_get(NuvioCache::Meta, "movie:tt3").unwrap(), b"meta");
        assert_eq!(store.cache_get(NuvioCache::Imdb, "movie:tt3").unwrap(), b"tt0133093");
    }

    #[test]
    fn cache_overwrite_wins() {
        let store = store();
        store.cache_put(NuvioCache::Imdb, "movie:tt4", Duration::from_secs(60), b"first").unwrap();
        store.cache_put(NuvioCache::Imdb, "movie:tt4", Duration::from_secs(60), b"second").unwrap();
        assert_eq!(store.cache_get(NuvioCache::Imdb, "movie:tt4").unwrap(), b"second");
    }

    #[test]
    fn imdb_tmdb_rows_are_plain_be_u64_without_a_ttl_envelope() {
        let store = store();
        // Eight bytes exactly: a ninth byte would mean someone wrapped it in a TTL envelope.
        store.put(Table::ImdbTmdb, "movie:tt0133093", &603u64.to_be_bytes()).unwrap();
        let raw = store.get(Table::ImdbTmdb, "movie:tt0133093").unwrap();
        assert_eq!(raw.len(), 8);
        assert_eq!(u64::from_be_bytes(raw.try_into().unwrap()), 603);
        // A movie and a show with the same IMDb id are different keys.
        store.put(Table::ImdbTmdb, "show:tt0133093", &7u64.to_be_bytes()).unwrap();
        assert_eq!(store.get(Table::ImdbTmdb, "show:tt0133093").unwrap(), 7u64.to_be_bytes());
        assert_eq!(u64::from_be_bytes(store.get(Table::ImdbTmdb, "movie:tt0133093").unwrap().try_into().unwrap()), 603);
        assert_eq!(store.get(Table::ImdbTmdb, "movie:tt0944947"), None);
        // No expiry: the row outlives every TTL the envelope caches use.
        std::thread::sleep(Duration::from_millis(1));
        assert_eq!(store.get(Table::ImdbTmdb, "movie:tt0133093").unwrap(), 603u64.to_be_bytes());
    }
}
