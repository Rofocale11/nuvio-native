# The Nuvio agent host loop (`nuviolive`)

This is the default way an agent tests a change. It boots the **real app** in the desktop simulator —
the same in-process shim, data layer, UI and player route the TV runs — with real public Stremio
addons and a real TMDB client, but a **sealed dev identity and a private redb**. No TV, no Nuvio
sign-in, no household account, no owner in the loop. Only playback needs the television, and only
with the owner's permission.

`tests/mock_pms.py` upstream is an HTTP PMS emulator. The fork does not need one: `plx_pmsshim`
already serves the PMS REST surface in-process. What was missing was a boot mode that combines that
real backend with an account-free identity — `plxnative-nuviolive` is that mode. The old
`plxnative-nuviofake` still exists for deterministic fixed-content tests; it is not the default.

To confirm a regression in a fork-specific behavior (sealed boot, person enrichment, the Streams
picker, privacy erase, playback), use the ready-made scenarios in `SCENARIO.md`: each names the
command, the pass evidence and the failure signature. This document stays the "how to drive" guide.

## One-time setup

```sh
make sim-nuvio          # macOS (host FFmpeg staged)
make sim-nuvio-linux    # Linux / WSLg (no host FFmpeg)
python3 tools/nuvio-sim.py selftest
```

The build uses `--no-default-features --features sim,play-uri,play-feed,devtriggers` in its own
`rust-modules/target-sim-nuvio` tree — both shipping playback backends, unlike `screenshots-sim`.
`devtriggers` is essential: plain `make sim-macos` builds the shipping feature set and **cannot**
arm `nuviolive`.

The dedicated TMDB key is already in the gitignored `rust-modules/stremio/nuvio.local.env` under
`TMDB_API_KEY`. The production key is a different value in a GitHub secret and is deliberately not
used here. An exported `TMDB_API_KEY` wins over the file. The driver never prints the value and
never copies it into the instance root.

## The loop

```sh
# Look at Home with the real catalog:
python3 tools/nuvio-sim.py --shot home

# Drive to a screen, then capture:
python3 tools/nuvio-sim.py --keys "down,down,right,ok" --shot detail --settle 12

# Assert a specific log line your change should produce:
python3 tools/nuvio-sim.py --keys "down,ok" --shot after \
  --expect 'a11y: refused' --expect 'nuvio: probing selected stream'

# Boot straight into a screen with a dev trigger:
python3 tools/nuvio-sim.py --trigger library=3 --shot library
```

Every run gets a fresh `/tmp/nuvio-sim-*` instance root unless `--runtime DIR` is given. The output
names the root, the log (`plxnative-events.log`) and the shot (`<name>-1.png`). Open the PNG — a
shot nobody looks at has verified nothing.

Flags: `--keys` (whitespace/comma tokens: `up down left right ok back play pause stop`, `ck:X,Y`,
`okdown`/`okup`, `shot`), `--key-gap`, `--settle`, `--shot`, `--trigger NAME[=VALUE]`, `--expect`
(repeatable substring), `--addons PATH`, `--bin PATH`, `--runtime DIR`, `--timeout`, `--keep`
(leave the app running and print its pid for a11y inspection). Exit 0 means every expected line
appeared and the shot landed; 1 is a failed assertion, 2 a setup error.

`tools/a11y.py` is available for id-based driving. Prefer it over key walks to reach an element:

```sh
python3 tools/nuvio-sim.py --trigger detail=10001 --keep     # note the printed runtime dir and pid
python3 tools/a11y.py state /tmp/nuvio-sim-XXXX              # ids, labels, subjects, focused element
python3 tools/a11y.py focus /tmp/nuvio-sim-XXXX nav.movies
python3 tools/a11y.py press /tmp/nuvio-sim-XXXX <id> --subject <ratingKey>
kill <pid>
```

`a11y.py`'s CLI refuses a `press` of any title-bearing id without `--subject`, including cast cards,
even when `state` reports `subject: null`. For a sealed live boot it is safe to bypass that CLI
guard with `a11y.press()` from Python or a direct FIFO write:

```sh
python3 -c "import os; f=os.open('/tmp/nuvio-sim-XXXX/plxnative-remote', os.O_RDWR); \
os.write(f, b'press:detail.cast.card.0 '); os.close(f)"
```

(`os.open(..., O_RDWR)` is the FIFO-safe spelling; a shell `printf > fifo` blocks forever in
`open(2)` when no reader is present.)

