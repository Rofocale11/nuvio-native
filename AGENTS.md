# Project instructions

## Project

Nuvio Native is a native LG webOS client for Stremio/Nuvio addons: it browses and plays
content from Stremio addons (Cinemeta, Torrentio, AIOStreams, …) and the Nuvio account,
keeping PlxNative's GPU-rendered UI and native Starfish/ACB video pipeline. It is a fork
of PlxNative (GPL-3.0-or-later, upstream `GLinnik21/plx-native@4e454ea`).

Most of the application is Rust under `rust-modules/` (the application in `src/`, the
layers in their own crates); `src/main.c` is the boot/crash shim, `src/starfish.c` is the
StarfishMediaAPIs/ACB seam, and `src/svg.c` rasterizes SVGs. Keep changes properly
factored and finished; "only a demo" is never a reason to leave a shortcut behind.

The target is a cross-compiled 32-bit ARM application with a hardware video plane. Host
tests and the macOS simulator are valuable, but they cannot prove every device behavior.
App ids: `com.nuvionative.app.debug` and `com.nuvionative.app`, title "Nuvio Native".

The fork's design authority is `docs/nuvio/`: `ARCHITECTURE.md`, `ID-MANAGEMENT.md`,
`TMDB.md`, `PLAYBACK.md`, `WATCH-PROGRESS.md`. Read the relevant one before changing that
area. Upstream PlxNative docs in `docs/` may describe Plex-only behavior and do not always
apply to the addon backend.

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

- **Trunk-based development, and `main` takes SQUASH merges only.** Work happens on
  short-lived branches or worktrees cut from `main`. Commit as often as is useful on a
  branch. When verified, it lands on `main` as ONE squash commit, never as a fast-forward
  of a working branch's history and never as a merge commit. Push `main` only when the
  user asked for it.
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
  Never launch `make check` in the foreground with a short tool timeout.
- To run host unit tests, ALWAYS use `./tools/ai-check.sh`. It runs in the background to avoid terminal timeouts.
- After starting the check, poll `./tools/ai-check-status.sh` every 30-60 seconds until it prints the test results.
- Do NOT try to run `make check` directly in the foreground.
- While iterating on one crate, `make test-crate C=plx_ui [T=filter] [DEPS=1]` builds and
  runs only that crate's lib tests. It is a loop, not a gate.
- `make test-crate` does NOT enable `hostsim`: tests under
  `#[cfg(all(test, feature = "hostsim"))]` run as `cd rust-modules && CARGO_INCREMENTAL=1
  CARGO_TARGET_DIR=/tmp/<dir> PLXNATIVE_RUNTIME_DIR=$(mktemp -d) cargo +nightly test --lib
  -p plxnative-modules --features hostsim -- <filter>`. Keep that target dir OUTSIDE the
  checkout.
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

5. **Search All:** `./tools/ai-tv-log.sh grep "pattern"` — Searches the main events
   log, the P2P log, AND the stderr log simultaneously for specific patterns.

6. **Raw (Unfiltered):** `./tools/ai-tv-log.sh raw 200` — Shows EVERYTHING in the
   Rust log, including key presses. Use this only if you suspect a crash right
   after a button press.

7. **STRICTLY FORBIDDEN:** NEVER run raw `ssh` commands. NEVER use `nv-tv.py`.
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
- **`github` (Repo Management):** Use the `github` MCP to read PRs, check CI action logs, or look up issues. Do not use the `gh` CLI via bash.
- **`filesystem` (File Edits):** Use the `filesystem` MCP to read and write files. Always show the diff before applying changes.
- **`websearch`:** Use the remote `websearch` MCP when you need up-to-date crate documentation (e.g., checking if `reqwest` or `serde` changed their API recently).

### The `ultrawork` Trigger

When the user prefixes a prompt with `ultrawork:` or `ulw:`, you must immediately transition into an orchestrator role. Break the task down, assign it to subagents, use `rust-coder` to verify the code compiles, and do not stop until the tests pass.
