# Handoff: finish Nuvio Native (for Codex)

Repo: ~/Developer/nuvio-native/app (branch main, HEAD 5dec3ea). A fork of PlxNative (Rust native webOS
Plex client, GPL-3). Read docs/nuvio/ARCHITECTURE.md and docs/nuvio/PLAYBACK.md first. Commit as
`Chris <dahl1376@umn.edu>` with no AI attribution lines. Never modify the installed Nuvio app
`space.nuvio.webos`. Never print or commit addon URLs (they embed debrid credentials) or
rust-modules/stremio/nuvio.local.env.

## Architecture
- `rust-modules/stremio` (plx_stremio): Stremio addon client, Nuvio account (Supabase) + localStorage import,
  stream classification (resolution/HDR/DV/size/bitrate), torrent helpers. 37 tests pass.
- `rust-modules/pmsshim` (plx_pmsshim): in-process HTTP server on 127.0.0.1:32420 speaking the Plex
  Media Server dialect over plx_stremio, so PlxNative's UI and player work unchanged. Booted from
  rust-modules/src/app/nuvio.rs (dev-token boot, no plex.tv).
- App id com.nuvionative.app (debug: com.nuvionative.app.debug).

## Build / deploy (all working)
- `source ~/.cargo/env; make ipk` → pkg/com.nuvionative.app.debug_0.8.0_arm.ipk
- Release: `make FLAVOR=stable RELEASE=1 ipk` → pkg/com.nuvionative.app_0.8.0_arm.ipk
- TV: ares device `tv` (10.0.0.73, LG CX webOS 5, Developer Mode, NOT rooted). Use tools/nv-tv.py
  (install/launch/close/log/grep/keys/trigger/shot). `ares-push` is unsupported; use ares-novacom.
- Import the user's Nuvio addons (Nuvio must be running): `node tools/nv-import-nuvio.mjs 10.0.0.73 --appid com.nuvionative.app.debug`
- UI screenshot: `tools/nv-tv.py trigger capture`, launch, then `tools/nv-tv.py shot out.jpg` (UI plane only).

## Verified on the TV
- Installs beside Nuvio, launches, 60 fps UI (Nuvio web baseline: 22.8 fps avg, 37% late frames;
  tools/bench/webapp-fps.mjs).
- Shim starts, imports the user's 4 addons (Nuvio Catalog, OpenSubtitles v3, AIOStreams "SELection",
  BetterPosters), boots straight to Home, detail page renders with real metadata ("Direct Play").

## Remaining work, in priority order
1. **Playback on the TV does not start yet.** Pressing Play on a movie detail page didn't reach the player; the event log
   shows repeated `net: curl rc=7 — transport error` (connection refused) and
   `stream: GET /photo/:/transcode status=404`. Find which origin curl fails to reach: possibly the
   shim's advertised origin/link classification (`boot: dev token — link=Remote`), plex.tv/telemetry
   calls, or the stream fetch. Fix, then verify playback with an AIOStreams 4K HEVC/DV link
   (follow the TV checklist in PLAYBACK.md).
2. Shim: answer `/photo/:/transcode` (302 to the source image URL, or resize-free passthrough).
3. Persistence: in Developer Mode only /tmp is writable for the app uid, so addons live in
   /tmp/<appid>/nuvio and are lost on TV reboot. Persist through the existing DB8 storage helper
   (`plxnative-storage` / platform::storage) instead.
4. The route takes codecs from the detail store (guessed from filenames); use the decision response /
   `plx_media::ff::probe::probe_url` results instead.
5. Stream picker UI: the shim exposes `/nuvio/streams` and `/nuvio/select`; wire them into the existing
   version/source list (appkit/source_list.rs) so users can pick resolution/HDR/size.
6. Login screen still says Plex; rebrand strings. Nuvio QR account login is implemented but never run end to end.
7. Torrents: Nuvio's companion service runs a Stremio server on 127.0.0.1:2710-2714 (start via luna
   ping to space.nuvio.webos.service); user has P2P disabled, so this is low priority.
8. DTS audio can't be decoded (no FFmpeg audio decoders); falls back to AC3/EAC3/AAC tracks.
9. Final: `make FLAVOR=stable RELEASE=1 ipk`, install com.nuvionative.app, verify launch + Home +
   playback + seek/pause/subtitles on the TV, then report the package path and test results.
