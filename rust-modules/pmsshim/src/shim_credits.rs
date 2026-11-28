//! Fork Rofocale11: crowd-sourced credits boundaries for sources whose FILE has no chapter atoms.
//!
//! Addons and debrid links carry no intro/credits markers, and most scene rips have no named
//! chapters. SkipDB publishes crowd-sourced `outro` segments by IMDb id with a confidence and a
//! duration-match grade; we read it best-effort, never block playback on it, and keep the answer
//! in memory for the life of the process. A chapter from the file always wins (see
//! [`crate::shim_probe::credits_marker`]); this is only the fallback.
//!
//! The read is public and credential-free. Nothing here writes to an account, and a failure is a
//! missing answer, never a reason to change playback.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// One answer per title. `None` is a settled miss — just as valuable as a hit, because it keeps a
/// title no one has submitted from being looked up on every metadata read.
#[derive(Default)]
pub struct CreditsCache {
    answers: Arc<Mutex<HashMap<String, Option<(i64, i64)>>>>,
    inflight: Arc<Mutex<HashSet<String>>>,
}

impl CreditsCache {
    pub fn get(&self, key: &str) -> Option<Option<(i64, i64)>> {
        self.answers.lock().unwrap_or_else(|e| e.into_inner()).get(key).copied()
    }

    /// Seed an answer without a network worker, so the routing test can prove that a cached outro
    /// becomes a PMS marker. Production never calls this.
    #[cfg(test)]
    pub fn put_for_test(&self, key: String, value: Option<(i64, i64)>) {
        self.answers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, value);
    }

    /// Start one bounded worker for a key. The worker stores either a hit or a settled miss;
    /// a second caller for the same title never starts a second request.
    pub fn warm(&self, key: String, url: String, duration_ms: i64) {
        {
            let mut g = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
            if !g.insert(key.clone()) {
                return;
            }
        }
        let answers = Arc::clone(&self.answers);
        let inflight = Arc::clone(&self.inflight);
        let k = key.clone();
        let spawned = std::thread::Builder::new()
            .name("pmsshim-skipdb".into())
            .spawn(move || {
                let answer = fetch(&url, duration_ms);
                let mut a = answers.lock().unwrap_or_else(|e| e.into_inner());
                if a.len() > 512 {
                    a.clear();
                }
                a.insert(k.clone(), answer);
                drop(a);
                inflight.lock().unwrap_or_else(|e| e.into_inner()).remove(&k);
            });
        if spawned.is_err() {
            self.inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
        }
    }
}

/// Parse the `outro` segment of a SkipDB `/api/segments` reply. Pure so the acceptance rule can be
/// tested without network. A low-confidence or poorly matched timestamp is worse than no answer:
/// it would put Up Next in the middle of an episode.
///
/// Timestamps are milliseconds; `duration_ms` is the PLAYING file's duration, which is what makes
/// a segment stored for a slightly different cut usable.
pub fn parse_outro(body: &[u8], duration_ms: i64) -> Option<(i64, i64)> {
    if duration_ms <= 0 {
        return None;
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    let o = v.get("segments")?.get("outro")?;
    let start = o.get("start_ms")?.as_i64()?;
    if o.get("end_ms").and_then(Value::as_i64).is_none() {
        // SkipDB allows an omitted outro end (credits run to the end), but requiring it here keeps
        // the marker's finality check local and the answer easy to reason about.
        return None;
    }
    let end = o["end_ms"].as_i64()?;
    let grade = o.get("match").and_then(Value::as_str).unwrap_or("");
    if !matches!(grade, "exact" | "shifted") {
        return None;
    }
    if o.get("confidence").and_then(Value::as_f64).unwrap_or(0.0) < 0.7 {
        return None;
    }
    // Credits cannot start in the first half of a normal episode, and the segment must be inside
    // the file we are actually playing.
    if start < duration_ms / 2 || start >= duration_ms || end <= start || end > duration_ms + 2_000 {
        return None;
    }
    Some((start, end.max(duration_ms)))
}

/// One public, body-bounded GET. A non-200, a transport failure and bad JSON all become `None`.
pub fn fetch(url: &str, duration_ms: i64) -> Option<(i64, i64)> {
    let r = plx_net::net::https_get_public_bounded(url, 32 * 1024).ok()?;
    if !r.ok() {
        return None;
    }
    parse_outro(&r.body, duration_ms)
}

/// The documented public read. `type=outro` keeps the reply small and avoids the other three
/// segment types entirely. `duration` is in SECONDS on the wire.
pub fn url(imdb_id: &str, season: Option<u32>, episode: Option<u32>, duration_ms: i64) -> String {
    let mut u = format!(
        "https://api.skipdb.tv/api/segments?imdb_id={imdb_id}&type=outro&duration={}",
        duration_ms / 1000
    );
    if let (Some(s), Some(e)) = (season, episode) {
        u.push_str(&format!("&season={s}&episode={e}"));
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_confident_shifted_outro_is_accepted_as_the_credits_boundary() {
        let body = br#"{"segments":{"outro":{"start_ms":9057631,"end_ms":9654048,"match":"shifted","confidence":0.82}}}"#;
        assert_eq!(parse_outro(body, 9_654_048), Some((9_057_631, 9_654_048)));
    }

    #[test]
    fn an_out_of_range_or_low_confidence_answer_is_not_trusted() {
        let bad = |grade: &str, conf: f64| -> Vec<u8> {
            serde_json::json!({"segments": {"outro": {
                "start_ms": 1000, "end_ms": 2000, "match": grade, "confidence": conf
            }}})
            .to_string()
            .into_bytes()
        };
        assert!(parse_outro(&bad("out-of-range", 0.9), 3_000_000).is_none());
        assert!(parse_outro(&bad("exact", 0.4), 3_000_000).is_none());
        assert!(parse_outro(&bad("exact", 0.9), 3_000_000).is_none(), "a segment in the first half is not credits");
    }
}
