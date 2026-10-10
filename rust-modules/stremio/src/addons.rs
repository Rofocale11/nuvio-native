//! The installed addon collection and every addon request. CONTRACT: signatures are fixed; bodies
//! are the backend agent's to implement.

use crate::model::*;

/// One catalog the UI can show as a shelf / library section.
#[derive(Clone, Debug)]
pub struct CatalogRef {
    /// Index into [`AddonSet::addons`].
    pub addon: usize,
    pub decl: CatalogDecl,
}

impl CatalogRef {
    /// Stable key, e.g. `"com.linvo.cinemeta/movie/top"`.
    pub fn key(&self, set: &AddonSet) -> String {
        format!("{}/{}/{}", set.addons[self.addon].manifest.id, self.decl.type_, self.decl.id)
    }
}

#[derive(Clone, Debug, Default)]
pub struct AddonSet {
    pub addons: Vec<Addon>,
}

impl AddonSet {
    /// The default set when nothing is configured: Cinemeta (catalogs/meta) only.
    pub fn defaults() -> AddonSet {
        todo!()
    }

    /// Load from / save to the app's JSON file (format owned by this crate).
    pub fn load(path: &std::path::Path) -> Option<AddonSet> {
        let _ = path;
        todo!()
    }
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        let _ = path;
        todo!()
    }

    /// Fetch `<url>/manifest.json` and add (or replace by manifest id). Accepts `stremio://`
    /// URLs and URLs with or without `/manifest.json`.
    pub fn install(&mut self, url: &str) -> Result<usize> {
        let _ = url;
        todo!()
    }

    /// Catalogs suitable for Home shelves (those with no required extra), in addon order.
    pub fn home_catalogs(&self) -> Vec<CatalogRef> {
        todo!()
    }

    /// Catalogs that accept `search`.
    pub fn search_catalogs(&self) -> Vec<CatalogRef> {
        todo!()
    }

    /// `GET /catalog/{type}/{id}[/{extra}].json`. `skip` pages (Stremio pages are 100 by convention
    /// but addons vary).
    pub fn catalog(&self, cat: &CatalogRef, skip: u32, genre: Option<&str>) -> Result<Vec<MetaPreview>> {
        let _ = (cat, skip, genre);
        todo!()
    }

    /// Search every search-capable catalog in parallel; results in catalog order.
    pub fn search(&self, query: &str) -> Vec<(CatalogRef, Vec<MetaPreview>)> {
        let _ = query;
        todo!()
    }

    /// First addon (in order) that serves `meta` for this type and id prefix.
    pub fn meta(&self, type_: &str, id: &str) -> Result<Meta> {
        let _ = (type_, id);
        todo!()
    }

    /// Query every stream-capable addon in parallel with a per-addon deadline; `on_batch` is called
    /// (from the calling thread) as each addon answers, so a caller can publish progressively.
    /// Returns all streams, each tagged with `addon_name`/`addon_id`.
    pub fn streams(&self, type_: &str, id: &str, deadline: std::time::Duration, on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream> {
        let _ = (type_, id, deadline, on_batch);
        todo!()
    }

    /// Subtitles from every subtitles-capable addon.
    pub fn subtitles(&self, type_: &str, id: &str) -> Vec<Subtitle> {
        let _ = (type_, id);
        todo!()
    }
}
