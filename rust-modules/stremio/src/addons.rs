//! The installed addon collection and every addon request. CONTRACT: signatures are fixed; bodies
//! are the backend agent's to implement.
//!
//! Behaviour mirrors NuvioTVSmart (`js/data/repository/{addon,catalog,meta,stream}Repository.js`)
//! so a user's Nuvio addon list behaves the same here: URL canonicalisation (query strings on the
//! base URL are preserved and re-appended after the resource path, as AIOStreams/Torrentio config
//! URLs need), `encodeURIComponent` of ids, resource/type/idPrefix matching including the
//! id-prefix type recovery for meta, the meta fallback order, and per-addon stream fetches that
//! publish as each addon answers.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::addon_http as http;
use crate::lenient;
use crate::model::*;

pub const CINEMETA_URL: &str = "https://v3-cinemeta.strem.io";
pub const OPENSUBTITLES_URL: &str = "https://opensubtitles-v3.strem.io";

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

    /// Display name: the catalog's own name, else its id.
    pub fn name(&self) -> &str {
        self.decl.name.as_deref().filter(|n| !n.trim().is_empty()).unwrap_or(&self.decl.id)
    }

    /// Whether the catalog accepts `extra` named `name`.
    pub fn supports_extra(&self, name: &str) -> bool {
        self.decl.supports_extra(name)
    }
}

impl CatalogDecl {
    pub fn supports_extra(&self, name: &str) -> bool {
        let n = name.trim();
        self.extra.iter().any(|e| e.name.eq_ignore_ascii_case(n))
            || self.extra_supported.iter().any(|e| e.trim().eq_ignore_ascii_case(n))
            || self.extra_required.iter().any(|e| e.trim().eq_ignore_ascii_case(n))
    }

    /// Items per page for `skip` paging: the declared `pageSize`, else 100 (Nuvio's default).
    pub fn skip_step(&self) -> u32 {
        self.page_size.filter(|n| *n > 0).unwrap_or(100)
    }

    /// Extras the addon will refuse to answer without.
    pub fn required_extras(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.extra.iter().filter(|e| e.is_required).map(|e| e.name.as_str()).collect();
        for r in &self.extra_required {
            if !out.contains(&r.as_str()) {
                out.push(r);
            }
        }
        out
    }

    /// Genre options the catalog advertises (for a genre filter).
    pub fn genre_options(&self) -> Vec<String> {
        self.extra.iter().find(|e| e.name == "genre").map(|e| e.options.clone()).unwrap_or_default()
    }
}

impl Manifest {
    /// `resources` normalised to objects. A plain-name resource, and an object without `types`,
    /// inherit the manifest's `types`; `id_prefixes: None` means "inherit the manifest's".
    pub fn resources(&self) -> Vec<ResourceDecl> {
        self.raw_resources
            .iter()
            .filter_map(|r| match r {
                Value::String(name) => Some(ResourceDecl { name: name.clone(), types: Some(self.types.clone()), id_prefixes: None }),
                Value::Object(o) => {
                    let name = o.get("name")?.as_str()?.to_string();
                    let strings = |k: &str| -> Option<Vec<String>> {
                        o.get(k)?.as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    };
                    Some(ResourceDecl {
                        name,
                        types: strings("types").or_else(|| Some(self.types.clone())),
                        id_prefixes: strings("idPrefixes"),
                    })
                }
                _ => None,
            })
            .collect()
    }

    pub fn has_resource(&self, name: &str) -> bool {
        self.resources().iter().any(|r| r.name.eq_ignore_ascii_case(name))
    }
}

// --- URL handling -------------------------------------------------------------------------------

/// Nuvio's `canonicalizeUrl` + `normalizeCinemetaUrl`: trim, `stremio://` → `https://`, strip
/// trailing slashes and a trailing `/manifest.json` from the PATH while keeping any query string.
pub fn canonicalize_url(url: &str) -> String {
    let mut s = url.trim().to_string();
    if let Some(rest) = s.strip_prefix("stremio://") {
        s = format!("https://{rest}");
    }
    let s = s.trim_end_matches('/');
    let (path, query) = match s.find('?') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    };
    let mut path = path.trim_end_matches('/').to_string();
    if path.to_ascii_lowercase().ends_with("/manifest.json") {
        path.truncate(path.len() - "/manifest.json".len());
        path = path.trim_end_matches('/').to_string();
    }
    let lower = path.to_ascii_lowercase();
    for old in ["https://cinemeta-v3.strem.io", "http://cinemeta-v3.strem.io"] {
        if lower.starts_with(old) {
            path = format!("{CINEMETA_URL}{}", &path[old.len()..]);
            break;
        }
    }
    format!("{path}{query}")
}

/// Key two addon URLs compare equal under (Nuvio's `normalizeUrl`).
pub fn url_key(url: &str) -> String {
    canonicalize_url(url).to_ascii_lowercase()
}

fn split_base(base: &str) -> (String, String) {
    let clean = canonicalize_url(base);
    match clean.find('?') {
        Some(i) => (clean[..i].trim_end_matches('/').to_string(), clean[i..].to_string()),
        None => (clean, String::new()),
    }
}

pub fn manifest_url(base: &str) -> String {
    let (path, query) = split_base(base);
    format!("{path}/manifest.json{query}")
}

