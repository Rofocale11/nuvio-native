# Nuvio Native upstream merge audit

Merge of `upstream/main` (`plxnative/plx-native@0a1edc61`) into the fork on
`integrate/upstream-latest`, using the shared import base `4e454ea` (the fork root
`a296450a` imported that exact tree). 663 upstream commits, 2,288 files changed;
45 files had true conflicts and are recorded here.

**Resolution policy:** integrate both sides wherever they can coexist. The fork's
version wins only where the two are semantically incompatible (app identity, the
in-process PMS shim, Nuvio account/My List/rating-source behavior, public-root TLS
trust). Every fork-originated block retained in a comment-capable file carries
`// Fork Rofocale11: <detailed why>` inline. Files whose format cannot carry a
comment are listed here instead: locale JSON (strict `value`/`description` object
shape), replay JSONL and manifests (hash-pinned recordings), `Cargo.lock`, and the
Debian `control` file.

## Build / CI / packaging

| File | Integrated from upstream | Kept from fork / why |
|---|---|---|
| `.github/workflows/nightly.yml` | Tag-first push with ERR trap, separate release repo, `--repo` download | `com.nuvionative.app.nightly.manifest.json` — the fork's app id; upstream's `com.beb.plxnative.nightly` is a different install the TV would refuse. |
| `.gitignore` | New upstream target dirs, `tools/node_modules/`, `/.empryo/` | `rust-modules/stremio/nuvio.local.env` — private Stremio/Nuvio-account addon config; upstream has no addon backend. |
| `Makefile` | Emoji + Arabic/Hebrew font families | `pkg/public-roots.pem` — Mozilla CA list used as the roots-mode trust store for public Stremio CDNs and the Nuvio backend, whose issuers a 2020 TV firmware predates. |
| `THIRD-PARTY-NOTICES.md` | Upstream's header-availability statement | Embedded SDL2/Mesa/Khronos/jsmpeg notices and the Mozilla CA (MPL-2.0) attribution — still shipped in this fork and must be preserved. |
| `ci/mkmacapp.py` | Emoji/Arabic/Hebrew font bundling | `public-roots.pem` — same TLS trust reason as `Makefile`. |
| `ci/module-layers.ini` | `platform`→`os`, playback crates, host-doorway comment, `testrace`/`testheap` | `local_server`, `stremio`, `pmsshim` — the shim identity and addon backend must stay registered or the layer gate rejects the app's own references. |
| `ci/test_no_host_staticlib.py` | New layer lists, pinned toolchain, upstream workspace tree | `plx_stremio`/`plx_pmsshim` in both lists — the harness symlinks these paths; without them a cargo graph resolve in the throwaway copy fails. |
| `ipkroot/ctl/control` | Nothing lost (only upstream's identity fields) | `com.nuvionative.app`, version `0.8.0`, fork maintainer/homepage — upstream's `com.beb.plxnative` collides with the real PlxNative install and breaks opkg identity and DB8 separation. No safe comment syntax in a Debian control file. |
| `pkg/appinfo.json` | Nothing lost (other fields identical) | Fork app id/version/vendor for the same identity reason; JSON permits no comment. |
| `tools/tv-lock.sh` | `PLX_TV_LOCK_LOCAL` short-circuit, `SSH_TV_OPTS`, bounded-run helpers | `NV_DEVICE` ares-novacom branch — this fork drives a non-rooted LG CX through an ares device; upstream's ssh-only helper cannot reach it. |

## Locales and replay fixtures (no comment syntax)

- All six locale files: the fork's pre-existing Nuvio-branded auth, My List, stream-picker
  and rating-source strings are kept verbatim; upstream's new `browse.detail.*`,
  HUD, diagnostics, playback grouping and audio keys are added alongside. No new
  branding was invented, and no existing string was translated or rewritten.
- The three `rec-0000.jsonl` recordings and their manifests: upstream's newer recordings
  are taken as atomic traces (mixing frame-codec generations would be incoherent),
  with only the fork's in-process shim origin `127.0.0.1:36430` patched into the boot
  lines and the corresponding manifest authority fields. **Open verification item:**
  upstream's `hash`/`state_fp` were retained; if replay validates the hash against
  recording bytes, the fixtures need re-recording and re-pinning.
- `ALPHABET.json`: union of upstream's eight new source vocabularies and the fork's
  `nuvio_stream_picker` source / `Streams` literal.
- `tests/fixtures/replay/README.md`: both branches' chronology kept as prose (HTML
  comment marker added inline).

