# NuvioNative TMDB enrichment

The shim enriches the Plex rows it builds with TMDB data: cast on detail rows, a "More Like This"
hub of recommendations, the title logo (`clearLogo`) the heroes draw, a person page for a
clickable headshot, and — for a film in a franchise — a collection shelf of the other films in it.
Read with `ARCHITECTURE.md` and `ID-MANAGEMENT.md` (the synthetic keys described here).

## One request per title

Both kinds use a **single HTTP request** with
`append_to_response=credits,recommendations,images`, so cast, related titles and the title logo
arrive together. Do not add separate `/credits`, `/recommendations` or `/images` calls.

The IMDb→TMDB id resolution is a **second** request only when the meta does not already carry one:
Cinemeta returns `moviedb_id` (verified for movies and series), so `/find` is the fallback, not the
default. `DETAIL-LATENCY.md` records why that one round trip matters on the detail page's critical
path.

```rust
pub fn movie_details(&self, movie_id: u64) -> Result<TmdbMovieDetails> {
    self.get(&format!(
        "/movie/{movie_id}?append_to_response=credits,recommendations,images,videos"
    ))
}

pub fn tv_details(&self, tv_id: u64) -> Result<TmdbShowDetails> {
    self.get(&format!(
        "/tv/{tv_id}?append_to_response=credits,recommendations,images,videos"
    ))
}
```

- `videos` rides in the same `append_to_response`, so the details call also carries every
  trailer/teaser the title lists: `TmdbMovieDetails.videos` / `TmdbShowDetails.videos`
  (`TmdbVideos { results: Vec<TmdbVideo> }`). The detail shelf draws them all
  (`docs/nuvio/PLAYBACK.md`); a show's shelf — including a synthetic related-show key, which has
  no Cinemeta meta to read — comes from `tv_details` and nowhere else.
- The only other request is `/collection/{id}`, for a film in a franchise. It is the only one
  that adds latency to a detail page, and only for a film that has a franchise at all. The old
  separate `/movie/{id}/videos` hop is gone: the trailer list is part of the details call now, so
  `tv_details` is also genuinely one request.
- Responses are cached for 15 minutes by the shared `TmdbClient` cache, keyed by API key + path.
- `TMDB_API_KEY` is injected at compile time (`option_env!("TMDB_API_KEY")`, GitHub Secrets) with a
  runtime `std::env` fallback. An unset or blank key disables every TMDB lookup — the hub answers
  503 and rows keep their Cinemeta metadata — so local testing requires exporting the env var.

## ID resolution: `movie_results` vs `tv_results`

An IMDb id resolves differently per kind. `TmdbClient::movie_id()` reads `movie_results`;
`TmdbClient::series_id()` reads `tv_results` from the same `/find?external_source=imdb_id`
endpoint:

```rust
/// `/find` answers a show's IMDb id under `tv_results`, not `movie_results`.
pub fn series_id(&self, id: &str) -> Result<Option<u64>> {
    let id = crate::addons::content_level_id(id);   // tt0944947:1:2 → tt0944947
    if !id.starts_with("tt") {
        return self.movie_id(&id);                  // tmdb:123 / 123 need no /find
    }
    // … GET /find/{id}?external_source=imdb_id, then:
    Ok(found.tv_results.first().map(|show| show.id).filter(|id| *id > 0))
}
```

- The shim's `resolve_tmdb_movie_id` / `resolve_tmdb_series_id` check the `Meta`'s own
  `moviedb_id` before either helper, so a Cinemeta title never reaches `/find`.
- Using `movie_id()` for a series returns `None`, which silently drops the cast — always pick the
  helper that matches the kind.
- `content_level_id()` strips episode suffixes first, so an episode id resolves to its series.

## Field mapping

- **TMDB id:** Cinemeta's `moviedb_id` → `Meta::moviedb_id` (`moviedb_id` on the wire; absent from
  most other addons, hence the `/find` fallback). It is the first thing the id resolvers read.
- **Cast:** `credits.cast` → the Plex `Role` array (`tag`, `role`, `thumb`, `id`), capped at 10
  entries, for both `movie_details` and `tv_details`. `id` is the synthetic person key that makes
  the headshot clickable.
- **Movies:** `title` / `release_date` / `runtime` → `title` / `originallyAvailableAt` / `duration`.
- **TV Shows:** TMDB spells these `name` / `first_air_date`, and there is no runtime — rows use
  `DEFAULT_EPISODE_MS`. `TmdbShow` / `TmdbShowDetails` exist precisely because the field names
  differ.
- **Related Movies:** `recommendations.results` → the Plex `Hub` array, `type`/`hubIdentifier`
  `movie`. **Related TV Shows:** the same hub shape with `show`.
