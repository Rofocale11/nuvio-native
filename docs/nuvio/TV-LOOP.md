# The Nuvio TV loop (`nuvio-tv.py`)

This is the **device** twin of [`SIM-LOOP.md`](SIM-LOOP.md). It boots the same `plxnative-nuviolive`
profile on the dev television: real public Stremio addons, real TMDB enrichment, the in-process PMS
shim, a sealed dev-PMS identity and a private redb. No Nuvio or Plex account is opened and no watch
state is written. `SIM-LOOP.md` remains the host runbook — several simulators can run at once there;
there is one television and one app on it.

## Agent self-service contract

An agent may drive the set itself under this contract — no per-action owner prompt for a sealed
`nuviolive` session — but three boundaries do not move:

- **Debug install only, never stable.** `start`, `run` and `bench` refuse any `--flavor` but `debug`.
  The stable install is the owner's daily driver and this loop never touches it.
- **No unattended deploy.** Every boot path compares `pkg/plxnative` with the bytes on the set and
  **refuses on a mismatch**; `nuvio-tv.py` contains no deploy call. When a rebuild is needed, tell
  the owner and wait for them to approve `make FLAVOR=debug deploy`. Add `ARM_PROFILE=release` only
  for a timing run: the fast `tvdev` profile is the default for the debug loop because a release
  build is slow, and a frame rate graded on `tvdev` is refused by `arm_profile_guard`.
- **The lock still arbitrates.** `PLX_TV_IN_USE_OK=1` is the owner's override of an `IN USE:`
  pre-flight, not permission an agent grants itself. An `IN USE:` set means stop and tell the owner;
  wait with `tv-lock.sh acquire --wait` rather than overriding. Every session verb now fails closed
  when `tv-lock.sh require` refuses.

No Nuvio/Plex account, household redb or watch state is ever opened. Sound-off and panel-off are
requested on every boot (`tests/run.py --fps` and `profile-tv` follow the same standing rule), but
they are **best-effort**: the webOS 10.3.1 set currently answers `setMuted`/`getVolume` with
“Unknown method”, and the tool warns and continues rather than blocking a run. `bench` asks to blank the panel by default;
`bench --panel-on` leaves it lit so the owner can watch the walk, while sound stays muted. An interactive
`start`/`run` leaves the panel as the owner set it — an explicit owner override recorded 2026-11-27.
One television, one app, one lease.

## One-time setup

```sh
make                                   # build only; deploy is separate and owner-approved
python3 tools/nuvio-tv.py selftest     # offline wiring check, no ssh, no lock, no TV
```

The debug install must already exist (`make FLAVOR=debug install` once). The dedicated TMDB key is
baked into the debug ARM binary at build time from the gitignored
`rust-modules/stremio/nuvio.local.env` (`option_env!`); nothing is passed at runtime and no secret
is written to a tracked file. `selftest` checks that the build recipe still does this; the
`nuvio: LIVE tmdb=configured` boot line is the proof that the key reached the binary. The tracked public
addon list is staged into the install's runtime root at boot.

`nuviolive` is compiled out of release, so this works on the debug install only.

## Persistent session — boot once, then drive

This is the mode to use when an agent needs to look, move, look again and confirm. `start` boots the
app and holds one TV lease; every later verb acts on that **same running process** and renews the
lease. Nothing relaunches between commands.

```sh
python3 tools/nuvio-tv.py start                 # boot live, hold lease
python3 tools/nuvio-tv.py shot home             # capture wherever the app is now
python3 tools/nuvio-tv.py detail                # read the live snapshot, press a card into Detail
python3 tools/nuvio-tv.py shot detail
python3 tools/nuvio-tv.py key down down ok      # D-pad tokens through the real handlers
python3 tools/nuvio-tv.py state home.json       # a11y snapshot, by-id driving
python3 tools/nuvio-tv.py press home.shelf.0.card.0 --rk 10000
python3 tools/nuvio-tv.py log 'detail: sid='     # read the log; full copy saved locally
python3 tools/nuvio-tv.py expect 'detail: sid='  # nonzero exit if that line is missing
python3 tools/nuvio-tv.py status
python3 tools/nuvio-tv.py stop                  # hand the app back, release the lease
```

