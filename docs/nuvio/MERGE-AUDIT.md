# Nuvio Native upstream merge audit — current merge

A fresh `MERGE-AUDIT.md` is written for every upstream merge. The previous audit
is appended to `MERGE-AUDIT-HISTORY.md` under a heading naming its exact commit
range and kept there for reference only. This file records only the merge
currently in progress.

**Audit range:** `0a1edc61f` (previous upstream merge point, 2026-11-21) →
`efc05f025` (upstream head, 2026-11-26): 39 upstream commits, 455 files changed,
24 conflicting files. Merge into `nuvio-native` on `merge/upstream-2026-11-26`.
Product-branch landing commit: `e618494cc72818e0de23f7700e2ce4d712646a6c`.

When rotating this audit into history, keep the heading
`## 0a1edc61f..efc05f025 — 2026-11-21..2026-11-26` and add the landing commit.

**Status: conflicts resolved, gates in progress, not committed.** All 24 conflicts
are resolved and staged. Host verification is running; no device claim is made here.

## Proof certificates — durable fix for the recurring conflict

`ci/proofs/platform-generated-enums.json`, `ci/proofs/serde-json-immutable.json`,
`ci/readonly_generated.py` and `ci/readonly_dependencies.py` are byte-for-byte
upstream, so they cannot conflict on a future merge. Taking upstream's blobs
alone fails `ci/test_readonly_generated.py` (10 failures) and
`ci/test_readonly_dependencies.py` (11): the certificates enumerate workspace
crate manifests and `Cargo.lock`, which necessarily differ on this fork. The
fork's reviewed certificates therefore live in the fork-owned `ci/fork_proofs/`,
with regeneration and pin instructions in `ci/fork_proofs/pins.py`. The two
fork-owned test files route upstream's modules to those pins before running:
both suites pass (26/26 and 68/68), as do `ci/test_ci_split.py` and
`ci/test_step_scope.py`. One regeneration was required for the merged workspace
(the lockfile and member set changed); future merges regenerate only the
fork-owned copies, with no conflict to resolve.

## Mechanical resolutions (both sides kept where they coexist)

- `Makefile`, `ci/test_ci_split.py`, `ci/test_test_fast.py`: package lists keep
  the fork's `plx_nuvio_probe` / `plx_nuvio_store` and add upstream's
  `plx_platform_tizen`.
- `ci/test_no_host_staticlib.py`: fork's rlib-only crate superset, three
  platform crates, and `platform-tizen` in the throwaway-workspace source list.
- `.github/workflows/nightly.yml`, `ci/nightly.py`: the fork's
  `com.nuvionative.app.nightly` manifest id is kept — upstream's id names a
  different install the TV would refuse — while upstream's Samsung constants
  are retained and its Tizen assets are not published by this webOS-only fork.
- `THIRD-PARTY-NOTICES.md`: the fork's FFmpeg 9.0.2 row (including
  `libswresample-plx.so.7`) and Mozilla-CA / embedded-header attributions are
  kept; upstream's 9.1 row cannot describe libraries this fork ships.
- `ci/list-caps.ini`: fork ledger preserved, upstream's Tizen classifications
  appended because that platform crate is now in the workspace.
- `tests/fixtures/replay/ALPHABET.json`: `livetv` added to the fork's source
  vocabulary. `rust-modules/tests/host_interface.rs`: the fork's
  `"Nuvio Native"` title kept with upstream's platform-stated placement API.

## Semantic resolutions

- `rust-modules/src/app/boot.rs`: took upstream's platform-owned `Shell::window(...)`
  plan; the fork overrides only the sim window title to `"Nuvio Native"` (the webOS
  platform already states the device's `plxnative` process name). Also completed
  upstream's `hostsim`→`sim` feature rename in this file's remaining cfgs.
- `rust-modules/src/app/run.rs`: kept the fork's `stop_p2p_service()` and switched
  the duplicated metric/teardown block to upstream's extracted
  `end_playback_for_exit` (which also handles a parked URI original).
- `rust-modules/appkit/src/more_menu.rs`: took upstream's `quality_withheld` as the
  single Quality rule and kept the fork's `nuvio_rev`, so a Nuvio source change still
  rebuilds the Streams panel in place.
- `rust-modules/base/src/paths.rs`: kept the fork's app id in `LEGACY_APP_DIR` and
  upstream's migration-only semantics; kept the fork's P2P-state test alongside
  upstream's `use std::path::PathBuf`.
- `rust-modules/media/src/route/plan.rs`: kept the fork's single ledgered `client_for`
  read and shim `/decision` first, added upstream's Live-TV branch (never armed on
  Nuvio content), kept the fork's single `uri_decline_line` and added upstream's two
  no-backend log helpers.
