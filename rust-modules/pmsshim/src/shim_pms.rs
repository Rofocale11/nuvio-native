//! The PMS routes. Every shape here is one `plx_plex::plex::models` deserialises; fields it does not
//! read are left out rather than invented.

use crate::shim_backend::{Backend, CatalogInfo};
use crate::shim_core::{Config, MACHINE_ID};
use crate::shim_http::{pct_encode, Request, Response};
use crate::shim_media::{self, Classified, SubRef, PART_STRIDE};
use crate::shim_store::{now_secs, Ident, Keys, Watch, WatchState};
use plx_stremio::addons_tmdb::{
    TmdbCast, TmdbClient, TmdbCollection, TmdbCredit, TmdbImages, TmdbMovie, TmdbMovieDetails,
    TmdbShow, TmdbShowDetails, TmdbVideo,
};
use plx_stremio::model::{Meta, MetaPreview, Stream, Video};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How often a *playing* report may mirror to the Nuvio account. A paused or stopped report is a
/// real change and goes at once; the app reports every few seconds, and one API call per report
/// would be a call per ten seconds for the length of a film.
const WATCH_PUSH_INTERVAL: Duration = Duration::from_secs(60);

const META_TTL: Duration = Duration::from_secs(6 * 3600);
const CATALOG_TTL: Duration = Duration::from_secs(15 * 60);
/// How long `/hubs` waits for catalogs that are not cached yet; late ones land in the cache for the
/// next fetch rather than holding Home up.
const HUBS_BUDGET: Duration = Duration::from_secs(4);
/// Catalog fetches in flight at once (the TV's network and CPU are shared with the UI).
const CATALOG_PARALLEL: usize = 8;
const STREAM_TTL: Duration = Duration::from_secs(30 * 60);
/// Rows a catalog listing reports beyond what it has fetched while the catalog is not exhausted,
/// so a grid keeps asking for the next page.
const MORE_HINT: i64 = 50;
/// How long a DETAIL request waits for streams. The page must not hang on addons; a play waits
/// the full deadline at `/decision` / the Part instead.
const DETAIL_WAIT: Duration = Duration::from_millis(800);
/// The Part index that means "whichever stream leads when this is played".
const BEST_PART: usize = PART_STRIDE as usize - 1;
const DEFAULT_EPISODE_MS: i64 = 45 * 60_000;
const DEFAULT_MOVIE_MS: i64 = 110 * 60_000;
const SECTION_MOVIES: i64 = 1;
const SECTION_SHOWS: i64 = 2;
const TMDB_FALLBACK_KEY_BASE: i64 = 1_000_000_000;
/// Above the movie base so the two decoders cannot alias, a billion apart because TMDB movie ids
/// already pass 100,000.
const TMDB_SHOW_KEY_BASE: i64 = 2_000_000_000;
/// A Cast & Crew headshot's synthetic `personId` — TMDB's person id, encoded, above both title
/// bases so a person key never opens a title.
const TMDB_PERSON_KEY_BASE: i64 = 3_000_000_000;
/// A TMDB collection's ratingKey — the franchise page a member film's collection heading opens.
/// Above the person base, so no title or person key can land on one.
const TMDB_COLLECTION_KEY_BASE: i64 = 4_000_000_000;

/// A synthetic related-movie ratingKey's TMDB id.
fn synthetic_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_FALLBACK_KEY_BASE..TMDB_SHOW_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_FALLBACK_KEY_BASE) as u64)
}

/// A synthetic related-show ratingKey's TMDB id. Bounded above by the person base, or a person key
/// would decode as the show a billion ids below it.
fn synthetic_show_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_SHOW_KEY_BASE..TMDB_PERSON_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_SHOW_KEY_BASE) as u64)
}

/// A synthetic Cast & Crew person key's TMDB person id, bounded above by the collection base so a
/// collection page is never mistaken for a person.
fn synthetic_person_id(rk: i64) -> Option<u64> {
    (TMDB_PERSON_KEY_BASE..TMDB_COLLECTION_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_PERSON_KEY_BASE) as u64)
}

/// A synthetic collection ratingKey's TMDB collection id.
fn synthetic_collection_id(rk: i64) -> Option<u64> {
    (rk >= TMDB_COLLECTION_KEY_BASE).then_some((rk - TMDB_COLLECTION_KEY_BASE) as u64)
}

/// The Stremio id addons know a synthetic related ratingKey by (`tmdb:603`), if it is synthetic.
fn translate_to_stremio_id(rk: i64) -> Option<String> {
    synthetic_show_tmdb_id(rk)
        .or_else(|| synthetic_tmdb_id(rk))
        .map(|tmdb_id| format!("tmdb:{tmdb_id}"))
}

fn resolve_imdb_id(rk: i64, client: &TmdbClient) -> Option<String> {
    if let Some(tmdb_id) = synthetic_show_tmdb_id(rk) {
        return client.tv_external_ids(tmdb_id).ok().flatten();
    }
    synthetic_tmdb_id(rk).and_then(|tmdb_id| client.movie_external_ids(tmdb_id).ok().flatten())
}

/// The addon identity of a synthetic related key, or `None` for a real one. This is the fallback
/// for every route that needs an `Ident`: a synthetic key is never written to `keys.tsv`, so
/// `Keys::ident` cannot resolve it.
fn synthetic_ident(rk: i64) -> Option<Ident> {
    let id = translate_to_stremio_id(rk)?;
    let type_ = if synthetic_show_tmdb_id(rk).is_some() {
        "series"
    } else {
        "movie"
    };
    Some(Ident::Title {
        type_: type_.into(),
        id,
    })
}

struct CatalogCache {
    at: Instant,
    rows: Vec<MetaPreview>,
    exhausted: bool,
}

#[derive(Default)]
struct SlotState {
    /// Arrival order — a Part id names a stream by its position HERE, which never changes while the
    /// slot lives, however the ranking moves as later addons answer.
    streams: Vec<Stream>,
    done: bool,
    /// A stream (arrival index) the user picked; it leads `Media[]`.
    selected: Option<usize>,
    subs: Vec<SubRef>,
}

struct StreamSlot {
    started: Instant,
    state: Mutex<SlotState>,
    cv: Condvar,
}

pub struct Shim {
    backend: Arc<dyn Backend>,
    cfg: Config,
    keys: Keys,
    watch: WatchState,
    /// When each item's resume point was last mirrored to the Nuvio account, so a playing report
    /// (the app sends one every few seconds) does not become one API call every few seconds.
    watch_pushes: Mutex<HashMap<i64, Instant>>,
    metas: Mutex<HashMap<(String, String), (Instant, Arc<Meta>)>>,
    catalogs: Mutex<HashMap<(String, Option<String>), Arc<Mutex<CatalogCache>>>>,
    catalog_list: Mutex<Option<(Instant, Vec<CatalogInfo>)>>,
    streams: Mutex<HashMap<i64, Arc<StreamSlot>>>,
    sub_urls: Mutex<HashMap<i64, String>>,
    poster_fallbacks: Mutex<HashMap<String, String>>,
    tmdb_fallbacks: Mutex<HashMap<i64, TmdbMovie>>,
    /// The TMDB franchises a detail page has shown a collection shelf for, by TMDB collection id.
    /// The shelf's linked heading opens a collection PAGE, whose three routes (section listing,
    /// header, children) all read from here — TMDB is the only source, and a page the user just
    /// saw must not answer "unavailable" because nothing was kept.
    collections: Mutex<HashMap<u64, TmdbCollection>>,
    /// The rows of the last filmography served. A person page follows its
    /// `/library/people/{id}/media` answer with ONE batched `/library/metadata/{csv}` for the
    /// character captions; without this every key in it would cost a TMDB lookup (and a stream
    /// slot) — 48 of each for a prolific actor. One list because one person page is open at a
    /// time, which also keeps the caption right: the same film has a different part per person.
    credit_rows: Mutex<Vec<Value>>,
    next_sub: AtomicI64,
    permits: (Mutex<usize>, Condvar),
    /// Real stream metadata from the app's FFmpeg (installed at boot; absent on the host bin).
    pub probes: crate::shim_probe::ProbeCache,
    /// Torrent markers resolved on a streaming server (`/create`), by marker + ratingKey.
    resolved: Mutex<HashMap<(i64, String), String>>,
    torrent_server: std::sync::RwLock<Option<String>>,
    /// Whether peer-to-peer streams may be offered at all (see [`Config::p2p_enabled`]). Read by
    /// [`Shim::torrent_server`], so every stream list, pick and resolve that goes through a
    /// streaming server is gated by this one switch.
    p2p_enabled: AtomicBool,
    /// Audio codecs this TV decodes (`devcaps`); empty = unknown, rank on video alone.
    audio_codecs: std::sync::RwLock<Vec<String>>,
    /// Set by [`Shim::bind_self`]: background work started from a `&self` method.
    me: std::sync::OnceLock<std::sync::Weak<Shim>>,
    /// plex.tv's account surface, answered by the Nuvio account (`account.rs`).
    pub account: crate::shim_account::Account,
}

fn container(fields: Value) -> Value {
    let mut mc = Map::new();
    mc.insert("size".into(), json!(0));
    mc.insert("allowSync".into(), json!(false));
    mc.insert("identifier".into(), json!("com.plexapp.plugins.library"));
    if let Value::Object(f) = fields {
        for (k, v) in f {
            mc.insert(k, v);
        }
    }
    for k in ["Metadata", "Directory", "Hub"] {
        if let Some(n) = mc.get(k).and_then(Value::as_array).map(Vec::len) {
            mc.insert("size".into(), json!(n));
        }
    }
    json!({ "MediaContainer": Value::Object(mc) })
}

fn ok_empty() -> Response {
    Response::json(&container(json!({})))
}

/// A small stable id for a tag (genre/person) name: PMS tag ids are integers.
fn tag_id(s: &str) -> i64 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x01000193);
    }
    i64::from(h & 0x7fff_ffff)
}

fn tags(v: &[String], role: bool) -> Value {
    Value::Array(
        v.iter()
            .filter(|s| !s.is_empty())
            .take(20)
            .map(|s| {
                if role {
                    json!({"tag": s, "id": tag_id(s), "role": ""})
                } else {
                    json!({"tag": s, "id": tag_id(s)})
                }
            })
            .collect(),
    )
}

fn year_of(s: Option<&str>) -> Option<i64> {
    let s = s?;
    let digits: String = s
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take(4)
        .collect();
    (digits.len() == 4).then(|| digits.parse().ok()).flatten()
}

/// `"142 min"`, `"2h 22min"`, `"45"` → ms.
fn runtime_ms(s: Option<&str>) -> Option<i64> {
    let s = s?.to_lowercase();
    let mut total = 0i64;
    let mut num = String::new();
    let mut any = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        if !num.is_empty() {
            let n: i64 = num.parse().unwrap_or(0);
            num.clear();
            if c == 'h' {
                total += n * 60;
            } else {
                total += n;
            }
            any = true;
        }
    }
    if !num.is_empty() {
        total += num.parse::<i64>().unwrap_or(0);
        any = true;
    }
    (any && total > 0).then_some(total * 60_000)
}

fn date_of(s: Option<&str>) -> Option<String> {
    let s = s?;
    (s.len() >= 10 && s.as_bytes()[4] == b'-').then(|| s[..10].to_string())
}

fn rating_of(s: Option<&str>) -> Option<f64> {
    s?.trim().parse::<f64>().ok().filter(|r| *r > 0.0)
}

fn is_series(type_: &str) -> bool {
    type_ == "series"
}

/// Whether a report is worth an account call: a paused or stopped one always (it is a real
/// change), a playing one at most once per [`WATCH_PUSH_INTERVAL`] — the app reports every few
/// seconds and one call per report would be one per ten seconds for the length of a film.
fn watch_push_due(last: Option<Instant>, settled: bool, now: Instant) -> bool {
    settled || last.is_none_or(|at| now.duration_since(at) >= WATCH_PUSH_INTERVAL)
}

/// The TMDB id behind an addon id. A `tmdb:` or numeric id needs no lookup at all; an IMDb one is
/// resolved through TMDB, which needs the key and the network — so an install without a TMDB key
/// syncs only the ids that already are TMDB.
fn tmdb_id(id: &str, series: bool) -> Option<u64> {
    let id = plx_stremio::addons::content_level_id(id);
    let bare = id
        .strip_prefix("tmdb:")
        .or_else(|| id.strip_prefix("TMDB:"))
        .or_else(|| id.strip_prefix("tmdb"))
        .unwrap_or(&id);
    if let Ok(n) = bare.trim_start_matches(':').parse::<u64>() {
        return (n > 0).then_some(n);
    }
    let client = TmdbClient::from_env()?;
    if series { client.series_id(&id).ok().flatten() } else { client.movie_id(&id).ok().flatten() }
}

/// The series an episode id belongs to: `tt0944947:1:2` → `tt0944947`. Falls back to the entry's
/// content id when the id carries no season/episode numbers.
fn episode_series_id<'a>(video_id: &'a str, content_id: &'a str) -> &'a str {
    video_id
        .rsplit_once(':')
        .and_then(|(rest, _)| rest.rsplit_once(':'))
        .map(|(series, _)| series)
        .unwrap_or(content_id)
}

fn season_title(n: u32) -> String {
    if n == 0 {
        "Specials".into()
    } else {
        format!("Season {n}")
    }
}

/// Episodes in a stable (season, episode) order; season-less videos fold into season 1.
fn episodes_of(meta: &Meta) -> Vec<(u32, u32, &Video)> {
    let mut v: Vec<(u32, u32, &Video)> = meta
        .videos
        .iter()
        .enumerate()
        .map(|(i, x)| (x.season.unwrap_or(1), x.episode.unwrap_or(i as u32 + 1), x))
        .collect();
    // Specials (season 0) after the regular seasons, as Stremio lists them.
    v.sort_by_key(|(s, e, _)| (if *s == 0 { u32::MAX } else { *s }, *e));
    v
}

fn seasons_of(meta: &Meta) -> Vec<u32> {
    let mut s: Vec<u32> = episodes_of(meta).iter().map(|(s, _, _)| *s).collect();
    s.dedup();
    s
}

fn page_params(req: &Request) -> (usize, Option<usize>) {
    let start = req
        .q_or_header("X-Plex-Container-Start")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let size = req
        .q_or_header("X-Plex-Container-Size")
        .and_then(|v| v.parse().ok())
        .or_else(|| req.q("count").and_then(|v| v.parse().ok()));
    (start, size)
}

fn tmdb_image(path: Option<&str>) -> Option<String> {
    path.filter(|path| path.starts_with('/'))
        .map(|path| format!("https://image.tmdb.org/t/p/w300{path}"))
}

/// TMDB's `images.logos` as the row's `clearLogo`: the English logotype when TMDB has one, else
/// the first logo it lists (a wordmark in another language still beats the text fallback).
///
/// `None` when the response carried no logo, and the row is then sent with no `clearLogo` at all —
/// which is exactly what a real server sends for an item that has none, so the hero falls back to
/// the text title through its ordinary absent-logo path rather than through a second rule here.
fn tmdb_logo(images: Option<&TmdbImages>) -> Option<String> {
    let logos = &images?.logos;
    let pick = logos
        .iter()
        .find(|logo| logo.iso_639_1.as_deref() == Some("en") && logo.file_path.is_some())
        .or_else(|| logos.iter().find(|logo| logo.file_path.is_some()))?;
    pick.file_path
        .as_deref()
        .map(|path| format!("https://image.tmdb.org/t/p/w500{path}"))
}

fn tmdb_role(cast: &TmdbCast) -> Option<Value> {
    if cast.name.trim().is_empty() {
        return None;
    }
    let mut role = json!({"tag": cast.name, "role": cast.character});
    if let Some(thumb) = tmdb_image(cast.profile_path.as_deref()) {
        role["thumb"] = json!(thumb);
    }
    // `Role[].id` is the `personId` a headshot opens (`/library/people/{id}/media`), so a TMDB
    // person gets a synthetic one and the app's cast row follows it with no TMDB knowledge at all.
    // `tagKey` stays empty on purpose: that is plex.tv's global person guid, and TMDB has none.
    if cast.id > 0 && cast.id < (i64::MAX - TMDB_PERSON_KEY_BASE) as u64 {
        role["id"] = json!(TMDB_PERSON_KEY_BASE + cast.id as i64);
    }
    Some(role)
}

/// The Plex `Role` array for a TMDB cast list; left off the row when TMDB named nobody.
fn set_roles(row: &mut Value, cast: &[TmdbCast]) {
    let roles: Vec<Value> = cast.iter().filter_map(tmdb_role).take(10).collect();
    if !roles.is_empty() {
        row["Role"] = Value::Array(roles);
    }
}

fn related_hub_json(kind: &str, rows: Vec<Value>) -> Value {
    json!({
        "title": "More Like This", "type": kind, "hubIdentifier": format!("{kind}.related"),
        "size": rows.len(), "Metadata": rows
    })
}

fn tmdb_trailer_id(video: &TmdbVideo) -> Option<String> {
    (video.site.eq_ignore_ascii_case("youtube") && video.type_.eq_ignore_ascii_case("trailer"))
        .then(|| youtube_id(&Value::String(video.key.clone())))
        .flatten()
}

