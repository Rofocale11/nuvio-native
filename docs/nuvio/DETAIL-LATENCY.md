# Detail-page latency — plan

**Status:** Stage 0, Stage 1 (1A–1C), Stage 2A, Stage 2B and Stage 2D implemented (2026-11-03),
host + simulator verified. Written 2026-11-02 on `forward-port/v0.9-hq` (`7e79e57e`).
**Owner:** whoever picks this up next.
**Scope:** wall-clock from pressing OK on a Home/Library card to a painted movie/show detail page,
on the television. Not the player, not Home's own first paint, not poster throughput.

## The problem

On the TV, a cold detail open takes roughly **1–3 s**. Other Stremio/Nuvio clients on the same
television, same network, feel instant. The fork's own log already records the total
(`detail: … ms=`, `rust-modules/data/src/metadata.rs:3870`), so measure before trusting any
number here — the split below is structural, not measured on the set yet.

**Verdict: the shim architecture is not wrong.** Keeping the Plex dialect so the ported UI and
player run unmodified is the right call. The latency comes from three separable decisions layered
on top of it:

1. one shim request composites three unrelated data sources and the screen waits for all of them;
2. the detail route used to pay up to **800 ms** waiting for streams it does not need to paint
   (fixed by 1A);
3. every outbound HTTPS call is a **fresh DNS + TCP + TLS handshake** (no connection or DNS reuse),
   and nothing is prefetched on focus.

Fix those in that order and the page paints in roughly the time of one warm round trip.

---

## 1. What happens today — cold movie, end to end

App side:

1. OK on a card → `activate_card` (`rust-modules/src/app/input.rs:141`) →
   `bridge::open_detail(sid, rk)` (`input.rs:200`, `rust-modules/src/app/bridge.rs:3284`). `rk` is
   the hub row's numeric `ratingKey`.
2. The detail screen enters and emits `MetadataCmd::RequestDetail` the same frame
   (`rust-modules/screens/src/detail/mod.rs:2099`; a restored enter only re-requests when
   `self.detail(meta)` is empty, `:2094`).
3. `request_detail` spawns the `"detail"` worker (`rust-modules/data/src/metadata.rs:4016`,
   dispatched at `:4001`), which runs `fetch_full` (`:3699`).

Shim side, for `/library/metadata/{rk}` (`rust-modules/pmsshim/src/shim_pms.rs:3751`; the
single-key route falls through to the comma-list branch at `:3815`, which maps every key through
`self.item()` and `self.enrich_row()`):

4. `item()` → `item_probed()` (`:3264`, `:3151`) → `row()` → `meta()` (`:1894`) →
   `AddonBackend::meta` → `AddonSet::meta` (`rust-modules/stremio/src/addons.rs:730`) → **Cinemeta**
   `https://v3-cinemeta.strem.io/meta/{type}/{id}.json` (`addons.rs:21`). Candidates are tried
   **serially** (`addons.rs:659` order: id-prefix owners, then every addon serving the type, then
   the inferred type).
5. `item_probed` starts the stream slot (`shim_pms.rs:5092`) and — since 1A — **no longer blocks**
   on it: a detail read passes `Duration::ZERO` (`:5144`) and paints with the placeholder Part
   while the slot keeps answering in the background.
6. `enrich_row` (`:3235`, called at `:5922`) resolves the TMDB id and fetches cast/trailer/logo
   **synchronously**: since 1B the id comes from Cinemeta's `moviedb_id`, with `/find`
   (`rust-modules/stremio/src/addons_tmdb.rs:316` for movies, `:405` for series) only as the
   fallback; then `/movie/{id}?append_to_response=credits,recommendations,images,videos` (a show
   uses `/tv/{id}` the same way) — one request now carries the trailer list too, so the old
   sibling `/movie/{id}/videos` call is gone.
7. The app then requests `/extras` (the shim answers every trailer shelf row from cached meta and
   the step-6 `videos`; it deliberately does **not** resolve any of them — `docs/nuvio/PLAYBACK.md`
   — because a Node round-trip here cost 3–5 s per open and waited on the decipher gate; it does
   start a background resolve of the first tile so the 2 s autoplay dwell covers it) and
   `/related` (TMDB recommendations, usually a cache hit from step 6; `/collection/{id}` when the
   film has a franchise), plus `/nuvio/library` for the watchlist state (60 s cache) and then the
   images.
