//! What the shim persists: the ratingKey table (so a key the app cached — Continue Watching, the
//! last-opened page, the image cache's keys — means the same title after a restart) and the local
//! watch state (PMS keeps watch state server-side; this server is the only one there is).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// The first ratingKey handed out. Section keys are 1 and 2; keeping items far above them means a
/// stray `/library/metadata/1` can never alias a section.
pub const FIRST_KEY: i64 = 10_000;

/// A Stremio-side identity of something the app can open.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Ident {
    /// A movie or series (any Stremio type) by its addon id.
    Title { type_: String, id: String },
    /// Season `n` of a series.
    Season { series: String, n: u32 },
    /// An episode: the series id and the episode's video id.
    Episode { series: String, video: String },
}

impl Ident {
    fn encode(&self) -> String {
        match self {
            Ident::Title { type_, id } => format!("t\t{type_}\t{id}"),
            Ident::Season { series, n } => format!("s\t{series}\t{n}"),
            Ident::Episode { series, video } => format!("e\t{series}\t{video}"),
        }
    }
    fn decode(s: &str) -> Option<Ident> {
        let mut p = s.splitn(3, '\t');
        let (k, a, b) = (p.next()?, p.next()?, p.next()?);
        Some(match k {
            "t" => Ident::Title { type_: a.into(), id: b.into() },
            "s" => Ident::Season { series: a.into(), n: b.parse().ok()? },
            "e" => Ident::Episode { series: a.into(), video: b.into() },
            _ => return None,
        })
    }
}

/// Append-only key table: line N (0-based) is ratingKey `FIRST_KEY + N`.
pub struct Keys {
    inner: Mutex<KeysInner>,
}

#[derive(Default)]
struct KeysInner {
    by_key: Vec<Ident>,
    by_ident: HashMap<Ident, i64>,
}

impl Keys {
    pub fn open(dir: Option<&Path>) -> Keys {
        let mut inner = KeysInner::default();
        // Fork Rofocale11: the append-only TSV became one JSON row in the single redb file. The
        // slot-for-slot placeholder rule is unchanged so a cached ratingKey keeps its meaning.
        let rows = plx_nuvio_store::get()
            .and_then(|s| s.get(plx_nuvio_store::Table::Keys, "ids"))
            .and_then(|bytes| serde_json::from_slice::<Vec<String>>(&bytes).ok())
            .or_else(|| {
                // One-time migration from the old TSV: read it, persist it, then let the old file
                // be removed by the caller's cleanup. Never re-read it once redb has a record.
                let legacy = dir?.join("keys.tsv");
                let text = std::fs::read_to_string(legacy).ok()?;
                let rows: Vec<String> = text.lines().map(str::to_string).collect();
                let bytes = serde_json::to_vec(&rows).ok()?;
                let _ = plx_nuvio_store::get()?.put(plx_nuvio_store::Table::Keys, "ids", &bytes);
                Some(rows)
            });
        if let Some(rows) = rows {
            for line in rows {
                let ident = Ident::decode(&line).unwrap_or(Ident::Title { type_: String::new(), id: format!("\u{0}{}", inner.by_key.len()) });
                let key = FIRST_KEY + inner.by_key.len() as i64;
                inner.by_ident.insert(ident.clone(), key);
                inner.by_key.push(ident);
            }
        }
        Keys { inner: Mutex::new(inner) }
    }

    fn save(g: &KeysInner) {
        let Some(store) = plx_nuvio_store::get() else { return };
        let rows: Vec<String> = g.by_key.iter().map(Ident::encode).collect();
        if let Ok(bytes) = serde_json::to_vec(&rows) {
            let _ = store.put(plx_nuvio_store::Table::Keys, "ids", &bytes);
        }
    }

    pub fn key_of(&self, ident: &Ident) -> i64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(k) = g.by_ident.get(ident) {
            return *k;
        }
        let key = FIRST_KEY + g.by_key.len() as i64;
        g.by_ident.insert(ident.clone(), key);
        g.by_key.push(ident.clone());
        Self::save(&g);
        key
    }

    pub fn ident(&self, key: i64) -> Option<Ident> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let i = usize::try_from(key - FIRST_KEY).ok()?;
        g.by_key.get(i).cloned()
    }
}

/// One item's watch state. Times in ms, stamps in unix seconds.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Watch {
    pub offset_ms: i64,
    pub duration_ms: i64,
    pub view_count: i64,
    pub last_viewed_at: i64,
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct WatchFile {
    /// ratingKey (as a string: JSON keys) → state.
    items: HashMap<String, Watch>,
    /// The row snapshot of every item with progress, so Continue Watching draws without a fetch.
    rows: HashMap<String, serde_json::Value>,
}

