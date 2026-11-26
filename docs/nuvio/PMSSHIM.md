# The in-process PMS shim (`plx_pmsshim`)

This document explains how Nuvio Native's shim works end to end: what it is, how it starts, how a
remote is key press turns into a Stremio addon call, and how the answer comes back as something
PlxNative's unchanged UI and player can consume. It is written against the source in this checkout
(HEAD `872617531`, branch `nuvio-native`) and every behavioural claim below is cited to a file and
line. The owner's device results are identified as such; a source reading is not a device proof.

Related documents: `ARCHITECTURE.md` (system shape), `ID-MANAGEMENT.md` (ratingKeys), `TMDB.md`
(enrichment), `PLAYBACK.md` (decoder contract), `TRAILERS.md`, `WATCH-PROGRESS.md`,
`TORRENT-ENGINE.md`, `MERGE-AUDIT.md`. This document is the end-to-end shim
walkthrough and the place where the current artwork/transport behaviour is stated precisely.

## 1. What "the shim" is

Nuvio Native reuses PlxNative's GPU UI, typed Plex data layer, session/preference layers and native
player. Those layers speak the PMS REST dialect and plex.tv account dialect. Nuvio has no backend
service that speaks either: addon config lives in the Nuvio account (Supabase), and catalogs,
metadata and streams come from Stremio addons over HTTPS (`ARCHITECTURE.md:11-24`).

The shim is the adapter that lets the unchanged client talk to that world. It has **three seams**:

```
  UI / screens / data / player (upstream PlxNative, mostly unchanged)
        │  PMS REST + plex.tv account REST over http://127.0.0.1:<port>
        ▼
  ┌──────────────────── one native process ────────────────────────────┐
  │ plx_pmsshim                                                       │
  │   loopback HTTP/1.1 server  ──► Shim::route ──► shim_pms handlers  │
  │                                      │                           │
  │                    shim_account / shim_profiles / shim_store      │
  │                                      │                           │
  │                         Backend trait (shim_backend.rs)           │
  └──────────────────────────────┬────────────────────────────────────┘
                                 ▼
              plx_stremio::addons::AddonSet ──HTTPS──► Stremio addons
                                                    Nuvio account RPCs
                                 ▼ (synthetic boots)
                         shim_fake::FakeBackend
```

1. **A real loopback HTTP server.** It binds `127.0.0.1:<port>` on a dedicated OS thread
   (`rust-modules/pmsshim/src/shim_core.rs:67-70`, `:99-102`); the Plex data layer dials it through
   libcurl's transport (`plex/src/http.rs:10-14`). Inside the shim, dispatch is a direct Rust call
   to `Shim::handle`, not a second network hop (`shim_core.rs:99-101`).
2. **A content seam.** `Backend` (`shim_backend.rs:23-86`) is the only way the shim asks for
   catalogs, metadata, streams and subtitles. The shipping implementation wraps
   `plx_stremio::addons::AddonSet` (`shim_backend.rs:88-104`); `FakeBackend` is the network-free
   version for tests and synthetic boots (`shim_fake.rs:1-8`).
3. **A control seam.** The app also holds `Arc<Shim>` and calls Rust methods directly for the
   prober, reachability, audio caps, addon/P2P controls, warm and watchlist access
   (`rust-modules/src/app/nuvio.rs:307-329`, `:336-388`, `:421`; `app/watchlist_io.rs:37-43`).

The shim is **not** a Plex Media Server, a plex.tv service or a transcoder. It publishes upstream's
wire identifiers unchanged (`X-Plex-*`, `ratingKey`, PMS routes, `plx_plex::identity::PRODUCT`);
rebranding never changes a byte on the wire (`docs/nuvio/AGENTS.md:13-22`).

## 2. Boot, address and lifecycle

At boot, `boot.rs` calls `nuvio::boot()` for a normal (non-controlled) boot after
`plx_net::net::global_init()` (`rust-modules/src/app/boot.rs:1047-1053`). `nuvio::boot()` starts the
server on port `32_420`, falling back to any free port (`app/nuvio.rs:29-31`, `:295`), then
publishes three values through `plx_base::local_server::set`:

- the origin `http://127.0.0.1:<port>` (`shim_core.rs:60-65`; `app/nuvio.rs:330-331`),
- the local token (`app/nuvio.rs:330`),
- the stable machine id `nuvionative-pmsshim-0001` (`shim_core.rs:9-10`, re-exported at `lib.rs:44`).

That `OnceLock` (`rust-modules/base/src/local_server.rs:9-28`) redirects the plex.tv client base
(`rust-modules/plex/src/plex/account.rs:44-50`) and Discover reads — person biography/filmography
and the Watchlist (`account.rs:170-190`) — to the loopback origin. PMS discovery is a separate path:
release builds permit plaintext credentials only for the exact registered in-process server —
matching origin, token, machine id and `owned`, with a literal loopback address
(`plex/src/plex/probe.rs:430-458`). **That check is client-side discovery policy; the shim itself
does not validate the token** (`shim_core.rs:26-27`).

