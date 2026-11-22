# Nuvio Native handoff (2026-10-10)

Repo: ~/Developer/nuvio-native/app. Fork: https://github.com/current/nuvio-native. Commit as `Rofocale11 <131918567+Rofocale11@users.noreply.github.com>` with no AI
attribution lines. Never modify the installed `space.nuvio.webos` app. Never print
or commit addon URLs or `rust-modules/stremio/nuvio.local.env`.

Read `ARCHITECTURE.md` and `PLAYBACK.md` for the shim and decoder contracts.

## Build and TV

- Debug: `source ~/.cargo/env; make ipk` → `pkg/com.nuvionative.app.debug_0.8.0_arm.ipk`.
- Stable: `source ~/.cargo/env; make FLAVOR=stable RELEASE=1 ipk` →
  `pkg/com.nuvionative.app_0.8.0_arm.ipk`.
- TV: ares device `tv`, LG CX, webOS 5, Developer Mode, not rooted. Use
  `tools/nv-tv.py` under `NV_DEVICE=tv tools/tv-lock.sh with …`.
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
  `127.0.0.1:2710–2714`, not the shim at port 32420. Automatic scans now require
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
- BetterPosters (`btttr.cc`) supplies posters for IMDb movie/series identities across
  Home, search, My List and detail metadata. Posters request Rotten Tomatoes scores
  (`rs=RT`); non-IMDb titles retain their addon artwork. The image proxy falls back
  to the original image when the poster provider fails.
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
- Addons restored from DB8 after app reinstall/relaunch.

## Device playback verification (upstream merge, 2026-11-22)

The owner deployed the merged debug build to an LG `45UR7500PSC` (webOS 10.3.1,
board `K8LPN_DVB`) and confirmed playback of a probed addon stream. The event log
shows the probe (then `ff::probe`, now `plx_nuvio_probe`) reporting `hevc 3840x2160`,
a DIRECT PLAY decision, `SMP Load ok=1`, the exported window bound to the video plane,
`feed: presented`, `timeline playing` advancing for ~135 s, and a clean stop with the
callback gate reporting `dropped=0`. See `docs/nuvio/MERGE-AUDIT.md` for the exact
evidence and its scope (HDR10, not DV P8; no seek/resume cases in this run). The
extraction itself still needs a sim probe and one TV playback run.

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

- Watch progress and the shim key table now live in the same `nuvio.redb` as the addon config
  and every preference; they survive a reboot only when `nuvio_data_dir()` resolves to a
  persistent directory (confirm on the set — the last fallback is `/tmp`).
- The prior stable QR attempt authenticated but failed discovery with
  `discovery_insecure_only`. A host regression reproduces that failure and passes
  with the registered-shim fix. Fresh stable approval reached the server and loaded
  account addons.
- Rotten Tomatoes on posters comes from BetterPosters. Separate detail scores depend
  on addon metadata. The user chose poster ratings for now; no MDBList key was added.
- The Nuvio account backend session, addon configuration and all native preferences are
  records in the one `nuvio.redb`; none of them is a DB8 record or a separate runtime file.
- Torrents require a configured Stremio-compatible server or explicit local scanning.
  The user's P2P-disabled setup was tested with HTTP/debrid streams.
- <!-- Fork Rofocale11: was "No DTS/TrueHD audio conversion. A compatible AAC/AC3/EAC3 track is required." -->
  In Auto the app now converts DTS, TrueHD and Opus to stereo 48 kHz PCM when the TV has no native path; a compatible AAC/AC3/EAC3 track is no longer required for those.
- P7 enhancement-layer playback is unsupported; the HDR10 base-layer fallback has
  not been tested on this CX. P8 Dolby Vision was tested.
- Some upstream images/logos are absent; the proxy cannot manufacture missing images.

The stable verification procedure is in `VALIDATION.md`; the final task report records
its observed results.
