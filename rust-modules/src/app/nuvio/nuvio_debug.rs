//! Fork Rofocale11: the TWO synthetic debug modes shared by the simulator and the TV.
//!
//! `plx_pmsshim::shim_fake::FakeBackend` is a fixed, network-free library (four Blender films, one
//! synthetic series, two streams each). `plxnative-nuviofake` makes it a boot-time choice, behind
//! the same devtrig door every other harness case uses, so a headless run on the TV and `make sim`
//! on the desktop exercise the SAME content and the SAME in-process shim.
//!
//! `plxnative-nuviolive` is the agent-facing host loop: it keeps the SAME sealed identity and
//! private redb (no household account, addons or watch records) but selects the REAL `AddonBackend`
//! with public Stremio addons and the real TMDB client. That is the configuration in which UI,
//! enrichment and fetch/cache behavior can honestly be tested without a TV or an owner login.
//!
//! Both triggers are read only in the boot/runner half (never in a machine's `step`), and they are
//! compiled out of release builds with the `devtriggers` feature. Changing what Home shows marks
//! the boot automated and suppresses the who's-watching picker, exactly like any other trigger.

use std::sync::Arc;

/// Bare devtrig name — `plxnative-nuviofake` in the install's runtime directory.
const TRIGGER: &str = "nuviofake";
/// Bare devtrig name — `plxnative-nuviolive` in the simulator's instance root.
const LIVE_TRIGGER: &str = "nuviolive";
/// Optional bare devtrig naming a JSON file with a tracked public-addon list.
const ADDONS_TRIGGER: &str = "nuvioaddons";

/// Built-in public addon set for the live loop. No debrid key, no account, no household config.
const DEFAULT_LIVE_ADDONS: &[&str] = &[
    "https://v3-cinemeta.strem.io",
    // Fork Rofocale11: the configured Torrentio endpoint caps each provider at 2; the bare
    // endpoint returned hundreds of streams per detail open and got rate-limited on repeated
    // agent loops. The configured path must stay here and in the tracked addon list.
    "https://torrentio.strem.fun/qualityfilter=unknown,cam,other,scr,threed,480p,720p|limit=2/manifest.json",
    "https://opensubtitles-v3.strem.io",
];

/// Which boot shape the dev triggers asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    /// Ordinary boot: the install's stored sign-in and addon set.
    Normal,
    /// `plxnative-nuviofake`: fixed network-free content.
    Fake,
    /// `plxnative-nuviolive`: real addons + TMDB, sealed identity and store.
    Live,
}

impl Mode {
    /// Sealed boots never open the household account, addon config or redb.
    pub(super) fn sealed(self) -> bool {
        matches!(self, Mode::Fake | Mode::Live)
    }
}

/// Read the boot mode once. Live wins if both content triggers are armed.
pub(super) fn mode() -> Mode {
    #[cfg(feature = "devtriggers")]
    {
        if plx_base::devtrig::flag(LIVE_TRIGGER) {
            if plx_base::devtrig::flag(TRIGGER) {
                super::log("nuvio: both nuviofake and nuviolive armed — nuviolive wins");
            }
            return Mode::Live;
        }
        if plx_base::devtrig::flag(TRIGGER) {
            return Mode::Fake;
        }
    }
    #[cfg(not(feature = "devtriggers"))]
    {
        // The names are not read in a shipping build, but the literals exist to stay in step
        // with the dev trigger vocabulary; name them once so the build does not deny them.
        let _ = (TRIGGER, LIVE_TRIGGER, ADDONS_TRIGGER);
    }
    Mode::Normal
}

/// The public addon list for a live loop: a JSON file named by `plxnative-nuvioaddons` when
/// present, otherwise the built-in Cinemeta/Torrentio/OpenSubtitles set. The file is a tracked,
/// account-free list of manifest URLs; no Nuvio addon config is ever read on this path.
pub(super) fn public_addons() -> plx_stremio::AddonSet {
    let urls = live_addon_urls();
    let mut set = plx_stremio::AddonSet::default();
    for url in urls {
        match set.install(&url) {
            Ok(_) => super::log(&format!("nuvio: LIVE addon installed: {url}")),
            Err(e) => super::log(&format!("nuvio: LIVE addon FAILED {url}: {e:?}")),
        }
    }
    let tmdb = plx_stremio::TmdbClient::from_env().is_some_and(|c| c.configured());
    super::log(&format!(
        "nuvio: LIVE tmdb={} — real addons, real shim caches, sealed account",
        if tmdb { "configured" } else { "unconfigured" }
    ));
    set
}

fn live_addon_urls() -> Vec<String> {
    #[cfg(feature = "devtriggers")]
    if let Some(path) = plx_base::devtrig::read(ADDONS_TRIGGER) {
        if !path.is_empty() {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Some(urls) = parse_addon_urls(&text) {
                    return urls;
                }
            }
            super::log("nuvio: LIVE addon list unreadable — using built-in public addons");
        }
    }
    DEFAULT_LIVE_ADDONS.iter().map(|s| (*s).to_string()).collect()
}

/// Parse `{"addons": ["https://…", …]}`; only non-empty strings count.
#[cfg(any(feature = "devtriggers", test))]
fn parse_addon_urls(text: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let urls: Vec<String> = value
        .get("addons")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
        .collect();
    (!urls.is_empty()).then_some(urls)
}

/// What the shim should serve, and whether this boot has an addon sink at all.
pub(super) struct Selection {
    /// The backend handed to `plx_pmsshim::start`. `FakeBackend` only for Fake boots.
    pub backend: Arc<dyn plx_pmsshim::Backend>,
    /// The real addon backend, present except in Fake mode. The account addon sink and
    /// Settings > Addons need it; under fixed fake content they must be inert.
    pub addons: Option<Arc<plx_pmsshim::AddonBackend>>,
}