/// `{base}/{resource}/{type}/{id}.json{query}` with type and id `encodeURIComponent`-ed (meta,
/// stream, subtitles — what Nuvio sends).
pub fn resource_url(base: &str, resource: &str, type_: &str, id: &str) -> String {
    let (path, query) = split_base(base);
    format!("{path}/{resource}/{}/{}.json{query}", http::encode_component(type_), http::encode_component(id))
}

/// Nuvio's `buildCatalogUrl`: type and catalog id raw; extras `k=v&…` encoded; `skip` appended
/// after the other extras; no extras and no skip → the bare catalog path.
pub fn catalog_url(base: &str, type_: &str, catalog_id: &str, extras: &[(&str, String)], skip: u32) -> String {
    let (path, query) = split_base(base);
    if extras.is_empty() {
        return if skip > 0 {
            format!("{path}/catalog/{type_}/{catalog_id}/skip={skip}.json{query}")
        } else {
            format!("{path}/catalog/{type_}/{catalog_id}.json{query}")
        };
    }
    let mut parts: Vec<String> =
        extras.iter().map(|(k, v)| format!("{}={}", http::encode_component(k), http::encode_component(v))).collect();
    if skip > 0 && !extras.iter().any(|(k, _)| *k == "skip") {
        parts.push(format!("skip={skip}"));
    }
    format!("{path}/catalog/{type_}/{catalog_id}/{}.json{query}", parts.join("&"))
}

// --- capability matching (addonRepository.js) ---------------------------------------------------

fn resource_prefixes<'a>(addon: &'a Addon, r: &'a ResourceDecl) -> Vec<&'a str> {
    let list = match &r.id_prefixes {
        Some(p) if !p.is_empty() => p,
        _ => match &addon.manifest.id_prefixes {
            Some(p) => p,
            None => return Vec::new(),
        },
    };
    list.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect()
}

fn resource_supports_type(r: &ResourceDecl, type_: &str) -> bool {
    let t = type_.trim();
    if t.is_empty() {
        return false;
    }
    match &r.types {
        Some(types) if !types.iter().all(|x| x.trim().is_empty()) => types.iter().any(|x| x.trim().eq_ignore_ascii_case(t)),
        _ => true,
    }
}

fn resource_supports_id(addon: &Addon, r: &ResourceDecl, id: &str, case_insensitive: bool) -> bool {
    let prefixes = resource_prefixes(addon, r);
    if prefixes.is_empty() {
        return true;
    }
    if case_insensitive {
        let lid = id.to_ascii_lowercase();
        prefixes.iter().any(|p| lid.starts_with(&p.to_ascii_lowercase()))
    } else {
        prefixes.iter().any(|p| id.starts_with(p))
    }
}

/// Nuvio's `resolveResourceRequestType`: the type to request `resource` with from this addon, or
/// `None` if it does not serve it. `allow_id_type_fallback` recovers the type from an unambiguous
/// id-prefix owner (meta only; Nuvio never does it for streams).
pub fn resolve_request_type(
    addon: &Addon,
    resource: &str,
    requested_type: &str,
    id: &str,
    case_insensitive: bool,
    allow_id_type_fallback: bool,
) -> Option<String> {
    let resources: Vec<ResourceDecl> = addon
        .manifest
        .resources()
        .into_iter()
        .filter(|r| r.name.trim().eq_ignore_ascii_case(resource) && resource_supports_id(addon, r, id, case_insensitive))
        .collect();
    if resources.is_empty() {
        return None;
    }
    let req = requested_type.trim();
    if !req.is_empty() && resources.iter().any(|r| resource_supports_type(r, req)) {
        return Some(req.to_string());
    }
    if !allow_id_type_fallback {
        return None;
    }
    let mut recovered: Vec<String> = Vec::new();
    for r in &resources {
        if resource_prefixes(addon, r).is_empty() {
            continue;
        }
        let types = r.types.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| addon.manifest.types.clone());
        for t in types {
            let t = t.trim().to_string();
            if !t.is_empty() && !recovered.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                recovered.push(t);
            }
        }
    }
    if recovered.len() == 1 {
        recovered.pop()
    } else {
        None
    }
}

/// Nuvio's `inferCanonicalType`.
pub fn infer_type(type_: &str, id: &str) -> String {
    let t = type_.trim();
    let lower = t.to_ascii_lowercase();
    if lower == "tv" {
        return "series".into();
    }
    if ["movie", "series", "channel", "anime"].contains(&lower.as_str()) {
        return t.to_string();
    }
    let lid = id.to_ascii_lowercase();
    if lid.contains(":movie:") {
        return "movie".into();
    }
    if lid.contains(":series:") {
        return "series".into();
    }
    t.to_string()
}

/// Nuvio's `buildContentLevelMetaId`: `tt123:1:2` → `tt123`, `kitsu:42:3` → `kitsu:42`.
pub fn content_level_id(video_id: &str) -> String {
    let raw = video_id.trim();
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() <= 1 {
        return raw.to_string();
    }
    let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let trailing = parts.iter().rev().take_while(|p| numeric(p)).count();
    let first = parts[0];
    let min_segments = if first.to_ascii_lowercase().starts_with("tt") || numeric(first) { 1 } else { 2 };
    let drop = trailing.min(parts.len().saturating_sub(min_segments));
    if drop > 0 {
        parts[..parts.len() - drop].join(":")
    } else {
        raw.to_string()
    }
}

