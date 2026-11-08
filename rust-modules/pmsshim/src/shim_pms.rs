//! The PMS routes. Every shape here is one `plx_plex::plex::models` deserialises; fields it does not
//! read are left out rather than invented.

use crate::shim_backend::{Backend, CatalogInfo};
use crate::shim_core::{Config, MACHINE_ID};
use crate::shim_http::{pct_encode, Request, Response};
use crate::shim_media::{self, Classified, SubRef, PART_STRIDE};
use crate::shim_store::{
    now_secs, EnrichCache, Enrichment, Ident, Keys, LastStream, MetaCache, RelatedSource, Watch,
    WatchState,
};
use plx_stremio::addons_tmdb::{
    TmdbCast, TmdbClient, TmdbCollection, TmdbCredit, TmdbImages, TmdbMovie, TmdbMovieDetails,
    TmdbPerson, TmdbShow, TmdbShowDetails,
};
use plx_stremio::model::{Meta, MetaPreview, Stream, Video};
use serde_json::{json, Map, Value};
use std::cmp::Ordering as CmpOrdering;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How often a *playing* report may mirror to the Nuvio account. A paused or stopped report is a
/// real change and goes at once; the app reports every few seconds, and one API call per report
/// would be a call per ten seconds for the length of a film.
const WATCH_PUSH_INTERVAL: Duration = Duration::from_secs(60);

/// The `video_id` Nuvio's own clients write for an episode whose addon id they did not have.
/// The real episode is identified by the row's season/episode, not by this string.
const NUVIO_SYNTHETIC_EPISODE: &str = "__nuvio_episode__:";

const CATALOG_TTL: Duration = Duration::from_secs(15 * 60);
/// A failed catalog fill is not retried before this. A dead addon host must not be asked again on
/// every page read that follows the failure.
const CATALOG_RETRY: Duration = Duration::from_secs(30);
/// How long `/hubs` waits for catalogs that are not cached yet; late ones land in the cache for the
/// next fetch rather than holding Home up.
const HUBS_BUDGET: Duration = Duration::from_secs(4);
/// Catalog fetches in flight at once (the TV's network and CPU are shared with the UI).
const CATALOG_PARALLEL: usize = 8;
const STREAM_TTL: Duration = Duration::from_secs(30 * 60);
/// Rows a catalog listing reports beyond what it has fetched while the catalog is not exhausted,
/// so a grid keeps asking for the next page.
const MORE_HINT: i64 = 50;
/// The Part index that means "whichever stream leads when this is played".
const BEST_PART: usize = PART_STRIDE as usize - 1;
const DEFAULT_EPISODE_MS: i64 = 45 * 60_000;
const DEFAULT_MOVIE_MS: i64 = 110 * 60_000;
const SECTION_MOVIES: i64 = 1;
const SECTION_SHOWS: i64 = 2;
/// Where the C torrent engine's loopback serves, and the last-resort "torrents are available"
/// marker when no URL is configured: with the engine loaded the URL is never dialled, but the
/// stream list and the resolve gate both ask `torrent_server()` whether torrents may be offered.
const DEFAULT_P2P_SERVER: &str = "http://127.0.0.1:11470";
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
/// A trailer clip's ratingKey — a billion above the collection base and a billion
/// above every title base, so a clip key never decodes as a title, person or
/// collection. The clip is its own row (own `Media`/`Part`s) so `/decision` for
/// the clip never resolves the film's ranked streams.
const TRAILER_KEY_BASE: i64 = 5_000_000_000;

/// A synthetic related-movie ratingKey's TMDB id.
/// Whether a cached resolve for `marker` is still usable while the engine's active torrent is
/// `active`. A marker names its torrent (`torrent:<info hash>`), and the engine serves only the
/// torrent it last resolved: any other hash means the cached URL belongs to a torrent `remember`
/// deleted, whose `/stream/<id>` answers 404 for the rest of the session.
fn engine_url_is_current(marker: &str, active: Option<&str>) -> bool {
    let Some(hash) = marker.strip_prefix(shim_media::TORRENT_SCHEME) else {
        return true;
    };
    active.is_some_and(|a| a.eq_ignore_ascii_case(hash))
}

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

/// A synthetic collection ratingKey's TMDB collection id, bounded above by the
/// trailer base so a trailer clip key (5B+) never decodes as a collection.
fn synthetic_collection_id(rk: i64) -> Option<u64> {
    (TMDB_COLLECTION_KEY_BASE..TRAILER_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_COLLECTION_KEY_BASE) as u64)
}

/// A title may list several trailers (official, teaser, red-band). Each is its
/// own clip row, and its key sits one 10-billion-wide slot above the previous
/// one: index 0 is the historical `5B + parent`, so a stored key still
/// round-trips, and every later index is far from any title/person/collection
/// key. At most this many rows are advertised.
const TRAILER_KEY_SPAN: i64 = 10_000_000_000;
const MAX_TRAILERS: usize = 12;

/// The clip key for trailer `index` of `parent`.
fn trailer_rk_at(parent: i64, index: usize) -> i64 {
    TRAILER_KEY_BASE + parent + index as i64 * TRAILER_KEY_SPAN
}

/// The trailer clip key for a parent title's first trailer: `5B + parent`.
#[cfg(test)]
fn trailer_rk(parent: i64) -> i64 {
    trailer_rk_at(parent, 0)
}

/// `(parent, index)` behind any trailer clip key.
fn trailer_key_parts(rk: i64) -> Option<(i64, usize)> {
    let offset = rk.checked_sub(TRAILER_KEY_BASE)?;
    if offset < 0 {
        return None;
    }
    let index = (offset / TRAILER_KEY_SPAN) as usize;
    let parent = offset % TRAILER_KEY_SPAN;
    (index < MAX_TRAILERS).then_some((parent, index))
}

/// The parent title key behind the FIRST trailer clip key (`5B..10B`), so a
/// parent's own key (up to ~4B synthetic) round-trips and no collection,
/// person or title key can alias into the clip range.
#[cfg(test)]
fn trailer_parent_rk(rk: i64) -> Option<i64> {
    trailer_key_parts(rk)
        .filter(|(_, index)| *index == 0)
        .map(|(parent, _)| parent)
}

/// The parent title and trailer index behind a trailer **Part** id
/// (`clip * 1000 + 998/999`), or `None` for any other part. Lets `part`
/// resolve the exact trailer the player asked for, on its own thread.
fn trailer_part_at(pid: i64) -> Option<(i64, usize)> {
    let (clip, slot) = (pid / 1000, pid % 1000);
    if slot != 998 && slot != 999 {
        return None;
    }
    trailer_key_parts(clip)
}

/// One trailer the shim can offer: the YouTube id and the name TMDB/Cinemeta
/// gave it ("Official Trailer", "Teaser 2", …).
#[derive(Clone, Debug, PartialEq, Eq)]
struct TrailerCandidate {
    youtube_id: String,
    title: String,
}

/// Append a candidate unless its id is already listed. An empty title reads as
/// the generic "Official Trailer" rather than a blank tile.
fn push_candidate(rows: &mut Vec<TrailerCandidate>, youtube_id: String, title: &str) {
    if youtube_id.is_empty() || rows.iter().any(|row| row.youtube_id == youtube_id) {
        return;
    }
    let title = title.trim();
    rows.push(TrailerCandidate {
        youtube_id,
        title: if title.is_empty() {
            "Official Trailer".to_string()
        } else {
            title.to_string()
        },
    });
}

/// TMDB's `videos` list as shelf candidates: trailers first, then teasers.
fn video_candidates(videos: Option<&plx_stremio::addons_tmdb::TmdbVideos>) -> Vec<TrailerCandidate> {
    let mut out = Vec::new();
    let Some(videos) = videos else {
        return out;
    };
    for kind in ["trailer", "teaser"] {
        for video in videos.results.iter().filter(|video| {
            video.site.eq_ignore_ascii_case("youtube")
                && video.type_.eq_ignore_ascii_case(kind)
        }) {
            if let Some(yid) = youtube_id(&Value::String(video.key.clone())) {
                push_candidate(&mut out, yid, &video.name);
            }
        }
    }
    out
}

/// The GLOBAL person identity the shim's cast rows carry as `tagKey`. On a real PMS that is
/// plex.tv's person guid and Discover answers the biography and the filmography under it; under
/// Nuvio there is no plex.tv, so the shim — the one metadata provider the app has — names an id it
/// answers itself (`/library/people/{guid}` and `/credits`). The prefix keeps it disjoint from any
/// tagKey a real provider could send.
const PERSON_GUID_PREFIX: &str = "nuvio-person-";

fn person_guid(person_id: u64) -> String {
    format!("{PERSON_GUID_PREFIX}{person_id}")
}

fn person_id_of_guid(guid: &str) -> Option<u64> {
    guid.strip_prefix(PERSON_GUID_PREFIX)?
        .parse()
        .ok()
        .filter(|id| *id > 0)
}

/// The orders the shim can compute from a Stremio `MetaPreview` without another fetch. These are
/// the TMDB-facing baseline plus Title; a catalog's own order is the Popularity answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CatalogSort {
    Popularity,
    Rating,
    ReleaseDate,
    Title,
}

/// `sort=key[:asc|desc]`. Unknown keys are ignored so a stale saved view cannot empty a page.
fn parse_catalog_sort(raw: Option<&str>) -> Option<(CatalogSort, bool)> {
    let raw = raw?.trim();
    if raw.is_empty() {
        return None;
    }
    let (key, direction) = raw.split_once(':').unwrap_or((raw, ""));
    let sort = match key {
        "popularity" => CatalogSort::Popularity,
        "titleSort" | "title" => CatalogSort::Title,
        "rating" | "audienceRating" => CatalogSort::Rating,
        "releaseDate" | "originallyAvailableAt" | "year" => CatalogSort::ReleaseDate,
        _ => return None,
    };
    Some((sort, direction.eq_ignore_ascii_case("desc")))
}

/// The Sort menu advertised through `includeMeta=1` and `/sorts`. Popularity is the catalog's own
/// order (Cinemeta and Torrentio already rank by it); the other three are computed in memory.
/// Cinemeta only supports Popularity and Rating; other addons (TMDB, custom) also support
/// Release Date and Title.
fn sort_entries_for(addon_id: Option<&str>) -> Value {
    let cinemeta = addon_id == Some("com.linvo.cinemeta");
    if cinemeta {
        json!([
            {"key": "popularity", "defaultDirection": "desc", "title": "Popularity"},
            {"key": "rating", "defaultDirection": "desc", "title": "Rating"}
        ])
    } else {
        json!([
            {"key": "popularity", "defaultDirection": "desc", "title": "Popularity"},
            {"key": "rating", "defaultDirection": "desc", "title": "Rating"},
            {"key": "releaseDate", "defaultDirection": "desc", "title": "Release Date"},
            {"key": "titleSort", "defaultDirection": "asc", "title": "Title"}
        ])
    }
}

/// TMDB's standard movie genres. The shim offers these even when an addon declares no `genre`
/// extra, and filters the fetched rows in memory against the same names.
const TMDB_MOVIE_GENRES: &[&str] = &[
    "Action",
    "Adventure",
    "Animation",
    "Comedy",
    "Crime",
    "Documentary",
    "Drama",
    "Family",
    "Fantasy",
    "History",
    "Horror",
    "Music",
    "Mystery",
    "Romance",
    "Science Fiction",
    "TV Movie",
    "Thriller",
    "War",
    "Western",
];

/// TMDB's standard TV genres (they differ from the movie list in `help`/`format` only).
const TMDB_TV_GENRES: &[&str] = &[
    "Action & Adventure",
    "Animation",
    "Comedy",
    "Crime",
    "Documentary",
    "Drama",
    "Family",
    "Kids",
    "Mystery",
    "News",
    "Reality",
    "Sci-Fi & Fantasy",
    "Soap",
    "Talk",
    "War & Politics",
    "Western",
];

/// The TMDB baseline for a section's content type.
fn tmdb_genres(series: bool) -> &'static [&'static str] {
    if series {
        TMDB_TV_GENRES
    } else {
        TMDB_MOVIE_GENRES
    }
}

/// Fold a genre name to its comparison key: letters and digits only, lower-cased, with the two
/// spellings of science fiction made one. "Sci-Fi" and "Science Fiction" are the same genre.
fn genre_key(name: &str) -> String {
    let key: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if key == "scifi" {
        "sciencefiction".into()
    } else {
        key
    }
}

/// The `&`/`/` halves of a compound genre (`Action & Adventure`), each folded by [`genre_key`].
fn genre_parts(name: &str) -> Vec<String> {
    name.split(['&', '/'])
        .map(genre_key)
        .filter(|part| !part.is_empty())
        .collect()
}

/// `candidate` names the same genre as `wanted`. A compound TMDB name matches either of its
/// halves, and "Sci-Fi" matches "Science Fiction".
fn genre_matches(candidate: &str, wanted: &str) -> bool {
    let (have, want) = (genre_parts(candidate), genre_parts(wanted));
    !have.is_empty() && have.iter().any(|part| want.contains(part))
}

fn catalog_rating_of(meta: &MetaPreview) -> Option<f64> {
    let parse = |v: &str| v.trim().parse::<f64>().ok().filter(|n| n.is_finite());
    meta.imdb_rating
        .as_deref()
        .and_then(parse)
        .or_else(|| meta.rotten_tomatoes_rating.as_deref().and_then(parse))
}

fn catalog_year_of(meta: &MetaPreview) -> Option<i32> {
    let text = meta.release_info.as_deref()?.trim();
    let year = text.get(..4)?.parse::<i32>().ok()?;
    (1000..=3000).contains(&year).then_some(year)
}

fn compare_catalog(a: &MetaPreview, b: &MetaPreview, sort: CatalogSort) -> CmpOrdering {
    match sort {
        // The catalog already arrives in its own popularity order; sorting is a no-op.
        CatalogSort::Popularity => CmpOrdering::Equal,
        CatalogSort::Title => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        CatalogSort::Rating => match (catalog_rating_of(a), catalog_rating_of(b)) {
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => CmpOrdering::Less,
            (None, Some(_)) => CmpOrdering::Greater,
            (None, None) => CmpOrdering::Equal,
        },
        CatalogSort::ReleaseDate => match (catalog_date_of(a), catalog_date_of(b)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => CmpOrdering::Less,
            (None, Some(_)) => CmpOrdering::Greater,
            (None, None) => CmpOrdering::Equal,
        },
    }
}

/// The comparable release date (`YYYY-MM-DD`, else the year) as a string; missing dates sort last.
fn catalog_date_of(meta: &MetaPreview) -> Option<String> {
    let text = meta.release_info.as_deref()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(date) = text.get(..10) {
        if date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-') {
            return Some(date.to_string());
        }
    }
    catalog_year_of(meta).map(|year| format!("{year:04}"))
}

/// Merge a catalog's declared genre options with the TMDB baseline, de-duplicating spellings.
fn merged_genres(declared: &[String], series: bool) -> Vec<String> {
    let mut out: Vec<String> = declared
        .iter()
        .filter(|g| !g.trim().is_empty())
        .cloned()
        .collect();
    for genre in tmdb_genres(series) {
        if !out.iter().any(|have| genre_matches(have, genre)) {
            out.push((*genre).to_string());
        }
    }
    out
}

/// Resolve a request's `genre` value against the merged list: a display name stays as it is, a
/// numeric PMS tag id (what a real client sends) maps back to its name, and an unknown value is
/// kept so the in-memory filter still applies instead of silently listing everything.
fn resolve_genre<'a>(genres: &'a [String], raw: &'a str) -> &'a str {
    genres
        .iter()
        .map(String::as_str)
        .find(|genre| genre_matches(genre, raw) || tag_id(genre).to_string() == raw)
        .unwrap_or(raw)
}

/// Split a Stremio content id from its episode suffix: `tmdb:1396:1:2` → (`tmdb:1396`, `:1:2`).
fn split_content_id(id: &str) -> (&str, &str) {
    let base = plx_stremio::addons::content_level_id(id);
    if base.is_empty() || base == id {
        return (id, "");
    }
    match id.strip_prefix(base.as_str()) {
        Some(suffix) => (&id[..base.len()], suffix),
        None => (id, ""),
    }
}

/// The TMDB number in `tmdb:603`, or `None` for an id an addon already understands (`tt…`, `kitsu…`).
fn tmdb_content_number(id: &str) -> Option<u64> {
    id.strip_prefix("tmdb:")?.parse().ok().filter(|n| *n > 0)
}

/// `tt` followed by digits — the id form IMDb, Stremio addons and Nuvio's account share.
fn is_imdb_id(id: &str) -> bool {
    id.len() > 2 && id.starts_with("tt") && id[2..].bytes().all(|b| b.is_ascii_digit())
}

