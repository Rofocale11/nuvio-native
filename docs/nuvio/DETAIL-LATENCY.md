# Detail-page latency — current status

Fork Rofocale11. Scope: wall-clock from pressing OK on a Home/Library card to a painted detail
page on the TV. Not the player, Home's own first paint, or poster throughput.

This file supersedes the old fork's 452-line plan of the same name
(`/home/kungpow/dev/nuvio-native-old/docs/nuvio/DETAIL-LATENCY.md`). That document was a snapshot
of work on `forward-port/v0.9-hq`; its line numbers, commit ids and "next" list are stale. The
status below describes **this** tree. The old file remains the source of the problem analysis
(one shim request composites addon meta, TMDB enrichment and streams; per-call curl handles pay a
fresh DNS/TCP/TLS handshake; nothing was prefetched at focus).

## What is implemented here

| Stage | Where (current tree) | What it does |
|---|---|---|
| 0 — shim split | `pmsshim/src/shim_pms.rs` `detail: shim …` line | prints `meta_ms`, `streams_ms`, `tmdb_ms`, `total_ms` for the shim's half of the loopback |
| 0 — app split | `data/src/metadata.rs::detail_summary_line` | prints the same four phases on the app line: `meta_ms`, `seasons_ms`, `streams_ms`, `related_ms` before the total `ms=` |
| 1A — no stream wait on detail | `shim_pms.rs::item_probed` | a detail read passes `Duration::ZERO` to `ranked`; the Play button gets the `"Finding streams…"` placeholder Part while the slot fetches in the background |
| 1B — `moviedb_id` | `shim_pms.rs::resolve_tmdb_movie_id` / `…_series_id` | Cinemeta's TMDB id skips `/find`; the client lookup stays as the fallback |
| 1C — focus warm | `screens/src/home/mod.rs` (`WARM_DWELL_MS = 750`), `data/src/stores/metadata.rs` (`MetadataCmd::Warm`), `app/bridge.rs`, `app/adapters/stores.rs` | one background shim GET after a 750 ms focus rest, deduped per `(sid, rk)` for 60 s; installs no Detail and raises no notice; logs `detail: warm rk=… — focus prefetch` |
| 2A — persisted meta cache | `pmsshim/src/shim_store.rs::MetaCache` (`META_CACHE` in `nuvio.redb`) | movie rows keep 30 days, series 6 h; the old JSONL files are migrated once on first boot |
| 2A — boot clear guard | `shim_pms.rs::addons_changed` + `shim_backend.rs::Backend::addons_fingerprint` | re-applying the same addon set at boot no longer clears the just-loaded rows; a transport/enable/disable change still does |
| 2B — single-flight | `pmsshim/src/shim_flight.rs`, `stremio/src/addons_tmdb.rs` | one fetch per key; the prefetch and the click that follows share it instead of duplicating the TMDB GET |
| 2C — permanent id mapping | `nuvio-store` `imdb_tmdb` table, `pmsshim/src/shim_tmdb_ids.rs` | an IMDb id resolved through `/find` is stored as a plain BE u64 with no TTL; negatives are never cached |
| 2D — persisted enrichment | `shim_store.rs::EnrichCache` (`ENRICH_CACHE`) | cast, trailer, `More Like This` and franchise shelf survive a restart, 30-day TTL |
| 3A — connection reuse | `net/src/net.rs::share_handle` | one process-wide `CURLSH` sharing DNS and SSL-session caches across control-plane handles, with mutex callbacks; the connection cache is deliberately NOT shared (libcurl does not support that across threads); key mode, the lab's pinned transfers and resolve-pinned requests keep per-call handles |
| 3B — collapse the show chain | `plex/src/plex/nuvio.rs::all_leaves`, `data/src/metadata.rs::fetch_all_leaves` / `leaves_for_season` | the shim's `/library/metadata/{rk}/allLeaves` answers the opening season's page in one hop; answers over 2000 rows are refused and the paged `/children` path is used |
| 3C — parallel show chain | `data/src/metadata.rs::fetch_full` | seasons and all-leaves are `beside` siblings of the item GET; related, extras and the on-deck episode remain siblings too, so the page waits for the slowest chain rather than their sum |

The shim's season rows are still read from `/children` — leaves are episode rows only and cannot
supply the tab titles and viewed/leaf counts. Later episode pages still page through `/children`.
A non-Nuvio server keeps the original serial chain: `all_leaves` returns `None` there.

### A correction to the old fork's 3A

