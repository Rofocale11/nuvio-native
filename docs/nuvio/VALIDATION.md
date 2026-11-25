# Nuvio Native validation

## Current status (HEAD `1db875cc8`, 2026-11-25)

The dated sections below are historical snapshots; this section describes the current tree.
The branch is `nuvio-native` and nothing is pushed. The full `make check` gate passed on each of
the trees behind `84a00aa68`, `e0b584c0c` and `9e215acce` — **146 steps — all ok**: app 1491,
`plx_media` 2501, `plx_screens` 2347, `plx_data` 1667 and `plx_plex` 839 passed, with the Python
harness at 753 tests (`OK (skipped=4)`). The first `e0b584c0c` attempt flaked
`ci/test_disk_budget.py` on git auto-maintenance and was rerun clean; `9e215acce` removes that
flake. `1db875cc8` is documentation only. The local stable IPK and the deployed set still predate
`e0b584c0c`.

Every durable record now lives in one `nuvio.redb` (no DB8 helper, no permission grant), with
self-heal for unreadable canonical records and a 50 MiB cache-only soft budget. The 0.10.0
stable package built from `84a00aa68` was deployed with `make FLAVOR=stable RELEASE=1 deploy`
(`verify-deploy: 50 files match`) and the owner confirmed it works.

### Device verification on the 7500PSC (owner, 2026-11-25)

The owner confirmed on the LG `45UR7500PSC` (webOS 10.3.1, board `K8LPN_DVB`):

- DTS, TrueHD and Opus tracks each decode in the app to stereo 48 kHz PCM in Auto, because
  this set has no native path for those tracks. This closes the codec item in `PLAYBACK.md`.
  Recorded as an observation on this set, not as a rule that overrides a native decoder.
- The Catalogs selector in the Movies/TV Shows header, its capsule and the Collections
  fallback (`84a00aa68`); explicit addon failures, and catalog/subtitle caches staying warm
  across a TV relaunch (`e0b584c0c`).
- Every previously open device item: the P7 HDR10 base-layer fallback, cross-device watch
  sync on a real account, the new audio default and rating setting, the English audio default
  on a newly started stream, visible subtitle compositing, relaunch/reboot persistence of
  Settings, addons, My List and watch state, the startup-prime 1.2× rate and 3 s floor, the
  3 s progressive hold's `reason=time`, the 30 s deadline-only `/decision`, the two-level
  Streams picker's native playback and slide animation, the HUD logo on device, the
  last-played-source resume, and the in-app reset.
- The cold first detail open now meets the ≤800 ms target, and the dedicated
  `plx_nuvio_probe` sim-probe comparison is done. No new phase figures were captured in this
  update; the `ms=1439…2061` figures below belong to the earlier snapshot.

**Still not verified:** `make check-replay` against rerecorded fixtures. The replay anchors
need `tools/plxnative-rec rerecord` on a macOS simulator before that gate is trusted — the
Linux host cannot do that step.

**Owner confirmation (2026-11-26):** the other former gaps — fresh cold-detail phase figures,
engine prebuffer and the firmware/vendor unit matrix, release-tag CI gates, multi-hour stability,
and a TV run attributed to the post-extraction probe — are verified. No command or log for them was
attached to this document.

## Debug TV

On the LG CX (`tv`, webOS 5 Developer Mode), the debug app installed and launched,
loaded Home and real metadata/images, and used all four imported Nuvio addons.

An AIOStreams 4K HEVC/Dolby Vision P8 source played through Starfish with its decoder
reporting DolbyVision. Displayed frames and play position advanced; pause held the
position, resume advanced it, and multiple seeks succeeded. Audio switched from AC3
to EAC3. The Streams overlay selected another 4K Dolby Vision source, which played
and sought. Embedded subtitle selection succeeded and nonempty timed cues arrived.
An external addon subtitle also fetched successfully (2,047 parsed cues ready).
These are native event-log observations. UI-plane screenshots cannot prove video-plane
appearance or visible subtitle compositing.

