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
- `pkg/resources/ca/appinfo.json` and `pkg/resources/hu/appinfo.json` (JSON, no comment syntax):
  the localized tile title was still upstream's `PlxNative` while the other 13 locales said
  `Nuvio Native`; the package gate rejects a localized descriptor that un-badges a flavoured
  install. Set both to `Nuvio Native`.
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
- `ci/check-package.py` — excludes `*-plx-host.so.*` from the "distributed libraries" set.
  Those gitignored host FFmpeg libraries are staged into `pkg/` for the Linux simulator and are
  never in `APP_FILES`, so they cannot ship; counting them failed every local `make ipk` once
  host FFmpeg was staged. Carries an inline `Fork Rofocale11:` marker.
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

## Account preferences provided by the shim (Fork Rofocale11)

A real Plex account owns the audio/subtitle preference rows and a real PMS echoes them on
`/api/v2/user/profile`. Nuvio has neither, so the loopback shim provides that surface from the
one redb file; the app's unchanged account client — not a second preferences path — reads and
writes it. The contract is the one `tests/mock_pms.py` grades (`/user` embeds the same row as
`/user/profile`; PUT takes an empty body plus named query keys and returns the updated row).

- `rust-modules/pmsshim/src/shim_account.rs` (fork-heavy file, inline markers) — implements
  `GET`/`PUT /api/v2/user/profile` over `plx_nuvio_store::Table::AccountPrefs` with the mock's
  defaults and validation, and makes `/user`, `/home/users` and switch publish ONE identity:
  one Nuvio login is one Plex-style profile. The uuid is a fixed account identity rather than a
  live Nuvio profile id so an in-flight addon load at login cannot make the three answers disagree.
- `rust-modules/plex/src/plex/account/preferences.rs` (upstream-owned, inline markers) —
  **removed** the fork's earlier `Source::{Plex,Nuvio}` split and `nuvio_load`/`nuvio_save`, which
  read/wrote redb directly and bypassed the shim. `capture()` still refuses a request with neither
  an account token nor a registered local server; `load`/`save` now always use the account HTTP
  path, which `plex_tv()` already points at the shim on Nuvio. This structural field removal is
  recorded here because a future upstream merge may reintroduce the enum or a direct-store path.
  `PreferenceRequest::fixture_for_test` keeps its signature, so the screen/test crates are intact.

## Nuvio boot keeps the account authority (Fork Rofocale11)

The first device deploy of the account-preference change still showed Settings dimmed. The event
log showed why: `boot: dev token` — in this fork `dev::scenarios::dev_token()` returns
`local_server.token` on every Nuvio boot, so `SessionInit::captured_boot` classified it as
`DevPms`, wiped the stored credentials and (by upstream design) published no profile. Upstream
dims the rows when nobody is seated; the shim's account surface is unreachable from Settings until
a seat exists.

- `rust-modules/src/app/boot.rs` (upstream-owned, inline `Fork Rofocale11:` markers) — new pure
  `captured_nuvio_boot` keeps `BootstrapAuthority::Account` and the stored identity/extras; the
  boot chooses it whenever the shim is registered. The `dev_token` is still built so the shim
  stays the PMS, but a Nuvio boot skips the `ActivateDevBootstrap` branch and resumes the stored
  sign-in (or opens the Nuvio QR sign-in) instead. This changes the no-session first boot from
  Home to the QR screen, which is the point: a Settings value has to belong to a profile.
- `rust-modules/src/app/session_dev_bootstrap_tests.rs` (upstream-owned test file) — added
  `nuvio_boot_capture_keeps_the_account_authority_and_stored_identity`, contrasting the two
  captures. The machine itself is untouched: no new ledger site, no `cfg(test)` fork in `step`.

Verification: app crate 1490 lib + 3 host-interface + doc tests pass; `make check-nodef`,
`ci/check-deps.sh` and `ci/check-module-layers.py` green. Device re-deploy and sign-in not yet
verified at the time of writing.

## Account watch progress (Fork Rofocale11)

The shim now mirrors local resume points into the Nuvio account's per-profile watch history and
merges another device's rows back. The port came from the old fork's `053b7d982`, `33efb07ec` and
`6cc885a85`, re-implemented against this tree (the old `shim_pms.rs` is ~4x larger and its app had
generation polling this tree lacked). Every upstream-owned Rust file touched carries an inline
`Fork Rofocale11:` marker; the non-commentable Markdown edits are recorded here.

