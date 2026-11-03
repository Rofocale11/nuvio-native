# Detail-page latency — plan

**Status:** Stage 1 (1A–1C) implemented (2026-11-03), host + simulator verified. Written 2026-11-02 on
`forward-port/v0.9-hq` (`7e79e57e`).
**Owner:** whoever picks this up next.
**Scope:** wall-clock from pressing OK on a Home/Library card to a painted movie/show detail page,
on the television. Not the player, not Home's own first paint, not poster throughput.

## The problem

On the TV, a cold detail open takes roughly **1–3 s**. Other Stremio/Nuvio clients on the same
television, same network, feel instant. The fork's own log already records the total
(`detail: … ms=`, `rust-modules/data/src/metadata.rs:3805`), so measure before trusting any
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
3. `request_detail` spawns the `"detail"` worker (`rust-modules/data/src/metadata.rs:3950`,
   dispatched at `:4001`), which runs `fetch_full` (`:3699`).

Shim side, for `/library/metadata/{rk}` (`rust-modules/pmsshim/src/shim_pms.rs:3751`; the
single-key route falls through to the comma-list branch at `:3815`, which maps every key through
`self.item()` and `self.enrich_row()`):

4. `item()` → `item_probed()` (`:3264`, `:3151`) → `row()` → `meta()` (`:1894`) →
   `AddonBackend::meta` → `AddonSet::meta` (`rust-modules/stremio/src/addons.rs:730`) → **Cinemeta**
   `https://v3-cinemeta.strem.io/meta/{type}/{id}.json` (`addons.rs:21`). Candidates are tried
   **serially** (`addons.rs:659` order: id-prefix owners, then every addon serving the type, then
   the inferred type).
5. `item_probed` starts the stream slot (`shim_pms.rs:1961`) and — since 1A — **no longer blocks**
   on it: a detail read passes `Duration::ZERO` (`:3268`) and paints with the placeholder Part
   while the slot keeps answering in the background.
6. `enrich_row` (`:2285`, called at `:3829`) resolves the TMDB id and fetches cast/trailer/logo
   **synchronously**: since 1B the id comes from Cinemeta's `moviedb_id`, with `/find`
   (`rust-modules/stremio/src/addons_tmdb.rs:289` for movies, `:378` for series) only as the
   fallback; then `/movie/{id}?append_to_response=credits,recommendations,images` and
   `/movie/{id}/videos` on a sibling thread (`shim_pms.rs:2332`).
7. The app then requests `/extras` (local empty answer) and `/related` (TMDB recommendations,
   usually a cache hit from step 6; `/collection/{id}` when the film has a franchise), plus
   `/nuvio/library` for the watchlist state (60 s cache) and then the images.
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

### Stage 0 — make the split visible (not done; Stage 1 landed first on the owner's direction)

`detail: … ms=` already exists (`metadata.rs:3805`) and is the number to compare across every
stage. Extend it with the three phases so the TV, not inference, says what dominates:

```
detail: … meta=<ms> streams=<ms> tmdb=<ms> total=<ms>
```

- Time the `fetch_detail` call, the `fetch_item_streams`/`ranked` contribution, and the related/
  extras block. The shim can log its own side too (`meta_ms`, `streams_ms`, `tmdb_ms`) from
  `item_probed`; keep both — the app number includes loopback + parsing.
- Read it on the TV with `./tools/ai-tv-log.sh grep "detail:"`.
- Acceptance: one cold open after an app relaunch shows the split. Do not proceed on guesses.

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
  `metadata::run` dispatches it to `warm_detail` (`rust-modules/data/src/metadata.rs:4038`), which
  issues the same shim GET as `fetch_detail` (`client_for(sid)?.metadata(rk)?`) on a short-lived
  `detail-warm` worker and **drops the body**. It installs no `Detail`, does not touch `current()`
  and does not move the generation counters.
