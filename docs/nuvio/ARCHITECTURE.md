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
- **A press opens the picker; a pick IS the play.** The Detail page's **Streams** pill, an
  episode's still (for THAT episode), the Home hero's **Play** (for the billboard's film) and a
  movie page's pill all raise the same `ContentPanel::AltSources` panel, and the commit starts the
  thing the panel was about — `AppMsg::NuvioStreamSelected` is what a successful `/nuvio/select`
  delivers back to the page that opened it. On an ordinary Plex server the picker is not offered,
  so those presses play or navigate as before. A pick is taken while addons are still answering,
  and it ends the shim's own wait: `Shim::ranked`'s `chosen` argument stops the grace/deadline wait
  once a stream is recorded, so the `/decision` and `Part` that follow a pick answer at once.
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

## App identity

- appid `com.nuvionative.app`, title "Nuvio Native". Installs beside `space.nuvio.webos`; never
  touches it.
