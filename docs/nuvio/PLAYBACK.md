# NuvioNative playback: third-party stream URLs through the PMS shim

How a Stremio stream (debrid CDN link, AIOStreams proxy, Torrentio RD link, a LAN Stremio
streaming server) reaches LG's decoder through PlxNative's unchanged direct-play path, and what
the shim (`plx_pmsshim`) must do for that to work. Read with `ARCHITECTURE.md`.

## The path

`Part.key` (`/library/parts/<id>/<n>/file.mkv` on `http://127.0.0.1:<port>`) → shim answers
**302 `Location: <stream url>`** → the player follows it:

- plaintext hop chain in `plx_net::stream::redirect::open_following`; the first https hop is
  handed to `curlio` (libcurl), which follows any further hops itself (max 5 total, never
  https → http);
- **credentials never cross origin**: the `X-Plex-Token` query pair and the credential header
  block ride a hop only when scheme+host+port equal the shim's; a third-party host gets exactly
  what its `Location` spells (`stream_redirect.rs` module doc; curlio `next_hop`);
- a seek reopens with `Range: bytes=N-`. Measured on the host (probe over a loopback 302 to an
  https CDN): the curl source re-asks the shim's Part URL on each reopen, so the shim's 302 is
  re-issued per seek — cheap, and correct for short-lived per-request debrid links (the shim may
  re-resolve an expired link there). A server that ignores `Range` (200 instead of 206)
  fails the seek rather than silently restarting at byte 0;
- `Origin` 127.0.0.1/::1 may carry the token in plaintext even in a store build
  (`plex::origin::is_loopback`, this branch). Without it a RELEASE build refuses every shim request.

## TLS (this branch)