fn youtube_id(value: &Value) -> Option<String> {
    let raw = value.as_str().or_else(|| {
        ["ytId", "youtubeId", "source", "url", "link"]
            .iter()
            .find_map(|key| value.get(key).and_then(Value::as_str))
    })?;
    let raw = raw.trim();
    let id = if raw.contains("youtu.be/") {
        raw.split_once("youtu.be/")?
            .1
            .split(['?', '&', '/'])
            .next()?
    } else if raw.contains("youtube.com/watch?") {
        raw.split_once('?')?
            .1
            .split('&')
            .find_map(|part| part.strip_prefix("v="))?
    } else if let Some((_, rest)) = raw.split_once("youtube.com/") {
        rest.split('/').find_map(|part| {
            (part == "embed" || part == "shorts" || part == "live")
                .then(|| {
                    rest.split_once(&format!("{part}/"))
                        .map(|(_, tail)| tail.split(['?', '&', '/']).next().unwrap_or(""))
                })
                .flatten()
        })?
    } else {
        raw
    };
    ((6..=64).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'))
    .then(|| id.to_string())
}

fn trailer_extras(rk: i64, youtube_id: &str, title: &str) -> Value {
    json!({
        "size": 1,
        "Video": [{
            "ratingKey": "trailer",
            "key": format!("/library/metadata/{rk}/trailer"),
            "type": "clip",
            "title": title,
            "thumb": format!("https://img.youtube.com/vi/{youtube_id}/hqdefault.jpg")
        }]
    })
}

fn preview_from_meta(meta: &Meta, id: &str) -> MetaPreview {
    MetaPreview {
        id: id.to_string(),
        type_: meta.type_.clone(),
        name: meta.name.clone(),
        poster: meta.poster.clone(),
        background: meta.background.clone(),
        logo: meta.logo.clone(),
        description: meta.description.clone(),
        release_info: meta.release_info.clone(),
        imdb_rating: meta.imdb_rating.clone(),
        ratings: meta.ratings.clone(),
        mdb_list_ratings: meta.mdb_list_ratings.clone(),
        rotten_tomatoes_rating: meta.rotten_tomatoes_rating.clone(),
        genres: meta.genres.clone(),
    }
}

fn resolve_tmdb_movie_id(
    client: &TmdbClient,
    id: &str,
    imdb_id: Option<&str>,
) -> plx_stremio::model::Result<Option<u64>> {
    let tmdb_id = client.movie_id(id)?;
    if let Some(tmdb_id) = tmdb_id {
        return Ok(Some(tmdb_id));
    }
    match imdb_id.filter(|imdb| *imdb != id) {
        Some(imdb_id) => client.movie_id(imdb_id),
        None => Ok(None),
    }
}

fn resolve_tmdb_series_id(
    client: &TmdbClient,
    id: &str,
    imdb_id: Option<&str>,
) -> plx_stremio::model::Result<Option<u64>> {
    let tmdb_id = client.series_id(id)?;
    if let Some(tmdb_id) = tmdb_id {
        return Ok(Some(tmdb_id));
    }
    match imdb_id.filter(|imdb| *imdb != id) {
        Some(imdb_id) => client.series_id(imdb_id),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tmdb_metadata_tests {
    use super::*;

    struct TmdbMetaBackend;

    impl Backend for TmdbMetaBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> {
            Vec::new()
        }

        fn catalog_page(
            &self,
            _key: &str,
            _skip: u32,
            _genre: Option<&str>,
        ) -> Result<Vec<MetaPreview>, ()> {
            Ok(Vec::new())
        }

        fn search(&self, _query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
            Vec::new()
        }

        fn meta(&self, type_: &str, id: &str) -> Option<Meta> {
            (type_ == "movie" && id == "tmdb:603").then(|| Meta {
                id: id.into(),
                type_: type_.into(),
                name: "The Matrix".into(),
                poster: Some("https://image.tmdb.org/t/p/w300/matrix.jpg".into()),
                ..Default::default()
            })
        }

        fn streams(
            &self,
            _type_: &str,
            _id: &str,
            _deadline: Duration,
            _on_batch: &mut dyn FnMut(&[Stream]),
        ) -> Vec<Stream> {
            Vec::new()
        }

        fn subtitles(&self, _type_: &str, _id: &str) -> Vec<plx_stremio::model::Subtitle> {
            Vec::new()
        }
    }

    #[test]
    fn tmdb_cast_and_trailer_shapes_use_plex_fields() {
        let cast = TmdbCast {
            id: 638,
            name: "Actor Name".into(),
            character: "Character Name".into(),
            profile_path: Some("/actor.jpg".into()),
        };
        assert_eq!(
            tmdb_role(&cast).unwrap(),
            json!({
                // the synthetic `personId` a headshot opens — `/library/people/3000000638/media`
                "id": 3_000_000_638i64,
                "tag": "Actor Name",
                "role": "Character Name",
                "thumb": "https://image.tmdb.org/t/p/w300/actor.jpg"
            })
        );
        // A cast row TMDB sent no person id for opens nothing, rather than the bare base key.
        let anonymous = TmdbCast {
            name: "Nobody".into(),
            ..Default::default()
        };
        assert!(tmdb_role(&anonymous).unwrap().get("id").is_none());
        assert_eq!(
            trailer_extras(42, "youtube123", "Official Trailer"),
            json!({
                "size": 1,
                "Video": [{
                    "ratingKey": "trailer",
                    "key": "/library/metadata/42/trailer",
                    "type": "clip",
                    "title": "Official Trailer",
                    "thumb": "https://img.youtube.com/vi/youtube123/hqdefault.jpg"
                }]
            })
        );
    }

    #[test]
    fn youtube_ids_are_extracted_from_cinemeta_shapes() {
        assert_eq!(
            youtube_id(&json!({"ytId": "youtube123"})).as_deref(),
            Some("youtube123")
        );
        assert_eq!(
            youtube_id(&json!("https://youtu.be/youtube123?feature=share")).as_deref(),
            Some("youtube123")
        );
        assert_eq!(
            youtube_id(&json!("https://www.youtube.com/watch?v=youtube123&t=3")).as_deref(),
            Some("youtube123")
        );
        assert_eq!(youtube_id(&json!("not a youtube id")), None);
    }

    #[test]
    fn related_movie_preview_uses_stremio_id_and_metadata() {
        let metadata = Meta {
            id: "stale-id".into(),
            type_: "movie".into(),
            name: "Spider-Man: No Way Home".into(),
            poster: Some("https://images.example/poster.jpg".into()),
            background: Some("https://images.example/backdrop.jpg".into()),
            release_info: Some("2021-12-15".into()),
            ..Default::default()
        };
        let preview = preview_from_meta(&metadata, "tmdb:634649");
        assert_eq!(preview.id, "tmdb:634649");
        assert_eq!(preview.name, "Spider-Man: No Way Home");
        assert_eq!(
            preview.poster.as_deref(),
            Some("https://images.example/poster.jpg")
        );
        assert_eq!(
            preview.background.as_deref(),
            Some("https://images.example/backdrop.jpg")
        );
        assert_eq!(preview.release_info.as_deref(), Some("2021-12-15"));
    }

    /// The switch gates the engine, not just the Settings row: `torrent_server` is what
    /// `shim_media::rank`/`playable_url` consult, so `None` is what drops every torrent stream.
    #[test]
    fn p2p_off_hides_the_streaming_server_and_clearing_the_url_takes_effect() {
        let shim = Shim::new(Arc::new(TmdbMetaBackend), Config { torrent_server: Some("http://127.0.0.1:2710".into()), ..Config::default() });
        assert_eq!(shim.torrent_server(), None, "P2P is off until Settings says otherwise");
        shim.set_p2p(true, None);
        assert_eq!(shim.torrent_server(), None, "no URL configured: the app probes the default ports");
        shim.set_p2p(true, Some("  http://192.168.1.9:2710  ".into()));
        assert_eq!(shim.torrent_server().as_deref(), Some("http://192.168.1.9:2710"));
        shim.set_p2p(true, None);
        assert_eq!(shim.torrent_server(), None, "clearing the field is not ignored");
        shim.set_p2p(false, Some("http://192.168.1.9:2710".into()));
        assert_eq!(shim.torrent_server(), None, "the switch outranks a configured URL");
    }

    #[test]
    fn tmdb_related_rating_key_resolves_to_full_metadata() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        let movie = plx_stremio::addons_tmdb::TmdbMovie {
            id: 603,
            title: "The Matrix".into(),
            poster_path: Some("/matrix.jpg".into()),
            ..Default::default()
        };
        let related = shim.tmdb_recommendation_row(&movie);
        let rating_key = related["ratingKey"].as_str().unwrap();
        assert!(rating_key.parse::<i64>().is_ok());
        assert_eq!(related["key"], format!("/library/metadata/{rating_key}"));

        let response = shim.handle(&Request {
            method: "GET".into(),
            path: related["key"].as_str().unwrap().into(),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: Vec::new(),
        });
        assert_eq!(response.status, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(
            body["MediaContainer"]["Metadata"][0]["ratingKey"],
            rating_key
        );
        assert_eq!(body["MediaContainer"]["Metadata"][0]["title"], "The Matrix");

        let fallback = shim.tmdb_recommendation_row(&plx_stremio::addons_tmdb::TmdbMovie {
            id: 999,
            title: "Fallback Movie".into(),
            ..Default::default()
        });
        assert!(fallback["ratingKey"]
            .as_str()
            .unwrap()
            .parse::<i64>()
            .is_ok());
        assert_eq!(fallback["ratingKey"], "1000000999");

        let details = TmdbMovieDetails {
            movie: TmdbMovie {
                id: 999,
                title: "Fallback Movie".into(),
                overview: Some("TMDB summary".into()),
                backdrop_path: Some("/backdrop.jpg".into()),
                release_date: Some("2020-06-15".into()),
                runtime: Some(123),
                genres: vec![plx_stremio::addons_tmdb::TmdbGenre {
                    id: 18,
                    name: "Drama".into(),
                }],
                ..Default::default()
            },
            credits: plx_stremio::addons_tmdb::TmdbCredits {
                cast: vec![TmdbCast {
                    id: 638,
                    name: "Actor Name".into(),
                    character: "Character Name".into(),
                    profile_path: Some("/actor.jpg".into()),
                }],
            },
            ..Default::default()
        };
        let row = shim.tmdb_details_row(&details, 1_000_000_999);
        assert_eq!(row["ratingKey"], "1000000999");
        assert_eq!(row["Role"][0]["tag"], "Actor Name");
        assert_eq!(row["Role"][0]["role"], "Character Name");
        assert_eq!(row["Role"][0]["id"], 3_000_000_638i64);
        assert_eq!(row["Genre"][0]["tag"], "Drama");
        assert_eq!(row["art"], "https://image.tmdb.org/t/p/w300/backdrop.jpg");
        assert_eq!(row["duration"], 7_380_000);
        assert_eq!(row["summary"], "TMDB summary");
        assert_eq!(row["year"], 2020);
        assert_eq!(row["originallyAvailableAt"], "2020-06-15");
    }

    #[test]
    fn synthetic_related_rating_keys_decode_to_tmdb_ids() {
        assert_eq!(synthetic_tmdb_id(1_000_000_603), Some(603));
        assert_eq!(synthetic_tmdb_id(999_999_999), None);
        // A show key must never decode as a movie id, or the two bases collide.
        assert_eq!(synthetic_tmdb_id(2_000_000_603), None);
        assert_eq!(synthetic_show_tmdb_id(2_000_000_603), Some(603));
        assert_eq!(synthetic_show_tmdb_id(1_999_999_999), None);
        assert_eq!(
            translate_to_stremio_id(1_000_000_603).as_deref(),
            Some("tmdb:603")
        );
        assert_eq!(
            translate_to_stremio_id(2_000_000_603).as_deref(),
            Some("tmdb:603")
        );
        assert_eq!(translate_to_stremio_id(12_345), None);
        // A six-figure TMDB movie id stays inside the movie range (it used to cross the show base).
        assert_eq!(synthetic_tmdb_id(1_000_141_052), Some(141_052));
        assert_eq!(synthetic_show_tmdb_id(1_000_141_052), None);
    }

    /// The three synthetic ranges are disjoint, and a person key decodes as NOTHING but a person —
    /// otherwise a headshot would open a title a billion ids away.
    #[test]
    fn synthetic_person_keys_decode_only_as_people() {
        assert_eq!(synthetic_person_id(3_000_000_638), Some(638));
        assert_eq!(synthetic_person_id(2_999_999_999), None);
        assert_eq!(synthetic_show_tmdb_id(3_000_000_638), None);
        assert_eq!(synthetic_tmdb_id(3_000_000_638), None);
        assert_eq!(translate_to_stremio_id(3_000_000_638), None);
        // A collection key is above the person base, and decodes as neither.
        assert_eq!(synthetic_collection_id(4_000_000_234), Some(234));
        assert_eq!(synthetic_person_id(4_000_000_234), None);
        assert_eq!(synthetic_collection_id(3_000_000_638), None);
    }

    /// A member film's franchise shelf: headed by the franchise's name, carrying the OTHER films as
    /// ordinary recommendation keys (so they open and play), and naming the collection by `tagId`
    /// in the shape the app splits a collection shelf out of `/related` by.
    #[test]
    fn a_member_film_gets_a_collection_hub_of_the_other_films() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        let parts = plx_stremio::addons_tmdb::TmdbCollection {
            id: 234,
            name: "The Dark Knight Collection".into(),
            overview: Some("A franchise.".into()),
            parts: vec![
                TmdbMovie { id: 155, title: "The Dark Knight".into(), ..Default::default() },
                TmdbMovie { id: 272, title: "Batman Begins".into(), ..Default::default() },
                TmdbMovie { id: 49026, title: "The Dark Knight Rises".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        let hub = shim
            .collection_hub_json(&parts, 272, "Named by the member row")
            .expect("a franchise with other films gets a shelf");
        assert_eq!(hub["title"], "The Dark Knight Collection");
        assert_eq!(hub["hubIdentifier"], "collection.related.1.1");
        assert_eq!(hub["key"], "/library/sections/1/all?type=1&tagId=234");
        assert_eq!(hub["totalSize"], 3);
        let rows = hub["Metadata"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "the page's own film is not in its own shelf");
        assert_eq!(rows[0]["title"], "The Dark Knight");
        assert_eq!(rows[1]["title"], "The Dark Knight Rises");
        for row in rows {
            let rk: i64 = row["ratingKey"].as_str().unwrap().parse().unwrap();
            assert_eq!(row["key"], format!("/library/metadata/{rk}"));
            // A playable key: the movie's TMDB id behind the synthetic base.
            assert!(synthetic_tmdb_id(rk).is_some());
        }

        // A film that is the franchise's only member has no shelf: one poster is not a shelf.
        let alone = plx_stremio::addons_tmdb::TmdbCollection {
            parts: vec![TmdbMovie { id: 272, title: "Batman Begins".into(), ..Default::default() }],
            ..parts.clone()
        };
        assert!(shim.collection_hub_json(&alone, 272, "Solo").is_none());
    }

    /// The franchise's own page: the section listing resolves the member's `tagId`, the header
    /// answers its synthetic key, and the children are its films.
    #[test]
    fn a_collection_page_resolves_from_the_section_listing_to_its_films() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        shim.remember_collection(&plx_stremio::addons_tmdb::TmdbCollection {
            id: 234,
            name: "The Dark Knight Collection".into(),
            parts: vec![
                TmdbMovie { id: 155, title: "The Dark Knight".into(), ..Default::default() },
                TmdbMovie { id: 272, title: "Batman Begins".into(), ..Default::default() },
            ],
            ..Default::default()
        });
        let get = |path: &str| {
            let body: Value = serde_json::from_slice(
                &shim
                    .handle(&Request {
                        method: "GET".into(),
                        path: path.into(),
                        query: HashMap::new(),
                        headers: HashMap::new(),
                        body: Vec::new(),
                    })
                    .body,
            )
            .unwrap();
            body["MediaContainer"].clone()
        };
        let listing = get("/library/sections/1/collections");
        assert_eq!(listing["Metadata"][0]["index"], 234);
        assert_eq!(listing["Metadata"][0]["title"], "The Dark Knight Collection");

        let header = get("/library/metadata/4000000234");
        assert_eq!(header["Metadata"][0]["title"], "The Dark Knight Collection");
        assert_eq!(header["Metadata"][0]["childCount"], 2);

        let children = get("/library/collections/4000000234/children");
        assert_eq!(children["totalSize"], 2);
        assert_eq!(children["Metadata"][0]["title"], "The Dark Knight");
        assert_eq!(children["Metadata"][1]["title"], "Batman Begins");
    }

    /// A headshot's filmography: the kind is read per ROW (`combined_credits` merges both), the
    /// person key is stamped onto each so the page can caption the tile, and it is newest first.
    #[test]
    fn person_credits_split_by_each_rows_own_kind() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        let credits = vec![
            TmdbCredit {
                id: 603,
                media_type: "movie".into(),
                title: "The Matrix".into(),
                release_date: Some("1999-03-31".into()),
                character: "Neo".into(),
                poster_path: Some("/matrix.jpg".into()),
                ..Default::default()
            },
            TmdbCredit {
                id: 1396,
                media_type: "tv".into(),
                name: "Breaking Bad".into(),
                first_air_date: Some("2008-01-20".into()),
                character: "Walter White".into(),
                ..Default::default()
            },
            // Neither a film nor a show: no shelf would draw it, so it is dropped.
            TmdbCredit {
                id: 5,
                media_type: "person".into(),
                ..Default::default()
            },
        ];
        let rows = shim.person_credit_rows(638, &credits);
        assert_eq!(rows.len(), 2);
        // Newest first, so the 2008 show leads the 1999 film.
        assert_eq!(rows[0]["ratingKey"], "2000001396");
        assert_eq!(rows[0]["type"], "show");
        assert_eq!(rows[0]["title"], "Breaking Bad");
        assert_eq!(rows[1]["ratingKey"], "1000000603");
        assert_eq!(rows[1]["type"], "movie");
        // The character rides on the CREDIT, keyed by the person the headshot opened.
        assert_eq!(rows[0]["Role"][0]["id"], 3_000_000_638i64);
        assert_eq!(rows[0]["Role"][0]["role"], "Walter White");
        assert_eq!(rows[1]["Role"][0]["role"], "Neo");
    }

    /// A person id the shim does not know is an EMPTY container, not an error: the person page must
    /// settle on "nothing here" rather than retry a fetch that can never succeed.
    #[test]
    fn an_unknown_person_id_answers_an_empty_container() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        for id in ["123", "2999999999", "not-a-number"] {
            let response = shim.handle(&Request {
                method: "GET".into(),
                path: format!("/library/people/{id}/media"),
                query: HashMap::new(),
                headers: HashMap::new(),
                body: Vec::new(),
            });
            assert_eq!(response.status, 200, "id {id}");
            let body: Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(body["MediaContainer"]["Metadata"], json!([]), "id {id}");
        }
    }

    #[test]
    fn tmdb_show_rows_use_plex_show_fields() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        let show = TmdbShow {
            id: 1396,
            name: "Breaking Bad".into(),
            poster_path: Some("/poster.jpg".into()),
            first_air_date: Some("2008-01-20".into()),
            ..Default::default()
        };
        let fallback = shim.tmdb_show_recommendation_row(&show);
        assert_eq!(fallback["ratingKey"], "2000001396");
        assert_eq!(fallback["type"], "show");
        assert_eq!(fallback["title"], "Breaking Bad");
        assert_eq!(fallback["year"], 2008);
        assert_eq!(
            fallback["thumb"],
            "https://image.tmdb.org/t/p/w300/poster.jpg"
        );

        let details = TmdbShowDetails {
            show: TmdbShow {
                overview: Some("TMDB summary".into()),
                backdrop_path: Some("/backdrop.jpg".into()),
                genres: vec![plx_stremio::addons_tmdb::TmdbGenre {
                    id: 18,
                    name: "Drama".into(),
                }],
                ..show
            },
            credits: plx_stremio::addons_tmdb::TmdbCredits {
                cast: vec![TmdbCast {
                    id: 638,
                    name: "Actor Name".into(),
                    character: "Character Name".into(),
                    profile_path: Some("/actor.jpg".into()),
                }],
            },
            ..Default::default()
        };
        let row = shim.tmdb_show_details_row(&details, 2_000_001_396);
        assert_eq!(row["ratingKey"], "2000001396");
        assert_eq!(row["key"], "/library/metadata/2000001396");
        assert_eq!(row["type"], "show");
        assert_eq!(row["Role"][0]["tag"], "Actor Name");
        assert_eq!(row["Role"][0]["role"], "Character Name");
        assert_eq!(row["Genre"][0]["tag"], "Drama");
        assert_eq!(row["duration"], 2_700_000);
        assert_eq!(row["summary"], "TMDB summary");
        assert_eq!(row["originallyAvailableAt"], "2008-01-20");
        assert_eq!(row["art"], "https://image.tmdb.org/t/p/w300/backdrop.jpg");
    }

    #[derive(Default)]
    struct RecordingBackend {
        streams: Mutex<Vec<(String, String)>>,
        subtitles: Mutex<Vec<(String, String)>>,
    }

    impl Backend for RecordingBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> {
            Vec::new()
        }
        fn catalog_page(
            &self,
            _key: &str,
            _skip: u32,
            _genre: Option<&str>,
        ) -> Result<Vec<MetaPreview>, ()> {
            Ok(Vec::new())
        }
        fn search(&self, _query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
            Vec::new()
        }
        fn meta(&self, _type_: &str, _id: &str) -> Option<Meta> {
            None
        }
        fn streams(
            &self,
            type_: &str,
            id: &str,
            _deadline: Duration,
            _on_batch: &mut dyn FnMut(&[Stream]),
        ) -> Vec<Stream> {
            self.streams
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((type_.into(), id.into()));
            Vec::new()
        }
        fn subtitles(&self, type_: &str, id: &str) -> Vec<plx_stremio::model::Subtitle> {
            self.subtitles
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((type_.into(), id.into()));
            Vec::new()
        }
    }

    #[test]
    fn synthetic_rating_keys_reach_addons_as_tmdb_ids() {
        let backend = Arc::new(RecordingBackend::default());
        let shim = Arc::new(Shim::new(backend.clone(), Config::default()));
        let get = || Request {
            method: "GET".into(),
            path: "/nuvio/streams".into(),
            query: HashMap::from([("ratingKey".into(), "1000000603".into())]),
            headers: HashMap::new(),
            body: Vec::new(),
        };
        let body = |r: &Response| serde_json::from_slice::<Value>(&r.body).unwrap();
        assert_eq!(shim.handle(&get()).status, 200);
        // The slot starts its stream and subtitle workers as threads: wait for them to report.
        for _ in 0..100 {
            if !backend.streams.lock().unwrap().is_empty() && !backend.subtitles.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            backend.streams.lock().unwrap().as_slice(),
            [("movie".to_string(), "tmdb:603".to_string())]
        );
        assert_eq!(
            backend.subtitles.lock().unwrap().as_slice(),
            [("movie".to_string(), "tmdb:603".to_string())]
        );
        // A movie has no streams from this backend, but the list is finished: the picker must stop
        // asking rather than poll to its retry limit.
        assert_eq!(body(&shim.handle(&get()))["complete"], true);
    }

    #[test]
    fn a_related_show_answers_complete_without_waiting_for_a_stream() {
        let backend = Arc::new(RecordingBackend::default());
        let shim = Arc::new(Shim::new(backend.clone(), Config::default()));
        let response = shim.handle(&Request {
            method: "GET".into(),
            path: "/nuvio/streams".into(),
            query: HashMap::from([("ratingKey".into(), "2000001396".into())]),
            headers: HashMap::new(),
            body: Vec::new(),
        });
        assert_eq!(response.status, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        // A series has no stream id until an episode is chosen: the answer is an empty FINISHED
        // list, not `complete: false` forever (which made the Source Picker spin).
        assert_eq!(body["complete"], true);
        assert_eq!(body["streams"], json!([]));
        assert!(backend.streams.lock().unwrap().is_empty());
    }
}

