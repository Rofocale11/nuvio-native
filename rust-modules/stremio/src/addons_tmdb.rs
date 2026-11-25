//! TMDB movie/TV metadata for the in-process PMS shim (cast, recommendations, title logos,
//! trailers, person pages, franchise shelves). This is a Nuvio-only enrichment source: upstream
//! PlxNative talks to Plex, not TMDB, so there is no equivalent module to extend.
//!
//! added as a fork-owned metadata client. The shim needs one HTTP request per
//! detail row (`append_to_response=credits,recommendations,images,videos`) and a 15-minute cache,
//! so this module owns both rather than teaching the addon protocol layer about TMDB.

use crate::addon_http;
use crate::model::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

const API_BASE: &str = "https://api.themoviedb.org/3";
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

type ResponseCache = Mutex<HashMap<(String, String), (Instant, Value)>>;

fn shared_cache() -> Arc<ResponseCache> {
    static CACHE: OnceLock<Arc<ResponseCache>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(Mutex::new(HashMap::new()))).clone()
}

/// One in-flight fetch per cache key. A detail prefetch and the click it warmed both miss the
/// cache for the same title; the second must wait for the download already running, not start a
/// second one. upstream's `plx_base::flight` does not exist in this fork, so this
/// is a small local Condvar-based single-flight scoped to this module's cache keys.
struct Flight {
    done: Mutex<bool>,
    wake: Condvar,
}

type Flights = Mutex<HashMap<String, Arc<Flight>>>;

fn shared_flights() -> Arc<Flights> {
    static FLIGHTS: OnceLock<Arc<Flights>> = OnceLock::new();
    FLIGHTS.get_or_init(|| Arc::new(Mutex::new(HashMap::new()))).clone()
}

/// A cached TMDB client. `Clone` shares the cache and the single-flight table across clones.
/// `Debug` never prints the API key: these clients can be captured in diagnostics.
#[derive(Clone)]
pub struct TmdbClient {
    api_key: String,
    cache: Arc<ResponseCache>,
    flights: Arc<Flights>,
}

impl std::fmt::Debug for TmdbClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TmdbClient")
            .field("configured", &self.configured())
            .finish_non_exhaustive()
    }
}

impl Default for TmdbClient {
    fn default() -> Self {
        Self::new()
    }
}

// ---- public serde shapes ---------------------------------------------------------------------

/// One cast row. `id` is TMDB's person id: the shim encodes it into a synthetic ratingKey so a
/// headshot can open the person's filmography.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbCast {
    pub id: u64,
    pub name: String,
    pub character: String,
    pub profile_path: Option<String>,
}

/// One crew/cast row of a person's combined credits. The kind is per row (`media_type`), and the
/// title fields follow it: `title`/`release_date` for a film, `name`/`first_air_date` for a show.
/// the old fork carried these so a filmography row shows the WORK's poster and
/// synopsis; the merge reduced them to the person's profile path and name.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbCredit {
    pub id: u64,
    pub name: String,
    pub title: String,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub release_date: Option<String>,
    pub first_air_date: Option<String>,
    pub character: String,
    pub job: String,
    pub department: String,
    pub profile_path: Option<String>,
    pub media_type: String,
}

impl TmdbCredit {
    /// this credit as the film it is, for the row builders that speak TmdbMovie.
    pub fn movie(&self) -> TmdbMovie {
        TmdbMovie {
            id: self.id,
            title: if self.title.is_empty() { self.name.clone() } else { self.title.clone() },
            overview: self.overview.clone(),
            poster_path: self.poster_path.clone(),
            backdrop_path: self.backdrop_path.clone(),
            release_date: self.release_date.clone(),
            ..TmdbMovie::default()
        }
    }

    /// this credit as the show it is; TMDB spells a show's fields differently.
    pub fn show(&self) -> TmdbShow {
        TmdbShow {
            id: self.id,
            name: if self.name.is_empty() { self.title.clone() } else { self.name.clone() },
            overview: self.overview.clone(),
            poster_path: self.poster_path.clone(),
            backdrop_path: self.backdrop_path.clone(),
            first_air_date: self.first_air_date.clone(),
            ..TmdbShow::default()
        }
    }
}

/// a TMDB genre, used by the Library sort/filter baseline and recommendation rows.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbGenre {
    pub id: u64,
    pub name: String,
}