8. Since 1C, resting on a card for 250 ms has already issued the step-4–6 GET and dropped the
   body, so pressing OK finds the shim's meta, TMDB and stream caches warm.

The screen cannot paint until step 6 returns, because cast, trailer and logo all ride in the same
JSON. The 800 ms of step 5 is inside that JSON too.

Every external call in steps 4–6 is a cold handshake: `plx_net` builds and destroys a curl easy
handle per call on purpose — "A reusable handle (or a share/multi) would buy connection reuse"
(`rust-modules/net/src/net.rs:928`). There is no DNS cache either. On TV hardware one handshake is
roughly 150–400 ms; four to six of them is the observed 1–3 s.

Cloudflare/Cinemeta and TMDB latency are outside our control; the fix is to stop paying them at
click time.

---

## 2. Verified facts (checked 2026-11-02, not assumed)

- **Cinemeta already returns the TMDB id.** Live probes:
  - `https://v3-cinemeta.strem.io/meta/movie/tt0133093.json` → `"moviedb_id": 603`
  - `https://v3-cinemeta.strem.io/meta/series/tt0944947.json` → `"moviedb_id": 1399` (65 KB; it
    also carries all 128 episodes in `videos`).
  Our `Meta` model does **not** deserialize the field (`rust-modules/stremio/src/model.rs:160`),
  so it is dropped and `/find` is paid to rediscover it.
- **`ranked(max_wait = 0)` is safe.** The loop's deadline becomes `started + 0`, already past, so it
  returns whatever the slot has landed and does not block
  (`shim_pms.rs:2055`, deadline at `:2066`, break at `:2085`). The slot's background fetch keeps
  running (`:1990`), so the next request finds more.
- **The detail row already has a fallback Part.** When the stream list is empty and the slot is not
  done, `item_probed` emits one `BEST_PART` Media labelled `"Finding streams…"`, so the Play button
  always has something to start (`shim_pms.rs:3239`). This is exactly the zero-wait state.
- **Caches are memory-only.** Shim: meta 6 h / 400 entries (`shim_pms.rs:26`, `:1894`), streams
  30 min / 200 (`:33`, `:1969`), TMDB 15 min / 512 (`addons_tmdb.rs:12`, `:433`), account library
  60 s. Data layer: no parsed-detail cache at all — every fresh open calls `fetch_full`.
- **No single-flight.** Both the shim meta cache and the TMDB cache release their lock before the
  fetch, so two concurrent requests for the same title can both go to the network.
- **No metadata prefetch.** `RequestDetail` is emitted only on OK. `ui/src/dwell.rs` is hold-hint
  dwell, not networking.
- **Shim concurrency is fine.** One accept thread, one thread per connection, max 48
  (`rust-modules/pmsshim/src/shim_http.rs:71`, cap at `:17`). The loopback hops are not the cost.
- **`/allLeaves` already exists** (`shim_pms.rs:4312`), and seasons/episodes are built from the
  cached Cinemeta meta (`:4272`), so the show chain is cheap once the series meta is cached.

---

## 3. Why the other clients feel instant

They are not faster at fetching; they do not fetch at click time. Stremio's core asks for `meta`
when a card gains focus and keeps a persistent cache; Nuvio's JS layer keeps connections alive and
caches responses; their detail page paints from the catalog's preview immediately. Our fork does
all the work inside the click, over cold connections, with a memory-only cache.

---

## 4. The plan

### Stage 0 — make the split visible — **implemented**

`detail: … ms=` already existed (`metadata.rs:3760`) and is the number to compare across every
stage. It now carries the phases, and the shim prints its own side of the loopback:

```
detail: sid=0 rk=… … meta_ms=<fetch_detail> seasons_ms=<show only> streams_ms=<show backfill> related_ms=<related/extras> ms=<total>
detail: shim rk=… meta_ms=<row build minus stream wait> streams_ms=<ranked wait> tmdb_ms=<enrich_row> total_ms=<whole shim answer>
```

