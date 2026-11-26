# The Nuvio TV loop (`nuvio-tv.py`)

This is the **device** twin of [`SIM-LOOP.md`](SIM-LOOP.md). It boots the same `plxnative-nuviolive`
profile on the dev television: real public Stremio addons, real TMDB enrichment, the in-process PMS
shim, a sealed dev-PMS identity and a private redb. No Nuvio or Plex account is opened and no watch
state is written. `SIM-LOOP.md` remains the host runbook — several simulators can run at once there;
there is one television and one app on it.

## One-time setup

```sh
make                                   # build the ARM binary into pkg/plxnative
python3 tools/nuvio-tv.py selftest     # offline wiring check, no ssh, no lock, no TV
```

The debug install must already exist (`make FLAVOR=debug install` once). The dedicated TMDB key is
baked into the debug ARM binary at build time from the gitignored
`rust-modules/stremio/nuvio.local.env` (`option_env!`); nothing is passed at runtime and no secret
is written to a tracked file. `selftest` checks that the build still does this. The tracked public
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
python3 tools/nuvio-tv.py log 'detail: sid='     # read the on-device event log
python3 tools/nuvio-tv.py status
python3 tools/nuvio-tv.py stop                  # hand the app back, release the lease
```

The session record is local (`/tmp/nuvio-tv-session.json`, mode 0600), not on the television. If a
run is interrupted, `stop` clears it; the lease also expires on its own TTL.

### Why `detail` is a verb, not a boot screen

Every synthetic live boot rebuilds its private key table, so numeric `ratingKey`s are **not stable
across boots**. A key read from a previous run resolves to nothing: the page shows only a title and
a blank wash. `detail` avoids this by reading the current Home snapshot and pressing a shelf card
into Detail inside the same boot. `--screen detail=<rk>` in the one-shot form is only safe with a
key obtained during that same run; the tool fails loudly on a `no metadata` answer.

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
caller explicitly sets `PLX_TV_IN_USE_OK=1`. `stop` calls `tv-session.sh down` and releases the
lease; the one-shot form does both in a `finally` block, including on Ctrl-C. Do not run this while
another lane holds the television, and do not hold a lease while reading captures, editing code or
building.

## Limits and troubleshooting

- `--media`, `--host-engine`, `--keep` and `--bin` are host-only and are not accepted.
- Live catalogs move between boots. A changed shelf is not automatically a regression; compare the
  log and shot from the same run, and re-run before naming a cause.
- `state` times out → the app is down, a release build has no devtriggers, or the FIFO injection is
  broken; check the `install:` line and `tv-session.sh status`.
- `no frame from either capture path` → the boot did not arm `plxnative-capture`, or the app was not
  serving on the install's capture port (`make -s print-appport FLAVOR=debug`). The streamer's log is
  saved beside the shot directory.
- `no metadata` on a Detail boot → stale `ratingKey`; use `detail`, not `--screen detail=<rk>`.
- Exit 1 is a failed assertion or device command; 2 is usage/preconditions.
