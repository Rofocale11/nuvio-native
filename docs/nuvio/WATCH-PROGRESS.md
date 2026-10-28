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
  `content_id`, `content_type` (`movie` | `series`), `video_id`, `position`, `duration`,
  `last_watched` (unix **milliseconds**). Entries are validated as they are built: an unknown
  content type, an empty id, an id over 512 bytes, or an id containing a control character refuses
  the whole batch rather than sending it.
- `sync_pull_watch_progress` takes `p_limit` (200) and returns the same row shape. Field names are
  read in both `snake_case` and `camelCase` because the function's return type is not pinned to one.

A reply that is not an array is a parse error, not an empty history — silently treating a malformed
page as "nothing watched" would hide a backend change. Nothing here logs a response body: these rows
are the household's viewing record.

## Identity

Nuvio keys its history by TMDB id (`content_id` = `tmdb:550` for a film, `tmdb:1396` for a series)
and by the episode's own addon id in `video_id` (`tt0944947:1:2`). Our ratingKeys are interned addon
ids, usually IMDb. `nuvio_watch_identity` and `tmdb_id` bridge the two:

- A `tmdb:` id, a bare number, or a `TMDB:` id parses locally — no key, no network. This is the
  common case for a Nuvio catalog entry.
- An IMDb id (`tt…`) goes through `TmdbClient`, which needs `TMDB_API_KEY` in the environment (or at
  build time). Without it an IMDb-only title simply does not sync; nothing fails and nothing else
  is affected.
- An episode id is split: its series becomes `content_id`, the full episode id stays `video_id`.
- A season row and a series row report nothing. Neither has a single position of its own; their
  episodes do.

The reverse direction, `account_watch_key`, first asks the local `account_keys` map (below), then
rebuilds the `Ident` the row names and interns it through `Keys::key_of`. It deliberately does not
bind on a miss: a pull whose merge loses to the local state must not grow the map.

`account_keys` in `watch.json` is `"content_id|video_id" → ratingKey`, written when this device
pushes an item and when a pulled row actually merges. It is the durable answer to "which local item
is this account row", and it exists because the two sides name an episode differently: this device
interning an addon's `tt0944947:1:2` under its own series id is not the same `Ident` that a pulled
row rebuilds from the account's `tmdb:1396`, so rebuilding by identity alone can intern a second
ratingKey for one episode. Rebuilding stays the fallback for a row no device here pushed.

## Push

`/:timeline` already spawned a thread to write local progress off the reporting request. That thread
now also calls `push_watch_progress`, so the account never sees a request the local state did not.

The app reports every few seconds, so an unthrottled push would be an RPC per ten seconds for the
length of a film. `watch_push_due` allows one push per item per 60 s (`WATCH_PUSH_INTERVAL`) while
`state=playing`, and lets a `paused` or `stopped` report through at once — a pause or a stop is the
position a person actually returns to, and it is the last report of a session. The throttle map is
capped at 512 items and cleared wholesale when it overflows.

`removeFromContinueWatching` pushes a zero position. `merge_remote` ignores a zero position, so the
card does not come back on the next pull. Nuvio's RPC has no delete verb, so the row remains at 0 on
the account — worth knowing before anyone "fixes" the pull to treat zero as authoritative.

A failed push is logged as a status code only (`nuvio account: progress push failed status=N`) and
changes nothing locally. The local write already happened; losing the mirror is not losing playback.

## Pull

`pull_account_watch` runs on its own `pmsshim-watch` thread, from two places:

- at boot, right after the sink is installed (`shim_core::start_on`), so Home's shelf is populated
  before the first draw;
- whenever a profile is selected, because a profile's history is its own — switching profiles must
  not leave the previous one's resume points on screen.

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

## What is not covered

- A scrobble (watched) is not pushed. Nuvio's history has no separate watched flag in this RPC pair,
  so a title finished on the TV still shows as in progress on another device until that device
  plays past it.
- Nothing is pushed while signed out, and nothing is pushed for a title whose TMDB id cannot be
  resolved.
- The local `watch.json` and the `account_keys` map still live in the Developer Mode runtime
  directory (`/tmp`), so a TV reboot loses the local side. The next boot's pull restores what the
  account holds, which is now most of it.
- Deletions are a zero position, not a row delete (see Push).

## Verification

Host tests, `make test-crate C=plx_pmsshim` and `C=plx_stremio`:

- `shim_store::tests::merge_remote_takes_only_the_newer_side` — an older remote row is ignored, a
  newer one lands with its row snapshot and duration, a zero position is not a resume point, and a
  finished title keeps its `view_count` while taking a resume point.
- `shim_pms::account_progress_tests::a_playing_report_is_throttled_and_a_settled_one_is_not`.
- `shim_pms::account_progress_tests::a_tmdb_id_needs_no_lookup_and_an_episode_id_is_stripped_to_its_series`
  and `an_episode_id_names_its_series` — the local-parse fast path, `TMDB:` case, `tmdb:0`, and the
  episode→series split.
- `nuvio::progress_tests` — the push body's row shape and the 50-entry cap, and row parsing from
  both field spellings.

At the time of writing `plx_pmsshim` is 40/40 and `plx_stremio` is 49/49, the workspace clippy gate
passes with the change applied, and the shipping feature set type-checks under
`--no-default-features`.

Not verified on the TV. Nothing here has been observed against a real account: the pull's field
names come from the RPC's own row shape, and whether the backend's `last_watched` is milliseconds
(assumed) rather than seconds would show up as every remote row losing to the local state. A device
check should play past a few minutes on the TV, then open the same title in the Nuvio webOS app and
confirm the position carried, and the reverse.