- App side (`fetch_full`, `metadata.rs:3760`): `meta_ms` is the one `fetch_detail` GET; `seasons_ms`
  and `streams_ms` are the show path's two extra GETs; `related_ms` is the extras/related block.
  All four are main-thread wall time, so they plus parsing account for `ms=`.
- Shim side (`shim_pms.rs:3945`, metadata route): `item_probed` now returns the time it spent in
  `ranked` (usually zero for a detail read since 1A), and `enrich_row` is timed separately. One key
  in the URL is a detail read; a CSV listing has no single split and is not logged.
- Measured in the WSLg simulator, empty shim cache, boot straight to a detail: `detail: shim
  rk=10066 meta_ms=118 streams_ms=0 tmdb_ms=948 total_ms=1066` and the app's `meta_ms=1068 …
  ms=1072`. The same title re-opened: shim `total_ms=0`, app `ms=2`. **TMDB enrichment, not the
  addon meta fetch or the stream slot, is what a cold detail pays.**
- Read it on the TV with `./tools/ai-tv-log.sh grep "detail:"`; one grep shows both lines.

### Stage 1 — the three changes for tomorrow (the big win)

Keep each change a separate commit so it can be reverted alone.

#### 1A. Stop waiting 800 ms for streams on a detail read — **implemented**

**Where:** `rust-modules/pmsshim/src/shim_pms.rs:3268`.

The play path (`probe_index.is_some()`) keeps waiting the full `stream_deadline`. The detail read
passes `Duration::ZERO` (`DETAIL_WAIT` is gone), so it paints immediately from whatever the slot
has landed — usually the placeholder Part.

```rust
let wait = if probe_index.is_some() {
    self.cfg.stream_deadline
} else {
    Duration::ZERO
};
```

- The background slot fetch still runs, so a follow-up play or a second open is a cache hit.
- If the stream list is empty and not done, the row still gets `"Finding streams…"` as its Part
  (`:3239`) and Play still works (`BEST_PART`).
- **Visible tradeoff:** a cold detail page may show the generic play-mode capsule instead of
  "Direct Play · 1080p". 1C (prefetch) makes this rare, which is why the pair ships together.
- **Test:** shim test that a detail response for a slow/never-answering addon returns promptly
  (well under 100 ms) with the placeholder Part; a second request after the slot lands returns
  the real Media. Use the fake backend, not a socket.
- **Check first:** what the detail screen reads from `Media[]` (`ui::detail::draw_play_mode`,
  hero chips). If an empty/placeholder Part hides something important, either keep a small
  `DETAIL_WAIT` (e.g. 150 ms) or ensure the screen refreshes when streams land.

#### 1B. Use Cinemeta's `moviedb_id` instead of `/find` — **implemented**

`Meta` now decodes Cinemeta's `moviedb_id` (`rust-modules/stremio/src/model.rs:198`):

```rust
/// TMDB's id, carried by Cinemeta. Saves one `/find` round trip per title.
#[serde(rename = "moviedb_id", deserialize_with = "crate::lenient::opt_u64")]
pub moviedb_id: Option<u64>,
```

`resolve_tmdb_movie_id` / `resolve_tmdb_series_id` (`rust-modules/pmsshim/src/shim_pms.rs:801`,
`:821`) take it first and only fall back to the client lookup (then to the IMDb id):

```rust
fn resolve_tmdb_movie_id(client, id, imdb_id, moviedb_id: Option<u64>) -> … {
    if let Some(id) = moviedb_id.filter(|id| *id > 0) { return Ok(Some(id)); }
    // client.movie_id(id), then the imdb fallback
}
```

`enrich_row` and both related-hub resolvers pass `meta.moviedb_id`. `/find` remains the fallback
for addons that do not send it, and a `tmdb:` id still needs no lookup at all.

- **Test:** `a_meta_that_names_its_tmdb_id_skips_the_find_lookup` decodes the field (number and
  string shapes), resolves it through a lookup that panics if consulted, and proves a meta without
  it still uses the lookup. Both take a `TmdbIdLookup`, implemented by `TmdbClient` and by the
  test stubs.
- **Docs:** `docs/nuvio/TMDB.md` §"One request per title" and the field-mapping list are updated.

#### 1C. Prefetch the focused card (the felt fix) — **implemented**

**Goal:** by the time OK is pressed, the shim's meta / stream / TMDB caches are already warm, so
the detail request is a cache hit. This is what makes the other clients feel instant.

Implemented as "warm the caches, not the UI state":

- `MetadataCmd::WarmDetail { sid, rk }` (`rust-modules/data/src/stores/metadata.rs:175`).
  `metadata::run` dispatches it to `warm_detail` (`rust-modules/data/src/metadata.rs:4053`), which
  issues the same shim GET as `fetch_detail` (`client_for(sid)?.metadata(rk)?`) on a short-lived
  `detail-warm` worker and **drops the body**. It installs no `Detail`, does not touch `current()`
  and does not move the generation counters.
- Dedup is `WarmBook` (`metadata.rs:165`), a `(sid, rk) → Option<Instant>` map on the
  `MetadataAdapter`: `None` while a GET is in flight, then its finish time, with a 60 s TTL. A
  focus that jitters over a shelf asks once, not once per frame, and `Reset` (a profile switch)
  rotates it with the rest of the adapter. A refused worker spawn releases the key, so a transient
  `RLIMIT_NPROC` does not hide the card for the TTL.
- Home triggers at `HomeScreen::tick_prefetch` (`rust-modules/screens/src/home/mod.rs:2428`): the
  focused card's `(sid, rk)` with a **750 ms** dwell (`PREFETCH_DWELL_S`), reset on every focus
  move, one command per rest. It pushes the existing store effect — no `screens → pmsshim` edge.
  The dwell is a `Tick` timer (`PrefetchDwell`), not hashed logical state. Scoped to Home; the
  Library grid is the same helper in a follow-up.
  **Why 750 and not 250 (2026-11-04):** 250 ms sat inside the remote's own scroll rhythm. The
  device log showed a `detail: warm … — focus prefetch` line per card on every pass through six
  or seven Home rows — each one a shim GET that enriches the row (addon meta + TMDB, `tmdb_ms`
  1244–2589 in the same log) for one to three seconds while the next frames were drawn. A scroll
  tap rests on a card for ~300–500 ms, so the dwell must outlast a scroll step; a viewer who
  reads before pressing OK rests well past 750 ms and still gets the warm.
- A warm command raises no store notice (the `run` arm treats it like an optimistic view edit
  that changed nothing), so it cannot relayout a mounted Detail page.
- **Why warm the shim rather than `fetch_full` into a data cache:** no new landing/refresh state,
  no risk of replacing an open detail, and the shim caches are the expensive part. The app still
  re-runs `fetch_full` on OK, but every hop is local.

**Cost:** one Cinemeta meta (a series can be ~65 KB), one TMDB details, one stream fan-out per
focused card the viewer rests on. Bound it: one card, 750 ms dwell, one ask per key per 60 s.
**Test:** `warm_detail_admits_one_get_and_lands_nothing` (data) pins the dedup, no `current()` and
no loading movement; `the_warm_book_admits_one_fetch_per_key_per_ttl` pins the TTL and per-key
independence; `a_focused_card_warms_its_detail_once_it_rests` (Home) pins one command per rest,
none before the dwell, and a restart on focus move.
**Acceptance (the real one):** on the TV, rest on a card for a second, press OK → the `detail`
line's `ms=` drops to roughly a warm open. Compare against pressing OK immediately after moving
focus (cold).

### Stage 2 — stop losing the cache (~1 day)

- **2A. Persist the shim meta cache — implemented.** `MetaCache` (`shim_store.rs`) replaces the
  bare `metas` map. Rows are appended as they land to `meta-cache.jsonl` beside `keys.tsv`
  (one JSON line per fetch; the last line for a key wins at load, and a torn last line is
  skipped), so the write is small and runs on the shared persistence worker. The TTL is per kind:
  a series row lives 6 h because its `videos` array IS the episode list, while a movie row keeps
  30 days because it does not change once released. The in-memory map keeps 400 rows (oldest
  fetch evicted first), and once the log passes 8 MiB it is rewritten down to the newest rows
  under 4 MiB. `clear()` (the addons-changed path) removes the file too. **The boot path had to
  learn the difference
  between applying an addon override and changing one**: `app/nuvio.rs` applied the session's
  enable/disable overrides and called `addons_changed()` unconditionally, which wiped the rows
  it had just loaded. It now replaces the set and clears only when a flag actually changed.
  The boot log says `shim: meta cache warm-start with N row(s), M unreadable`, which is the line
  to read on the television before opening anything.
- **2B. Single-flight the meta and TMDB fetches — implemented.** `plx_base::flight::Flight`
  (`rust-modules/base/src/flight.rs`) is a per-key gate: one caller leads the fetch, the rest wait
  and then re-read their cache; a failed or panicking lead drops the claim and releases the next
  caller. Both caches now use it — `Shim::meta` (`shim_pms.rs`) and `cached_json`
  (`addons_tmdb.rs`), whose fetch closure is `FnMut` so the waiter can retry. Measured in the
  simulator, cold: the focus prefetch's `detail: shim` line took `tmdb_ms=732 total_ms=732`; the
  click that arrived 350 ms later took `tmdb_ms=355 total_ms=355` (the wait for the fetch already
  running) and the app's `ms=359` — instead of a second full TMDB fetch and the `ms=832` the
  Stage 1 run recorded for the same overlap.
- **2C. Make the IMDb→TMDB mapping permanent.** If 1B leaves any `/find` calls, cache the mapping
  without a 15-minute TTL — it never changes. A small persisted `imdb → (movie_id, tv_id)` map is
  enough.
- **2D. Persist the TMDB enrichment — implemented.** The Stage 0 split put `tmdb_ms=948` of
  `total_ms=1066`; after 2A `tmdb_ms=932` of `ms=936`. `enrich_row` was the whole cold open, so
  its answer is now kept: `EnrichCache` (`shim_store.rs`, `enrich-cache.jsonl` beside
  `meta-cache.jsonl`, same append-only log, 30-day TTL) stores one entry per `(type, id)` with
  everything the detail page needs from TMDB — the `Role[]` and `Extras` it writes, **the
  `More Like This` titles**, and the franchise shelf's films. A hit applies the two row fields and
  returns without constructing a `TmdbClient`; `related_hub` answers from the same entry, so the
  shelf that follows the row is free too. Measuring only the row was not enough: after the first
  cut, a restart went `tmdb_ms=0` but the page still paid ~950 ms in `related_ms`, because the
  related route re-fetched the same TMDB details the enrichment had skipped. The related titles are
  kept **as TMDB returned them**, not as rendered rows, and rebuilt per request — a row builder is
  what registers the fallback `ratingKey` a click on that card needs, so a stored row would be a
  dead card after a restart. Only a complete TMDB exchange is stored (no key, an unresolved id, or
  a failed call falls back to the Cinemeta trailer and retries next time); an entry without the
  related list is treated as incomplete and refreshed, so a build that predates the shelf
  converges after one open. The franchise's films are attached by the related route once it
  fetches them, which is also where that call already happened. The TMDB client's own 15-minute
  response cache stays memory-only; this is the parsed result, not an HTTP cache. Measured in the
  simulator after a restart: `detail: shim … meta_ms=0 streams_ms=0 tmdb_ms=0 total_ms=0`,
  the app's `meta_ms=1 related_ms=1–3`, and a whole open at `ms=2–6` for a movie, a franchise
  member (shelf intact, `collection=3`) and a series. Tests: an entry survives a restart, expires
  with the TTL, `enrich_row` and `related_hub` answer with no TMDB key (the test binary has none),
  and an old-shape entry is not taken as the answer.

### Stage 3 — transport and shows (optional, ~2–3 days)

- **3A. Connection reuse in `plx_net`.** ✅ Implemented as a process-wide `CURLSH` share (DNS +
  connection + SSL session caches). The share's caches are the only thing shared: the easy
  handle stays per call, so headers, `CUSTOMREQUEST` and the token a request carries never
  bleed into another caller's. Key mode and the lab's `Tls::Pinned` never attach
  (`FORBID_REUSE`/`FRESH_CONNECT`/session-cache-off per `keypin::apply`); a libcurl that
  refuses the share gets today's per-call behaviour, not a failure. New `curl_share_*` symbols
  joined the `dynlib!` table (libcurl 7.9, three years before the 7.53.1 floor). Benefit is
  app-wide (hub reads, posters, player), not just detail. TV measurement still owed (re-measure
  a cold `tmdb_ms` after a restart).
- **3B. Collapse the show chain.** The shim already answers `/library/metadata/{rk}/allLeaves`
  (`shim_pms.rs:4312`) from the cached series meta. Teach the data layer to read all episodes in
  one hop instead of seasons → chosen season → episode metadata (`metadata.rs:3734` onwards),
  when the season count is above a threshold. Keep the current path for very long shows if the
  JSON is large.
- **3C. Parallelize the show metadata chain** if the log still shows it serial after 3A/3B. The
  movie path is already 2 serial hops; the show path is 5 (`metadata.rs:3702`).

---

## 5. Verification

- **Host (Stage 0 update):** `make test-crate C=plx_pmsshim` (65 passed); the shipping feature
  check and `make lint`'s clippy gate were re-run. `make lint` still fails on
  `rust-modules/src/app/input.rs:1493` (`if_same_then_else` on an untouched `if` chain) — a
  pre-existing red on the committed tree, not this diff.
- **Simulator (`ui-sim`), run 2026-11-03 on WSLg** (host network — correctness and relative
  timings only, never a TV number): prefetch fires on focus rest (`detail: warm rk=… — focus
  prefetch`); a warm open after it completed logged `ms=7` and `ms=5`; a cold boot straight to a
  detail (`plxnative-detail=10066`, empty shim cache, no prefetch) logged `ms=828` with `subs=0`,
  and the page painted (screenshot: logo, cast, `Direct Play`) — under the old code that open also
  paid the stream wait. One open where the prefetch GET was still in flight logged `ms=832` with
  `subs=0 audio=0`: prefetch start is not single-flight yet (Stage 2), and the read still paid
  only the meta/TMDB path. The Stage 0 split on the same cold open: shim `meta_ms=118 streams_ms=0
  tmdb_ms=948 total_ms=1066`, app `meta_ms=1068 … ms=1072`; re-opened warm: shim `total_ms=0`,
  app `ms=2`. The Stage 2B single-flight was then measured on the overlap it targets: `back` to the
  rail, `right`, OK 350 ms later, cold — the prefetch's shim line `tmdb_ms=732 total_ms=732`, the
  click's `tmdb_ms=355 total_ms=355`, app `ms=359`. With the Stage 2A warm start, a restart that
  boots straight to the same detail loads 72 rows (0 unreadable) and answers `meta_ms=0`; the
  remaining `tmdb_ms=932` of `ms=936` was TMDB, then not persisted. Stage 2D moved that too: after
  the enrichment cache, a restart of the same movie answers shim `meta_ms=0 streams_ms=0 tmdb_ms=0
  total_ms=0` and app `ms=3` (a second open `ms=2`), a franchise member (The Matrix) restores its
  cast, `More Like This` and the 3-film collection shelf at `ms=5` (cold, its first open:
  `meta_ms=96 tmdb_ms=825 related_ms=601 ms=1525`), and a series comes back at `ms=6` with its
  season/episode chain intact. Host tests and the sim target build are green; the TV measurement
  below is still owed.

  Recipe (one runtime root, WSLg display exported as the `ui-sim` skill says):

  ```sh
  D=/tmp/sim-detail; make -j10 sim-linux
  PLXNATIVE_APP_DIR=$PWD/pkg PLXNATIVE_RUNTIME_DIR=$D \
    rust-modules/target-sim/release/plxnative-sim &   # sign in once; session persists
  # warm: rest focus >1 s (watch for `detail: warm rk=`), then
  exec 3<> "$D/plxnative-remote"; printf 'down ok ' >&3
  # cold: stop, boot straight to a detail with an empty shim cache, then remove the trigger
  printf '10066' > "$D/plxnative-detail"             # any ratingKey from keys.tsv
  rg "detail:" "$D/nuvionative-events.log"             # warm lines, the split, and `ms=`
  ```
- **TV:** acquire the `tv-lock`, deploy a debug build, and measure the `detail:` pair:
  1. cold: relaunch, open a card immediately → `ms=` and the shim's `meta_ms/streams_ms/tmdb_ms`;
  2. warm: rest on a card > 1 s, then OK → `ms=`;
  3. leave and re-enter the same detail → `ms=`.
  Acceptance targets: warm open **≤ 300 ms**, cold open **≤ 800 ms** on the set (pick the numbers
  after Stage 0 shows the real split). Playback must still start and auto-play as before; the
  `"Finding streams…"` Part must play via `BEST_PART`.
  **Use a guest/non-writing profile. Opening a detail page is a read, but do not press Play, add
  to My List or change progress on a real account.** TV logs are read-only through
  `./tools/ai-tv-log.sh`.
- **Regression watch:** detail page's play-mode capsule and hero chips when Media is the
  placeholder; person pages and collections (they share the TMDB resolvers); watchlist state.

## 6. Non-goals / do not do

- Do not drop TMDB enrichment. Cast headshots, trailer, logo and "More Like This" are product
  features; the fix is to move the work off the first paint, not to remove it.
- Do not rewrite the PMS dialect or the shim. The UI port is the reason the fork exists.
- Do not merge `upstream/feature/v0.9-hq` wholesale for this. Upstream has no prefetch either, and
  its detail path is PMS-native; take fixes (e.g. the poster eviction order, `#1105`) by
  cherry-pick, not by merge.
