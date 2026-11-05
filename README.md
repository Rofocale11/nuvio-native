# Nuvio Native

A native LG webOS client for Stremio/Nuvio addons, maintained by [Chris](https://github.com/current).
This is a fork of [RichiL10/plx-native](https://github.com/RichiL10/plx-native), itself based on
[PlxNative](https://github.com/GLinnik21/plx-native); the upstream authors' work and the GPL
license are retained.

See [architecture](docs/nuvio/ARCHITECTURE.md), [ID management](docs/nuvio/ID-MANAGEMENT.md),
[TMDB enrichment](docs/nuvio/TMDB.md), [playback](docs/nuvio/PLAYBACK.md) and
[watch progress](docs/nuvio/WATCH-PROGRESS.md) for this fork's design, build and TV instructions.
The native debug and stable app IDs are `com.nuvionative.app.debug` and `com.nuvionative.app`.
[docs/README.md](docs/README.md) indexes every document and marks which are upstream references.

**Release status (this fork):** no release is published yet. `build-ipk.yml` builds the `.ipk` on
demand or on a `v*` tag, and `tools/cut-release.sh` is the release procedure. Use the fork app id
`com.nuvionative.app` (`com.nuvionative.app.debug` for debug builds).

**What it is not:** this is not a Plex client. There is no plex.tv credential and no Plex Media
Server; the UI speaks the Plex HTTP dialect to an in-process shim that reads Stremio addons and
the Nuvio account. See [architecture](docs/nuvio/ARCHITECTURE.md) and the
[feature catalogue](docs/nuvio/FEATURE.md).

## Upstream PlxNative

The upstream project's README — install steps, site links, privacy statement and star history —
describes the Plex client this fork came from, not this app. It is kept for attribution in
[docs/upstream-readme.md](docs/upstream-readme.md).

## Licence

[GPL-3.0-or-later](LICENSE), © 2026 Gleb Linnik. The PlxNative name and its brand artwork are
excluded — see [`TRADEMARKS.md`](TRADEMARKS.md), which also carries the Plex and LG
non-affiliation statements. Third-party components and their licences are in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md) and `licenses/`; those notices ship inside the
`.ipk` too.

This is an unofficial client, not affiliated with, endorsed by, or sponsored by Plex GmbH or LG
Electronics. "Plex", "Rotten Tomatoes", "IMDb", "TMDB", "LG" and "webOS" are trademarks of their
respective owners.
