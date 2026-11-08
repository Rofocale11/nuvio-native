# NuvioNative features (since fork)

NuvioNative is a fork of RichiL10/plx-native's `feature/v0.8-hq` branch, itself based on PlxNative
(`GLinnik21/plx-native`). It turns the client into a native LG webOS browser/player for **Stremio
addons** (Cinemeta, Torrentio, AIOStreams, …) and the **Nuvio account**, while keeping PlxNative's
GPU UI and native Starfish/ACB video pipeline. This file is a running catalog of what the fork adds
on top of upstream; companion docs (`ARCHITECTURE.md`, `ID-MANAGEMENT.md`, `TMDB.md`,
`PLAYBACK.md`, `WATCH-PROGRESS.md`, `DETAIL-LATENCY.md`) hold the design detail.

## Addon backend (Stremio → Plex dialect)

- In-process PMS shim (`plx_pmsshim`) on `127.0.0.1:<port>` speaking the Plex Media Server
  REST dialect over Stremio addons, so PlxNative's UI/stores/player need no changes.
- `plx_stremio`: Stremio addon client, Nuvio account + localStorage import, stream classification,
  torrent resolution.
- Catalogs → PMS library sections and Home hubs; metas → movie/show/season/episode; search →
  `/hubs/search`; streams → an item's `Media[]` (one Media per stream, with resolution/codec/HDR/
  size/bitrate from `plx_stremio::classify`).
- **Library Type menu as a catalog selector.** `includeMeta=1` advertises every browseable catalog
  of the section (with the shim-only `catalogKey`); choosing one sends `?catalog=<key>` and the
  shim lists that catalog only. Genre is filtered through the addon when declared and in memory
  when not; Title/Rating/Release Date sorts are computed in memory and Popularity is the catalog's
  own order. Real PMS servers and failed addon loads omit `catalogKey`, so the client keeps its
  original hardcoded Type menu as the fallback.
- **The Library header's Catalogs control opens that menu.** The header slot that used to hold a
  "Collections · N" button now holds a *Catalogs* control that names the catalog in force
  ("Catalogs · Netflix Movies") and stands at the left edge over the first poster, beside
  *Recommended*; *Sort & Filter* stays at the right edge. It opens `LibraryMenuKind::Type`, so it is
  tab-dependent for free: Movies offers the section's movie catalogs, TV Shows its show catalogs.
  The Collections button was deleted rather than fixed: its count was fabricated (cached rows plus a
  page-size hint) and it spun forever. The Collections page is no longer offered from Sort & Filter
  either, so the menu is now Sort and Filter only.
