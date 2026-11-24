//! NuvioNative's one durable store: a single `redb` file in the shim's data directory.
//!
//! Every record the fork owns lives here, and the upstream persistence layers reach the same
//! file through the in-process storage backend in `plx_os::storage::redb_backend`. There is no
//! second database and no DB8 record on the Nuvio path.
//!
//! This crate deliberately knows no application schema: a value is an opaque byte string. The
//! owner of a table decides its encoding, just as the owner of a DB8 object used to. All IO is
//! explicit — callers run it on a worker, never in a machine's `step`.

use plx_base::eventlog::log;
use plx_base::paths;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Fork Rofocale11: the single store's soft budget (50 MiB). It is enforced by removing expired
/// and oldest TTL cache rows and by compacting at boot — NEVER by truncating a live redb file,
/// which would destroy the durable records in it. If the non-cache rows alone exceed this, the
/// store is logged and left intact; no account data is sacrificed to hit a round number.
pub const BUDGET_BYTES: u64 = 50 * 1024 * 1024;

/// The TTL-enveloped tables. `ImdbTmdb` is deliberately absent: its rows never expire and are
/// expensive to rebuild, so they are durable data, not cache.
const CACHE_TABLES: [Table; 3] = [Table::MetaCache, Table::EnrichCache, Table::ImdbIds];

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
    path: PathBuf,
    budget: u64,
}

impl Store {
    pub fn open_at(path: &Path) -> Result<Arc<Self>, String> {
        Self::open_at_with_budget(path, BUDGET_BYTES)
    }

    fn open_at_with_budget(path: &Path, budget: u64) -> Result<Arc<Self>, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot create store dir: {e}"))?;
        }
        let db = Database::create(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        Ok(Arc::new(Self { db, path: path.to_path_buf(), budget }))
    }

    /// The shipping path: the shim's data directory under the fork's data root. Maintenance is
    /// best effort — a TV with a full or read-only data directory must still boot — so its
    /// failure is logged, not propagated.
    pub fn open() -> Result<Arc<Self>, String> {
        let path = paths::nuvio_store_path();
        let maintained = if path.is_file() { Some(maintain(&path)) } else { None };
        match maintained {
            Some(Ok(report)) => log(&format!(
                "nuvio: store maintenance {:.1} -> {:.1} MiB (expired {}, shed {}, compacted {})",
                report.before as f64 / (1024.0 * 1024.0),
                report.after as f64 / (1024.0 * 1024.0),
                report.expired, report.shed, report.compacted,
            )),
            Some(Err(error)) => log(&format!("nuvio: store maintenance skipped: {error}")),
            None => (),
        }
        Self::open_at(&path)
    }

    /// Keep the file's live cache footprint under budget while the app is running. redb reuses
    /// freed pages, so removing rows here stops growth; only the boot pass can return the pages.
    fn enforce_budget(&self) {
        let Ok(len) = std::fs::metadata(&self.path).map(|m| m.len()) else { return };
        if len <= self.budget { return; }
        let expired = purge_expired(&self.db);
        let after = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(len);
        let shed = if after > self.budget { shed_oldest(&self.db, self.budget, after) } else { 0 };
        if expired + shed > 0 {
            log(&format!("nuvio: store over budget ({len} bytes), purged {expired} expired and shed {shed} cache rows"));
        }
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
        self.enforce_budget();
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

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn expiry_of(value: &[u8]) -> Option<u64> {
    let (head, _) = value.split_at_checked(8)?;
    Some(u64::from_be_bytes(head.try_into().ok()?))
}

/// Remove TTL cache rows whose envelope has expired. `cache_get` only ignores them; this pass
/// returns their pages to redb. Durable tables are never touched.
fn purge_expired(db: &Database) -> usize {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64).unwrap_or(u64::MAX);
    let mut removed = 0;
    for table in CACHE_TABLES {
        let Ok(write) = db.begin_write() else { continue };
        let expired: Vec<String> = {
            let Ok(t) = write.open_table(table.def()) else { continue };
            let Ok(iter) = t.iter() else { continue };
            iter.flatten().filter_map(|(k, v)| {
                (expiry_of(v.value()).is_some_and(|e| now >= e)).then(|| k.value().to_string())
            }).collect()
        };
        if expired.is_empty() { continue; }
        if let Ok(mut t) = write.open_table(table.def()) {
            for key in &expired { let _ = t.remove(key.as_str()); }
        }
        let _ = write.commit();
        removed += expired.len();
    }
    removed
}

/// Oldest-first cache rows, ordered by expiry. Pure data collection, so the caller can decide
/// how much to shed without holding a transaction open while it reads metadata.
fn cache_rows(db: &Database) -> Vec<(u64, Table, String, usize)> {
    let Ok(read) = db.begin_read() else { return Vec::new() };
    let mut rows = Vec::new();
    for table in CACHE_TABLES {
        let Ok(t) = read.open_table(table.def()) else { continue };
        let Ok(iter) = t.iter() else { continue };
        for entry in iter.flatten() {
            let (key, value) = entry;
            let bytes = value.value();
            rows.push((expiry_of(bytes).unwrap_or(0), table, key.value().to_string(), bytes.len()));
        }
    }
    rows
}

