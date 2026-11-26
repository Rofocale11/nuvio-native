# Nuvio Native architecture

Nuvio Native is a fork of PlxNative (GPL-3.0-or-later, upstream GLinnik21/plx-native@4e454ea) that
browses and plays content from Stremio addons — the same addons a user configured in Nuvio — with
PlxNative's GPU-rendered UI and native Starfish/ACB video pipeline.

Current state: branch `nuvio-native` at `1db875cc8` (2026-11-25); the deployed 0.10.0 stable
build is from `84a00aa68`. Dated delivery history lives in `HANDOFF.md` and `VALIDATION.md`;
the upstream conflict surface is in `MERGE-AUDIT-HISTORY.md`.

## Findings that drive the design

- **Nuvio has no reusable backend service.** NuvioTVSmart (the code behind the webOS app
  `space.nuvio.webos`) is a browser app: addon management, catalogs, metadata, stream discovery and
  playback all run as JS inside the WebView. Its webOS JS services (`services/webos`) only proxy
  media/images/subtitles for the WebView. Addon config lives in WebView localStorage (unreadable
  from another app in Developer Mode) and is synced to the user's **Nuvio account** (Supabase).
  So "reusing Nuvio's backend" means: speak the same Stremio addon protocol, and import the same
  account-synced addon list.
- **PlxNative's Plex coupling is deep but sits behind one HTTP boundary.** ~54k lines of
  `plex/` + `session/` + `data/` speak the PMS REST dialect; the UI, player, media pipeline and
  stores consume the typed results. `tests/mock_pms.py` documents exactly the endpoints and fields
  the app needs, and the app already has a dev boot that connects straight to a PMS host/port
  with a token.

## Shape

```
 ┌──────────────── one native process (the .ipk) ────────────────┐
 │  PlxNative UI / stores / player   (unchanged where possible)   │
 │          │  PMS REST over http://127.0.0.1:<port>               │
 │          ▼                                                      │
 │  plx_pmsshim  — in-process HTTP server, PMS dialect             │
 │          │  Rust API                                             │
 │          ▼                                                      │
 │  plx_stremio  — addon protocol, Nuvio import, stream classify   │
 └──────────┼──────────────────────────────────────────────────────┘
            ▼ HTTPS (libcurl)
     Stremio addons (Cinemeta, Torrentio, AIOStreams, …), Nuvio account backend
```

- Catalogs → PMS library sections and Home hubs. Metas → movie/show/season/episode items.
  Search → `/hubs/search`. Streams → the item's `Media[]` list (one Media per stream, with
  resolution/codec/HDR/size/bitrate from `plx_stremio::classify`), so PlxNative's existing
  version/source picker is the stream picker. Part URLs answer **302** to the stream's real URL;
  the shim has no byte proxy, even for addons that declare `behaviorHints.proxyHeaders`
  (`PMSSHIM.md` §9). The decision endpoint waits for discovery/probing
  and refuses media that cannot direct play; the native route uses its probed fields.
  The Streams overlay uses `/nuvio/streams` and `/nuvio/select`, retaining the shim's
  ranked index instead of confusing it with the selected-first Media list.
- **One durable store, no DB8.** Every Nuvio Native record — the canonical session and all its
  Settings preferences, addon configuration, install-wide preferences (language, resolution,
  client id), the Nuvio account's audio/subtitle preferences, watch state, the ratingKey table,
  the Nuvio backend session and the shim's local settings — lives in a single `nuvio.redb` in
  `<data>/shim/`. Upstream's `storage_service::backend::Backend` is run **in process** by
  `plx_os::storage::redb_backend`: DB8's objects and Keymanager3's AES-256-GCM envelopes are two
  more tables in that file, so the session/preference/consent layers keep their exact wire
  semantics and tests. The separate `plxstorage` helper is no longer built, packaged or activated
  (the ipk declares no service). Host/simulator builds still use upstream's `Files` authority.
  Legacy on-disk files (`settings.json`, `watch.json`, `keys.tsv`, the account-session JSON and
  `language.json`) are read once as migration sources and written into redb. DB8 records cannot
  be migrated — no helper is available to read them — so anything that lived only there starts
  fresh.
