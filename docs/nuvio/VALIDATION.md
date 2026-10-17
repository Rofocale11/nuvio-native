# Nuvio Native validation — 2026-10-10

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

Addon configuration restored from DB8 after reinstall/relaunch. Stable import testing
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
harness, plus the default and hostsim Rust suites and lint checks. Representative
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
cover canonical DB8 preference splitting and switching a cached detail without
substituting IMDb when RT is absent. The picker was inspected in the simulator;
all three fresh synthetic fixtures passed both replay modes with zero differences.

## Stable release

Package: `pkg/com.nuvionative.app_0.8.0_arm.ipk`, built with
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
launched, restored four addons from DB8, and its payload hash matched the installed
binary; the boot identity reported `features=release`.

A fresh QR approval then logged `authorized — discovering server`, reached the
registered Nuvio server, installed its PMS client and loaded the account's addons.
The previous `discovery_insecure_only` failure no longer occurred. A newly selected
source probed as HEVC 3840×1604, 23.976 fps, Dolby Vision, two audio tracks and three
embedded subtitles. That probe alone does not establish playback or English audio.
Physical confirmation of the new audio default and rating setting is still pending.

## Limitations

See `HANDOFF.md`: watch state/key table and account backend session persistence
across TV reboot, configured torrent service, DTS/TrueHD conversion and P7 fallback
remain follow-up work. RT scores require metadata supplied by an addon; MDBList
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
playback/subtitle check was run on this artwork/list follow-up; the earlier stable
viewer confirmation and login observations above remain the playback evidence.
