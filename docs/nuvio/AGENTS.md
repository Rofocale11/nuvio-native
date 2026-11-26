# Nuvio Native agent rules

Read this before changing anything in this checkout. The root `AGENTS.md` is upstream PlxNative
guidance; everything below overrides it where the two disagree.

## This is not a Plex client

Nuvio Native browses and plays Stremio addon streams through an in-process PMS-dialect shim.
`plex/`, `session/` and the PMS REST vocabulary in the UI are a compatibility boundary, not the
product. Never reintroduce a plex.tv account flow, a Plex server list, a Play Version disc for a
Nuvio item, or upstream's sign-in copy. Nuvio addon sources belong in the **Streams** menu only.

## Brand and attribution

The product is **Nuvio Native**. No user-facing string may describe it as a Plex client, name a Plex
server/account as something the viewer uses, or print `PlxNative` except as upstream attribution.
Brand copy lives in the fork-owned `nuvio-locales/brand.json`, applied by
`rust-modules/os/build_support/catalog_brand.rs` during the catalog build — never edit an upstream
`locales/<tag>/*.json` file for branding. The About page must keep the exact TMDB notice, the TMDB
mark and the BetterPosters credit, and state that this is a fork of PlxNative under GPL-3.0-or-later
with upstream's © Gleb Linnik attribution intact. Wire identifiers (`X-Plex-*`, `ratingKey`, PMS
routes, `plx_plex::identity::PRODUCT`) stay upstream's: rebranding must not change a byte on the wire.

## Mark every upstream file you touch

For any file owned upstream that this fork edits, put the literal string `Fork Rofocale11:` in a
comment beside the edit, with a sentence explaining *why*, not just *what*. Examples:

```rust
// Fork Rofocale11: the Nuvio account has no plex.tv row, so ...
```

Formats that have no comment syntax (generated lockfiles, JSON, some Markdown prose) cannot carry
the marker inline: record the edit in `docs/nuvio/MERGE-AUDIT.md` instead. Never treat "it's only a
doc" as an exemption. New fork behaviour should land in fork-owned places (`plx_nuvio_probe`,
`plx_nuvio_store`, `plx_nuvio_dash`, `docs/nuvio/`) so an upstream merge cannot reintroduce the
conflict. Where a feature genuinely needs an upstream crate's private helpers — the trailer DASH
demux needs the Feed's AVIO and packet-framing machinery — put it in a NEW file that is a child
module of the upstream file (`play-feed/src/dash.rs` under `ff`), so the diff to the upstream file
is only a `mod` declaration and a routing branch. `MERGE-AUDIT.md` records the exact surface.

## Every feature lives in its own fork crate or file when it can

Put each new fork feature in a fork-owned crate (`plx_nuvio_probe`, `plx_nuvio_store`,
`plx_nuvio_dash`, …) or, when it genuinely needs an upstream crate's private helpers, in a NEW
fork-owned file that is a child module of the upstream file (`appkit/src/more_menu/nuvio_streams.rs`
under `more_menu.rs`, `play-feed/src/dash.rs` under `ff`, `src/app/nuvio_more.rs` under `app`). The
upstream file then changes by only a `mod` declaration and one or two small, marked routing
branches.

This is not tidiness, it is the cost of merging. Every line we edit in an upstream file is a line
someone has to reconcile the next time `upstream/main` moves that file; a fork-owned file never
conflicts, no matter how much upstream changes around it. Before writing a feature, ask "can this
be a new file?" and only touch upstream code when the answer is no. When it is no, keep the edit to
the minimum (a `mod`, a variant, a branch) with a `Fork Rofocale11:` marker and a record in
`MERGE-AUDIT.md`. Deleting an upstream feature outright is the expensive way to "remove" it: the
code stays and is simply not offered on fork content, as with the player's quality ladder and the
Nuvio Streams picker.

## One durable store

Every Nuvio Native record lives in a single `nuvio.redb` under `<data>/shim/`: canonical session
and Settings preferences, addon config, install preferences, Nuvio account preferences, watch
state, key table, account session, local settings, the metadata/enrichment caches and the durable
IMDb→TMDB id map. There is no DB8 record and no `plxstorage` helper in the package. Add new
persisted state as a table in `plx_nuvio_store`, not a new file or a new DB8 kind. The file has a
50 MiB soft budget: expired and oldest TTL cache rows are shed and the file is compacted at boot,
never truncated; durable records are kept even if the budget is exceeded. An unreadable canonical
record is quarantined as `<id>.rejected-<millis>` and read as missing so sign-in recovers without
SSH. This is the normal-boot rule: a synthetic boot installs a private ephemeral redb and host tests
use temp stores; the shim's `data_dir` field is only a legacy-migration directory, not a store
location (`PMSSHIM.md` §11). See `ARCHITECTURE.md` for the in-process backend and the migration
limits.

