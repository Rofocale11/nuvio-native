# Project instructions

## Project

Nuvio Native is a native LG webOS client for Stremio/Nuvio addons: it browses and plays
content from Stremio addons (Cinemeta, Torrentio, AIOStreams, …) and the Nuvio account,
keeping PlxNative's GPU-rendered UI and native Starfish/ACB video pipeline. It is a fork
of PlxNative (GPL-3.0-or-later). The recorded base, the policy for taking upstream work,
and the measured size of the divergence are `docs/nuvio/UPSTREAM.md` — do not restate a
base commit from memory here.

Most of the application is Rust under `rust-modules/` (the application in `src/`, the
layers in their own crates); `src/main.c` is the boot/crash shim, `src/starfish.c` is the
StarfishMediaAPIs/ACB seam, and `src/svg.c` rasterizes SVGs. Keep changes properly
factored and finished; "only a demo" is never a reason to leave a shortcut behind.

The target is a cross-compiled 32-bit ARM application with a hardware video plane. Host
tests and the macOS simulator are valuable, but they cannot prove every device behavior.
App ids: `com.nuvionative.app.debug` and `com.nuvionative.app`, title "Nuvio Native".

The fork's design authority is `docs/nuvio/`: `ARCHITECTURE.md`, `ID-MANAGEMENT.md`,
`TMDB.md`, `PLAYBACK.md`, `WATCH-PROGRESS.md`, and `UPSTREAM.md` (which upstream work to
take, and how). Read the relevant one before changing that area. Upstream PlxNative docs in
`docs/` may describe Plex-only behavior and do not always apply to the addon backend.

## Load the right context

- `docs/agent-reference.md` is the detailed architecture, build, portability, and
  verification reference. Read the relevant section before changing a subsystem; do not
  load the whole file when a narrower section is enough.
- Before addon/backend work, read `docs/nuvio/ARCHITECTURE.md` and the `stremio` /
  `pmsshim` crate docs.
- Before playback work, read `docs/nuvio/PLAYBACK.md` and
  `rust-modules/media/src/player/CLAUDE.md`.
- Before Plex data-layer or shim HTTP-dialect work, read
  `rust-modules/plex/src/plex/CLAUDE.md`, `docs/pms-api.md`,
  `docs/nuvio/ID-MANAGEMENT.md` and `docs/nuvio/TMDB.md`.
- Before UI work, read `rust-modules/ui/src/CLAUDE.md` and use the shared theme, layout,
  and widget systems instead of adding screen-local visual primitives. Before touching a
  screen, also read `rust-modules/screens/src/CLAUDE.md`; the restructure's design record
  is `docs/ui-restructure-spec-v4.md`, which `rust-modules/ui/src/lib.rs` cites by section
  number.
- Before account/watch-progress work, read `docs/nuvio/WATCH-PROGRESS.md`.
- Before taking any upstream PlxNative work — cherry-picking a fix, absorbing a Richi branch, or
  reasoning about how far this fork has drifted — read `docs/nuvio/UPSTREAM.md` and run
  `tools/upstream-seam.sh`. The reference branch is `upstream/feature/v0.9-hq`, which is a
  superset of `upstream/main`, and the measured merge cost is in that file.
- Repository skills live in `.agents/skills/`. Use the matching skill whenever its
  description fits the task; the TV, simulator, FFI, release, and verification workflows
  have non-obvious constraints that generic commands miss.

## Fork constraints that shape the code

- **Pure Rust only — no C libraries or JS engines in the TV build.** The app compiles for
  an ancient LG toolchain (`arm-webos-linux-gnueabi`). Crates such as `innertube-rs`,
  `rusty_ytdl`, `rquickjs` or `boa_engine` pull C code or need `libclang`/`bindgen`, and
  the ARM build fails on them. The bundled FFmpeg is built `--disable-everything` plus the
  demuxers/decoders the player actually uses; adding one means a change to
  `ci/build-ffmpeg.sh` and the buffer-feed pump. The one in-app audio decode/re-encode is
  the fork's AC3 encode lane (`media/src/ff_enc.rs`). The P2P server is an external `node`
  child process, not an embedded JS engine.
- **`ratingKey` must be numeric.** PlxNative parses it as an `i64`; a string such as
  `"tmdb:603"` blanks the screen. `docs/nuvio/ID-MANAGEMENT.md` is how the shim keeps that
  promise (key table + synthetic bases).
- **No native YouTube trailers.** In-app trailer playback was abandoned. A detail row may
  carry a trailer thumbnail, but `/library/metadata/{rk}/trailer` is not a route the shim
  answers. Do not re-add extraction.
