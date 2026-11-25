# Nuvio Native ID management: numeric keys, the key table, synthetic keys

The shim is the only place that knows both vocabularies — PlxNative's numeric `ratingKey` and the
string ids Stremio addons and TMDB speak. Read with `ARCHITECTURE.md` and `TMDB.md`.

## Why two vocabularies

- **PlxNative requires numbers.** `ratingKey` is parsed as an `i64`; a string such as `tmdb:603`
  blanks the screen or crashes the UI. Every item the shim hands the app carries a numeric id.
- **Stremio and TMDB require strings.** Addons are asked for `tmdb:603`, `tt0133093`,
  `tt0944947:1:2`; TMDB's own id is a bare integer inside a JSON body.

The shim gives the UI a numeric key and remembers — or decodes — the string identity behind it.

## The one durable store

Fork Rofocale11: all Nuvio Native state lives in a single `nuvio.redb` under `<data>/shim/`.
In particular:

- **The key table** lives in `plx_nuvio_store`'s `Table::Keys` as one JSON row. The old fork's
  append-only `keys.tsv` is read ONCE as a migration source; runtime reads and writes never touch
  it. `Keys::key_of` / `Keys::ident` behave exactly as in the old fork, and real keys still start at
  `FIRST_KEY = 10_000`.
- **The IMDb→TMDB mapping** (a `tt…` → bare TMDB id resolution used by the detail client) is the
  durable one: `Table::ImdbTmdb` in the same redb holds a plain big-endian `u64` with **no** TTL, so
  a `/find` answer is paid for once per title. `shim_tmdb_ids::PersistentIdLookup` is its only
  writer; negatives are never stored.
- **The reverse mapping** used by `imdb_base()` (`tmdb:<n>` → `tt…`) is an in-memory
  `Shim::imdb_ids` map (512 entries, negative answers cached too), not a redb row. A restart
  re-pays a cold resolution; `TmdbClient`'s 15-minute `(api_key, path)` cache and keyed
  single-flight keep it to one request per burst.
- **Meta and enrichment** answers are one JSON row per `(type, id)` in `Table::MetaCache` /
  `Table::EnrichCache` of that one store, written directly by the shim's `MetaCache` /
  `EnrichCache` with an `at` timestamp. No new JSONL sidecar is written; the old logs are migrated
  once on an empty table. The generic `NuvioCache` / `cache_get` wrapper in `nuvio-store` is now
  exercised only by that crate's own tests.

## Synthetic key bases (do not change or overlap)

| Kind | Formula | Example | Range |
| --- | --- | --- | --- |
| Related **Movies** | `1_000_000_000 + TMDB_ID` | `1_000_000_603` | `1_000_000_000 .. 2_000_000_000` |
| Related **TV Shows** | `2_000_000_000 + TMDB_ID` | `2_000_001_396` | `2_000_000_000 .. 3_000_000_000` |
| **People** (Cast & Crew) | `3_000_000_000 + TMDB_PERSON_ID` | `3_000_000_638` | `3_000_000_000 .. 4_000_000_000` |
| **Collections** | `4_000_000_000 + TMDB_COLLECTION_ID` | `4_000_000_234` | `4_000_000_000 .. 5_000_000_000` |
| **Trailers** | `5_000_000_000 + parent + index` | — | `5_000_000_000 .. TRAILER_KEY_END` |

A billion-wide gap keeps a real movie key (`10_000 + N`) from ever aliasing a synthetic space, and
each decoder is **bounded above** by the next base — `3_000_000_638` must not also decode as a show.
A person key is a **`personId`** in `Role[].id`, not a `ratingKey`; its `tagKey` is the separate
global identity `nuvio-person-<TMDB person id>` that addresses `/library/people/{guid}` and
`/library/people/{guid}/credits`. A collection key is a `ratingKey` for a franchise row and is
never sent to an addon.

## Global ID translation

Addons never see a synthetic number. `translate_to_stremio_id(rk)` maps a related movie/show key to
`tmdb:<id>`; `Shim::slot()` applies it before **both** addon-facing fetches — streams and subtitles.
`unwrap_or(id)` leaves real catalog items untouched. An untranslated synthetic key reaches
Torrentio as an unknown id: empty Source Picker and no subtitles.

A real catalog item whose own id is `tmdb:<n>` is translated too: `addon_id_for()` splits any
episode tail, asks TMDB's `/movie|tv/{id}/external_ids` for the IMDb id, and rejoins the tail. The
mapping is cached in the one redb store, so a polling picker costs one lookup; a miss falls back to
the original `tmdb:` id rather than asking an addon for nothing.

`/nuvio/streams` additionally accepts a synthetic key with no `Ident` (synthetic keys are never in
the key table), synthesising `Title { type_: "movie" | "series", id: "tmdb:<id>" }` so the Source
Picker works on a related title. `complete` comes from `ranked()`, never from the slot map: an
absent slot means *nothing to ask for*, so it answers `complete: true` with an empty list — `false`
would re-poll forever.

## Artwork ids

A hub row's poster is BetterPosters by IMDb id, with the addon's (Cinemeta) poster as the strict
fallback. Home's Cinemeta ids are already `tt…`; Recommended's extra catalogs carry `tmdb:<n>` ids,
which `poster_url` resolves to IMDb **once** via `imdb_base` — the in-memory `Shim::imdb_ids` map
first, then a single cached TMDB `/external_ids` call on a cold miss (positive and negative answers
are remembered in that map). No TMDB image is ever used for a hub; TMDB beyond that one id mapping
is only for the detail page and its children (cast, logo, `art`, trailers, More Like This,
collections). See `TMDB.md`.
