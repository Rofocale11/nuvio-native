# Trailers in Nuvio Native

How a YouTube trailer becomes a playable clip, how the detail page previews it, and how the
one-shot "Play Trailer" press differs from autoplay. Written against `564b011cb` from the code
that owns each step; when a constant below changes, change the code first and then this file.

This document describes **fork behaviour**. The PMS-shaped `ratingKey`, `Media`/`Part`, `/extras`
and `/library/parts` vocabulary is a compatibility boundary, not a Plex feature. Companion
documents: `PLAYBACK.md` (probe, route and decision), `ID-MANAGEMENT.md` (synthetic key ranges),
`TMDB.md` (where the video ids come from), `DETAIL-LATENCY.md` (why resolution moved off the
detail open) and `docs/trailer-ux-plan.md` (the original UX investigation, not the shipped spec).

## The pipeline in one line

```
TMDB videos / Cinemeta meta
  → candidate YouTube ids (shim_pms::trailer_candidates)
  → system Node child (shim_trailer + js-service/trailer-decipher.js)
  → TrailerResolved: DASH video+audio pair, or 360p muxed fallback
  → probe + tail-reach gate (shim_pms::publish_trailer)
  → clip row with own ratingKey and Part ids (build_trailer_row)
  → GET /library/parts/<id> (MPD 200 or muxed 302)
  → plx_nuvio_dash parser + play-feed's DASH demux over fMP4
  → Feed preview session (autoplay) or a full play with TRAILER_CONTEXT
```

There is no third-party trailer proxy in the byte path and no UA spoof for googlevideo. The
signed URLs are handed to the transport as they came from YouTube; the shared HTTPS Range
transport fetches the MPD's byte ranges. `js-service/trailer-proxy.js` and
`shim_trailer::TrailerProxy` are still in the tree but are **unused for track bytes** —
`publish_trailer` passes no BaseURL overrides and `dash_urls()` has no production caller. Do not
"fix" a 403 by reintroducing a proxy hop.

## 1. Finding the trailer ids

`Shim::trailer_candidates(rk, with_alternates)` in `rust-modules/pmsshim/src/shim_pms.rs` builds an
ordered, de-duplicated list of `(youtube_id, title)`:

1. **TMDB `videos`** — trailers first, then teasers, only `site == "youtube"`. It reads the
   `videos` answer already cached in the fork-owned enrichment row first; a network fallback is
   only used when that row has no videos and never under `cfg(test)`. An empty title becomes
   "Official Trailer".
2. **Cinemeta meta** — `trailer_streams` and `trailers` from the title's meta row, with its own
   `name`/`title` as the label. For a real key this is always consulted; for a synthetic key only
   when TMDB named nothing, so a related-show key does not pay a live `tmdb:` meta miss per Part.
3. **IMDb fallback** — when nothing else was found and the meta has a different `tt…` id, the
   same title's IMDb-keyed meta is tried.

`playable_candidates` filters out ids in the resolver's blocked map and takes at most
**`MAX_TRAILERS = 12`**. `push_candidate` dedupes by id. `docs/nuvio/MERGE-AUDIT.md` records why
the enrichment-cache read is a fork edit.

## 2. Keys and Part ids

| Value | Rule | Where |
|---|---|---|
| Clip `ratingKey` | `5_000_000_000 + parent + index * 10_000_000_000` | `trailer_rk_at`, `TRAILER_KEY_BASE`, `TRAILER_KEY_SPAN` |
| DASH Part id | `clip * 1000 + 998` | `build_trailer_row` |
| Muxed Part id | `clip * 1000 + 999` | `build_trailer_row` |
| Part → clip | `pid / 1000`, `pid % 1000` in `{998, 999}` | `trailer_part_at` |
| Shelf `guid` | `nuvio://trailer/<parent>` for index 0, `nuvio://trailer/<parent>/<index>` after | `trailer_shelf_row_at` |

The clip is a **separate PMS row** with its own `Media`, `Part` and `/decision`; it never resolves
the film's ranked addon streams. The 5B base is deliberately above the synthetic title, person
and collection ranges, and the 10B span keeps the twelve clip slots from aliasing a parent key.
Part ids are bounded well below `i64::MAX` (there is a debug assertion for it).

