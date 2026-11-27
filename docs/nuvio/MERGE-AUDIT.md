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

## Fork Rofocale11: Stage 1 scroll-hitch contained wins (2026-11-28)

Three behaviour-identical reductions, all in upstream-owned files; each edit carries a
`Fork Rofocale11:` marker at the site. They were deployed only to the debug install for an
A/B against a clean HEAD build; no stable install or release was touched. That A/B used the
gate's default blanked panel and is not a smoothness result — see `VALIDATION.md`.

- `rust-modules/src/app/adapters/poster.rs`: the two/three-worker tier is replaced by
  `POSTER_WORKERS = 4` on every set (core and RAM readings remain in the boot log only),
  restoring the old fork's measured cold-row floor. The tiered `workers_for` test is
  replaced by one pinning the constant.
- `rust-modules/ui/src/detail_layout.rs`: new shared `BACKDROP_W`/`BACKDROP_H` (1280×720)
  constants, the common detail vocabulary rather than a screen private to Detail —
  `check-deps`' sibling gate forbids one screen naming another.
- `rust-modules/screens/src/detail/mod.rs`: the backdrop request uses those constants
  instead of literals, so Home cannot probe a different key from the one this page
  draws. Same pixels — `Rect::FULL.cover` scales either box to the same rect.
- `rust-modules/screens/src/home/mod.rs`: a focus dwell that fires the shim metadata
  warm now also probes the exact Detail backdrop key; `ScreenEvent::Enter` re-probes the
  selected hero's backdrop before the fade-in. Both use `resolve_tex_wh_on` (demand),
  never `warm_tex_on`, and share Detail's constants. One test pins the probe key.

Deliberately NOT ported: the old fork's frame-scoped art-resolve memo. This tree already
has `KEY_MEMO`, one hash per `lookup` probe, `card_motion` sharing the arrival answer
across walks, and `is_recording()` culling the discovery walk; skipping the remaining
`lookup` would drop its LRU/draw touches and can change which art survives eviction,
i.e. the picture, so it is not behaviour-identical.

## Fork Rofocale11: trailer autoplay jank (2026-11-27)

Fixing the `detail-nav-deep` FPS regression added one fork-owned file and narrow marked
branches in upstream files; this is the surface a future `upstream/main` merge must reconcile.

- `rust-modules/play-feed/src/preview_teardown.rs` (new, fork-owned): `ParkedDemux` owns a
  preview's demux thread plus the two AU queues and `HttpStream` it holds by raw pointer, and
  releases them only after a deferred join. `reap` is called from the existing per-frame
  `reap_abandoned_load_resource` hook.
- `rust-modules/play-feed/src/lib.rs`: one `pub(crate) mod preview_teardown` declaration.
- `rust-modules/play-feed/src/native_executor.rs`: `NativeStop` parks a `PrimeProfile::Preview`
  demux instead of joining it on the key frame, skips `http_close` while parked, hands the set
  to `FeedResources` in `release_and_drain`, reaps it on the existing hook, refuses a native
  start while one is parked, and exposes `drain_one` to the reaper. Each site carries a marker.
- `rust-modules/play-feed/src/feed_resources.rs`: one per-owner slot plus accessors, mirroring
  `abandoned_load`; no process-global registry.
- `rust-modules/play-feed/src/feed_driver.rs`: `exit_disposition` also refuses to call playback
  clean while a demux is parked.
- `rust-modules/screens/src/detail/mod.rs`: a key-dismissed trailer is recorded in
  `preview_played_for` (the slot natural EOS already writes), so walking back to the hero does
  not spend a second Load and video-plane BOUND edge. Upstream change is one marked branch in
  `preview_tick`, one in `collapse_background_preview`, the helper and a field-doc correction.
- `pkg/resources/{ca,hr,hu}/appinfo.json` (non-commentable JSON): upstream added these locales
  with the upstream brand in `title` and descriptions calling the app an unofficial Plex client.
  The titles are now `Nuvio Native` and the descriptions say "native Stremio addon client" in
  each language, matching the other 13 locales. `catalog_brand.rs` rewrites CATALOG strings only,
  so `nuvio-locales/brand.json`'s "Plex" -> "Nuvio" rule cannot reach appinfo resources; the
  fix has to live in these files.
- `ci/check-package.py`: added a gate rejecting "plex" in any locale appDescription. Without it
  a newly translated locale can silently ship a Plex client claim in the webOS store listing,
  because the existing check only required a non-empty, non-English description.

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
- `rust-modules/src/app/adapters/poster.rs`: ported the two missing halves of old-fork
  `204ad687f` (in the `3c4bfd60..41cf768f` push). A slot now stores `art`, an FNV-1a hash of its
  token-less key, stamped by `set_key`; `lookup` computes one hash per probe and `same_art_h`
  compares integers, so the 96-slot scan no longer pays ~250-byte comparisons on every visible
  tile of every page walk. Decoded pixels are converted to a Rust-owned buffer **on the worker**
  (`owned_px`) and released by owner (`free_pixels`), so the render thread no longer takes the
  page faults for a fresh multi-megabyte allocation. The oversized-art cap from the same commit
  was already present in this tree. `art_hash_stamps_the_stored_bytes_and_ignores_the_token`
  pins the hash rule.
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