The session record is local (`/tmp/nuvio-tv-session.json`, mode 0600), not on the television. `status`
prints the session's runtime directory; `log` writes the full event log there as `event-log.txt` before
printing the last 200 matching lines — the set's log is overwritten on the next boot, so copy it if
the finding matters. If a run is interrupted, `stop` clears it; the lease also expires on its own TTL.

### Why `detail` is a verb, not a boot screen

Every synthetic live boot rebuilds its private key table, so numeric `ratingKey`s are **not stable
across boots**. A key read from a previous run resolves to nothing: the page shows only a title and
a blank wash. `detail` avoids this by reading the current Home snapshot and pressing a shelf card
into Detail inside the same boot. `--screen detail=<rk>` in the one-shot form is only safe with a
key obtained during that same run; the tool fails loudly on a `no metadata` answer.

## Graded benchmark lane

`bench` is the device-side perf loop for an agent: one sealed `nuviolive` boot, LIVE proofs asserted,
panel blanked and sound muted by default (`--panel-on` keeps the panel lit for watching), one key walk,
then a grade from the app's own once/sec `loop=`/`fps=` heartbeat. It reuses `tests/run.py`'s parsers and floor verdicts rather than defining a second set of
gates, and it holds one lock exactly like the one-shot form.

```sh
python3 tools/nuvio-tv.py scenes                       # list the fork-owned scenes
python3 tools/nuvio-tv.py bench --scene home-nav --shot home-nav
python3 tools/nuvio-tv.py bench --scene detail-idle    # picks a card from THIS boot's snapshot
python3 tools/nuvio-tv.py bench --scene home-idle --trigger imgtrace
python3 tools/nuvio-tv.py bench --scene home-nav-release  # graded; needs owner-approved release
python3 tools/nuvio-tv.py bench --suite                  # every scene, one sealed boot each
python3 tools/nuvio-tv.py bench --scene home-nav-deep    # 15 down @0.5s, 2s pause, 15 up @0.5s
python3 tools/nuvio-tv.py bench --scene home-nav-deep --panel-on  # same walk, panel left on
```

A scene's walk goes out through `tv-session.sh keyseq <gap> <tokens>`: ONE ssh command spaces the
FIFO writes on the set at `key_gap` seconds. The older `key` verb spends ~2 s per token in its remote
time box, which made a nominal 0.5 s cadence arrive at ~2.5 s. A scene with `pause_after` splits the
walk into two keyseq legs with a rest between, and the bench prints each leg's worst-frame and drop
count alongside the whole-run grade; the grade itself still covers the whole run. Those leg counts
are informational — the authoritative cold-vs-warm comparison is the gate's two boots (below),
where the cache state is actually reset, not just the focus path reversed.

Benchmark boots use `tests/nuvio-tv-benchmark-addons.json`, staged on the set as
`nuvio-benchmark-addons.json`. That catalog addon is **benchmark-only**: `start` and the one-shot
form still read `tests/nuvio-public-addons.json`, so an ordinary session never sees it. A scene may
name its own `addons` file; otherwise the benchmark list is the default.

`bench --suite` is this fork's equivalent of upstream's `--fps` suite; it lives in `nuvio-tv.py`,
not in a second `run-nuvio.py` harness. Each scene still gets its own boot and its own lease, and
the suite reports one PASS/FAIL line per scene.

Scenes live in `tests/nuvio-tv-scenes.json` (fork-owned). Each names a boot screen, route, key
tokens (`key_gap`, plus `pause_after`/`pause_s` for a mid-walk rest), warmup/run seconds and optional
`loop_floor` / `fps_floor` / `framedrop` gates. Add a scene
there rather than hand-writing a boot command. `home-idle`, `home-nav` and `detail-idle` have **no
floor**: they run on the fast `tvdev` debug build and only report numbers, which is the cheap loop.
`home-nav-release` carries a floor and refuses a `tvdev` binary through
`tools/arm_profile_guard.py` before it launches; building and deploying `ARM_PROFILE=release` stays
an owner-approved step. Establish a baseline before adding a floor to a scene, because live
catalogs move and a changed shelf is not a regression.