## Rust code

| File | Integrated from upstream | Kept from fork / why |
|---|---|---|
| `rust-modules/Cargo.toml` | New `plx_shell_sdl` member | `plx_stremio` + `plx_pmsshim` — addon backend and loopback shim feed the player's probed decision. |
| `rust-modules/Cargo.lock` (no comments) | Upstream's resolved versions, `indexmap`, `plx_shell_sdl` | Fork's `image-webp`, `plx_stremio`, `plx_pmsshim` entries — same local-crate reason. |
| `rust-modules/data/src/metadata.rs` | Upstream's `fetch_playing_item_choosing`, `PlayedVersion`, new test modules | Fork's `nuvio_rating_preference_tests` — IMDb/RT scores live on the cached detail and the canonical session picks the provider. **Dropped:** the fork's `playing_item_from_metadata` body, which depended on removed APIs. |
| `rust-modules/media/src/ff.rs` | Upstream's move of the demuxer into `plx_play_feed` | Whole fork probe/binding core including `ff::probe` — third-party addon streams are probed with bundled libavformat and unsupported media is refused, not loaded with guessed codecs. **Follow-up:** old imports (`crate::aq::AuQueue`, `crate::player::threads::SendPtr`) need porting. |
| `rust-modules/media/src/route/plan.rs` | Git's non-conflicted changes | Whole fork `build_stream` — shim `/decision` first, probed Media replaces guessed codecs, `Quality::Original` because the shim has no transcoder. **Follow-up:** port to upstream's new metadata/playqueue APIs. |
| `rust-modules/net/src/net.rs` | `RedirectFloor` doc rewrite | `diagnostic_peer()` — reduces addon/debrid URLs to scheme+authority before logging so per-request tokens and credentials are never written. |
| `rust-modules/plex/src/plex/session.rs` | Flattened `Preferences` refactor, `plx_os` rename, new enums/tests | `rating_source` across struct/defaults/constructors/accessors and its DB8 round-trip test — upstream has no rating-provider key, so a pick would be lost on reload. |
| `rust-modules/plex/src/plex/session/migration_tests.rs` | `plx_os` rename | DB8 namespace `com.nuvionative.app.storage` — upstream's namespace names a different record. |
| `rust-modules/src/app/preferences.rs` | Single `Choose` write door, `AutoSignIn`, owned present gate | `PreferenceCmd::RatingSource` → `nuvio::set_rating_source` — upstream's door only covers plex.tv-backed player settings. Fork's superseded `Quality` command dropped. |
| `rust-modules/src/dev/scenarios.rs` | Shared `settings_boot_page` helper | `settings-boot picker-ratings` target — the Nuvio rating picker has no upstream equivalent. |
| `rust-modules/screens/src/detail/geometry_tests.rs`, `identity_tests.rs`, `detail/tests.rs`, `detail/mod.rs` | Upstream's `caption_policy`, control refactor, watchlist discs, tests | Fork's `list_*` My List state, `NuvioStreamSelected` handling and My List tests — the Nuvio account backend owns those lists, not the plex.tv Watchlist. |
| `rust-modules/screens/src/family.rs`, `preferences.rs`, `preferences/picker.rs`, `preferences_text_fit_tests.rs`, `settings_nav_structure_tests.rs`, `preferences_tests.rs`, `registry.rs` | Upstream's picker/preference `V5` refactor, new picker kinds, grouped fields, `Choose` | `RatingSource` vocabulary, command, write path and UI — the Nuvio IMDb/RT choice is a detail-presentation preference upstream cannot express. |
| `rust-modules/screens/src/detail/hero.rs` | Upstream's whole hero control/disc refactor | `ELEM_MY_LIST`, `HeroCtl::MyList`, `HeroSet.list`, `HeroWidths.list`, trailing My List pill — account-backed list toggle, mutually exclusive with upstream's plex.tv Watchlist because `catalog_hex_of_guid` only accepts `plex://` guids. |
| `rust-modules/screens/src/detail/hero_watchlist_tests.rs`, `no_driver_tests.rs` | Upstream's watchlist/no-driver sweeps | Added `list: false` / `list: 0.0` to their literals — the fork's extra hero field must be initialised in upstream's tests. |