## Agent host loop (`nuviolive`) — fork surface after the merge

The live simulator profile is a later fork feature on the merged tree. Its exact upstream surface:

- `rust-modules/src/app/nuvio/nuvio_debug.rs` — fork-owned child module rewritten for
  `Mode::{Normal,Fake,Live}`, the two dev trigger reads and a public-addon installer. No upstream
  file changed for the mode itself.
- `rust-modules/src/app/nuvio.rs` — TWO marked `Fork Rofocale11:` edits: the sealed-boot latch now
  covers both debug modes, and the addon-set match adds the `Live` branch. Everything else in that
  file is pre-existing fork work.
- `Makefile` — one marked `Fork Rofocale11:` block adding `sim-nuvio` / `sim-nuvio-linux` and one
  `.PHONY` line. Plain `sim-macos` is untouched and still has no `devtriggers`.
- `.gitignore` and `ci/check-python-steps.txt` — one marked line each: the new target dir and the
  driver's offline selftest step. Both are upstream files with only that fork addition.
- New fork-owned files with no upstream counterpart: `tools/nuvio-sim.py`,
  `tests/nuvio-public-addons.json`, `docs/nuvio/SIM-LOOP.md`. The JSON has no comment syntax and is
  recorded here instead: it holds https manifest URLs only, never a debrid token, an account key or
  the production TMDB key.

## Post-merge TV loop — fork surface

The device twin of the host loop is a later fork feature, not part of the merge above.

- `tools/tv-session.sh` — upstream file, marked `Fork Rofocale11:` edits: `fifo_send` now reads
  the jailed FIFO's uid/gid and writes the token with `setpriv`, because this firmware refuses a
  root write and the old error was swallowed; the offline FIFO selftest stubs `tvq` so it never
  opens an ssh session to the television. Later in the TV-loop work the same file gained ONE more
  marked edit, the `keyseq <gap> <token>...` verb: `key` spends two seconds in `fifo_send`'s remote
  time box per token, which stretched a 0.5 s benchmark cadence to ~2.5 s, so keyseq sends a whole
  walk in ONE ssh command and spaces the FIFO writes on the set. Its refusal and batching cases are
  covered by the offline key-door selftest. No other upstream behaviour changed.
- New fork-owned files with no upstream counterpart: `tools/nuvio-tv.py`,
  `tests/nuvio-tv-scenes.json`, `tests/nuvio-tv-benchmark-addons.json`,
  `docs/nuvio/TV-LOOP.md` and
  `.agents/skills/nuvio-tv-loop/SKILL.md`. `SIM-LOOP.md` stays host-only; the pointer in
  `.agents/skills/nuvio-sim-loop/SKILL.md` is fork-owned.

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

## Hermetic scene harness — new upstream-file surface (uncommitted)

`tools/nuvio-sim-scenes.py` now starts a local Stremio addon and a loopback TMDB
API/image fixture per scene, so routine coverage never dials a CDN. Two edit
surfaces touch file-owned code:

- `rust-modules/stremio/src/addons_tmdb.rs` (fork-owned): `TMDB_API_BASE` overrides
  the API origin; new public `tmdb_image_base()` reads `TMDB_IMAGE_BASE`.
- `rust-modules/pmsshim/src/shim_pms.rs` (upstream-owned): five production image
  URL builders now format through `plx_stremio::tmdb_image_base()` instead of the
  hardcoded `https://image.tmdb.org/t/p` string, each marked `Fork Rofocale11:`.
  With neither env var set, the emitted URLs are byte-identical to upstream's.

Merge rule: if upstream moves those five builders, re-apply the env-origin indirection
at the new sites; do not let the hardcoded CDN string return.

### Upstream-discipline jank fixes (uncommitted)

- `rust-modules/pmsshim/src/shim_pms.rs`: `tmdb_image_base` is imported through
  the `addons_tmdb` module, not the crate root — the layer gate classifies a bare
  `plx_stremio::` root item as `[app]`, which `[pmsshim]` may not name.
- `Makefile`: `sim-nuvio` and `sim-nuvio-linux` are in `DISK_ENTRIES`, since both
  run cargo and `ci/test_disk_preflight.py` enforces the list.

- `rust-modules/screens/src/detail/mod.rs` (upstream-owned): the detail backdrop is now
  requested at 1280x720 instead of 1920x1080 (`Fork Rofocale11:` marker at the call),
  restoring upstream's modest art-size discipline. A device FRAMEDROP line measured a
  48.6 ms one-shot upload of 2,073,600 px on detail open; `Rect::FULL.cover` scales the
  smaller texture over the same rect.



- `rust-modules/ui/src/cards/shelf.rs` (upstream-owned): restored old fork `204ad687f`'s
  off-band `card_row::paint_visible` cull (`Fork Rofocale11:` marker at the call). Detail's
  Related/Collection/Extras/Cast shelves use this same `Shelf`; an off-band card was paying
  art resolution and label fitting even though it contributes no fragment. The current tree
  had lost this gate in the shim port.
