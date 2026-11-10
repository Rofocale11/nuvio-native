# Project instructions

## Project

Nuvio Native is a native LG webOS client for Stremio/Nuvio addons: it browses and plays content
from Stremio addons (Cinemeta, Torrentio, AIOStreams, …) and the Nuvio account, keeping
PlxNative's GPU-rendered UI and native Starfish/ACB video pipeline.

It is a fork of RichiL10/plx-native's `feature/v0.8-hq`, itself a fork of PlxNative
(`GLinnik21/plx-native`, GPL-3.0-or-later). The recorded base, the policy for taking upstream
work and the measured divergence are in `docs/nuvio/UPSTREAM.md`; the design authority is
`docs/nuvio/` (`FEATURE.md`, `ARCHITECTURE.md`, `PLAYBACK.md`, `ID-MANAGEMENT.md`, `TMDB.md`,
`WATCH-PROGRESS.md`, `DETAIL-LATENCY.md`). `docs/README.md` indexes every document and says
which describe this product and which are upstream references.

**It is not a Plex client.** There is no plex.tv credential, and its only server is the
in-process shim. The UI still speaks the Plex HTTP dialect, but the account, library and watch
progress are Nuvio's. An upstream path that reaches plex.tv or a real PMS is a data-boundary
bug: re-implement it against the shim, never "fix" it with a credential.

App ids: `com.nuvionative.app.debug` and `com.nuvionative.app`, title "Nuvio Native". The
target is a cross-compiled 32-bit ARM application with a hardware video plane; host tests and
the desktop simulator are valuable but cannot prove every device behaviour.

## Working rules

- **Commit and push require the owner's explicit confirmation — every time.** Finishing a task
  does not authorize either. When the work is ready, stop and ask, and wait for a clear yes that
  names the action (commit, push, or both). This applies to every branch, including the trunk and
  the working line, and to docs-only changes; approval from an earlier task never carries over.
- **Trunk-based development on `nuvio-native`, with `forward-port/v0.9-hq` as the working line.**
  `nuvio-native` is the trunk, the product and the repository's default branch. Short-lived
  branches and worktrees are cut from the working line (the stable `.ipk` line) and land back
  on it; the trunk is fast-forwarded only when the owner asks. Merge and squash commits are both
  acceptable. `main` is **not** this project: it holds upstream-side history and none of the
  fork's commits — never cut work from it, never build or ship it. How far the working line is
  ahead is a question for git, not a number kept here: `git rev-list --count
  nuvio-native..forward-port/v0.9-hq` (and `git log --oneline` for the list).
- **Derived numbers are generated, not hand-written — and only the ones that can be stored.**
  `./tools/ai-docs-refresh.sh` rewrites the seam counts (files changed, contested, upstream-owned
  edited/added, the reference and merge-base SHAs) in `docs/nuvio/UPSTREAM.md`; its `check` form
  runs in `make check` and CI. Counts that increment on **every commit** are deliberately not
  stored — a stored count that the next commit must rewrite turns every commit into a docs commit
  and every commit made outside the hook (a GitHub rebase merge, an amend, a bot) into a follow-up
  one. Store what moves when the *shape* of the fork changes; ask git for the rest.
  **A commit does not require you to run it.** `.githooks/pre-commit` runs `update --pending` and
  stages both files with every commit, so one commit carries the change *and* any number it
  implies. Enable it once per repository: `git config core.hooksPath .githooks` (worktrees share
  that setting). The only number a single commit can move is a file it adds to one of the seam
  counts. Do not run the update form by hand before a commit, do not add a second docs-only commit
  for it, never pass `--no-verify`, and never hand-edit a marked number. The hook deliberately
  **aborts** if you have unstaged edits in those two files (staging the whole file would sweep your
  prose into the commit): `git add` them and run the commit again. It skips during a rebase.
  `PLX_DOCS_REFRESH=0` is the general escape hatch, and `check` now prints the exact line that
  disagrees instead of telling you to regenerate and hope.
- **A bug fix starts with a failing test or reproducible artifact.** Observe the failure,
  implement the fix, then observe it pass.
- **Preserve unrelated changes and generated artifacts.** Never clean or reset a dirty tree to
  make a task easier.