- `rust-modules/stremio/src/nuvio.rs` (upstream-owned, inline markers) — `WatchProgress` and the
  two `sync_*_watch_progress` RPCs beside My List; `content_id_sNe` episode keys and a guard that
  refuses the whole batch before any request.
- `rust-modules/pmsshim/src/shim_store.rs`, `shim_pms.rs`, `shim_core.rs`, `shim_account.rs`
  (fork-heavy, inline markers) — `account_keys` in the one redb watch record, newer-wins
  `merge_remote`, IMDb-first identity, the 60 s throttle, pull-on-boot/profile-switch, and the
  `content_generation` counter. `load_profile` now drops the `library` guard before firing the sink
  and installs addons before pulling history (both orderings are load-bearing).
- `rust-modules/src/app/{nuvio,boot,run}.rs` and `rust-modules/data/src/stores/deck.rs`
  (upstream-owned, inline markers) — expose the generation, capture its baseline BEFORE the first
  `/hubs`, re-read Home once per newly observed value (deferred until a server exists), and let a
  lane placed on an empty preview adopt later cards. No machine step performs IO: `playback_tick`
  polls a counter and emits the existing refetch effects.
- `docs/nuvio/{AGENTS,HANDOFF,WATCH-PROGRESS}.md` — Markdown has no comment syntax: added the new
  document of record and the delivery note here instead of inline markers.

Verification: focused host tests for `plx_stremio`, `plx_pmsshim`, `plx_data` and the app crate
pass; `cargo check --workspace` green. Cross-device behaviour on a real account is not yet verified.

## Last played source is local (Fork Rofocale11)

- `rust-modules/pmsshim/src/shim_store.rs` and `shim_pms.rs` (fork-heavy, inline markers) — the
  `Watch` record gains a `LastStream` fingerprint (addon + `info_hash`/`file_idx`, or direct
  filename/size/height; never a signed URL or arrival index), written only at a successful
  `/decision` probe and promoted in `ranked()` only when the viewer has not picked in the slot.
  Finish/scrobble/remove clear it; an account merge does not touch it. This is local-only state in
  the one redb watch record, so the old fork's `watch.json` wording is deliberately not carried over.
- `docs/nuvio/{HANDOFF,WATCH-PROGRESS}.md` — Markdown has no comment syntax: the new section and
  delivery note are recorded here instead of inline markers.

Verification: focused `plx_pmsshim` tests for the fingerprint's persistence, promotion and explicit
pick precedence; `cargo check --workspace` green. No device verification of a resume choosing the
same file after re-resolution.

## Version alignment after the upstream merge (2026-11-23)

The upstream merge moved `rust-modules/Cargo.toml` and `Cargo.lock` to 0.10.0 while the fork's
`pkg/appinfo.json` and `ipkroot/ctl/control` stayed at 0.8.0. `ci/check-package.py` requires all
version witnesses to agree, so no `make ipk` could pass on this tree. Bumped the two packaging
files to 0.10.0 with `ci/bump-version.py 0.10.0` (the repo's shared version writer); neither JSON
nor a Debian control file can carry an inline `Fork Rofocale11:` marker. Debug/stable package
filenames and the docs' historical 0.8.0 build records are from before this alignment.

## Catalog hub bounding lost in the upstream merge (2026-11-23)

The pre-merge fork's `shim_pms.rs` bounded catalog work so a slow or dead addon could not hold a
hub read: `/hubs` honoured the client's hub window, `catalog_hub` passed a `wait` budget into
`catalog_range`, the catalog cache used one single-flight fill worker with a `retry_at` backoff
instead of holding a mutex across network fetches, and a finished background warm bumped the
content generation so the app re-read a partial answer. Commit `06e15f82` replaced all four with
upstream's simpler unbounded version. The result on the set: `/hubs` took ~19 s cold (0.6 s once
cached), the app logged 7 × `hubs: source 0 FAILED` before one success, Home needed 10–15 minutes
to appear, and 8 `pmsshim-catalog` workers outliving the 4 s budget choked browsing.

Restored the bounded `wait` on `catalog_range` (callers pass the hub budget minus 100 ms), the
single-flight `filling`/`cv` fill worker with a 30 s `CATALOG_RETRY` backoff, and the
`bump_content()` after `warm()`. Hub-window paging and My List offset handling were deliberately
left unchanged to avoid a second behaviour change at the same time. A first port also bumped
`content_gen` from each completed catalog fill, copying the pre-merge fork's separate catalog
generation; on this tree that drove an endless Home re-read (`home: hubs refresh queued after
account pull`, FPS → 0) and was removed. All edits in `shim_pms.rs` carry inline `Fork Rofocale11:`
markers. Regression tests: `playback_regressions::a_dead_catalog_is_bounded_and_backed_off` (a dead
addon returns inside the budget and is not re-fetched during backoff) and
`nuvio_tmdb_tests::a_catalog_fill_does_not_bump_account_content`; `plx_pmsshim` 48/48.