- **A named catalog is never rewritten by the sort.** The Sort menu reorders *within* the catalog
  the reader picked (Popularity is the catalog's own order); only a listing that names no catalog
  still maps Popularity/Rating onto Cinemeta's `top`/`imdbRating` catalogs.
- **The Library tab is grid-only.** It does not fetch or draw the section's horizontal hub rows;
  Movies/TV Shows open with Type/Sort/Filter directly above the grid. Home shelves are unaffected.
- **TMDB baseline for the Filter and Sort menus.** The genre list merges the catalog's declared
  genres with TMDB's standard movie/TV list, and the Sort menu always offers Popularity, Rating,
  Release Date and Title. A genre the addon does not support is matched in memory against the
  rows' `Genre[]` (alias-aware: `Sci-Fi` = `Science Fiction`, `Action` matches `Action & Adventure`).
- Two-level stream pickers (addons → that addon's streams) shared by the player `…` popover, the
  Detail page **Streams** pill, and the Home hero **Play**, with BACK walking one level at a time.
- Addon configuration stored as an opaque JSON record in the app's private DB8 kind (separate records
  per debug/stable/nightly), loaded/saved through the authenticated storage helper with readback
  verification.
- Torrents (`infoHash`) play through the in-process C engine (`libsbengine.so`), the only
  streaming engine — the legacy Node streaming server (`js-service/server.js`) is retired and
  no longer spawned; when the C engine cannot load, torrents are unavailable on that install.
  A configured `torrentServer` URL can be used directly; debrid-resolved HTTP links
  (Torrentio/AIOStreams) play directly. Local server scanning is off by default and
  requires `enableTorrents`.
- Nuvio account sign-in surface on the TV; addon list imported from the account (Supabase) since
  WebView localStorage is unreadable cross-app in Developer Mode.
- Release discovery permits plaintext credentials for the registered in-process shim only when its
  origin/token/machine identity match boot registration and its address is literal loopback.

## Playback

- Part URLs redirect (302) or proxy (when headers needed) to the stream's real URL; credentials
  never cross origin; seeks reopen with `Range`.
- TLS roots-mode fallback: debrid/addon hosts verified against the TV store first, retried once
  against bundled Mozilla roots on missing-issuer failure; control plane gets the same ladder.
- 45s first-byte grace for cold media streams; torrent resolve waits for a short prebuffer
  before serving the loopback URL; TrueHD in forced direct play is routed through the AC3
  encode lane.
- Auto-play movies immediately after stream selection; immediate playback during background polling;
  Home hero play button and TV episode clicks route to the Streams picker instead of direct play.
- Source picker groups streams by addon on both Detail and in-player menu; Plex quality ladder
  removed.
- Seeder counts shown on the in-player source page and leading stream details.
- P2P streaming toggle in Playback settings; the server URL is optional (the C engine needs
  none) and empty uses the local engine's ports. One engine ships: the in-process C engine
  (`libsbengine.so`); the legacy Node streaming server (`js-service/server.js`) is retired
  and no longer spawned — when the C engine cannot load, torrents are unavailable instead.
- Buffer tuning for low-RAM TVs (50 MiB video / 4 MiB audio), skip disk cache when
  caching disabled.
- Subtitle and audio preferences: `defaultAudioLanguage`, `defaultSubtitleLanguage`,
  `autoSelectSubtitle` and `defaultSubtitleForced` are written to `nuvio-session.json` whether or
  not a Nuvio session is signed in, through one atomic writer; they survive a cold boot and are
  wiped on sign-out. Playback reads that same file — for a Nuvio profile it is the only account
  surface — so the first launch of the day starts on the saved language without a network round
  trip.

## TMDB enrichment & discovery

- Cast on detail rows, "More Like This" recommendations, title logo (`clearLogo`), person pages for
  clickable headshots, collection/franchise shelves.
- Single HTTP request per title (`append_to_response=credits,recommendations,images`); responses
  cached 15 min.
- `TMDB_API_KEY` baked at compile time (GitHub repository variable) with runtime env fallback; unset key
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
- Watch progress mirrored to the Nuvio account (push on timeline; pull/merge on boot or profile
  switch, after that profile's addons are installed so every resume point can build its row); the
  client re-reads Home when the merge lands, so Continue Watching does not wait for a relaunch.
- TMDB↔IMDb ID translation for My List sync, related items, and every addon-facing stream/subtitle
  request whose title is TMDB-only (`tmdb:<id>` → `tt…`, episode tails preserved).
- Player subtitle search (`Subtitles → Search`) answered from the builtin OpenSubtitles v3 addon
  and the account's own subtitle addons, filtered to the chosen language and opening on the
  preferred subtitle language from `nuvio-session.json`; installing a candidate adds it to the
  item as a sidecar and makes it the selected track.

## UI polish

- BetterPosters Rotten Tomatoes posters; WebP poster decode; RT/IMDb rating-source preference in
  native session record.
- ClearArt logos are cropped to their ink (title logo only) with the crop cached, so
  transparent margins do not offset the OSD logo.
- Subtitles rendered at video resolution (crisp 4K) with thicker outlines; scaled outlines.
- Movie collections and franchise shelves on Home.
- Addons settings page with per-addon enable/disable (session-scoped).
- Focus-engine dispatch fixes focus being left off the table;
  More menu cursor stays on a row.
- Dynamic ambient background gradient (Home/Library/Detail) restored: `plx_pmsshim` now emits a
  deterministic `UltraBlurColors` envelope per poster URL, so the wash tracks the focused artwork
  instead of the static brown ground.
- Auto-crop transparent margins from ClearArt logos.

## Audio / video codec handling (fork)

- Dolby Vision Profile 7 → 8.1 in-app conversion; DV preserved on every video-copy remux.
- In-app audio lanes: AC3 encode for FLAC/PCM above 2ch and TrueHD (always direct play); DTS
  routed through the AC3 encode lane; native Opus; DTS-HD core above ceiling; lossless PCM lane
  for FLAC/ALAC, with stereo above 48 kHz decimated in-app; MP3/Vorbis app decode behind the
  `lossy` knob.
- Playback speed 0.5×–2× done in the app: timestamps scaled, sound decoded and time-stretched
  (WSOLA) to 48 kHz PCM.
- Settings > Playback > Dolby Vision (Auto / Off), one pick per profile: Off sends no Dolby
  Vision node, so a P8/P7 file plays its HDR10 base layer and a P5 file with no base to fall back
  to is refused. Diagnostics and the `dv:` log line still report what the set's probe answered.
- Runtime playback knobs from `/media/developer/plxnative-fork.conf` (`dts_core` auto|never, etc.).
- Direct-play Dolby Vision profiles 5/8; 50 MiB video / 4 MiB audio AU queues for high-bitrate
  direct play.
- Full native video sink counts as a primed decoder.

## Build, packaging & tooling

- Rebrand: app id `com.nuvionative.app` (debug `.debug`), title, icons from Nuvio brand mark.
- GitHub Actions workflow building the IPK (manual dispatch, or on tags; CI also runs on push/PR).
- P2P server merged as a webOS service into a single IPK.
- ffmpeg fallback to GitHub; node invoked directly instead of luna service.
- Developer-mode tooling: tv-lock/tv-session (device access only via the skills),
  addon importer over DevTools, web-app FPS benchmark.
- `TMDB_API_KEY` injected at compile time via GitHub repository variable.

## Notes

- Upstream PlxNative features (60 fps GPU UI, QR sign-in, direct play, resume/seek/chapters,
  Skip Intro/Credits, Up Next, etc.) are inherited and not listed here.
- For design rationale and constraints see `ARCHITECTURE.md` and the per-topic docs in this folder.
