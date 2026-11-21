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
- `locales/{ca,de,fr,hu,it,nl,pl,pt,ru}/{browse,settings}.json` (18 files, JSON permits no
  comment) — added the fork's 12 pre-existing Nuvio keys (My List, stream picker, rating
  source) with the English wording verbatim. Upstream's catalog build requires every
  English key in every locale; no new wording or branding was introduced. These are the
  keys to re-check on every upstream locale change.

## Still to verify

- `SCREEN_SHAPES_PIN` in `registry.rs` is upstream's value; the merged tree needs a
  re-pin and replay re-record.
- Compiler diagnostics from the fork code retained in `media/src/ff.rs` and
  `media/src/route/plan.rs`.
- `make check`, `make check-nodef`, ARM build and UI capture. A host pass is not a
  device proof.