## The old fork's shim behavior is authoritative

`pmsshim` is the old fork's shim source, ported wholesale. When a merge or a host port disagrees
with the old fork about shim behavior — hub rows, BetterPoster selection, `/photo` fallbacks,
poster URLs — the old fork is right and the newer tree is the regression. Port the old behavior;
adapt only the **boundary** (storage, renamed crate APIs, types this tree no longer owns) and add a
test that pins the behavior. Do not "improve" the shim's poster or catalog logic to match the newer
fork's rules. `MERGE-AUDIT.md` records the API adaptations made in the wholesale port.

## Verification and device work

- Prefer focused host gates over repeated full `make check` loops. After Rust edits also run
  `make check-nodef`; an ARM cross-build is required before claiming the target still builds.
- **The default agent host loop is `nuviolive`, not upstream's mock and not the TV.** Load the
  `nuvio-sim-loop` skill (or read `SIM-LOOP.md`) first: `make sim-nuvio` (or `make sim-nuvio-linux`) once, then
  `python3 tools/nuvio-sim.py --shot home`. It boots the real app with real public addons and the
  dedicated TMDB key from the gitignored `nuvio.local.env`, with a sealed identity and private
  store; no sign-in and no owner involvement. Use `plxnative-nuviofake` only for fixed-content
  deterministic tests.
  The device twin is `docs/nuvio/TV-LOOP.md` (`tools/nuvio-tv.py`): same live profile on the TV, with
  a persistent session and one lease, for agents with explicit permission. Keep `SIM-LOOP.md` host-only.
- The owner tests the TV, and only with explicit permission in the session. Acquire the TV lock for
  any device command; release it immediately after. Never boot with a real account for screenshots.
  Upstream's `--guest`/mock cannot do this on this fork: the in-process shim short-circuits its
  identity and an **ordinary** boot needs a stored Nuvio sign-in to reach Home (`PMSSHIM.md` §4).
  A synthetic boot is the deliberate exception. Playback is the only remaining device need and is
  already verified; a simulator result is never a device claim.
- No commit, push, merge, PR or deploy without the owner's explicit instruction in that session.

## Documents of record

- `docs/nuvio/ARCHITECTURE.md` — current components, storage, identity.
- `docs/nuvio/ID-MANAGEMENT.md` — numeric `ratingKey`s, the key table and the five synthetic key
  ranges; read with `TMDB.md`.
- `docs/nuvio/TMDB.md` — the TMDB client contract and where TMDB is allowed (detail page and its
  children only; no TMDB image is used for a hub row). Artwork behavior as actually shipped is in
  `PMSSHIM.md` §7–8.
- `docs/nuvio/PLAYBACK.md` — probe, route and shim playback contract.
- `docs/nuvio/PMSSHIM.md` — end-to-end shim walkthrough: boot, HTTP, identity, catalog, artwork,
  playback, trailers, store, synthetic mode, tests and open code decisions.
- `docs/nuvio/TRAILERS.md` — trailer ids, Node decipher, MPD/Feed demux, autoplay and full-trailer
  modes, and the held-play supersede rule.
- `docs/nuvio/TORRENT-ENGINE.md` — the `libsbengine.so` / Nuvio Engine layers, budgets, stream
  resolve, warm and settings.
- `docs/nuvio/DETAIL-LATENCY.md` — detail-page phase timings, what is measured on TV and what is
  still over target.
- `docs/nuvio/WATCH-PROGRESS.md` — account mirror: IMDb identity, push/pull RPCs and Home refresh.
- `docs/nuvio/VALIDATION.md` — what has and has not been verified. Its early sections are dated
  snapshots; the current status is at the top.
- `docs/nuvio/SCENARIO.md` — reproducible host-loop scenarios for fork-specific behaviors, with the
  pass evidence and regression signature of each. An agent confirming a regression starts here.
- `docs/nuvio/HANDOFF.md` — delivery history and known limitations. It opens with the current
  status; everything below the first section is the 2026-10-10 snapshot.
- `docs/nuvio/MERGE-AUDIT.md` — the CURRENT merge only: its conflicts, regressions and non-commentable fork edits. A fresh file is written at the start of every upstream merge, after rotating the previous one into history.
- `docs/nuvio/MERGE-AUDIT-HISTORY.md` — completed audits, reference only, never current policy. Every entry MUST be headed with its exact commit range, `## <previous upstream merge point>..<new upstream head> — <date>..<date>`, plus the product-branch merge commit once it lands, so a reader can tell precisely which upstream commits that audit resolved. A merge is not finished until the old audit is appended under that marker and a fresh `MERGE-AUDIT.md` exists.