Debrid/addon hosts are verified against the TV's own store first. If that fails for a missing
issuer (a 2020 CX firmware lacks newer roots), the request is retried once against
`pkg/public-roots.pem` (Mozilla's list, curl.se extract) — chain, dates and name still verified.
`*.plex.direct` keeps `le-roots.pem`; IP literals and `localhost` never use a bundle. This is the
shared `net::keypin` ladder, so the **control plane (addon JSON via `plx_net::net::request`) gets
the same fallback**.

## Real metadata: `plx_nuvio_probe::probe_url`

The route, the Load payload (`"video":"H265"`, `"audio":"AC3 PLUS"`), the audio-track pick and
the Dolby Vision presentation are all derived from PMS metadata **before** the stream opens
(`engine::start_bufferfeed_inner` reads `route::stream_vcodec/stream_acodec`). A label-guessed
codec that is wrong makes the Load declare the wrong decoder and stall. So:

```rust
let p = plx_nuvio_probe::probe_url(&stream_url, Duration::from_secs(45))?;
```

opens the URL with the bundled libavformat over a private curl transfer and returns container,
duration, size, the video track (codec, WxH, fps, bit depth, profile, `color_trc/primaries/space`,
`dovi {profile, level, bl_compat, rpu, el, bl}`), and every audio/subtitle track in **file order**
(codec, profile, channels, language, title, default, forced). Callable from any thread after
`plx_media_codecs::ffi::boot()` — it is the same single ABI table the demuxer uses, not a second
one. The shim runs it when the player asks `/video/:/transcode/universal/decision`
for an item (or when a stream is picked), caches it per stream URL, and emits the PMS fields from it:

| PMS field | from |
|---|---|
| `Media.container` | `p.container()` (`mkv`/`mp4`/`mpegts`) |
| `Media.videoCodec`, `width`, `height`, `videoFrameRate` | `p.video` |
| `Stream(type 1).codec/bitDepth/colorTrc/colorPrimaries/colorSpace/frameRate` | `p.video` (`colorTrc`: 16 → `smpte2084`, 18 → `arib-std-b67`; primaries/space 9 → `bt2020`/`bt2020nc`) |
| `Stream(type 1).DOVIPresent/DOVIProfile/DOVILevel/DOVIBLCompatID/DOVIRPUPresent/DOVIELPresent/DOVIBLPresent` | `p.video.dovi` (send all, together) |
| `Stream(type 2)` per audio track, in order: `codec`, `channels`, `languageCode`, `title`, `default`/`selected` | `p.audio` (`profile` 60 → `DTS-HD MA`, 30 on eac3/truehd → Atmos `title`/`displayTitle`) |
| `Stream(type 3)` per embedded sub, in order: `codec`, `languageCode`, `title`, `forced`, `default` | `p.subtitles` |
| `Part.size`, `Part.duration`, `Media.duration` | `p.size`, `p.duration_ms` |

Until the probe answers, label guesses are acceptable for the **detail page** only; a play must
not start on guessed fields. The Nuvio route fetches this decision BEFORE queueing or
planning playback, replaces the detail item with the probed item, and uses Original
quality because the shim has no transcoder. Discovery has a 60 s budget, probing 45 s,
and the control-plane decision request has a 120 s deadline for a Plex server — for a Nuvio
shim decision it is **30 s, deadline-only** (no silence watchdog), as below. A probe failure
(`Err`) means "not playable here" (unsupported
container — the bundled FFmpeg demuxes only Matroska/WebM, MP4/MOV, MPEG-TS and raw H.264/HEVC —
or unreachable); say so instead of starting the player.

The probe also **pre-warms** slow sources: a cold torrent takes far longer than the player's 15 s
plaintext header timeout (`stream.rs MEDIA_RECV_TIMEOUT_MS`) and curl's 30 s low-speed floor. The
C engine's `warm` opens the torrent to restore its kept swarm state without preparing a stream,
and `resolve_stream` waits for metadata plus a bounded prebuffer before handing back the URL;
probing at decision time (generous deadline) means the player's own open hits a warmed torrent.

Fork Rofocale11: the Nuvio `/decision` is sent with a **30-second deadline and no silence watchdog**.
The shim may be waiting on the torrent engine to find peers and hand back a URL, during which it
sends no bytes; the ordinary deadline policy layered a 15-second inactivity timer on top of the
absolute deadline and threw a playback error before a cold source could answer. The deadline-only
transport removes that inactivity timer, so a cold engine has the full 30 s to answer. All other
deadline-bearing requests keep the watchdog. The engine's own resolve cap is also 30 s, so a
resolve that uses its whole budget and the player's wait end together.

### Startup prime (Fork Rofocale11)

The first Play follows one of four profiles, fixed at Engine construction and carried through a
reload by the route's `PrimePlan`. The policy table and the rate sampler are pure, host-tested code
in `play-feed/src/prime_profile.rs`; the only upstream player edit is one marked gate term in
`native_executor::try_prime`.

| profile | when | playable floor | pipe must fill | ceiling |
|---|---|---|---|---|
| `Initial` | fresh foreground Load | 3 s video / 2 s audio | ≥ 1.2× realtime | 12 s |
| `Resume` | cold resume at a saved offset | 2 s / 1 s | ≥ 1.2× | 8 s |
| `Seek` | seek, reload, in-place rebase | 0.7 s / 0.3 s | ≥ 1.2× | 3 s |
| `Preview` | hero trailer | one accepted AU | none | none |

The rate is sampled every 250 ms from the demuxer's published media tail (progressive streams
publish it too), as media milliseconds per wall second: 1200 means a 20 % faster-than-realtime
pipe, scaled by playback speed. A playable buffer of ≥ 8 s is proof on its own (the counter goes
blind when the app queue is full), and at the ceiling the floors drop to the seek floors so a
source that never ramps still shows a picture. Both lanes are required unless the demuxer proved
the file audioless. **Where there is no rate controller — segmented HLS, which keeps its own
runway certificate, and rigs with no demux tail — the deeper floors are not applied; the accepted
700/300 ms floors are the start decision**, so a scrub reload cannot stall behind a film's 3 s
gate. A landed seek restarts both the ceiling and the rate baseline at its keyframe. The
`primed: … rate=<n>ms/s profile=<…> -> Play` event-log line records what released the gate;
`-9223372036854775808` means no rate sample (not a fast pipe).

A mid-play stall on a progressive stream is a separate, older mechanism: the clock-debt/dead-link
watch pauses the sink and holds until it has a certificate. Fork Rofocale11 adds a second
certificate to the existing byte-cap and EOF ones: **three seconds of playable video ahead of the
held playhead** (with 300 ms of audio when the stream has an audio lane) resumes the hold. The byte
cap is still checked FIRST, so a remux whose queues only hold ~2 s still resumes at the cap; a
slow source that can stage more now gets short holds instead of one 20 s hold. The resume line
prints `reason=time`, `reason=full` or `reason=eof`. On a link slower than the file this repeats
with duty `1 - delivered/required` — nothing here makes a slow source fast.

