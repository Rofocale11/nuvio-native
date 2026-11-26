//! Fork Rofocale11: the Nuvio half of Settings ▸ Privacy & data's "Delete all local data".
//!
//! Upstream's `delete_all_local_data` sweeps session, telemetry and runtime files and clears its
//! metadata store. Nuvio's one redb file sits outside every one of those candidate sets, so before
//! this module an erase signed out — removing the account session row — but left the household's
//! watch state, addon configuration, ratingKey table, caches and IMDb→TMDB map on the television.
//!
//! The account is already revoked by the caller before this runs. This module only removes data;
//! it never decides routing, and it never touches the synthetic boot's private store on disk.

use super::{clear_table_in, get, EPHEMERAL, Store, Table};
use plx_base::eventlog::log;
use std::path::Path;

/// Fork Rofocale11: every table in the one file. A new durable record must be added here as well
/// as to `Table`, or a privacy erase would silently leave it behind.
pub const ALL_TABLES: [Table; 16] = [
    Table::HelperObjects,
    Table::HelperKeys,
    Table::Addons,
    Table::LocalSettings,
    Table::Watch,
    Table::Keys,
    Table::AccountSession,
    Table::AccountPrefs,
    Table::InstallPrefs,
    Table::ProfilePins,
    Table::MetaCache,
    Table::EnrichCache,
    Table::ImdbIds,
    Table::ImdbTmdb,
    Table::SubtitleCache,
    Table::CatalogCache,
];

/// Fork Rofocale11: clear every row in this store, reporting a table whose transaction failed.
pub fn clear_all_in(store: &Store) -> Vec<String> {
    ALL_TABLES.iter().filter_map(|table| {
        clear_table_in(store, *table).err().map(|e| format!("{table:?}: {e}"))
    }).collect()
}

/// Fork Rofocale11: remove a file or a directory without following a symlink. A dangling symlink
/// is an owned artefact too; `symlink_metadata` sees it where `metadata` would not.
fn remove_owned(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path).map_err(|e| e.to_string()),
        Ok(_) => std::fs::remove_file(path).map_err(|e| e.to_string()),
    }
}

/// Fork Rofocale11: erase every Nuvio record for THIS install, returning paths/tables that could
/// not be removed. Called from the loop's confirmed `DeleteAllLocalData` branch only; an ordinary
/// sign-out keeps its language and does not reach here.
pub fn erase() -> Vec<String> {
    let mut failures = Vec::new();
    if EPHEMERAL.get().is_some() {
        // A synthetic boot's store is private and its file is deleted at every boot. Clear the
        // fake records, but never unlink a real install's data from a headless run.
        if let Some(store) = EPHEMERAL.get() {
            failures.extend(clear_all_in(store));
        }
        log("nuvio: synthetic local data erased");
        return failures;
    }
    // Clear first: on a jail that mounts the data rung read-only the file cannot be unlinked,
    // but an empty table set leaves no record for the next boot to read back.
    if let Some(store) = get() {
        failures.extend(clear_all_in(store));
    }
    for path in plx_base::paths::nuvio_erase::all_data_paths() {
        if let Err(error) = remove_owned(&path) {
            failures.push(format!("{}: {error}", path.display()));
        }
    }
    if failures.is_empty() {
        log("nuvio: all local data erased");
    } else {
        log(&format!("nuvio: local data erased; {} target(s) could not be removed: {}", failures.len(), failures.join("; ")));
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nuvio-erase-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn clearing_every_table_leaves_no_rows() {
        let dir = scratch("tables");
        let store = Store::open_at(&dir.join("shim/nuvio.redb")).unwrap();
        for table in ALL_TABLES {
            store.put(table, "row", b"value").unwrap();
        }
        assert!(ALL_TABLES.iter().all(|t| store.list(*t).len() == 1));
        assert!(clear_all_in(&store).is_empty());
        assert!(ALL_TABLES.iter().all(|t| store.list(*t).is_empty()), "a durable table survived the erase");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_a_directory_does_not_follow_a_symlink_out_of_the_data_root() {
        let dir = scratch("symlink");
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let link = dir.join("data-link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        remove_owned(&link).unwrap();
        assert!(std::fs::read(outside.join("keep.txt")).is_ok(), "the target must survive");
        assert!(std::fs::symlink_metadata(&link).is_err(), "the owned link must be gone");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absent_target_is_not_a_failure() {
        let dir = scratch("absent");
        assert!(remove_owned(&dir.join("never-existed")).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
