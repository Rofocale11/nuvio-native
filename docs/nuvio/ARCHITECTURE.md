# NuvioNative architecture

NuvioNative is a fork of PlxNative (GPL-3.0-or-later, upstream GLinnik21/plx-native@4e454ea) that
browses and plays content from Stremio addons — the same addons a user configured in Nuvio — with
PlxNative's GPU-rendered UI and native Starfish/ACB video pipeline.

Companion documents: `ID-MANAGEMENT.md` (numeric keys, the key table, synthetic keys),
`TMDB.md` (detail enrichment, person pages, collections), `PLAYBACK.md` (the stream → decoder
path), `WATCH-PROGRESS.md` (local watch state and the account mirror).

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

The P2P streaming server is the one component outside that process: a `node` child spawned from
the app's own `js-service/` directory. It is the streaming server torrent picks resolve through;
see "The P2P streaming server" below.

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
- **Account preferences live in one file, with one writer.** `defaultAudioLanguage`,
  `defaultSubtitleLanguage`, `autoSelectSubtitle` and `defaultSubtitleForced` sit under
  `preferences` in `nuvio-session.json`. `GET/PUT /api/v2/user/profile` on the shim is the editor
  and `Account::write_prefs` is the only writer (one lock, one snapshot, one atomic rename, a
  returned error the PUT turns into a 500). Playback reads the file through
  `account::local_preferences()`, not the account endpoint: a Nuvio profile has no plex.tv
  credential, and the shim's `/api/v2/user` carries no `profile` block. There is no Nuvio cloud
  preference API (`plx_stremio::nuvio` has profiles, addons, library and watch progress only), so
  the file is the source of truth; signing out wipes it with the session.
- Torrents (`infoHash`): played through the headless Node server the app spawns from its own
  `js-service/` directory, as `<server>/<infoHash>/<fileIdx>`, with
  `http://127.0.0.1:11470` as the default address. See "The P2P streaming server" below.
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
- Watch state / progress: stored locally by the shim (the app already reports timeline to "the
  server") and mirrored to the active profile through Nuvio's `sync_push_watch_progress` /
  `sync_pull_watch_progress` RPCs, so Continue Watching follows the account across devices. A
  paused or stopped report goes at once, a playing one at most once per 60 s per item; the newer
  `last_viewed_at` wins on the way back. See `WATCH-PROGRESS.md`. Developer Mode's
  writable-directory fallback is `/tmp`, so the local copy and the key table are not yet durable
  across TV reboots — the account copy is.

## The P2P streaming server

Torrent (`infoHash`) streams need a Stremio-compatible streaming server. The app ships Stremio's
desktop `server.js` and runs the server as a **direct child process** of the native app.

- **Why not an LS2 service.** `services.json` plus `luna-send` is the usual webOS shape, but this
  firmware's Node lacks the `palmbus`/`pmloglib` native modules, so a Node service cannot register
  or answer. A child process needs only the interpreter and the app's own directory.
- **Payload.** `make ipk` stages `js-service/server.js` into
  `usr/palm/applications/<APPID>/js-service/` inside the package. `make deploy` refreshes
  `<app_dir>/js-service/` on the television, and `make install` re-installs the whole package.
  `server.js` is the only JS payload: the old `js-service/server-settings.json` is gone, and both
  `make deploy` and `verify-deploy` copy/check only `server.js`.
- **Data directory.** `appPath` is `/tmp/nuvio-serverp2p` unless `APP_PATH` overrides it, and
  `cacheRoot` follows `appPath`. The generated settings and state are therefore
  `/tmp/nuvio-serverp2p/server-settings.json`,
  `/tmp/nuvio-serverp2p/stremio-cache/` (only while caching is on), and
  `/tmp/nuvio-serverp2p/httpsCert.json`. `/tmp` is
  writable on read-only production installs, and with `cacheSize: 0` nothing there needs to survive
  a reboot. `boot.rs` creates the directory and refuses to spawn when it is owned by a different
  uid, logging `rm -rf /tmp/nuvio-serverp2p` rather than trying to chown it. Startup also removes
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
  a test — `app::boot::start_p2p_service` creates `/tmp/nuvio-serverp2p` and spawns
  `/usr/bin/node server.js` with its working directory set to `<app_dir>/js-service`. If
  `/usr/bin/node` is absent, it falls back to `node` on `PATH`. The child's stdout and stderr
  append to `/tmp/nuvio-p2p.log` (both are discarded if the file cannot be opened). The child
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
- **Failure is local.** A missing `server.js`, a missing interpreter or a bind failure is logged
  and leaves the app fully usable; torrent picks simply cannot play. Debrid-resolved HTTP links do
  not use this server.

## Constraints that shape the code

- **Pure Rust only — no C libraries or JS engines in the TV build.** The app compiles for an ancient
  LG toolchain (`arm-webos-linux-gnueabi`). Crates such as `innertube-rs`, `rusty_ytdl`,
  `rquickjs` or `boa_engine` pull C code or need `libclang`/`bindgen`, and the ARM build fails on
  them. The bundled FFmpeg is built `--disable-everything` plus the demuxers/decoders the player
  actually uses; adding one means a change to `ci/build-ffmpeg.sh` and the buffer-feed pump. The
  one place audio is decoded and re-encoded in-app is the fork's AC3 encode lane
  (`media/src/ff_enc.rs`: `dca`/`truehd`/`mlp`/`flac` decoders, the integer `ac3_fixed` encoder),
  which is why those components are in the list — see `PLAYBACK.md`. The P2P server is an external
  `node` child process (see above), not an embedded JS engine: nothing here links a JS runtime into
  the app.
- **`ratingKey` must be numeric.** PlxNative parses it as an `i64`; a string like `"tmdb:603"`
  blanks the screen. `ID-MANAGEMENT.md` is how the shim keeps that promise.
- **No native YouTube trailers.** In-app trailer playback was abandoned: extracting YouTube MP4s on
  the TV without C libraries is too unstable. A detail row may carry a trailer thumbnail
  (`Extras` with `ratingKey: "trailer"` and a YouTube poster), but
  `/library/metadata/{rk}/trailer` is not a route the shim answers — the press fails rather than
  playing. Do not re-add extraction.
- **The shim is a loopback PMS.** One in-process HTTP server (`plx_pmsshim`, default
  `127.0.0.1:32420`) speaking the Plex dialect, in the same process as the UI.

## Crates

| crate | owner | depends on |
|---|---|---|
| `rust-modules/stremio` (`plx_stremio`) | backend | `plx_base`, `plx_net`, serde |
| `rust-modules/pmsshim` (`plx_pmsshim`) | shim | `plx_stremio`, `plx_base` |
| app wiring (boot, appinfo, settings) | lead/integration | all |

## Build and release

- **Host simulator:** `make -j10 sim-linux` (needs the Linux `libav*`/libass host libraries staged
  in `pkg/`); `make sim-macos` on macOS.
- **TV build (.ipk):** `make ipk`. A release is cut by pushing a tag
  (`git tag -a v0.8.1 -m "…" && git push origin v0.8.1`); the GitHub Actions runner builds the
  `.ipk` and attaches it to the Release. Do not add `libclang` or ARM cross-compiler steps to the
  workflow unless C code is genuinely being compiled — which it should not be.

## App identity

- appid `com.nuvionative.app`, title "Nuvio Native". Installs beside `space.nuvio.webos`; never
  touches it.