The first corrected build still left Home spinning: bounding catalog fetches was not enough,
because `/hubs` itself did two synchronous network calls the pre-merge fork did not have —
`saved_hub` pulled the Nuvio account list, and `continue_rows` fetched a TMDB backdrop per
Continue Watching row. Both are now cached-only on the hub path: `Account::saved_library_cached()`
reads the 60 s My List cache (the dedicated `/hubs/nuvio/list` route and the background warm still
refresh it), and `continue_rows` uses `enrich_get(..).art` instead of a network lookup; the now-unused
`tmdb_backdrop_for` was removed. A cold `/hubs` no longer waits on those hosts. Built for validation
as a DEV ipk (`make -j10 ipk`, per the owner's preference) — all packaging gates passed.

A third pass restored the last dropped behavior: `catalog_hubs` now takes a hub window and returns
the whole filtered-list total, and `/hubs` plus `/hubs/sections/{k}` honour `X-Plex-Container-Start`
/ `X-Plex-Container-Size` and echo `offset` / `totalSize`. This is what made a Recommended page
re-fetch every catalog on every window and choke the TV; the old fork's own comment named it.
Regression test `a_hub_window_fetches_only_that_slice_of_catalogs`; `plx_pmsshim` 49/49.

A fourth pass restored the fork's frame-priority mechanism, which the merge had dropped whole.
`plx_base::task::yield_to_frame_thread()` (renices the calling worker to `+10`) is back in
`base/src/task.rs`, and `plx_pmsshim/Cargo.toml` regains the `plx_base` dep the pre-merge fork had
(the crate table in `ARCHITECTURE.md` is updated to match). It is called at the entry of every
speculative worker: the shim's per-connection handler, catalog fills, stream/subtitle fetches,
the pre-probe and warm threads, profile fetches, watch-state meta reads, and the poster
fetch/decode workers. The old fork's comment records the device measurement this answers — a cold
Library load at `loop=6, 8, 7, 8` with `budget=0/0`, i.e. the frame loop itself starved by fetch
workers sharing four cores — which is exactly the `budget=…/solo:residency`, `fps=8` signature seen
after the hub fix. All edits carry inline `Fork Rofocale11:` markers. `plx_pmsshim` 49/49,
`make check-nodef` clean, dev ipk rebuilt.

A fifth pass fixed the missing catalogs on the Recommended page. The merge had deleted the fork's
two-list distinction entirely: `Backend::all_catalogs()`, `AddonBackend::all_catalogs()` (backed by
`stremio::browse_catalogs()`), and `Shim::all_catalogs()` with its own cached `browse_list`. The
shim's `catalog_page()` also looked up keys only in `home_catalogs()`, so a `showInHome:false`
catalog could be named by the Library selector but never fetched. `catalog_hubs` now takes the
numbered list explicitly again: Home passes the full browse list filtered to Home-eligible keys,
Recommended passes the full browse list filtered by content type, and `/hubs/nuvio/{i}` indexes
that same full list — so a row's index names the catalog it previewed. This also removes the
`nuvio.catalog.10/12 read failed` churn seen in the foreground log. Regression test
`a_catalog_hidden_from_home_still_gets_a_recommended_shelf`; `plx_pmsshim` 50/50.

A sixth pass ported the rest of the old fork's fix for bare headings (`8acf0953e`, whose message
names these exact symptoms). A CSV listing read (`/library/metadata/{rk1},{rk2},…`) re-resolved
every card through addon `meta` — measured 9.2 s for 12 cards, past the TV's 15 s loopback deadline,
so the row read failed and left a heading with a spinner while churning the UI. `Shim` now keeps a
bounded `cards` map; `remember_card` is called from `preview_row_raw` (where hub rows are built,
not merely `row()`), and the CSV route answers from it, falling back to the addon only for a key
never shown. Regression test `a_listing_read_answers_from_cards_already_built`; `plx_pmsshim` 51/51.

