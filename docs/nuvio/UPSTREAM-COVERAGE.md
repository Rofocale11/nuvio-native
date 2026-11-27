# Upstream scenario coverage in Nuvio Native

Generated from `tests/manifest.json` by `tools/nuvio-scenes.py`. A host PASS from
`tools/nuvio-sim-scenes.py` is **not** a TV verdict: simulator frame rates are uncapped and
the gates calibrated to the LG set or to a full bench window are listed as deferred.

## Summary

- Upstream UI fps scenes translated: **82**
- Upstream UI fps scenes skipped: **9** (see reasons below)
- Host smoke results: **0 PASS / 0 FAIL / 82 not run**
- Player tier, playback cases and pipeline cases are **not covered** by this harness (Stage 4: they need PMS/stream fixtures the in-process shim does not serve).
- Fork behavior scenes — all 16 `docs/nuvio/SCENARIO.md` pins plus the source picker, episode click, subtitle drill and catalog-shape checks: **17** — 17 PASS / 0 FAIL / **0 not run**
- Routine coverage uses the deterministic local Stremio addon (`tools/nuvio-fixture-addon.py`) and a loopback TMDB API/image fixture (`tools/nuvio-tmdb-fixture.py`, via `TMDB_API_BASE` / `TMDB_IMAGE_BASE`); `nuviolive` with public addons stays the one-off realism check, so no routine scene depends on a rate-limited CDN or on TMDB.
- Run the whole fork set fast with `python3 tools/nuvio-sim-scenes.py --scenes tests/nuvio-fork-scenes.json --quick` (3 parallel boots, shortened dwells; fps scenes stay serial at full length).
- S8 (About copy), S11 (P2P switch state) and S14 (clear logo / hero plot) produce PNGs: their pass signal is partly visual and must be read from the shot, not from the event log alone.

## Skipped upstream scenes

| Scene | Reason |
|---|---|
| info-panel | player tier (Stage 4) |
| track-menu | player tier (Stage 4) |
| track-menu-subs | player tier (Stage 4) |
| track-menu-submenu-osc | player tier (Stage 4) |
| more-quality-osc | player tier (Stage 4) |
| fixed-480p-reencode-declaration | player tier (Stage 4) |
| boost-remux-declares-source-rate | player tier (Stage 4) |
| timing-capsule | player tier (Stage 4) |
| chapters-panel | player tier (Stage 4) |

## Translated scenes