Construction opens the key table, watch state and caches and builds the account
(`shim_pms.rs:2840-2904`), self-binds and sets the account origin (`shim_pms.rs:2906-2908`;
`shim_core.rs:82-84`). Unless this is a synthetic boot, Continue Watching is merged from the Nuvio
account at boot and again on profile switch (`shim_core.rs:85-98`; `shim_account.rs:675-703`).

There is **no shutdown or join path** in the shim crate: the listener thread serves for the process
lifetime, connections end on close or a 30 s idle timeout, and background workers are detached
threads (`shim_http.rs:18`, `:96`; e.g. `shim_pms.rs:3036`, `:3459`, `:3778`, `:6756`). This is a
current design property, not a claim that it is ideal; see §15.

## 3. HTTP transport and dispatch

`shim_core::start_on` binds a `TcpListener` and spawns a thread named `pmsshim` running
`shim_http::serve` (`shim_core.rs:73-78`, `:99-102`). `serve` accepts at most 48 live connections,
spawning a `pmsshim-conn` thread per connection with a 1 MiB stack and yielding to the frame thread
after each accept (`shim_http.rs:71-93`). Each connection loops on keep-alive, answering a panic
with 500 (`shim_http.rs:95-111`); requests are parsed with percent-decoding and the last repeated
query key winning (`shim_http.rs:113-165`, `:25-27`, `:160-165`, `:204-229`). Responses write an
HTTP/1.1 status line, `Content-Type`, `Content-Length` and keep-alive, suppressing the body for HEAD
(`shim_http.rs:167-202`). The helpers in `shim_http.rs:43-66` build JSON, byte, redirect and 404
responses; `Response::redirect` is a plain **302** with `Location` (`:60-62`).

`Shim::handle` optionally traces the request under `PMSSHIM_TRACE`, then `Shim::route` splits the
path into segments and matches on `(method, segments)` (`shim_pms.rs:6811-6827`, `:6946-6967`).
`/api/v2/*` is delegated to `Account::handle`, which has its own match (`shim_pms.rs:6977`;
`shim_account.rs:375-389`). The main route families are:

| Family | Where |
| --- | --- |
| `/`, `/identity` | `shim_pms.rs:6968-6975` |
| `/api/v2/*` (pins, QR, user, resources, home users, switch, ping) | `shim_pms.rs:6977`; `shim_account.rs:377-389`, `:484-553` |
| `/library/sections`, `…/{k}/all` and the sorts/genres/filters directories | `shim_pms.rs:6980-6986`, `:7576-7728` |
| `/library/metadata/{rk}` (single or CSV), `…/children`, `…/allLeaves`, `…/related`, `…/extras`, `…/trailer`, `…/subtitles` | `shim_pms.rs:6987-7154` |
| `/library/parts` PUT, `/library/parts/{pid}/…`, `/library/streams/{sid}` | `shim_pms.rs:7155-7159` |
| `/library/people/…`, `/library/collections/…` | `shim_pms.rs:7160-7201` |
| `/hubs`, `/hubs/sections/{k}`, `/hubs/search`, `/hubs/nuvio/{i}`, `/hubs/nuvio/list`, Continue Watching | `shim_pms.rs:7203-7333` |
| `/nuvio/library`, `/nuvio/streams`, `/nuvio/select`, `/nuvio/warm`, `/nuvio/profiles`, `/nuvio/avatars`, `/nuvio/profile-pin` | `shim_pms.rs:7313-7471`, `:6832-6943` |
| `/photo/:/transcode` | `shim_pms.rs:7335`, handler `:8126-8352` |
| `/:/timeline`, `/:/progress`, `/:/scrobble`, `/:/unscrobble`, remove-from-CW | `shim_pms.rs:7337-7399` |
| `/playQueues`, `/video/:/transcode/universal/decision`, `start.*` (404), generic `/video/*` (200) | `shim_pms.rs:7400-7409` |

PMS payloads are wrapped by `container()` into `{"MediaContainer": …}` with a computed `size`
(`shim_pms.rs:766-786`). An unmatched path is logged and answered with **200 and an empty
container**, not a 404 (`shim_pms.rs:7472-7474`). Only `Content-Length`/`Connection` and paging
headers (`X-Plex-Container-Start/Size`, via `q_or_header`) are read from the request
(`shim_http.rs:38-40`, `:148`, `:154`; `shim_pms.rs:935-945`).

## 4. Identity, account and profiles

The shim stands in for PMS *and* plex.tv. Its PMS identity is `machineIdentifier =
nuvionative-pmsshim-0001`, `version = 1.41.0.0000-nuvio`, `friendlyName = Nuvio`, `myPlex=false`
(`shim_core.rs:9-10`; `shim_pms.rs:6968-6975`). Because the plex.tv base is redirected to the shim
origin, the upstream account client's `create_pin`, `poll_pin`, `resources`, `home_users` and
`switch_user` calls all land in `shim_account` (`plex/account.rs:573-577`, `:592-599`, `:611-615`,
`:646-655`, `:669-676`). The QR PNG is generated locally and the screen caption names Nuvio's link
URL (`shim_account.rs:417-423`, `:788-812`; `screens/src/login.rs:1807-1814`).

