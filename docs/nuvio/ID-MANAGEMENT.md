# NuvioNative ID management: numeric keys, the key table, synthetic keys

The shim is the only place that knows both vocabularies — PlxNative's numeric `ratingKey` and the
string ids Stremio addons and TMDB speak. This is how it translates between them. Read with
`ARCHITECTURE.md`.

## Why the two vocabularies exist

- **PlxNative requires numbers.** `ratingKey` is parsed as an `i64`. A string such as `"tmdb:603"`
  blanks the screen or crashes the UI, so every item the shim hands the app MUST carry a numeric id.
- **Stremio and TMDB require strings.** Addons are asked for `tmdb:603`, `tt0133093`,
  `tt0944947:1:2`; TMDB's own id is a bare integer inside a JSON body.

The shim therefore gives the UI a **numeric** key and remembers — or decodes — the **string**
identity behind it. Two mechanisms do that:

- **The key table (`shim_store::Keys`)** — an append-only `keys.tsv`. Line *N* is ratingKey
  `10_000 + N` (`shim_store::FIRST_KEY`), and the line records the real `Ident`
  (`Title { type_, id }`, `Season`, `Episode`). This is what real catalog items use, and it
  survives restarts so Continue Watching and cached images keep resolving.
- **Synthetic keys** — for TMDB recommendations and franchises that no addon carries there is no
  `Ident` to store, so the TMDB id is encoded arithmetically and decoded on the way back in.

## The synthetic bases (do not change or overlap)

| Kind | Formula | Example | Range |
| --- | --- | --- | --- |
| Related **Movies** | `1_000_000_000 + TMDB_ID` | `1_000_000_603` (The Matrix) | `1_000_000_000 .. 2_000_000_000` |
| Related **TV Shows** | `2_000_000_000 + TMDB_ID` | `2_000_001_396` (Breaking Bad) | `2_000_000_000 .. 3_000_000_000` |
| **People** (Cast & Crew) | `3_000_000_000 + TMDB_PERSON_ID` | `3_000_000_638` (Keanu Reeves) | `3_000_000_000 .. 4_000_000_000` |
| **Collections** (franchises) | `4_000_000_000 + TMDB_COLLECTION_ID` | `4_000_000_234` (The Dark Knight Collection) | `4_000_000_000 ..` |

The bases are a billion apart on purpose: TMDB movie ids already pass 100,000, so a narrower gap
lets a real movie key (`9_641_052` = movie `141_052` on the old bases) cross into the show range and
load the wrong title.

```rust
const TMDB_FALLBACK_KEY_BASE: i64 = 1_000_000_000;
const TMDB_SHOW_KEY_BASE: i64 = 2_000_000_000;
const TMDB_PERSON_KEY_BASE: i64 = 3_000_000_000;
const TMDB_COLLECTION_KEY_BASE: i64 = 4_000_000_000;

/// A synthetic related-movie ratingKey's TMDB id.
fn synthetic_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_FALLBACK_KEY_BASE..TMDB_SHOW_KEY_BASE)   // bounded: a show key never decodes as a movie
        .contains(&rk)
        .then_some((rk - TMDB_FALLBACK_KEY_BASE) as u64)
}

/// A synthetic related-show ratingKey's TMDB id.
fn synthetic_show_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_SHOW_KEY_BASE..TMDB_PERSON_KEY_BASE)     // bounded: a person key is not a show
        .contains(&rk)
        .then_some((rk - TMDB_SHOW_KEY_BASE) as u64)
}

/// A Cast & Crew headshot's synthetic `personId` → TMDB's person id.
fn synthetic_person_id(rk: i64) -> Option<u64> {
    (TMDB_PERSON_KEY_BASE..TMDB_COLLECTION_KEY_BASE) // bounded: a collection is not a person
        .contains(&rk)
        .then_some((rk - TMDB_PERSON_KEY_BASE) as u64)
}

/// A synthetic collection ratingKey's TMDB collection id.
fn synthetic_collection_id(rk: i64) -> Option<u64> {
    (rk >= TMDB_COLLECTION_KEY_BASE).then_some((rk - TMDB_COLLECTION_KEY_BASE) as u64)
}
```

