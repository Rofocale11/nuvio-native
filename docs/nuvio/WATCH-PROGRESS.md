# Nuvio Native — account watch progress

Continue Watching is the account's, not the device's. Before this, a resume point existed only in
the shim's local watch file: play twenty minutes on the TV, open the same film in the Nuvio webOS
app or on a phone, and it started from zero. The Nuvio account already keeps a per-profile watch
history (its own apps write it), so the shim now mirrors every timeline report into that history and
merges what another device wrote back into its local state.

Nothing about the player, the UI or the PMS dialect changed. The app still reports
`/:timeline` and still reads Continue Watching from `/hubs/continueWatching`; the account is a second
store behind those two endpoints.

## Shape

```
  player ──/:timeline──────────────▶ Shim::push_watch_progress ──▶ Account::push_watch_progress
             (existing, spawned)        │  identity + throttle        │  session + profile
                                        ▼                             ▼
                                   WatchState (local)        nuvio::push_watch_progress
                                   watch.json                        │ POST
                                                                     ▼
                                              /rest/v1/rpc/sync_push_watch_progress

  boot, or a profile switch ──▶ Shim::pull_account_watch ──▶ Account::pull_watch_progress
                                        │  merge newer wins        │ POST
                                        ▼                          ▼
                                   WatchState.merge_remote   /rest/v1/rpc/sync_pull_watch_progress
```

| piece | file |
|---|---|
| RPC bodies, row parsing, `WatchProgress` | `rust-modules/stremio/src/nuvio.rs` |
| session, profile resolution, `WatchSink` | `rust-modules/pmsshim/src/shim_account.rs` |
| identity mapping, throttle, pull/merge wiring | `rust-modules/pmsshim/src/shim_pms.rs` |
| merge policy, account-key map, `watch.json` | `rust-modules/pmsshim/src/shim_store.rs` |
| boot pull and sink installation | `rust-modules/pmsshim/src/shim_core.rs` |

## The two RPCs

Both are Supabase functions on the same backend the My List calls use, and both take the profile as
`p_profile_id` — a `u32` parsed by the same `library_profile` guard My List uses, so a malformed
profile is refused before any request is made.

- `sync_push_watch_progress` takes `p_entries`, an array of at most 50 rows. Each row is
  `content_id`, `content_type` (`movie` | `series`), `video_id`, `season`, `episode`, `position`,
  `duration`, `last_watched` (unix **milliseconds**) and the `progress_key` the account reads by.
  Entries are validated as they are built: an unknown content type, an empty id, an id over 512
  bytes, or an id containing a control character refuses the whole batch rather than sending it.
- `sync_pull_watch_progress` takes `p_limit` (200) and returns the same row shape (plus `season`
  and `episode`). Field names are read in both `snake_case` and `camelCase` because the function's
  return type is not pinned to one.

A reply that is not an array is a parse error, not an empty history — silently treating a malformed
page as "nothing watched" would hide a backend change. Nothing here logs a response body: these rows
are the household's viewing record.

## Identity

**Nuvio names a title by its IMDb id** (`content_id` = `tt0133093` for a film, `tt0903747` for a
series); `tmdb:` is its fallback for a title IMDb is not known for. Its apps match Continue
Watching on that form, so a `tmdb:` row they cannot match is as good as missing — this device
resolves a local `tmdb:` catalog id to IMDb through `TmdbClient` before pushing, and keeps `tt…`
ids untouched. The `video_id` is the film's own id, or the episode's addon id
(`tt0903747:1:2`). The account also keys an episode by `content_id_s{season}e{episode}`, so the
season and episode are sent as their own fields (`progress_key`) — without them every episode of a
series would overwrite the same row.

An episode row from another Nuvio client may name its episode synthetically
(`__nuvio_episode__:S:E`) when the playback did not carry the addon's episode id. The pull places
such a row through the series meta's season/episode, so it lands on the addon's real episode (and
has a row snapshot Continue Watching can draw) instead of a key no addon can answer.

- A `tmdb:` id needs no key and no network: it is used as the pushed id only when TMDB names no
  IMDb id for it.
- An IMDb id resolves through `TmdbClient`, which needs `TMDB_API_KEY` (environment or build
  time). A title with neither an IMDb nor a TMDB id (kitsu, …) does not sync.
- A season row and a series row report nothing. Neither has a single position of its own; their
  episodes do.

The reverse direction, `account_watch_key`, first asks the local `account_keys` map (below), then
canonicalises the row to IMDb (a legacy `tmdb:` row resolves the same way the push does) and
rebuilds the `Ident` it names, interning it through `Keys::key_of`. Both directions therefore name
one title the same way: a row written earlier as `tmdb:N` and a newer one as `tt…` land on one
local item, one Continue Watching card. It deliberately does not bind on a miss: a pull whose merge
loses to the local state must not grow the map.