- Torrents (`infoHash`): played through the **in-process C engine** (`libsbengine.so`, `dlopen`ed
  beside the binary by `plx_stremio::sbengine`, never linked). `boot::start_p2p_service` starts it
  on a parked thread with durable swarm state and a RAM-backed payload window; the shim resolves a
  stream with `sbengine::resolve_stream` and serves it on the engine's own loopback Range URL.
  When the library cannot load, torrents are unavailable — there is deliberately no Node fallback.
  Debrid-resolved HTTP links from Torrentio/AIOStreams play directly and need no engine. The
  Node scripts shipped in `js-service/` are only the trailer decipher/proxy children: system Node
  spawns them per resolve from `<app_dir>/js-service/`, independent of the P2P engine.
  `ci/build-sbengine.sh` builds the engine (Nuvio Engine + libtorrent 2.0.12 + statically linked
  OpenSSL 1.1.1) and the Makefile stages it in both `deploy` and the `.ipk`, where its bytes are
  pinned by `pkg/link-evidence/<flavour>/unreceipted.sha256`.
- Release discovery permits plaintext credentials for the registered in-process shim
  only when its origin, token and machine identity match the boot registration and
  its address is literal loopback. Identity probing and authenticated library admission
  still run. Remote HTTP resources do not gain this exception. This is enforced by the
  Plex client's discovery code (`plex/src/plex/probe.rs:430-458`), **not** by the shim: the
  shim carries the token but validates nothing (`shim_core.rs:26-27`; `PMSSHIM.md` §2–3).
- Rating-source preference lives in the native canonical session record; the shim
  retains supplied IMDb and Rotten Tomatoes scores so cached details can switch
  providers. It does not replace a missing RT detail score with IMDb. BetterPosters
  image URLs request RT directly; these baked-in poster scores are independent of
  the native detail-score preference. Original artwork is retained as a fetch fallback.
- My List uses Nuvio's `sync_pull_library`, `sync_push_library_items` and
  `sync_delete_library_items` RPCs. Writes contain one title and the active profile,
  never a whole-list replacement. The account context serializes profile changes and
  mutations, caches reads for 60 seconds and confirms each write with a fresh pull.
  The selected profile is retained with the backend session. The UI uses
  `/nuvio/library`, runs requests on a worker and refreshes Home after a confirmed
  reply; the shelf pages through `/hubs/nuvio/list`.
- Watch state / progress: stored by the shim in the same redb file (the app already reports
  timeline to "the server"). Durability across a TV reboot is now whatever `nuvio_data_dir()`
  resolves to: `/media/developer/<appid>.nuvio` when writable, else the persistent state root,
  else the Developer Mode runtime fallback (`/tmp`). Confirm the chosen directory on the set
  before claiming reboot durability.
- Account addon changes: a relaunch makes **one cheap REST read** of the signed-in profile's addon
  rows (`nuvio::addons_fingerprint`) and compares it with the fingerprint stored when the set was
  last applied. Unchanged → keep the persisted set and the durable catalog cache (fast launch).
  Changed → the existing profile-load path fetches manifests, replaces the set, clears the caches
  and re-warms Home. A transport fault is not a change: the persisted set is kept. A Stremio addon
  publishing new *items* still cannot be detected without asking it, so the catalog TTL bounds that.
