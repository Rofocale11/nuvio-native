<!-- upstream-base: acca94a449a5f7db2c662d6501bd88199ce66d4e -->
<!-- upstream-ref: upstream/feature/v0.9-hq -->
# Nuvio Native — lineage, and the upstream relationship

This repository is the product of three codebases, and confusing them is the single easiest way
to reason wrong about it. Read this before deciding whether an upstream change can be taken.

```
GLinnik21/plx-native      origin: UI, GPU render path, Starfish/ACB, device portability
        │
        ▼
  release 0.8.0  acca94a4 — the merge-base of everything below
        │
        ├── feature/v0.8-hq ───────────────► THIS FORK (the trunk, the product)
        │     AC3 encode lane, DV, route,       + DTS -> AC3 in the encode lane
        │     subtitles.  tip 2026-10-12        + the Nuvio/Stremio backend:
        │     no longer maintained                loopback PMS shim, addons,
        │                                          account, watch progress
        │
        └── feature/v0.9-hq ───────────────► what this fork ABSORBS FROM
              the same HQ work, rebuilt        contains v0.9.0 and upstream/main
              on top of 0.9.0                  still maintained.  tip 2026-11-01
```

**`v0.8-hq` and `v0.9-hq` are siblings, not parent and child.** Neither contains the other: both
branch from `0.8.0`, and Richi rebuilt the HQ work on top of the 0.9.0 release rather than
fast-forwarding the old branch. The AC3 encode lane therefore exists **twice**, as different
commits: `1bcf30b2` (reachable from `v0.8-hq` and therefore from this trunk) and `8d8a443f`
(reachable only from `v0.9-hq`). Their `ff_enc.rs` files are both 977 lines and differ by **two
lines**, so the work is the same work, arrived at twice.

Two consequences worth knowing before touching any of this:

- **Absorbing `v0.9-hq` is a real merge, not a continuation.** No ref is a descendant of another,
  which is why the merge produces hundreds of conflicts rather than a fast-forward.
- **The `ff_enc.rs` conflict is near-noise.** It is the same 977-line file with a 2-line
  difference, so this fork's DTS delta reapplies against essentially unchanged code. The tier-A
  merge is cheap exactly where the fork cares most.

`feature/v0.8-hq` is spent — its tip is 2026-10-12 and nothing after it exists. It is the fork
point, not a line to follow.

Everything to the right of `v0.8-hq` is this fork's own work and exists nowhere upstream. And the
HQ branch is why this fork exists at all: PlxNative could not transcode DTS or TrueHD on the TV's
CPU, and Richi's fork added that.

## The reference branch is `upstream/feature/v0.9-hq`

"The reference branch" means **what this fork absorbs work from**, which is not the same as the
branch it was **cut from**. The fork point is `feature/v0.8-hq`; the target is `feature/v0.9-hq`,
because `v0.8-hq` stopped on 2026-10-12 while `v0.9-hq` is still being maintained and carries the
0.9.0 release on top of the same HQ work.

Not `upstream/main`. Richi's HQ branch **contains** it (verified: the `upstream/main` head
`0b0b2209` is an ancestor of `feature/v0.9-hq`, as is the `v0.9.0` release), so it is a superset
of the origin project's line plus the audio/DV/routing fork. Measuring against `upstream/main`
therefore *understates* what is available — 268 commits there versus 526 on the HQ branch — and it
points at files the HQ branch has already moved past.

`feature/v0.9-hq` and `feature/v0.9-p13` are the same commit (`c2138acb`, 2026-11-01). Use the
`-hq` name; `-p13` is an alias at this point, not a separate line. `-p11` (`d12e8659`) is the
package the tier-C port was cut from, and `-p12` (`16fed7e9`) is a sibling of the tip: it shares
the package-12 merge (`fb05279e`) but has three commits the tip does not.

The base is the **merge-base of the trunk with the reference branch**, computed and never
remembered:

| | |
|---|---|
| reference | `upstream/feature/v0.9-hq` (`c2138acb`, 2026-11-01) |
| merge-base with the trunk | `acca94a4` — `release: 0.8.0`, 2026-10-07 |
| commits on our side since the base | 173 |
| commits on the reference side since the base | 526 |

(Measured 2026-11-02; `drift` is the authority when this drifts.)

## What this fork adds on top of Richi's branch

Two deltas, of very different size and very different merge cost:

- **DTS → AC3 in the encode lane.** Three commits on 2026-10-16 (`55402aa3`, `f92e1951`,
  `251dbfe0`) extending `media/src/ff_enc.rs`. It is still unique: the HQ branch's `ff_enc.rs`
  contains no `dts`/`dca` handling at all, even though its *routing* learned to direct-play `dca`
  (`d156a858`). So a forward-port does not make this delta redundant — it has to be reapplied.
  It is also small and self-contained, which makes it a reasonable thing to send upstream to Richi
  so it stops being a delta at all.
- **The Nuvio/Stremio backend** — the shim, the addon protocol, the account and the watch
  progress (`ARCHITECTURE.md`). This is the whole reason the fork cannot merge anything
  whole-cloth: the UI and player still speak the Plex dialect, but they speak it to `plx_pmsshim`
  in-process, and upstream assumes a PMS answered. A change that assumes a server will not apply
  here unchanged.

## What taking upstream work costs

Measured against the base on 2026-11-02. These numbers drift; `tools/upstream-seam.sh` recomputes
them, and the command is right when this file disagrees.

