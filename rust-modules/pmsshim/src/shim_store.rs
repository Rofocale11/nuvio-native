//! What the shim persists: the ratingKey table (so a key the app cached — Continue Watching, the
//! last-opened page, the image cache's keys — means the same title after a restart) and the local
//! watch state (PMS keeps watch state server-side; this server is the only one there is).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    /// `content_id|video_id` → the ratingKey this device pushed it under, so a resume point pulled
    /// back from the account lands on the same item instead of a duplicate keyed by its TMDB id.
    account_keys: HashMap<String, i64>,
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

    /// Apply a resume point pulled from the account. The newer side wins: a local report made after
    /// the remote one is the truth, so the merge only lands when `last_viewed_at` is at least the
    /// local stamp. Returns whether anything changed (so a caller can log/report a real merge).
    pub fn merge_remote(
        &self,
        key: i64,
        offset_ms: i64,
        duration_ms: i64,
        last_viewed_at: i64,
        row: Option<serde_json::Value>,
    ) -> bool {
        if offset_ms <= 0 {
            return false;
        }
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let w = g.items.entry(key.to_string()).or_default();
        if last_viewed_at < w.last_viewed_at {
            return false;
        }
        w.offset_ms = offset_ms;
        if duration_ms > 0 {
            w.duration_ms = duration_ms;
        }
        w.last_viewed_at = last_viewed_at;
        if let Some(r) = row {
            g.rows.insert(key.to_string(), r);
        }
        self.save(&g);
        true
    }

    /// Remember which local item an account resume point was pushed under.
    pub fn bind_account_key(&self, content_id: &str, video_id: &str, key: i64) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.account_keys.insert(format!("{content_id}|{video_id}"), key);
        self.save(&g);
    }

    pub fn account_key(&self, content_id: &str, video_id: &str) -> Option<i64> {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.account_keys.get(&format!("{content_id}|{video_id}")).copied()
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

/// How long a series' meta stays usable — in memory and in the persisted log. Its `videos` array
/// IS the episode list (`seasons_of`/`episodes_of` read it), so it has to refresh as episodes air.
pub const META_TTL_SERIES: Duration = Duration::from_secs(6 * 3600);
/// A movie's meta does not change once released, so the warm start may keep it far longer.
pub const META_TTL_MOVIE: Duration = Duration::from_secs(30 * 24 * 3600);

/// The TTL for one row. Anything that is not a series is treated as a movie: the addon types the
/// shim serves are `movie` and `series` (`is_series` in `shim_pms`), and a longer life is the
/// safer default for a type this cache does not know.
fn meta_ttl(type_: &str) -> i64 {
    if type_ == "series" {
        META_TTL_SERIES.as_secs() as i64
    } else {
        META_TTL_MOVIE.as_secs() as i64
    }
}
/// The most titles kept in memory; past it the oldest fetch is dropped.
const META_ROWS: usize = 400;
/// Rewrite the log once it passes this, down to about [`META_COMPACT`] bytes.
const META_LOG_LIMIT: u64 = 8 * 1024 * 1024;
const META_COMPACT: usize = 4 * 1024 * 1024;

#[derive(serde::Serialize, serde::Deserialize)]
struct MetaRow {
    #[serde(rename = "type")]
    type_: String,
    id: String,
    /// Unix seconds when the addon answered.
    at: i64,
    meta: plx_stremio::model::Meta,
    /// The addons-changed generation this row was fetched under; rows from an older generation
    /// are dropped rather than appended after a `clear`.
    #[serde(skip, default)]
    generation: u64,
}

/// The addon-meta cache, with a warm-start copy on disk.
///
/// A restart used to make every detail open cold. The rows are the expensive part (a series meta
/// can be ~65 KB), so they are appended to `meta-cache.jsonl` as they land — the last line for a
/// key wins at load — and the log is rewritten down to [`META_COMPACT`] when it grows past
/// [`META_LOG_LIMIT`]. Appending one small line is what keeps this off the request path; the
/// write itself runs on the shared persistence worker.
pub struct MetaCache {
    file: Option<PathBuf>,
    rows: Mutex<HashMap<(String, String), (i64, Arc<plx_stremio::model::Meta>)>>,
    /// Fetched rows not yet appended to the log.
    unsaved: Mutex<Vec<MetaRow>>,
    /// One writer at a time: appends and a compaction both take it.
    write: Mutex<()>,
    pending: AtomicBool,
    generation: AtomicU64,
}

impl MetaCache {
    pub fn open(dir: Option<&Path>) -> Arc<MetaCache> {
        let file = dir.map(|d| d.join("meta-cache.jsonl"));
        let now = now_secs();
        let mut rows = HashMap::new();
        let mut skipped = 0;
        if let Some(text) = file.as_ref().and_then(|f| std::fs::read_to_string(f).ok()) {
            for line in text.lines() {
                // A torn last line (the app died mid-append) is skipped, not fatal to the cache.
                let Ok(row) = serde_json::from_str::<MetaRow>(line) else {
                    skipped += 1;
                    continue;
                };
                if now.saturating_sub(row.at) >= meta_ttl(&row.type_) {
                    continue;
                }
                rows.insert((row.type_, row.id), (row.at, Arc::new(row.meta)));
            }
        }
        if file.is_some() {
            plx_base::eventlog::log(&format!(
                "shim: meta cache warm-start with {} row(s), {skipped} unreadable",
                rows.len()
            ));
        }
        Arc::new(MetaCache {
            file,
            rows: Mutex::new(rows),
            unsaved: Mutex::new(Vec::new()),
            write: Mutex::new(()),
            pending: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        })
    }

    pub fn get(&self, type_: &str, id: &str) -> Option<Arc<plx_stremio::model::Meta>> {
        let rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let (at, meta) = rows.get(&(type_.to_string(), id.to_string()))?;
        (now_secs().saturating_sub(*at) < meta_ttl(type_)).then(|| Arc::clone(meta))
    }

    pub fn insert(self: &Arc<Self>, type_: &str, id: &str, meta: Arc<plx_stremio::model::Meta>) {
        let at = now_secs();
        let key = (type_.to_string(), id.to_string());
        {
            let mut rows = self.rows.lock().unwrap_or_else(|e| e.into_inner());
            if !rows.contains_key(&key) && rows.len() >= META_ROWS {
                if let Some(oldest) = rows.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) {
                    rows.remove(&oldest);
                }
            }
            rows.insert(key, (at, Arc::clone(&meta)));
        }
        if self.file.is_none() {
            return;
        }
        self.unsaved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(MetaRow {
                type_: type_.to_string(),
                id: id.to_string(),
                at,
                meta: (*meta).clone(),
                generation: self.generation.load(Ordering::SeqCst),
            });
        self.schedule_save();
    }

    /// Forget everything — the addon set changed, so the metas may describe other content.
    pub fn clear(self: &Arc<Self>) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.rows.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.unsaved.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let Some(file) = &self.file else { return };
        let _writing = self.write.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::remove_file(file);
    }

    fn schedule_save(self: &Arc<Self>) {
        if self.pending.swap(true, Ordering::SeqCst) {
            return;
        }
        let cache = Arc::clone(self);
        if plx_base::storage_worker::submit(move || cache.save_pending()).is_err() {
            self.pending.store(false, Ordering::SeqCst);
        }
    }

    /// Append whatever is queued, and keep going if fetches landed while the write ran. `pending`
    /// is cleared under the `unsaved` lock, so a row that arrives during that moment either is in
    /// the batch just written or sees `pending == false` and schedules the next write.
    fn save_pending(&self) {
        loop {
            let batch = std::mem::take(&mut *self.unsaved.lock().unwrap_or_else(|e| e.into_inner()));
            if !batch.is_empty() {
                self.append(batch);
            }
            let unsaved = self.unsaved.lock().unwrap_or_else(|e| e.into_inner());
            if unsaved.is_empty() {
                self.pending.store(false, Ordering::SeqCst);
                return;
            }
        }
    }

    fn append(&self, batch: Vec<MetaRow>) {
        let Some(file) = &self.file else { return };
        let generation = self.generation.load(Ordering::SeqCst);
        let mut out = String::new();
        for row in batch.into_iter().filter(|row| row.generation == generation) {
            if let Ok(line) = serde_json::to_string(&row) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        if out.is_empty() {
            return;
        }
        let _writing = self.write.lock().unwrap_or_else(|e| e.into_inner());
        if std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .and_then(|mut f| f.write_all(out.as_bytes()))
            .is_err()
        {
            return;
        }
        if std::fs::metadata(file).map(|m| m.len()).unwrap_or(0) > META_LOG_LIMIT {
            self.compact();
        }
    }

    /// Rewrite the log with one line per held row, newest first, under [`META_COMPACT`].
    fn compact(&self) {
        let Some(file) = &self.file else { return };
        let mut newest: Vec<(i64, String, String, Arc<plx_stremio::model::Meta>)> = self
            .rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|((type_, id), (at, meta))| (*at, type_.clone(), id.clone(), Arc::clone(meta)))
            .collect();
        newest.sort_by_key(|(at, ..)| std::cmp::Reverse(*at));
        let mut out = String::new();
        for (at, type_, id, meta) in newest {
            let row = MetaRow { type_, id, at, meta: (*meta).clone(), generation: 0 };
            let Ok(line) = serde_json::to_string(&row) else { continue };
            if out.len() + line.len() + 1 > META_COMPACT {
                break;
            }
            out.push_str(&line);
            out.push('\n');
        }
        let tmp = file.with_extension(format!("jsonl.tmp.{}", std::process::id()));
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, file);
        }
    }

    /// Run the queue now. Tests only: production must not write the log on a request thread.
    #[cfg(test)]
    fn flush_for_test(&self) {
        self.save_pending();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: &str, name: &str) -> plx_stremio::model::Meta {
        plx_stremio::model::Meta { id: id.into(), type_: "movie".into(), name: name.into(), ..Default::default() }
    }

    fn meta_scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pmsshim-meta-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_fetched_meta_is_there_after_a_restart() {
        let dir = meta_scratch("restart");
        let cache = MetaCache::open(Some(&dir));
        cache.insert("movie", "tt1", Arc::new(meta("tt1", "Kept")));
        cache.flush_for_test();
        drop(cache);
        let reopened = MetaCache::open(Some(&dir));
        let got = reopened.get("movie", "tt1").expect("the row survived the restart");
        assert_eq!(got.name, "Kept");
    }

    #[test]
    fn a_series_expires_in_hours_and_a_movie_keeps_for_a_month() {
        let dir = meta_scratch("expiry");
        let hours_ago = META_TTL_SERIES.as_secs() as i64 + 3600;
        let months_ago = META_TTL_MOVIE.as_secs() as i64 + 3600;
        let row = |type_: &str, id: &str, name: &str, at: i64| MetaRow {
            type_: type_.into(),
            id: id.into(),
            at,
            meta: plx_stremio::model::Meta { type_: type_.into(), ..meta(id, name) },
            generation: 0,
        };
        let text = [
            // A show's episode list must refresh as episodes air; a movie's meta does not change.
            row("series", "tt1", "Stale Show", now_secs() - hours_ago),
            row("movie", "tt2", "Old Movie", now_secs() - hours_ago),
            row("movie", "tt3", "Ancient Movie", now_secs() - months_ago),
            row("series", "tt4", "Fresh Show", now_secs()),
        ]
        .iter()
        .map(|row| serde_json::to_string(row).unwrap())
        .chain(std::iter::once("{\"type\":\"movie\",\"id\":\"tt5\",\"at\":0".into()))
        .collect::<Vec<_>>()
        .join("\n");
        std::fs::write(dir.join("meta-cache.jsonl"), text).unwrap();
        let cache = MetaCache::open(Some(&dir));
        assert!(cache.get("series", "tt1").is_none(), "a series past its hours");
        assert_eq!(cache.get("movie", "tt2").unwrap().name, "Old Movie", "a movie keeps for a month");
        assert!(cache.get("movie", "tt3").is_none(), "a movie past its month");
        assert_eq!(cache.get("series", "tt4").unwrap().name, "Fresh Show");
    }

    #[test]
    fn clearing_the_cache_forgets_the_file_too() {
        let dir = meta_scratch("clear");
        let cache = MetaCache::open(Some(&dir));
        cache.insert("movie", "tt5", Arc::new(meta("tt5", "Gone")));
        cache.flush_for_test();
        assert!(dir.join("meta-cache.jsonl").exists());
        cache.clear();
        assert!(cache.get("movie", "tt5").is_none());
        assert!(!dir.join("meta-cache.jsonl").exists(), "the warm-start copy is gone with the memory");
    }

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

    #[test]
    fn merge_remote_takes_only_the_newer_side() {
        let w = WatchState::open(None);
        w.progress(7, 1000, 10_000, None);
        let local_stamp = w.get(7).last_viewed_at;
        // Older than the local report: ignored.
        assert!(!w.merge_remote(7, 4000, 10_000, local_stamp - 60, None));
        assert_eq!(w.get(7).offset_ms, 1000);
        // Newer: lands, and its row snapshot becomes the Continue Watching row.
        assert!(w.merge_remote(7, 4000, 20_000, local_stamp + 60, Some(serde_json::json!({"ratingKey": "7"}))));
        let got = w.get(7);
        assert_eq!((got.offset_ms, got.duration_ms, got.last_viewed_at), (4000, 20_000, local_stamp + 60));
        assert_eq!(w.in_progress().len(), 1);
        // No resume point: nothing to merge.
        assert!(!w.merge_remote(7, 0, 20_000, local_stamp + 120, None));
        // A finished title keeps its view count but accepts a resume point.
        w.scrobble(8, true);
        assert!(w.merge_remote(8, 2000, 10_000, now_secs() + 60, None));
        assert_eq!((w.get(8).view_count, w.get(8).offset_ms), (1, 2000));
    }
}