/// `.../library/metadata/123` or `123` → 123.
fn trailing_key(s: &str) -> Option<i64> {
    s.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

impl Shim {
    pub fn new(backend: Arc<dyn Backend>, cfg: Config) -> Shim {
        let dir = cfg.data_dir.clone();
        let cfg_ts = cfg.torrent_server.clone();
        let cfg_p2p = cfg.p2p_enabled;
        let cfg_token = cfg.token.clone();
        Shim {
            backend,
            keys: Keys::open(dir.as_deref()),
            watch: WatchState::open(dir.as_deref()),
            watch_pushes: Mutex::new(HashMap::new()),
            cfg,
            metas: Mutex::new(HashMap::new()),
            catalogs: Mutex::new(HashMap::new()),
            catalog_list: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
            sub_urls: Mutex::new(HashMap::new()),
            poster_fallbacks: Mutex::new(HashMap::new()),
            tmdb_fallbacks: Mutex::new(HashMap::new()),
            collections: Mutex::new(HashMap::new()),
            credit_rows: Mutex::new(Vec::new()),
            next_sub: AtomicI64::new(900_000_000),
            permits: (Mutex::new(CATALOG_PARALLEL), Condvar::new()),
            probes: crate::shim_probe::ProbeCache::default(),
            resolved: Mutex::new(HashMap::new()),
            torrent_server: std::sync::RwLock::new(cfg_ts),
            p2p_enabled: AtomicBool::new(cfg_p2p),
            audio_codecs: std::sync::RwLock::new(Vec::new()),
            me: std::sync::OnceLock::new(),
            account: crate::shim_account::Account::new(dir.as_deref(), &cfg_token),
        }
    }

    pub fn bind_self(self: &Arc<Self>) {
        let _ = self.me.set(Arc::downgrade(self));
    }

    // ---- account watch progress ------------------------------------------------------------

    /// The identity Nuvio keys a resume point by, for a local ratingKey: `content_id` is the TMDB
    /// id the account's watch history uses (`tmdb:550`), `video_id` the film's own id or the
    /// episode's addon id. `None` for an item that has no single position to report — a season, a
    /// whole series, or a title whose TMDB id cannot be resolved.
    fn nuvio_watch_identity(&self, rk: i64) -> Option<(String, String, String)> {
        let ident = self.keys.ident(rk).or_else(|| synthetic_ident(rk))?;
        match ident {
            // A series row has no position of its own; its episodes do.
            Ident::Title { type_, .. } if is_series(&type_) => None,
            Ident::Title { id, .. } => {
                let content_id = format!("tmdb:{}", tmdb_id(&id, false)?);
                Some((content_id.clone(), "movie".into(), content_id))
            }
            Ident::Episode { series, video } => {
                Some((format!("tmdb:{}", tmdb_id(&series, true)?), "series".into(), video))
            }
            Ident::Season { .. } => None,
        }
    }

    /// Mirror one item's resume point to the account. A paused or stopped report is a real change
    /// and goes at once; a playing one at most once per [`WATCH_PUSH_INTERVAL`].
    fn push_watch_progress(&self, rk: i64, offset_ms: i64, duration_ms: i64, state: &str) {
        if !self.account.signed_in() {
            return;
        }
        let mut pushes = self.watch_pushes.lock().unwrap_or_else(|e| e.into_inner());
        let settled = matches!(state, "paused" | "stopped");
        if !watch_push_due(pushes.get(&rk).copied(), settled, Instant::now()) {
            return;
        }
        if pushes.len() >= 512 {
            pushes.clear();
        }
        pushes.insert(rk, Instant::now());
        drop(pushes);
        let Some((content_id, content_type, video_id)) = self.nuvio_watch_identity(rk) else { return };
        self.watch.bind_account_key(&content_id, &video_id, rk);
        let entry = plx_stremio::nuvio::WatchProgress {
            content_id,
            content_type,
            video_id,
            position: offset_ms.max(0),
            duration: duration_ms,
            last_watched: now_secs() * 1000,
        };
        match self.account.push_watch_progress(&[entry]) {
            Ok(()) => {}
            Err(error) => {
                let status = match error { plx_stremio::model::Error::Status(s) => s, _ => 0 };
                plx_stremio::nuvio::log_account_event(&format!("nuvio account: progress push failed status={status}"));
            }
        }
    }

    /// Pull the account's resume points and merge them into the local watch state, so Continue
    /// Watching draws what another device played. Runs off the caller's thread.
    pub fn pull_account_watch(&self) {
        let Some(me) = self.me.get().and_then(|w| w.upgrade()) else { return };
        let _ = std::thread::Builder::new().name("pmsshim-watch".into()).spawn(move || {
            let Ok(entries) = me.account.pull_watch_progress() else { return };
            let mut merged = 0;
            for e in entries {
                // Cheap rejection first: the row snapshot is a meta fetch, so never pay for an
                // entry the local state already beats.
                if e.position <= 0 {
                    continue;
                }
                let stamp = e.last_watched / 1000;
                let Some(rk) = me.account_watch_key(&e) else { continue };
                if me.watch.get(rk).last_viewed_at > stamp {
                    continue;
                }
                if me.watch.merge_remote(rk, e.position, e.duration, stamp, me.row(rk)) {
                    me.watch.bind_account_key(&e.content_id, &e.video_id, rk);
                    merged += 1;
                }
            }
            if merged > 0 {
                plx_stremio::nuvio::log_account_event(&format!("nuvio account: merged {merged} resume point(s)"));
            }
        });
    }

    /// The local ratingKey an account resume point belongs to: what this device pushed that item
    /// under, else the key its identity names. Does not bind on a miss — a pull that loses to the
    /// local state must not grow the map.
    fn account_watch_key(&self, e: &plx_stremio::nuvio::WatchProgress) -> Option<i64> {
        if let Some(key) = self.watch.account_key(&e.content_id, &e.video_id) {
            return Some(key);
        }
        let ident = if e.content_type == "series" {
            Ident::Episode { series: episode_series_id(&e.video_id, &e.content_id).to_string(), video: e.video_id.clone() }
        } else {
            Ident::Title { type_: "movie".into(), id: e.content_id.clone() }
        };
        Some(self.keys.key_of(&ident))
    }

    /// The Stremio-compatible streaming server torrents play through, if any. `None` whenever P2P
    /// is off, which is what makes [`shim_media::rank`] and `playable_url` drop every torrent and
    /// magnet stream: an unplayable pick is not offered.
    pub fn torrent_server(&self) -> Option<String> {
        if !self.p2p_enabled() {
            return None;
        }
        self.torrent_server
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Is peer-to-peer streaming on (Settings > Playback > P2P Streaming)?
    pub fn p2p_enabled(&self) -> bool {
        self.p2p_enabled.load(Ordering::Relaxed)
    }

    /// Apply the session's P2P preference: the switch, and the engine's base URL (`None` = probe
    /// the default local ports). The URL is assigned, not merged, so clearing the field in
    /// Settings takes effect without a relaunch.
    pub fn set_p2p(&self, enabled: bool, server: Option<String>) {
        self.p2p_enabled.store(enabled, Ordering::Relaxed);
        *self.torrent_server.write().unwrap_or_else(|e| e.into_inner()) =
            server.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
        // The lists that were ranked under the old policy no longer answer it.
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// The audio codecs this TV decodes: a stream whose label names another ranks after the
    /// playable ones of the same resolution.
    pub fn set_audio_codecs(&self, codecs: Vec<String>) {
        *self.audio_codecs.write().unwrap_or_else(|e| e.into_inner()) = codecs;
    }

    pub fn set_torrent_server(&self, server: Option<String>) {
        *self
            .torrent_server
            .write()
            .unwrap_or_else(|e| e.into_inner()) = server;
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Drop every cache that depends on the addon set (after Settings changed it).
    pub fn addons_changed(&self) {
        self.metas.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.poster_fallbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self.catalog_list.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    // ---- data access ------------------------------------------------------------------------

    fn catalog_list(&self) -> Vec<CatalogInfo> {
        let mut g = self.catalog_list.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = g.as_ref() {
            if at.elapsed() < CATALOG_TTL {
                return v.clone();
            }
        }
        let v = self.backend.catalogs();
        *g = Some((Instant::now(), v.clone()));
        v
    }

    fn first_catalog(&self, section: i64) -> Option<CatalogInfo> {
        self.catalog_list().into_iter().find(|c| {
            if section == SECTION_SHOWS {
                is_series(&c.type_)
            } else {
                c.type_ == "movie"
            }
        })
    }

    /// Rows `[start, start+size)` of a catalog and the total to report.
    fn catalog_range(
        &self,
        key: &str,
        genre: Option<&str>,
        start: usize,
        size: usize,
    ) -> (Vec<MetaPreview>, i64) {
        let entry = {
            let mut g = self.catalogs.lock().unwrap_or_else(|e| e.into_inner());
            let k = (key.to_string(), genre.map(str::to_string));
            let fresh = g.get(&k).is_some_and(|c| {
                c.lock().unwrap_or_else(|e| e.into_inner()).at.elapsed() < CATALOG_TTL
            });
            if !fresh {
                g.insert(
                    k.clone(),
                    Arc::new(Mutex::new(CatalogCache {
                        at: Instant::now(),
                        rows: Vec::new(),
                        exhausted: false,
                    })),
                );
            }
            g[&k].clone()
        };
        // One fetcher per catalog at a time: the lock is held across the page fetch on purpose.
        let mut c = entry.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = 0;
        while c.rows.len() < start + size && !c.exhausted && guard < 20 {
            guard += 1;
            match self.backend.catalog_page(key, c.rows.len() as u32, genre) {
                Ok(page) => {
                    let before = c.rows.len();
                    for p in page {
                        if !c.rows.iter().any(|r| r.id == p.id) {
                            c.rows.push(p);
                        }
                    }
                    if c.rows.len() == before {
                        c.exhausted = true;
                    }
                }
                Err(()) => break,
            }
        }
        let rows = c.rows.iter().skip(start).take(size).cloned().collect();
        let total = c.rows.len() as i64 + if c.exhausted { 0 } else { MORE_HINT };
        (rows, total)
    }

    fn meta(&self, type_: &str, id: &str) -> Option<Arc<Meta>> {
        let k = (type_.to_string(), id.to_string());
        if let Some((at, m)) = self.metas.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
            if at.elapsed() < META_TTL {
                return Some(m.clone());
            }
        }
        let m = Arc::new(self.backend.meta(type_, id)?);
        let mut g = self.metas.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() > 400 {
            g.clear();
        }
        g.insert(k, (Instant::now(), m.clone()));
        Some(m)
    }

    /// The (type, id) an item's streams are asked for, and its runtime.
    fn stream_target(&self, ident: &Ident) -> Option<(String, String)> {
        match ident {
            Ident::Title { type_, id } if !is_series(type_) => Some((type_.clone(), id.clone())),
            Ident::Episode { video, .. } => Some(("series".into(), video.clone())),
            _ => None,
        }
    }

    fn slot(&self, rk: i64, ident: &Ident) -> Option<Arc<StreamSlot>> {
        let (type_, id) = self.stream_target(ident)?;
        let translated_id = match translate_to_stremio_id(rk) {
            // An IMDb id is preferred where TMDB can name one (some addons rank by it), but it is
            // an improvement, never a prerequisite: with no TMDB key, or no `/find` match, the
            // `tmdb:` id is still what the addon knows the title by, and asking for nothing at all
            // would answer an empty, permanently-"loading" Source Picker.
            Some(tmdb_id) => TmdbClient::from_env()
                .and_then(|client| resolve_imdb_id(rk, &client))
                .unwrap_or(tmdb_id),
            None => id,
        };
        let mut g = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.get(&rk) {
            if s.started.elapsed() < STREAM_TTL {
                return Some(s.clone());
            }
        }
        if g.len() > 200 {
            g.retain(|_, s| s.started.elapsed() < STREAM_TTL);
        }
        let slot = Arc::new(StreamSlot {
            started: Instant::now(),
            state: Mutex::new(SlotState::default()),
            cv: Condvar::new(),
        });
        g.insert(rk, slot.clone());
        drop(g);
        let (backend, s2, deadline) =
            (self.backend.clone(), slot.clone(), self.cfg.stream_deadline);
        let subs_backend = self.backend.clone();
        let (st, sid) = (type_.clone(), translated_id.clone());
        // Subtitles ride beside the streams, never in front of them.
        let slot_subs = slot.clone();
        let started = std::thread::Builder::new()
            .name("pmsshim-streams".into())
            .spawn(move || {
                let mut on_batch = |b: &[Stream]| {
                    let mut st = s2.state.lock().unwrap_or_else(|e| e.into_inner());
                    let room = (PART_STRIDE as usize - 1).saturating_sub(st.streams.len());
                    st.streams.extend(b.iter().take(room).cloned());
                    s2.cv.notify_all();
                };
                let all = backend.streams(&type_, &translated_id, deadline, &mut on_batch);
                let mut st = s2.state.lock().unwrap_or_else(|e| e.into_inner());
                // A backend that only returns at the end (no batches) still lands here.
                if st.streams.is_empty() && !all.is_empty() {
                    st.streams = all.into_iter().take(PART_STRIDE as usize - 1).collect();
                }
                st.done = true;
                s2.cv.notify_all();
            })
            .is_ok();
        // A slot whose thread never started must still report `done`, or the Source Picker polls
        // to its retry limit and shows "loading" forever.
        if !started {
            let mut st = slot.state.lock().unwrap_or_else(|e| e.into_inner());
            st.done = true;
            slot.cv.notify_all();
        }
        let _ = std::thread::Builder::new()
            .name("pmsshim-subs".into())
            .spawn(move || {
                let subs = subs_backend.subtitles(&st, &sid);
                let mut g = slot_subs.state.lock().unwrap_or_else(|e| e.into_inner());
                // Ids are assigned by the shim on first read (see `ranked`), so only stash here.
                g.subs = subs
                    .into_iter()
                    .take(40)
                    .map(|s| SubRef {
                        id: 0,
                        lang: format!("{}\u{0}{}", s.lang, s.url),
                        selected: false,
                    })
                    .collect();
            });
        Some(slot)
    }

    /// The ranked playable streams of an item (waiting per the deadline/grace rules) and the
    /// subtitles known so far. Each entry carries its arrival index.
    ///
    /// **`chosen` ends the wait once a stream has been picked.** `/nuvio/select` records the
    /// viewer's file, and from that moment no addon still answering can change which one plays —
    /// so the resolve that follows a pick must not sit out the grace window (nor, for a picked
    /// stream the local playable test does not recognise, the whole deadline) waiting for a list
    /// it will not use. That wait is what made a pick made during progressive loading look like it
    /// had to wait for every addon to finish. The DETAIL page's own fetch passes `false`: its list
    /// keeps filling, and the picker polls it until the addons are done.
    fn ranked(
        &self,
        rk: i64,
        ident: &Ident,
        runtime_secs: Option<u32>,
        max_wait: Duration,
        chosen: bool,
    ) -> (Vec<(usize, Classified)>, Vec<SubRef>, bool) {
        let Some(slot) = self.slot(rk, ident) else {
            return (Vec::new(), Vec::new(), true);
        };
        let deadline = self
            .cfg
            .stream_deadline
            .min(slot.started.elapsed() + max_wait);
        let mut st = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        let ts_owned = self.torrent_server();
        let ts = ts_owned.as_deref();
        let mut first_playable: Option<Instant> = None;
        loop {
            if st.done {
                break;
            }
            if chosen && st.selected.is_some_and(|i| i < st.streams.len()) {
                break;
            }
            let elapsed = slot.started.elapsed();
            if elapsed >= deadline {
                break;
            }
            if first_playable.is_none()
                && st
                    .streams
                    .iter()
                    .any(|s| shim_media::playable_url(s, ts).is_some())
            {
                first_playable = Some(Instant::now());
            }
            let mut wait = deadline - elapsed;
            if let Some(t) = first_playable {
                let g = self.cfg.stream_grace.saturating_sub(t.elapsed());
                if g.is_zero() {
                    break;
                }
                wait = wait.min(g);
            }
            st = slot
                .cv
                .wait_timeout(st, wait.min(Duration::from_millis(250)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        // Assign subtitle ids once, registering their URLs for `/library/streams/{id}`.
        for s in st.subs.iter_mut().filter(|s| s.id == 0) {
            let id = self.next_sub.fetch_add(1, Ordering::Relaxed);
            if let Some((lang, url)) = s.lang.split_once('\u{0}') {
                self.sub_urls
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, url.to_string());
                s.lang = lang.to_string();
            }
            s.id = id;
        }
        // The viewer's account language decides which sidecar starts ON, once per slot: the
        // tracks are cloned below for every Part, so the mark has to live on the slot's own list.
        let pref = self.account.subtitle_language();
        shim_media::mark_preferred_subtitle(&mut st.subs, pref.as_deref());
        let subs: Vec<SubRef> = st
            .subs
            .iter()
            .map(|s| SubRef {
                id: s.id,
                lang: s.lang.clone(),
                selected: s.selected,
            })
            .collect();
        let indexed: Vec<Stream> = st.streams.clone();
        let selected = st.selected;
        let done = st.done;
        drop(st);
        let mut out: Vec<(usize, Classified)> = Vec::new();
        let audio = self
            .audio_codecs
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for c in shim_media::rank(&indexed, runtime_secs, ts, &audio) {
            // `rank` clones; recover the arrival index by identity of the URL + name, which the
            // addon tag completes — two addons can offer the same file under the same label, and
            // the index is what `/nuvio/select` and the picker's `addon_name` group address.
            let i = indexed
                .iter()
                .position(|s| {
                    shim_media::playable_url(s, ts).as_deref() == Some(c.url.as_str())
                        && s.name == c.stream.name
                        && s.addon_name == c.stream.addon_name
                })
                .unwrap_or(0);
            out.push((i, c));
        }
        if let Some(sel) = selected {
            if let Some(p) = out.iter().position(|(i, _)| *i == sel) {
                let x = out.remove(p);
                out.insert(0, x);
            }
        }
        (out, subs, done)
    }

    // ---- rows -------------------------------------------------------------------------------

    fn watch_fields(&self, row: &mut Value, rk: i64) {
        let w: Watch = self.watch.get(rk);
        if w.offset_ms > 0 {
            row["viewOffset"] = json!(w.offset_ms);
        }
        if w.view_count > 0 {
            row["viewCount"] = json!(w.view_count);
        }
        if w.last_viewed_at > 0 {
            row["lastViewedAt"] = json!(w.last_viewed_at);
        }
    }

    fn preview_row(&self, p: &MetaPreview) -> Value {
        let rk = self.keys.key_of(&Ident::Title {
            type_: p.type_.clone(),
            id: p.id.clone(),
        });
        let series = is_series(&p.type_);
        let section = if series {
            SECTION_SHOWS
        } else {
            SECTION_MOVIES
        };
        let poster = self.poster_url(&p.id, p.poster.as_deref());
        let mut row = json!({
            "ratingKey": rk.to_string(), "key": format!("/library/metadata/{rk}"),
            "guid": format!("nuvio://{}/{}", p.type_, p.id),
            "type": if series { "show" } else { "movie" },
            "title": p.name, "titleSort": p.name,
            "librarySectionID": section, "librarySectionTitle": if series { "TV Shows" } else { "Movies" },
            "librarySectionKey": format!("/library/sections/{section}"),
            "summary": p.description.clone().unwrap_or_default(),
            "thumb": poster,
            "art": p.background.clone().unwrap_or_default(),
            "addedAt": 0, "updatedAt": 0,
            "Genre": tags(&p.genres, false),
            "UltraBlurColors": blur_colors(&poster),
        });
        // The addon's title logo is PMS's `clearLogo`; `/photo/…?url=…/clearLogo` resolves it back
        // out of this row, so the heroes get real title art instead of the text fallback.
        if let Some(logo) = p.logo.as_deref().filter(|logo| !logo.is_empty()) {
            row["clearLogo"] = json!(logo);
        }
        if let Some(y) = year_of(p.release_info.as_deref()) {
            row["year"] = json!(y);
        }
        if let Some(r) = rating_of(p.imdb_rating.as_deref()) {
            row["audienceRating"] = json!(r);
            row["audienceRatingImage"] = json!("imdb://image.rating");
        }
        if let Some(score) = tomato_score(p) {
            let image = if score >= 60.0 {
                "rottentomatoes://image.rating.ripe"
            } else {
                "rottentomatoes://image.rating.rotten"
            };
            row["rating"] = json!(score / 10.0);
            row["ratingImage"] = json!(image);
            let mut ratings =
                vec![json!({"type": "critic", "value": score / 10.0, "image": image})];
            if let Some(imdb) = rating_of(p.imdb_rating.as_deref()) {
                ratings.push(
                    json!({"type": "audience", "value": imdb, "image": "imdb://image.rating"}),
                );
            }
            row["Rating"] = json!(ratings);
        }
        if !series {
            self.watch_fields(&mut row, rk);
        }
        row
    }

    fn meta_row(&self, rk: i64, m: &Meta) -> Value {
        let p = preview_from_meta(m, &m.id);
        let mut row = self.preview_row(&p);
        row["ratingKey"] = json!(rk.to_string());
        row["key"] = json!(format!("/library/metadata/{rk}"));
        if let Some(d) = date_of(m.released.as_deref()) {
            row["originallyAvailableAt"] = json!(d);
        }
        if row.get("year").is_none() {
            if let Some(y) = year_of(m.released.as_deref()) {
                row["year"] = json!(y);
            }
        }
        row["Role"] = tags(&m.cast, true);
        row["Director"] = tags(&m.director, false);
        row["Writer"] = tags(&m.writer, false);
        if let Some(c) = &m.country {
            row["Country"] = tags(&[c.clone()], false);
        }
        let series = is_series(&m.type_);
        row["duration"] = json!(runtime_ms(m.runtime.as_deref()).unwrap_or(if series {
            DEFAULT_EPISODE_MS
        } else {
            DEFAULT_MOVIE_MS
        }));
        if series {
            let eps = episodes_of(m);
            let viewed = eps
                .iter()
                .filter(|(_, _, v)| self.watch.get(self.episode_key(&m.id, &v.id)).view_count > 0)
                .count();
            row["childCount"] = json!(seasons_of(m).len());
            row["leafCount"] = json!(eps.len());
            row["viewedLeafCount"] = json!(viewed);
        }
        row
    }

    /// TMDB cast on a detail row (a movie also gets its TMDB trailer). One request per title:
    /// `movie_details` / `tv_details` already append `credits` (and `recommendations`).
    fn enrich_row(&self, rk: i64, row: &mut Value) {
        let Some(Ident::Title { type_, id }) = self.keys.ident(rk) else {
            return;
        };
        let series = is_series(&type_);
        let Some(meta) = self.meta(&type_, &id) else {
            return;
        };
        let meta_trailer = || {
            meta.trailer_streams
                .iter()
                .chain(&meta.trailers)
                .find_map(youtube_id)
        };
        let Some(client) = TmdbClient::from_env() else {
            if let Some(youtube_id) = meta_trailer() {
                row["Extras"] = trailer_extras(rk, &youtube_id, "Official Trailer");
            }
            return;
        };
        let tmdb_id = match if series {
            resolve_tmdb_series_id(&client, &id, meta.imdb_id.as_deref())
        } else {
            resolve_tmdb_movie_id(&client, &id, meta.imdb_id.as_deref())
        } {
            Ok(id) => id,
            Err(_) => None,
        };
        let Some(tmdb_id) = tmdb_id else {
            if let Some(youtube_id) = meta_trailer() {
                row["Extras"] = trailer_extras(rk, &youtube_id, "Official Trailer");
            }
            return;
        };

        if series {
            // A show's trailers come from Cinemeta; TMDB only supplies the cast here.
            match client.tv_details(tmdb_id) {
                Ok(details) => set_roles(row, &details.credits.cast),
                Err(_) => {}
            }
            if let Some(youtube_id) = meta_trailer() {
                row["Extras"] = trailer_extras(rk, &youtube_id, "Official Trailer");
            }
            return;
        }

        let (details, videos) = std::thread::scope(|scope| {
            let details = scope.spawn(|| client.movie_details(tmdb_id));
            let videos = scope.spawn(|| client.videos(tmdb_id));
            (details.join(), videos.join())
        });

        match details {
            Ok(Ok(details)) => set_roles(row, &details.credits.cast),
            Ok(Err(_)) | Err(_) => {}
        }

        let trailer = match videos {
            Ok(Ok(videos)) => videos.iter().find_map(|video| {
                tmdb_trailer_id(video).map(|youtube_id| {
                    let title = if video.name.trim().is_empty() {
                        "Official Trailer"
                    } else {
                        video.name.as_str()
                    };
                    (youtube_id, title.to_string())
                })
            }),
            Ok(Err(_)) | Err(_) => None,
        }
        .or_else(|| {
            meta.trailer_streams
                .iter()
                .chain(&meta.trailers)
                .find_map(youtube_id)
                .map(|id| (id, "Official Trailer".into()))
        });
        if let Some((youtube_id, title)) = trailer {
            row["Extras"] = trailer_extras(rk, &youtube_id, &title);
        }
    }

    fn related_hub(&self, rk: i64) -> Response {
        // A synthetic key or a title ident decides the kind; anything else has no hub.
        let series = synthetic_show_tmdb_id(rk).is_some()
            || matches!(
                self.keys.ident(rk),
                Some(Ident::Title { type_, .. }) if is_series(&type_)
            );
        if !series
            && synthetic_tmdb_id(rk).is_none()
            && !matches!(
                self.keys.ident(rk),
                Some(Ident::Title { type_, .. }) if type_ == "movie"
            )
        {
            return Response::json(&container(
                json!({"Hub": [related_hub_json("movie", Vec::new())]}),
            ));
        }
        let kind = if series { "show" } else { "movie" };
        let empty_hub = || related_hub_json(kind, Vec::new());
        let Some(client) = TmdbClient::from_env() else {
            return Response::json_status(
                503,
                &container(json!({
                    "Hub": [empty_hub()], "error": "TMDB_API_KEY is not configured"
                })),
            );
        };
        let tmdb_id = match if series {
            self.related_show_tmdb_id(rk, &client)
        } else {
            self.related_tmdb_id(rk, &client)
        } {
            Ok(Some(id)) => id,
            Ok(None) => {
                return Response::json(&container(json!({"Hub": [empty_hub()]})));
            }
            Err(error) => {
                return Response::json_status(
                    503,
                    &container(json!({
                        "Hub": [empty_hub()], "error": format!("TMDB ID lookup failed: {error}")
                    })),
                );
            }
        };
        let (rows, collection_hub): (Vec<Value>, Option<Value>) = if series {
            let details = match client.tv_details(tmdb_id) {
                Ok(details) => details,
                Err(error) => {
                    return Response::json_status(
                        503,
                        &container(json!({
                            "Hub": [empty_hub()], "error": format!("TMDB TV details lookup failed: {error}")
                        })),
                    );
                }
            };
            let rows = details
                .recommendations
                .results
                .iter()
                .filter(|show| {
                    show.id > 0 && show.id <= i64::MAX as u64 - TMDB_SHOW_KEY_BASE as u64
                })
                .take(10)
                .map(|show| self.tmdb_show_recommendation_row(show))
                .collect();
            (rows, None)
        } else {
            let details = match client.movie_details(tmdb_id) {
                Ok(details) => details,
                Err(error) => {
                    return Response::json_status(
                        503,
                        &container(json!({
                            "Hub": [empty_hub()], "error": format!("TMDB movie details lookup failed: {error}")
                        })),
                    );
                }
            };
            let rows = details
                .recommendations
                .results
                .iter()
                .filter(|movie| {
                    movie.id > 0 && movie.id <= i64::MAX as u64 - TMDB_FALLBACK_KEY_BASE as u64
                })
                .take(10)
                .map(|movie| self.tmdb_recommendation_row(movie))
                .collect();
            let hub = self.collection_hub(&details, &client);
            (rows, hub)
        };
        let mut hubs = vec![related_hub_json(kind, rows)];
        hubs.extend(collection_hub);
        Response::json(&container(json!({"Hub": hubs})))
    }

    /// A member film's franchise, in the shape PMS uses for the same shelf: a
    /// `collection.related.*` hub, so the Detail page draws it as the collection shelf under the
    /// collection's own name rather than folding the films into Related. The rows are the OTHER
    /// films — this page's own title is the page — built by the same row builder as a
    /// recommendation, so each one is a clickable, playable key.
    ///
    /// The hub's `key` names the collection by `tagId` (its TMDB id), which is the identity the
    /// app's collection store resolves; the franchise itself is kept in [`Shim::collections`] so
    /// the heading's page (`/library/sections/{s}/collections` → header → children) answers
    /// without a second TMDB round trip.
    fn collection_hub(&self, details: &TmdbMovieDetails, client: &TmdbClient) -> Option<Value> {
        let collection = details
            .movie
            .belongs_to_collection
            .as_ref()
            .filter(|collection| collection.id > 0)?;
        let parts = client.collection_details(collection.id).ok()?;
        let hub = self.collection_hub_json(&parts, details.movie.id, &collection.name)?;
        self.remember_collection(&parts);
        Some(hub)
    }

    /// The shelf itself, from the franchise TMDB returned: **PURE**, so the two things that can be
    /// wrong here are host-testable — the page's own film is left out, and the heading is the
    /// franchise's name rather than the one the member row named it by.
    fn collection_hub_json(
        &self,
        parts: &TmdbCollection,
        own_id: u64,
        fallback_title: &str,
    ) -> Option<Value> {
        let rows: Vec<Value> = parts
            .parts
            .iter()
            .filter(|movie| {
                movie.id > 0
                    && movie.id != own_id
                    && movie.id <= i64::MAX as u64 - TMDB_FALLBACK_KEY_BASE as u64
            })
            .map(|movie| self.tmdb_recommendation_row(movie))
            .collect();
        if rows.is_empty() {
            return None;
        }
        let title = if parts.name.trim().is_empty() {
            fallback_title
        } else {
            parts.name.as_str()
        };
        Some(json!({
            "title": title, "type": "movie",
            "hubIdentifier": format!("collection.related.{SECTION_MOVIES}.1"),
            "key": format!("/library/sections/{SECTION_MOVIES}/all?type=1&tagId={}", parts.id),
            "size": rows.len(), "totalSize": parts.parts.len(), "more": false,
            "Metadata": rows,
        }))
    }

    /// Keep a franchise the shim has served, so its own page answers without another TMDB read.
    /// Bounded like every other in-memory cache here; an evicted franchise is simply fetched again.
    fn remember_collection(&self, collection: &TmdbCollection) {
        let mut map = self
            .collections
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if map.len() >= 256 {
            map.clear();
        }
        map.insert(collection.id, collection.clone());
    }

    /// A franchise by TMDB id: the one a detail page just showed, else one cached TMDB request.
    fn tmdb_collection(&self, id: u64) -> Option<TmdbCollection> {
        if let Some(collection) = self
            .collections
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&id)
        {
            return Some(collection.clone());
        }
        let collection = TmdbClient::from_env()?.collection_details(id).ok()?;
        self.remember_collection(&collection);
        Some(collection)
    }

    /// A franchise as a PMS collection row — the header the collection page draws, and the row a
    /// member's `tagId` resolves against in the section's collection listing (`index`).
    fn collection_row(&self, collection: &TmdbCollection) -> Value {
        let rk = TMDB_COLLECTION_KEY_BASE + collection.id as i64;
        let poster = tmdb_image(collection.poster_path.as_deref())
            .or_else(|| {
                collection
                    .parts
                    .iter()
                    .find_map(|part| tmdb_image(part.poster_path.as_deref()))
            })
            .unwrap_or_default();
        json!({
            "ratingKey": rk.to_string(), "key": format!("/library/collections/{rk}/children"),
            "guid": format!("nuvio://collection/{}", collection.id),
            "type": "collection", "title": collection.name, "titleSort": collection.name,
            "index": collection.id,
            "librarySectionID": SECTION_MOVIES, "librarySectionTitle": "Movies",
            "librarySectionKey": format!("/library/sections/{SECTION_MOVIES}"),
            "summary": collection.overview.clone().unwrap_or_default(),
            "thumb": poster, "childCount": collection.parts.len(),
            "UltraBlurColors": blur_colors(&poster),
            // TMDB lists a franchise in release order, which is PMS's own default here.
            "collectionSort": 0, "addedAt": 0, "updatedAt": 0,
        })
    }

    /// A franchise's films as collection children, in TMDB's release order and paged like any
    /// other collection listing.
    fn collection_children(&self, collection: &TmdbCollection, start: usize, size: usize) -> Value {
        let rows: Vec<Value> = collection
            .parts
            .iter()
            .filter(|movie| {
                movie.id > 0 && movie.id <= i64::MAX as u64 - TMDB_FALLBACK_KEY_BASE as u64
            })
            .skip(start)
            .take(size)
            .map(|movie| self.tmdb_recommendation_row(movie))
            .collect();
        let mut out = container(json!({"Metadata": rows}));
        out["MediaContainer"]["totalSize"] = json!(collection.parts.len());
        out
    }

    fn related_tmdb_id(
        &self,
        rk: i64,
        client: &TmdbClient,
    ) -> plx_stremio::model::Result<Option<u64>> {
        if let Some(tmdb_id) = synthetic_tmdb_id(rk) {
            return Ok(Some(tmdb_id));
        }
        let Some(Ident::Title { id, .. }) = self
            .keys
            .ident(rk)
            .filter(|ident| matches!(ident, Ident::Title { type_, .. } if type_ == "movie"))
        else {
            return Ok(None);
        };
        let imdb_id = self
            .meta("movie", &id)
            .and_then(|meta| meta.imdb_id.clone());
        resolve_tmdb_movie_id(client, &id, imdb_id.as_deref())
    }

    fn related_show_tmdb_id(
        &self,
        rk: i64,
        client: &TmdbClient,
    ) -> plx_stremio::model::Result<Option<u64>> {
        if let Some(tmdb_id) = synthetic_show_tmdb_id(rk) {
            return Ok(Some(tmdb_id));
        }
        let Some(Ident::Title { id, .. }) = self
            .keys
            .ident(rk)
            .filter(|ident| matches!(ident, Ident::Title { type_, .. } if is_series(type_)))
        else {
            return Ok(None);
        };
        let imdb_id = self
            .meta("series", &id)
            .and_then(|meta| meta.imdb_id.clone());
        resolve_tmdb_series_id(client, &id, imdb_id.as_deref())
    }

    fn tmdb_recommendation_row(&self, movie: &TmdbMovie) -> Value {
        let stremio_id = format!("tmdb:{}", movie.id);
        if let Some(meta) = self.meta("movie", &stremio_id) {
            let preview = preview_from_meta(&meta, &stremio_id);
            return self.preview_row(&preview);
        }

        let fallback_key = TMDB_FALLBACK_KEY_BASE + movie.id as i64;
        self.tmdb_fallbacks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(fallback_key, movie.clone());
        self.tmdb_fallback_row(movie, fallback_key)
    }

    fn tmdb_fallback_row(&self, movie: &TmdbMovie, fallback_key: i64) -> Value {
        let poster = movie.poster_path.as_deref().and_then(|path| {
            path.starts_with('/')
                .then(|| format!("https://image.tmdb.org/t/p/w300{path}"))
        });
        let preview = MetaPreview {
            id: format!("tmdb:{}", movie.id),
            type_: "movie".into(),
            name: movie.title.clone(),
            poster: poster.clone(),
            background: movie
                .backdrop_path
                .as_deref()
                .map(|path| format!("https://image.tmdb.org/t/p/w300{path}")),
            description: movie.overview.clone(),
            release_info: movie.release_date.clone(),
            ..MetaPreview::default()
        };
        let mut row = self.preview_row(&preview);
        row["ratingKey"] = json!(fallback_key.to_string());
        row["key"] = json!(format!("/library/metadata/{fallback_key}"));
        if let Some(poster) = poster {
            row["thumb"] = json!(poster);
        }
        row
    }

    fn tmdb_fallback_movie(&self, rk: i64) -> Option<TmdbMovie> {
        self.tmdb_fallbacks
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&rk)
            .cloned()
    }

    /// The row of a synthetic related key that the in-memory fallback map no longer holds (a
    /// restart, or a key reached from Continue Watching). One cached TMDB request, as the route
    /// itself makes; `None` when TMDB is unconfigured or the title is gone.
    fn tmdb_synthetic_row(&self, rk: i64) -> Option<Value> {
        let client = TmdbClient::from_env()?;
        if let Some(tmdb_id) = synthetic_show_tmdb_id(rk) {
            let details = client.tv_details(tmdb_id).ok()?;
            return Some(self.tmdb_show_details_row(&details, rk));
        }
        let details = client.movie_details(synthetic_tmdb_id(rk)?).ok()?;
        Some(self.tmdb_details_row(&details, rk))
    }

    fn tmdb_show_recommendation_row(&self, show: &TmdbShow) -> Value {
        let stremio_id = format!("tmdb:{}", show.id);
        if let Some(meta) = self.meta("series", &stremio_id) {
            let preview = preview_from_meta(&meta, &stremio_id);
            return self.preview_row(&preview);
        }
        self.tmdb_show_fallback_row(show, TMDB_SHOW_KEY_BASE + show.id as i64)
    }

    fn tmdb_show_fallback_row(&self, show: &TmdbShow, fallback_key: i64) -> Value {
        let preview = MetaPreview {
            id: format!("tmdb:{}", show.id),
            type_: "series".into(),
            name: show.name.clone(),
            poster: tmdb_image(show.poster_path.as_deref()),
            background: tmdb_image(show.backdrop_path.as_deref()),
            description: show.overview.clone(),
            release_info: show.first_air_date.clone(),
            ..MetaPreview::default()
        };
        let mut row = self.preview_row(&preview);
        row["ratingKey"] = json!(fallback_key.to_string());
        row["key"] = json!(format!("/library/metadata/{fallback_key}"));
        if let Some(poster) = preview.poster {
            row["thumb"] = json!(poster);
        }
        row
    }

    fn tmdb_show_details_row(&self, details: &TmdbShowDetails, rk: i64) -> Value {
        let show = &details.show;
        let preview = MetaPreview {
            id: format!("tmdb:{}", show.id),
            type_: "series".into(),
            name: show.name.clone(),
            poster: tmdb_image(show.poster_path.as_deref()),
            background: tmdb_image(show.backdrop_path.as_deref()),
            description: show.overview.clone(),
            release_info: show.first_air_date.clone(),
            genres: show.genres.iter().map(|genre| genre.name.clone()).collect(),
            ..MetaPreview::default()
        };
        let mut row = self.preview_row(&preview);
        row["ratingKey"] = json!(rk.to_string());
        row["key"] = json!(format!("/library/metadata/{rk}"));
        set_roles(&mut row, &details.credits.cast);
        row["Genre"] = tags(&preview.genres, false);
        row["duration"] = json!(DEFAULT_EPISODE_MS);
        if let Some(date) = date_of(show.first_air_date.as_deref()) {
            row["originallyAvailableAt"] = json!(date);
        }
        if let Some(backdrop) = preview.background {
            row["art"] = json!(backdrop);
        }
        if let Some(logo) = tmdb_logo(details.images.as_ref()) {
            row["clearLogo"] = json!(logo);
        }
        row
    }

    fn tmdb_details_row(&self, details: &TmdbMovieDetails, rk: i64) -> Value {
        let movie = &details.movie;
        let image_url = |path: Option<&str>| {
            path.filter(|path| path.starts_with('/'))
                .map(|path| format!("https://image.tmdb.org/t/p/w300{path}"))
        };
        let preview = MetaPreview {
            id: format!("tmdb:{}", movie.id),
            type_: "movie".into(),
            name: movie.title.clone(),
            poster: image_url(movie.poster_path.as_deref()),
            background: image_url(movie.backdrop_path.as_deref()),
            description: movie.overview.clone(),
            release_info: movie.release_date.clone(),
            genres: details
                .movie
                .genres
                .iter()
                .map(|genre| genre.name.clone())
                .collect(),
            ..MetaPreview::default()
        };
        let mut row = self.preview_row(&preview);
        row["ratingKey"] = json!(rk.to_string());
        row["key"] = json!(format!("/library/metadata/{rk}"));
        row["Role"] = Value::Array(
            details
                .credits
                .cast
                .iter()
                .filter_map(tmdb_role)
                .take(10)
                .collect(),
        );
        row["Genre"] = tags(&preview.genres, false);
        row["duration"] = json!(movie
            .runtime
            .filter(|minutes| *minutes > 0)
            .map(|minutes| i64::from(minutes) * 60_000)
            .unwrap_or(DEFAULT_MOVIE_MS));
        if let Some(date) = date_of(movie.release_date.as_deref()) {
            row["originallyAvailableAt"] = json!(date);
        }
        if let Some(backdrop) = preview.background {
            row["art"] = json!(backdrop);
        }
        if let Some(logo) = tmdb_logo(details.images.as_ref()) {
            row["clearLogo"] = json!(logo);
        }
        row
    }

    fn episode_key(&self, series: &str, video: &str) -> i64 {
        self.keys.key_of(&Ident::Episode {
            series: series.into(),
            video: video.into(),
        })
    }

    /// The filmography of a Cast & Crew headshot, as the rows `/library/people/{person key}/media`
    /// answers with — the route the app's person page reads its two shelves from.
    ///
    /// `combined_credits` merges films and shows into ONE array and states the kind per row
    /// (`media_type`), so each row is built by the builder for its own kind — the same rule
    /// `split_by_type` applies to a real server's listing. Pure, so the mapping, the person key and
    /// the order are host-testable without TMDB (the route does the request).
    ///
    /// Both builders render from TMDB alone, deliberately: the credits are already in hand, where
    /// `tmdb_recommendation_row`'s addon `meta()` would cost one HTTP request per credit (a
    /// prolific actor has 60).
    fn person_credit_rows(&self, person_id: u64, credits: &[TmdbCredit]) -> Vec<Value> {
        let key = TMDB_PERSON_KEY_BASE + person_id as i64;
        let mut rows: Vec<(i64, Value)> = credits
            .iter()
            .filter(|credit| credit.id > 0 && credit.id < (i64::MAX - TMDB_PERSON_KEY_BASE) as u64)
            .filter_map(|credit| {
                let (year, mut row) = match credit.media_type.as_str() {
                    "movie" => (
                        year_of(credit.release_date.as_deref()).unwrap_or(0),
                        self.tmdb_fallback_row(&credit.movie(), TMDB_FALLBACK_KEY_BASE + credit.id as i64),
                    ),
                    "tv" => (
                        year_of(credit.first_air_date.as_deref()).unwrap_or(0),
                        self.tmdb_show_fallback_row(&credit.show(), TMDB_SHOW_KEY_BASE + credit.id as i64),
                    ),
                    // Neither a film nor a show. The page has exactly two shelves, and filing an
                    // unknown kind under either would be a guess.
                    _ => return None,
                };
                // What they played IN THIS CREDIT — a fact about the credit, not about the title.
                // The person page captions each tile from `Role[]` (`person::roles_from`, which
                // joins on this same key), and a credit with no character just leaves it blank.
                if !credit.character.trim().is_empty() {
                    row["Role"] = json!([{"id": key, "tag": "", "role": credit.character}]);
                }
                Some((year, row))
            })
            .collect();
        // Newest first, undated last — `person::filmography` orders its own list the same way.
        rows.sort_by_key(|(year, _)| -year);
        let rows: Vec<Value> = rows.into_iter().map(|(_, row)| row).collect();
        // The page's next request is a batched `/library/metadata/{csv}` for the captions, whose
        // keys are exactly these rows'. Holding them keeps that request free — the whole list is
        // replaced rather than merged, because the same film has a different part per person.
        *self.credit_rows.lock().unwrap_or_else(|e| e.into_inner()) = rows.clone();
        rows
    }

    /// A filmography row the person page already has, so its batched caption read costs nothing.
    ///
    /// The fallback is [`Shim::tmdb_synthetic_row`] rather than a miss: a credit key IS a title key
    /// (the movie or show base plus its TMDB id), and that row's own `Role[]` carries this person's
    /// character under the same synthetic id — so the caption is still right, at one cached request.
    /// Answering nothing would leave the batch 404ing and the store retrying it forever.
    fn person_row(&self, rk: i64) -> Option<Value> {
        let cached = self
            .credit_rows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|row| row["ratingKey"].as_str().and_then(|v| v.parse::<i64>().ok()) == Some(rk))
            .cloned();
        cached.or_else(|| self.tmdb_synthetic_row(rk))
    }

    /// The URL to open for a ranked stream: itself, or — for a torrent marker — what the streaming
    /// server answers `/create` with (season/episode let it pick the file in a season pack).
    fn open_url(&self, rk: i64, stream: &Stream, url: &str) -> Option<String> {
        if !url.starts_with(shim_media::TORRENT_SCHEME) {
            return Some(url.to_string());
        }
        let key = (rk, url.to_string());
        if let Some(u) = self
            .resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            return Some(u.clone());
        }
        let server = self.torrent_server()?;
        let server = server.as_str();
        let (season, episode) = match self.keys.ident(rk) {
            Some(Ident::Episode { video, .. }) => {
                let mut it = video.rsplit(':').filter_map(|x| x.parse::<u32>().ok());
                let e = it.next();
                (it.next(), e)
            }
            _ => (None, None),
        };
        let u = plx_stremio::torrent::resolve(server, stream, season, episode).ok()?;
        self.resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, u.clone());
        Some(u)
    }

    fn season_row(&self, show_rk: i64, m: &Meta, n: u32) -> Value {
        let rk = self.keys.key_of(&Ident::Season {
            series: m.id.clone(),
            n,
        });
        let eps: Vec<_> = episodes_of(m)
            .into_iter()
            .filter(|(s, _, _)| *s == n)
            .collect();
        let viewed = eps
            .iter()
            .filter(|(_, _, v)| self.watch.get(self.episode_key(&m.id, &v.id)).view_count > 0)
            .count();
        json!({
            "ratingKey": rk.to_string(), "key": format!("/library/metadata/{rk}"),
            "guid": format!("nuvio://season/{}/{n}", m.id),
            "type": "season", "title": season_title(n), "titleSort": season_title(n), "index": n,
            "parentRatingKey": show_rk.to_string(), "parentKey": format!("/library/metadata/{show_rk}"),
            "parentTitle": m.name, "parentThumb": m.poster.clone().unwrap_or_default(),
            "thumb": m.poster.clone().unwrap_or_default(), "art": m.background.clone().unwrap_or_default(),
            "UltraBlurColors": blur_colors(&m.poster.clone().unwrap_or_default()),
            "librarySectionID": SECTION_SHOWS, "librarySectionTitle": "TV Shows",
            "librarySectionKey": format!("/library/sections/{SECTION_SHOWS}"),
            "summary": m.description.clone().unwrap_or_default(),
            "leafCount": eps.len(), "viewedLeafCount": viewed,
            "addedAt": 0, "updatedAt": 0,
        })
    }

    fn episode_row(&self, show_rk: i64, m: &Meta, season: u32, ep: u32, v: &Video) -> Value {
        let rk = self.episode_key(&m.id, &v.id);
        let season_rk = self.keys.key_of(&Ident::Season {
            series: m.id.clone(),
            n: season,
        });
        let title = v
            .title
            .clone()
            .or_else(|| v.name.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("Episode {ep}"));
        let mut row = json!({
            "ratingKey": rk.to_string(), "key": format!("/library/metadata/{rk}"),
            "guid": format!("nuvio://episode/{}", v.id),
            "type": "episode", "title": title, "titleSort": title, "index": ep, "parentIndex": season,
            "parentRatingKey": season_rk.to_string(), "parentKey": format!("/library/metadata/{season_rk}"),
            "parentTitle": season_title(season),
            "grandparentRatingKey": show_rk.to_string(), "grandparentKey": format!("/library/metadata/{show_rk}"),
            "grandparentTitle": m.name, "grandparentThumb": m.poster.clone().unwrap_or_default(),
            "grandparentArt": m.background.clone().unwrap_or_default(),
            "thumb": v.thumbnail.clone().or_else(|| m.background.clone()).unwrap_or_default(),
            "art": m.background.clone().unwrap_or_default(),
            "UltraBlurColors": blur_colors(m.background.as_deref().unwrap_or_default()),
            "summary": v.overview.clone().unwrap_or_default(),
            "librarySectionID": SECTION_SHOWS, "librarySectionTitle": "TV Shows",
            "librarySectionKey": format!("/library/sections/{SECTION_SHOWS}"),
            "duration": runtime_ms(m.runtime.as_deref()).unwrap_or(DEFAULT_EPISODE_MS),
            "addedAt": 0, "updatedAt": 0,
        });
        if let Some(d) = date_of(v.released.as_deref()) {
            row["year"] = json!(year_of(Some(&d)));
            row["originallyAvailableAt"] = json!(d);
        }
        self.watch_fields(&mut row, rk);
        row
    }

    /// The series meta behind a season/episode ident, with the show's ratingKey.
    fn series_of(&self, series: &str) -> Option<(i64, Arc<Meta>)> {
        let m = self.meta("series", series)?;
        let rk = self.keys.key_of(&Ident::Title {
            type_: "series".into(),
            id: series.into(),
        });
        Some((rk, m))
    }

    /// A list row (no Media) for any ratingKey; `None` when its source is gone.
    fn row(&self, rk: i64) -> Option<Value> {
        match self.keys.ident(rk)? {
            Ident::Title { type_, id } => {
                let m = self.meta(&type_, &id)?;
                Some(self.meta_row(rk, &m))
            }
            Ident::Season { series, n } => {
                let (show, m) = self.series_of(&series)?;
                Some(self.season_row(show, &m, n))
            }
            Ident::Episode { series, video } => {
                let (show, m) = self.series_of(&series)?;
                let (s, e, v) = episodes_of(&m)
                    .into_iter()
                    .find(|(_, _, v)| v.id == video)?;
                Some(self.episode_row(show, &m, s, e, v))
            }
        }
    }

    /// The full item: the row plus, for a playable leaf, its `Media[]`. Media whose stream was
    /// probed already carry the real fields; `probe_index` probes that Media now (the decision
    /// path) and returns the verdict beside the row.
    fn item_probed(
        &self,
        rk: i64,
        probe_index: Option<usize>,
    ) -> Option<(Value, Option<Result<crate::shim_probe::ProbeInfo, String>>)> {
        let ident = self.keys.ident(rk);
        let fallback = if ident.is_none() {
            self.tmdb_fallback_movie(rk)
        } else {
            None
        };
        let ident = ident
            .or_else(|| {
                fallback.as_ref().map(|movie| Ident::Title {
                    type_: "movie".into(),
                    id: format!("tmdb:{}", movie.id),
                })
            })
            // The fallback map is in-memory only: after a restart a synthetic key still has to
            // resolve, or a title the user can see refuses to play.
            .or_else(|| synthetic_ident(rk))?;
        let mut row = match fallback {
            Some(movie) => self.tmdb_fallback_row(&movie, rk),
            None => match self.row(rk) {
                Some(row) => row,
                None => self.tmdb_synthetic_row(rk)?,
            },
        };
        let mut verdict = None;
        if self.stream_target(&ident).is_some() {
            let dur = row["duration"].as_i64().unwrap_or(0);
            let wait = if probe_index.is_some() {
                self.cfg.stream_deadline
            } else {
                DETAIL_WAIT
            };
            let (ranked, subs, done) =
                self.ranked(rk, &ident, u32::try_from(dur / 1000).ok(), wait, probe_index.is_some());
            let mut media = Vec::with_capacity(ranked.len().max(1));
            // Pre-probe the leading stream once the list is complete, off this request: the play that
            // usually follows a detail page then finds real fields (and a warmed source) waiting.
            if probe_index.is_none() && done && self.probes.has_prober() {
                if let Some((_, c)) = ranked.first() {
                    let url = self
                        .resolved
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&(rk, c.url.clone()))
                        .cloned()
                        .unwrap_or_else(|| c.url.clone());
                    if !url.starts_with(shim_media::TORRENT_SCHEME)
                        && self.probes.cached(&url).is_none()
                    {
                        if let Some(me) = self.me.get().and_then(std::sync::Weak::upgrade) {
                            let _ = std::thread::Builder::new()
                                .name("pmsshim-preprobe".into())
                                .spawn(move || {
                                    let _ = me.probes.probe(&url);
                                });
                        }
                    }
                }
            }
            for (n, (i, c)) in ranked.iter().enumerate() {
                let mut m = shim_media::media_json(rk, *i, c, dur, &subs);
                let probed = if probe_index == Some(n) {
                    let r = match self.open_url(rk, &c.stream, &c.url) {
                        Some(u) => self.probes.probe(&u),
                        None => Some(Err(
                            "the streaming server could not open this torrent".into()
                        )),
                    };
                    verdict = r.clone();
                    r
                } else {
                    self.resolved
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&(rk, c.url.clone()))
                        .cloned()
                        .or_else(|| Some(c.url.clone()))
                        .and_then(|u| self.probes.cached(&u))
                };
                if let Some(Ok(p)) = &probed {
                    crate::shim_probe::apply(&mut m, p);
                }
                media.push(m);
            }
            if media.is_empty() && !done {
                // Addons still answering: a Part that plays whichever stream leads at play time, so
                // the page is never held up and its Play button always has something to start.
                let pending = shim_media::Classified {
                    stream: Stream::default(),
                    info: plx_stremio::classify::StreamInfo {
                        label: "Finding streams…".into(),
                        ..Default::default()
                    },
                    url: String::new(),
                };
                media.push(shim_media::media_json(rk, BEST_PART, &pending, dur, &subs));
            }
            if let Some(d) = media
                .first()
                .and_then(|m| m["duration"].as_i64())
                .filter(|d| *d > 0)
            {
                row["duration"] = json!(d);
            }
            row["Media"] = Value::Array(media);
        }
        Some((row, verdict))
    }

    fn item(&self, rk: i64) -> Option<Value> {
        self.item_probed(rk, None).map(|(row, _)| row)
    }

    fn decision(&self, req: &Request) -> Response {
        let path_key = req.q("path").and_then(trailing_key);
        let rk = path_key.unwrap_or(-1);
        let rk = if rk < 0 {
            req.q("path")
                .and_then(|path| path.trim_end_matches('/').rsplit('/').next())
                .and_then(|id| id.strip_prefix("tmdb:"))
                .and_then(|id| id.parse::<u64>().ok())
                .and_then(|tmdb_id| {
                    self.account.saved_library(false).ok()?.into_iter()
                        .find(|item| item.id == format!("tmdb:{tmdb_id}"))
                        .map(|item| if is_series(&item.type_) { TMDB_SHOW_KEY_BASE } else { TMDB_FALLBACK_KEY_BASE } + tmdb_id as i64)
                })
                .unwrap_or(-1)
        } else { rk };
        let index: usize = req
            .q("mediaIndex")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let refuse = |why: String| {
            Response::json(&container(
                json!({"generalDecisionCode": 2000, "generalDecisionText": "Direct play is not available.",
                "transcodeDecisionCode": 4005, "transcodeDecisionText": why, "mdeDecisionCode": 2000, "Metadata": []}),
            ))
        };
        let Some((mut row, verdict)) = self.item_probed(rk, Some(index)) else {
            return refuse("This title is no longer available from your addons.".into());
        };
        if row["Media"].as_array().is_some_and(Vec::is_empty) {
            return refuse("No playable stream was found for this title.".into());
        }
        match verdict {
            Some(Err(e)) => {
                return refuse(format!(
                    "This stream could not be opened ({e}). Pick another stream."
                ))
            }
            Some(Ok(p)) => {
                if let Some(why) = crate::shim_probe::refusal(&p) {
                    return refuse(why);
                }
            }
            None if self.probes.has_prober() => {
                return refuse(
                    "Streams are still being discovered. Try again or choose another stream."
                        .into(),
                )
            }
            None => {}
        }
        // Return the chosen Media first: the native route consumes this response as the
        // authoritative codec/track description, including an explicit non-leading choice.
        if let Some(ms) = row["Media"].as_array_mut() {
            if index >= ms.len() {
                return refuse("This stream is no longer available. Choose another stream.".into());
            }
            ms.swap(0, index);
        }
        if let Some(ms) = row["Media"].as_array_mut() {
            for m in ms.iter_mut() {
                if let Some(parts) = m["Part"].as_array_mut() {
                    for part in parts {
                        part["decision"] = json!("directplay");
                        if let Some(ss) = part["Stream"].as_array_mut() {
                            for s in ss {
                                s["decision"] = json!("copy");
                            }
                        }
                    }
                }
            }
        }
        Response::json(&container(
            json!({"generalDecisionCode": 1000, "generalDecisionText": "Direct play OK", "mdeDecisionCode": 1000, "Metadata": [row]}),
        ))
    }

    // ---- hubs ------------------------------------------------------------------------------

    fn poster_url(&self, id: &str, original: Option<&str>) -> String {
        let poster = self.backend.poster(id, original).unwrap_or_default();
        if let Some(original) = original.filter(|u| *u != poster) {
            let mut fallbacks = self
                .poster_fallbacks
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if fallbacks.len() >= 4096 {
                fallbacks.clear();
            }
            fallbacks.insert(poster.clone(), original.to_string());
        }
        poster
    }

    fn continue_rows(&self, section: Option<i64>) -> Vec<Value> {
        self.watch
            .in_progress()
            .into_iter()
            .filter_map(|(rk, w, mut row)| {
                if section.is_some_and(|s| row["librarySectionID"].as_i64() != Some(s)) {
                    return None;
                }
                row["viewOffset"] = json!(w.offset_ms);
                row["lastViewedAt"] = json!(w.last_viewed_at);
                if w.duration_ms > 0 {
                    row["duration"] = json!(w.duration_ms);
                }
                if let Some(ident) = self.keys.ident(rk) {
                    let id = match ident {
                        Ident::Title { id, .. } => id,
                        Ident::Season { series, .. } | Ident::Episode { series, .. } => series,
                    };
                    row["thumb"] = json!(self.poster_url(&id, row["thumb"].as_str()));
                }
                if row.get("UltraBlurColors").is_none() {
                    row["UltraBlurColors"] =
                        blur_colors(row["thumb"].as_str().unwrap_or_default());
                }
                Some(row)
            })
            .collect()
    }

    fn continue_hub(&self, count: usize, section: Option<i64>) -> Value {
        let rows: Vec<Value> = self
            .continue_rows(section)
            .into_iter()
            .take(count)
            .collect();
        json!({"title": "Continue Watching", "type": "mixed", "hubIdentifier": "home.continue",
               "key": "/hubs/continueWatching", "size": rows.len(), "more": false, "Metadata": rows})
    }

    fn saved_hub(&self, count: usize) -> Option<Value> {
        let items = self.account.saved_library(false).ok()?;
        let rows: Vec<_> = items
            .iter()
            .take(count)
            .map(|p| {
                let mut row = self.preview_row(p);
                row["librarySectionTitle"] = json!("");
                row
            })
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(
            json!({"title":"My List","type":"mixed","hubIdentifier":"movie.recentlyadded.nuvio-list",
            "key":"/hubs/nuvio/list","size":rows.len(),"totalSize":items.len(),"more":items.len()>rows.len(),"Metadata":rows}),
        )
    }

    fn saved_item(&self, req: &Request) -> Response {
        let Some(rk) = req
            .q("ratingKey")
            .and_then(|v| v.parse().ok())
        else {
            return Response::json_status(400, &json!({"error":"invalid title"}));
        };
        let Some(Ident::Title { type_, id }) =
            self.keys.ident(rk).or_else(|| synthetic_ident(rk))
        else {
            return Response::json_status(400, &json!({"error":"invalid title"}));
        };
        if !self.account.signed_in() {
            return Response::json(&json!({"signedIn":false,"saved":false}));
        }
        let result = (|| {
            if req.method == "POST" {
                let saved = match req.q("saved") {
                    Some("1") => true,
                    Some("0") => false,
                    _ => return Err(plx_stremio::model::Error::Status(400)),
                };
                let (saved_type, saved_id) = translate_to_stremio_id(rk)
                    .map(|id| {
                        (
                            if synthetic_show_tmdb_id(rk).is_some() {
                                "series"
                            } else {
                                "movie"
                            }
                            .to_owned(),
                            id,
                        )
                    })
                    .unwrap_or_else(|| (type_.clone(), id.clone()));
                let client = TmdbClient::from_env();
                let tmdb_id = if saved_type == "series" {
                    synthetic_show_tmdb_id(rk)
                } else {
                    synthetic_tmdb_id(rk)
                };
                let imdb_id = tmdb_id.and_then(|tmdb_id| {
                    client.as_ref().and_then(|client| {
                        if saved_type == "series" {
                            client.tv_external_ids(tmdb_id).ok().flatten()
                        } else {
                            client.movie_external_ids(tmdb_id).ok().flatten()
                        }
                    })
                });
                let save_id = imdb_id.as_deref().unwrap_or(&saved_id);
                let meta = if let Some(meta) = self.meta(&saved_type, save_id) {
                    meta
                } else {
                    let (name, poster_path, description, release_info) =
                        if saved_type == "series" {
                            let tmdb_id = synthetic_show_tmdb_id(rk)
                                .ok_or(plx_stremio::model::Error::Status(404))?;
                            let client = client
                                .as_ref()
                                .ok_or(plx_stremio::model::Error::Status(404))?;
                            let details = client.tv_details(tmdb_id)?;
                            (
                                details.show.name,
                                details.show.poster_path,
                                details.show.overview,
                                details.show.first_air_date,
                            )
                        } else {
                            let tmdb_id = synthetic_tmdb_id(rk)
                                .ok_or(plx_stremio::model::Error::Status(404))?;
                            let client = client
                                .as_ref()
                                .ok_or(plx_stremio::model::Error::Status(404))?;
                            let details = client.movie_details(tmdb_id)?;
                            (
                                details.movie.title,
                                details.movie.poster_path,
                                details.movie.overview,
                                details.movie.release_date,
                            )
                        };
                    Arc::new(Meta {
                        id: save_id.to_owned(),
                        type_: saved_type.clone(),
                        name,
                        poster: tmdb_image(poster_path.as_deref()),
                        description,
                        release_info: year_of(release_info.as_deref())
                            .map(|year| year.to_string()),
                        ..Meta::default()
                    })
                };
                eprintln!("saving My List item: type={} id={}", meta.type_, meta.id);
                self.account.set_saved(&meta, saved)?;
            }
            let items = self.account.saved_library(req.method != "POST")?;
            let translated_id = translate_to_stremio_id(rk);
            let imdb_id = if let Some(tmdb_id) = synthetic_show_tmdb_id(rk) {
                TmdbClient::from_env()
                    .and_then(|client| client.tv_external_ids(tmdb_id).ok().flatten())
            } else if let Some(tmdb_id) = synthetic_tmdb_id(rk) {
                TmdbClient::from_env()
                    .and_then(|client| client.movie_external_ids(tmdb_id).ok().flatten())
            } else {
                None
            };
            Ok(items.iter().any(|item| {
                item.type_ == type_
                    && (item.id == id
                        || translated_id
                            .as_ref()
                            .is_some_and(|translated| item.id == *translated)
                        || imdb_id.as_ref().is_some_and(|imdb| item.id == *imdb))
            }))
        })();
        match result {
            Ok(saved) => Response::json(&json!({"signedIn":true,"saved":saved})),
            Err(plx_stremio::model::Error::Status(400)) => {
                Response::json_status(400, &json!({"error":"invalid request"}))
            }
            Err(_) => {
                Response::json_status(503, &json!({"error":"My List could not sync. Try again."}))
            }
        }
    }

    /// `pageable`: Home's hubs use the per-library "recently added" identifier family, the only
    /// one `plx_data::pms` pages past the first window, with an empty `librarySectionTitle` on the
    /// rows so the app keeps the catalog's own title (`hub_title::localized_hub_title` falls back to
    /// it). A section's own hubs must not: there that family is retitled "Recently Added".
    fn catalog_hub(&self, i: usize, c: &CatalogInfo, count: usize, pageable: bool) -> Value {
        let (rows, total) = self.catalog_range(&c.key, None, 0, count);
        let series = is_series(&c.type_);
        let rows: Vec<Value> = rows
            .iter()
            .map(|p| {
                let mut r = self.preview_row(p);
                if pageable {
                    r["librarySectionTitle"] = json!("");
                }
                r
            })
            .collect();
        let id = if pageable {
            format!(
                "{}.recentlyadded.nuvio-{i}",
                if series { "show" } else { "movie" }
            )
        } else {
            format!("nuvio.catalog.{i}")
        };
        json!({"title": c.title, "type": if series { "show" } else { "movie" },
               "hubIdentifier": id, "key": format!("/hubs/nuvio/{i}"),
               "more": total > rows.len() as i64, "size": rows.len(), "totalSize": total, "Metadata": rows})
    }

    fn acquire(&self) {
        let (m, cv) = &self.permits;
        let mut n = m.lock().unwrap_or_else(|e| e.into_inner());
        while *n == 0 {
            n = cv.wait(n).unwrap_or_else(|e| e.into_inner());
        }
        *n -= 1;
    }

    fn release(&self) {
        let (m, cv) = &self.permits;
        *m.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        cv.notify_one();
    }

    /// Catalog hubs in catalog order, fetched with bounded parallelism. Whatever has not answered
    /// within `budget` is left out of THIS answer and keeps fetching into the cache.
    fn catalog_hubs(
        self: &Arc<Self>,
        filter: impl Fn(&CatalogInfo) -> bool,
        count: usize,
        pageable: bool,
        budget: Duration,
    ) -> Vec<Value> {
        let cats: Vec<(usize, CatalogInfo)> = self
            .catalog_list()
            .into_iter()
            .enumerate()
            .filter(|(_, c)| filter(c))
            .collect();
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Value)>();
        for (pos, (i, c)) in cats.iter().cloned().enumerate() {
            let (me, tx) = (self.clone(), tx.clone());
            let _ = std::thread::Builder::new()
                .name("pmsshim-catalog".into())
                .spawn(move || {
                    me.acquire();
                    let hub = me.catalog_hub(i, &c, count, pageable);
                    me.release();
                    let _ = tx.send((pos, hub));
                });
        }
        drop(tx);
        let deadline = Instant::now() + budget;
        let mut got: Vec<Option<Value>> = vec![None; cats.len()];
        let mut n = 0;
        while n < cats.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((pos, hub)) => {
                    got[pos] = Some(hub);
                    n += 1;
                }
                Err(_) => break,
            }
        }
        got.into_iter()
            .flatten()
            .filter(|h| h["size"].as_u64() != Some(0))
            .collect()
    }

    /// Fill the catalog list and every catalog's first page in the background (boot, or after the
    /// addon set changed), so the first `/hubs` is mostly cache hits.
    pub fn warm(self: &Arc<Self>) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("pmsshim-warm".into())
            .spawn(move || {
                let _ = me.account.saved_library(false);
                let _ = me.catalog_hubs(|_| true, 12, true, Duration::from_secs(60));
            });
    }

    // ---- routing ---------------------------------------------------------------------------

    pub fn handle(self: &Arc<Self>, req: &Request) -> Response {
        // Paths only: a query can carry the token, and part redirects carry credentialed URLs.
        static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRACE.get_or_init(|| std::env::var_os("PMSSHIM_TRACE").is_some()) {
            let t = Instant::now();
            let r = self.route(req);
            eprintln!(
                "pmsshim: {} {} -> {} ({} ms)",
                req.method,
                req.path,
                r.status,
                t.elapsed().as_millis()
            );
            return r;
        }
        self.route(req)
    }

    fn route(self: &Arc<Self>, req: &Request) -> Response {
        let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
        let p = req.path.as_str();
        let tmdb_rating_key = |raw: &str| {
            let tmdb_id = raw.strip_prefix("tmdb:")?.parse::<u64>().ok()?;
            let saved_id = format!("tmdb:{tmdb_id}");
            let type_ = self.account.saved_library(false).ok()?.into_iter()
                .find(|item| item.id == saved_id)?.type_;
            Some((if is_series(&type_) { TMDB_SHOW_KEY_BASE } else { TMDB_FALLBACK_KEY_BASE } + tmdb_id as i64))
        };
        match (req.method.as_str(), segs.as_slice()) {
            (_, []) | (_, ["identity"]) => Response::json(&container(json!({
                "machineIdentifier": MACHINE_ID, "friendlyName": "Nuvio", "version": "1.41.0.0000-nuvio",
                "platform": "webOS", "myPlex": false, "myPlexSubscription": true,
            }))),
            // plex.tv: the app's plex.tv base points here; sign-in maps onto the Nuvio account.
            (_, ["api", "v2", rest @ ..]) => self.account.handle(req, rest),
            (_, ["api", ..]) => Response::json_status(404, &json!({"error": "no plex.tv here"})),

            (_, ["library", "sections"]) => Response::json(&container(json!({"Directory": [
                {"key": "1", "type": "movie", "title": "Movies", "agent": "tv.plex.agents.movie", "scanner": "Plex Movie", "language": "en-US", "refreshing": false, "allowSync": false},
                {"key": "2", "type": "show", "title": "TV Shows", "agent": "tv.plex.agents.series", "scanner": "Plex TV Series", "language": "en-US", "refreshing": false, "allowSync": false},
            ]}))),
            (_, ["library", "sections", k, rest @ ..]) => {
                self.section(req, k.parse().unwrap_or(0), rest)
            }
            (_, ["library", "metadata", rk]) => {
                if let Some(key) = tmdb_rating_key(rk) {
                    if let Some(row) = self.item(key) {
                        return Response::json(&container(json!({"Metadata": [row]})));
                    }
                }
                if let Ok(key) = rk.parse::<i64>() {
                    if let Some(collection_id) = synthetic_collection_id(key) {
                        let Some(collection) = self.tmdb_collection(collection_id) else {
                            return Response::json_status(
                                503,
                                &container(json!({
                                    "Metadata": [], "error": "This collection is not available."
                                })),
                            );
                        };
                        return Response::json(&container(json!({
                            "Metadata": [self.collection_row(&collection)]
                        })));
                    }
                    if let Some(tmdb_id) = synthetic_show_tmdb_id(key) {
                        let Some(client) = TmdbClient::from_env() else {
                            return Response::json_status(
                                503,
                                &container(json!({
                                    "Metadata": [], "error": "TMDB_API_KEY is not configured"
                                })),
                            );
                        };
                        return match client.tv_details(tmdb_id) {
                            Ok(details) => Response::json(&container(json!({
                                "Metadata": [self.tmdb_show_details_row(&details, key)]
                            }))),
                            Err(error) => Response::json_status(
                                503,
                                &container(json!({
                                    "Metadata": [],
                                    "error": format!("TMDB TV details lookup failed: {error}")
                                })),
                            ),
                        };
                    }
                    if let Some(tmdb_id) = synthetic_tmdb_id(key) {
                        let Some(client) = TmdbClient::from_env() else {
                            return Response::json_status(
                                503,
                                &container(json!({
                                    "Metadata": [], "error": "TMDB_API_KEY is not configured"
                                })),
                            );
                        };
                        return match client.movie_details(tmdb_id) {
                            Ok(details) => Response::json(&container(json!({
                                "Metadata": [self.tmdb_details_row(&details, key)]
                            }))),
                            Err(error) => Response::json_status(
                                503,
                                &container(json!({
                                    "Metadata": [],
                                    "error": format!("TMDB movie details lookup failed: {error}")
                                })),
                            ),
                        };
                    }
                }
                let mut rows: Vec<Value> = rk
                    .split(',')
                    .filter_map(|x| x.parse().ok())
                    .filter_map(|k| self.person_row(k).or_else(|| self.item(k)))
                    .collect();
                if rows.is_empty() {
                    return Response::json_status(404, &container(json!({"Metadata": []})));
                }
                for row in &mut rows {
                    if let Some(key) = row["ratingKey"]
                        .as_str()
                        .and_then(|value| value.parse().ok())
                    {
                        self.enrich_row(key, row);
                    }
                }
                Response::json(&container(json!({"Metadata": rows})))
            }
            (_, ["library", "metadata", rk, "children"]) => {
                let key = rk.parse().ok().or_else(|| tmdb_rating_key(rk)).unwrap_or(0);
                Response::json(&container(json!({"Metadata": self.children(key)})))
            }
            (_, ["library", "metadata", rk, "allLeaves"]) => Response::json(&container(
                json!({"Metadata": self.leaves(rk.parse().unwrap_or(0))}),
            )),
            (_, ["library", "metadata", rk, "related"]) => {
                let key = rk.parse().ok().or_else(|| tmdb_rating_key(rk)).unwrap_or(-1);
                self.related_hub(key)
            }
            (_, ["library", "metadata", _, ..]) => ok_empty(),
            ("PUT", ["library", "parts", ..]) => ok_empty(),
            (_, ["library", "parts", pid, ..]) => self.part(pid.parse().unwrap_or(-1)),
            (_, ["library", "streams", sid]) => {
                self.subtitle(sid.split('.').next().unwrap_or("").parse().unwrap_or(-1))
            }
            // A Cast & Crew headshot's person page reads its two shelves from
            // `/library/people/{personId}/media`. A TMDB person is addressed by a synthetic key and
            // the filmography is the answer; any other id is a library person this shim has never
            // had, which is an empty container — an answer, not an error.
            (_, ["library", "people", id, "media"]) => {
                let rows = id
                    .parse()
                    .ok()
                    .and_then(synthetic_person_id)
                    .and_then(|person_id| {
                        let credits = TmdbClient::from_env()?.person_credits(person_id).ok()?;
                        Some(self.person_credit_rows(person_id, &credits))
                    })
                    .unwrap_or_default();
                Response::json(&container(json!({"Metadata": rows})))
            }
            (_, ["library", "collections", rk, "children"]) => {
                let collection = rk
                    .parse()
                    .ok()
                    .and_then(synthetic_collection_id)
                    .and_then(|id| self.tmdb_collection(id));
                let Some(collection) = collection else {
                    return Response::json_status(
                        404,
                        &container(json!({"Metadata": [], "error": "This collection is not available."})),
                    );
                };
                let (start, size) = page_params(req);
                Response::json(&self.collection_children(&collection, start, size.unwrap_or(60)))
            }
            (_, ["library", "collections", ..]) | (_, ["library", "people", ..]) => {
                Response::json(&container(json!({"Metadata": []})))
            }

            (_, ["hubs"]) | (_, ["hubs", "promoted"]) => {
                let count = req
                    .q("count")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(12usize)
                    .clamp(1, 50);
                let mut hubs = vec![self.continue_hub(count, None)];
                if req.q("excludeContinueWatching") == Some("1") {
                    hubs.clear();
                }
                if let Some(list) = self.saved_hub(count) {
                    hubs.push(list);
                }
                hubs.extend(self.catalog_hubs(|_| true, count, true, HUBS_BUDGET));
                Response::json(&container(json!({"Hub": hubs})))
            }
            (_, ["hubs", "continueWatching"]) => {
                let count = req
                    .q("count")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(12usize);
                Response::json(&container(json!({"Hub": [self.continue_hub(count, None)]})))
            }
            (_, ["hubs", "home", "recentlyAdded"]) => {
                let section = if req.q("type") == Some("2") || req.q("type") == Some("4") {
                    SECTION_SHOWS
                } else {
                    SECTION_MOVIES
                };
                let (start, size) = page_params(req);
                self.catalog_listing(self.first_catalog(section), None, start, size.unwrap_or(12))
            }
            (_, ["hubs", "sections", k]) => {
                let section: i64 = k.parse().unwrap_or(0);
                let count = req
                    .q("count")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(12usize)
                    .clamp(1, 50);
                let mut hubs = vec![self.continue_hub(count, Some(section))];
                hubs.extend(self.catalog_hubs(
                    |c| (section == SECTION_SHOWS) == is_series(&c.type_),
                    count,
                    false,
                    HUBS_BUDGET,
                ));
                Response::json(&container(json!({"Hub": hubs})))
            }
            (_, ["hubs", "nuvio", "list"]) => {
                let (start, size) = page_params(req);
                match self.account.saved_library(true) {
                    Ok(items) => {
                        let rows: Vec<_> = items
                            .iter()
                            .skip(start)
                            .take(size.unwrap_or(50))
                            .map(|p| self.preview_row(p))
                            .collect();
                        Response::json(&container(
                            json!({"Metadata":rows,"totalSize":items.len(),"offset":start}),
                        ))
                    }
                    Err(_) => Response::json_status(
                        503,
                        &container(json!({"error":"My List is unavailable"})),
                    ),
                }
            }
            ("GET" | "POST", ["nuvio", "library"]) => self.saved_item(req),
            (_, ["hubs", "nuvio", i]) => {
                let (start, size) = page_params(req);
                let cat = i
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| self.catalog_list().into_iter().nth(i));
                self.catalog_listing(cat, None, start, size.unwrap_or(50))
            }
            (_, ["hubs", "search"]) => self.search(
                req.q("query").unwrap_or(""),
                req.q("limit").and_then(|v| v.parse().ok()).unwrap_or(10),
            ),

            (_, ["photo", ":", "transcode"]) => self.photo(req),

            (_, [":", "timeline"]) | (_, [":", "progress"]) => {
                let rk = req
                    .q("ratingKey")
                    .and_then(|v| v.parse().ok())
                    .or_else(|| req.q("key").and_then(trailing_key));
                let time: Option<i64> = req.q("time").and_then(|v| v.parse().ok());
                if let (Some(rk), Some(t)) = (rk, time) {
                    let dur = req.q("duration").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let state = req.q("state").unwrap_or("playing").to_string();
                    if t > 0 && state != "stopped" || state == "stopped" && t > 0 {
                        let me = self.clone();
                        // The row snapshot may need a meta fetch: never on the reporting request.
                        std::thread::spawn(move || {
                            let row = me.row(rk);
                            me.watch.progress(rk, t, dur, row);
                            me.push_watch_progress(rk, t, dur, &state);
                        });
                    }
                }
                ok_empty()
            }
            (_, [":", "scrobble"]) | (_, [":", "unscrobble"]) => {
                if let Some(rk) = req
                    .q("key")
                    .and_then(trailing_key)
                    .or_else(|| req.q("ratingKey").and_then(|v| v.parse().ok()))
                {
                    self.watch.scrobble(rk, p == "/:/scrobble");
                    if let Some(Ident::Title { type_, id }) = self
                        .keys
                        .ident(rk)
                        .filter(|i| matches!(i, Ident::Title { type_, .. } if is_series(type_)))
                    {
                        // Marking a show watched marks every episode.
                        if let Some(m) = self.meta(&type_, &id) {
                            for (_, _, v) in episodes_of(&m) {
                                self.watch
                                    .scrobble(self.episode_key(&id, &v.id), p == "/:/scrobble");
                            }
                        }
                    }
                    if let Some(Ident::Season { series, n }) = self.keys.ident(rk) {
                        if let Some((_, m)) = self.series_of(&series) {
                            for (_, _, v) in episodes_of(&m).into_iter().filter(|(s, _, _)| *s == n)
                            {
                                self.watch
                                    .scrobble(self.episode_key(&series, &v.id), p == "/:/scrobble");
                            }
                        }
                    }
                }
                ok_empty()
            }
            (_, ["actions", "removeFromContinueWatching"]) => {
                if let Some(rk) = req.q("ratingKey").and_then(|v| v.parse().ok()) {
                    self.watch.clear_progress(rk);
                    // A cleared resume point must not come back on the next pull: overwrite the
                    // account's copy with a zero position, which `merge_remote` ignores on the way
                    // back. ponytail: Nuvio's RPC has no delete verb, so the row stays at 0.
                    self.push_watch_progress(rk, 0, 0, "stopped");
                }
                ok_empty()
            }
            (_, ["playQueues"]) => self.play_queue(req),
            (_, ["playQueues", ..]) => ok_empty(),
            (_, ["video", ":", "transcode", "universal", "decision"]) => self.decision(req),
            (_, ["video", ":", "transcode", "universal", "start", ..])
            | (_, ["video", ":", "transcode", "universal", "start.m3u8"]) => Response::bytes(
                404,
                "text/plain",
                b"no transcoder: streams play direct".to_vec(),
            ),
            (_, ["video", ..]) | (_, ["status", ..]) => ok_empty(),

            (_, ["nuvio", "streams"]) => {
                let rk = req
                    .q("ratingKey")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(-1);
                // A synthetic related key has no ident of its own: addons know the title as
                // `tmdb:<id>`, which is also the id the subtitles are asked for.
                let Some(ident) = self.keys.ident(rk).or_else(|| synthetic_ident(rk)) else {
                    return Response::not_found();
                };
                // `done` from `ranked` is authoritative: it is true when there is nothing to ask an
                // addon for (a related show has no episode yet, or no slot could be started). The
                // slot map cannot answer that — an absent slot must NOT read as "still loading".
                // The picker polls this route while addon workers report batches. Never wait here:
                // return the streams cached so far and let the next poll observe later addons.
                let (ranked, _, done) = self.ranked(rk, &ident, None, Duration::ZERO, false);
                Response::json(&json!({"ratingKey": rk, "complete": done,
                    "streams": ranked.iter().map(|(i, c)| shim_media::summary_json(*i, c)).collect::<Vec<_>>()}))
            }
            (_, ["nuvio", "select"]) => {
                let rk = req
                    .q("ratingKey")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(-1);
                let idx: Option<usize> = req.q("index").and_then(|v| v.parse().ok());
                match (
                    self.streams
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&rk),
                    idx,
                ) {
                    (Some(slot), Some(i)) => {
                        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
                        if i >= state.streams.len() {
                            return Response::json_status(404, &json!({"ok": false}));
                        }
                        state.selected = Some(i);
                        Response::json(&json!({"ok": true}))
                    }
                    _ => Response::json_status(404, &json!({"ok": false})),
                }
            }
            _ => {
                eprintln!("pmsshim: unhandled {} {}", req.method, req.path);
                ok_empty()
            }
        }
    }

    fn catalog_listing(
        &self,
        cat: Option<CatalogInfo>,
        genre: Option<&str>,
        start: usize,
        size: usize,
    ) -> Response {
        let Some(cat) = cat else {
            return Response::json(&container(
                json!({"Metadata": [], "totalSize": 0, "offset": start}),
            ));
        };
        let (rows, total) = self.catalog_range(&cat.key, genre, start, size.min(200));
        let rows: Vec<Value> = rows.iter().map(|p| self.preview_row(p)).collect();
        Response::json(&container(
            json!({"Metadata": rows, "totalSize": total, "offset": start}),
        ))
    }

    fn section(&self, req: &Request, k: i64, rest: &[&str]) -> Response {
        if k != SECTION_MOVIES && k != SECTION_SHOWS {
            return Response::json_status(404, &container(json!({})));
        }
        let cat = self.first_catalog(k);
        match rest {
            ["all"] | ["recentlyAdded"] | ["newest"] => {
                let (start, size) = page_params(req);
                let genre = req.q("genre").filter(|g| !g.is_empty());
                let mut resp = self.catalog_listing(cat, genre, start, size.unwrap_or(60));
                if req.q("includeMeta") == Some("1") {
                    if let Ok(mut v) = serde_json::from_slice::<Value>(&resp.body) {
                        let kind = if k == SECTION_SHOWS { "show" } else { "movie" };
                        v["MediaContainer"]["Meta"] = json!({"Type": [{"key": format!("/library/sections/{k}/all?type={}", if k == SECTION_SHOWS { 2 } else { 1 }),
                            "type": kind, "title": kind, "active": true,
                            "Sort": [{"key": "titleSort", "defaultDirection": "asc", "title": "Popular"}]}]});
                        resp = Response::json(&v);
                    }
                }
                resp
            }
            ["genre"] => {
                let genres = cat.map(|c| c.genres).unwrap_or_default();
                let dirs: Vec<Value> = genres
                    .iter()
                    .map(|g| json!({"key": g, "title": g, "fastKey": format!("/library/sections/{k}/all?genre={}", pct_encode(g))}))
                    .collect();
                Response::json(&container(json!({"Directory": dirs})))
            }
            ["firstCharacter"] => {
                Response::json(&container(json!({"Directory": [], "Metadata": []})))
            }
            // The section's collection listing: what a member's `collection.related` hub (a TMDB
            // franchise shelf) resolves its `tagId` against. Only the franchises this shim has
            // served are known — there is no library scan to list from — which always includes the
            // one whose heading was just pressed, since its shelf came from this same process.
            // Ordered by id so a page's `start` means the same rows twice.
            ["collections"] => {
                let (start, size) = page_params(req);
                let mut known: Vec<TmdbCollection> = self
                    .collections
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .values()
                    .cloned()
                    .collect();
                known.sort_by_key(|collection| collection.id);
                let rows: Vec<Value> = known
                    .iter()
                    .skip(start)
                    .take(size.unwrap_or(60))
                    .map(|collection| self.collection_row(collection))
                    .collect();
                let mut out = container(json!({"Metadata": rows}));
                out["MediaContainer"]["totalSize"] = json!(known.len());
                Response::json(&out)
            }
            _ => Response::json(&container(json!({"Directory": []}))),
        }
    }

    fn children(&self, rk: i64) -> Vec<Value> {
        if let Some(tmdb_id) = synthetic_show_tmdb_id(rk) {
            let client = TmdbClient::from_env();
            let mut meta = client
                .as_ref()
                .and_then(|client| client.tv_external_ids(tmdb_id).ok().flatten())
                .and_then(|imdb_id| self.meta("series", &imdb_id));
            if meta.is_none() {
                meta = self.meta("series", &format!("tmdb:{tmdb_id}"));
            }
            let Some(m) = meta else { return Vec::new() };
            return seasons_of(&m)
                .into_iter()
                .map(|n| self.season_row(rk, &m, n))
                .collect();
        }
        match self.keys.ident(rk) {
            Some(Ident::Title { type_, id }) if is_series(&type_) => {
                let Some(m) = self.meta(&type_, &id) else {
                    return Vec::new();
                };
                seasons_of(&m)
                    .into_iter()
                    .map(|n| self.season_row(rk, &m, n))
                    .collect()
            }
            Some(Ident::Season { series, n }) => {
                let Some((show, m)) = self.series_of(&series) else {
                    return Vec::new();
                };
                episodes_of(&m)
                    .into_iter()
                    .filter(|(s, _, _)| *s == n)
                    .map(|(s, e, v)| self.episode_row(show, &m, s, e, v))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    fn leaves(&self, rk: i64) -> Vec<Value> {
        if let Some(tmdb_id) = synthetic_show_tmdb_id(rk) {
            let client = TmdbClient::from_env();
            let mut meta = client
                .as_ref()
                .and_then(|client| client.tv_external_ids(tmdb_id).ok().flatten())
                .and_then(|imdb_id| self.meta("series", &imdb_id));
            if meta.is_none() {
                meta = self.meta("series", &format!("tmdb:{tmdb_id}"));
            }
            let Some(m) = meta else { return Vec::new() };
            return episodes_of(&m)
                .into_iter()
                .map(|(s, e, v)| self.episode_row(rk, &m, s, e, v))
                .collect();
        }
        match self.keys.ident(rk) {
            Some(Ident::Title { type_, id }) if is_series(&type_) => {
                let Some(m) = self.meta(&type_, &id) else {
                    return Vec::new();
                };
                episodes_of(&m)
                    .into_iter()
                    .map(|(s, e, v)| self.episode_row(rk, &m, s, e, v))
                    .collect()
            }
            Some(Ident::Season { .. }) => self.children(rk),
            _ => Vec::new(),
        }
    }

    fn search(self: &Arc<Self>, query: &str, limit: usize) -> Response {
        let q = query.trim();
        let mut movies: Vec<Value> = Vec::new();
        let mut shows: Vec<Value> = Vec::new();
        if !q.is_empty() {
            let mut seen = std::collections::HashSet::new();
            for (_, rows) in self.backend.search(q) {
                for p in rows {
                    if !seen.insert((p.type_.clone(), p.id.clone())) {
                        continue;
                    }
                    let row = self.preview_row(&p);
                    if is_series(&p.type_) {
                        shows.push(row)
                    } else {
                        movies.push(row)
                    }
                }
            }
        }
        let limit = limit.max(1);
        movies.truncate(limit);
        shows.truncate(limit);
        let hub = |kind: &str, rows: Vec<Value>| json!({"title": kind, "type": kind, "hubIdentifier": kind, "size": rows.len(), "Metadata": rows});
        Response::json(&container(
            json!({"Hub": [hub("movie", movies), hub("show", shows),
            {"title": "episode", "type": "episode", "hubIdentifier": "episode", "size": 0, "Metadata": []}]}),
        ))
    }

    fn play_queue(&self, req: &Request) -> Response {
        let rk = req
            .q("uri")
            .and_then(trailing_key)
            .or_else(|| req.q("key").and_then(trailing_key))
            .unwrap_or(-1);
        // The play is about to start: wait for the streams and probe the lead, so the queue's row
        // carries the real fields rather than the detail page's early guesses.
        let Some((first, _)) = self.item_probed(rk, Some(0)) else {
            return Response::json(&container(json!({"Metadata": [], "playQueueID": 1})));
        };
        let mut rows = vec![first];
        if req.q("continuous") == Some("1") {
            if let Some(Ident::Episode { series, video }) = self.keys.ident(rk) {
                if let Some((show, m)) = self.series_of(&series) {
                    let eps = episodes_of(&m);
                    if let Some(at) = eps.iter().position(|(_, _, v)| v.id == video) {
                        rows.extend(
                            eps[at + 1..]
                                .iter()
                                .filter(|(s, _, _)| *s != 0)
                                .take(20)
                                .map(|(s, e, v)| self.episode_row(show, &m, *s, *e, v)),
                        );
                    }
                }
            }
        }
        let rows: Vec<Value> = rows
            .into_iter()
            .enumerate()
            .map(|(n, mut r)| {
                r["playQueueItemID"] = json!(n + 1);
                r
            })
            .collect();
        let total = rows.len();
        Response::json(&container(
            json!({"Metadata": rows, "playQueueID": 1, "playQueueSelectedItemID": 1,
            "playQueueSelectedItemOffset": 0, "playQueueTotalCount": total, "playQueueVersion": 1}),
        ))
    }

    fn part(&self, pid: i64) -> Response {
        if pid < 0 {
            return Response::not_found();
        }
        let (rk, idx) = (pid / PART_STRIDE, (pid % PART_STRIDE) as usize);
        let Some(ident) = self.keys.ident(rk).or_else(|| {
            let client = TmdbClient::from_env()?;
            resolve_imdb_id(rk, &client).map(|id| Ident::Title {
                type_: if synthetic_show_tmdb_id(rk).is_some() {
                    "series".into()
                } else {
                    "movie".into()
                },
                id,
            })
        }) else {
            return Response::not_found();
        };
        // Make sure the slot exists (a resume after a restart lands here first) and has waited.
        // A pick already made ends that wait: the player is opening THAT file, and no addon still
        // answering can change it.
        let (ranked, _, _) = self.ranked(rk, &ident, None, self.cfg.stream_deadline, true);
        let stream = if idx == BEST_PART {
            ranked.first().map(|(_, c)| c.stream.clone())
        } else {
            let slot = self
                .streams
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&rk)
                .cloned();
            slot.and_then(|s| {
                s.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .streams
                    .get(idx)
                    .cloned()
            })
        };
        let url = stream.as_ref().and_then(|s| {
            let u = shim_media::playable_url(s, self.torrent_server().as_deref())?;
            self.open_url(rk, s, &u)
        });
        match url {
            Some(url) => Response::redirect(&url),
            None => Response::bytes(502, "text/plain", b"stream no longer available".to_vec()),
        }
    }

    fn subtitle(&self, sid: i64) -> Response {
        let Some(url) = self
            .sub_urls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&sid)
            .cloned()
        else {
            return Response::not_found();
        };
        match plx_net::net::request(
            &url,
            &[],
            "GET",
            None,
            plx_net::net::API,
            true,
            Some(4 << 20),
            None,
        ) {
            Some(r) if r.ok() => Response::bytes(
                200,
                "text/plain; charset=utf-8",
                crate::shim_subs::to_utf8_text(r.body),
            ),
            _ => Response::bytes(502, "text/plain", b"subtitle fetch failed".to_vec()),
        }
    }

    fn photo(&self, req: &Request) -> Response {
        let Some(source) = req.q("url") else {
            return Response::not_found();
        };
        // PMS artwork callers may send an absolute CDN URL or the item's local art path.
        let local = source.split('?').next().unwrap_or(source);
        let pieces: Vec<_> = local.trim_matches('/').split('/').collect();
        let resolved = match pieces.as_slice() {
            ["library", "metadata", rk, kind @ ("thumb" | "art" | "clearLogo")] => {
                // A synthetic related key has no `Ident` in the key table, so `row` alone answers
                // `None` — and its clearLogo is the one piece of art the app asks for BY PATH
                // (`logo_key` in `app/adapters/poster.rs` always builds `/library/metadata/{rk}/
                // clearLogo`), which is why a Related title drew text where a catalog one drew its
                // logotype. The row is rebuilt from TMDB exactly as `/library/metadata/{rk}` does
                // (one cached request), and its `clearLogo` URL then takes the proxy path below.
                rk.parse()
                    .ok()
                    .and_then(|rk| self.row(rk).or_else(|| self.tmdb_synthetic_row(rk)))
                    .and_then(|row| {
                        row[*kind]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                    })
            }
            _ => None,
        };
        let url = resolved.as_deref().unwrap_or(source);
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Response::not_found();
        }
        // A clearLogo is the one piece of art the app aligns by its EDGE (the hero and the player
        // HUD both anchor the logo's box), so the transparent margins a logotype is authored with
        // read as an offset. Only the logo is cropped: a poster or a backdrop owns its full frame.
        let logo_rk = logo_path_key(source);
        let width: u32 = req.q("width").and_then(|v| v.parse().ok()).unwrap_or(0);
        let height: u32 = req.q("height").and_then(|v| v.parse().ok()).unwrap_or(0);
        let sized = sized_image_url(url, width.max(height.saturating_mul(2) / 3));
        let fallback = self
            .poster_fallbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(url)
            .cloned();
        let mut urls = vec![sized];
        if let Some(original) = fallback {
            urls.push(sized_image_url(
                &original,
                width.max(height.saturating_mul(2) / 3),
            ));
        }
        let mut status = 502;
        for url in urls {
            if let Some(r) = plx_net::net::request(
                &url,
                &[],
                "GET",
                None,
                plx_net::net::API,
                true,
                Some(12 << 20),
                None,
            ) {
                if r.ok() && !r.body.is_empty() {
                    let content_type = sniff_image(&r.body);
                    // A PNG is the only flavour that carries an alpha border to cut; a JPEG logo
                    // has none and is served exactly as it came.
                    let body = match (logo_rk, content_type) {
                        (Some(rk), "image/png") => logo_png(rk, &url, (width, height), r.body),
                        _ => r.body,
                    };
                    return Response::bytes(200, content_type, body);
                }
                status = r.status.max(400);
            }
        }
        Response::bytes(status, "text/plain", Vec::new())
    }
}

