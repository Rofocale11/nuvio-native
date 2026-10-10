//! What the shim needs from a content source. [`AddonBackend`] is the real one (the user's Stremio
//! addons through `plx_stremio`); [`crate::fake::FakeBackend`] serves a fixed library for host tests
//! and for running the simulator with no network.

use plx_stremio::{Meta, MetaPreview, Stream, Subtitle};
use std::sync::RwLock;
use std::time::Duration;

/// One catalog the shim shows as a Home shelf and/or a library listing.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogInfo {
    /// Stable key (`addon-id/type/catalog-id`); the shim maps it to a small hub index.
    pub key: String,
    pub title: String,
    /// Stremio type: `"movie"` / `"series"` / anything else (skipped by the library sections).
    pub type_: String,
    /// Values the catalog's `genre` extra accepts (empty when it has none).
    pub genres: Vec<String>,
}

pub trait Backend: Send + Sync {
    /// Home-shelf catalogs, in the user's addon order.
    fn catalogs(&self) -> Vec<CatalogInfo>;
    /// One page of a catalog. `Err` is a failure (shown as an empty page, not cached).
    fn catalog_page(&self, key: &str, skip: u32, genre: Option<&str>) -> Result<Vec<MetaPreview>, ()>;
    /// Search across every search-capable catalog.
    fn search(&self, query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)>;
    fn meta(&self, type_: &str, id: &str) -> Option<Meta>;
    /// Streams for a movie (`id`) or an episode (the video id), progressively through `on_batch`.
    fn streams(&self, type_: &str, id: &str, deadline: Duration, on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream>;
    fn subtitles(&self, type_: &str, id: &str) -> Vec<Subtitle>;
}

/// The user's addons. Swappable at run time ([`AddonBackend::replace`]) so Settings can install,
/// remove or re-import addons without restarting the shim.
pub struct AddonBackend {
    set: RwLock<plx_stremio::AddonSet>,
}

impl AddonBackend {
    pub fn new(set: plx_stremio::AddonSet) -> AddonBackend {
        AddonBackend { set: RwLock::new(set) }
    }
    pub fn replace(&self, set: plx_stremio::AddonSet) {
        *self.set.write().unwrap_or_else(|e| e.into_inner()) = set;
    }
    pub fn snapshot(&self) -> plx_stremio::AddonSet {
        self.set.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn info(set: &plx_stremio::AddonSet, c: &plx_stremio::addons::CatalogRef) -> CatalogInfo {
    let addon = &set.addons[c.addon].manifest;
    let genres = c.decl.extra.iter().find(|e| e.name == "genre").map(|e| e.options.clone()).unwrap_or_default();
    let base = c.decl.name.clone().filter(|n| !n.is_empty()).unwrap_or_else(|| c.decl.id.clone());
    let kind = match c.decl.type_.as_str() {
        "movie" => "Movies",
        "series" => "Series",
        other => other,
    };
    // "Popular - Movies" is how Stremio itself titles a shelf; Cinemeta's names are bare ("Popular").
    let title = if base.to_lowercase().contains(&kind.to_lowercase()) { base } else { format!("{base} {kind}") };
    let title = if set.addons.len() > 1 && addon.id != "com.linvo.cinemeta" { format!("{title} · {}", addon.name) } else { title };
    CatalogInfo { key: c.key(set), title, type_: c.decl.type_.clone(), genres }
}

impl Backend for AddonBackend {
    fn catalogs(&self) -> Vec<CatalogInfo> {
        let set = self.set.read().unwrap_or_else(|e| e.into_inner());
        set.home_catalogs().iter().map(|c| info(&set, c)).collect()
    }
    fn catalog_page(&self, key: &str, skip: u32, genre: Option<&str>) -> Result<Vec<MetaPreview>, ()> {
        let set = self.snapshot();
        let mut all = set.home_catalogs();
        all.extend(set.search_catalogs());
        let cat = all.into_iter().find(|c| c.key(&set) == key).ok_or(())?;
        set.catalog(&cat, skip, genre).map_err(|_| ())
    }
    fn search(&self, query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
        let set = self.snapshot();
        set.search(query).into_iter().map(|(c, v)| (info(&set, &c), v)).collect()
    }
    fn meta(&self, type_: &str, id: &str) -> Option<Meta> {
        self.snapshot().meta(type_, id).ok()
    }
    fn streams(&self, type_: &str, id: &str, deadline: Duration, on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream> {
        self.snapshot().streams(type_, id, deadline, on_batch)
    }
    fn subtitles(&self, type_: &str, id: &str) -> Vec<Subtitle> {
        self.snapshot().subtitles(type_, id)
    }
}