`/api/v2/user` always answers a local `Nuvio Native` user carrying the seated profile's `id` and
`uuid` and the shim token, which is what makes upstream's preference-identity check resolve
(`shim_account.rs:472-482`; `plex/account/preferences.rs:13-17`). `/api/v2/resources` advertises
exactly one owned server with the loopback connection (`shim_account.rs:532-542`), with a local
roster fallback when signed out (`:544-553`). The real Nuvio roster is projected into Plex-shaped
tiles by pure helpers (`shim_account.rs:558-574`; `shim_profiles.rs:50-72`); `shim_profiles` is the
IO-free uuid↔index/avatar/settings projection layer (`shim_profiles.rs:1-8`, `:13-46`, `:93-101`).

`shim_account` owns the device login and Nuvio backend session (token storage/refresh), PIN state,
preferences/profile cache, account watch push/pull, My List and addon fetch/sink
(`shim_account.rs:34-55`, `:96-136`, `:241-263`, `:297-352`, `:655-703`). The account session is a
redb row, not the legacy `nuvio-session.json`, despite stale comments in that file
(`shim_account.rs:84-94`, `:1110-1143`). TV PINs are PBKDF2-HMAC-SHA-256 verifiers in `ProfilePins`
(`nuvio-store/src/pins.rs:15-32`, `:96-110`). A **Sign out** in the app now clears every verifier in
that table through the session layer's `clear_for_erase` (`plex/session.rs:5565-5571`,
`session/nuvio_signout.rs:1-46`), but only after the canonical credential clear is confirmed durable —
so a failed sign-out cannot let a remote-holder drop a profile lock. Re-signing in with the phone as
the same account is the documented recovery for a forgotten PIN; PINs cannot be recovered from a
sign-out alone.

Normal boots keep the **Account** authority so a stored Nuvio sign-in seats a profile
(`boot.rs:609-621`, `:1222-1225`, `:1275-1280`); only synthetic boots take the dev-PMS identity path
to Home with nobody seated (`boot.rs:1217-1221`, `:1280-1317`). `docs/nuvio/AGENTS.md:87` is true for
normal boots; synthetic boots are the deliberate exception (`nuvio_debug.rs:28-39`).

## 5. Catalogs, sections, hubs and search

The only content source for a normal boot is the addon set: `Backend::{catalogs, all_catalogs,
catalog_page, search, meta, streams, subtitles}` (`shim_backend.rs:23-86`). The default set is
Cinemeta + OpenSubtitles (`stremio/src/addons.rs:553-558`). A catalog page is a Stremio
`GET /catalog/{type}/{id}[/{extra}].json` with `skip`/genre (`addons.rs:694-709`;
`shim_backend.rs:144-150`). TMDB is not called while a hub is filled
(`shim_pms.rs:3333-3425`, `:6674-6705`).

- **Sections:** exactly two PMS libraries — Movies `1`, TV Shows `2` (`shim_pms.rs:58-59`,
  `:6980-6983`).
- **Home `/hubs`:** Continue Watching first, then the Home-eligible catalogs taken from the *full
  browse list* so a hub index names the catalog the row came from (`shim_pms.rs:7203-7234`,
  `:6732-6751`). Each catalog gets a 4 s window (`shim_pms.rs:38-42`, `:7228`); a late fill keeps
  writing the cache and a finished warm bumps `content_gen` (`:6792-6806`). Empty hubs are dropped
  (`:6780-6789`).
- **Recommended `/hubs/sections/{k}`** lists every browseable catalog of that type, including
  catalogs hidden from Home (`shim_pms.rs:7277-7291`); `/hubs/nuvio/{i}` indexes the same list
  (`:7322-7329`). This is the old-fork behavior pinned by
  `recommended_rows_show_the_full_browse_list_not_only_home_catalogs` (`:2259`).
- **Listings** select a named catalog or map the client's sort to Cinemeta's `top`/`imdbRating`,
  forward declared genres to the addon and filter TMDB-baseline genres in memory
  (`shim_pms.rs:7581-7662`). A sorted or genre-filtered read needs the whole catalog; otherwise it
  fetches only `[start, start+size)` (`:3379-3388`). One background filler walks up to 20 pages,
  dedupes by id and never holds the row lock across a fetch (`:3429-3477`, `:3498-3578`).
  `totalSize` is fetched rows plus `MORE_HINT = 50` while not exhausted (`:51-53`, `:3423`).
- **Rows:** `preview_row` publishes a numeric `ratingKey`, `key=/library/metadata/{rk}`,
  `guid=nuvio://{type}/{id}`, title/summary, `thumb` (poster), `art` (addon background),
  `Genre`, `UltraBlurColors`, addon `clearLogo`, year, IMDb/RT scores and watch fields, and
  remembers the card for CSV listings (`shim_pms.rs:4056-4116`, `:4118-4127`, `:7063-7082`).
  `meta_row` adds dates, roles, duration and show child/leaf counts (`:4129-4165`).

