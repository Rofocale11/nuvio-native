# NuvioNative architecture

> **Status:** current — the system shape, constraints and crate map for this product.
> `docs/README.md` marks the upstream PlxNative documents that are kept for reference only.

NuvioNative is a fork of RichiL10/plx-native's `feature/v0.8-hq` branch, itself based on PlxNative
(`GLinnik21/plx-native`, GPL-3.0-or-later); `UPSTREAM.md` records the computed base. It browses and
plays content from Stremio addons — the same addons a user configured in Nuvio — with PlxNative's
GPU-rendered UI and native Starfish/ACB video pipeline.

Companion documents: `FEATURE.md` (the running catalogue of what the fork adds),
`ID-MANAGEMENT.md` (numeric keys, the key table, synthetic keys),
`TMDB.md` (detail enrichment, person pages, collections), `PLAYBACK.md` (the stream → decoder
path), `WATCH-PROGRESS.md` (local watch state and the account mirror), `DETAIL-LATENCY.md`
(why a detail open is several round trips, and the plan to prefetch and cache them).

## Findings that drive the design

- **Nuvio has no reusable backend service.** NuvioTVSmart (the code behind the webOS app
  `space.nuvio.webos`) is a browser app: addon management, catalogs, metadata, stream discovery and
  playback all run as JS inside the WebView. Its webOS JS services (`services/webos`) only proxy
  media/images/subtitles for the WebView. Addon config lives in WebView localStorage (unreadable
  from another app in Developer Mode) and is synced to the user's **Nuvio account** (Supabase).
  So "reusing Nuvio's backend" means: speak the same Stremio addon protocol, and import the same
  account-synced addon list.
- **PlxNative's Plex coupling is deep but sits behind one HTTP boundary.** ~54k lines of
  `plex/` + `session/` + `data/` speak the PMS REST dialect; the UI, player, media pipeline and
  stores consume the typed results. `tests/mock_pms.py` documents exactly the endpoints and fields
  the app needs, and the app already has a dev boot that connects straight to a PMS host/port
  with a token.

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

Torrent picks resolve through a streaming engine. When `libsbengine.so` loads it runs inside this
process (`dlopen`, never linked); otherwise the fallback is a `node` child spawned from the app's
own `js-service/` directory — the only component outside the process. See "The P2P streaming
server" below.

- Catalogs → PMS library sections and Home hubs. Metas → movie/show/season/episode items.
  Search → `/hubs/search`. Streams → the item's `Media[]` list (one Media per stream, with
  resolution/codec/HDR/size/bitrate from `plx_stremio::classify`). Each Media also carries
  `addon_name`, the `Addon::label()` of the addon that returned it (empty only on the
  "Finding streams…" placeholder). Part URLs redirect (or proxy, when headers are needed) to the
  stream's real URL. The decision endpoint waits for discovery/probing and refuses media that
  cannot direct play; the native route uses its probed fields.
- **A person page's global reads are the shim's too.** A cast headshot carries a synthetic
  `tagKey` (`nuvio-person-<TMDB person id>`); `/library/people/{guid}` and
  `/library/people/{guid}/credits` are answered from TMDB, and `plex::account::discover()` resolves
  to the installed local server, so the biography, life facts and department filmography never
  reach plex.tv — which has no Nuvio credential behind this app. The numeric synthetic key on
  `Role[].id` still opens `/library/people/{personId}/media`, the page's two shelves. The television
  sees the same shapes a real PMS + Discover pair used to send, so the ported person/filmography
  screens run unmodified.