## Upstream files edited after the merge (for future merges)

These were touched only to keep the fork's own code building against newer upstream files;
each is a place to watch on the next upstream merge.

- `rust-modules/screens/src/detail/hero_watchlist_tests.rs`,
  `rust-modules/screens/src/detail/no_driver_tests.rs` — added `list: false` / `list: 0.0`
  because NuvioNative's `HeroSet`/`HeroWidths` carry an extra My List field.
- `rust-modules/screens/src/preferences_tests.rs` — added `PickerKind::RatingSource` to the
  expected Playback field list (the Nuvio IMDb/RT picker).
- `ci/arm-cc.py` — drops rustc 1.100's `-fuse-ld=lld` for cargo-driven links; the
  evidence checker parses GNU ld maps, so without this the storage helper cannot link
  on this host. Reproduced on the original fork commit before changing the wrapper.
- `locales/{ca,de,fr,hu,it,nl,pl,pt,ru}/{browse,settings}.json` (18 files, JSON permits no
  comment) — added the fork's 12 pre-existing Nuvio keys (My List, stream picker, rating
  source) with the English wording verbatim. Upstream's catalog build requires every
  English key in every locale; no new wording or branding was introduced. These are the
  keys to re-check on every upstream locale change.

## In-app DTS / TrueHD / Opus conversion (Fork Rofocale11, 2026-11-22)

All of these carry `// Fork Rofocale11:` comments inline. Re-check each on every upstream merge:

- `rust-modules/os/src/devcaps.rs` — `AppDecoded::auto_convert` opt-in on the DTS, TrueHD and new
  Opus rows; `auto_converts_codec` / `auto_convert_codecs` helpers; an Auto arm in
  `Caps::app_decodes_lossy_track`. Upstream's policy (`AppLossy`) is unchanged: the fork's arm is
  independent of a measured table row and of the debug trigger, but a set that decodes natively
  still does.
- `rust-modules/src/app/nuvio.rs` — the shim's `set_audio_codecs` includes the converted codecs,
  and the prober marks such tracks `decodable`. This is the real Nuvio gate: without it the shim
  refuses "Audio format not supported" before the player opens the stream. The shim does not use
  a PMS transcode profile, so no `plex/transcoder.rs` change is needed here.
- `rust-modules/os/src/devcaps.rs` also carries an AtomicBool opt-in
  (`fork_enable_auto_convert`), set only by the Nuvio boot; upstream tests never set it, so Plex
  behavior and its characterization tests stay byte-for-byte unchanged.
- `docs/nuvio/{PLAYBACK,HANDOFF,VALIDATION}.md` — HTML comment markers replace the old
  "no DTS/TrueHD conversion" limitation.

## Post-merge port to upstream APIs (plx_media)

The fork's retained FFmpeg core needed adapting to the APIs upstream moved:
- `media/src/ff.rs`: unconditional std/FFI imports (upstream had them `#[cfg(test)]`
  because its `ff.rs` is now a facade), `Venc` re-export aliased away from the fork's
  local struct, header/font caps restored locally, `ass_source::begin` adapted.
  `#![allow(dead_code)]` / `#![allow(hidden_glob_reexports)]` are required because the
  fork's ABI table shadows the codecs re-export and most callers moved to `plx_play_feed`.
- `media/src/ff_probe.rs`: ABI bring-up restored — the probe now loads and version-checks
  the fork's own dynlib table on first use, otherwise it would refuse every stream.