## 6. Identities: real ratingKeys and synthetic ranges

PlxNative parses `ratingKey` as `i64`; Stremio and TMDB speak string ids. `Keys` maps every distinct
`Ident::{Title,Season,Episode}` to a number `FIRST_KEY + N` (`FIRST_KEY = 10_000`) in one JSON row
in the store (`shim_store.rs:15-17`, `:30-95`). TMDB-derived rows get bounded synthetic ranges
(`shim_pms.rs:64-78`, `:92-120`):

| Kind | Formula |
| --- | --- |
| Related movie | `1_000_000_000 + TMDB_ID` |
| Related show | `2_000_000_000 + TMDB_ID` |
| Person (a `personId`, not a `ratingKey`) | `3_000_000_000 + TMDB_PERSON_ID` |
| Collection | `4_000_000_000 + TMDB_COLLECTION_ID` |
| Trailer clip | `5_000_000_000 + parent + index * 10_000_000_000`, `index < 12` |

Each decoder is bounded above by the next base so a key cannot decode as two kinds (`shim_pms.rs:92-165`).
The trailer stride is **10 billion**, not a plain `+ index`; see `ID-MANAGEMENT.md` for the full
table. `translate_to_stremio_id` turns a related key back into `tmdb:<id>` before **both** addon
fetches — streams and subtitles (`shim_pms.rs:501-506`, `:3736-3742`, `:7981-7988`). A real item
whose own id is `tmdb:<n>` is upgraded to an IMDb id by `addon_id_for` (`:3688-3698`).

The durable `IMDB_TMDB` table holds only `tt… → u64` TMDB ids, with no TTL and no negatives
(`shim_tmdb_ids.rs:9-10`, `:24-43`). The opposite `tmdb:<n> → tt…` direction is a bounded in-memory
`Shim::imdb_ids` map (512 entries, negative answers cached) used for account identity/watch
mirroring and addon id translation (`shim_pms.rs:721-723`, `:2929-2948`, `:3704-3733`).

## 7. Detail metadata and TMDB

A detail read is built from addon metadata plus an optional TMDB enrichment: `MetaCache` holds one
row per `(type,id)` (series 6 h, movie 30 d, 400 in-memory rows) and `EnrichCache` holds cast,
extras, videos, related and collection rows for 30 d with 800 in-memory rows
(`shim_store.rs:369-397`, `:408-482`, `:495-535`, `:548-632`). Enrichment is refreshed if an older
row lacks the related shelf (`shim_pms.rs:4178-4187`). Single-flight gates prevent a prefetch and
the click that warmed it from issuing two addon/TMDB fetches (`shim_flight.rs:1-25`;
`shim_pms.rs:8415`). TMDB is used **only** on detail pages and their children: cast, title logo,
landscape `art`, trailers, More Like This and franchise collections (`TMDB.md:83-101`).

This is the actual artwork behavior in the shipped code, which is narrower than some older prose:

- A row's `thumb` is `poster_url(id, original)` (`shim_pms.rs:4067`, `:6386-6399`). It passes the
  item's **own** id to `Backend::poster`; `AddonBackend::poster` rewrites only a literal `tt…` id
  with at least seven digits to `https://btttr.cc/poster/imdb/poster-default/{id}.jpg?rs=RT`, and
  returns the addon's original for every other id, including `tmdb:603`
  (`shim_backend.rs:158-162`). `imdb_base` is **not** on this path. Tests:
  `better_posters_apply_to_titles_from_other_catalogs` (`shim_backend.rs:192`) and
  `the_items_own_id_is_the_poster_lookup_identity` (`shim_pms.rs:9285`).
- `art` and `clearLogo` on real rows are the addon's `background` and `logo` verbatim
  (`shim_pms.rs:4077`, `:4082-4085`); enrichment does not overwrite them (`:4174-4268`). TMDB
  backdrops/logos are written only on **synthetic TMDB detail rows** (`:5580-5585`, `:5639-5644`
  via `tmdb_image`/`tmdb_logo` at `:947-967`). Continue Watching preserves stored `art`
  (`:6401-6426`). There is no linear "TMDB first, then BetterPosters, then metahub" selector.
- `UltraBlurColors` is derived from the poster actually chosen (`shim_pms.rs:4080`).
- A missing TMDB logo omits `clearLogo` on a synthetic row instead of substituting the poster
  (`shim_pms.rs:952-967`).

## 8. `/photo` — how image bytes are actually served

`GET /photo/:/transcode` accepts a `url` that may itself be a local metadata path such as
`/library/metadata/{rk}/thumb`, `art` or `clearLogo`; those are resolved from the shim's row,
rebuilding a synthetic row from TMDB if needed (`shim_pms.rs:7335`, `:8126-8159`). A row that has
not landed yet answers **503 (retryable)**, not a latched 404 (`:8126-8159`; test `:9793`).
Non-http(s) URLs get 404 (`:8161-8163`).