/// Choose the shim backend. A pure function of the boot mode and the addon set.
pub(super) fn select(set: plx_stremio::AddonSet, mode: Mode) -> Selection {
    if mode == Mode::Fake {
        super::log(
            "nuvio: SYNTHETIC debug backend — fixed catalog, no Nuvio account reads or writes",
        );
        return Selection {
            backend: Arc::new(plx_pmsshim::shim_fake::FakeBackend),
            addons: None,
        };
    }
    if mode == Mode::Live {
        super::log("nuvio: LIVE backend — public Stremio addons, no household records");
    }
    let addons = Arc::new(plx_pmsshim::AddonBackend::new(set));
    Selection {
        backend: addons.clone(),
        addons: Some(addons),
    }
}

/// Fake boots open NOTHING durable and force P2P off. Live boots keep the shim's real cache
/// directory (inside the private store installed by `prepare_store`) and the engine's own gate;
/// only the account is sealed, via `Config::synthetic = true`.
pub(super) fn config(mut cfg: plx_pmsshim::Config, mode: Mode) -> plx_pmsshim::Config {
    match mode {
        Mode::Fake => {
            cfg.synthetic = true;
            cfg.data_dir = None;
            cfg.torrent_server = None;
            cfg.p2p_enabled = false;
        }
        Mode::Live => {
            cfg.synthetic = true;
        }
        Mode::Normal => {}
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fake_selection_serves_the_fixed_catalog_and_has_no_addon_backend() {
        let sel = select(plx_stremio::AddonSet::default(), Mode::Fake);
        assert!(sel.addons.is_none());
        let keys: Vec<String> = sel.backend.catalogs().into_iter().map(|c| c.key).collect();
        assert!(
            keys.iter().any(|k| k == "fake/movie/top"),
            "the fixed catalog must reach the shim"
        );
    }

    #[test]
    fn a_normal_selection_keeps_the_addon_backend() {
        let sel = select(plx_stremio::AddonSet::default(), Mode::Normal);
        assert!(sel.addons.is_some());
    }

    #[test]
    fn a_live_selection_keeps_the_real_addon_backend() {
        let sel = select(plx_stremio::AddonSet::default(), Mode::Live);
        assert!(sel.addons.is_some());
    }

    #[test]
    fn fake_config_opens_no_durable_state() {
        let cfg = config(
            plx_pmsshim::Config {
                data_dir: Some(std::path::PathBuf::from("/must/not/be/opened")),
                torrent_server: Some("http://127.0.0.1:11470".into()),
                p2p_enabled: true,
                ..Default::default()
            },
            Mode::Fake,
        );
        assert!(cfg.synthetic);
        assert!(cfg.data_dir.is_none());
        assert!(cfg.torrent_server.is_none());
        assert!(!cfg.p2p_enabled);
    }

    #[test]
    fn live_config_keeps_the_real_cache_path_but_seals_the_account() {
        let cfg = config(
            plx_pmsshim::Config {
                data_dir: Some(std::path::PathBuf::from("private/shim")),
                p2p_enabled: true,
                ..Default::default()
            },
            Mode::Live,
        );
        assert!(cfg.synthetic, "the account must stay sealed");
        assert_eq!(cfg.data_dir.as_deref(), Some(std::path::Path::new("private/shim")));
        assert!(cfg.p2p_enabled);
    }

    #[test]
    fn both_modes_are_sealed_and_normal_is_not() {
        assert!(Mode::Fake.sealed());
        assert!(Mode::Live.sealed());
        assert!(!Mode::Normal.sealed());
    }

    #[test]
    fn parse_addon_urls_accepts_only_non_empty_strings() {
        let urls = parse_addon_urls(r#"{"addons":["https://a","",3,"https://b"]}"#).unwrap();
        assert_eq!(urls, vec!["https://a", "https://b"]);
        assert!(parse_addon_urls("{}").is_none());
        assert!(parse_addon_urls("not json").is_none());
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn the_fake_trigger_is_read_from_the_install_runtime_root() {
        let _serial = plx_base::testlock::serial();
        plx_base::devtrig::with_private_triggers(|| {
            assert_eq!(mode(), Mode::Normal, "an unarmed runtime root must stay normal");
            let path = plx_base::devtrig::path(TRIGGER);
            std::fs::write(&path, "").expect("arm trigger");
            assert_eq!(mode(), Mode::Fake);
            std::fs::remove_file(&path).expect("disarm trigger");
            assert_eq!(mode(), Mode::Normal);
        });
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn the_live_trigger_wins_over_the_fake_one() {
        let _serial = plx_base::testlock::serial();
        plx_base::devtrig::with_private_triggers(|| {
            let fake = plx_base::devtrig::path(TRIGGER);
            let live = plx_base::devtrig::path(LIVE_TRIGGER);
            std::fs::write(&fake, "").expect("arm fake");
            assert_eq!(mode(), Mode::Fake);
            std::fs::write(&live, "").expect("arm live");
            assert_eq!(mode(), Mode::Live);
            std::fs::remove_file(&live).expect("disarm live");
            std::fs::remove_file(&fake).expect("disarm fake");
        });
    }

    #[test]
    fn normal_config_is_untouched() {
        let cfg = config(
            plx_pmsshim::Config {
                data_dir: Some(std::path::PathBuf::from("keep")),
                p2p_enabled: true,
                ..Default::default()
            },
            Mode::Normal,
        );
        assert!(!cfg.synthetic);
        assert_eq!(cfg.data_dir.as_deref(), Some(std::path::Path::new("keep")));
        assert!(cfg.p2p_enabled);
    }
}