/// **A title logo with its transparent border cut away, cached as the PNG the app will draw.**
/// The app aligns a logo by its box (`plx_ui::hero_logo::place`), and a TMDB/Stash logotype is
/// authored on a canvas padded with transparency — often more on one side — so the visible mark
/// lands off the edge it must line up with (the player HUD's `SB_X`, the hero's column). Cutting
/// the border to the ink's bounding box makes the box and the mark the same rectangle.
///
/// Crop-and-re-encode is real CPU work, so the result is stored in the app's own artwork cache
/// (`plx_platform::imgcache`) under an identity of its OWN (`classify_baked`, kind `logo-ink-v1`)
/// rather than the transcode slot the front end reads: an install that cached the uncropped bytes
/// before this existed must not be served them as if they were cropped. The identity is the item,
/// the logo's URL and the requested size — never the pixels — so the second request for a logo,
/// this run or after a reboot, is one file read.
fn logo_png(rk: &str, url: &str, size: (u32, u32), bytes: Vec<u8>) -> Vec<u8> {
    use plx_platform::imgcache;
    let key = imgcache::classify_baked(
        crate::shim_core::MACHINE_ID,
        "logo-ink-v1",
        rk,
        url,
        size.0.max(1),
        size.1.max(1),
    );
    let generation = imgcache::generation();
    if let Some(cached) = key.as_ref().and_then(|k| imgcache::read_at(generation, k)) {
        return cached.bytes;
    }
    let Some(cropped) = crop_ink(&bytes) else {
        // Nothing to cut (no alpha, or the ink already fills the canvas) or bytes that do not
        // decode: the source is the answer. Not cached — a decode failure must be retried, not
        // remembered as the crop.
        return bytes;
    };
    if let Some(key) = key.as_ref() {
        imgcache::write_at(generation, key, &cropped);
    }
    cropped
}

