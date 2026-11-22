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