## 3. Resolving the id (the slow half)

`rust-modules/pmsshim/src/shim_trailer.rs` defines the `TrailerResolver` seam. Production uses
`NodeTrailerResolver::default_paths()`:

- **One short-lived child per resolve.** System `/usr/bin/node` (fallback `node` on `PATH`, Node
  16.20.2 floor) runs `js-service/trailer-decipher.js`, found beside the binary under
  `js-service/`. Stdout is one JSON line; stderr is log-only; 10 s wall-clock cap. A spawn
  failure, non-zero exit, timeout or unparseable answer becomes `None`, never an error string
  containing a URL.
- **Pacing.** A process-wide gate holds one spawn at a time with at least 300 ms between starts;
  holding it across the child avoids a burst of youtubei POSTs throttling the TV's IP into zero
  formats.
- **Device ceilings.** `with_device_limits()` reads `plx_os::devcaps`: `h264` is always offered,
  VP9/AV1 only when that row exists, and `PLX_TRAILER_MAXH` is the device's highest row clamped
  to 1080–2160. `extract_error` and `log_excerpt` keep reasons short and URL-free.

`js-service/trailer-decipher.js` fetches the youtube.com page with a Safari UA to get
`visitorData`, then POSTs `youtubei/v1/player` with the **VISIONOS** client and that visitor
identity. That is the one anonymous client measured to serve adaptive tracks to their **end**;
every other anonymous client grants only the first 60 s. Without `visitorData`, VISIONOS itself
answers `LOGIN_REQUIRED`. The child ranks DASH pairs by height (VP9/AV1 above h264 where the set
can decode them), prefers the original audio track over auto-dubs (`audioIsDefault`, and never an
`isAutoDubbed` entry), and falls back first to IOS adaptive and then to the ANDROID muxed
itag-18/360p file. It does not run a `signatureCipher` decipher: no measured client returns one
that needs it. Only built-in `https`, no npm deps, no disk write.

**Caching and age gate** (`shim_trailer::TrailerCache` / `resolve_cached`):

| Item | Value |
|---|---|
| Cache entries | 256 max; at the cap, expired rows go first, then the oldest surviving id |
| Positive answer | 30 min (signed URLs live for hours) |
| Negative answer | 60 s (a blip must not freeze a tile for half an hour) |
| Single-flight | `Flight` on youtube id, like the addon meta loop |
| `LOGIN_REQUIRED` id | Blocked for `BLOCK_TTL = 6 h`; the shelf walks to the next candidate |

The 256 cap is a conservative bound on a low-RAM television, not a URL or memory hard limit —
each answer holds a few signed strings. It used to `clear()` the whole table at the cap, so
roughly 21 titles (12 candidates each) threw away every still-valid 30-minute answer and made the
next browse re-pay a Node spawn per trailer. Eviction is now one id at a time.

The block map is checked in `playable_candidates` as well as at resolve time, so a known
age-gated trailer is never offered as a tile that fails on OK.

## 4. Choosing and validating a rendition

`trailer_shelf_rows(parent)` builds every tile **without resolving anything** — `/extras` is on
the detail page's critical path and used to spawn a Node child per title (a measured 3–5 s open
against a 1 s baseline). Each shelf row advertises a deterministic DASH Part key with a
placeholder 1080/h264 row and no `Stream[]`; the real row is built when that Part is requested.

`prefetch_trailer(parent)` then warms only **index 0** in a background `trailer-warm` thread
(no-op under test, at most 64 parents remembered) because the detail screen starts autoplay after
2 s. `trailer_row` is the autoplay resolver:

- It resolves up to **`TRAILER_QUALITY_SCAN = 2`** candidates and stops early at the device
  ceiling — a title's first TMDB video is often 1080p with the 4K cut second.
- `trailer_rung(width, height)` uses width as well as height, so a 3840×1600 trailer is still 4K.
  Rungs: 2160 / 1080 / 720 / 576 / 480.
- DASH candidates are sorted tallest-first and published at index 0; the first verified muxed
  row is the fallback only when no DASH survives.

`publish_trailer` runs two independent checks in parallel for a DASH pair:

