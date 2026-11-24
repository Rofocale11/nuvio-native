//! **NuvioNative boot**: start the in-process PMS shim (`plx_pmsshim`) over the user's Stremio
//! addons and install it as the app's one server (`plx_base::local_server`), before anything asks
//! for a server or for plex.tv. See `docs/nuvio/ARCHITECTURE.md`.
//!
//! - Every Nuvio Native record (TV addons included) lives in ONE `nuvio.redb`; host addons fall back
//!   to `<data>/addons.json` (`plx_stremio::AddonSet`'s format); Cinemeta + OpenSubtitles
//!   when there is none.
//! - `<runtime dir>/nuvio-import/nuvio-import.json` (written by `tools/nv-import-nuvio.mjs` from the user's Nuvio
//!   install) replaces that set once, is persisted, then deleted.
//! - The shim's watch state, key table, account session and local settings live in the same
//!   `<data>/shim/nuvio.redb`. `<data>` survives an app reinstall only when a persistent writable
//!   directory is available; Developer Mode's last fallback is `/tmp`, so durability across a
//!   reboot depends on which `nuvio_data_dir()` candidate the set grants.
//! - Streams are probed with the bundled FFmpeg at play time ([`prober`]).
//! - Torrents play through a Stremio-compatible streaming server: `<data>/settings.json`'s
//!   `torrentServer`, else an explicitly enabled (`enableTorrents`) local scan.
//!
//! Never logs an addon URL: Nuvio/AIOStreams URLs embed credentials. Hosts at most.

use plx_base::eventlog::log;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::sync::atomic::{AtomicBool, Ordering};

/// The loopback port the shim prefers. Fixed so the server's origin is the same every boot (the
/// session and the per-server caches key on it); any free port if it is taken.
const PORT: u16 = 32_420;

static HANDLE: OnceLock<plx_pmsshim::Handle> = OnceLock::new();
static BACKEND: OnceLock<Arc<plx_pmsshim::AddonBackend>> = OnceLock::new();

/// Fork Rofocale11: the persisted P2P switch, mirrored into a plain atomic for the frame thread.
/// Screens ask for it before the shim exists (and on controlled boots with no shim at all), so the
/// fallback must never read redb or load the engine from the frame loop. Boot seeds it from the
/// Settings record; the storage worker updates it alongside the shim when the user flips the row.
static P2P_FALLBACK: AtomicBool = AtomicBool::new(true);

/// The running shim, for Settings screens that change addons.
#[allow(dead_code)]
pub(crate) fn handle() -> Option<&'static plx_pmsshim::Handle> {
    HANDLE.get()
}

/// Fork Rofocale11: the shim bumps this only when an account pull actually merged a resume point.
/// Home polls it and re-reads its hubs once per observed value; the baseline is captured before the
/// first `/hubs`, not when the `App` literal is built, so a merge racing that fetch is not lost.
pub(crate) fn content_generation() -> Option<u64> {
    handle().map(|h| h.shim.content_generation())
}

/// Fork Rofocale11: the live P2P decision for the frame thread. The shim owns the switch once it
/// exists (a Settings change takes effect without a restart); before then the boot-seeded atomic
/// answers, so no screen ever reads redb or loads the engine from the frame loop.
pub(crate) fn p2p_enabled() -> bool {
    handle().map(|h| h.shim.p2p_enabled())
        .unwrap_or_else(|| P2P_FALLBACK.load(Ordering::Relaxed))
}

/// Fork Rofocale11: persist the Settings P2P row in the ONE redb store and apply it to the running
/// shim. Called from the preferences executor on the retained storage worker, never the frame
/// thread. The whole `Settings` value is written back to the SAME key, so the two existing fields
/// keep their on-disk shape while an install without `p2pEnabled` gains the ON default. The reply
/// is the durability of the write: an applied-but-unwritten toggle is never reported as saved.
pub(crate) fn set_p2p_enabled(on: bool) -> bool {
    let Some(store) = plx_nuvio_store::get() else { return false; };
    let mut settings: Settings = store
        .get(plx_nuvio_store::Table::LocalSettings, "settings")
        .as_deref()
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or_default();
    settings.p2p_enabled = on;
    let durable = serde_json::to_vec(&settings)
        .ok()
        .and_then(|bytes| store.put(plx_nuvio_store::Table::LocalSettings, "settings", &bytes).ok())
        .is_some();
    P2P_FALLBACK.store(on, Ordering::Relaxed);
    if let Some(handle) = handle() {
        handle.shim.set_p2p(on, settings.torrent_server.clone());
    }
    durable
}

