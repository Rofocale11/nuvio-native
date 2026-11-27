---
name: nuvio-tv-loop
description: >
  On-device debug, verification and benchmark loop for Nuvio Native through `tools/nuvio-tv.py`:
  a persistent sealed `nuviolive` session, by-id driving, log assertions, local captures and graded
  bench scenes. Use when a change needs the ACTUAL television — device-only behavior, reproducing
  a failure on the set, or frame-rate/benchmark evidence — and NOT for the default host loop
  (`nuvio-sim-loop`), which needs no TV, lock or owner. Debug install only, never stable, no agent
  deploy. Hands off lock/IN USE/wake to `tv-lock`/`wake-tv` and playback/profiler work to
  `profile-tv`.
---

# nuvio-tv-loop — the device twin of nuvio-sim-loop

**Read `docs/nuvio/TV-LOOP.md` before any device command.** That file is the runbook; this skill is
only the router and the boundaries. The host loop stays `nuvio-sim-loop`: run that first for UI,
data-layer, enrichment and most performance iteration. The simulator's `sim=1` heartbeat is never a
device claim, and a TV run needs the one television and one app instance.

## When to load this skill

- A bug only reproduces on the cross-compiled ARM install, not in `nuviolive` on the host.
- A device-only behavior needs evidence (see the unpinned list in `docs/nuvio/SCENARIO.md`).
- A benchmark needs the app's own `loop=`/`fps=` heartbeat from the set, not the simulator.
- You already hold a session from `tools/nuvio-tv.py start` and need to drive, assert or capture it.

Not for: ordinary UI/data work (`nuvio-sim-loop`), deciding which tier applies (`which-tier`),
frame-drop diagnosis or LG decoder/video-plane measurement (`profile-tv`), lock/IN USE/wake
(`tv-lock`, `wake-tv`), or upstream's guest/mock boots — on this fork they cannot reach Home.

## Boundaries — the owner's standing authorization

An agent may drive the **debug** install itself under `TV-LOOP.md`'s self-service contract:
sealed `nuviolive`, no Nuvio/Plex account, household redb or watch state, sound muted on every boot,
one lease, lock arbitration. `bench` blanks the panel by default; `bench --panel-on` leaves it lit
so the owner can watch, and an interactive session leaves it as the owner set it (explicit owner
override 2026-11-27). The authorization does **not** extend to:

- the **stable** install — the owner's daily driver, never touched by this loop;
- any **deploy** — the tool refuses a stale binary; ask the owner, then they approve
  `make FLAVOR=debug deploy`;
- `PLX_TV_IN_USE_OK=1` — only the owner may override an `IN USE:` pre-flight;
- `ARM_PROFILE=release` — only for a graded timing scene, with owner-approved deploy. The normal
  bench scenes run on `tvdev` and report numbers without a floor.

## The loop

```sh
python3 tools/nuvio-tv.py selftest          # offline, no ssh/lock/TV
python3 tools/nuvio-tv.py scenes            # list fork-owned bench scenes
python3 tools/nuvio-tv.py start             # boot once, hold one lease
python3 tools/nuvio-tv.py shot home
python3 tools/nuvio-tv.py state home.json   # a11y snapshot for by-id driving
python3 tools/nuvio-tv.py press <id> --rk <rk>
python3 tools/nuvio-tv.py expect 'detail: sid='   # nonzero exit when missing
python3 tools/nuvio-tv.py log 'detail: sid='      # full log saved to the runtime dir
python3 tools/nuvio-tv.py stop              # hand the app back, release the lease

python3 tools/nuvio-tv.py bench --scene home-nav
python3 tools/nuvio-tv.py bench --suite                    # every scene, one boot each
python3 tools/nuvio-tv.py bench --scene home-nav-release   # graded; needs approved release deploy
python3 tools/nuvio-tv.py bench --scene home-nav-deep --panel-on  # watch the walk, panel stays on
python3 tools/nuvio-tv.py gate --scene home-nav-deep            # cold boot, then warm boot
python3 tools/nuvio-tv.py gate --scene home-nav-deep --runs 3   # 1 cold + 2 warm
```

`gate` run 1 deletes the debug install's imgcache and `nuvio.redb` before boot (pure cold); runs 2+
reuse what run 1 created (warm). The evidence JSON carries each run's cache label and grade so the
two can be compared. Smoothness is graded **from the first key onward**: ≤5 frames over 22 ms and
none over 33 ms across the 30-click walk; boot/first-mount is reported, not graded. It currently
FAILS on this set — see `VALIDATION.md` — do not read an older PASS as "smooth". `home-nav-deep` is
15 Down @0.5 s, 2 s rest, 15 Up @0.5 s; the walk goes out as ONE `tv-session.sh keyseq` call per
leg, so the gap is exact on the set. Benchmark boots read
`tests/nuvio-tv-benchmark-addons.json`, staged as `nuvio-benchmark-addons.json`; normal sessions keep
the tracked public addon list.

Every session verb fails closed if the lease check refuses. Usage errors exit before taking a lease.
Do not hold a lease while reading captures, editing code or building.

## What counts as confirmed

Same run: LIVE boot proof lines, expected log lines or a11y ids, a shot you actually opened, and —
for timing — a `bench` PASS from a release-profile binary with at least five post-warmup heartbeats.
`expect` and `bench` fail closed; `log` retains the full event log locally. Record any new TV-only
check's command, pass evidence and regression signature in `TV-LOOP.md` or `SCENARIO.md`.

This lane grades UI heartbeat only. It proves nothing about LG decoding, the video plane, visible
text rasterization or profiler-instrumented pacing; those need `profile-tv`, and playback stays
subject to the owner's permission.