- **The shim is a loopback PMS.** One in-process HTTP server (`plx_pmsshim`, default
  `127.0.0.1:32420`) speaking the Plex dialect, in the same process as the UI. The front
  end registers it as its one server.
- **Account preferences live in one file, with one writer.** `defaultAudioLanguage`,
  `defaultSubtitleLanguage`, `autoSelectSubtitle` and `defaultSubtitleForced` sit under
  `preferences` in `nuvio-session.json`. `Account::write_prefs` is the only writer; playback
  reads through `account::local_preferences()`.
- **My List and watch progress use Nuvio RPCs.** Writes are one title + active profile, never
  whole-list replacement; watch progress is pushed throttled and pulled/merged on boot or
  profile switch. See `docs/nuvio/WATCH-PROGRESS.md`.
- **The P2P streaming server is external.** Stremio's `server.js` runs as a direct child of
  the native app from `js-service/`; it is spawned on a live boot and killed on shutdown.
  A missing interpreter/server/bind failure is logged and leaves the app usable.

## Crates

| crate | owner | depends on |
|---|---|---|
| `rust-modules/base` (`plx_base`) | leaf utilities | — |
| `rust-modules/machine` (`plx_machine`) | state-machine runtime | `plx_base` |
| `rust-modules/platform` (`plx_platform`) | webOS identity, storage, codecs, artwork, i18n | `plx_base`, `plx_machine` |
| `rust-modules/gfx` (`plx_gfx`) | GL/EGL, text, images, icons | `plx_base`, `plx_machine`, `plx_platform` |
| `rust-modules/net` (`plx_net`) | libcurl HTTPS + plaintext stream | `plx_base`, `plx_platform` |
| `rust-modules/plex` (`plx_plex`) | typed Plex API, session store | `plx_base`, `plx_machine`, `plx_platform`, `plx_net` |
| `rust-modules/stremio` (`plx_stremio`) | Stremio addon protocol, Nuvio account/import, classify, torrents | `plx_base`, `plx_net` |
| `rust-modules/pmsshim` (`plx_pmsshim`) | loopback PMS dialect over `plx_stremio` | `plx_base`, `plx_net`, `plx_platform`, `plx_stremio` |
| `rust-modules/telemetry` (`plx_telemetry`) | consent, crash/usage channels | `plx_base`, `plx_machine`, `plx_platform`, `plx_net`, `plx_plex` |
| `rust-modules/data` (`plx_data`) | data layer / stores | see `ci/module-layers.ini` |
| `rust-modules/session` (`plx_session`) | session layer | see `ci/module-layers.ini` |
| `rust-modules/media` (`plx_media`) | media pipeline / player | see `ci/module-layers.ini` |
| `rust-modules/appkit` (`plx_appkit`) | shared widgets (player HUD, track/source menus) | see `ci/module-layers.ini` |
| `rust-modules/screens` (`plx_screens`) | owned screens | `ui` and data crates; never `app` |
| `rust-modules/storage` (`plxnative-storage`) | private storage helper | isolated package |
| `rust-modules/ui` (`plx_ui`) | shared UI system / design tokens | see `ci/module-layers.ini` |

`ci/module-layers.ini` is the authority for allowed references; `ci/check-module-layers.py`
fails the build on a forbidden edge.

## Working rules

- **Trunk-based development on `nuvio-native`.** `nuvio-native` is the trunk and the product;
  work happens on short-lived branches or worktrees cut from it, and lands back on it. Merge
  commits and squash commits are both acceptable there. `main` is NOT the trunk: it holds
  upstream-side history and none of the fork's commits — never cut work from it, never build or
  ship it. The upstream relationship (base commit, freeze-and-cherry-pick policy, what a merge
  would cost) is `docs/nuvio/UPSTREAM.md`, and `tools/upstream-seam.sh` recomputes it.
  Push anything only when the user asked for it.
- **A squash onto a MOVING `main` silently reverts it.** Before pushing any squash, run
  `git diff --stat HEAD^ HEAD` and confirm every path is one the branch meant to touch.
  Prefer squashing onto the same base the picks were made on and rebasing. `git fetch -q`
  can fail silently, so check `git log HEAD..origin/main` after fetching.
- **A diff stat is not a measure of change.** Size a change by content, not by lines.
- **New module references only point down, and only the webOS port names webOS.** Fix the
  reference (`docs/module-layers.md`) instead of adding a (file, member) pair to
  `ci/allow/layers.txt`, which is empty and only shrinks.
- Preserve unrelated user changes and generated artifacts. Never clean or reset a dirty
  tree to make a task easier.