- Dedup is `WarmBook` (`metadata.rs:165`), a `(sid, rk) → Option<Instant>` map on the
  `MetadataAdapter`: `None` while a GET is in flight, then its finish time, with a 60 s TTL. A
  focus that jitters over a shelf asks once, not once per frame, and `Reset` (a profile switch)
  rotates it with the rest of the adapter. A refused worker spawn releases the key, so a transient
  `RLIMIT_NPROC` does not hide the card for the TTL.
- Home triggers at `HomeScreen::tick_prefetch` (`rust-modules/screens/src/home/mod.rs:2428`): the
  focused card's `(sid, rk)` with a **250 ms** dwell (`PREFETCH_DWELL_S`), reset on every focus
  move, one command per rest. It pushes the existing store effect — no `screens → pmsshim` edge.
  The dwell is a `Tick` timer (`PrefetchDwell`), not hashed logical state. Scoped to Home; the
  Library grid is the same helper in a follow-up.
- A warm command raises no store notice (the `run` arm treats it like an optimistic view edit
  that changed nothing), so it cannot relayout a mounted Detail page.
- **Why warm the shim rather than `fetch_full` into a data cache:** no new landing/refresh state,
  no risk of replacing an open detail, and the shim caches are the expensive part. The app still
  re-runs `fetch_full` on OK, but every hop is local.

**Cost:** one Cinemeta meta (a series can be ~65 KB), one TMDB details, one stream fan-out per
focused card the viewer rests on. Bound it: one card, 250 ms dwell, one ask per key per 60 s.
**Test:** `warm_detail_admits_one_get_and_lands_nothing` (data) pins the dedup, no `current()` and
no loading movement; `the_warm_book_admits_one_fetch_per_key_per_ttl` pins the TTL and per-key
independence; `a_focused_card_warms_its_detail_once_it_rests` (Home) pins one command per rest,
none before the dwell, and a restart on focus move.
**Acceptance (the real one):** on the TV, rest on a card for a second, press OK → the `detail`
line's `ms=` drops to roughly a warm open. Compare against pressing OK immediately after moving
focus (cold).

### Stage 2 — stop losing the cache (~1 day)

- **2A. Persist the shim meta cache.** Today a restart makes the first open cold again. Write the
  meta rows (type/id → parsed `Meta`, with fetch timestamp) to the runtime directory, bounded
  (e.g. 400 entries / a few MB, LRU), load at boot, honour `META_TTL`. Do not persist the raw
  `Value` stream slots. Where: the shim's store (`shim_store.rs`) or a small JSON cache beside
  `keys.tsv`; the runtime dir is per-instance, which keeps sim and TV separate.
- **2B. Single-flight the meta and TMDB fetches.** Per-key in-flight map + condvar so a prefetch
  and the real request share one network call. The current code checks-then-fetches with the lock
  released (`shim_pms.rs:1894`, `addons_tmdb.rs:433`).
- **2C. Make the IMDb→TMDB mapping permanent.** If 1B leaves any `/find` calls, cache the mapping
  without a 15-minute TTL — it never changes. A small persisted `imdb → (movie_id, tv_id)` map is
  enough.

### Stage 3 — transport and shows (optional, ~2–3 days)

- **3A. Connection reuse in `plx_net`.** Add a `CURLSH` share (DNS + connection) or a per-thread
  easy handle with explicit `curl_easy_reset` between calls; the current per-call handle is a
  deliberate anti-state design (`net.rs:928`) and can stay the fallback. Must verify the device
  libcurl/OpenSSL is thread-safe first — the serial fallback only applies to legacy OpenSSL
  (`net.rs:2171`). Benefit is app-wide (hub reads, posters, player), not just detail.
- **3B. Collapse the show chain.** The shim already answers `/library/metadata/{rk}/allLeaves`
  (`shim_pms.rs:4312`) from the cached series meta. Teach the data layer to read all episodes in
  one hop instead of seasons → chosen season → episode metadata (`metadata.rs:3734` onwards),
  when the season count is above a threshold. Keep the current path for very long shows if the
  JSON is large.