Addon configuration restored from DB8 after reinstall/relaunch (DB8 at this snapshot; the
single redb replaced it on 2026-11-22). Stable import testing
exposed a sticky-`/tmp` unlink failure; the handoff now uses a group-writable directory
so a verified import is consumed rather than replayed on each launch. The original Nuvio
app was read only for addon import and was not modified.

## Host checks

The detail Play regression and unprobed decision refusal tests failed before their
fixes and passed afterward. Storage tests cover opaque addon persistence separately
from session state, restart, readback verification and rejected writes. Shim tests
cover image passthrough, actual HEVC/DV/track metadata and decision refusal.

Fresh synthetic UI anchors pass Targets and Resolve (six replays, zero differences).
The shipping Rust feature set passes `cargo +nightly check --lib --no-default-features`.
The complete `make check` gate passed: all 79 auxiliary steps and the 443-test Python
harness at this snapshot (in the current tree: 146 steps and a 753-test harness), plus the
default and hostsim Rust suites and lint checks. Representative
unit-test result lines (the gate retains every crate's result):

```
test result: ok. 493 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
test result: ok. 1165 passed; 0 failed; 7 ignored; 0 measured; 0 filtered out;
test result: ok. 1205 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
```

The ignored media cases require the native host FFmpeg/libass integration harness;
the other ignored case requires a breadcrumb capsule produced on the TV. The shim and Stremio addon suites also passed:

```
test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
test result: ok. 37 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
```

The release discovery regression first failed under `HttpsOnly` and then passed.
It covers the registered shim plus rejected absent registration, wrong token, wrong
machine identity, wrong port, unowned resource and a falsely registered LAN server.
English audio tests cover a foreign default and English commentary. Rating tests
cover the canonical preference (DB8 at this snapshot; the redb session record now) and
switching a cached detail without
substituting IMDb when RT is absent. The picker was inspected in the simulator;
all three fresh synthetic fixtures passed both replay modes with zero differences.

## Stable release

Historical 0.8.0 package (the current package is 0.10.0) built with
`source ~/.cargo/env; make FLAVOR=stable RELEASE=1 ipk`.

Install `com.nuvionative.app` with `tools/nv-tv.py`, import the same addons into that
app ID, and launch it. Verify the installed binary against the packaged binary and
the event log's `features=release` identity. Check Home, then use the physical remote
to play a 4K Dolby Vision source, seek, pause/resume and enable subtitles. Collect
native decoder, timeline and subtitle-cue evidence alongside the viewer's observations.
The release disables injected remote keys and UI capture, so visible playback/subtitle
checks require the physical remote. Addon restoration should be checked by relaunching
with the consumed import file absent.

## Stable observations

The user confirmed Home, 4K Dolby Vision playback, seek, pause/resume and visible
English subtitles on the initial stable build. The follow-up release installed and
launched, restored four addons from DB8 (the store in use at that time), and its payload
hash matched the installed binary; the boot identity reported `features=release`.

A fresh QR approval then logged `authorized — discovering server`, reached the
registered Nuvio server, installed its PMS client and loaded the account's addons.
The previous `discovery_insecure_only` failure no longer occurred. A newly selected
source probed as HEVC 3840×1604, 23.976 fps, Dolby Vision, two audio tracks and three
embedded subtitles. That probe alone does not establish playback or English audio. Later (2026-11-25) the owner
confirmed the new audio default and rating setting, playback and English audio on the 7500PSC;
see Current status.

## Single redb store, no DB8 helper (2026-11-22)

The fork now keeps every durable record in one `nuvio.redb` under `<data>/shim/`: canonical session
and Settings preferences, addon config, install-wide preferences, Nuvio account audio/subtitle
picks, watch state, key table, Nuvio backend session and local settings. Upstream's storage backend
runs in process against that file (`plx_os::storage::redb_backend`), with DB8-shaped objects and
Keymanager3 AES-256-GCM envelopes as two more tables. The `plxstorage` helper is no longer built,
packaged, activated or verified; the ipk declares no service.

Evidence on this tree:

- ARM cross-build `make pkg/plxnative`: pass; `make check-nodef`: pass.
- `check-deps` and `check-module-layers`: green (24 layers, new `nuvio_store` layer).
- Host tests: app 1486/1486, screens 2325/2325, os 424/424 (three new redb/keymanager tests),
  pmsshim 18/18; rewritten no-service package tests 4/4.
- Simulator rebuilt and booted twice from a fresh runtime: `state/nuvio/shim/nuvio.redb` created,
  no errors.
- Debug build deployed to `com.nuvionative.app.debug` on the set with the owner's HDMI override;
  `verify-deploy` matched 47 files. **Not launched or driven after that deploy** — later debug
  and stable builds were launched and driven; the 2026-11-25 confirmation is in Current status.

Three known limits, not claimed as verified:

- DB8 records are not migrated (no helper can read them); legacy on-disk files are migrated once.
- Reboot durability depends on which `nuvio_data_dir()` candidate the set grants; the last fallback
  is `/tmp`.
- `plx_plex` had three pre-existing `transcoder.rs` failures at that time (the fork had added
  `opus` to the codec list without updating those expectations). The later `make check` alignment
  pass fixed the expectations; `plx_plex` was 826/0 at `eef501647` and is 839/0 in the current
  tree.

At that time the owner still needed to check relaunch persistence of a Settings pick, and that
addons, My List and watch state load after the switch. Later (2026-11-25) the owner confirmed
all four on the 7500PSC; see Current status.

## Limitations

<!-- Fork Rofocale11: DTS/TrueHD conversion was listed here as follow-up work; it is now
     implemented for Auto (in-app stereo PCM) and must be re-checked on every merge. -->
Watch state/key table and account backend session persistence across TV reboot and the P7 HDR10
fallback were confirmed on the 7500PSC on 2026-11-25 (see Current status). The configured torrent service
from this snapshot has been replaced by the in-process C engine with a Settings P2P switch
(default ON). RT scores require metadata supplied by an addon; MDBList
was not configured or tested here.


## BetterPosters and My List follow-up

The public BetterPosters configure page generates the IMDb poster URL pattern and
advertises uppercase `RT` as its Rotten Tomatoes source. A live Matrix poster fetch
returned HTTP 200 WebP with a visible 83% score. Its addon metadata did not include
an independent Rotten Tomatoes number. The user chose poster ratings for now.

The deployed Nuvio OpenAPI schema confirmed the profile-scoped library item/delta
RPCs. A temporary single-title add/remove on profile 1 was followed by fresh pulls:
the added title appeared, the existing saved title remained, and removal restored
the original key set. No original-app state was written.

Host coverage includes provider fallback, identity validation, single-item RPC
payloads, failed sync presentation, login routing and translated action-row fit.
Fresh synthetic recordings (shorter settled key cadence on a 120 Hz host) pass
all six Targets/Resolve replays with zero differences. The default recording
cadence exceeded the parser's decoded-memory budget; the budget was retained.
The debug package was installed with its payload hash checked. Native detail Add
changed to Remove only after the backend confirmed the saved title; Remove restored
the original library. The button layout was inspected on the TV. The broad `make
check` follow-up was stopped at the user's request to prioritize delivery; the
focused screen/shim/addon suites, six replays and shipping compile had passed.
Continue Watching now refreshes stored artwork through the same poster policy.
The first TV poster check exposed `WebP is not supported` decode failures:
BetterPosters serves WebP even for `.jpg` paths. Both image dependency declarations
now enable the pure Rust WebP decoder, retaining the existing allocation limits.
The shipping compile passed after enabling it. On the refreshed debug TV build,
Continue Watching displayed BetterPosters artwork with RT percentages and Home's
My List shelf displayed the existing saved title; the decode failures stopped.

The final stable package was built from `6e9c2acc` with `FLAVOR=stable RELEASE=1`.
Its package/ELF assertions passed, installation payload hash matched, launch logged
`features=release`, four addons restored from DB8, and Home loaded 41 shelves.
The stable library endpoint reported the original one saved title. No fresh physical
playback/subtitle check was run on that artwork/list follow-up at the time; the earlier stable
viewer confirmation and login observations above remained the playback evidence until the
owner's 7500PSC confirmation on 2026-11-25 (see Current status).