/// The alpha a pixel needs before it counts as ink. A 0 would crop to a stray anti-aliased fringe;
/// 1/255 is 0.4 % covered and invisible on a television.
const INK_ALPHA: u8 = 8;

/// `bytes` re-encoded as a PNG with its fully transparent border removed, or `None` when there is
/// nothing to cut: a source with no alpha channel (a JPEG), a canvas whose ink already reaches
/// every edge, or bytes that do not decode as an image.
fn crop_ink(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = image::load_from_memory(bytes).ok()?;
    if !image.color().has_alpha() {
        return None;
    }
    let rgba = image.to_rgba8();
    let (w, h) = rgba.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, px) in rgba.enumerate_pixels() {
        if px[3] < INK_ALPHA {
            continue;
        }
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x + 1);
        y1 = y1.max(y + 1);
    }
    // Nothing visible at all (`x1 == 0`) or the ink already fills the canvas: the source IS the
    // crop, and re-encoding it would only cost time.
    if x1 == 0 || (x0, y0, x1, y1) == (0, 0, w, h) {
        return None;
    }
    let cropped = image::imageops::crop_imm(&rgba, x0, y0, x1 - x0, y1 - y0).to_image();
    let mut png = Vec::new();
    cropped
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// The ratingKey when `source` is the app's own art path for a title LOGO —
/// `…/library/metadata/{rk}/clearLogo`, absolute (`http://host/…`) or the bare path the row's
/// `clearLogo` is fetched by. Nothing else (a poster, a backdrop, an addon URL) is a logo.
fn logo_path_key(source: &str) -> Option<&str> {
    let path = source.split('?').next().unwrap_or(source);
    let mut segs = path.trim_end_matches('/').rsplit('/');
    let (kind, rk, metadata) = (segs.next()?, segs.next()?, segs.next()?);
    (kind == "clearLogo" && metadata == "metadata" && !rk.is_empty()).then_some(rk)
}