- `media/src/ff_subs.rs`: local `pts_ns` and subtitle cue submission mapped to the new
  typed `ass_source` / `CaptionRetention` API.
- `media/src/route/plan.rs`: `plx_platform` → `plx_os`, playqueue/metadata/decision
  signatures, `audio_direct_plays_with`, caps plumbing. Probe-first `build_stream`
  behavior is unchanged.
- The fork's duplicate HLS terminal stall guard was **not** kept in-crate: upstream has
  the identical rule in `plx_play_feed::ff::{StallGuard, arm_active_stall_guard}`, which
  is the live guard for the shipped demuxer. `ff_acquisition.rs` was removed as dead.

All adaptations carry `// Fork Rofocale11:` comments.

## Verification result

- `make check`: **146 steps all ok**, harness 753 tests OK, all 22 Rust test binaries
  green (notably `plx_media` 2183, `plx_screens` 2322, `plx_plex` 822, `plx_net` 338,
  app crate 1486). No test was skipped, ignored or weakened to get there.
- `SCREEN_SHAPES_PIN` was re-pinned for the merged Also-available shape (which gained
  `nuvio` and a stream-request correlation id).
- Three historical replay fixtures were re-recorded by the merge; their reviewed
  digests are consolidated in `tests/replay-historical-profiles.json`, and the
  stale doc claim about byte-for-byte unchanged fixtures was corrected.
- Three reach caps in the fork's shim are now flagged `status = open` by
  `ci/list-caps.ini` (200 streams, 40 subtitle refs, 20 cast/crew tags) — a known
  limitation, not a gate failure.

## Post-gate simulator fixes

- `Makefile`: `RUST_INPUTS`'s `find` now excludes colon-named files. A private
  `nuvio.local.env:Zone.Identifier` Windows artifact in `rust-modules/stremio/`
  made the prerequisite rule parse as multiple target patterns.
