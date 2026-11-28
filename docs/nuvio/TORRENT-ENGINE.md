# The torrent engine: sbengine and Nuvio Engine

What actually downloads a Stremio torrent on the TV, how the app talks to it, and where the
bytes and the resume state live. Written against `564b011cb` from the sources named below.

**Source of truth.** The engine is vendored at `vendor/engine-sources/nuvio-engine` (the
[Nuvio Engine](https://github.com/NuvioMedia/nuvio-engine) project, GPL-3.0-or-later). This app
does **not** link or call that project directly: it loads a small C wrapper,
`libsbengine.so`, built from `vendor/engine-sources/sbengine.cpp` and
`vendor/engine-sources/core/sbengine.h`. This document is derived from those checked-in sources
and headers.

```
plx_stremio::sbengine          Rust FFI, ONE dlopen symbol (sbe_get)
        ↓  sbe_api function-pointer table, SBE_API_VERSION 5
libsbengine.so                 C++ wrapper: torrent map, event loop, non-blocking calls
        ↓  nuvio_engine_* C API
nuvio_engine (static)          C++20 core: libtorrent 2.0, scheduler, cache, HTTP range server
        ↓
127.0.0.1 stream URL           consumed as a PMS Part redirect by the Feed demuxer
```

## 1. Build, loading and firmware posture

`ci/build-sbengine.sh` builds a staged CMake project: `nuvio_engine` as a static library plus
`sbengine.cpp` as a shared `libsbengine.so`. Pinned inputs recorded in
`rust-modules/stremio/src/sbengine.rs`: Nuvio Engine **0.1.5** (`99a1860`), libtorrent **2.0.12**,
Boost 1.86 headers, OpenSSL **1.1.1w** from `ci/build-openssl.sh`, NDK
`arm-webos-linux-gnueabi-g++` 12.2.0. Only `sbe_get` is exported (hidden visibility and
`--exclude-libs,ALL`); libstdc++, libgcc and OpenSSL are linked **statically**, because the
TV's libstdc++ lacks three NDK symbols and its libssl is 1.0.x / 1.1 / 3 depending on release.
`tools/fwcompat.py` grades the one `.so` OK across its inventory (webOS 1.2–11.2).

`vendor/engine-sources/engine.patch` is a **historical artifact, not a build input**:
`ci/build-sbengine.sh` never applies it. Its changes are already baked into the checked-in
vendored tree — `nuvio-engine/CMakeLists.txt` compiles `src/storage/streaming_window.cpp`, and
`cmake/Dependencies.cmake` applies the `nuvio-drop-piece` and `nuvio-forget-all` libtorrent
patches that the patch added. The only live references are `hashFiles(...)` cache keys in the two
GitHub workflows, so editing it invalidates a dependency cache but changes no source. It is also
no longer applicable as-is (`git apply --check --reverse` reports a corrupt patch at line 2154);
treat the vendored source as authoritative.

`make ipk` / `make deploy` stage the `.so` beside the binary as `libsbengine.so`; a missing
artifact is a packaging note, not a build error. `plx_stremio::sbengine::api()` uses the repo's
`dynlib!` seam to open exactly that app-dir file, call `sbe_get()`, and check `api() ==
API_VERSION (4)`. The handle is deliberately **never** `dlclose`d: engine threads live inside
the library. A missing library, incomplete symbol table or version mismatch latches `None` for
the process; `available()` is then false and **torrents are unavailable on that install**.

There is deliberately **no working Node fallback**. `Shim::torrent_server()` returns `None`
when P2P is off or when no server is configured and the engine did not load, and
`shim_media::rank` then drops every torrent/magnet from the lists; `open_url`'s legacy
`plx_stremio::torrent::resolve` line is unreachable on that path.

The upstream README describes Nuvio Engine as under active development and not yet
production-ready; this fork pins a specific commit and version and ships that, rather than
tracking upstream HEAD. Upgrading is a build-input change: it needs the same review and
`make build-bench` table as any other dependency/toolchain change.

## 2. Boot lifecycle and directories

`app::boot::start_p2p_service()` runs only on ARM, non-hostsim, non-test. It parks a thread,
then calls `start_sbengine()`:

- payload cache: `<runtime_dir()>/nuvio-p2p/payload` — under the TV's RAM-backed tmpfs;
- swarm state: first creatable `paths::p2p_state_candidates()`, else
  `<runtime_dir()>/nuvio-p2p/state`; made `0700` when the candidate is world-writable
  (it holds saved peer addresses);
- `sbengine::start_engine(state, cache)` passes the app's budgets and then applies libtorrent
  settings at runtime: `connections_limit=500`, `allow_multiple_connections_per_ip=1`,
  `connections_slack=100`, `connection_speed=200`, uTP in and out on, `peer_connect_timeout=10`,
  `handshake_timeout=30`, `max_web_seed_connections=10`;
- a parked stats thread logs one `sbengine: peers=… seeds=… down=… delivered=… window=…`
  line every 10 s, only while something is moving.

`stop_p2p_service()` / `stop_engine()` joins the engine's event thread and destroys the engine;
the C++ shim also has a `StopAtExit` destructor so a process exit that skipped `stop` cannot
abort on a live `std::thread`.

## 3. The C ABI (`core/sbengine.h`)

Every call returns immediately; the app polls. Ids are 40-character lowercase info hashes.

| Call | Meaning |
|---|---|
| `start(stateDir, cacheDir, windowBytes, uploadBps, memCacheBytes, segmentBytes)` | create engine and event thread; 0 = running |
| `open(magnet, ih)` | add the torrent once; 0 = ok or already open |
| `files(ih)` | file count; `-1` metadata still in flight, `-2` failed |
| `stream(ih, idx, url)` | `1` ready, `0` preparing, `-1` failed; fills a loopback URL |
| `error`, `stats`, `limits`, `window`, `setting`, `relocate`, `restored`, `remove`, `remove_all` | diagnostics and controls |
| Fork Rofocale11 / API 5: `stop_stream(ih, idx)` | stop ONE prepared file stream without removing its torrent; used for a season pack's next episode |

`libsbengine`'s `nuvio_engine_config` is set to the **SOFT** profile (35 peers, 35 connections
per second), limited upload (2 MiB/s), and **zero** stream-inactivity / warm-torrent timeouts:
a paused stream must still be there, and the app decides when it ends. App-side budgets:

| Budget | Value | Why |
|---|---|---|
| Streaming window (disk/RAM payload) | 32 MiB | ~10 s of lookahead at the ~24 Mbit/s measured on the set; engine minimum is 24 MiB |
| Verified-piece memory cache | 8 MiB | a second copy of window bytes for fast re-reads |
| Total | 40 MiB | both are **RAM** on the TV; the set measured 78 MB `MemAvailable` mid-film at the old 72 MiB setting |
| Segment size | 0 | per-torrent files with hole punching; `relocate()` switches to segment files for storage that cannot punch holes (FAT32) |

`sbengine.rs::budget_tests` fails if the total rises above 40 MiB or the window holds less than
8 s at 25 Mbit/s. Changing either constant requires device memory evidence first.

## 4. Event loop and wrapper state

The C++ wrapper keeps `torrents` (info hash → state, files, streams), `addRequests` (request id →
hash) and a `mu` mutex; an `event_loop` thread polls `nuvio_engine_poll_event` roughly once per
frame and maps:

- `TORRENT_ADDED` → record the engine's torrent id;
- `METADATA_READY` → copy the file list; set `restored` when the event message carries
  `metadata_source=restored` (a kept `.resume` file, not a cold fetch), and refresh files;
- `STREAM_PREPARED` / `STREAM_STOPPED` → fill or drop a per-file stream URL;
- `TORRENT_REMOVED` → if the engine dropped a warm torrent on its own (disk-cache policy), erase
  the wrapper's stale entry so the next `open` re-adds the magnet and restores from the kept
  resume file; otherwise a later `open` would answer "already open" and `prepare_stream` would
  answer `NOT_FOUND` forever;
- `TORRENT_ERROR` → attach to the stream or torrent; repeated messages are logged at most once
  per 10 s.

Calls hold `mu` but never call into the engine while holding it where the event thread also
takes the lock — `stats()` in particular reads the playing stream id under the lock, then calls
`nuvio_engine_get_stream_stats` after unlocking, to avoid inverting the order.

## 5. From a stream to a playable URL

`shim_media::playable_url` turns a torrent stream into the marker `torrent:<info hash>` and marks
its PMS `Media` row `p2p: true`. An addon/debrid HTTP URL is never a marker. At play time:

1. `Shim::open_url` returns an HTTP URL unchanged; a marker goes through `cached_open_url` first.
2. `torrent_server()` is checked **before** the engine branch, so turning P2P off drops every
   torrent even if the library is loaded.
3. `resolve_with_sbengine` takes the lowercase hash from `torrent::info_hash`. Magnet choice: the
   stream's own `magnet:` URL, else a synthesized one from `peer_sources` (trackers only; `dht:`
   entries are skipped), else a bare `magnet:?xt=urn:btih:<hash>` that relies on DHT. `file_idx`
   is the preferred file index; season/episode currently do not feed the engine's file guesser.
