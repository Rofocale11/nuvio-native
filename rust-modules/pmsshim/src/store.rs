//! What the shim persists: the ratingKey table (so a key the app cached — Continue Watching, the
//! last-opened page, the image cache's keys — means the same title after a restart) and the local
//! watch state (PMS keeps watch state server-side; this server is the only one there is).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
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
    file: Option<PathBuf>,
    inner: Mutex<KeysInner>,
}

#[derive(Default)]
struct KeysInner {
    by_key: Vec<Ident>,
    by_ident: HashMap<Ident, i64>,
}

impl Keys {
    pub fn open(dir: Option<&Path>) -> Keys {
        let file = dir.map(|d| d.join("keys.tsv"));
        let mut inner = KeysInner::default();
        if let Some(text) = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()) {
            for line in text.lines() {
                // A line that does not decode still occupies its slot, so later keys keep their numbers.
                let ident = Ident::decode(line).unwrap_or(Ident::Title { type_: String::new(), id: format!("\u{0}{}", inner.by_key.len()) });
                let key = FIRST_KEY + inner.by_key.len() as i64;
                inner.by_ident.insert(ident.clone(), key);
                inner.by_key.push(ident);
            }
        }
        Keys { file, inner: Mutex::new(inner) }
    }

    pub fn key_of(&self, ident: &Ident) -> i64 {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(k) = g.by_ident.get(ident) {
            return *k;
        }
        let key = FIRST_KEY + g.by_key.len() as i64;
        if let Some(f) = &self.file {
            if let Ok(mut fh) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
                let _ = writeln!(fh, "{}", ident.encode());
            }
        }
        g.by_ident.insert(ident.clone(), key);
        g.by_key.push(ident.clone());
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
    file: Option<PathBuf>,
    inner: Mutex<WatchFile>,
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl WatchState {
    pub fn open(dir: Option<&Path>) -> WatchState {
        let file = dir.map(|d| d.join("watch.json"));
        let inner = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        WatchState { file, inner: Mutex::new(inner) }
    }

    fn save(&self, g: &WatchFile) {
        let Some(f) = &self.file else { return };
        let tmp = f.with_extension("json.tmp");
        if let Ok(bytes) = serde_json::to_vec(g) {
            if std::fs::write(&tmp, bytes).is_ok() {
                let _ = std::fs::rename(&tmp, f);
            }
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