/// The season and episode an episode id ends in (`tt0944947:1:2`). `None` when it carries no such
/// tail (a bare series or film id).
fn episode_numbers(video_id: &str) -> (Option<i64>, Option<i64>) {
    let mut parts = video_id.rsplit(':');
    let episode = parts.next().and_then(|s| s.parse::<i64>().ok());
    let season = parts.next().and_then(|s| s.parse::<i64>().ok());
    match (season, episode) {
        (Some(s), Some(e)) if s >= 0 && e > 0 => (Some(s), Some(e)),
        _ => (None, None),
    }
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

/// One catalog window's cache. Readers answer from `rows` under a bounded wait; the network work
/// is done by one [`Shim::fill_catalog`] worker at a time (`filling`), which never holds this lock
/// across a fetch, so a request can always take the rows that have already landed.
struct CatalogCache {
    at: Instant,
    rows: Vec<MetaPreview>,
    exhausted: bool,
    /// One fill at a time; waiters sleep on `cv` and are woken when it ends.
    filling: bool,
    /// A failed fill is not retried before this instant.
    retry_at: Option<Instant>,
    /// Wakes the readers waiting for this fill. Behind an `Arc` so a waiter can hold it while it
    /// hands its guard to `wait_timeout` (the same guard cannot also carry the borrow).
    cv: Arc<Condvar>,
}

/// Rows a subtitle search may return. The page's language filter usually leaves far fewer; the
/// cap keeps one answer from minting thousands of `/library/streams/{id}` keys.
const SUBTITLE_SEARCH_MAX: usize = 60;
/// Installs remembered per item, and items remembered, so a long session cannot grow either map
/// without bound. The newest install is the one a later play starts with.
const INSTALLED_SUBS_PER_ITEM: usize = 20;
const INSTALLED_SUB_ITEMS: usize = 128;

/// One candidate the Search page returned, remembered by the id its key names: the install `PUT`
/// carries only `/library/streams/{id}`, so this is how it learns what it is attaching.
#[derive(Clone, Debug)]
struct SearchSub {
    id: i64,
    lang: String,
    title: String,
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
    /// Subtitle addons have reported (possibly empty). `ranked` may wait on this so a play does not
    /// start before the sidecars it must auto-enable have arrived.
    subs_done: bool,
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
    /// The addon metas this session has fetched, with the last few hours persisted beside the key
    /// table so a restart does not make the first detail open cold.
    metas: Arc<MetaCache>,
    /// Everything the detail page takes from TMDB — the cast and trailer `enrich_row` writes and
    /// the `More Like This`/franchise data `related_hub` builds — persisted so a restart does not
    /// pay for it again.
    enrich: Arc<EnrichCache>,
    /// One addon-meta fetch per `(type, id)` at a time: the focus prefetch and the detail click
    /// it was warming would otherwise each ask the addon for the same title.
    meta_flight: plx_base::flight::Flight,
    catalogs: Mutex<HashMap<(String, Option<String>), Arc<Mutex<CatalogCache>>>>,
    catalog_list: Mutex<Option<(Instant, Vec<CatalogInfo>)>>,
    /// The full browse list, same cache shape. **Every `/hubs/nuvio/{i}` index is an index into
    /// this list**, so Home and Recommended must both build their rows from it — a row's key has to
    /// page the catalog the row previewed.
    browse_list: Mutex<Option<(Instant, Vec<CatalogInfo>)>>,
    /// The last card the shim built for a rating key, by `ratingKey`. A LISTING read
    /// (`/library/metadata/{rk1},{rk2},…`) is a row of cards the shim itself just handed out, so
    /// answering it from here costs nothing; resolving it through the addon's `meta` + TMDB costs
    /// ~0.7 s a card, which is what made a Recommended page fill in one row a minute. A detail
    /// read (one key) never uses this.
    cards: Mutex<HashMap<i64, Value>>,
    streams: Mutex<HashMap<i64, Arc<StreamSlot>>>,
    sub_urls: Mutex<HashMap<i64, String>>,
    /// The candidates the Search page has handed out, by the id their key names ([`SearchSub`]).
    search_subs: Mutex<HashMap<i64, SearchSub>>,
    /// Subtitles the viewer installed from the Search page, by rating key. They are sidecars of
    /// the item from then on — `ranked` merges them beside the addons' own — for this process:
    /// nothing here persists, unlike the key table and watch state.
    installed_subs: Mutex<HashMap<i64, Vec<SearchSub>>>,
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
    /// Byte-offset reach (installed at boot; absent on the host bin). Proves a DASH pair can be
    /// fetched to its END before the DASH `Media` is published — see
    /// [`trailer_tracks_reachable`](Self::trailer_tracks_reachable).
    trailer_reach: Mutex<Option<crate::shim_probe::Reacher>>,
    /// Trailer Part targets `part_id -> resolved CDN URL` (the 360p muxed fallback
    /// beside a DASH pair, and the muxed row when there is no DASH pair). A DASH
    /// Part instead maps `part_id -> MPD XML` in `trailer_dash`; `part()` checks
    /// the DASH map first, then this one.
    trailer_parts: Mutex<HashMap<i64, String>>,
    /// Trailer DASH manifests `part_id -> MPD XML` (see [`dash_manifest`]). The
    /// Part itself is `file.mpd` and answers 200 `application/dash+xml`; the
    /// MPD's BaseURLs are the signed googlevideo URLs themselves — `curlio`
    /// states the bounded window those URLs require (see `GOOGLEVIDEO_WINDOW`
    /// in `media/src/curlio.rs`), and `trailer_proxy` carries no track bytes.
    trailer_dash: Mutex<HashMap<i64, String>>,
    /// `youtubeId -> resolved candidate` for [`crate::shim_trailer::resolve_cached`],
    /// with the 30-min/60-s TTL split [`crate::shim_trailer::TrailerCache`] owns.
    trailer_cache: crate::shim_trailer::TrailerCache,
    /// Single-flight gate for the same resolve: the `meta()` loop shape, so a
    /// focus prefetch and the click that follows spawn one Node child, not two.
    trailer_flight: plx_base::flight::Flight,
    /// Parents whose first trailer a background thread is resolving right now.
    /// The detail page's 2 s autoplay dwell is that thread's budget; without
    /// this set a burst of `/extras` reads would queue N waiters on the flight.
    #[cfg(not(test))]
    trailer_warm: Mutex<std::collections::HashSet<i64>>,
    /// The decipher seam. Production is the Node child (`NodeTrailerResolver`);
    /// tests swap in a fake. A `Box<dyn>` keeps the spawn machinery behind one
    /// trait so the shim stays testable without Node.
    trailer_resolver: Box<dyn crate::shim_trailer::TrailerResolver>,
    /// The persistent Node proxy child (`TrailerProxy`): IOS-UA loopback hop
    /// for the DASH pair's bytes. Lazy (`NotStarted` until the first DASH
    /// trailer) and degraded to direct googlevideo when it cannot start.
    // Unused after the direct-libcurl switch (track bytes now go via curlio's
    // googlevideo UA override; the Node proxy child's TLS/egress 403'd on TV).
    // Kept so the child and its docs/tests stay in the tree for a future
    // re-enable without a re-add.
    #[allow(dead_code)]
    trailer_proxy: crate::shim_trailer::TrailerProxy,
    /// Torrent markers resolved on a streaming server (`/create`), by marker + ratingKey.
    resolved: Mutex<HashMap<(i64, String), String>>,
    /// `tmdb:` content ids already asked of TMDB's `/external_ids`, by `(type, content id)`.
    /// `None` is a settled miss: addons get the catalog's own id, exactly as before.
    imdb_ids: Mutex<HashMap<(String, String), Option<String>>>,
    torrent_server: std::sync::RwLock<Option<String>>,
    /// Whether peer-to-peer streams may be offered at all (see [`Config::p2p_enabled`]). Read by
    /// [`Shim::torrent_server`], so every stream list, pick and resolve that goes through a
    /// streaming server is gated by this one switch.
    p2p_enabled: AtomicBool,
    /// Audio codecs this TV decodes (`devcaps`); empty = unknown, rank on video alone.
    audio_codecs: std::sync::RwLock<Vec<String>>,
    /// A monotonic count of changes the client should re-read: the addon set was replaced, a
    /// profile's resume points merged, or a boot warm filled the catalog caches. The app polls
    /// this once a frame (a worker may not touch the dispatcher) and refetches Home when it moves.
    /// Unlike a boolean it cannot lose a change that lands between two frames.
    content_gen: AtomicU64,
    /// A monotonic count of catalog fills that landed rows after a section listing answered with a
    /// partial window. Separate from [`Shim::content_gen`]: Home re-reads on that one, and a fill
    /// for the library must not repaint every Home shelf. The app re-lists a holed section when
    /// this moves ([`Shim::catalog_generation`]).
    catalog_gen: AtomicU64,
    /// Set by [`Shim::bind_self`]: background work started from a `&self` method.
    me: std::sync::OnceLock<std::sync::Weak<Shim>>,
    /// plex.tv's account surface, answered by the Nuvio account (`account.rs`).
    pub account: crate::shim_account::Account,
}

/// One My List title, as the shim can state it: the account's metadata plus the numeric key the
/// shim's key table gives the title. This is the Nuvio backend's counterpart of a discover
/// watchlist row (`/library/sections/watchlist/all`), and the reason the Watchlist does not need a
/// plex.tv credential.
#[derive(Clone, Debug, PartialEq)]
pub struct MyListEntry {
    pub rating_key: i64,
    /// Stremio's content type: `movie` or `series`.
    pub type_: String,
    pub id: String,
    pub title: String,
    pub year: Option<i32>,
    /// The addon's `releaseInfo`, used as the title's release date.
    pub release: Option<String>,
    pub rating: Option<f32>,
    /// The shim's poster proxy URL.
    pub thumb: String,
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
    // `get` keeps a multi-byte release string from panicking on a non-char boundary.
    s.get(..10)
        .filter(|date| date.as_bytes().get(4) == Some(&b'-'))
        .map(str::to_string)
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
    if series {
        client.series_id(&id).ok().flatten()
    } else {
        client.movie_id(&id).ok().flatten()
    }
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
    // `tagKey` is the person page's GLOBAL identity — the biography and filmography are read from
    // `discover()` under it, and under Nuvio that base is this shim, which answers the two routes.
    if cast.id > 0 && cast.id < (i64::MAX - TMDB_PERSON_KEY_BASE) as u64 {
        role["id"] = json!(TMDB_PERSON_KEY_BASE + cast.id as i64);
        role["tagKey"] = json!(person_guid(cast.id));
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

/// A TMDB crew department as a filmography tab's `(type, title)`. The three departments a viewer
/// most often knows an actor for take Plex's own group names ("Director", "Writer", "Producer");
/// every other department keeps TMDB's display name, with a slug `type` for the page's fallback.
fn person_department_group(department: &str) -> Option<(String, String)> {
    let name = department.trim();
    if name.is_empty() {
        return None;
    }
    match name.to_ascii_lowercase().as_str() {
        "directing" => return Some(("director".into(), "Director".into())),
        "writing" => return Some(("writer".into(), "Writer".into())),
        "production" => return Some(("producer".into(), "Producer".into())),
        // TMDB's catch-all bucket ("Creator", "Thanks", stunts, …). Plex files this under "Other"
        // too, and it is not a role anyone would name in a header's roles line.
        "crew" => return Some(("other".into(), "Other".into())),
        _ => {}
    }
    let slug: String = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-");
    (!slug.is_empty()).then(|| (slug, name.to_string()))
}

/// One credit inside a filmography department, as the person page's Discover shape spells it.
///
/// `ratingKey` is the availability join key: the page compares it against the TAIL of a library
/// row's guid, and every shim row's guid ends in the same `tmdb:<id>` (`nuvio://movie/tmdb:603`).
fn person_credit_item(credit: &TmdbCredit, role: &str) -> Option<Value> {
    if credit.id == 0 {
        return None;
    }
    let (kind, year, title, poster) = match credit.media_type.as_str() {
        "movie" => (
            "movie",
            year_of(credit.release_date.as_deref()).unwrap_or(0),
            credit.title.trim(),
            credit.poster_path.as_deref(),
        ),
        "tv" => (
            "show",
            year_of(credit.first_air_date.as_deref()).unwrap_or(0),
            credit.name.trim(),
            credit.poster_path.as_deref(),
        ),
        // Neither a film nor a show: the page has two kinds and neither would draw it.
        _ => return None,
    };
    if title.is_empty() {
        return None;
    }
    Some(json!({
        "role": role,
        "Metadata": {
            "type": kind,
            "title": title,
            "year": year,
            "thumb": tmdb_image(poster).unwrap_or_default(),
            "ratingKey": format!("tmdb:{}", credit.id),
        }
    }))
}

/// One `CreditGroup` object: a department, its own count, and its rows in billing order.
fn person_credit_group(kind: &str, title: &str, mut rows: Vec<Value>) -> Value {
    for (order, row) in rows.iter_mut().enumerate() {
        row["order"] = json!(order as i64);
    }
    json!({
        "type": kind,
        "title": title,
        "size": rows.len() as i64,
        "Credit": rows,
    })
}

fn related_hub_json(kind: &str, rows: Vec<Value>) -> Value {
    json!({
        "title": "More Like This", "type": kind, "hubIdentifier": format!("{kind}.related"),
        "size": rows.len(), "Metadata": rows
    })
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

fn trailer_extras(rk: i64, candidates: &[TrailerCandidate]) -> Value {
    let videos: Vec<Value> = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let clip_rk = trailer_rk_at(rk, index);
            json!({
                "ratingKey": clip_rk.to_string(),
                "key": format!("/library/metadata/{clip_rk}"),
                "type": "clip",
                "subtype": "trailer",
                "extraType": 1,
                "title": candidate.title,
                "thumb": format!(
                    "https://img.youtube.com/vi/{}/hqdefault.jpg",
                    candidate.youtube_id
                )
            })
        })
        .collect();
    json!({"size": videos.len(), "Video": videos})
}

/// Escape the five XML characters for `dash_manifest` BaseURLs.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Static SegmentBase MPD for a trailer DASH pair, in the exact shape
/// `media/src/dash.rs` parses: two AdaptationSets (video then audio), each
/// with one `SegmentBase` (init + index ranges) and one `Representation`
/// whose `BaseURL` is the signed googlevideo URL itself (`video_url`/`audio_url`
/// overrides exist for tests). The transport, not the manifest, handles what
/// those URLs require: see `GOOGLEVIDEO_WINDOW` in `media/src/curlio.rs`.
///
/// Played by the Rust DASH engine (`media/src/dash.rs` + `ff.rs dash_demux`),
/// which opens each track's BaseURL as a seekable progressive fMP4 source
/// through the bundled `mov` demuxer — no FFmpeg `dash` demuxer (it is not
/// in the build), no new C library.
fn dash_manifest(
    d: &crate::shim_trailer::DashInfo,
    video_url: Option<&str>,
    audio_url: Option<&str>,
) -> String {
    fn split_mime(mime: &str) -> (&str, Option<String>) {
        let (m, rest) = match mime.split_once(';') {
            Some((m, r)) => (m.trim(), Some(r.to_string())),
            None => (mime.trim(), None),
        };
        let codecs = rest.and_then(|r| {
            let r = r.trim();
            let r = r.strip_prefix("codecs=")?.trim();
            let quote = r.chars().next()?;
            if quote != '"' && quote != '\'' {
                return None;
            }
            r.strip_prefix(quote)?
                .split_once(quote)
                .map(|(c, _)| c.to_string())
        });
        (if m.is_empty() { "video/mp4" } else { m }, codecs)
    }
    let (video_mime, video_codecs) = split_mime(&d.video_mime);
    let (audio_mime, audio_codecs) = split_mime(&d.audio_mime);
    // Empty mimes fall back to the mp4 shape the parser accepts.
    let (video_mime, audio_mime) = if video_mime.is_empty() && audio_mime.is_empty() {
        ("video/mp4", "audio/mp4")
    } else if video_mime.is_empty() {
        ("video/mp4", audio_mime)
    } else if audio_mime.is_empty() {
        (video_mime, "audio/mp4")
    } else {
        (video_mime, audio_mime)
    };
    let duration = format!("PT{}S", ((d.duration_ms.max(0) + 999) / 1000).max(0));
    let v_codecs_attr = video_codecs
        .as_deref()
        .map(|c| format!(" codecs=\"{}\"", xml_escape(c)))
        .unwrap_or_default();
    let a_codecs_attr = audio_codecs
        .as_deref()
        .map(|c| format!(" codecs=\"{}\"", xml_escape(c)))
        .unwrap_or_default();
    let frame_attr = if d.fps > 0 {
        format!(" frameRate=\"{}\"", d.fps)
    } else {
        String::new()
    };
    let video_url = video_url.unwrap_or(&d.video_url);
    let audio_url = audio_url.unwrap_or(&d.audio_url);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\" type=\"static\" mediaPresentationDuration=\"{duration}\" minBufferTime=\"PT1.5S\">\n<Period>\n<AdaptationSet mimeType=\"{video_mime}\"{v_codecs_attr} width=\"{}\" height=\"{}\"{} segmentAlignment=\"true\">\n<SegmentBase timescale=\"1000\" indexRange=\"{}-{}\"><Initialization range=\"{}-{}\"/></SegmentBase>\n<Representation id=\"{}\" bandwidth=\"{}\">\n<BaseURL>{}</BaseURL>\n</Representation>\n</AdaptationSet>\n<AdaptationSet mimeType=\"{audio_mime}\"{a_codecs_attr} segmentAlignment=\"true\">\n<SegmentBase timescale=\"1000\" indexRange=\"{}-{}\"><Initialization range=\"{}-{}\"/></SegmentBase>\n<Representation id=\"{}\" bandwidth=\"{}\">\n<BaseURL>{}</BaseURL>\n</Representation>\n</AdaptationSet>\n</Period>\n</MPD>\n",
        d.width,
        d.height,
        frame_attr,
        d.video_index.0,
        d.video_index.1,
        d.video_init.0,
        d.video_init.1,
        d.video_itag,
        d.video_bitrate,
        xml_escape(video_url),
        d.audio_index.0,
        d.audio_index.1,
        d.audio_init.0,
        d.audio_init.1,
        d.audio_itag,
        d.audio_bitrate,
        xml_escape(audio_url)
    )
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

/// The TMDB id lookups the resolvers need, so a test can prove an id that the meta already
/// carries never reaches the network.
trait TmdbIdLookup {
    fn movie_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>>;
    fn series_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>>;
}

impl TmdbIdLookup for TmdbClient {
    fn movie_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>> {
        TmdbClient::movie_id(self, id)
    }
    fn series_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>> {
        TmdbClient::series_id(self, id)
    }
}

fn resolve_tmdb_movie_id(
    client: &impl TmdbIdLookup,
    id: &str,
    imdb_id: Option<&str>,
    moviedb_id: Option<u64>,
) -> plx_stremio::model::Result<Option<u64>> {
    // Cinemeta names the TMDB id; `/find` is the fallback, not the default.
    if let Some(tmdb_id) = moviedb_id.filter(|id| *id > 0) {
        return Ok(Some(tmdb_id));
    }
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
    client: &impl TmdbIdLookup,
    id: &str,
    imdb_id: Option<&str>,
    moviedb_id: Option<u64>,
) -> plx_stremio::model::Result<Option<u64>> {
    if let Some(tmdb_id) = moviedb_id.filter(|id| *id > 0) {
        return Ok(Some(tmdb_id));
    }
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

    #[test]
    fn a_meta_that_names_its_tmdb_id_skips_the_find_lookup() {
        let meta: Meta = serde_json::from_value(json!({
            "id": "tt0133093", "type": "movie", "name": "The Matrix", "moviedb_id": 603
        }))
        .unwrap();
        assert_eq!(meta.moviedb_id, Some(603));
        assert_eq!(
            resolve_tmdb_movie_id(&PanicLookup, &meta.id, None, meta.moviedb_id).unwrap(),
            Some(603),
            "Cinemeta's id answers without asking /find"
        );
        // Some addons spell numbers as strings; the lenient decoder takes both.
        let lenient: Meta = serde_json::from_value(json!({
            "id": "x", "type": "movie", "moviedb_id": "603"
        }))
        .unwrap();
        assert_eq!(lenient.moviedb_id, Some(603));
        // A meta without it still resolves through the lookup (the /find fallback).
        let without: Meta = serde_json::from_value(json!({
            "id": "tt0133093", "type": "movie"
        }))
        .unwrap();
        assert_eq!(without.moviedb_id, None);
        assert_eq!(
            resolve_tmdb_movie_id(&StubLookup, &without.id, None, without.moviedb_id).unwrap(),
            Some(603)
        );
        assert_eq!(
            resolve_tmdb_series_id(&PanicLookup, "tt0944947", None, Some(1399)).unwrap(),
            Some(1399)
        );
        assert_eq!(
            resolve_tmdb_series_id(&StubLookup, "tt0944947", None, None).unwrap(),
            Some(1399)
        );
    }

    /// Any lookup is a failure: the meta's own id must answer first.
    struct PanicLookup;

    impl TmdbIdLookup for PanicLookup {
        fn movie_id(&self, _id: &str) -> plx_stremio::model::Result<Option<u64>> {
            panic!("the meta's TMDB id must not be looked up")
        }
        fn series_id(&self, _id: &str) -> plx_stremio::model::Result<Option<u64>> {
            panic!("the meta's TMDB id must not be looked up")
        }
    }

    struct StubLookup;

    impl TmdbIdLookup for StubLookup {
        fn movie_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>> {
            Ok((id == "tt0133093").then_some(603))
        }
        fn series_id(&self, id: &str) -> plx_stremio::model::Result<Option<u64>> {
            Ok((id == "tt0944947").then_some(1399))
        }
    }

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
                // …and the global identity its biography/filmography reads are addressed to
                "tagKey": "nuvio-person-638",
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
        let clip = trailer_rk(42);
        assert_eq!(
            trailer_extras(
                42,
                &[TrailerCandidate {
                    youtube_id: "youtube123".into(),
                    title: "Official Trailer".into(),
                }]
            ),
            json!({
                "size": 1,
                "Video": [{
                    "ratingKey": clip.to_string(),
                    "key": format!("/library/metadata/{clip}"),
                    "type": "clip",
                    "subtype": "trailer",
                    "extraType": 1,
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
        let shim = Shim::new(
            Arc::new(TmdbMetaBackend),
            Config {
                torrent_server: Some("http://127.0.0.1:2710".into()),
                ..Config::default()
            },
        );
        assert_eq!(
            shim.torrent_server(),
            None,
            "P2P is off until Settings says otherwise"
        );
        shim.set_p2p(true, None);
        assert_eq!(
            shim.torrent_server(),
            None,
            "no URL configured: the app probes the default ports"
        );
        shim.set_p2p(true, Some("  http://192.168.1.9:2710  ".into()));
        assert_eq!(
            shim.torrent_server().as_deref(),
            Some("http://192.168.1.9:2710")
        );
        shim.set_p2p(true, None);
        assert_eq!(
            shim.torrent_server(),
            None,
            "clearing the field is not ignored"
        );
        shim.set_p2p(false, Some("http://192.168.1.9:2710".into()));
        assert_eq!(
            shim.torrent_server(),
            None,
            "the switch outranks a configured URL"
        );
    }

    /// The P2P switch must run BEFORE the C-engine branch in `open_url`: a torrent marker with the
    /// switch off resolves to nothing even when `libsbengine.so` loaded (regression guard — the
    /// engine path used to be tried first and would have played with P2P disabled).
    #[test]
    fn p2p_off_does_not_resolve_a_torrent_through_the_engine() {
        let shim = Shim::new(
            Arc::new(TmdbMetaBackend),
            Config {
                torrent_server: Some("http://127.0.0.1:2710".into()),
                ..Config::default()
            },
        );
        let stream = Stream {
            info_hash: Some("0123456789abcdef0123456789abcdef01234567".into()),
            ..Default::default()
        };
        let marker = format!(
            "{}{}",
            shim_media::TORRENT_SCHEME,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(
            shim.open_url(1, &stream, &marker),
            None,
            "P2P off must drop every torrent"
        );
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
                TmdbMovie {
                    id: 155,
                    title: "The Dark Knight".into(),
                    ..Default::default()
                },
                TmdbMovie {
                    id: 272,
                    title: "Batman Begins".into(),
                    ..Default::default()
                },
                TmdbMovie {
                    id: 49026,
                    title: "The Dark Knight Rises".into(),
                    ..Default::default()
                },
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
            parts: vec![TmdbMovie {
                id: 272,
                title: "Batman Begins".into(),
                ..Default::default()
            }],
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
                TmdbMovie {
                    id: 155,
                    title: "The Dark Knight".into(),
                    ..Default::default()
                },
                TmdbMovie {
                    id: 272,
                    title: "Batman Begins".into(),
                    ..Default::default()
                },
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
        assert_eq!(
            listing["Metadata"][0]["title"],
            "The Dark Knight Collection"
        );

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

    /// Every headshot carries the GLOBAL person identity as `tagKey` — the id the person page's
    /// biography and filmography reads are addressed to, and one this shim answers itself.
    #[test]
    fn a_headshot_carries_the_shim_person_guid() {
        let role = tmdb_role(&TmdbCast {
            id: 638,
            name: "Hugh Jackman".into(),
            character: "Logan".into(),
            profile_path: None,
        })
        .expect("a named headshot");
        assert_eq!(role["id"], 3_000_000_638i64);
        assert_eq!(role["tagKey"], "nuvio-person-638");
        assert_eq!(person_id_of_guid("nuvio-person-638"), Some(638));
        // A real plex.tv tagKey (and a truncated synthetic one) is not ours: the routes answer it
        // with an empty container rather than guessing a person.
        assert_eq!(person_id_of_guid("nuvio-person-"), None);
        assert_eq!(person_id_of_guid("5d77682aeb5d26001f1de4b0"), None);
    }

    /// The filmography's departments: cast leads as "Actor", each crew row carries its job under
    /// its own department, and every row's `ratingKey` is the `tmdb:<id>` a library row's
    /// `nuvio://…/tmdb:<id>` guid ends in — the availability join.
    #[test]
    fn a_persons_career_is_grouped_by_department() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        let cast = vec![TmdbCredit {
            id: 603,
            media_type: "movie".into(),
            title: "The Matrix".into(),
            release_date: Some("1999-03-31".into()),
            character: "Neo".into(),
            poster_path: Some("/matrix.jpg".into()),
            ..Default::default()
        }];
        let crew = vec![
            TmdbCredit {
                id: 155,
                media_type: "movie".into(),
                title: "The Dark Knight".into(),
                release_date: Some("2008-07-16".into()),
                department: "Directing".into(),
                job: "Director".into(),
                ..Default::default()
            },
            TmdbCredit {
                id: 1396,
                media_type: "tv".into(),
                name: "Breaking Bad".into(),
                first_air_date: Some("2008-01-20".into()),
                department: "Writing".into(),
                job: "Writer".into(),
                ..Default::default()
            },
            // TMDB's catch-all bucket is Plex's "Other", not a tab called "Crew".
            TmdbCredit {
                id: 27205,
                media_type: "movie".into(),
                title: "Inception".into(),
                department: "Crew".into(),
                job: "Thanks".into(),
                ..Default::default()
            },
        ];
        let groups = shim.person_credit_groups(&cast, &crew);
        assert_eq!(groups.len(), 4, "Actor, then one group per crew department");
        assert_eq!(groups[0]["type"], "actor");
        assert_eq!(groups[0]["title"], "Actor");
        assert_eq!(groups[0]["size"], 1);
        assert_eq!(groups[0]["Credit"][0]["role"], "Neo");
        assert_eq!(groups[0]["Credit"][0]["order"], 0);
        assert_eq!(groups[0]["Credit"][0]["Metadata"]["ratingKey"], "tmdb:603");
        assert_eq!(groups[0]["Credit"][0]["Metadata"]["type"], "movie");
        assert_eq!(groups[0]["Credit"][0]["Metadata"]["year"], 1999);
        let directing = groups
            .iter()
            .find(|g| g["type"] == "director")
            .expect("the directing tab");
        assert_eq!(directing["title"], "Director");
        assert_eq!(directing["Credit"][0]["role"], "Director");
        // Renaming a department to Plex's name does not lose the item kind: a show stays a show.
        let writing = groups
            .iter()
            .find(|g| g["type"] == "writer")
            .expect("the writing tab");
        assert_eq!(writing["Credit"][0]["Metadata"]["type"], "show");
        assert_eq!(writing["Credit"][0]["Metadata"]["title"], "Breaking Bad");
        let other = groups
            .iter()
            .find(|g| g["type"] == "other")
            .expect("the Other tab");
        assert_eq!(other["title"], "Other");
        assert_eq!(other["Credit"][0]["role"], "Thanks");
    }

    /// The header record is TMDB's person record in Discover's spelling, including the department
    /// counts the roles line and the Filmography pill read.
    #[test]
    fn a_person_header_states_their_facts_and_departments() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        let person = TmdbPerson {
            id: 638,
            name: "Hugh Jackman".into(),
            biography: "An Australian actor.".into(),
            birthday: Some("1968-10-12".into()),
            deathday: None,
            place_of_birth: Some("Sydney, Australia".into()),
            known_for_department: Some("Acting".into()),
            profile_path: Some("/portrait.jpg".into()),
        };
        let cast = vec![TmdbCredit {
            id: 603,
            media_type: "movie".into(),
            title: "The Matrix".into(),
            character: "Neo".into(),
            ..Default::default()
        }];
        let crew = vec![
            TmdbCredit {
                id: 155,
                media_type: "movie".into(),
                title: "The Dark Knight".into(),
                department: "Directing".into(),
                job: "Director".into(),
                ..Default::default()
            },
            TmdbCredit {
                id: 27205,
                media_type: "movie".into(),
                title: "Inception".into(),
                department: "Crew".into(),
                job: "Thanks".into(),
                ..Default::default()
            },
        ];
        let row = shim.person_profile_row(&person, &cast, &crew);
        assert_eq!(row["title"], "Hugh Jackman");
        assert_eq!(row["summary"], "An Australian actor.");
        assert_eq!(row["bornAt"], "1968-10-12");
        assert_eq!(row["diedAt"], "");
        assert_eq!(row["birthPlace"], "Sydney, Australia");
        assert_eq!(row["knownFor"], "Acting");
        assert_eq!(row["thumb"], "https://image.tmdb.org/t/p/w300/portrait.jpg");
        assert_eq!(
            row["CreditType"][0],
            json!({"type": "actor", "title": "Actor"})
        );
        assert_eq!(
            row["CreditType"][1],
            json!({"type": "director", "title": "Director"})
        );
        assert_eq!(
            row["CreditType"].as_array().unwrap().len(),
            2,
            "Crew is a tab, not a role"
        );
    }

    /// A guid this shim never handed out is an EMPTY container on both routes — the answer a real
    /// provider gives for someone it has never heard of, so the page settles instead of retrying.
    #[test]
    fn an_unknown_person_guid_answers_empty_containers() {
        let shim = Arc::new(Shim::new(Arc::new(TmdbMetaBackend), Config::default()));
        for (path, key) in [
            ("/library/people/nuvio-person-", "Metadata"),
            ("/library/people/somebody-else", "Metadata"),
            ("/library/people/somebody-else/credits", "CreditGroup"),
        ] {
            let response = shim.handle(&Request {
                method: "GET".into(),
                path: path.into(),
                query: HashMap::new(),
                headers: HashMap::new(),
                body: Vec::new(),
            });
            assert_eq!(response.status, 200, "{path}");
            let body: Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(body["MediaContainer"][key], json!([]), "{path}");
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
        fn all_catalogs(&self) -> Vec<CatalogInfo> {
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
        fn meta(&self, _type_: &str, _id_: &str) -> Option<Meta> {
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

    /// Regression: Movies / TV Shows Recommended must show the full browse list (all_catalogs),
    /// not just home_catalogs(). A backend returning 1 home + 1 hidden catalog makes /hubs
    /// contain 1 hub but /hubs/sections/1 contain 2.
    #[derive(Default)]
    struct TwoCatalogBackend {
        // 1 movie home catalog, 1 movie hidden catalog
        m_home: Mutex<Vec<CatalogInfo>>,
        m_hidden: Mutex<Vec<CatalogInfo>>,
        /// How many times the addon's `meta` was asked: a card the shim already built must not
        /// cost one.
        meta_reads: std::sync::atomic::AtomicUsize,
    }

    impl TwoCatalogBackend {
        fn meta_reads(&self) -> usize {
            self.meta_reads.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl TwoCatalogBackend {
        fn new() -> Self {
            Self {
                meta_reads: std::sync::atomic::AtomicUsize::new(0),
                m_home: std::sync::Mutex::new(vec![CatalogInfo {
                    key: "a/movie/top".into(),
                    title: "Top Movies".into(),
                    type_: "movie".into(),
                    genres: vec![],
                }]),
                m_hidden: std::sync::Mutex::new(vec![CatalogInfo {
                    key: "b/movie/hidden".into(),
                    title: "Hidden Movies".into(),
                    type_: "movie".into(),
                    genres: vec![],
                }]),
            }
        }
    }

    impl Backend for TwoCatalogBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> {
            self.m_home.lock().unwrap().clone()
        }
        fn all_catalogs(&self) -> Vec<CatalogInfo> {
            let home = self.m_home.lock().unwrap().clone();
            let hidden = self.m_hidden.lock().unwrap().clone();
            home.into_iter().chain(hidden).collect()
        }
        fn catalog_page(
            &self,
            key: &str,
            skip: u32,
            _genre: Option<&str>,
        ) -> Result<Vec<MetaPreview>, ()> {
            if skip == 0 {
                Ok(vec![MetaPreview {
                    id: format!("tt{key}"),
                    type_: "movie".into(),
                    name: format!("Movie for {key}"),
                    poster: None,
                    background: None,
                    logo: None,
                    description: None,
                    release_info: None,
                    imdb_rating: None,
                    ratings: serde_json::Value::Null,
                    mdb_list_ratings: serde_json::Value::Null,
                    rotten_tomatoes_rating: None,
                    genres: vec![],
                }])
            } else {
                Ok(Vec::new())
            }
        }
        fn search(&self, _query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
            Vec::new()
        }
        fn meta(&self, _type_: &str, _id: &str) -> Option<Meta> {
            self.meta_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
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
    fn recommended_rows_show_the_full_browse_list_not_only_home_catalogs() {
        let shim = Arc::new(Shim::new(
            Arc::new(TwoCatalogBackend::new()),
            Config::default(),
        ));
        // /hubs (Home) must stay gated on showInHome: only 1 movie hub (plus Continue Watching).
        let home = serde_json::from_slice::<Value>(
            &shim
                .handle(&Request {
                    method: "GET".into(),
                    path: "/hubs".into(),
                    query: HashMap::new(),
                    headers: HashMap::new(),
                    body: Vec::new(),
                })
                .body,
        )
        .unwrap();
        let home_hubs = home["MediaContainer"]["Hub"].as_array().unwrap();
        let home_movie_hubs = home_hubs.iter().filter(|h| h["type"] == "movie").count();
        assert_eq!(
            home_movie_hubs, 1,
            "Home stays gated on showInHome: {home_hubs:?}"
        );

        // /hubs/sections/1 (Movies Recommended) must show BOTH catalogs, including the hidden one.
        let rec = serde_json::from_slice::<Value>(
            &shim
                .handle(&Request {
                    method: "GET".into(),
                    path: "/hubs/sections/1".into(),
                    query: HashMap::new(),
                    headers: HashMap::new(),
                    body: Vec::new(),
                })
                .body,
        )
        .unwrap();
        let rec_hubs = rec["MediaContainer"]["Hub"].as_array().unwrap();
        let rec_movie_hubs = rec_hubs.iter().filter(|h| h["type"] == "movie").count();
        assert_eq!(
            rec_movie_hubs, 2,
            "Recommended must show all browseable movie catalogs: {rec_hubs:?}"
        );
        let titles: Vec<&str> = rec_hubs
            .iter()
            .filter(|h| h["type"] == "movie")
            .filter_map(|h| h["title"].as_str())
            .collect();
        assert!(titles.contains(&"Top Movies"), "home catalog present");
        assert!(
            titles.contains(&"Hidden Movies"),
            "hidden catalog must appear in Recommended"
        );

        // A row's `key` is read back for its cards. It must page the SAME catalog the row previewed,
        // whatever list the section was built from (Home's list is shorter than the browse list).
        for hub in rec_hubs.iter().filter(|h| h["type"] == "movie") {
            let key = hub["key"].as_str().unwrap();
            let body = serde_json::from_slice::<Value>(
                &shim
                    .handle(&Request {
                        method: "GET".into(),
                        path: key.into(),
                        query: HashMap::new(),
                        headers: HashMap::new(),
                        body: Vec::new(),
                    })
                    .body,
            )
            .unwrap();
            let rows = body["MediaContainer"]["Metadata"].as_array().unwrap();
            assert_eq!(rows.len(), 1, "{key} must page its catalog: {body:?}");
            assert_eq!(
                rows[0]["title"], hub["Metadata"][0]["title"],
                "{key} paged a different catalog than the row previewed"
            );
        }
    }

    /// The hub list pages BY HUB: a window must fetch only that window and report the whole list's
    /// size. A shim that ignored the window re-ran every catalog on every window the client read,
    /// which is what made a Recommended page take a minute to fill in.
    #[test]
    fn a_hub_window_fetches_only_its_rows_and_reports_the_total() {
        let backend = Arc::new(TwoCatalogBackend::new());
        let shim = Arc::new(Shim::new(backend.clone(), Config::default()));
        let get = |path: &str, query: HashMap<String, String>| -> Value {
            serde_json::from_slice(
                &shim
                    .handle(&Request {
                        method: "GET".into(),
                        path: path.into(),
                        query,
                        headers: HashMap::new(),
                        body: Vec::new(),
                    })
                    .body,
            )
            .unwrap()
        };
        let page = |start: usize, size: usize| {
            HashMap::from([
                ("X-Plex-Container-Start".to_string(), start.to_string()),
                ("X-Plex-Container-Size".to_string(), size.to_string()),
            ])
        };
        // Row 0 is Continue Watching; the two movie catalogs follow it.
        let first = get("/hubs/sections/1", page(0, 2));
        let hubs = first["MediaContainer"]["Hub"].as_array().unwrap();
        assert_eq!(hubs.len(), 2, "{first:?}");
        assert_eq!(first["MediaContainer"]["offset"], 0);
        assert_eq!(first["MediaContainer"]["totalSize"], 3, "1 CW + 2 catalogs");

        let second = get("/hubs/sections/1", page(2, 2));
        let hubs = second["MediaContainer"]["Hub"].as_array().unwrap();
        assert_eq!(
            hubs.len(),
            1,
            "the window is the tail of the list: {second:?}"
        );
        assert_eq!(hubs[0]["title"], "Hidden Movies");
        assert_eq!(
            hubs[0]["key"], "/hubs/nuvio/1",
            "the index is the row's place in the list"
        );
        assert_eq!(second["MediaContainer"]["offset"], 2);
        assert_eq!(second["MediaContainer"]["totalSize"], 3);
    }

    /// A LISTING read (`/library/metadata/{rk1},{rk2},…`) must be answered from the cards the shim
    /// already built, not by resolving each one through the addon's `meta` again. Measured on the
    /// sim, a 12-card listing cost 9.2 s that way (addon meta + TMDB per title, serially) and the
    /// page reads its rows one at a time — a Recommended page filled in a row a minute.
    #[test]
    fn a_listing_read_answers_from_cards_already_built() {
        let backend = Arc::new(TwoCatalogBackend::new());
        let shim = Arc::new(Shim::new(backend.clone(), Config::default()));
        let get = |path: &str| -> Value {
            serde_json::from_slice(
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
            .unwrap()
        };
        // Build the row, which is what remembers its cards.
        let hub = get("/hubs/sections/1");
        let rows = hub["MediaContainer"]["Hub"][1]["Metadata"]
            .as_array()
            .unwrap()
            .clone();
        let key = rows[0]["ratingKey"].as_str().unwrap().to_string();
        assert_eq!(
            backend.meta_reads(),
            0,
            "a preview row must not need the addon"
        );

        let csv = get(&format!("/library/metadata/{key},{key}"));
        let listed = csv["MediaContainer"]["Metadata"].as_array().unwrap();
        assert_eq!(listed.len(), 2, "{csv:?}");
        assert_eq!(listed[0]["title"], rows[0]["title"]);
        assert_eq!(
            backend.meta_reads(),
            0,
            "a listing must not resolve the addon again: {csv:?}"
        );
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
            if !backend.streams.lock().unwrap().is_empty()
                && !backend.subtitles.lock().unwrap().is_empty()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // WHICH spelling reaches the addon is a build fact, not this test's subject: `addon_id_for`
        // turns `tmdb:603` into its IMDb id when a TMDB key is compiled in AND the lookup answers (an
        // addon indexes better by `tt`), and passes `tmdb:603` through otherwise — so a checkout built
        // with `nuvio.local.env` asks for `tt0133093` and a CI build without the secret asks for
        // `tmdb:603`. Both name the Matrix, and a failed lookup falls back to the tmdb id, so this
        // holds offline too. What the route promises, and what this pins, is that a synthetic
        // ratingKey reaches the addon as THAT title — not as the key's decimal digits.
        let asked = backend.streams.lock().unwrap().clone();
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert_eq!(asked[0].0, "movie");
        assert!(
            matches!(asked[0].1.as_str(), "tmdb:603" | "tt0133093"),
            "the addon was asked for {:?}, which is not the Matrix",
            asked[0].1
        );
        let subtitle_ids = backend.subtitles.lock().unwrap().clone();
        assert_eq!(
            subtitle_ids, asked,
            "streams and subtitles ask for the same title id"
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

/// One local item's identity as Nuvio's account stores it. `content_type` is `"movie"` or
/// `"series"`, and a series row names its episodes, not itself.
struct AccountIdentity {
    content_id: String,
    content_type: &'static str,
    video_id: String,
    season: Option<i64>,
    episode: Option<i64>,
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
            metas: MetaCache::open(dir.as_deref()),
            enrich: EnrichCache::open(dir.as_deref()),
            meta_flight: plx_base::flight::Flight::default(),
            catalogs: Mutex::new(HashMap::new()),
            catalog_list: Mutex::new(None),
            browse_list: Mutex::new(None),
            cards: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            sub_urls: Mutex::new(HashMap::new()),
            search_subs: Mutex::new(HashMap::new()),
            installed_subs: Mutex::new(HashMap::new()),
            poster_fallbacks: Mutex::new(HashMap::new()),
            tmdb_fallbacks: Mutex::new(HashMap::new()),
            collections: Mutex::new(HashMap::new()),
            credit_rows: Mutex::new(Vec::new()),
            next_sub: AtomicI64::new(900_000_000),
            permits: (Mutex::new(CATALOG_PARALLEL), Condvar::new()),
            probes: crate::shim_probe::ProbeCache::default(),
            trailer_reach: Mutex::new(None),
            trailer_parts: Mutex::new(HashMap::new()),
            trailer_dash: Mutex::new(HashMap::new()),
            trailer_cache: crate::shim_trailer::TrailerCache::new(),
            trailer_flight: plx_base::flight::Flight::default(),
            #[cfg(not(test))]
            trailer_warm: Mutex::new(std::collections::HashSet::new()),
            trailer_resolver: Box::new(crate::shim_trailer::NodeTrailerResolver::default_paths()),
            trailer_proxy: crate::shim_trailer::TrailerProxy::default_paths(),
            resolved: Mutex::new(HashMap::new()),
            imdb_ids: Mutex::new(HashMap::new()),
            torrent_server: std::sync::RwLock::new(cfg_ts),
            p2p_enabled: AtomicBool::new(cfg_p2p),
            audio_codecs: std::sync::RwLock::new(Vec::new()),
            content_gen: AtomicU64::new(0),
            catalog_gen: AtomicU64::new(0),
            me: std::sync::OnceLock::new(),
            account: crate::shim_account::Account::new(dir.as_deref(), &cfg_token),
        }
    }

    pub fn bind_self(self: &Arc<Self>) {
        let _ = self.me.set(Arc::downgrade(self));
    }

    // ---- account watch progress ------------------------------------------------------------

    /// The identity Nuvio keys a resume point by, for a local ratingKey. **Nuvio names a title by
    /// its IMDb id** (`tt0133093`), not by TMDB: its own apps match Continue Watching on that form,
    /// and a `tmdb:` row they cannot match is as good as missing. `content_id` is the film's or the
    /// series' IMDb id, `video_id` the film's own id or the episode's addon id, and an episode
    /// carries the season/episode pair the account's `progress_key` is built from. `None` for an
    /// item that has no single position to report — a season, a whole series, or a title neither
    /// IMDb nor TMDB can name.
    fn nuvio_watch_identity(&self, rk: i64) -> Option<AccountIdentity> {
        let ident = self.keys.ident(rk).or_else(|| synthetic_ident(rk))?;
        match ident {
            // A series row has no position of its own; its episodes do.
            Ident::Title { type_, .. } if is_series(&type_) => None,
            Ident::Title { type_, id } => {
                let base = split_content_id(&id).0;
                // A local catalog id is usually IMDb already; a Nuvio catalog's `tmdb:<n>` is
                // resolved through TMDB. Falling back to `tmdb:` keeps the old behaviour for an
                // id TMDB has no IMDb counterpart for.
                let content_id = match self.imdb_base(&type_, base) {
                    Some(imdb) => imdb,
                    None => format!("tmdb:{}", tmdb_id(&id, false)?),
                };
                Some(AccountIdentity {
                    video_id: content_id.clone(),
                    content_id,
                    content_type: "movie",
                    season: None,
                    episode: None,
                })
            }
            Ident::Episode { series, video } => {
                // `series` is the addon's series id; the account wants that series' IMDb id.
                let content_id = self
                    .imdb_base("series", &series)
                    .or_else(|| tmdb_id(&series, true).map(|n| format!("tmdb:{n}")))?;
                let (video_base, _) = split_content_id(&video);
                let video_id = self
                    .imdb_base("series", video_base)
                    .map(|base| format!("{base}{}", &video[video_base.len()..]))
                    .unwrap_or_else(|| video.clone());
                let mut numbers = episode_numbers(&video_id);
                if numbers.0.is_none() {
                    // An addon that ids an episode without a `:S:E` tail still has the numbers in
                    // its meta. Without them the account would key every episode of the series to
                    // one row.
                    if let Some((_, meta)) = self.series_of(&series) {
                        if let Some((s, e, _)) =
                            episodes_of(&meta).into_iter().find(|(_, _, v)| v.id == video)
                        {
                            numbers = (Some(i64::from(s)), Some(i64::from(e)));
                        }
                    }
                }
                let (season, episode) = numbers;
                Some(AccountIdentity {
                    content_id,
                    content_type: "series",
                    video_id,
                    season,
                    episode,
                })
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
        let Some(identity) = self.nuvio_watch_identity(rk) else {
            return;
        };
        self.watch
            .bind_account_key(&identity.content_id, &identity.video_id, rk);
        let entry = plx_stremio::nuvio::WatchProgress {
            content_id: identity.content_id,
            content_type: identity.content_type.into(),
            video_id: identity.video_id,
            season: identity.season,
            episode: identity.episode,
            position: offset_ms.max(0),
            duration: duration_ms,
            last_watched: now_secs() * 1000,
        };
        match self.account.push_watch_progress(&[entry]) {
            Ok(()) => {}
            Err(error) => {
                let status = match error {
                    plx_stremio::model::Error::Status(s) => s,
                    _ => 0,
                };
                plx_stremio::nuvio::log_account_event(&format!(
                    "nuvio account: progress push failed status={status}"
                ));
            }
        }
    }

    /// Pull the account's resume points and merge them into the local watch state, so Continue
    /// Watching draws what another device played. Runs off the caller's thread.
    pub fn pull_account_watch(&self) {
        let Some(me) = self.me.get().and_then(|w| w.upgrade()) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("pmsshim-watch".into())
            .spawn(move || {
                let Ok(entries) = me.account.pull_watch_progress() else {
                    return;
                };
                let mut merged = 0;
                for e in entries {
                    // Cheap rejection first: the row snapshot is a meta fetch, so never pay for an
                    // entry the local state already beats.
                    if e.position <= 0 {
                        continue;
                    }
                    let stamp = e.last_watched / 1000;
                    let Some(rk) = me.account_watch_key(&e) else {
                        continue;
                    };
                    if me.watch.get(rk).last_viewed_at > stamp {
                        continue;
                    }
                    if me
                        .watch
                        .merge_remote(rk, e.position, e.duration, stamp, me.row(rk))
                    {
                        me.watch.bind_account_key(&e.content_id, &e.video_id, rk);
                        merged += 1;
                    }
                }
                me.account_watch_merged(merged);
            });
    }

    /// The end of an account pull: a real merge changes what Continue Watching should show, and
    /// the client's first hub read may already have landed with the empty row. A pull that
    /// changed nothing says nothing, so a steady-state boot does not make Home read twice.
    fn account_watch_merged(&self, merged: usize) {
        if merged == 0 {
            return;
        }
        self.bump_content();
        plx_stremio::nuvio::log_account_event(&format!(
            "nuvio account: merged {merged} resume point(s)"
        ));
    }

    /// The local ratingKey an account resume point belongs to: what this device pushed that item
    /// under, else the key its identity names. Does not bind on a miss — a pull that loses to the
    /// local state must not grow the map.
    fn account_watch_key(&self, e: &plx_stremio::nuvio::WatchProgress) -> Option<i64> {
        if let Some(key) = self.watch.account_key(&e.content_id, &e.video_id) {
            return Some(key);
        }
        // Nuvio names a title by its IMDb id; canonicalise a legacy `tmdb:` row the same way the
        // push does, so a title written once as `tmdb:N` and once as `tt…` is ONE local item (one
        // Continue Watching card) instead of two.
        if e.content_type != "series" {
            let id = self
                .imdb_base("movie", &e.content_id)
                .unwrap_or_else(|| e.content_id.clone());
            return Some(self.keys.key_of(&Ident::Title {
                type_: "movie".into(),
                id,
            }));
        }
        let content_id = self
            .imdb_base("series", &e.content_id)
            .unwrap_or_else(|| e.content_id.clone());
        // Nuvio's own clients name an episode synthetically (`__nuvio_episode__:S:E`) when the
        // playback did not carry the addon's episode id, and a row may even come back with no
        // video id. Both are placed through the series meta's season/episode instead: interning
        // the synthetic string would name an episode no addon can answer, and the merge would
        // store a resume point whose row never exists.
        let synthetic = e.video_id.is_empty()
            || e.video_id.starts_with(NUVIO_SYNTHETIC_EPISODE)
            || e.video_id == e.content_id;
        if synthetic {
            let (season, episode) = e.season.zip(e.episode)?;
            let (_, meta) = self.series_of(&content_id)?;
            let (_, _, video) = episodes_of(&meta)
                .into_iter()
                .find(|(s, n, _)| i64::from(*s) == season && i64::from(*n) == episode)?;
            return Some(self.episode_key(&content_id, &video.id));
        }
        let raw_series = episode_series_id(&e.video_id, &e.content_id);
        let series = self
            .imdb_base("series", raw_series)
            .unwrap_or_else(|| raw_series.to_string());
        let (video_base, _) = split_content_id(&e.video_id);
        let video = match self.imdb_base("series", video_base) {
            Some(base) => format!("{base}{}", &e.video_id[video_base.len()..]),
            None => e.video_id.clone(),
        };
        Some(self.keys.key_of(&Ident::Episode { series, video }))
    }

    /// The Stremio-compatible streaming server torrents play through, if any. `None` whenever P2P
    /// is off, which is what makes [`shim_media::rank`] and `playable_url` drop every torrent and
    /// magnet stream: an unplayable pick is not offered.
    ///
    /// With the in-process C engine loaded, the address is not dialled at all — the engine serves
    /// its own loopback URL — but it is still the marker that torrents are available, so an empty
    /// Settings field falls back to the default address instead of hiding every torrent.
    pub fn torrent_server(&self) -> Option<String> {
        if !self.p2p_enabled() {
            return None;
        }
        let configured = self
            .torrent_server
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if configured.is_some() {
            return configured;
        }
        // The URL field is optional now: the engine needs no server, and an absent URL must not
        // silently remove torrents from every list. There is no Node streaming fallback;
        // when the engine is unavailable the URL stays absent and torrents are not offered.
        if plx_stremio::sbengine::available() {
            return Some(DEFAULT_P2P_SERVER.to_string());
        }
        None
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
        *self
            .torrent_server
            .write()
            .unwrap_or_else(|e| e.into_inner()) = server
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty());
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

    /// **The count a client polls to learn that account content arrived.** A sign-in loads its
    /// addons on a worker and its watch history on another, so Home's first hub read can land
    /// before either; the app re-reads the hubs when this moves. Monotonic, and only ever bumped
    /// by real changes (see [`Shim::bump_content`]'s callers).
    pub fn content_generation(&self) -> u64 {
        self.content_gen.load(Ordering::Relaxed)
    }

    /// Record that the content behind Home changed. Cheap enough for a worker thread.
    fn bump_content(&self) {
        self.content_gen.fetch_add(1, Ordering::Relaxed);
    }

    /// **The count a library page polls to learn that a catalog it answered short has filled.**
    /// A section listing answers within [`Config::catalog_wait`] even when the addon is still
    /// fetching, so the grid can hold a partial first page; the fill bumps this when it lands and
    /// the app re-lists a section whose first page still has holes.
    pub fn catalog_generation(&self) -> u64 {
        self.catalog_gen.load(Ordering::Relaxed)
    }

    /// Drop every cache that depends on the addon set (after Settings changed it).
    pub fn addons_changed(&self) {
        self.metas.clear();
        self.enrich.clear();
        self.poster_fallbacks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self.catalog_list.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.browse_list.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // LAST, not first: a client that sees the new generation must never read a cache this
        // call has not cleared yet.
        self.bump_content();
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

    /// Every browseable catalog belonging to this section's content type, in addon order.
    fn section_catalogs(&self, section: i64) -> Vec<CatalogInfo> {
        self.all_catalogs()
            .into_iter()
            .filter(|c| {
                if section == SECTION_SHOWS {
                    is_series(&c.type_)
                } else {
                    c.type_ == "movie"
                }
            })
            .collect()
    }

    /// The full browse list, cached like [`Self::catalog_list`]. `/hubs/nuvio/{i}` indexes THIS
    /// list, so every row builder (Home and Recommended) must hand it to [`Self::catalog_hubs`].
    fn all_catalogs(&self) -> Vec<CatalogInfo> {
        let mut g = self.browse_list.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = g.as_ref() {
            if at.elapsed() < CATALOG_TTL {
                return v.clone();
            }
        }
        let v = self.backend.all_catalogs();
        *g = Some((Instant::now(), v.clone()));
        v
    }

    /// The catalog the request asked for, or the section's first catalog.
    fn section_catalog(&self, req: &Request, section: i64) -> Option<CatalogInfo> {
        let cats = self.section_catalogs(section);
        req.q("catalog")
            .and_then(|key| cats.iter().find(|c| c.key == key).cloned())
            .or_else(|| self.first_catalog(section))
            .or_else(|| cats.into_iter().next())
    }

    /// Rows `[start, start+size)` of a catalog and the total to report. `fetch_genre` is what the
    /// addon is asked for (only when it declares the genre); `match_genre` is applied in memory
    /// to the fetched rows when it does not.
    ///
    /// **The network is never on this path longer than `wait`.** The rows the cache already holds
    /// are the answer; a window it does not cover starts (or joins) one background fill, and the
    /// caller takes what has landed by the deadline. So the copy Home already warmed is served
    /// immediately, and a slow addon costs a partial first page rather than holding the HTTP
    /// response for its own 30 s budget — which is what made the app's 15 s loopback read report
    /// "Can't reach your Plex server". The fill continues after the answer and bumps
    /// [`Shim::catalog_generation`]; the library re-lists a page that still has holes.
    fn catalog_range(
        self: &Arc<Self>,
        key: &str,
        fetch_genre: Option<&str>,
        match_genre: Option<&str>,
        sort: Option<(CatalogSort, bool)>,
        start: usize,
        size: usize,
        wait: Duration,
    ) -> (Vec<MetaPreview>, i64) {
        let k = (key.to_string(), match_genre.or(fetch_genre).map(genre_key));
        let entry = {
            let mut g = self.catalogs.lock().unwrap_or_else(|e| e.into_inner());
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
                        filling: false,
                        retry_at: None,
                        cv: Arc::new(Condvar::new()),
                    })),
                );
            }
            g[&k].clone()
        };
        // Popularity IS the catalog's own order, so it needs no full read; the other keys do.
        let sorted = sort.is_some_and(|(kind, _)| kind != CatalogSort::Popularity);
        // A sorted or genre-filtered listing needs the WHOLE catalog before it can answer a page
        // correctly; an unsorted one needs only the requested window.
        let want = if sorted || match_genre.is_some() {
            usize::MAX
        } else {
            start.saturating_add(size)
        };
        self.ensure_catalog_fill(&k, &entry, fetch_genre, match_genre, want);
        let deadline = Instant::now() + wait;
        let mut c = entry.lock().unwrap_or_else(|e| e.into_inner());
        while c.filling && !c.exhausted && c.rows.len() < want {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let cv = Arc::clone(&c.cv);
            let (guard, timed) = cv.wait_timeout(c, left).unwrap_or_else(|e| e.into_inner());
            c = guard;
            if timed.timed_out() {
                break;
            }
        }
        let mut ordered: Vec<&MetaPreview> = c.rows.iter().collect();
        if let Some((sort, desc)) = sort.filter(|(kind, _)| *kind != CatalogSort::Popularity) {
            // Sorting must never take the shim down: a non-total comparator (bad data) is
            // caught here and the catalog keeps its original order instead.
            let sort_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                ordered.sort_by(|a, b| compare_catalog(a, b, sort));
            }));
            if sort_result.is_err() {
                ordered = c.rows.iter().collect();
            }
            if desc && sort_result.is_ok() {
                ordered.reverse();
            }
        }
        let rows = ordered
            .into_iter()
            .skip(start)
            .take(size)
            .cloned()
            .collect();
        let total = c.rows.len() as i64 + if c.exhausted { 0 } else { MORE_HINT };
        (rows, total)
    }

    /// Start the one fill for `entry` that would cover `want`, unless it is covered, exhausted, in
    /// flight, or backing off after a failure. Never blocks on the network.
    fn ensure_catalog_fill(
        self: &Arc<Self>,
        k: &(String, Option<String>),
        entry: &Arc<Mutex<CatalogCache>>,
        fetch_genre: Option<&str>,
        match_genre: Option<&str>,
        want: usize,
    ) {
        {
            let mut c = entry.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if c.filling
                || c.exhausted
                || c.rows.len() >= want
                || c.retry_at.is_some_and(|at| now < at)
            {
                return;
            }
            c.filling = true;
        }
        let (me, filling, clear, claim, k) = (
            Arc::clone(self),
            Arc::clone(entry),
            Arc::clone(entry),
            Arc::clone(entry),
            k.clone(),
        );
        let (fetch_genre, match_genre) =
            (fetch_genre.map(str::to_string), match_genre.map(str::to_string));
        let spawned = std::thread::Builder::new()
            .name("pmsshim-catalog-fill".into())
            .spawn(move || {
                // A backend that panics must not leave the catalog claimed: the waiter would then
                // sit out every budget, and no later read could ever fill it.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    me.fill_catalog(filling, k, fetch_genre, match_genre, want);
                }));
                let mut c = clear.lock().unwrap_or_else(|e| e.into_inner());
                if c.filling {
                    c.filling = false;
                    c.cv.notify_all();
                }
            });
        if spawned.is_err() {
            let mut c = claim.lock().unwrap_or_else(|e| e.into_inner());
            c.filling = false;
            c.cv.notify_all();
        }
    }

    /// Is `entry` still the map's cache for `k`? `addons_changed` clears the map and a TTL expiry
    /// replaces the entry, and a fill must not keep fetching into an orphan (nor report it to the
    /// client as news).
    fn catalog_entry_is_current(
        &self,
        k: &(String, Option<String>),
        entry: &Arc<Mutex<CatalogCache>>,
    ) -> bool {
        self.catalogs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(k)
            .is_some_and(|current| Arc::ptr_eq(current, entry))
    }

    /// Fetch pages into the cache until `want` rows are held (or the catalog is exhausted, or the
    /// page cap is reached). The lock is taken per page, never across a fetch, so readers answer
    /// from what has landed. On success the generation moves: a listing that answered short is
    /// re-read by the library.
    fn fill_catalog(
        self: &Arc<Self>,
        entry: Arc<Mutex<CatalogCache>>,
        k: (String, Option<String>),
        fetch_genre: Option<String>,
        match_genre: Option<String>,
        want: usize,
    ) {
        self.acquire();
        let mut failed = false;
        let mut added = false;
        let mut stale = false;
        let mut pages = 0;
        while pages < 20 {
            if !self.catalog_entry_is_current(&k, &entry) {
                stale = true;
                break;
            }
            let (skip, done) = {
                let c = entry.lock().unwrap_or_else(|e| e.into_inner());
                (c.rows.len() as u32, c.exhausted || c.rows.len() >= want)
            };
            if done {
                break;
            }
            pages += 1;
            match self.backend.catalog_page(&k.0, skip, fetch_genre.as_deref()) {
                Ok(page) => {
                    let mut c = entry.lock().unwrap_or_else(|e| e.into_inner());
                    let before = c.rows.len();
                    for p in page {
                        if match_genre
                            .as_deref()
                            .is_some_and(|g| !p.genres.iter().any(|x| genre_matches(x, g)))
                        {
                            continue;
                        }
                        if !c.rows.iter().any(|r| r.id == p.id) {
                            c.rows.push(p);
                        }
                    }
                    if c.rows.len() == before {
                        c.exhausted = true;
                    }
                    added |= c.rows.len() > before;
                }
                Err(()) => {
                    failed = true;
                    break;
                }
            }
        }
        {
            let mut c = entry.lock().unwrap_or_else(|e| e.into_inner());
            // A failure is NOT an empty catalog. Expire the entry so a later read retries, and
            // back the retry off: a dead addon must not be asked on every page read that follows.
            if failed && c.rows.is_empty() {
                c.at = Instant::now() - CATALOG_TTL;
            }
            if failed {
                c.retry_at = Some(Instant::now() + CATALOG_RETRY);
            }
            c.filling = false;
            c.cv.notify_all();
        }
        self.release();
        if !stale && (!failed || added) {
            self.catalog_gen.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn meta(&self, type_: &str, id: &str) -> Option<Arc<Meta>> {
        let gate = format!("{type_}\u{0}{id}");
        loop {
            if let Some(m) = self.metas.get(type_, id) {
                return Some(m);
            }
            // The focus prefetch and the click that follows it miss together; the second caller
            // waits here for the first fetch and then finds it in the cache. A refused read
            // (`None` below) releases the key, so the waiter asks again rather than reading the
            // refusal as the title not existing.
            let Some(_turn) = self.meta_flight.claim(&gate) else {
                continue;
            };
            let m = Arc::new(self.backend.meta(type_, id)?);
            self.metas.insert(type_, id, Arc::clone(&m));
            return Some(m);
        }
    }

    /// A stable source identity for one ranked stream — exactly the fields
    /// `shim_store::LastStream` carries, built from the classified stream. The
    /// signed URL is never included: a debrid link expires, and a torrent digest
    /// is the file the engine selects by `file_idx` anyway.
    fn last_stream_of(c: &Classified) -> LastStream {
        LastStream {
            addon: c.stream.addon_name.clone(),
            info_hash: plx_stremio::torrent::info_hash(&c.stream),
            file_idx: c.stream.file_idx,
            filename: c.info.filename.clone(),
            size_bytes: c.info.size_bytes,
            height: c.info.height,
            label: c.info.label.clone(),
        }
    }

    /// Does candidate `c` answer the remembered source? Express release identity
    /// outranks profile identity: a torrent hash (+ its file index) names one
    /// file in one swarm, while addon + size + filename + height is the direct/
    /// debrid fallback, since a signed URL expires with every re-resolution.
    fn matches_last(c: &Classified, last: &LastStream) -> bool {
        if let Some(ch) = plx_stremio::torrent::info_hash(&c.stream) {
            return last.info_hash.as_deref() == Some(ch.as_str())
                && (last.file_idx.is_none() || c.stream.file_idx == last.file_idx);
        }
        if last.info_hash.is_some() {
            return false;
        }
        let same_addon = !last.addon.is_empty() && c.stream.addon_name == last.addon;
        let same_name = match (&last.filename, &c.info.filename) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        };
        let same_size = match (last.size_bytes, c.info.size_bytes) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        };
        let same_height = match (last.height, c.info.height) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        };
        same_addon && same_name && same_size && same_height
    }

    /// Re-order a fresh ranking, promoting the remembered source ahead of the
    /// best-ranked stream — unless the viewer already picked inside the slot.
    fn promote_last_stream(
        out: &mut Vec<(usize, Classified)>,
        last: Option<&LastStream>,
        already_selected: bool,
    ) {
        if already_selected {
            return;
        }
        let Some(last) = last else { return };
        if let Some(p) = out.iter().position(|(_, c)| Self::matches_last(c, last)) {
            let x = out.remove(p);
            out.insert(0, x);
        }
    }

    /// The (type, id) an item's streams are asked for, and its runtime.
    fn stream_target(&self, ident: &Ident) -> Option<(String, String)> {
        match ident {
            Ident::Title { type_, id } if !is_series(type_) => Some((type_.clone(), id.clone())),
            Ident::Episode { video, .. } => Some(("series".into(), video.clone())),
            _ => None,
        }
    }

    /// The id an addon should be asked for: the catalog's own id, upgraded to the IMDb id TMDB
    /// names when it is a `tmdb:<id>`. Episodes keep their `:S:E` tail. A miss (no TMDB key, or no
    /// external id) leaves the original id, because an empty Source Picker is worse than one an
    /// addon may still resolve.
    fn addon_id_for(&self, type_: &str, id: &str) -> String {
        let (base, suffix) = split_content_id(id);
        match self.imdb_base(type_, base) {
            Some(imdb) => format!("{imdb}{suffix}"),
            None => id.to_string(),
        }
    }

    /// The IMDb id behind a content id, resolved through TMDB when it is a `tmdb:<id>` (`type_`
    /// decides movie vs. series). An `tt…` id is already the answer; anything else (kitsu, …) has
    /// none. Cached with [`Shim::imdb_ids`], whose negative answers are cached too: a title TMDB
    /// has no IMDb id for is not retried on every request.
    fn imdb_base(&self, type_: &str, base: &str) -> Option<String> {
        if is_imdb_id(base) {
            return Some(base.to_string());
        }
        let tmdb_id = tmdb_content_number(base)?;
        let key = (type_.to_string(), base.to_string());
        if let Some(hit) = self
            .imdb_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
        {
            return hit;
        }
        let imdb = TmdbClient::from_env()
            .and_then(|client| {
                if is_series(type_) {
                    client.tv_external_ids(tmdb_id).ok().flatten()
                } else {
                    client.movie_external_ids(tmdb_id).ok().flatten()
                }
            })
            .filter(|imdb| is_imdb_id(imdb));
        let mut g = self.imdb_ids.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() > 512 {
            g.clear();
        }
        g.insert(key, imdb.clone());
        imdb
    }

    fn slot(&self, rk: i64, ident: &Ident) -> Option<Arc<StreamSlot>> {
        let (type_, id) = self.stream_target(ident)?;
        // Synthetic related keys have no `Ident` in the table; they name the title as `tmdb:<id>`
        // to addons. Both that and a real catalog item whose id IS a `tmdb:` id go through the
        // same translation, so Torrentio and the subtitle addons get the IMDb id they index by.
        let addon_id = translate_to_stremio_id(rk).unwrap_or(id);
        let translated_id = self.addon_id_for(&type_, &addon_id);
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
        let subs_spawned = std::thread::Builder::new()
            .name("pmsshim-subs".into())
            .spawn(move || {
                let subs = subs_backend.subtitles(&st, &sid);
                let mut g = slot_subs.state.lock().unwrap_or_else(|e| e.into_inner());
                // Ids are assigned by the shim on first read (see `ranked`), so only stash here.
                g.subs = subs
                    .into_iter()
                    .take(40)
                    .map(|s| {
                        let title = s.release_title();
                        SubRef {
                            id: 0,
                            lang: format!("{}\u{0}{}", s.lang, s.url),
                            title,
                            selected: false,
                        }
                    })
                    .collect();
                g.subs_done = true;
                drop(g);
                slot_subs.cv.notify_all();
            })
            .is_ok();
        if !subs_spawned {
            let mut g = slot.state.lock().unwrap_or_else(|e| e.into_inner());
            g.subs_done = true;
            drop(g);
            slot.cv.notify_all();
        }
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
        // Only block playback on subtitle addons when the viewer actually has a preferred
        // subtitle language; otherwise there is nothing to auto-enable, so return streams at once.
        let wait_for_subs = self.account.subtitle_language().is_some();
        let mut first_playable: Option<Instant> = None;
        loop {
            if st.done && (!wait_for_subs || st.subs_done) {
                break;
            }
            if chosen
                && st.selected.is_some_and(|i| i < st.streams.len())
                && (!wait_for_subs || st.subs_done)
            {
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
                // Strict 2.5s cap: a slow subtitle addon never blocks playback.
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
        shim_media::apply_subtitle_preference(&mut st.subs, pref.as_deref());
        // Subtitles the viewer installed from the Search page ride beside the addons' own, and the
        // newest install is the server's pick — exactly what a real PMS does with an installed
        // sidecar (`docs/pms-api.md` §8). They outlive the slot, so they come from the shim, not
        // from the addon threads that filled it.
        let installed = self
            .installed_subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&rk)
            .cloned()
            .unwrap_or_default();
        if let Some(newest) = installed.last() {
            for s in &installed {
                if !st.subs.iter().any(|x| x.id == s.id) {
                    st.subs.push(SubRef {
                        id: s.id,
                        lang: s.lang.clone(),
                        title: s.title.clone(),
                        selected: false,
                    });
                }
            }
            let newest = newest.id;
            for s in st.subs.iter_mut() {
                s.selected = s.id == newest;
            }
        }
        let subs: Vec<SubRef> = st
            .subs
            .iter()
            .map(|s| SubRef {
                id: s.id,
                lang: s.lang.clone(),
                title: s.title.clone(),
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
        if selected.is_none() {
            let last = self.watch.get(rk).last_stream;
            Self::promote_last_stream(&mut out, last.as_ref(), false);
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
        self.remember_card(rk, &row);
        row
    }

    /// Keep the card a listing read will ask for again, so that read costs no addon `meta` and no
    /// TMDB. Bounded: a huge library clears rather than grows without limit, and a cleared card is
    /// simply re-resolved the old way.
    fn remember_card(&self, rk: i64, row: &Value) {
        let mut cards = self.cards.lock().unwrap_or_else(|e| e.into_inner());
        if cards.len() > 8192 {
            cards.clear();
        }
        cards.insert(rk, row.clone());
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
    ///
    /// The result is cached under `(type, id)`, so a title opened before costs nothing after a
    /// restart. Only a complete TMDB exchange is cached: no key, an unresolved id or a failed call
    /// falls back to the Cinemeta trailer and is retried next time, rather than freezing a
    /// transient failure for a month.
    fn enrich_row(&self, rk: i64, row: &mut Value) {
        let Some(Ident::Title { type_, id }) = self.keys.ident(rk) else {
            return;
        };
        if let Some(cached) = self.enrich.get(&type_, &id) {
            // A row written before the related shelf was kept has no recommendations in it: treat
            // it as incomplete and refresh it, so an upgraded install converges after one open.
            if cached.related.is_some() {
                cached.apply(row);
                return;
            }
        }
        let series = is_series(&type_);
        let Some(meta) = self.meta(&type_, &id) else {
            return;
        };
        let Some(client) = TmdbClient::from_env() else {
            let candidates = self.trailer_candidates(rk, false);
            if !candidates.is_empty() {
                row["Extras"] = trailer_extras(rk, &candidates);
            }
            return;
        };
        let tmdb_id = match if series {
            resolve_tmdb_series_id(&client, &id, meta.imdb_id.as_deref(), meta.moviedb_id)
        } else {
            resolve_tmdb_movie_id(&client, &id, meta.imdb_id.as_deref(), meta.moviedb_id)
        } {
            Ok(id) => id,
            Err(_) => None,
        };
        let Some(tmdb_id) = tmdb_id else {
            let candidates = self.trailer_candidates(rk, false);
            if !candidates.is_empty() {
                row["Extras"] = trailer_extras(rk, &candidates);
            }
            return;
        };

        let mut learned = Enrichment {
            tmdb_id,
            ..Default::default()
        };
        let mut complete = true;
        // One request now carries the cast, the related shelf, the logo AND
        // every trailer/teaser (`append_to_response=…,videos`). The old split
        // spent a second call per movie on `/movie/{id}/videos` just to pick
        // the first one; the shelf needs them all anyway.
        let videos = if series {
            match client.tv_details(tmdb_id) {
                Ok(details) => {
                    set_roles(row, &details.credits.cast);
                    learned.roles = row.get("Role").cloned();
                    learned.related = Some(RelatedSource::Shows(details.recommendations.results));
                    details.videos
                }
                Err(_) => {
                    complete = false;
                    None
                }
            }
        } else {
            match client.movie_details(tmdb_id) {
                Ok(details) => {
                    set_roles(row, &details.credits.cast);
                    learned.roles = row.get("Role").cloned();
                    learned.related = Some(RelatedSource::Movies(details.recommendations.results));
                    learned.collection_ref = details.movie.belongs_to_collection;
                    details.videos
                }
                Err(_) => {
                    complete = false;
                    None
                }
            }
        };
        let candidates = self.trailer_candidates_from_videos(rk, videos.as_ref());
        if !candidates.is_empty() {
            row["Extras"] = trailer_extras(rk, &candidates);
            learned.extras = row.get("Extras").cloned();
        }
        if complete {
            self.enrich.insert(&type_, &id, learned);
        }
    }

    /// Every trailer this title could answer with, most-preferred first.
    ///
    /// The order is the shelf order: TMDB's trailers before its teasers, then
    /// the ids Cinemeta already names. Deduped by YouTube id, so the same
    /// video listed by both sources appears once.
    ///
    /// `with_alternates` is the shelf/play-time call. A detail open has just
    /// fetched `movie_details`/`tv_details` (which now append `videos`), so
    /// the TMDB half is a cache hit and costs nothing; the cheap pass exists
    /// for the single-id callers.
    fn trailer_candidates(&self, rk: i64, with_alternates: bool) -> Vec<TrailerCandidate> {
        let real_ident = self.keys.ident(rk);
        let Some(Ident::Title { type_, id }) =
            real_ident.clone().or_else(|| synthetic_ident(rk))
        else {
            plx_base::eventlog::log(&format!("trailer: youtube id miss rk={rk} no title"));
            return Vec::new();
        };
        let series = is_series(&type_);
        let mut out = Vec::new();
        let mut meta_loaded = false;
        // Cheap pass first: the ids the title's cached meta already names.
        if !with_alternates {
            out = self.meta_candidates(&type_, &id);
            meta_loaded = true;
        }
        // TMDB `videos` for movies AND shows. A show's Cinemeta meta often names
        // no trailer at all, and a synthetic related-show key has no meta worth
        // a live miss (its `tmdb:` id is TMDB's own) — this is the source that
        // gives those a shelf. Host tests never reach YouTube/TMDB here; the
        // in-hand `trailer_candidates_from_videos` is what they exercise.
        if (with_alternates || out.is_empty()) && !cfg!(test) {
            if let Some(client) = TmdbClient::from_env() {
                let tmdb_id = self
                    .enrich
                    .get(&type_, &id)
                    .map(|enrich| enrich.tmdb_id)
                    .or_else(|| self.meta(&type_, &id).and_then(|m| m.moviedb_id))
                    .or_else(|| synthetic_show_tmdb_id(rk))
                    .or_else(|| synthetic_tmdb_id(rk))
                    .or_else(|| tmdb_id(&id, series));
                if let Some(tmdb_id) = tmdb_id {
                    let videos = if series {
                        client
                            .tv_details(tmdb_id)
                            .ok()
                            .and_then(|details| details.videos)
                    } else {
                        client
                            .movie_details(tmdb_id)
                            .ok()
                            .and_then(|details| details.videos)
                    };
                    if let Some(videos) = videos {
                        let mut from_tmdb = video_candidates(Some(&videos));
                        if !from_tmdb.is_empty() {
                            plx_base::eventlog::log(&format!(
                                "trailer: youtube ids from tmdb videos rk={rk} ({})",
                                from_tmdb.len()
                            ));
                            for candidate in from_tmdb.drain(..) {
                                push_candidate(&mut out, candidate.youtube_id, &candidate.title);
                            }
                        }
                    }
                }
            }
        }
        // Meta: always for a real key (Cinemeta can list a trailer TMDB does
        // not); for a synthetic key only when TMDB named none, so a related
        // show does not pay a live `tmdb:` meta miss on every Part request.
        if !meta_loaded && (real_ident.is_some() || out.is_empty()) {
            for candidate in self.meta_candidates(&type_, &id) {
                push_candidate(&mut out, candidate.youtube_id, &candidate.title);
            }
            if !out.is_empty() {
                plx_base::eventlog::log(&format!("trailer: youtube id from meta rk={rk}"));
            }
        }
        // Cinemeta IMDb fallback: `tmdb:` metas often carry no trailer while
        // the same title under its `tt…` id does.
        if out.is_empty() {
            if let Some(m) = self.meta(&type_, &id) {
                if let Some(imdb) = m
                    .imdb_id
                    .as_deref()
                    .filter(|s| s.starts_with("tt") && *s != id)
                {
                    plx_base::eventlog::log(&format!(
                        "trailer: youtube id imdb fallback rk={rk}"
                    ));
                    if let Some(imdb_meta) = self.meta(&type_, imdb) {
                        for yid in imdb_meta
                            .trailer_streams
                            .iter()
                            .chain(&imdb_meta.trailers)
                            .filter_map(youtube_id)
                        {
                            push_candidate(&mut out, yid, "Official Trailer");
                        }
                    }
                }
            }
        }
        if out.is_empty() {
            plx_base::eventlog::log(&format!(
                "trailer: youtube id miss rk={rk} type={type_} id={id}"
            ));
        }
        out
    }

    /// The ids one meta row names, with their labels.
    fn meta_candidates(&self, type_: &str, id: &str) -> Vec<TrailerCandidate> {
        let mut out = Vec::new();
        if let Some(meta) = self.meta(type_, id) {
            for value in meta.trailer_streams.iter().chain(&meta.trailers) {
                let Some(yid) = youtube_id(value) else { continue };
                let label = value
                    .get("name")
                    .or_else(|| value.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                push_candidate(&mut out, yid, label);
            }
        }
        out
    }

    /// The same candidates, built from a `details.videos` answer already in
    /// hand plus the title's Cinemeta meta. No TMDB round trip: the row builder
    /// that has just parsed the details must not ask TMDB the same question
    /// again (and a host test must stay offline).
    fn trailer_candidates_from_videos(
        &self,
        rk: i64,
        videos: Option<&plx_stremio::addons_tmdb::TmdbVideos>,
    ) -> Vec<TrailerCandidate> {
        let mut out = video_candidates(videos);
        // A synthetic key is never in `metas`, so its `meta()` is a live addon
        // miss. TMDB already named the trailers; do not pay that hop too.
        if !out.is_empty() && self.keys.ident(rk).is_none() {
            return out;
        }
        let Some(Ident::Title { type_, id }) = self.keys.ident(rk).or_else(|| synthetic_ident(rk))
        else {
            return out;
        };
        if let Some(meta) = self.meta(&type_, &id) {
            for value in meta.trailer_streams.iter().chain(&meta.trailers) {
                let Some(yid) = youtube_id(value) else { continue };
                let label = value
                    .get("name")
                    .or_else(|| value.get("title"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                push_candidate(&mut out, yid, label);
            }
        }
        out
    }

    /// The candidates the shelf may show: YouTube's anonymous age gate
    /// (`LOGIN_REQUIRED`) is permanent for an id, so a blocked one is dropped
    /// rather than offered as a tile that fails on OK.
    fn playable_candidates(&self, parent: i64) -> Vec<TrailerCandidate> {
        self.trailer_candidates(parent, true)
            .into_iter()
            .filter(|candidate| !self.trailer_resolver.blocked(&candidate.youtube_id))
            .take(MAX_TRAILERS)
            .collect()
    }

    /// Install the byte-offset reach used to gate DASH on its whole file (boot does this).
    pub fn set_trailer_reacher(&self, r: crate::shim_probe::Reacher) {
        *self.trailer_reach.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    }

    /// Start resolving the first trailer in the background as soon as the
    /// detail shelf is served. The screen autoplays after `DWELL_S` (2 s), and
    /// the resolve is the slow half (Node spawn, visitor fetch, player call,
    /// tail checks); doing it here means the player's first Part request finds
    /// the MPD already in `trailer_dash` instead of waiting for it.
    ///
    /// Only the first tile is warmed — the rest resolve on the click that asks
    /// for them (`trailer_row_at`). A no-op under test so call counts stay
    /// deterministic, and a cheap check of the published maps keeps a cached
    /// page from spawning a thread per open.
    fn prefetch_trailer(self: &Arc<Self>, parent: i64) {
        #[cfg(test)]
        {
            let _ = parent;
            return;
        }
        #[cfg(not(test))]
        {
            if self.playable_candidates(parent).is_empty() {
                return;
            }
            let dash_pid = trailer_rk_at(parent, 0) * 1000 + 998;
            let muxed_pid = trailer_rk_at(parent, 0) * 1000 + 999;
            let already = self
                .trailer_dash
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&dash_pid)
                || self
                    .trailer_parts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&muxed_pid);
            if already {
                return;
            }
            {
                let mut warm = self.trailer_warm.lock().unwrap_or_else(|e| e.into_inner());
                if !warm.insert(parent) {
                    return;
                }
                if warm.len() > 64 {
                    warm.clear();
                    warm.insert(parent);
                }
            }
            let me = Arc::clone(self);
            let spawned = std::thread::Builder::new()
                .name("trailer-warm".into())
                .spawn(move || {
                    let _ = me.trailer_row_at(parent, 0);
                    me.trailer_warm
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&parent);
                });
            if spawned.is_err() {
                self.trailer_warm
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&parent);
            }
        }
    }

    /// Can the whole DASH pair be fetched? googlevideo grants most anonymous client identities
    /// only the first 60 s of a trailer's adaptive track — the head answers and every later range
    /// is `403`, so the track starts and never finishes. Ask for the last 64 KiB of each track and
    /// publish the DASH row only when both answer. `true` when no reacher is installed (host bin,
    /// tests), where the probe alone decides exactly as before.
    ///
    /// The two tracks are checked together: each is head→tail on its own, so serialising them
    /// would spend four round trips where two are enough.
    fn trailer_tracks_reachable(&self, video: &str, audio: &str) -> bool {
        let reach = self
            .trailer_reach
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(reach) = reach else { return true };
        let deadline = std::time::Duration::from_secs(6);
        let check = |label: &str, url: &str| -> bool {
            let size = match reach(url, 0, deadline) {
                Ok(size) => size,
                Err(e) => {
                    plx_base::eventlog::log(&format!("trailer: dash {label} open failed: {e}"));
                    return false;
                }
            };
            if size <= 0 {
                return true;
            }
            let tail = (size - 65_536).max(0);
            if let Err(e) = reach(url, tail, deadline) {
                plx_base::eventlog::log(&format!(
                    "trailer: dash {label} throttled — refuses {} of {size}: {e}",
                    tail
                ));
                return false;
            }
            true
        };
        let ok = std::thread::scope(|scope| {
            let v = scope.spawn(|| check("video", video));
            let a = check("audio", audio);
            (v.join().unwrap_or(false), a)
        });
        if ok.0 && ok.1 {
            plx_base::eventlog::log("trailer: dash pair served to its end");
            true
        } else {
            false
        }
    }

    /// Muxed trailer probe: `mp4`/`mov` + `h264` + decodable `aac`. Hard-gated
    /// (the muxed `Media` is only served when true); logs the verdict.
    fn trailer_probe_ok_muxed_logged(&self, url: &str) -> bool {
        let Some(res) = self.probes.probe(url) else {
            plx_base::eventlog::log("trailer: muxed probe no prober");
            return false;
        };
        match res {
            Err(e) => {
                plx_base::eventlog::log(&format!("trailer: muxed probe failed: {e}"));
                false
            }
            Ok(p) => {
                let ok = (p.container == "mp4" || p.container == "mov")
                    && p.video.as_ref().is_some_and(|v| v.codec == "h264")
                    && p.video_decodable
                    && p.audio.iter().any(|a| a.codec == "aac" && a.decodable);
                plx_base::eventlog::log(&format!(
                    "trailer: muxed probe ok={ok} container={} vcodec={} aac={}",
                    p.container,
                    p.video.as_ref().map(|v| v.codec.as_str()).unwrap_or("-"),
                    p.audio.iter().any(|a| a.codec == "aac")
                ));
                ok
            }
        }
    }

    /// DASH video-track probe: `mp4`/`mov`/`webm` + `h264`/`vp9` + decodable
    /// video. Probes the address the player will actually open — the proxy
    /// loopback URL when the proxy answered, else the raw video-track URL.
    /// `None` = no prober (unverified: the caller publishes the DASH as
    /// before); `Some(false)` = a known refusal (e.g. a 403 on the
    /// video-only URL), which must not ship as a playable `Media`.
    fn trailer_probe_dash_logged(&self, url: &str) -> Option<bool> {
        let res = self.probes.probe(url)?;
        match res {
            Err(e) => {
                plx_base::eventlog::log(&format!("trailer: dash probe failed: {e}"));
                Some(false)
            }
            Ok(p) => {
                let ok = matches!(p.container.as_str(), "mp4" | "mov" | "webm")
                    && p.video
                        .as_ref()
                        .is_some_and(|v| v.codec == "h264" || v.codec == "vp9" || v.codec == "av1")
                    && p.video_decodable;
                plx_base::eventlog::log(&format!(
                    "trailer: dash probe ok={ok} container={} vcodec={}",
                    p.container,
                    p.video.as_ref().map(|v| v.codec.as_str()).unwrap_or("-")
                ));
                Some(ok)
            }
        }
    }

    /// The trailer clip row for one shelf tile: own `ratingKey`
    /// (`trailer_rk_at(parent, index)`), own `Media[]` (DASH idx0 when its
    /// probe passed, muxed idx1) and a Part in each map. `None` = no candidate
    /// or a muxed refusal. `dash_urls` carries the proxy BaseURLs for the DASH
    /// manifest (`None` = raw googlevideo, proxy unavailable).
    fn build_trailer_row(
        &self,
        parent: i64,
        index: usize,
        resolved: &crate::shim_trailer::TrailerResolved,
        youtube_id: &str,
        title: &str,
        dash_urls: Option<(String, String)>,
    ) -> Option<Value> {
        let clip_rk = trailer_rk_at(parent, index);
        let dash_part_id = clip_rk * 1000 + 998;
        let muxed_part_id = clip_rk * 1000 + 999;
        // i64 guard: ~5e9 * 1000 + 999 << i64::MAX, so the stranded part ids
        // can never wrap into another title's range.
        debug_assert!(dash_part_id < i64::MAX && muxed_part_id < i64::MAX);
        let mut media: Vec<Value> = Vec::new();
        if let Some(d) = &resolved.dash {
            let vmime = d.video_mime.to_ascii_lowercase();
            let is_vp9 = vmime.contains("vp9");
            let is_av1 = vmime.contains("av01") || vmime.contains("av1");
            let (container, vcodec) = if is_vp9 {
                ("webm", "vp9")
            } else if is_av1 {
                ("mp4", "av1")
            } else {
                ("mp4", "h264")
            };
            let duration = d.duration_ms;
            let bitrate_kbps = ((d.video_bitrate.max(0) + d.audio_bitrate.max(0)) / 1000) as i64;
            let (video_url, audio_url) = match &dash_urls {
                Some((v, a)) => (Some(v.as_str()), Some(a.as_str())),
                None => (None, None),
            };
            let mpd = dash_manifest(d, video_url, audio_url);
            self.trailer_dash
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(dash_part_id, mpd);
            let frame = if d.fps >= 49 {
                "60p"
            } else if d.fps >= 29 {
                "NTSC"
            } else {
                "24p"
            };
            let res = if d.height >= 2000 || d.width >= 3800 {
                "4k"
            } else if d.height >= 1000 || d.width >= 1900 {
                "1080"
            } else if d.height >= 700 || d.width >= 1260 {
                "720"
            } else if d.height >= 560 {
                "576"
            } else {
                "sd"
            };
            media.push(json!({
                "id": dash_part_id,
                "duration": duration,
                "bitrate": bitrate_kbps,
                "width": d.width,
                "height": d.height,
                "videoResolution": res,
                "container": container,
                "videoCodec": vcodec,
                "audioCodec": "aac",
                "audioChannels": 2,
                "videoProfile": "",
                "videoFrameRate": frame,
                "title": format!("Trailer ({}p)", d.height),
                "addon_name": "YouTube",
                "Part": [{
                    "id": dash_part_id,
                    "key": format!("/library/parts/{dash_part_id}/0/file.mpd"),
                    "duration": duration,
                    "file": "",
                    "size": 0,
                    "container": container,
                    "Stream": [
                        {"id": dash_part_id * 10 + 1, "streamType": 1, "codec": vcodec, "index": 0,
                         "width": d.width, "height": d.height, "frameRate": d.fps as f64,
                         "bitDepth": 8, "displayTitle": format!("{} ({})", res.to_uppercase(), vcodec.to_uppercase()),
                         "selected": true, "default": true},
                        {"id": dash_part_id * 10 + 2, "streamType": 2, "codec": "aac", "index": 1,
                         "channels": 2, "language": "en", "languageCode": "en",
                         "title": "AAC", "displayTitle": "en (AAC 2ch)",
                         "selected": true, "default": true},
                    ],
                }],
            }));
        }
        if let Some(muxed_url) = &resolved.muxed_url {
            let duration = resolved.dash.as_ref().map(|d| d.duration_ms).unwrap_or(0);
            let height = resolved.muxed_height;
            let width = match height {
                0 => 0,
                h if h >= 2000 => 3840,
                h if h >= 1000 => 1920,
                h if h >= 700 => 1280,
                h => h * 16 / 9,
            };
            let res = if height >= 1000 || width >= 1900 {
                "1080"
            } else if height >= 700 || width >= 1260 {
                "720"
            } else if height >= 560 {
                "576"
            } else {
                "sd"
            };
            self.trailer_parts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(muxed_part_id, muxed_url.clone());
            media.push(json!({
                "id": muxed_part_id,
                "duration": duration,
                "bitrate": 0,
                "width": width,
                "height": height,
                "videoResolution": res,
                "container": "mp4",
                "videoCodec": "h264",
                "audioCodec": "aac",
                "audioChannels": 2,
                "videoProfile": "",
                "videoFrameRate": "24p",
                "title": format!("Trailer ({}p)", height),
                "addon_name": "YouTube",
                "Part": [{
                    "id": muxed_part_id,
                    "key": format!("/library/parts/{muxed_part_id}/0/file.mp4"),
                    "duration": duration,
                    "file": "",
                    "size": 0,
                    "container": "mp4",
                    "Stream": [
                        {"id": muxed_part_id * 10 + 1, "streamType": 1, "codec": "h264", "index": 0,
                         "width": width, "height": height, "frameRate": 24.0,
                         "bitDepth": 8, "displayTitle": format!("{} (H264)", res.to_uppercase()),
                         "selected": true, "default": true},
                        {"id": muxed_part_id * 10 + 2, "streamType": 2, "codec": "aac", "index": 1,
                         "channels": 2, "language": "en", "languageCode": "en",
                         "title": "AAC", "displayTitle": "en (AAC 2ch)",
                         "selected": true, "default": true},
                    ],
                }],
            }));
        }
        if media.is_empty() {
            return None;
        }
        let duration = media[0]["duration"].as_i64().unwrap_or(0);
        let guid = if index == 0 {
            format!("nuvio://trailer/{parent}")
        } else {
            format!("nuvio://trailer/{parent}/{index}")
        };
        Some(json!({
            "ratingKey": clip_rk.to_string(),
            "key": format!("/library/metadata/{clip_rk}"),
            "guid": guid,
            "type": "clip",
            "subtype": "trailer",
            "extraType": 1,
            "title": title,
            "summary": "",
            "thumb": format!("https://img.youtube.com/vi/{youtube_id}/hqdefault.jpg"),
            "librarySectionID": 1,
            "librarySectionKey": "/library/sections/1",
            "duration": duration,
            "Media": Value::Array(media),
        }))
    }

    /// Resolve the FIRST trailer for `parent`, walking the candidates the
    /// shelf shows. A candidate whose DASH pair fails the probe/tail gate falls
    /// through to the next; the first verified muxed row is kept only when no
    /// DASH candidate survives. The result is published at index 0, so the
    /// shelf's first tile and `GET /library/metadata/{rk}/trailer` agree.
    ///
    /// This never runs on a detail open: `/extras` answers the shelf rows from
    /// `trailer_shelf_rows`, and the resolve happens on the player's first Part
    /// request (`part`) or in the background warm that request then finds.
    fn trailer_row(&self, parent: i64) -> Option<Value> {
        let candidates = self.playable_candidates(parent);
        let mut muxed_only: Option<(TrailerCandidate, crate::shim_trailer::TrailerResolved)> =
            None;
        for candidate in &candidates {
            let Some(resolved) = crate::shim_trailer::resolve_cached(
                self.trailer_resolver.as_ref(),
                &self.trailer_cache,
                &self.trailer_flight,
                &candidate.youtube_id,
            ) else {
                continue;
            };
            if resolved.dash.is_some() {
                if let Some(row) = self.publish_trailer(parent, 0, resolved, candidate) {
                    return Some(row);
                }
                continue;
            }
            if muxed_only.is_none() {
                muxed_only = Some((candidate.clone(), resolved));
            }
        }
        let (candidate, resolved) = muxed_only?;
        self.publish_trailer(parent, 0, resolved, &candidate)
    }

    /// Resolve the trailer at `index` exactly — the tile the viewer clicked,
    /// not whichever candidate answers best. `None` leaves that Part 404 rather
    /// than silently playing a different trailer.
    fn trailer_row_at(&self, parent: i64, index: usize) -> Option<Value> {
        let candidates = self.playable_candidates(parent);
        if let Some(candidate) = candidates.get(index) {
            let resolved = crate::shim_trailer::resolve_cached(
                self.trailer_resolver.as_ref(),
                &self.trailer_cache,
                &self.trailer_flight,
                &candidate.youtube_id,
            );
            if let Some(row) = resolved
                .and_then(|resolved| self.publish_trailer(parent, index, resolved, candidate))
            {
                return Some(row);
            }
            if index != 0 {
                // The viewer pressed THIS tile; do not silently play another.
                return None;
            }
        }
        // Tile 0 is the autoplay tile: if its grant is limited, keep the old
        // walk so the screen still gets a playable trailer instead of a 404.
        if index == 0 {
            return self.trailer_row(parent);
        }
        None
    }

    /// Probe/tail-gate one resolved candidate and build its clip row, or
    /// `None` when it must not be served. `index` names the shelf tile this
    /// candidate belongs to, so its clip key and Part ids are that tile's.
    fn publish_trailer(
        &self,
        parent: i64,
        index: usize,
        resolved: crate::shim_trailer::TrailerResolved,
        candidate: &TrailerCandidate,
    ) -> Option<Value> {
        let youtube_id = candidate.youtube_id.as_str();
        if let Some(dash) = &resolved.dash {
            // Direct googlevideo bytes: the manifest carries the signed https
            // BaseURLs, and `curlio` states the bounded window those URLs
            // require (no UA override, no proxy hop — the proxy child stays in
            // the tree unused for track bytes).
            let dash_urls: Option<(String, String)> = None;
            let probe_url = &dash.video_url;
            // Probe picks the codec; reach proves the file's END. A throttled
            // pair passes the probe and then fails mid-playback, which is worse
            // than a consistent 360p row. Both asks are independent network
            // reads — run them together so the gate costs one round trip, not
            // two, on the autoplay path.
            let (reachable, probed) = std::thread::scope(|scope| {
                let reach =
                    scope.spawn(|| self.trailer_tracks_reachable(&dash.video_url, &dash.audio_url));
                let probe = self.trailer_probe_dash_logged(probe_url);
                (reach.join().unwrap_or(false), probe)
            });
            match probed.map(|ok| ok && reachable) {
                Some(false) => {
                    // Known-bad DASH (e.g. a 403 on the video-only URL):
                    // fall back to the verified muxed row so autoplay still
                    // plays 360p instead of erroring on idx0.
                    let muxed_url = resolved
                        .muxed_url
                        .clone()
                        .unwrap_or_else(|| resolved.url.clone());
                    if muxed_url.is_empty() {
                        return None;
                    }
                    if !self.trailer_probe_ok_muxed_logged(&muxed_url) {
                        plx_base::eventlog::log(&format!(
                            "trailer: muxed probe refusal rk={parent} (dash fallback)"
                        ));
                        return None;
                    }
                    let mut muxed_only = resolved.clone();
                    muxed_only.dash = None;
                    return self.build_trailer_row(
                        parent,
                        index,
                        &muxed_only,
                        youtube_id,
                        &candidate.title,
                        None,
                    );
                }
                Some(true) | None => {
                    return self.build_trailer_row(
                        parent,
                        index,
                        &resolved,
                        youtube_id,
                        &candidate.title,
                        dash_urls,
                    );
                }
            }
        }
        let muxed_url = resolved
            .muxed_url
            .clone()
            .unwrap_or_else(|| resolved.url.clone());
        if muxed_url.is_empty() {
            return None;
        }
        if !self.trailer_probe_ok_muxed_logged(&muxed_url) {
            plx_base::eventlog::log(&format!("trailer: muxed probe refusal rk={parent}"));
            return None;
        }
        self.build_trailer_row(
            parent,
            index,
            &resolved,
            youtube_id,
            &candidate.title,
            None,
        )
    }

    /// Every trailer's shelf row, built **without** a YouTube resolve.
    ///
    /// `/extras` is on the detail page's critical path, and resolving there
    /// spawned a Node child for every title touched: measured as a 3–5 s detail
    /// open against a 1 s baseline, serialized by the decipher gate. Each clip's
    /// DASH Part key is deterministic, so the shelf advertises all of them
    /// immediately and the player resolves the one it asked for.
    fn trailer_shelf_rows(&self, parent: i64) -> Vec<Value> {
        self.playable_candidates(parent)
            .iter()
            .enumerate()
            .map(|(index, candidate)| self.trailer_shelf_row_at(parent, index, candidate))
            .collect()
    }

    fn trailer_shelf_row_at(
        &self,
        parent: i64,
        index: usize,
        candidate: &TrailerCandidate,
    ) -> Value {
        let clip_rk = trailer_rk_at(parent, index);
        let dash_part_id = clip_rk * 1000 + 998;
        let guid = if index == 0 {
            format!("nuvio://trailer/{parent}")
        } else {
            format!("nuvio://trailer/{parent}/{index}")
        };
        json!({
            "ratingKey": clip_rk.to_string(),
            "key": format!("/library/metadata/{clip_rk}"),
            "guid": guid,
            "type": "clip",
            "subtype": "trailer",
            "extraType": 1,
            "title": candidate.title,
            "summary": "",
            "thumb": format!(
                "https://img.youtube.com/vi/{}/hqdefault.jpg",
                candidate.youtube_id
            ),
            "librarySectionID": 1,
            "librarySectionKey": "/library/sections/1",
            "duration": 0,
            "Media": [{
                "id": dash_part_id,
                "duration": 0,
                "bitrate": 0,
                "width": 1920,
                "height": 1080,
                "videoResolution": "1080",
                "container": "mp4",
                "videoCodec": "h264",
                "audioCodec": "aac",
                "audioChannels": 2,
                "videoProfile": "",
                "videoFrameRate": "24p",
                "title": "Trailer",
                "addon_name": "YouTube",
                "Part": [{
                    "id": dash_part_id,
                    "key": format!("/library/parts/{dash_part_id}/0/file.mpd"),
                    "duration": 0,
                    "file": "",
                    "size": 0,
                    "container": "mp4",
                    "Stream": [],
                }],
            }],
        })
    }

    /// `GET /library/metadata/{rk}/extras` (the shelf: every trailer, no
    /// resolve) and `GET /library/metadata/{rk}/trailer` (the single resolved
    /// clip row the bridge reads).
    fn trailer_single_response(&self, rk: &str, shelf: bool) -> Response {
        let Ok(parent) = rk.parse::<i64>() else {
            return Response::json(&container(json!({"Metadata": []})));
        };
        let rows = if shelf {
            self.trailer_shelf_rows(parent)
        } else {
            self.trailer_row(parent).into_iter().collect()
        };
        Response::json(&container(json!({"Metadata": rows})))
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
        // A title this shim enriched before answers its shelf from the persisted enrichment — no
        // TMDB call, and no key needed. The rows are still rebuilt through the normal builders,
        // because building one is what registers the fallback key a click on that card needs.
        if let Some(Ident::Title { type_, id }) = self.keys.ident(rk) {
            if let Some(cached) = self.enrich.get(&type_, &id) {
                if let Some(source) = &cached.related {
                    let (kind, rows): (&str, Vec<Value>) = match source {
                        RelatedSource::Movies(movies) => (
                            "movie",
                            movies
                                .iter()
                                .filter(|movie| {
                                    movie.id > 0
                                        && movie.id
                                            <= i64::MAX as u64 - TMDB_FALLBACK_KEY_BASE as u64
                                })
                                .take(10)
                                .map(|movie| self.tmdb_recommendation_row(movie))
                                .collect(),
                        ),
                        RelatedSource::Shows(shows) => (
                            "show",
                            shows
                                .iter()
                                .filter(|show| {
                                    show.id > 0
                                        && show.id <= i64::MAX as u64 - TMDB_SHOW_KEY_BASE as u64
                                })
                                .take(10)
                                .map(|show| self.tmdb_show_recommendation_row(show))
                                .collect(),
                        ),
                    };
                    let mut hubs = vec![related_hub_json(kind, rows)];
                    // The franchise shelf. Its films are attached by the fresh path below (and
                    // reattached here when an older entry only kept the reference), so the next
                    // open builds it from disk too.
                    if let Some(parts) = cached.collection.as_ref() {
                        self.remember_collection(parts);
                        hubs.extend(self.collection_hub_json(parts, cached.tmdb_id, &parts.name));
                    } else if let Some(collection) = &cached.collection_ref {
                        if let Some(parts) = self.tmdb_collection(collection.id) {
                            self.remember_collection(&parts);
                            hubs.extend(self.collection_hub_json(
                                &parts,
                                cached.tmdb_id,
                                &collection.name,
                            ));
                            self.enrich.remember_collection(&type_, &id, &parts);
                        }
                    }
                    return Response::json(&container(json!({"Hub": hubs})));
                }
            }
        }
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
            let hub = self.collection_hub(rk, &details, &client);
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
    fn collection_hub(
        &self,
        rk: i64,
        details: &TmdbMovieDetails,
        client: &TmdbClient,
    ) -> Option<Value> {
        let collection = details
            .movie
            .belongs_to_collection
            .as_ref()
            .filter(|collection| collection.id > 0)?;
        let parts = client.collection_details(collection.id).ok()?;
        self.remember_collection(&parts);
        // Attach the films to the persisted enrichment, so the next open builds this shelf from
        // disk instead of asking TMDB again.
        if let Some(Ident::Title { type_, id }) = self.keys.ident(rk) {
            self.enrich.remember_collection(&type_, &id, &parts);
        }
        let hub = self.collection_hub_json(&parts, details.movie.id, &collection.name)?;
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
        let meta = self.meta("movie", &id);
        let imdb_id = meta.as_ref().and_then(|meta| meta.imdb_id.clone());
        let moviedb_id = meta.as_ref().and_then(|meta| meta.moviedb_id);
        resolve_tmdb_movie_id(client, &id, imdb_id.as_deref(), moviedb_id)
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
        let meta = self.meta("series", &id);
        let imdb_id = meta.as_ref().and_then(|meta| meta.imdb_id.clone());
        let moviedb_id = meta.as_ref().and_then(|meta| meta.moviedb_id);
        resolve_tmdb_series_id(client, &id, imdb_id.as_deref(), moviedb_id)
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
        // Synthetic keys never reach `enrich_row` (no `Ident`), so their
        // trailer `Extras` come from the details just parsed — TMDB `videos`
        // is the only source a related show has.
        let candidates = self.trailer_candidates_from_videos(rk, details.videos.as_ref());
        if !candidates.is_empty() {
            row["Extras"] = trailer_extras(rk, &candidates);
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
        let candidates = self.trailer_candidates_from_videos(rk, details.videos.as_ref());
        if !candidates.is_empty() {
            row["Extras"] = trailer_extras(rk, &candidates);
        }
        row
    }

    fn episode_key(&self, series: &str, video: &str) -> i64 {
        self.keys.key_of(&Ident::Episode {
            series: series.into(),
            video: video.into(),
        })
    }

    /// `/library/people/{guid}` — the person page's header (name, biography, life facts, roles).
    ///
    /// `guid` is the synthetic [`person_guid`] a cast row carried as `tagKey`. Anything else is an
    /// EMPTY container, the same answer a real Discover provider gives for a person it has never
    /// heard of: the page settles on portrait + name rather than retrying forever.
    fn person_profile_page(&self, guid: &str) -> Response {
        let Some(person_id) = person_id_of_guid(guid) else {
            return Response::json(&container(json!({"Metadata": []})));
        };
        let Some(client) = TmdbClient::from_env() else {
            return Response::json_status(
                503,
                &container(json!({"Metadata": [], "error": "TMDB_API_KEY is not configured"})),
            );
        };
        let details = match client.person_details(person_id) {
            Ok(details) => details,
            Err(error) => {
                return Response::json_status(
                    503,
                    &container(json!({
                        "Metadata": [],
                        "error": format!("TMDB person lookup failed: {error}")
                    })),
                )
            }
        };
        // The roles line and the filmography tabs are department COUNTS. They are derived from the
        // same combined-credits answer the `/credits` route reads, and TMDB's response cache makes
        // the second request free; a failure here still leaves a page worth drawing.
        let (cast, crew) = client.person_career(person_id).unwrap_or_default();
        Response::json(&container(json!({
            "Metadata": [self.person_profile_row(&details, &cast, &crew)]
        })))
    }

    /// `/library/people/{guid}/credits` — the person page's filmography, one `CreditGroup` per
    /// department. Cast leads, then the crew departments by size; each group's rows carry the
    /// character (cast) or the job (crew) and the `tmdb:<id>` identity the library join speaks.
    fn person_career_page(&self, guid: &str) -> Response {
        let Some(person_id) = person_id_of_guid(guid) else {
            return Response::json(&container(json!({"CreditGroup": []})));
        };
        let Some(client) = TmdbClient::from_env() else {
            return Response::json_status(
                503,
                &container(json!({"error": "TMDB_API_KEY is not configured"})),
            );
        };
        match client.person_career(person_id) {
            Ok((cast, crew)) => Response::json(&container(json!({
                "CreditGroup": self.person_credit_groups(&cast, &crew)
            }))),
            Err(error) => Response::json_status(
                503,
                &container(json!({"error": format!("TMDB person credits lookup failed: {error}")})),
            ),
        }
    }

    /// The header record of [`Self::person_profile_page`], from TMDB's person record and the
    /// department counts of the same career answer.
    fn person_profile_row(
        &self,
        details: &TmdbPerson,
        cast: &[TmdbCredit],
        crew: &[TmdbCredit],
    ) -> Value {
        let mut departments: Vec<(usize, String, String)> = Vec::new();
        let acted = cast
            .iter()
            .filter(|credit| person_credit_item(credit, &credit.character).is_some())
            .count();
        if acted > 0 {
            departments.push((acted, "actor".to_string(), "Actor".to_string()));
        }
        let mut crew_departments: HashMap<String, (usize, String, String)> = HashMap::new();
        for credit in crew {
            let Some((kind, title)) = person_department_group(&credit.department) else {
                continue;
            };
            // The catch-all bucket is a tab, never a role: a header reading "Actor · Producer ·
            // Other" says less than the two departments the viewer came to know them by.
            if kind == "other" || person_credit_item(credit, &credit.job).is_none() {
                continue;
            }
            let entry = crew_departments
                .entry(kind.clone())
                .or_insert((0, kind, title));
            entry.0 += 1;
        }
        departments.extend(crew_departments.into_values());
        // Most credits first, exactly the order the provider's own roles line uses.
        departments.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let credit_types: Vec<Value> = departments
            .into_iter()
            .map(|(_, kind, title)| json!({"type": kind, "title": title}))
            .collect();
        json!({
            "title": details.name,
            "summary": details.biography,
            "bornAt": details.birthday.clone().unwrap_or_default(),
            "diedAt": details.deathday.clone().unwrap_or_default(),
            "birthPlace": details.place_of_birth.clone().unwrap_or_default(),
            "knownFor": details.known_for_department.clone().unwrap_or_default(),
            "thumb": tmdb_image(details.profile_path.as_deref()).unwrap_or_default(),
            "CreditType": credit_types,
        })
    }

    /// [`Self::person_career_page`]'s `CreditGroup[]`, without a request: the mapping, the group
    /// order and each row's identity, host-testable without TMDB.
    fn person_credit_groups(&self, cast: &[TmdbCredit], crew: &[TmdbCredit]) -> Vec<Value> {
        let mut groups: Vec<Value> = Vec::new();
        let acted: Vec<Value> = cast
            .iter()
            .filter_map(|credit| person_credit_item(credit, &credit.character))
            .collect();
        if !acted.is_empty() {
            groups.push(person_credit_group("actor", "Actor", acted));
        }
        let mut by_department: Vec<(String, String, Vec<Value>)> = Vec::new();
        for credit in crew {
            let Some((kind, title)) = person_department_group(&credit.department) else {
                continue;
            };
            let Some(row) = person_credit_item(credit, &credit.job) else {
                continue;
            };
            match by_department
                .iter_mut()
                .find(|(known, _, _)| *known == kind)
            {
                Some((_, _, rows)) => rows.push(row),
                None => by_department.push((kind, title, vec![row])),
            }
        }
        // Most credits first, then the department's own name, so the tab strip is stable.
        by_department.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then_with(|| a.1.cmp(&b.1)));
        for (kind, title, rows) in by_department {
            groups.push(person_credit_group(&kind, &title, rows));
        }
        groups
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
                        self.tmdb_fallback_row(
                            &credit.movie(),
                            TMDB_FALLBACK_KEY_BASE + credit.id as i64,
                        ),
                    ),
                    "tv" => (
                        year_of(credit.first_air_date.as_deref()).unwrap_or(0),
                        self.tmdb_show_fallback_row(
                            &credit.show(),
                            TMDB_SHOW_KEY_BASE + credit.id as i64,
                        ),
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
            .find(|row| {
                row["ratingKey"]
                    .as_str()
                    .and_then(|v| v.parse::<i64>().ok())
                    == Some(rk)
            })
            .cloned();
        cached.or_else(|| self.tmdb_synthetic_row(rk))
    }

    /// The URL to open for a ranked stream: itself, or — for a torrent marker — a URL the streaming
    /// engine serves (season/episode let it pick the file in a season pack). The in-process C engine
    /// (`libsbengine.so`) is the only streaming engine; there is no Node `server.js` fallback.
    fn open_url(&self, rk: i64, stream: &Stream, url: &str) -> Option<String> {
        if !url.starts_with(shim_media::TORRENT_SCHEME) {
            return Some(url.to_string());
        }
        if let Some(u) = self.cached_open_url(rk, url) {
            return Some(u);
        }
        let (season, episode) = match self.keys.ident(rk) {
            Some(Ident::Episode { video, .. }) => {
                let mut it = video.rsplit(':').filter_map(|x| x.parse::<u32>().ok());
                let e = it.next();
                (it.next(), e)
            }
            _ => (None, None),
        };
        let key = (rk, url.to_string());
        // P2P off drops every torrent before any resolve: `torrent_server()` is `None` then, and
        // that gate must run BEFORE the C-engine branch or a disabled switch would still play.
        let server = self.torrent_server()?;
        let server = server.as_str();
        // The C engine serves its own 127.0.0.1 Range URLs so no server prefix is required;
        // there is no Node fallback to dial when it cannot load.
        if let Some(u) = self.resolve_with_sbengine(stream, season, episode) {
            self.resolved
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, u.clone());
            return Some(u);
        }
        let u = plx_stremio::torrent::resolve(server, stream, season, episode).ok()?;
        self.resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key, u.clone());
        Some(u)
    }

    /// The cached resolve for a torrent marker, if it is still usable. An engine URL names one of
    /// the engine's streams; switching sources calls `sbengine::remember`, which deletes the
    /// previous torrent and with it that stream, and the engine then answers `/stream/<id>` with
    /// 404 for the rest of the session (the player retried it forever: `ff: http_open FAILED
    /// status=404`, `playback retry: resolving item again`, never recovering). Reuse a cached URL
    /// only while its torrent is still the engine's active one; a stale entry is dropped so the
    /// caller resolves again. Node URLs never move, so nothing is checked without the engine.
    fn cached_open_url(&self, rk: i64, marker: &str) -> Option<String> {
        let key = (rk, marker.to_string());
        let cached = self
            .resolved
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()?;
        if plx_stremio::sbengine::available()
            && !engine_url_is_current(marker, plx_stremio::sbengine::active_hash().as_deref())
        {
            self.resolved
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&key);
            return None;
        }
        Some(cached)
    }

    /// Try the in-process C engine (`libsbengine.so`). `libsbengine.so` links OpenSSL statically
    /// (graded OK on every webOS release in fwcompat's inventory). Where `available()`
    /// is false torrents are unavailable on that install — deliberately; there is no Node
    /// fallback path.
    fn resolve_with_sbengine(
        &self,
        stream: &Stream,
        season: Option<u32>,
        episode: Option<u32>,
    ) -> Option<String> {
        use plx_stremio::torrent;
        if !plx_stremio::sbengine::available() {
            return None;
        }
        let hash = torrent::info_hash(stream)?;
        // The engine takes a magnet URI. Prefer the stream's own magnet; when the
        // stream only carries `infoHash`, synthesize one from peer_sources
        // trackers (tracker:/dht: prefixes stripped to bare `tr=` params).
        let magnet = stream
            .url
            .as_deref()
            .filter(|u| u.trim().to_ascii_lowercase().starts_with("magnet:"))
            .map(str::to_owned)
            .or_else(|| {
                let sources = torrent::peer_sources(stream, &hash);
                if sources.is_empty() {
                    // No tracker info: a hash alone still lets the engine try
                    // DHT. Fall back to a bare hash-magnet.
                    return Some(format!("magnet:?xt=urn:btih:{hash}"));
                }
                let mut m = format!("magnet:?xt=urn:btih:{hash}");
                for s in &sources {
                    let bare = s
                        .strip_prefix("tracker:")
                        .or_else(|| s.strip_prefix("dht:"))
                        .unwrap_or(s);
                    // dht: entries are not trackers; skip them as `tr=` params.
                    if s.starts_with("dht:") {
                        continue;
                    }
                    m.push_str(&format!(
                        "&tr={}",
                        plx_stremio::addon_http::encode_component(bare)
                    ));
                }
                Some(m)
            })?;
        // `torrent::info_hash` gives us the engine's lowercased hash; `open`
        // keys on it, so pass it verbatim.
        let preferred = stream.file_idx;
        // season/episode currently only feed the Node-side file guesser; the
        // engine picks the largest file when `preferred_idx` is None, which
        // matches that heuristic for season packs.
        let _ = (season, episode);
        match plx_stremio::sbengine::resolve_stream(&magnet, &hash, preferred, 30_000) {
            Ok(u) => {
                // One line per opened stream: the log then says which engine served it, which is
                // the only evidence on a TV that the C engine (rather than Node) is in play.
                plx_base::eventlog::log(&format!(
                    "sbengine: serving {} via engine (file {})",
                    &hash[..hash.len().min(12)],
                    preferred.map_or("largest".to_string(), |i| i.to_string())
                ));
                Some(u)
            }
            Err(e) => {
                plx_base::eventlog::log(&format!("sbengine: resolve failed, torrents unavailable ({e})"));
                None
            }
        }
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
    ///
    /// The third element is the time spent waiting on the stream slot, so the metadata route can
    /// say what the detail read actually spent and what it spent it on (Stage 0's split).
    fn item_probed(
        &self,
        rk: i64,
        probe_index: Option<usize>,
    ) -> Option<(
        Value,
        Option<Result<crate::shim_probe::ProbeInfo, String>>,
        Duration,
    )> {
        // Trailer clip keys are their own row: own Media/Parts, probe-gated at
        // resolve time. Returning the clip Media directly (with a synthetic
        // `Ok` verdict) makes `/decision` direct-play the trailer Part;
        // falling through to the film's ranked streams is the "clicking the
        // trailer played the debrid movie" bug.
        if let Some((parent, index)) = trailer_key_parts(rk) {
            let _ = probe_index;
            let row = self.trailer_row_at(parent, index)?;
            let media0 = row["Media"][0].clone();
            let container = media0["container"].as_str().unwrap_or("mp4").to_string();
            let vcodec = media0["videoCodec"].as_str().unwrap_or("h264").to_string();
            let width = media0["width"].as_i64().unwrap_or(0) as i32;
            let height = media0["height"].as_i64().unwrap_or(0) as i32;
            let duration_ms = media0["duration"].as_i64().unwrap_or(0);
            let probe = crate::shim_probe::ProbeInfo {
                container,
                duration_ms,
                size: 0,
                video_decodable: true,
                video: Some(crate::shim_probe::Video {
                    codec: vcodec,
                    width,
                    height,
                    fps: 24.0,
                    bit_depth: 8,
                    profile: 0,
                    color_trc: 0,
                    color_primaries: 0,
                    color_space: 0,
                    dovi: None,
                }),
                audio: vec![crate::shim_probe::Audio {
                    codec: "aac".into(),
                    profile: 0,
                    channels: 2,
                    sample_rate: 0,
                    language: "en".into(),
                    title: String::new(),
                    default: true,
                    decodable: true,
                }],
                subtitles: Vec::new(),
            };
            return Some((row, Some(Ok(probe)), Duration::ZERO));
        }
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
        let mut streams_wait = Duration::ZERO;
        if self.stream_target(&ident).is_some() {
            let dur = row["duration"].as_i64().unwrap_or(0);
            // A detail read paints without Media: the placeholder Part below stands in while the
            // slot's background fetch keeps answering, and a play waits the full deadline at
            // `/decision` instead. Blocking the page on addons was the 800 ms in the click.
            let wait = if probe_index.is_some() {
                self.cfg.stream_deadline
            } else {
                Duration::ZERO
            };
            let ranked_at = Instant::now();
            let (ranked, subs, done) = self.ranked(
                rk,
                &ident,
                u32::try_from(dur / 1000).ok(),
                wait,
                probe_index.is_some(),
            );
            streams_wait = ranked_at.elapsed();
            let mut media = Vec::with_capacity(ranked.len().max(1));
            // Pre-probe the leading stream once the list is complete, off this request: the play that
            // usually follows a detail page then finds real fields (and a warmed source) waiting.
            if probe_index.is_none() && done && self.probes.has_prober() {
                if let Some((_, c)) = ranked.first() {
                    let url = self
                        .cached_open_url(rk, &c.url)
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
                    // Remember the source the viewer actually plays, so a
                    // resume re-selects it while it is still offered.
                    if let Some(Ok(_)) = r.clone() {
                        self.watch.set_last_stream(rk, Self::last_stream_of(c));
                    }
                    r
                } else {
                    self.cached_open_url(rk, &c.url)
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
        Some((row, verdict, streams_wait))
    }

    fn item(&self, rk: i64) -> Option<Value> {
        self.item_probed(rk, None).map(|(row, _, _)| row)
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
        } else {
            rk
        };
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
        let Some((mut row, verdict, _)) = self.item_probed(rk, Some(index)) else {
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
                    row["UltraBlurColors"] = blur_colors(row["thumb"].as_str().unwrap_or_default());
                }
                Some(row)
            })
            .collect()
    }

    fn continue_hub(&self, count: usize, section: Option<i64>) -> Value {
        let all = self.continue_rows(section);
        let total = all.len();
        let rows: Vec<Value> = all.into_iter().take(count).collect();
        json!({"title": "Continue Watching", "type": "mixed", "hubIdentifier": "home.continue",
               "key": "/hubs/continueWatching", "size": rows.len(), "more": total > rows.len(),
               "totalSize": total, "Metadata": rows})
    }

    /// The listing behind the Continue Watching hub: the same rows the preview shows, paged.
    /// `data`'s deck reads this (one window per server) and requires the echoed `offset` to equal
    /// the requested start; `totalSize` is the whole listing, not a page cap.
    fn continue_watching_items(&self, start: usize, size: usize) -> Response {
        let rows = self.continue_rows(None);
        let total = rows.len();
        let page: Vec<Value> = rows.into_iter().skip(start).take(size.min(200)).collect();
        Response::json(&container(json!({
            "Metadata": page,
            "offset": start,
            "totalSize": total,
        })))
    }
    /// The account's My List, keyed by the shim's numeric ratingKeys. `refresh` forces a network
    /// pull on the first page; the account caches the answer for a minute otherwise.
    ///
    /// This is the one read the Nuvio Watchlist is built from. Upstream's page asks
    /// `discover.provider.plex.tv` for a plex.tv Watchlist, which this fork has no credential for;
    /// the fork's list is the account's saved library, and the shim is where that boundary lives.
    pub fn my_list_entries(&self, refresh: bool) -> plx_stremio::model::Result<Vec<MyListEntry>> {
        let items = self.account.saved_library(refresh)?;
        Ok(items
            .into_iter()
            .map(|p| MyListEntry {
                rating_key: self.keys.key_of(&Ident::Title {
                    type_: p.type_.clone(),
                    id: p.id.clone(),
                }),
                type_: p.type_.to_string(),
                id: p.id.clone(),
                title: p.name.clone(),
                year: year_of(p.release_info.as_deref()).and_then(|y| i32::try_from(y).ok()),
                release: p.release_info.clone(),
                rating: rating_of(p.imdb_rating.as_deref()).map(|r| r as f32),
                thumb: self.poster_url(&p.id, p.poster.as_deref()),
            })
            .collect())
    }

    /// Set one title's My List membership by the shim's numeric key (the Write half of
    /// [`my_list_entries`](Self::my_list_entries)).
    pub fn set_my_list_saved(
        &self,
        rating_key: i64,
        saved: bool,
    ) -> plx_stremio::model::Result<()> {
        if !self.account.signed_in() {
            return Err(plx_stremio::model::Error::Status(401));
        }
        self.save_my_list_title(rating_key, saved)
    }

    /// The one save/remove path: resolve the key to the account's own id, resolve the metadata the
    /// addon needs, and let the account confirm the new list.
    fn save_my_list_title(&self, rk: i64, saved: bool) -> plx_stremio::model::Result<()> {
        let Some(Ident::Title { type_, id }) = self.keys.ident(rk).or_else(|| synthetic_ident(rk))
        else {
            return Err(plx_stremio::model::Error::Status(404));
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
        // A removal needs only the account's own `(type, id)`. It must NOT wait on the add-on
        // metadata cache: that cache is in memory with a six-hour TTL, and the Add that warmed it
        // may be hours old. Requiring a full `Meta` here made a Remove fail with a 404 once the
        // cache had forgotten the title, while the Add (made from the page that had just loaded
        // it) still worked.
        let meta =
            match Self::write_meta(self.meta(&saved_type, save_id), &saved_type, save_id, saved) {
                Some(meta) => meta,
                None => {
                    let (name, poster_path, description, release_info) = if saved_type == "series" {
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
                        let tmdb_id =
                            synthetic_tmdb_id(rk).ok_or(plx_stremio::model::Error::Status(404))?;
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
                        release_info: year_of(release_info.as_deref()).map(|year| year.to_string()),
                        ..Meta::default()
                    })
                }
            };
        eprintln!("saving My List item: type={} id={}", meta.type_, meta.id);
        self.account.set_saved(&meta, saved)
    }

    /// The metadata one My List write hands the account. A removal needs only the account's own
    /// `(type, id)`; the name, poster and description exist to CREATE the row, not to delete it.
    /// An add therefore still requires the cache (or the TMDB fallback), while a removal may go
    /// ahead without it.
    fn write_meta(
        cached: Option<Arc<Meta>>,
        saved_type: &str,
        save_id: &str,
        saved: bool,
    ) -> Option<Arc<Meta>> {
        cached.or_else(|| {
            (!saved).then(|| {
                Arc::new(Meta {
                    id: save_id.to_owned(),
                    type_: saved_type.to_owned(),
                    ..Meta::default()
                })
            })
        })
    }

    #[allow(dead_code)]
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
        let Some(rk) = req.q("ratingKey").and_then(|v| v.parse().ok()) else {
            return Response::json_status(400, &json!({"error":"invalid title"}));
        };
        let Some(Ident::Title { type_, id }) = self.keys.ident(rk).or_else(|| synthetic_ident(rk))
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
                self.save_my_list_title(rk, saved)?;
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
    fn catalog_hub(
        self: &Arc<Self>,
        i: usize,
        c: &CatalogInfo,
        count: usize,
        pageable: bool,
        wait: Duration,
    ) -> Value {
        let (rows, total) = self.catalog_range(&c.key, None, None, None, 0, count, wait);
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
    /// within `budget` is left out of THIS answer and keeps fetching into the cache; a hub that
    /// answered with zero rows is dropped, so a shelf that failed to load shows no heading rather
    /// than a bare one (its failure is not cached, so the row returns on the next read).
    ///
    /// `window` is the hub window the client asked for, over the FILTERED list (the list pages by
    /// HUB, `docs/pms-api.md`): only that slice is fetched, and `i` stays the row's position in the
    /// whole filtered list, because that index is what `/hubs/nuvio/{i}` resolves. Ignoring the
    /// window is what made a Recommended page take a minute: the client reads it a window at a
    /// time, and every window used to re-fetch every catalog.
    fn catalog_hubs(
        self: &Arc<Self>,
        list: &[CatalogInfo],
        filter: impl Fn(&CatalogInfo) -> bool,
        count: usize,
        pageable: bool,
        budget: Duration,
        window: Option<(usize, usize)>,
    ) -> (Vec<Value>, usize) {
        let all: Vec<(usize, CatalogInfo)> = list
            .iter()
            .cloned()
            .enumerate()
            .filter(|(_, c)| filter(c))
            .collect();
        let total = all.len();
        let cats: Vec<(usize, CatalogInfo)> = match window {
            Some((start, size)) => all.into_iter().skip(start).take(size).collect(),
            None => all,
        };
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Value)>();
        for (pos, (i, c)) in cats.iter().cloned().enumerate() {
            let (me, tx) = (self.clone(), tx.clone());
            let _ = std::thread::Builder::new()
                .name("pmsshim-catalog".into())
                .spawn(move || {
                    // The fill worker takes the parallelism permit; holding one here while the
                    // hub waits on the cache would only starve the fill that has to answer it.
                    // A hair under the receiver's deadline, which still has to build and send.
                    let wait = budget.saturating_sub(Duration::from_millis(100));
                    let hub = me.catalog_hub(i, &c, count, pageable, wait);
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
        // A hub with no rows is a heading over nothing: drop it rather than draw the heading. The
        // failure that produced it is not cached (see `catalog_range`), so the row returns on the
        // next window read instead of blanking a shelf for the catalog's whole TTL.
        (
            got.into_iter()
                .flatten()
                .filter(|h| h["size"].as_u64().unwrap_or(0) > 0)
                .collect(),
            total,
        )
    }

    /// Fill the catalog list and every catalog's first page in the background (boot, or after the
    /// addon set changed), so the first `/hubs` is mostly cache hits.
    pub fn warm(self: &Arc<Self>) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("pmsshim-warm".into())
            .spawn(move || {
                let _ = me.account.saved_library(false);
                let list = me.catalog_list();
                let _ = me.catalog_hubs(&list, |_| true, 12, true, Duration::from_secs(60), None);
                // `/hubs` gives each catalog 4 s and leaves the late ones out of that answer.
                // A client that already asked may be holding one of those partial answers, so a
                // finished warm is itself a content change worth re-reading.
                me.bump_content();
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
            let type_ = self
                .account
                .saved_library(false)
                .ok()?
                .into_iter()
                .find(|item| item.id == saved_id)?
                .type_;
            Some(
                (if is_series(&type_) {
                    TMDB_SHOW_KEY_BASE
                } else {
                    TMDB_FALLBACK_KEY_BASE
                } + tmdb_id as i64),
            )
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
                let rows_at = Instant::now();
                let mut streams_wait = Duration::ZERO;
                // A CSV is a LISTING: the cards the shim handed out for a row the page released.
                // Answer it from the cards already built; only a key the shim has never shown falls
                // back to the addon. Resolving all 12 through addon `meta` + TMDB took ~9 s per row,
                // and the page reads rows one at a time.
                let listing = rk.contains(',');
                let mut rows: Vec<Value> = rk
                    .split(',')
                    .filter_map(|x| x.parse::<i64>().ok())
                    .filter_map(|k| {
                        if listing {
                            if let Some(card) = self
                                .cards
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get(&k)
                                .cloned()
                            {
                                return Some(card);
                            }
                        }
                        match self.person_row(k) {
                            Some(row) => Some(row),
                            None => self.item_probed(k, None).map(|(row, _, wait)| {
                                streams_wait += wait;
                                row
                            }),
                        }
                    })
                    .collect();
                if rows.is_empty() {
                    return Response::json_status(404, &container(json!({"Metadata": []})));
                }
                let meta_elapsed = rows_at.elapsed();
                let enrich_at = Instant::now();
                // A listing's cards are drawn as they are; the TMDB enrichment is for a DETAIL page,
                // and paying it per card is the other half of the same minute.
                if !listing {
                    for row in &mut rows {
                        if let Some(key) = row["ratingKey"]
                            .as_str()
                            .and_then(|value| value.parse().ok())
                        {
                            self.enrich_row(key, row);
                        }
                    }
                }
                let tmdb_elapsed = enrich_at.elapsed();
                // Stage 0's split, on the shim's side of the loopback: the app's own `detail:`
                // line times the whole GET it made, which cannot say whether the shim spent it
                // fetching addon meta, waiting on a stream slot, or enriching from TMDB. One key
                // in the URL is a detail read; a CSV is a listing and has no single split.
                if let Ok(key) = rk.parse::<i64>() {
                    plx_base::eventlog::log(&format!(
                        "detail: shim rk={key} meta_ms={} streams_ms={} tmdb_ms={} total_ms={}",
                        meta_elapsed.saturating_sub(streams_wait).as_millis(),
                        streams_wait.as_millis(),
                        tmdb_elapsed.as_millis(),
                        (meta_elapsed + tmdb_elapsed).as_millis(),
                    ));
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
                let key = rk
                    .parse()
                    .ok()
                    .or_else(|| tmdb_rating_key(rk))
                    .unwrap_or(-1);
                self.related_hub(key)
            }
            (_, ["library", "metadata", rk, "trailer"]) => self.trailer_single_response(rk, false),
            (_, ["library", "metadata", rk, "extras"]) => {
                let response = self.trailer_single_response(rk, true);
                // The screen autoplays the first tile after its dwell; resolve
                // it while the page paints instead of on the click.
                if let Ok(parent) = rk.parse::<i64>() {
                    self.prefetch_trailer(parent);
                }
                response
            }
            // The player's Subtitles → Search page (`docs/pms-api.md` §8): candidates from the
            // builtin OpenSubtitles v3 addon, and the install that makes one a sidecar.
            ("GET", ["library", "metadata", rk, "subtitles"]) => self.subtitle_search(rk, req),
            ("PUT", ["library", "metadata", rk, "subtitles"]) => self.subtitle_install(rk, req),
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
            // The person page's two GLOBAL reads. On a real PMS they are plex.tv Discover calls
            // keyed by the cast row's `tagKey`; under Nuvio the app's Discover base IS this shim
            // (`account::discover` follows the installed local server), so the shim answers them
            // from TMDB under the synthetic `nuvio-person-<id>` guid it stamped on every headshot.
            (_, ["library", "people", guid, "credits"]) => self.person_career_page(guid),
            (_, ["library", "people", guid]) => self.person_profile_page(guid),
            (_, ["library", "collections", rk, "children"]) => {
                let collection = rk
                    .parse()
                    .ok()
                    .and_then(synthetic_collection_id)
                    .and_then(|id| self.tmdb_collection(id));
                let Some(collection) = collection else {
                    return Response::json_status(
                        404,
                        &container(
                            json!({"Metadata": [], "error": "This collection is not available."}),
                        ),
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
                // The list pages BY HUB, so the window has to be honoured here too (see
                // `catalog_hubs`); Continue Watching is the list's first hub, as on a real PMS.
                let (start, size) = page_params(req);
                let size = size.unwrap_or(usize::MAX).max(1);
                let skip_cw = req.q("excludeContinueWatching") == Some("1");
                let mut hubs = Vec::new();
                if start == 0 && !skip_cw {
                    hubs.push(self.continue_hub(count, None));
                }
                // Home rows come from the same numbered list `/hubs/nuvio/{i}` pages, filtered to
                // the Home-eligible catalogs: a row's index must name the catalog it previewed.
                let home = self.catalog_list();
                let browse = self.all_catalogs();
                let lead = usize::from(!skip_cw);
                let (rows, total) = self.catalog_hubs(
                    &browse,
                    |c| home.iter().any(|h| h.key == c.key),
                    count,
                    true,
                    HUBS_BUDGET,
                    Some((start.saturating_sub(lead), size.saturating_sub(hubs.len()))),
                );
                hubs.extend(rows);
                Response::json(&container(json!({
                    "Hub": hubs, "offset": start, "totalSize": total + lead,
                })))
            }
            (_, ["hubs", "continueWatching"]) => {
                let count = req
                    .q("count")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(12usize);
                Response::json(&container(json!({"Hub": [self.continue_hub(count, None)]})))
            }
            (_, ["hubs", "continueWatching", "items"]) => {
                let (start, size) = page_params(req);
                self.continue_watching_items(start, size.unwrap_or(12))
            }
            (_, ["hubs", "home", "recentlyAdded"]) => {
                let section = if req.q("type") == Some("2") || req.q("type") == Some("4") {
                    SECTION_SHOWS
                } else {
                    SECTION_MOVIES
                };
                let (start, size) = page_params(req);
                self.catalog_listing(
                    self.first_catalog(section),
                    None,
                    None,
                    start,
                    size.unwrap_or(12),
                )
            }
            (_, ["hubs", "sections", k]) => {
                let section: i64 = k.parse().unwrap_or(0);
                let count = req
                    .q("count")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(12usize)
                    .clamp(1, 50);
                // Pages BY HUB (docs/pms-api.md): a window here is a window of rows, and ignoring
                // it made every window re-fetch every catalog — a minute on a Recommended page.
                let (start, size) = page_params(req);
                let size = size.unwrap_or(usize::MAX).max(1);
                let mut hubs = Vec::new();
                if start == 0 {
                    hubs.push(self.continue_hub(count, Some(section)));
                }
                // Recommended rows use the full browse list (the Library selector already shows
                // them under catalogKey) — not home_catalogs(), which hides showInHome:false.
                let browse_list = self.all_catalogs();
                let (rows, total) = self.catalog_hubs(
                    &browse_list,
                    |c| (section == SECTION_SHOWS) == is_series(&c.type_),
                    count,
                    false,
                    HUBS_BUDGET,
                    Some((start.saturating_sub(1), size.saturating_sub(hubs.len()))),
                );
                hubs.extend(rows);
                Response::json(&container(json!({
                    "Hub": hubs, "offset": start, "totalSize": total + 1,
                })))
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
                    .and_then(|i| self.all_catalogs().into_iter().nth(i));
                self.catalog_listing(cat, None, None, start, size.unwrap_or(50))
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
        self: &Arc<Self>,
        cat: Option<CatalogInfo>,
        genre: Option<&str>,
        sort: Option<(CatalogSort, bool)>,
        start: usize,
        size: usize,
    ) -> Response {
        let Some(cat) = cat else {
            return Response::json(&container(
                json!({"Metadata": [], "totalSize": 0, "offset": start}),
            ));
        };
        // The Filter menu offers the catalog's own genres plus TMDB's; a value the addon does not
        // declare is filtered in memory from the fetched rows, so a TMDB baseline genre works even
        // when the catalog has no `genre` extra at all.
        let all = merged_genres(&cat.genres, is_series(&cat.type_));
        let wanted = genre
            .map(|g| resolve_genre(&all, g))
            .filter(|g| !g.is_empty());
        // A declared genre is forwarded under the addon's OWN spelling; anything else (the TMDB
        // baseline included) is matched against the fetched rows here.
        let native = wanted.and_then(|g| cat.genres.iter().find(|have| genre_matches(have, g)));
        let (fetch_genre, match_genre) = match native {
            Some(name) => (Some(name.as_str()), None),
            None => (None, wanted),
        };
        let (rows, total) = self.catalog_range(
            &cat.key,
            fetch_genre,
            match_genre,
            sort,
            start,
            size.min(200),
            self.cfg.catalog_wait,
        );
        let rows: Vec<Value> = rows.iter().map(|p| self.preview_row(p)).collect();
        Response::json(&container(
            json!({"Metadata": rows, "totalSize": total, "offset": start}),
        ))
    }

    fn section(self: &Arc<Self>, req: &Request, k: i64, rest: &[&str]) -> Response {
        if k != SECTION_MOVIES && k != SECTION_SHOWS {
            return Response::json_status(404, &container(json!({})));
        }
        match rest {
            ["all"] | ["recentlyAdded"] | ["newest"] => {
                let (start, size) = page_params(req);
                let genre = req.q("genre").filter(|g| !g.is_empty());
                let sort = parse_catalog_sort(req.q("sort"));
                // A request that NAMED a catalog gets that catalog: the choice IS the order, and
                // the Sort menu only reorders within it (Popularity is the catalog's own order).
                // Rewriting the catalog from the sort here is what made every page past the first
                // one revert to Popular Movies: the client sends `?catalog=<key>&sort=popularity`
                // on the pages it fetches after the landing taught it the menu.
                //
                // Without a named catalog (a listing the selector never reached), Cinemeta does
                // not honour `?sort=`; instead it exposes dedicated catalogs for each ordering
                // ("top" = Popular, "imdbRating" = Top Rated). Map the Sort dropdown to those
                // catalogs so the addon's own ranking is used, and never forward `sort` to the
                // addon URL (catalog_page does not send it). "Release Date" has no clean catalog
                // mapping without a concrete year, so it keeps the "top" catalog and is sorted in
                // memory by the panic-free comparator above.
                let named_catalog = req.q("catalog").filter(|key| !key.is_empty());
                let (cat, sort) = if named_catalog.is_some() {
                    (self.section_catalog(req, k), sort)
                } else if let Some((kind, _)) = sort {
                    let target_id = match kind {
                        CatalogSort::Popularity => Some("top"),
                        CatalogSort::Rating => Some("imdbRating"),
                        _ => None,
                    };
                    if let Some(id) = target_id {
                        if let Some(c) = self
                            .section_catalogs(k)
                            .into_iter()
                            .find(|c| c.key.split('/').last() == Some(id))
                        {
                            (Some(c), None)
                        } else {
                            (self.section_catalog(req, k), Some((kind, true)))
                        }
                    } else {
                        (self.section_catalog(req, k), sort)
                    }
                } else {
                    (self.section_catalog(req, k), sort)
                };
                // Preserve the original requested direction when we fell back to in-memory sorting.
                let sort = match (sort, parse_catalog_sort(req.q("sort"))) {
                    (Some((k, _)), Some((_, desc))) => Some((k, desc)),
                    (other, _) => other,
                };
                let mut resp =
                    self.catalog_listing(cat.clone(), genre, sort, start, size.unwrap_or(60));
                if req.q("includeMeta") == Some("1") {
                    if let Ok(mut v) = serde_json::from_slice::<Value>(&resp.body) {
                        let catalogs = self.section_catalogs(k);
                        let active = cat.as_ref().map(|c| c.key.as_str());
                        let types: Vec<Value> = if catalogs.is_empty() {
                            let kind = if k == SECTION_SHOWS { "show" } else { "movie" };
                            vec![json!({"key": format!("/library/sections/{k}/all"),
                                "type": kind, "title": kind, "active": true,
                                "Sort": sort_entries_for(None)})]
                        } else {
                            catalogs
                                .iter()
                                .map(|c| {
                                    let listing = format!(
                                        "/library/sections/{k}/all?catalog={}",
                                        pct_encode(&c.key)
                                    );
                                    json!({
                                        "key": listing,
                                        "type": if k == SECTION_SHOWS { "show" } else { "movie" },
                                        "catalogKey": c.key,
                                        "title": c.title,
                                        "active": active == Some(c.key.as_str()),
                                        "Sort": sort_entries_for(c.key.split('/').next())
                                    })
                                })
                                .collect()
                        };
                        v["MediaContainer"]["Meta"] = json!({"Type": types});
                        resp = Response::json(&v);
                    }
                }
                resp
            }
            ["genre"] => {
                // The genre list belongs to the SELECTED catalog, so the filter menu matches the
                // grid's current catalog rather than the section's first one. Its own declared
                // genres come first; the TMDB baseline fills the rest so the Filter menu is never
                // empty, even for a catalog that declares no `genre` extra.
                let cat = self.section_catalog(req, k);
                let series = cat
                    .as_ref()
                    .map_or(k == SECTION_SHOWS, |c| is_series(&c.type_));
                let genres = merged_genres(
                    cat.as_ref()
                        .map(|c| c.genres.as_slice())
                        .unwrap_or_default(),
                    series,
                );
                let dirs: Vec<Value> = genres
                    .iter()
                    .map(|g| json!({"key": g, "title": g, "fastKey": format!("/library/sections/{k}/all?genre={}", pct_encode(g))}))
                    .collect();
                Response::json(&container(json!({"Directory": dirs})))
            }
            ["sorts"] => {
                let addon_id = self
                    .section_catalog(req, k)
                    .and_then(|c| c.key.split('/').next().map(str::to_string));
                Response::json(&container(
                    json!({"Sort": sort_entries_for(addon_id.as_deref())}),
                ))
            }
            ["filters"] => Response::json(&container(json!({"Filter": [
                {"filter": "genre", "filterType": "string",
                 "key": format!("/library/sections/{k}/genre"), "title": "Genre"},
                {"filter": "unwatched", "filterType": "boolean",
                 "key": format!("/library/sections/{k}/all?unwatched=1"), "title": "Unwatched"}
            ]}))),
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
        let Some((first, _, _)) = self.item_probed(rk, Some(0)) else {
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
        // A trailer Part can be asked for before anything resolved it: the
        // shelf rows (`trailer_shelf_rows`) state their keys without paying the
        // YouTube round-trip that used to run on every detail open. Resolve
        // here — on the player's own thread — then fall through to the tables.
        if let Some((parent, index)) = trailer_part_at(pid) {
            if !self
                .trailer_dash
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(&pid)
                && !self
                    .trailer_parts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains_key(&pid)
            {
                let _ = self.trailer_row_at(parent, index);
            }
        }
        // Trailer Parts answer before any stream ranking: DASH manifests
        // 200, muxed trailers 302.
        if let Some(mpd) = self
            .trailer_dash
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
            .cloned()
        {
            return Response::bytes(200, "application/dash+xml", mpd.into_bytes());
        }
        if let Some(url) = self
            .trailer_parts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&pid)
            .cloned()
        {
            return Response::redirect(&url);
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
        // A Part id is a stream's arrival index: the player asks for the
        // Media it was given. Preferring the remembered stream here would
        // serve a different file from the one the viewer picked in the
        // picker, so it is not done on this path.
        let url = stream.as_ref().and_then(|s| {
            let u = shim_media::playable_url(s, self.torrent_server().as_deref())?;
            self.open_url(rk, s, &u)
        });
        match url {
            Some(url) => Response::redirect(&url),
            None => Response::bytes(502, "text/plain", b"stream no longer available".to_vec()),
        }
    }

    /// `GET /library/metadata/{rk}/subtitles` — the player's Subtitles → Search page. Candidates
    /// come from the builtin OpenSubtitles v3 addon (independent of the user's installed set) and
    /// from the user's own subtitle addons, are filtered to the 2-letter language the page asked
    /// for, and each is handed a `/library/streams/{id}` key with its URL registered — so the
    /// install that follows names exactly what the search showed.
    fn subtitle_search(&self, rk: &str, req: &Request) -> Response {
        let Some(key) = rk.parse::<i64>().ok() else {
            return ok_empty();
        };
        let Some(ident) = self.keys.ident(key).or_else(|| synthetic_ident(key)) else {
            return ok_empty();
        };
        let Some((type_, id)) = self.stream_target(&ident) else {
            return ok_empty();
        };
        let addon_id = translate_to_stremio_id(key).unwrap_or(id);
        let id = self.addon_id_for(&type_, &addon_id);
        // The builtin provider AND the viewer's own subtitle addons — the same set that feeds the
        // item's "Other languages" rows — answer CONCURRENTLY: a language the page is asked for
        // may only be in one of the two, and neither read may push the other past the request's
        // budget.
        let (own, builtin) = std::thread::scope(|scope| {
            let own = scope.spawn(|| self.backend.subtitles(&type_, &id));
            (own.join().unwrap_or_default(), self.backend.search_subtitles(&type_, &id))
        });
        if builtin.is_none() && own.is_empty() {
            // A fault, not an empty answer: the page draws a retry for this one only.
            return Response::json_status(
                502,
                &container(json!({"Stream": [], "error": "subtitle search failed"})),
            );
        }
        let want = req.q("language").unwrap_or("").trim().to_ascii_lowercase();
        let mut hits = builtin.unwrap_or_default();
        let mut seen: HashSet<String> = hits.iter().map(|s| s.url.clone()).collect();
        for sub in own {
            if seen.insert(sub.url.clone()) {
                hits.push(sub);
            }
        }
        let total = hits.len() as i64;
        let mut streams = Vec::new();
        for (i, sub) in hits.into_iter().enumerate() {
            if !want.is_empty() && shim_media::lang_key(&sub.lang) != want {
                continue;
            }
            if streams.len() >= SUBTITLE_SEARCH_MAX {
                break;
            }
            let sid = self.next_sub.fetch_add(1, Ordering::Relaxed);
            let title = sub.release_title();
            let provider = sub.provider.clone().unwrap_or_else(|| "Subtitles".into());
            self.sub_urls
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(sid, sub.url);
            let mut known = self.search_subs.lock().unwrap_or_else(|e| e.into_inner());
            if known.len() > 4096 {
                known.clear();
            }
            known.insert(sid, SearchSub { id: sid, lang: sub.lang.clone(), title: title.clone() });
            drop(known);
            // `score` is a STRING on the wire (`"2301"`), the field's type is lenient; the value
            // only has to be descending so `hits_of`'s stable sort keeps the addon's own order.
            streams.push(json!({
                "id": sid,
                "key": format!("/library/streams/{sid}.srt"),
                "streamType": 3, "codec": "srt", "format": "srt",
                "language": sub.lang.as_str(), "languageCode": sub.lang.as_str(),
                "languageTag": sub.lang.as_str(),
                "providerTitle": provider.as_str(),
                "title": title.as_str(), "displayTitle": title.as_str(),
                "extendedDisplayTitle": title.as_str(),
                "score": (total - i as i64).to_string(),
                "hearingImpaired": 0, "forced": 0,
            }));
        }
        Response::json(&container(json!({"size": streams.len(), "Stream": streams})))
    }

    /// `PUT /library/metadata/{rk}/subtitles?key=/library/streams/{id}.srt` — make a searched
    /// candidate a sidecar of the item. The answer is 200 with an empty body: the created stream
    /// is not named, and the app finds it by re-reading the item (`docs/pms-api.md` §8). An
    /// unknown id is accepted and does nothing, the way a stale candidate key behaves on a real
    /// PMS.
    fn subtitle_install(&self, rk: &str, req: &Request) -> Response {
        let (Some(key), Some(candidate)) = (rk.parse::<i64>().ok(), req.q("key")) else {
            return ok_empty();
        };
        let Some(sid) = candidate
            .strip_prefix("/library/streams/")
            .and_then(|tail| tail.split('.').next())
            .and_then(|n| n.parse::<i64>().ok())
        else {
            return ok_empty();
        };
        let known = self
            .search_subs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&sid)
            .cloned();
        let Some(sub) = known else { return ok_empty() };
        let mut installed = self.installed_subs.lock().unwrap_or_else(|e| e.into_inner());
        if installed.len() > INSTALLED_SUB_ITEMS {
            installed.clear();
        }
        let list = installed.entry(key).or_default();
        if !list.iter().any(|s| s.id == sub.id) {
            if list.len() >= INSTALLED_SUBS_PER_ITEM {
                list.remove(0);
            }
            list.push(sub);
        }
        drop(installed);
        ok_empty()
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
                let row = rk
                    .parse()
                    .ok()
                    .and_then(|rk| self.row(rk).or_else(|| self.tmdb_synthetic_row(rk)));
                let Some(row) = row else {
                    // The metadata row is not ready yet (or its TMDB rebuild answered nothing).
                    // That is a WAIT, not a missing asset: a 404 here latches in the poster store
                    // as final, so the tile stays blank even after the row lands. 5xx is the
                    // store's retryable class; a row that EXISTS with no art still falls through
                    // to the 404 below.
                    return Response::bytes(503, "text/plain", Vec::new());
                };
                row[*kind]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
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
                    // MetaHub's ladder is `small` 300×450, `medium` 500×750, `large` 780×1170.
                    // A card draws 250×375 (`widgets::POSTER_RES`), so `medium` is a 1.5 MB RGBA
                    // upload — over `tex::RESIDENCY_BYTES` (1 MiB), which makes every poster a
                    // solo `Residency` take and owns the frame. `small` is 540 KB and lands as an
                    // ordinary `Poster` take; asking for `medium` here measured as fps 44–54 with
                    // `budget …/solo:residency` during a fast Home scroll (2026-11-02).
                    let to = match (kind, width) {
                        ("poster", w) if w > 0 && w <= 300 => "small",
                        ("poster", w) if w > 0 && w <= 500 => "medium",
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
mod meta_flight_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Barrier;

    /// Counts the addon reads for one title and answers them slowly, so two callers released
    /// together really are in flight at once.
    struct SlowMetaBackend {
        calls: AtomicUsize,
    }

    impl Backend for SlowMetaBackend {
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
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(150));
            Some(Meta {
                id: id.into(),
                type_: type_.into(),
                name: "Slow Meta".into(),
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

    /// The focus prefetch and the click it was warming ask for the same title at the same time;
    /// the addon must see one read, and both callers must get the answered meta.
    #[test]
    fn a_prefetch_and_a_detail_read_share_one_addon_meta_fetch() {
        let backend = Arc::new(SlowMetaBackend {
            calls: AtomicUsize::new(0),
        });
        let shim = Arc::new(Shim::new(backend.clone(), Config::default()));
        let started = Barrier::new(2);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let (shim, started) = (Arc::clone(&shim), &started);
                scope.spawn(move || {
                    started.wait();
                    let meta = shim.meta("movie", "slow-meta").expect("the addon answered");
                    assert_eq!(meta.name, "Slow Meta");
                });
            }
        });
        assert_eq!(
            backend.calls.load(Ordering::SeqCst),
            1,
            "the second caller reused the fetch in flight"
        );
    }
}

#[cfg(test)]
mod enrich_cache_tests {
    use super::*;

    /// Answers no metas: the test seeds the caches directly, so it exercises only the enrichment
    /// decision (a cache hit must not depend on the addon or on TMDB).
    struct NoBackend;

    impl Backend for NoBackend {
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

    /// A cached enrichment is the whole answer. The test binary has no TMDB key and the meta has
    /// no trailer, so nothing on the fresh path could produce these two fields: if the cache were
    /// not consulted first, the row would keep neither.
    #[test]
    fn a_cached_enrichment_is_applied_without_asking_tmdb() {
        let shim = Arc::new(Shim::new(Arc::new(NoBackend), Config::default()));
        let rk = shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: "tt1".into(),
        });
        shim.metas.insert(
            "movie",
            "tt1",
            Arc::new(Meta {
                id: "tt1".into(),
                type_: "movie".into(),
                name: "Title".into(),
                ..Default::default()
            }),
        );
        shim.enrich.insert(
            "movie",
            "tt1",
            Enrichment {
                roles: Some(json!([{"name": "Cached Actor"}])),
                extras: Some(json!({"Video": [{"title": "Cached Trailer"}]})),
                related: Some(RelatedSource::Movies(Vec::new())),
                ..Default::default()
            },
        );
        let mut row = json!({"ratingKey": rk});
        shim.enrich_row(rk, &mut row);
        assert_eq!(row["Role"][0]["name"], "Cached Actor");
        assert_eq!(row["Extras"]["Video"][0]["title"], "Cached Trailer");
    }

    /// A row written before the related shelf was kept is treated as incomplete: applying it and
    /// returning would leave that title with a live TMDB call on every shelf for a month.
    #[test]
    fn an_enrichment_without_the_shelf_is_refreshed() {
        let shim = Arc::new(Shim::new(Arc::new(NoBackend), Config::default()));
        let rk = shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: "tt3".into(),
        });
        shim.metas.insert(
            "movie",
            "tt3",
            Arc::new(Meta {
                id: "tt3".into(),
                type_: "movie".into(),
                name: "Title".into(),
                ..Default::default()
            }),
        );
        shim.enrich.insert("movie", "tt3", Enrichment::default());
        let mut row = json!({"ratingKey": rk});
        shim.enrich_row(rk, &mut row);
        assert!(
            row.get("Role").is_none(),
            "the old-shape entry was not taken as the answer"
        );
    }

    /// The `More Like This` shelf comes from the same persisted entry: a fresh fetch would need
    /// the TMDB key this binary does not have, so a 200 with the row proves the cache answered.
    #[test]
    fn a_cached_enrichment_answers_the_related_shelf_without_tmdb() {
        let shim = Arc::new(Shim::new(Arc::new(NoBackend), Config::default()));
        let rk = shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: "tt2".into(),
        });
        shim.enrich.insert(
            "movie",
            "tt2",
            Enrichment {
                tmdb_id: 999,
                related: Some(RelatedSource::Movies(vec![TmdbMovie {
                    id: 603,
                    title: "Recommended".into(),
                    ..Default::default()
                }])),
                ..Default::default()
            },
        );
        let response = shim.related_hub(rk);
        assert_eq!(response.status, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        let hub = &body["MediaContainer"]["Hub"][0];
        assert_eq!(hub["title"], "More Like This");
        assert_eq!(hub["Metadata"][0]["title"], "Recommended");
    }
}

#[cfg(test)]
mod playback_regressions {
    use super::*;

    /// A movie whose stream addon answers late: the detail read must not wait for it.
    struct SlowStreamBackend;

    impl Backend for SlowStreamBackend {
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
            (type_ == "movie" && id == "slow-movie").then(|| Meta {
                id: id.into(),
                type_: type_.into(),
                name: "Slow Movie".into(),
                runtime: Some("2 h".into()),
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
            std::thread::sleep(Duration::from_millis(400));
            vec![Stream {
                url: Some("https://example.invalid/slow.mkv".into()),
                name: Some("Slow\n1080p".into()),
                addon_name: "Slow".into(),
                addon_id: "slow".into(),
                ..Default::default()
            }]
        }
        fn subtitles(&self, _type_: &str, _id: &str) -> Vec<plx_stremio::model::Subtitle> {
            Vec::new()
        }
    }

    #[test]
    fn a_detail_read_does_not_wait_for_a_slow_stream_addon() {
        let shim = Arc::new(Shim::new(Arc::new(SlowStreamBackend), Config::default()));
        let rk = shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: "slow-movie".into(),
        });
        let get = || Request {
            method: "GET".into(),
            path: format!("/library/metadata/{rk}"),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: Vec::new(),
        };
        let started = Instant::now();
        let response = shim.handle(&get());
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "a detail read waited {elapsed:?} for streams"
        );
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        let media = body["MediaContainer"]["Metadata"][0]["Media"]
            .as_array()
            .unwrap();
        assert_eq!(media.len(), 1, "the placeholder Part stands in");
        assert_eq!(media[0]["title"], "Finding streams…");
        assert_eq!(media[0]["addon_name"], "", "the placeholder names no addon");

        // The slot's background fetch keeps running; once it lands, the same read has the real
        // Media instead of the placeholder.
        for _ in 0..100 {
            let body: Value = serde_json::from_slice(&shim.handle(&get()).body).unwrap();
            let media = body["MediaContainer"]["Metadata"][0]["Media"]
                .as_array()
                .unwrap();
            if media[0]["addon_name"] == "Slow" {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the background stream fetch never reached the detail row");
    }

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
        let padded = encode_rgba(40, 20, |x, y| {
            ((10..30).contains(&x) && (5..15).contains(&y)).then_some([255, 255, 255, 255])
        });
        let cropped = logo_png("424242", &url, (600, 240), padded.clone());
        assert_ne!(
            cropped, padded,
            "the transparent border was served as it came"
        );
        let drawn = image::load_from_memory(&cropped).unwrap().to_rgba8();
        assert_eq!(
            drawn.dimensions(),
            (20, 10),
            "the crop is not the ink's bounding box"
        );
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
        assert!(
            super::crop_ink(&opaque).is_none(),
            "an opaque image has no border"
        );
        let full = encode_rgba(12, 12, |_, _| Some([255, 0, 0, 255]));
        assert!(
            super::crop_ink(&full).is_none(),
            "ink already filling the canvas was re-encoded"
        );
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
        let padded = encode_rgba(64, 32, |x, y| {
            ((16..48).contains(&x) && (8..24).contains(&y)).then_some([255, 255, 255, 255])
        });
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let served = padded.clone();
        let worker = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
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
                (
                    "url".into(),
                    format!("http://{addr}/library/metadata/4242/{kind}"),
                ),
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
        assert_eq!(
            image::load_from_memory(&logo.body)
                .unwrap()
                .to_rgba8()
                .dimensions(),
            (32, 16)
        );
        let poster = shim.photo(&art("thumb"));
        assert_eq!(
            poster.body, padded,
            "an ordinary poster must be served as it came"
        );
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
        assert!(!watch_push_due(
            Some(now),
            false,
            now + Duration::from_secs(59)
        ));
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

    /// Nuvio's account names a title by its IMDb id, so an item that already has one must be
    /// pushed as it is — never rewritten to `tmdb:` — and an episode must carry its season and
    /// episode beside the series id.
    #[test]
    fn a_movie_and_an_episode_are_named_by_imdb() {
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let movie = shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: "tt0133093".into(),
        });
        let i = shim.nuvio_watch_identity(movie).expect("a movie is a resume point");
        assert_eq!(i.content_id, "tt0133093");
        assert_eq!(i.content_type, "movie");
        assert_eq!(i.video_id, "tt0133093");
        assert_eq!((i.season, i.episode), (None, None));

        let episode = shim.keys.key_of(&Ident::Episode {
            series: "tt0944947".into(),
            video: "tt0944947:1:2".into(),
        });
        let i = shim.nuvio_watch_identity(episode).expect("an episode is a resume point");
        assert_eq!(i.content_id, "tt0944947");
        assert_eq!(i.content_type, "series");
        assert_eq!(i.video_id, "tt0944947:1:2");
        assert_eq!((i.season, i.episode), (Some(1), Some(2)));

        // A season names no single position; neither does a whole series.
        let season = shim
            .keys
            .key_of(&Ident::Season { series: "tt0944947".into(), n: 1 });
        assert!(shim.nuvio_watch_identity(season).is_none());
    }

    #[test]
    fn episode_numbers_reads_only_a_season_episode_tail() {
        assert_eq!(episode_numbers("tt0944947:1:2"), (Some(1), Some(2)));
        assert_eq!(episode_numbers("tmdb:1396:0:5"), (Some(0), Some(5)));
        assert_eq!(episode_numbers("tt0944947"), (None, None));
        assert_eq!(episode_numbers("tmdb:1396"), (None, None));
        assert_eq!(episode_numbers("tt0944947:1"), (None, None));
    }

    fn classified(name: &str, url: &str, hash: Option<&str>, label: &str) -> Classified {
        let mut stream = Stream {
            name: Some(name.to_string()),
            url: Some(url.to_string()),
            info_hash: hash.map(Into::into),
            ..Default::default()
        };
        stream.addon_name = "Torrentio".into();
        Classified {
            url: url.to_string(),
            stream,
            info: plx_stremio::classify::StreamInfo {
                label: label.to_string(),
                filename: Some(format!("{name}.mkv")),
                size_bytes: Some(10_000),
                height: Some(1080),
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_remembered_torrent_promotes_even_when_ranking_demotes_it() {
        let pinned = classified("pinned", "https://x/pinned.mkv", None, "pinned");
        let best = classified("best", "https://x/best.mkv", None, "best");
        let last = Shim::last_stream_of(&pinned);
        let mut out = vec![(0usize, best), (1usize, pinned)];
        Shim::promote_last_stream(&mut out, Some(&last), false);
        assert_eq!(out[0].1.info.label, "pinned");
    }

    #[test]
    fn a_missing_source_and_an_explicit_pick_steer_randomly_differently() {
        let best = classified("best", "https://x/best.mkv", None, "best");
        let other = classified("other", "https://x/other.mkv", None, "other");
        let last = LastStream {
            addon: "Torrentio".into(),
            filename: Some("gone.mkv".into()),
            size_bytes: Some(10_000),
            height: Some(1080),
            label: "gone".into(),
            ..Default::default()
        };
        let mut out = vec![(0usize, best), (1usize, other)];
        Shim::promote_last_stream(&mut out, Some(&last), false);
        assert_eq!(out[0].1.info.label, "best", "nothing promoted, ranking stands");

        let best = classified("best", "https://x/best.mkv", None, "best");
        let pinned = classified("pinned", "https://x/pinned.mkv", None, "pinned");
        let last = Shim::last_stream_of(&pinned);
        let mut out = vec![(0usize, best), (1usize, pinned)];
        Shim::promote_last_stream(&mut out, Some(&last), true);
        assert_eq!(out[0].1.info.label, "best", "an explicit pick beats the memory");
    }

    /// A row Nuvio's own app pushed with a synthetic episode video id is placed through the
    /// series meta's season/episode, so it lands on the addon's real episode (and has a row
    /// snapshot Continue Watching can draw); an episode the series does not have is skipped.
    #[test]
    fn a_synthetic_episode_row_is_placed_through_season_and_episode() {
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let series = crate::shim_fake::FAKE_SERIES_ID;
        let found = shim
            .account_watch_key(&plx_stremio::nuvio::WatchProgress {
                content_id: series.into(),
                content_type: "series".into(),
                video_id: format!("{NUVIO_SYNTHETIC_EPISODE}1:2"),
                season: Some(1),
                episode: Some(2),
                position: 1000,
                duration: 2000,
                last_watched: 3000,
            })
            .expect("season 1 episode 2 exists");
        assert_eq!(
            shim.keys.ident(found),
            Some(Ident::Episode {
                series: series.into(),
                video: format!("{series}:1:2"),
            })
        );
        assert!(shim
            .account_watch_key(&plx_stremio::nuvio::WatchProgress {
                content_id: series.into(),
                content_type: "series".into(),
                video_id: format!("{NUVIO_SYNTHETIC_EPISODE}9:9"),
                season: Some(9),
                episode: Some(9),
                position: 1000,
                duration: 2000,
                last_watched: 3000,
            })
            .is_none());
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
        let hue =
            (((h >> (i * 13)) as f32 / (1u64 << 13) as f32) * 360.0 + (i as f32 * 97.0)) % 360.0;
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

#[cfg(test)]
mod catalog_selector_tests {
    use super::*;

    fn meta(name: &str, rating: Option<&str>, year: Option<&str>, genres: &[&str]) -> MetaPreview {
        MetaPreview {
            id: format!("tt-{name}"),
            name: name.into(),
            imdb_rating: rating.map(str::to_string),
            release_info: year.map(str::to_string),
            genres: genres.iter().map(|g| g.to_string()).collect(),
            ..Default::default()
        }
    }

    struct GenreBackend;

    impl Backend for GenreBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> {
            vec![CatalogInfo {
                key: "genre/movie/all".into(),
                title: "All".into(),
                type_: "movie".into(),
                genres: Vec::new(),
            }]
        }
        fn catalog_page(
            &self,
            _key: &str,
            _skip: u32,
            _genre: Option<&str>,
        ) -> Result<Vec<MetaPreview>, ()> {
            Ok(vec![meta(
                "Alpha",
                Some("8.0"),
                Some("2020-01-02"),
                &["Science Fiction"],
            )])
        }
        fn search(&self, _query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
            Vec::new()
        }
        fn meta(&self, _type_: &str, _id: &str) -> Option<Meta> {
            None
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

    fn get(path: &str, query: &[(&str, &str)]) -> Request {
        Request {
            method: "GET".into(),
            path: path.into(),
            query: query
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            headers: HashMap::new(),
            body: Vec::new(),
        }
    }

    /// Two movie catalogs, one of them Cinemeta's `top`, so a request that names a catalog can be
    /// told apart from the sort menu's old "popularity means top" rewrite.
    struct TwoCatalogBackend;

    impl Backend for TwoCatalogBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> {
            vec![
                CatalogInfo {
                    key: "addon/movie/top".into(),
                    title: "Popular Movies".into(),
                    type_: "movie".into(),
                    genres: Vec::new(),
                },
                CatalogInfo {
                    key: "addon/movie/nfx".into(),
                    title: "Netflix Movies".into(),
                    type_: "movie".into(),
                    genres: Vec::new(),
                },
            ]
        }
        fn catalog_page(
            &self,
            key: &str,
            skip: u32,
            _genre: Option<&str>,
        ) -> Result<Vec<MetaPreview>, ()> {
            if skip > 0 {
                return Ok(Vec::new());
            }
            match key {
                "addon/movie/top" => Ok(vec![meta(
                    "Popular One",
                    Some("8.0"),
                    Some("2020-01-02"),
                    &[],
                )]),
                "addon/movie/nfx" => Ok(vec![meta(
                    "Netflix One",
                    Some("7.0"),
                    Some("2021-01-02"),
                    &[],
                )]),
                _ => Err(()),
            }
        }
        fn search(&self, _query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
            Vec::new()
        }
        fn meta(&self, _type_: &str, _id: &str) -> Option<Meta> {
            None
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

    fn first_title(response: &Response) -> String {
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        body["MediaContainer"]["Metadata"][0]["title"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    /// The app fetches the first page without a sort, then sends `sort=popularity` on every page
    /// after it learned the menu. That sort must reorder *within* the chosen catalog, never rewrite
    /// the catalog: doing so made every page past the first revert to Popular Movies.
    #[test]
    fn a_named_catalog_survives_the_sort_the_client_sends_on_later_pages() {
        let shim = Arc::new(Shim::new(Arc::new(TwoCatalogBackend), Config::default()));
        let first = shim.handle(&get(
            "/library/sections/1/all",
            &[("catalog", "addon/movie/nfx")],
        ));
        assert_eq!(first_title(&first), "Netflix One");
        for sort in [
            "popularity:desc",
            "popularity:asc",
            "rating:desc",
            "titleSort",
        ] {
            let later = shim.handle(&get(
                "/library/sections/1/all",
                &[("catalog", "addon/movie/nfx"), ("sort", sort)],
            ));
            assert_eq!(
                first_title(&later),
                "Netflix One",
                "sort {sort:?} replaced the chosen catalog"
            );
        }
    }

    /// A listing that names no catalog keeps the old behavior: Cinemeta exposes orders as catalogs,
    /// so "Popularity" means its `top` one.
    #[test]
    fn without_a_named_catalog_a_popularity_sort_still_means_the_top_catalog() {
        let shim = Arc::new(Shim::new(Arc::new(TwoCatalogBackend), Config::default()));
        let response = shim.handle(&get(
            "/library/sections/1/all",
            &[("sort", "popularity:desc")],
        ));
        assert_eq!(first_title(&response), "Popular One");
    }

    #[test]
    fn parses_only_the_sorts_the_shim_advertises() {
        assert_eq!(
            parse_catalog_sort(Some("titleSort")),
            Some((CatalogSort::Title, false))
        );
        assert_eq!(
            parse_catalog_sort(Some("rating:desc")),
            Some((CatalogSort::Rating, true))
        );
        assert_eq!(
            parse_catalog_sort(Some("releaseDate:desc")),
            Some((CatalogSort::ReleaseDate, true))
        );
        assert_eq!(
            parse_catalog_sort(Some("year:asc")),
            Some((CatalogSort::ReleaseDate, false))
        );
        assert_eq!(
            parse_catalog_sort(Some("popularity:desc")),
            Some((CatalogSort::Popularity, true))
        );
        assert_eq!(parse_catalog_sort(Some("addedAt:desc")), None);
        assert_eq!(parse_catalog_sort(None), None);
    }

    #[test]
    fn compares_catalogs_by_the_advertised_keys() {
        let a = meta("Alpha", Some("7.1"), Some("2001-01-01"), &["Action"]);
        let b = meta("beta", Some("8.4"), Some("2020-01-01"), &["Drama"]);
        assert_eq!(
            compare_catalog(&a, &b, CatalogSort::Title),
            CmpOrdering::Less
        );
        assert_eq!(
            compare_catalog(&a, &b, CatalogSort::Rating),
            CmpOrdering::Less
        );
        assert_eq!(
            compare_catalog(&a, &b, CatalogSort::ReleaseDate),
            CmpOrdering::Less
        );
        assert_eq!(
            compare_catalog(&b, &a, CatalogSort::Rating),
            CmpOrdering::Greater
        );
        // Popularity is the catalog's own order: never a re-sort.
        assert_eq!(
            compare_catalog(&a, &b, CatalogSort::Popularity),
            CmpOrdering::Equal
        );
        // Missing scores/dates sort last rather than pretending to be zero.
        let missing = meta("Gamma", None, None, &[]);
        assert_eq!(
            compare_catalog(&a, &missing, CatalogSort::Rating),
            CmpOrdering::Less
        );
        assert_eq!(
            compare_catalog(&missing, &a, CatalogSort::ReleaseDate),
            CmpOrdering::Greater
        );
    }

    #[test]
    fn genre_matching_folds_addon_and_tmdb_spellings() {
        assert!(genre_matches("Sci-Fi", "Science Fiction"));
        assert!(genre_matches("Science Fiction", "Sci-Fi"));
        assert!(genre_matches("Action", "Action & Adventure"));
        assert!(genre_matches("Fantasy", "Sci-Fi & Fantasy"));
        assert!(!genre_matches("Comedy", "Action"));
        assert!(!genre_matches("", "Action"));
    }

    #[test]
    fn the_genre_menu_merges_the_tmdb_baseline_without_duplicates() {
        let merged = merged_genres(&["Sci-Fi".into(), "Action".into()], false);
        assert_eq!(merged[0], "Sci-Fi");
        assert_eq!(
            merged
                .iter()
                .filter(|g| genre_key(g) == genre_key("Science Fiction"))
                .count(),
            1
        );
        assert!(merged.iter().any(|g| g == "Drama"));
        let series = merged_genres(&[], true);
        assert!(series.iter().any(|g| g == "Sci-Fi & Fantasy"));
        let id = tag_id("Drama").to_string();
        assert_eq!(resolve_genre(&merged, &id), "Drama");
    }

    #[test]
    fn a_tmdb_content_id_is_split_from_its_episode_tail() {
        assert_eq!(split_content_id("tmdb:1396:1:2"), ("tmdb:1396", ":1:2"));
        assert_eq!(split_content_id("tt0944947:1:2"), ("tt0944947", ":1:2"));
        assert_eq!(split_content_id("tmdb:603"), ("tmdb:603", ""));
        assert_eq!(tmdb_content_number("tmdb:603"), Some(603));
        assert_eq!(tmdb_content_number("tt603"), None);
    }

    /// The reported crash: a genre with spaces/`&`/`%`/non-ASCII (or a numeric tag id) must answer
    /// a page, never take the shim down. This exercises the same handler the app calls.
    #[test]
    fn a_genre_with_spaces_and_specials_answers_a_page() {
        let shim = Arc::new(Shim::new(Arc::new(GenreBackend), Config::default()));
        for genre in [
            "Sci-Fi & Fantasy",
            "100%",
            "Café",
            &tag_id("Science Fiction").to_string(),
            "Science Fiction",
        ] {
            let response = shim.handle(&get(
                "/library/sections/1/all",
                &[("genre", genre), ("catalog", "genre/movie/all")],
            ));
            assert_eq!(response.status, 200, "genre {genre:?} crashed the shim");
            let body: Value = serde_json::from_slice(&response.body).unwrap();
            assert!(
                body["MediaContainer"]["Metadata"].is_array(),
                "genre {genre:?} answered no listing"
            );
        }
        // A TMDB baseline genre and its numeric tag id both reach the one matching row.
        for genre in ["Science Fiction", &tag_id("Science Fiction").to_string()] {
            let response = shim.handle(&get(
                "/library/sections/1/all",
                &[("genre", genre), ("catalog", "genre/movie/all")],
            ));
            let body: Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(
                body["MediaContainer"]["Metadata"][0]["title"], "Alpha",
                "{genre:?}"
            );
        }
    }

    #[test]
    fn the_genre_directory_offers_the_tmdb_baseline() {
        let shim = Arc::new(Shim::new(Arc::new(GenreBackend), Config::default()));
        let response = shim.handle(&get(
            "/library/sections/1/genre",
            &[("catalog", "genre/movie/all")],
        ));
        assert_eq!(response.status, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        let names: Vec<&str> = body["MediaContainer"]["Directory"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["title"].as_str())
            .collect();
        assert!(names.contains(&"Science Fiction"), "{names:?}");
        assert!(names.contains(&"Drama"), "{names:?}");
    }

    #[test]
    fn an_addon_id_is_only_rewritten_when_it_is_a_tmdb_content_id() {
        // No TMDB key in the unit environment: the id is passed through, never dropped.
        let shim = Arc::new(Shim::new(Arc::new(GenreBackend), Config::default()));
        assert_eq!(shim.addon_id_for("movie", "tt0133093"), "tt0133093");
        assert_eq!(shim.addon_id_for("movie", "kitsu:42"), "kitsu:42");
        // With a TMDB key the same id becomes an IMDb id; the episode tail survives either way.
        let episode = shim.addon_id_for("series", "tmdb:1396:1:2");
        assert!(
            episode == "tmdb:1396:1:2" || (episode.starts_with("tt") && episode.ends_with(":1:2")),
            "{episode}"
        );
    }

    /// The failure this fix is about: a catalog whose first page is cached (Home warmed it) and
    /// whose next page hangs. The section listing must answer with the cached page inside its
    /// budget — never hold the HTTP response for the addon's 30 s — and the background fill must
    /// land the rest, move `catalog_generation`, and make a re-read complete.
    struct SlowPageBackend {
        released: Mutex<bool>,
        cv: Condvar,
        second_page_reads: std::sync::atomic::AtomicUsize,
    }

    impl SlowPageBackend {
        fn new() -> Self {
            Self {
                released: Mutex::new(false),
                cv: Condvar::new(),
                second_page_reads: std::sync::atomic::AtomicUsize::new(0),
            }
        }
        fn release(&self) {
            *self.released.lock().unwrap_or_else(|e| e.into_inner()) = true;
            self.cv.notify_all();
        }
    }

    impl Backend for SlowPageBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> {
            vec![CatalogInfo {
                key: "slow/movie/all".into(),
                title: "Slow Movies".into(),
                type_: "movie".into(),
                genres: Vec::new(),
            }]
        }
        fn catalog_page(
            &self,
            key: &str,
            skip: u32,
            _genre: Option<&str>,
        ) -> Result<Vec<MetaPreview>, ()> {
            if key != "slow/movie/all" {
                return Err(());
            }
            if skip == 0 {
                return Ok((0..12).map(|i| meta(&format!("Warm {i}"), Some("7.0"), Some("2020-01-01"), &[])).collect());
            }
            self.second_page_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let mut released = self.released.lock().unwrap_or_else(|e| e.into_inner());
            // Bounded so a shim that blocks on this page fails the timing assertion instead of
            // hanging the suite.
            let (guard, _) = self
                .cv
                .wait_timeout(released, Duration::from_secs(2))
                .unwrap_or_else(|e| e.into_inner());
            released = guard;
            if !*released {
                return Err(());
            }
            drop(released);
            Ok((12..112).map(|i| meta(&format!("Late {i}"), Some("7.0"), Some("2020-01-01"), &[])).collect())
        }
        fn search(&self, _query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
            Vec::new()
        }
        fn meta(&self, _type_: &str, _id: &str) -> Option<Meta> {
            None
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
    fn a_hanging_catalog_page_answers_from_cache_and_the_fill_lands_later() {
        let backend = Arc::new(SlowPageBackend::new());
        let cfg = Config {
            catalog_wait: Duration::from_millis(50),
            ..Config::default()
        };
        let shim = Arc::new(Shim::new(backend.clone(), cfg));
        let before = shim.catalog_generation();
        let listing = || {
            get(
                "/library/sections/1/all",
                &[("catalog", "slow/movie/all"), ("X-Plex-Container-Size", "60")],
            )
        };

        let began = Instant::now();
        let first: Value = serde_json::from_slice(&shim.handle(&listing()).body).unwrap();
        let elapsed = began.elapsed();
        assert!(
            elapsed < Duration::from_millis(500),
            "a section listing waited {elapsed:?} on the addon's own page budget"
        );
        assert_eq!(
            first["MediaContainer"]["Metadata"].as_array().unwrap().len(),
            12,
            "the cached first page must be served as-is: {first:?}"
        );

        backend.release();
        let deadline = Instant::now() + Duration::from_secs(2);
        while shim.catalog_generation() == before && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            shim.catalog_generation() > before,
            "the background fill never reported that it landed rows"
        );
        assert!(
            backend.second_page_reads.load(std::sync::atomic::Ordering::SeqCst) >= 1,
            "the fill never asked for the second page"
        );

        let second: Value = serde_json::from_slice(&shim.handle(&listing()).body).unwrap();
        assert_eq!(
            second["MediaContainer"]["Metadata"].as_array().unwrap().len(),
            60,
            "the re-read must be answered from the filled cache: {second:?}"
        );
    }

    /// A poster asked for before its metadata row has landed is a WAIT: 5xx is the poster store's
    /// retryable class, while a 404 is final and would leave the tile blank even after the row
    /// arrived.
    #[test]
    fn a_photo_for_an_unlanded_metadata_row_is_a_retryable_wait() {
        let shim = Arc::new(Shim::new(Arc::new(GenreBackend), Config::default()));
        let response = shim.handle(&get(
            "/photo/:/transcode",
            &[("url", "/library/metadata/999999/thumb")],
        ));
        assert_eq!(
            response.status, 503,
            "an unlanded row must not latch as a missing poster"
        );
    }
}

#[cfg(test)]
mod my_list_removal_tests {
    use super::*;

    /// A removal is keyed by the account's own `(type, id)` and must not depend on the add-on
    /// metadata cache, which is in memory with a six-hour TTL. An add still needs that metadata.
    #[test]
    fn a_remove_needs_no_cached_metadata_but_an_add_does() {
        let removal =
            Shim::write_meta(None, "movie", "tt0133093", false).expect("a removal has its key");
        assert_eq!(
            (removal.id.as_str(), removal.type_.as_str()),
            ("tt0133093", "movie")
        );
        assert!(
            Shim::write_meta(None, "movie", "tt0133093", true).is_none(),
            "an add still needs metadata"
        );
        let cached = Arc::new(Meta {
            id: "tt1".into(),
            type_: "movie".into(),
            name: "Matrix".into(),
            ..Default::default()
        });
        assert_eq!(
            Shim::write_meta(Some(cached), "movie", "tt1", false)
                .expect("cached")
                .name,
            "Matrix"
        );
    }
}

#[cfg(test)]
mod continue_watching_paging_tests {
    use super::*;

    fn request(path: &str, query: &[(&str, &str)]) -> Request {
        Request {
            method: "GET".into(),
            path: path.into(),
            query: query
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            headers: HashMap::new(),
            body: Vec::new(),
        }
    }

    fn json_of(response: Response) -> Value {
        assert_eq!(response.status, 200, "the deck's listing must not refuse");
        serde_json::from_slice(&response.body).unwrap()
    }

    /// Five in-progress rows, most recent first by insertion order (same second is fine: the
    /// assertion below compares sets, not order).
    fn seeded(shim: &Arc<Shim>) -> Vec<i64> {
        (0..5)
            .map(|i| {
                let rk = shim.keys.key_of(&Ident::Title {
                    type_: "movie".into(),
                    id: format!("tt90{i}"),
                });
                shim.watch.progress(
                    rk,
                    60_000,
                    7_200_000,
                    Some(json!({
                        "ratingKey": rk.to_string(),
                        "type": "movie",
                        "title": format!("Film {i}"),
                        "librarySectionID": 1,
                    })),
                );
                rk
            })
            .collect()
    }

    /// `data/src/stores/deck.rs` reads each server's listing a window at a time and rejects a page
    /// whose echoed `offset` is not the start it asked for. The shim answered only the hub preview,
    /// so every paged read fell through to the unhandled route and the deck never read past it.
    #[test]
    fn the_deck_listing_pages_contiguously_and_states_its_total() {
        let shim = Arc::new(Shim::new(
            Arc::new(crate::shim_fake::FakeBackend),
            Config::default(),
        ));
        let mut want = seeded(&shim);

        let first = json_of(shim.handle(&request(
            "/hubs/continueWatching/items",
            &[
                ("X-Plex-Container-Start", "0"),
                ("X-Plex-Container-Size", "2"),
            ],
        )));
        assert_eq!(first["MediaContainer"]["offset"], 0);
        assert_eq!(first["MediaContainer"]["totalSize"], 5);
        assert_eq!(first["MediaContainer"]["size"], 2);

        let second = json_of(shim.handle(&request(
            "/hubs/continueWatching/items",
            &[
                ("X-Plex-Container-Start", "2"),
                ("X-Plex-Container-Size", "2"),
            ],
        )));
        assert_eq!(
            second["MediaContainer"]["offset"], 2,
            "the deck re-anchors on this echo"
        );
        assert_eq!(second["MediaContainer"]["totalSize"], 5);

        let last = json_of(shim.handle(&request(
            "/hubs/continueWatching/items",
            &[
                ("X-Plex-Container-Start", "4"),
                ("X-Plex-Container-Size", "2"),
            ],
        )));
        assert_eq!(
            last["MediaContainer"]["size"], 1,
            "the last page is what is left"
        );

        let mut seen: Vec<i64> = [first, second, last]
            .iter()
            .flat_map(|p| p["MediaContainer"]["Metadata"].as_array().unwrap().iter())
            .map(|row| row["ratingKey"].as_str().unwrap().parse().unwrap())
            .collect();
        seen.sort();
        want.sort();
        assert_eq!(seen, want, "one page each, no gap and no repeat");
    }

    /// The preview is the head of the same listing, so it must say how long the whole thing is:
    /// `data`'s `project_deck` uses `more`/`totalSize` to decide whether the deck has more to read.
    #[test]
    fn the_preview_hub_states_the_whole_listing_and_its_more_flag() {
        let shim = Arc::new(Shim::new(
            Arc::new(crate::shim_fake::FakeBackend),
            Config::default(),
        ));
        seeded(&shim);
        let body = json_of(shim.handle(&request("/hubs/continueWatching", &[("count", "2")])));
        let hub = &body["MediaContainer"]["Hub"][0];
        assert_eq!(hub["size"], 2);
        assert_eq!(hub["totalSize"], 5);
        assert_eq!(hub["more"], true);
    }
}

/// The count the app polls to learn that content arrived after its first Home read. Each of
/// these is a path that changes what `/hubs` should answer; a steady-state boot must not move it.
#[cfg(test)]
mod content_generation_tests {
    use super::*;

    fn shim() -> Arc<Shim> {
        Arc::new(Shim::new(
            Arc::new(crate::shim_fake::FakeBackend),
            Config::default(),
        ))
    }

    #[test]
    fn replacing_the_addon_set_asks_home_to_read_again() {
        let shim = shim();
        let before = shim.content_generation();
        shim.addons_changed();
        assert_eq!(shim.content_generation(), before + 1);
    }

    #[test]
    fn a_pull_that_merged_nothing_does_not_ask_home_to_read_again() {
        let shim = shim();
        let before = shim.content_generation();
        shim.account_watch_merged(0);
        assert_eq!(shim.content_generation(), before);
    }

    #[test]
    fn a_pull_that_merged_a_resume_point_asks_home_to_read_again() {
        let shim = shim();
        let before = shim.content_generation();
        shim.account_watch_merged(2);
        assert_eq!(shim.content_generation(), before + 1);
    }

    /// `/hubs` omits catalogs that miss its 4 s budget. A finished warm has those rows cached, so
    /// the client that saw the partial answer must be told to ask again.
    #[test]
    fn a_finished_warm_asks_home_to_read_again() {
        let shim = shim();
        let before = shim.content_generation();
        shim.warm();
        let deadline = Instant::now() + Duration::from_secs(5);
        while shim.content_generation() == before && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            shim.content_generation() > before,
            "a finished warm must be announced"
        );
    }
}

#[cfg(test)]
mod image_sizing_tests {
    use super::sized_image_url;

    /// The card box is 250×375 (`widgets::POSTER_RES`). MetaHub's `small` is 300×450 (540 KB
    /// decoded); its `medium` is 500×750 (1.5 MB), which crosses `tex::RESIDENCY_BYTES` and makes
    /// every poster a solo upload. Asking for medium here measured fps 44–54 during a fast Home
    /// scroll, with `budget …/solo:residency` on the television (2026-11-02).
    #[test]
    fn a_card_sized_metahub_poster_asks_for_the_small_ladder() {
        let base = "https://images.metahub.space/poster/medium/tt0111161/img";
        let at = |w| sized_image_url(base, w);
        assert_eq!(
            at(250),
            "https://images.metahub.space/poster/small/tt0111161/img"
        );
        assert_eq!(
            at(300),
            "https://images.metahub.space/poster/small/tt0111161/img"
        );
        assert_eq!(
            at(375),
            "https://images.metahub.space/poster/medium/tt0111161/img"
        );
        assert_eq!(
            at(500),
            "https://images.metahub.space/poster/medium/tt0111161/img"
        );
        assert_eq!(
            at(700),
            "https://images.metahub.space/poster/large/tt0111161/img"
        );
    }

    /// TMDB's ladder is unchanged: a 250-wide card box is still served `w342`, not the `w500` that
    /// would cross the same 1 MiB threshold.
    #[test]
    fn tmdb_posters_keep_their_ladder() {
        let url = "https://image.tmdb.org/t/p/w500/abc.jpg";
        assert_eq!(
            sized_image_url(url, 250),
            "https://image.tmdb.org/t/p/w342/abc.jpg"
        );
        assert_eq!(
            sized_image_url(url, 500),
            "https://image.tmdb.org/t/p/w500/abc.jpg"
        );
        assert_eq!(
            sized_image_url(url, 780),
            "https://image.tmdb.org/t/p/w780/abc.jpg"
        );
    }
}

#[cfg(test)]
mod resolved_url_cache_tests {
    use super::{engine_url_is_current, shim_media};

    /// Switching sources and then switching back returned the first source's cached engine URL
    /// after `sbengine::remember` had deleted its torrent: the engine answered `/stream/<id>` with
    /// 404 for the rest of the session and the player retried it forever (`ff: http_open FAILED
    /// status=404`, `playback retry: resolving item again`). A cached engine URL is only usable
    /// while its marker names the torrent the engine is serving now.
    #[test]
    fn a_cached_engine_url_is_only_reused_for_the_active_torrent() {
        let hash = "0123456789abcdef0123456789abcdef01234567";
        let marker = format!("{}{hash}", shim_media::TORRENT_SCHEME);
        assert!(engine_url_is_current(&marker, Some(hash)));
        assert!(engine_url_is_current(&marker, Some(&hash.to_uppercase())));
        assert!(
            !engine_url_is_current(&marker, Some("ffffffffffffffffffffffffffffffffffffffff")),
            "a released torrent's URL is dead, not reusable"
        );
        assert!(!engine_url_is_current(&marker, None));
        // Nothing that is not a marker has an engine stream behind it.
        assert!(engine_url_is_current("https://example.invalid/a.mkv", None));
    }
}
#[cfg(test)]
mod trailer_tests {
    use super::*;
    use crate::shim_backend::Backend;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TrailerMetaBackend {
        calls: AtomicUsize,
        trailer: String,
    }

    impl Backend for TrailerMetaBackend {
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
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some(Meta {
                id: id.into(),
                type_: type_.into(),
                name: "Trailer Host".into(),
                trailer_streams: vec![json!(format!(
                    "https://www.youtube.com/watch?v={}",
                    self.trailer
                ))],
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

    struct FixedResolver {
        resolved: Option<crate::shim_trailer::TrailerResolved>,
    }

    impl crate::shim_trailer::TrailerResolver for FixedResolver {
        fn resolve(&self, _youtube_id: &str) -> Option<crate::shim_trailer::TrailerResolved> {
            self.resolved.clone()
        }
    }

    fn muxed_resolved() -> crate::shim_trailer::TrailerResolved {
        crate::shim_trailer::TrailerResolved {
            url: "https://cdn.example/muxed.mp4?sig=x".into(),
            itag: 18,
            height: 360,
            is_dash: false,
            dash: None,
            muxed_url: Some("https://cdn.example/muxed.mp4?sig=x".into()),
            muxed_itag: 18,
            muxed_height: 360,
        }
    }

    fn dash_resolved() -> crate::shim_trailer::TrailerResolved {
        crate::shim_trailer::TrailerResolved {
            url: "https://cdn.example/video?sig=a&exp=1".into(),
            itag: 137,
            height: 1080,
            is_dash: true,
            dash: Some(crate::shim_trailer::DashInfo {
                video_url: "https://cdn.example/video?sig=a&exp=1".into(),
                audio_url: "https://cdn.example/audio?sig=c&exp=1".into(),
                video_itag: 137,
                audio_itag: 140,
                width: 1920,
                height: 1080,
                fps: 24,
                video_mime: "video/mp4; codecs=\"avc1.640028\"".into(),
                audio_mime: "audio/mp4; codecs=\"mp4a.40.2\"".into(),
                video_bitrate: 5_000_000,
                audio_bitrate: 128_000,
                video_init: (0, 999),
                video_index: (1000, 2000),
                audio_init: (2001, 2999),
                audio_index: (3000, 4000),
                duration_ms: 145_000,
            }),
            muxed_url: Some("https://cdn.example/muxed.mp4?sig=x".into()),
            muxed_itag: 18,
            muxed_height: 360,
        }
    }

    fn ok_probe(container: &str, vcodec: &str) -> crate::shim_probe::ProbeInfo {
        crate::shim_probe::ProbeInfo {
            container: container.into(),
            duration_ms: 145_000,
            size: 1_000_000,
            video_decodable: true,
            video: Some(crate::shim_probe::Video {
                codec: vcodec.into(),
                width: 1920,
                height: 1080,
                fps: 24.0,
                bit_depth: 8,
                profile: 0,
                color_trc: 0,
                color_primaries: 0,
                color_space: 0,
                dovi: None,
            }),
            audio: vec![crate::shim_probe::Audio {
                codec: "aac".into(),
                profile: 0,
                channels: 2,
                sample_rate: 0,
                language: "en".into(),
                title: String::new(),
                default: true,
                decodable: true,
            }],
            subtitles: Vec::new(),
        }
    }

    fn parent_with_trailer() -> (Arc<Shim>, i64) {
        let backend = Arc::new(TrailerMetaBackend {
            calls: AtomicUsize::new(0),
            trailer: "dQw4w9WgXcQ".into(),
        });
        let shim = Arc::new(Shim::new(backend, Config::default()));
        let parent = shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: "tt9990001".into(),
        });
        shim.metas.insert(
            "movie",
            "tt9990001",
            Arc::new(Meta {
                id: "tt9990001".into(),
                type_: "movie".into(),
                name: "Trailer Host".into(),
                trailer_streams: vec![json!("https://www.youtube.com/watch?v=dQw4w9WgXcQ")],
                ..Default::default()
            }),
        );
        (shim, parent)
    }

    #[test]
    fn trailer_key_never_decodes_as_collection() {
        let parent = 10_137i64;
        let clip = trailer_rk(parent);
        assert_eq!(clip, 5_000_010_137);
        assert_eq!(trailer_parent_rk(clip), Some(parent));
        // A collection key still decodes, a trailer key never does.
        assert_eq!(synthetic_collection_id(4_000_000_234), Some(234));
        assert_eq!(synthetic_collection_id(clip), None);
        assert_eq!(synthetic_collection_id(trailer_rk(1_000_000_603)), None);
        // Part ids are numeric and bounded.
        assert!(clip * 1000 + 999 < i64::MAX);
    }

    #[test]
    fn trailer_clip_has_its_own_rating_key_and_decision() {
        let (shim, parent) = parent_with_trailer();
        let clip = trailer_rk(parent);
        assert_ne!(clip, parent);
        assert_eq!(trailer_parent_rk(clip), Some(parent));
        // Replace the Node resolver with a muxed fake and install a good prober.
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.trailer_resolver = Box::new(FixedResolver {
            resolved: Some(muxed_resolved()),
        });
        shim.probes.set_prober(Arc::new(|url: &str, _deadline| {
            assert!(url.contains("muxed.mp4"));
            Ok(ok_probe("mp4", "h264"))
        }));
        let shim = Arc::new(shim);
        let row = shim.trailer_row(parent).expect("a clip row");
        assert_eq!(row["ratingKey"].as_str().unwrap(), clip.to_string());
        assert_eq!(
            row["key"].as_str().unwrap(),
            format!("/library/metadata/{clip}")
        );
        assert_eq!(row["type"], "clip");
        let media = row["Media"].as_array().unwrap();
        assert_eq!(media.len(), 1, "muxed-only has one Media");
        assert_eq!(media[0]["container"], "mp4");
        assert_eq!(media[0]["videoCodec"], "h264");
        // `/decision` direct-plays the clip Part, not the film's ranked streams.
        let (drow, verdict, _) = shim.item_probed(clip, Some(0)).expect("clip item");
        assert_eq!(drow["ratingKey"].as_str().unwrap(), clip.to_string());
        assert!(matches!(verdict, Some(Ok(_))));
        let resp = shim.handle(&Request {
            method: "GET".into(),
            path: "/video/:/transcode/universal/decision".into(),
            query: HashMap::from([("path".into(), format!("/library/metadata/{clip}"))]),
            headers: HashMap::new(),
            body: vec![],
        });
        let body: Value = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(body["MediaContainer"]["generalDecisionCode"], 1000);
    }

    #[test]
    fn trailer_dash_serves_mpd_and_decision_direct_plays() {
        let (shim, parent) = parent_with_trailer();
        let clip = trailer_rk(parent);
        let dash_part_id = clip * 1000 + 998;
        let muxed_part_id = clip * 1000 + 999;
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.trailer_resolver = Box::new(FixedResolver {
            resolved: Some(dash_resolved()),
        });
        shim.probes.set_prober(Arc::new(|_url: &str, _deadline| {
            Ok(ok_probe("mp4", "h264"))
        }));
        let shim = Arc::new(shim);
        // MPD shape matches the dash.rs parser contract verbatim.
        let mpd = super::dash_manifest(
            shim.trailer_resolver
                .resolve("x")
                .as_ref()
                .unwrap()
                .dash
                .as_ref()
                .unwrap(),
            None,
            None,
        );
        assert!(mpd.contains("<MPD"));
        assert!(mpd.contains("type=\"static\""));
        assert!(mpd.contains("mediaPresentationDuration=\"PT145S\""));
        assert!(mpd.contains("<SegmentBase"));
        assert!(mpd.contains("<Initialization range=\"0-999\""));
        assert!(mpd.contains("indexRange=\"1000-2000\""));
        assert!(mpd.contains("&amp;"), "BaseURL must be xml-escaped");
        assert!(
            !mpd.contains("<SegmentTemplate"),
            "parser rejects SegmentTemplate"
        );
        let row = shim.trailer_row(parent).expect("a dash clip row");
        let media = row["Media"].as_array().unwrap();
        assert_eq!(media.len(), 2, "DASH leads, muxed falls back");
        assert_eq!(media[0]["Part"][0]["id"], dash_part_id);
        assert_eq!(
            media[0]["Part"][0]["key"].as_str().unwrap(),
            format!("/library/parts/{dash_part_id}/0/file.mpd")
        );
        assert_eq!(media[0]["container"], "mp4");
        assert_eq!(media[0]["videoCodec"], "h264");
        assert_eq!(media[1]["Part"][0]["id"], muxed_part_id);
        // `/library/parts` answers: MPD bytes for DASH, 302 for muxed.
        let mpd_resp = shim.part(dash_part_id);
        assert_eq!(mpd_resp.status, 200);
        assert_eq!(mpd_resp.content_type, "application/dash+xml");
        assert!(String::from_utf8(mpd_resp.body).unwrap().contains("<MPD"));
        let muxed_resp = shim.part(muxed_part_id);
        assert_eq!(muxed_resp.status, 302);
        // `/decision` for clip idx0 direct-plays the DASH Part.
        let resp = shim.handle(&Request {
            method: "GET".into(),
            path: "/video/:/transcode/universal/decision".into(),
            query: HashMap::from([("path".into(), format!("/library/metadata/{clip}"))]),
            headers: HashMap::new(),
            body: vec![],
        });
        let body: Value = serde_json::from_slice(&resp.body).unwrap();
        assert_eq!(body["MediaContainer"]["generalDecisionCode"], 1000);
        assert_eq!(
            body["MediaContainer"]["Metadata"][0]["Media"][0]["Part"][0]["id"],
            dash_part_id
        );
        // `/trailer` single route returns the same clip row.
        let single = shim.trailer_single_response(&parent.to_string(), false);
        assert_eq!(single.status, 200);
        let sbody: Value = serde_json::from_slice(&single.body).unwrap();
        assert_eq!(
            sbody["MediaContainer"]["Metadata"][0]["ratingKey"]
                .as_str()
                .unwrap(),
            clip.to_string()
        );
    }

    #[test]
    fn trailer_dash_probe_refusal_falls_back_to_muxed_only() {
        let (shim, parent) = parent_with_trailer();
        let clip = trailer_rk(parent);
        let dash_part_id = clip * 1000 + 998;
        let muxed_part_id = clip * 1000 + 999;
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.trailer_resolver = Box::new(FixedResolver {
            resolved: Some(dash_resolved()),
        });
        // Video-track probe refuses (e.g. a 403 on the video-only URL): the
        // DASH Media must not ship, the verified muxed row replaces it.
        shim.probes.set_prober(Arc::new(|url: &str, _deadline| {
            if url.contains("cdn.example/video") {
                Err("Status(403)".to_string())
            } else {
                Ok(ok_probe("mp4", "h264"))
            }
        }));
        let shim = Arc::new(shim);
        let row = shim
            .trailer_row(parent)
            .expect("muxed fallback row after dash refusal");
        let media = row["Media"].as_array().unwrap();
        assert_eq!(media.len(), 1, "known-403 DASH must not publish");
        assert_eq!(
            media[0]["Part"][0]["key"].as_str().unwrap(),
            format!("/library/parts/{muxed_part_id}/0/file.mp4")
        );
        // The refused DASH part never entered the map.
        assert_eq!(shim.part(dash_part_id).status, 404);
    }

    #[test]
    fn trailer_dash_probe_uses_direct_googlevideo_baseurls() {
        let (shim, parent) = parent_with_trailer();
        let clip = trailer_rk(parent);
        let dash_part_id = clip * 1000 + 998;
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.trailer_resolver = Box::new(FixedResolver {
            resolved: Some(dash_resolved()),
        });
        let probed = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let probed_clone = probed.clone();
        shim.probes
            .set_prober(Arc::new(move |url: &str, _deadline| {
                probed_clone.lock().unwrap().push(url.to_string());
                Ok(ok_probe("mp4", "h264"))
            }));
        let shim = Arc::new(shim);
        let row = shim.trailer_row(parent).expect("a direct dash clip row");
        let media = row["Media"].as_array().unwrap();
        assert_eq!(media.len(), 2, "DASH leads, muxed falls back");
        // The probe asked the player-visible address (raw googlevideo, fetched
        // via curlio with the IOS UA override), not a loopback proxy.
        let probed = probed.lock().unwrap();
        assert!(
            probed.iter().any(|u| u.contains("cdn.example/video")),
            "probe must hit raw googlevideo, got {probed:?}"
        );
        assert!(
            !probed.iter().any(|u| u.starts_with("http://127.0.0.1")),
            "probe must not hit a loopback proxy, got {probed:?}"
        );
        drop(probed);
        let mpd_resp = shim.part(dash_part_id);
        assert_eq!(mpd_resp.status, 200);
        let mpd = String::from_utf8(mpd_resp.body).unwrap();
        assert!(
            mpd.contains("cdn.example/video"),
            "MPD BaseURLs carry the raw googlevideo URLs"
        );
    }

    /// **The DASH gate's second question.** A googlevideo URL can answer its first window (the
    /// probe passes) and then refuse every later range: the track starts and never finishes, and
    /// the viewer sees "playback failed" instead of the 360p file that plays. The tail ask decides
    /// it, and a refused tail publishes the muxed row only.
    #[test]
    fn a_throttled_dash_pair_publishes_the_muxed_row_only() {
        let (shim, parent) = parent_with_trailer();
        let clip = trailer_rk(parent);
        let dash_part_id = clip * 1000 + 998;
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.trailer_resolver = Box::new(FixedResolver {
            resolved: Some(dash_resolved()),
        });
        shim.probes
            .set_prober(Arc::new(|_url: &str, _deadline| Ok(ok_probe("mp4", "h264"))));
        shim.set_trailer_reacher(Arc::new(|_url: &str, at: i64, _deadline| {
            if at == 0 {
                Ok(42_000_000)
            } else {
                Err("Status(403)".to_string())
            }
        }));
        let shim = Arc::new(shim);
        let row = shim.trailer_row(parent).expect("the muxed row still plays");
        let media = row["Media"].as_array().unwrap();
        assert_eq!(media.len(), 1, "a throttled DASH pair must not publish");
        assert_eq!(
            media[0]["Part"][0]["key"].as_str().unwrap(),
            format!("/library/parts/{}/0/file.mp4", clip * 1000 + 999),
            "the muxed fallback is the row"
        );
        assert_eq!(shim.part(dash_part_id).status, 404, "no MPD was registered");
    }

    /// The same pair when the server does serve its end: the DASH `Media` leads and the muxed
    /// file stays as the fallback.
    #[test]
    fn a_fully_served_dash_pair_publishes_both_medias() {
        let (shim, parent) = parent_with_trailer();
        let clip = trailer_rk(parent);
        let dash_part_id = clip * 1000 + 998;
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.trailer_resolver = Box::new(FixedResolver {
            resolved: Some(dash_resolved()),
        });
        shim.probes
            .set_prober(Arc::new(|_url: &str, _deadline| Ok(ok_probe("mp4", "h264"))));
        shim.set_trailer_reacher(Arc::new(|_url: &str, _at: i64, _deadline| Ok(42_000_000)));
        let shim = Arc::new(shim);
        let row = shim.trailer_row(parent).expect("the dash row");
        assert_eq!(row["Media"].as_array().unwrap().len(), 2);
        assert_eq!(shim.part(dash_part_id).status, 200, "the MPD is registered");
    }

    /// A 2160p VP9 pair (the level a 4K-capable codec table unlocks) must be
    /// declared as what it actually is — `webm`/`vp9`/`4k` — not as h264. The
    /// player picks its feed lane from those fields, and the probe gate now
    /// admits VP9 and AV1.
    #[test]
    fn a_vp9_2160p_dash_pair_is_declared_as_webm_4k() {
        let (shim, parent) = parent_with_trailer();
        let mut resolved = dash_resolved();
        {
            let d = resolved.dash.as_mut().expect("dash fixture");
            d.video_mime = "video/webm; codecs=\"vp9\"".into();
            d.width = 3840;
            d.height = 2160;
        }
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.trailer_resolver = Box::new(FixedResolver {
            resolved: Some(resolved),
        });
        shim.probes
            .set_prober(Arc::new(|_url: &str, _deadline| Ok(ok_probe("webm", "vp9"))));
        let shim = Arc::new(shim);
        let row = shim.trailer_row(parent).expect("the vp9 row");
        let media = row["Media"].as_array().expect("Media");
        assert_eq!(media[0]["container"], "webm");
        assert_eq!(media[0]["videoCodec"], "vp9");
        assert_eq!(media[0]["videoResolution"], "4k");
    }

    struct CountingResolver {
        resolved: Option<crate::shim_trailer::TrailerResolved>,
        calls: Arc<AtomicUsize>,
    }

    impl crate::shim_trailer::TrailerResolver for CountingResolver {
        fn resolve(&self, _youtube_id: &str) -> Option<crate::shim_trailer::TrailerResolved> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.resolved.clone()
        }
    }

    /// A title's next trailer is what plays when the first id is age-gated.
    struct FirstBlocked {
        blocked: String,
        resolved: crate::shim_trailer::TrailerResolved,
        calls: Arc<AtomicUsize>,
    }

    impl crate::shim_trailer::TrailerResolver for FirstBlocked {
        fn resolve(&self, youtube_id: &str) -> Option<crate::shim_trailer::TrailerResolved> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            (youtube_id != self.blocked).then(|| self.resolved.clone())
        }
        fn blocked(&self, youtube_id: &str) -> bool {
            youtube_id == self.blocked
        }
    }

    /// The detail page's `/extras` GET is on its critical path and is asked
    /// again by every focus prefetch. It must answer the shelf row without
    /// spawning Node; the resolve belongs to the first Part request.
    #[test]
    fn the_detail_shelf_row_is_built_without_resolving_the_trailer() {
        let (shim, parent) = parent_with_trailer();
        let clip = trailer_rk(parent);
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        let calls = Arc::new(AtomicUsize::new(0));
        shim.trailer_resolver = Box::new(CountingResolver {
            resolved: Some(dash_resolved()),
            calls: calls.clone(),
        });
        let shim = Arc::new(shim);
        let resp = shim.handle(&Request {
            method: "GET".into(),
            path: format!("/library/metadata/{parent}/extras"),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: vec![],
        });
        assert_eq!(resp.status, 200);
        let body: Value = serde_json::from_slice(&resp.body).unwrap();
        let rows = body["MediaContainer"]["Metadata"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["ratingKey"].as_str().unwrap(), clip.to_string());
        assert_eq!(
            rows[0]["Media"][0]["Part"][0]["key"].as_str().unwrap(),
            format!("/library/parts/{}/0/file.mpd", clip * 1000 + 998)
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "the shelf must not resolve");

        // The player's first byte request is where the Node child runs.
        let mpd = shim.part(clip * 1000 + 998);
        assert_eq!(mpd.status, 200, "the lazy resolve produced the MPD");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// A red-band trailer answers `LOGIN_REQUIRED` to every anonymous client.
    /// The clip row must fall through to the next trailer the title lists
    /// instead of leaving the button dead.
    #[test]
    fn an_age_blocked_trailer_uses_the_next_candidate() {
        let (shim, parent) = parent_with_trailer();
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.metas.insert(
            "movie",
            "tt9990001",
            Arc::new(Meta {
                id: "tt9990001".into(),
                type_: "movie".into(),
                name: "Trailer Host".into(),
                trailer_streams: vec![
                    json!("https://www.youtube.com/watch?v=BBBBBBBBBBB"),
                    json!("https://www.youtube.com/watch?v=CCCCCCCCCCC"),
                ],
                ..Default::default()
            }),
        );
        let calls = Arc::new(AtomicUsize::new(0));
        shim.trailer_resolver = Box::new(FirstBlocked {
            blocked: "BBBBBBBBBBB".into(),
            resolved: dash_resolved(),
            calls: calls.clone(),
        });
        let shim = Arc::new(shim);
        let row = shim.trailer_row(parent).expect("the playable alternate");
        assert!(
            row["thumb"]
                .as_str()
                .unwrap()
                .contains("CCCCCCCCCCC"),
            "the row names the candidate that actually plays"
        );
        // The blocked id is filtered before the resolve, so no Node spawn is
        // spent re-learning a permanent refusal: only the playable candidate
        // is asked for.
        assert_eq!(calls.load(Ordering::SeqCst), 1, "only the playable candidate");
    }

    /// Every trailer a title lists becomes its own shelf tile, each with its
    /// own deterministic Part key; the shelf itself resolves nothing, and a
    /// press resolves exactly the tile that was pressed.
    /// A related show is addressed by a synthetic key and has no Cinemeta
    /// meta to read; its TMDB `videos` answer is the only trailer source, and
    /// it must reach the shelf in trailer-before-teaser order.
    #[test]
    fn a_shows_tmdb_videos_become_shelf_candidates() {
        use plx_stremio::addons_tmdb::{TmdbVideo, TmdbVideos};
        let backend = Arc::new(TrailerMetaBackend {
            calls: AtomicUsize::new(0),
            trailer: String::new(),
        });
        let shim = Arc::new(Shim::new(backend, Config::default()));
        let videos = TmdbVideos {
            results: vec![
                TmdbVideo {
                    key: "BBBBBBBBBBB".into(),
                    name: "Teaser 2".into(),
                    site: "YouTube".into(),
                    type_: "Teaser".into(),
                    ..Default::default()
                },
                TmdbVideo {
                    key: "AAAAAAAAAAA".into(),
                    name: "Official Trailer".into(),
                    site: "YouTube".into(),
                    type_: "Trailer".into(),
                    ..Default::default()
                },
            ],
        };
        let rk = TMDB_SHOW_KEY_BASE + 1396;
        let candidates = shim.trailer_candidates_from_videos(rk, Some(&videos));
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].youtube_id, "AAAAAAAAAAA", "trailer before teaser");
        assert_eq!(candidates[1].youtube_id, "BBBBBBBBBBB");
        let rows = shim.trailer_shelf_rows(rk);
        // `Shim::trailer_candidates` would ask TMDB here; this test exercises
        // the details-in-hand path, so check the row shape directly.
        assert_eq!(trailer_extras(rk, &candidates)["Video"][0]["title"], "Official Trailer");
        assert_eq!(rows.len(), 0, "no TMDB key/meta in this test: nothing to resolve");
    }

    #[test]
    fn the_shelf_lists_every_trailer_and_resolves_only_the_tile_asked_for() {
        let (shim, parent) = parent_with_trailer();
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.metas.insert(
            "movie",
            "tt9990001",
            Arc::new(Meta {
                id: "tt9990001".into(),
                type_: "movie".into(),
                name: "Trailer Host".into(),
                trailer_streams: vec![
                    json!({"name": "Official Trailer", "ytId": "AAAAAAAAAAA"}),
                    json!({"name": "Teaser 2", "ytId": "BBBBBBBBBBB"}),
                ],
                ..Default::default()
            }),
        );
        struct Recorder {
            seen: Arc<Mutex<Vec<String>>>,
        }
        impl crate::shim_trailer::TrailerResolver for Recorder {
            fn resolve(&self, youtube_id: &str) -> Option<crate::shim_trailer::TrailerResolved> {
                self.seen
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(youtube_id.to_string());
                Some(dash_resolved())
            }
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        shim.trailer_resolver = Box::new(Recorder { seen: seen.clone() });
        shim.probes.set_prober(Arc::new(|_url: &str, _deadline| {
            Ok(ok_probe("mp4", "h264"))
        }));
        let shim = Arc::new(shim);

        let rows = shim.trailer_shelf_rows(parent);
        assert_eq!(rows.len(), 2, "both trailers are tiles");
        assert_eq!(rows[0]["title"].as_str().unwrap(), "Official Trailer");
        assert_eq!(rows[1]["title"].as_str().unwrap(), "Teaser 2");
        let clip1 = trailer_rk_at(parent, 1);
        assert_eq!(rows[1]["ratingKey"].as_str().unwrap(), clip1.to_string());
        assert_eq!(
            rows[1]["Media"][0]["Part"][0]["key"].as_str().unwrap(),
            format!("/library/parts/{}/0/file.mpd", clip1 * 1000 + 998)
        );
        assert_eq!(
            rows[1]["thumb"].as_str().unwrap(),
            "https://img.youtube.com/vi/BBBBBBBBBBB/hqdefault.jpg"
        );
        assert!(
            seen.lock().unwrap_or_else(|e| e.into_inner()).is_empty(),
            "the shelf must not resolve"
        );

        // Pressing the SECOND tile resolves the second id only.
        let resp = shim.part(clip1 * 1000 + 998);
        assert_eq!(resp.status, 200);
        assert_eq!(
            *seen.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["BBBBBBBBBBB".to_string()]
        );

        // The first tile is still untouched until it is asked for.
        let first = shim.part(trailer_rk_at(parent, 0) * 1000 + 998);
        assert_eq!(first.status, 200);
        assert_eq!(
            *seen.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["BBBBBBBBBBB".to_string(), "AAAAAAAAAAA".to_string()]
        );
    }

    #[test]
    fn trailer_extras_route_serves_playable_trailer_row() {
        let (shim, parent) = parent_with_trailer();
        let clip = trailer_rk(parent);
        let mut shim = Arc::try_unwrap(shim).ok().expect("single owner");
        shim.trailer_resolver = Box::new(FixedResolver {
            resolved: Some(dash_resolved()),
        });
        shim.probes.set_prober(Arc::new(|_url: &str, _deadline| {
            Ok(ok_probe("mp4", "h264"))
        }));
        let shim = Arc::new(shim);
        // `GET /library/metadata/{parent}/extras` is the detail shelf source
        // (`fetch_extras_rows` -> `pick_trailer`); it must return the clip row
        // with the trailer subtype contract plus a non-empty Part key.
        let resp = shim.handle(&Request {
            method: "GET".into(),
            path: format!("/library/metadata/{parent}/extras"),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: vec![],
        });
        assert_eq!(resp.status, 200);
        let body: Value = serde_json::from_slice(&resp.body).unwrap();
        let rows = body["MediaContainer"]["Metadata"]
            .as_array()
            .expect("extras Metadata array");
        assert_eq!(rows.len(), 1, "one playable trailer row");
        let row = &rows[0];
        assert_eq!(row["ratingKey"].as_str().unwrap(), clip.to_string());
        assert_eq!(row["type"], "clip");
        assert_eq!(row["subtype"], "trailer");
        let extra_type = row["extraType"]
            .as_i64()
            .or_else(|| row["extraType"].as_str().and_then(|s| s.parse().ok()));
        assert_eq!(extra_type, Some(1));
        let part_key = row["Media"][0]["Part"][0]["key"]
            .as_str()
            .expect("first Part key");
        assert!(
            !part_key.is_empty(),
            "playable_trailer_meta needs a Part key"
        );
        // `is_trailer_meta` / `playable_trailer_meta` equivalent.
        let is_trailer = row["subtype"] == "trailer" || extra_type == Some(1);
        assert!(
            is_trailer && !part_key.is_empty(),
            "pick_trailer must accept it"
        );
        // Non-numeric rk and a thumbnail-only title both answer empty, not 404.
        let bad = shim.handle(&Request {
            method: "GET".into(),
            path: "/library/metadata/nope/extras".into(),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: vec![],
        });
        let bbody: Value = serde_json::from_slice(&bad.body).unwrap();
        assert!(bbody["MediaContainer"]["Metadata"]
            .as_array()
            .is_some_and(|a| a.is_empty()));
        // A title whose trailer the resolver cannot serve still advertises the
        // shelf row: the detail open must not wait for (or depend on) a Node
        // round-trip. The Parts answer 404 only when the player asks for bytes.
        let (shim2, parent2) = parent_with_trailer();
        let mut shim2 = Arc::try_unwrap(shim2).ok().expect("single owner");
        shim2.trailer_resolver = Box::new(FixedResolver { resolved: None });
        let shim2 = Arc::new(shim2);
        let shelf = shim2.handle(&Request {
            method: "GET".into(),
            path: format!("/library/metadata/{parent2}/extras"),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: vec![],
        });
        let ebody: Value = serde_json::from_slice(&shelf.body).unwrap();
        let rows = ebody["MediaContainer"]["Metadata"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "the shelf row needs no resolve");
        let part = rows[0]["Media"][0]["Part"][0]["key"].as_str().unwrap();
        assert!(part.ends_with("/file.mpd"));
        let pid = trailer_rk(parent2) * 1000 + 998;
        assert_eq!(
            shim2.part(pid).status,
            404,
            "with no resolver the first byte request is refused"
        );
    }

    #[test]
    fn synthetic_movie_show_extras_nonempty_from_live_fetch() {
        // Synthetic keys have no `Ident` and never populate `metas`, so the
        // live `meta()` fetch is the only way the trailer id is found.
        let backend = Arc::new(TrailerMetaBackend {
            calls: AtomicUsize::new(0),
            trailer: "dQw4w9WgXcQ".into(),
        });
        let shim = Arc::new(Shim::new(backend.clone(), Config::default()));
        // No Keys entry, no metas entry: the rug.
        assert!(shim.keys.ident(TMDB_FALLBACK_KEY_BASE + 603).is_none());
        assert!(shim.keys.ident(TMDB_SHOW_KEY_BASE + 1396).is_none());
        for rk in [TMDB_FALLBACK_KEY_BASE + 603, TMDB_SHOW_KEY_BASE + 1396] {
            let candidates = shim.trailer_candidates(rk, false);
            assert_eq!(
                candidates.first().map(|c| c.youtube_id.as_str()),
                Some("dQw4w9WgXcQ"),
                "live fetch finds the id"
            );
            let extras = trailer_extras(rk, &candidates);
            assert_eq!(extras["Video"].as_array().unwrap().len(), 1);
            assert_eq!(
                extras["Video"][0]["thumb"].as_str().unwrap(),
                "https://img.youtube.com/vi/dQw4w9WgXcQ/hqdefault.jpg"
            );
            assert_eq!(
                extras["Video"][0]["key"].as_str().unwrap(),
                format!("/library/metadata/{}", trailer_rk(rk))
            );
        }
        assert!(
            backend.calls.load(Ordering::SeqCst) >= 2,
            "both kinds went to the backend live"
        );
    }
}

#[cfg(test)]
mod subtitle_search_tests {
    use super::*;
    use plx_stremio::model::Subtitle;

    /// Answers the on-demand search with one English and one German candidate, and records what
    /// `(type, id)` the shim asked for.
    #[derive(Default)]
    struct SearchBackend {
        asked: Mutex<Vec<(String, String)>>,
        fail: bool,
        /// What the viewer's own installed addons carry (the automatic lane's source).
        own: Vec<Subtitle>,
    }

    impl Backend for SearchBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> {
            Vec::new()
        }
        fn catalog_page(&self, _key: &str, _skip: u32, _genre: Option<&str>) -> Result<Vec<MetaPreview>, ()> {
            Ok(Vec::new())
        }
        fn search(&self, _query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
            Vec::new()
        }
        fn meta(&self, type_: &str, id: &str) -> Option<Meta> {
            Some(Meta { id: id.into(), type_: type_.into(), name: "Test Title".into(), ..Default::default() })
        }
        fn streams(&self, _type_: &str, _id_: &str, _deadline: Duration, _on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream> {
            vec![Stream { url: Some("https://example.com/f.mkv".into()), name: Some("Source".into()), ..Default::default() }]
        }
        fn subtitles(&self, _type_: &str, _id_: &str) -> Vec<Subtitle> {
            self.own.clone()
        }
        fn search_subtitles(&self, type_: &str, id: &str) -> Option<Vec<Subtitle>> {
            self.asked.lock().unwrap().push((type_.into(), id.into()));
            if self.fail {
                return None;
            }
            Some(vec![
                Subtitle {
                    id: Some("a".into()),
                    url: "https://subs.example/en.srt".into(),
                    lang: "eng".into(),
                    movie_release_name: Some("The.Matrix.1999.1080p.BluRay".into()),
                    provider: Some("OpenSubtitles".into()),
                    ..Default::default()
                },
                Subtitle {
                    id: Some("b".into()),
                    url: "https://subs.example/de.srt".into(),
                    lang: "ger".into(),
                    subtitle_file_name: Some("Matrix.german.srt".into()),
                    provider: Some("OpenSubtitles".into()),
                    ..Default::default()
                },
            ])
        }
    }

    fn request(method: &str, path: &str, query: &[(&str, &str)]) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            query: query.iter().map(|(k, v)| ((*k).into(), (*v).into())).collect(),
            headers: HashMap::new(),
            body: Vec::new(),
        }
    }

    fn body(response: &Response) -> Value {
        serde_json::from_slice(&response.body).unwrap()
    }

    fn search_path(rk: i64) -> String {
        format!("/library/metadata/{rk}/subtitles")
    }

    /// The Search page asks in a 2-letter language; the addon answers in 639-2 spellings. Only the
    /// asked language comes back, each row carrying a servable key and the release name it will be
    /// recognised by later.
    #[test]
    fn the_search_answers_the_builtin_provider_in_the_asked_language() {
        let backend = Arc::new(SearchBackend::default());
        let shim = Arc::new(Shim::new(backend.clone(), Config::default()));
        let rk = shim.keys.key_of(&Ident::Title { type_: "movie".into(), id: "tt0133093".into() });

        let response = shim.handle(&request("GET", &search_path(rk), &[("language", "en")]));
        assert_eq!(response.status, 200);
        let body = body(&response);
        let streams = body["MediaContainer"]["Stream"].as_array().unwrap();
        assert_eq!(streams.len(), 1, "the German row is on the page's other language");
        let row = &streams[0];
        assert_eq!(row["languageCode"], "eng");
        assert_eq!(row["providerTitle"], "OpenSubtitles");
        assert_eq!(row["title"], "The.Matrix.1999.1080p.BluRay");

        let key = row["key"].as_str().unwrap();
        assert!(key.starts_with("/library/streams/") && key.ends_with(".srt"), "{key}");
        let sid: i64 = key.trim_start_matches("/library/streams/").trim_end_matches(".srt").parse().unwrap();
        assert_eq!(
            shim.sub_urls.lock().unwrap().get(&sid).map(String::as_str),
            Some("https://subs.example/en.srt"),
            "the key's id is how the install finds the URL"
        );
        assert_eq!(
            *backend.asked.lock().unwrap(),
            vec![("movie".to_string(), "tt0133093".to_string())]
        );
    }

    /// A series is searched by the EPISODE's video id (`tt…:S:E`), which is what OpenSubtitles v3
    /// indexes; asking for the show would return the show's own subtitles, not this episode's.
    #[test]
    fn an_episode_is_searched_by_its_video_id() {
        let backend = Arc::new(SearchBackend::default());
        let shim = Arc::new(Shim::new(backend.clone(), Config::default()));
        let rk = shim.keys.key_of(&Ident::Episode {
            series: "tt0944947".into(),
            video: "tt0944947:1:1".into(),
        });
        let _ = shim.handle(&request("GET", &search_path(rk), &[("language", "eng")]));
        assert_eq!(
            *backend.asked.lock().unwrap(),
            vec![("series".to_string(), "tt0944947:1:1".to_string())]
        );
    }

    /// The `PUT` names only the key; the item's next read must list the sidecar under that
    /// release name, selected — the same shape a real PMS produces, which is what `identify`
    /// diffs and the panel then auto-selects.
    #[test]
    fn an_installed_candidate_becomes_a_selected_sidecar_of_the_item() {
        let shim = Arc::new(Shim::new(Arc::new(SearchBackend::default()), Config::default()));
        let rk = shim.keys.key_of(&Ident::Title { type_: "movie".into(), id: "tt0133093".into() });
        let found = body(&shim.handle(&request("GET", &search_path(rk), &[("language", "en")])));
        let key = found["MediaContainer"]["Stream"][0]["key"].as_str().unwrap().to_string();

        let put = shim.handle(&request("PUT", &search_path(rk), &[("key", &key)]));
        assert_eq!(put.status, 200);

        let item = body(&shim.handle(&request("GET", &format!("/library/metadata/{rk}"), &[])));
        let media = item["MediaContainer"]["Metadata"][0]["Media"].as_array().expect("a part carries the sidecars");
        let streams = media[0]["Part"][0]["Stream"].as_array().unwrap();
        let sub = streams
            .iter()
            .find(|s| s["streamType"] == 3)
            .expect("the installed subtitle is on the item");
        assert_eq!(sub["title"], "The.Matrix.1999.1080p.BluRay");
        assert_eq!(sub["languageCode"], "eng");
        assert_eq!(sub["selected"], true, "the newest install is the server's pick");
    }

    /// **The page searches what the viewer already has, too.** A language the builtin provider's
    /// answer does not carry may still be on the item from the viewer's own subtitle addons — the
    /// same set that fills "Other languages" — and the Search page must not say "none found" for
    /// a subtitle the menu beside it lists. Rows carry the answering addon's name.
    #[test]
    fn a_language_only_the_viewers_addons_carry_is_found() {
        let backend = Arc::new(SearchBackend {
            own: vec![Subtitle {
                id: Some("c".into()),
                url: "https://subs.example/id.srt".into(),
                lang: "ind".into(),
                movie_release_name: Some("The.Matrix.1999.1080p.BluRay".into()),
                provider: Some("SubDL".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        let shim = Arc::new(Shim::new(backend, Config::default()));
        let rk = shim.keys.key_of(&Ident::Title { type_: "movie".into(), id: "tt0133093".into() });

        let found = body(&shim.handle(&request("GET", &search_path(rk), &[("language", "id")])));
        let rows = found["MediaContainer"]["Stream"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "the builtin answers only English/German");
        assert_eq!(rows[0]["languageCode"], "ind");
        assert_eq!(rows[0]["providerTitle"], "SubDL");
        assert_eq!(rows[0]["title"], "The.Matrix.1999.1080p.BluRay");
    }

    /// A provider that did not answer is a fault the page offers a retry for, never "none found".
    #[test]
    fn a_provider_failure_is_a_fault_the_page_can_retry() {
        let backend = Arc::new(SearchBackend { fail: true, ..Default::default() });
        let shim = Arc::new(Shim::new(backend, Config::default()));
        let rk = shim.keys.key_of(&Ident::Title { type_: "movie".into(), id: "tt0133093".into() });
        let response = shim.handle(&request("GET", &search_path(rk), &[("language", "en")]));
        assert_eq!(response.status, 502);
    }
}


/// The host twin of the device's C-engine path: the real shim, the real `libsbengine.so` and a
/// real loopback socket. Run it with `tools/host-engine-check.sh`; it is ignored everywhere else
/// because a host without the x86-64 engine has nothing to exercise.
#[cfg(test)]
mod host_c_engine {
    use super::*;
    use plx_stremio::model::Stream;
    use std::io::{Read, Write};

    const PEERLESS_ID: &str = "tt9900001";
    const SEEDED_ID: &str = "tt9900002";
    /// No tracker, no DHT entry, no peers: the engine accepts the magnet and never learns its
    /// metadata, so `resolve_stream` sits out its whole 30 s budget before failing.
    const PEERLESS_HASH: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f001234567";

    struct OneMagnetBackend {
        magnet: String,
        id: &'static str,
    }

    impl OneMagnetBackend {
        fn peerless() -> OneMagnetBackend {
            OneMagnetBackend {
                magnet: format!("magnet:?xt=urn:btih:{PEERLESS_HASH}"),
                id: PEERLESS_ID,
            }
        }
        fn seeded(magnet: String) -> OneMagnetBackend {
            OneMagnetBackend { magnet, id: SEEDED_ID }
        }
    }

    impl Backend for OneMagnetBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> {
            Vec::new()
        }
        fn catalog_page(&self, _key: &str, _skip: u32, _genre: Option<&str>) -> Result<Vec<MetaPreview>, ()> {
            Ok(Vec::new())
        }
        fn search(&self, _query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> {
            Vec::new()
        }
        fn meta(&self, type_: &str, id: &str) -> Option<Meta> {
            (type_ == "movie" && id == self.id).then(|| Meta {
                id: id.into(),
                type_: type_.into(),
                name: "Host Movie".into(),
                runtime: Some("2 h".into()),
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
            vec![Stream {
                url: Some(self.magnet.clone()),
                name: Some("Torrentio\n1080p".into()),
                addon_name: "Torrentio".into(),
                addon_id: "torrentio".into(),
                ..Default::default()
            }]
        }
        fn subtitles(&self, _type_: &str, _id: &str) -> Vec<plx_stremio::model::Subtitle> {
            Vec::new()
        }
    }

    fn host_engine_enabled() -> bool {
        std::env::var_os("PLX_HOST_ENGINE_CHECK").as_deref() == Some(std::ffi::OsStr::new("1"))
    }

    fn start_host_engine() {
        assert!(
            plx_stremio::sbengine::available(),
            "libsbengine.so did not load; point PLXNATIVE_APP_DIR at build/sbengine-host"
        );
        let root = std::env::temp_dir().join(format!("nuvio-host-engine-{}", std::process::id()));
        let (state, cache) = (root.join("state"), root.join("cache"));
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&cache).unwrap();
        assert!(
            plx_stremio::sbengine::start_engine(
                state.to_str().unwrap(),
                cache.to_str().unwrap()
            ),
            "the host engine refused to start"
        );
    }

    fn host_shim(backend: OneMagnetBackend, id: &str) -> (crate::Handle, i64) {
        let cfg = Config {
            p2p_enabled: true,
            // Only the marker that torrents are on; the engine serves its own URL and this
            // address is never dialled.
            torrent_server: Some("http://127.0.0.1:1".into()),
            ..Config::default()
        };
        let handle = crate::start(Arc::new(backend), cfg, 0).expect("bind loopback");
        let rk = handle.shim.keys.key_of(&Ident::Title {
            type_: "movie".into(),
            id: id.into(),
        });
        (handle, rk)
    }

    struct HttpReply {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl HttpReply {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        }
    }

    fn http_get(host: &str, port: u16, target: &str, range: Option<&str>, wait: Duration) -> HttpReply {
        let mut sock = std::net::TcpStream::connect((host, port)).expect("connect");
        sock.set_read_timeout(Some(wait)).unwrap();
        let mut request = format!(
            "GET {target} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n"
        );
        if let Some(r) = range {
            request.push_str(&format!("Range: {r}\r\n"));
        }
        request.push_str("\r\n");
        sock.write_all(request.as_bytes()).unwrap();
        let mut raw = Vec::new();
        sock.read_to_end(&mut raw).unwrap();
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("a complete HTTP response");
        let head = std::str::from_utf8(&raw[..split]).unwrap();
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .expect("an HTTP status");
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        HttpReply { status, headers, body: raw[split + 4..].to_vec() }
    }

    fn http_get_url(url: &str, range: Option<&str>, wait: Duration) -> HttpReply {
        let rest = url.strip_prefix("http://").expect("a plaintext engine URL");
        let (authority, target) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = authority.rsplit_once(':').expect("host:port");
        http_get(host, port.parse().expect("a port"), target, range, wait)
    }

    #[test]
    #[ignore = "host-only: needs a host libsbengine.so (tools/host-engine-check.sh)"]
    fn host_c_engine_keeps_the_decision_silent_past_the_media_budget() {
        if !host_engine_enabled() {
            return;
        }
        start_host_engine();
        let (handle, rk) = host_shim(OneMagnetBackend::peerless(), PEERLESS_ID);

        let started = Instant::now();
        let reply = http_get(
            "127.0.0.1",
            handle.port,
            &format!("/video/:/transcode/universal/decision?path=/library/metadata/{rk}"),
            None,
            Duration::from_secs(90),
        );
        let elapsed = started.elapsed();

        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        let container = &body["MediaContainer"];
        // The peerless magnet never resolves, so the answer is a refusal — but it ARRIVES, after
        // the engine's 30 s metadata budget. The old client capped this at the 15 s media stall
        // budget and gave up first, which is the first-play "playback failed" this test guards.
        assert_eq!(container["generalDecisionCode"], 2000, "{body}");
        assert!(
            container["transcodeDecisionText"]
                .as_str()
                .is_some_and(|t| t.contains("could not be opened")),
            "{body}"
        );
        assert!(
            elapsed >= Duration::from_secs(20),
            "the decision answered in {elapsed:?}; the engine was not the thing holding it"
        );
        assert_eq!(
            plx_stremio::sbengine::active_hash().as_deref(),
            Some(PEERLESS_HASH),
            "the engine never adopted the magnet"
        );
        println!("host C engine held /decision for {elapsed:?} and then answered");
    }

    /// The first-try playback the release gate is about: one `/decision` request, no retry, a
    /// directplay verdict, and the player's Part URL serving the seeded file's own bytes through
    /// the engine. `tools/host-engine-check.sh` starts the loopback seeder and sets both env vars.
    #[test]
    #[ignore = "host-only: needs a host libsbengine.so and a loopback seeder (tools/host-engine-check.sh)"]
    fn host_torrent_plays_on_first_try() {
        if !host_engine_enabled() {
            return;
        }
        let (Ok(magnet), Ok(payload_path)) = (
            std::env::var("PLX_HOST_TORRENT_MAGNET"),
            std::env::var("PLX_HOST_TORRENT_FILE"),
        ) else {
            return;
        };
        let payload = std::fs::read(payload_path).expect("the seeded payload");
        assert!(!payload.is_empty(), "the seeded payload is empty");
        start_host_engine();
        let (handle, rk) = host_shim(OneMagnetBackend::seeded(magnet), SEEDED_ID);

        // Exactly one decision request, by construction: no retry can hide a cold-start failure.
        let started = Instant::now();
        let reply = http_get(
            "127.0.0.1",
            handle.port,
            &format!("/video/:/transcode/universal/decision?path=/library/metadata/{rk}&mediaIndex=0"),
            None,
            Duration::from_secs(90),
        );
        let decision_wait = started.elapsed();
        assert_eq!(reply.status, 200, "the decision answered {}", reply.status);
        let body: Value = serde_json::from_slice(&reply.body).unwrap();
        let container = &body["MediaContainer"];
        assert_eq!(
            container["generalDecisionCode"],
            1000,
            "{body} engine stats {:?}",
            plx_stremio::sbengine::stats()
        );
        let media = &container["Metadata"][0]["Media"][0];
        assert_eq!(media["Part"][0]["decision"], "directplay", "{body}");
        let part_key = media["Part"][0]["key"]
            .as_str()
            .expect("the Part key the player opens")
            .to_string();

        let part = http_get("127.0.0.1", handle.port, &part_key, None, Duration::from_secs(30));
        assert_eq!(part.status, 302, "the Part must hand the player the engine URL");
        let location = part.header("location").expect("a Location header").to_string();
        assert!(location.starts_with("http://127.0.0.1:"), "{location}");

        let served = http_get_url(&location, Some("bytes=0-262143"), Duration::from_secs(90));
        assert!(
            served.status == 200 || served.status == 206,
            "the engine answered {} for the stream",
            served.status
        );
        assert!(!served.body.is_empty(), "the engine served no bytes");
        let n = served.body.len().min(payload.len());
        assert_eq!(
            &served.body[..n],
            &payload[..n],
            "the engine served bytes that are not the seeded file"
        );
        assert!(
            served.body.len() >= 256 * 1024 || served.body.len() == payload.len(),
            "only {} bytes were served",
            served.body.len()
        );
        println!(
            "first decision directplayed in {decision_wait:?}; {} bytes from the engine match the seeded file",
            served.body.len()
        );
    }
}
