# Nuvio Native agent scenarios

Fork Rofocale11. A working list of **fork-specific** things an agent can reproduce or debug, derived
from the 84 fork commits after the upstream merge `06e15f824` (merge base `0a1edc61`) and from
`MERGE-AUDIT.md`, `VALIDATION.md`, `HANDOFF.md` and the subsystem docs. It is a map, not a claim that
each item has been run. Before calling one reproduced, run it and cite its log/test output. After
an upstream merge, the ordered gate is `MERGE-REGRESSION.md`.

## Classes

- **A — synthetic boot.** Reproducible with `plxnative-nuviofake`: the real in-process shim over
  `FakeBackend`, no Nuvio account, private empty store. Recipe and fake ratingKeys:
  `SYNTHETIC-DEBUG.md`. **All A items currently depend on the uncommitted synthetic work in this
  checkout** (`rust-modules/src/app/nuvio/`, `nuvio-store` override, `pmsshim` gates, three marked
  branches in `app/boot.rs`). Check `git status` before assuming another checkout has it.
- **B — host / simulator / fixture.** No Nuvio account needed. Unit tests, `hostsim`, or the
  `tests/fixtures` media generator plus the `tests/run.py` pipe tier. A fixture-driven TV run still
  needs the TV lock, but no account and no real addon content.
- **C — real account, addons, network or TV.** The owner's domain. An agent can prepare and observe
  these, but must not sign into a real account or write to one. Documented here so the boundary is
  visible, not as agent-owned work.

Every device command needs `tv-lock`: `tools/tv-lock.sh status` first, an `IN USE:` line means stop
and ask the owner, build with the lease released, acquire a short lease, `down` then `release` after
the run. Never press OK on a title whose `ratingKey` you have not read from `state` or the log.

## A — synthetic boot scenarios

| Scenario | What it proves | How to drive it |
| --- | --- | --- |
| Account isolation | A signed-in TV does not leak its session, Continue Watching, PINs or addons into a headless run | `tools/tv-session.sh up --screen home --arm nuviofake`; expect `nuvio: SYNTHETIC private redb store`, `hubs: … 0 in CW`, `/api/v2/user` = local `Nuvio Native`, `state` `owner=page`, no `coldopen screen=account` |
| Fixed catalog and detail | Home/Library hubs, item detail and search through the real shim with no addons | Read keys from a host `pmsshim-serve --fake` (see `SYNTHETIC-DEBUG.md`); `--screen library=1`, `library=2`, `detail=10002` with `--arm nuviofake` |
| Movie hero Streams pill | Two-level addon/source picker from a movie | `--screen detail=10002 --arm nuviofake`, `press detail.hero.alt`, `press alt.row.0`, `state` → `1080p AVC WEB-DL AAC · 150 MB · Fake` |
| Episode / player More → Streams | Same picker through an episode and the player overlay | `--screen detail=10001 --arm nuviofake`, `press detail.hero.play --rk <state subject>`, `press player.control.more`, `press more.streams`, `press more.addon.0` |
| Warm detail latency phases | The app/shim phase log lines exist and a re-open is cheap | Open `detail=10002`, then re-open after a >750 ms focus rest; read `detail: shim …` and `detail: sid=…` lines. **Cold-first-open figures are class C.** |
| Rating-source picker UI | The preference page is reachable under synthetic content | `plxnative-settings=picker-ratings` beside `plxnative-nuviofake` (see `rust-modules/src/dev/scenarios.rs:1345`); the preference WRITE is account-scoped and inert here |
| Selector / store unit proof | The trigger, backend choice, config and store override work | `make test-crate C=plx_nuvio_store`, `C=plxnative-modules`, `C=plx_pmsshim`; `make check-nodef` proves the mode is compiled out of release |

Synthetic content is only fixed metadata: artwork points at `images.metahub.space`, the fake torrent
row has no bytes, and the MP4 row is a real remote URL. Pixels and decoded playback still need a TV.

## B — host / simulator / fixture scenarios

- **Hidden Direct Play Auto.** The row is hidden because the shim has no transcoder; the default
  remains Auto (`DirectPlayMode::Auto`). The per-profile persistence contract is pinned in
  `rust-modules/src/app/restore_characterisation_tests.rs`. Run `make test-crate C=plxnative-modules`.
  No UI toggle is needed for this scenario.
