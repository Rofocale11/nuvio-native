//! The addon-catalog side of the Library's browse state. Fork Rofocale11: the shim exposes its
//! addon catalogs as `Meta.Type` rows carrying `catalogKey`; the Movies/TV Shows header offers
//! them as a Catalogs dropdown instead of upstream's hardcoded LibraryType menu. The state the
//! store needs still lives in [`super::SecState`] (one table, one store), but the catalog's parse,
//! validation, landing and wire filter live here so an upstream merge has one `mod` seam to
//! reconcile instead of a scattering of blocks through the upstream state machine.

use super::*;

/// One catalog the shim advertises as a browsable "Type" entry for a section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogEntry {
    pub key: String,
    pub title: String,
    /// The server marked this catalog as the one the current listing came from.
    pub active: bool,
}

/// A shim catalog row is any `Meta.Type` carrying a non-empty `catalogKey`; ordinary PMS rows
/// omit it and keep the upstream LibraryType menu. The server's `active` flag names the catalog
/// the listing came from; the caller falls back to the first on landing.
pub(super) fn from_meta(meta: &plx_plex::plex::Meta) -> Option<Vec<CatalogEntry>> {
    let catalogs: Vec<CatalogEntry> = meta.types.iter()
        .filter(|kind| !kind.catalog_key.is_empty())
        .map(|kind| {
            let key = kind.catalog_key.clone();
            CatalogEntry {
                title: if kind.title.is_empty() { key.clone() } else { kind.title.clone() },
                key,
                active: kind.active != 0,
            }
        })
        .collect();
    (!catalogs.is_empty()).then_some(catalogs)
}

impl SecState {
    /// A named catalog is a wire filter of both the listing and its directories, so it must ride
    /// every request this state builds.
    pub(super) fn catalog_filter(&self, filters: &mut Vec<(String, String)>) {
        if let Some(catalog) = &self.catalog {
            filters.push(("catalog".into(), catalog.clone()));
        }
    }

    /// A shim section's catalogs replace the upstream LibraryType menu. Keep a choice the server
    /// still advertises; otherwise seat the active one (or the first). An empty answer is a
    /// section that advertised none, not a reason to drop a choice on this same section.
    pub(super) fn land_catalogs(&mut self, catalogs: Option<Vec<CatalogEntry>>) {
        let Some(catalogs) = catalogs.filter(|catalogs| !catalogs.is_empty()) else { return };
        self.catalogs = Arc::new(catalogs);
        self.library_type = LibraryType::Primary;
        if self.catalog.as_ref().is_none_or(|key| !self.catalogs.iter().any(|c| &c.key == key)) {
            self.catalog = self.catalogs.iter()
                .find(|c| c.active)
                .or_else(|| self.catalogs.first())
                .map(|c| c.key.clone());
        }
    }
}

impl BrowseState {
    /// The Catalogs dropdown picks a catalog the server advertised (or `None` to return to its
    /// default). The choice re-queries the listing but keeps its primary metadata type, so
    /// Unwatched, genre and sort still apply: the catalog replaces the upstream LibraryType menu,
    /// not the grid. An un-advertised key is refused without disturbing state.
    pub(super) fn set_catalog(&mut self, key: Option<String>) -> bool {
        let current = self.cur();
        let Some(state) = self.states.get_mut(current) else { return false };
        if state.catalog == key {
            return true;
        }
        if let Some(wanted) = &key {
            if !state.catalogs.iter().any(|catalog| &catalog.key == wanted) {
                return false;
            }
        }
        if state.library_type == LibraryType::Collections {
            state.loaned = None;
        }
        state.catalog = key;
        state.library_type = LibraryType::Primary;
        state.forget_letters();
        state.sorts = Arc::default();
        state.sort_idx = 0;
        state.sort_desc = false;
        state.genre = None;
        state.genres = Arc::default();
        state.genres_done = false;
        state.sort_resolved = true;
        state.genre_resolved = true;
        self.requery();
        true
    }
}

/// Seed the server-advertised catalog list the header's dropdown reads, with `shown` seating the
/// catalog in force. The store method exposes this as `seed_catalogs_for_test`.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_for_owner_test(
    state: &mut BrowseState,
    catalogs: Vec<CatalogEntry>,
    shown: Option<&str>,
) {
    plx_base::testlock::assert_held("browse's section table (seed_catalogs_for_test)");
    let cur = state.cur();
    let Some(state) = state.states.get_mut(cur) else { return };
    state.catalogs = Arc::new(catalogs);
    state.catalog = shown.map(str::to_owned);
    state.library_type = LibraryType::Primary;
}