4. `sbengine::resolve_stream(magnet, ih, preferred_idx, 30_000)`:
   - `reset_session_download()` (session byte baseline) and `resume_foreground()` (download
     unlimited, upload 2 MiB/s);
   - `open()` adds the torrent once. `remember(ih)` evicts the previously playing torrent
     **before** the baseline is taken, so old swarm bytes cannot satisfy the new torrent's gate;
   - poll `files()` every 100 ms until metadata arrives or the 30 s deadline; pick the explicit
     index if valid, else the largest file (season-pack heuristic), else the first;
   - poll `stream()` until it returns a ready loopback URL;
   - prebuffer: wait until **1 MiB more** than the baseline has been downloaded, capped at 10 s
     and the resolve deadline. It is advisory — a slow swarm still gets its URL. A same-hash
     retry and a restored swarm skip the gate (they already have data). The engine's HTTP reads
     block until pieces are verified, so the player's own buffering can start while the rest of
     the lookahead is still in flight.
   - One `sbengine: <hash12> metadata …ms (n files, index …) -> url …ms -> prebuffer … -> resolve
     …ms` line per stream, plus `sbengine: serving … via engine (file …)`.
5. `Shim::part` answers the Part with a **302** to that URL. The shim's decision request uses a
   30 s deadline with no inactivity watchdog, matching the engine's own 30 s resolve cap.

