//! Import a Nuvio install's addon configuration from its WebView localStorage (read off the TV
//! with DevTools or exported by a helper), without the account backend.
//!
//! Nuvio 1.2.3 keeps, per profile (`js/data/repository/addonRepository.js`):
//!
//! * `installedAddonUrls` — `{"__profileScoped":true,"version":1,"profiles":{"<id>":[url…]}}`
//!   (a pre-profile install stored the bare array);
//! * `installedAddonEnabledStates` — same envelope around `{url: bool}`;
//! * `installedAddonDisplayNames` — same envelope around `{url: name}`;
//! * `addonManifestCacheV2` — `{"version":2,"entries":{baseUrl:{"cachedAtMs","addon":{…}}}}`, the
//!   addon in Nuvio's mapped form (`catalogs[].apiType`, normalised `resources`);
//! * `activeProfileId`, `profiles` (`usesPrimaryAddons` makes a profile read profile 1's addons).
//!
//! The export wrapper [`AddonSet::from_import_file`] reads is
//! `{"source":"nuvio-localstorage","version":1,"profile_id":"1","local_storage":{key: raw string}}`.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;

use crate::addons::{self, AddonSet};
use crate::lenient;
use crate::model::{Addon, Error, Manifest, Result};

fn parse(raw: Option<&str>) -> Option<Value> {
    serde_json::from_str(raw?.trim()).ok()
}

/// The value for `profile_id` inside a profile-scoped envelope (falling back to profile 1, which
/// is where Nuvio put pre-profile state), or the value itself when it is not an envelope.
fn scoped(v: Value, profile_id: &str) -> Option<Value> {
    let is_envelope = v.get("__profileScoped").and_then(Value::as_bool) == Some(true) && v.get("profiles").is_some_and(Value::is_object);
    if !is_envelope {
        return Some(v);
    }
    let profiles = v.get("profiles")?;
    profiles.get(profile_id).or_else(|| profiles.get("1")).cloned()
}

fn url_map<T>(v: Option<Value>, f: impl Fn(&Value) -> Option<T>) -> Vec<(String, T)> {
    match v {
        Some(Value::Object(o)) => o.iter().filter_map(|(k, v)| f(v).map(|t| (addons::url_key(k), t))).collect(),
        _ => Vec::new(),
    }
}

fn lookup<'a, T>(map: &'a [(String, T)], url: &str) -> Option<&'a T> {
    let key = addons::url_key(url);
    map.iter().find(|(k, _)| *k == key).map(|(_, t)| t)
}

/// A Nuvio-mapped cached addon back into a [`Manifest`].
fn manifest_from_cache(addon: &Value) -> Option<Manifest> {
    let mut v = addon.clone();
    if let Value::Object(o) = &mut v {
        // Mapped-form leftovers that are not manifest fields.
        for k in ["baseUrl", "displayName", "rawTypes"] {
            o.remove(k);
        }
        // The mapped form always writes `showInHome` as a boolean; it only means anything when
        // `hasExplicitShowInHome` (the manifest declared it).
        if let Some(Value::Array(cats)) = o.get_mut("catalogs") {
            for c in cats.iter_mut().filter_map(Value::as_object_mut) {
                if c.get("hasExplicitShowInHome").and_then(Value::as_bool) != Some(true) {
                    c.remove("showInHome");
                }
            }
        }
    }
    let m: Manifest = lenient::decode(v)?;
    (!m.id.trim().is_empty()).then_some(m)
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split(['/', '?']).next().unwrap_or(url).to_string()
}

impl AddonSet {
    /// Build the set from Nuvio's raw localStorage values (each the JSON text Nuvio stored).
    /// Manifests come from `addonManifestCacheV2`; an addon missing from it gets the built-in
    /// manifest (Cinemeta, OpenSubtitles) or an empty one — call
    /// [`refresh_missing_manifests`](Self::refresh_missing_manifests) to fetch those.
    pub fn from_nuvio_local_storage(
        urls_json: &str,
        enabled_json: Option<&str>,
        names_json: Option<&str>,
        manifest_cache_json: Option<&str>,
        profile_id: &str,
    ) -> Result<AddonSet> {
        let pid = if profile_id.trim().is_empty() { "1" } else { profile_id.trim() };
        let urls_v = parse(Some(urls_json)).ok_or_else(|| Error::Parse("installedAddonUrls is not JSON".into()))?;
        let urls: Vec<String> = match scoped(urls_v, pid) {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
            _ => return Err(Error::Parse("installedAddonUrls has no list for this profile".into())),
        };
        let enabled = url_map(parse(enabled_json).and_then(|v| scoped(v, pid)), |v| match v {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => Some(n.as_f64().unwrap_or(0.0) != 0.0),
            Value::String(s) => Some(matches!(s.trim().to_ascii_lowercase().as_str(), "true" | "1")),
            _ => None,
        });
        let names = url_map(parse(names_json).and_then(|v| scoped(v, pid)), |v| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string));
        let cache = url_map(parse(manifest_cache_json).and_then(|v| v.get("entries").cloned()), |e| e.get("addon").and_then(manifest_from_cache));