A seventh pass enforced the owner's artwork rule: hub posters are BetterPosters by IMDb id with the
addon's Cinemeta poster as the STRICT fallback, and TMDB is never consulted on that path. The merge
had added `imdb_base()` (TMDB `/external_ids`) inside `poster_url`, which resolved `tmdb:` catalog
ids through TMDB while building a hub. `poster_url` now only accepts an IMDb id already on the
preview or its `Meta`; otherwise it keeps the addon art. TMDB remains confined to the detail page
and its children (cast, logo, `art`, trailers, More Like This, collections). The existing
`poster_url_resolves_imdb_ids_and_keeps_addon_art_otherwise` test still covers both branches.

An eighth pass restored the rest of the old fork's artwork and TMDB/ID surface, adapted to this
repo's one redb store and `plx_os` platform seam:

- `blur_colors`, `UltraBlurColors` on preview/season/episode/collection/Continue Watching rows, the
  addon's `logo` as the hub row's `clearLogo`, and the MetaHub `small` poster ladder (the measured
  `solo:residency` fix at card width).
- `crop_ink` / `logo_png` / `logo_path_key`, logo-only PNG cropping cached in `plx_os::imgcache`;
  the `photo` route returns a retryable 503 when a row is not ready instead of latching a 404.
- `TmdbClient::from_env`, `credits`, `person_credits`, `external_ids`, `videos`, `recommendations`,
  the `series_id` fallback, and the richer `TmdbCredit`/`TmdbMovie`/`TmdbShow`/`TmdbPerson`/
  `TmdbGenre` types with `TmdbCredit::movie()` / `::show()`.
- Filmography rows built from the credit's WORK, sorted newest first/undated last, remembered for
  the batched caption read.
- `docs/nuvio/ID-MANAGEMENT.md` restored (redb-adapted) and `TMDB.md` updated with the artwork rule.

`plx_pmsshim` 53/53, `plx_stremio` 50/50, `make check-nodef` clean, dev ipk built and deployed.

A ninth pass fixed the two reported page failures. The person page was dialling
`discover.provider.plex.tv` and getting **401** — the merge had dropped the old fork's
`local_discover()` first hop in `plex/account.rs::discover()`, which routes Discover reads to the
installed in-process shim. Restored `local_discover()` / `resolve_local_base()` (plain loopback http
only), included it in `stand_in_armed()` and the read gate, with the old fork's loopback-only test.

On a related title the shim short-circuited `tmdb_synthetic_row()` to a remembered bare fallback,
losing cast, Extras and summary; it now always rebuilds the full detail row as the old fork did
(the client caches details for 15 min). `tmdb_details_row` / `tmdb_show_details_row` publish the
overview as `summary`, TMDB poster and Genre tags again; fallback rows carry them too. Added
`warm_synthetic_streams` so clicking a related movie starts the addon stream fetch on detail open,
and switched `/library/people/{id}/media` to the cast-only `person_credits`.

A tenth pass made Recommended posters use BetterPoster too, per the owner's explicit decision. Home
gets Cinemeta `tt…` ids; Recommended's extra "Streaming Catalogs" catalogs carry `tmdb:<n>` ids, so
the `tt…`-only gate left them on the addon poster. `poster_url` now calls `imdb_base` for those: the
one redb store is read first, a cold miss makes ONE cached TMDB `/external_ids` call (positive and
negative answers cached under `IMDB_TTL`, `TmdbClient` keyed single-flight shares concurrent
misses), and all later reads are redb-only. No TMDB image is ever used for a hub. `TMDB.md` and
`ID-MANAGEMENT.md` record the rule; `plx_pmsshim` 53/53.

An eleventh pass traced the remaining spinner/mixed-art symptom past the shim entirely: every hub
thumb is BetterPoster and the `/photo` bytes are correct, but `plx_ui::tex` reported
`budget …/solo:residency` with `residency lost … rearmed … refused …`. The old fork caps an
oversized CDN decode to the card box and pins the MRU/recent Residency entry; this tree had
neither. Ported `request_box` / `cap_dims` / `downscale_rgba` into the poster worker (a 500×750
btttr.cc card now decodes at 250×375, an ordinary `Poster` take), `RECENT_RESIDENCY_GRACE` +
`mru_residency` into `victim_ok`, and restored `PT_CAP` 96. The temporary `TRACE photo` logging
used for the diagnosis was removed. `plx_ui` 1761/1761, focused poster cap test passes.