fn sniff_image(b: &[u8]) -> &'static str {

    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

/// Ask the image CDNs addons use for a size near what the app draws, instead of the original
/// (a 2000x3000 TMDB poster costs the TV a lot to decode for a 250px card).
pub fn sized_image_url(url: &str, width: u32) -> String {
    if let Some(i) = url.find("image.tmdb.org/t/p/") {
        let head = &url[..i + "image.tmdb.org/t/p/".len()];
        let rest = &url[head.len()..];
        if let Some((_, tail)) = rest.split_once('/') {
            let size = match width {
                0 => "w780",
                w if w <= 185 => "w185",
                w if w <= 342 => "w342",
                w if w <= 500 => "w500",
                w if w <= 780 => "w780",
                w if w <= 1280 => "w1280",
                _ => "original",
            };
            return format!("{head}{size}/{tail}");
        }
    }
    if url.contains("images.metahub.space/") {
        for kind in ["poster", "background", "logo"] {
            for from in ["small", "medium", "large"] {
                let pat = format!("/{kind}/{from}/");
                if url.contains(&pat) {
                    let to = match (kind, width) {
                        ("poster", w) if w > 0 && w <= 400 => "medium",
                        ("poster", _) => "large",
                        ("background", w) if w > 0 && w <= 1280 => "medium",
                        ("background", _) => "large",
                        _ => "medium",
                    };
                    return url.replacen(&pat, &format!("/{kind}/{to}/"), 1);
                }
            }
        }
    }
    url.to_string()
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<Shim>();
    let _ = now_secs;
}