The proxy tries the sized URL first (`shim_pms.rs:8170`, `:8310-8352`), then the original registered
by `poster_url` in `poster_fallbacks` (up to 4096 entries) (`:6386-6399`, `:8171-8183`). **Any**
non-ok or empty response advances to the next candidate; if none succeeds it answers 502
(`:8184-8209`; test `a_failed_poster_provider_falls_back_to_the_original_artwork` at `:8945`).
CDN URLs are rewritten to a size near the card box: TMDB images use the
`w185/w342/w500/w780/w1280/original` ladder (`:8310-8326`; test `:10096`), MetaHub images the
`small/medium/large` ladder (`:8327-8350`; test `:10068`). `clearLogo` paths alone receive
transparent-border ink cropping, cached in `plx_os::imgcache` under `logo-ink-v1`; posters and JPEGs
pass through unchanged
(`:8164-8202`, `:8213-8286`; tests `:8828`, `:8854`, `:8884`).

## 9. Playback, end to end

1. **Slot creation.** `stream_target()` maps the ratingKey to an addon `(type,id)`
   (`shim_pms.rs:3679-3686`). `slot()` starts one background `pmsshim-streams` thread per item,
   calling `Backend::streams_fetch` with batching, plus a `pmsshim-subs` thread
   (`shim_pms.rs:3736-3850`; `shim_backend.rs:169-185`).
2. **Discovery and rank.** `ranked()` waits up to `stream_deadline` (60 s) and `stream_grace`
   (2.5 s) after the first playable stream (`shim_core.rs:43-50`), then ranks by
   `playable_url`, direct-first, resolution, audio-bad, seeders, HDR and size, capped at 200
   (`shim_media.rs:9-13`, `:25-49`, `:72-97`, `:3873-4038`). A detail read does not block on this.
3. **Media/Part/Stream rows.** Each ranked stream becomes one `Media` with
   `id = ratingKey*1000 + index` and a `Part` whose `key` is `/library/parts/<pid>/…` and whose
   `file` is the release filename (not a filesystem path) (`shim_media.rs:334-416`, `:411`).
   Label guesses fill codec/resolution fields (`:353-393`); there are **no** chapter/intro/credit
   markers and no `protocol` field in those rows.
4. **Real probe.** The app installs `prober()` (`app/nuvio.rs:303-329`, `:485-546`) backed by
   `plx_nuvio_probe::probe_url`, which opens a private curl transfer at byte 0, wraps it in AVIO and
   calls libavformat (`nuvio-probe/src/lib.rs:291-354`). There is no HEAD request. `shim_probe::apply`
   replaces the guesses with container, duration, size, frame rate, color primaries/space/transfer,
   all DOVI keys together, audio tracks in file order and embedded subtitles in file order
   (`shim_probe.rs:414-518`). `ProbeCache` keys by scheme+host+path with query stripped (torrents by
   content+file): URL-keyed verdicts expire after 30 min, content-keyed ones never expire, and a
   failure is cached under a URL key (except googlevideo) but never under a content key
   (`:92-103`, `:241-248`, `:300-311`). A detail read pre-probes the leading stream in the
   background (`shim_pms.rs:6228-6249`).
5. **Decision.** `GET /video/:/transcode/universal/decision` parses `path`/`mediaIndex`, calls
   `item_probed(rk, Some(index))`, answers `generalDecisionCode:1000` with
   `Part.decision="directplay"` and `Stream.decision="copy"` when video and a decodable audio track
   are known, or `2000` with `transcodeDecisionText` when not (`shim_pms.rs:6305-6382`;
   `shim_probe.rs:369-379`). The app asks this **before** queueing playback, forces Original quality
   and shadows its item with the probed Media (`media/src/route/plan.rs:1815-1822`, `:1853-1913`).
   For a Nuvio server it uses a 30 s deadline-only read (`plex/src/plex/transcoder.rs:545-617`).
6. **Part open.** `GET <Part.key>` decodes `pid = rk*1000 + idx`, resolves a stale index from the
   remembered stream identity, resolves torrent markers through the C engine and answers a **302**
   to the stream URL, or 502 if it cannot resolve one (`shim_pms.rs:7918-7969`, `:5884-6018`).
   The media transports follow that hop and confine credentials to the shim origin
   (`net/src/stream_redirect.rs:1-33`, `:46-56`). A seek reopens the Part URL, so the 302 is reissued
   per seek (`PLAYBACK.md:18-22`).
7. **No byte proxy and no transcode.** `part()` has no proxy branch, even for streams whose addon
   declares `behaviorHints.proxyHeaders`; the only `behavior_hints` use is a filename fallback
   (`shim_media.rs:114`). `start.*` answers 404 `"no transcoder: streams play direct"`
   (`shim_pms.rs:7403-7408`). `PUT /library/parts/{pid}` now commits the **same pick** as
   `/nuvio/select`: a Part id is `ratingKey*1000 + arrival index`, so the route resolves that slot
   and sets its selected stream (`:7155`, `:7881-7897`); `/nuvio/select` addresses the same slot by
   key and index (`:7447-7469`), and a successful decision probe can remember a source
   (`:6262-6264`). Trailer Parts keep their harmless 200 without touching a slot.
