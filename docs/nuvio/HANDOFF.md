# Nuvio Native handoff (2026-10-10)

## Current status (HEAD `eef501647`, 2026-11-24)

Everything from "Build and TV" below is the original 2026-10-10 snapshot, kept as history. The
shipped tree is `nuvio-native` at `eef501647` in https://github.com/Rofocale11/nuvio-native; the
authoritative old fork is https://github.com/Rofocale11/nuvio-native-old (branch `nuvio-native`)
at `83339172d`. This last port was not pushed, tagged, PR'd or released. Current facts that
supersede the snapshot:

- Version **0.10.0**, app id `com.nuvionative.app` (debug `.debug`); all packaging witnesses
  agree and `requiredPermissions` is empty. IPKs: `pkg/com.nuvionative.app_0.10.0_arm.ipk` and
  `pkg/com.nuvionative.app.debug_0.10.0_arm.ipk`. The 0.8.0 names below are historical.
- Every durable record is in **one `nuvio.redb`** — no DB8, no `plxstorage` helper, no DB8
  permission grant. Self-heal quarantines unreadable canonical records; a 50 MiB soft budget
  sheds only cache rows. References to DB8 restoration below describe the pre-migration builds.
- Torrents run through the **in-process C engine** (`libsbengine.so`, no Node fallback), not a
  separate Stremio-compatible server. Settings > Playback has a P2P switch (absent key means ON)
  and Settings > Addons lists/flips installed addons; Default quality, Direct Play and Playback
  engine rows are hidden, not deleted. A torrent/debrid diagnostics panel replaced the inert
  playback-controller placeholders.
- On the committed tree `make check` reported **146 steps — all ok** (harness 753 tests), with
  `plx_screens` 2347/2347, app crate 1503/1503, `plx_plex` 826/826, `plx_data` 1663/1663,
  `plx_pmsshim` 159/159, `plx_net` 338/338, `plx_os` 427/427, `plx_nuvio_store` 6/6 and
  `plx_stremio` 54/54. `make check-nodef` and a 32-bit ARM cross-build passed. The three
  `transcoder.rs` opus failures below were fixed by the `make check` alignment pass.
- The stable 0.10.0 build was deployed with `make FLAVOR=stable RELEASE=1 deploy`
  (`verify-deploy: 50 files match`) and the owner confirmed it works. Still open: P7 HDR10
  fallback, cross-device watch sync on a real account, a cold detail open inside 800 ms, a
  dedicated sim-probe comparison after the `plx_nuvio_probe` extraction, and an in-app reset.

Repo / current fork: https://github.com/Rofocale11/nuvio-native. Commit as
`Rofocale11 <131918567+Rofocale11@users.noreply.github.com>` with no AI
attribution lines. Never modify the installed `space.nuvio.webos` app. Never print
or commit addon URLs or `rust-modules/stremio/nuvio.local.env`.

Read `ARCHITECTURE.md` and `PLAYBACK.md` for the shim and decoder contracts.

---

## 2026-10-10 snapshot

## Build and TV

- Debug: `source ~/.cargo/env; make ipk` → `pkg/com.nuvionative.app.debug_0.8.0_arm.ipk`.
- Stable: `source ~/.cargo/env; make FLAVOR=stable RELEASE=1 ipk` →
  `pkg/com.nuvionative.app_0.8.0_arm.ipk`.
- TV (at this snapshot): ares device `tv`, LG CX, webOS 5, Developer Mode, not rooted. Use
  `tools/nv-tv.py` under `NV_DEVICE=tv tools/tv-lock.sh with …`. Later verification used an LG
  `45UR7500PSC` (webOS 10.3.1, board `K8LPN_DVB`) reached over SSH by `make deploy`; hold
  `tools/tv-lock.sh` either way and release it as soon as the device work pauses.
- With the original Nuvio running, import its configuration using
  `node tools/nv-import-nuvio.mjs 10.0.0.73 --appid com.nuvionative.app.debug`
  (use `com.nuvionative.app` for stable). This reads the original app's localStorage.
- Debug UI captures require the `capture` trigger at launch. `shot --frames 6`
  while navigating skips cached frames. Captures exclude the hardware video plane.
  Stable releases have no capture listener or remote FIFO; use the physical remote.

## Completed work

