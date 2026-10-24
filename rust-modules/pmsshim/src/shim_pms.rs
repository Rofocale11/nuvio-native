//! The PMS routes. Every shape here is one `plx_plex::plex::models` deserialises; fields it does not
//! read are left out rather than invented.

use crate::shim_backend::{Backend, CatalogInfo};
use crate::shim_core::{Config, MACHINE_ID};
use crate::shim_http::{pct_encode, Request, Response};
use crate::shim_media::{self, Classified, SubRef, PART_STRIDE};
use crate::shim_store::{now_secs, Ident, Keys, Watch, WatchState};
use plx_stremio::addons_tmdb::{
    TmdbCast, TmdbClient, TmdbMovie, TmdbMovieDetails, TmdbShow, TmdbShowDetails, TmdbVideo,
};
use plx_stremio::model::{Meta, MetaPreview, Stream, Video};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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
const TMDB_FALLBACK_KEY_BASE: i64 = 9_500_000;
/// Synthetic keys for TMDB recommendations that no addon carries; above the movie base so the two
/// decoders cannot alias.
const TMDB_SHOW_KEY_BASE: i64 = 9_600_000;

/// A synthetic related-movie ratingKey's TMDB id.
fn synthetic_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_FALLBACK_KEY_BASE..TMDB_SHOW_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_FALLBACK_KEY_BASE) as u64)
}

/// A synthetic related-show ratingKey's TMDB id.
fn synthetic_show_tmdb_id(rk: i64) -> Option<u64> {
    (rk >= TMDB_SHOW_KEY_BASE).then_some((rk - TMDB_SHOW_KEY_BASE) as u64)
}

