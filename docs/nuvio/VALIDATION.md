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
test result: ok. 227 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
test result: ok. 1165 passed; 0 failed; 7 ignored; 0 measured; 0 filtered out;
test result: ok. 979 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;
```

The ignored media cases require the native host FFmpeg/libass integration harness;
the other ignored case requires a breadcrumb capsule produced on the TV. The stable package and physical-remote checks
are recorded in the final task report.

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

## Limitations

See `HANDOFF.md`: watch state/key table persistence across TV reboot, account QR login,
configured torrent service, DTS/TrueHD conversion and P7 fallback remain follow-up work.