`account_keys` in `watch.json` is `"content_id|video_id" → ratingKey`, written when this device
pushes an item and when a pulled row actually merges. It is the durable answer to "which local item
is this account row", and it exists because the two sides can name an episode differently: this
device interning an addon's `tt0944947:1:2` under its own series id is not the same `Ident` that a
pulled row rebuilds from the account's `tt0903747`, so rebuilding by identity alone can intern a
second ratingKey for one episode. Rebuilding stays the fallback for a row no device here pushed.

## Push

`/:timeline` already spawned a thread to write local progress off the reporting request. That thread
now also calls `push_watch_progress`, so the account never sees a request the local state did not.

The app reports every few seconds, so an unthrottled push would be an RPC per ten seconds for the
length of a film. `watch_push_due` allows one push per item per 60 s (`WATCH_PUSH_INTERVAL`) while
`state=playing`, and lets a `paused` or `stopped` report through at once — a pause or a stop is the
position a person actually returns to, and it is the last report of a session. The throttle map is
capped at 512 items and cleared wholesale when it overflows.

`removeFromContinueWatching` pushes a zero position. `merge_remote` ignores a zero position, so the
card does not come back on the next pull. Nuvio's clients have a `sync_delete_watch_progress` RPC;
this shim still writes zero instead, so the row remains at 0 on the account — worth knowing before
anyone "fixes" the pull to treat zero as authoritative.

A failed push is logged as a status code only (`nuvio account: progress push failed status=N`) and
changes nothing locally. The local write already happened; losing the mirror is not losing playback.

## Pull

`pull_account_watch` runs on its own `pmsshim-watch` thread, from two places:

- at boot, right after the sink is installed (`shim_core::start_on`), so Home's shelf is populated
  before the first draw;
- whenever a profile is selected, because a profile's history is its own — switching profiles must
  not leave the previous one's resume points on screen.

**The profile's addons are installed before that pull fires** (`Account::load_profile`). That order
is load-bearing: the pull builds each resume point's row through the addon backend (`Shim::row`),
and `WatchState::in_progress` drops an entry whose row snapshot is missing. Pulling first stored the
position with no row, so Continue Watching stayed empty until the next boot pulled it again against
the real addons. The load also re-checks the library generation after the manifest fetch, so a
profile picked while an older load was still in flight neither installs its set nor pulls its
history.

