# TMDB enrichment client (Nuvio Native, fork-owned)

This document describes the TMDB client in `plx_stremio` (`rust-modules/stremio/src/addons_tmdb.rs`).
The in-process PMS shim uses it to enrich the rows it builds: cast on detail rows, a "More Like
This" hub, the title logo (`clearLogo`), clickable person pages with biography and filmography,
trailers, and franchise shelves. This is Nuvio-only — upstream PlxNative talks to Plex, not TMDB.

## Public API (the shim codes against exactly this)

```rust
pub struct TmdbClient { /* Clone, Debug (redacted), Default */ }

impl TmdbClient {
    pub fn new() -> Self;                                   // option_env!("TMDB_API_KEY") -> std::env; blank/unset => configured()==false
    pub fn from_env() -> Option<Self>;                      // the old fork's constructor: None when unconfigured
    pub fn configured(&self) -> bool;
    pub fn movie_details(&self, id: u64) -> Result<TmdbMovieDetails>;
    pub fn tv_details(&self, id: u64) -> Result<TmdbShowDetails>;
    pub fn movie_id(&self, id: &str) -> Result<Option<u64>>;
    pub fn series_id(&self, id: &str) -> Result<Option<u64>>;
    pub fn movie_external_ids(&self, id: u64) -> Result<Option<String>>;
    pub fn external_ids(&self, id: u64) -> Result<Option<String>>;  // old alias
    pub fn tv_external_ids(&self, id: u64) -> Result<Option<String>>;
    pub fn credits(&self, movie_id: u64) -> Result<Vec<TmdbCast>>;
    pub fn recommendations(&self, movie_id: u64) -> Result<Vec<TmdbMovie>>;
    pub fn videos(&self, movie_id: u64) -> Result<Vec<TmdbVideo>>;
    pub fn person_details(&self, id: u64) -> Result<TmdbPerson>;
    pub fn person_credits(&self, id: u64) -> Result<Vec<TmdbCredit>>;  // cast-only filmography
    pub fn person_career(&self, id: u64) -> Result<TmdbCombinedCredits>;
    pub fn collection_details(&self, id: u64) -> Result<TmdbCollection>;
}
```

`Result`/`Error` come from `plx_stremio::model`. Every network method returns `Err(Error::NoAddon)`
on an unconfigured client **before** touching the HTTP door, so an unset key makes no request.
`Debug` on the client prints only `configured`, never the key.

### Types

All public serde types derive `Clone, Debug, Default, Serialize, Deserialize`, carry
`#[serde(default)]` at the container level, and have all fields `pub`, so a missing field never
fails a whole response. The fields are exactly:

- `TmdbCast { id, name, character, profile_path }` — the Role array; `id` is the synthetic person key.
- `TmdbCredit { id, name, title, overview, poster_path, backdrop_path, release_date, first_air_date, character, job, department, profile_path, media_type }` — one combined-credit row. `TmdbCredit::movie()` / `::show()` turn it into the WORK the person appeared in, so a filmography row shows that title's poster, synopsis and year rather than the person's face.
- `TmdbGenre { id, name }`, `TmdbCollectionRef { id, name }`, `TmdbMovie { id, title, overview, release_date, runtime, backdrop_path, poster_path, genres, belongs_to_collection }`, `TmdbCollection { id, name, overview, poster_path, backdrop_path, parts }`.
- `TmdbShow { id, name, overview, first_air_date, backdrop_path, poster_path, genres }` — a show spells a movie's `title`/`release_date` as `name`/`first_air_date`.
- `TmdbPerson { id, name, biography, birthday, deathday, place_of_birth, known_for_department, profile_path }`.
- `TmdbVideo { id, key, site, name, type_ }` (`type_` is the wire `type`), `TmdbVideos { results }`.
- `TmdbCredits { cast: Vec<TmdbCast>, crew: Vec<TmdbCredit> }` — cast rows carry the character,
  crew rows carry department/job for the filmography's tabs.
- `TmdbLogo { file_path, iso_639_1 }`, `TmdbImages { logos }`, `TmdbRecommendations { results }`, `TmdbShowRecommendations { results }`.
- `TmdbMovieDetails { id, title, release_date, runtime, backdrop_path, belongs_to_collection, credits, recommendations, images, videos }` — `images`/`videos` are `Option<…>`.
- `TmdbShowDetails { id, name, first_air_date, backdrop_path, credits, recommendations, images, videos }` — same, with show-shaped recommendations.
- `TmdbCombinedCredits { cast: Vec<TmdbCredit>, crew: Vec<TmdbCredit> }`.

