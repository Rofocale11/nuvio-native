# Post-upstream-merge regression gate (Nuvio Native)

Fork Rofocale11. When `upstream/main` moves, run this gate on the merged fork branch before calling
the merge good. The point is that an agent can now reproduce the fork's behavior on the TV, so a
merge no longer has to be waved through on "it compiles".

Authority: `MERGE-AUDIT.md` records the conflict surface; `SCENARIOS.md` is the scenario catalogue;
`SYNTHETIC-DEBUG.md` is the exact synthetic-mode recipe. This document is the ordered gate. A
result is evidence about the tree it ran on: commit, confirm a clean `git status --short`, run the
gates, then re-check the tree before reporting.

## 0. Static conflict surface

An upstream merge can delete a fork branch without a compile error when the surrounding API still
exists. Check the markers first:

```bash
rg -n 'Fork Rofocale11:' rust-modules/src/app/boot.rs rust-modules/src/app/nuvio.rs \
  rust-modules/src/app/nuvio/ rust-modules/nuvio-store/src/lib.rs \
  rust-modules/pmsshim/src/{shim_account,shim_core,shim_pms}.rs | head -80
```

Required branches, each still carrying its marker and reason:

- `app/boot.rs` — private store installed before `session::load`; synthetic session-init keeps
  `captured_boot`'s DevPms authority; boot gate gains `|| synthetic_boot`.
- `app/nuvio.rs` / `app/nuvio/nuvio_debug.rs` — `mod nuvio_debug;`, `prepare_store()`, and the
  `plxnative-nuviofake` selection still feed `plx_pmsshim::start`.
- `nuvio-store` — `EPHEMERAL` override on both `get()` and `get_arc()`, and `install_ephemeral()`.
- `pmsshim` — `Config::synthetic`, `Account::new_synthetic`, and the watch pull/push gates.

Also check the other marked sites listed in `MERGE-AUDIT.md` ("Upstream files edited after the
merge", "In-app DTS / TrueHD / Opus conversion", and the current synthetic-mode entry). A missing
marker is a fail, not a rebasing nicety.

## 1. Host gates (no TV, no lease)

```bash
make check                         # full gate, on the committed tree
make check-nodef                   # shipping feature set, including play-uri/play-feed
python3 ci/check-module-layers.py
ci/check-deps.sh
python3 ci/check-ledger-ratchet.py  # needs PLX_LEDGER_BASE or GITHUB_BASE_REF to be meaningful
git diff --check
```

Focused loop while resolving conflicts: `make test-crate C=<crate>`,
`make test-hostsim C=plx_media`. A focused pass is not the full gate.

## 2. Target build

```bash
PATH="$HOME/.cargo/bin:$PATH" make          # ARM debug, tvdev profile
file pkg/plxnative                          # must be a 32-bit ARM ELF
```

For a release/ship merge also run `make FLAVOR=stable RELEASE=1 ipk` and the release-only package
gates. Do not infer the target build from a host `cargo check`.

## 3. TV acceptance — synthetic boot (one lease)

Build with the lease **released**. Then:

```bash
tools/tv-lock.sh status
# An IN USE: line means stop and tell the owner. Override only with their explicit go-ahead.
tools/tv-lock.sh acquire --ttl 30 --why "post-merge synthetic regression gate"
PLX_TV_IN_USE_OK=1 tools/tv-session.sh up --screen home --arm nuviofake
PLX_TV_IN_USE_OK=1 tools/tv-session.sh log 'nuvio|hubs:|pms: server|coldopen'
PLX_TV_IN_USE_OK=1 tools/tv-ssh ssh tv 'curl -s http://127.0.0.1:32420/identity; echo; curl -s http://127.0.0.1:32420/api/v2/user'
PLX_TV_IN_USE_OK=1 tools/tv-session.sh state --out /tmp/merge-state.json
PLX_TV_IN_USE_OK=1 tools/tv-session.sh down
tools/tv-lock.sh release
```

Fail on any of these:

- no `nuvio: SYNTHETIC private redb store` or `nuvio: SYNTHETIC debug backend` line;
- `pms: server 0 version=` other than `1.41.0.0000-nuvio`, or identity machine id other than
  `nuvionative-pmsshim-0001`;
- `hubs: … N in CW` with `N > 0`, a `coldopen screen=account` line, `state` `owner` not `page`,
  or `/api/v2/user` naming anything other than the local `Nuvio Native` user;
- Home did not reach `route=home`, or the deployed binary md5 did not match.

The port above is the shim's fixed `PORT` (`32420`); if an upstream change moves it, read the
`nuvio: shim serving on` line instead and use that port.

## 4. TV acceptance — Streams picker (one lease, or continue the one above)

Use the deterministic keys from `SYNTHETIC-DEBUG.md` (do not guess on the remote):

```bash
tools/tv-session.sh up --screen detail=10002 --arm nuviofake
tools/tv-session.sh press detail.hero.alt
tools/tv-session.sh press alt.row.0
tools/tv-session.sh state                    # expect 1080p AVC WEB-DL AAC · 150 MB · Fake
```

and the episode path:

```bash
tools/tv-session.sh up --screen detail=10001 --arm nuviofake
tools/tv-session.sh state                    # read detail.hero.play's subject
tools/tv-session.sh press detail.hero.play --rk <that subject>
tools/tv-session.sh press player.control.more
tools/tv-session.sh press more.streams
tools/tv-session.sh press more.addon.0
tools/tv-session.sh state                    # expect more.stream.0
```

Fail if either picker is empty or shows real account/addon data. The fake torrent row has no bytes;
decoded playback is not part of this gate.

## 5. Scenario spot checks and recording

From `SCENARIOS.md`, run every **A** scenario whose code the merge touched; run the relevant **B**
tests; leave **C** to the owner. `make check-replay` is the only unverified item — do not use it as
a gate. Filmed trailer end-to-end was owner-confirmed on 2026-11-26.

This gate is itself a fork-owned document; re-check it on every merge alongside `MERGE-AUDIT.md`
and `SCENARIOS.md`.

Record in `MERGE-AUDIT.md`: merged commit hash, each command, the `test result:` line or log excerpt
that proves it, and the tree state after the run. A green compile plus a skipped TV scenario is not
a passed gate. If a check fails, stop and report the exact failure rather than weakening the check.
