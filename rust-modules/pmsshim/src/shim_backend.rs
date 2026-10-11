//! What the shim needs from a content source. [`AddonBackend`] is the real one (the user's Stremio
//! addons through `plx_stremio`); [`crate::shim_fake::FakeBackend`] serves a fixed library for host tests
//! and for running the simulator with no network.

use plx_stremio::model::{Meta, MetaPreview, Stream, Subtitle};
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
    /// Poster provider; fake/other backends retain their supplied artwork.
    fn poster(&self, _id: &str, original: Option<&str>) -> Option<String> { original.map(str::to_string) }
    /// Streams for a movie (`id`) or an episode (the video id), progressively through `on_batch`.
    fn streams(&self, type_: &str, id: &str, deadline: Duration, on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream>;
    fn subtitles(&self, type_: &str, id: &str) -> Vec<Subtitle>;
}

/// The user's addons. Swappable at run time ([`AddonBackend::replace`]) so Settings can install,
/// remove or re-import addons without restarting the shim.
pub struct AddonBackend {
    set: RwLock<plx_stremio::addons::AddonSet>,
}

impl AddonBackend {
    pub fn new(set: plx_stremio::addons::AddonSet) -> AddonBackend {
        AddonBackend { set: RwLock::new(set) }
    }
    pub fn replace(&self, set: plx_stremio::addons::AddonSet) {
        *self.set.write().unwrap_or_else(|e| e.into_inner()) = set;
    }
    pub fn snapshot(&self) -> plx_stremio::addons::AddonSet {
        self.set.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

fn info(set: &plx_stremio::addons::AddonSet, c: &plx_stremio::addons::CatalogRef) -> CatalogInfo {
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
    fn poster(&self, id: &str, original: Option<&str>) -> Option<String> {
        if id.strip_prefix("tt").is_some_and(|digits| digits.len() >= 7 && digits.bytes().all(|b| b.is_ascii_digit())) {
            Some(format!("https://btttr.cc/poster/imdb/poster-default/{id}.jpg?rs=RT"))
        } else { original.map(str::to_string) }
    }
    fn streams(&self, type_: &str, id: &str, deadline: Duration, on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream> {
        self.snapshot().streams(type_, id, deadline, on_batch)
    }
    fn subtitles(&self, type_: &str, id: &str) -> Vec<Subtitle> {
        self.snapshot().subtitles(type_, id)
    }
}

#[cfg(test)]
mod poster_tests {
    use super::*;
    #[test]
    fn better_posters_apply_to_titles_from_other_catalogs() {
        let backend = AddonBackend::new(plx_stremio::addons::AddonSet::defaults());
        let p = backend.poster("tt0133093", Some("https://example.invalid/original.jpg")).unwrap();
        assert!(p.starts_with("https://btttr.cc/poster/imdb/poster-default/tt0133093.jpg"));
        assert!(p.ends_with("rs=RT"));
        assert_eq!(backend.poster("tmdb:123", Some("original")), Some("original".into()));
        assert_eq!(backend.poster("tt123?credential", Some("original")), Some("original".into()));
    }
}
