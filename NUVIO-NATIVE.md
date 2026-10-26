NuvioNative is a fork of PlxNative (GPL-3.0-or-later, upstream GLinnik21/plx-native@4e454ea) that
browses and plays content from Stremio addons — the same addons a user configured in Nuvio — with
PlxNative's GPU-rendered UI and native Starfish/ACB video pipeline.

## Findings that drive the design

- **Nuvio has no reusable backend service.** NuvioTVSmart (the code behind the webOS app
  `space.nuvio.webos`) is a browser app: addon management, catalogs, metadata, stream discovery and
  playback all run as JS inside the WebView. Its webOS JS services (`services/webos`) only proxy
  media/images/subtitles for the WebView. Addon config lives in WebView localStorage (unreadable
  from another app in Developer Mode) and is synced to the user's **Nuvio account** (Supabase).
  So "reusing Nuvio's backend" means: speak the same Stremio addon protocol, and import the same
  account-synced addon list.

## Shape

```
 ┌──────────────── one native process (the .ipk) ────────────────┐
 │  PlxNative UI / stores / player   (unchanged where possible)   │
 │          │  PMS REST over http://127.0.0.1:<port>               │
 │          ▼                                                      │
 │  plx_pmsshim  — in-process HTTP server, PMS dialect             │
 │          │  Rust API                                             │
 │          ▼                                                      │
 │  plx_stremio  — addon protocol, Nuvio import, stream classify   │
 └──────────┼──────────────────────────────────────────────────────┘
            ▼ HTTPS (libcurl)
     Stremio addons (Cinemeta, Torrentio, AIOStreams, …), Nuvio account backend
```

- Catalogs → PMS library sections and Home hubs. Metas → movie/show/season/episode items.
  Search → `/hubs/search`. Streams → the item's `Media[]` list (one Media per stream, with
  resolution/codec/HDR/size/bitrate from `plx_stremio::classify`). Each Media also carries
  `addon_name`, the `Addon::label()` of the addon that returned it (empty only on the
  "Finding streams…" placeholder). Part URLs redirect (or proxy, when headers are needed) to the
  stream's real URL. The decision endpoint waits for discovery/probing and refuses media that
  cannot direct play; the native route uses its probed fields.