Opening a cast card reaches a real person page: TMDB biography header plus Filmography shelves
(`person.filmography.*`) with real posters. That is the way to check enrichment on this loop.

## What is real, and what is sealed

| Surface | Live loop |
|---|---|
| Shim, PMS REST dialect, data layer, screens, focus, motion | the shipping code |
| Catalogs, metadata, streams, subtitles | real addon HTTP calls (Cinemeta, Torrentio, OpenSubtitles by default) |
| TMDB cast/logo/person/trailers | real client with the dedicated key |
| Fetch, cache and store paths | real shim cache + private `nuvio-synthetic.redb` |
| Identity, account, watch state, addon config | sealed: dev PMS identity, nobody seated, no household records |
| Person biography / filmography | the shim answers `/library/people/{guid}` and `/credits` from TMDB — open a Cast & Crew headshot to check enrichment |

The boot log proves all of it:

```
nuvio: LIVE addon installed: https://…
nuvio: LIVE tmdb=configured — real addons, real shim caches, sealed account
nuvio: 3 addon(s): Cinemeta, Torrentio, OpenSubtitles v3
nuvio: shim serving on http://127.0.0.1:…
```

The boot line may still say `discover: base=real reads=open`. In this fork it is **not** a verdict
about where person reads go: `account::discover()` prefers `local_discover()` on every call, and the
shim is installed as that local server, so a person page's biography and filmography are served by
the shim from TMDB (`shim_pms.rs:5659`, `:5695`). Do not arm `plxnative-discover` to a hard-coded
port to "fix" that line — it would break when the shim falls back to a free port.

`nuvio: SYNTHETIC private redb store — household records untouched` appears by design. The
`ff: FFmpeg unavailable` line on `sim-linux` appears until the host FFmpeg is staged (see below);
without it playback refuses. Nothing in this mode writes to a Nuvio account, a Plex account or the
household install.

To change the addon set, edit the tracked `tests/nuvio-public-addons.json` (account-free https
manifest URLs) or pass `--addons PATH`. Debrid/configured URLs belong in a local override, never in
the tracked file. The app reads the list from `plxnative-nuvioaddons` in the instance root.

## Source picker (Streams) and the host engine

The tracked addon list uses Torrentio's configured endpoint
`https://torrentio.strem.fun/qualityfilter=unknown,cam,other,scr,threed,480p,720p|limit=2/manifest.json`
— the bare endpoint returned hundreds of streams per detail open and got rate-limited on repeated
agent loops. Direct reads of the configured endpoint give 9 streams for The Odyssey (`tt33764258`)
and 6 for a 2026 movie instead of 433 and 54; the live picker showed **Torrentio · 8** for a
2026 movie. Every source is still a torrent/magnet,
and the shim deliberately drops all torrent
and magnet streams when `p2p_enabled` is false (`shim_media::playable_url` / `rank`). A plain host
build has no `libsbengine.so` in `pkg/`, so the Streams pill correctly says "No playable streams
found" — that is the engine gate, not a missing addon.

Stage the already-built HOST engine to exercise the picker:

```sh
python3 tools/nuvio-sim.py --host-engine --keep --trigger detail=10005
```

`--host-engine` makes a private app dir of symlinks to `pkg/` plus
`build/sbengine-host/libsbengine.so`; the ARM library `make deploy` ships is untouched. The detail
pill then shows a bounded **Torrentio** source count for a Cinemeta movie, and pressing an episode
still (`detail.episodes.still.0` on a show page) opens the same picker carrying that episode's
ratingKey. Build the host engine with `tools/host-engine-check.sh --build-only` if
`build/sbengine-host/libsbengine.so` is missing.

Listing sources is hermetic — it is an addon HTTP read. **Picking a source is not**: it opens the
real engine against public peers. Do not press a source row unless that is intended.

## What this does NOT prove

- **No device claims.** Every simulator heartbeat carries `sim=1`. Frame rate, text rasterization,
  LG decoder behavior and video-plane composition need the TV, with the owner's permission.
- **No playback proof.** The host has no video plane; TV playback is already verified and is the
  only remaining reason to ask for the set.
- **Live content can move.** A changed catalog is not automatically a regression in your code;
  compare against the log and the shot from the run, and re-run before naming a cause.
- Pixel-identical screenshot determinism stays with `make screenshots`, which pins every moving
  value. That pipeline is a different job from this loop.

## Troubleshooting