- This repository is public. Never publish values from gitignored private files such as
  `.tv-host`, `.tv-mac`, `src/config.local.h`, `tests/manifest.local.json`,
  `pkg/auth.json`, `pkg/lab.json`, or the other paths enumerated by
  `.claude/hooks/outbound-guard.py` — including `~/.config/plxnative/tv-host` and
  `tv-mac`. Use the documented placeholders.
- webOS native behavior is under-documented. When an answer depends on platform behavior,
  verify it from primary documentation, vendored source, firmware inventories, or the
  device binaries; do not infer behavior from a symbol name or from a webOS web-app
  example.
- Fixing a reported bug starts with a regression test or reproducible artifact that fails
  against the broken behavior. Observe the failure, implement the fix, then observe it pass.
- Do not use `make -p` or `make -pn`; recursive variables such as `TV` are misleading
  there. Use the `make -s print-*` query targets documented in `docs/agent-reference.md`.

## Build and verification

- **A gate result is evidence about the exact tree it ran on.** Gates run on the COMMITTED
  tree: commit, confirm `git status --short` is empty, run the gates, confirm it is empty
  again, then push. Report the `test result:` line itself rather than a summary.
- `make check` runs the fast host unit suite and lint gate. Use it for ordinary Rust
  changes. It is bounded machine-wide to TWO concurrent runs via `tools/check-lock.py`.
- **Inner Loop (Fast):** to see a change behave, build and run the simulator:
  `make -j10 sim-linux` (it needs the host libav*/libass staged under `pkg/`). It exercises
  real rendering and linking, which is what a widget, layout or player change needs. For pure
  logic use the cheapest loop that reaches the code: `make test-crate C=<crate> [T=filter]`
  for one crate, `./tools/ai-test.sh` for the crates your diff touches.
- **Outer Loop (Gate):** `make check` is the pre-push gate — the host unit suite (16 test
  binaries), lint, and the `ci/` python/shell gates. It takes ~5 minutes and is bounded
  machine-wide to two runs, so start it in the background (`nohup ... &` or the agent's own
  background execution) and poll its log; do not hold a short foreground timeout over it.
  Nothing else runs that suite, and a push to a feature branch starts no CI job either, so a
  change that never runs it is a change nobody checked.
- While iterating on one crate, `make test-crate C=plx_ui [T=filter] [DEPS=1]` builds and
  runs only that crate's lib tests. It is a loop, not a gate.