- **No workspace-wide `cargo fmt` or `cargo fix`.** Format only the files you are editing.
- **A gate result is evidence about the exact tree it ran on — so gate ONCE, before the commit,
  and never again after it.** `make check` is not a step an agent takes on its own initiative: it
  runs when the owner asks for a commit. Run it on the working tree; if it passes, commit, push,
  and stop. A second full run after the commit is not allowed — it is the same suite over the same
  code, and the only thing the commit can add is the generated numbers `.githooks/pre-commit`
  stages, which `./tools/ai-docs-refresh.sh check` validates in seconds (`make check` runs that
  same check). After the push, confirm `git status --short` is empty and report the `test result:`
  line itself rather than a summary. Run the suite again only if the owner asks, or if the commit
  changed CODE after the run that graded it (an amend that edits source).

## The fork's constraints

The detail and the reasoning are in `docs/nuvio/ARCHITECTURE.md` §Constraints. These are the
rules an agent must not break:

- **No new C libraries or JS engines in the TV build.** The fixed exceptions are the bundled
  FFmpeg and the C shims (`src/main.c` boot/crash, `src/starfish.c` Starfish/ACB, `src/svg.c`).
  The ARM toolchain is ancient; crates that pull C code or need `bindgen` fail on it.
- **`ratingKey` must be numeric.** PlxNative parses it as an `i64`; a string such as
  `"tmdb:603"` blanks the screen. `docs/nuvio/ID-MANAGEMENT.md` is how the shim keeps that.
- **No native YouTube trailers.** `/library/metadata/{rk}/trailer` is the shim's own
  trailer-metadata route; never re-add a native YouTube extractor.
- **The shim is a loopback PMS.** One in-process `plx_pmsshim` (default `127.0.0.1:32420`)
  speaking the Plex dialect over the addon backend; the front end registers it as its one server.
- **Account preferences have one writer** (`Account::write_prefs`); playback reads through
  `account::local_preferences()`.
- **My List and watch progress are Nuvio RPCs**: one title and the active profile per write,
  never whole-list replacement. See `docs/nuvio/WATCH-PROGRESS.md`.
- **The P2P streaming server is either the staged C engine (`libsbengine.so`, dlopen'd, never
  linked) or the external `node` child from `js-service/`** — never an embedded JS engine. The
  C engine is preferred when it loads (graded OK on every webOS release; it links libstdc++ and
  OpenSSL statically); when it starts, the Node child is not spawned at all, and when it cannot
  load the Node child is the path. Both a missing `.so` and a missing interpreter are logged and
  leave the app usable. See `docs/nuvio/ARCHITECTURE.md`.

## Documentation

**A behaviour change lands in the same commit as its owning document.** If the change makes a
sentence false, fixing that sentence is part of the change, not a follow-up. After a change that
can falsify prose, run the `doc-claim-auditor` agent (or perform the same audit). The owning
document for each area:

| area | owning document |
|---|---|
| what the fork adds, user-visible features | `docs/nuvio/FEATURE.md` |
| system shape, constraints, crates | `docs/nuvio/ARCHITECTURE.md` |
| upstream work, branch topology, policy | `docs/nuvio/UPSTREAM.md` (history: `UPSTREAM-LOG.md`) |
| playback, audio lanes, memory | `docs/nuvio/PLAYBACK.md` |
| Plex IDs, key table, synthetic bases | `docs/nuvio/ID-MANAGEMENT.md` |
| TMDB enrichment | `docs/nuvio/TMDB.md` |
| account, My List, watch progress | `docs/nuvio/WATCH-PROGRESS.md` |
| detail-page latency, prefetch, caching | `docs/nuvio/DETAIL-LATENCY.md` |
| build, deploy, portability, test tiers | `docs/agent-reference.md` |
| UI system conventions | `rust-modules/ui/src/CLAUDE.md` |
| screen conventions | `rust-modules/screens/src/CLAUDE.md` |
| player conventions | `rust-modules/media/src/player/CLAUDE.md` |
| Plex data layer / shim HTTP dialect | `rust-modules/plex/src/plex/CLAUDE.md`, `docs/pms-api.md` |
| agent workflow, tools, context map | `docs/nuvio/AGENT-GUIDE.md` |

Upstream PlxNative documents in `docs/` may describe Plex-only behaviour. `docs/README.md` marks
which are current and which are upstream references; do not treat an upstream page as authority
for this product.

## Build and verification

- **Simulator:** `./tools/ai-sim.sh` builds (`make -j10 sim-linux`) and launches the app with
  the standard environment; it needs a graphical session (`DISPLAY`, e.g. WSLg). `make -j10
  sim-linux` alone builds and links it.