- `stremio/src/addon_http.rs`: upstream's new net guard refuses redirect-following
  requests that carry any caller header. Benign addon GETs now follow redirects
  with no headers (like upstream's public fetch); Nuvio account calls keep their
  `apikey`/`Authorization` headers and do not follow redirects, so tokens are
  never replayed to a redirect target. Before this fix every addon request was
  refused and Home landed with 0 shelves; after it the simulator lands 48 items
  across 4 shelves.
- `sim-linux` deliberately needs no host FFmpeg; the ARM `.so`s staged in `pkg/`
  are not loadable by the host simulator, so `ff: ... MISSING` in a plain
  `PLXNATIVE_APP_DIR=pkg` run is expected. For verification here,
  `HOST=1 ./ci/build-ffmpeg.sh` was rebuilt in this checkout. The four x86-64
  libraries were staged into `pkg/` under distinct loader names
  (`libavutil-plx-host.so.61`, etc.) added to `media-codecs/src/ffi.rs`'s candidate
  lists, so the ARM `.so`s used by `make`/the ipk stay untouched. With the normal
  `PLXNATIVE_APP_DIR=$(pwd)/pkg PLXNATIVE_RUNTIME_DIR=/tmp/plxnative-sim` launch,
  the sim logs `ff: bound avutil/avcodec/avformat/swscale -> ...-host.so.*` and
  lands 336 items / 28 shelves from 11 addons, with no redirect refusals.
  The fork's probe has its OWN ABI table in `media/src/ff.rs`; it needed the same
  host candidate names, or `ff::probe` refused every addon stream even though
  `media-codecs` reported FFmpeg bound. A direct sim launch also needs the
  `/tmp/plxnative-sim/plxnative-token` dev-token trigger, otherwise it boots the
  plex.tv sign-in flow instead of registering the Nuvio shim.
  `ci/stage-host-ffmpeg.sh` remains macOS-only (`.dylib` + `install_name_tool`),
  so this Linux staging is manual and the staged binaries are untracked.

## Device verification (owner-run, debug install)

Deployed with `make FLAVOR=debug deploy TV=<address>`; `verify-deploy: 48 files match`.

**Set:** LG `45UR7500PSC`, board `K8LPN_DVB`, webOS `10.3.1` (codename
`ponytail-papikonda`), PowerVR B-Series BXE-4-32. The debug install boot line was
`install: id=com.nuvionative.app.debug flavour=debug … features=dev`.

**Evidence from the owner's event log** (hostnames and token values omitted):

- `nuvio: shim serving …`, `plex: server slot 1 registered at 127.0.0.1:32420`;
  the Nuvio QR sign-in completed (`auth: reached "Nuvio" … via 127.0.0.1:32420`)
  and the account pulled 11 addons (Cinemeta, Streaming Catalogs, Sootio, Meteor,
  subtitle addons, Unraid, Torrentio, BetterPosters).
- `ff: bound avutil/avcodec/avformat/swscale -> libav*-plx.so.*` — the ARM FFmpeg
  libraries bound on the device.
- `nuvio: probe hevc 3840x2160 fps=23.976 dovi=false audio=4 subs=4`, then
  `decision: part=directplay … -> DIRECT PLAY`, `SMP: Load returned ok=1`,
  `vplane: exported window placed` and `videoplane: BOUND`.
- `ff: open https status=200 …`, `ff: container names 8/8 tracks`,
  `ff: v=#0 codec=hevc … 3840x2160`, `smp_cb type=4 … hdrType:HDR10`,
  `feed: presented`, repeated `sink: displayed` (type=49), and
  `timeline playing t=5s/10606s` advancing through `t=135s/10606s`.
- A side-loaded subtitle track fetched and prepared (`sidecar: … ready, 1917 cues`,
  `assperf … late=0 stale=0`).
- Clean stop: `timeline stopped t=144s/10606s ok=1`,
  `native callback gate: epoch=1 intercepted=1455 dropped=0 proven=1`,
  `native lifecycle: off-frame Unload epoch=1 released 510 ms after the stop`,
  `videoplane: unbound`; no crash-tracer line in the log.

**Scope of the claim:** direct play of a probed addon stream with the hardware
video plane works on this set. This run played an HDR10 item (`dovi=false` on the
probe), so it does **not** by itself prove Dolby Vision P8, seek/resume cases, or
long-duration stability.

**Observed in the same session, not attributed to the merge:** poster art
`GET /photo/:/transcode status=404`, repeated `curl rc=28` timeouts to a subtitle
provider host, `marker-grants: seat=owner unavailable (Refused)` in the automated
boot, and brief `main-thread hang` lines during heavy concurrent probing.
Playback itself advanced continuously for the whole observed interval.

## Follow-up: the probe is now `plx_nuvio_probe`

The merge left the 383-line fork probe (`ff_probe.rs`) declared from upstream's
`media/src/ff.rs` with `#[path = "ff_probe.rs"] pub mod probe;` and `use super::*`.
That is the single largest source of repeated merge pain: any upstream rewrite of
`ff.rs` changes the names the probe borrows, and the probe carried a **second** FFmpeg
`dynlib!` table. The two tables disagreed in the Linux simulator — `media-codecs`
reported FFmpeg bound while the probe refused every addon stream — which is why the
host-candidate names had to be fixed in both places.

The probe now lives in the fork-owned crate `rust-modules/nuvio-probe`
(`plx_nuvio_probe`), using `plx_media_codecs::ffi` as its only ABI table. The app's
adapter (`src/app/nuvio.rs`) calls `plx_nuvio_probe::probe_url` directly; `ff.rs`
only loses its fork-added module declaration, so a future merge has no fork code to
re-resolve there. The three local items that are not in the shared table
(`OFF_STREAM_DISPOSITION`, `OFF_STREAM_AVG_FRAME_RATE` — both pinned by
`ci/ffabi-assert.c` — and the AVIO seek constants) live in the new crate with a
comment explaining why they were not added to the upstream file. The transport is
the same `plx_net` byte-range source with the same `credential_transport_allowed`
policy as `media::curlio::PlexAuthorization`, bound locally to avoid a dependency
from the probe back up into the media layer.

**Not in this change:** the older duplicate ABI table still in `ff.rs` is also used
by the retained `Venc` capture path and by the media FFmpeg tests. Removing it means
migrating those callers to `plx_media_codecs::ffi` and is a separate, larger change.

Gates: `cargo check --workspace --all-features` clean; `plx_nuvio_probe` unit tests
3/3; `ci/check-module-layers.py` green (new `[nuvio_probe]` layer, mounted via
`[package.metadata.plx]`); `make check-nodef` clean; ARM `make` links. Two unrelated
pre-existing failures were observed and reproduced on the untouched baseline:
`plx_media::player::tests::the_feed_counts_a_gap_only_between_two_stamped_callbacks`
and `ci/check-deps.sh`'s `forks` ledger gap at `plex/servers.rs::register_with_client_id`.

## Agent instructions and non-commentable edits (single-store change)

- `AGENTS.md` is upstream-owned. The only fork edit in it is the two-line pointer under
  "Load the right context" that sends agents to `docs/nuvio/AGENTS.md`. It carries the literal
  `Fork Rofocale11:` marker, but is recorded here too because a future upstream rewrite of that
  section is the likeliest place for the pointer to be dropped. The fork rules themselves live in
  fork-owned `docs/nuvio/AGENTS.md`, never in the upstream file.
- `rust-modules/Cargo.lock` gained `redb 4.6.0` and `ring 0.17.14` edges for `plx_nuvio_store` and
  `plx_os`. Lockfiles cannot carry a comment; regenerate rather than hand-edit on merge.
- `ci/test_storage_service_package.py` was rewritten (not merely patched) to grade the *absence*
  of a storage helper. It is upstream-owned but carries the fork reason in its module docstring.
- The new `docs/nuvio/AGENTS.md` is fork-owned and needs no marker.

## TMDB metadata client (Fork Rofocale11)

New fork behaviour, confined to the shim's crate and fork-owned docs. The public API and field
shapes are pinned in `docs/nuvio/TMDB.md` so the shim lane can code against them.

- `rust-modules/stremio/src/addons_tmdb.rs` (NEW, fork-owned) — TMDB client with a 15-minute
  `(api_key, path)` cache and a local per-key `Condvar` single-flight (upstream's
  `plx_base::flight` does not exist here). No request is made when the key is unset. Module doc
  carries the `Fork Rofocale11:` marker.
- `rust-modules/stremio/src/lib.rs` — added `pub mod addons_tmdb; pub use addons_tmdb::*;` with
  an inline `Fork Rofocale11:` comment. Upstream has no TMDB module.
- `rust-modules/stremio/src/model.rs` — added `Meta::moviedb_id` (`moviedb_id` wire name,
  lenient `opt_u64` for number/string shapes) with an inline `Fork Rofocale11:` comment. Upstream's
  Plex model carries no TMDB id; the shim reads this before paying a `/find` round trip.
- `docs/nuvio/TMDB.md` (NEW, fork-owned doc) — needs no marker.

### Makefile edit (non-commentable per fork policy; recorded here)

`Makefile` is upstream-owned and its variable/hash lines cannot carry a stable inline marker, so
the edit is recorded here (inline `# Fork Rofocale11:` notes were added above each line too):

- `NUVIO_ENV_VARS` gained `TMDB_API_KEY` so `option_env!("TMDB_API_KEY")` in `plx_stremio` sees the
  key. The existing `$(if $($(v)),$(v)='$($(v))',)` means an unset key passes nothing to cargo —
  identical to upstream's build.
- `TELEMETRY_CFG` gained a conditional `$(if $(TMDB_API_KEY),|$(TMDB_API_KEY),)` field in the
  `printf` piped to `shasum`. The key is **hashed only**, never written to a tracked file. When the
  key is unset the field is empty and the `printf` output is byte-identical to upstream's four-field
  string, so the build stamp and behavior are unchanged; when set, the stamp changes so a
  configured binary is relinked rather than silently shared with an unconfigured one.

## Still to verify

- Dolby Vision P8 playback, seek/resume, and multi-hour stability on the device.
- Re-run the sim probe and one TV playback after the `plx_nuvio_probe` extraction.