### What the bench actually grades

| Heartbeat field | What it is | Gate when the scene declares one |
|---|---|---|
| `loop=` | loop iterations per second — **liveness only**; a settled screen still reads ~60 | `loop_floor` on the 2nd-lowest sample (one transient dip tolerated) |
| `fps=` | frames actually swapped per second — the presented frame rate | `fps_floor` on the median (or a count of live windows); a settled screen legitimately reads ~1 |
| `worstframe=` | worst whole-iteration time in that second, present only with `plxnative-framedrop` armed | `worst_ceiling_ms` on the 2nd-highest sample |
| `FRAMEDROP total=` | one line per frame over the armed threshold | `stall_ceiling_ms` on the whole-run peak, warmup included |
| `drift` | first-third vs last-third loop mean | reported only — a thermal signature needs a long soak |

So "FPS drops" are the last two rows, not `loop=`. All six upstream UI-tier gate families are now
wired into `grade_scene` (`loop_floor`, `fps_floor`, `fps_ceiling`, `worst_ceiling_ms`,
`stall_ceiling_ms`, `coldopen_ceiling_ms`) plus upload pacing; the shipped scenes still declare no
ceilings, so they prove route liveness and report numbers on the cheap `tvdev` loop. After an
owner-approved `ARM_PROFILE=release` baseline, add ceilings to a scene to make it fail on slow or
dropped frames. The grade fails closed if the detector was not armed (no `worstframe=` samples).

**Pass** means: exit 0, all LIVE proof lines plus the scene's `expect` lines still in the final log,
at least five post-warmup heartbeats on the named route, and every declared floor met. The full log
is saved as `bench-<scene>.log` in the printed runtime directory; open the shot before calling a
visual change verified. **Fail** is exit 1 with the failing gate in the message. Nothing here grades
the LG decoder, the video plane, visible text rasterization or device frame pacing under a
profiler — those go through `which-tier` → `profile-tv`, with the owner's permission for playback.

## One-shot form

For a single boot-and-capture, the script takes its own lease, boots, asserts the LIVE proof lines
and hands the app back automatically:

```sh
python3 tools/nuvio-tv.py --shot home
python3 tools/nuvio-tv.py --open-detail --shot detail
python3 tools/nuvio-tv.py --screen home --trigger imgtrace --shot home
```

Both forms assert the same proofs as the host loop:

```
nuvio: LIVE addon installed:
nuvio: LIVE tmdb=configured
nuvio: shim serving on
```

## By-id driving

`state FILE`, `press ID` and `press ID --rk RK` expose the same accessibility driving as the host
loop's `tools/a11y.py`. A press of any element that acts on a title it names needs `--rk`.

`tools/tv-session.sh` injects remote tokens as the **FIFO's own uid** via `setpriv`. The FIFO is
created by the jailed app (mode 0660); root cannot write it on this firmware, and the old root write
silently reported success while sending nothing. That bug was why `state` and `key` never worked on
a flavoured install.

## Screenshots

`shot NAME` first tries the panel capture service (`com.webos.service.tv.capture`). Where the
firmware lacks it, it falls back to the app's own capture stream: the boot arms `plxnative-capture`,
a local `stream-screen.py` pulls a JPEG from the app on port 8911, and the file is saved **locally**
as `<name>.jpg`. No screenshot is written to the television. The app stream is the UI plane only —
it cannot show the hardware video overlay, so playback must still be judged from a panel capture or
the video-plane tools.

## Lock and handback

One lease covers one interactive session. `start` refuses a fresh `IN USE:` pre-flight unless the
**owner** sets `PLX_TV_IN_USE_OK=1`; an agent must not set it for itself. `stop` calls `tv-session.sh down` and releases the
lease; the one-shot form does both in a `finally` block, including on Ctrl-C. Do not run this while
another lane holds the television, and do not hold a lease while reading captures, editing code or
building.

## Pre-release gate

`gate` turns a graded scene into a **local, owner-run pre-release check**:

```sh
python3 tools/nuvio-tv.py gate --scene home-nav-deep            # 2 boots: cold, then warm
python3 tools/nuvio-tv.py gate --scene home-nav-deep --runs 3   # 1 cold + 2 warm
```

