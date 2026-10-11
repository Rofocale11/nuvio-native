# Nuvio Native handoff (2026-10-10)

Repo: ~/Developer/nuvio-native/app. Commit as `Chris <dahl1376@umn.edu>` with no AI
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
- TV addon configuration uses the private DB8 storage helper, separate from the
  session record. Legacy/import files are removed only after verified persistence. The importer uses a
  group-writable handoff directory so the native UID can unlink its dev-owned file,
  including stable's sticky `/tmp` runtime root.
  Host configuration remains file-backed.
- The Streams overlay lists the shim's ranked choices, with codec, HDR/DV,
  resolution, size and bitrate, and selects through `/nuvio/select` before refreshing
  detail metadata. It waits for discovery so indices do not shift under a selection.
- Login copy is branded Nuvio in all three supplied locales.
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

## Known limitations / follow-up

- Watch progress and the shim key table can still be lost on a TV reboot because
  their Developer Mode fallback is `/tmp`; the addon configuration is in DB8.
- Nuvio account QR login is implemented, but account authentication was not exercised
  end to end. Imported addon configuration was used for device testing.
- Torrents require a configured Stremio-compatible server or explicit local scanning.
  The user's P2P-disabled setup was tested with HTTP/debrid streams.
- No DTS/TrueHD audio conversion. A compatible AAC/AC3/EAC3 track is required.
- P7 enhancement-layer playback is unsupported; the HDR10 base-layer fallback has
  not been tested on this CX. P8 Dolby Vision was tested.
- Some upstream images/logos are absent; the proxy cannot manufacture missing images.

The stable verification procedure is in `VALIDATION.md`; the final task report records
its observed results.