A twelfth pass tried the requested wholesale swap — copying the old fork's whole `pmsshim/src`
(including `shim_trailer.rs`) over this tree's and rewiring its file caches onto `plx_nuvio_store`.
It was reverted, not shipped: the old shim is not self-contained. It fails `cargo check -p
plx_pmsshim` with 56 errors because it calls `plx_base::flight` (7.5 KB module absent here),
`plx_stremio::sbengine` (32 KB in-process C engine that would REPLACE this tree's EngineFS torrent
path), `addons::opensubtitles_candidates`, the old flattened `TmdbMovieDetails`/`TmdbShowDetails`
shape, `Meta::trailer_streams`, `Subtitle::release_title/provider`, `libc`, and `plx_platform`
(renamed `plx_os` here). Making it build means porting those crates too, not just the shim. The
old and current shims' `/photo` handler and `poster_url`/`poster_fallbacks` logic are also
effectively identical, so the swap would not have changed the mixed-poster symptom. The tree was
restored from a byte-exact backup and `cargo check -p plx_pmsshim` passes again.

A thirteenth pass shipped the requested port after all: the old fork's whole `pmsshim/src` is now
in this tree, with its **behavior** left intact and only its storage/API boundary adapted:

- Redb: `Keys`, `WatchState`, `MetaCache` and `EnrichCache` now persist in the one `nuvio.redb`
  (legacy TSV/JSON/JSONL migrated once); account session/preferences likewise. Added
  `plx_nuvio_store::clear_table`.
- Missing APIs adapted at the boundary, not ported as second stacks: a local `shim_flight` (the old
  `base::flight`), `shim_sbengine` backed by this tree's `plx_stremio::torrent`/EngineFS, a local
  `opensubtitles_candidates` in `plx_stremio::addons`, `plx_os::{imgcache,devcaps}`, `libc`, and
  the old nested `TmdbMovieDetails`/`TmdbShowDetails` shape plus `Meta::trailer_streams` and the
  fuller `Subtitle`. Old file-based tests were rewritten to assert redb rows and to serialise on
  the process-wide store.
- Deliberately **not** changed: `poster_url` still passes the item's own id to the provider (old
  fork behavior), and a regression test pins it. The old `/photo` and `poster_fallbacks` paths are
  unchanged.

Verification: workspace `cargo check` clean, `plx_pmsshim` 154/154, `plx_stremio` 50/50,
`plx_nuvio_store` 3/3, `plxnative-modules --lib` 1492/1492, `make check-nodef` clean. The owner
confirmed afterwards that everything is fixed on device. `docs/nuvio/AGENTS.md` now records that
the old fork's shim behavior is authoritative for future merges.

## Still to verify

- Dolby Vision P8 playback, seek/resume, and multi-hour stability on the device.
- Re-run the sim probe and one TV playback after the `plx_nuvio_probe` extraction.


## Fourteenth pass — the torrent engine and trailer service

The owner asked for the remaining old-fork pieces as well: `torrent.rs`, the C engine and its
`ci/build-sbengine.sh`, and the `js-service/trailer` children. This replaced the temporary
`shim_sbengine` adapter with the real stack:

- `plx_stremio::sbengine` is the old fork's FFI module verbatim (one `sbe_get` symbol through
  `plx_base::dynlib!`, nested `Tmdb*` details untouched elsewhere). `plx_pmsshim` now calls it
  directly; the adapter is gone. `torrent.rs` is back to the old rule: always `POST /{hash}/create`
  before playback, with its regression tests.
- `boot.rs` starts/stops the engine (`p2p_state_candidates` / `p2p_state_dir_is_private` ported into
  `plx_base::paths`), `run.rs` stops it on leaving the player and at shutdown. `plx_os` replaced
  upstream's `platform` edge in `ci/module-layers.ini`, and the new shim/engine modules are listed.
- Build/packaging: `ci/build-sbengine.sh`, `ci/build-openssl.sh`, `ci/cmake-toolchains/webos-arm.cmake`
  and `vendor/engine-sources/` ported; `openssl` added to `ci/dependencies.json`. The Makefile stages
  `libsbengine.so` and the two trailer scripts in `deploy` and `ipk`, verifies them, and pins the
  engine digest in `unreceipted.sha256`; `ci/check-packaged-elf.py` accepts digest-pinned ELFs.
  CI builds the engine before packaging and asserts it is staged. The trailer `server.js` is NOT
  shipped — the engine is the only streaming path, exactly as the old fork retired it.