| Nuvio scene | Upstream source | Host verdict | Notes |
|---|---|---|---|
| `up-poster-scroll-settle` | `poster-scroll-settle` | NOT RUN |  |
| `up-poster-eviction-reversal` | `poster-eviction-reversal` | NOT RUN |  |
| `up-poster-hero-grid-dive` | `poster-hero-grid-dive` | NOT RUN |  |
| `up-home-hero` | `home-hero` | NOT RUN |  |
| `up-home-fold` | `home-fold` | NOT RUN |  |
| `up-home-grid` | `home-grid` | NOT RUN |  |
| `up-home-grid-deep` | `home-grid-deep` | NOT RUN |  |
| `up-home-recent-paging` | `home-recent-paging` | NOT RUN |  |
| `up-home-cw-row-walk` | `home-cw-row-walk` | NOT RUN |  |
| `up-home-recent-paging-held` | `home-recent-paging-held` | NOT RUN |  |
| `up-home-recent-paging-held-3g` | `home-recent-paging-held-3g` | NOT RUN |  |
| `up-home-recent-paging-held-remote-wan` | `home-recent-paging-held-remote-wan` | NOT RUN |  |
| `up-library-grid-paging-held-3g` | `library-grid-paging-held-3g` | NOT RUN |  |
| `up-library-grid-paging-held-remote-wan` | `library-grid-paging-held-remote-wan` | NOT RUN |  |
| `up-library-shelf-paging-held-3g` | `library-shelf-paging-held-3g` | NOT RUN |  |
| `up-library-shelf-paging-held-remote-wan` | `library-shelf-paging-held-remote-wan` | NOT RUN |  |
| `up-search-row-paging-held-3g` | `search-row-paging-held-3g` | NOT RUN |  |
| `up-search-row-paging-held-remote-wan` | `search-row-paging-held-remote-wan` | NOT RUN |  |
| `up-related-row-paging-held-3g` | `related-row-paging-held-3g` | NOT RUN |  |
| `up-related-row-paging-held-remote-wan` | `related-row-paging-held-remote-wan` | NOT RUN |  |
| `up-collection-paging-held-3g` | `collection-paging-held-3g` | NOT RUN |  |
| `up-collection-paging-held-remote-wan` | `collection-paging-held-remote-wan` | NOT RUN |  |
| `up-long-season-paging-held-3g` | `long-season-paging-held-3g` | NOT RUN |  |
| `up-long-season-paging-held-remote-wan` | `long-season-paging-held-remote-wan` | NOT RUN |  |
| `up-library-shelves-deep` | `library-shelves-deep` | NOT RUN |  |
| `up-library-burst` | `library-burst` | NOT RUN |  |
| `up-library-hold` | `library-hold` | NOT RUN |  |
| `up-home-idle` | `home-idle` | NOT RUN |  |
| `up-home-hint` | `home-hint` | NOT RUN |  |
| `up-login-spinner` | `login-spinner` | NOT RUN |  |
| `up-detail-transition` | `detail-transition` | NOT RUN |  |
| `up-show-detail` | `show-detail` | NOT RUN |  |
| `up-item-menu` | `item-menu` | NOT RUN |  |
| `up-person-page` | `person-page` | NOT RUN |  |
| `up-filmography-idle` | `filmography-idle` | NOT RUN |  |
| `up-collection-page` | `collection-page` | NOT RUN |  |
| `up-detail-collection-shelf` | `detail-collection-shelf` | NOT RUN |  |
| `up-library-scroll` | `library-scroll` | NOT RUN |  |
| `up-library-collections` | `library-collections` | NOT RUN |  |
| `up-library-recommended` | `library-recommended` | NOT RUN |  |
| `up-library-switch` | `library-switch` | NOT RUN |  |
| `up-library-idle` | `library-idle` | NOT RUN |  |
| `up-watchlist-idle` | `watchlist-idle` | NOT RUN |  |
| `up-catalog-idle` | `catalog-idle` | NOT RUN |  |
| `up-search-type` | `search-type` | NOT RUN |  |
| `up-search-idle` | `search-idle` | NOT RUN |  |
| `up-home-library-nav` | `home-library-nav` | NOT RUN |  |
| `up-home-detail-nav` | `home-detail-nav` | NOT RUN |  |
| `up-push-100` | `push-100` | NOT RUN |  |
| `up-hero-pong` | `hero-pong` | NOT RUN |  |
| `up-grid-pong` | `grid-pong` | NOT RUN |  |
| `up-home-hold` | `home-hold` | NOT RUN |  |
| `up-collection-hold` | `collection-hold` | NOT RUN |  |
| `up-home-tap` | `home-tap` | NOT RUN |  |
| `up-collection-tap` | `collection-tap` | NOT RUN |  |
| `up-onboard-sources` | `onboard-sources` | NOT RUN |  |
| `up-consent-crash` | `consent-crash` | NOT RUN |  |
| `up-consent-product` | `consent-product` | NOT RUN |  |
| `up-settings-root` | `settings-root` | NOT RUN |  |
| `up-settings-playback` | `settings-playback` | NOT RUN |  |
| `up-settings-picker` | `settings-picker` | NOT RUN |  |
| `up-settings-privacy` | `settings-privacy` | NOT RUN |  |
| `up-settings-home` | `settings-home` | NOT RUN |  |
| `up-settings-legal` | `settings-legal` | NOT RUN |  |
| `up-settings-language` | `settings-language` | NOT RUN |  |
| `up-settings-audio` | `settings-audio` | NOT RUN |  |
| `up-settings-under-posters` | `settings-under-posters` | NOT RUN |  |
| `up-settings-under-posters-movie` | `settings-under-posters-movie` | NOT RUN |  |
| `up-settings-under-posters-show` | `settings-under-posters-show` | NOT RUN |  |
| `up-settings-player-hud` | `settings-player-hud` | NOT RUN |  |
| `up-settings-diagnostics` | `settings-diagnostics` | NOT RUN |  |
| `up-settings-idle` | `settings-idle` | NOT RUN |  |
| `up-home-acct-glass` | `home-acct-glass` | NOT RUN |  |
| `up-modal-ramp` | `modal-ramp` | NOT RUN |  |
| `up-modal-100` | `modal-100` | NOT RUN |  |
| `up-deep-100` | `deep-100` | NOT RUN |  |
| `up-legal-document` | `legal-document` | NOT RUN |  |
| `up-page-panel` | `page-panel` | NOT RUN |  |
| `up-about-panel` | `about-panel` | NOT RUN |  |
| `up-person-bio` | `person-bio` | NOT RUN |  |
| `up-decision-alert` | `decision-alert` | NOT RUN |  |
| `up-cold-open` | `cold-open` | NOT RUN |  |

## Fork-specific scenes

