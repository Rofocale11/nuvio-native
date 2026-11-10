# v0.9.0 — your Stremio addons on the television

Nuvio Native is a native LG webOS client for Stremio and Nuvio addons: the catalogs you already have become the library, the streams your addons answer become one picker, and the file plays on the television's own decoder. It is a fork of PlxNative, which was a Plex client; this release is the first one that talks to addons and a Nuvio account instead of a Plex Media Server.

## If you used PlxNative before: this is a different app

Nuvio Native is installed as `com.nuvionative.app`, beside PlxNative rather than over it, and it carries nothing across. There is no Plex sign-in and no server to point it at: you sign in with a **Nuvio account** by QR code, and the addons installed on that account appear in the app. Nothing is read from, or written to, a Plex account or a Plex Media Server.

## What's new

- **Your addons are the library.** The catalogs a Nuvio account's addons publish (Cinemeta, Torrentio, AIOStreams and the rest) become Home shelves and a poster grid; the Library's Catalog control names the catalog in force, and genre, sort and filter work over it.
- **One stream picker, grouped by addon.** Every stream the addons answer is listed under the addon that answered, with seeders, resolution, codec, HDR and size. The same picker is on a title's page, on the Home hero and in the player.
- **A Nuvio account on the television.** QR sign-in, the account's profiles, **My List** and **watch progress** kept on the account — a film started on the TV resumes at the same place on another device using that profile.
- **Torrents play in the app.** An `infoHash` stream is served by the bundled streaming engine; debrid-resolved HTTP links play directly. The file is played as it is, because the app has no transcoder.
- **Detail pages with what an addon does not send.** Cast and person pages, More Like This, collection shelves and title logos come from TMDB, and the trailers a title's metadata lists are played from the video's own DASH tracks — there is no embedded web player.
- **The set's own decoder, used properly.** Dolby Vision Profile 7 is converted to 8.1 in the app; TrueHD, DTS and FLAC above two channels are routed through an in-app AC-3 lane; Opus plays directly; speed runs from 0.5× to 2× by decoding and time-stretching the sound.
- **Subtitles in every text format**, drawn at video resolution, with an external subtitle search that asks the account's own subtitle addons.
- **Eight interface languages:** English, Español, Беларуская, Français, Italiano, Português, Deutsch and Magyar.

## Compatibility

- **Plays video, verified by watching it — one 2024 LG set.** One television, an LG B4 reporting platform release 10.3.1: sign-in, browsing, search, My List, watch progress, direct play, torrents, subtitles and trailers (1080p and 4K) are the features exercised on it while this release was written.
- **Everything else is untested.** No other model or firmware has been run with this app. It is the same 32-bit ARM webOS package PlxNative ships, but a build that starts is not a claim that video plays.

If your set is not a 2024 LG, [tell us what happened](https://github.com/Rofocale11/nuvio-native/issues). It working is as useful a report as it failing.

## Known issues

- **A file the television cannot decode does not play.** There is no transcoder: an addon's stream is either playable on the set as it is, or it is not. Settings → Playback no longer offers a quality ceiling or a direct-play policy for that reason.
- **Torrents need the bundled engine.** It loads on every webOS release that has been tested; on an install where it cannot load, torrent streams are unavailable and HTTP streams are unaffected.

## Installing

Download **com.nuvionative.app_0.9.0_arm.ipk** from [the release page](https://github.com/Rofocale11/nuvio-native/releases) and install it with [webOS Dev Manager](https://github.com/webosbrew/dev-manager-desktop) over LG's Developer Mode, or with the [Homebrew Channel](https://github.com/webosbrew/webos-homebrew-channel). You do **not** need a rooted TV.

Nothing in this distribution chain is signed, so this sha256 is what tells you the file you have is the file published here:

```
__IPK_SHA256__  com.nuvionative.app_0.9.0_arm.ipk
```

Developer Mode expires if it is not renewed, and LG then removes the apps installed through it. The package is built by [`build-ipk.yml`](https://github.com/Rofocale11/nuvio-native/blob/forward-port/v0.9-hq/.github/workflows/build-ipk.yml) from the tagged commit, and what the fork adds on top of PlxNative is catalogued in [`docs/nuvio/FEATURE.md`](https://github.com/Rofocale11/nuvio-native/blob/forward-port/v0.9-hq/docs/nuvio/FEATURE.md).