- Built from source locally: `build/sbengine/libsbengine.so` (ARMv7 soft-float, `sbe_get` exported,
  no libssl/libstdc++ in DT_NEEDED). Debug `make -j10 ipk` passes every gate: 7 receipted + 1
  digest-pinned ELF. `nextest` 1981/1981 across the affected crates, `check-nodef`, module layers
  and `check-deps`, `make lint` and the ledger ratchet all green. `tools/host-engine-check.sh`
  (host build of the same engine plus its loopback seeder, running the two `#[ignore]`d real-engine
  shim tests) was ported and both checks pass on this machine: the real engine held `/decision`
  silent for 30.04 s past the 15 s media budget and still answered, and a loopback-seeded torrent
  direct-played on the FIRST decision with 262144 bytes served through the engine byte-identical
  to the source file. The host tool builds its own pinned OpenSSL 1.1.1w and points CMake at the
  real `libz.so` because this host ships OpenSSL 4.1 and no standard multiarch zlib symlink.
  Nothing deployed or committed for this pass.

## Fifteenth pass — enable torrent offers when the engine is present

The engine boot was wired but the shim's `Config::p2p_enabled` still defaulted to `false`, and this
fork has no boot-time `restore_p2p` or Settings switch to turn it on. `torrent_server()` therefore
returned `None` and `shim_media::rank` dropped every torrent/magnet stream before the engine could
resolve one. `app/nuvio.rs` now sets `p2p_enabled: plx_stremio::sbengine::available()`, so torrent
picks are offered exactly when `libsbengine.so` actually loads; a build without it still hides
unplayable picks. This is the cheap enable, not the Settings-toggle port — the persisted
preference and UI row remain unported. `nextest` 1646/1646 on the affected crates; workspace check
clean.

**Trailer DASH is ported (fifteenth pass).** The player-side pieces from the old fork are now
present: the strict 529-line SegmentBase MPD parser in the new fork-owned crate `plx_nuvio_dash`
(pure Rust, no dependencies), the `.mpd` → two progressive fMP4 tracks demux in the new file
`play-feed/src/dash.rs`, and boot's `set_trailer_reacher` / `probes.attach` in `app/nuvio.rs`.
Porting was not a verbatim copy: this fork moved the physical demuxer to `plx_play_feed`, so the
demux was adapted to its session effects (file size, duration, raster and tails publish as
`DemuxEffect`, not direct `SHARED` atomics), two-lane curl transport and `hls::Resource` API.

**Trailer audio pacing (device log, "plays then cuts out").** The DASH demux originally pushed
one audio AU per video frame, mirroring the old fork. A 4K VP9 frame is ~42 ms but an AAC AU is
~23.2 ms, so audio media time fell behind video from the first second (~16 s by the end of a
150 s trailer) and Starfish's audio clock ran out of data. The audio lane now reads AUs until its
pts reaches the last video AU's pts (bounded to 12 AUs per demux iteration), with a pure
`audio_needs_catch_up` predicate and three host tests. This is a NEW rule, not an old-fork port:
the old loop had the same 1:1 cadence. The old fork's actual audio-sync fixes — `#1343`
(`audio_stale_ahead`, counted drops) and `#874` (`PushGate`) — are already present in this tree,
and `pts_ns`/ADTS framing in the DASH demux are byte-for-byte the old code. The old fork's
`9ccab8649` (`PrimeProfile::Preview`: start the audioSync master clock only after an audio AU is
accepted) is NOT ported — this tree's prime implementation is upstream's and has no preview
profile; the device log showed audio accepted before the sink was configured, so it was not the
cause here.

**The lost upstream arm that made all of the above unreachable.** The first device run after the
port still failed: the log showed no `dash:` line at all — the planner sent the trailer Part to
`/video/:/transcode/universal/start.mkv` (`directPlay=0&directStream=1`), and the shim answered a
90-byte "no transcoder" body. `plan.rs::part_is_streamable` accepted only `.mkv`/`.mp4`/`.m4v`;
the old fork also accepted `.mpd`, and that arm — plus its test — was dropped by an earlier upstream
merge. Restored both (with a `Fork Rofocale11:` marker). This is the exact merge hazard the new
crate layout is meant to shrink: a one-line arm in an upstream file can silently disable a whole
fork feature, so the contract now has tests at both ends (generator→parser, and part-key→route).

**What an upstream merge touches.** The edits to upstream-owned files for this feature are:
two comments in `play-feed/src/ff.rs` (a `#[path = "dash.rs"] mod dash;` declaration and the
`.mpd` routing branch in `demux`), the restored `.mpd` arm in `media/src/route/plan.rs` and its
test, one dependency line each in `play-feed/Cargo.toml` and the workspace manifest, and two `uses`
edges in `ci/module-layers.ini`. Everything else is new files:
`rust-modules/nuvio-dash/` and `rust-modules/play-feed/src/dash.rs`. As a child module of `ff`,
the demux borrows the private AVIO helpers (`packet_to_annexb`, `parse_extradata`, `adts_params`,
`HlsInput`, `read_cb`…) without making any of them `pub`, so an upstream rewrite of `ff.rs` cannot
drag fork code into the merge. The generator/parser contract is pinned by a test that feeds the
MPD `shim_pms::dash_manifest` actually emits to `plx_nuvio_dash::parse_manifest`.