The old fork's curl constants were wrong and made its share a silent no-op: `CURLOPT_SHARE` was
spelled `10237` instead of `10100`, and `CURL_LOCK_DATA_*` values were passed as `CURLSHOPT_*`
option ids instead of as the third argument of `CURLSHOPT_SHARE`. Verified against curl 7.53.1's
`curl.h`/`share.c` and a host runtime probe. The values in this tree are corrected
(`DNS=3`, `SSL_SESSION=4`). A second correction followed device review: libcurl requires
`CURLSHOPT_LOCKFUNC`/`UNLOCKFUNC` whenever one share is used by several threads, and explicitly
says `CURL_LOCK_DATA_CONNECT` is unsupported between concurrent threads even with locks. This tree
therefore installs per-data mutex callbacks and shares only DNS and TLS sessions; per-call easy
handles still own their connections. Adding the `curl_share_*` symbols to the `dynlib!` table is
all-or-nothing: a libcurl without them would fail the whole table, but they date from curl 7.10,
well below the television's 7.53.1 floor.

## Logs to read on a run

```
detail: shim rk=… meta_ms=… streams_ms=… tmdb_ms=… total_ms=…
detail: sid=… rk=… show=… … extras=… meta_ms=… seasons_ms=… streams_ms=… related_ms=… ms=…
detail: warm rk=… sid=… — focus prefetch
shim: meta cache warm-start with N row(s), M unreadable
shim: enrich cache warm-start with N row(s), M unreadable
detail: rk=… /allLeaves answered with N rows (> 2000) — using /children for this show
```

A warm open is the case where the shim's meta/enrich caches already hold the title: both shim
phase times and the app's `meta_ms`/`related_ms` should be near zero. A cold first open still pays
TMDB once; the second open of the same title should not.

## Verification

Host, 2026-11-24, on the working tree (`2b7c2067e` plus these changes): `plx_net` 338 passed,
`plx_pmsshim` 158 passed, `plx_data` 1662 passed, `plx_nuvio_store` 4 passed, `plx_plex` 821
passed with 3 pre-existing lossy-gate `opus` profile-list failures; `make check-nodef` green;
`ci/check-module-layers.py` and `ci/check-deps.sh` green; `git diff --check` clean; ARM
cross-build green (`pkg/plxnative` is a 32-bit ARM ELF). A focused `plx_plex` re-run against the
unmodified `nuvio.rs` reproduced the same three failures, so they are not from `all_leaves`.

**First two device runs (2026-11-24, debug install).** The focus warm fired on every Home card
tested, with warm shim answers (`tmdb_ms=0…12`). The first run's single cold movie open logged
`related_ms=6342 ms=6357`; the second run's four opens logged `meta_ms=20…53` but
`related_ms=1315…1944`, consistently over target. Every slow open was preceded by **two**
`trailer: youtube ids from tmdb videos` lines. Those come from the shim's `/extras` route: both the
trailer shelf and the play-time alternate picker rebuild candidates, and each re-fetched the full
TMDB `movie_details`/`tv_details` object even though enrichment had already paid for it and kept
the rendered `Extras`. The `net: curl rc=28` subtitle-host timeouts in the same window were a
separate addon, not the cause.

Fix: `Enrichment` now persists the raw TMDB `videos` answer beside the rendered `Extras`, and
`trailer_candidates` reads it before any network call. Rows written before this change have no
`videos` and are treated as incomplete, so they refresh once. Host regression tests pin both the
cached-videos path and the migration rule.

**Verified on the TV (2026-11-24, debug install, after deploying that fix).** Movie re-opens and
series re-opens landed at `ms=84…282` with `related_ms=5…12` for movies and `43…233` for shows,
comfortably inside the ≤300 ms warm target. Series opens also confirmed plan 3B/3C on device:
`seasons_ms=12…81`, `eps=8`/`10`, no `/allLeaves` size-cap refusal. A title never seen before still
paid its one TMDB enrichment cost up front (`tmdb_ms=1005…2085`, `ms=1439…2061`) — that is the
expected cold first open, and it is over the ≤800 ms cold target. Part of that first open is the
one-time migration refresh of enrichment rows written before the `videos` field existed. The
`trailer: youtube ids from tmdb videos` log line is emitted for the cached path too, so its
presence alone no longer proves a network call; the phase times do.

**Not yet verified on the television.** The acceptance gate from the old plan still stands:
acquire the TV lock, boot with a guest profile, and compare a cold open (press OK immediately
after focus) with a warm one (rest on the card over a second, then OK) and a leave/re-enter of the
same title. Targets: warm open ≤ 300 ms, cold open ≤ 800 ms. Playback must still start through the
placeholder Part, and cast, trailer, logo and collection shelf must survive a restart. Opening a
detail page is a read, but never press Play or change a real account's state during this check.

## Non-goals

- Do not drop TMDB enrichment — cast headshots, trailer, logo and `More Like This` are product
  features; the work moves off the first paint, it is not removed.
- Do not rewrite the PMS dialect or the shim to chase these numbers.
- Do not hold a mutex across a network call.
- Host and simulator timings are relative evidence only; they are never TV numbers.
