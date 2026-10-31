<!-- upstream-base: acca94a449a5f7db2c662d6501bd88199ce66d4e -->
<!-- upstream-ref: upstream/feature/v0.9-hq -->
# Nuvio Native — lineage, and the upstream relationship

This repository is the product of three codebases, and confusing them is the single easiest way
to reason wrong about it. Read this before deciding whether an upstream change can be taken.

```
GLinnik21/plx-native          the origin project: UI, GPU render path, Starfish/ACB playback,
        │                     the device/codec portability work
        ▼
RichiL10/plx-native           "HQ" fork: the in-app AC3 encode lane (FLAC/PCM above 2 ch, TrueHD),
 feature/v0.9-hq              Dolby Vision P7 -> 8.1 in-app, routing and subtitle fixes
        │  << this fork forked HERE (feature/v0.8-hq is an ancestor of the trunk)
        ▼
Rofocale11/nuvio-native       + DTS -> AC3 in the encode lane            (this fork)
  = nuvio-native (trunk)      + a full Nuvio/Stremio backend: the loopback PMS shim,
                              addon protocol, account, watch progress
```

Nothing in the third block exists upstream, and the second block is why this fork exists at all:
PlxNative could not transcode DTS or TrueHD on the TV's CPU, and Richi's fork added that.

## The reference branch is `upstream/feature/v0.9-hq`

Not `upstream/main`. Richi's HQ branch **contains** it (verified: the `upstream/main` head
`0b0b2209` is an ancestor of `feature/v0.9-hq`, as is the `v0.9.0` release), so it is a superset
of both lineages: origin work plus the audio/DV/routing fork. Measuring
against `upstream/main` therefore *understates* what is available — 268 commits there versus 360
on the HQ branch — and it points at files the HQ branch has already moved past.

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

**Status: not started.** Nothing has been merged from `feature/v0.9-hq`; the trunk's only
relationship to it today is the computed base above.

## Branches

| branch | what it is |
|---|---|
| `nuvio-native` | **the trunk.** This is the product. Feature branches merge into it. |
| `origin/nuvio-native` | the pushed trunk. |
| `feature/*`, `agent/*`, `docs/*` | scratch lanes cut from the trunk so an experiment cannot break it. May lag the trunk. |
| `main` | **not this project.** It holds upstream-side history and none of the fork's 168 commits. Never cut work from it; never build or ship it. |
| `upstream/feature/v0.9-hq` | Richi's HQ line — the reference for upstream work. Read-only. |

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
`check` is deliberately **not** wired into `make check` or CI: a moved base is usually something a
human just did, and it is a question to ask before a pick, not a gate.
