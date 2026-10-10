//! **NuvioNative boot**: start the in-process PMS shim (`plx_pmsshim`) over the user's Stremio
//! addons and install it as the app's one server (`plx_base::local_server`), before anything asks
//! for a server or for plex.tv. See `docs/nuvio/ARCHITECTURE.md`.
//!
//! - Addons live in `<data>/addons.json` (`plx_stremio::AddonSet`'s format); Cinemeta + OpenSubtitles
//!   when there is none.
//! - `<runtime dir>/nuvio-import.json` (written by `tools/nv-import-nuvio.mjs` from the user's Nuvio
//!   install) replaces that set once, is persisted, then deleted.
//! - The shim's watch state and key table live in `<data>/shim/`. `<data>` survives an app
//!   reinstall on the TV (`/media/developer/<appid>.nuvio`, beside the session file), so watch
//!   progress is not lost with each `.ipk`.
//! - Streams are probed with the bundled FFmpeg at play time ([`prober`]).
//! - Torrents play through a Stremio-compatible streaming server: `<data>/settings.json`'s
//!   `torrentServer`, else one found running on this device.
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
    let tv = Path::new("/media/developer");
    if tv.is_dir() {
        let d = tv.join(format!("{}.nuvio", plx_base::paths::app_id()));
        if std::fs::create_dir_all(&d).is_ok() {
            return d;
        }
    }
    plx_base::paths::persistent_state_root().join("nuvio")
}

#[derive(serde::Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Settings {
    torrent_server: Option<String>,
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

/// Start the shim and install it. Call once, after `plx_net::net::global_init`, before the boot
/// reads its token/origin. A failure leaves the app as upstream (it will ask for a Plex sign-in),
/// which is loud enough to notice and never worse than not starting.
pub(crate) fn boot() {
    let data = data_dir();
    let _ = std::fs::create_dir_all(&data);
    let addons_path = data.join("addons.json");
    let import_path = plx_base::paths::runtime_dir().join("nuvio-import.json");
    let mut fresh_import = false;
    if import_path.is_file() {
        if let Some(set) = import(&import_path) {
            log(&format!("nuvio: imported {} addon(s) from Nuvio: {}", set.addons.len(), describe(&set)));
            if set.save(&addons_path).is_err() {
                log("nuvio: could not persist imported addons");
            }
            fresh_import = true;
        }
        let _ = std::fs::remove_file(&import_path);
    }
    let set = plx_stremio::AddonSet::load(&addons_path).unwrap_or_else(plx_stremio::AddonSet::defaults);
    log(&format!("nuvio: {} addon(s): {}", set.addons.len(), describe(&set)));
    let settings: Settings = std::fs::read(data.join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let cfg = plx_pmsshim::Config {
        data_dir: Some(data.join("shim")),
        torrent_server: settings.torrent_server.clone(),
        ..Default::default()
    };
    let backend = Arc::new(plx_pmsshim::AddonBackend::new(set));
    let started = plx_pmsshim::start(backend.clone(), cfg.clone(), PORT).or_else(|_| plx_pmsshim::start(backend.clone(), cfg, 0));
    let handle = match started {
        Ok(h) => h,
        Err(e) => {
            log(&format!("nuvio: shim failed to start: {e}"));
            return;
        }
    };
    handle.shim.probes.set_prober(prober());
    handle.shim.set_audio_codecs(plx_platform::devcaps::caps().audio.split(',').map(str::to_string).collect());
    plx_base::local_server::set(handle.origin(), handle.token.clone());
    log(&format!("nuvio: shim serving on {}", handle.origin()));
    let shim = handle.shim.clone();
    // A Nuvio sign-in (the QR screen) or a profile pick hands its addons over here.
    {
        let (backend, shim, path) = (backend.clone(), shim.clone(), addons_path.clone());
        shim.clone().account.set_sink(Arc::new(move |set: plx_stremio::AddonSet| {
            log(&format!("nuvio: account addons: {}", describe(&set)));
            if set.save(&path).is_err() {
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
    // Off the boot path: manifests an import could not carry, and a streaming server on this TV.
    let need_server = settings.torrent_server.is_none();
    let _ = plx_base::task::spawn("nuvio-boot", move || {
        if fresh_import {
            let mut set = backend.snapshot();
            if set.refresh_manifests(std::time::Duration::from_secs(20)) > 0 {
                let _ = set.save(&addons_path);
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

/// The shim's view of `plx_media::ff::probe::probe_url`, with this TV's decode verdicts.
fn prober() -> plx_pmsshim::shim_probe::Prober {
    use plx_pmsshim::shim_probe as p;
    Arc::new(|url: &str, deadline: std::time::Duration| {
        let m = plx_media::ff::probe::probe_url(url, deadline)?;
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
                    language: a.language.clone(),
                    title: a.title.clone(),
                    default: a.default,
                    decodable: caps.audio_supports(&a.codec, i64::from(a.channels)),
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