// --- decoding -----------------------------------------------------------------------------------

/// Decode a manifest; `None` unless it has an id and at least the shape of one.
pub fn parse_manifest(v: Value) -> Option<Manifest> {
    let m: Manifest = lenient::decode(v)?;
    if m.id.trim().is_empty() {
        return None;
    }
    Some(m)
}

/// Cinemeta/Kitsu/… `videos` carry `number` beside or instead of `episode`, and `description`
/// beside or instead of `overview`; fold them before typed decoding.
fn normalise_videos(meta: &mut Value) {
    if let Some(Value::Array(videos)) = meta.get_mut("videos") {
        for v in videos.iter_mut() {
            if let Value::Object(o) = v {
                let number = o.remove("number");
                if !o.get("episode").is_some_and(|e| !e.is_null()) {
                    if let Some(n) = number {
                        o.insert("episode".into(), n);
                    }
                }
                let desc = o.remove("description");
                if !o.get("overview").is_some_and(|e| !e.is_null()) {
                    if let Some(d) = desc {
                        o.insert("overview".into(), d);
                    }
                }
            }
        }
    }
}

pub fn parse_meta(mut v: Value) -> Option<Meta> {
    normalise_videos(&mut v);
    let m: Meta = lenient::decode(v)?;
    if m.id.trim().is_empty() {
        return None;
    }
    Some(m)
}

fn parse_streams(body: Value, addon: &Addon) -> Vec<Stream> {
    let streams = match body {
        Value::Object(mut o) => o.remove("streams"),
        _ => None,
    };
    tag(lenient::decode_list::<Stream>(streams), addon)
}

fn tag(mut streams: Vec<Stream>, addon: &Addon) -> Vec<Stream> {
    streams.retain(|s| s.url.is_some() || s.info_hash.is_some() || s.external_url.is_some() || s.yt_id.is_some());
    for s in streams.iter_mut() {
        s.addon_name = addon.label().to_string();
        s.addon_id = addon.manifest.id.clone();
        s.subtitles.retain(|sub| !sub.url.is_empty());
    }
    streams
}

// --- persistence --------------------------------------------------------------------------------

#[derive(serde::Serialize, serde::Deserialize)]
struct Persisted {
    version: u32,
    addons: Vec<Addon>,
}

const PERSIST_VERSION: u32 = 1;

// --- the set ------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct AddonSet {
    pub addons: Vec<Addon>,
}

/// The manifest Cinemeta serves (trimmed to what this app reads), so a first boot shows Home
/// without waiting on a manifest fetch. `install`/`refresh_manifests` replace it with the live one.
pub fn builtin_cinemeta() -> Addon {
    let v = serde_json::json!({
        "id": "com.linvo.cinemeta",
        "name": "Cinemeta",
        "version": "builtin",
        "description": "The official addon for movie and series catalogs",
        "resources": ["catalog", "meta", "addon_catalog"],
        "types": ["movie", "series"],
        "idPrefixes": ["tt"],
        "catalogs": [
            {"type": "movie", "id": "top", "name": "Popular",
             "extra": [{"name": "genre"}, {"name": "search"}, {"name": "skip"}]},
            {"type": "series", "id": "top", "name": "Popular",
             "extra": [{"name": "genre"}, {"name": "search"}, {"name": "skip"}]},
            {"type": "movie", "id": "imdbRating", "name": "Featured",
             "extra": [{"name": "genre"}, {"name": "skip"}]},
            {"type": "series", "id": "imdbRating", "name": "Featured",
             "extra": [{"name": "genre"}, {"name": "skip"}]}
        ]
    });
    Addon { transport_url: CINEMETA_URL.into(), manifest: parse_manifest(v).expect("builtin manifest"), disabled: false, display_name: None }
}

pub fn builtin_opensubtitles() -> Addon {
    let v = serde_json::json!({
        "id": "org.stremio.opensubtitlesv3",
        "name": "OpenSubtitles v3",
        "version": "builtin",
        "resources": ["subtitles"],
        "types": ["movie", "series"],
        "idPrefixes": ["tt"],
        "catalogs": []
    });
    Addon { transport_url: OPENSUBTITLES_URL.into(), manifest: parse_manifest(v).expect("builtin manifest"), disabled: false, display_name: None }
}

/// Run `jobs` (one per addon index) on worker threads — or serially if libcurl's TLS is not
/// thread-safe here — delivering results in completion order until `deadline`.
fn fan_out<T: Send + 'static>(
    jobs: Vec<Job<T>>,
    deadline: Duration,
    on_result: &mut dyn FnMut(usize, T),
) {
    let start = Instant::now();
    if jobs.is_empty() {
        return;
    }
    if !http::parallel_ok() {
        for (i, job) in jobs {
            let left = deadline.saturating_sub(start.elapsed());
            if left.is_zero() {
                break;
            }
            on_result(i, job(left));
        }
        return;
    }
    let (tx, rx) = mpsc::channel::<(usize, T)>();
    let mut pending = 0usize;
    for (i, job) in jobs {
        let tx = tx.clone();
        let spawned = plx_base::task::spawn_small("stremio-addon", move || {
            let _ = tx.send((i, job(deadline)));
        });
        if spawned {
            pending += 1;
        }
    }
    drop(tx);
    while pending > 0 {
        let left = deadline.saturating_sub(start.elapsed());
        if left.is_zero() {
            break;
        }
        match rx.recv_timeout(left) {
            Ok((i, r)) => {
                pending -= 1;
                on_result(i, r);
            }
            Err(_) => break,
        }
    }
}

