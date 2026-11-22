# Nuvio Native — account watch progress

Continue Watching is the account's, not the device's. The shim already kept a local resume point in
`nuvio.redb`; it now mirrors every timeline report into the Nuvio account's per-profile watch
history and merges what another device wrote back. Nothing about the player, the UI or the PMS
dialect changed: the app still reports `/:timeline` and still reads `/hubs/continueWatching`, with
the account as a second store behind those two endpoints.

## Shape

```
  player ──/:timeline──────────▶ Shim::push_watch_progress ──▶ Account::push_watch_progress
             (existing thread)      identity + throttle         session + profile
                                          │                           │ POST
                                          ▼                           ▼
                                   WatchState (redb)      /rest/v1/rpc/sync_push_watch_progress

  boot / profile switch ──▶ Shim::pull_account_watch ──▶ Account::pull_watch_progress
                              │  newer wins, row first     │ POST
                              ▼                            ▼
                           WatchState::merge_remote   /rest/v1/rpc/sync_pull_watch_progress
```

| piece | file |
|---|---|
| RPC bodies, row parsing, `WatchProgress` | `rust-modules/stremio/src/nuvio.rs` |
| session, profile resolution, watch sink | `rust-modules/pmsshim/src/shim_account.rs` |
| identity mapping, throttle, pull/merge, generation | `rust-modules/pmsshim/src/shim_pms.rs` |
| merge policy, account-key map | `rust-modules/pmsshim/src/shim_store.rs` |
| boot pull and sink installation | `rust-modules/pmsshim/src/shim_core.rs` |
| Home's re-read and deck adoption | `rust-modules/src/app/{nuvio,boot,run}.rs`, `rust-modules/data/src/stores/deck.rs` |

## The two RPCs

Both are Supabase functions on the same backend as My List, and both take `p_profile_id`, parsed by
the same `library_profile` guard, so a malformed profile is refused before a request is made.

- `sync_push_watch_progress` takes `p_entries`, at most 50 rows: `content_id`, `content_type`
  (`movie` | `series`), `video_id`, `season`, `episode`, `position`, `duration`, `last_watched`
  (unix **milliseconds**) and `progress_key`. Every id is checked as the batch is built: an unknown
  content type, an empty id, an id over 512 bytes or a control character refuses the whole batch.
- `sync_pull_watch_progress` takes `p_limit` (200) and returns the same rows. Field names are read
  in both `snake_case` and `camelCase` because the function's return type is not pinned to one.
  A reply that is not an array is a parse error, not an empty history — silently calling a backend
  change "nothing watched" would hide it.

No response body is ever logged: these rows are a household's viewing record. A failed push logs
only its status code (`nuvio account: progress push failed status=N`) and changes nothing locally —
the local write already happened.

## Identity

**Nuvio names a title by IMDb id** (`tt0133093` for a film, `tt0903747` for a series); `tmdb:` is
its fallback. Its apps match Continue Watching on that form, so a `tmdb:` row they cannot match is
as good as missing. A local `tmdb:` catalog id is resolved through `TmdbClient` before pushing;
`tt…` ids are left untouched, and a title with neither (kitsu, …) does not sync.

`video_id` is the film's own id or the episode's addon id (`tt0903747:1:2`). Nuvio also keys an
episode by `content_id_s{season}e{episode}`, so season and episode are sent as their own fields and
in `progress_key` — without them every episode of the series overwrites the same row.

A row from another Nuvio client may name its episode synthetically
(`__nuvio_episode__:S:E`) when the playback carried no addon episode id. The pull places it through
the series meta's season/episode, so it lands on the addon's real episode instead of a key no addon
can answer; a season/episode the series does not list is skipped rather than stored rowless.

`account_keys` is part of the same redb watch record: `"content_id|video_id" → ratingKey`, written
when this device pushes and when a pulled row actually merges. It is durable because the two sides
can name one episode differently, and rebuilding identity alone can intern a second ratingKey for
one episode. Rebuilding remains the fallback for a row no device here pushed; a pull that loses to
local state does not grow the map. A legacy `tmdb:` row is canonicalised to IMDb on the way back, so
an id written once as `tmdb:N` and once as `tt…` is one local card.

A season row and a whole-series row report nothing: neither has a single position of its own.

## Push