1. `trailer_probe_dash_logged(video_url)` — `mp4`/`mov`/`webm`/`mkv` (WebM is the VP9/AV1 arm),
   h264/VP9/AV1 video, decodable. `mkv` must be allowed or every 4K pair is demoted.
2. `trailer_tracks_reachable(video, audio)` — with a boot-installed reacher, asks each track for
   its **last 64 KiB** within 6 s; anonymous clients that only serve the first 60 s pass the probe
   and then die mid-play, which is worse than a consistent 360p row. No reacher (host/tests)
   means the probe alone decides. The verdict is kept for **60 s** keyed by the pair, so a detail
   publish and the player's Part request do not repeat four googlevideo round trips; the short TTL
   still lets a URL that starts throttling be caught within a minute (fork change).

A known-bad DASH pair falls back to the verified muxed row, so autoplay still plays at 360p. A
muxed row itself requires `mp4`/`mov` + h264 + decodable AAC (`trailer_probe_ok_muxed_logged`).
The URLs are never logged; only verdicts are.

Index 0 is therefore *"the title's best-resolved trailer"*, not "candidate 0". `trailer_row_at`
resolves **exactly** the clicked index for every other tile and leaves its Part 404 rather than
silently playing a different trailer.

## 5. The clip row and the MPD

`build_trailer_row(parent, index, resolved, youtube_id, title, dash_urls)` emits a `type: "clip"`,
`subtype: "trailer"`, `extraType: 1` row with a YouTube `hqdefault.jpg` thumb, its own duration,
and one `Media` per surviving rendition:

- **DASH** — one part with both video and audio `Stream` rows, `addon_name: "YouTube"`, container
  `mp4` (h264/AV1) or `webm` (VP9), bitrate = video + audio, and resolution/frame-rate labels from
  the rungs above.
- **Muxed** — one part with h264/AAC streams at the resolved height.

`dash_manifest(d, video_url, audio_url)` in `shim_pms.rs` produces a **static SegmentBase MPD**:
two `AdaptationSet`s (video, then audio), each with one `Representation`, one `SegmentBase`
(init + index byte ranges) and a `BaseURL` that is the signed googlevideo URL (XML-escaped).
`GET /library/metadata/<rk>/extras` returns the shelf; `GET /library/metadata/<rk>/trailer`
returns the single resolved clip.

`GET /library/parts/<id>` answers an MPD with **200** and a muxed trailer with **302** to the
signed URL. It resolves a trailer Part on demand if the shelf row has not warmed it, before any
stream ranking.

The bundled FFmpeg has no `dash` demuxer, so `rust-modules/nuvio-dash` (`plx_nuvio_dash`, the
fork-owned strict parser) validates that exact MPD shape and
`rust-modules/play-feed/src/dash.rs` opens each track's BaseURL as a seekable progressive fMP4
source through the bundled `mov` demuxer — two independent format contexts, packets alternated
onto the two AU lanes. `dash.rs` is a child module of upstream `ff.rs`: the only upstream edits
are a `#[path]` declaration and the `.mpd` routing branch. There is no ABR; trailers are
direct-play. The manifest is capped at 1 MiB when fetched.

## 6. Two ways to play a trailer

### Background autoplay (the default)

`plx_media::player::preview` owns the session. `screens/src/detail/mod.rs` accumulates a dwell
while the hero is active, not promoted, no picture is playing, no preview occupies the engine,
and the item is neither blocked nor already played. At `DWELL_S = 2.0` it emits
`ContentReq::PreviewStart` with the extra's `rk`, `part`, codecs and title. Each accepted preview
spends one of the 14 process-lifetime admitted Loads; 0 would spend one on every browsed-past hero
and exhaust the budget for the run. A 1 s trial was reverted: the device log showed `preview: start`
while the prefetch's Node decipher was still in flight, so the shorter gate only moved the wait
into `dwell_ms` and spent the Loads sooner.

A preview shares the engine but **not** the session services: no PlayQueue, no timeline reporter,
no scrobble, no watch-state write. `/decision` still creates Activity, which is a server write but
not a watch-state write. The route context is `plx_data::metadata::TRAILER_CONTEXT`, which also
omits `continuous`. Startup priming uses the `Preview` profile (one accepted AU, no fill
requirement, no ceiling). Per-item negative facts (`NoExtra`, `RefusedDirect`, 64 entries) and a
process-wide breaker stop a known-bad trailer from being retried on every dwell.