| | |
|---|---|
| files this fork changed | 585 |
| files the reference branch changed | 1581 |
| **contested** — changed on **both** sides | 484 |
| of those, under `rust-modules/screens` or `rust-modules/media` | 119 |
| upstream-owned source **edited** (`rust-modules/`, `src/`, `Makefile`, `Cargo.*`, `ipkroot/`) | 247 |
| upstream-owned source **added** (the fork's own crates) | 78 |

Added files cannot conflict: they did not exist upstream. The 247 edited files are the seam, and
it is concentrated where the Plex coupling had to be cut — `screens`, `media`, `appkit`, `plex`,
`platform`, `data`, `src`. Two entries in the raw churn are not hand-written code and are tagged as
such by the script: `js-service/server.js` (~102k lines, vendored Stremio server) and
`tests/fixtures/replay/*.jsonl` (recorded player sessions).

## Policy

**Richi's HQ line is the source of everything: origin work, audio, Dolby Vision, routing. The
Nuvio backend is ours. There is no third kind of change.**

- A fix that touches the player, decoder, route or UI: take it **from `feature/v0.9-hq`**, against
  `upstream/main`. If it is one commit and its files are absent from `contested`, cherry-pick it
  (`git cherry-pick -x`).
- A fix that touches the **data boundary** — anything that assumes a PMS answered — is a
  re-implementation in the shim's terms, not a pick. Reusing an upstream code path the fork no
  longer runs is debt; matching the behaviour is the goal.
- A batch of upstream work worth a real forward-port (a release, a DV/audio rework) is a
  **project**, not a step: cut a branch, port, verify at the playback tier. The fork has done
  this once already at `feature/v0.8-hq`, and `docs/v0.7.0-forward-port-plan.md` is the origin
  project's record of how they do it.
- **Never** resolve a conflict by deleting fork behaviour to match upstream. If the only way to
  take a fix is to restore a PMS assumption, the fix does not apply here.
- Verify at the tier the change can reach (`which-tier`): host tests for addon/shim code,
  captures for UI, the television for playback. 119 of the 478 contested files are
  `screens`/`media`, so a pick there is a UI or playback change.

### Taking a fix, in order

```sh
git fetch upstream
./tools/upstream-seam.sh drift        # the base, and both sides, from the HQ branch
./tools/upstream-seam.sh contested    # read BEFORE picking: this is the cost estimate
git log --oneline acca94a4..upstream/feature/v0.9-hq -- <path>
git cherry-pick -x <sha>
./tools/ai-test.sh                    # inner loop (crate-scoped)
```

A commit whose files never appear in `contested` is a cheap pick. One near the top of `contested`
is where the work is, and deserves "do we even run this code path?" before it deserves a conflict
resolution.

## Forward-port log

### 2026-10-31 — the direction is set: a tiered forward-port, not a merge and not a PR

The goal is Richi's HQ features **inside this fork**, keeping this fork's own line. Two
consequences that are decisions, not suggestions:

- **The DTS delta stays local.** Its author does not want it sent upstream to Richi, so do not
  propose it, and do not treat "upstream would take this" as a reason to drop or reshape it.
- **Not one heroic merge.** A single `git merge upstream/feature/v0.9-hq` into the trunk hits
  **247 conflicting files** (201 content, 45 add/add, 1 modify/delete), measured with
  `git merge-tree --write-tree nuvio-native upstream/feature/v0.9-hq` on 2026-10-31:

  | area | conflicts |
  |---|---|
  | `rust-modules/screens` | 49 |
  | `rust-modules/ui` | 26 |
  | `rust-modules/media` | 25 |
  | `rust-modules/data` | 25 |
  | `rust-modules/src` | 16 |
  | `tests/fixtures` | 10 |
  | `rust-modules/plex` | 6 |

  The conflicts that carry this fork's reason for existing are the smallest group: about ten
  files under `media/src/route/` plus `media/src/ff_enc.rs`. The largest group is the UI.

### The tiers

Work happens on a scratch branch cut from `nuvio-native`, one merge per tier, and a tier is not
"done" until it is verified at the tier that can actually see it (`which-tier`).

- **A — audio, Dolby Vision, routing.** `media/src/route/*`, `media/src/ff.rs`, `ff_enc.rs` and the
  handful of `src`/`plex` files that route through them. The DTS delta is reapplied on top: Richi's
  `ff_enc.rs` still has no `dts`/`dca` handling, even though his routing learned `dca` direct-play
  in `d156a858`, so both behaviours have to coexist afterwards. Verify: host tests, then playback
  on the television.
- **B — the player/pipeline remainder.** `media/src/player/*`. Television only; a host result says
  nothing about the ACB/Starfish path.
- **C — `screens`, `ui`, `data`, and the replay fixtures.** Largest and lowest conceptual value
  first: it is Richi's UI redesign meeting this fork's shim rewiring. Fixture conflicts are data;
  take upstream's recording when the screen did not change, and re-record when it did.

### Status: A+B merged on `forward-port/v0.9-hq` (`656e0cb2`), host suites green

Target `d12e8659` — then the tip of `feature/v0.9-hq` ("Package 11"), the newest commit in
Richi's repository. Merged into the branch on 2026-10-31: **247 conflicts**, resolved in tier
order.

**A merge cannot be committed half-resolved.** The tiers are an *order of resolution*, not separate
commits: A and B are one commit because git does not let you close a merge with files still
conflicted. The checkpoint that matters is "the merge is in and the workspace compiles".

What landed, and how each was decided:

- **A+B from upstream**: `media` (`route/*`, `ff.rs`, `ff_enc.rs`, `player/*`), `plex`, `platform`,
  `machine`, `base`, `telemetry`. The DTS delta survives — `ff_enc::takes` still accepts `"dts"`
  inside an otherwise identical 977-line file.
- **C stays ours**: `data`, `screens`, `ui`, `appkit`, `gfx` are the trunk's own files again.
  Resolving only the *conflicted* files ours left those crates half-upstream and uncompilable
  (83 errors). A deferred tier must be entirely ours, or it is neither.
- **`src` is ours**, adapted to the new A/B APIs.

The seam is real: upstream's `media` calls `data` and `plex` APIs the fork does not have, so a few
compatibility additions were unavoidable, and each is a dependency rather than a patch:

| where | what, and why |
|---|---|
| `plex` | `rating_source` / `p2p_enabled` / `p2p_server_url` and `RatingSource` moved into upstream's new `Preferences`; accessors read `self.prefs` |
| `plex` | `Retained::with_language` / `language()` — `clear_for_erase` takes `Option<Retained>` now, and a sign-out retry must carry its language |
| `plex` | `MAX_SERVERS` reinstated as a stated constant: the registry is dynamic now, but the fixed arrays below still index by it |
| `data` | `dv_video_codec`, `sub_layout::{SubTraits, sub_traits}`, `viewstate::StopRecord`, `PlayingItem::{kind, hdr}` |
| `media` | `SEEK_STUCK_MS` stays **3000**, not upstream's 1200: a public-internet reopen is two round trips, which is this fork's whole use case |
| `appkit`/`screens` | the HUD's own text-cue draw is retired — upstream moved every text cue into libass, so there is no `active_subtitle` and no `SHARED.sub_cues`; the player screen already paints libass |
| `system`/`port` | the frame-probe prepare hooks exist and do nothing: upstream's prepare-cost span is a dev-trigger diagnostic nothing here reads |

**Verification so far: the host suites run green by crate.** Each measured with
`make test-crate C=<crate>`, which reconstructs the full suite's feature resolution — a bare
`cargo test -p <crate>` runs without `devtriggers` and is not evidence:

| crate | result |
|---|---|
| `plx_platform` | 290/290 |
| `plx_data` | 529/529 |
| `plx_plex` | 726/726 |
| `plx_media` | 1754/1754 |
| `plx_screens` | 1192/1192 |
| `plx_appkit` | 236/236 |
| `plx_telemetry` | 279/279 |
| `plx_ui`, `plx_base`, `plx_machine`, `plx_session`, `plx_gfx` | green |
| `plxnative-modules` (the shell) | green except the two named below |

Two `plxnative-modules` failures are not code: `menu_play_season_tests::a_failed_season_fetch_opens_the_page_instead_of_playing`
needs `127.0.0.1:1` to REFUSE, and `plx_net`'s `a_refused_connection_fails_fast…` needs the same
from port 1; this sandbox holds low ports instead of answering RST (a raw socket times out on 1 and
2, and refuses in 8 ms on 45678), and both fail identically on `nuvio-native`. The television has
not seen any of this yet.

A third red appeared once, under `make check`'s parallel load:
`app::adapters::session::tests::a_signin_after_signout_behind_a_read_only_mount_is_written`
panicked at its `"admitted, not yet run"` assertion. **The test was the defect, not the queue.**
`take_committed` admits the commit to the storage worker and then probes its ticket in the same
breath, so "not yet run" only holds if the scheduler does not run the worker in between — it is a
statement about timing dressed up as a statement about the queue, and 20 ms of load is enough to
make it false. Widening the window with a temporary sleep in `take_committed` fails the old
assertion deterministically and passes the fixed one, which holds the session IO lock across the
probe (the commit's write needs that lock, so the worker is still blocked when the empty ticket is
read). Production code is untouched; the test now grades admission, which is what it claims.

### What running the suites found

Eleven defects only a running test suite could show. Each is fixed; this table is what the next
forward-port should expect:

| where | what the merge had broken |
|---|---|
| `locales` | the resolution replaced this fork's reviewed `be`/`es` strings with English, and refilled the five upstream languages (`de/fr/hu/it/pt`) wholesale, discarding upstream's real translations. Restored by priority trunk → upstream → English fallback. The `translate: false` markers (205 of them, upstream's mechanism) had also been dropped, so the diagnostics panel was translating text that must stay English; restored, and those keys deleted from every translated catalogue, which is the catalogue contract |
| `plex`, `platform`, `telemetry` tests | every DB8 double still spoke upstream's identity (`plxstate.stable`, `com.beb.plxnative.storage`). This fork's manifest generates `plxstate.nuvio` / `plxstate.dbg` / `plxstate.ngt`; the tests now ask `Flavor::object_id()` or the packaged app id instead of hardcoding |
| `src/app/boot.rs`, `bridge.rs` | no per-profile restore existed at all: boot restored nine cells individually from install-wide keys and a profile switch changed nothing, so a per-profile pick would survive neither a switch nor a restart. Boot now seeds `restore_install_rows` + `restore_profile_rows` for `Unseated`, and `SeatWatch` restores at each real seat change (never a republication) |
| `src/dev/framedump.rs` | armed the BLOCKING dump form, which `plx_base::task::assert_may_block` refuses on a frame thread. Now arms `arm_dump_paced` + `dump_frame` + `take_starved`/`take_lagged` like upstream, with `PLXNATIVE_DUMP_LAND_LAG_MS`; `framedump_lag_tests.rs` is attached again |
| `plex/session/prefs.rs` | the fork's `rating_source` / `p2p_enabled` / `p2p_server_url` had no `Setting` descriptors. Described now (`Install` scope, no live cell); `p2p_server_url` is the one `Row`-only row because `Setting::Value` is `Copy` |
| `plex/account/preferences.rs` | upstream's identity proof had been disabled for *every* caller while the Nuvio shim needed it dropped. Exempted only for the synthesized `nuvio-local` identity |
| `net/net.rs` | key mode stopped TCP reuse but let libcurl's TLS session cache resume a session, which presents no certificate, so `keypin::confirm` failed and a seek could not reopen. `CURLOPT_SSL_SESSIONID_CACHE` is off in key mode |
| `telemetry/playback.rs` | `event_body` was the pre-merge serialiser: it omitted the six `playback.attempt_*` tags and `pipeline_error` that its own context and consent preview already carried |
| `appkit` | four `enh_rows_disabled_*` tests all used the same fixture, so all four claimed `NotOriginalQuality`; each now stands for the shape its name says (Auto-HLS, fixed rung, relay, forced) and the `ServerConverting` case exists |
| `media/route/plan_dolby_vision_tests.rs` | a merge hunk had replaced the non-cross-compatible P7 assertions with the convertible case. Restored: `bl_compat == 0` is still refused ("dual-layer"); the cross-compatible conversion is untouched (`T=profile_7`, `T=dts` green) |
| `tests/manifest.json`, `tests/mock_fps.py` | the manifest was overwritten with upstream's scene list — scenes naming `watchlist`/`catalog`/recommended pages this shell cannot emit; removed and the survivors re-derived against the compiled tree. `mock_fps.py` had lost four helper functions but kept their call sites, so `run.py --list --fps` could not start |

The replay fixtures also had to be re-recorded against the current screen-shape census
(`16921795122013909805` → `14134545847820828830`): `tools/plxnative-rec rerecord`,
`SDL_VIDEODRIVER=x11` pacing, all three anchors replaying SAME with every difference counter zero in
both Targets and Resolve. `tests/fixtures/replay/README.md` carries the procedure.

**Known gaps, not hidden:** the television has not seen this. Two trees had to be reconciled
before the gates could run at all:

- **The upstream-only files the reverted crates no longer reference are deleted** (`git rm`, 107 of
  them across `data/`, `screens/`, `ui/`, `appkit/`): a module no `lib.rs`/`mod.rs` declares is not
  compiled, so it could never be covered — but `ci/check-deps.sh` and `ci/check-statics.sh` scan the
  tree, not the module graph, and were reading them as production code (six `thread::spawn`s, eight
  `X-Plex-Container-` literals, an unfrosted `Glass` call). The Watchlist, Catalog, Recommended and
  Collections pages come back with tier C, from `upstream/feature/v0.9-hq`.
- **`ci/allow` is the fork's again, not upstream's.** The merge took upstream's entries, which
  describe an upgraded `ui` (`thread_local!` render caches); this fork's `ui` keeps its process-wide
  caches, so `statics.txt` names them again with their owning thread, plus the fork's own
  `gfx/text.rs` memos, poster key memo and text-input queue. `wall.txt` names `app/boot.rs` (the P2P
  `node` readiness gate waits on a child process starting, not on logical state). `sibling` also
  required moving `item_menu::fallback_anchor` into `registry`, the shared vocabulary both `home`
  and `detail` are allowed to name. Both of the last two were red on `nuvio-native` itself, and the
  heap-statics rule is new from upstream.
- **The CI step manifest is upstream's, and every step in it now says something true about THIS
  tree.** `ci/check-python-steps.txt` came over with the merge (93 added lines, ~17 gates this fork
  never ran); reverting it would have been smaller, but these gates audit the tree, not the
  workflow, and a gate that is not run cannot be argued with later. What that meant, gate by gate:
  the two registries were re-derived (`ci/plex-requests.ini` gained the three Nuvio shim sites —
  they are loopback shim calls, not Plex API requests — and `ci/list-caps.ini` lost 33 sections for
  symbols that no longer exist and gained 34 for sites that do); `ci/third-party-crates.json` now
  classifies `image-webp` and `quick-error` (both in the ARM runtime graph, both elected MIT);
  `THIRD-PARTY-NOTICES.md` was regenerated and §1.1's FFmpeg source rows restored; the
  language-list gate's 16 sites became `i18n::SHIPPED` loops rather than allowlist entries.
  Two gates looked host-dependent and were not:
  - `test_build_gc` / `test_disk_preflight` and the planner behind them read a builder's working
    directory with `lsof`. Linux exposes the same answer at `/proc/<pid>/cwd`, so `tools/build-gc.sh`
    and `tools/disk-budget.py` fall back to `/proc` when `lsof` is absent (macOS has no `/proc` and
    ships `lsof`), and the preflight's safety refusal — "a pretend emergency over a real checkout is
    not run" — now precedes the volume test, so a host whose `/tmp` is its own tmpfs is answered
    with exit 2 instead of an ENOSPC verdict.
  - `test_check_elf`'s 29 failures were the merge keeping trunk's *older* self-test (no VFP lines in
    its synthetic `objdump`, three upstream tests missing) beside upstream's unchanged gate script.
    The upstream file is restored; nothing was relaxed.
  Found on the way and fixed: `ci/build-ffmpeg.sh` had been bumped to FFmpeg 9.0.2 in
  `ci/dependencies.json` while its GitHub fallback pin was still the `n9.0` tag archive, so the
  unreachable-ffmpeg.org path could never verify; the real `n9.0.2` archive was fetched and hashed
  (`6e374ed6…`), the stale `VERSION=`/`SHA256=` literals were removed (the manifest is the one
  authority, and `ci/collect-source-inputs.py` now reads it instead of scraping them), and the
  release notice names the pin.

### What the ARM build found, and the gates it produced

The first GitHub Actions build of this branch failed at the link: `src/main.c` calls
`plx_launch_args`, and the merge resolution had kept `port.rs` from the trunk, dropping upstream's
`#[no_mangle]` export of it. Nothing on this host could see that — `cargo check` never links, the
simulator builds `src/bin/sim.rs` rather than `src/main.c`, and `make check-c-unit` compiles only
the crash tracer and the log shim — so the ARM `make ipk` was the first and only link.

`ci/check-ffi-symbols.py` closes that hole without a toolchain: every `extern` function declaration
in `src/*.c` and `src/*.h` must be defined by one of those C files or exported from `rust-modules/`
by `#[no_mangle]`, `#[unsafe(no_mangle)]` or `#[export_name]`, with test items dropped the way the
other Rust gates drop them. Firmware symbols (`__asm__(…)` labels on the C++) and `extern`
variables are not this repository's to define and are skipped. Its self-test covers each verdict,
and the real tree is one of its cases.

The next run got through the link and failed at PACKAGING, on two merge losses of one shape — a
line BOTH sides edited, where the resolution kept the fork's copy and upstream's addition went
with it:

| where | what was lost | symptom |
|---|---|---|
| `Makefile` `APP_FILES` | `pkg/appfont-emoji.ttf` (upstream added the face; the fork had edited the same line for its brand and its own roots bundle) | `check-package.py`: "payload carries all 31 app files" — the face was staged by nothing, so an emoji in a title would have been a box |
| `pkg/resources/hu/appinfo.json` | the fork's brand and Stremio description (upstream's new Hungarian descriptor still read `PlxNative` + "native Plex client") | `check-package.py`: "hu/appinfo.json title is the untranslated brand" and "staged title == staged top-level title" |

`ci/test_deploy_manifest.py` now asserts that every `pkg/appfont*.ttf` on disk is in both
`APP_FILES` and `DEPLOY_FILES`, which is the cheap host answer to the font half; the descriptor
half stays with `check-package.py`, since it grades a translated string. The lesson for the next
slice is in `git diff upstream/feature/v0.9-hq -- Makefile`-shaped reading, not in a new gate: a
merge that keeps a whole line must keep both sides' edits to it.

### What the first television boot found

The stable `.ipk` from this branch installed and booted (home, 12 Continue Watching rows, the video
plane exported, P2P up). Then sign-in failed, and the log said why:

```
nuvio account: start failed category=network status=0
account: POST 127.0.0.1:32420/api/v2/pins -> HTTP 503
net: request refused: a request that follows redirects carries no caller headers   (52 times)
```

Upstream's `net` refuses a request that asks libcurl to follow a redirect while carrying ANY caller
header — on this firmware libcurl replays custom headers to whatever host a `Location:` names, so an
`Authorization:` on such a request would travel to a stranger. The guard is right. The fork's
`plx_stremio::addon_http` predates it and always passed `follow_redirects = true` with headers (an
`Accept`, plus `Authorization`/`apikey` for the Nuvio backend), so **every addon and Nuvio-account
request was refused before it reached the wire** — no shelves, no sign-in. The host suite could not
see it: nothing there asserts that a request with headers is allowed to exist.

The fix keeps the guard and moves the decision into the transport: `net::Resp` now carries the
answer's `Location`, and `addon_http` follows redirects itself, one hop at a time, with the policy
spelled out as a value (`step`): credentials are dropped when the origin changes, `https` may not
downgrade to `http`, a 303 (and a 301/302 answering a POST) becomes a bodiless GET, only `http(s)`
targets are followed, and five hops bound the chain. A chain that ends in a redirect is returned as
that 3xx answer, never as a success. Tested on the wire against loopback servers (same origin keeps
the headers, another origin does not, a 303 changes the method and drops the body) plus unit tests
for the policy value.

**A second hole, found while wiring that test:** `plx_stremio` (56 lib tests) and `plx_pmsshim` (51)
had no recipe anywhere that ran them — `make check`, the hostsim pass and CI all named the other
fourteen crates — so the addon backend and the shim were the least-gated code in the tree, which is
exactly where a regression like the above could live for a whole merge. Both are in
`check-cargo-unit-default` and `test-fast` now (`UNIT_SUITE`, with `UNIT_SUITE_NO_HOSTSIM` declaring
why the hostsim pass still names fourteen: those two crates do not define that feature).

### What the first playback test found: the merge had removed the fork's audio lanes

The next stable build installed and signed in. Then a TrueHD title did not start at all and a DTS
title loaded to a black screen. The device log named both causes:

```
playing item: rk=10069 audio=5 ...
start_bufferfeed: unsupported audio codec "truehd"; refusing incorrect Load declaration

load: v=H264 a="DTS" ...
smp_cb type=8 ... audio/x-dts ...        <- the ES was parsed
                                          (no `type=7` out-channels, no FRAMEREADY ever)
feed a#200 ... fed=2130000000            <- audio accepted, then the queue stopped draining
```

Both are deltas this fork wrote and the merge overwrote, and neither is a bug in upstream's code —
upstream changed policy on the set it measures on:

| where | what the merge did | why it matters here |
|---|---|---|
| `platform/devcaps.rs` `enc_lane_candidate` | dropped the fork's `"dts" => true` arm, so DTS stopped taking the AC3 encode lane and went out as a native DTS Load | this set accepts the Load and then never configures its audio sink; with `audioSync` the video sink slaves to that dead clock — black screen, ~2 s of audio then nothing |
| `platform/devcaps.rs` `enc_lane_profile_codecs` / `profile_channel_limit_with` | dropped `dts` from the lane's profile codecs and the 8-channel statement | PMS/MDE would have answered "convert with the server" for the tracks the app converts itself |
| `media/player/engine.rs` `AudioLaneInfo::for_track_with` | an unknown sample rate stopped taking the lane (`rate == ENC_LANE_RATE` only; the fork had `rate == 0 \|\|`) | the shim published no `samplingRate`, so **every** track arrived rate-less and TrueHD had no declaration at all. `ff_enc`'s open already refuses a non-48 kHz stream with a named log line, which is where the truth is known |
| `pmsshim/shim_probe.rs` | the probe carried `ProbedAudio::sample_rate` but the shim never published it | the fork lanes read PMS `samplingRate`; without it the AC3 lane cannot decide and the PCM lane cannot state its `pcmInfo` |

Restored: DTS is a lane tenant again (unconditionally, as `docs/nuvio/PLAYBACK.md` has documented
all along — the doc and the code had stopped agreeing), the unknown-rate door is back, and the shim
publishes the probed `samplingRate`. `plex/transcoder.rs` also had two cosmetic merge wounds in the
same profile: `dca` named twice in the direct-play audio list, and the lane's `audio.channels` /
`audio.samplingRate` limitations scoped `scopeName=dts`, which is not a name PMS knows (it spells DTS
`dca`) — both fixed, with the limitation list deduplicated per codec.

`plx_platform` 290/290, `plx_media` 1754/1754, `plx_plex` 726/726, `plx_pmsshim` 53/53 after the fix.

### 2026-11-01 — the second television report: first-try failures and the memory ceiling

The audio lanes were confirmed on the set ("now both works"). Two reports followed, and both are
pre-existing rather than merge damage (`curlio`'s counters are byte-identical at `ac0443bc`):

**A debrid/P2P stream sometimes fails on the first try and plays on the second, and a source switch
mid-playback does the same.** The device log's shape: `ff: open https status=200 clen=73292868747`,
then 39 minutes later `ff: demux ended` with `timeline stopped … ok=1` — no transport error, no
`seek_cb` heal, and a manual retry that plays from the same second. Two mechanisms, one gap: the
transport already heals a cut it *notices* (curl rc 18/55/56/92, or a clean `0` before the declared
length), but nothing compared what arrived against what was declared when libavformat ended on its
own terms, and the reopen budget was 3 attempts with **no wait** — which is exactly wrong for a cold
P2P engine, where the very GET that stopped is what created the engine and the swarm needs seconds
before it can serve. Both are addressed in `docs/nuvio/PLAYBACK.md` ("A body that ends early is a
cut, not the end of the film"): a demux-level recovery on `off < size` plus the container duration
as a second opinion, and immediate-then-backing-off reopens (5 attempts, ~3.75 s) that never hold
teardown hostage. A budget that runs out now raises the transport failure it is instead of a silent
EOS.

**Playback runs the process out of memory mid-film.** The fork had raised the AU queues to
96 MiB / 8 MiB on an LG B4 measurement (2026-09-22) and the poster pool to 32 MiB; on this set that
peak is what dies. Back to the fork's own first sizing — **50 MiB / 4 MiB** — and the poster pool to
**24 MiB** (16 measured as constant re-arming on the simulator: 183 refused arrivals against 0 at
32 MiB in the same 60 s Home session); `abr/sim.rs`'s carried-by-value constants moved with them. The image-subtitle store stays
at 40 MiB on purpose: it is not a cache but the delayed-caption window, with a measured 31.1 MB
worst case at 4K (see `docs/nuvio/PLAYBACK.md`, "Memory: what playback is allowed to hold"). On
2026-11-02 the pool went back to **32 MiB**: 24 could not hold one Home — a simulator session that
hovered the shelves and let hubs land refused 171 arrivals (`lost=65 rearmed=130 refused=171`),
each a tile blank until its cooldown, and the queues' 50 MB cut is what actually fixed the OOM.

### The exact next action

`make check` on the committed tree (start it in the background and poll its log; the
`ai-check` wrappers are gone), then playback on the television under the TV lock. The host suite
says the seams line up; it says nothing about the ACB/Starfish path, the GL composer, or the video
plane. The `which-tier` skill picks the tier; `profile-tv` is what diagnoses a dropped frame.

**The `.ipk` is not built on this workstation**: it holds the ARM webOS NDK toolchain, not a full
ARM target system, so `make ipk` is not the local path. The artifact comes from GitHub Actions —
`.github/workflows/build-ipk.yml` runs on `ubuntu-24.04-arm`, on a `v*` tag or by
`workflow_dispatch` (which takes the branch, so a pushed branch is enough). The build therefore
needs the branch on the remote before there is anything to install; install on the set, then read
the device with `./tools/ai-tv-log.sh` (`perf`, `panic`, `mem`, `grep`) while it plays — never a raw
`ssh` command.

### Then tier C — and it is a PORT, not a merge

Because tier C was resolved *ours*, `feature/v0.9-hq` is already an ancestor of this branch. Merging
it again is a no-op, so tier C cannot be "merge and resolve": it is an explicit take,

```sh
git checkout upstream/feature/v0.9-hq -- rust-modules/{screens,ui,appkit,gfx,data}
```

followed by re-applying the fork's own deltas there, with `docs/nuvio/ARCHITECTURE.md` as the
authority on what the shim needs. Expect the same shape as A/B: take theirs wholesale, then add
back the pieces our layers provide.

If a **future** forward-port turns into a swamp rather than a job of work, abandon that branch —
it is disposable — and cherry-pick the audio/DV/routing commits instead. The candidates, in
`feature/v0.9-hq`:

`134d1244` DV on every video-copy remux / no re-encode for audio / AC3 for optical · `2b4a9177` an
unreported output gets AC3 · `5d446cbe` a direct-play audio pick releases the fork remux ·
`b2d20b58` subtitles keep DV and direct play · `17eb48b9` DV P7 direct-play, converted 8.1 in-app ·
`d156a858` `dca` direct-plays, Force Direct Play DTS gating · `046f3058` DV declaration kept when
the server copies · `3ac9e03f` DV Profile 10 on AV1 · `2f2f5a51` remux declares source frame rate ·
`5648058a` audio stored ahead of video still plays.

`8d8a443f` is **not** on that list: it is the `v0.9-hq` copy of the AC3 encode lane this fork
already has as `1bcf30b2`. Taking it would replace working code with its twin.

## Branches

| branch | what it is |
|---|---|
| `nuvio-native` | **the trunk.** This is the product. Feature branches merge into it. |
| `origin/nuvio-native` | the pushed trunk. |
| `feature/*`, `agent/*`, `docs/*` | scratch lanes cut from the trunk so an experiment cannot break it. May lag the trunk. |
| `main` | **not this project.** It holds upstream-side history and none of the fork's commits. Never cut work from it; never build or ship it. |
| `upstream/feature/v0.9-hq` | Richi's HQ line — the reference for upstream work. Read-only. |
| `forward-port/v0.9-hq` | the integration line: the A+B merge (`656e0cb2`), then tier C merged on top (`5297c0aa`). Pushed; not the trunk. This is the branch the stable `.ipk` is built from. |
| `tier-c/ui` | tier C ported on top of the A+B merge (`741def78`), plus the port fixes through `f41d84a5`. Pushed and merged into `forward-port/v0.9-hq`. The television has not seen the newest fixes (poster pool/workers, subtitle Middle); verify before merging to `nuvio-native`. |

Merge commits into `nuvio-native` are normal here. `AGENTS.md`'s inherited "`main` takes SQUASH
merges only" is the origin project's policy, not this repository's.

## What would change this policy

Revisit deliberately, in this file, if:

- the HQ line stops being maintained, or diverges into a direction this product does not follow;
- origin (`GLinnik21`) splits from the HQ branch in a way that makes "HQ contains main" false;
- the DTS delta is accepted upstream, removing the fork's only audio divergence;
- the edited-file count falls far enough (say, under ~40) that a merge stops being expensive.


### 2026-11-01 — tier C ported in a separate worktree (`tier-c/ui`)

Tier C (`screens`, `ui`, `data`, `appkit`, `gfx`) is ported as a git-checkout port rather than a
merge, on branch `tier-c/ui` (`/home/kungpow/nuvio-tier-c`), so the TV branch keeps building.
The port overlays upstream's tier-C crates on the fork's tree, re-applies the fork's deltas, and
gets the simulator booting the new UI (`make -j10 sim-linux` links and boots, `route=login`, no
panic). Commit `741def78`, pushed as `origin/tier-c/ui`.

Status after the port fixes: the fork's preference persistence is restored (RatingSource,
P2pEnabled, P2pServer write through `Account::write_prefs` and push the P2P policy to the shim),
`super::nuvio::restore_p2p(...)` is back in the boot path, and the post-playback detail
reconciliation is handled by the ported `viewreconcile`/`Scope` machinery rather than the dropped
`DetailRefresh`. The More menu stays the fork's source-lead version by deliberate design: under the
Stremio shim a playback's quality is a property of the stream picked, so the quality ladder
belongs only in Settings > Playback (see `appkit/src/more_menu.rs`'s module doc) — upstream's
quality page would be a second, contradictory answer. The remaining `#[allow(dead_code)]` sites
are upstream's own module-level allowance, not port stubs. `ui/src/value_chip.rs` was removed
(orphaned; `page_header` replaced it). The P2P streaming server, addon backend, and DTS/TrueHD
lanes are untouched (still the fork's). `plx_ui`, `plx_screens`, `plx_appkit` and the full
workspace (`--all-targets`) compile clean; the simulator (`make -j10 sim-linux`) links and boots.
TV verification of the new UI is still outstanding.


### 2026-11-02 — tier C's Watchlist data boundary is My List, not plex.tv

The tier C port had left the Watchlist on upstream's data path: it called
`discover.provider.plex.tv/library/sections/watchlist/all` with the Nuvio token and got HTTP 401,
so the page read "Can't reach your Watchlist" and the list was empty by construction. A guard that
merely disabled the call for `nuvio-local` did not help a real signed-in account. This is the data
boundary `UPSTREAM.md` says must be re-implemented in the shim's terms, so it was:

- **`app::watchlist_io::MyListIo`** replaces `LiveIo` for the Watchlist store. `page` reads the
  shim's `saved_library`; `find` reports the shim as holding every title (so a tile is `Available`
  and opens the shim-backed Detail); `write` calls the shim's save/remove; `released` derives from
  the titles' dates. Nothing in it reaches plex.tv.
- **The portable identity is the shim's numeric ratingKey**, written as `NVIO` + the key in the
  same 12 bytes a discover id uses (`plex::hex_of_rating_key` / `catalog_hex_of_item`). The
  Watchlist row's guid is `plex://movie|show/<that hex>`; `Detail::watchlist_hex` and
  `Metadata::guid_hex` derive the same value, so Home, Detail and the Watchlist share one
  membership answer. See `ID-MANAGEMENT.md`.
- **The shim exposes My List read/write** as `Shim::my_list_entries` / `set_my_list_saved`, sharing
  the `/library/metadata/{rk}` save path (one title, confirmed by a fresh pull).
- **The app's Watchlist action arms were missing** from the port: the item menu's Add/Remove and
  the catalog "go" row produced `Action`s that `apply_item_action` ignored. Restored, and the
  action test now grades them.
- **`AppFx::Watchlist` and `AppFx::Catalog` were stubbed in the bridge.** Every request the page
  itself raises — Sort & Filter, the hold menu, the strip pills — went into a no-op arm, which is
  why OK on the pill did nothing and five app tests failed. They are collected now
  (`watchlist_reqs` / `catalog_effect`), and those five tests pass.
- **`route_word` had no `Watchlist` arm**, so the heartbeat printed `route=` with no name.

Verified on the simulator against a copy of the signed-in session: the page renders My List
posters, a tile opens the shim-backed Detail (`rk=10369`), the Detail's bookmark disc shows the
title's membership, and the Sort & Filter menu opens (`overlay=watchlist_menu`) with the grid
reflowing under it. The television has not seen this yet.

Two further tier-C presentation defects were found on the same simulator and are fixed:

- **The shared strip's capsule froze on the pill the user came from while the Watchlist was on
  top.** The Watchlist wears the bar, but `run::update`'s chrome chain only stepped
  `Bridge::update_home_chrome` on Home, Library and Search, so the capsule's springs never saw the
  Watchlist's selection. The arm covers `AppArg::Watchlist` too now.
- **OK on a Continue Watching tile did nothing under "Open details".** Home asks
  `deck_press().press_plays()` and emits either `Play` (which carries a resume point) or `Detail`.
  The port's `activate_home_item` then passed `home_menu_from_deck` as `activate_card`'s
  `want_play`, turning every deck press into a play attempt — which, for a title with no playable
  stream, silently lost — instead of opening the page. `want_play` now comes from the request
  itself (`resume_ns.is_some()`), and the route-test pump performs the production opener so a World
  test grades that wiring.

Both were verified on the simulator: Movies → Watchlist leaves the capsule on the bookmark, and a
Continue Watching tile opens its Detail page.

**Known gap, not fixed here:** `plx_data`'s suite has four concurrency-shaped failures under
`make test-crate` (`metadata::demo_fixture_tests::…`, `metadata::record::…committed_fixture…`,
`person::…forty_servers…`, `search::account_scope_tests::…`) that are not Watchlist code.

### 2026-11-02 — tier C dropped two preference paths, so picks did not survive a restart

A report that the subtitle size/position picks reset between playbacks reproduced on the simulator
against a copy of the signed-in session: the pick WAS saved (`profile_prefs` held
`"subtitle_size": "extra_large"`), but the next cold boot showed Medium. The port had kept
`SeatWatch` and its documentation while dropping the call that feeds it.

- **`SessionFx::PublishProfile` no longer ran `SeatWatch::arrive`**, so the per-profile player cells
  were never restored at a seat change. Boot restores only the `Unseated` fallback, which is the
  type's default (Medium/Low etc.), so every cold boot reset quality, Direct Play, subtitle
  size/position/tone, HDR overlay, next-episode mode, skip interval, deck press, under-posters,
  audio enhancements and trailer autoplay to their defaults. The arm restores again, computing the
  seat from the publication itself.
- **`app::playback::commit_track` discarded every `memo`**, calling the plain
  `commit_audio_selection`/`commit_subtitle_selection`, and its `SubtitleForcedOnly` arm was empty.
  The route writes the per-show track memory only through `commit_audio_pick` /
  `commit_subtitle_and_sidecar` / `commit_forced_only_and_sidecar`, so the player menu's audio and
  subtitle picks (including Off) were never saved, and Forced only did nothing. `commit_track` now
  hands each pick to its remembering commit; a source-pin test grades the three arms.

Verified on the simulator: Extra Large picked in Settings > Playback survives a cold boot. The
hostsim regression `restore_characterisation_tests::stored_boot_restores_the_seated_profiles_own_values`
was RED (all cells at defaults) and is green with the seat restore. A stale `player_requests` call
in `run.rs`'s hostsim tests was adapted so that suite compiles again; its
`play_automatically_publishes_direct_play_auto_once` failure is a separate pre-existing
double-publish and is not this work.

### 2026-11-02 — the reference branch moved 166 commits past the ported tip

`git fetch upstream` on 2026-11-02 moved `feature/v0.9-hq` from `d12e8659` (Package 11, the tip
the tier-C port was cut from) to `c2138acb` (Package 13, 2026-11-01). The new span is **166
commits / 875 files**; against the base the fork and the reference now change **585 / 1581**
files with **484 contested**. The tier-C branch already contains `d12e8659`, so this is not
"merge and resolve": taking it is a port against the already-ported UI. Measured with
`git merge-tree --write-tree tier-c/ui upstream/feature/v0.9-hq`, it is **87 conflicted files**
(`src` 13, `screens` 10, replay fixtures 8, `site/media` 6, `media` 5, `plex` 4, `data` 4,
`telemetry` 2, workflows 2).

What is in it, by theme:

- **HUD plugins** — a large `appkit`/`ui` rework: a package format with a strict parser, a
  layout compiler (classic compiles to the hand-written geometry), a package store, a JSON
  schema and lint, `--hud-lint`/`--hud-schema`/`--hud-export` simulator flags, and one shared
  command performer. BACK now dismisses a Skip/Up Next offer and restores the discs.
- **Library** — the A-Z rail (pure band model, PMS band-count probe, data job, drawing and
  navigation for value and date bands), sort value under posters and a header sort control,
  and the Watchlist's sort button becoming the Library's control. That last one is a data
  boundary here: it must read My List through the shim, not plex.tv.
- **Playback** — a per-profile Dolby Vision Auto/Off setting (`007696c0`), an H.264 non-IDR
  I-slice treated as a keyframe so Blu-ray remux seeks stop stalling (`da8424b7`), and leaving
  for the next episode marking the finished one watched (`0e5cb47f`).
- **Input and text** — Magic Remote edge scrolling and sideways row glide (`36e5ffea`), Arabic
  and Hebrew RTL paragraphs (`411f6d33`), and Dutch as a UI language (`11f714dc`).
- **Presentation fixes** — the top bar reading its ground from the band image and keeping its
  glass at strip width, a page opened over a closing menu leaving the host uncovered on Back,
  and poster eviction ordering.

`feature/v0.9-p12` is not on this line: it shares the package-12 merge (`fb05279e`) with the tip
but has three commits of its own that `c2138acb` does not (an encode-lane borrow, a `DvCapability`
remux test, and multichannel FLAC/PCM tests). Treat `-hq` as the reference, and check a p12
commit against it before taking it.

The cheap picks against the fork's playback tier are `da8424b7` and `007696c0`; the HUD rework
and the A-Z rail are a project, not a pick. The television still has not seen tier C, so verify
the ported UI on the set first, then take this span as one tier-C-shaped port — or cherry-pick
the playback fixes while the UI is being verified.

### 2026-11-02 — the first tier C television pass: four defects and their fixes

The stable `forward-port/v0.9-hq` build (`da56aa21`) ran on the set. Boot, Home, Library,
Watchlist, Detail and playback worked, P2P worked, and the subtitle size/position preference
survived a cold boot. Four defects did not:

- **The Detail/Home bookmark disc never learned membership.** `frame_ingest` fed only the
  Watchlist page (`watchlist_feed`); `catalog_feed`, which feeds every open Detail and Home
  disc, existed but was called from tests alone. A title on the list therefore still offered
  Add, and the page could never issue its Remove. `frame_ingest` calls it after
  `watchlist_feed` now; the route tests no longer feed it themselves, and were RED
  (`Unknown` vs `Off`) without the call.
- **A Remove after the metadata cache had gone failed with 404.** `save_my_list_title`
  re-resolved the full add-on `Meta` before deleting; that cache is in memory with a six-hour
  TTL, so an Add made from a Detail page worked and a Remove hours later did not. A removal
  now needs only the account's own `(type, id)` (`Shim::write_meta`); an Add still needs the
  metadata (or its TMDB fallback).
- **The player HUD lost the clearLogo.** The tier C port replaced the HUD's `draw_title_logo`
  calls with the text-only `draw_player_title`, leaving the logo path with no caller. The HUD
  now takes the logo path when `logo_src` is resident (clipped to the title's control row) and
  falls back to the clipped/marquee text otherwise.
- **The Home hero's scrim snapped off at the `hero_a > 0.01` gate.** `base_scrim_bottom_a`
  had a 0.30 floor while the ramp was gated off at 0.01, so the bottom third dropped in one
  frame mid-dive. The foot now scales with `hero_a` and reaches zero with the photo; Detail's
  ramp never had a floor.

The simulator confirmed the Detail disc's filled face (a checkmark) for a My List title and
the new Home. The television has not seen these fixes yet, and a Remove was not exercised on
the simulator because that would write to the account.

**The poster pool was too small, not the art source.** The first reading (one Watchlist tile with
no poster, backdrop or logo, `lost=0, refused=16`) looked like a source gap, and one real latch was
fixed with it: a `/photo` request for a metadata row that had not landed returned 404, which the
poster store treats as final; it returns 503 (retryable) now, while a row that exists without art
still returns 404. A longer simulator Home session then settled it the other way —
`posters: residency lost=65 rearmed=130 refused=171` with no fetch or decode failure in the log.
Every refusal is an arrival the render cache dropped because it would have evicted art drawn in
the current or last frame; on a screen whose own art exceeds the pool (hero + two prefetched
neighbours ≈ 11 MB, plus the visible grid), the retry is refused again, so the tile stays blank
for as long as the screen is up. The pool is back to the measured zero-refusal point,
**32 MiB** (`ui::tex::TEX_RESIDENT_BYTES_MAX`); the OOM was the 96/8 MiB AU queues, which stay at
50/4. The remaining blank-until-late tiles were first-load latency, not loss: a cold row of 20
images took 450–600 ms each through the fixed two poster workers and drew last at +4.0 s. From
the same imgtrace, `POSTER_WORKERS` is now 4; that row finishes at +1.2 s, and the decoded-byte
admission cap (8 MiB) still bounds the batch. All of it is simulator evidence: the pool size,
the worker count and the cold-row timing have not been exercised on the set.

**The Middle subtitle position sits lower.** `position_lift` lifted Middle 160 px above Low; it is
60 px now (`media/src/player/text_style.rs`), the pick the viewer already saves is unchanged, and
the two assertions that pinned the old lift (`text_style`, `text_ass`) moved with it.

Slow-torrent seeks still stutter. This build predates `da8424b7` (an H.264 non-IDR I-slice as
a keyframe) and the P2P engine's own buffering dominates; that is a candidate for the next
upstream pick, not a defect found here.

**Full-suite state on this branch:** `make test-crate C=plxnative-modules` reports 831 passed,
18 failed. The isolated failure is a stale tier-C expectation — `library_query_tests` expects
`LibraryMenuKind::Filter` while the tier-C Library opens `SortFilter` — and the rest are
Library route/recorder tests plus the known sandbox `menu_play_season` port refusal. None are
in the files these fixes touch; they are a separate triage, not evidence against these fixes.

## Recomputing

```sh
./tools/upstream-seam.sh              # drift: base, both sides, contested count
./tools/upstream-seam.sh diff         # the seam by crate, and the edited/added source split
./tools/upstream-seam.sh contested    # the conflict surface, worst first by our own churn
./tools/upstream-seam.sh check        # exit 1 if this file's base marker disagrees with git
```

Run `drift` and `diff` on the trunk as well: on a port branch (`tier-c/ui`,
`forward-port/v0.9-hq`) the computed base is the older reference tip the branch contains, so
their counts describe the port rather than the trunk.
`--ref` measures against another branch (try `upstream/main` to see why it is the wrong one).
`check` is a **trunk** command: on a branch that already contains the reference tip it was
ported from (`forward-port/v0.9-hq`, or `tier-c/ui` at `d12e8659`), it reports base drift by
construction, because the computed merge-base is that older tip rather than the trunk's. Reading
that as drift on the port branch is a mistake; reading it as drift on `nuvio-native` is not.
`check` is deliberately **not** wired into `make check` or CI: a moved base is usually something a
human just did, and it is a question to ask before a pick, not a gate.