- **Inner loop:** `./tools/ai-test.sh` runs nextest for the crates the diff touches. From
  `rust-modules/`, one crate:
  `PATH="$HOME/.cargo/bin:$PATH" CARGO_TARGET_DIR=target-fast PLXNATIVE_RUNTIME_DIR="$(mktemp -d)"
  cargo nextest run -p <crate> [filter]`. Spell the package name (`plx_pmsshim`,
  `plxnative-modules` for the application).
- **Feature parity for a layer crate:** `make test-crate C=<crate> [T=filter] [DEPS=1]`.
- **`cfg(hostsim)` tests:** `make test-hostsim [C=<crate>] [T=filter]` builds into the
  gitignored `rust-modules/target-hostsim` with a fresh runtime dir. Example:
  `make test-hostsim C=plx_media T=<filter>`. Do not hand-write a cargo invocation with a target
  dir outside the checkout.
- **Outer loop:** `make check` (the host unit suite, lint and the `ci/` gates, ~4 minutes) is the
  gate for a commit the owner asked for — once, on the working tree, before that commit. It is
  never a step an agent adds on its own initiative, and never repeated after the commit: see the
  gate-order rule above.
- **ARM caveat:** `make`, `make deploy` and `make ipk` need the webOS NDK. A host-only green
  result proves nothing about the target; never report an ARM result you did not run. The release
  artifact is built by CI (`.github/workflows/build-ipk.yml`).
- After editing `rust-modules/**/*.rs`, also run
  `CARGO_INCREMENTAL=0 cargo +nightly check --manifest-path rust-modules/Cargo.toml --lib
  --no-default-features` (the shipping feature set).
- Test builds deny warnings: a new `App` field must be read and initialised at both construction
  sites (`app/boot.rs` and the `app/run.rs` tests).
- Use `which-tier` to choose host / simulator / TV verification, and `profile-tv` for frame
  drops. FFI, linkage, Starfish, ACB, curl or bundled-FFmpeg changes need the
  `fw-compat-reviewer` review before push.
- A PR touching `Makefile`, `Cargo.toml`, `Cargo.lock`, `build.rs`, `.cargo/` config,
  `.github/workflows/` or the module layout pastes a before/after `make build-bench` table.

## Television and release safety

- There is one physical television. Before any command that deploys, runs, drives, captures or
  tests it, acquire the repository TV lock with the `tv-lock` skill. Prefer `ui-sim` when the
  device is unnecessary. Read-only log collection is the documented exception. Hold the lock
  around the device commands themselves and release it as soon as the device work pauses.
- **Never write to a real person's Plex/Nuvio account.** Boot with `--guest` and verify the
  identity line in the log before sending any key; never press `ok` on a card whose `ratingKey`
  you have not read from the log; reopen a screen by relaunching with `--screen` rather than
  walking Home. Seeding, scrobbling, unscrobbling, marking watched, "Remove from Deck", My List
  writes and watch-progress pushes are all writes to a household's viewing record. When demo or
  screenshot work needs particular content, mock the server.
- **Device access goes through the skills, never raw commands.** The only sanctioned path is
  `tv-lock` plus `tv-session` / `wake-tv`; reads use `./tools/ai-tv-log.sh`. **Never** run raw
  `ssh`, **never** use `tools/nv-tv.py`, **never** write outside the app's own directory, and
  **never** install a stable package, change permissions or modify system files. The television
  is rooted: a wrong write can brick it. If a step would do any of that, stop and ask the owner.
- `./tools/ai-tv-log.sh` modes: `200` (main log), `p2p 200`, `perf`, `panic`, `mem`,
  `grep "pattern"`, `raw 200`.
- The tracked default is `FLAVOR=debug`. Any stable install, package, deploy or release action
  must name `FLAVOR=stable` explicitly and follow the `cut-release` skill. `RELEASE=1` must be
  present on every make invocation that builds or ships the release artifact; it is not
  persistent between commands.
- This repository is public. Never publish values from gitignored private files such as
  `.tv-host`, `.tv-mac`, `src/config.local.h`, `tests/manifest.local.json`, `pkg/auth.json`,
  `pkg/lab.json`, `~/.config/plxnative/tv-host` and `tv-mac`. Use the documented placeholders.

## Harness-specific files

- `.agents/skills/` and `.agents/agents/` are the canonical shared content.
- `.claude/skills` and `.claude/agents/*.md` are compatibility symlinks; Claude-only hooks and
  settings remain under `.claude/`.
- `.codex/agents/*.toml` are thin wrappers around the canonical prompts and inherit the parent
  model and reasoning effort unless a task deliberately changes that policy.
- `CLAUDE.md` imports this file; edit this one, not the copy.