/// Fork Rofocale11: the data root now lives in `plx_base::paths` so the app, the shim and the
/// in-process storage backend all resolve the SAME single redb file without a circular edge.
fn data_dir() -> PathBuf {
    plx_base::paths::nuvio_data_dir()
}

/// Fork Rofocale11: an install that predates the Settings P2P row has no `p2pEnabled` key. It
/// defaults ON so existing installs keep the engine-gated torrent behavior they have today; only
/// an explicit write can turn it off. The other two fields keep their exact on-disk spelling.
fn default_p2p_enabled() -> bool { true }

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(default, rename_all = "camelCase")]
struct Settings {
    torrent_server: Option<String>,
    enable_torrents: bool,
    // Fork Rofocale11: the Settings P2P switch lives in this same one-redb value; an absent key
    // reads ON (see `default_p2p_enabled`) so an older install keeps its torrents.
    #[serde(default = "default_p2p_enabled")]
    p2p_enabled: bool,
}

// Fork Rofocale11: a derived `Default` would set `p2p_enabled: false` and disagree with the serde
// field default, so a fresh install (no redb record and no settings.json) would silently lose
// torrents. The hand-written impl keeps the two doors in agreement: absent key OR absent record
// both mean ON, while the other two fields keep their ordinary derived values.
impl Default for Settings {
    fn default() -> Self {
        Self { torrent_server: None, enable_torrents: false, p2p_enabled: default_p2p_enabled() }
    }
}

/// `{"local_storage": {"installedAddonUrls": "<raw>", ...}, "profile_id": "1"}`.
fn import(file: &Path) -> Option<plx_stremio::AddonSet> {
    let bytes = std::fs::read(file).ok()?;
    let v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            log("nuvio: import file is not JSON — ignored");
            return None;
        }
    };
    let ls = &v["local_storage"];
    let raw = |k: &str| ls[k].as_str();
    let profile = v["profile_id"].as_str().or_else(|| raw("activeProfileId").map(|s| s.trim_matches('"'))).unwrap_or("1");
    match plx_stremio::AddonSet::from_nuvio_local_storage(
        raw("installedAddonUrls")?,
        raw("installedAddonEnabledStates"),
        raw("installedAddonDisplayNames"),
        raw("addonManifestCacheV2"),
        profile,
    ) {
        Ok(set) => Some(set),
        Err(e) => {
            log(&format!("nuvio: import failed: {e:?}"));
            None
        }
    }
}