8. **Progress.** `GET /:/timeline`, scrobble/unscrobble and remove-from-CW are answered and fed to
   the same redb watch table (`shim_pms.rs:7337-7399`, `:7832-7873`); when signed in they are also
   mirrored to the Nuvio account (`WATCH-PROGRESS.md`).
9. **Subtitles.** External sidecars are type-3 rows with `key:/library/streams/<id>.srt`
   (`shim_media.rs:386-393`). The shim fetches the addon URL itself (4 MiB limit) and answers 200
   `text/plain; charset=utf-8` after gunzip/BOM/cp1252 handling (`shim_pms.rs:8097-8124`;
   `shim_subs.rs:6-39`). Search/install use
   `GET|PUT /library/metadata/<rk>/subtitles` (`:7977-8095`). Faults are never cached as "no
   subtitles" (`shim_subtitle_cache.rs:25-34`, `:103-186`).

## 10. Trailers

Trailer candidates come from cached TMDB `videos` (trailers before teasers, YouTube only), then
Cinemeta meta, then an IMDb-keyed fallback (`shim_pms.rs:4280-4446`), capped at 12. A clip's own key
is `5B + parent + index*10B`, with DASH part `clip*1000+998` and muxed `+999` (`:78`, `:127-198`).

Resolving a clip spawns Node with `js-service/trailer-decipher.js` (10 s cap, one child per resolve,
≥300 ms pacing, `PR_SET_PDEATHSIG`) and accepts only `https://` URLs from its JSON contract
(`shim_trailer.rs:128-206`, `:209-291`, `:436-543`). `LOGIN_REQUIRED` marks an id blocked for 6 h
(`:270-315`). Results are cached for 30 min positive / 60 s negative with single-flight
(`:545-625`). `publish_trailer` probes the DASH video in parallel with a ranged reachability check
on both tracks (each verdict cached 60 s); a bad DASH pair falls back to a verified muxed row
(`shim_pms.rs:4526-4655`, `:4921-4990`). `dash_manifest()` emits a static SegmentBase MPD whose
BaseURLs are the signed googlevideo URLs (`:1141-1238`), which `plx_nuvio_dash` parses and
`play-feed/src/dash.rs` demuxes as two seekable fMP4 sources (`nuvio-dash/src/lib.rs:1-20`;
`play-feed/src/dash.rs:21-93`, `:235-261`). The Part handler serves the MPD with 200 and the muxed
row with 302 (`shim_pms.rs:7879-7917`). Shelf rows are built without a resolve; autoplay resolves at
most two candidates and publishes the tallest DASH at index 0, while the clicked index is resolved
on demand (`:4471-4524`, `:4847-4919`). `TRAILERS.md` has the id, mode and supersede contract.

## 11. The one durable store

All shim state is in the fork-owned `nuvio.redb` under `<data>/shim/` (`nuvio-store/src/lib.rs:33`,
`:150-151`; `base/src/paths.rs:1434-1465`). `plx_nuvio_store` is a schema-free byte store; the shim
owns the encoding, keys and TTL policy (`nuvio-store/src/lib.rs:1-9`, `:213-247`).

| Table | Written by | Key / value |
| --- | --- | --- |
| `Keys` | `shim_store.rs` | one `"ids"` row: JSON-encoded `Ident` list; line N ⇒ key `10_000+N` (`:15-17`, `:30-95`) |
| `Watch` | `shim_store.rs` | `"state:{index}"` for profile slots 1–6, JSON resume/watch rows (`:116-233`) |
| `MetaCache` | `shim_store.rs` | `"{type}\t{id}"`, TTL envelope + JSON `MetaRow { type, id, at, meta }` (`:390-399`, `:416-505`) |
| `EnrichCache` | `shim_store.rs` | `"{type}\t{id}"`, TTL envelope + JSON `EnrichRow { …at, enrich }` (`:512-660`) |
| `AccountSession`, `InstallPrefs`, `ProfilePins` | `shim_account.rs` | session JSON, addon fingerprint, PBKDF2 verifiers (`:84-94`, `:765-766`; `pins.rs:51-68`) |
| `ImdbTmdb` | `shim_tmdb_ids.rs` | `movie:{tt…}`/`show:{tt…}` → plain 8-byte BE `u64`, no TTL (`:9-10`, `:24-43`) |
| `CatalogCache` | `shim_catalog_cache.rs` | TTL envelope + `CachedCatalog`, 15 min, namespaced by addon set and manifests (`:21-31`, `:57-117`) |
| `SubtitleCache` | `shim_subtitle_cache.rs` | TTL envelope + candidates, 6 h / real empty 10 min, never a fault (`:61-186`) |