#[cfg(test)]
mod playback_regressions {
    use super::*;

    #[test]
    fn an_inflight_stream_list_cannot_direct_play_a_placeholder() {
        let shim = Arc::new(Shim::new(
            Arc::new(crate::shim_fake::FakeBackend),
            Config::default(),
        ));
        let rk = shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: "tt1254207".into(),
        });
        shim.streams.lock().unwrap().insert(
            rk,
            Arc::new(StreamSlot {
                started: Instant::now() - Duration::from_secs(90),
                state: Mutex::new(SlotState::default()),
                cv: Condvar::new(),
            }),
        );
        shim.probes
            .set_prober(Arc::new(|_, _| panic!("no stream exists to probe")));
        let request = Request {
            method: "GET".into(),
            path: "/video/:/transcode/universal/decision".into(),
            query: HashMap::from([("path".into(), format!("/library/metadata/{rk}"))]),
            headers: HashMap::new(),
            body: Vec::new(),
        };
        let response = shim.handle(&request);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["MediaContainer"]["generalDecisionCode"], 2000);
    }
    #[test]
    fn decision_carries_probed_hevc_dovi_tracks_and_refuses_missing_selection() {
        let shim = Arc::new(Shim::new(
            Arc::new(crate::shim_fake::FakeBackend),
            Config::default(),
        ));
        let rk = shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: "tt1254207".into(),
        });
        shim.streams.lock().unwrap().insert(
            rk,
            Arc::new(StreamSlot {
                started: Instant::now(),
                state: Mutex::new(SlotState {
                    streams: vec![Stream {
                        url: Some("https://example.invalid/media.mkv".into()),
                        ..Default::default()
                    }],
                    done: true,
                    ..Default::default()
                }),
                cv: Condvar::new(),
            }),
        );
        shim.probes.set_prober(Arc::new(|_, _| {
            Ok(crate::shim_probe::ProbeInfo {
                container: "mkv".into(),
                duration_ms: 90000,
                size: 1000,
                video_decodable: true,
                video: Some(crate::shim_probe::Video {
                    codec: "hevc".into(),
                    width: 3840,
                    height: 2160,
                    fps: 23.976,
                    dovi: Some(crate::shim_probe::Dovi {
                        profile: 8,
                        level: 6,
                        bl_compat: 1,
                        rpu: true,
                        bl: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                audio: vec![crate::shim_probe::Audio {
                    codec: "eac3".into(),
                    channels: 6,
                    decodable: true,
                    ..Default::default()
                }],
                subtitles: vec![crate::shim_probe::Sub {
                    codec: "subrip".into(),
                    language: "eng".into(),
                    ..Default::default()
                }],
            })
        }));
        let mut request = Request {
            method: "GET".into(),
            path: "/video/:/transcode/universal/decision".into(),
            query: HashMap::from([("path".into(), format!("/library/metadata/{rk}"))]),
            headers: HashMap::new(),
            body: vec![],
        };
        let response = shim.handle(&request);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        let media = &body["MediaContainer"]["Metadata"][0]["Media"][0];
        assert_eq!(media["videoCodec"], "hevc");
        assert_eq!(media["audioCodec"], "eac3");
        assert_eq!(media["Part"][0]["Stream"][0]["DOVIProfile"], 8);
        assert_eq!(media["Part"][0]["Stream"].as_array().unwrap().len(), 3);
        request.query.insert("mediaIndex".into(), "99".into());
        let response = shim.handle(&request);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["MediaContainer"]["generalDecisionCode"], 2000);
    }

    #[test]
    fn photo_endpoint_passes_through_source_image_bytes() {
        use std::io::{Read, Write};
        plx_net::net::global_init();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let image = b"\x89PNG\r\n\x1a\nfixture";
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut buffer = [0; 4096];
            let _ = stream.read(&mut buffer).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                image.len()
            )
            .unwrap();
            stream.write_all(image).unwrap();
        });
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let request = Request {
            method: "GET".into(),
            path: "/photo/:/transcode".into(),
            query: HashMap::from([
                ("url".into(), format!("http://{addr}/poster.png")),
                ("height".into(), u32::MAX.to_string()),
            ]),
            headers: HashMap::new(),
            body: vec![],
        };
        let response = shim.photo(&request);
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "image/png");
        assert_eq!(response.body, image);
        worker.join().unwrap();
    }

    /// **A logo's transparent border is cut away, and the cut is remembered.** The app anchors a
    /// logo by its box, so the padding a logotype is authored with was drawing the mark off the
    /// edge it must line up with. The second answer comes from `imgcache`: it is returned even
    /// though the bytes handed in this time are not an image at all, i.e. nothing was decoded again.
    #[test]
    fn a_title_logo_is_cropped_to_its_ink_and_the_crop_is_cached() {
        let url = format!("https://example.invalid/logo/{}.png", std::process::id());
        let padded = encode_rgba(40, 20, |x, y| ((10..30).contains(&x) && (5..15).contains(&y)).then_some([255, 255, 255, 255]));
        let cropped = logo_png("424242", &url, (600, 240), padded.clone());
        assert_ne!(cropped, padded, "the transparent border was served as it came");
        let drawn = image::load_from_memory(&cropped).unwrap().to_rgba8();
        assert_eq!(drawn.dimensions(), (20, 10), "the crop is not the ink's bounding box");
        assert_eq!(
            logo_png("424242", &url, (600, 240), b"not an image".to_vec()),
            cropped,
            "the second request decoded the source again instead of reading the cache"
        );
    }

    /// Whatever has no border to cut is served exactly as it came: an opaque image, a mark that
    /// already fills its canvas, and bytes that are not an image at all.
    #[test]
    fn only_a_padded_transparent_logo_is_re_encoded() {
        let opaque = encode_rgba(12, 12, |_, _| Some([9, 9, 9, 255]));
        assert!(super::crop_ink(&opaque).is_none(), "an opaque image has no border");
        let full = encode_rgba(12, 12, |_, _| Some([255, 0, 0, 255]));
        assert!(super::crop_ink(&full).is_none(), "ink already filling the canvas was re-encoded");
        assert!(super::crop_ink(b"\x89PNG\r\n\x1a\nnot really").is_none());
    }

    /// A blank `w`×`h` RGBA PNG whose pixel `(x, y)` is `ink(x, y)`, else transparent.
    fn encode_rgba(w: u32, h: u32, ink: impl Fn(u32, u32) -> Option<[u8; 4]>) -> Vec<u8> {
        let mut buf = image::RgbaImage::new(w, h);
        for (x, y, px) in buf.enumerate_pixels_mut() {
            *px = image::Rgba(ink(x, y).unwrap_or([0, 0, 0, 0]));
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// **The `photo` route is where the cut happens** — the app never asks for a logo by CDN URL,
    /// it asks for `/library/metadata/{rk}/clearLogo`, and that is the path this route sees.
    #[test]
    fn the_photo_route_crops_a_clear_logo_but_not_a_poster() {
        use std::io::{Read, Write};
        plx_net::net::global_init();
        let padded = encode_rgba(64, 32, |x, y| ((16..48).contains(&x) && (8..24).contains(&y)).then_some([255, 255, 255, 255]));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = padded.clone();
        let worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut buffer = [0; 4096];
                let _ = stream.read(&mut buffer).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    served.len()
                )
                .unwrap();
                stream.write_all(&served).unwrap();
            }
        });
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let art = |kind: &str| Request {
            method: "GET".into(),
            path: "/photo/:/transcode".into(),
            query: HashMap::from([
                ("url".into(), format!("http://{addr}/library/metadata/4242/{kind}")),
                ("width".into(), "600".into()),
                ("height".into(), "240".into()),
            ]),
            headers: HashMap::new(),
            body: vec![],
        };
        // A clearLogo path is cropped to its ink; a poster is not the app's title mark.
        let logo = shim.photo(&art("clearLogo"));
        assert_eq!(logo.status, 200);
        assert_eq!(logo.content_type, "image/png");
        assert_eq!(image::load_from_memory(&logo.body).unwrap().to_rgba8().dimensions(), (32, 16));
        let poster = shim.photo(&art("thumb"));
        assert_eq!(poster.body, padded, "an ordinary poster must be served as it came");
        worker.join().unwrap();
    }

    #[test]
    fn a_failed_poster_provider_falls_back_to_the_original_artwork() {
        use std::io::{Read, Write};

        plx_net::net::global_init();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let image = b"\x89PNG\r\n\x1a\noriginal";
        let worker = std::thread::spawn(move || {
            for status in [503, 200] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut buffer = [0; 4096];
                let _ = stream.read(&mut buffer).unwrap();
                let body: &[u8] = if status == 200 { image } else { b"" };
                write!(
                    stream,
                    "HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let provider = format!("http://{addr}/provider");
        shim.poster_fallbacks
            .lock()
            .unwrap()
            .insert(provider.clone(), format!("http://{addr}/original"));
        let response = shim.photo(&Request {
            method: "GET".into(),
            path: "/photo/:/transcode".into(),
            query: HashMap::from([("url".into(), provider)]),
            headers: HashMap::new(),
            body: vec![],
        });
        assert_eq!(response.status, 200);
        assert_eq!(response.body, image);
        worker.join().unwrap();
    }
}

/// Preserve the source's percent scale, and reject missing/invalid scores.
fn tomato_score(meta: &MetaPreview) -> Option<f64> {
    fn percent(v: &Value) -> Option<f64> {
        let score = v
            .as_f64()
            .or_else(|| v.as_str()?.trim().trim_end_matches('%').parse().ok())?;
        (score.is_finite() && (0.0..=100.0).contains(&score)).then_some(score)
    }
    meta.rotten_tomatoes_rating
        .as_deref()
        .and_then(|v| percent(&json!(v)))
        .or_else(|| percent(&meta.mdb_list_ratings["tomatoes"]))
        .or_else(|| {
            meta.ratings.as_array()?.iter().find_map(|r| {
                let source = r["source"].as_str()?.to_ascii_lowercase();
                matches!(
                    source.as_str(),
                    "tomatoes" | "rottentomatoes" | "rotten tomatoes"
                )
                .then(|| percent(&r["value"]))
                .flatten()
            })
        })
}

#[cfg(test)]
mod rating_tests {
    use super::*;
    #[test]
    fn metadata_provider_tomato_scores_keep_percent_units() {
        let m: MetaPreview =
            serde_json::from_value(json!({"ratings":[{"source":"tomatoes","value":94}]})).unwrap();
        assert_eq!(tomato_score(&m), Some(94.0));
        let m: MetaPreview =
            serde_json::from_value(json!({"mdbListRatings":{"tomatoes":"63%"}})).unwrap();
        assert_eq!(tomato_score(&m), Some(63.0));
        assert_eq!(tomato_score(&MetaPreview::default()), None);
        let m: MetaPreview = serde_json::from_value(json!({"rottenTomatoesRating":101})).unwrap();
        assert_eq!(tomato_score(&m), None);
    }
}

#[cfg(test)]
mod account_progress_tests {
    use super::*;

    #[test]
    fn a_playing_report_is_throttled_and_a_settled_one_is_not() {
        let now = Instant::now();
        assert!(watch_push_due(None, false, now));
        assert!(!watch_push_due(Some(now), false, now + Duration::from_secs(59)));
        assert!(watch_push_due(Some(now), false, now + WATCH_PUSH_INTERVAL));
        assert!(watch_push_due(Some(now), true, now));
    }

    #[test]
    fn an_episode_id_names_its_series() {
        assert_eq!(episode_series_id("tt0944947:1:2", "tmdb:1396"), "tt0944947");
        assert_eq!(episode_series_id("tmdb:1396:3:7", "tmdb:1396"), "tmdb:1396");
        // No season/episode numbers: the content id is the only series name there is.
        assert_eq!(episode_series_id("tt0944947", "tmdb:1396"), "tmdb:1396");
    }

    #[test]
    fn a_tmdb_id_needs_no_lookup_and_an_episode_id_is_stripped_to_its_series() {
        assert_eq!(tmdb_id("tmdb:550", false), Some(550));
        assert_eq!(tmdb_id("TMDB:1396", true), Some(1396));
        assert_eq!(tmdb_id("550", false), Some(550));
        assert_eq!(tmdb_id("tmdb:1396:3:7", true), Some(1396));
        assert_eq!(tmdb_id("tmdb:0", false), None);
        assert_eq!(tmdb_id("", false), None);
    }
}

/// Deterministic four-corner `UltraBlurColors` envelope derived from a poster URL's hash.
fn blur_colors(seed: &str) -> serde_json::Value {
    fn fnv(s: &str) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }
    fn hsl_to_hex(h: f32, s: f32, l: f32) -> String {
        let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = l - c / 2.0;
        let (r1, g1, b1) = if h < 60.0 {
            (c, x, 0.0)
        } else if h < 120.0 {
            (x, c, 0.0)
        } else if h < 180.0 {
            (0.0, c, x)
        } else if h < 240.0 {
            (0.0, x, c)
        } else if h < 300.0 {
            (x, 0.0, c)
        } else {
            (c, 0.0, x)
        };
        let r = ((r1 + m) * 255.0).round() as u8;
        let g = ((g1 + m) * 255.0).round() as u8;
        let b = ((b1 + m) * 255.0).round() as u8;
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    }
    let h = fnv(if seed.is_empty() { "nuvio" } else { seed });
    let mut corners: Vec<String> = vec![String::new(); 4];
    for i in 0..4u64 {
        let hue = (((h >> (i * 13)) as f32 / (1u64 << 13) as f32) * 360.0
            + (i as f32 * 97.0))
            % 360.0;
        let sat = 0.62 + (((h >> (i * 7 + 3)) & 0x7f) as f32 / 127.0) * 0.28;
        let light = 0.34 + (((h >> (i * 5 + 11)) & 0x3f) as f32 / 63.0) * 0.16;
        corners[i as usize] = hsl_to_hex(hue, sat, light);
    }
    serde_json::json!({
        "topLeft": corners[0],
        "topRight": corners[1],
        "bottomRight": corners[2],
        "bottomLeft": corners[3],
    })
}
