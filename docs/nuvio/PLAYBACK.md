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

## Trailer DASH (`.mpd`)

The trailer shelf is `GET /library/metadata/{rk}/extras`: the shim returns a clip row for
**every** trailer the title lists, each with `subtype: "trailer"`, `extraType: 1` and a playable
`Media[0].Part[0].key`, so `pick_trailer` accepts them and the detail shelf draws them all. Rows
are built **without a YouTube round-trip** (`trailer_shelf_rows`): each clip's DASH Part key is
deterministic (`trailer_rk_at(parent, index) * 1000 + 998`), so `/extras` — which sits on the
detail page's critical path — never spawns the Node child. The resolve happens on the player's
first Part request (`part`), and the shelf's FIRST tile is also warmed in the background as soon
as `/extras` is served: the detail page autoplays after its 2 s dwell, so that thread's work is
normally already in `trailer_dash` by the time the player asks. That warm resolves the title's
**best** trailer, not TMDB's first entry: it scans up to two candidates (stopping at the device's
own ceiling rung, width counted because a widescreen 4K is `3840x1600`) and publishes the tallest
pair, since TMDB routinely lists a 1080p cut before the 4K rendition — the case that pinned
autoplay to 1080p while a 2160p VP9 trailer sat later in the shelf. Each resolve is a paced Node
spawn, so the budget is two, not the whole shelf: every 4K trailer measured on the dev set was the
first or second candidate, and a wider scan only slowed the autoplay. This used to run inside `/extras`
on the request thread, which measured as a 3–5 s detail open against a 1 s baseline because each
resolve also waited on the 300 ms decipher gate. `/library/metadata/{rk}/trailer` still answers
the *resolved* first row. A title whose meta and TMDB `videos` name no YouTube id at all (and
whose IMDb fallback finds none) answers empty `Metadata:[]`, never a 404.

**Where the ids come from.** TMDB `videos` is now part of `append_to_response` on both
`/movie/{id}` and `/tv/{id}`, so a detail open pays no extra request and the shelf has the full
list: trailers before teasers, then the ids Cinemeta names. This is also what gives a **related
show** a trailer shelf: a synthetic `TMDB_SHOW_KEY_BASE` key has no Cinemeta meta to read, and the
show's TMDB `videos` is its only source (`trailer_candidates_from_videos`). Clicking trailer N
plays that row's own Part (`trailer_row_at(parent, n)`), not the first trailer again.

A trailer Part (`/library/parts/<id>/0/file.mpd`) is direct-playable: `part_is_streamable`
accepts `.mpd`, the demuxer fetches the shim's static SegmentBase MPD (200 with or without a
token) and demuxes it with the Rust DASH engine (`media/src/dash.rs` + `ff.rs dash_demux`),
which opens each track's BaseURL as a seekable progressive fMP4 source through the
bundled `mov` demuxer. The bundled FFmpeg has no `dash` demuxer, so `.mpd` never goes through
the progressive open. `ff_probe` still cannot probe `.mpd` directly — it validates a video
track URL, not a manifest — so trailer metadata comes from the manifest/decode path, not the
probe. 1080p H.264 trailers are verified on the dev set (`dash: manifest video=video/mp4
1920x1080`, ten `trailer: dash pair served to its end` lines, no throttle refusal), and so is the
2160p VP9/AV1 rung: a 2160p trailer was played to its end on that set while v0.9.0 was prepared,
which is the observation the ceiling was built for. Any other trailer rung is unclaimed rather
than known broken.

BaseURL origin — **which client is granted the whole track.** The DASH pair is resolved by the
Node child (`trailer-decipher.js`) with YouTube's `VISIONOS` client (`clientVersion 1.02`) plus a
`visitorData` identity read from the `www.youtube.com` page and sent as `X-Goog-Visitor-Id`, under
the page's Safari `User-Agent`. That combination is the only one measured to be granted a trailer's
adaptive tracks to their **end**. Every other anonymous client — IOS 18/19/20/21 and iPad, ANDROID
20.10.38 and 21.03.36, `ANDROID_VR`, MWEB — is granted exactly the **first 60.0 seconds** of the
same tracks and answers `403` past that byte. Measured 2026-11-06 over seven trailer IDs on one
network: the IOS URL served 60.0–60.4 s and then refused from that byte onward (e.g. a 145 s
trailer: `206` through byte 6 398 072, `403` at 6 398 073 — 41.6 %), while the same title under
VISIONOS answered `206` at 99 % of the track, both video and audio. The muxed 360p file (`itag 18`)
is exempt from that limit for every client, which is why it always played. Without `visitorData`
VISIONOS itself answers `LOGIN_REQUIRED`; a real PO token attached to the IOS/ANDROID player request
did not move the 60 s boundary, and neither did changing CDN host, IP family, `range=` or `rn=`.

