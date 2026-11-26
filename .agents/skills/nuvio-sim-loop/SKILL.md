---
name: nuvio-sim-loop
description: >
  The default host debug/test loop for Nuvio Native agents. Boots the desktop simulator AS THE REAL
  APP — in-process shim, public Stremio addons, dedicated TMDB key, real fetch/cache paths — with a
  sealed identity and private store, so no Nuvio login, owner or TV is needed. Use for UI, data-layer,
  enrichment and performance iteration instead of upstream's mock_pms.py or a device run. TV is only
  for playback, and only with the owner's permission.
---

# nuvio-sim-loop — test the real app on the host

The fork's `plx_pmsshim` already serves the PMS REST surface in-process, so there is no upstream
`mock_pms.py` to imitate. The missing piece was an account-free boot that still uses the REAL addon
backend: `plxnative-nuviolive`. Full details: `docs/nuvio/SIM-LOOP.md`.

## First time

```sh
make sim-nuvio          # macOS; Linux/WSLg: make sim-nuvio-linux
python3 tools/nuvio-sim.py selftest
```

`make sim-macos` deliberately has no `devtriggers`, so it cannot arm this mode. The dedicated TMDB
key is already in the gitignored `rust-modules/stremio/nuvio.local.env`; the production key is a
different GitHub secret and is never used here. Nothing to export, nothing to ask the owner.

## Every iteration

```sh
python3 tools/nuvio-sim.py --shot home
python3 tools/nuvio-sim.py --keys "down,down,right,ok" --shot detail
python3 tools/nuvio-sim.py --trigger library=3 --shot library
python3 tools/nuvio-sim.py --keys "down,ok" --expect 'a11y: refused' --shot after
```

Each run gets a fresh instance root; the output names the log and the numbered PNG. **Open the
image.** Use `tools/a11y.py state|focus|press <runtime>` to reach an element by id instead of key
walks; a `press:<id>` of a title-bearing element needs `--subject <ratingKey>` from `state`.

The log must show the live proofs before you trust the run:

```
nuvio: LIVE tmdb=configured
nuvio: LIVE addon installed:
discover: base=stand-in reads=open
```

## Rules

- No TV, no account, no owner. The boot is sealed (dev-PMS identity, private redb, nobody seated).
  A person page's biography and filmography are served by the shim from TMDB — verified with a real
  Matt Damon header and 227 filmography credits. Open a Cast & Crew headshot to check enrichment;
  `a11y.py`'s CLI guards cast-card presses, so use `--keep` plus a FIFO write for those. Never write
  to a real Nuvio or Plex account.
- The boot line `discover: base=real reads=open` is stale here: `account::discover()` prefers the
  installed shim on every call. Do not arm `plxnative-discover` to a fixed port to silence it.
- **Nothing here is a device result.** Every heartbeat carries `sim=1`; frame rate, text
  rasterization, LG decoding and the video plane need the TV with explicit permission. Playback is
  already verified and is the only remaining device need.
- Live catalogs change. An agent attributing a changed shelf to its own edit needs a second run and
  the log as evidence, not a guess.
- Pixel-identical screenshot determinism belongs to `make screenshots`, not this loop.
- On Linux/WSL2 `ff: FFmpeg unavailable` means the host FFmpeg is not staged; the recipe is in
  `SIM-LOOP.md` (`HOST=1 ./ci/build-ffmpeg.sh` + five `pkg/lib*-plx-host.so.*` copies). macOS
  `make sim-nuvio` stages it automatically.
- Streams/source picker: plain host builds have no `libsbengine.so`, so torrent-only sources are
  correctly dropped and the pill says "No playable streams found". Use `--host-engine` to stage the
  host engine in a private app dir; the detail pill and an episode still then show Torrentio sources
  from the tracked `qualityfilter=…|limit=2` endpoint (bounded, not the hundreds the bare
  endpoint returned). Listing is hermetic; picking a source opens real public torrents.
- Playback check: `tools/nuvio-sim.py --media <local H.264/AAC file> --shot playback`. It serves the
  file with Range, arms `playurl` + `clocksink`, and fails unless the log proves demux and HUD
  position advance. The picture is black by design — no decoder runs on the host.
- Add public addons in `tests/nuvio-public-addons.json` (https manifest URLs only); no debrid
  token, account key or production TMDB key is ever checked in.
