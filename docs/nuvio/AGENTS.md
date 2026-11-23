# Nuvio Native agent rules

Read this before changing anything in this checkout. The root `AGENTS.md` is upstream PlxNative
guidance; everything below overrides it where the two disagree.

## This is not a Plex client

Nuvio Native browses and plays Stremio addon streams through an in-process PMS-dialect shim.
`plex/`, `session/` and the PMS REST vocabulary in the UI are a compatibility boundary, not the
product. Never reintroduce a plex.tv account flow, a Plex server list, a Play Version disc for a
Nuvio item, or upstream's sign-in copy. Nuvio addon sources belong in the **Streams** menu only.

## Mark every upstream file you touch

For any file owned upstream that this fork edits, put the literal string `Fork Rofocale11:` in a
comment beside the edit, with a sentence explaining *why*, not just *what*. Examples:

```rust
// Fork Rofocale11: the Nuvio account has no plex.tv row, so ...
```

Formats that have no comment syntax (generated lockfiles, JSON, some Markdown prose) cannot carry
the marker inline: record the edit in `docs/nuvio/MERGE-AUDIT.md` instead. Never treat "it's only a
doc" as an exemption. New fork behaviour should land in fork-owned places (`plx_nuvio_probe`,
`plx_nuvio_store`, `docs/nuvio/`) so an upstream merge cannot reintroduce the conflict.

## One durable store

Every Nuvio Native record lives in a single `nuvio.redb` under `<data>/shim/`: canonical session
and Settings preferences, addon config, install preferences, Nuvio account preferences, watch
state, key table, account session and local settings. There is no DB8 record and no `plxstorage`
helper in the package. Add new persisted state as a table in `plx_nuvio_store`, not a new file or a
new DB8 kind. See `ARCHITECTURE.md` for the in-process backend and the migration limits.

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
- The owner tests the TV. Acquire the TV lock for any device command; release it immediately after.
  Never boot with a real account for screenshots — use `--guest`/mock and read the identity line.
- No commit, push, merge, PR or deploy without the owner's explicit instruction in that session.

## Documents of record

- `docs/nuvio/ARCHITECTURE.md` — components, storage, identity.
- `docs/nuvio/TMDB.md` — the TMDB client contract, the five synthetic key ranges, and the
  artwork order: TMDB backdrop/logo first, BetterPosters posters by resolved IMDb id,
  metahub art fallback-only.
- `docs/nuvio/PLAYBACK.md` — probe, route and shim playback contract.
- `docs/nuvio/WATCH-PROGRESS.md` — account mirror: IMDb identity, push/pull RPCs and Home refresh.
- `docs/nuvio/VALIDATION.md` — what has and has not been verified.
- `docs/nuvio/HANDOFF.md` — delivery history and known limitations.
- `docs/nuvio/MERGE-AUDIT.md` — upstream conflicts and non-commentable fork edits.
