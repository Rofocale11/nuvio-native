<!-- upstream-base: d12e865962aaa268658fde469b0d9923610cbd79 -->
<!-- upstream-ref: upstream/feature/v0.9-hq -->
# Nuvio Native — lineage, and the upstream relationship

This repository is the product of three codebases, and confusing them is the single easiest way
to reason wrong about it. Read this before deciding whether an upstream change can be taken.

```
GLinnik21/plx-native          the original project: UI, GPU render path, Starfish/ACB,
        │                     device portability
        ▼
RichiL10/plx-native           the HQ fork this repository builds on: in-app AC3 encode
        │                     lane, Dolby Vision P7→8.1, routing, subtitles
        │
        ▼
   release 0.8.0  acca94a4 — the common ancestor: both HQ lines branch here
        │
        ├── feature/v0.8-hq ───────────────► THIS FORK (Rofocale11/nuvio-native)
        │     tip 0c53dc41, 2026-10-12;        cut from that tip
        │     no longer maintained             + DTS → AC3 in the encode lane
        │                                      + the Nuvio/Stremio backend:
        │                                        loopback PMS shim, addons,
        │                                        account, watch progress
        │
        └── feature/v0.9-hq ───────────────► what this fork ABSORBS FROM
              the same HQ work, rebuilt        contains v0.9.0 and upstream/main
              on top of 0.9.0; maintained       tip 2026-11-01
```

**The direct upstream is RichiL10/plx-native, not GLinnik21/plx-native.** The `upstream` git
remote is RichiL10/plx-native; that repository is itself based on `GLinnik21/plx-native`, the
original project. This fork was cut from RichiL10's `feature/v0.8-hq` tip `0c53dc41` (2026-10-12),
not from the 0.8.0 release. `acca94a4` is where the two HQ lines diverge, and it is the common
ancestor used for distance measurements, not the branch this fork was cut from.

**The fork point is not the current base.** Since the tier-C forward-port the trunk contains
`d12e8659` (Package 11), so the computed merge-base with `v0.9-hq` is now that commit. `0c53dc41`
is where this fork was cut; `d12e8659` is where it re-joined the reference line. For any "what is
left" question, use the computed base (below), not the diagram.

**`v0.8-hq` and `v0.9-hq` are siblings, not parent and child.** Neither contains the other: both
branch from `0.8.0`, and Richi rebuilt the HQ work on top of the 0.9.0 release rather than
fast-forwarding the old branch. The AC3 encode lane therefore exists **twice**, as different
commits: `1bcf30b2` (reachable from `v0.8-hq` and therefore from this trunk) and `8d8a443f`
(reachable only from `v0.9-hq`). Their `ff_enc.rs` files are both 977 lines and differ by **two
lines**, so the work is the same work, arrived at twice.

Two consequences worth knowing before touching any of this:

- **Absorbing `v0.9-hq` is a real port, not a continuation.** The two tips are not descendants of
  each other; they now share `d12e8659`, so taking new work is a merge/port rather than a
  fast-forward.
- **The `ff_enc.rs` conflict is near-noise.** It is the same 977-line file with a 2-line
  difference, so this fork's DTS delta reapplies against essentially unchanged code. The tier-A
  merge is cheap exactly where the fork cares most.

`feature/v0.8-hq` is spent — its tip is `0c53dc41` (2026-10-12) and nothing after it exists. It
is the branch this fork was cut from, not a line to follow.

The fork's own additions (the right-hand annotation) exist nowhere upstream. And the
HQ line is why this fork exists at all: PlxNative could not transcode DTS or TrueHD on the TV's
CPU, and RichiL10/plx-native added that.

## The reference branch is `upstream/feature/v0.9-hq`

"The reference branch" means **what this fork absorbs work from**, which is not the same as the
branch it was **cut from**. The fork point is RichiL10/plx-native's `feature/v0.8-hq` (tip
`0c53dc41`); the target is `feature/v0.9-hq`,
because `v0.8-hq` stopped on 2026-10-12 while `v0.9-hq` is still being maintained and carries the
0.9.0 release on top of the same HQ work.