- `simulator binary not found` → build with `make sim-nuvio` / `make sim-nuvio-linux`.
- `no dedicated TMDB key` → put `TMDB_API_KEY=` in `rust-modules/stremio/nuvio.local.env` or export it.
- `log did not show nuvio: LIVE tmdb=configured` → the binary was built without the local env's
  key and no runtime key was supplied; rebuild or export the key.
- `log did not show nuvio: LIVE addon installed:` → addon manifest fetch failed; check network and
  the URLs in the addon list. The loop fails closed rather than showing an empty catalog as success.
- `ff: FFmpeg unavailable` / `swresample is MISSING` → stage the HOST FFmpeg below. This is not a
  driver failure; the simulation just cannot demux until those x86-64 libraries are in `pkg/`.

## Host FFmpeg on Linux / WSL2 (playback in the simulator)

`make sim-linux` / `make sim-nuvio-linux` deliberately skip bundled FFmpeg. The ARM `.so`s in `pkg/`
cannot be dlopened by the host binary, but the same `ci/build-ffmpeg.sh` with `HOST=1` builds the
same FFmpeg 9.0 for this machine. The Linux host names must be **distinct** from the shipping ARM
ones so `make ipk` can never pick them up (`ci/check-package.py` excludes `*-plx-host.so.*`).

```sh
HOST=1 ./ci/build-ffmpeg.sh        # -> vendor/ffmpeg-prefix-host/lib/lib*-plx.so.*
for n in avutil:61 avcodec:63 avformat:63 swresample:7 swscale:10; do
  lib=${n%:*}; ver=${n#*:}
  cp "vendor/ffmpeg-prefix-host/lib/lib${lib}-plx.so.${ver}" \
     "pkg/lib${lib}-plx-host.so.${ver}"
done
make sim-nuvio-linux
python3 tools/nuvio-sim.py --shot home
```

All five are required: FFmpeg 9's native Opus decoder makes libavcodec depend on libswresample, so a
build that predates the fork's `--enable-swresample` line still logs `ff: swresample is MISSING` and
refuses playback. On a successful boot the log binds all five by their `-host.so.*` names and Play
reaches the host sink. This is host evidence only — no decoder, no video plane, no device FPS.
On macOS the equivalent is automatic: `make sim-nuvio` stages the host build as `.dylib` files via
`ci/stage-host-ffmpeg.sh`.

## Playback check (one command)

With the host FFmpeg staged, the driver can serve a local H.264/AAC file through the repo's
Range-capable fixture server and drive a real playback with the clock sink:

```sh
python3 tools/nuvio-sim.py --media ~/.cache/plxnative-demo/src/sintel-film.mp4 \
  --shot playback --settle 16
```

It picks a free port, writes `plxnative-playurl` (the stream + Load declaration) and
`plxnative-clocksink` into the instance root, and refuses to pass unless the log proves
`clocksink: ARMED`, `ff: v=#0 codec=h264`, `route=player` and the live-boot lines. The screenshot is
the real player HUD at a real position; the picture area is black **by design** — the clock sink
throws the access units away and nothing decodes. A run that gets a 404 or a codec the framing
refuses fails closed instead of calling a failure read-out a pass. `--vcodec` / `--acodec` override
the declaration for another sample. Use `--keep` to hold both the app and server for a11y work.

This proves the pipeline between the socket and the decoder: Range GETs, the FFmpeg demux, AU
framing, the presentation clock, HUD position. It proves nothing about LG's decoder, the video
plane or device frame rate.

## File map

- `rust-modules/src/app/nuvio/nuvio_debug.rs` — `Mode::{Normal,Fake,Live}`, trigger reads, addon install.
- `rust-modules/src/app/nuvio.rs` — one routed branch for the addon set; both upstream-file edits carry `Fork Rofocale11:`.
- `rust-modules/pmsshim/src/shim_fake.rs` — unchanged fixed fake backend, now only for `nuviofake`.
- `tests/nuvio-public-addons.json` — tracked public addon URLs.
- `tools/nuvio-sim.py` — the one-command agent driver.
- `tools/nuvio-fixture-addon.py` — loopback Stremio addon for checks that need a real ratingKey
  (subtitles drill-down, clear logo, resume, trailers); see `SCENARIO.md`.
- `Makefile` — `sim-nuvio` / `sim-nuvio-linux` targets (fork-marked).
- `docs/nuvio/MERGE-AUDIT.md` — exact upstream surface for this feature.
