# NuvioNative features (since fork)

NuvioNative is a fork of PlxNative (upstream `GLinnik21/plx-native`) that turns the client into a
native LG webOS browser/player for **Stremio addons** (Cinemeta, Torrentio, AIOStreams, …) and the
**Nuvio account**, while keeping PlxNative's GPU UI and native Starfish/ACB video pipeline. This
file is a running catalog of what the fork adds on top of upstream; companion docs
(`ARCHITECTURE.md`, `ID-MANAGEMENT.md`, `TMDB.md`, `PLAYBACK.md`, `WATCH-PROGRESS.md`) hold the
design detail.

## Addon backend (Stremio → Plex dialect)

- In-process PMS shim (`plx_pmsshim`) on `127.0.0.1:<port>` speaking the Plex Media Server
  REST dialect over Stremio addons, so PlxNative's UI/stores/player need no changes.
- `plx_stremio`: Stremio addon client, Nuvio account + localStorage import, stream classification,
  torrent resolution.
- Catalogs → PMS library sections and Home hubs; metas → movie/show/season/episode; search →
  `/hubs/search`; streams → an item's `Media[]` (one Media per stream, with resolution/codec/HDR/
  size/bitrate from `plx_stremio::classify`).
- Two-level stream pickers (addons → that addon's streams) shared by the player `…` popover, the
  Detail page **Streams** pill, and the Home hero **Play**, with BACK walking one level at a time.
- Addon configuration stored as an opaque JSON record in the app's private DB8 kind (separate records
  per debug/stable/nightly), loaded/saved through the authenticated storage helper with readback
  verification.
- Torrents (`infoHash`) play through a Stremio-compatible streaming server URL when configured;
  debrid-resolved HTTP links (Torrentio/AIOStreams) play directly; local server scanning off by
  default, gated by `enableTorrents`.
- Nuvio account sign-in surface on the TV; addon list imported from the account (Supabase) since
  WebView localStorage is unreadable cross-app in Developer Mode.
- Release discovery permits plaintext credentials for the registered in-process shim only when its
  origin/token/machine identity match boot registration and its address is literal loopback.

## Playback

- Part URLs redirect (302) or proxy (when headers needed) to the stream's real URL; credentials
  never cross origin; seeks reopen with `Range`.
- TLS roots-mode fallback: debrid/addon hosts verified against the TV store first, retried once
  against bundled Mozilla roots on missing-issuer failure; control plane gets the same ladder.
- 45s first-byte grace for cold media streams; prewarm torrent pieces on create; reject TrueHD in
  forced direct play.
- Auto-play movies immediately after stream selection; immediate playback during background polling;
  Home hero play button and TV episode clicks route to the Streams picker instead of direct play.
- Source picker groups streams by addon on both Detail and in-player menu; Plex quality ladder
  removed.
- Seeder counts shown on the in-player source page and leading stream details.
- P2P streaming toggle and server URL in Playback settings; headless Stremio P2P server bundled as
  a webOS service (node child spawned from `js-service/`).
- Buffer tuning for low-RAM TVs (50 MB video / 4 MB audio; later 15/2), skip disk cache when
  caching disabled.
- Subtitle language preference respected; audio/subtitle preferences persist across cold boot.

## TMDB enrichment & discovery

- Cast on detail rows, "More Like This" recommendations, title logo (`clearLogo`), person pages for
  clickable headshots, collection/franchise shelves.
- Single HTTP request per title (`append_to_response=credits,recommendations,images`); responses
  cached 15 min.
- `TMDB_API_KEY` baked at compile time (GitHub Secrets) with runtime env fallback; unset key
  disables TMDB gracefully.
- Synthetic numeric rating keys for TMDB-only items (related movies/shows, people, collections)
  with a key table for real catalog items; see `ID-MANAGEMENT.md`.
- Infinite TMDB related drill-down with single-request optimization.
- ClearLogo fetched via TMDB images on synthetic related rows; ClearArt transparent-margin auto-crop.

## My List & watch progress (account sync)

- My List uses Nuvio's `sync_pull_library`, `sync_push_library_items` and
  `sync_delete_library_items` RPCs, one title + active profile per write, with 60s read cache and
  confirmed-write refresh.
- `/nuvio/library` and `/hubs/nuvio/list` shelf paging; UI runs requests on a worker and refreshes
  Home after a confirmed reply.
- Watch progress mirrored to the Nuvio account (push on timeline, pull/merge on boot or profile
  switch); Continue Watching becomes the account's, not the device's.
- TMDB↔IMDb ID translation for My List sync and related items.

## UI polish

- BetterPosters Rotten Tomatoes posters; WebP poster decode; RT/IMDb rating-source preference in
  native session record.
- Dynamic OSD layout for movie vs episode ClearArt; 4× ClearArt; left-aligned OSD logo.
- Subtitles rendered at video resolution (crisp 4K) with thicker outlines; scaled outlines.
- Movie collections and franchise shelves on Home.
- Addons settings page with per-addon enable/disable (session-scoped).
- Focus-engine dispatch patch resolving dead cursor trap on OK press; More menu cursor stays on a
  row.
- Dynamic ambient background gradient (Home/Library/Detail) restored: `plx_pmsshim` now emits a
  deterministic `UltraBlurColors` envelope per poster URL, so the wash tracks the focused artwork
  instead of the static brown ground.
- Auto-crop transparent margins from ClearArt logos.

## Audio / video codec handling (fork)

- Dolby Vision Profile 7 → 8.1 in-app conversion; DV preserved on every video-copy remux.
- In-app AC3 encode lane for FLAC/PCM above 2ch and TrueHD (always direct play); DTS routed
  through AC3 encode lane; native Opus, DTS-HD core above ceiling, lossless PCM lane.
- Runtime playback knobs from `/media/developer/plxnative-fork.conf` (`dts_core` auto|never, etc.).
- Direct-play Dolby Vision profiles 5/8; 96 MiB video / 8 MiB audio AU queues for high-bitrate
  direct play.
- Full native video sink counts as a primed decoder.

## Build, packaging & tooling

- Rebrand: app id `com.nuvionative.app` (debug `.debug`), title, icons from Nuvio brand mark.
- GitHub Actions workflow building the IPK; CI builds only on tag creation.
- P2P server merged as a webOS service into a single IPK; `launch:true` descriptor.
- ffmpeg fallback to GitHub; node invoked directly instead of luna service.
- Developer-mode tooling: `tools/nv-tv.py` (install/launch/keys/log/capture without root),
  addon importer over DevTools, web-app FPS benchmark.
- `TMDB_API_KEY` injected at compile time via GitHub Secrets.

## Notes

- Upstream PlxNative features (60 fps GPU UI, QR sign-in, direct play, resume/seek/chapters,
  Skip Intro/Credits, Up Next, etc.) are inherited and not listed here.
- For design rationale and constraints see `ARCHITECTURE.md` and the per-topic docs in this folder.