/// The Stremio id addons know a synthetic related ratingKey by (`tmdb:603`), if it is synthetic.
fn translate_to_stremio_id(rk: i64) -> Option<String> {
    synthetic_show_tmdb_id(rk)
        .or_else(|| synthetic_tmdb_id(rk))
        .map(|tmdb_id| format!("tmdb:{tmdb_id}"))
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
    metas: Mutex<HashMap<(String, String), (Instant, Arc<Meta>)>>,
    catalogs: Mutex<HashMap<(String, Option<String>), Arc<Mutex<CatalogCache>>>>,
    catalog_list: Mutex<Option<(Instant, Vec<CatalogInfo>)>>,
    streams: Mutex<HashMap<i64, Arc<StreamSlot>>>,
    sub_urls: Mutex<HashMap<i64, String>>,
    poster_fallbacks: Mutex<HashMap<String, String>>,
    tmdb_fallbacks: Mutex<HashMap<i64, TmdbMovie>>,
    next_sub: AtomicI64,
    permits: (Mutex<usize>, Condvar),
    /// Real stream metadata from the app's FFmpeg (installed at boot; absent on the host bin).
    pub probes: crate::shim_probe::ProbeCache,
    /// Torrent markers resolved on a streaming server (`/create`), by marker + ratingKey.
    resolved: Mutex<HashMap<(i64, String), String>>,
    torrent_server: std::sync::RwLock<Option<String>>,
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

fn tmdb_role(cast: &TmdbCast) -> Option<Value> {
    if cast.name.trim().is_empty() {
        return None;
    }
    let mut role = json!({"tag": cast.name, "role": cast.character});
    if let Some(thumb) = tmdb_image(cast.profile_path.as_deref()) {
        role["thumb"] = json!(thumb);
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
            name: "Actor Name".into(),
            character: "Character Name".into(),
            profile_path: Some("/actor.jpg".into()),
        };
        assert_eq!(
            tmdb_role(&cast).unwrap(),
            json!({
                "tag": "Actor Name",
                "role": "Character Name",
                "thumb": "https://image.tmdb.org/t/p/w300/actor.jpg"
            })
        );
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
        assert_eq!(fallback["ratingKey"], "9500999");

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
                    name: "Actor Name".into(),
                    character: "Character Name".into(),
                    profile_path: Some("/actor.jpg".into()),
                }],
            },
            ..Default::default()
        };
        let row = shim.tmdb_details_row(&details, 9_500_999);
        assert_eq!(row["ratingKey"], "9500999");
        assert_eq!(row["Role"][0]["tag"], "Actor Name");
        assert_eq!(row["Role"][0]["role"], "Character Name");
        assert_eq!(row["Genre"][0]["tag"], "Drama");
        assert_eq!(row["art"], "https://image.tmdb.org/t/p/w300/backdrop.jpg");
        assert_eq!(row["duration"], 7_380_000);
        assert_eq!(row["summary"], "TMDB summary");
        assert_eq!(row["year"], 2020);
        assert_eq!(row["originallyAvailableAt"], "2020-06-15");
    }

    #[test]
    fn synthetic_related_rating_keys_decode_to_tmdb_ids() {
        assert_eq!(synthetic_tmdb_id(9_500_603), Some(603));
        assert_eq!(synthetic_tmdb_id(9_499_999), None);
        // A show key must never decode as a movie id, or the two bases collide.
        assert_eq!(synthetic_tmdb_id(9_600_603), None);
        assert_eq!(synthetic_show_tmdb_id(9_600_603), Some(603));
        assert_eq!(synthetic_show_tmdb_id(9_599_999), None);
        assert_eq!(
            translate_to_stremio_id(9_500_603).as_deref(),
            Some("tmdb:603")
        );
        assert_eq!(
            translate_to_stremio_id(9_600_603).as_deref(),
            Some("tmdb:603")
        );
        assert_eq!(translate_to_stremio_id(12_345), None);
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
        assert_eq!(fallback["ratingKey"], "9601396");
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
                    name: "Actor Name".into(),
                    character: "Character Name".into(),
                    profile_path: Some("/actor.jpg".into()),
                }],
            },
            ..Default::default()
        };
        let row = shim.tmdb_show_details_row(&details, 9_601_396);
        assert_eq!(row["ratingKey"], "9601396");
        assert_eq!(row["key"], "/library/metadata/9601396");
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
        let response = shim.handle(&Request {
            method: "GET".into(),
            path: "/nuvio/streams".into(),
            query: HashMap::from([("ratingKey".into(), "9500603".into())]),
            headers: HashMap::new(),
            body: Vec::new(),
        });
        assert_eq!(response.status, 200);
        assert_eq!(
            backend.streams.lock().unwrap().as_slice(),
            [("movie".to_string(), "tmdb:603".to_string())]
        );
        for _ in 0..100 {
            if !backend.subtitles.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            backend.subtitles.lock().unwrap().as_slice(),
            [("movie".to_string(), "tmdb:603".to_string())]
        );
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
        let cfg_token = cfg.token.clone();
        Shim {
            backend,
            keys: Keys::open(dir.as_deref()),
            watch: WatchState::open(dir.as_deref()),
            cfg,
            metas: Mutex::new(HashMap::new()),
            catalogs: Mutex::new(HashMap::new()),
            catalog_list: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
            sub_urls: Mutex::new(HashMap::new()),
            poster_fallbacks: Mutex::new(HashMap::new()),
            tmdb_fallbacks: Mutex::new(HashMap::new()),
            next_sub: AtomicI64::new(900_000_000),
            permits: (Mutex::new(CATALOG_PARALLEL), Condvar::new()),
            probes: crate::shim_probe::ProbeCache::default(),
            resolved: Mutex::new(HashMap::new()),
            torrent_server: std::sync::RwLock::new(cfg_ts),
            audio_codecs: std::sync::RwLock::new(Vec::new()),
            me: std::sync::OnceLock::new(),
            account: crate::shim_account::Account::new(dir.as_deref(), &cfg_token),
        }
    }

    pub fn bind_self(self: &Arc<Self>) {
        let _ = self.me.set(Arc::downgrade(self));
    }

    /// The Stremio-compatible streaming server torrents play through, if any.
    pub fn torrent_server(&self) -> Option<String> {
        self.torrent_server
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
        // A synthetic related key exists only as `tmdb:<id>` to addons: streams and subtitles are
        // asked for that, never for the shim's own number.
        let id = translate_to_stremio_id(rk).unwrap_or(id);
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
        let (st, sid) = (type_.clone(), id.clone());
        // Subtitles ride beside the streams, never in front of them.
        let slot_subs = slot.clone();
        std::thread::Builder::new()
            .name("pmsshim-streams".into())
            .spawn(move || {
                let mut on_batch = |b: &[Stream]| {
                    let mut st = s2.state.lock().unwrap_or_else(|e| e.into_inner());
                    let room = (PART_STRIDE as usize - 1).saturating_sub(st.streams.len());
                    st.streams.extend(b.iter().take(room).cloned());
                    s2.cv.notify_all();
                };
                let all = backend.streams(&type_, &id, deadline, &mut on_batch);
                let mut st = s2.state.lock().unwrap_or_else(|e| e.into_inner());
                // A backend that only returns at the end (no batches) still lands here.
                if st.streams.is_empty() && !all.is_empty() {
                    st.streams = all.into_iter().take(PART_STRIDE as usize - 1).collect();
                }
                st.done = true;
                s2.cv.notify_all();
            })
            .ok()?;
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
                    })
                    .collect();
            });
        Some(slot)
    }

    /// The ranked playable streams of an item (waiting per the deadline/grace rules) and the
    /// subtitles known so far. Each entry carries its arrival index.
    fn ranked(
        &self,
        rk: i64,
        ident: &Ident,
        runtime_secs: Option<u32>,
        max_wait: Duration,
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
        let subs: Vec<SubRef> = st
            .subs
            .iter()
            .map(|s| SubRef {
                id: s.id,
                lang: s.lang.clone(),
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
            // `rank` clones; recover the arrival index by identity of the URL + name.
            let i = indexed
                .iter()
                .position(|s| {
                    shim_media::playable_url(s, ts).as_deref() == Some(c.url.as_str())
                        && s.name == c.stream.name
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
        });
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
            Err(error) => {
                eprintln!("pmsshim: TMDB metadata ID lookup failed: {error}");
                None
            }
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
                Err(error) => eprintln!("pmsshim: TMDB TV details lookup failed: {error}"),
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
            Ok(Err(error)) => eprintln!("pmsshim: TMDB movie details lookup failed: {error}"),
            Err(_) => eprintln!("pmsshim: TMDB movie details lookup thread panicked"),
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
            Ok(Err(error)) => {
                eprintln!("pmsshim: TMDB videos lookup failed: {error}");
                None
            }
            Err(_) => {
                eprintln!("pmsshim: TMDB videos lookup thread panicked");
                None
            }
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
        let rows: Vec<Value> = if series {
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
            details
                .recommendations
                .results
                .iter()
                .filter(|show| {
                    show.id > 0 && show.id <= i64::MAX as u64 - TMDB_SHOW_KEY_BASE as u64
                })
                .take(10)
                .map(|show| self.tmdb_show_recommendation_row(show))
                .collect()
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
            details
                .recommendations
                .results
                .iter()
                .filter(|movie| {
                    movie.id > 0 && movie.id <= i64::MAX as u64 - TMDB_FALLBACK_KEY_BASE as u64
                })
                .take(10)
                .map(|movie| self.tmdb_recommendation_row(movie))
                .collect()
        };
        Response::json(&container(json!({"Hub": [related_hub_json(kind, rows)]})))
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
        row
    }

    fn episode_key(&self, series: &str, video: &str) -> i64 {
        self.keys.key_of(&Ident::Episode {
            series: series.into(),
            video: video.into(),
        })
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
        let ident = ident.or_else(|| {
            fallback.as_ref().map(|movie| Ident::Title {
                type_: "movie".into(),
                id: format!("tmdb:{}", movie.id),
            })
        })?;
        let mut row = match fallback {
            Some(movie) => self.tmdb_fallback_row(&movie, rk),
            None => self.row(rk)?,
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
                self.ranked(rk, &ident, u32::try_from(dur / 1000).ok(), wait);
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
        let rk = req.q("path").and_then(trailing_key).unwrap_or(-1);
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
        let Some(Ident::Title { type_, id }) = req
            .q("ratingKey")
            .and_then(|v| v.parse().ok())
            .and_then(|rk| self.keys.ident(rk))
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
                let meta = self
                    .meta(&type_, &id)
                    .ok_or(plx_stremio::model::Error::Status(404))?;
                self.account.set_saved(&meta, saved)?;
            }
            let items = self.account.saved_library(req.method != "POST")?;
            Ok(items.iter().any(|p| p.type_ == type_ && p.id == id))
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
                if let Ok(key) = rk.parse::<i64>() {
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
                    .filter_map(|k| self.item(k))
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
            (_, ["library", "metadata", rk, "children"]) => Response::json(&container(
                json!({"Metadata": self.children(rk.parse().unwrap_or(0))}),
            )),
            (_, ["library", "metadata", rk, "allLeaves"]) => Response::json(&container(
                json!({"Metadata": self.leaves(rk.parse().unwrap_or(0))}),
            )),
            (_, ["library", "metadata", rk, "related"]) => {
                self.related_hub(rk.parse().unwrap_or(-1))
            }
            (_, ["library", "metadata", _, ..]) => ok_empty(),
            ("PUT", ["library", "parts", ..]) => ok_empty(),
            (_, ["library", "parts", pid, ..]) => self.part(pid.parse().unwrap_or(-1)),
            (_, ["library", "streams", sid]) => {
                self.subtitle(sid.split('.').next().unwrap_or("").parse().unwrap_or(-1))
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
                    let state = req.q("state").unwrap_or("playing");
                    if t > 0 && state != "stopped" || state == "stopped" && t > 0 {
                        let me = self.clone();
                        // The row snapshot may need a meta fetch: never on the reporting request.
                        std::thread::spawn(move || {
                            let row = me.row(rk);
                            me.watch.progress(rk, t, dur, row);
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
                let ident = self.keys.ident(rk).or_else(|| {
                    translate_to_stremio_id(rk).map(|id| Ident::Title {
                        type_: if synthetic_show_tmdb_id(rk).is_some() {
                            "series".into()
                        } else {
                            "movie".into()
                        },
                        id,
                    })
                });
                let Some(ident) = ident else {
                    return Response::not_found();
                };
                let (ranked, _, _) = self.ranked(rk, &ident, None, DETAIL_WAIT);
                let done = self
                    .streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&rk)
                    .is_some_and(|s| s.state.lock().map(|g| g.done).unwrap_or(true));
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
            ["firstCharacter"] | ["collections"] => {
                Response::json(&container(json!({"Directory": [], "Metadata": []})))
            }
            _ => Response::json(&container(json!({"Directory": []}))),
        }
    }

    fn children(&self, rk: i64) -> Vec<Value> {
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
        let Some(ident) = self.keys.ident(rk) else {
            return Response::not_found();
        };
        // Make sure the slot exists (a resume after a restart lands here first) and has waited.
        let (ranked, _, _) = self.ranked(rk, &ident, None, self.cfg.stream_deadline);
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
                rk.parse().ok().and_then(|rk| self.row(rk)).and_then(|row| {
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
                    return Response::bytes(200, sniff_image(&r.body), r.body);
                }
                status = r.status.max(400);
            }
        }
        Response::bytes(status, "text/plain", Vec::new())
    }
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
