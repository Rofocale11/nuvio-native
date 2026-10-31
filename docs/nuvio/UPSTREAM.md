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
              on top of 0.9.0                  still maintained.  tip 2026-10-22
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
therefore *understates* what is available — 268 commits there versus 360 on the HQ branch — and it
points at files the HQ branch has already moved past.

`feature/v0.9-hq` and `feature/v0.9-p11` are the same commit (`d12e8659`, 2026-10-22). Use the
`-hq` name; `-p11` is an alias at this point, not a separate line.

The base is the **merge-base of the trunk with the reference branch**, computed and never
remembered:

| | |
|---|---|
| reference | `upstream/feature/v0.9-hq` (`d12e8659`, 2026-10-22) |
| merge-base with the trunk | `acca94a4` — `release: 0.8.0`, 2026-10-07 |
| commits on our side since the base | 168 |
| commits on the reference side since the base | 360 |

(The counts were measured before this file was committed; `drift` will report one or two more.)

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

Measured against the base on 2026-10-31. These numbers drift; `tools/upstream-seam.sh` recomputes
them, and the command is right when this file disagrees.

| | |
|---|---|
| files this fork changed | 583 |
| files the reference branch changed | 1261 |
| **contested** — changed on **both** sides | 478 |
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

Target `d12e8659` — the tip of `feature/v0.9-hq` ("Package 11"), the newest commit in Richi's
repository. Merged into the branch on 2026-10-31: **247 conflicts**, resolved in tier order.

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

**Known gaps, not hidden:** the television has not seen this; upstream-only files that the reverted
crates no longer reference (test modules, mostly) still sit in `data/`, `screens/`, `ui/` and
`appkit/` as unreferenced leftovers.

### The exact next action

Playback on the television, under the TV lock. The host suite says the seams line up; it says
nothing about the ACB/Starfish path, the GL composer, or the video plane. The `which-tier` skill
picks the tier; `profile-tv` is what diagnoses a dropped frame. Then `make check` on the committed
tree as the pre-push gate (`./tools/ai-check.sh`, polled with `./tools/ai-check-status.sh`).

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
| `main` | **not this project.** It holds upstream-side history and none of the fork's 168 commits. Never cut work from it; never build or ship it. |
| `upstream/feature/v0.9-hq` | Richi's HQ line — the reference for upstream work. Read-only. |
| `forward-port/v0.9-hq` | the A+B merge of it (`656e0cb2`). Parity work in progress; not shipped, not pushed. |

Merge commits into `nuvio-native` are normal here. `AGENTS.md`'s inherited "`main` takes SQUASH
merges only" is the origin project's policy, not this repository's.

## What would change this policy

Revisit deliberately, in this file, if:

- the HQ line stops being maintained, or diverges into a direction this product does not follow;
- origin (`GLinnik21`) splits from the HQ branch in a way that makes "HQ contains main" false;
- the DTS delta is accepted upstream, removing the fork's only audio divergence;
- the edited-file count falls far enough (say, under ~40) that a merge stops being expensive.

## Recomputing

```sh
./tools/upstream-seam.sh              # drift: base, both sides, contested count
./tools/upstream-seam.sh diff         # the seam by crate, and the edited/added source split
./tools/upstream-seam.sh contested    # the conflict surface, worst first by our own churn
./tools/upstream-seam.sh check        # exit 1 if this file's base marker disagrees with git
```

`--ref` measures against another branch (try `upstream/main` to see why it is the wrong one).
`check` is a **trunk** command: on `forward-port/v0.9-hq` it reports base drift by construction,
because that branch's merge-base with the reference IS the reference tip (`d12e8659`). Reading that
as drift on the forward-port branch is a mistake; reading it as drift on `nuvio-native` is not.
`check` is deliberately **not** wired into `make check` or CI: a moved base is usually something a
human just did, and it is a question to ask before a pick, not a gate.