The timeline thread already wrote local progress off the reporting request. It now also calls
`push_watch_progress` after that write, so the account never sees a report the local state did not.
`watch_push_due` allows one push per item per 60 s while `state=playing`, and lets `paused` or
`stopped` through at once — that is the position a person returns to and the last report of a
session. The throttle map is capped at 512 items and cleared wholesale on overflow.

`removeFromContinueWatching` pushes a zero position. `merge_remote` ignores zero, so the card does
not come back on the next pull. Nuvio has a `sync_delete_watch_progress` RPC; this shim still
writes zero instead, so the row remains at 0 on the account — worth knowing before anyone "fixes"
the pull to treat zero as authoritative. A scrobble is not pushed: the RPC pair has no watched flag.

## Pull

`pull_account_watch` runs on its own `pmsshim-watch` thread from two places: at boot, right after the
sink is installed (`shim_core::start_on`), and whenever a profile is selected (a profile's history
is its own).

**The profile's addons are installed before that pull fires** (`Account::load_profile`). The pull
builds each resume point's row through the addon backend (`Shim::row`), and `WatchState::in_progress`
drops an entry whose row snapshot is missing. `load_profile` also drops the `library` guard before
firing the sink: the pull resolves its profile through that same lock, and holding it across the
call deadlocked on the first switch. A superseded load still installs neither addons nor history.

Each row is filtered cheapest-first: a zero position is dropped, the local stamp is compared before
any row snapshot is fetched, and only a merge that lands pays for `me.row(rk)` and a bind.
`WatchState::merge_remote` lets the newer side win, comparing `last_viewed_at` in seconds (the
account's milliseconds divided by 1000). `view_count` is never touched — Nuvio knows nothing about
our scrobbles, so a finished title can take a resume point without being un-finished.

**The pull also tells the client when it changed something.** A non-zero merge bumps
`Shim::content_generation`; the app polls that count once per frame and re-reads Home's hubs (and
invalidates library shelves) for each newly observed value. Without the count the merge landed
after Home's first `/hubs` and nothing asked again. A merge before any server is registered (a
first sign-in can do this) stays owed until the roster exists. Its baseline is captured at the top
of `construct`, before `activate_session` issues the first `/hubs`, not when the `App` literal is
built: otherwise a merge landing between the fetch and the literal reads as "already seen".

The re-read alone was not enough — the deck store must also adopt the cards. Home's first answer of
a fresh account is *empty* Continue Watching. `stores::deck::place` seated a source's lane on that
preview and marked it placed; the follow-up answer with rows was skipped as "already placed", so
Home stayed empty until the next launch, whose first answer already held the persisted rows.
`place` now treats a placed-but-empty lane as unplaced when a later preview brings cards.

## What is not covered

- The local watch state and account-key map live in `nuvio.redb` under the shim's data directory;
  a TV reboot keeps them only when `nuvio_data_dir()` resolves to a persistent path (the last
  fallback is `/tmp`). The boot pull still refreshes positions from the account.
- A row whose identity no local addon can resolve to a meta has no row snapshot and stays off
  Continue Watching.
- Deletions are a zero position, not a row delete (see Push).
- Nothing is pushed while signed out. Host tests validate the RPC shapes; the push/pull contract
  follows Nuvio's own clients (WebOS `js/core/profile/watchProgressSyncService.js`, Android
  `core/sync/WatchProgressSyncService.kt`) and is not yet verified against a live account on the TV.

## Verification

Host tests (`make test-crate C=plx_pmsshim`, `C=plx_stremio`, `C=plx_data`, plus the app crate):

- `nuvio::progress_tests` — push body shape and profile scoping, `content_id_sNe` episode keys,
  the 50-row cap, validation before any request, row parsing from both field spellings.
- `shim_store::tests::merge_remote_takes_only_the_newer_side_and_never_binds_a_key`.
- `shim_pms::account_progress_tests` — IMDb naming for a film and an episode, the throttle,
  synthetic-episode placement through season/episode, and bumping only on a real merge.
- `shim_account::tests::a_profile_load_installs_addons_before_it_pulls_watch`.
- `stores::deck::tests::a_lane_placed_on_an_empty_preview_adopts_the_cards_it_gets_later`.
- `app::run::account_content_tests` — a merge is owed once and only after a server exists.

Not verified on the TV against a real account. A device check should play past a few minutes on the
TV, then open the same title in the Nuvio webOS app and confirm the position carried, and the
reverse.
