# C-engine migration (worktree `c-engine-skeleton`)

Status: built and wired end to end, **not committed, not run on a TV**.

## What changed
- `vendor/engine-sources/`: Nuvio Engine pinned at `99a1860` (0.1.5) with `engine.patch`
  applied, plus `sbengine.cpp` / `core/sbengine.h` from stremio-origin `tools/engine`
  (`SBE_API_VERSION 3`, single exported symbol `sbe_get`).
- `ci/build-openssl.sh`: OpenSSL **1.1.1w** for ARM, static (`no-shared no-zlib no-async
  no-engine`), into `vendor/openssl-prefix`. The NDK sysroot's static `libcrypto.a` is unusable
  (it wants brotli/zstd objects the sysroot does not ship), and the sysroot's shared 1.1 library
  exists on almost no television.
- `ci/build-sbengine.sh` + `ci/cmake-toolchains/webos-arm.cmake`: CMake super-project against the
  webOS ARM NDK (g++ 12.2.0, C++20), with `_FILE_OFFSET_BITS=64` so the engine's `lstat` on a
  payload entry does not return `EOVERFLOW` for files past 2 GB (a 26 GB 4K file logged
  `disk cache policy error: inspect torrent payload entry: Value too large …` without it).
  Output: `build/sbengine/build/libsbengine.so` (with symbols)
  and the stripped `build/sbengine/libsbengine.so` (6.9 MB), ELF32 ARM soft-float.
  libstdc++/libgcc and OpenSSL are linked statically.
- `rust-modules/stremio/src/sbengine.rs`: `dynlib!` binding of `sbe_get`, `repr(C)` API table,
  `available()` / `start_engine` / `stop_engine` / `open` / `files` / `stream` /
  `resolve_stream` / `stats` / `limits` / `window` / `setting` / `remove` / `relocate`.
- `boot.rs`: `start_sbengine` / `stop_sbengine`, called from `start_p2p_service` /
  `stop_p2p_service`, dirs `<runtime_dir>/serverp2p/{state,cache}`, gated on `available()`.
- `pmsshim/shim_pms.rs::open_url`: P2P switch gate first, then C engine (`resolve_with_sbengine`,
  magnet from the stream or synthesized from `peer_sources`), then the Node EngineFS HTTP path.
- `curlio.rs`: torrent timeout exemption keyed on loopback host, not the `11470` literal.
- `sbengine::resolve_stream` remembers the playing info hash and `remove`s the previous torrent
  when a source change resolves a different one: the engine keeps every opened torrent
  downloading (and holding a window) until removed, so without this the old source competed with
  the new one for bandwidth and RAM. `sbengine::active_hash()` exposes that hash, because a
  released torrent's `/stream/<id>` URL is dead for the rest of the session.
- `pmsshim/shim_pms.rs::cached_open_url`: the shim's resolved-URL cache is reused only while its
  `torrent:<hash>` marker still names the engine's active torrent. Without that check, switching
  source A -> B -> A returned A's cached engine URL after A had been released; the engine answered
  404 and the player retried forever (`ff: http_open FAILED status=404`, `playback retry:
  resolving item again`). Node URLs never move, so the check only applies when the engine loaded.
- `pmsshim/shim_pms.rs::torrent_server`: with the engine loaded an empty Settings server URL
  falls back to the default loopback address, so clearing the field no longer hides torrents
  (the address is only dialled on the Node fallback).
- `Makefile`: both `make deploy` and `make ipk` ship `libsbengine.so` beside the binary when it
  is built. The ipk path pins its SHA-256 in `pkg/link-evidence/<flavour>/unreceipted.sha256`
  because it is a CMake artifact with no Rust link receipt; `ci/check-packaged-elf.py` verifies
  that pin.
- Docs updated in the same worktree: `AGENTS.md` constraint, `ARCHITECTURE.md` (§P2P + §Constraints),
  `FEATURE.md`.

## Firmware posture (measured, not guessed)
`libsbengine.so` has `DT_NEEDED libdl`, `libpthread`, `libm`, `librt`, `libc`, `ld-linux` —
**no `libssl`/`libcrypto`, no `libstdc++`/`libgcc`** (all linked statically). `tools/fwcompat.py`
on the stripped library:

| release | verdict |
|---|---|
| 1.2.0 … 11.2.0 (every release in the inventory) | **OK** |

It also exports exactly one symbol, `sbe_get`. The static OpenSSL is why the 2025 sets (which
ship `libssl.so.3`) can load the engine at all; the previous dynamic build loaded on no shipping
firmware older than 7.4 and none newer than 8.3.