- **A Library section's Type menu is a catalog selector, not a movie/show switch.** The section
  already fixes the content type, so `includeMeta=1` returns one `MediaContainer.Meta.Type[]` entry
  per browseable catalog of that type. Each entry carries the shim extension `catalogKey`, the
  display `title`, `active` for the catalog the current page came from, and the same `Sort[]` menu.
  Selecting a row sends `?catalog=<key>` on `/library/sections/{k}/all`; the shim resolves the key
  against `plx_stremio`'s browse catalogs, fetches only that catalog, applies `?genre=` (defensively
  in memory when the addon does not declare genre support) and sorts in memory by `popularity`
  (the catalog's own order), `rating`, `releaseDate`, or `titleSort`. **A request that names a
  catalog keeps it whatever `?sort=` says**: the sort only reorders inside the chosen catalog, so
  the page the client fetches after the landing teaches it the menu cannot revert to the section's
  first catalog. `/library/sections/{k}/genre`
  merges the catalog's declared genres with TMDB's standard list (movie or TV by section), so the
  Filter menu is populated even when the addon declares no `genre` extra; a TMDB genre the addon
  does not declare is matched against the rows' `Genre[]` in memory, alias-aware so `Sci-Fi` and
  `Science Fiction` are the same filter. `/library/sections/{k}/genre?catalog=<key>` scopes the
  list to the selected catalog; standalone `/sorts` and `/filters` return the same menus for
  clients that ask directly.
  **The Library tab is grid-only.** It never requests `/hubs/sections/{k}` and never emits
  `PublishShelves`, so Movies/TV Shows open with Type/Sort/Filter immediately above the grid
  instead of below a run of horizontal hub rows. Home's shelves are a different store and are
  unaffected.
  The extension is deliberately additive. A real PMS, or a shim whose addons failed to load, sends
  entries without `catalogKey`; the client then falls back to its hardcoded `LibraryType` menu, so
  the screen never goes blank. Catalog keys are shim-local strings and never become `ratingKey`s —
  the numeric-key invariant in `ID-MANAGEMENT.md` is unchanged.
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
  `(index, label, addon_name, seeders)` list) and `addon_pages` (the panel's own `StreamChoice[]`,
  which also carries the stream's technical facts). `route::AVAILABLE_STREAMS` is written only
  through `route::set_available_streams`, so a revision stamp tells the player panel's per-tick
  poll that nothing moved.
- **A press opens the picker; a pick IS the play.** Every surface that used to start a file
  directly raises this panel first, and the commit starts the thing the panel was about:
  - **Detail page**: the hero's **Streams** pill lists the page's own item; an **episode's still**
    opens the same panel for THAT episode (`ContentPanel::AltSources { item }` names the subject,
    so the show page's own item is overridden) and the pick starts the episode;
  - **Home**: the hero's **Play** raises it for the billboard's film (`item` names it, since Home
    has no page item of its own; the panel opens centred, `item_menu::fallback_anchor`). A show or
    season hero still plays its on-deck episode — it has no stream list of its own;
  - **a movie page's pill** plays the movie on the pick, exactly as its Play button does. The pick
    used to only re-read the item, which left the film sitting on its page with the new stream
    selected and nothing playing.

  A picker on any other source (an ordinary Plex server) is not offered at all: it has no stream
  list, so those presses play or navigate exactly as before. `AppMsg::NuvioStreamSelected` is what
  a successful commit delivers back to the page that opened the panel — the panel names no item, so
  Detail remembers which episode (or, for the movie pill, none) its picker was about, and Home
  remembers which hero item.
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
  confused with it.
- Addon configuration: a separate opaque JSON record in the app's private DB8 kind,
  loaded/saved through the authenticated storage helper. Stable/debug/nightly own
  separate records. The helper verifies readback before import files are removed.
- Session-scoped addon enable/disable: Settings > Addons lists the installed addons for the active
  profile and toggles them for this session. The override (keyed by the addon's canonical transport
  URL) is stored under `preferences.addonEnabled` in `nuvio-session.json` and applied on top of the
  account's own `enabled` flag whenever a profile's addons are loaded; the running `AddonBackend`
  is swapped and its caches cleared so disabled addons disappear immediately.
- **A sign-in's content arrives after Home's first read, and Home is told to read again.** The
  profile's addon set and its resume points are fetched on workers (manifests take up to the 15 s
  deadline), so the first `/hubs` answer can predate either. `Shim::content_generation` counts
  every change that makes the answer stale — the addon set replaced, a pull that actually merged a
  resume point, and a finished `warm()` that filled catalog caches `/hubs`' 4 s budget had missed —
  and the app compares that count once per frame, refetching the hubs (and invalidating the
  library shelves) once per observed value. `Account::load_profile` installs the addons **before**
  it fires the watch pull: the pull resolves each resume point's row through the addon backend, and
  a position merged with no row snapshot is invisible in Continue Watching. The fetch is also
  generation-checked again after the network call, so a profile picked while an older load's
  manifests were out cannot install the wrong set or pull the wrong history.
- **Account preferences live in one file, with one writer.** `defaultAudioLanguage`,
  `defaultSubtitleLanguage`, `autoSelectSubtitle` and `defaultSubtitleForced` sit under
  `preferences` in `nuvio-session.json`. `GET/PUT /api/v2/user/profile` on the shim is the editor
  and `Account::write_prefs` is the only writer (one lock, one snapshot, one atomic rename, a
  returned error the PUT turns into a 500). Playback reads the file through
  `account::local_preferences()`, not the account endpoint: a Nuvio profile has no plex.tv
  credential, and the shim's `/api/v2/user` carries no `profile` block. There is no Nuvio cloud
  preference API (`plx_stremio::nuvio` has profiles, addons, library and watch progress only), so
  the file is the source of truth; signing out wipes it with the session.
- **Player preferences are per-profile; one restore trigger.** The live audio-DSP pair, subtitle
  tone/size/position, quality ceiling, Direct Play, next-episode, skip interval, deck press, under
  posters, HDR overlay and trailer autoplay all live in one file, as the seated profile's own values
  under `Preferences`. `route::choose` (media) is the only write door and `player::set_audio_enhancements`
  & co. are its thin wrappers; boot restores the process-start seat, `Unseated`, with
  `route::restore_profile_rows`, and `bridge::SeatWatch` restores at every later seat change — never
  a republication of the same seat, so a pick in flight is not clobbered. A pick never writes the
  install-wide top-level key; that key is only the frozen fallback for a seat with no value of its own.
- Torrents (`infoHash`): played through the in-process C engine's own loopback Range URL when
  `libsbengine.so` loads; when it does not, torrents are unavailable on that install — there is
  no Node streaming fallback. See "The P2P streaming server" below.
  Debrid-resolved HTTP links from Torrentio/AIOStreams play directly. Local server scanning is off
  by default and requires `enableTorrents`; a configured `torrentServer` can be used directly.
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
- **The Watchlist tab IS My List.** Upstream's Watchlist page is built over the plex.tv
  Watchlist, which this fork has no credential for. `app::watchlist_io::MyListIo` replaces that
  seam: `page` reads the shim's `saved_library` (one title's metadata and its numeric ratingKey),
  `find` reports the shim as holding every title so a tile is playable and opens the shim-backed
  Detail, and `write` goes back through the shim's one save path. The shim exposes the read/write
  as `Shim::my_list_entries` / `set_my_list_saved`; nothing here calls plex.tv.