The **first boot is pure cold**: before it, the gate deletes the debug install's image cache
(`/media/developer/<appid>-imgcache` and its fallbacks) and its `nuvio.redb`, so the catalog, posters
and store all start empty. **Every later boot reuses exactly what run 1 populated** — no deletion —
so the warm run measures the same walk with a warm image cache and store. This is a cache-state A/B
ACROSS boots, not the down/up legs within one boot.

**What "smooth" means here.** The verdict is not an `fps=` median: a settled screen legitimately
skips presents. `home-nav-deep` grades only the window **from the first key onward** — boot and the
first Home mount are reported as `boot max=… (informational)` and do not decide browsing smoothness.
A frame over 22 ms is a hitch; over 33 ms is a visible jank (two 60 Hz refreshes missed). The scene's
contract is **at most 5 frames over 22 ms and none over 33 ms across the 30-click walk**; the old
220/300 ms ceilings remain only as gross-regression backstops. As of 2026-11-27 this contract FAILS
on this set — see `VALIDATION.md` — so a PASS currently means "no jank found," not "release allowed."

Each boot gets its own sealed scene and its own grade; the verdict is a strict majority (2 boots →
both must pass, 3 boots → 2 of 3). `gate-<scene>.json` in the printed runtime directory records each
run's `cache` label and full grade string, so cold and warm numbers can be compared side by side.
The reset is fail-closed: only a debug app id from `make print-appid` is ever named; the stable
install is never a candidate.

This is deliberately **not in CI** — `ci.yml` says CI has no runtime that renders or holds 60 fps —
and it does **not** prove the stable install, which this loop never touches. It grades the debug
install with an `ARM_PROFILE=release` binary, the closest artifact this loop is allowed to use. Run
it before cutting a release and record the verdict in the release note as local evidence, never as a
CI gate.

## What counts as confirmed

A device check is confirmed only when the **same run** has (a) the LIVE boot proof lines, (b) the
expected log lines or a11y ids, (c) a shot you actually opened, and — for timing — (d) a `bench`
PASS from a release-profile binary with enough heartbeat samples. `expect` and `bench` fail closed;
`log` retains the full event log locally instead of trusting a tail. `SCENARIO.md` pins the host
behaviors and says which ones have no device result; when you add a TV-only check, record its exact
command, pass evidence and regression signature there or in this file instead of leaving it to
memory.

## Limits and troubleshooting

- `--media`, `--host-engine`, `--keep` and `--bin` are host-only and are not accepted.
- `deployed debug binary does not match pkg/plxnative` → a rebuild needs the owner's approval
  (`make FLAVOR=debug deploy`, plus `ARM_PROFILE=release` for a timing run). The tool will not deploy
  itself; never "fix" this by adding a deploy call.
- `REFUSING: the television is held by another lane` → wait, do not set `PLX_TV_IN_USE_OK=1` on your
  own. Session verbs fail closed now; a lost lease needs the owner to re-acquire before `stop`.
- `refusing to grade timing: ... tvdev` → the deployed binary is the fast debug build. Ask for an
  owner-approved `make ARM_PROFILE=release deploy` before grading any `loop=`/`fps=` floor.
- Live catalogs move between boots. A changed shelf is not automatically a regression; compare the
  log and shot from the same run, and re-run before naming a cause.
- `state` times out → the app is down, a release build has no devtriggers, or the FIFO injection is
  broken; check the `install:` line and `tv-session.sh status`.
- `no frame from either capture path` → the boot did not arm `plxnative-capture`, or the app was not
  serving on the install's capture port (`make -s print-appport FLAVOR=debug`). The streamer's log is
  saved beside the shot directory.
- app-stream shot shows the PREVIOUS screen → the stream only updates on a present; after a by-id
  press, send a key (or wait) before shooting. Verify the route from `state` and the log, not from a
  stale keepalive frame.
- `no metadata` on a Detail boot → stale `ratingKey`; use `detail`, not `--screen detail=<rk>`.
- Exit 1 is a failed assertion or device command; 2 is usage/preconditions.
