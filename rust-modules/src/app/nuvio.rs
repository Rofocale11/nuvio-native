//! **NuvioNative boot**: start the in-process PMS shim (`plx_pmsshim`) over the user's Stremio
//! addons and install it as the app's one server (`plx_base::local_server`), before anything asks
//! for a server or for plex.tv. See `docs/nuvio/ARCHITECTURE.md`.
//!
//! - TV addons live in private DB8 storage; host addons live in `<data>/addons.json` (`plx_stremio::AddonSet`'s format); Cinemeta + OpenSubtitles
//!   when there is none.
//! - `<runtime dir>/nuvio-import/nuvio-import.json` (written by `tools/nv-import-nuvio.mjs` from the user's Nuvio
//!   install) replaces that set once, is persisted, then deleted.
//! - The shim's watch state and key table live in `<data>/shim/`. `<data>` survives an app
//!   reinstall only when a persistent writable directory is available. Developer Mode falls
//!   back to `/tmp`, so watch progress and key mappings can be lost on reboot.
//! - Streams are probed with the bundled FFmpeg at play time ([`prober`]).
//! - Torrents play through a Stremio-compatible streaming server, and only when the person turned
//!   P2P Streaming on in Settings > Playback: `Session::p2p_server_url`, else a scan of the
//!   engine's default local ports. The imported `settings.json` seed is deliberately not honoured
//!   — an off switch that an import can flip is not a switch.
//!
//! Never logs an addon URL: Nuvio/AIOStreams URLs embed credentials. Hosts at most.

use plx_base::eventlog::log;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// The loopback port the shim prefers. Fixed so the server's origin is the same every boot (the
/// session and the per-server caches key on it); any free port if it is taken.
const PORT: u16 = 32_420;

static HANDLE: OnceLock<plx_pmsshim::Handle> = OnceLock::new();
static BACKEND: OnceLock<Arc<plx_pmsshim::AddonBackend>> = OnceLock::new();

/// The running shim, for Settings screens that change addons.
#[allow(dead_code)]
pub(crate) fn handle() -> Option<&'static plx_pmsshim::Handle> {
    HANDLE.get()
}