- Watch state / progress: stored locally by the shim (the app already reports timeline to "the
  server") and mirrored to the active profile through Nuvio's `sync_push_watch_progress` /
  `sync_pull_watch_progress` RPCs, so Continue Watching follows the account across devices. A
  paused or stopped report goes at once, a playing one at most once per 60 s per item; the newer
  `last_viewed_at` wins on the way back. See `WATCH-PROGRESS.md`. Developer Mode's
  writable-directory fallback is `/tmp`, so the local copy and the key table are not yet durable
  across TV reboots — the account copy is.

## The P2P streaming server

Torrent (`infoHash`) streams need a streaming engine. The app has one: the in-process C engine
(`libsbengine.so`). The legacy Node streaming server (`js-service/server.js`) is retired and is
no longer spawned; when the C engine cannot load, torrents are unavailable.

- **C engine (`libsbengine.so`)** — primary when `sbengine::available()` is true. `boot.rs`
  starts it via `sbengine::start_engine` with pre-created `state`/`cache` dirs under
  `<runtime_dir>/serverp2p`, before the Node spawn in `start_p2p_service`, and stops it
  first in `stop_p2p_service`. When it started, `start_p2p_service_inner` returns without
  spawning `node` at all — the engine owns torrents and the idle child would only spend RAM.
  The shim's `open_url` tries `sbengine::resolve_stream` first and only uses Node's `POST
  /create` when the engine is unavailable; the engine serves the file itself on its own loopback
  `127.0.0.1` Range URL, so nothing goes through the Node HTTP dialect on the engine path.
  The Settings server URL is therefore optional: `Shim::torrent_server` falls back to the
  default loopback address when the field is empty and the engine is loaded, so torrents stay
  offered.
  The engine keeps every opened torrent downloading until it is removed, so resolving a
  different hash removes the previous one (`sbengine: released previous torrent <hash>`), which
  also deletes its payload. That invalidates every URL the engine handed out for it, so
  `sbengine::active_hash()` names the live torrent and `open_url`'s resolved-URL cache reuses an
  engine URL only while its `torrent:<hash>` marker still matches — after a source change back,
  a stale entry is resolved again instead of 404ing forever.
  `libsbengine.so` links libstdc++/libgcc and OpenSSL 1.1.1 statically — the NDK's libstdc++
  would otherwise want three symbols the TV's lacks, and the firmware's `libssl` SONAME differs
  across releases (1.0.x, 1.1, 3) — so `tools/fwcompat.py` grades it OK on every release in its
  inventory, 1.2 to 11.2. Where `dlopen` still fails (an unknown firmware, a missing file)
  `available()` is false and torrents are unavailable on that install. A missing `libsbengine.so` is a normal state, not an error:
  `make deploy` carries it to a device when built, and neither deploy nor `make ipk` fails for
  its absence. `make ipk` ships it when built, and `build-ipk.yml` builds it before packaging
  (`ci/build-sbengine.sh`) and then fails if the staged package has no `libsbengine.so`, so a
  release cannot ship a Node-only package by accident; because the engine is a CMake artifact with
  no Rust link receipt, the packaging gate pins its exact SHA-256 instead of a `.link.json`.
- **Node streaming fallback — retired.** `js-service/server.js` is no longer spawned. The
  trailer decipher (`js-service/trailer-decipher.js`) is a separate Node child and is
  unaffected.

- **Data directory.** `appPath` is `<runtime_dir>/serverp2p` (the Rust half passes it as
  `APP_PATH`), and `cacheRoot`
  follows `appPath`; the C engine uses `<runtime_dir>/serverp2p/state` and `…/cache` under the
  same root. The runtime root is per install — `/tmp` for the stable app, `/tmp/<app id>`
  for a developer install beside it (`paths::resolve_runtime_dir`) — which is required here, not
  cosmetic: the two installs run under different uids and the stable one could not write the
  debug one's directory, so a shared path made whichever booted second refuse to start P2P. The
  generated settings and state are therefore
  `<runtime_dir>/serverp2p/server-settings.json`,
  `<runtime_dir>/serverp2p/stremio-cache/` (only while caching is on), and
  `<runtime_dir>/serverp2p/httpsCert.json`. The root is
  writable on read-only production installs, and with `cacheSize: 0` nothing there needs to survive
  a reboot. `boot.rs` creates the directory and refuses to spawn when it is owned by a different
  uid, logging `rm -rf <path>` rather than trying to chown it. Startup also removes
  a legacy `<app_dir>/.stremio-server/` and the earlier `<app_dir>/nuvio-serverp2p/`, logging each
  removal; both are non-fatal and idempotent.
- **Generated defaults.** `cacheSize` and `transcodeMaxWidth` are `0`; `transcodeMaxBitRate` is
  `0`; the local addon and stream proxy are off. A fresh `/tmp` therefore reports
  `cacheSize: 0` and `transcodeMaxWidth: 0` from `/settings`. The settings module logs the
  effective values as `nuvio: settings cacheSize=<n> transcodeMaxWidth=<n> dataDir=<path>`.
- **Memory-only store when caching is off.** With `cacheSize: 0` (also `--no-cache` or
  `DISABLE_CACHING`), `getDefaults` configures a 32 MiB circular buffer (`type: "memory"`) with a
  24 MiB read-ahead, and nothing on that path touches disk: `getCachePath` does not create
  `stremio-cache/`, the engine does not write its `.torrent` sidecar under the cache path, and the
  `engine-created` cache-size sweep (a tree walk plus a `df` child) is skipped. Turning caching on
  restores the disk chunk store and the size-driven cleanup unchanged.
- **Response head first, pieces later.** `handleTorrent` flushes the `200`/`206` head before
  pumping, so an empty swarm delays the body rather than the HTTP response — which is what keeps
  the player's 15 s header read from expiring. `/:infoHash/create` then selects the first two
  pieces of the guessed or known file before answering, so they download during the shim's `302`
  handoff and the player's open; the selection is bounded, so an abandoned create cannot pull the
  whole file. The player's own cold-start budget is the `MEDIA_FIRST_BYTE_TIMEOUT_MS` grace
  described in `PLAYBACK.md`.
- **Start.** On a live boot — `app::boot::construct`, never a controlled/replay boot, a host run or
  a test — `app::boot::start_p2p_service` creates `<runtime_dir>/serverp2p` and spawns
  `libsbengine.so` (dlopen, never linked; see above). The child's stdout and stderr
  append to `<runtime_dir>/serverp2p.log` (both are discarded if the file cannot be opened); the
  root is per install, so the stable and debug installs never share it. The child
  handle lives in a process-global slot; the event log records the interpreter and pid
  (`nuvio: P2P server started with …`) or the exact spawn error. The spawn does not block the
  first frame.
- **Stop.** `app::run::shutdown` calls `app::boot::stop_p2p_service`, which kills and reaps the
  child before the process exits. Backgrounding does not reach `shutdown`; a hard crash no longer
  orphans the child, because `start_p2p_service` arms `PR_SET_PDEATHSIG(SIGTERM)` (with a
  `getppid()` race guard) so the kernel delivers SIGTERM to `node` when this process dies.
- **Address and switch.** The session preference `p2p_server_url` defaults to
  `http://127.0.0.1:11470` (`plx_plex::plex::session::DEFAULT_P2P_SERVER_URL`); an explicit value
  overrides it. The shim is told the address and the switch at boot and after every save. The
  server is spawned whatever the switch says — `p2p_enabled` only controls whether the Source
  Picker offers torrent streams.
- **Failure is local.** A missing `libsbengine.so`, a failed `dlopen` or a failed `start_engine` is logged
  and leaves the app fully usable; torrent picks simply cannot play. Debrid-resolved HTTP links do
  not use this server.

## Constraints that shape the code

- **No new C libraries or JS engines in the TV build.** The fixed exceptions are the bundled
  FFmpeg and the C shims: `src/main.c` (boot/crash), `src/starfish.c` (the Starfish/ACB seam) and
  `src/svg.c` (the SVG rasteriser). Everything else is Rust. The app compiles for an ancient LG
  toolchain (`arm-webos-linux-gnueabi`); crates such as `innertube-rs`, `rusty_ytdl`, `rquickjs`
  or `boa_engine` pull C code or need `libclang`/`bindgen`, and the ARM build fails on them. The
  bundled FFmpeg is built `--disable-everything` plus the demuxers/decoders the player actually
  uses; adding one means a change to `ci/build-ffmpeg.sh` and the buffer-feed pump. Audio is
  decoded and re-encoded in-app in two places: the lossless PCM lane (`media/src/ff_pcm.rs`:
  `flac`/`alac`/PCM decoders, plus `mp3`/`vorbis` behind the `lossy` knob) and the AC3 encode lane
  (`media/src/ff_enc.rs`: `dca`/`truehd`/`mlp`/`flac` decoders, the integer `ac3_fixed` encoder).
  The playback-speed path (`ff::SpeedDecoder` + `media/src/speed_audio.rs`) decodes the selected
  audio to PCM as well. Those are why the components are in the list — see `PLAYBACK.md`. The P2P
  engine is the in-process `libsbengine.so` (C, dlopen'd, never linked) or, when it cannot load,
  the external `node` child process (see above) — never an embedded JS engine: nothing here links
  a JS runtime into the app. The `.so` is the one owner-approved exception to this rule: it is
  staged beside the binary, its absence is normal (that package was built without it), and the app
  is fully functional without it. Nuvio Engine is GPL-3.0-or-later, libtorrent BSD-3, Boost
  BSL-1.0, OpenSSL 1.1.1 (dual OpenSSL/SSLeay, linked statically); `ci/build-sbengine.sh` and
  `ci/build-openssl.sh` build it.
- **`ratingKey` must be numeric.** PlxNative parses it as an `i64`; a string like `"tmdb:603"`
  blanks the screen. `ID-MANAGEMENT.md` is how the shim keeps that promise.
- **Trailers play direct, never via a native YouTube extractor.** Trailer URLs are deciphered
  and fetched by the Node child (`js-service/trailer-decipher.js`, Node v16.20.2 floor), and the
  shim serves the resolved first clip at `/library/metadata/{rk}/trailer`, and `/library/metadata/{rk}/extras`
  (the detail shelf source) gets one shelf row **per trailer** with `subtype: "trailer"`,
  `extraType: 1` and a deterministic Part key, built **without** a YouTube resolve so a detail
  open never spawns Node. TMDB `videos` rides in `append_to_response` on `/movie/{id}` and
  `/tv/{id}`, so a show (including a synthetic related-show key, which has no Cinemeta meta) gets
  the same shelf. Each `.mpd` comes from its clip's Part URL `/library/parts/<id>/0/file.mpd`,
  which the shim resolves on that Part request (the first tile is also warmed in the background)
  and the Rust DASH engine (`media/src/dash.rs` + `ff.rs dash_demux`) demuxes with the bundled
  `mov` demuxer per track — no FFmpeg `dash` demuxer (it is not in the build), no new C library.
  A detail row may carry trailer thumbnails (`Extras` with numeric clip keys and YouTube posters);
  the clip's playable Media is the shim's own answer, not a native YouTube route. An
  age-restricted (red-band) trailer answers `LOGIN_REQUIRED` to every anonymous client, so that
  id is remembered and filtered out of the shelf; the title's other trailers/teasers are the
  tiles that remain (`PLAYBACK.md`). The pair is resolved with YouTube's
  `VISIONOS` client plus a `visitorData` identity — the only anonymous client
  measured to be granted the whole adaptive track; every other one is cut at
  60 s (`PLAYBACK.md`). Its track bytes come straight from the signed
  googlevideo URLs the decipher returned: there is no proxy hop and no
  user-agent override. Those URLs refuse the request *shape* — a GET
  with no `Range` header, or an open-ended `bytes=N-`, answers `403`, while a
  bounded `bytes=a-b` answers `206` — so `curlio` states an 8 MiB window for
  `*.googlevideo.com` media and continues it at the next byte
  (`GOOGLEVIDEO_WINDOW`). `js-service/trailer-proxy.js` stays in the tree but
  carries no track bytes.
- **The shim is a loopback PMS.** One in-process HTTP server (`plx_pmsshim`, default
  `127.0.0.1:32420`) speaking the Plex dialect, in the same process as the UI.
- **Account preferences live in one file, with one writer.** `defaultAudioLanguage`,
  `defaultSubtitleLanguage`, `autoSelectSubtitle` and `defaultSubtitleForced` sit under
  `preferences` in `nuvio-session.json`; `Account::write_prefs` is the only writer, and playback
  reads through `account::local_preferences()`.
- **My List and watch progress are Nuvio RPCs.** A write names one title and the active profile,
  never a whole-list replacement; watch progress is pushed throttled and pulled/merged on boot or
  a profile switch. `WATCH-PROGRESS.md` is the contract.

## Crates

`ci/module-layers.ini` is the authority for allowed references; `ci/check-module-layers.py` fails
the build on a forbidden edge.

| crate | owner | depends on |
|---|---|---|
| `rust-modules/base` (`plx_base`) | leaf utilities | — |
| `rust-modules/machine` (`plx_machine`) | state-machine runtime | `plx_base` |
| `rust-modules/platform` (`plx_platform`) | webOS identity, storage, codecs, artwork, i18n | `plx_base`, `plx_machine` |
| `rust-modules/gfx` (`plx_gfx`) | GL/EGL, text, images, icons | `plx_base`, `plx_machine`, `plx_platform` |
| `rust-modules/net` (`plx_net`) | libcurl HTTPS + plaintext stream | `plx_base`, `plx_platform` |
| `rust-modules/plex` (`plx_plex`) | typed Plex API, session store | `plx_base`, `plx_machine`, `plx_platform`, `plx_net` |
| `rust-modules/stremio` (`plx_stremio`) | Stremio addon protocol, Nuvio account/import, classify, torrents | `plx_base`, `plx_net` |
| `rust-modules/pmsshim` (`plx_pmsshim`) | loopback PMS dialect over `plx_stremio` | `plx_base`, `plx_net`, `plx_platform`, `plx_stremio` |
| `rust-modules/telemetry` (`plx_telemetry`) | consent, crash/usage channels | `plx_base`, `plx_machine`, `plx_platform`, `plx_net`, `plx_plex` |
| `rust-modules/data` (`plx_data`) | data layer / stores | see `ci/module-layers.ini` |
| `rust-modules/session` (`plx_session`) | session layer | see `ci/module-layers.ini` |
| `rust-modules/media` (`plx_media`) | media pipeline / player | see `ci/module-layers.ini` |
| `rust-modules/appkit` (`plx_appkit`) | shared widgets (player HUD, track/source menus) | see `ci/module-layers.ini` |
| `rust-modules/screens` (`plx_screens`) | owned screens | `ui` and data crates; never `app` |
| `rust-modules/storage` (`plxnative-storage`) | private storage helper | isolated package |
| `rust-modules/ui` (`plx_ui`) | shared UI system / design tokens | see `ci/module-layers.ini` |

## Build and release

- **Host simulator:** `make -j10 sim-linux` (needs the Linux `libav*`/libass host libraries staged
  in `pkg/`); `make sim-macos` on macOS.
- **TV build (.ipk):** `make ipk` where the webOS NDK is installed. The release artifact is built
  by CI — `.github/workflows/build-ipk.yml`, on a `v*` tag or by `workflow_dispatch` on a pushed
  branch.
- **Release:** `tools/cut-release.sh` (see the `cut-release` skill) bumps the version files,
  commits and tags. A stable install, package or deploy names `FLAVOR=stable`, and every release
  build carries `RELEASE=1`. Do not add `libclang` or ARM cross-compiler steps to the workflow
  unless C code is genuinely being compiled — which it should not be.

## App identity

- appids `com.nuvionative.app` and `com.nuvionative.app.debug` (they can sit beside each other),
  title "Nuvio Native". Installs beside `space.nuvio.webos`; never touches it.