type Job<T> = (usize, Box<dyn FnOnce(Duration) -> T + Send>);

impl AddonSet {
    /// The default set when nothing is configured: Cinemeta (catalogs/meta) and OpenSubtitles v3,
    /// Nuvio's own defaults. No network.
    pub fn defaults() -> AddonSet {
        AddonSet { addons: vec![builtin_cinemeta(), builtin_opensubtitles()] }
    }

    /// Load from / save to the app's JSON file (format owned by this crate).
    pub fn load(path: &Path) -> Option<AddonSet> {
        let bytes = std::fs::read(path).ok()?;
        Self::from_json(std::str::from_utf8(&bytes).ok()?)
    }

    /// Versioned, opaque storage payload. Contains addon credentials; never log it.
    pub fn from_json(payload: &str) -> Option<AddonSet> {
        let p: Persisted = serde_json::from_str(payload).ok()?;
        if p.version != PERSIST_VERSION {
            return None;
        }
        Some(AddonSet { addons: p.addons })
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string(&Persisted { version: PERSIST_VERSION, addons: self.addons.clone() })
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let bytes = self.to_json().map_err(std::io::Error::other)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, path)
    }

    /// Fetch one addon's manifest.
    pub fn fetch_manifest(url: &str) -> Result<Manifest> {
        let base = canonicalize_url(url);
        if base.is_empty() {
            return Err(Error::Parse("empty addon URL".into()));
        }
        let v = http::get_json(&manifest_url(&base), http::CONTROL)?;
        parse_manifest(v).ok_or_else(|| Error::Parse("not a Stremio manifest".into()))
    }

    /// Fetch `<url>/manifest.json` and add (or replace by manifest id). Accepts `stremio://`
    /// URLs and URLs with or without `/manifest.json`.
    pub fn install(&mut self, url: &str) -> Result<usize> {
        let base = canonicalize_url(url);
        let manifest = Self::fetch_manifest(&base)?;
        Ok(self.put(Addon { transport_url: base, manifest, disabled: false, display_name: None }))
    }

    /// Insert or replace (same canonical URL, else same manifest id) without network.
    pub fn put(&mut self, addon: Addon) -> usize {
        let key = url_key(&addon.transport_url);
        if let Some(i) = self.addons.iter().position(|a| url_key(&a.transport_url) == key) {
            self.addons[i] = addon;
            return i;
        }
        if let Some(i) = self.addons.iter().position(|a| a.manifest.id == addon.manifest.id && !a.manifest.id.is_empty()) {
            self.addons[i] = addon;
            return i;
        }
        self.addons.push(addon);
        self.addons.len() - 1
    }

    pub fn remove(&mut self, index: usize) -> Option<Addon> {
        (index < self.addons.len()).then(|| self.addons.remove(index))
    }

    /// Re-fetch every manifest in parallel; an addon whose fetch fails keeps the manifest it had.
    /// Returns how many refreshed.
    pub fn refresh_manifests(&mut self, deadline: Duration) -> usize {
        let jobs: Vec<Job<Result<Manifest>>> = self
            .addons
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let url = a.transport_url.clone();
                (i, Box::new(move |_left: Duration| AddonSet::fetch_manifest(&url)) as Box<dyn FnOnce(Duration) -> _ + Send>)
            })
            .collect();
        let mut n = 0;
        let addons = &mut self.addons;
        fan_out(jobs, deadline, &mut |i, r| {
            if let Ok(m) = r {
                addons[i].manifest = m;
                n += 1;
            }
        });
        n
    }

    fn enabled(&self) -> impl Iterator<Item = (usize, &Addon)> {
        self.addons.iter().enumerate().filter(|(_, a)| !a.disabled)
    }

    /// Catalogs suitable for Home shelves, in addon order: no required extra, and — Nuvio's
    /// `catalogShouldShowOnHome` — not explicitly `showInHome: false`.
    pub fn home_catalogs(&self) -> Vec<CatalogRef> {
        self.enabled()
            .flat_map(|(i, a)| {
                a.manifest
                    .catalogs
                    .iter()
                    .filter(|c| c.required_extras().is_empty() && c.show_in_home != Some(false))
                    .map(move |c| CatalogRef { addon: i, decl: c.clone() })
            })
            .collect()
    }

    /// Every catalog of every enabled addon that can be browsed without a search query (Home
    /// shelves plus the ones hidden from Home) — the library/discover view.
    pub fn browse_catalogs(&self) -> Vec<CatalogRef> {
        self.enabled()
            .flat_map(|(i, a)| {
                a.manifest
                    .catalogs
                    .iter()
                    .filter(|c| !c.required_extras().iter().any(|r| r.eq_ignore_ascii_case("search")))
                    .map(move |c| CatalogRef { addon: i, decl: c.clone() })
            })
            .collect()
    }

    /// Catalogs that accept `search`.
    pub fn search_catalogs(&self) -> Vec<CatalogRef> {
        self.enabled()
            .flat_map(|(i, a)| {
                a.manifest
                    .catalogs
                    .iter()
                    .filter(|c| c.supports_extra("search") && c.required_extras().iter().all(|r| r.eq_ignore_ascii_case("search")))
                    .map(move |c| CatalogRef { addon: i, decl: c.clone() })
            })
            .collect()
    }

    /// `GET /catalog/{type}/{id}[/{extra}].json`. `skip` pages (Stremio pages are 100 by convention
    /// but addons vary).
    pub fn catalog(&self, cat: &CatalogRef, skip: u32, genre: Option<&str>) -> Result<Vec<MetaPreview>> {
        let mut extras: Vec<(&str, String)> = Vec::new();
        if let Some(g) = genre.filter(|g| !g.is_empty()) {
            extras.push(("genre", g.to_string()));
        }
        self.catalog_with(cat, &extras, skip)
    }

    /// [`catalog`](Self::catalog) with arbitrary extras.
    pub fn catalog_with(&self, cat: &CatalogRef, extras: &[(&str, String)], skip: u32) -> Result<Vec<MetaPreview>> {
        let addon = self.addons.get(cat.addon).ok_or(Error::NoAddon)?;
        let url = catalog_url(&addon.transport_url, &cat.decl.type_, &cat.decl.id, extras, skip);
        catalog_from_url(&url, http::ADDON, &cat.decl.type_)
    }

    /// Search every search-capable catalog in parallel; results in catalog order.
    pub fn search(&self, query: &str) -> Vec<(CatalogRef, Vec<MetaPreview>)> {
        self.search_within(query, Duration::from_secs(15))
    }

    pub fn search_within(&self, query: &str, deadline: Duration) -> Vec<(CatalogRef, Vec<MetaPreview>)> {
        let q = query.trim();
        if q.is_empty() {
            return Vec::new();
        }
        let cats = self.search_catalogs();
        let jobs: Vec<Job<Result<Vec<MetaPreview>>>> = cats
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let url = catalog_url(&self.addons[c.addon].transport_url, &c.decl.type_, &c.decl.id, &[("search", q.to_string())], 0);
                let ty = c.decl.type_.clone();
                (i, Box::new(move |left: Duration| catalog_from_url(&url, http::within_ms(left.as_millis() as u64), &ty)) as Box<dyn FnOnce(Duration) -> _ + Send>)
            })
            .collect();
        let mut got: Vec<Option<Vec<MetaPreview>>> = vec![None; cats.len()];
        fan_out(jobs, deadline, &mut |i, r| {
            if let Ok(items) = r {
                got[i] = Some(items);
            }
        });
        cats.into_iter().zip(got).filter_map(|(c, r)| r.filter(|v| !v.is_empty()).map(|v| (c, v))).collect()
    }

    /// Candidate (addon index, request type) pairs for `meta`, in Nuvio's order: id-prefix owners
    /// first, then every addon serving the requested type, then the inferred type.
    pub fn meta_candidates(&self, type_: &str, id: &str) -> Vec<(usize, String)> {
        let requested = type_.trim();
        let inferred = infer_type(requested, id);
        let mut out: Vec<(usize, String)> = Vec::new();
        let mut add = |i: usize, t: String| {
            if !out.iter().any(|(j, u)| *j == i && u.eq_ignore_ascii_case(&t)) {
                out.push((i, t));
            }
        };
        for (i, a) in self.enabled() {
            let owns = a.manifest.resources().iter().any(|r| {
                r.name.eq_ignore_ascii_case("meta") && !resource_prefixes(a, r).is_empty() && resource_supports_id(a, r, id, true)
            });
            if owns {
                if let Some(t) = resolve_request_type(a, "meta", requested, id, true, false) {
                    add(i, t);
                }
            }
        }
        for (i, a) in self.enabled() {
            if let Some(t) = resolve_request_type(a, "meta", requested, id, true, false) {
                add(i, t);
            }
        }
        if !inferred.eq_ignore_ascii_case(requested) {
            for (i, a) in self.enabled() {
                if let Some(t) = resolve_request_type(a, "meta", &inferred, id, true, false) {
                    add(i, t);
                }
            }
        }
        out
    }

    /// Like [`meta`](Self::meta), but asks `source` first — the addon whose catalog the item came
    /// from, which is what Nuvio's detail page does — when it serves meta for this id (with the
    /// id-prefix type recovery). Falls back to the ordinary candidate order.
    pub fn meta_preferring(&self, source: Option<usize>, type_: &str, id: &str) -> Result<Meta> {
        if let Some(i) = source {
            if let Some(a) = self.addons.get(i).filter(|a| !a.disabled) {
                let req = infer_type(type_, id);
                let t = resolve_request_type(a, "meta", type_, id, true, true).or_else(|| resolve_request_type(a, "meta", &req, id, true, true));
                if let Some(t) = t {
                    if let Ok(m) = self.meta_from(i, &t, id) {
                        return Ok(m);
                    }
                }
            }
        }
        self.meta(type_, id)
    }

    /// The id to ask stream addons for. Streams are keyed by IMDb id almost everywhere; an item
    /// from a `tmdb:` catalog carries its IMDb id in `meta.imdb_id`. When fewer stream addons
    /// serve `id` than its IMDb twin, the twin wins (`tmdb:603:1:2` → `tt0133093:1:2`).
    pub fn stream_id(&self, type_: &str, id: &str, imdb_id: Option<&str>) -> String {
        let Some(imdb) = imdb_id.map(str::trim).filter(|s| s.starts_with("tt")) else { return id.to_string() };
        let content = content_level_id(id);
        let suffix = &id[content.len().min(id.len())..];
        let twin = format!("{imdb}{suffix}");
        if twin == id {
            return twin;
        }
        if self.stream_addons(type_, &twin).len() > self.stream_addons(type_, id).len() {
            twin
        } else {
            id.to_string()
        }
    }

    /// First addon (in order) that serves `meta` for this type and id prefix.
    pub fn meta(&self, type_: &str, id: &str) -> Result<Meta> {
        let mut last = Error::NoAddon;
        for (i, t) in self.meta_candidates(type_, id) {
            match self.meta_from(i, &t, id) {
                Ok(m) => return Ok(m),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// `GET /meta/{type}/{id}.json` from one addon.
    pub fn meta_from(&self, addon: usize, type_: &str, id: &str) -> Result<Meta> {
        let a = self.addons.get(addon).ok_or(Error::NoAddon)?;
        let body = http::get_json(&resource_url(&a.transport_url, "meta", type_, id), http::ADDON)?;
        let meta = match body {
            Value::Object(mut o) => o.remove("meta"),
            _ => None,
        };
        meta.and_then(parse_meta).ok_or(Error::Status(404))
    }

    /// Addons (index, request type) that serve streams for this type/id — no id-type fallback.
    pub fn stream_addons(&self, type_: &str, id: &str) -> Vec<(usize, String)> {
        self.enabled().filter_map(|(i, a)| resolve_request_type(a, "stream", type_, id, false, false).map(|t| (i, t))).collect()
    }

    /// Query every stream-capable addon in parallel with a per-addon deadline; `on_batch` is called
    /// (from the calling thread) as each addon answers, so a caller can publish progressively.
    /// Returns all streams, each tagged with `addon_name`/`addon_id`, grouped in ADDON order
    /// (the user's order) — `on_batch` sees them in completion order.
    pub fn streams(&self, type_: &str, id: &str, deadline: std::time::Duration, on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream> {
        let targets = self.stream_addons(type_, id);
        let jobs: Vec<Job<Vec<Stream>>> = targets
            .iter()
            .enumerate()
            .map(|(slot, (i, t))| {
                let addon = self.addons[*i].clone();
                let (t, id) = (t.clone(), id.to_string());
                (slot, Box::new(move |left: Duration| streams_from_addon(&addon, &t, &id, left)) as Box<dyn FnOnce(Duration) -> _ + Send>)
            })
            .collect();
        let mut got: Vec<Vec<Stream>> = vec![Vec::new(); targets.len()];
        fan_out(jobs, deadline, &mut |slot, streams| {
            if !streams.is_empty() {
                on_batch(&streams);
            }
            got[slot] = streams;
        });
        got.into_iter().flatten().collect()
    }

    /// Subtitles from every subtitles-capable addon.
    pub fn subtitles(&self, type_: &str, id: &str) -> Vec<Subtitle> {
        self.subtitles_within(type_, id, Duration::from_secs(10))
    }

    pub fn subtitles_within(&self, type_: &str, id: &str, deadline: Duration) -> Vec<Subtitle> {
        let targets: Vec<(usize, String)> = self
            .enabled()
            .filter_map(|(i, a)| resolve_request_type(a, "subtitles", type_, id, false, false).map(|t| (i, t)))
            .collect();
        let jobs: Vec<Job<Vec<Subtitle>>> = targets
            .iter()
            .enumerate()
            .map(|(slot, (i, t))| {
                let url = resource_url(&self.addons[*i].transport_url, "subtitles", t, id);
                (slot, Box::new(move |left: Duration| {
                    match http::get_json(&url, http::within_ms(left.as_millis() as u64)) {
                        Ok(Value::Object(mut o)) => lenient::decode_list::<Subtitle>(o.remove("subtitles")),
                        _ => Vec::new(),
                    }
                }) as Box<dyn FnOnce(Duration) -> _ + Send>)
            })
            .collect();
        let mut got: Vec<Vec<Subtitle>> = vec![Vec::new(); targets.len()];
        fan_out(jobs, deadline, &mut |slot, subs| got[slot] = subs);
        let mut out: Vec<Subtitle> = got.into_iter().flatten().filter(|s| !s.url.is_empty()).collect();
        let mut seen = std::collections::HashSet::new();
        out.retain(|s| seen.insert(s.url.clone()));
        out
    }
}

fn catalog_from_url(url: &str, t: plx_net::net::Timeouts, type_: &str) -> Result<Vec<MetaPreview>> {
    let body = http::get_json(url, t)?;
    let metas = match body {
        Value::Object(mut o) => o.remove("metas"),
        _ => None,
    };
    let mut items: Vec<MetaPreview> = lenient::decode_list(metas);
    items.retain(|m| !m.id.trim().is_empty());
    for m in items.iter_mut() {
        if m.type_.is_empty() {
            m.type_ = type_.to_string();
        }
        if m.name.trim().is_empty() {
            m.name = "Untitled".into();
        }
    }
    Ok(items)
}

/// One addon's streams; Nuvio's inline-meta fallback when the stream endpoint answers empty.
fn streams_from_addon(addon: &Addon, type_: &str, id: &str, left: Duration) -> Vec<Stream> {
    let start = Instant::now();
    let t = http::within_ms(left.as_millis() as u64);
    let body = match http::get_json(&resource_url(&addon.transport_url, "stream", type_, id), t) {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let streams = parse_streams(body, addon);
    if !streams.is_empty() {
        return streams;
    }
    // Inline streams on the meta's matching video (debrid-cloud style addons).
    let content = content_level_id(id);
    let mut candidates = vec![content.clone()];
    if content != id {
        candidates.push(id.to_string());
    }
    for meta_id in candidates {
        let left = left.saturating_sub(start.elapsed());
        if left < Duration::from_millis(500) {
            break;
        }
        let Ok(Value::Object(mut o)) = http::get_json(&resource_url(&addon.transport_url, "meta", type_, &meta_id), http::within_ms(left.as_millis() as u64)) else {
            continue;
        };
        let Some(Value::Object(mut meta)) = o.remove("meta") else { continue };
        let Some(Value::Array(videos)) = meta.remove("videos") else { continue };
        let single = type_ != "series" && videos.len() == 1;
        let matching = videos
            .iter()
            .position(|v| v.get("id").and_then(Value::as_str) == Some(id))
            .or(if single { Some(0) } else { None });
        if let Some(ix) = matching {
            let found = tag(lenient::decode_list(videos[ix].get("streams").cloned()), addon);
            if !found.is_empty() {
                return found;
            }
        }
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addon(manifest: Value, url: &str) -> Addon {
        Addon { transport_url: url.into(), manifest: parse_manifest(manifest).unwrap(), disabled: false, display_name: None }
    }

    fn torrentio() -> Addon {
        addon(
            serde_json::json!({
                "id": "com.stremio.torrentio.addon", "name": "Torrentio", "version": "0.0.15",
                "resources": [{"name": "stream", "types": ["movie", "series", "anime"], "idPrefixes": ["tt", "kitsu"]}],
                "types": ["movie", "series", "anime", "other"], "catalogs": []
            }),
            "https://torrentio.strem.fun/providers=yts|eztv/manifest.json",
        )
    }

    #[test]
    fn canonicalizes_like_nuvio() {
        assert_eq!(canonicalize_url(" https://a.b/x/manifest.json/ "), "https://a.b/x");
        assert_eq!(canonicalize_url("stremio://a.b/cfg/manifest.json"), "https://a.b/cfg");
        assert_eq!(canonicalize_url("https://a.b/x/MANIFEST.JSON?k=1"), "https://a.b/x?k=1");
        assert_eq!(canonicalize_url("https://cinemeta-v3.strem.io/"), CINEMETA_URL);
        assert_eq!(manifest_url("https://a.b/x?k=1"), "https://a.b/x/manifest.json?k=1");
    }

    #[test]
    fn builds_resource_urls() {
        assert_eq!(
            resource_url("https://torrentio.strem.fun/providers=yts/manifest.json", "stream", "series", "tt0944947:1:2"),
            "https://torrentio.strem.fun/providers=yts/stream/series/tt0944947%3A1%3A2.json"
        );
        assert_eq!(resource_url("https://a.b/c?x=1", "meta", "movie", "tt1"), "https://a.b/c/meta/movie/tt1.json?x=1");
    }

    #[test]
    fn builds_catalog_urls() {
        let b = CINEMETA_URL;
        assert_eq!(catalog_url(b, "movie", "top", &[], 0), format!("{b}/catalog/movie/top.json"));
        assert_eq!(catalog_url(b, "movie", "top", &[], 100), format!("{b}/catalog/movie/top/skip=100.json"));
        assert_eq!(
            catalog_url(b, "movie", "top", &[("genre", "Sci-Fi & Fantasy".into())], 50),
            format!("{b}/catalog/movie/top/genre=Sci-Fi%20%26%20Fantasy&skip=50.json")
        );
        assert_eq!(
            catalog_url(b, "series", "top", &[("search", "the office".into())], 0),
            format!("{b}/catalog/series/top/search=the%20office.json")
        );
    }

    #[test]
    fn matches_resources() {
        let t = torrentio();
        assert_eq!(resolve_request_type(&t, "stream", "movie", "tt0111161", false, false).as_deref(), Some("movie"));
        assert_eq!(resolve_request_type(&t, "stream", "series", "tt0944947:1:1", false, false).as_deref(), Some("series"));
        assert_eq!(resolve_request_type(&t, "stream", "movie", "tmdb:123", false, false), None);
        assert_eq!(resolve_request_type(&t, "meta", "movie", "tt1", false, false), None);
        let c = builtin_cinemeta();
        assert_eq!(resolve_request_type(&c, "meta", "series", "tt1", true, false).as_deref(), Some("series"));
        assert_eq!(resolve_request_type(&c, "meta", "channel", "tt1", true, false), None);
        // id-prefix owner with one declared type recovers the type
        let k = addon(
            serde_json::json!({"id": "kitsu", "name": "Kitsu", "resources": [{"name": "meta", "types": ["anime"], "idPrefixes": ["kitsu"]}], "types": ["anime"], "catalogs": []}),
            "https://k.example",
        );
        assert_eq!(resolve_request_type(&k, "meta", "series", "kitsu:1", true, true).as_deref(), Some("anime"));
        assert_eq!(resolve_request_type(&k, "meta", "series", "kitsu:1", true, false), None);
    }

    #[test]
    fn plain_string_resources_inherit_manifest_types_and_prefixes() {
        let a = addon(
            serde_json::json!({"id": "x", "name": "X", "resources": ["stream"], "types": ["movie"], "idPrefixes": ["tt"], "catalogs": []}),
            "https://x.example",
        );
        assert!(resolve_request_type(&a, "stream", "movie", "tt1", false, false).is_some());
        assert!(resolve_request_type(&a, "stream", "series", "tt1", false, false).is_none());
        assert!(resolve_request_type(&a, "stream", "movie", "kitsu:1", false, false).is_none());
    }

    #[test]
    fn meta_candidates_prefer_prefix_owners() {
        let generic = addon(
            serde_json::json!({"id": "g", "name": "G", "resources": ["meta"], "types": ["movie", "series"], "catalogs": []}),
            "https://g.example",
        );
        let set = AddonSet { addons: vec![generic, builtin_cinemeta()] };
        let c = set.meta_candidates("movie", "tt1");
        assert_eq!(c, vec![(1, "movie".into()), (0, "movie".into())]);
        // `tv` is resolved through the inferred `series` pass, which is plain addon order (Nuvio).
        assert_eq!(set.meta_candidates("tv", "tt1"), vec![(0, "series".into()), (1, "series".into())]);
    }

    #[test]
    fn content_level_ids() {
        assert_eq!(content_level_id("tt0944947:1:2"), "tt0944947");
        assert_eq!(content_level_id("kitsu:42:3"), "kitsu:42");
        assert_eq!(content_level_id("kitsu:42"), "kitsu:42");
        assert_eq!(content_level_id("tt1"), "tt1");
        assert_eq!(content_level_id("dmm:abc"), "dmm:abc");
    }

    #[test]
    fn catalogs_and_search() {
        let set = AddonSet::defaults();
        let home = set.home_catalogs();
        assert_eq!(home.len(), 4);
        assert_eq!(home[0].key(&set), "com.linvo.cinemeta/movie/top");
        let search = set.search_catalogs();
        assert_eq!(search.len(), 2);
        assert!(search.iter().all(|c| c.decl.id == "top"));
        assert!(set.stream_addons("movie", "tt1").is_empty());
    }

    #[test]
    fn required_extra_catalogs_are_not_home_shelves() {
        let a = addon(
            serde_json::json!({"id": "s", "name": "S", "resources": ["catalog"], "types": ["movie"],
                "catalogs": [{"type": "movie", "id": "search", "extra": [{"name": "search", "isRequired": true}]},
                             {"type": "movie", "id": "legacy", "extraRequired": ["genre"]}]}),
            "https://s.example",
        );
        let set = AddonSet { addons: vec![a] };
        assert!(set.home_catalogs().is_empty());
        assert_eq!(set.search_catalogs().len(), 1);
    }

    #[test]
    fn parses_streams_and_tags_them() {
        let t = torrentio();
        let body = serde_json::json!({"streams": [
            {"name": "Torrentio\n4k DV", "title": "Movie.2160p.mkv\n👤 12 💾 58.2 GB ⚙️ RARBG", "infoHash": "ABCDEF0123456789ABCDEF0123456789ABCDEF01", "fileIdx": 0,
             "behaviorHints": {"bingeGroup": "torrentio|4k", "filename": "Movie.2160p.mkv"}},
            {"name": "bad entry with no locator"},
            {"url": "https://example.com/x.mp4", "name": "Direct", "subtitles": [{"url": "", "lang": "en"}, {"url": "https://s/x.srt", "lang": "en"}]}
        ]});
        let s = parse_streams(body, &t);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].addon_name, "Torrentio");
        assert_eq!(s[0].file_idx, Some(0));
        assert_eq!(s[0].behavior_hints.filename.as_deref(), Some("Movie.2160p.mkv"));
        assert_eq!(s[1].subtitles.len(), 1);
    }

    #[test]
    fn parses_cinemeta_style_videos() {
        let v = serde_json::json!({"id": "tt0944947", "type": "series", "name": "GoT", "videos": [
            {"id": "tt0944947:1:1", "season": 1, "episode": 1, "number": 1, "name": "Winter Is Coming", "description": "d", "overview": "o"},
            {"id": "tt0944947:1:2", "season": 1, "number": 2, "title": "The Kingsroad", "description": "d2"}
        ]});
        let m = parse_meta(v).unwrap();
        assert_eq!(m.videos.len(), 2);
        assert_eq!(m.videos[0].episode, Some(1));
        assert_eq!(m.videos[0].overview.as_deref(), Some("o"));
        assert_eq!(m.videos[1].episode, Some(2));
        assert_eq!(m.videos[1].overview.as_deref(), Some("d2"));
    }

    #[test]
    fn persists_round_trip() {
        let dir = std::env::temp_dir().join(format!("plx-stremio-test-{}", std::process::id()));
        let path = dir.join("addons.json");
        let mut set = AddonSet::defaults();
        set.put(torrentio());
        set.addons[2].disabled = true;
        set.save(&path).unwrap();
        let back = AddonSet::load(&path).unwrap();
        assert_eq!(back.addons.len(), 3);
        assert!(back.addons[2].disabled);
        assert_eq!(back.addons[2].manifest.resources().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn put_replaces_by_url_then_id() {
        let mut set = AddonSet::defaults();
        let i = set.put(torrentio());
        let mut again = torrentio();
        again.transport_url = "https://torrentio.strem.fun/other-config".into();
        assert_eq!(set.put(again), i);
        assert_eq!(set.addons.len(), 3);
    }
}
