//! TMDB's movie and TV metadata endpoints, using the Stremio crate's existing HTTP transport.

use crate::addon_http;
use crate::model::{Error, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const API_BASE: &str = "https://api.themoviedb.org/3";
const CACHE_TTL: Duration = Duration::from_secs(15 * 60);

type ResponseCache = Mutex<HashMap<(String, String), (Instant, Value)>>;

fn shared_cache() -> Arc<ResponseCache> {
    static CACHE: OnceLock<Arc<ResponseCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

#[derive(Clone, Debug)]
pub struct TmdbClient {
    api_key: String,
    cache: Arc<ResponseCache>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbCast {
    pub name: String,
    pub character: String,
    pub profile_path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbMovie {
    pub id: u64,
    pub title: String,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub release_date: Option<String>,
    pub runtime: Option<u32>,
    pub genres: Vec<TmdbGenre>,
}

/// A TV show, as TMDB spells it: `name`/`first_air_date` where a movie has `title`/`release_date`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbShow {
    pub id: u64,
    pub name: String,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub first_air_date: Option<String>,
    pub genres: Vec<TmdbGenre>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbGenre {
    pub id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbVideo {
    pub id: String,
    pub key: String,
    pub name: String,
    pub site: String,
    #[serde(rename = "type")]
    pub type_: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbCredits {
    pub cast: Vec<TmdbCast>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbRecommendations {
    pub results: Vec<TmdbMovie>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbMovieDetails {
    #[serde(flatten)]
    pub movie: TmdbMovie,
    pub credits: TmdbCredits,
    pub recommendations: TmdbRecommendations,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbShowRecommendations {
    pub results: Vec<TmdbShow>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct TmdbShowDetails {
    #[serde(flatten)]
    pub show: TmdbShow,
    pub credits: TmdbCredits,
    pub recommendations: TmdbShowRecommendations,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct VideosResponse {
    results: Vec<TmdbVideo>,
}

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

impl TmdbClient {
    /// Uses the compiled-in or runtime `TMDB_API_KEY`; an unset or blank key disables TMDB lookups.
    pub fn from_env() -> Option<Self> {
        let api_key = option_env!("TMDB_API_KEY")
            .map(|s| s.to_string())
            .or_else(|| std::env::var("TMDB_API_KEY").ok())?;
        let api_key = api_key.trim();
        (!api_key.is_empty()).then(|| Self {
            api_key: api_key.to_string(),
            cache: shared_cache(),
        })
    }

    fn url(&self, path: &str) -> String {
        let separator = if path.contains('?') { '&' } else { '?' };
        format!(
            "{API_BASE}{path}{separator}api_key={}",
            addon_http::encode_component(&self.api_key)
        )
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let cache_key = (self.api_key.clone(), path.to_string());
        let body = cached_json(&self.cache, cache_key, || {
            addon_http::get_json(&self.url(path), addon_http::ADDON)
        })?;
        serde_json::from_value(body).map_err(|error| Error::Parse(error.to_string()))
    }

    /// Resolves a Stremio movie id (`tmdb:123`, `123`, or an IMDb `tt…` id) to TMDB's numeric id.
    pub fn movie_id(&self, id: &str) -> Result<Option<u64>> {
        let id = crate::addons::content_level_id(id);
        if let Some(value) = id
            .strip_prefix("tmdb:")
            .or_else(|| id.strip_prefix("TMDB:"))
        {
            return Ok(value
                .split(':')
                .next()
                .and_then(|v| v.parse().ok())
                .filter(|id| *id > 0));
        }
        if let Some(value) = id.strip_prefix("tmdb") {
            let value = value.strip_prefix(':').unwrap_or(value);
            if let Ok(id) = value.parse() {
                return Ok((id > 0).then_some(id));
            }
        }
        if let Ok(id) = id.parse() {
            return Ok((id > 0).then_some(id));
        }
        if id.starts_with("tt") {
            let path = format!(
                "/find/{}?external_source=imdb_id&api_key={}",
                addon_http::encode_component(&id),
                addon_http::encode_component(&self.api_key)
            );
            let cache_path = format!("/find/{id}?external_source=imdb_id");
            let body = cached_json(&self.cache, (self.api_key.clone(), cache_path), || {
                addon_http::get_json(&format!("{API_BASE}{path}"), addon_http::ADDON)
            })?;
            let found: FindResponse =
                serde_json::from_value(body).map_err(|error| Error::Parse(error.to_string()))?;
            return Ok(found
                .movie_results
                .first()
                .map(|movie| movie.id)
                .filter(|id| *id > 0));
        }
        Ok(None)
    }

    pub fn credits(&self, movie_id: u64) -> Result<Vec<TmdbCast>> {
        let response: TmdbCredits = self.get(&format!("/movie/{movie_id}/credits"))?;
        Ok(response.cast)
    }

    pub fn recommendations(&self, movie_id: u64) -> Result<Vec<TmdbMovie>> {
        let response: TmdbRecommendations =
            self.get(&format!("/movie/{movie_id}/recommendations"))?;
        Ok(response.results)
    }

    pub fn movie_details(&self, movie_id: u64) -> Result<TmdbMovieDetails> {
        self.get(&format!(
            "/movie/{movie_id}?append_to_response=credits,recommendations"
        ))
    }

    /// Resolves a Stremio series id (`tmdb:123`, `123`, or an IMDb `tt…` id) to TMDB's numeric id.
    /// `/find` answers a show's IMDb id under `tv_results`, not `movie_results`.
    pub fn series_id(&self, id: &str) -> Result<Option<u64>> {
        let id = crate::addons::content_level_id(id);
        if !id.starts_with("tt") {
            return self.movie_id(&id);
        }
        let path = format!(
            "/find/{}?external_source=imdb_id&api_key={}",
            addon_http::encode_component(&id),
            addon_http::encode_component(&self.api_key)
        );
        let cache_path = format!("/find/{id}?external_source=imdb_id");
        let body = cached_json(&self.cache, (self.api_key.clone(), cache_path), || {
            addon_http::get_json(&format!("{API_BASE}{path}"), addon_http::ADDON)
        })?;
        let found: FindResponse =
            serde_json::from_value(body).map_err(|error| Error::Parse(error.to_string()))?;
        Ok(found
            .tv_results
            .first()
            .map(|show| show.id)
            .filter(|id| *id > 0))
    }

    pub fn tv_details(&self, tv_id: u64) -> Result<TmdbShowDetails> {
        self.get(&format!(
            "/tv/{tv_id}?append_to_response=credits,recommendations"
        ))
    }

    pub fn movie_external_ids(&self, movie_id: u64) -> Result<Option<String>> {
        let response: ExternalIdsResponse = self.get(&format!("/movie/{movie_id}/external_ids"))?;
        Ok(response
            .imdb_id
            .map(|id| id.trim().to_string())
            .filter(|id| id.starts_with("tt")))
    }

    pub fn external_ids(&self, movie_id: u64) -> Result<Option<String>> {
        self.movie_external_ids(movie_id)
    }

    pub fn tv_external_ids(&self, tv_id: u64) -> Result<Option<String>> {
        let response: ExternalIdsResponse = self.get(&format!("/tv/{tv_id}/external_ids"))?;
        Ok(response
            .imdb_id
            .map(|id| id.trim().to_string())
            .filter(|id| id.starts_with("tt")))
    }

    pub fn videos(&self, movie_id: u64) -> Result<Vec<TmdbVideo>> {
        let response: VideosResponse = self.get(&format!("/movie/{movie_id}/videos"))?;
        Ok(response.results)
    }
}

fn cached_json<F>(cache: &ResponseCache, key: (String, String), fetch: F) -> Result<Value>
where
    F: FnOnce() -> Result<Value>,
{
    if let Some((at, value)) = cache
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get(&key)
    {
        if at.elapsed() < CACHE_TTL {
            return Ok(value.clone());
        }
    }

    let value = fetch()?;
    let mut cache = cache.lock().unwrap_or_else(|error| error.into_inner());
    if cache.len() >= 512 {
        cache.retain(|_, (at, _)| at.elapsed() < CACHE_TTL);
        if cache.len() >= 512 {
            cache.clear();
        }
    }
    cache.insert(key, (Instant::now(), value.clone()));
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_tmdb_movie_endpoint_shapes() {
        let details: TmdbMovieDetails = serde_json::from_value(serde_json::json!({
            "id": 123,
            "title": "Movie",
            "overview": "Overview",
            "backdrop_path": "/backdrop.jpg",
            "release_date": "2024-01-02",
            "runtime": 123,
            "genres": [{"id": 18, "name": "Drama"}],
            "credits": {
                "cast": [{"name": "Actor", "character": "Role", "profile_path": "/actor.jpg"}]
            },
            "recommendations": {
                "results": [{"id": 603, "title": "Movie", "poster_path": "/poster.jpg"}]
            }
        }))
        .unwrap();
        assert_eq!(details.movie.id, 123);
        assert_eq!(details.movie.runtime, Some(123));
        assert_eq!(details.movie.genres[0].name, "Drama");
        assert_eq!(details.credits.cast[0].name, "Actor");
        assert_eq!(details.credits.cast[0].character, "Role");
        assert_eq!(details.recommendations.results[0].id, 603);

        let videos: VideosResponse = serde_json::from_value(serde_json::json!({
            "results": [{"id": "video-id", "key": "youtube-id", "site": "YouTube", "type": "Trailer"}]
        }))
        .unwrap();
        assert_eq!(videos.results[0].type_, "Trailer");

        let external_ids: ExternalIdsResponse = serde_json::from_value(serde_json::json!({
            "imdb_id": "tt10872600"
        }))
        .unwrap();
        assert_eq!(external_ids.imdb_id.as_deref(), Some("tt10872600"));
    }

    #[test]
    fn builds_authenticated_urls_and_resolves_direct_tmdb_ids() {
        let client = TmdbClient {
            api_key: "key +".into(),
            cache: shared_cache(),
        };
        assert_eq!(
            client.url("/movie/603/credits"),
            "https://api.themoviedb.org/3/movie/603/credits?api_key=key%20%2B"
        );
        assert_eq!(
            client.url("/movie/603?append_to_response=credits,recommendations"),
            "https://api.themoviedb.org/3/movie/603?append_to_response=credits,recommendations&api_key=key%20%2B"
        );
        assert_eq!(
            client.url("/tv/1396?append_to_response=credits,recommendations"),
            "https://api.themoviedb.org/3/tv/1396?append_to_response=credits,recommendations&api_key=key%20%2B"
        );
        assert_eq!(client.movie_id("tmdb:603:1:2").unwrap(), Some(603));
        assert_eq!(client.movie_id("603").unwrap(), Some(603));
        assert_eq!(client.movie_id("not-a-movie-id").unwrap(), None);
    }

    #[test]
    fn decodes_tmdb_tv_endpoint_shapes() {
        let details: TmdbShowDetails = serde_json::from_value(serde_json::json!({
            "id": 1396,
            "name": "Breaking Bad",
            "overview": "Overview",
            "poster_path": "/poster.jpg",
            "first_air_date": "2008-01-20",
            "genres": [{"id": 18, "name": "Drama"}],
            "credits": {
                "cast": [{"name": "Actor", "character": "Role", "profile_path": "/actor.jpg"}]
            },
            "recommendations": {
                "results": [{"id": 60059, "name": "Better Call Saul", "poster_path": "/bcs.jpg"}]
            }
        }))
        .unwrap();
        assert_eq!(details.show.id, 1396);
        assert_eq!(details.show.name, "Breaking Bad");
        assert_eq!(details.show.first_air_date.as_deref(), Some("2008-01-20"));
        assert_eq!(details.show.genres[0].name, "Drama");
        assert_eq!(details.credits.cast[0].character, "Role");
        assert_eq!(details.recommendations.results[0].name, "Better Call Saul");
    }

    #[test]
    fn cached_json_reuses_a_fresh_response() {
        let cache = ResponseCache::default();
        let key = ("test-key".into(), "/movie/603/credits".into());
        let first = cached_json(&cache, key.clone(), || {
            Ok(serde_json::json!({"cast": [{"name": "Actor"}]}))
        })
        .unwrap();
        let second = cached_json(&cache, key, || {
            panic!("fresh cached response must not trigger another fetch")
        })
        .unwrap();
        assert_eq!(first, second);
    }
}