- **Multi-profile (1–6, the public API's range).** One account holds up to six profile slots;
  everything account-backed is keyed by `profile_id`/`p_profile_id`. `nuvio::profiles()` reads
  `sync_pull_profiles`; `home_users()` projects those rows into Plex-dialect tiles whose uuid is
  `nuvio-profile-<index>`, and `switch` validates the index against a fresh roster before
  `load_profile`. `uses_primary_addons` still resolves a child profile to profile 1's addons.
  Settings > Profiles (a fork-owned page behind the boot-registered `ProfileControl`) creates,
  renames and deletes slots through `sync_push_profiles`/`sync_delete_profile_data` — both
  destructive full-state calls, so every push carries the COMPLETE roster and `p_client_max_profiles:
  6`, and slot 1 is never deletable. Avatars resolve `avatar_url` or `avatar_id` →
  `get_avatar_catalog` → `https://api.nuvio.tv/storage/v1/object/public/avatars/<storage_path>`;
  `avatar_color_hex` is a tint, never an image. Settings preferences sync through
  `sync_pull/push_profile_settings_blob` under the fork-owned `p_platform = "webos-native"`, so
  a full blob upsert cannot overwrite the Nuvio web app's `tv` namespace. Content caches and the
  ratingKey table stay shared; local watch state and prefs are keyed by profile slot in the same
  redb, with the old flat rows migrated as profile 1.
  The public API exposes `pin_enabled` and `pin_locked_until` as READ-ONLY state and has no PIN
  verification call. The account flag is therefore advisory: a tile is locked on this TV only
  when a local PBKDF2 verifier exists. Setting the first TV PIN is unguarded (physical access);
  changing or clearing one requires the current PIN. A forgotten PIN is recovered from the
  management page's **Forgot PIN? Scan QR** row: it arms a one-shot reset on the shim and opens the
  existing QR sign-in; only when that QR is approved as the SAME account does the shim remove the
  verifier, logging `nuvio: TV PIN reset by account QR for profile N`. Arming alone never unlocks.
  The QR sign-in is followed from any route while the session is `Creating`, so raising it from
  Settings actually shows the QR page. Signing out also clears every verifier as a fallback, but
  still needs the phone to sign back in. Auto Sign In never skips a locked profile: the boot gate
  reads the durable verifier directly, before any HTTP snapshot has landed.
  The management page is a pure cache reader: the application refreshes the snapshot once at
  launch from `install_pms_owned`, where the PMS client is current (the earlier `nuvio::boot()`
  refresh ran before any client existed and published an empty roster), and again after each write.
  A landing refresh keeps the row under the cursor instead of re-seating focus on profile 1. The
  rows that raise the TV keyboard (Rename, Set/Clear TV PIN) are the FIRST section so the keyboard
  cannot cover the field; Add and Delete show a small inline spinner on the affected row until the
  worker's refreshed snapshot lands. A roster write keeps the row that triggered it (Avatar, Share
  main addons, Rename), and confirming a delete seats the neighbouring profile rather than slot 1;
  slot 1 is never deletable and its Delete row is inert. The shim's `/api/v2/user` answers the
  seated profile's `id`/`uuid`, so upstream's account-preference identity check resolves and
  Settings > Audio & subtitles reads the profile's own values.
- Settings: Addons lists the installed addons and flips one through the boot-registered
  `nuvio::AddonControl` (the screens layer may not name `pmsshim`). Playback's P2P switch is
  persisted in the one redb `LocalSettings` value (absent key means ON) and applied live through
  `shim.set_p2p`; screens read the published `ScreenInputs.p2p_enabled`. Default quality, Direct
  Play and Playback engine rows are hidden, not deleted — the shim has no transcoder and forces
  Original, and all three already default to Original / Auto / Standard.

## Crates

The fork-owned crates are separate workspace members so an upstream rewrite of a shared file
cannot drag fork code through a merge. `plx_os`, `plx_plex` and the application crate have small
marked seams into `plx_nuvio_store`; `plx_stremio`, `plx_pmsshim`, `plx_nuvio_probe`,
`plx_nuvio_store` and `plx_nuvio_dash` are entirely fork-owned.

| crate | owner | depends on |
|---|---|---|
| `rust-modules/stremio` (`plx_stremio`) | fork-owned backend | `plx_base`, `plx_net`, serde |
| `rust-modules/pmsshim` (`plx_pmsshim`) | fork-owned shim | `plx_base`, `plx_net`, `plx_os`, `plx_stremio`, `plx_nuvio_store`, serde |
| `rust-modules/nuvio-probe` (`plx_nuvio_probe`) | fork-owned probe | `plx_base`, `plx_net`, `plx_plex`, `plx_media_codecs` |
| `rust-modules/nuvio-store` (`plx_nuvio_store`) | fork-owned single store | `redb`, `plx_base` |
| `rust-modules/nuvio-dash` (`plx_nuvio_dash`) | fork-owned trailer MPD parser | pure Rust, no dependencies |
| `rust-modules/os` (`plx_os`) | upstream, fork-marked | `plx_nuvio_store`, `ring` — runs the canonical storage backend in process |
| `rust-modules/plex` (`plx_plex`) | upstream, fork-marked | `plx_nuvio_store` — Nuvio account preferences |
| `rust-modules/src/app` (`plxnative-modules`) | upstream, fork-marked | all of the above — boot, appinfo, settings |

### Cache and id tables in the one store

All of these are tables in the SAME `nuvio.redb`; there is no second cache file and no new JSONL
sidecar:

- `META_CACHE` and `ENRICH_CACHE` — one TTL-enveloped JSON row per `(type, id)` written by the
  shim's `MetaCache` / `EnrichCache` (`pmsshim/src/shim_store.rs`), each carrying its own `at`
  unix-seconds stamp and an in-memory warm copy. On open the shim reads both the enveloped rows and
  the pre-envelope raw JSON of earlier builds (`decode_cached`, `:415-423`), deletes the expired
  ones from redb and rewrites the live ones under an envelope (`:447-476`, `:616-645`). The row's own
  `at` stays authoritative for in-memory freshness (`:483-485`, `:640-642`). The old
  `meta-cache.jsonl` / `enrich-cache.jsonl` files are read **once** as migration sources only when
  the redb table is empty; nothing writes them any more.
