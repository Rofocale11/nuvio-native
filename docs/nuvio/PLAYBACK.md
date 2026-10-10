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
- every seek reopens the **effective** URL with `Range: bytes=N-` (the chain is not replayed), so
  short-lived per-request debrid links work. A server that ignores `Range` (200 instead of 206)
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

## Real metadata: `plx_media::ff::probe::probe_url`

The route, the Load payload (`"video":"H265"`, `"audio":"AC3 PLUS"`), the audio-track pick and
the Dolby Vision presentation are all derived from PMS metadata **before** the stream opens
(`engine::start_bufferfeed_inner` reads `route::stream_vcodec/stream_acodec`). A label-guessed
codec that is wrong makes the Load declare the wrong decoder and stall. So:

```rust
let p = plx_media::ff::probe::probe_url(&stream_url, Duration::from_secs(45))?;
```

opens the URL with the bundled libavformat over a private curl transfer and returns container,
duration, size, the video track (codec, WxH, fps, bit depth, profile, `color_trc/primaries/space`,
`dovi {profile, level, bl_compat, rpu, el, bl}`), and every audio/subtitle track in **file order**
(codec, profile, channels, language, title, default, forced). Callable from any thread after
`ff::boot()`. The shim should run it when the player asks `/video/:/transcode/universal/decision`
for an item (or when a stream is picked), cache it per stream URL, and emit the PMS fields from it:

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
not start on guessed fields. A probe failure (`Err`) means "not playable here" (unsupported
container — the bundled FFmpeg demuxes only Matroska/WebM, MP4/MOV, MPEG-TS and raw H.264/HEVC —
or unreachable); say so instead of starting the player.

The probe also **pre-warms** slow sources: a cold torrent on a Stremio streaming server can take
far longer than the player's 15 s plaintext header timeout (`stream.rs MEDIA_RECV_TIMEOUT_MS`)
and curl's 30 s low-speed floor; probing at decision time (generous deadline) means the player's
own open hits a warmed torrent.

### Audio

`devcaps` admits DTS only when the TV's codec table lists it (a 2020 CX does not); TrueHD is never
direct-play. With accurate `Stream(type 2)` rows the route's existing smart-direct-play picks a
compatible AAC/AC3/EAC3 sibling of a DTS/TrueHD default. **When there is no compatible track the
route asks the server to convert it — the shim cannot.** The shim must answer such a decision with
a refusal and the UI says "audio format not supported"; `ff::probe::preferred_audio` is the helper
that tells the shim this ahead of time (rank such streams last / mark them in the source list).
On-device DTS→AC3 was assessed and NOT implemented: the bundled FFmpeg is built
`--disable-everything` with no audio decoders/encoders; it would need `dca` decode + `ac3`
encode added to `ci/build-ffmpeg.sh` and an audio-transcode lane in the buffer-feed pump (AU
timing, seek, A/V sync) — a multi-day change.

### Dolby Vision on the CX

With the probe's `dovi` record in the PMS fields, `Dovi::presentation` behaves as on a Plex server:
P8.x and P5 single-layer **declare** (when configd confirms DV) and direct play; **P7 (UHD BluRay
remux, `el=true`) is refused** ("dual-layer") — the buffer feed carries one elementary stream.
To play a P7 remux as HDR10 instead, the shim may **omit `DOVIELPresent`/all DOVI keys for a
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