/// Drop the oldest cache rows until the live cache footprint fits the budget. Expired rows were
/// already removed by [`purge_expired`]; these are live but least recently useful entries.
fn shed_oldest(db: &Database, budget: u64, len: u64) -> usize {
    if len <= budget { return 0; }
    let need = (len - budget) as usize;
    let mut rows = cache_rows(db);
    rows.sort_by_key(|row| row.0);
    let mut keys: Vec<(Table, String)> = Vec::new();
    let mut freed = 0usize;
    for (_, table, key, bytes) in rows {
        if freed >= need { break; }
        freed += bytes;
        keys.push((table, key));
    }
    let mut removed = 0;
    for table in CACHE_TABLES {
        let selected: Vec<&String> = keys.iter().filter(|(t, _)| *t == table).map(|(_, k)| k).collect();
        if selected.is_empty() { continue; }
        let Ok(write) = db.begin_write() else { continue };
        if let Ok(mut t) = write.open_table(table.def()) {
            for key in selected {
                if t.remove(key.as_str()).is_ok() { removed += 1; }
            }
        }
        let _ = write.commit();
    }
    removed
}

/// The result of a boot-time maintenance pass, for the log line and the tests.
pub struct Maintenance {
    pub before: u64,
    pub after: u64,
    pub expired: usize,
    pub shed: usize,
    pub compacted: bool,
}

/// Purge, shed and compact a store that is NOT yet open by this process. Called from [`Store::open`]
/// before the file is installed, when no transactions or second handle can block compaction.
pub fn maintain(path: &Path) -> Result<Maintenance, String> {
    maintain_with_budget(path, BUDGET_BYTES)
}

fn maintain_with_budget(path: &Path, budget: u64) -> Result<Maintenance, String> {
    let before = file_len(path);
    let mut db = Database::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let expired = purge_expired(&db);
    let shed = if file_len(path) > budget { shed_oldest(&db, budget, file_len(path)) } else { 0 };
    let compacted = db.compact().unwrap_or(false);
    drop(db);
    let after = file_len(path);
    if after > budget {
        log(&format!(
            "nuvio: store is {after} bytes after maintenance, above the {budget}-byte budget; \
             durable records were kept intact"
        ));
    }
    Ok(Maintenance { before, after, expired, shed, compacted })
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
    fn expired_cache_rows_are_purged_but_durable_rows_survive() {
        let dir = plx_base::testscratch::process_dir("nuvio-store-purge");
        let path = dir.join("purge.redb");
        let budget = 64 * 1024 * 1024;
        let store = Store::open_at_with_budget(&path, budget).unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
        store.put(Table::MetaCache, "old", &(now - 10).to_be_bytes()).unwrap();
        store.put(Table::MetaCache, "fresh", &(now + 600_000).to_be_bytes()).unwrap();
        store.put(Table::ImdbTmdb, "movie:tt1", &603u64.to_be_bytes()).unwrap();
        store.put(Table::AccountSession, "session", b"token").unwrap();
        drop(store);

        let report = maintain_with_budget(&path, budget).unwrap();
        assert_eq!(report.expired, 1, "only the expired row should be purged");

        let store = Store::open_at_with_budget(&path, budget).unwrap();
        assert!(store.get(Table::MetaCache, "old").is_none());
        assert!(store.get(Table::MetaCache, "fresh").is_some());
        assert_eq!(store.get(Table::ImdbTmdb, "movie:tt1").unwrap(), 603u64.to_be_bytes());
        assert_eq!(store.get(Table::AccountSession, "session").unwrap(), b"token");
    }

    #[test]
    fn an_over_budget_store_is_shed_and_compacted_without_touching_durable_rows() {
        const BUDGET: u64 = 2 * 1024 * 1024;
        let dir = plx_base::testscratch::process_dir("nuvio-store-budget");
        let path = dir.join("budget.redb");
        let store = Store::open_at_with_budget(&path, BUDGET).unwrap();
        let future = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 + 3_600_000;
        let payload = vec![7u8; 512 * 1024];
        for i in 0..8 {
            let mut envelope = future.to_be_bytes().to_vec();
            envelope.extend_from_slice(&payload);
            store.put(Table::EnrichCache, &format!("movie:tt{i}"), &envelope).unwrap();
        }
        store.put(Table::ImdbTmdb, "movie:tt1", &603u64.to_be_bytes()).unwrap();
        store.put(Table::Watch, "w1", b"watch").unwrap();
        let before = file_len(&path);
        assert!(before > BUDGET, "fixture should exceed the budget: {before}");
        drop(store);

        let report = maintain_with_budget(&path, BUDGET).unwrap();
        assert!(report.shed > 0, "the oldest cache rows must be shed");
        assert!(report.after < before, "compaction must return pages: {before} -> {}", report.after);
        assert!(report.after <= BUDGET + 1024 * 1024, "still too large: {}", report.after);

        let store = Store::open_at_with_budget(&path, BUDGET).unwrap();
        assert_eq!(store.get(Table::ImdbTmdb, "movie:tt1").unwrap(), 603u64.to_be_bytes());
        assert_eq!(store.get(Table::Watch, "w1").unwrap(), b"watch");
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