- **The stream pickers are two-level menus, not flat lists** — the player's `…` popover, the
  Detail page's **Streams** pill and the Home hero's Play all open the same panel and behave
  identically:
  1. **the addons that answered for the item**, in ranked order (the addon holding the best stream
     leads), each stating how many streams it offered;
  2. **one addon's streams**, best first, headed by that addon's name — the page's rows omit the
     addon from their fact line, because the band already says it.
  Picking a stream commits it; BACK walks one level at a time before the panel closes. The addon
  label is the shim's own `addon_name` (the `Media[]` field), which `/nuvio/streams` returns as
  `addon`. Grouping rules: `plx_appkit::source_list::addon_groups` (the route's ranked
  `(index, label, addon_name)` list) and `addon_pages` (the panel's own `StreamChoice[]`, which
  also carries the stream's technical facts). `route::AVAILABLE_STREAMS` is written only through
  `route::set_available_streams`, so a revision stamp tells the player panel's per-tick poll that
  nothing moved.
- **A press opens the picker; a pick IS the play.** Every surface that used to start a file
  directly raises this panel first, and the commit starts the thing the panel was about:
  * **Detail page**: the hero's **Streams** pill lists the page's own item; an **episode's still**
    opens the same panel for THAT episode (`ContentPanel::AltSources { item }` names the subject,
    so the show page's own item is overridden) and the pick starts the episode;
  * **Home**: the hero's **Play** raises it for the billboard's film (`item` names it, since Home
    has no page item of its own; the panel opens centred, `item_menu::fallback_anchor`). A show or
    season hero still plays its on-deck episode — it has no stream list of its own;
  * **a movie page's pill** plays the movie on the pick, exactly as its Play button does. The pick
    used to only re-read the item, which left the film sitting on its page with the new stream
    selected and nothing playing.

  A picker on any other source (an ordinary Plex server) is not offered at all: it has no stream
  list, so those presses play or navigate exactly as before.
- **A pick never waits for the addons to finish.** The panel polls `/nuvio/streams` until every
  addon has reported (`complete`), and the press is taken the whole time — `stream_selecting` is
  the only latch (one commit at a time), never the in-flight list read. On the shim's side a pick
  also ends the wait: `Shim::ranked`'s `chosen` argument stops the grace/deadline wait as soon as
  `/nuvio/select` has recorded a stream, so the `/decision` and the `Part` that follow a pick
  answer immediately instead of sitting out `stream_grace` — or, for a picked entry the local
  playable test does not recognise, the whole 60 s deadline. The Detail page's own fetch and
  `/nuvio/streams` pass `chosen: false`: their list keeps filling progressively.
- **The player's Source page carries no quality ladder.** Under the shim a playback's quality is a
  property of the STREAM picked — the shim resolves a concrete file and the route plays it — so a
  list of bitrate ceilings beside the stream list would be a second answer to the same question.
  The ladder exists only in Settings > Playback, where it is a policy for ordinary Plex servers.
  (It was drawn in the player popover until 2026-10-26.) The failure read-out's `Change Source`
  button opens this picker and is offered under Force Direct Play too: Force overrides every
  quality RUNG, not which file plays.
- The stream pick itself is `/nuvio/select?index=` on the shim's own ranked index, then a fresh
  resolve of the same item; the Media list a `/decision` returns is selected-first and must not be
  confused with it. `AppMsg::NuvioStreamSelected` is what a successful commit delivers back to the
  page that opened the panel — the panel names no item, so Detail remembers which episode (or,
  for the movie pill, none) its picker was about, and Home remembers which hero item.
- Addon configuration: a separate opaque JSON record in the app's private DB8 kind,
  loaded/saved through the authenticated storage helper. Stable/debug/nightly own
  separate records. The helper verifies readback before import files are removed.
- Torrents (`infoHash`): played through a Stremio-compatible streaming server URL
  (`<server>/<infoHash>/<fileIdx>`) when one is configured; debrid-resolved HTTP links from
  Torrentio/AIOStreams play directly. Local server scanning is off by default and
  requires `enableTorrents`; a configured `torrentServer` can be used directly.
- Release discovery permits plaintext credentials for the registered in-process shim
  only when its origin, token and machine identity match the boot registration and
  its address is literal loopback. Identity probing and authenticated library admission
  still run. Remote HTTP resources do not gain this exception.
- Rating-source preference lives in the native canonical session record; the shim
  retains supplied IMDb and Rotten Tomatoes scores so cached details can switch
  providers. It does not replace a missing RT detail score with IMDb. BetterPosters
  image URLs request RT directly; these baked-in poster scores are independent of
  the native detail-score preference. Original artwork is retained as a fetch fallback.
- My List uses Nuvio's `sync_pull_library`, `sync_push_library_items` and
  `sync_delete_library_items` RPCs. Writes contain one title and the active profile,
  never a whole-list replacement. The account context serializes profile changes and
  mutations, caches reads for 60 seconds and confirms each write with a fresh pull.
  The selected profile is retained with the backend session. The UI uses
  `/nuvio/library`, runs requests on a worker and refreshes Home after a confirmed
  reply; the shelf pages through `/hubs/nuvio/list`.
- Watch state / progress: stored locally by the shim (the app already reports timeline to "the
  server"). Developer Mode's writable-directory fallback is `/tmp`, so this state
  and the key table are not yet durable across TV reboots.

## Crates

| crate | owner | depends on |
|---|---|---|
| `rust-modules/stremio` (`plx_stremio`) | backend | `plx_base`, `plx_net`, serde |
| `rust-modules/pmsshim` (`plx_pmsshim`) | shim | `plx_stremio`, `plx_base` |
| app wiring (boot, appinfo, settings) | lead/integration | all |


# PROJECT CONTEXT: Nuvio Native (PlxNative v0.8.1 + Stremio Shim)

This repository is a custom fork of **PlxNative** (an LG webOS native media client originally by RichiL10) combined with **Nuvio Native** (a Stremio addon bridge).

## 1. High-Level Architecture
*   **Frontend (UI):** PlxNative (Rust + custom UI). It expects a standard Plex Media Server API.
*   **The Bridge (`plx_pmsshim`):** A custom in-process HTTP server that runs on `127.0.0.1:32420`. It acts as a "Fake Plex Server". It intercepts Plex HTTP requests from the UI, fetches data from Stremio addons (Cinemeta, Torrentio, etc.), and translates the JSON into strict Plex JSON format.
*   **The Stremio Client (`plx_stremio`):** Handles communication with Stremio addons and TMDB.

## 2. Critical Technical Constraints (READ THIS BEFORE CODING)
*   **NO C-LIBRARIES OR JS ENGINES FOR THE TV BUILD:** The app compiles for an ancient LG TV ARM toolchain (`arm-webos-linux-gnueabi`). If you add crates like `innertube-rs`, `rusty_ytdl`, `rquickjs`, or `boa_engine` (which use C-code or require `libclang`/`bindgen`), the GitHub Actions TV build will fail. **Strictly use Pure Rust.**
*   **PLX `ratingKey` MUST BE NUMERIC:** PlxNative strictly parses `ratingKey` as an `i64`. If you pass a string like `"tmdb:603"`, the UI will hang on a blank screen or crash. Every item MUST have a numeric ID.
*   **NO NATIVE YOUTUBE TRAILERS:** We abandoned in-app trailer playback. Extracting YouTube MP4s natively on the TV without C-libraries is too unstable. The UI may show a trailer thumbnail, but the backend returns an error/blank if clicked. Do not attempt to re-add YouTube extraction.

## 3. How ID Management Works

### 3.1 Why synthetic IDs exist (CRITICAL)
The two halves of the app disagree about what an ID is:

*   **PlxNative requires numbers.** `ratingKey` is parsed as an `i64` (see §2). A string such as `"tmdb:603"` blanks the screen.
*   **Stremio and TMDB require strings.** Addons are asked for `tmdb:603`, `tt0133093`, `tt0944947:1:2`, and TMDB's own id is a bare integer inside a JSON body.

The shim is the only place that knows both vocabularies, so it hands the UI a **numeric** key and remembers — or decodes — the **string** identity behind it. Two mechanisms do that:

*   **The key table (`shim_store::Keys`)** — append-only `keys.tsv`. Line *N* is ratingKey `10_000 + N`, and the line records the real `Ident` (`Title { type_, id }`, `Season`, `Episode`). This is what real catalog items use, and it survives restarts so Continue Watching and cached images keep resolving.
*   **Synthetic keys** — for TMDB recommendations and franchises that no addon carries there is no `Ident` to store, so the TMDB id is encoded arithmetically and decoded on the way back in.

### 3.2 The synthetic bases (CRITICAL — do not change or overlap)
| Kind | Formula | Example | Range |
| --- | --- | --- | --- |
| Related **Movies** | `1_000_000_000 + TMDB_ID` | `1_000_000_603` (The Matrix) | `1_000_000_000 .. 2_000_000_000` |
| Related **TV Shows** | `2_000_000_000 + TMDB_ID` | `2_000_001_396` (Breaking Bad) | `2_000_000_000 .. 3_000_000_000` |
| **People** (Cast & Crew) | `3_000_000_000 + TMDB_PERSON_ID` | `3_000_000_638` (Keanu Reeves) | `3_000_000_000 .. 4_000_000_000` |
| **Collections** (franchises) | `4_000_000_000 + TMDB_COLLECTION_ID` | `4_000_000_234` (The Dark Knight Collection) | `4_000_000_000 ..` |

The bases are a billion apart on purpose: TMDB movie ids already pass 100,000, so a narrower gap lets a real movie key (`9_641_052` = movie `141_052` on the old bases) cross into the show range and load the wrong title.

```rust
const TMDB_FALLBACK_KEY_BASE: i64 = 1_000_000_000;
const TMDB_SHOW_KEY_BASE: i64 = 2_000_000_000;
const TMDB_PERSON_KEY_BASE: i64 = 3_000_000_000;
const TMDB_COLLECTION_KEY_BASE: i64 = 4_000_000_000;

/// A synthetic related-movie ratingKey's TMDB id.
fn synthetic_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_FALLBACK_KEY_BASE..TMDB_SHOW_KEY_BASE)   // bounded: a show key never decodes as a movie
        .contains(&rk)
        .then_some((rk - TMDB_FALLBACK_KEY_BASE) as u64)
}

/// A synthetic related-show ratingKey's TMDB id.
fn synthetic_show_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_SHOW_KEY_BASE..TMDB_PERSON_KEY_BASE)     // bounded: a person key is not a show
        .contains(&rk)
        .then_some((rk - TMDB_SHOW_KEY_BASE) as u64)
}

/// A Cast & Crew headshot's synthetic `personId` → TMDB's person id.
fn synthetic_person_id(rk: i64) -> Option<u64> {
    (TMDB_PERSON_KEY_BASE..TMDB_COLLECTION_KEY_BASE) // bounded: a collection is not a person
        .contains(&rk)
        .then_some((rk - TMDB_PERSON_KEY_BASE) as u64)
}

/// A synthetic collection ratingKey's TMDB collection id.
fn synthetic_collection_id(rk: i64) -> Option<u64> {
    (rk >= TMDB_COLLECTION_KEY_BASE).then_some((rk - TMDB_COLLECTION_KEY_BASE) as u64)
}
```

*   Each decoder is **bounded above** by the next base. Without that, `3_000_000_638` would also decode as the show a billion ids below it, and a headshot would open the wrong title.
*   The bases are above every real key (`10_000 + N`), so the five spaces cannot collide.
*   **Check the bases highest-first.** A key `>= 4_000_000_000` is always a collection, `>= 3_000_000_000` a person, `>= 2_000_000_000` a show, and a key in the movie range is always a movie.
*   A person key is a **`personId`, not a `ratingKey`** — it travels in `Role[].id` on a cast row, which is what the app's cast shelf reads to open `/library/people/{id}/media` (see §4.5).
*   A collection key is a **`ratingKey`** for a franchise row — it is never sent to an addon (nothing plays a collection), and it exists so the shelf's linked heading can open the collection page (§4.6).

Every synthetic kind is handled on the way back in by `/library/metadata/{rk}`: parse the key, decode it with the matching helper, fetch TMDB, and return the enriched Plex row (`tmdb_details_row` for movies, `tmdb_show_details_row` for shows, `collection_row` for a franchise). `/library/metadata/{rk}/related` recognises the synthetic title ranges too, so a TMDB-only related title can drill down forever, and a film in a franchise carries its collection shelf there (§4.6). The `photo` route resolves the same keys, so a synthetic title's `clearLogo`/`thumb`/`art` are served even though the key table never heard of it (§4.3).

### 3.3 Global ID translation (MUST be applied)
Addons never see a synthetic number. Every addon-facing request translates the key first:

```rust
/// The Stremio id addons know a synthetic related ratingKey by (`tmdb:603`), if it is synthetic.
fn translate_to_stremio_id(rk: i64) -> Option<String> {
    synthetic_show_tmdb_id(rk)
        .or_else(|| synthetic_tmdb_id(rk))
        .map(|tmdb_id| format!("tmdb:{tmdb_id}"))
}
```

**Streams and subtitles MUST both be translated.** They are the two requests that leave the shim for an addon, and an untranslated `1_000_000_603` reaches Torrentio (and every subtitle addon) as an unknown id — an empty Source Picker and no subtitles.

Rather than patch each caller, the translation lives in `Shim::slot()`, the one function that starts **both** fetches:

```rust
fn slot(&self, rk: i64, ident: &Ident) -> Option<Arc<StreamSlot>> {
    let (type_, id) = self.stream_target(ident)?;
    // A synthetic related key exists only as `tmdb:<id>` to addons: streams and subtitles are
    // asked for that, never for the shim's own number.
    let id = translate_to_stremio_id(rk).unwrap_or(id);
    // … spawns "pmsshim-streams" (backend.streams) and "pmsshim-subs" (backend.subtitles)
}
```

*   `unwrap_or(id)` leaves real catalog items untouched — their `Ident` id is already the Stremio id.
*   Because `slot()` is the single entry point for both threads, a new addon-facing call must go through it (or translate explicitly) or it will leak synthetic keys.
*   `/nuvio/streams` additionally accepts a synthetic key that has **no** `Ident` (synthetic keys are never written to `keys.tsv`), synthesising `Title { type_: "movie" | "series", id: "tmdb:<id>" }` so the Source Picker works on a related title.
*   `complete` comes from `ranked()`, never from the slot map. An absent slot means *nothing to ask for* (a related show has no episode yet), so it must answer `complete: true` with an empty list — reporting `false` makes the picker re-poll 60× and show "loading" forever.
*   `ranked()` takes a `chosen` flag, `true` on the two routes a PICK leads to (`/nuvio/select`'s own resolve: the `/decision` and the `Part` it opens) and `false` on the detail page's fetch and `/nuvio/streams`. With it set, the wait for more addons ends the moment a stream is recorded — the chosen file cannot be superseded, so those requests must not sit out `stream_grace` (2.5 s after the first playable stream) or, for a picked entry the local playable test does not recognise, the full `stream_deadline`. That wait was why a pick made during progressive loading appeared to wait for every addon to finish.

## 4. TMDB Enrichment (Movies & TV Shows)
The app is enriched with TMDB data: cast on detail rows, a "More Like This" hub of recommendations,
the title logo (`clearLogo`) the heroes draw, and — for a film in a franchise — a collection shelf of
the other films in it (§4.6).

### 4.1 One request per title
*   **Performance:** both kinds use a **single HTTP request** with `append_to_response=credits,recommendations,images`, so cast, related titles and the title logo arrive together. Do not add separate `/credits`, `/recommendations` or `/images` calls.

```rust
pub fn movie_details(&self, movie_id: u64) -> Result<TmdbMovieDetails> {
    self.get(&format!("/movie/{movie_id}?append_to_response=credits,recommendations,images"))
}

pub fn tv_details(&self, tv_id: u64) -> Result<TmdbShowDetails> {
    self.get(&format!("/tv/{tv_id}?append_to_response=credits,recommendations,images"))
}
```

*   The only other requests are a movie's `/movie/{id}/videos` (trailer thumbnail) and, for a film
    in a franchise, `/collection/{id}` (§4.6). The first is spawned in parallel with the details
    call; the second is the only one that adds latency to a detail page, and only for a film that
    has a franchise at all. TV shows take their trailer from Cinemeta, so `tv_details` is genuinely
    one request.
*   Responses are cached for 15 minutes by the shared `TmdbClient` cache, keyed by API key + path.

### 4.2 TMDB ID resolution: `movie_results` vs `tv_results`
An IMDb id resolves differently per kind. `TmdbClient::movie_id()` reads `movie_results`; `TmdbClient::series_id()` reads `tv_results` from the same `/find?external_source=imdb_id` endpoint:

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

*   Using `movie_id()` for a series returns `None`, which silently drops the cast — always pick the helper that matches the kind.
*   `content_level_id()` strips episode suffixes first, so an episode id resolves to its series.

### 4.3 Field mapping
*   **Cast:** `credits.cast` → the Plex `Role` array (`tag`, `role`, `thumb`, `id`), capped at 10 entries, for both `movie_details` and `tv_details`. `id` is the synthetic person key (§3.2) that makes the headshot clickable.
*   **Movies:** `title` / `release_date` / `runtime` → `title` / `originallyAvailableAt` / `duration`.
*   **TV Shows:** TMDB spells these `name` / `first_air_date`, and there is no runtime — rows use `DEFAULT_EPISODE_MS`. `TmdbShow` / `TmdbShowDetails` exist precisely because the field names differ.
*   **Related Movies:** `recommendations.results` → the Plex `Hub` array, `type`/`hubIdentifier` `movie`.
*   **Related TV Shows:** the same hub shape with `type`/`hubIdentifier` `show`.
*   **Franchises:** `belongs_to_collection` + `/collection/{id}` → a SECOND `Hub` in the same answer, `collection.related.{section}.{n}` (§4.6).
*   **Logos:** `images.logos` → the row's Plex `clearLogo`, the title treatment both heroes draw. Prefer the logo whose `iso_639_1` is `"en"`, else the first one with a `file_path`; the URL is `https://image.tmdb.org/t/p/w500{file_path}` (`tmdb_logo`). A title with no logo sends **no** `clearLogo` field at all, exactly as a real server omits it, so the hero falls back to its text title through its ordinary absent-logo path — do not substitute the poster or the title string.
*   **Artwork is fetched by PATH, not by URL.** The app never reads an image URL out of a row: it asks the server for `/library/metadata/{rk}/thumb|art|clearLogo` through `/photo/:/transcode` (`app/adapters/poster.rs`'s `logo_key` always builds that path). A synthetic related key has no `Ident`, so the `photo` route resolves it through `tmdb_synthetic_row` when `row` answers `None` — one cached TMDB request, then the resolved CDN URL is proxied byte-for-byte. Without that fallback a Related title's logo (and its poster and backdrop) 404s even though the row carries the URL.
*   A recommendation is rendered from the addon's own metadata when available (`meta("movie", "tmdb:<id>")` / `meta("series", "tmdb:<id>")`) and otherwise from TMDB, with a synthetic ratingKey (§3.2).
*   **API Key:** The `TMDB_API_KEY` is injected at compile time via GitHub Secrets (`option_env!("TMDB_API_KEY")`), with a runtime `std::env` fallback. An unset or blank key disables every TMDB lookup (the hub answers 503 and rows keep their Cinemeta metadata). Local testing requires exporting the env var.

### 4.4 TV Show limitations
*   **A TMDB-only related show has no episodes.** TMDB recommendations carry no season/episode list, and the shim does not spend a second request fetching them. Such a show opens with full metadata (Cast, Summary, Genre, poster/backdrop/logo, air date) and its own related hub, but has **nothing to play**.
*   **It becomes playable when an addon also has it.** `tmdb_show_recommendation_row` prefers `meta("series", "tmdb:<id>")`; when Cinemeta knows the show, the row is built from that `Meta` and the normal season/episode/stream path applies.
*   Do not "fix" this by adding a `/tv/{id}/season/{n}` fetch to the hub: that is one extra request per recommendation and multiplies the hub's latency. Add it only for a show that must be playable rather than merely browsable.
*   **A TMDB-only show is not in `keys.tsv`** — it is a pure arithmetic key, so anything that needs an `Ident` must special-case it the way `related_hub`, `/library/metadata/{rk}`, the `photo` route and `slot()` do.

### 4.5 Cast & Crew → the person page (filmography)
A headshot on a detail page is clickable, and what it opens is the app's **person page** — the same
screen a real Plex server's cast row opens, driven by the same `/library/people/{personId}/media`
fetch.

*   **The link is `Role[].id`.** `tmdb_role` sets it to `TMDB_PERSON_KEY_BASE + cast.id` (§3.2), so the
    app's cast shelf (`screens::detail::cast`) needs no TMDB knowledge at all: `Cast::person_key`
    returns that number and the Person store addresses `/library/people/{key}/media` with it.
    `tagKey` stays empty — it is plex.tv's global person guid, and TMDB has none.
*   **The route** answers `MediaContainer.Metadata[]` of ordinary movie/show rows. `PersonStore`
    splits them into its Movies and Shows shelves **by each row's own `type`**, so a film must be
    tagged `movie` and a show `show` — a mismatch lands on the wrong shelf.
*   **One request.** `person_credits` is `GET /person/{id}/combined_credits`, which merges films and
    shows into one array and states the kind per row in `media_type`. Do not add a `/person/{id}`
    call for the name or portrait: the page's header is drawn from the `Role[]` entry that opened it.
*   **The rows come from TMDB alone** (`tmdb_fallback_row` / `tmdb_show_fallback_row`), not from
    `meta()`. A recommendation row prefers the addon's own metadata, but a filmography has dozens of
    credits and one `meta()` per credit would be one HTTP request each.
*   **The character name rides on the row's `Role[]`**, keyed by the person's synthetic id, because
    `person::roles_from` joins on exactly that to caption each tile. It is a fact about the CREDIT,
    so dropping it loses the only place "Neo" appears on that page.
*   **Newest first, undated last** — the page sorts its own list the same way, so a stable server
    order keeps the shelves from reshuffling between fetches.
*   **A person with no TMDB id opens nothing.** `tmdb_role` omits `id` rather than emitting the bare
    base, which would address person `0`.
*   **A failed or unconfigured lookup is an empty container**, matching the other TMDB routes: the
    page then draws its header (from the `Role[]` entry) with no shelves, not an error screen.

### 4.6 Movie Collections (franchises)
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

*   **The source is `TmdbMovie::belongs_to_collection`** (`None` for a standalone film), and the
    shelf's films are `TmdbClient::collection_details(id)` → `GET /collection/{id}`. One extra
    request per franchise film's detail page, through the same 15-minute cache.
*   **It is a `collection.related.{section}.{n}` hub**, not a second recommendation hub. That is the
    identifier the app's `/related` reader splits a member's collection shelf out of
    (`plx_data::metadata::collection_shelf`), and it is what makes the shelf draw above Related
    under its own linked heading instead of folding the films into "More Like This". The answer is
    `"Hub": [<More Like This>, <franchise>]`, and the shim adds no third hub.
*   **The rows are ordinary recommendation rows** (`tmdb_recommendation_row`), so each film is a
    clickable, playable key. The page's own film is left out — it IS the page — and a franchise
    whose only member is the film on screen gets no shelf at all.
*   **The heading opens the collection page.** The hub's `key` names the collection by `tagId`
    (its TMDB id), which is the identity the app's collection store resolves, and the shelf's films
    are kept in `Shim::collections` so the page answers from what the detail page already fetched.
    The three routes that page needs:
    *   `/library/sections/{s}/collections` — the `tagId` resolution, as a collection row whose
        `index` is the TMDB collection id.
    *   `/library/metadata/{collection_rk}` — the header (`collection_row`), by the synthetic key
        `4_000_000_000 + TMDB_COLLECTION_ID` (§3.2).
    *   `/library/collections/{rk}/children` — the films, in TMDB's release order, paged like any
        other collection listing.
*   **The shim has no library scan**, so the section listing carries only franchises it has served
    (bounded at 256, cleared whole when full), ordered by id so a page's `start` means the same rows
    twice. It always includes the one whose heading was pressed, since that shelf came from this
    same process — but a franchise never shown is not listed, and `/library/collections/{rk}/children`
    for an unknown key answers 404 rather than inventing one.
*   **TV shows have no collection shelf.** TMDB's `belongs_to_collection` is a movie field.

## 5. Build & Release Pipeline
*   **Local Simulator:** `make -j10 sim-linux` (Requires the Linux `libav*` host libraries to be built and placed in `pkg/`).
*   **TV Build (.ipk):** Triggered by pushing a Git tag (e.g., `git tag -a v0.8.1 -m "..." && git push origin v0.8.1`). 
*   **GitHub Actions:** The runner builds the `.ipk` file and attaches it to the Release. Do not add `libclang` or ARM cross-compiler steps to the YAML unless you are explicitly compiling C-code (which you should avoid).

## 6. App Identity
*   The app has been rebranded to `com.nuvionative.app` so it can be installed side-by-side with the official PlxNative app.