Not `upstream/main`. Richi's HQ branch **contains** it (verified: the `upstream/main` head
`0b0b2209` is an ancestor of `feature/v0.9-hq`, as is the `v0.9.0` release), so it is a superset
of the origin project's line plus the audio/DV/routing fork. Measuring against `upstream/main`
therefore *understates* what is available — from the common ancestor `acca94a4`, `upstream/main`
has 268 commits and the HQ branch 526 (both measured 2026-11-02); against today's computed base
the HQ line alone is 166 commits ahead — and it points at files the HQ branch has already moved
past.

`feature/v0.9-hq` and `feature/v0.9-p13` are the same commit (`c2138acb`, 2026-11-01). Use the
`-hq` name; `-p13` is an alias at this point, not a separate line. `-p11` (`d12e8659`) is the
package the tier-C port was cut from, and `-p12` (`16fed7e9`) is a sibling of the tip: it shares
the package-12 merge (`fb05279e`) but has three commits the tip does not.

The base is the **merge-base of the trunk with the reference branch**, computed and never
remembered:

<!-- BEGIN docs-fresh:base-stats -->
| | |
|---|---|
| reference | `upstream/feature/v0.9-hq` (`c2138acb`, 2026-11-01) |
| merge-base with `nuvio-native` and `forward-port/v0.9-hq` | `d12e8659` — Package 11: adapt four tests to the fork after the 0.9.0 merge, 2026-10-22 |
| commits on the reference side since the base | 166 |

(Generated by `tools/ai-docs-refresh.sh`; `drift` is the authority when this drifts.)
<!-- END docs-fresh:base-stats -->

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

Measured against the base. These numbers drift; `tools/ai-docs-refresh.sh` rewrites them, and the
command is right when this file disagrees.