## Deliberate limits in this worktree
- `.ipk` carries the `.so` when it was built, under a digest pin (see above). When it was not
  built, the package ships without it and uses Node; that is not an error on the device, but a
  release is required to carry it: `build-ipk.yml` runs `ci/build-sbengine.sh` before `make ipk`
  and then asserts the staged package contains `libsbengine.so`, so a release cannot silently
  ship a Node-only package.
- Node is **not** spawned when the engine starts (`start_p2p_service_inner` returns early), so the
  engine owns torrents on every firmware it loads on; there is no mid-session Node fallback. If
  the engine cannot load, Node is spawned exactly as before.
- File pick under the engine uses `stream.file_idx`, else largest file; `(season, episode)` still
  only feeds the Node guesser.

## Verification actually run
- `cargo nextest -p plx_pmsshim -p plx_stremio`: 150 passed, 0 skipped.
- `cargo nextest -p plx_media curlio`: 85 passed.
- `cargo nextest -p plxnative-modules boot`: 53 passed.
- `cargo +nightly check --lib --no-default-features`: clean.
- `ci/check-module-layers.py`: green. `ci/check-module-cycle.py`: ok.
- ARM app build: `make -j10` **succeeded** against the pinned NDK.
- `make FLAVOR=stable RELEASE=1 ipk`: **succeeded**, 24 MB package. `check-package.py` passed
  every packaging assertion and `check-packaged-elf.py` reported
  `PASS actual IPK: 7 receipted + 1 digest-pinned ELF files verified`. The packaged
  `libsbengine.so` is byte-identical to `build/sbengine/libsbengine.so`.
- `tools/fwcompat.py build/sbengine/libsbengine.so`: **OK on every release in the inventory**
  (1.2.0 … 11.2.0); `DT_NEEDED` is libdl/libpthread/libm/librt/libc/ld-linux only.
- **On the television** (debug install, `--guest`): `sbengine: engine started (0.1.5)`, then
  `sbengine: serving <hash> via engine (file 0)`; no `node` process in `/proc`; 5–12 MB/s, steady
  48 fps, `VmRSS` ~142 MiB, `VmHWM` ~151 MiB, `VmSwap` ~20 MiB; switching source A→B→A logs
  `released previous torrent <a>` then serves B, and the player recovers instead of 404ing.
- **The GitHub-built package, on the television** (`build-ipk.yml` dispatch, run 42041322327 on
  `84433947`): engine linked at 7.1 MB, `DT_NEEDED` unchanged; artifact
  `com.nuvionative.app_0.8.0_arm.ipk` 24,863,668 bytes, `libsbengine.so` inside it hashes
  `f9b83548…` (the digest the run pinned) and is the only staged copy. Installed stable and
  played: `sbengine: engine started (0.1.5 / libtorrent …)`, `serving 343d7045b543 via engine
  (file 0)` then a second title, **no `node` line at all**, 19–29 peers at 0–3 MB/s, engine
  `window=40MB mem=32MB` (its ceiling holding), 45 minutes in at a steady **48 fps with 0
  dropped / 0 carried / 0 evicted**. Memory on this build for that session: `VmRSS` 114 MB,
  `VmHWM` **179 MB**, `VmSwap` 88 MB — a higher peak than the earlier debug session above, which
  is the one number here that still wants a look during a long 4K playback run.

## Building and handing over a test build
```
./ci/build-openssl.sh           # once: -> vendor/openssl-prefix (static 1.1.1w)
./ci/build-sbengine.sh          # -> build/sbengine/libsbengine.so (6.9 MB, stripped)
make -j10                       # -> pkg/plxnative (ARM)
make FLAVOR=stable RELEASE=1 ipk  # -> pkg/com.nuvionative.app_<version>_arm.ipk
```
Both `make ipk` and `make deploy` copy `libsbengine.so` beside the binary when it exists. The
ipk pins its SHA-256 in `pkg/link-evidence/<flavour>/unreceipted.sha256`; an install therefore
uses the engine automatically on any graded firmware, with no setting to change.

Log evidence that the C engine is the one playing (`./tools/ai-tv-log.sh grep "sbengine"`):
- `sbengine: engine started (<version>)` — the `.so` loaded and started.
- `sbengine: libsbengine.so not available` / `sbengine: start_engine failed` — engine did not
  load; the Node child is spawned as before.
- `nuvio: C engine owns P2P; Node server not spawned` — the engine-only path is running.
- `sbengine: serving <hash> via engine (file <n>)` — one line per opened torrent.
- `sbengine: peers=N seeds=N down=NKB/s up=NKB/s got=NMB window=NMB …` — once per 10 s while the
  torrent moves; this is how "slow torrent" is told apart from a player-side problem.
- `sbengine: resolve failed, Node fallback (<reason>)` — the engine could not serve that stream
  (there is no Node process to fall back to when the skip line above is present).