- **DTS / TrueHD / Opus media.** `make fixtures`, or targeted:
  `python3 tests/fixtures/make_fixtures.py --tier pipeline --only pipe_h264_dts_1080p,… --out ~/plxnative-fixtures`.
  The generator synthesizes the tracks from lavfi and reads them back (`tests/fixtures/README.md`).
  The pipe tier drives them with `plxnative-playurl` and a codec declaration:
  `tools/tv-lock.sh with --why "dts fixture" -- ./tests/run.py --filter pipe_h264_dts_1080p`.
  Shapes exist for TrueHD (`movie_hevc_4k_hdr10_truehd`) and Opus, but the **in-app conversion to
  stereo PCM on a TV with no native path is class C** — the owner verified that on real addon
  content (`VALIDATION.md:23`).
- **Startup prime / 3 s progressive hold / 30 s decision.** `make test-hostsim C=plx_media`
  (`prime_profile.rs`, `hold_resume.rs`, `route/decision.rs`); the 1.2× rate and floor on a cold
  source still need a TV run.
- **Opus clock reframe.** `play-feed/src/opus_reframe.rs`; `make test-crate C=plx_play_feed`.
- **Subtitles drill-down.** `plx_data metadata::sub_layout::tests`; `make test-crate C=plx_data`.
- **Trailer cache eviction and reach TTL.** `pmsshim/src/shim_trailer.rs`;
  `make test-crate C=plx_pmsshim T=trailer`.
- **redb self-heal and 50 MiB budget.** `nuvio-store/src/lib.rs`; `make test-crate C=plx_nuvio_store`.
  Reboot durability itself is class C.
- **Dead-addon bounding and durable catalog cache.** `shim_catalog_cache.rs` and the `plx_pmsshim`
  catalog tests; `make test-crate C=plx_pmsshim T=catalog`. Real relaunch-warm behaviour is class C.
- **TV PIN verifier and profile form.** `nuvio-store/src/pins.rs`, `screens/src/settings_nuvio_profiles.rs`;
  `make test-crate C=plx_nuvio_store` and `C=plx_screens`. The QR/account half is class C.
- **Host C-engine check.** `tools/host-engine-check.sh` runs the ignored engine tests and the
  deadline test against a loopback seeder (`docs/nuvio/TORRENT-ENGINE.md`).
- **FFmpeg load order / ABI.** `dependencies_load_before_their_dependents`; `make test-crate
  C=plx_media_codecs`. The first-playback stall itself was TV-verified by the owner.

## C — account / real-content scenarios (owner)

These are for boundary awareness. Do not drive them with a real account as an agent.

- Cross-device watch mirror, last-played resume and Continue Watching cards (`WATCH-PROGRESS.md`).
- My List, Watchlist, addon changes and the one profile seat (`Settings > Addons`, account prefs).
- Multi-profile management, TV PIN setup and forgot-PIN QR (`891776a3c`, `MERGE-AUDIT.md`).
- Real addon failures, P2P switch, Catalogs selector with real catalogs, diagnostics columns.
- Trailer end-to-end: Node decipher, MPD/Feed demux, autoplay dwell, HUD logo and hero pixels.
- DTS/TrueHD/Opus → stereo PCM on real addon streams, on a set with no native path.
- Cold detail ≤800 ms with real TMDB/BetterPosters artwork, and phase figures from the TV log.
- Trailer-takeover, stale-Part and resume bug reproduction (`TRAILERS.md`, `MERGE-AUDIT.md`).
- Release-tag CI gates and a real RELEASE/ARM package (`make ipk`, `ci/test_ci_workflows.py`).

## Still unverified — do not report as green

- `make check-replay`: anchors need `tools/plxnative-rec rerecord` on a macOS simulator; the Linux
  host cannot do that step (`VALIDATION.md:40`).

The owner confirmed on 2026-11-26 that the other former gaps are verified: fresh cold-detail phase
numbers after the latest fix, engine prebuffer and the firmware/vendor unit matrix, release-tag CI
gates, multi-hour stability, a TV run attributed to the post-extraction probe, and a filmed device
run of an actual trailer reaching its end at each rendition plus the two-candidate quality-scan
latency (`TRAILERS.md:252`). No command or log for those was attached to this catalog, so this
paragraph records the owner's ruling, not proof.
`DETAIL-LATENCY.md`, `TORRENT-ENGINE.md`, `MERGE-AUDIT.md` and `VALIDATION.md` now carry the same
owner-confirmed wording; none of them attaches the underlying command or log.

Keep this list current when a fork feature lands: add the scenario under A/B/C with the command or
log line that proves it, and move an item out of "still unverified" only with cited evidence.
