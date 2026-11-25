# Synthetic debug mode (agents, simulator and TV)

Fork Rofocale11: a single devtrig that makes **both** the macOS simulator and a debug TV boot the
real in-process PMS shim over `plx_pmsshim::shim_fake::FakeBackend`. It replaces the upstream
`--guest`/`--mock` identity, which this fork short-circuits (`docs/nuvio/TV-LOCK-MCP.md`).

## Arm it

The trigger is `plxnative-nuviofake` in the install's runtime directory. It is a normal devtrig:
arming it marks the boot automated (so the who's-watching picker is skipped) and it is compiled
out of release builds.

- **Simulator / host:** create an empty file in the runtime root before launching, e.g.
  `touch "$PLXNATIVE_RUNTIME_DIR/plxnative-nuviofake"` then `make sim`. A plain host run without
  `hostsim` resolves the runtime root to `/tmp`, so use the simulator's own directory to avoid
  affecting other checkouts.
- **TV:** check `tools/tv-lock.sh status` first. An `IN USE:` line means a person may be on the set
  — stop and ask the owner; the `PLX_TV_IN_USE_OK=1` override is only for their explicit go-ahead.
  Build with the lease released, acquire a short lease, then use the existing `tv-session.sh` door,
  which writes boot triggers before launch:
  `tools/tv-session.sh up --screen home --arm nuviofake`. Do **not** combine it with `--mock`
  (that grades the Plex synthetic identity) or sign in first. Run `down` then `release` as soon as
  the run is over, as the `tv-lock` skill requires.

Disarm by removing the trigger and relaunching; triggers are read only at boot.

## What it gives an agent

- A deterministic Home with two catalogs (Popular Movies, Popular Series), four Blender films and
  one two-season series. Detail, search and a two-source Streams picker (one direct MP4, one fake
  torrent infohash) work through the same shim routes as real content.
- A dev-PMS boot: no Nuvio account is resumed and nobody is seated, so even a TV with a stored
  household session reaches Home instead of the QR sign-in.
- The shim's own identity, `1.41.0.0000-nuvio` on machine `nuvionative-pmsshim-0001`. Grade that;
  never grade upstream's `1.41.0.0000-synthetic`. The event log names the mode:
  `nuvio: SYNTHETIC debug backend — fixed catalog, no Nuvio account reads or writes`.

## Driving it (proven on a debug TV, 2026-11-26)

Fake `ratingKey`s are deterministic. Read them from a **host-only** shim instead of guessing a
title on the remote:

```bash
cargo build --manifest-path rust-modules/Cargo.toml --locked -p plx_pmsshim --bin pmsshim-serve
rust-modules/target/debug/pmsshim-serve --fake --port 32421 &
PID=$!
curl -s http://127.0.0.1:32421/hubs | python3 -c \
  'import json,sys; d=json.load(sys.stdin); print([(h["title"], [(m["ratingKey"],m["title"]) for m in h.get("Metadata",[])]) for h in d["MediaContainer"]["Hub"]])'
kill "$PID"
```

Observed keys: `10000` = Open Movie Anthology (series), `10001` = Big Buck Bunny, `10002` = Sintel,
`10003` = Tears of Steel; the series' episode cards were `10006`–`10009`. Re-read `/hubs` rather
than pinning these numbers into a harness if the fake list ever changes.

Movie hero Streams pill (the shortest path):

```bash
tools/tv-session.sh up --screen detail=10002 --arm nuviofake
tools/tv-session.sh state --out /tmp/detail.json          # detail.hero.alt is the pill
tools/tv-session.sh press detail.hero.alt
tools/tv-session.sh press alt.row.0                       # row labelled "Fake"
tools/tv-session.sh state                                 # now "1080p AVC WEB-DL AAC · 150 MB · Fake"
```

Episode / player More path:

```bash
tools/tv-session.sh up --screen detail=10001 --arm nuviofake
tools/tv-session.sh state --out /tmp/detail.json          # detail.hero.play's subject is the episode rk
tools/tv-session.sh press detail.hero.play --rk <that subject>
tools/tv-session.sh press player.control.more
tools/tv-session.sh press more.streams
tools/tv-session.sh press more.addon.0
tools/tv-session.sh state                                 # more.stream.0 is the source row
```

The rule about subject-bearing presses still applies: give `--rk` for anything that acts on a
title, using a key you read from `state` or the log. The openers above (`detail.hero.alt`,
`player.control.more`, `more.streams`, `more.addon.0`, `alt.row.0`) carry no subject and are pressed
bare.

Proof it is still isolated: `hubs: source 0 ok — … 0 in CW`, `nuvio: SYNTHETIC private redb store`,
`/api/v2/user` returning the local `Nuvio Native` user, `state` showing `owner=page`, and no
`coldopen screen=account` line. The fake torrent row has no bytes behind it; the MP4 row is a real
remote URL, so decoded playback needs a reachable TV and `shot` (the UI stream cannot see the video
plane).

## What is deliberately inert

- **A private, empty `nuvio-synthetic.redb` overrides every store read and write.** `data_dir: None`
  alone is NOT enough: the shim's `WatchState`, `Keys` and caches read the global one-store handle
  regardless of `data_dir`. The private file is deleted at every synthetic boot. `prepare_store()`
  installs it before `session::load`, and `nuvio_store::get`/`get_arc` prefer it over the shipping
  store even when platform seams opened that file earlier — so no real account, Continue Watching,
  PIN or cache row is visible or written.
- The household's imported addon set and `settings.json` are not read either; the fixed catalog
  needs neither.
- No account watch pull at boot, no `/:/timeline` progress push, no My List, no TV PIN or addon
  writes, no Settings > Addons / Profiles control, no addon fingerprint worker, no torrent engine.
- The fake torrent stream is listed but has no bytes behind it; the sim has no video pipeline and
  can only grade control flow. Pixel output and decoded playback still need the TV.
- Artwork URLs point at `images.metahub.space`, so "network-free" describes the catalog data, not
  the pixels.

## Code shape

- `rust-modules/src/app/nuvio/nuvio_debug.rs` (fork-owned child of the fork-owned `nuvio.rs`) owns the
  trigger name and the backend/config selection. The only upstream edits are three small
  `Fork Rofocale11:` branches in `app/boot.rs`: install the private store before `session::load`,
  keep the DevPms session-init for a synthetic boot, and take the dev identity path at the boot gate.
- `Config::synthetic`, `Account::new_synthetic` and the watch-pull/push gates in fork-owned
  `plx_pmsshim` make the account isolation explicit. `plx_nuvio_store::install_ephemeral`'s
  `EPHEMERAL` override on `get`/`get_arc` is what actually keeps the household's `nuvio.redb` rows
  out of every consumer — see `MERGE-AUDIT.md` for the bug this replaced.

## Verification

Host: `make test-crate C=plx_pmsshim`, `C=plxnative-modules` and `C=plx_nuvio_store` cover the
selector, trigger read, synthetic config, signed-out account and the store override (including a
test that plants a household row and proves the override hides it); `make check-nodef` covers the
shipping feature set (no devtrig means no synthetic mode). An ARM `make` build is required before
claiming the target still builds. TV acceptance is a device run under the TV lock by the owner:
boot to Home with `0 in CW`, then the two picker paths above.
