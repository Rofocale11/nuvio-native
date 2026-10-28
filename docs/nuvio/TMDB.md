# NuvioNative TMDB enrichment

The shim enriches the Plex rows it builds with TMDB data: cast on detail rows, a "More Like This"
hub of recommendations, the title logo (`clearLogo`) the heroes draw, a person page for a
clickable headshot, and — for a film in a franchise — a collection shelf of the other films in it.
Read with `ARCHITECTURE.md` and `ID-MANAGEMENT.md` (the synthetic keys described here).

## One request per title

Both kinds use a **single HTTP request** with
`append_to_response=credits,recommendations,images`, so cast, related titles and the title logo
arrive together. Do not add separate `/credits`, `/recommendations` or `/images` calls.

```rust
pub fn movie_details(&self, movie_id: u64) -> Result<TmdbMovieDetails> {
    self.get(&format!("/movie/{movie_id}?append_to_response=credits,recommendations,images"))
}

pub fn tv_details(&self, tv_id: u64) -> Result<TmdbShowDetails> {
    self.get(&format!("/tv/{tv_id}?append_to_response=credits,recommendations,images"))
}
```

- The only other requests are a movie's `/movie/{id}/videos` (trailer thumbnail) and, for a film in
  a franchise, `/collection/{id}`. The first is spawned in parallel with the details call; the
  second is the only one that adds latency to a detail page, and only for a film that has a
  franchise at all. TV shows take their trailer from Cinemeta, so `tv_details` is genuinely one
  request.
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

- Using `movie_id()` for a series returns `None`, which silently drops the cast — always pick the
  helper that matches the kind.
- `content_level_id()` strips episode suffixes first, so an episode id resolves to its series.

## Field mapping

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

## Cast & Crew → the person page (filmography)

A headshot on a detail page is clickable, and what it opens is the app's **person page** — the same
screen a real Plex server's cast row opens, driven by the same `/library/people/{personId}/media`
fetch.

- **The link is `Role[].id`.** `tmdb_role` sets it to `TMDB_PERSON_KEY_BASE + cast.id`, so the app's
  cast shelf (`screens::detail::cast`) needs no TMDB knowledge at all: `Cast::person_key` returns
  that number and the Person store addresses `/library/people/{key}/media` with it. `tagKey` stays
  empty — it is plex.tv's global person guid, and TMDB has none.
- **The route** answers `MediaContainer.Metadata[]` of ordinary movie/show rows. `PersonStore`
  splits them into its Movies and Shows shelves **by each row's own `type`**, so a film must be
  tagged `movie` and a show `show` — a mismatch lands on the wrong shelf.
- **One request.** `person_credits` is `GET /person/{id}/combined_credits`, which merges films and
  shows into one array and states the kind per row in `media_type`. Do not add a `/person/{id}`
  call for the name or portrait: the page's header is drawn from the `Role[]` entry that opened it.
- **The rows come from TMDB alone** (`tmdb_fallback_row` / `tmdb_show_fallback_row`), not from
  `meta()`. A recommendation row prefers the addon's own metadata, but a filmography has dozens of
  credits and one `meta()` per credit would be one HTTP request each.
- **The character name rides on the row's `Role[]`**, keyed by the person's synthetic id, because
  `person::roles_from` joins on exactly that to caption each tile. It is a fact about the CREDIT, so
  dropping it loses the only place "Neo" appears on that page.
- **Newest first, undated last** — the page sorts its own list the same way, so a stable server
  order keeps the shelves from reshuffling between fetches.
- **A person with no TMDB id opens nothing.** `tmdb_role` omits `id` rather than emitting the bare
  base, which would address person `0`.
- **A failed or unconfigured lookup is an empty container**, matching the other TMDB routes: the
  page then draws its header (from the `Role[]` entry) with no shelves, not an error screen.

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