**Cached URLs are torrent-scoped.** `remember` removes the old torrent, and its `/stream/<id>`
then 404s for the rest of the session (the old "playback retry" loop). `engine_url_is_current`
compares the marker's hash with `sbengine::active_hash()` case-insensitively and drops a cached
URL when they differ. A same-hash retry of the SAME file is a no-op in `remember`, so it never
tears its own stream down.

Fork Rofocale11: a **season pack is one hash with many file indexes**. The active identity is
therefore `(hash, file_idx)`, not the hash alone. A same-hash resolve with a different `file_idx`
is a FILE switch, not a retry, but the shipping path does **not** clean up explicitly. The
vendored engine's `register_stream` already stops the previous stream for that torrent when the
new file is prepared (`libtorrent_stream_bridge.cpp:465-477`). Two explicit cleanup attempts were
tried and reverted after stable-set failures: API-5 `stop_stream` left the torrent with no active
stream, so the engine unloaded it and the next resolve raced that unload (`EAGAIN` /
"Resource temporarily unavailable"); remove + re-open then failed with an empty resolve error
right after the re-add, and resetting the engine BEFORE the switch failed on the very first
handoff. The shipping path leaves the torrent alone and retries the engine resolve once on any
transient error: `resolve_with_sbengine` re-opens the torrent (which clears the wrapper's error
entry and re-activates the handle), waits 700 ms, and tries again within the same 30 s budget.
The owner measured the resident growth as harmless sparse payload on this set.
The stable set measured `window=32MB resident=160MB` across a file 33 → file 34
handoff, with the resident payload landing in the RAM-backed tmpfs; the app then left playback
with no OOM line in the event log (so the kill itself is not proven by that log). A different hash still takes
the old path: `remove` drops the torrent and its payload (`delete_files`), keeping only resume
state.