- **Franchises:** `belongs_to_collection` + `/collection/{id}` → a SECOND `Hub` in the same answer,
  `collection.related.{section}.{n}` (below).
- **Logos:** `images.logos` → the row's Plex `clearLogo`, the title treatment both heroes draw.
  Prefer the logo whose `iso_639_1` is `"en"`, else the first one with a `file_path`; the URL is
  `https://image.tmdb.org/t/p/w500{file_path}` (`tmdb_logo`). A title with no logo sends **no**
  `clearLogo` field at all, exactly as a real server omits it, so the hero falls back to its text
  title through its ordinary absent-logo path — do not substitute the poster or the title string.
- **Artwork is fetched by PATH, not by URL.** The app never reads an image URL out of a row: it asks
  the server for `/library/metadata/{rk}/thumb|art|clearLogo` through `/photo/:/transcode`
  (`app/adapters/poster.rs`'s `logo_key` always builds that path). A synthetic related key has no
  `Ident`, so the `photo` route resolves it through `tmdb_synthetic_row` when `row` answers `None` —
  one cached TMDB request, then the resolved CDN URL is proxied byte-for-byte. Without that fallback
  a Related title's logo (and its poster and backdrop) 404s even though the row carries the URL.
- A recommendation is rendered from the addon's own metadata when available
  (`meta("movie", "tmdb:<id>")` / `meta("series", "tmdb:<id>")`) and otherwise from TMDB, with a
  synthetic ratingKey.

## Library genres and sorts (the menu baseline)

The Library's Filter and Sort menus come from the shim, not from TMDB directly. They combine:

- **The catalog's own declarations** (its `extra` genres and native order), which are what the
  addon actually accepts;
- **a static TMDB baseline** (`TMDB_MOVIE_GENRES` / `TMDB_TV_GENRES` in `shim_pms.rs`) plus the
  Popularity / Rating / Release Date / Title sorts, so a catalog with no `genre` extra still has a
  usable menu.

A baseline genre is matched against the fetched rows' `Genre[]` in memory, with no per-item TMDB
request: `genre_key` folds case and punctuation and treats `Sci-Fi` and `Science Fiction` as one,
while compound names (`Action & Adventure`, `Sci-Fi & Fantasy`) match either half. Popularity is
the catalog's own order; Rating reads `imdb_rating`, Release Date reads `release_info`. A numeric
PMS tag id is resolved back to its display name before matching. Fetching is bounded to 20 pages,
so an in-memory filter cannot page forever.

## TV show limitations

- **A TMDB-only related show has no episodes.** TMDB recommendations carry no season/episode list,
  and the shim does not spend a second request fetching them. Such a show opens with full metadata
  (Cast, Summary, Genre, poster/backdrop/logo, air date) and its own related hub, but has **nothing
  to play**.
- **It becomes playable when an addon also has it.** `tmdb_show_recommendation_row` prefers
  `meta("series", "tmdb:<id>")`; when Cinemeta knows the show, the row is built from that `Meta` and
  the normal season/episode/stream path applies.
- Do not "fix" this by adding a `/tv/{id}/season/{n}` fetch to the hub: that is one extra request
  per recommendation and multiplies the hub's latency. Add it only for a show that must be playable
  rather than merely browsable.
- **A TMDB-only show is not in `keys.tsv`** — it is a pure arithmetic key, so anything that needs an
  `Ident` must special-case it the way `related_hub`, `/library/metadata/{rk}`, the `photo` route
  and `slot()` do.

## Cast & Crew → the person page (biography + filmography)

A headshot on a detail page is clickable, and what it opens is the app's **person page** — the same
screen a real Plex server's cast row opens. It reads three shim routes: `/media` for its two
shelves, and the two GLOBAL reads the page inherited from plex.tv Discover (the biography and the
department filmography) under the person's `tagKey`.

- **Two identities, both synthetic, and neither substitutes for the other.** `Role[].id` is
  `TMDB_PERSON_KEY_BASE + cast.id` and addresses `/library/people/{personId}/media`
  (`Cast::person_key`). `Role[].tagKey` is `nuvio-person-<TMDB person id>` and is the identity the
  person store asks for the biography and credits; on a real install it is plex.tv's person guid,
  and here the shim answers the same two shapes from TMDB.
- **`/library/people/{guid}` — the header.** `person_profile_page` reads TMDB `/person/{id}` and
  answers `MediaContainer.Metadata[0]` with `summary` (biography), `bornAt`, `diedAt`, `birthPlace`,
  `knownFor`, `thumb`, and `CreditType[]` built from the same career answer (cast first, then each
  crew department, most-credits-first). That is what draws the biography, the born/place line, the
  roles line and the `MORE` mark.
- **`/library/people/{guid}/credits` — the Filmography.** `person_career_page` reads
  `combined_credits` **both halves**: `cast` becomes the `actor` group, and `crew` is grouped by its
  own `department`. Directing/Writing/Production take Plex's "Director"/"Writer"/"Producer" names,
  and TMDB's catch-all `Crew` bucket becomes "Other" (a tab, never a role). A row's `role` is the
  character (cast) or the job (crew).
- **The credit's join key is `tmdb:<id>`.** It is exactly the tail of the shim row's
  `nuvio://movie|series/tmdb:<id>` guid, so `person::guid_tail` matches and the filmography marks
  credits the app holds — and OK on one opens that title.
- **The routes answer ordinary rows** (`/media`: `MediaContainer.Metadata[]`; both global reads:
  the Discover shapes above). `PersonStore` splits `/media` into Movies and Shows **by each row's
  own `type`**, so a film must be tagged `movie` and a show `show` — a mismatch lands on the wrong
  shelf.
- **Two TMDB requests per person, both cached.** `person_details` is `/person/{id}`; `person_career`
  is `/person/{id}/combined_credits` (the same request the cast-only `person_credits` makes for the
  shelves). The profile handler also needs the department counts, so it reads the career answer too
  — the shared response cache makes that the same request, not a third.
- **The rows come from TMDB alone** (`tmdb_fallback_row` / `tmdb_show_fallback_row`), not from
  `meta()`. A recommendation row prefers the addon's own metadata, but a filmography has dozens of
  credits and one `meta()` per credit would be one HTTP request each.
- **The character name rides on the row's `Role[]`**, keyed by the person's synthetic id, because
  `person::roles_from` joins on exactly that to caption each tile. It is a fact about the CREDIT, so
  dropping it loses the only place "Neo" appears on that page.
- **Newest first, undated last** — the page sorts its own list the same way, so a stable server
  order keeps the shelves from reshuffling between fetches.
- **A person with no TMDB id opens nothing.** `tmdb_role` omits `id`/`tagKey` rather than emitting
  the bare base, which would address person `0`.
- **A failed or unconfigured lookup is an empty container**, matching the other TMDB routes: the
  page then draws what it was handed and no biography, shelves or filmography, not an error screen.
  An unknown or empty guid is that same empty answer, which the store settles on.

## Movie collections (franchises)

A film that belongs to a franchise gets a collection shelf on its Detail page, headed by the
franchise's name and listing the OTHER films in it.

```rust
/// `/movie/{id}` names the franchise; `/collection/{id}` lists it.
pub struct TmdbCollectionRef { pub id: u64, pub name: String }      // belongs_to_collection
pub struct TmdbCollection {
    pub id: u64, pub name: String, pub overview: Option<String>,
    pub poster_path: Option<String>, pub backdrop_path: Option<String>,
    pub parts: Vec<TmdbMovie>,
}
```

- **The source is `TmdbMovie::belongs_to_collection`** (`None` for a standalone film), and the
  shelf's films are `TmdbClient::collection_details(id)` → `GET /collection/{id}`. One extra request
  per franchise film's detail page, through the same 15-minute cache.
- **It is a `collection.related.{section}.{n}` hub**, not a second recommendation hub. That is the
  identifier the app's `/related` reader splits a member's collection shelf out of
  (`plx_data::metadata::collection_shelf`), and it is what makes the shelf draw above Related under
  its own linked heading instead of folding the films into "More Like This". The answer is
  `"Hub": [<More Like This>, <franchise>]`, and the shim adds no third hub.
- **The rows are ordinary recommendation rows** (`tmdb_recommendation_row`), so each film is a
  clickable, playable key. The page's own film is left out — it IS the page — and a franchise whose
  only member is the film on screen gets no shelf at all.
- **The heading opens the collection page.** The hub's `key` names the collection by `tagId` (its
  TMDB id), which is the identity the app's collection store resolves, and the shelf's films are
  kept in `Shim::collections` so the page answers from what the detail page already fetched. The
  three routes that page needs:
  - `/library/sections/{s}/collections` — the `tagId` resolution, as a collection row whose `index`
    is the TMDB collection id.
  - `/library/metadata/{collection_rk}` — the header (`collection_row`), by the synthetic key
    `4_000_000_000 + TMDB_COLLECTION_ID`.
  - `/library/collections/{rk}/children` — the films, in TMDB's release order, paged like any other
    collection listing.
- **The shim has no library scan**, so the section listing carries only franchises it has served
  (bounded at 256, cleared whole when full), ordered by id so a page's `start` means the same rows
  twice. It always includes the one whose heading was pressed, since that shelf came from this same
  process — but a franchise never shown is not listed, and
  `/library/collections/{rk}/children` for an unknown key answers 404 rather than inventing one.
- **TV shows have no collection shelf.** TMDB's `belongs_to_collection` is a movie field.