## Sixteenth pass — the Watchlist page reads My List (2026-11-23)

Clicking the Watchlist pill read "Can't reach your Watchlist" because the page still used upstream's
`LiveWatchlistIo` → `discover.provider.plex.tv/library/sections/watchlist/all` with a Nuvio token
(HTTP 401). **This is not the account watch-progress feature recorded above**: that is Continue
Watching (`/:timeline` → `sync_*_watch_progress`), which was ported earlier. My List itself is
**not in redb** — `plx_stremio::nuvio::saved_library` pages the Nuvio backend's `sync_pull_library`
RPC and the shim keeps a 60-second in-memory cache. redb holds the session, preferences, watch
state, key table and metadata caches; the shim's `my_list_entries` / `set_my_list_saved` are the
existing door to My List.

Ported the old fork's 2026-11-02 tier-C fix (`UPSTREAM-LOG.md` there), adapted to this tree's
Sans-IO job design:

- **New fork-owned `rust-modules/src/app/watchlist_io.rs`** — `MyListIo` implements this tree's
  `WatchlistIo` (`page`, `released`, `write`, `find`, generations) from `Shim::my_list_entries` /
  `set_my_list_saved`. Page 0 refreshes the account cache; every row is reported as held by the
  shim, so a tile is playable and opens the shim-backed Detail; writes map the 12-byte id back to
  a shim ratingKey. Three host tests pin the identity, guid form and series/movie kind.
- `rust-modules/src/app/adapters/stores.rs` — the one production IO construction now builds
  `MyListIo` with the captured snapshot instead of `LiveWatchlistIo`. Fork marker inline.
- `rust-modules/src/app/bridge.rs` — `watchlist_input` seats a local identity when no plex.tv
  token exists but the Nuvio shim is signed in; otherwise `WatchlistStore::open` stops at
  `NoAccount` before any read. The shim IO ignores those credential fields; no request is built
  from them. Fork marker inline.
- `rust-modules/src/app/mod.rs` — new `mod watchlist_io;` (fork marker inline).
- `rust-modules/plex/src/plex/models.rs` — restored `NUVIO_HEX_MARK`, `hex_of_rating_key`,
  `rating_key_of_hex` and `catalog_hex_of_item`, and routed `guid_hex` / `show_guid_hex` through
  the latter so shim rows carry the same `NVIO`+ratingKey id as Watchlist tiles; without it the
  Detail/Home disc and the hold-menu rows cannot agree with the page. Fork markers inline.

Upstream file surface on the next merge: the four inline edits above plus the new fork-owned file.
The Discover catalog page (a list title with no server copy) still uses upstream's plex.tv read in
both forks and is not part of this port. Verification: full nextest 1495/1495, `make check-nodef`,
module layers, `check-deps` and ledger ratchet all green. No device verification yet.

**Device follow-up — the Detail disc was still gated on a plex guid.** The Watchlist page worked on
the set, but Detail showed the older named "Add/Remove My List" button instead of the bookmark disc,
and no menu Add/Remove row. Two gates in `screens/src/detail/mod.rs` still used
`catalog_hex_of_guid(&d.guid)`, which refuses a shim's `nuvio://...` guid: the cached
`watchlist_hex` the app feeds the disc, and `HeroSet::watchlist`. Both now use
`catalog_hex_of_item(&d.guid, &d.rk)` (fork markers inline), and `HeroSet::list` stands down when
the disc exists — the old fork had ONE My List control, not two. The disc's press also had to be
keyed by the resolved NVIO hex (`hero::press` decodes a `plex://` guid tail, which a `nuvio://`
guid does not carry); its Add/Remove actions now name the same id as the Watchlist store's writes.
Regression test `a_shim_movie_gets_the_watchlist_disc_and_a_working_press` pins all three.

A workspace-wide `--workspace --lib` run has six failures that are NOT from this change: four
(`the_feed_counts_a_gap_only_between_two_stamped_callbacks` and the three transcoder lossy-gate
tests) reproduce unchanged on a detached worktree at `b26d45dd`; the other two are order-dependent
and pass in isolation. The project's default nextest scope is green.