- Do not hold a mutex across a network call; the shim's meta and TMDB caches deliberately do not
  (`shim_pms.rs:1894`), and the account library is the known exception (`shim_account.rs:215`).

## 7. File map

| Concern | Where |
|---|---|
| Detail request / fetch chain | `rust-modules/data/src/metadata.rs:3760` (`fetch_full`), `:4016` (`request_detail`), `:4053` (`warm_detail`) |
| App-side split log | `rust-modules/data/src/metadata.rs:3870` |
| Detail screen's request | `rust-modules/screens/src/detail/mod.rs:2099` |
| Shim detail route + Media | `rust-modules/pmsshim/src/shim_pms.rs:3751`, `:3237` (`item_probed`), `:3239` (placeholder) |
| Shim-side split log | `rust-modules/pmsshim/src/shim_pms.rs:3945` |
| Detail stream wait (zero) | `shim_pms.rs:3268`, `:2138` (`ranked`), `:2055` (`slot`) |
| TMDB enrichment | `shim_pms.rs:2386` (`enrich_row`), `:2502` (`related_hub`), `:2663` (`collection_hub`), `:788`/`:803` (resolvers), `stremio/src/addons_tmdb.rs:289`/`:378`/`:433` |
| Cinemeta + addon meta selection | `rust-modules/stremio/src/addons.rs:21`, `:659`, `:730` |
| Meta model (`moviedb_id`) | `rust-modules/stremio/src/model.rs:160` |
| HTTP transport (no reuse) | `rust-modules/net/src/net.rs:928`, serial fallback `:2164` |
| Home focus / tick / prefetch | `rust-modules/screens/src/home/mod.rs:1641`, `:2407`, `:2428` (`tick_prefetch`) |
| Single-flight primitive | `rust-modules/base/src/flight.rs` (`Flight::claim`), used by `shim_pms.rs` `meta()` and `addons_tmdb.rs` `cached_json` |
| Persisted meta cache | `rust-modules/pmsshim/src/shim_store.rs:296` (`MetaCache`), file `meta-cache.jsonl` in the shim data dir; boot override applied in `rust-modules/src/app/nuvio.rs:239` |
| Persisted TMDB enrichment | `rust-modules/pmsshim/src/shim_store.rs:542` (`EnrichCache`, `Enrichment`, `RelatedSource`), file `enrich-cache.jsonl` beside it |
| Design docs | `docs/nuvio/ARCHITECTURE.md`, `ID-MANAGEMENT.md`, `TMDB.md` |