/// `belongs_to_collection` on a movie: the id to ask `/collection/{id}` with, and the name to head
/// the shelf with until that answer arrives. `None` for a standalone film.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbCollectionRef {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbMovie {
    pub id: u64,
    pub title: String,
    /// a recommendation/filmography row's synopsis; the merge dropped it.
    pub overview: Option<String>,
    pub release_date: Option<String>,
    pub runtime: Option<u32>,
    pub backdrop_path: Option<String>,
    pub poster_path: Option<String>,
    /// Genre tags on a TMDB-only recommendation row.
    pub genres: Vec<TmdbGenre>,
    pub belongs_to_collection: Option<TmdbCollectionRef>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbCollection {
    pub id: u64,
    pub name: String,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub parts: Vec<TmdbMovie>,
}

/// A show, where TMDB spells a movie's `title`/`release_date` as `name`/`first_air_date`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbShow {
    pub id: u64,
    pub name: String,
    /// restoration of the old fork's show shape.
    pub overview: Option<String>,
    pub first_air_date: Option<String>,
    pub backdrop_path: Option<String>,
    pub poster_path: Option<String>,
    pub genres: Vec<TmdbGenre>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbPerson {
    /// TMDB's person id, restored from the old fork's type.
    pub id: u64,
    pub name: String,
    pub biography: String,
    pub birthday: Option<String>,
    pub deathday: Option<String>,
    pub place_of_birth: Option<String>,
    pub known_for_department: Option<String>,
    pub profile_path: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbVideo {
    pub id: String,
    pub key: String,
    pub site: String,
    pub name: String,
    #[serde(rename = "type")]
    pub type_: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbVideos {
    pub results: Vec<TmdbVideo>,
}

/// The `credits` half of a details response. Cast rows carry the character/profile path a Role
/// needs; crew rows carry the department/job the person page groups by.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbCredits {
    pub cast: Vec<TmdbCast>,
    pub crew: Vec<TmdbCredit>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbLogo {
    pub file_path: Option<String>,
    /// `"en"`, `"de"`, `"xx"` for a wordmark with no words, or `null`.
    pub iso_639_1: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbImages {
    pub logos: Vec<TmdbLogo>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbRecommendations {
    pub results: Vec<TmdbMovie>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbShowRecommendations {
    pub results: Vec<TmdbShow>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbMovieDetails {
    /// the old fork's nested shape. `TmdbMovie` already carries the About text,
    /// Genre tags and collection reference the flattened version duplicated.
    #[serde(flatten)]
    pub movie: TmdbMovie,
    pub credits: TmdbCredits,
    pub recommendations: TmdbRecommendations,
    pub images: Option<TmdbImages>,
    pub videos: Option<TmdbVideos>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbShowDetails {
    /// the old fork's nested shape, matching `TmdbMovieDetails`.
    #[serde(flatten)]
    pub show: TmdbShow,
    pub credits: TmdbCredits,
    pub recommendations: TmdbShowRecommendations,
    pub images: Option<TmdbImages>,
    pub videos: Option<TmdbVideos>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TmdbCombinedCredits {
    pub cast: Vec<TmdbCredit>,
    pub crew: Vec<TmdbCredit>,
}

// ---- wire helpers ---------------------------------------------------------------------------

#[derive(Default, Deserialize)]
#[serde(default)]
struct FindResponse {
    movie_results: Vec<TmdbMovie>,
    tv_results: Vec<TmdbShow>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ExternalIdsResponse {
    imdb_id: Option<String>,
}

/// `/find` answers a movie's IMDb id under `movie_results`.
fn movie_from_find(found: &FindResponse) -> Option<u64> {
    found.movie_results.first().map(|m| m.id).filter(|id| *id > 0)
}

/// `/find` answers a show's IMDb id under `tv_results`, not `movie_results` — using the movie
/// helper for a series silently drops the cast and every show-only field.
fn tv_from_find(found: &FindResponse) -> Option<u64> {
    found.tv_results.first().map(|s| s.id).filter(|id| *id > 0)
}

fn trimmed_imdb_id(response: &ExternalIdsResponse) -> Option<String> {
    response
        .imdb_id
        .as_deref()
        .map(str::trim)
        .filter(|id| id.starts_with("tt"))
        .map(str::to_string)
}

/// `tmdb:123`, `123` and `tmdb603` need no `/find` round trip. Returns `None` for an IMDb id or
/// any other shape, which is the signal to fall through to the cached `/find` call.
fn direct_tmdb_id(id: &str) -> Option<u64> {
    let value = id
        .strip_prefix("tmdb:")
        .or_else(|| id.strip_prefix("TMDB:"))
        .or_else(|| id.strip_prefix("tmdb"))
        .unwrap_or(id);
    value
        .split(':')
        .next()
        .unwrap_or("")
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
}

// ---- client ----------------------------------------------------------------------------------

impl TmdbClient {
    /// the old fork's constructor — `None` when no key is configured, so a caller
    /// can skip the lookup entirely instead of checking `configured()`.
    pub fn from_env() -> Option<Self> {
        let client = Self::new();
        client.configured().then_some(client)
    }

    /// Uses the compiled-in or runtime `TMDB_API_KEY`; an unset or blank key disables every lookup.
    /// `Debug` and the cache key never expose the value.
    pub fn new() -> Self {
        let api_key = option_env!("TMDB_API_KEY")
            .map(str::to_string)
            .or_else(|| std::env::var("TMDB_API_KEY").ok())
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .unwrap_or_default();
        Self {
            api_key,
            cache: shared_cache(),
            flights: shared_flights(),
        }
    }

    pub fn configured(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn url(&self, path: &str) -> String {
        let separator = if path.contains('?') { '&' } else { '?' };
        format!(
            "{API_BASE}{path}{separator}api_key={}",
            addon_http::encode_component(&self.api_key)
        )
    }

    /// The single door to the cache and the HTTP client. An unconfigured client never reaches the
    /// HTTP door, so an unset key makes no request at all.
    fn cached(&self, path: &str) -> Result<Value> {
        if !self.configured() {
            return Err(Error::NoAddon);
        }
        let key = (self.api_key.clone(), path.to_string());
        let url = self.url(path);
        cached_json(&self.cache, &self.flights, key, || {
            addon_http::get_json(&url, addon_http::ADDON)
        })
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let body = self.cached(path)?;
        serde_json::from_value(body).map_err(|error| Error::Parse(error.to_string()))
    }

    /// `/movie/{id}?append_to_response=credits,recommendations,images,videos` — one request for the
    /// cast, related films, title logo and every trailer.
    pub fn movie_details(&self, id: u64) -> Result<TmdbMovieDetails> {
        self.get(&format!(
            "/movie/{id}?append_to_response=credits,recommendations,images,videos"
        ))
    }

    /// `/tv/{id}` with the same append. A show's `credits`/`videos` live only here; there is no
    /// separate `/videos` hop.
    pub fn tv_details(&self, id: u64) -> Result<TmdbShowDetails> {
        self.get(&format!(
            "/tv/{id}?append_to_response=credits,recommendations,images,videos"
        ))
    }

    /// Resolves a Stremio movie id (`tmdb:123`, `123`, `tt…`) to TMDB's numeric id. An IMDb id uses
    /// the shared `/find` endpoint and reads `movie_results`.
    pub fn movie_id(&self, id: &str) -> Result<Option<u64>> {
        let id = crate::addons::content_level_id(id);
        if let Some(parsed) = direct_tmdb_id(&id) {
            return Ok(Some(parsed));
        }
        if id.starts_with("tt") {
            let found: FindResponse = self.get(&format!("/find/{id}?external_source=imdb_id"))?;
            return Ok(movie_from_find(&found));
        }
        Ok(None)
    }

    /// Resolves a series id. Same endpoint as [`Self::movie_id`], but a show's IMDb id is returned
    /// under `tv_results` — reading `movie_results` here loses the show.
    pub fn series_id(&self, id: &str) -> Result<Option<u64>> {
        let id = crate::addons::content_level_id(id);
        if let Some(parsed) = direct_tmdb_id(&id) {
            return Ok(Some(parsed));
        }
        if !id.starts_with("tt") {
            // the old fork fell back to the movie lookup for any non-IMDb id —
            // a bare numeric id or `tmdb:<n>` is a series just as much as a film.
            return self.movie_id(&id);
        }
        let found: FindResponse = self.get(&format!("/find/{id}?external_source=imdb_id"))?;
        Ok(tv_from_find(&found))
    }

    /// the old fork's separate cast call, retained for callers that do not need a
    /// full details answer.
    pub fn credits(&self, movie_id: u64) -> Result<Vec<TmdbCast>> {
        let response: TmdbCredits = self.get(&format!("/movie/{movie_id}/credits"))?;
        Ok(response.cast)
    }

    /// cast-only filmography, as the old `/library/people/{id}/media` used it.
    pub fn person_credits(&self, person_id: u64) -> Result<Vec<TmdbCredit>> {
        let response: TmdbCombinedCredits = self.get(&format!("/person/{person_id}/combined_credits"))?;
        Ok(response.cast)
    }

    /// the old alias kept for callers that spoke the generic name.
    pub fn external_ids(&self, movie_id: u64) -> Result<Option<String>> {
        self.movie_external_ids(movie_id)
    }

    /// the old standalone trailer-list call. Current details fold `videos` into
    /// their `append_to_response`, so this is a cached single-path helper, not a new request shape.
    pub fn videos(&self, movie_id: u64) -> Result<Vec<TmdbVideo>> {
        let response: TmdbVideos = self.get(&format!("/movie/{movie_id}/videos"))?;
        Ok(response.results)
    }

    /// the old standalone recommendations call.
    pub fn recommendations(&self, movie_id: u64) -> Result<Vec<TmdbMovie>> {
        let response: TmdbRecommendations = self.get(&format!("/movie/{movie_id}/recommendations"))?;
        Ok(response.results)
    }

    pub fn movie_external_ids(&self, id: u64) -> Result<Option<String>> {
        let response: ExternalIdsResponse = self.get(&format!("/movie/{id}/external_ids"))?;
        Ok(trimmed_imdb_id(&response))
    }

    pub fn tv_external_ids(&self, id: u64) -> Result<Option<String>> {
        let response: ExternalIdsResponse = self.get(&format!("/tv/{id}/external_ids"))?;
        Ok(trimmed_imdb_id(&response))
    }

    /// `/person/{id}` — the biography and life facts the person page's header draws.
    pub fn person_details(&self, id: u64) -> Result<TmdbPerson> {
        self.get(&format!("/person/{id}"))
    }

    /// `/person/{id}/combined_credits` — both halves: what they played and what they worked on.
    pub fn person_career(&self, id: u64) -> Result<TmdbCombinedCredits> {
        self.get(&format!("/person/{id}/combined_credits"))
    }

    /// `/collection/{id}` — the franchise a movie's `belongs_to_collection` names.
    pub fn collection_details(&self, id: u64) -> Result<TmdbCollection> {
        self.get(&format!("/collection/{id}"))
    }
}

/// Cached GET with a tiny per-key single-flight. The cache is checked first; on a miss the first
/// caller claims the key and fetches while the others wait on its Condvar, then re-check the cache.
/// An error releases the key so a waiter retries rather than inheriting a failure it never saw.
fn cached_json<F>(
    cache: &ResponseCache,
    flights: &Flights,
    key: (String, String),
    mut fetch: F,
) -> Result<Value>
where
    F: FnMut() -> Result<Value>,
{
    loop {
        if let Some((at, value)) = cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(&key)
        {
            if at.elapsed() < CACHE_TTL {
                return Ok(value.clone());
            }
        }

        let flight_key = format!("{}\u{0}{}", key.0, key.1);
        let (flight, leader) = {
            let mut map = flights.lock().unwrap_or_else(|error| error.into_inner());
            match map.get(&flight_key) {
                Some(existing) => (existing.clone(), false),
                None => {
                    let entry = Arc::new(Flight {
                        done: Mutex::new(false),
                        wake: Condvar::new(),
                    });
                    map.insert(flight_key.clone(), entry.clone());
                    (entry, true)
                }
            }
        };

        if !leader {
            let mut done = flight.done.lock().unwrap_or_else(|error| error.into_inner());
            while !*done {
                done = flight.wake.wait(done).unwrap_or_else(|error| error.into_inner());
            }
            continue;
        }

        let result = fetch();
        if let Ok(value) = &result {
            let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
            if cache.len() >= 512 {
                cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
                if cache.len() >= 512 {
                    cache.clear();
                }
            }
            cache.insert(key.clone(), (Instant::now(), value.clone()));
        }
        // Retire the key before waking waiters: anyone arriving after this point sees the fresh
        // cache on their first check, or claims a brand-new flight if the fetch failed.
        {
            let mut map = flights.lock().unwrap_or_else(|error| error.into_inner());
            if map.get(&flight_key).is_some_and(|cur| Arc::ptr_eq(cur, &flight)) {
                map.remove(&flight_key);
            }
        }
        *flight.done.lock().unwrap_or_else(|error| error.into_inner()) = true;
        flight.wake.notify_all();
        return result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Meta;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn client(api_key: &str) -> TmdbClient {
        TmdbClient {
            api_key: api_key.to_string(),
            cache: Arc::new(Mutex::new(HashMap::new())),
            flights: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[test]
    fn moviedb_id_decodes_a_number_and_a_string() {
        let number: Meta = serde_json::from_value(serde_json::json!({
            "id": "tt1", "type": "movie", "name": "M", "moviedb_id": 603
        }))
        .unwrap();
        assert_eq!(number.moviedb_id, Some(603));

        let string: Meta = serde_json::from_value(serde_json::json!({
            "id": "tt2", "type": "series", "name": "S", "moviedb_id": "1396"
        }))
        .unwrap();
        assert_eq!(string.moviedb_id, Some(1396));

        let absent: Meta = serde_json::from_value(serde_json::json!({
            "id": "tt3", "type": "movie", "name": "X"
        }))
        .unwrap();
        assert_eq!(absent.moviedb_id, None);
    }

    #[test]
    fn movie_id_reads_movie_results_and_series_id_reads_tv_results() {
        let found: FindResponse = serde_json::from_value(serde_json::json!({
            "movie_results": [{"id": 603, "title": "The Matrix"}],
            "tv_results": [{"id": 1396, "name": "Breaking Bad"}]
        }))
        .unwrap();
        assert_eq!(movie_from_find(&found), Some(603));
        assert_eq!(tv_from_find(&found), Some(1396));

        // The classic bug: a show answered only under tv_results must not be lost by the movie
        // helper, and a movie must not be misread by the series helper.
        let shows_only: FindResponse = serde_json::from_value(serde_json::json!({
            "movie_results": [],
            "tv_results": [{"id": 1396, "name": "Breaking Bad"}]
        }))
        .unwrap();
        assert_eq!(movie_from_find(&shows_only), None);
        assert_eq!(tv_from_find(&shows_only), Some(1396));
    }

    #[test]
    fn direct_tmdb_ids_need_no_request() {
        let c = client("key");
        assert_eq!(c.movie_id("tmdb:603:1:2").unwrap(), Some(603));
        assert_eq!(c.series_id("1396").unwrap(), Some(1396));
        assert_eq!(c.movie_id("tmdb603").unwrap(), Some(603));
        assert_eq!(c.movie_id("not-an-id").unwrap(), None);
        assert!(c.cache.lock().unwrap().is_empty());
    }

    #[test]
    fn an_unconfigured_client_makes_no_request() {
        let c = client("");
        assert!(!c.configured());
        assert!(matches!(c.movie_details(603), Err(Error::NoAddon)));
        assert!(matches!(c.tv_details(1396), Err(Error::NoAddon)));
        assert!(matches!(c.series_id("tt0944947"), Err(Error::NoAddon)));
        assert!(matches!(c.movie_external_ids(603), Err(Error::NoAddon)));
        assert!(matches!(c.person_career(638), Err(Error::NoAddon)));
        assert!(c.cache.lock().unwrap().is_empty());
        assert!(c.flights.lock().unwrap().is_empty());
    }

    #[test]
    fn external_ids_returns_a_trimmed_imdb_id() {
        let response: ExternalIdsResponse = serde_json::from_value(serde_json::json!({
            "imdb_id": " tt10872600 "
        }))
        .unwrap();
        assert_eq!(trimmed_imdb_id(&response).as_deref(), Some("tt10872600"));

        let none: ExternalIdsResponse = serde_json::from_value(serde_json::json!({
            "imdb_id": "not-an-imdb-id"
        }))
        .unwrap();
        assert_eq!(trimmed_imdb_id(&none), None);
    }

    #[test]
    fn cache_serves_the_second_call_without_a_second_fetch() {
        let cache = ResponseCache::default();
        let flights = Flights::default();
        let key = ("test-key".to_string(), "/movie/603".to_string());
        let calls = AtomicUsize::new(0);
        let first = cached_json(&cache, &flights, key.clone(), || {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({"id": 603}))
        })
        .unwrap();
        let second = cached_json(&cache, &flights, key, || {
            panic!("a fresh cached response must not trigger another fetch")
        })
        .unwrap();
        assert_eq!(first, second);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_misses_share_one_fetch() {
        let cache = ResponseCache::default();
        let flights = Flights::default();
        let calls = AtomicUsize::new(0);
        let key = ("test-key".to_string(), "/movie/603".to_string());
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let (cache, flights, calls, key, barrier) =
                    (&cache, &flights, &calls, &key, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    let value = cached_json(cache, flights, key.clone(), || {
                        calls.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(serde_json::json!({"id": 603}))
                    })
                    .unwrap();
                    assert_eq!(value["id"], 603);
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