- `rust-modules/plex/src/plex/session.rs`: kept the fork's top-level `rating_source`
  key in the canonical DB8 round-trip and upstream's updated flattened-preference
  doc (`recent_channels`).
- `rust-modules/src/app/input.rs`: kept both the fork's `miss_label` / Home-Resume
  wait and upstream's devtrigger `api` field / `for_api` constructor.
- `rust-modules/screens/src/registry.rs` and `rust-modules/src/app/recorder.rs`:
  kept both shape inventories, used upstream's frozen `prior_vector` iterator for
  historical identities, and re-pinned the combined tree:
  `SCREEN_SHAPES_PIN = 0xe13e_5eec_6a32_71ff`, `ARMS = 52`, current V13 producer
  census `0x69ea_58c7_af97_cbe2`. Replay fixtures still need `plxnative-rec rerecord`
  on a macOS simulator.
- `pkg/splash.png`: kept the fork's Nuvio-branded binary; upstream's pure-black OLED
  field is not applicable to this fork's splash or the owner's LCD test set.

## Regressions found while building the merged tree

- Upstream's new Croatian catalog lacked the fork's 33 brand-overlay keys: added
  `hr` values in the fork-owned `nuvio-locales/brand.json` (English fallback, the
  file's existing convention), and removed the stale `library_header.collections`
  key from `locales/hr/library_header.json` — this fork ships Catalogs, not
  Collections. Neither locale JSON can carry a marker, so both are recorded here.
- Upstream added `carry` to `ff::HlsNet` for Live TV; the fork-owned trailer DASH
  demux in `play-feed/src/dash.rs` now initialises it to `None`.
- Upstream's Croatian strings overflowed two fork UI columns after brand
  substitution: added short `hr` overrides for `widgets.menu.quality_converted_note`
  and `settings.language.restart_hint` in `nuvio-locales/brand.json`.
- The webOS platform's `INSTALL_DIR` still named `com.beb.plxnative`, so its test
  disagreed with the fork's migration source: changed to `com.nuvionative.app`.
- Upstream's Live-TV disabled-mode test expects `build_live_stream` to own the
  refused conversion; the fork's earlier library-item refusal now skips a live
  tune so that branch runs.
- Upstream's nightly-workflow test requires the Samsung asset hook: the fork keeps
  its Nuvio manifest id but retains the (empty on webOS) `samsung_assets` array.
- The dependency checker requires notices to match `ci/dependencies.json`: the
  FFmpeg row now says 9.1 while keeping the fork's `libswresample-plx.so.7`.
- Machine-shape proofs also consume the certificates: `ci/machine_readonly.py`
  routes to the fork-owned pins with a marked edit, so upstream's proof files stay
  byte-identical and every consumer proves the fork tree.

## Gate results on merged tree (uncommitted)

- `make check`: **151 steps — all ok** (`753` harness tests not shown in this line;
  cargo suites all green after the fixes above).
- `make check-nodef`: clean; shipping feature set compiles.
- ARM cross-build `make -j4`: links successfully with the webOS NDK.
- These gates ran on the working tree before a commit. They are evidence for this
  tree, not for a later commit; rerun after committing before pushing.

## Required after all conflicts are resolved

Static `Fork Rofocale11:` marker audit on the diff; host gates and
`make check-nodef`; an ARM cross-build; then the owner-run synthetic TV boot
(`plxnative-nuviofake`, read the identity line) and Streams-picker acceptance.
Replay fingerprints changed by this merge need `tools/plxnative-rec rerecord`
on a macOS simulator before `make check-replay` is trusted.