## 8. Working state (2026-11-03) — resume point

- **Branch:** `forward-port/v0.9-hq`, not pushed. Stage 0 commit: `8a8ffb51`; Stage 1 commits:
  `ca2de133` (1A), `a2b43791` (1B), `e270e61f` (1C), plus docs `01cacbb9`, `04af6099` and
  `4c1cf564`; Stage 2B is `becd2905`; Stage 2A is `b1873d09`; the per-kind TTL (movies 30 days,
  series 6 h) is `64f565d6`; Stage 2D is the commit titled `perf(detail): keep the TMDB enrichment
  and its shelf across a restart`.
- **Verified:** host suites and the shipping feature check are green; the WSLg simulator showed
  prefetch firing, warm opens `ms=5–7`, a cold boot-to-detail `ms=828` with `subs=0`, and the
  Stage 0 split putting ~95% of a cold shim answer in TMDB enrichment (`tmdb_ms=948` of
  `total_ms=1066`). With single-flight, the cold overlap (OK 350 ms after focus) went from the
  Stage 1 `ms=832` to `ms=359`. With the persisted meta cache, a restart's `meta_ms` is 0. With
  Stage 2D as well, a restarted app opens the same movie at `ms=3`, a franchise member at `ms=5`
  with its collection shelf, and a series at `ms=6` — shim `tmdb_ms=0 total_ms=0` in every case.
  A title the shim has never enriched is the cold case: 1525 ms for The Matrix (96 meta + 825
  TMDB + 601 collection/details), i.e. the first open still pays TMDB once and everything after
  it is local. TV is not measured.