- Play now uses the loaded detail item when the catalog selection has no Part.
  Before creating the playback plan, the route asks the shim for its probed
  decision and consumes that response's codecs, frame rate, tracks and DV fields.
  Unprobed or unsupported media is refused, rather than loaded with guessed codecs.
- The refused curl connections were optional torrent-server scans at
  `127.0.0.1:2710–2714`, not the shim's own loopback port (now 8911 debug / 8910 stable;
  32420 appears only in the old log fixtures). Automatic scans now require
  `enableTorrents: true`; HTTP/debrid playback works without them.
- The photo endpoint resolves relative metadata images and proxies image bytes;
  supported CDN sizing avoids decoding unnecessarily large posters.
- TV addon configuration, the canonical session and every Settings preference now live in ONE
  `nuvio.redb` under `<data>/shim/`, with upstream's storage backend running in process against
  that file. No DB8 helper is packaged or activated; legacy/import files are removed only after
  verified redb persistence. The importer still uses a group-writable handoff directory so the
  native UID can unlink its dev-owned file. Host configuration remains file-backed.
- The Streams overlay lists the shim's ranked choices, with codec, HDR/DV,
  resolution, size and bitrate, and selects through `/nuvio/select` before refreshing
  detail metadata. It waits for discovery so indices do not shift under a selection.
- Login copy is branded Nuvio in all three supplied locales. Release discovery now
  admits the exact registered in-process shim (matching origin, token and machine
  identity); unregistered loopback and remote HTTP candidates retain the release policy.
- Initial audio selection prefers compatible English dialogue over a foreign-language
  file default or English commentary. Explicit audio selection still takes precedence.
- Settings → Playback → Rating source offers IMDb or Rotten Tomatoes. The preference
  uses the canonical session record in the single redb file and updates cached detail presentation. Rotten
  Tomatoes detail scores must be supplied by a metadata addon; absent scores remain absent.
- BetterPosters (`btttr.cc`) supplies posters wherever the shim resolves an IMDb id — not
  only for a literal `tt…` item id, but also for `tmdb:` catalog items and synthetic TMDB
  rows, whose IMDb id comes from the addon meta or one cached TMDB `external_ids` lookup.
  Posters request Rotten Tomatoes scores (`rs=RT`); titles with no IMDb mapping retain
  their addon artwork, and the image proxy falls back to the original image on a 404.
  TMDB-only related cards keep their TMDB poster: a recommendation result carries no IMDb
  id, and resolving one per card would add ten HTTP round trips to the detail page.
- TMDB is the primary source for landscape backdrops and clear logos, including on detail,
  Continue Watching, related and franchise rows. Cinemeta's `images.metahub.space` art is
  **strictly a fallback** when TMDB has no answer or the key is not configured; it is never
  chosen as a Continue Watching backdrop over a poster crop. A missing TMDB logo omits
  `clearLogo` entirely rather than substituting the poster.
- Movie/show details have Add to My List / Remove from My List. The active Nuvio
  account profile owns the list; individual item RPCs preserve other devices' titles.
  A fresh pull confirms each mutation before the button updates. Home has a paged
  My List shelf. Signed-out users can open login from the button; errors offer Retry.
- Diagnostic redirect logs omit private URL paths, and the TV helper redacts URLs.

## Verified on debug TV

- Install, launch, Home, real metadata, posters/backdrops, and all four imported
  addons: Nuvio Catalog, OpenSubtitles v3, AIOStreams SELection, BetterPosters.
- AIOStreams 86.4 GB HEVC 3840×2160 at 23.976 fps, DV P8, AC3: Starfish reported
  `hdrType: DolbyVision`; displayed frame counts and play position advanced.
- Pause held the play position; resume advanced it. Multiple seeks returned success
  and resumed playback through CDN range reopens.
- Audio switch to EAC3, selection of another 4K DV source (3832×1600), and playback
  after that source switch. Selected embedded subtitles returned HTTP 200 and emitted
  nonempty timed subtitle cues (dialogue is never logged).
- Addons restored after app reinstall/relaunch (from DB8 at this snapshot; the single redb
  replaced DB8 on 2026-11-22).

## Device playback verification (upstream merge, 2026-11-22)

