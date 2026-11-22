# NuvioNative architecture

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

- Catalogs → PMS library sections and Home hubs. Metas → movie/show/season/episode items.
  Search → `/hubs/search`. Streams → the item's `Media[]` list (one Media per stream, with
  resolution/codec/HDR/size/bitrate from `plx_stremio::classify`), so PlxNative's existing
  version/source picker is the stream picker. Part URLs redirect (or proxy, when headers are
  needed) to the stream's real URL. The decision endpoint waits for discovery/probing
  and refuses media that cannot direct play; the native route uses its probed fields.
  The Streams overlay uses `/nuvio/streams` and `/nuvio/select`, retaining the shim's
  ranked index instead of confusing it with the selected-first Media list.
- **One durable store, no DB8.** Every Nuvio Native record — the canonical session and all its
  Settings preferences, addon configuration, install-wide preferences (language, resolution,
  client id), the Nuvio account's audio/subtitle preferences, watch state, the ratingKey table,
  the Nuvio backend session and the shim's local settings — lives in a single `nuvio.redb` in
  `<data>/shim/`. Upstream's `storage_service::backend::Backend` is run **in process** by
  `plx_os::storage::redb_backend`: DB8's objects and Keymanager3's AES-256-GCM envelopes are two
  more tables in that file, so the session/preference/consent layers keep their exact wire
  semantics and tests. The separate `plxstorage` helper is no longer built, packaged or activated
  (the ipk declares no service). Host/simulator builds still use upstream's `Files` authority.
  Legacy on-disk files (`settings.json`, `watch.json`, `keys.tsv`, the account-session JSON and
  `language.json`) are read once as migration sources and written into redb. DB8 records cannot
  be migrated — no helper is available to read them — so anything that lived only there starts
  fresh.
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
- Watch state / progress: stored by the shim in the same redb file (the app already reports
  timeline to "the server"). Durability across a TV reboot is now whatever `nuvio_data_dir()`
  resolves to: `/media/developer/<appid>.nuvio` when writable, else the persistent state root,
  else the Developer Mode runtime fallback (`/tmp`). Confirm the chosen directory on the set
  before claiming reboot durability.

## Crates

The fork-owned crates are separate workspace members so an upstream rewrite of a shared file
cannot drag fork code through a merge. `plx_os`, `plx_plex` and the application crate have small
marked seams into `plx_nuvio_store`; `plx_pmsshim` and `plx_nuvio_probe` are entirely fork-owned.

| crate | owner | depends on |
|---|---|---|
| `rust-modules/stremio` (`plx_stremio`) | fork-owned backend | `plx_base`, `plx_net`, serde |
| `rust-modules/pmsshim` (`plx_pmsshim`) | fork-owned shim | `plx_stremio`, `plx_net`, `plx_nuvio_store`, serde |
| `rust-modules/nuvio-probe` (`plx_nuvio_probe`) | fork-owned probe | `plx_base`, `plx_net`, `plx_plex`, `plx_media_codecs` |
| `rust-modules/nuvio-store` (`plx_nuvio_store`) | fork-owned single store | `redb`, `plx_base` |
| `rust-modules/os` (`plx_os`) | upstream, fork-marked | `plx_nuvio_store`, `ring` — runs the canonical storage backend in process |
| `rust-modules/plex` (`plx_plex`) | upstream, fork-marked | `plx_nuvio_store` — Nuvio account preferences |
| `rust-modules/src/app` (`plxnative-modules`) | upstream, fork-marked | all of the above — boot, appinfo, settings |

## App identity

- appid `com.nuvionative.app`, title "Nuvio Native". Installs beside `space.nuvio.webos`; never
  touches it.