**Age-restricted trailers.** A red-band trailer answers `LOGIN_REQUIRED` ("Sign in to confirm your
age") to *every* anonymous client — VISIONOS, IOS, ANDROID, TVHTML5, MWEB, WEB and the embedded
player were all measured, and `yt-dlp` fails the same way without cookies. No client identity fixes
it, so the child reports `login_required` as its own reason and `NodeTrailerResolver` remembers that
id for six hours (`BLOCK_TTL`) instead of re-spawning. A blocked id is filtered out of
`playable_candidates`, so it never becomes a shelf tile and no Node spawn is spent re-learning a
permanent refusal; the title's other trailers/teasers are the tiles that remain. Measured on
*Nightcrawler*: `7uaYhPpV7G4` (Red Band) is refused, and the shelf offers the 720p teaser instead
of a dead Trailer button.
The child asks each chosen track for its last 64 KiB before returning, retries the resolve once with
a fresh visitor when either refuses, and reports the pair only when both answer.

The manifest carries those signed googlevideo URLs directly — no proxy hop, and no user-agent
override anywhere in `net`/`curlio`. What the edge refuses is the *shape* of the request — a GET
with no `Range` header, and an open-ended `bytes=N-`, both answer `403` (or a `302` with no
`Location`), while a bounded `bytes=a-b` answers `206`. `curlio` therefore states an 8 MiB window
(`GOOGLEVIDEO_WINDOW`) for `*.googlevideo.com` media URLs — including the first open at offset 0,
which used to send no `Range` at all — and continues it at the next byte when the window ends
(`continue_window`), so a window boundary is not EOF. Every other host keeps the old shape: no
`Range` at offset 0, `bytes=N-` for a seek.

The shim probes the direct video URL (the address the player will open) and asks each track for
its **last 64 KiB** (`Shim::trailer_tracks_reachable`, through the `Reacher` installed at boot).
The probe and the tail check run together on separate threads, and the two tracks' head→tail walks
run together, so the gate waits for its slowest ask instead of the sum of them. Both are hard
gates: a limited grant passes the
head probe and then fails mid-playback, so the tail ask is what catches it. The video-track probe admits `mp4`/`mov` and the Matroska family (`mkv`, as `ff_probe` names a `matroska,webm` stream) beside `h264`/`vp9`/`av1`, so a 4K VP9 pair is not mistaken for an unknown container. A refused probe *or* tail does not publish the DASH `Media` at all — the
row falls back to the verified muxed row, so playback still plays 360p instead of erroring. That
refusal now happens at the Part request too: the shelf advertises the trailer, and a candidate that
cannot be served simply leaves the MPD route 404 rather than hiding the control on the detail page.
`js-service/trailer-proxy.js` remains in the tree but carries no track bytes. The video track is
bounded by **the television's own codec table** (`devcaps`): the shim passes the child
`PLX_TRAILER_CODECS` (preference order, `h264` always first, then `vp9`, `av1` only when the device
table lists the row) and `PLX_TRAILER_MAXH` (that row's ceiling, at most 2160). A set with no VP9
or AV1 row therefore never receives a 2160p track it cannot decode, and one with the row gets the
best level YouTube offers — measured on the dev set (`k8lpn`, webOS TV Lite 10): `vp9=true
4096x2176@60`, `av1=true 4096x2176@60`, so its trailers resolve to 2160p VP9 (or AV1) and the
manifest declares `container=webm`, `videoCodec=vp9`, `videoResolution=4k`. The audio track is
chosen by `audioTrack.audioIsDefault`
— YouTube returns one m4a/AAC track per **auto-dub** (German, Spanish, Hindi, …) with the same
itag and bitrate beside the original, and taking the first match gave every trailer a dubbed
soundtrack; the original is the entry marked `audioIsDefault: true` and never one flagged
`isAutoDubbed`.

## TLS (this branch)

Debrid/addon hosts are verified against the TV's own store first. If that fails for a missing
issuer (a 2020 CX firmware lacks newer roots), the request is retried once against
`pkg/public-roots.pem` (Mozilla's list, curl.se extract) — chain, dates and name still verified.
`*.plex.direct` keeps `le-roots.pem`; IP literals and `localhost` never use a bundle. This is the
shared `net::keypin` ladder, so the **control plane (addon JSON via `plx_net::net::request`) gets
the same fallback**.

## Seeks: what a seek costs here

Every direct-play seek under the shim is a **full reload**, not an in-place re-anchor.
`player::INPLACE_SEEK_OK` is cleared whenever the Starfish object layout is not the one this
fork was decompiled against, which is every webOS 5+ firmware (`engine::start_bufferfeed` →
`inplace_layout_known`; the in-place path reaches LG-private C++ offsets derived from a 4.5
binary). So a seek = teardown, fresh `Load`, fresh Starfish object, new `Range: bytes=N-`
transfer. Two things are tuned for that reality:

- **The reopen budget is 3 s** (`SEEK_STUCK_MS`, `player/pump.rs`). The watchdog's job is to
  catch a *lost* reopen, and it does that by re-interrupting the demuxer — which closes the
  socket the reopen is blocked on. Over a LAN PMS that reopen takes ~700 ms, so 1.2 s was
  generous. Through the shim it is: `av_seek_frame` → `seek_cb` → re-ask the shim's Part URL
  (302 re-issued, a debrid link possibly re-resolved) → a fresh TLS transfer with `Range:`. Two
  public-internet round trips. A watchdog tuned for one LAN read fires mid-flight and restarts
  a transfer that was about to land, which is exactly the failure it exists to avoid.
- **The spinner waits 250 ms** (`SEEK_SPINNER_GRACE_MS`, `player/mod.rs`). A reload that lands
  inside the grace paints nothing at all — the transport stays up, the playhead is already
  frozen at the target, and the picture returns. This is a *drawing* grace only:
  `player::state()` still reports `Seeking` on the frame the seek is requested, because the
  playhead freeze, `is_playing`, `intended_pos_ns` and the rebuffer accounting all key on that
  state and none of them should lag. The gate lives in `player_hud::busy()`, the one impure
  sampler, so `busy_surface` keeps its pure rule.

A seek that never lands still ends: two retries, then a full reload, then the failure read-out.
The added latency (1.2 s → 3 s per attempt) is paid only by a seek that was already broken.

## A body that ends early is a cut, not the end of the film

A direct-play source is one HTTP body per connection, and **any** party can end it early: a debrid
CDN that closes a range response, the P2P server while its engine is still finding peers, a
Matroska element cut in half by the last byte received. libavformat has no way to tell that from a
finished file — an AVIO `0` is EOF, and a parser error at a truncated element stops the demuxer
just as quietly. The measured shape on the set was `ff: demux ended` after 2337 s of a 6443 s film,
`timeline stopped … ok=1`, and a manual retry that played through from the same second.

So the demux asks its own facts before it believes an end:

| fact | from | meaning |
|---|---|---|
| `off < size` | `AvioState` (`off`, and `Content-Length` / `Content-Range` total at the open) | fewer bytes arrived than the resource declared |
| `hls_video_tail_ns + 15 s < duration_ns` | the last video PTS this demux fed, the container's duration | the demux is short of the film, not merely of its last byte; the 15 s lets a finish whose tail carries no video (credits) still read as a finish |

Both, and the read is a cut. The recovery has two arms, and both end in **the same reopen a manual
retry performs**: `seek_cb(at)` with `Range: bytes=<at>-` through `curlio` (TLS) or the plaintext
socket. `curlio` refuses an answer served from byte 0, so a source that cannot honour the range is
a failure rather than silently spliced corruption.

- **Inside `read_cb`**, when the transport itself reports an end or a failure: a clean `0` before
  the declared length reopens at the byte reached, and a failed transfer whose curl rc is a cut
  (18/55/56/92) does the same.
- **In the demux loop**, when `av_read_frame` returns for any reason while the two facts above
  hold: `avio_seek` clears libavformat's `eof_reached`, the next read reopens and playback
  continues in place. This is the arm that covers a *parser* error at a truncated element, which
  never reaches `read_cb` at all.

Both arms are bounded by `BODY_HEAL_ATTEMPTS` **5** reopens at one byte, spent from one budget
(`AvioState::heal`, refilled by any read that gets through), and the wait before each is
`HealBudget::wait_ms`: immediate, then 250/500/1000/2000 ms. The immediacy matters for a mid-film
cut; the backoff is what a **cold** source needs, because the retry that succeeds is often seconds
later — the very GET that stopped is what created the P2P engine, and the swarm cannot serve until
the engine has metadata. ~3.75 s of patience sits behind the buffering spinner, not a spinning
demux thread, and every slice of it re-checks the abort flag so teardown is never held by it.

A budget that runs out is **not** a silent end: the demux logs
`ff: body stopped short at <off> of <size> bytes with the picture at <t>s of <dur>s; no reopen
crossed it — transport failure, not EOF` and raises `demux_io_failed`, so the failure read-out
offers a retry instead of the player sitting at a frozen second.

## Memory: what playback is allowed to hold

The set is a 32-bit ARM box and `pkg/appinfo.json` declares `requiredMemory: 280` MB. Peak RSS — not
average — is what dies during playback, and the queues are only ever spent when the link outruns
the decoder, so they are the first thing to size honestly:

| term | where | now | note |
|---|---|---|---|
| video AU queue | `player::engine::AQ_VIDEO_BYTES` | **50 MiB** | ~4.4 s at 90 Mbit/s, ~10 s at 40 — past the 1.6 s feed lead |
| audio AU queue | `AQ_AUDIO_BYTES` | **4 MiB** | same ratio, so a 3.6 Mbit/s DTS-HD MA track never fills first |
| pre-audio video grant | `ff::PRE_AUDIO_VIDEO_HEADROOM` | 24 MiB | only for a file whose audio starts late; walked back as it drains |
| audio-ahead grant | `ff::AUDIO_AHEAD_HEADROOM` | 16 MiB | only for a file that stores more audio than video ahead of it |
| poster pool | `ui::tex::TEX_RESIDENT_BYTES_MAX` | **32 MiB** | the measured zero-refusal point for one Home session; 24 MiB refused 171 arrivals (blank tiles) in a longer simulator session, 16 MiB re-armed constantly. The rest re-upload from the disk cache |
| image-subtitle store | `player::SUB_BITMAP_BUDGET` | 40 MiB | **not** a cache: this IS the delayed-caption window, measured at 31.1 MB worst case for 4K PGS |
| torrent engine RAM | `sbengine::DEFAULT_MEM_CACHE_BYTES` | **8 MiB** | verified pieces the in-process engine keeps for re-reads; it is a ceiling, and it saturates (`sbengine: … mem=` in the event log). A duplicate of bytes the window file already holds, so it is small on purpose |
| torrent engine window | `sbengine::DEFAULT_WINDOW_BYTES` | **32 MiB in RAM** | payload around the playhead, trimmed in whole pieces, a file smaller than the window kept whole, and removed with the torrent. The byte cap is a target, not a floor: head, tail, two pieces behind the playhead and the forward schedule are each whole pieces, so a 16 MiB-piece season pack settles near 80–128 MB however the cap is set. **Not "storage" on the set:** the runtime root is a RAM-backed tmpfs, the window is read with `pread`, so it is shmem — invisible in `VmRSS`, paid for out of the TV's 1.2 GB all the same |
| Starfish source buffers | `PAYLOAD_AV.bufferingCtrInfo` | 8 MiB video / 1 MiB audio | the pipeline's own, not ours |

The two queues were 96/8 MiB until 2026-11-01 (an LG B4 sizing, 2026-09-22); on this set that peak
ran the process out of memory mid-playback. That is ~50 MB of queue, which is where the peak was;
those queues stay at 50/4. The poster pool was cut 32→24 MiB with them, then put back to 32 on
2026-11-02: the 24 MiB pool could not hold one Home (the hero, its prefetched neighbours, and the
visible grid) and refused 171 arrivals in a single simulator session, each a tile rendered blank
until its cooldown cleared. The 8 MiB the pool takes back is still far inside the ~50 MB the queues
gave up. `abr/sim.rs` carries the queue numbers by value and a test fails if the two models
disagree.

The engine's two rows above were 40 MiB window / 32 MiB RAM cache until 2026-11-05, and they were
sized as if the window were storage. Measured on the set while a film played: `MemAvailable`
**78 MB**, `/tmp` a 611 MB tmpfs holding **148.9 MB**, of which 48.8 MB was the engine's cache
directory — a window that never showed in `VmRSS` and was invisible to every prior sizing
decision. The set is also not the only thing in `/tmp`: four leftover
`.hbchannel-incoming-*.ipk` uploads from Homebrew Channel installs were holding **93.8 MB** of
it, and removing them took `MemFree` from 57.8 MB to **148.7 MB** and `MemAvailable` from 78.1 MB
to **169.0 MB**. That is the first thing to check when the set looks short of memory — the
package installer never cleans up after itself.

The window is evicted in whole pieces, and that has a network consequence. The behind cushion was
a flat 4 MiB, smaller than one 16 MiB piece of a 1080p season pack: the piece the player had just
left was punched the moment the read head moved 4 MiB past it, so the next re-read of it (a
container re-sync, a heal, a small step back) downloaded that whole piece from the swarm again.
Measured on the set with True Detective S01E1: the engine pulled 4.9–8.8 MB/s for minutes while
the player's queue sat full at 81.5 s and it consumed ~0.5 MB/s, `got` reached 487 MB against
122 MB delivered, and the refetch counter climbed from 6 to 27 — essentially every freed piece
was re-downloaded. The cushion is now at least two pieces (`trim_route`), so a re-read up to two
pieces behind the playhead is served from RAM. The forward schedule and the head/tail keep the
window above its byte cap for large pieces, which is intentional: the cap bounds small-piece
torrents, and the Diagnostics Window row reports the cap while Cache resident reports what the
whole-piece floor actually holds (verified pieces only — in-flight partials no longer count, so
a fast download can no longer inflate it past the cap). `nuvio_engine_streaming_window_resync_test` pins the cushion
with 16 MiB pieces. The same set then played a 162-minute film with the fixed engine: `got`
stopped at 126 MB once the window plus the player's 46 MB queue were full, `down` fell from
5.8 MB/s to 3 KB/s while the picture kept playing at 48 fps, and `refetch` stayed at 1 — the
transfer line is now `sbengine: peers seeds down up got delivered window resident scheduled
demands at piece mem refetch unsupported`, so "downloading for the player" (`delivered`) can be
told from "downloading for itself" (the difference, with `scheduled` naming how many pieces the
schedule still asks for, `at` the file offsets of the live reads, and `piece` the eviction
granularity). The fallback schedule (used when no read is missing data) follows the
**most recently served** demand that is missing data — never the newest request, and never a
re-read that is already resident — so a probe or an abandoned seek cannot keep the download
pointed at an offset the window then evicts and refetches. Every event-log record also carries a
UTC timestamp (`2026-11-09T12:34:56.789Z line`), written by the one sink both log exits share.

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
`ff::boot()`. The shim runs it when the player asks `/video/:/transcode/universal/decision`
for an item (or when a stream is picked), caches the answer **per content** — a torrent's info
hash + file index, every other stream its host+path (`shim_probe::content_key`) — and emits the
PMS fields from it:

| PMS field | from |
|---|---|
| `Media.container` | `p.container()` (`mkv`/`mp4`/`mpegts`) |
| `Media.videoCodec`, `width`, `height`, `videoFrameRate` | `p.video` |
| `Stream(type 1).codec/bitDepth/colorTrc/colorPrimaries/colorSpace/frameRate` | `p.video` (`colorTrc`: 16 → `smpte2084`, 18 → `arib-std-b67`; primaries/space 9 → `bt2020`/`bt2020nc`) |
| `Stream(type 1).DOVIPresent/DOVIProfile/DOVILevel/DOVIBLCompatID/DOVIRPUPresent/DOVIELPresent/DOVIBLPresent` | `p.video.dovi` (send all, together) |
| `Stream(type 2)` per audio track, in order: `codec`, `channels`, `samplingRate`, `languageCode`, `title`, `default`/`selected` | `p.audio` (`profile` 60 → `DTS-HD MA`, 30 on eac3/truehd → Atmos `title`/`displayTitle`) |
| `Stream(type 3)` per embedded sub, in order: `codec`, `languageCode`, `title`, `forced`, `default` | `p.subtitles` |
| `Part.size`, `Part.duration`, `Media.duration` | `p.size`, `p.duration_ms` |

The cache is split by what the key means (`ProbeCache`):

- **Content keys** (`torrent:<hash>/<fileIdx>`) cannot go stale — an infohash fixes the bytes — so
  they never expire, and they persist to `probe-cache.json` in the P2P state directory
  (`p2p_state_candidates()[0]`, beside the engine's resume files). Probing a 4K remux costs 10 s
  because the container header **and the tail cues** have to come back from the swarm, and the
  payload is tmpfs, so before this a relaunch re-paid it. Relaunching the app now reuses the
  verdict.
- **URL keys** keep `PROBE_TTL` (30 min): a signed debrid link can be re-pointed and a 403 is
  transient, so those verdicts are trusted only briefly and are never written to disk.
- Either way the map holds at most `MAX_ENTRIES` (512, oldest dropped first, ~2 MB), and a failure
  is never cached under a content key: the same file becomes readable the moment the swarm
  delivers its header.

Until the probe answers, label guesses are acceptable for the **detail page** only; a play must
not start on guessed fields. The Nuvio route fetches this decision BEFORE queueing or
planning playback, replaces the detail item with the probed item, and uses Original
quality because the shim has no transcoder. Discovery has a 60 s budget, probing 45 s,
and the control-plane decision request has a 120 s deadline — the caller's deadline is the ONLY
bound on that request (`request_until_outcome_deadline_only`): the media inactivity budget
(`MEDIA_RECV_TIMEOUT_MS`, 15 s) must not cap it, because a cold torrent legitimately stays silent
past that while discovery and the probe run (capping it there made the first play of a cold P2P
source fail and the warmed retry play). A probe failure (`Err`) means "not playable here" (unsupported
container — the bundled FFmpeg demuxes only Matroska/WebM, MP4/MOV, MPEG-TS and raw H.264/HEVC —
or unreachable); say so instead of starting the player.

The probe also **pre-warms** slow sources: a cold torrent on the P2P engine (the in-process
`libsbengine.so`, or the Node child when it cannot load; see `ARCHITECTURE.md` "The P2P streaming
server") can take far longer than the steady-state media inactivity timeout
(`stream.rs MEDIA_RECV_TIMEOUT_MS`, 15 s) and curl's 30 s low-speed floor; probing at decision
time (generous deadline) means the player's own open hits a warmed torrent.

**Resume warms the swarm on intent.** The resume file and the durable probe verdict make a
resume's metadata and track table free (measured: metadata 100–309 ms, resolve 200–410 ms), and a
replay inside one process skips the probe entirely. What is left is that a torrent closed for a
day has no open connections: the first telemetry tick after a resume measured 75–714 KB/s with
0–4 MB across the test set, reaching 1.4–8 MB/s one tick later. So opening the title's detail page
— or, under the Play setting, resting one second on its Continue Watching card — asks the shim to
open the remembered torrent (`/nuvio/warm`): info hash only, no stream, no file priorities, no
payload, so the engine restores the kept swarm and dials its peers before the press. A debrid or
direct link has no swarm and the warm is a no-op for it.

While such a torrent plays, the diagnostics read-out's `DELIVERY / CONTROL` column shows the live
swarm instead of the Plex adaptive-model rows (which the shim never runs): the file name with its
size on the row beneath, connected/connecting/known peers, seeds, down/up rate, the streaming
window, resident/freed cache, pieces re-fetched after eviction, and the set's RAM as `used/total`
MB (`/proc/meminfo`'s `MemTotal` minus `MemAvailable`, resampled with the panel). A direct or debrid
link — no `Media.p2p` — has no swarm and no adaptive session either, so it shows the compact
file/size/rate/quality block rather than nine "model idle" placeholders; the chart takes the rows a
block does not spend. The swap is keyed on the decision's `Media.p2p` marker.

**A Continue Watching card under Play starts from the row itself.** The shim's hub rows carry no
`Media` — a row is the list projection, not the item — so the card has no Part key. The press
plays it anyway: `request_play_movie` admits an empty Part on the shim, whose `/decision` names
the file from the item's ranked streams (the same door the hero picker's `PlayIntent::Item` takes
with an empty part), and refuses it on any other server, where a row with no Part names no file.
Refusing it there too left the press queuing nothing at all — a dead tile with no error.

That play fetches its addons at the press rather than on a page. The play-start poll keeps asking
`/nuvio/streams` until the shim reports the ranking complete — not until the first non-empty
answer, which is usually one addon's partial list — so the player's Sources list fills in as the
addons reply. The ranked subtitle sidecars ride the same answer and are appended to the playing
item's track list as they land (the same ids, `/library/streams/{id}` keys and pick behaviour a
detail-page play carries), instead of being frozen at whatever `/decision` had when it landed.
A late sidecar the answer marks `selected` also turns **on** when nothing is showing yet — direct
play, or a plain remux the app may draw over, never a burn — which is what lets a **Resume that
skips the detail page pick the same subtitle a detail-page play does**: that play's plan snapshot
was taken before the subtitle addons answered, so this drain
(`playback::drain_ranked_subs`) is the frame that restores the pick, logged as
`late server-selected sidecar <id> restored on resume`.
The answer's `subtitlesComplete` flag is separate from the streams' `complete` on purpose: subtitle
addons routinely answer after the last stream (the streams unblock the play, the subtitles do not),
so the poll keeps going until both are done.

Cold starts that skip the probe are covered by a **first-byte grace**. A plaintext media open
arms `MEDIA_FIRST_BYTE_TIMEOUT_MS` (45 s) on its socket for the FIRST body byte only; the first
successful read restores `MEDIA_RECV_TIMEOUT_MS` (15 s) for the rest of the transfer. It is
armed by `redirect::open_following` (the media path), never by `http_open`/`http_open_until_result`
(the control plane), and a caller's absolute deadline still outranks it. The header read keeps
its 15 s, which suffices because `server.js` flushes the response head as soon as a range request
is accepted instead of holding it until the first piece; the end-to-end shape of that handoff is
in `ARCHITECTURE.md` "The P2P streaming server".

### Audio

Initial selection prefers a decodable English dialogue track (`en`, `eng`, including
regional tags) over a foreign-language file default. A default English dialogue
track leads other English dialogue tracks; commentary is excluded from the English
preference. Without English dialogue, the compatible file default is retained.
An explicit user audio choice takes precedence over this initial selection.


`devcaps` decides a track's lane from the TV's own codec table (`Caps::enc_lane_candidate`):

| track | lane |
|---|---|
| FLAC / ALAC / native PCM inside the PCM lane's counts (mono, stereo, 6-ch) | PCM lane, lossless (`ff_pcm.rs`) |
| FLAC / ALAC / PCM 3–8 ch the PCM lane does not admit | **AC3 encode lane** (`ff_enc.rs`) |
| TrueHD / MLP, any channel count | **AC3 encode lane** |
| DTS, any channel count — **whether or not the table has a `dts` row** | **AC3 encode lane** |
| Opus above 2 channels (Opus is 48 kHz by definition) | **AC3 encode lane** |
| MP3 / Vorbis — only with `lossy=on` (off by default) | PCM lane, stereo S16 (`ff_pcm.rs`) |

ALAC rides the lossless PCM lane: the bundled FFmpeg decodes it with the same
`avcodec_send_packet`/`receive_frame` path as FLAC, and it is admitted only where a FLAC row
exists, with that row's channel ceiling (`devcaps`). `flac=off` turns both off together, because
they are one lane and one knob.

**The lossless PCM lane resamples the stereo it cannot declare at the source rate.** libpf's PCM
table names no rate above 48 kHz on any image from 2018 to 2025, so a stereo FLAC/ALAC at 96,
88.2, 192 or 176.4 kHz is decimated in-app by a Kaiser-windowed sinc (`ff_pcm::Downsampler`,
integer ratios only) and the Load declares 48 kHz (44.1 kHz for the 44.1 family). A ratio that is
not whole keeps the source rate, which the sink may refuse — the route then leaves the track with
the server. A surround hi-res track is untouched here: the lane never remixes, so its rate stays
as authored and the encode lane or the server decides its fate.

**MP3 and Vorbis are app-decoded only with `lossy=on`.** This set's own table names MPEG and
Vorbis, so its pipeline plays them compressed and the gate is off by default; a set whose table
lacks a row (or whose pipeline refuses the track) turns the knob on and the app decodes it,
downmixes whatever layout arrives to stereo (`speed_audio::downmix`) and feeds S16 at the source
rate (decimated above 48 kHz like the lossless lane). The knob also announces `mp3`/`vorbis` in
the direct-play profile and states an 8-channel bound, because the lane downmixes locally.

**Native Opus is bounded by what this firmware is known to take.** Opus is the one codec fed
straight to the pipeline here (FLAC/PCM and, with `lossy=on`, MP3/Vorbis are app-decoded), and its
Load string is `OPUS`. What the set announces is one monotone fold, logged at boot as
`devcaps: ... audio layers:`: its codec table, minus the codecs `/etc/gst/gstcool.conf` ranks
every decoder of at off (`apply_gst_decoders`; a codec the file never names keeps its default
rank), minus `devcaps::EXCLUSIONS` — measured rows per firmware codename, each carrying its
evidence (the C1's `kisscurl-koli` limits Opus to stereo; wider tracks stay with the server), minus
the refusals this install learned. Auto's own stereo ceiling for Opus
(`OPUS_AUTO_MAX_CHANNELS`) holds whatever a table row claims **only while the AC3 encode lane has
its sink** (a measured AC3 row of six channels), and a measured row is honoured by Force Direct
Play too (`Caps::forced_feeds_audio_track`).

**A surround Opus track is converted in-app, not direct-played.** Opus above two channels takes
the AC3 encode lane: the bundled FFmpeg decodes it (Opus is 48 kHz by definition, so the lane's
no-resampler rule costs nothing here), the lane folds it to 5.1 and feeds AC3 640 kbps, so the
television's hardware sees a format it already decodes. That is the answer to the failure this
fork measured on an LG B4 (webOS 10.3.1, `ponytail-papikonda`, 2026-11-08): the native decoder
takes the Load and a few access units and then presents nothing, and this product has no server
to convert for it. Stereo Opus stays native — it is the one width measured clean. The ceiling
above is gated on the lane for the same reason: with no lane and no converter, taking the width
away turns a silent track into a failed route (the plan stops direct-playing, asks for an audio
conversion, and the shim answers `start.mkv` with an empty body, which demuxes to no access unit).

**A refusal is learned, and the next play converts.** A native Load that ends in
`smp_cb type=18 num=200` (`Audio Codec Not Supported`) before any picture, or a surround Opus
feed that stalls silently (`player::native_stall`: more than two channels, Load returned, both AU
queues holding data, no AU accepted for 4 s, no picture), is taken off the capability answer at
once and recorded in the install-scope `native_audio_refused` preference keyed by the firmware
BUILD (`tv::device::Info::build`). An LG update is a new decoder, so another build's rows stop
counting and the new build gets a fresh native try. A surround refusal limits the codec to stereo,
a stereo refusal removes it. The failed open then recovers in the same frame through
`open_failure_action` → `route::dispatch_native_audio_conversion`, which rebuilds today's route as
the plain Original remux (video copied, only the refused audio converted) and reloads onto it;
the viewer keeps the spinner, and a conversion the server refuses ends in the ordinary read-out.
`nativeaudiolearn`/`nativeaudioforget`/`nativeaudiowide` are the debug switches.

**The shim does not convert.** `/video/:/transcode/universal/start*` answers 404 ("no transcoder:
streams play direct") and `/decision` always directplays, so `session::server_converts_audio` is
false with the in-process shim installed: a learned refusal still persists, but the recovery adds
no action of its own (the ordinary failure ladder decides) and the route is left alone — the shim
is never asked for a route it cannot serve.

The encode lane is real and shipped: the bundled FFmpeg carries `dca`/`truehd`/`mlp`/`flac`/`opus`
decoders and the `ac3_fixed` encoder (`ci/build-ffmpeg.sh`), the demux thread decodes the track,
folds it to 5.1(side) by channel role in Q15 integer arithmetic, and encodes AC3 640 kbps at
48 kHz. The decoded frame is read in whatever sample format the decoder emits — S16/S32 for FLAC,
TrueHD and DTS, **float (FLTP) for Opus, whose decoder has no integer path** — and float is scaled
to full-scale i32 on the way in (`ff_enc::interleave`). Reading only the integer formats is what
made every Opus frame "unsupported" and left the pipeline an AC3 track with no frames: video
presented, audio clock dead, stall (owner's B4, 2026-11-08). The Load then declares it exactly as a
native AC3 5.1 track (`AudioLaneInfo::Ac3Encode`),
so the television's hardware sees a format it already decodes. The lane does no resampling of its
own, so it takes 48 kHz sources only; the route leaves any other rate with the server. (The
build ships libswresample because FFmpeg's `opus` decoder depends on it and configure drops the
decoder silently without it; nothing in the lane calls into it. The
lossless PCM lane is the exception to the rate rule: it decimates stereo hi-res itself, see
above.) A rate the Media does not state takes the lane anyway and the lane's own open
refuses it if the stream reports anything else — the shim publishes the probed `samplingRate`
(`shim_probe::Audio::sample_rate`), and before that every track arrived with the rate unstated,
which refused the TrueHD Load outright (2026-11-01).

**DTS is deliberately the one unconditional arm.** The two sets this fork runs on have opposite
DTS problems and the lane is the only answer both accept:

- a set *with* a `dts` row plays the core only — `ff::dts_core_packet` drops the DTS-HD
  extension, so a 7.1 MA arrives as 5.1 core at 1.5 Mbps, and anything above the row's channel
  ceiling is refused outright. **The dev set is this case, and on it a native DTS Load does not
  play** (measured 2026-11-01): the pipeline accepts it, reports `smp_cb type=8 audio/x-dts`, then
  never emits the `type=7` out-channels callback, no `FRAMEREADY` tick reaches the app, and with
  `audioSync` the video sink slaves to that dead audio clock — black screen, audio queue filling
  to ~2 s and stalling. That is why DTS is unconditional rather than table-gated;
- a set *without* one (a 2020 CX) has no DTS decoder at all, so the same core reaches the
  pipeline as **silence**.

Folding to AC3 gives both the same 5.1 output, decoded from the full lossless stream (`dca`
decodes the extension, so nothing is discarded). The log says which path ran:
`ff: DTS -> AC3 encode lane (DTS-HD extensions decoded, not discarded)` when the lane is on,
`ff: DTS core extraction on (DTS-HD extensions discarded)` only when it is off.

Consequences the shim must respect:

- **A DTS-only stream is now playable on a set with no DTS row**, provided the table has a
  measured AC3 row of ≥6 channels (the lane's sink, never assumed). "DTS with no compatible
  sibling" is no longer a refusal.
- The direct-play profile states the lane's codecs itself — `enc_lane_profile_codecs` returns
  `truehd,mlp,dts`, appended to `audioCodec=` (deduped against the table, so `dts` is not listed
  twice) — and `profile_channel_limit_with` states 8 channels for them. DTS is exempt from the
  lane's `audio.samplingRate=48000` limitation: a rate the lane refuses falls back to core
  extraction, which plays at any rate.
- `preferred_audio`'s `decodable` predicate must be the lane-aware rule, not `audio_has`:
  a DTS default is decodable when the AC3 5.1 sink exists.
- `forkknobs`' `encode=off` turns the whole lane off, restoring server conversion and core
  extraction. The boot line `devcaps: fork lanes …` states the lanes and the encode row in force.

With accurate `Stream(type 2)` rows the route's smart-direct-play still prefers a compatible
AAC/AC3/EAC3 sibling when one exists. **When there is no track any lane takes, the route asks
the server to convert it — the shim cannot.** The shim must answer such a decision with a
refusal and the UI says "audio format not supported"; `ff::probe::preferred_audio` is the helper
that tells the shim this ahead of time (rank such streams last / mark them in the source list).

### Playback speed

The player's `…` menu carries a **Playback speed** row (0.5×, 0.75×, Normal, 1.25×, 1.5×, 1.75×,
2×) when the current playback can actually offer a speed. LG's own `SetPlayRate` cannot drive this
pipeline — the buffer-feed source's next SEGMENT resets it to 1× and libpf mutes audio at any other
rate — so the speed is done the way a browser does it:

- every timestamp sent to the pipeline is divided by the speed (`player::to_sink_ns`) and every
  presented one multiplied back (`from_sink_ns`), so the UI stays in film time;
- the sound is decoded in the app (`ff::SpeedDecoder`), time-stretched pitch-correctly by
  `speed_audio`'s WSOLA and fed as 48 kHz stereo PCM, because the compressed track cannot be
  pitched by the TV;
- a speed change is a seek to the viewer's own position marked `rate_reload`; at 1× a direct-play
  seek stays in place, off 1× every mode reloads (`pump::seek_must_reload`).

`player::play_rate_permille` is the viewer's choice and persists across reloads;
`applied_rate_permille` is what the current pipeline is playing and is reset with the session.
`speed_offer` hides the row where a speed cannot be honoured: previews, segmented HLS, a source
whose audio has no decoder in the bundled FFmpeg, and a source that at this speed would feed more
frames per second than the Load envelope (60.5) or the device's measured decoder row allows (24p
reaches 2×; 50p/60p offer nothing above Normal).

### Dolby Vision on the CX

With the probe's `dovi` record in the PMS fields, `Dovi::presentation` behaves as on a Plex server:
P8.x and P5 single-layer **declare** (when configd confirms DV) and direct play. A **P7 (UHD BluRay
remux, `el=true`) whose base layer is cross-compatible (`bl_compat != 0`) also direct plays**: the
feed path (`ff::packet_to_annexb`, the `to_annexb_p7` conversion) drops the enhancement-layer NALs
and rewrites each RPU to Profile 8.1, and the Load declares that single-layer 8.1 — the HDR10 base
plus the dynamic metadata is what reaches the decoder. A P7 whose base layer is **not**
cross-compatible (`bl_compat == 0`) is still refused ("dual-layer"): the buffer feed carries one
elementary stream and there is no displayable base to fall back to. Details and evidence:
`docs/dolby-vision.md` §1.

**Settings > Playback > Dolby Vision is the second input, one pick per profile** (`Auto` by
default; `Off`). It gates the *presentation* only, where a route is planned
(`route::frozen_dv_decision`): `Off` plans every route as an unsupported set, so no `DolbyHdrInfo`
node is sent, a P8/P7 file plays its HDR10 base layer, and a P5 — which has no base layer to fall
back to — is refused direct play and answered by the shim's `/decision` with code `2000`, so it
does not play at all (the shim has no transcoder to convert it, and the copy must not promise
one). The frozen `DvDecision` keeps the probe's own `capability`, so Diagnostics' Video row still
reads `base-layer (supported)` on a set that reports support, and only a failed probe or a set
without Dolby Vision reads `unknown`/`unsupported`. The boot-latched `dvcaps0`/`dvcaps1` overrides
change capability only; they do not bypass a profile's `Off`.

### Subtitles

- Look: the viewer's Settings > Playback picks (and the player's Subtitles → Style page) set a text
  caption's size and vertical position for the profile. The shipped defaults are **Large** and
  **Auto**: at Auto the block's bottom ends 12 px ABOVE the decoded picture's own bottom edge
  (`text_style::AUTO_GAP`), inside the image rather than in the black bar under it, so a 2.39:1
  film keeps its captions just clear of the picture edge; a picture that fills the frame is exactly
  Low. The fixed rungs: Low sits 60 px above the frame's bottom (`text_style::LOW_GAP`), Middle
  lifts 60 px from the historical baseline, High 160 px. The picture edge comes from the published
  geometry (`player::video_viewport`: ACB's `sourceInfo` raster and pixel aspect, else the coded
  raster), and the synthesized ASS header's `MarginV` carries the same value, so libass text and
  the fallback caption draw agree. The lift is part of `TextStyle::generation`, so a header built
  before the first metadata callback is restyled once the real aspect lands. Image (PGS/VobSub)
  captions and native ASS/SSA keep their authored placement. Black bars baked into a full-frame
  encode are pixels, not geometry, so Auto reads such a picture as full-height and lands on Low.
- Embedded (SRT/ASS/PGS/VOBSUB/mov_text/WebVTT in the container): drawn by the app from the
  demuxer; they only need correct `Stream(type 3)` rows in file order (the probe gives them).
- External (addon subtitles, OpenSubtitles): give each a `Stream(type 3)` row with
  `key: /library/streams/<digits>.srt` (digits only, ≤20, ext alnum ≤10 — `sidecar_key_allowed`)
  and `codec: srt` (or `ass`). The app GETs, in order, `<key>?encoding=utf-8&format=srt`,
  `<key>?encoding=utf-8`, `<key>`, over the control plane, which does **not** follow redirects —
  the shim must fetch the addon URL itself and answer **200 with UTF-8 text** (gunzip, transcode
  cp1252/etc. to UTF-8). SRT and WebVTT both parse; ASS goes to libass.
- Auto-selection: when the account's `autoSelectSubtitle` mode is not Manual (`0`), the shim
  marks the sidecar whose language matches the account's `defaultSubtitleLanguage` as
  `selected` (`shim_media::apply_subtitle_preference`, gated in `ranked()`), so the app's
  `player::sidecar::restore_selection` enables it at a direct-play landing, and
  `/nuvio/streams` carries `selected` per subtitle so a play that landed before the subtitle
  addons answered can restore the same pick (the play-start poll below). **The pick is
  release-matched**: among language candidates it prefers language+source+resolution, then
  language+source, then language+resolution, then language only — a `movie1.1080.bluray.mkv`
  video picks the `movie1.1080.bluray.*.srt` sidecar over a bare one, and a WEBRip video
  prefers a WEBRip subtitle (WEB-DL is a distinct source). The BluRay family (`BluRay`,
  `BluRay REMUX`, `BDRip`, `BRRip`) folds together; every other source label stands on its
  own, so `HDTV` never source-matches a `DVDRip` subtitle. A source or resolution that either
  side does not name cannot match, so a torrent whose filename is not known yet ranks every
  candidate equal and falls back to language and file order. An exact tag (`pt-BR`) outranks a folded
  one (`pt`). 639-2/B and /T spellings (`ind`, `spa`, `ger`) and display names (`Indonesian`,
  `Bahasa Indonesia`) fold to the picker's code through a curated table — **never by truncating to
  two letters**, which mapped Kannada (`kan`) onto Georgian (`ka`) and Spanish (`spa`) onto `sp`.
  Manual mode (`0`) clears the pick entirely, preference or not. In a non-Manual mode with no
  preference set the first sidecar is selected; a preference that matches nothing claims
  nothing, so the file's own order stands. Forced tracks cannot be ranked: Stremio subtitle objects
  carry no forced flag, and `defaultSubtitleForced` is a mode, not a per-track attribute. The
  decision waits (up to the 2.5 s stream grace) for subtitle addons to report before publishing the
  part, but only when auto mode is on and a preferred language is set; a slow or absent addon never blocks playback and
  an empty result is handled without panicking.
- Search on demand: the player's Subtitles → Search page reads
  `GET /library/metadata/{rk}/subtitles?language=<2-letter>`, answered by the **builtin**
  OpenSubtitles v3 addon (`https://opensubtitles-v3.strem.io`) **and** the user's own subtitle
  addons (the same set the automatic lane above reads) in parallel, deduplicated by URL. The
  builtin adds coverage the installed addons may not have; the installed addons add any language
  the builtin's answer happens to lack, so the page is never narrower than the "Other languages"
  rows beside it. The page opens on the account's preferred subtitle language as saved in
  `nuvio-session.json` (`defaultSubtitleLanguage`) — the playback plan reads that file directly
  for the local Nuvio account (`nuvio` and `nuvio-profile-…`), because the shim's
  `/api/v2/user` carries no `profile` block and an HTTP preference read would answer defaults.
  The builtin answers in 639-2 spellings, takes no language parameter and returns a capped list
  per title, so the shim folds each candidate's code (`eng`, `pob`, `spl`, …) onto the page's
  2-letter code and filters locally. Every row is keyed `/library/streams/{id}.srt` with its URL
  registered, its release name in `title` (`movieReleaseName`, else the file name) and the
  answering addon's name in `providerTitle`.
  `PUT /library/metadata/{rk}/subtitles?key=/library/streams/{id}.srt` makes that candidate a
  sidecar: the shim remembers it by rating key, `ranked()` merges it beside the addons' own, and
  the item's next read — the one the app diffs with `identify` — lists it under the same release
  name and `selected`. That install state lives for the process; it is not persisted across a
  restart. A provider that did not answer is a `502` (the page's retry), never an empty answer the
  page would draw as "none found".

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

### Verified on the CX (2026-11-05, stable install, `forward-port/v0.9-hq` + the audio wave)

A TrueHD 8-channel 4K source played through the AC3 encode lane and at elevated speed.
The event log shows, in order:

- `start_bufferfeed: truehd 8ch -> AC3 5.1 encode lane (declared AC3)`, then
  `ff: audio lane truehd 8ch 48000 Hz decoded in-app -> AC3 5.1 640 kbps (encoded in-app)`
  and `ff: encode lane folds roles [0, 1, 2, 3, 9, 10] to 5.1(side)` — the encode lane and
  its channel mapping, on the device;
- at 1.5× and 2×, `speed: this Load plays at 1500|2000 (audio decoded in the app, fed as
  PCM)`, `speed: audio decoder for 1500|2000 open`, `speed: decoding audio fmt=2 ch=8
  rate=48000`, and `speed: pipeline audio audio/x-raw 48 kHz x2 -> ok` — the pipeline
  accepted the PCM the speed path fed, with one `"pcmInfo"` node
  (`channelMode stereo, S16LE, 48.0 kHz, 16 bit, interleaved`) in the Load;
- audio transcoding to `eac3,ac3,aac,dts` on `sound output=tv_speaker` behaved as
  specified.

The reported experience was normal playback, working speed changes and working AC3/
transcoded audio, with no error path; nothing in the log was a failure (the
`sched_setscheduler` and Matroska resync lines are long-standing and benign).

### Not yet verified on the CX (the lossless and lossy lanes)

This household plays ordinary film sources (AAC/AC3/E-AC3/DTS/TrueHD in MKV/MP4), so
the following paths are **host-verified only** — the unit and hostsim suites above, no
device run. They are correct by construction and by test, not by measurement on a
television:

- the lossless **PCM lane** for **FLAC** and **ALAC** (`ff_pcm.rs`), including the
  declaration and the demuxer feed;
- the **hi-res stereo decimator** (96 / 88.2 / 192 / 176.4 kHz → 48 / 44.1 kHz);
- the **`lossy=on` MP3/Vorbis lane** (off unless the knob is set; nothing in the
  default configuration touches it).

The first report from a device playing one of these should be recorded here. Until
then, treat a defect in them as unmeasured rather than absent.