| Scene | What it pins | Host verdict | Notes |
|---|---|---|---|
| `fork-source-picker` | Fork-only: Detail ▸ Sources opens a bounded, non-empty source list from the local addon. Picking a source is deliberately NOT pressed (it opens the real engine against peers). | PASS | FOUND 'coldopen screen=alt' | a11y id alt.row.0: FOUND | alt.row. count 8 >= 2: PASS | alt.row. count 8 <= 9: PASS |
| `fork-episode-click` | Fork-only: the local addon's series has real episodes with synthetic id tt8000000:1:1; pressing its still enters playback with a genuine ratingKey. | PASS | FOUND 'coldopen screen=alt' | FOUND 'coldopen screen=player' | route 'player' == 'player': PASS | a11y id player.control.subtitles: FOUND | a11y id player.co… |
| `fork-subtitle-drill` | Fork-only: Subtitles ▸ Other languages ▸ a language ▸ a track, then REOPEN the menu. The regression is a flat 12+ row list after the pick; the pass is a compact one/two-language menu with the picked row checked. | PASS | FOUND 'coldopen screen=alt' | FOUND 'coldopen screen=player' | route 'player' == 'player': PASS | a11y id menu.off: FOUND | a11y id menu.forced: FOUND | a11y… |
| `fork-home-catalog-shape` | Deterministic replacement for the public-benchmark catalog: 41 shelves, 300 movies, locally generated 250x375 art with 220-character tokenized URLs. Host smoke only; the device gate stays on the real addon set. | PASS | route=home loop samples=22, median=120, robust_min=111, drift=+5.4 | loop_floor 50: PASS | (fps_floor 20: PASS | median=120 fps (min=83, max=126, n=22) vs fp… |
| `fork-detail-telemetry` | S10 pin: the shim's four detail phase legs are present, on a deterministic local item. | PASS | MATCH /detail: shim rk=\d+ meta_ms=\d+ streams_ms=\d+ tmdb_ms=\d+ total_ms=\d+/ | route 'detail' == 'detail': PASS | a11y id detail.hero.play: FOUND | a11y i… |
| `fork-privacy-erase` | S4 pin: Delete all local data clears the private redb only; never run this on a real install. | PASS | FOUND 'nuvio: synthetic local data erased' | a11y id privacy.delete: ABSENT |
| `fork-catalogs-selector` | S7 pin: the Movies header exposes Catalogs, the menu opens, and selecting a row changes the grid. | PASS | FOUND 'coldopen screen=library_menu' | route 'library' == 'library': PASS | a11y id library.header.catalogs: FOUND |
| `sc-s1-sealed-boot` | S1 + S9: sealed nuviolive boot with local addon and all five host FFmpeg binds, no MISSING line. | PASS | FOUND 'nuvio: LIVE addon installed:' | FOUND 'nuvio: LIVE tmdb=configured' | FOUND 'nuvio: SYNTHETIC private redb store' | FOUND 'nuvio: shim serving on' | M… |
| `sc-s2-person` | S2: cast card opens a person page served by the shim from the local TMDB fixture. | PASS | FOUND 'coldopen screen=person' | MATCH /person: profile guid=/ | MATCH /person: credits .* rows=/ | route 'person' == 'person': PASS | person.filmography. co… |
| `sc-s5-playback-pipeline` | S5: demux/framing/clock with a real ratingKey from the local addon; picture is black by design. | PASS | MATCH /clocksink: ARMED/ | MATCH /ff: v=#0 codec=h264/ | MATCH /playing item: rk=\d+ audio=\d+ subs=\d+/ | route 'player' == 'player': PASS | a11y id player.… |
| `sc-s6-dead-addon` | S6: one dead addon degrades gracefully; the local addon still lands a populated Home. | PASS | FOUND 'nuvio: LIVE addon FAILED' | FOUND 'hubs: landed' | route 'home' == 'home': PASS | home.shelf.0.card. count 12 >= 1: PASS |
| `sc-s8-brand-about` | S8: About page opens; copy must be read from the shot (GPL, upstream attribution, TMDB notice, BetterPosters). | PASS | FOUND 'overlay=legal' |
| `sc-s11-p2p-gate` | S11: toggle Enable P2P off in the SAME boot, then open Sources; the toggle state and the empty-picker text must be read from sc-s11-p2p-off-1.png (log-only proof is unreliable for a visual setting). | PASS | FOUND 'coldopen screen=alt' | route 'detail' == 'detail': PASS |
| `sc-s12-resume` | S12: stop a partial playback, back to Home, and assert the Continue Watching card; the Resume pill label must be read from the shot. | PASS | MATCH /clocksink: ARMED/ | a11y id home.continue.card.0: FOUND |
| `sc-s14-logo-plot` | S14: Home hero plot and player clear logo are VISUAL pins — open sc-s14-home-hero-1.png; no log-only proof. | PASS | FOUND 'coldopen screen=home' | route 'home' == 'home': PASS | home.shelf.0.card. count 12 >= 1: PASS |
| `sc-s15-trailer-dash` | S15: trailer card opens the player; DASH manifest/framing evidence is in the log if the host demux is armed. | PASS | MATCH /trailer:|dash:/ | route 'player' == 'player': PASS |
| `sc-s16-fake-mode` | S16: fixed fake catalog with no public addons and no TMDB; the control that makes S1 meaningful. | PASS | FOUND 'nuvio: SYNTHETIC private redb store' | FOUND 'nuvio: SYNTHETIC debug backend' | ABSENT 'nuvio: LIVE' | route 'home' == 'home': PASS | home.shelf.0.car… |

## What 'covered' means here

A translated scene boots the real app in the sealed nuviolive profile with upstream's
own dev triggers, route/overlay and key walk, and is graded by `tests/run.py`'s
heartbeat parsers. Reach assertions and page-landing checks from upstream's `walk`
blocks are not yet implemented in the fork, and the generated key sequences are
approximations sized from the manifest rather than from the live catalog.

Regenerate with `tools/nuvio-items.py`, `tools/nuvio-scenes.py` and
`tools/nuvio-sim-scenes.py --max-run-s N`; pass the run log to this tool with `--results`.