**UP promotes the running preview to full-trailer mode** without a route change: all page chrome
fades out and `screens/src/detail/trailer.rs` draws the player HUD's own scrim/title/playbar.
Keys: OK / PLAYPAUSE / dedicated PLAY / PAUSE toggle playback; UP toggles the transport; LEFT and
RIGHT hold-to-scrub (a tap hops a skip interval, a held key ramps); DOWN and BACK collapse to
background. The transport auto-hides on the HUD's `LINGER_MS` unless paused. LEFT/RIGHT go through
`player::preview::seek`'s **admitted reload** accounting, never `player::request_seek`: an
un-admitted reload would spend a 64 KiB Starfish slot outside the cycle budget and a failure
would arm the breaker for every later item. The ceiling is `CYCLE_BUDGET = 14` admitted Loads,
derived from source arithmetic (958 KiB headroom / 64 KiB slot), **not** a TV measurement. Sound
stays on; a preview has no mute control.

### One-shot full trailer (item menu)

`Action::PlayTrailer` (`screens/src/item_menu.rs`) builds a `PlayIntent::Item` with
`TRAILER_CONTEXT` and goes through `app::content::clear_engine_for_play`, or holds it with
`hold_feature` when the menu press lands while the autoplay preview's Load still owns the engine.
That hold is deliberate: starting a second Load against an installed engine hits the
double-start guard and silently refuses.

**Held play supersedes the preview's in-flight resolve.** Without this, no `request_play` is
issued, `PLAY_GEN` does not advance, and the preview's late resolve is applied as a full play —
the observed `playing item: rk=<trailer>` followed by a 404 and fps 0 in the owner's event log
(lines 754–836 of `plxnative-events.log`). `hold_feature` now calls
`plx_media::route::cancel_play()` so the stale landing is dropped at the generation gate;
`route/decision_resolve_route_tests.rs` pins the regression.

## 7. Numbers worth knowing

| Constant | Value | Meaning |
|---|---|---|
| `MAX_TRAILERS` | 12 | shelf tiles per title |
| `TRAILER_KEY_BASE` / span | 5B / 10B | clip key space |
| `TRAILER_QUALITY_SCAN` | 2 | candidates resolved for the autoplay tile |
| `BLOCK_TTL` | 6 h | age-gated id skipped |
| positive / negative cache | 30 min / 60 s | 256 entries, single-flight |
| Node timeout / spawn gap | 10 s / ≥300 ms | one child at a time |
| tail-reach probe | last 64 KiB, 6 s | both tracks in parallel |
| `DWELL_S` | 2.0 s | hero stillness before autoplay |
| `CYCLE_BUDGET` | 14 | admitted Starfish Loads per process (source math) |

Useful event-log lines: `trailer: youtube ids from tmdb videos`, `trailer: node resolved`,
`trailer: youtube id is not anonymously playable`, `trailer: dash pair served to its end`,
`trailer: dash probe ok=…`, `trailer: muxed probe refusal`, `sbengine:`/player lines for the
preview's engine. The trailers themselves never appear in the log as URLs.

## 8. Verification status

- Host coverage: `plx_pmsshim` pins shelf rows built without a resolve, the quality scan and
  autoplay Part, the age-block walk, DASH/muxed probe fallbacks, direct BaseURLs, cache TTLs,
  single-flight and the log-excerpt rules. `plx_media` pins the held-play supersede. These are
  deterministic fakes — no YouTube or Node is contacted under `cfg(test)`.
- Device evidence from the owner's log established the failure sequence behind the held-play
  fix; the owner confirmed the deployed 0.10.0 stable build works after that fix (2026-11-25).
- **Owner-confirmed (2026-11-26):** a filmed device run of an actual trailer reaching its end at
  each rendition, and a measurement of the two-candidate quality scan's latency on the set, are
  verified. No command or log was attached to this document. VISIONOS "serves to the end" remains a
  measured 2026-11-06 host fact recorded in the decipher script's header, not a firmware matrix.