**Retiring the old named My List UI once the disc worked.** With the disc now the only My List
control, the fork's earlier `HeroCtl::MyList` / `ELEM_MY_LIST` / `HeroSet::list` / `HeroWidths::list`
button was removed from `screens/src/detail/hero.rs` and `detail/mod.rs`: no variant, element id,
focus stop, measured width, label (`list_label`), draw arm, press arm or a11y entry. Its two UI
tests were removed and the remaining `HeroSet` literals in detail tests no longer initialise a
`list` bit. This is a UI-only removal: `DetailScreen::SHAPE` still names `list_state`,
`list_pending` and `list_error`, and `state_fp()` hashes every screen SHAPE into the pinned replay
fingerprints, so those fields, their addressed `NuvioReq::MyList` read, its landing and the
`browse.list.*` locale data stay. They are marked in code as legacy replay state, never as a second
Watchlist implementation. Removing them properly means re-recording the V13 anchor and historical
replay profiles, which is a separate change.

**Two-level Streams picker (2026-11-23).** The old fork's Detail Streams menu was a drill-in: first
the addons that answered for the item, then one addon's ranked streams, picked with the same
`plx_ui::page_stack::PageStack` + `plx_ui::panel_motion::PanelMotion` the player's Subtitles/More
menus and the Library menu stand on. That UI was lost in the merged tree, which left a flat stream
list; it has now been restored **without the old fork's IO**. The old `screens/src/alt_sources.rs`
started two workers from the screen (`task::spawn` at old `request_streams` and `commit`), slept on
a thread, read `client_for` in `new()` and on both workers, locked an `Arc<Mutex<Option<StreamReply>>>`
mailbox in `Tick`, and wrote the process-global `route::set_available_streams` from that step. The
merged tree's `AppFx::Nuvio(NuvioReq::Streams/Select)` effects, the app's `execute_nuvio` worker and
the addressed `ScreenEvent::Async` answers with a `RequestId` fence are kept unchanged; the screen
only builds forms, steps motion from `Tick` and emits effects. The pure helpers that were also lost
(`source_list::addon_pages`, `addon_name`, `untagged_source`, `seeders_fact`, `stream_row_with` and
`StreamChoice.seeders`, which the shim already sends) were ported back into `appkit`/`plex`, and the
`browse.streams.other` locale key into every shipped language. `NuvioStreamSelected` now starts the
picked stream immediately through Detail's `play_hero` (the old fork's behavior) instead of only
re-reading the item.

This changes `AltSourcesScreen::SHAPE`, so both shape pins moved and **the committed replay
fixtures need `tools/plxnative-rec rerecord` on a macOS simulator** before `make check-replay` is
trusted; the Linux host cannot do that step. Host evidence for the change: 33 focused `alt_sources`
tests (including six pinned two-level tests, the all-language fit test and the title-band pop),
a Detail test that a pick emits a Play effect, full nextest 1495/1495, `make check-nodef`, module
layers, `check-deps`, ledger ratchet. Native playback and the slide animation still need a TV run
under the TV lock; the host suite proves the state transitions and effect payloads, not device
frame pacing.

**Post-deploy fix — BACK no longer snaps the addon list to the top.** `AltSourcesScreen::reconcile`
fell back to `form.opening_key()` when the engine's focus key was from the page that had just been
popped, which re-seated the first row (and scrolled the list back to the top) even though
`pop_stream_page` had restored the cursor onto the opener. It now uses the selected row first, the
same rule `LibraryMenuScreen::reconcile` uses for its Genre drill-in. Regression test
`a_pop_reconciles_focus_to_the_opener_not_the_first_row` pins a stale second-level focus key through
the pop.

**Episode rows open the Streams picker on Nuvio shows (2026-11-23).** `ContentPanel::AltSources`
carries an optional episode ratingKey (the panel is no longer `Copy`), `ContentPanel::surface`
uses it as the picker's subject, and Detail keeps an unhashed `streams_episode` latch. On a Nuvio
show BOTH episode rows — the still (`episodes::Action::Play`) and the title
(`Action::OpenDetail`) — now open the same two-level picker for THAT episode instead of playing
`Media[0]` or pushing its detail page; the addressed `NuvioStreamSelected` answer then calls
`play_episode_at` for the latched episode, or `play_hero` when the picker came from the hero pill.
The Nuvio gate is the pure `is_nuvio_item(d)` guid check, not the old fork's `client_for` in a
screen step. Plain Plex episodes keep direct play and their detail page. Three tests pin the two
rows, the latch and the hero fallback; full nextest, `check-nodef` and `check-deps` are green.
