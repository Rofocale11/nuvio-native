# What this fork added, and the scenarios that pin it

Two jobs in one document:

1. **An inventory of fork additions**, derived from git history — not from memory — so an agent can
   see what is actually Nuvio-specific before deciding what a change could break.
2. **Host scenarios** that reproduce the most important fork-only behaviors on the `nuviolive` loop,
   with the pass evidence and regression signature of each. An agent confirming a regression starts
   here; `SIM-LOOP.md` remains the "how to drive the simulator" runbook.

Nothing here is a device result: every heartbeat carries `sim=1`. TV, account and fixture-pipeline
behaviors are marked as such rather than given a fake host proof.

## How the inventory was derived

The fork is a root commit, not a branch off a shared ancestor:

```
a296450aa  Import PlxNative upstream at GLinnik21/plx-native@4e454ea (GPL-3.0-or-later)
```

To regenerate the list exactly:

```sh
git log --first-parent --reverse --format='%h|%ad|%s' --date=short a296450aa..nuvio-native
```

That is **104 first-parent commits** (99 non-merge, 5 merges). Three early merges are internal
branch joins; two bring upstream in (`06e15f824` ← `0a1edc61`, `e618494cc` ← `efc05f025`) and are
not fork features. The tables below group the **73 commits that changed shipped behavior or its
build/docs contract** by behavior. The other 31 are merge-compatibility compile/test repairs,
locale-key porting steps, replay proof re-pins and audit records — they are kept in
`MERGE-AUDIT.md` / `MERGE-AUDIT-HISTORY.md` rather than repeated here. A behavior can have several
commits (introduced, then ported/fixed); the hashes name the ones that matter. Fix and plumbing
commits are labelled as such — they are not all net-new features.

Commit subjects say what was intended; `VALIDATION.md`, `HANDOFF.md`, `PMSSHIM.md` and the code say
what shipped. Where those disagree, the shipped behavior wins.

## The fork's additions

### Foundation, shim and durable store

| Behavior | Commit(s) | Host pin |
|---|---|---|
| Upstream import — the fork point | `a296450aa` | — |
| `plx_stremio` contract crate + architecture doc | `3fdb3ea5b` | — |
| PMS-dialect loopback shim over Stremio addons | `a98c8f9ba` | S1 |
| In-process shim boot, Nuvio sign-in surface, probe-backed decisions | `ae6c8dcde` | S1 |
| Writable data-dir candidate for the Developer Mode jail | `5dec3ea48` | — (device jail) |
| One `nuvio.redb` replaces DB8; self-heal and 50 MiB cache budget | `75059ed6f`, `0eee19ea0` | S1 (sealed store); self-heal is unit-test only here |
| No DB8 permission declaration / helper; guard TV DB8 access | `025309bbb`, `89ed74790` | — |
| Fork-owned `plx_nuvio_probe` crate and CI module layer | `397085e44`, `3678f6913`, `5cd479cdf` | — (separate probe compare) |
| Nuvio stream/My List IO behind typed app effects | `a9ad2351f` | — |
| Synthetic debug modes: `nuviofake`, then `nuviolive` | `872617531`, `89e6face6` | S1–S16 |
| Privacy erase clears every fork table and this install's data paths | `85f17819b` | S4 |

### Catalogs, enrichment and account

| Behavior | Commit(s) | Host pin |
|---|---|---|
| TMDB enrichment and synthetic IDs in the shim | `75defe5a6` | S2 |
| Person routes / filmography served by the shim; cache, sign-out and pick fixes | `e5966d5f1` | S2 |
| Nuvio addon sources in **Streams**; two-level picker in the `…` menu | `49d97265f`, `085c546af`, `ed3f9df76`, `d4febdb8f` | S3 |
| Nuvio addon + P2P Settings; inert Playback rows hidden | `eef501647` | S3 (engine gate), S11 (shipped switch) |
| Explicit addon failures; catalog/subtitle caches warm across TV relaunch | `e0b584c0c` | S6 (failure half; warm cache is TV) |
| Catalogs selector in the Movies/TV Shows header, Collections fallback | `84a00aa68` | S7 (selector; Collections fallback is TV) |
| BetterPosters RT posters, synced My List, WebP decode | `3facef1d5`, `6e9c2acce`, `2946fdc0b` | — (account/TV) |
| Nuvio account preferences from the shim; one seated profile | `31df3531d` | — |
| Watch-progress mirror and last-played source per resume point | `bc81d4014`, `0715b5e5a` | — (account) |
| Multi-profile management with TV PINs | `891776a3c` | — (TV) |
| Torrent and debrid diagnostics | `9a7a904c3` | — (TV) |

### Playback, engine and trailers