- **3C. Parallelize the show metadata chain** if the log still shows it serial after 3A/3B. The
  movie path is already 2 serial hops; the show path is 5 (`metadata.rs:3702`).

---

## 5. Verification

- **Host:** the crates the Stage 1 diff touches: `make test-crate C=plx_pmsshim` (65 passed),
  `C=plx_stremio` (57), `C=plx_screens` (1703), `C=plx_data` (1121 passed, 4 failed — all four
  reproduce on `5db3b9dc`: the two known concurrency reds plus
  `committed_fixture_detail_replies_stay_canonical` and `a_headshot_is_served_only_for_a_person`).
  Shipping feature set: `CARGO_INCREMENTAL=0 cargo +nightly check --manifest-path
  rust-modules/Cargo.toml --lib --no-default-features` passes. `ci/check-module-layers.py` still
  reports the 15 pre-existing unlayered `plx_data` modules (`catalog`, `viewledger`, `watchlist`, …)
  on both the base and this tree; the Stage 1 modules themselves are covered.
- **Simulator (`ui-sim`):** correctness and visible behaviour only. WSL/desktop timing is host
  network, not the TV, so do not report latency from it. Use it to confirm the detail page still
  paints, the placeholder Part still plays, and prefetch fires on dwell (`detail: warm rk=… —
  focus prefetch` in the event log). **Not run on the WSL checkout that implemented Stage 1** (no
  DISPLAY or GL stack there); the sim target is built, the behaviour is pinned by the host tests
  above, and the live check is owed on a simulator host or the TV.
- **TV:** acquire the `tv-lock`, deploy a debug build, and measure the `detail:` line:
  1. cold: relaunch, open a card immediately → `ms=`;
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
| Detail request / fetch chain | `rust-modules/data/src/metadata.rs:3699` (`fetch_full`), `:3950` (`request_detail`), `:4038` (`warm_detail`) |
| Detail screen's request | `rust-modules/screens/src/detail/mod.rs:2099` |
| Shim detail route + Media | `rust-modules/pmsshim/src/shim_pms.rs:3751`, `:3151` (`item_probed`), `:3239` (placeholder) |
| Detail stream wait (zero) | `shim_pms.rs:3268`, `:2055` (`ranked`), `:1961` (`slot`) |
| TMDB enrichment | `shim_pms.rs:2285` (`enrich_row`), `:788`/`:803` (resolvers), `stremio/src/addons_tmdb.rs:289`/`:378`/`:433` |
| Cinemeta + addon meta selection | `rust-modules/stremio/src/addons.rs:21`, `:659`, `:730` |
| Meta model (`moviedb_id`) | `rust-modules/stremio/src/model.rs:160` |
| HTTP transport (no reuse) | `rust-modules/net/src/net.rs:928`, serial fallback `:2164` |
| Home focus / tick / prefetch | `rust-modules/screens/src/home/mod.rs:1641`, `:2407`, `:2428` (`tick_prefetch`) |
| Design docs | `docs/nuvio/ARCHITECTURE.md`, `ID-MANAGEMENT.md`, `TMDB.md` |

## 8. Open questions to settle before coding

1. What exactly does the detail screen read from `Media[]`, and what does it show when the row
   carries only the `"Finding streams…"` Part?
2. Does every meta addon we ship (not only Cinemeta) send `moviedb_id`? If not, the `/find`
   fallback must stay, and 2C matters more.
3. Is `moviedb_id` ever a string in any addon's JSON? `opt_u64` should handle the normal cases;
   check a sample of the installed addons.
4. Is a 250 ms dwell the right prefetch threshold on the TV, and does the remote's repeat rate
   make focus jitter expensive?
5. On the target firmware, is libcurl/OpenSSL thread-safe (does `threaded_tls_ready()` hold)? This
   decides whether 3A is a `CURLSH` change or also needs the serial fallback kept.
6. Does the TV's network really pay one handshake per call, or does the OS/the addons use HTTP/2?
   Stage 0 should show `tmdb_ms` and `meta_ms` separately to confirm.