- Each decoder is **bounded above** by the next base. Without that, `3_000_000_638` would also
  decode as the show a billion ids below it, and a headshot would open the wrong title.
- The bases are above every real key (`10_000 + N`), so the five spaces cannot collide.
- **Check the bases highest-first.** A key `>= 4_000_000_000` is always a collection,
  `>= 3_000_000_000` a person, `>= 2_000_000_000` a show, and a key in the movie range is always a
  movie.
- A person key is a **`personId`, not a `ratingKey`** — it travels in `Role[].id` on a cast row,
  which is what the app's cast shelf reads to open `/library/people/{id}/media` (see `TMDB.md`).
- A collection key is a **`ratingKey`** for a franchise row — it is never sent to an addon (nothing
  plays a collection), and it exists so the shelf's linked heading can open the collection page.

Every synthetic kind is handled on the way back in by `/library/metadata/{rk}`: parse the key,
decode it with the matching helper, fetch TMDB, and return the enriched Plex row
(`tmdb_details_row` for movies, `tmdb_show_details_row` for shows, `collection_row` for a
franchise). `/library/metadata/{rk}/related` recognises the synthetic title ranges too, so a
TMDB-only related title can drill down forever, and a film in a franchise carries its collection
shelf there. The `photo` route resolves the same keys, so a synthetic title's
`clearLogo`/`thumb`/`art` are served even though the key table never heard of it.

## Global ID translation

Addons never see a synthetic number. Every addon-facing request translates the key first:

```rust
/// The Stremio id addons know a synthetic related ratingKey by (`tmdb:603`), if it is synthetic.
fn translate_to_stremio_id(rk: i64) -> Option<String> {
    synthetic_show_tmdb_id(rk)
        .or_else(|| synthetic_tmdb_id(rk))
        .map(|tmdb_id| format!("tmdb:{tmdb_id}"))
}
```

**Streams and subtitles MUST both be translated.** They are the two requests that leave the shim for
an addon, and an untranslated `1_000_000_603` reaches Torrentio (and every subtitle addon) as an
unknown id — an empty Source Picker and no subtitles.

Rather than patch each caller, the translation lives in `Shim::slot()`, the one function that
starts **both** fetches:

```rust
fn slot(&self, rk: i64, ident: &Ident) -> Option<Arc<StreamSlot>> {
    let (type_, id) = self.stream_target(ident)?;
    // A synthetic related key exists only as `tmdb:<id>` to addons: streams and subtitles are
    // asked for that, never for the shim's own number.
    let id = translate_to_stremio_id(rk).unwrap_or(id);
    // … spawns "pmsshim-streams" (backend.streams) and "pmsshim-subs" (backend.subtitles)
}
```

- `unwrap_or(id)` leaves real catalog items untouched — their `Ident` id is already the Stremio id.
- Because `slot()` is the single entry point for both threads, a new addon-facing call must go
  through it (or translate explicitly) or it will leak synthetic keys.
- `/nuvio/streams` additionally accepts a synthetic key that has **no** `Ident` (synthetic keys are
  never written to `keys.tsv`), synthesising `Title { type_: "movie" | "series", id: "tmdb:<id>" }`
  so the Source Picker works on a related title.
- `complete` comes from `ranked()`, never from the slot map. An absent slot means *nothing to ask
  for* (a related show has no episode yet), so it must answer `complete: true` with an empty list —
  reporting `false` makes the picker re-poll 60× and show "loading" forever.
- `ranked()` takes a `chosen` flag, `true` on the two routes a PICK leads to (`/nuvio/select`'s own
  resolve: the `/decision` and the `Part` it opens) and `false` on the detail page's fetch and
  `/nuvio/streams`. With it set, the wait for more addons ends the moment a stream is recorded — the
  chosen file cannot be superseded, so those requests must not sit out `stream_grace` (2.5 s after
  the first playable stream) or, for a picked entry the local playable test does not recognise, the
  full `stream_deadline` (60 s). That wait was why a pick made during progressive loading appeared
  to wait for every addon to finish.
