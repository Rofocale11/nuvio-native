# Project Handoff: NuvioNative + RichiL10 PlxNative Hybrid

## 📌 Project Context
This project is a custom build of a webOS media player. It merges two distinct forks:
1. **`current/nuvio-native`**: Adds Stremio addons, a local PMS Shim (fake Plex server), AV1/VP9 support, and BetterPosters.
2. **`Rofocale11/plx-native-local-transcode`** (Fork of `RichiL10/plx-native`): Features a custom in-app AC3 encoding lane (`ff_enc.rs`) for local DTS/TrueHD audio transcoding.

The ultimate goal was to merge these into one branch (`plx-nuvio`) and add features that neither original fork had, specifically bringing Stremio/Addon streams into the native Plex-style player UI.

---

## 🛠️ Phase 1: The Core Merge & Audio Transcoding Fix
Because the two repos had unrelated Git histories, we used `git replace --graft` to create a common ancestor, then merged, resolving 6 major file conflicts.

**The Audio Bug:**
After the merge, Stremio streams (via the Nuvio Shim) refused TrueHD and hung on DTS. This happened because:
1. Nuvio's Shim is hardcoded to always say `directplay` or `refuse` based on the TV's native capabilities. It didn't know about RichiL10's AC3 software encoder.
2. RichiL10's encoder only supports **48kHz**.

**The Audio Fix:**
*   **`rust-modules/src/app/nuvio.rs`**: Modified the `prober()` to mark audio as `decodable: true` and added `truehd, mlp, dts` to `set_audio_codecs`.
*   **`rust-modules/platform/src/devcaps.rs`** & **`rust-modules/media/src/player/engine.rs`**: Forced the client to use `AudioLane::Ac3Encode` for DTS/TrueHD, **BUT** added a critical safety check: `if rate == 0 || rate == ENC_LANE_RATE` to prevent the encoder from crashing on 96kHz streams.

---

## 💾 Phase 2: Memory Optimization (OOM Fix)
The app was Out-Of-Memory (OOM) crashing on LG TVs (which only have ~1.2GB RAM). A reboot helped, but the real issue was buffer sizes. We reduced the buffers for stability.

**Files & Changes:**
*   **`pkg/appinfo.json`**: Changed `"requiredMemory": 280` to `168`.
*   **`rust-modules/media/src/player/engine.rs`**:
    *   `AQ_VIDEO_BYTES`: Changed from `96 * 1024 * 1024` to `50 * 1024 * 1024`.
    *   `AQ_AUDIO_BYTES`: Changed from `8 * 1024 * 1024` to `4 * 1024 * 1024`.
    *   `AUDIO_QUEUE_BYTES`: Changed from `8` to `4`.
*   **`rust-modules/media/src/abr/sim.rs`**:
    *   `VIDEO_QUEUE_BYTES`: Changed from `96` to `50`.
    *   `AUDIO_QUEUE_BYTES`: Changed from `8` to `4`.

---

## 🎬 Phase 3: The In-Player Source Picker (Latest Feature)
**The Goal:** The Movie Detail page had a "Streams" dropdown showing Torrentio/AIOStreams links. TV Episodes auto-played the first stream. We wanted to bring the Addon Stream list into the in-player `...` (More) menu (replacing the useless Plex "Quality" menu) so the user can switch sources mid-playback.

**The Architecture:**
Because `PlaybackSession` is heavily coupled, we used a **Global Variable** to cache the streams fetched from the Shim, allowing the UI menu to read them instantly without waiting for a network request.

**Files & Changes:**
1.  **`locales/en/widgets.json`**: Changed `"Quality"` to `"Source"` and `"Change quality"` to `"Change source"`.
2.  **`rust-modules/media/src/route/decision.rs`**:
    *   Added a global: `pub static AVAILABLE_STREAMS: Mutex<Vec<(usize, String)>> = Mutex::new(Vec::new());`
    *   Added `set_stream_index(ps, index)` to tell the Nuvio Shim to switch streams.
    *   Made `PlaybackSession.request`, `PlaybackRequest`, `PlaybackRequest.sid`, and `PlaybackRequest.rk` **`pub`** so the player UI can read them.
3.  **`rust-modules/src/app/playback.rs`**:
    *   In `start_playback_with` (the universal function for all playbacks), we clear the global `AVAILABLE_STREAMS` and spawn a background task: `client.nuvio_streams(&req.rk)`. This fetches the correct streams for the specific Movie/Episode being played and saves them globally.
    *   In `apply_more_action`, added `Action::SetStream(index)` which calls `nuvio_select` on the Shim and triggers `retry_failed_playback` to reload the stream.
4.  **`rust-modules/appkit/src/more_menu.rs`**:
    *   Added `Action::SetStream(usize)`.
    *   Modified `rows_for`, `label`, `FormId`, and `row_for` to read from `plx_media::route::AVAILABLE_STREAMS` instead of the hardcoded `Quality` ladder.
    *   Updated `root_form` and `quality_form` to correctly place the streams inside the sub-page with checkmarks, not toggles.

---

## ⚠️ Important Warnings for the Future
*   **The `nuvio.rs` Path Trap**: There is `rust-modules/src/app/nuvio.rs` (the Shim boot) and `rust-modules/stremio/src/nuvio.rs` (the Stremio client). **Always double-check the file path before pasting code.**
*   **No `cargo fmt` on the whole workspace**: If you have a dirty tree, running `cargo fmt` will reformat 700+ files and cause a merge nightmare. Format files individually.
*   **webOS OS Memory Leaks**: If the app crashes repeatedly during development, the TV's OS might leak memory. A full TV reboot (unplugging it) is necessary to clear the system RAM.
*   **The 96kHz Limit**: `ff_enc.rs` was compiled without `swresample`. It only handles 48kHz. The safety check in `engine.rs` prevents crashes on 96kHz audio.

---

## 🔮 Future Roadmap (Ideas)
1.  **Addon Catalogs as Libraries**: Modify `shim_pms.rs` (`/library/sections`) to read the `catalogs` array from each Stremio addon's `manifest.json` and present each as a separate Plex Library (e.g., "IMDb Top 250", "Cinemeta Trending").
2.  **Embedded Torrent Server**: Integrate a Rust torrent server (like `librqbit` or `stremio-native/stream-server`) directly into the app so `enableTorrents: true` works without needing external software like TorrentServer.
3.  **Track Selection via In-Player Menu**: Extend the Source Picker to also show Audio/Subtitle tracks from the specific Addon stream.