| Behavior | Commit(s) | Host pin |
|---|---|---|
| In-process torrent engine, trailer JS service, shims taken wholesale | `d160b62cb` | S3 (engine gate) |
| Rust DASH trailer demux, end-to-end trailers; cache eviction/reach gate | `b26d45dd1`, `290423d2d` | S15 (host demux); autoplay/device playback is TV |
| Native addon playback, stream selection, persistent config | `a8ff5c309` | S5 (pipeline only) |
| FFmpeg core retained/extracted, ABI unified with upstream | `64b9b60f1`, `3ac3a170b`, `bf50d6af7`, `8aadbdcd1` | S5 (host staging) |
| `libswresample` shipped; sole fork `build-ipk.yml` workflow | `25b37b750`, `bbf99e502` | S5, S9 (host bind) |
| First-playback stall fixed by DT_NEEDED load order | `2d3373dfb` | S9 |
| Host FFmpeg candidate names; probe ABI finds it in the Linux sim | `81eb6d158`, `240879693`, `44309a834`, `63d6d9771`, `b95b444c6` | S5 |
| Startup prime 1.2×, 3 s progressive hold, 30 s deadline-only `/decision` | `bd7db6b72` | — (TV timing) |
| Continue Watching resume, trailer takeover, stale Part index, hero Resume | `564b011cb`, `3bff6080e` | S12 (resume), S15 (trailer takeover) |
| Subtitles drill-down in the player track menu | `ad28a157d` | S13 (compact menu after a pick) |
| DTS/TrueHD/Opus → stereo PCM; multichannel Opus | `47b44dde5`, `7c5064a7b` | — (TV decoder) |
| English audio default and rating source preference | `e9cc9d084` | — (TV) |
| Plot on Home, clear player logo, dissolve flip, detail-latency work | `a4b5cb1ed`, `2b7c2067e`, `edcc67f60` | S14 (logo/plot), S10 (phase log) |

### Brand, docs, CI and releases (not product features, but fork-owned)

| Behavior | Commit(s) | Host pin |
|---|---|---|
| Rebrand to Nuvio Native, honest GPL attribution, Nuvio QR caption | `05484c141`, `05ba74e5f`, `6ebbfddfa` | S8 (copy; QR caption is TV) |
| Stable IPK build/verify workflow; owner-verified TV playback record | `25b37b750`, `b3484f7fe`, `de5dc183e` | — |
| Merge audits, doc reconciliation and validation records | `42363e3c7`, `cdc6d4a50`, `e99fdfa4a`, `1db875cc8`, `e8c3c64f7` | — |
| CI registries, replay digests, GC flake and host-loop steps | `dfa82ddfe`, `7fd3db1cb`, `9e215acce` | — |
| Upstream syncs — code brought IN, not fork additions | `06e15f824`, `e618494cc` | — |

---

## Host scenarios

These sixteen are the host-pinnable subset of the inventory above. Each was run on a `nuviolive`
boot before being written down.

Live catalogs move. A changed shelf, cast list or source count is **not** by itself a regression in
your code — compare against the log and the shot from the run, and re-run before naming a cause.
Open the PNG every time; a shot nobody looks at has verified nothing.

| # | Fork behavior pinned | One-line reproduction | Pass signal |
|---|---|---|---|
| S1 | Sealed real-catalog boot | `python3 tools/nuvio-sim.py --shot home` | live-boot lines + real posters in the PNG |
| S2 | Person enrichment via the shim, no Discover stand-in | boot `detail=10016`, press `detail.cast.card.0` | `person: profile …`, credits, filmography ids |
| S3 | Bounded Torrentio source list, p2p engine gate | `--host-engine`, boot `detail=10005`, open the Streams pill | 1 provider row → bounded source rows |
| S4 | Privacy erase clears fork records | boot `settings=privacy`, press `privacy.delete` → `privacy.alert.delete` | `nuvio: synthetic local data erased` |
| S5 | Playback pipeline behind the clock sink | `--media <local H.264/AAC>` | demux + HUD position advance |
| S6 | One dead addon degrades, does not blank Home | `--addons` with one dead manifest | `LIVE addon FAILED` + good addon's shelves land |
| S7 | Catalogs selector in the Movies header | `--trigger library=3`, press `library.header.catalogs` | menu opens; selecting changes pill + grid |
| S8 | Brand and About attribution | `--trigger settings=root`, press `settings.about` | GPL / © Gleb Linnik / TMDB / BetterPosters copy |
| S9 | Host FFmpeg bind + DT_NEEDED load order | any live boot | five `ff: bound … -host.so` lines, no MISSING |
| S10 | Detail-page phase telemetry | boot `detail=10005` | `detail: shim rk=… meta_ms=… streams_ms=… tmdb_ms=…` |
| S11 | Settings P2P switch drives the Streams gate | engine + `settings=playback`, toggle `playback.p2p_toggle` | same pill goes from sources to "No playable streams found" |
| S12 | Continue Watching resume / hero Resume | play a fixture-backed item, STOP, reopen | `home.continue.card.0`, detail pill reads **Resume** |
| S13 | Subtitles drill-down → compact menu after pick | real-ratingKey playback, pick a track, reopen menu | one/two grouped rows, picked track checked |
| S14 | Clear logo in player, plot on Home | same real-ratingKey playback | TMDB logo in HUD, plot on Home hero |
| S15 | Trailer DASH demux and play | press `detail.extras.card.0` | `dash: demux start`, manifest, `dash: v=#0` |
| S16 | Fake mode as a control | manual boot with only `plxnative-nuviofake` | fixed-catalog line, no LIVE proofs |