**Privacy erase.** Settings ▸ Privacy & data ▸ *Delete all local data* revokes the account first
(`session.rs::clear_for_erase`), then `plx_nuvio_store::erase::erase()` clears every table in the
file and unlinks this install's data paths (`nuvio-store/src/erase.rs`, enumerated in
`base/src/paths/nuvio_erase.rs`): the data root (`<data>/shim/` with `nuvio.redb`, `addons.json`,
`settings.json`), the poster cache, torrent swarm state and probe verdict, installed HUD packages
and the dev-shell import handoff. Every path is keyed by `app_id()`, so one flavour's erase cannot
reach another's data. Clearing the tables before unlinking is deliberate: a jail that mounts a
data rung read-only cannot be unlinked, but an empty table set leaves no record for the next boot.
A synthetic boot only clears its private store; it never unlinks a real install's file.

`Store::open` runs maintenance when the file exists: purge expired TTL-envelope rows, shed oldest
live cache rows, then compact, under a 50 MiB soft budget; durable rows are never shed and an
over-budget durable-only file is logged and left intact (`nuvio-store/src/lib.rs:22-30`, `:147-164`,
`:259-328`, `:339-360`). Unreadable canonical DB8-shaped records are quarantined as
`<id>.rejected-<millis>` and read as missing (`os/src/storage/redb_backend.rs:86-120`, `:186-199`).
Legacy `keys.tsv`, `watch.json`, `meta-cache.jsonl`, `enrich-cache.jsonl` and `nuvio-session.json`
are one-time migration inputs only (`shim_store.rs:69-77`, `:192-199`, `:425-436`, `:577-587`;
`shim_account.rs:97-117`).

Two precise caveats the older prose glosses over:

- `MetaCache`/`EnrichCache` now write the store's 8-byte BE-millis TTL envelope around their JSON
  (`shim_store.rs:469-474`, `:500-507`, `:636-641`, `:666-673`), so the store's `purge_expired` and
  `shed_oldest` can reclaim the heaviest rows. `open()` still accepts the pre-envelope raw-JSON rows
  written by earlier builds (`decode_cached`, `:415-423`), deletes ones past the shim's own TTL and
  rewrites the live ones under an envelope (`:447-476`, `:616-645`); no stored-data migration was
  needed. The row's own `at` remains authoritative for in-memory freshness (`:483-485`, `:640-642`).
- `SubtitleCache`'s durable `durable_get`/`durable_put` are compiled to no-ops under `#[cfg(test)]`
  (`shim_subtitle_cache.rs:75-101`). `CatalogCache` still no-ops them by default, but a thread-local
  test store (`shim_catalog_cache.rs:20-30`, `:74-103`) lets the one real restart test close and reopen
  the redb and prove a shelf survives (`a_relaunch_reopens_the_store_and_reads_the_catalog_page`,
  `:178-214`). The older test named `a_relaunch_reads_catalog_pages_from_the_durable_window`
  (`shim_pms.rs:2588`) still exercises only the in-memory map. Real relaunch warmth was also
  confirmed on device by the owner (`VALIDATION.md:26-28`).

## 12. Synthetic and fake modes

`FakeBackend` is a fixed, network-free library of four Blender films and one two-season series,
each with a direct MP4 and a fake torrent infohash (`shim_fake.rs:1-8`, `:20-33`, `:96-168`). There
are two distinct ways to use it:

- `pmsshim-serve --fake` (`bin/serve.rs:1-34`) is a host-only dev binary. It swaps the backend but
  leaves `Config::synthetic = false` and installs **no** redb store (`serve.rs:15-27` builds the
  `Config` and calls `start_on` without ever opening a `Store`), so keys/watch fall back to memory.
  Its advertised origin is still `http://127.0.0.1:{port}` even with `--host`
  (`shim_core.rs:84`). No TV packaging reference for this binary was found in the checkout; its host
  usage is summarized in `bin/serve.rs:1-6`.
- `plxnative-nuviofake` is a full synthetic boot (compiled out of release): a private empty
  `nuvio-synthetic.redb` installed before `session::load`, `Config{synthetic:true, data_dir:None,
  torrent_server:None, p2p_enabled:false}`, `FakeBackend`, no account reads/writes and a dev-PMS
  identity path to Home (`nuvio-debug.rs:16-39`, `:41-70`; `app/nuvio.rs:42-53`; `shim_core.rs:80-98`).
  Arm `plxnative-nuviofake` in the instance root to drive it.

`shim_flight` is unrelated to either mode: it is a generic keyed single-flight registry used for
meta, trailer and subtitle fetches (`shim_flight.rs:1-25`, `:30-99`). Probes have their own
per-key `inflight` map (`shim_probe.rs:194-202`, `:289-318`).

## 13. Tests and evidence

`plx_pmsshim` has 181 `#[test]` functions in this tree, two of them ignored. The modules pin:

