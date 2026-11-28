//! Fork Rofocale11: carry the played source across episodes of one series.
//!
//! The shim remembers the played `LastStream` per ratingKey for resume, but an Up Next / Next
//! handoff is a DIFFERENT ratingKey. Ranking then runs from scratch and `shim_media::rank` floats
//! any direct/debrid HTTP stream ahead of torrents, so a viewer who was playing a season pack can
//! land on an unrelated (and often expired) debrid URL on the next episode — measured on the
//! stable set as `curlio: status=404` with no `sbengine:` line at all.
//!
//! Stremio's answer is `behaviorHints.bingeGroup`: implicitly pick the next episode's stream with
//! the same group. This module keeps the same idea in a small in-memory map keyed by the series
//! id (which the shim already has on every `Ident::Episode`). No new durable table: the memory is
//! a best-effort selection hint, not account state, and a process restart simply falls back to
//! today's ranking.

use crate::shim_media::Classified;
use plx_stremio::torrent::info_hash;
use std::collections::HashMap;
use std::sync::Mutex;

/// The stable, URL-independent identity of the source the viewer actually played.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceMemory {
    pub addon: String,
    pub info_hash: Option<String>,
    pub filename: Option<String>,
    pub size_bytes: Option<u64>,
    pub height: Option<u32>,
    pub binge_group: Option<String>,
}

/// Last played source per series. Bounded: a long session cannot grow it without limit, and a
/// restart falls back to ranking, so evicting an old entry is harmless.
static SERIES: std::sync::LazyLock<Mutex<HashMap<String, SourceMemory>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
const MAX_SERIES: usize = 256;

pub fn from_classified(c: &Classified) -> SourceMemory {
    SourceMemory {
        addon: c.stream.addon_name.clone(),
        info_hash: info_hash(&c.stream),
        filename: c.info.filename.clone(),
        size_bytes: c.info.size_bytes,
        height: c.info.height,
        binge_group: c.stream.behavior_hints.binge_group.clone(),
    }
}

/// Record the source just accepted by `/decision` for this series.
pub fn remember(series: &str, c: &Classified) {
    let mut map = SERIES.lock().unwrap_or_else(|e| e.into_inner());
    if map.len() >= MAX_SERIES {
        map.clear();
    }
    map.insert(series.to_string(), from_classified(c));
}

fn matches(c: &Classified, m: &SourceMemory) -> bool {
    // A torrent hash names the RELEASE, not the episode: the next episode's own stream from the
    // same pack carries the correct fileIdx, so match the hash and ignore the file index here.
    if let (Some(candidate), Some(remembered)) = (info_hash(&c.stream), m.info_hash.as_deref()) {
        return candidate == remembered;
    }
    if m.info_hash.is_some() {
        return false;
    }
    // Stremio's documented binge hint is the strongest cross-episode signal when there is no hash.
    if let (Some(candidate), Some(remembered)) =
        (c.stream.behavior_hints.binge_group.as_deref(), m.binge_group.as_deref())
    {
        if candidate == remembered {
            return true;
        }
    }
    // Direct/debrid links re-sign on every resolve, so compare stable fields only. Keep the
    // variant's height: "same addon, same resolution" is the closest fallback to a binge group.
    !m.addon.is_empty()
        && c.stream.addon_name == m.addon
        && m.height.is_none_or(|h| c.info.height == Some(h))
}

/// Promote the remembered source in a fresh ranking, ahead of the higher-ranked direct link.
/// Item-level memory and an explicit in-session pick have already been applied by the caller and
/// win over this: this only fills the gap on an episode that has never been played.
pub fn promote(out: &mut Vec<(usize, Classified)>, series: &str) -> bool {
    let memory = {
        let map = SERIES.lock().unwrap_or_else(|e| e.into_inner());
        let Some(m) = map.get(series) else { return false };
        m.clone()
    };
    let Some(p) = out.iter().position(|(_, c)| matches(c, &memory)) else {
        return false;
    };
    let x = out.remove(p);
    out.insert(0, x);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shim_media::Classified;
    use plx_stremio::classify::StreamInfo;
    use plx_stremio::model::Stream;

    fn classified(addon: &str, hash: Option<&str>, binge: Option<&str>, height: Option<u32>) -> Classified {
        let mut stream = Stream::default();
        stream.addon_name = addon.to_string();
        stream.info_hash = hash.map(str::to_string);
        stream.behavior_hints.binge_group = binge.map(str::to_string);
        Classified {
            stream,
            info: StreamInfo { height, ..Default::default() },
            url: String::new(),
        }
    }

    #[test]
    fn a_season_pack_torrent_beats_a_higher_ranked_direct_link() {
        let torrent = classified("Torrentio", Some(&"a".repeat(40)), None, Some(1080));
        let direct = classified("DebridAddon", None, None, Some(2160));
        let mut out = vec![(1usize, direct), (0usize, torrent)];

        assert!(promote(&mut out, "series-A") == false, "nothing remembered yet");
        SERIES.lock().unwrap().insert(
            "series-A".to_string(),
            SourceMemory {
                addon: "Torrentio".into(),
                info_hash: Some("a".repeat(40)),
                height: Some(1080),
                ..Default::default()
            },
        );
        assert!(promote(&mut out, "series-A"));
        assert_eq!(out[0].1.stream.addon_name, "Torrentio");
        assert!(out[0].1.stream.info_hash.is_some());
    }

    #[test]
    fn a_debrid_link_is_matched_by_addon_and_height_not_its_url() {
        let first = classified("AIO", None, Some("aio|1080"), Some(1080));
        remember("series-B", &first);
        let second = classified("AIO", None, Some("aio|1080"), Some(1080));
        let other = classified("Other", None, None, Some(1080));
        let mut out = vec![(1usize, other), (0usize, second)];
        assert!(promote(&mut out, "series-B"));
        assert_eq!(out[0].1.stream.addon_name, "AIO");
    }
}