### Audio

Initial selection prefers a decodable English dialogue track (`en`, `eng`, including
regional tags) over a foreign-language file default. A default English dialogue
track leads other English dialogue tracks; commentary is excluded from the English
preference. Without English dialogue, the compatible file default is retained.
An explicit user audio choice takes precedence over this initial selection.


<!-- Fork Rofocale11: this section previously said DTS/TrueHD conversion was NOT implemented.
     The upstream merge brought the bundled `dca`/`truehd`/`opus` decoders and the app's PCM
     Load path; this fork now uses them in Auto, so keep this paragraph on the merge checklist. -->
`devcaps` still admits DTS only when the TV's codec table lists it (a 2020 CX does not), and TrueHD
is never natively direct-play — **but in Auto the app now converts DTS, TrueHD and Opus itself**
when the TV has no native path for the track. `src/app/nuvio.rs` marks such tracks `decodable` in
the probe (so the shim's `refusal()` no longer refuses them) and includes them in the shim's
audio-codec list (so stream ranking does not mark them bad). The route then direct-plays the
stream, `route::stream_pcm` declares a stereo 48 kHz S16 PCM Load, and `ff_pcm::PcmDecoder`
downmixes by the frame's channel layout and feeds raw PCM to libpf. A 5.1/7.1 source is mixed
down to stereo; the track menu labels it "played in stereo". A TV that can decode a codec
natively keeps using its own decoder, because the app arm only fires when there is no native
path. A stream that is unprobed, has undecodable video, or has no playable audio at all is still
refused by the shim as before.

### Dolby Vision on the CX

With the probe's `dovi` record in the PMS fields, `Dovi::presentation` behaves as on a Plex server:
P8.x and P5 single-layer **declare** (when configd confirms DV) and direct play; **P7 (UHD BluRay
remux, `el=true`) is refused** ("dual-layer") — the buffer feed carries one elementary stream.
**Decision (lead/playback): the player keeps upstream's rule; the shim does this:** to play a P7
remux as HDR10 instead, the shim must **omit `DOVIELPresent`/all DOVI keys for a
P7 stream whose `bl_compat` is 6 or 1 (HDR10 base)** — the route then direct-plays it as plain
HEVC; the decoder ignores the EL NAL units (layer id > 0) and shows the HDR10 base layer. This is
unverified on the CX; see the TV checklist.

### Subtitles

- Embedded (SRT/ASS/PGS/VOBSUB/mov_text/WebVTT in the container): drawn by the app from the
  demuxer; they only need correct `Stream(type 3)` rows in file order (the probe gives them).
- External (addon subtitles, OpenSubtitles): give each a `Stream(type 3)` row with
  `key: /library/streams/<digits>.srt` (digits only, ≤20, ext alnum ≤10 — `sidecar_key_allowed`)
  and `codec: srt` (or `ass`). The app GETs, in order, `<key>?encoding=utf-8&format=srt`,
  `<key>?encoding=utf-8`, `<key>`, over the control plane, which does **not** follow redirects —
  the shim must fetch the addon URL itself and answer **200 with UTF-8 text** (gunzip, transcode
  cp1252/etc. to UTF-8). SRT and WebVTT both parse; ASS goes to libass.

## What the shim must answer during playback

All on the shim origin with the token; JSON shapes per `tests/mock_pms.py`.

1. `GET /library/metadata/<rk>` — item with `Media[]` (one per stream) / `Part[]` / `Stream[]`.
2. `GET /video/:/transcode/universal/decision?path=/library/metadata/<rk>&mediaIndex=0&partIndex=0&hasMDE=1&directPlay=1&…&audioStreamID=&subtitleStreamID=` —
   answer `MediaContainer{generalDecisionCode:1000, Metadata:[{…, Media:[{…, Part:[{decision:"directplay", …, Stream:[…]}]}]}]}`
   for a stream whose probed video and chosen audio are decodable. For one that would need
   conversion (no decodable audio track, an undecodable video codec, a failed probe) answer
   `generalDecisionCode: 2000` with `transcodeDecisionText: "<why>"` — `route::plan::refusal`
   stops the playback before a byte moves and shows that sentence. Never answer `transcode`: the
   app would then request `start.mkv` from a server that has no transcoder.
3. `GET <Part.key>` (with `X-Plex-Session-Identifier`/`session` query) — **302** to the stream URL
   (or a byte proxy, only for streams with `behaviorHints.proxyHeaders`: curlio cannot add
   per-URL request headers).
4. `PUT /library/parts/<id>?allParts=1&subtitleStreamID=&audioStreamID=` — 200, remember the pick.
5. `GET /:/timeline?ratingKey=&key=&state=playing|paused|stopped&time=&duration=&…` (~10 s) — 200,
   store resume offset / watched (≥90%).
6. `GET /:/scrobble?key=<rk>&identifier=com.plexapp.plugins.library`, `/:/unscrobble` — 200.
7. `POST /playQueues?type=video&uri=…&continuous=1` and `GET /playQueues/<id>` — for Up Next
   (series: return the following episodes).
8. `GET /library/streams/<id>[.ext]` — sidecar subtitle bytes (above).
9. `GET /library/metadata/<rk>/subtitles?language=` (subtitle SEARCH, optional) — `Stream[]` of
   candidates; may return empty.
10. `GET /identity` — `{MediaContainer:{machineIdentifier}}`.
Never reached for direct play: `/video/:/transcode/universal/start.mkv`, `/transcode/sessions/*`,
`/video/:/transcode/universal/stop` (answer 200 if called).

## Verified on the host (2026-10-10, `cargo test -p plx_media --lib live_probe`, host FFmpeg 9.0)

| URL | result |
|---|---|
| jellyfin `4K DV P8.1.mp4` (https) | mp4, hevc 3840x2160 60 fps, 10-bit, trc 16 / pri 9 / spc 9, dovi P8 L9 bl_compat 1, el false |
| jellyfin `1080p DV P5.mp4` (https) | mp4, hevc 1920x1080, trc/pri/spc 2/2/2, dovi P5 bl_compat 0 → 10-bit inferred |
| github.com `test5.mkv` (302 → raw.githubusercontent) | mkv, h264 1024x576 24 fps, 2× aac (default; eng "Commentary"), 8× subrip with languages |
| `http://127.0.0.1:<port>/library/parts/7/1/file.mp4?X-Plex-Token=…` → 302 → jellyfin P8.1 | probed through the hop; the token only ever reached the loopback server |

## Verified on the CX (2026-10-10, debug install)

An AIOStreams 86.4 GB source probed as HEVC 3840×2160, 23.976 fps, DV P8, two audio
tracks and eight embedded subtitles. The native Load declared H265/AC3 with DV P8,
and Starfish reported `hdrType: DolbyVision`. Displayed frames and position advanced.
Pause held the position and resume advanced it. Seeks reopened the CDN range and
returned success. An audio switch loaded AC3 PLUS (EAC3).

The Streams overlay selected a second 4K DV source (HEVC 3832×1600, 23.976 fps,
four audio tracks, three embedded subtitle tracks). That source played and sought.
Selecting an embedded text subtitle returned HTTP 200, followed by nonempty timed
subtitle cues. Native UI screenshots do not include the video plane and are suppressed
while it is bound, so they cannot prove visible subtitle compositing.

The user also confirmed stable Home, 4K Dolby Vision playback, seeking, pause/resume
and visible English subtitles with the physical remote. P7 HDR10 fallback remains
unverified. Fresh follow-up release QR approval reached the local server and loaded
account addons. English audio default behavior is host-tested; physical confirmation
on a newly started stream remains pending.
See `VALIDATION.md`
for the stable install's final checks.

## Current tree (2026-11-24, HEAD `eef501647`)

The contracts above still describe the shipped path. The current 0.10.0 stable build
(Addons page, P2P switch default ON, three hidden Playback rows) was deployed with
`make FLAVOR=stable RELEASE=1 deploy` — `verify-deploy: 50 files match` — and the owner
confirmed it works. On that exact committed tree `make check` reported **146 steps — all ok**;
`plx_plex` 826/826, `plx_screens` 2347/2347 and the app crate 1503/1503. The three
`transcoder.rs` lossy-gate failures mentioned in older validation notes were fixed by the
`make check` alignment pass and no longer occur. Still unverified on device: the P7 HDR10
base-layer fallback, a dedicated sim-probe comparison after the `plx_nuvio_probe` extraction,
and a newly started stream's English audio default.