- **TTL policy (settled):** storage is not the constraint — the log is bounded regardless — so
  the TTL is about staleness. A movie row keeps 30 days; a series row keeps 6 h because its
  `videos` array is the episode list and must refresh as episodes air. The 2D enrichment uses 30
  days for both, since cast, trailer, recommendations and a franchise are stable.
- **Both files are disposable:** deleting `meta-cache.jsonl` or `enrich-cache.jsonl` from the
  shim data dir only makes the next opens cold again; nothing references them by identity.
- **Next:** the owner's TV run is the gate (§5): warm open **≤ 300 ms**, cold open **≤ 800 ms**.
  Read `shim: meta cache warm-start` / `shim: enrich cache warm-start` first; a slow warm open on
  the set points at the title not being enriched yet (`related_ms` pays TMDB — open it twice), at
  the prefetch dwell, or at Stage 3A's handshakes. Any `/find` line in the log means 2C; a show
  whose chain is still serial in the log means 3B/3C. Do not push until the owner asks.

## 9. Open questions to settle before coding

1. What exactly does the detail screen read from `Media[]`, and what does it show when the row
   carries only the `"Finding streams…"` Part?
2. Does every meta addon we ship (not only Cinemeta) send `moviedb_id`? If not, the `/find`
   fallback must stay, and 2C matters more.
3. Is `moviedb_id` ever a string in any addon's JSON? `opt_u64` should handle the normal cases;
   check a sample of the installed addons.
4. **Settled 2026-11-04: 250 ms was the wrong threshold on the TV.** The remote's repeat rate
   makes a scroll step rest ~300–500 ms on each card, so every step admitted a warm and the
   enrichment (TMDB 1–3 s) ran continuously under a Home scroll (see Stage 1's dwell note). The
   dwell is now 750 ms and `a_scroll_through_cards_does_not_warm_each_one` pins it: at 250 ms
   that test admits exactly one warm per card (`rk` 1…5).
5. On the target firmware, is libcurl/OpenSSL thread-safe (does `threaded_tls_ready()` hold)? This
   decides whether 3A is a `CURLSH` change or also needs the serial fallback kept.
6. Does the TV's network really pay one handshake per call, or does the OS/the addons use HTTP/2?
   Stage 0 should show `tmdb_ms` and `meta_ms` separately to confirm.