fn describe(set: &plx_stremio::AddonSet) -> String {
    set.addons.iter().map(|a| a.manifest.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn save_addons(set: &plx_stremio::AddonSet, path: &Path) -> bool {
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    {
        let _ = path;
        set.to_json().ok().is_some_and(|payload| plx_os::storage::client::save_nuvio(payload).is_ok())
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm")))]
    { set.save(path).is_ok() }
}

fn load_addons(path: &Path) -> Option<plx_stremio::AddonSet> {
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    {
        match plx_os::storage::client::load_nuvio() {
            Ok(Some(payload)) => {
                log("nuvio: addon configuration restored from the single store");
                return plx_stremio::AddonSet::from_json(&payload);
            }
            Err(_) => log("nuvio: addon storage unavailable; retaining import/cache for retry"),
            Ok(None) => (),
        }
        if let Some(set) = plx_stremio::AddonSet::load(path) {
            if save_addons(&set, path) {
                log("nuvio: addon configuration migrated to the single store");
                let _ = std::fs::remove_file(path);
            }
            return Some(set);
        }
        None
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm")))]
    { plx_stremio::AddonSet::load(path) }
}

/// Start the shim and install it. Call once, after `plx_net::net::global_init`, before the boot
/// reads its token/origin. A failure leaves the app as upstream (it will ask for a Plex sign-in),
/// which is loud enough to notice and never worse than not starting.
pub(crate) fn boot() {
    let data = data_dir();
    let _ = std::fs::create_dir_all(&data);
    let addons_path = data.join("addons.json");
    // The dev shell and native app have different UIDs. A group-writable, non-sticky
    // handoff directory lets the app remove the dev-owned file after a verified store save.
    // Stable's runtime root is sticky /tmp; a file placed directly there cannot be removed.
    let handoff = plx_base::paths::runtime_dir().join("nuvio-import/nuvio-import.json");
    let import_path = if handoff.is_file() { handoff } else {
        plx_base::paths::runtime_dir().join("nuvio-import.json")
    };
    let mut fresh_import = false;
    let mut imported = None;
    if import_path.is_file() {
        if let Some(set) = import(&import_path) {
            log(&format!("nuvio: imported {} addon(s) from Nuvio: {}", set.addons.len(), describe(&set)));
            if !save_addons(&set, &addons_path) {
                log("nuvio: could not persist imported addons");
            } else {
                log("nuvio: imported addon configuration persisted");
                if std::fs::remove_file(&import_path).is_err() {
                    log("nuvio: persisted import retained — removal denied");
                }
            }
            fresh_import = true;
            imported = Some(set);
        }
    }
    let set = imported.or_else(|| load_addons(&addons_path)).unwrap_or_else(plx_stremio::AddonSet::defaults);
    log(&format!("nuvio: {} addon(s): {}", set.addons.len(), describe(&set)));
    // Fork Rofocale11: one store for the whole app. Published before the shim starts so the
    // shim's key table, watch state and account session land in the SAME file as the canonical
    // session record (the storage backend opens it lazily on its first call).
    match plx_nuvio_store::install_default() {
        Ok(_) => log("nuvio: single redb store opened"),
        Err(e) => log(&format!("nuvio: redb store unavailable: {e}")),
    }
    let settings_path = data.join("settings.json");
    let settings: Settings = plx_nuvio_store::get()
        .and_then(|s| s.get(plx_nuvio_store::Table::LocalSettings, "settings"))
        .and_then(|b| serde_json::from_slice(&b).ok())
        .or_else(|| {
            let from_file = std::fs::read(&settings_path).ok().and_then(|b| serde_json::from_slice(&b).ok());
            if let (Some(store), Some(value)) = (plx_nuvio_store::get(), from_file.as_ref()) {
                if let Ok(bytes) = serde_json::to_vec(value) {
                    let _ = store.put(plx_nuvio_store::Table::LocalSettings, "settings", &bytes);
                }
            }
            from_file
        })
        .unwrap_or_default();
    let cfg = plx_pmsshim::Config {
        data_dir: Some(data.join("shim")),
        torrent_server: settings.torrent_server.clone(),
        // Fork Rofocale11: the persisted Settings switch is now authoritative, but the engine is
        // still the only streaming path, so a build without libsbengine.so never lists picks it
        // cannot play. An absent `p2pEnabled` key deserializes ON, preserving today's behavior.
        p2p_enabled: settings.p2p_enabled && plx_stremio::sbengine::available(),
        ..Default::default()
    };
    // Fork Rofocale11: the same decision is the frame-thread fallback until the shim is published.
    P2P_FALLBACK.store(cfg.p2p_enabled, Ordering::Relaxed);
    let backend = Arc::new(plx_pmsshim::AddonBackend::new(set));
    let started = plx_pmsshim::start(backend.clone(), cfg.clone(), PORT).or_else(|_| plx_pmsshim::start(backend.clone(), cfg, 0));
    let handle = match started {
        Ok(h) => h,
        Err(e) => {
            log(&format!("nuvio: shim failed to start: {e}"));
            return;
        }
    };
    // Fork Rofocale11: Nuvio-only switch — in Auto, DTS/TrueHD/Opus that this TV cannot decode
    // are converted by the app to stereo 48 kHz PCM instead of refused. Upstream Plex builds do
    // not call this, so their behavior is untouched across merges.
    plx_os::devcaps::fork_enable_auto_convert();
    handle.shim.probes.set_prober(prober());
    // A torrent's probe is the 10 s on a 4K remux (container header + tail cues, fetched from the
    // swarm), re-paid on every relaunch because the payload is tmpfs. The verdict itself is a few
    // KB and cannot go stale (an infohash fixes the bytes), so it persists beside the swarm state —
    // the same ladder the engine uses, first writable rung.
    if let Some(dir) = plx_base::paths::p2p_state_candidates()
        .into_iter()
        .find(|d| std::fs::create_dir_all(d).is_ok())
    {
        handle.shim.probes.attach(dir.join(plx_pmsshim::shim_probe::PROBE_CACHE_FILE));
    }
    // The DASH gate's second question: can each trailer track be read at its END? googlevideo
    // serves some signed URLs only up to a prefix, and such a pair passes the probe and then fails
    // mid-playback (see `Shim::trailer_tracks_reachable`).
    handle.shim.set_trailer_reacher(trailer_reacher());
    // Fork Rofocale11: DTS/TrueHD/Opus are converted by the app in Auto when this TV has no
    // native path, so the shim's stream ranking must not mark those tracks unplayable.
    handle.shim.set_audio_codecs(
        plx_os::devcaps::caps().audio.split(',')
            .map(str::to_string)
            .chain(plx_os::devcaps::auto_convert_codecs().map(str::to_string))
            .collect::<Vec<_>>()
    );
    plx_base::local_server::set(handle.origin(), handle.token.clone(), plx_pmsshim::MACHINE_ID.to_string());
    log(&format!("nuvio: shim serving on {}", handle.origin()));
    let shim = handle.shim.clone();
    // A Nuvio sign-in (the QR screen) or a profile pick hands its addons over here.
    {
        let (backend, shim, path) = (backend.clone(), shim.clone(), addons_path.clone());
        shim.clone().account.set_sink(Arc::new(move |set: plx_stremio::AddonSet| {
            log(&format!("nuvio: account addons: {}", describe(&set)));
            if !save_addons(&set, &path) {
                log("nuvio: could not persist account addons");
            }
            backend.replace(set);
            shim.addons_changed();
            shim.warm();
        }));
    }
    // Fork Rofocale11: the old fork's addon-enable seam, adapted to this tree's single redb store
    // and `plx_pmsshim` backend. Settings cannot name pmsshim itself, so it reaches the installed
    // addon set through this registered control; a flip replaces the live backend set, records the
    // override in the shim's account prefs, and drops the caches that depended on the old set.
    {
        struct Ctl {
            backend: Arc<plx_pmsshim::AddonBackend>,
            shim: Arc<plx_pmsshim::shim_pms::Shim>,
        }
        impl plx_plex::plex::nuvio::AddonControl for Ctl {
            fn installed(&self) -> Vec<(String, String, bool)> {
                self.backend.snapshot().addons.into_iter()
                    .map(|a| {
                        let name = a.display_name.clone().unwrap_or_else(|| a.manifest.name.clone());
                        (a.transport_url, name, !a.disabled)
                    })
                    .collect()
            }
            fn set_enabled(&self, key: String, enabled: bool) -> bool {
                let mut set = self.backend.snapshot();
                let mut found = false;
                for a in set.addons.iter_mut() {
                    if a.transport_url == key {
                        a.disabled = !enabled;
                        found = true;
                    }
                }
                if !found { return false; }
                self.backend.replace(set);
                self.shim.account.set_addon_enabled(&key, enabled);
                self.shim.addons_changed();
                self.shim.warm();
                true
            }
        }
        plx_plex::plex::nuvio::register_addon_control(Arc::new(Ctl {
            backend: backend.clone(),
            shim: handle.shim.clone(),
        }));
    }
    let _ = BACKEND.set(backend.clone());
    let _ = HANDLE.set(handle);
    shim.warm();
    // Off the boot path: manifests an import could not carry, and a streaming server on this TV.
    let need_server = settings.enable_torrents && settings.torrent_server.is_none();
    let _ = plx_base::task::spawn("nuvio-boot", move || {
        if fresh_import {
            let mut set = backend.snapshot();
            if set.refresh_manifests(std::time::Duration::from_secs(20)) > 0 {
                let _ = save_addons(&set, &addons_path);
                backend.replace(set);
                shim.addons_changed();
                shim.warm();
            }
        }
        if need_server {
            if let Some(s) = plx_stremio::torrent::probe_local_server() {
                log("nuvio: streaming server found on this device; torrents enabled");
                shim.set_torrent_server(Some(s));
            }
        }
    });
}

/// The shim's view of `plx_nuvio_probe::probe_url`, with this TV's decode verdicts.
/// Fork Rofocale11: the probe used to live in upstream's `media::ff::probe`; it is now the
/// fork-owned plx_nuvio_probe crate so a merge cannot reintroduce the conflict or a second ABI table.
/// The shim's view of a trailer track's byte-offset reach: open `url` at `at` with a PRIVATE
/// transfer (not the one the player's `ACTIVE` holds, so asking never disturbs playback) and
/// report the total size. The transport's own bounded open/read deadlines apply; the shim's
/// deadline argument is a policy hint it passes through, not a second clock to run here.
fn trailer_reacher() -> plx_pmsshim::shim_probe::Reacher {
    Arc::new(|url: &str, at: i64, _deadline: std::time::Duration| {
        let (src, _abort) = plx_media::curlio::CurlSource::open_private(url, at)
            .map_err(|e| format!("{e:?}"))?;
        Ok(src.size())
    })
}

fn prober() -> plx_pmsshim::shim_probe::Prober {
    use plx_pmsshim::shim_probe as p;
    Arc::new(|url: &str, deadline: std::time::Duration| {
        log("nuvio: probing selected stream");
        let m = plx_nuvio_probe::probe_url(url, deadline)?;
        if let Some(v) = &m.video {
            log(&format!("nuvio: probe {} {}x{} fps={:.3} dovi={} audio={} subs={}",
                v.codec, v.width, v.height, v.fps, v.dovi.is_some(), m.audio.len(), m.subtitles.len()));
        }
        let caps = plx_os::devcaps::caps();
        let video_decodable = m.video.as_ref().is_some_and(|v| match v.codec.as_str() {
            "h264" => true,
            "hevc" => caps.hevc,
            "vp9" => caps.vp9,
            "av1" => caps.av1,
            _ => false,
        });
        Ok(p::ProbeInfo {
            container: m.container().to_string(),
            duration_ms: m.duration_ms,
            size: m.size,
            video: m.video.as_ref().map(|v| p::Video {
                codec: v.codec.clone(),
                width: v.width,
                height: v.height,
                fps: v.fps,
                bit_depth: v.bit_depth,
                profile: v.profile,
                color_trc: v.color_trc,
                color_primaries: v.color_primaries,
                color_space: v.color_space,
                dovi: v.dovi.as_ref().map(|d| p::Dovi { profile: d.profile, level: d.level, bl_compat: d.bl_compat, rpu: d.rpu, el: d.el, bl: d.bl }),
            }),
            video_decodable,
            audio: m
                .audio
                .iter()
                .map(|a| p::Audio {
                    codec: a.codec.clone(),
                    profile: a.profile,
                    channels: a.channels,
                    // Fork Rofocale11: the old shim publishes the file's sample rate as PMS
                    // `samplingRate`; the player's AC3/PCM lanes read it.
                    sample_rate: a.sample_rate,
                    language: a.language.clone(),
                    title: a.title.clone(),
                    default: a.default,
                    // Fork Rofocale11: "decodable" also covers a track the app converts to
                    // stereo PCM itself in Auto when the TV has no native path for it. Without
                    // this the shim refuses DTS/TrueHD/Opus before the player ever opens them.
                    decodable: caps.audio_supports(&a.codec, i64::from(a.channels))
                        || caps.app_decodes_lossy_track(&a.codec, i64::from(a.channels), 0, false),
                })
                .collect(),
            subtitles: m
                .subtitles
                .iter()
                .map(|s| p::Sub { codec: s.codec.clone(), language: s.language.clone(), title: s.title.clone(), default: s.default, forced: s.forced })
                .collect(),
        })
    })
}

/// Fork Rofocale11: test-only seam for the frame-thread fallback; the bridge publication test
/// drives the switch without booting a shim or touching the process-wide store.
#[cfg(test)]
pub(crate) fn set_p2p_fallback_for_test(on: bool) { P2P_FALLBACK.store(on, Ordering::Relaxed); }

#[cfg(test)]
mod tests {
    use super::*;

    /// Fork Rofocale11: an install whose settings.json predates the switch must keep torrents on,
    /// while an explicit off survives the write-back. This pins the on-disk shape of the one redb
    /// LocalSettings value the storage worker rewrites as a whole.
    #[test]
    fn p2p_setting_defaults_on_and_round_trips_explicit_off() {
        assert!(Settings::default().p2p_enabled, "a fresh install keeps torrents on");
        let older: Settings = serde_json::from_str(r#"{"torrentServer":"http://127.0.0.1:2710","enableTorrents":true}"#).unwrap();
        assert!(older.p2p_enabled, "an absent key means ON, so existing installs keep torrents");
        assert_eq!(older.torrent_server.as_deref(), Some("http://127.0.0.1:2710"));
        assert!(older.enable_torrents);
        let explicit: Settings = serde_json::from_str(r#"{"p2pEnabled":false}"#).unwrap();
        assert!(!explicit.p2p_enabled);
        let value = serde_json::to_value(&explicit).unwrap();
        assert_eq!(value["p2pEnabled"], false);
        assert_eq!(value["enableTorrents"], false, "the other fields keep their shape on write");
        assert!(value["torrentServer"].is_null());
    }
}