**The pull also tells the client when it changed something.** A non-zero merge bumps
`Shim::content_generation`; the app polls that count and re-reads Home's hubs once per observed
value. Without the count the merge landed after Home's first `/hubs` and nothing asked again. The
app does not consume that change while it has no registered server (a first sign-in can merge before
the account's server is installed): it stays owed until one exists.

**The re-read alone was not enough — the deck store must also adopt the cards.** Home's first answer
of a fresh account is *empty* Continue Watching (`/hubs` has no rows because the pull has not merged
yet, and `/hubs/continueWatching` is empty). `stores::deck::place` seats a source's lane on that
preview and marks it placed; the follow-up answer, now carrying rows, was skipped ("already
placed"), so `fit` had nothing to fit and the merge published no `home.continue` row. The result was
exactly the reported bug: the rows were in the shim (the Library's section rows showed them) and
Home stayed empty until the next launch, whose *first* answer already had the pull's persisted rows.
`place` now treats a placed-but-empty lane as unplaced when a later preview brings cards.

The app's baseline for that count is captured before the first `/hubs` is issued, not when the `App`
struct is built: `activate_session` installs the server and starts Home's first fetch before the
literal exists, so a merge landing in between used to be recorded as "already seen" while the fetch
in flight predated it.

Each row is filtered cheapest-first: a zero position is dropped, the local stamp is compared before
any row snapshot is fetched, and only a merge that lands pays for `me.row(rk)` and a bind.

The merge policy is in `WatchState::merge_remote`: the newer side wins, compared on
`last_viewed_at` in seconds (the account's milliseconds divided by 1000). A remote row that is older
than or equal to the local one is ignored. `view_count` is never touched — Nuvio has no idea about
our scrobbles, so a finished title can take a resume point without being un-finished.

## Profile switching and lock order

`load_profile` fires the watch sink after it commits a new profile. It had to stop holding the
`library` guard across that call: the sink's pull path resolves its profile through the same guard,
so calling it inside the critical section deadlocks on the first switch. The guard is dropped, the
generation is re-checked, and only then does the sink fire — a superseded load still does not nudge.

`push_watch_progress` and `pull_watch_progress` take the `library` guard only briefly inside
`profile_id`, never across the HTTP call. A push happens while the player is reporting; holding that
lock across a network round trip would stall the UI's account reads behind playback.

## Last played source

The account mirror above carries only the position. A resume that re-ranked
from scratch often picked a different file: seeders move, addon order moves,
and the film's best source on Monday is not the best source on Friday. To
keep Continue Watching on the same release a viewer actually chose, the
shim also keeps — **local to this TV, inside the same `watch.json`** — a
stable identity of the stream each resume point last played:

- addon name, and for a torrent the `info_hash` + `file_idx` (the engine
  selects its file by that digest); for a direct/debrid stream the addon,
  file name, size and height. **Never the signed URL and never the arrival
  index:** a debrid link expires on every re-resolution, and the arrival
  index shifts as addons answer.
- The identity is written when a stream is actually probed at `/decision`
  (the play that starts), not on a detail read and not on every
  `/:timeline` tick.
- On the next play, `ranked()` promotes a still-offered stream matching
  that identity ahead of the best-ranked one — an explicit picker choice in
  the current slot outranks the memory, and a source the addons no longer
  offer falls back to plain ranking instead of promoting a dead pick.
- `merge_remote` does **not** touch it: a position pulled from the account
  is about where a viewer stopped, not which file this TV should prefer.
  Each episode is its own ratingKey, so an episode's pick cannot bleed into
  another episode of the same series.

## What is not covered

- A scrobble (watched) is not pushed. Nuvio's history has no separate watched flag in this RPC pair,
  so a title finished on the TV still shows as in progress on another device until that device
  plays past it.
- Nothing is pushed while signed out, and nothing is pushed for a title that has neither an IMDb
  nor a TMDB id (a kitsu-only entry).
- The local `watch.json` and the `account_keys` map live in the shim's
  data directory — `/media/developer/{app id}.nuvio/shim` on a TV, which is
  persistent storage; the runtime directory (`/tmp`) is only the fallback
  when that path is not writable. A TV reboot therefore keeps the local
  side; the boot pull still refreshes positions from the account. A row
  whose account identity no local addon can resolve a meta for (an episode
  the series does not list, a title whose id the addon set does not answer)
  has no row snapshot and stays off Continue Watching.
- Deletions are a zero position, not a row delete (see Push).

## Verification

Host tests, `make test-crate C=plx_pmsshim` and `C=plx_stremio`:

- `shim_store::tests::merge_remote_takes_only_the_newer_side` — an older remote row is ignored, a
  newer one lands with its row snapshot and duration, a zero position is not a resume point, and a
  finished title keeps its `view_count` while taking a resume point.
- `shim_pms::account_progress_tests::a_movie_and_an_episode_are_named_by_imdb` — a local `tt…`
  movie is pushed under that id (never rewritten to `tmdb:`), and an episode carries its series'
  IMDb id, its own episode id and the season/episode pair.
- `shim_pms::account_progress_tests::a_synthetic_episode_row_is_placed_through_season_and_episode`
  — a `__nuvio_episode__:S:E` row lands on the addon's real episode, and a season/episode the
  series does not have is skipped rather than stored rowless.
- `shim_pms::account_progress_tests::a_remembered_torrent_promotes_even_when_ranking_demotes_it`
  — a ranking that puts the remembered release second promotes it back to `Media[0]`;
  `a_missing_source_and_an_explicit_pick_steer_randomly_differently` — a source the addons no
  longer offer leaves the ranking alone, and an explicit in-slot pick beats the memory.
- `shim_store::tests::last_stream_survives_a_reopen_and_clears_with_the_resume_point` — the
  identity persists in `watch.json` across a reopen, and finishing at/after 90% clears it.
- `nuvio::progress_tests::an_episode_row_names_its_series_and_its_own_progress_key` — the
  `content_id_sNe` key and the season/episode fields of the push body.
- `nuvio::progress_tests` — the push body's row shape and the 50-entry cap, and row parsing from
  both field spellings.

At the time of writing `plx_pmsshim` is 125/125 and `plx_stremio` is 65/65, and the shipping
feature set type-checks under `--no-default-features`.

Not verified on the TV against a real account. The push/pull shape follows Nuvio's own clients
(WebOS `js/core/profile/watchProgressSyncService.js` and Android
`core/sync/WatchProgressSyncService.kt`): IMDb `content_id`, `content_id_sNe` progress keys, and
`last_watched` in milliseconds. A device check should play past a few minutes on the TV, then open
the same title in the Nuvio webOS app and confirm the position carried, and the reverse.