### S1 — Sealed live boot with the real catalog

**Fork feature:** `nuviolive` boots the real in-process shim and data layer against public Stremio
addons and TMDB, but with a dev-PMS identity and a private redb. It is what makes self-service host
testing possible without the owner (`nuvio_debug.rs`, `nuvio.rs`).

```sh
python3 tools/nuvio-sim.py --shot home
python3 tools/nuvio-sim.py --shot home --expect 'nuvio: SYNTHETIC private redb store'
```

**Pass:** the driver exits 0 and the log contains all of:

```
nuvio: LIVE addon installed: https://…
nuvio: LIVE tmdb=configured — real addons, real shim caches, sealed account
nuvio: 3 addon(s): Cinemeta, Torrentio, OpenSubtitles v3
nuvio: SYNTHETIC private redb store — household records untouched
nuvio: shim serving on http://127.0.0.1:…
```

`home-1.png` shows real catalog posters and TMDB hero copy, not a fixed fake set.

**Regression signature:** a sign-in / who's-watching gate before Home; an empty catalog; a missing
`LIVE tmdb=configured` line (key not read from the gitignored local env); fewer addons than the
tracked `tests/nuvio-public-addons.json`; or a boot that opens the household store. The driver
requires at least one installed addon, so an **all-dead** list fails closed rather than presenting an
empty Home as success; one dead addon among good ones is the graceful-degradation case in S6. Do
**not** "fix" a stale-looking `discover: base=real reads=open` line by arming a
`plxnative-discover` stand-in — see S2.

### S2 — Person enrichment through the shim (no Discover stand-in)

**Fork feature:** the shim answers `/library/people/{guid}` and `/credits` from TMDB, so a cast card
opens a real person page without any external Discover service. This is the behavior that made the
stand-in removable (`shim_pms.rs`, `PMSSHIM.md` §7–8).

```sh
python3 tools/nuvio-sim.py --trigger detail=10016 --keep --timeout 120
# note the printed runtime root R and pid
python3 tools/a11y.py state R                     # detail.cast.card.0 = Anne Hathaway here
python3 -c "import os; f=os.open('R/plxnative-remote', os.O_RDWR); \
os.write(f,b'press:detail.cast.card.0 '); os.close(f)"
python3 tools/a11y.py state R
rg -n 'person:|coldopen screen=person' R/plxnative-events.log
kill <pid>
```

`a11y.py press` refuses a title-bearing id without `--subject`; the FIFO write above bypasses that
guard, which is safe **because this boot is sealed**.

**Pass:** route `person`; `coldopen screen=person`; log lines `person: profile guid=… bio=…`,
`person: credits … rows=…` and `person: source 0 'Anne Hathaway' …`; a11y ids `person.filmography.*`
and `person.shelf.*` with real posters. A live run on 2026-11-26 produced a 1711-byte biography and
153 credit rows. Counts come from TMDB and will drift; assert on the lines and ids, not the numbers.

**Regression signature:** an empty person page, biography missing, no `person: profile`/`person:
credits` lines, or a page that only works when a hard-coded Discover port is armed. The log line
`discover: base=real reads=open` is stale in this fork — `account::discover()` prefers the installed
shim on every call — so it is **not** evidence of where person reads went.

### S3 — Bounded Torrentio source list and the p2p engine gate

