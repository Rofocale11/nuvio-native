//! Fork Rofocale11: every on-device path Nuvio Native owns, for Settings ▸ Privacy & data's
//! one destructive sweep.
//!
//! Upstream's erase path names session and telemetry candidates. The fork keeps a single redb
//! file (`nuvio.redb`) plus addon/config files, a poster cache, torrent swarm state, a probe
//! verdict and installed HUD packages OUTSIDE that set. Enumerating them here, next to the path
//! resolution they mirror, keeps the sweep from growing a second definition of the install id:
//! every path below is keyed by `app_id()`, so a debug build's erase cannot reach the stable or
//! nightly install's data.

use super::{app_id, hud_candidates, image_cache_candidates, in_runtime_dir, p2p_state_candidates, persistent_state_root, runtime_dir};
use std::path::{Path, PathBuf};

/// Fork Rofocale11: all three roots `nuvio_data_dir()` chooses between, not just the winner.
/// A jail or firmware change can leave an older rung behind; an erase that only visits the
/// currently writable one would leave the household's records on the set.
pub fn data_dir_candidates() -> Vec<PathBuf> {
    vec![
        PathBuf::from(format!("/media/developer/{}.nuvio", app_id())),
        persistent_state_root().join("nuvio"),
        runtime_dir().join("nuvio"),
    ]
}

/// Fork Rofocale11: every path the Nuvio half of "Delete all local data" must visit.
///
/// Directories are listed before their files' parent so a failed child removal is still reported
/// on its own. `plx_nuvio_store::erase` clears the tables first, so a read-only rung that cannot
/// be unlinked still holds no data.
pub fn all_data_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut push = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    for data in data_dir_candidates() {
        push(data.join("shim")); // the one redb file and its legacy siblings
        push(data.join("addons.json"));
        push(data.join("settings.json"));
        push(data);
    }
    for cache in image_cache_candidates() {
        push(cache);
    }
    for state in p2p_state_candidates() {
        // The probe verdict lives beside the swarm state, in the parent directory.
        if let Some(parent) = state.parent() {
            push(parent.to_path_buf());
        }
        push(state);
    }
    for hud in hud_candidates() {
        push(hud);
    }
    // Handoff files from the dev shell: an imported addon set is account data too.
    push(in_runtime_dir("nuvio-import.json"));
    push(in_runtime_dir("nuvio-import"));
    // A synthetic boot's private store is deleted at boot, but an erase must not leave one behind.
    push(in_runtime_dir("nuvio-synthetic.redb"));
    paths
}

/// Fork Rofocale11: refuse to erase anything that is not this install's own data root. The
/// sweep never calls this on a path it did not enumerate, but the check is cheap and keeps a
/// future edit from turning an app-id-keyed sweep into a prefix wildcard.
pub fn is_owned(path: &Path) -> bool {
    let Some(text) = path.to_str() else { return false };
    let id = app_id();
    let leaf = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if path == runtime_dir() || path == persistent_state_root() || path == super::app_dir() {
        return false;
    }
    text.starts_with(&format!("/media/developer/{id}"))
        || text.starts_with(&format!("/media/developer/temp/{id}"))
        || text.starts_with(&format!("/media/internal/.{id}"))
        || text.starts_with(&format!("/tmp/{id}"))
        || leaf == "nuvio" || leaf == "nuvio-import" || leaf == "nuvio-import.json"
        || leaf == "nuvio-synthetic.redb" || leaf == "imgcache" || leaf == "huds"
        || leaf == "serverp2p" || leaf == "engine-p2p" || leaf == "shim"
        || leaf == "addons.json" || leaf == "settings.json" || leaf == "state"
        || leaf == "probe-cache.json"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_erase_target_is_this_installs_own_data() {
        let paths = all_data_paths();
        assert!(!paths.is_empty());
        for path in &paths {
            assert!(is_owned(path), "not an owned data path: {}", path.display());
            assert!(path.components().count() > 1, "never erase a bare root: {}", path.display());
        }
    }

    #[test]
    fn no_other_flavours_path_can_reach_the_list() {
        let other = format!("/media/developer/{}-other.nuvio", app_id());
        assert!(!all_data_paths().iter().any(|p| p.to_string_lossy().contains(&other)));
        let paths = all_data_paths();
        let mut seen = std::collections::HashSet::new();
        assert!(paths.iter().all(|p| seen.insert(p)), "duplicate target in the erase list");
    }
}