## 6. Warming a resume before Play

A cold resume spends its first seconds re-establishing a peer set. `Shim::warm_remembered_torrent(rk)`
does this best-effort, silently, on a `p2p-warm` thread:

- requires P2P on, a positive watch offset, and a remembered `last_stream` with a 40-hex
  `info_hash`;
- calls `sbengine::warm(ih)`, which opens `magnet:?xt=urn:btih:<ih>` **without** preparing a
  stream, setting file priorities or reading payload. A kept `.resume` file restores metadata and
  saved peers, so the swarm is dialling before the viewer presses Play; a cold add simply starts
  earlier;
- shared by `GET /nuvio/warm?ratingKey=…` and, since the resume fix, the **detail read** — the
  page the viewer resumes from. The log line is `sbengine: warm <hash12> — swarm opened (no
  stream, no payload)` or `engine refused`.

`part_stream` in `shim_pms.rs` closes the other resume gap: an arrival index encoded into a Part
id can be stale after a restart reorders addons. A stream still at that index wins (explicit
in-session pick); when the index is absent, the remembered source is resolved by stable identity
from the **fresh** ranking, instead of answering 502 "stream no longer available".

## 7. Stopping, source changes and settings

`sbengine::stop_background()` (when the player leaves) removes **every** torrent and its window
payload and clears `active_hash()`. That is cheaper than a rate cap — a torrent left downloading
at 64 KiB/s still stuttered Home — and the `.resume` file survives, so the next play restores
metadata and peers instead of a cold fetch. `resume_foreground()` runs at the head of every
resolve and when the player page appears.

The P2P switch lives in the one `nuvio.redb` store (`LocalSettings`, key `settings`, camelCase
`p2pEnabled`). It defaults **ON** for an install with no such key, is written on the retained
storage worker, and is applied live to the shim without a restart. Turning it off sets
`torrent_server()` to `None` before any engine branch and stops offering torrents. Screens read
the published `ScreenInputs::p2p_enabled`, never the store or the engine.

The diagnostics page's delivery column uses a three-way switch in
`app/diagnostics/diagnostics_nuvio.rs`: a P2P session gets file/size, swarm rows and TV RAM; a
live adaptive session gets the upstream controller rows; a debrid/addon link gets file, addon,
rate, headroom, quality and RAM. API-4 stream counters (`scheduled_pieces`, `active_demands`,
`delivered`, `piece_bytes`, `primary_demand_start`) are what let the log say whether the engine
is downloading **for the player** or for itself.

## 8. Verification status

- Build: `ci/build-sbengine.sh`, `make ipk` and `make deploy` stage the library; fwcompat grades
  the single artifact across its firmware inventory; `ci/check-package.py` verifies the payload.
- Host: `pmsshim` tests cover marker creation, the P2P-off gate, cached URL invalidation, warm's
  inert cases and the stale Part index. Two ignored tests under `shim_pms::host_c_engine` need a
  host `libsbengine.so` and a loopback seeder via `tools/host-engine-check.sh`; they are not part
  of `make check` and a pass there is not a device pass.
- Device: the owner's 0.10.0 stable run after the resume fixes (2026-11-25) confirmed the
  resume behavior; `sbengine: … via engine` in the event log is the on-TV evidence of which
  engine served a stream.
- **Owner-confirmed (2026-11-26):** multi-hour stability under load, a filmed measurement of
  prebuffer versus peer discovery on a cold swarm, the ARM comparison at different window sizes,
  and the firmware/vendor unit matrix are verified. No command or log for them was attached to
  this document. The engine's own unit suite in `vendor/engine-sources/nuvio-engine/tests` remains
  outside this repo's gates.