## One request per title

`movie_details` and `tv_details` each fetch once:
`/movie|tv/{id}?append_to_response=credits,recommendations,images,videos`. Do not add separate
`/credits`, `/recommendations`, `/images` or `/videos` calls — trailers ride in the same append and
are the only source a synthetic related-show key has. `/collection/{id}` is the one extra hop, for
a film that belongs to a franchise. `/person/{id}` and `/person/{id}/combined_credits` are the two
requests behind a person's header and filmography (they share the cache).

Responses are cached in memory for **15 minutes**, keyed by `(api_key, path)`. Clones of a client
share one cache. The cache is bounded to 512 entries; when full it drops expired entries, then
clears wholesale — a bound on work/memory, never on reachable content.

## ID resolution: `movie_results` vs `tv_results`

`direct_tmdb_id` resolves `tmdb:123`, `123` and `tmdb603` locally with no request. IMDb `tt…` ids
go through `GET /find/{id}?external_source=imdb_id`, cached under the same key for both helpers
(one request serves both). `movie_id` reads `movie_results`; `series_id` reads `tv_results` from
that same answer. Using `movie_id` for a series returns `None` and silently drops the cast, so
always pick the helper that matches the kind. `crate::addons::content_level_id` first strips
episode suffixes (`tt0944947:1:2` → `tt0944947`). The shim checks `Meta::moviedb_id` (Cinemeta's
`moviedb_id`) before either helper, so a Cinemeta title never reaches `/find`.

`movie_external_ids` / `tv_external_ids` read `/movie|tv/{id}/external_ids`, trim the `imdb_id`,
and keep it only when it starts with `tt`.

## Where TMDB is allowed (owner's rule)

TMDB is **only** for the detail page and its children: cast, title logo (`clearLogo`), landscape
`art`, trailers, More Like This, and franchise collections. It is never used while building a hub.

A hub row's poster is **BetterPosters** by IMDb id, with the addon's own (Cinemeta) poster as the
**strict fallback**. No TMDB image is ever used for a hub.

Home's Cinemeta catalogs carry `tt…` ids and need no resolution. The extra catalogs in Recommended
("Streaming Catalogs" — Apple TV+, Netflix, HBO Max…) carry `tmdb:<n>` ids. Per the owner's decision,
`poster_url` resolves those to IMDb **once** with `imdb_base`: the one redb store is checked first,
and only a cold miss calls TMDB `/external_ids`. Positive **and negative** answers are cached under
`IMDB_TTL`, and `TmdbClient`'s keyed single-flight makes concurrent cold hub fills share the one
request. Every later read comes from redb. An id that resolves to no IMDb id keeps the addon poster.

The addon's `logo` is published as the row's `clearLogo`, and `UltraBlurColors` is derived from the
poster actually chosen.

## Filmography

The person page's cast/crew rows are built from `TmdbCredit::movie()` / `::show()` and ordered
newest first, undated last. The rows are remembered in the shim so the page's batched
`/library/metadata/{csv}` caption read is answered locally. `person_credits()` supplies the
cast-only filmography the `/library/people/{id}/media` route uses.

## Single-flight choice

This fork has **no** `plx_base::flight`, so `addons_tmdb.rs` implements a tiny local per-key
single-flight (a `Mutex<HashMap<key, Arc<Flight>>>` of `Condvar` gates) inside the module. A detail
prefetch and the click it warmed miss the cache together; the second waits for the first download
instead of issuing a duplicate request. On error the key is released so a waiter retries rather
than inheriting a failure. The `concurrent_misses_share_one_fetch` test proves one fetch for four
simultaneous misses.

## Build wiring

`TMDB_API_KEY` is injected at compile time (`option_env!("TMDB_API_KEY")`) with a runtime
`std::env` fallback. The Makefile reads it from the gitignored
`rust-modules/stremio/nuvio.local.env`, passes it to cargo via `NUVIO_ENV_VARS`/`TELEMETRY_ENV`,
and folds its **hash** into `TELEMETRY_CFG` (never writing the value to a tracked file). When the
key is unset the hash contribution is empty, so the build stamp — and the binary's behavior — is
byte-for-byte what it was without this feature. Never log the key; `Debug` on `TmdbClient` is
redacted and `addon_http` logs no URLs.

## Tests

`cargo test -p plx_stremio --lib addons_tmdb` (no network): `moviedb_id` number/string decoding,
`movie_results` vs `tv_results` extraction from fake JSON, direct ids and unconfigured clients
making no request, trimmed `external_ids`, cache reuse without a second fetch, and single-flight
sharing.