The owner deployed the merged debug build to an LG `45UR7500PSC` (webOS 10.3.1,
board `K8LPN_DVB`) and confirmed playback of a probed addon stream. The event log
shows the probe (then `ff::probe`, now `plx_nuvio_probe`) reporting `hevc 3840x2160`,
a DIRECT PLAY decision, `SMP Load ok=1`, the exported window bound to the video plane,
`feed: presented`, `timeline playing` advancing for ~135 s, and a clean stop with the
callback gate reporting `dropped=0`. See `docs/nuvio/MERGE-AUDIT.md` for the exact
evidence and its scope (HDR10, not DV P8; no seek/resume cases in this run). At that
point the extraction itself still needed a sim probe and one TV playback run; later
debug and stable installs shipped it and the owner confirmed the current stable build
works, but a dedicated sim-probe comparison is still not recorded here.

## Stable TV observation

The user confirmed Home, 4K Dolby Vision playback, seek, pause/resume and visible
English subtitles on the stable build before the audio/login/rating follow-up.
A fresh approval on the follow-up release reached the local server and loaded the
account addons. Physical checks of the new audio default and rating setting remain
pending; do not treat the stream probe as proof of playback.

## BetterPosters / My List delivery

The refreshed debug TV rendered BetterPosters with RT percentages, including
Continue Watching. Add and Remove from My List were confirmed by fresh backend
pulls, and the existing list returned after the test. The final stable package
built from `6e9c2acc` installed with a matching payload hash, launched Home with
`features=release`, restored four DB8 addons and loaded the saved library.
See `VALIDATION.md` for the checks and the intentionally stopped broad host run.

## Known limitations / follow-up

- Watch progress, its account-key map and the shim key table now live in the same `nuvio.redb` as
  the addon config and every preference; they survive a reboot only when `nuvio_data_dir()` resolves
  to a persistent directory (confirm on the set — the last fallback is `/tmp`). The last-played
  source fingerprint lives there too: a resume re-selects the file the viewer actually played while
  addons still offer it, without ever storing a signed URL. The shim now mirrors
  `/:timeline` into the Nuvio account and pulls other devices' resume points back, with IMDb-first
  identity, a 60 s playing throttle and a newer-wins merge. Home re-reads its hubs when a real merge
  bumps the shim's content generation, and a lane seated on an empty first answer adopts the cards
  that arrive later. See `WATCH-PROGRESS.md`; cross-device sync is not yet verified on a real account.
- The prior stable QR attempt authenticated but failed discovery with
  `discovery_insecure_only`. A host regression reproduces that failure and passes
  with the registered-shim fix. Fresh stable approval reached the server and loaded
  account addons.
- Rotten Tomatoes on posters comes from BetterPosters. Separate detail scores depend
  on addon metadata. The user chose poster ratings for now; no MDBList key was added.
- The Nuvio account backend session, addon configuration and all native preferences are
  records in the one `nuvio.redb`; none of them is a DB8 record or a separate runtime file.
- Account audio/subtitle preferences are **provided by the shim** on the same
  `/api/v2/user`, `/api/v2/user/profile` surface the mock grades: one Nuvio login is one profile,
  `/user`, the Home roster and switch share its identity, the Settings rows read and PUT through
  that surface, and the values persist in `AccountPrefs` across a restart.
- A Nuvio boot no longer takes upstream's dev-bootstrap path: the shim token is a PMS credential,
  not a dev identity, so the boot keeps the account authority, resumes the stored Nuvio sign-in
  and publishes its one profile. That seat is what makes the Settings rows editable. A fresh
  install with no stored sign-in now opens the Nuvio QR sign-in instead of dropping straight into
  Home.
- At this snapshot, torrents required a configured Stremio-compatible server or explicit
  local scanning, and the user's P2P-disabled setup was tested with HTTP/debrid streams.
  Superseded: the current tree runs the in-process C engine (`libsbengine.so`) and offers
  torrent picks when Settings > Playback P2P is ON (default ON); HTTP/debrid streams need
  no engine.
- <!-- Fork Rofocale11: was "No DTS/TrueHD audio conversion. A compatible AAC/AC3/EAC3 track is required." -->
  In Auto the app now converts DTS, TrueHD and Opus to stereo 48 kHz PCM when the TV has no native path; a compatible AAC/AC3/EAC3 track is no longer required for those.
- P7 enhancement-layer playback is unsupported; the HDR10 base-layer fallback has
  not been tested on this CX. P8 Dolby Vision was tested.
- Some upstream images/logos are absent; the proxy cannot manufacture missing images.

The stable verification procedure is in `VALIDATION.md`; the final task report records
its observed results.