pub struct WatchState {
    inner: Mutex<WatchFile>,
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl WatchState {
    pub fn open(dir: Option<&Path>) -> WatchState {
        // Fork Rofocale11: watch state moved from `watch.json` to the one redb file. The JSON
        // document is unchanged; the old file is migrated once when redb has no record yet.
        let loaded = plx_nuvio_store::get()
            .and_then(|s| s.get(plx_nuvio_store::Table::Watch, "state"))
            .and_then(|b| serde_json::from_slice::<WatchFile>(&b).ok())
            .or_else(|| {
                let legacy = dir?.join("watch.json");
                let bytes = std::fs::read(legacy).ok()?;
                let parsed = serde_json::from_slice::<WatchFile>(&bytes).ok()?;
                if let Ok(encoded) = serde_json::to_vec(&parsed) {
                    let _ = plx_nuvio_store::get()?.put(plx_nuvio_store::Table::Watch, "state", &encoded);
                }
                Some(parsed)
            });
        WatchState { inner: Mutex::new(loaded.unwrap_or_default()) }
    }

    fn save(&self, g: &WatchFile) {
        let Some(store) = plx_nuvio_store::get() else { return };
        if let Ok(bytes) = serde_json::to_vec(g) {
            let _ = store.put(plx_nuvio_store::Table::Watch, "state", &bytes);
        }
    }

    pub fn get(&self, key: i64) -> Watch {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.items.get(&key.to_string()).cloned().unwrap_or_default()
    }

    /// A playback position report. Past 90% counts as watched, as PMS does.
    pub fn progress(&self, key: i64, offset_ms: i64, duration_ms: i64, row: Option<serde_json::Value>) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let w = g.items.entry(key.to_string()).or_default();
        w.last_viewed_at = now_secs();
        if duration_ms > 0 {
            w.duration_ms = duration_ms;
        }
        if w.duration_ms > 0 && offset_ms * 10 >= w.duration_ms * 9 {
            w.view_count += 1;
            w.offset_ms = 0;
        } else {
            w.offset_ms = offset_ms.max(0);
        }
        if let Some(r) = row {
            g.rows.insert(key.to_string(), r);
        }
        self.save(&g);
    }

    pub fn scrobble(&self, key: i64, watched: bool) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let w = g.items.entry(key.to_string()).or_default();
        w.offset_ms = 0;
        if watched {
            w.view_count += 1;
            w.last_viewed_at = now_secs();
        } else {
            w.view_count = 0;
        }
        self.save(&g);
    }

    pub fn clear_progress(&self, key: i64) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(w) = g.items.get_mut(&key.to_string()) {
            w.offset_ms = 0;
        }
        self.save(&g);
    }

    /// Items with a resume point, most recent first, with their row snapshots.
    pub fn in_progress(&self) -> Vec<(i64, Watch, serde_json::Value)> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<_> = g
            .items
            .iter()
            .filter(|(_, w)| w.offset_ms > 0)
            .filter_map(|(k, w)| Some((k.parse().ok()?, w.clone(), g.rows.get(k)?.clone())))
            .collect();
        v.sort_by_key(|(_, w, _)| -w.last_viewed_at);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_persist_and_are_stable() {
        static STORE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        STORE.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!("nuvio-store-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let store = plx_nuvio_store::Store::open_at(&dir.join("t.redb")).unwrap();
            let _ = plx_nuvio_store::install(store);
        });
        let dir = std::env::temp_dir().join(format!("pmsshim-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = Ident::Title { type_: "movie".into(), id: "tt1".into() };
        let b = Ident::Episode { series: "tt2".into(), video: "tt2:1:3".into() };
        let (ka, kb) = {
            let k = Keys::open(Some(&dir));
            (k.key_of(&a), k.key_of(&b))
        };
        assert_eq!(ka, FIRST_KEY);
        let k = Keys::open(Some(&dir));
        assert_eq!(k.key_of(&b), kb);
        assert_eq!(k.ident(ka), Some(a));
        assert_eq!(k.key_of(&Ident::Season { series: "tt2".into(), n: 1 }), FIRST_KEY + 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn progress_past_ninety_percent_marks_watched() {
        let w = WatchState::open(None);
        w.progress(5, 1000, 10_000, Some(serde_json::json!({"ratingKey": "5"})));
        assert_eq!(w.get(5).offset_ms, 1000);
        assert_eq!(w.in_progress().len(), 1);
        w.progress(5, 9500, 10_000, None);
        assert_eq!(w.get(5).view_count, 1);
        assert!(w.in_progress().is_empty());
    }
}