        let mut set = AddonSet::default();
        let mut seen = std::collections::HashSet::new();
        for raw in urls {
            let base = addons::canonicalize_url(&raw);
            if base.is_empty() || !seen.insert(addons::url_key(&base)) {
                continue;
            }
            let key = addons::url_key(&base);
            let manifest = lookup(&cache, &base).cloned().unwrap_or_else(|| {
                if key == addons::url_key(addons::CINEMETA_URL) {
                    addons::builtin_cinemeta().manifest
                } else if key == addons::url_key(addons::OPENSUBTITLES_URL) {
                    addons::builtin_opensubtitles().manifest
                } else {
                    Manifest { name: host_of(&base), ..Default::default() }
                }
            });
            set.addons.push(Addon {
                transport_url: base.clone(),
                manifest,
                disabled: !lookup(&enabled, &base).copied().unwrap_or(true),
                display_name: lookup(&names, &base).cloned(),
            });
        }
        Ok(set)
    }

    /// Read the `nuvio-localstorage` export wrapper. The profile is the file's `profile_id`, else
    /// `activeProfileId`; a profile that uses the primary profile's addons reads profile 1.
    pub fn from_import_file(path: &Path) -> Result<AddonSet> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::Parse(format!("cannot read import file: {}", e.kind())))?;
        let root: Value = serde_json::from_str(&text).map_err(|e| Error::Parse(e.to_string()))?;
        let ls = root.get("local_storage").ok_or_else(|| Error::Parse("no local_storage in import file".into()))?;
        // Values are the raw localStorage strings; tolerate an exporter that already parsed them.
        let raw = |k: &str| -> Option<String> {
            match ls.get(k)? {
                Value::String(s) => Some(s.clone()),
                Value::Null => None,
                other => Some(other.to_string()),
            }
        };
        let mut pid = root.get("profile_id").and_then(Value::as_str).map(str::to_string).unwrap_or_default();
        if pid.trim().is_empty() {
            pid = raw("activeProfileId")
                .map(|s| serde_json::from_str::<Value>(&s).ok().and_then(|v| v.as_str().map(str::to_string).or_else(|| Some(v.to_string()))).unwrap_or(s))
                .unwrap_or_else(|| "1".into());
        }
        if pid != "1" {
            if let Some(Value::Array(profiles)) = raw("profiles").and_then(|s| serde_json::from_str(&s).ok()) {
                let inherits = profiles.iter().any(|p| {
                    let id = p.get("id").or_else(|| p.get("profileIndex")).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()));
                    id.as_deref() == Some(pid.as_str())
                        && ["usesPrimaryAddons", "uses_primary_addons"].iter().any(|k| match p.get(*k) {
                            Some(Value::Bool(b)) => *b,
                            Some(Value::String(s)) => s == "true" || s == "1",
                            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0) != 0.0,
                            _ => false,
                        })
                });
                if inherits {
                    pid = "1".into();
                }
            }
        }
        let urls = raw("installedAddonUrls").ok_or_else(|| Error::Parse("no installedAddonUrls in import file".into()))?;
        AddonSet::from_nuvio_local_storage(
            &urls,
            raw("installedAddonEnabledStates").as_deref(),
            raw("installedAddonDisplayNames").as_deref(),
            raw("addonManifestCacheV2").as_deref(),
            &pid,
        )
    }

    /// Whether any addon still lacks a manifest (imported without one).
    pub fn needs_manifests(&self) -> bool {
        self.addons.iter().any(|a| a.manifest.id.trim().is_empty())
    }

    /// Fetch manifests only for addons that have none; returns how many were filled.
    pub fn refresh_missing_manifests(&mut self, deadline: Duration) -> usize {
        let missing: Vec<usize> = (0..self.addons.len()).filter(|i| self.addons[*i].manifest.id.trim().is_empty()).collect();
        if missing.is_empty() {
            return 0;
        }
        let mut sub = AddonSet { addons: missing.iter().map(|i| self.addons[*i].clone()).collect() };
        let n = sub.refresh_manifests(deadline);
        for (slot, i) in missing.into_iter().enumerate() {
            if !sub.addons[slot].manifest.id.trim().is_empty() {
                self.addons[i].manifest = sub.addons[slot].manifest.clone();
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn env(v: Value) -> String {
        json!({"__profileScoped": true, "version": 1, "profiles": {"1": v}}).to_string()
    }

    #[test]
    fn imports_profile_scoped_storage_with_cached_manifests() {
        let urls = env(json!([
            "https://catalog.example/manifest.json",
            "https://opensubtitles-v3.strem.io",
            "https://aio.example/stremio/u/cfg/manifest.json",
            "https://unknown.example"
        ]));
        let enabled = env(json!({"https://aio.example/stremio/u/cfg/manifest.json": true, "https://unknown.example": false}));
        let names = env(json!({"https://aio.example/stremio/u/cfg/manifest.json": "SELection"}));
        let cache = json!({"version": 2, "entries": {
            "https://catalog.example": {"cachedAtMs": 1, "addon": {
                "id": "com.nuvio.tmdb.catalogs", "name": "Nuvio Catalog Addon", "displayName": "Nuvio Catalog Addon",
                "version": "1.0.0", "description": null, "logo": null, "baseUrl": "https://catalog.example",
                "types": ["movie", "series"], "rawTypes": ["movie", "series"], "idPrefixes": [],
                "catalogs": [{"id": "trending", "name": "Trending", "apiType": "movie",
                              "extra": [{"name": "skip", "isRequired": false, "options": null}],
                              "pageSize": null, "showInHome": true, "hasExplicitShowInHome": true,
                              "extraSupported": [], "extraRequired": []},
                             {"id": "hidden", "name": "Hidden", "apiType": "movie", "extra": [],
                              "showInHome": false, "hasExplicitShowInHome": true},
                             {"id": "implicit", "name": "Implicit", "apiType": "series", "extra": [],
                              "showInHome": false, "hasExplicitShowInHome": false}],
                "resources": [{"name": "catalog", "types": ["movie", "series"], "idPrefixes": null},
                              {"name": "meta", "types": ["movie", "series"], "idPrefixes": ["tt", "tmdb:"]}]}},
            "https://aio.example/stremio/u/cfg": {"cachedAtMs": 1, "addon": {
                "id": "com.aiostreams.viren070", "name": "AIOStreams", "baseUrl": "https://aio.example/stremio/u/cfg",
                "types": ["movie", "series"], "idPrefixes": [], "catalogs": [],
                "resources": [{"name": "stream", "types": ["movie", "series"], "idPrefixes": ["tt", "tmdb:"]}]}}
        }});
        let set = AddonSet::from_nuvio_local_storage(&urls, Some(&enabled), Some(&names), Some(&cache.to_string()), "1").unwrap();
        assert_eq!(set.addons.len(), 4);
        assert_eq!(set.addons[0].manifest.id, "com.nuvio.tmdb.catalogs");
        assert_eq!(set.addons[0].manifest.catalogs[0].type_, "movie");
        assert_eq!(set.addons[1].manifest.id, "org.stremio.opensubtitlesv3");
        assert_eq!(set.addons[2].label(), "SELection");
        assert!(set.addons[3].disabled && set.addons[3].manifest.id.is_empty());
        assert!(set.needs_manifests());
        // showInHome is honoured
        let home: Vec<String> = set.home_catalogs().iter().map(|c| c.decl.id.clone()).collect();
        assert_eq!(home, vec!["trending", "implicit"]);
        // meta for tmdb ids goes to the catalog addon; streams for tmdb ids to AIOStreams
        assert_eq!(set.meta_candidates("movie", "tmdb:603"), vec![(0, "movie".to_string())]);
        assert_eq!(set.stream_addons("movie", "tmdb:603"), vec![(2, "movie".to_string())]);
        // Debug never prints the configured path
        let dbg = format!("{:?}", set.addons[2]);
        assert!(dbg.contains("https://aio.example") && !dbg.contains("cfg"), "{dbg}");
    }

    #[test]
    fn legacy_bare_list_and_profile_fallback() {
        let set = AddonSet::from_nuvio_local_storage(r#"["https://v3-cinemeta.strem.io"]"#, None, None, None, "3").unwrap();
        assert_eq!(set.addons[0].manifest.id, "com.linvo.cinemeta");
    }

    #[test]
    fn reads_the_export_wrapper() {
        let dir = std::env::temp_dir().join(format!("plx-stremio-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("import.json");
        let ls = json!({
            "installedAddonUrls": json!({"__profileScoped": true, "version": 1, "profiles": {"1": ["https://v3-cinemeta.strem.io"], "2": ["https://opensubtitles-v3.strem.io"]}}).to_string(),
            "activeProfileId": "\"2\"",
            "profiles": json!([{"id": "2", "usesPrimaryAddons": true}]).to_string()
        });
        std::fs::write(&path, json!({"source": "nuvio-localstorage", "version": 1, "local_storage": ls}).to_string()).unwrap();
        let set = AddonSet::from_import_file(&path).unwrap();
        assert_eq!(set.addons.len(), 1);
        assert_eq!(set.addons[0].manifest.id, "com.linvo.cinemeta");
        let _ = std::fs::remove_dir_all(dir);
    }
}