**Fork feature:** the tracked addon list uses Torrentio's configured endpoint
`qualityfilter=unknown,cam,other,scr,threed,480p,720p|limit=2`, not the bare one, so repeated agent
loops do not pull hundreds of streams and get rate-limited. Torrent/magnet sources are also
deliberately dropped unless `libsbengine.so` is loadable (`shim_media::playable_url` / `rank`, the
fork's Settings P2P switch).

```sh
python3 tools/nuvio-sim.py --host-engine --trigger detail=10005 --keep --timeout 120
# FIFO: press:detail.hero.alt                -> a11y route detail, alt.row.0 "Torrentio" (provider)
# FIFO: press:alt.row.0                      -> a11y alt.row.0..N, one row per source
# FIFO: shot                                 -> open shot-1.png
```

**Pass:** `coldopen screen=alt`; one provider row labelled `Torrentio`; pressing it yields a
**bounded** non-empty source list (8 rows on 2026-11-26) whose labels carry quality, size and peer
counts. The boot log shows the configured URL including `limit=2`. Direct reads of that endpoint
gave 9 streams for The Odyssey (`tt33764258`) and 6 for a 2026 movie, versus 433 from the bare
endpoint.

**Control:** run the same command **without** `--host-engine`. "No playable streams found" is the
*expected* result of the engine gate on a host build, not a missing addon. The episode case behaves
the same way: on a show page press `detail.episodes.still.0` and the picker carries that episode's
ratingKey.

**Regression signature:** with `--host-engine`, no sources at all (gate or addon load broke), or
hundreds of rows / a restored bare endpoint / rate-limit errors. Listing sources is hermetic — it is
an addon HTTP read. **Picking a source is not**: it opens the real engine against public peers. Do
not press a source row unless that is intended.

### S4 — Privacy erase clears the fork's records

**Fork feature:** Settings ▸ Privacy & data ▸ *Delete all local data* clears every table in the one
`nuvio.redb` and unlinks this install's data paths. Upstream's sweep could not see that file, so
before this fix a "full" erase left watch state, addon config, the ratingKey table and caches behind
(`nuvio-store/src/erase.rs`, `base/src/paths/nuvio_erase.rs`, commit `85f17819b`).

```sh
python3 tools/nuvio-sim.py --trigger settings=privacy --keep --timeout 120
python3 tools/a11y.py state R                    # privacy.delete is present
# FIFO: press:privacy.delete                    -> confirmation alert
# FIFO: press:privacy.alert.delete              -> confirmed erase
rg -n 'nuvio: synthetic local data erased' R/plxnative-events.log
```

**Pass:** the log contains `nuvio: synthetic local data erased` and the app lands on the Sign in
screen. A synthetic boot clears its **private** store only; it never unlinks a real install's file.
The table-level sweep is pinned separately by the unit test `clearing_every_table_leaves_no_rows`
and `ALL_TABLES` — a new durable table must be added there or a privacy erase silently leaves it.

**Regression signature:** no erase line, the app stays in Settings, or a record is still readable
after the erase. Do not run this scenario on a real install to "check": the production path revokes
the account and deletes household data. This sealed run is the safe proof.

### S5 — Playback pipeline behind the clock sink

**Fork feature:** the driver can serve a local H.264/AAC file through the Range-capable fixture
server, arm `playurl` + `clocksink`, and drive a real demux/framing/clock/HUD run on the host. Host
FFmpeg (including the fork's `libswresample-plx-host.so.7`) is required; TV playback is already
verified and is not what this checks.

```sh
python3 tools/nuvio-sim.py --media ~/.cache/plxnative-demo/src/sintel-film.mp4 \
  --shot playback --settle 16
```

**Pass:** the driver asserts `clocksink: ARMED`, `ff: v=#0 codec=…` and `route=player`, and the log
shows 200/206 Range GETs. Open `playback-1.png`: the player HUD is at a real, advanced position
(e.g. 1 s → 17 s in the recorded run). The picture area is **black by design** — the clock sink
discards access units, so no decoder runs on the host.

**Regression signature:** `ff: FFmpeg unavailable`, `ff: swresample is MISSING`, `ff: no framing for
video codec …` (MPEG-4 Part 2 samples are not supported by the framing path), a 404 from the
fixture server (use a real file, not a symlink — it is contained to its root), or a HUD position
that never advances. None of these is a device playback failure; they are host-pipeline failures.

### S6 — One dead addon degrades; it does not blank Home

**Fork feature:** commit `e0b584c0c` made addon failures explicit instead of silently turning an
outage into an empty catalog. `public_addons()` installs each URL independently and logs a
`FAILED` line for a bad manifest while keeping the good ones
(`rust-modules/src/app/nuvio/nuvio_debug.rs`). The driver supports `--addons` for account-free
local lists.

```sh
cat > /tmp/opencode/nuvio-addons-one-dead.json <<'JSON'
{"addons": ["https://127.0.0.1:9/dead-manifest.json", "https://v3-cinemeta.strem.io"]}
JSON
python3 tools/nuvio-sim.py --addons /tmp/opencode/nuvio-addons-one-dead.json \
  --shot failed-addon --expect 'nuvio: LIVE addon FAILED' --timeout 120
```

**Pass:** the log shows the transport error and the failed URL, then the surviving addon and a
populated Home:

```
net: curl rc=7 peer=127.0.0.1:9 — transport error
nuvio: LIVE addon FAILED …/dead-manifest.json: Network
nuvio: LIVE addon installed: …
nuvio: 1 addon(s): Cinemeta
hubs: landed — 48 items, 4 shelves
```

`failed-addon-1.png` shows real Cinemeta posters and TMDB hero copy. The item and shelf counts are
live; assert on the `FAILED` line, the surviving addon count and non-empty shelves, not the numbers.

**Control:** an **all-dead** list has no `nuvio: LIVE addon installed:` line, so the driver's live
proof fails closed. That is the intended difference between "one addon is down" and "no backend at
all".

**Regression signature:** the boot aborts because one addon failed, Home has no shelves, the dead
URL is logged as installed, or there is no `FAILED` line at all. No TV or account is needed for this
check; it never touches an installed addon list.

### S7 — Catalogs selector in the Movies header

**Fork feature:** commit `84a00aa68` replaced the upstream "Collections" control with a Catalogs
selector in the Movies/TV Shows library header, backed by the addon's catalog list
(`rust-modules/screens/src/library/header.rs`, `library/menu.rs`).

```sh
python3 tools/nuvio-sim.py --trigger library=3 --keep --shot library --timeout 120
# R = printed runtime root
python3 tools/a11y.py press R library.header.catalogs
python3 tools/a11y.py state R          # library_menu.catalog.0..2; coldopen screen=library_menu
python3 tools/a11y.py press R library_menu.catalog.1
python3 tools/a11y.py state R          # pill now "Catalogs · New Movies", grid changed
# FIFO: shot                           -> library-3.png
```

**Pass:** the library header exposes a11y id `library.header.catalogs` labelled `Catalogs`; pressing
it opens the `library_menu` overlay with `library_menu.catalog.0..2` and a `coldopen
screen=library_menu` log line. `library-2.png` shows the menu with a check on the active catalog.
Pressing a different row dismisses the overlay; `library-3.png` shows the pill reading `Catalogs ·
New Movies` and a visibly different grid (the recorded run changed the first card from *The
Bodyguard* to *Steps*). Catalog names and cards come from Cinemeta and drift — assert on the ids,
the pill and a changed grid, not on a particular title.

**Regression signature:** no `library.header.catalogs` id, the header says "Collections" again, the
menu does not open, or selecting a row leaves the pill and grid unchanged. This is pure navigation;
nothing here writes to an account.

### S8 — Brand copy and About attribution

**Fork feature:** `05484c141`, `05ba74e5f` and `6ebbfddfa` moved fork copy into
`nuvio-locales/brand.json`, applied by `catalog_brand.rs` at catalog build time. This is the most
merge-sensitive fork surface: an upstream locale sync can overwrite it, and the About page is where
the required notices are legally visible.

```sh
python3 tools/nuvio-sim.py --trigger settings=root --keep --shot settings-root --timeout 120
python3 tools/a11y.py state R          # settings.about = "About Nuvio Native"
python3 tools/a11y.py press R settings.about
# FIFO: shot                           -> settings-root-2.png
```

**Pass:** a11y id `settings.about` opens the `settings-about.document` page. The shot shows, in
one frame:

- title **About Nuvio Native** and the line "A native media client built for LG webOS."
- "Nuvio Native — a fork of PlxNative (GPL-3.0-or-later)" and "Upstream © Gleb Linnik and contributors"
- "This product uses the TMDB API but is not endorsed or certified by TMDB." plus the TMDB mark
- "Posters by BetterPosters (bttr.cc); Rotten Tomatoes scores come from that addon."
- "Nuvio Native is independent and is not affiliated with Plex or LG."

No string describes the product as a Plex client or names a Plex account/server the viewer uses.
The build SHA and version on the same page identify the binary being inspected.

**Regression signature:** upstream copy returns, the GPL line or © Gleb Linnik attribution is gone,
the TMDB notice or mark is missing, the BetterPosters credit disappears, or Plex-client/sign-in copy
reappears. This is a visual pin: read the PNG, do not infer it from a passing driver exit alone.

### S9 — Host FFmpeg binding and DT_NEEDED load order

**Fork feature:** `25b37b750` ships `libswresample` and names the host libraries distinctly from
the ARM ones; `2d3373dfb` fixed a first-playback stall by loading the bundled FFmpeg in DT_NEEDED
order. S5 proves the demux pipeline runs; this scenario pins that the *binding* still succeeds and
in the right order.

```sh
python3 tools/nuvio-sim.py --shot home
# R = printed runtime root
rg -n 'ff: bound|ff: swresample is MISSING|ff: FFmpeg unavailable' R/plxnative-events.log
```

**Pass:** all five host libraries bind, in this order, with no `MISSING`/`unavailable` line:

```
ff: bound avutil -> libavutil-plx-host.so.61
ff: bound swresample -> libswresample-plx-host.so.7
ff: bound avcodec -> libavcodec-plx-host.so.63
ff: bound avformat -> libavformat-plx-host.so.63
ff: bound swscale -> libswscale-plx-host.so.10
ff: avformat=… avcodec=… avutil=…
```

`swresample` binds before `avcodec` because FFmpeg 9's native Opus decoder makes `avcodec` depend
on it — that dependency is exactly what the load-order fix addresses. Version suffixes may move
with an FFmpeg bump; assert on the five `-plx-host.so` names, their order and the absence of the
failure lines.

**Regression signature:** `ff: FFmpeg unavailable` (host libraries not staged in `pkg/`), `ff:
swresample is MISSING` (a build without the fork's `--enable-swresample`), a dlopen failure, or ARM
`*-plx.so.*` names being bound by the host binary. This is host evidence only — it says nothing
about LG decoding or device frame rate.

### S10 — Detail-page phase telemetry

**Fork feature:** `edcc67f60` ported the old fork's phase timings. The shim logs one line with the
metadata, stream and TMDB legs of a fresh detail read (`shim_pms.rs`).

```sh
python3 tools/nuvio-sim.py --trigger detail=10005 --shot detail-telemetry \
  --expect 'detail: shim rk=' --timeout 120
# R = printed runtime root
rg -n 'detail: shim|plxnative-detail' R/plxnative-events.log
```

**Pass:** a line like `detail: shim rk=10005 meta_ms=33 streams_ms=0 tmdb_ms=697 total_ms=731`,
plus `plxnative-detail: rk=10005 server=0 start`. All four legs are present; a zero `streams_ms` is
normal when the addon answer was cached. Host numbers are **not** device numbers — assert that no
leg is missing and that the line names the requested ratingKey.

**Regression signature:** no `detail: shim` line, a phase missing from the format, or a line for a
different rk. This is a log pin; the shot is only there to prove the page actually opened.

### Fixture-backed playback (setup for S12–S15)

`playurl` has no ratingKey field, so a local file can never resolve a subtitle search or show a
TMDB clear logo. The hermetic way to get a **real item identity** on the host is a tiny local
Stremio addon that serves the same local MP4 as a real stream. It uses loopback only — no torrent,
debrid key, account or TV.

```sh
PORT=18765
python3 tools/nuvio-fixture-addon.py "$PORT" tt33764258 \
  ~/.cache/plxnative-demo/src/sintel-film.mp4 & ADDON_PID=$!
cat > /tmp/opencode/nuvio-addons-fixture.json <<JSON
{"addons": ["https://v3-cinemeta.strem.io", "http://127.0.0.1:$PORT", "https://opensubtitles-v3.strem.io"]}
JSON
python3 tools/nuvio-sim.py --addons /tmp/opencode/nuvio-addons-fixture.json \
  --trigger clocksink --keep --shot fixture-home --timeout 120
# R = printed runtime root; app pid is printed too
```

The boot log names `nuvio: 3 addon(s): Cinemeta, Local Fixture, OpenSubtitles v3`. In `state`,
*The Odyssey* is a card with a ratingKey in `subject` (e.g. `home.shelf.0.card.1`, subject `10001`).
Read that subject from your own run; do not hard-code the number. Then open the detail and play it:

```sh
python3 tools/a11y.py state R                       # find The Odyssey's id and subject RK
python3 -c "import os; f=os.open('R/plxnative-remote', os.O_RDWR); \
os.write(f,b'press:<card-id>@<RK> '); os.close(f)"  # detail
python3 -c "import os; f=os.open('R/plxnative-remote', os.O_RDWR); \
os.write(f,b'press:detail.hero.play@<RK> '); os.close(f)"
```

The a11y CLI refuses title-bearing ids without `--subject`; the FIFO writes above are safe because
the boot is sealed. The log proves real playback with an item identity: `clocksink: ARMED`,
`ff: v=#0 codec=h264 …`, `route=player`, and `detail: shim rk=<RK> …`. Keep this boot alive for
S12–S15; kill `ADDON_PID` and the app pid when done.

### S11 — The Settings P2P switch drives the Streams gate

**Fork feature:** `eef501647` added the shipped **Enable P2P** row; `set_p2p_enabled()` writes the
one redb and applies the switch to the running shim without a restart
(`nuvio.rs`, `screens/src/preferences.rs`). S3 pins only engine presence.

```sh
python3 tools/nuvio-sim.py --host-engine --trigger settings=playback --keep \
  --shot p2p-engine-on --timeout 120
# R = printed runtime root
# shot: Enable P2P reads "On" because the engine is staged
python3 tools/a11y.py press R playback.p2p_toggle       # shot: now "Off"
# FIFO: back, focus:nav.movies, press:nav.movies        -> library
# FIFO: press:library.grid.card.0@<RK>                  -> detail (read RK from state)
python3 tools/a11y.py press R detail.hero.alt
python3 tools/a11y.py press R alt.row.0
```

**Pass:** with the setting On, the picker has provider rows and source rows (S3). After toggling
**Off in the same boot**, the a11y `alt` namespace has **zero** rows and the shot shows **"No
playable streams found."** with the engine still staged. Repeating S3 in a fresh boot (setting
back at its default) restores the rows.

**Why the same boot matters:** `install_ephemeral()` deletes `nuvio-synthetic.redb` at *every*
synthetic boot, so a Settings change never survives a restart in this loop. A reboot test would
show sources again and prove nothing. Fake mode also forces P2P off; Live does not hard-code it on.

**Regression signature:** toggling Off leaves the source rows intact, the toggle does not visibly
change, or sources vanish without a toggle (that is the engine gate, S3).

### S12 — Continue Watching resume and hero Resume

**Fork feature:** `564b011cb` / `3bff6080e` gave watched items a resume point and made the detail
hero's first pill read **Resume** mid-title. Starting from the fixture-backed playback above:

```sh
# while the item is playing:
# FIFO: stop, then back until route=detail
python3 tools/a11y.py state R       # detail.hero.play + detail.hero.restart, both on RK
# FIFO: shot                        -> fixture-home-7.png
# FIFO: back                        -> Home; home.continue.card.0 appears
```

**Pass:** the detail hero pill visibly reads **Resume** with a "… min left" line (the recorded run
showed "12 min left"), and `detail.hero.restart` appears as a second control. Home gains
`home.continue.card.0` carrying the same ratingKey; pressing it returns to that detail with the
Resume pill. The a11y id of the first pill stays `detail.hero.play` — its *label* changes with watch
state, so read the shot, not just the id list.

**Regression signature:** stopping a partial playback leaves no Continue Watching card, the hero
still says "Play", or the card carries a different ratingKey.

### S13 — Subtitles "Other languages" drill-down and the compact menu after a pick

**Fork feature:** `ad28a157d` groups the player's subtitle tracks by language behind **Other
languages**, so after a track is picked the Subtitles menu shows only the one or two languages in
use instead of every track as a flat list. This needs a playback with a real ratingKey — see the
note below.

From the fixture-backed playback in the setup above:

```sh
python3 tools/a11y.py press R player.control.subtitles   # menu.sub.0, menu.other, ...
python3 tools/a11y.py press R menu.other                 # OTHER LANGUAGES: menu.lang.0..N
python3 tools/a11y.py press R menu.lang.0                 # that language's tracks: menu.sub.N...
python3 tools/a11y.py state R                             # each track row carries subject=<rk>
# FIFO: press:menu.sub.<N>@<rk>                           # pick a track; menu closes
python3 tools/a11y.py press R player.control.subtitles   # REOPEN the Subtitles menu
python3 tools/a11y.py state R
# FIFO: shot                                              -> open it
```

`a11y.py press` refuses a track row without `--subject`; the state call shows each row's
`subject` (the ratingKey), so either pass `--subject <rk>` or use the FIFO write above, which is
safe in a sealed boot.

**Pass:** the reopened menu's a11y list is **compact** — `menu.off`, `menu.forced`, `menu.sub.0`,
`menu.sub.<picked>`, `menu.other`, `menu.timing`, `menu.style`, `menu.search` (eight rows in the
recorded run, not one row per available track). The shot shows one or two grouped language entries
with a check on the picked track: the run showed `eng` (EXTERNAL) plus `alb` (EXTERNAL, checked),
**Other languages 11**, and the chosen Albanian line rendered on the picture. The user's device
example shows the same shape with `eng` and a checked `ind` track. Language codes and counts come
from the item and drift; assert on **grouping, the checkmark and the small row count**, not on
`alb`/`ind` specifically.

**Regression signature (the actual bug this pins):** after picking a track and reopening the
Subtitles menu, **all** subtitle tracks are listed as one flat list instead of the compact one/two
language groups; or the picked track has no checkmark, or the rendered subtitle never changes.
Stopping at the OTHER LANGUAGES page (merely seeing `menu.lang.*`) does *not* prove the fix — the
check is the menu **after** the pick.

**The ratingKey is the premise.** With a `playurl` local file there is no rk, the first menu has no
`menu.sub.0`/`menu.other`, and Search sits at "Searching…" forever. With a real item the same
Search flow returns `menu.hit.0..4` from OpenSubtitles — but that is a different feature from the
grouping pin above.

### S14 — Clear logo in the player, plot on Home

**Fork feature:** `a4b5cb1ed` renders the TMDB clear logo in the player HUD and restores the plot
line on Home. Both are visual and need a real catalog item with TMDB metadata.

```sh
# from the fixture-backed detail/player in setup above:
# FIFO: shot on Home hero, and shot while the player HUD is up
```

**Pass:** the Home hero shows the plot paragraph under the title (not just genre/year), and the
player HUD shows the stylized TMDB clear logo for *The Odyssey* at the lower left (visible in
`fixture-home-2.png` and `-4.png`). The picture area is still black by design — the clock sink
discards access units; only the HUD art is real.

**Regression signature:** the player HUD falls back to plain title text, or the Home hero loses its
summary. This cannot be asserted from the event log; open both PNGs.

### S15 — Trailer DASH demux and play

**Fork feature:** `b26d45dd1` / `290423d2d` ported the trailer DASH demux and end-to-end trailer
playback. From the fixture-backed detail, the Extras shelf carries real trailer cards with
synthetic keys (e.g. `5000010001`):

```sh
python3 tools/a11y.py state R    # detail.extras.card.0 and its subject
# FIFO: press:detail.extras.card.0@<subject>
rg -n 'trailer:|dash:|route=player' R/plxnative-events.log
```

**Pass:** the log shows `trailer: dash probe ok=true container=mp4 vcodec=h264`, the shim serving a
`file.mpd`, `dash: demux start`, `dash: fetching trailer manifest`, `dash: manifest video=… audio=…`,
and `dash: v=#0 codec=h264 …`, with the app on `route=player`. That proves the Node-deciphered DASH
manifest is fetched and framed on the host. The screenshot can be black: no decoder runs here.

**Regression signature:** no `dash: demux start`, no manifest, a codec the framing refuses, or the
press does not reach the player. Device autoplay and full-trailer mode are still TV-only, despite
this host demux proof.

### S16 — Fake mode as a control

**Fork feature:** `872617531` made fixed-content `nuviofake` a boot-time choice. The driver always
arms `nuviolive`, and Live wins when both are armed, so this one needs a manual launch.

```sh
R=/tmp/opencode/nuvio-fake-control; rm -rf "$R"; mkdir -p "$R"; : > "$R/plxnative-nuviofake"
PLXNATIVE_RUNTIME_DIR="$R" PLXNATIVE_APP_DIR="$PWD/pkg" PLXNATIVE_SHOT="$R/fake.png" \
  rust-modules/target-sim-nuvio/debug/plxnative-sim 127.0.0.1 32400 >/dev/null 2>&1 & APP=$!
# wait for the log, FIFO-write "shot ", inspect, then kill "$APP"
```

**Pass:** the log has `nuvio: SYNTHETIC private redb store`, `nuvio: synthetic store in use — no
household records opened` and `nuvio: SYNTHETIC debug backend — fixed catalog, no Nuvio account
reads or writes`. There are **no** `nuvio: LIVE …` lines. `fake-1.png` shows the fixed four-film
catalog, not the Cinemeta shelves from S1.

**Regression signature:** a fake boot fetches public addons or TMDB, or a live boot serves the
fixed fake set. This control is what makes S1's "real catalog" claim meaningful.

---

## Behaviors with no host scenario here

Deliberately left unpinned here: they need the TV, a real account, a durability reboot, or a
separate probe comparison, and must not be claimed as verified from the host loop. That covers
BetterPosters RT artwork and My List writes, watch-state sync across devices, multi-profile PINs,
DTS/TrueHD/Opus decoding, trailer **autoplay and full-trailer modes** on device (S15 pins only the
host DASH demux), frame rate / text rasterization / video-plane composition, TV timing budgets,
and `make check-replay` fixtures (which need `tools/plxnative-rec rerecord` on a macOS simulator).

Two fork behaviors deserve a specific note:

- **redb self-heal / quarantine** (`0eee19ea0`) is covered by `plx_nuvio_store` unit tests. A host
  boot cannot prove it: `install_ephemeral()` deletes `nuvio-synthetic.redb` at every synthetic
  boot, so a corrupted record has no next boot to be quarantined on.
- **P2P Settings durability** cannot be shown for the same reason (S11 must toggle in-session);
  the shipped write path is still pinned there.

See `VALIDATION.md` and `HANDOFF.md` for their verification status.

## Adding a scenario

Keep the template: **fork feature** (and the commit / file that owns it) → exact command → **pass**
evidence (log lines, a11y ids, shot) → **regression signature** → safety line if the real-world path
writes anything. Prefer log lines and ids over absolute numbers from live services. If the behavior
cannot be observed in a sealed host boot, mark it `—` and say why instead of inventing a host proof.