fn data_dir() -> PathBuf {
    // The first candidate this process can really WRITE: a Developer Mode jail answers `is_dir`
    // and even `create_dir_all` on paths it then refuses files in. The runtime dir is the last
    // resort — writable everywhere, but on a TV it is /tmp, so it lasts until the set reboots.
    let writable = |d: &Path| {
        std::fs::create_dir_all(d).is_ok() && {
            let probe = d.join(".probe");
            let ok = std::fs::write(&probe, b"1").is_ok();
            let _ = std::fs::remove_file(&probe);
            ok
        }
    };
    let candidates = [
        PathBuf::from(format!(
            "/media/developer/{}.nuvio",
            plx_base::paths::app_id()
        )),
        plx_base::paths::persistent_state_root().join("nuvio"),
        plx_base::paths::runtime_dir().join("nuvio"),
    ];
    for d in &candidates {
        if writable(d) {
            log(&format!("nuvio: data dir {}", d.display()));
            return d.clone();
        }
    }
    candidates[2].clone()
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
    let profile = v["profile_id"]
        .as_str()
        .or_else(|| raw("activeProfileId").map(|s| s.trim_matches('"')))
        .unwrap_or("1");
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
    set.addons
        .iter()
        .map(|a| a.manifest.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn save_addons(set: &plx_stremio::AddonSet, path: &Path) -> bool {
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    {
        let _ = path;
        set.to_json()
            .ok()
            .is_some_and(|payload| plx_platform::storage::client::save_nuvio(payload).is_ok())
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm")))]
    {
        set.save(path).is_ok()
    }
}

fn load_addons(path: &Path) -> Option<plx_stremio::AddonSet> {
    #[cfg(all(target_os = "linux", target_arch = "arm"))]
    {
        match plx_platform::storage::client::load_nuvio() {
            Ok(Some(payload)) => {
                log("nuvio: addon configuration restored from DB8");
                return plx_stremio::AddonSet::from_json(&payload);
            }
            Err(_) => log("nuvio: addon storage unavailable; retaining import/cache for retry"),
            Ok(None) => (),
        }
        if let Some(set) = plx_stremio::AddonSet::load(path) {
            if save_addons(&set, path) {
                log("nuvio: addon configuration migrated to DB8");
                let _ = std::fs::remove_file(path);
            }
            return Some(set);
        }
        None
    }
    #[cfg(not(all(target_os = "linux", target_arch = "arm")))]
    {
        plx_stremio::AddonSet::load(path)
    }
}

/// Start the shim and install it. Call once, after `plx_net::net::global_init`, before the boot
/// reads its token/origin. A failure leaves the app as upstream (it will ask for a Plex sign-in),
/// which is loud enough to notice and never worse than not starting.
pub(crate) fn boot() {
    let data = data_dir();
    let _ = std::fs::create_dir_all(&data);
    let addons_path = data.join("addons.json");
    // The dev shell and native app have different UIDs. A group-writable, non-sticky
    // handoff directory lets the app remove the dev-owned file after verified DB8 save.
    // Stable's runtime root is sticky /tmp; a file placed directly there cannot be removed.
    let handoff = plx_base::paths::runtime_dir().join("nuvio-import/nuvio-import.json");
    let import_path = if handoff.is_file() {
        handoff
    } else {
        plx_base::paths::runtime_dir().join("nuvio-import.json")
    };
    let mut fresh_import = false;
    let mut imported = None;
    if import_path.is_file() {
        if let Some(set) = import(&import_path) {
            log(&format!(
                "nuvio: imported {} addon(s) from Nuvio: {}",
                set.addons.len(),
                describe(&set)
            ));
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
    let set = imported
        .or_else(|| load_addons(&addons_path))
        .unwrap_or_else(plx_stremio::AddonSet::defaults);
    log(&format!(
        "nuvio: {} addon(s): {}",
        set.addons.len(),
        describe(&set)
    ));
    let cfg = plx_pmsshim::Config {
        data_dir: Some(data.join("shim")),
        ..Default::default()
    };
    let backend = Arc::new(plx_pmsshim::AddonBackend::new(set));
    let started = plx_pmsshim::start(backend.clone(), cfg.clone(), PORT)
        .or_else(|_| plx_pmsshim::start(backend.clone(), cfg, 0));
    let handle = match started {
        Ok(h) => h,
        Err(e) => {
            log(&format!("nuvio: shim failed to start: {e}"));
            return;
        }
    };
    handle.shim.probes.set_prober(prober());
    // FORK (RichiL10): the shim's `audio_ok` list is used to rank streams (a stream whose label
    // names a codec outside it sits below playable ones) and, more importantly, to decide
    // direct-play vs refusal in `refusal()`. The client-side AC3 encode lane (ff_enc.rs) rescues
    // DTS/TrueHD/MLP to AC3 at play time, so we declare them as playable here; otherwise the
    // shim refuses TrueHD streams the fork can actually handle.
    handle.shim.set_audio_codecs({
        let caps = plx_platform::devcaps::caps();
        let mut v: Vec<String> = caps.audio.split(',').map(str::to_string).collect();
        for c in ["truehd", "mlp", "dts", "dca", "dca-ma", "dts-hd", "dtshd"] {
            if !v.iter().any(|x| x.eq_ignore_ascii_case(c)) {
                v.push(c.to_string());
            }
        }
        v
    });
    plx_base::local_server::set(
        handle.origin(),
        handle.token.clone(),
        plx_pmsshim::MACHINE_ID.to_string(),
    );

    // FORK: register the shim's session file with plx_base so plx_plex can read
    // account preferences (audio/subtitle) straight off disk instead of HTTP.
    if let Some(p) = handle.shim.account.session_file() {
        plx_base::local_server::set_session_file(p.to_path_buf());
    }
    log(&format!("nuvio: shim serving on {}", handle.origin()));
    let shim = handle.shim.clone();
    // A Nuvio sign-in (the QR screen) or a profile pick hands its addons over here.
    {
        let (backend, shim, path) = (backend.clone(), shim.clone(), addons_path.clone());
        shim.clone()
            .account
            .set_sink(Arc::new(move |set: plx_stremio::AddonSet| {
                log(&format!("nuvio: account addons: {}", describe(&set)));
                if !save_addons(&set, &path) {
                    log("nuvio: could not persist account addons");
                }
                backend.replace(set);
                shim.addons_changed();
                shim.warm();
            }));
    }
    let _ = BACKEND.set(backend.clone());
    let _ = HANDLE.set(handle);
    shim.warm();
    // Off the boot path: manifests an import could not carry.
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
    });
}

/// Push the session's P2P preference into the running shim: whether torrent/magnet streams may be
/// offered at all, and the engine they play through (`Session::p2p_server_url`).
///
/// The session's accessor resolves an unsaved preference to the headless P2P server's loopback
/// address, so an ordinary boot hands `Some` here. A `None` from a direct caller still probes the
/// engine's default local ports — on a worker, never the caller's thread, because five refused
/// connections cost seconds. Either way the switch drives the shim, so turning P2P off really does
/// stop torrents from being listed.
pub(crate) fn restore_p2p(enabled: bool, server: Option<String>) {
    let Some(handle) = handle() else { return };
    let shim = handle.shim.clone();
    shim.set_p2p(enabled, server);
    if !enabled || shim.torrent_server().is_some() {
        return;
    }
    let _ = plx_base::task::spawn("nuvio-p2p", move || {
        if let Some(url) = plx_stremio::torrent::probe_local_server() {
            log("nuvio: streaming server found on this device");
            shim.set_p2p(true, Some(url));
        }
    });
}

/// The shim's view of `plx_media::ff::probe::probe_url`, with this TV's decode verdicts.
fn prober() -> plx_pmsshim::shim_probe::Prober {
    use plx_pmsshim::shim_probe as p;
    Arc::new(|url: &str, deadline: std::time::Duration| {
        log("nuvio: probing selected stream");
        let m = plx_media::ff::probe::probe_url(url, deadline)?;
        if let Some(v) = &m.video {
            log(&format!(
                "nuvio: probe {} {}x{} fps={:.3} dovi={} audio={} subs={}",
                v.codec,
                v.width,
                v.height,
                v.fps,
                v.dovi.is_some(),
                m.audio.len(),
                m.subtitles.len()
            ));
        }
        let caps = plx_platform::devcaps::caps();
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
                dovi: v.dovi.as_ref().map(|d| p::Dovi {
                    profile: d.profile,
                    level: d.level,
                    bl_compat: d.bl_compat,
                    rpu: d.rpu,
                    el: d.el,
                    bl: d.bl,
                }),
            }),
            video_decodable,
            audio: m
                .audio
                .iter()
                .map(|a| p::Audio {
                    codec: a.codec.clone(),
                    profile: a.profile,
                    channels: a.channels,
                    language: a.language.clone(),
                    title: a.title.clone(),
                    default: a.default,
                    decodable: true, // <--- Make sure this is still here!
                })
                .collect(),
            subtitles: m
                .subtitles
                .iter()
                .map(|s| p::Sub {
                    codec: s.codec.clone(),
                    language: s.language.clone(),
                    title: s.title.clone(),
                    default: s.default,
                    forced: s.forced,
                })
                .collect(),
        })
    })
}