<!-- BEGIN docs-fresh:cost-stats -->
| | |
|---|---|
| files this fork changed | 736 |
| files the reference branch changed | 875 |
| **contested** — changed on **both** sides | 311 |
| of those, under `rust-modules/screens` or `rust-modules/media` | 66 |
| upstream-owned source **edited** (`rust-modules/`, `src/`, `Makefile`, `Cargo.*`, `ipkroot/`) | 266 |
| upstream-owned source **added** (the fork's own crates) | 44 |
<!-- END docs-fresh:cost-stats -->

Added files cannot conflict: they did not exist upstream. The
<!-- docs-fresh:edited -->266<!-- /docs-fresh:edited --> edited files are the seam, and
it is concentrated where the Plex coupling had to be cut — `screens`, `media`, `appkit`, `plex`,
`platform`, `data`, `src`. Two entries in the raw churn are not hand-written code and are tagged as
such by the script: `js-service/server.js` (~102k lines, vendored Stremio server) and
`tests/fixtures/replay/*.jsonl` (recorded player sessions).

## Policy

**Richi's HQ line is the source of everything: origin work, audio, Dolby Vision, routing. The
Nuvio backend is ours. There is no third kind of change.**

- A fix that touches the player, decoder, route or UI: take it **from `feature/v0.9-hq`**, not
  `upstream/main` (the HQ branch contains it). If it is one commit and its files are absent from
  `contested`, cherry-pick it (`git cherry-pick -x`).
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
  captures for UI, the television for playback.
  <!-- docs-fresh:contested-screens -->66<!-- /docs-fresh:contested-screens --> of the
  <!-- docs-fresh:contested -->311<!-- /docs-fresh:contested --> contested files are
  `screens`/`media`, so a pick there is a UI or playback change.

### Taking a fix, in order

```sh
git fetch upstream
./tools/upstream-seam.sh drift        # the base, and both sides, from the HQ branch
./tools/upstream-seam.sh contested    # read BEFORE picking: this is the cost estimate
BASE=$(git merge-base HEAD upstream/feature/v0.9-hq)
git log --oneline "$BASE"..upstream/feature/v0.9-hq -- <path>
git cherry-pick -x <sha>
./tools/ai-test.sh                    # inner loop (crate-scoped)
```

A commit whose files never appear in `contested` is a cheap pick. One near the top of `contested`
is where the work is, and deserves "do we even run this code path?" before it deserves a conflict
resolution.

## Forward-port history

The chronological record — the tiers, what each merge and suite found, the television passes and
the defect accounts — is in [`UPSTREAM-LOG.md`](UPSTREAM-LOG.md). Read it before repeating a
mistake; the rules in this file are the conclusions drawn from it.

## Branches

| branch | what it is |
|---|---|
| `nuvio-native` | **the trunk, the product, and the GitHub default branch.** Feature branches merge into it. It carries tier C and the `forward-port/v0.9-hq` line through this commit (the 2026-11-05 fast-forwards: `63825383`, the docs reorg at `229b88e8`/`38b4a268`, and the disk-budget correction at `025a4a2c`/`6123b588`; the 2026-11-02 merge landed at `541a5ad9`). How far the working line is ahead is not stored here — it changes on every commit; ask `git rev-list --count nuvio-native..forward-port/v0.9-hq`, and `git log --oneline` for the list. |
| `origin/nuvio-native` | the pushed trunk. |
| `feature/*`, `agent/*`, `docs/*` | scratch lanes cut from the working line (`forward-port/v0.9-hq`, or the trunk) so an experiment cannot break it. May lag. |
| `main` | **not this project.** It holds upstream-side history and none of the fork's commits. Never cut work from it; never build or ship it. |
| `upstream/feature/v0.9-hq` | Richi's HQ line — the reference for upstream work. Read-only. |
| `forward-port/v0.9-hq` | **the working line since 2026-11-02** (the owner directed new work here): the A+B merge (`656e0cb2`), tier C on top (`5297c0aa`), the port fixes, the actor page (`1f692378`), the two follow-ups (`1443fc03`), the branch-topology docs (`66375ded`) and the poster cap (`7e79e57e`). Fast-forwarded into `nuvio-native` at `541a5ad9` (2026-11-02) and again on 2026-11-05 (`63825383`, `38b4a268`, `6123b588`, and the numbers commit that follows). Still the line the stable `.ipk` is built from; its distance from the trunk is `git rev-list --count nuvio-native..forward-port/v0.9-hq`, and it is not stored here because it changes on every commit. |
| ~~`tier-c/ui`~~ | tier C ported on top of the A+B merge (`741def78`), plus the port fixes through `f41d84a5`. Merged into `forward-port/v0.9-hq` and verified on the television on 2026-11-02, then **deleted** (local and `origin`) on 2026-11-02. `f41d84a5` stays in `forward-port/v0.9-hq`'s history. |

The p11 merge attempts are gone too: `nuvio-native-p11-merge` (a failed merge attempt, deleted
with its worktree on 2026-11-02), plus the stale `docs/upstream-policy` and
`feature-nuvio-test-p11-merge` lanes. `origin/nuvio-v0.9-p11` is the remaining remote p11 line;
`forward-port/v0.9-hq` does not contain it.

Other `origin/*` branches — `ci-metrics`, `drvitality-stremio`, `plx-stremio`, `v0.8-plex-dts`,
`v9-plx-dts` — are experiments/backups from the fork's first weeks (their tips are commits like
`test123` and `bug`). **None is merged into the working line and none is maintained.** Do not cut
from them, build them, or treat them as a source of fixes.

Merge commits into `nuvio-native` are normal here. The origin project's "`main` takes squash
merges only" policy is not this repository's.

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

Run `drift` and `diff` on the trunk as well as the working line: both now contain `d12e8659`, so
their counts agree until the working line moves ahead.
`--ref` measures against another branch (try `upstream/main` to see why it is the wrong one).
`check` compares the marker with `git merge-base HEAD upstream/feature/v0.9-hq`. It passes on both
the trunk and `forward-port/v0.9-hq` while they share the base `d12e8659`; it reports drift on any
branch cut from an older base, which answers "did the fork move, or did this file go stale?".
`check` is deliberately **not** wired into `make check` or CI: a moved base is usually something a
human just did, and it is a question to ask before a pick, not a gate.