- `make test-crate` does NOT enable `hostsim`. Those tests run through
  `make test-hostsim
  [C=<crate>] [T=filter]`, which builds into the gitignored `rust-modules/target-hostsim`
  (inside the checkout, so `make disk`'s sweep and `tools/build-gc.sh` own it) and gives each
  run a fresh `PLXNATIVE_RUNTIME_DIR`. Spell the crate as for `test-crate` — `C=plx_media`,
  `C=media`, several allowed — and pass `--require-match`: a filter that matches no test
  fails the run instead of reporting "0 tests" as success. Do not hand-write a cargo
  invocation with a target dir outside the checkout.
- `make sim-macos` / `make` fail with `make: cargo: No such file` from a non-login shell:
  prefix `PATH="$HOME/.cargo/bin:$PATH"`. Test builds deny warnings, so a new `App` field
  must be read (`dead-code`) and initialised at BOTH construction sites (`app/boot.rs` and
  the `app/run.rs` tests).
- A PR that touches `Makefile`, `Cargo.toml`, `Cargo.lock`, `build.rs`, `.cargo/` config,
  `.github/workflows/` or the module layout pastes a before/after `make build-bench` table
  in its body.
- `make` performs the ARM cross-build. Do not assume a host-only green result proves the
  target still builds.
- A plain local `make` / `make deploy` compiles with the fast `tvdev` cargo profile (no
  LTO); CI, `RELEASE=1`, `SYMBOLS=1` and `FLAVOR=stable|nightly` keep the fat-LTO
  `release` profile. Timing runs enforce `ARM_PROFILE=release`.
- After editing `rust-modules/src/**/*.rs` or a layer crate's `rust-modules/*/src/**/*.rs`,
  also check the shipping feature set with `CARGO_INCREMENTAL=0 cargo +nightly check
  --manifest-path rust-modules/Cargo.toml --lib --no-default-features` when the
  Claude-only release hook did not run. Keep the `CARGO_INCREMENTAL=0` prefix on every
  direct `cargo` call.
- **`make disk` before and after a fleet.** `tools/build-gc.sh` deletes only rebuildable
  output.
- Use the `which-tier` skill to choose between host checks, `ui-sim`, and real-device
  verification. Pixel output, LG text rasterization, video-plane composition, performance,
  and native playback generally need the TV before being called verified.
- **A frame drop is diagnosed with the tools that point at it, not guessed at.** Any late
  frame, fps dip or stutter on the TV goes straight to the `profile-tv` skill.
- FFI, linkage, `dynlib!`, Starfish, ACB, curl, or bundled-FFmpeg changes require the
  `fw-compat-reviewer` custom agent (or equivalent manual review) before push.
- After behavior changes, use the `doc-claim-auditor` custom agent (or perform the same
  audit) to find prose that the change made false.
- Always update the docs after you finish
## 🔍 Offline Debugging Tools

These run entirely on the host (x86_64) with no TV, simulator, or ARM toolchain.

- **Module layers:** `./tools/ai-layers.sh` — runs `python3 ci/check-module-layers.py` and prints
  "Layer check PASSED" or the forbidden edges. Run after touching module references.
- **Addon prober:** `python3 tools/probe-addon.py ...` — queries Stremio addons and TMDB directly:
  - `manifest <url>` — fetches the manifest, prints catalogs/resources/types.
  - `meta <url> <type> <id>` — prints title, year, genres, cast (e.g. Cinemeta).
  - `streams <url> <type> <id>` — lists stream names, qualities, and sizes (e.g. Torrentio).
  - `tmdb <type> <id>` — fetches TMDB using `TMDB_API_KEY` from `.env` or the environment,
    printing title, genres, and overview. Set the key in `.env` (repo root) or export it.
- **Memory snapshot (on device):** `./tools/ai-tv-log.sh mem` — reads `VmRSS`/`VmHWM`/`VmSwap`
  for the app process (read-only, requires TV lock for device access).
- **Change-scoped tests:** `./tools/ai-test.sh` — runs `cargo nextest` only for crates touched
  by uncommitted changes (inner loop); `make check` is the outer-loop gate.
- **Hostsim tests:** `./tools/ai-test-hostsim.sh [filter]` wraps `make test-hostsim` (same
  `C=`/`T=` spelling, same in-checkout `rust-modules/target-hostsim`): the `cfg(hostsim)` lib
  tests, with a fresh `PLXNATIVE_RUNTIME_DIR` per run.
- **Perf summary:** `./tools/ai-perf-summary.sh` — pulls TV perf logs and prints a one-paragraph
  summary (min FPS, carried/dropped frames, highest vgap, worst route).
- **Simulator:** `./tools/ai-sim.sh` — builds (`make -j10 sim-linux`) and launches the x86_64
  Linux simulator with the standard env vars (`PLXNATIVE_APP_DIR`, `PLXNATIVE_RUNTIME_DIR`).
  The one local path that exercises real rendering without the TV or ARM toolchain.

## Television and release safety

- There is one physical television. Before any command that deploys, runs, drives, captures,
  or tests it, use the `tv-lock` skill and acquire the repository TV lock. Prefer `ui-sim`
  when the device is unnecessary or busy. Read-only status/log collection is the documented
  exception.
- **Never write to a real person's Plex/Nuvio account.** Boot with `--guest` and verify the
  identity line in the log before sending any key; never press `ok` on a card whose
  `ratingKey` you have not read from the log; reopen a screen by relaunching with
  `--screen` rather than walking Home. Seeding, scrobbling, unscrobbling, marking watched,
  "Remove from Deck", My List writes, and watch-progress pushes are all writes to a
  household's actual viewing record — the managed and Guest profiles are real users too.
  When screenshot or demo work needs particular content, mock the server.
- Hold the TV lock around the device commands themselves and release as soon as the device
  work pauses.
- The tracked default is `FLAVOR=debug`. Any stable install, package, deploy, or release
  action must name `FLAVOR=stable` explicitly and follow the `cut-release` skill. Do not
  bypass the stable guard or the TV lock unless the user explicitly directs the exceptional
  action.
- `RELEASE=1` must be present on every make invocation that builds or ships the release
  artifact; it is not persistent between commands.
- Developer-mode tooling: `tools/nv-tv.py` (install/launch/keys/log/capture without root).
  Host simulator: `make -j10 sim-linux` (Linux libav*/libass staged in `pkg/`) or
  `make sim-macos` on macOS. TV build: `make ipk`. A release is cut by pushing a tag
  (`git tag -a v0.8.1 -m "…" && git push origin v0.8.1`); CI builds the `.ipk`.

## 📺 STRICT TV LOGGING RULES (READ-ONLY)

The user's TV is rooted and must never be put at risk by automated actions. The
following rules are absolute and override any other instruction in this file.

1. **Default (Rust App):** `./tools/ai-tv-log.sh 200` — Shows the last 200 lines
   of the main app log, hiding the `key type=hex` spam. Use this for general
   debugging and layout issues.

2. **P2P Server (Node.js):** `./tools/ai-tv-log.sh p2p 200` — Shows the last 200
   lines of the Node P2P server log. Use this to debug torrent playback, Torrentio
   failures, or stream timeouts.

3. **Performance:** `./tools/ai-tv-log.sh perf` — Shows only frame rates, image
   cache, and layout routes. Use this to debug FPS drops.

4. **Panics & Crashes:** `./tools/ai-tv-log.sh panic` — Reads both
   `plxnative-crash.log` and `plxnative-stderr.log`. ALWAYS run this immediately if
   the app closes unexpectedly or freezes.

5. **Memory Snapshot:** `./tools/ai-tv-log.sh mem` — Finds the app's PID via
   `pgrep -f "com.nuvionative.app"` and prints only `VmRSS`, `VmHWM` (peak), and
   `VmSwap` from `/proc/<PID>/status`. Read-only; use it to verify RAM usage and
   the measured peak (155,292 KiB) on the device.

6. **Search All:** `./tools/ai-tv-log.sh grep "pattern"` — Searches the main events
   log, the P2P log, AND the stderr log simultaneously for specific patterns.

7. **Raw (Unfiltered):** `./tools/ai-tv-log.sh raw 200` — Shows EVERYTHING in the
   Rust log, including key presses. Use this only if you suspect a crash right
   after a button press.

8. **STRICTLY FORBIDDEN:** NEVER run raw `ssh` commands. NEVER use `nv-tv.py`.
   NEVER run commands that write to the TV, install packages, or modify system
   files. You are restricted to the `ai-tv-log.sh` script only.

## 🚫 NO WORKSPACE-WIDE FORMATTING

Do NOT run `cargo fmt` or `cargo fix` across the entire workspace. It creates massive,
noisy git diffs that ruin history. If you need to format, only format the specific
files you are actively editing (e.g. `cargo fmt -- path/to/file.rs`).

## Harness-specific files

- `.agents/skills/` and `.agents/agents/` are canonical shared content.
- `.claude/skills` and `.claude/agents/*.md` are compatibility symlinks. Claude-only hooks
  and settings remain under `.claude/`.
- `.codex/agents/*.toml` are thin Codex wrappers around the canonical prompts. They inherit
  the parent model and reasoning effort unless a future task deliberately changes that
  policy.

## 🧠 Tooling, MCPs & Orchestration

You are equipped with advanced orchestration capabilities and specialized MCP servers. You must use them.

- **Subagents:** When a task requires researching external documentation, auditing multiple files, or checking GitHub issues simultaneously, do NOT do it sequentially. Spawn subagents (e.g., `Oracle` for architecture, `Explore` for codebase search, `Librarian` for web docs) to run in parallel.
- **`rust-coder` (Semantic Navigation):** Do NOT use `grep` or `cat` to find code definitions. You MUST use the `rust-coder` MCP tools:
  - Use `symbols` to list functions in a file.
  - Use `definition` to find where a struct or function is declared.
  - Use `references` to find all usages of a symbol before refactoring.
  - Use `hierarchy` or `implementations` to map out traits.
- **Search discipline (STRICT):** NEVER use `grep` to search the codebase. For Rust symbols
  (functions, structs, variables, traits) you MUST use the `rust-coder` MCP (`symbols`,
  `definition`, `references`). For plain text search, use `rg` (ripgrep), which respects
  `.gitignore` and skips `target/` instantly — e.g. `rg -n "playback error" rust-modules/`
  instead of `grep -rn ...`. The `grep` tool is reserved for the rare case ripgrep cannot cover.
- **`github` (Repo Management):** Use the `github` MCP to read PRs, check CI action logs, or look up issues. Do not use the `gh` CLI via bash.
- **`filesystem` (File Edits):** Use the `filesystem` MCP to read and write files. Always show the diff before applying changes.
- **`websearch`:** Use the remote `websearch` MCP when you need up-to-date crate documentation (e.g., checking if `reqwest` or `serde` changed their API recently).

### The `ultrawork` Trigger

When the user prefixes a prompt with `ultrawork:` or `ulw:`, you must immediately transition into an orchestrator role. Break the task down, assign it to subagents, use `rust-coder` to verify the code compiles, and do not stop until the tests pass.