- `IMDB_IDS` — the old generic TTL table, retained in `nuvio-store` and still used by its own
  tests. Meta/enrich rows are written directly to their redb tables, but the shim **does** use
  `NuvioCache::{Catalogs,Subtitles}` / `Store::cache_get` / `cache_put` for the catalog and subtitle
  caches (`shim_catalog_cache.rs:71-83`, `shim_subtitle_cache.rs:86-98`).
- `IMDB_TMDB` — the durable IMDb→TMDB id map: a plain big-endian `u64` with **no** TTL, written by
  `shim_tmdb_ids::PersistentIdLookup` so a `/find` answer is paid for once per title. Negatives and
  non-IMDb ids are not stored.
- `CATALOG_CACHE` — the first pages of Home/Library catalogs, written by
  `pmsshim/src/shim_catalog_cache.rs` with the same 15-minute TTL the in-memory catalog rows had.
  A relaunch with unchanged addons answers Home from redb instead of re-fetching every catalog.
  The key is namespaced by the addon set **and** the installed manifests' catalog declarations, so
  adding/removing an addon, switching profile, or a manifest that changes its catalogs misses the
  old rows. Catalog *content* has no change signal in the Stremio protocol, so the 15-minute TTL
  is the freshness bound; a failed fill is never cached. Its durable helpers no-op by default under
  `#[cfg(test)]`, but a thread-local test store lets one test close and reopen the real redb
  (`shim_catalog_cache.rs:178-214`); the older shim test named "relaunch" still covers in-memory
  only. Device relaunch warmth is the owner's separate evidence.
- `SUBTITLE_CACHE` — addon subtitle candidates, 6 h for a real answer and 10 min for a real empty
  answer, written by `pmsshim/src/shim_subtitle_cache.rs`. A provider **fault is never persisted**:
  it gets only a 30 s in-memory backoff, so a dead addon is neither hammered on every poll nor
  remembered as "no subtitles" after it recovers. The same distinction applies to the in-memory
  stream slots: an all-addon failure is `failed`/`faulted` with a 30 s retry window, not an empty
  list for the slot's 30 min TTL. `/nuvio/streams?retry=1` clears the slot and fans out again; the
  player's Streams picker offers that as a Retry row. Its durable helpers are also no-ops under
  `#[cfg(test)]` (`shim_subtitle_cache.rs:75-101`).

The opposite direction (`tmdb:<n>` → `tt…`, used by `imdb_base()` for account identity/watch mirroring
and addon id translation, **not** for hub posters) is deliberately **not** a redb row in this tree:
`Shim::imdb_ids` is a bounded in-memory map (512 entries, negative answers cached too). A hub row's
poster is `poster_url(id, original)` with the item's own id; only a literal `tt…` id is rewritten to
BetterPosters (`shim_backend.rs:158-162`, `shim_pms.rs:6386-6399`; `PMSSHIM.md` §7). See
`ID-MANAGEMENT.md` and `TMDB.md`. The 50 MiB soft budget sheds expired and oldest cache rows only;
`IMDB_TMDB` and the durable tables are never discarded to meet it. Meta/enrich rows carry the same
TTL envelope as catalog/subtitle rows, so all four cache tables participate correctly in the
boot-time purge and shed (2026-11-26; `PMSSHIM.md` §11).

### Focus-warm (sans-IO)

Home issues one background shim metadata GET when focus rests on a card for 750 ms, so the shim's
cache is warm before OK. The screen is a machine: it emits the typed `MetadataCmd::Warm { sid, rk }`
command; the application runner executes a single `client_for(sid)?.metadata(rk)` GET on a worker
and drops the body. No Detail is installed, `current()` and the loading/generation counters are
untouched, and no store notice is raised (a notice would relayout a mounted Detail page). The
runner dedups by `(sid, rk)` for 60 s and releases the key if the worker is refused. A scroll step
(300-500 ms per card) never reaches the dwell.

## App identity

- appid `com.nuvionative.app` (debug: `com.nuvionative.app.debug`), title "Nuvio Native". Installs
  beside `space.nuvio.webos`; never touches it.
- Packaging version is **0.10.0** — `pkg/appinfo.json`, `ipkroot/ctl/control` and the Cargo
  manifests agree, and `requiredPermissions` is empty because the app declares no DB8/keymanager
  service. Earlier 0.8.0 package names in the dated docs are historical.