- storage and migration (`shim_store.rs:696-911`), account/profile/PIN (`shim_account.rs:855-1083`,
  `shim_profiles.rs:112-182`), id persistence (`shim_tmdb_ids.rs:117-145`);
- hub windows, catalog selection, old-fork poster rules and `/photo` fallbacks (`shim_pms.rs:2259`,
  `:2343`, `:4056-4116`, `:8784-8986`, `:9285`, `:9450-9793`);
- playback refusals, probe fields and remembered-source resume (`shim_pms.rs:8630-9192`,
  `shim_probe.rs:532-774`);
- trailer candidates, DASH/MUXED publication and cache eviction (`shim_pms.rs:10443-11329`,
  `shim_trailer.rs:950-1131`).

Focused host loops: `make test-crate C=plx_pmsshim` (add `T=<filter>` for a module),
`make check-nodef` for the shipping feature set; an ARM `make` is required before claiming the target
still builds (`docs/nuvio/AGENTS.md:81-90`). The two ignored engine tests run through
`tools/host-engine-check.sh`. Device results belong to the owner and are recorded in
`VALIDATION.md`, `MERGE-AUDIT.md` and `HANDOFF.md`; this document does not add a device claim.

## 14. What the shim deliberately does not do

- No transcoder: `start.*` is 404 and a failing probe is a refusal, never a `transcode` decision.
- No token check in the server: loopback plus client-side discovery gating is the boundary.
- No LAN server: `start_on` can bind another interface for the host tool, but the advertised origin
  is always `127.0.0.1` (`shim_core.rs:73-84`).
- No plex.tv account of its own: the upstream account routes are a compatibility surface backed by
  the local Nuvio identity and a locally drawn QR (`shim_account.rs:377-389`, `:417-423`).
- No second store in normal boots; the synthetic private store and host test temp stores are the
  documented exceptions (`nuvio-store/src/lib.rs:384-426`).
- No async runtime: work is done with detached OS threads and keyed single-flight gates.

## 15. Code questions awaiting the owner's decision

These are properties of the current code that this documentation records but does **not** change.
They need a product call before anyone edits behavior:

1. `MetaCache`/`EnrichCache` stored raw JSON while the store's budget pass expects a TTL envelope
   (§11). **Resolved 2026-11-26 at the owner's direction:** both caches now write the envelope;
   `open()` accepts old raw rows, deletes expired ones and rewrites live ones under an envelope, so
   no stored-data migration was needed. Pinned by two regression tests (`shim_store.rs:753-794`,
   `:894-928`).
2. The durable catalog/subtitle cache I/O is disabled under `cfg(test)`, so the "relaunch" unit tests
   do not reopen a store (§11). **Resolved 2026-11-26:** a thread-local store seam now lets a catalog
   test close and reopen the real redb (`shim_catalog_cache.rs:178-214`); subtitle cache I/O is still
   test-no-op'd, with device warmth as its evidence.
3. `Config::data_dir` is documented as controlling persistence but actually only names the legacy
   migration directory (§11). Decide whether to rename/deprecate the field or just fix its comment.
4. `pmsshim-serve --data` installs no store and `--host` still advertises loopback (§12). Decide
   whether the host tool should get a real store and a truthful advertised origin.
5. `PUT /library/parts` was a no-op 200 while the pick was committed only by `/nuvio/select` (§9).
   **Resolved 2026-11-26:** the PMS route now decodes the Part id's ratingKey and arrival index and
   commits the same slot selection (`shim_pms.rs:7881-7897`), pinned by a regression test.
6. Unmatched paths return 200 with an empty container rather than 404 (§3). Decide whether changing
   that is safe for the upstream client's parsing.
7. There is no shim shutdown/join path; workers are detached threads for process lifetime (§2).
   Decide whether a clean shutdown is worth the plumbing.
8. `Account::sign_out()` itself still has no route and no production caller
   (`shim_account.rs:265-295`, `:377-389`). Its forgotten-PIN behavior is now covered by the session
   sign-out hook (`plex/session.rs:5565-5571`, `session/nuvio_signout.rs:37-46`), so it is dead code
   rather than a broken recovery path. Decide whether to remove it or route it as well.

## 16. Corrections made to the other documents

The following older claims were false against this checkout and have been corrected in place; the
history is preserved with dated correction notes in `MERGE-AUDIT-HISTORY.md`:

- artwork selection (`AGENTS.md`, `ARCHITECTURE.md`, `TMDB.md`, `ID-MANAGEMENT.md`, `HANDOFF.md`,
  `MERGE-AUDIT-HISTORY.md`) — see §7 and §8 for the actual behavior;
- the trailer key stride (`ID-MANAGEMENT.md`) — §6;
- `NuvioCache` no longer used by the shim (`ARCHITECTURE.md`, `ID-MANAGEMENT.md`) — §11;
- `Part` proxy, `PUT /library/parts` and the transcode `start.*` response (`PLAYBACK.md`) — §9;
- the shim's own auth and `data_dir` scope (`AGENTS.md`, `ARCHITECTURE.md`) — §2 and §11.
