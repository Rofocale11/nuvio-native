//! Fork Rofocale11: the ONE synthetic debug mode shared by the simulator and the TV.
//!
//! `plx_pmsshim::shim_fake::FakeBackend` is a fixed, network-free library (four Blender films, one
//! synthetic series, two streams each). It was used only by unit tests and the standalone
//! `pmsshim-serve --fake` CLI. This module makes it a boot-time choice, behind the same devtrig
//! door every other harness case uses, so a headless run on the TV and `make sim` on the desktop
//! exercise the SAME content and the SAME in-process shim.
//!
//! The trigger is deliberately NOT in `dev::DIAG`: changing what Home shows must mark the boot
//! automated and suppress the who's-watching picker, exactly like any other content trigger.
//! It is read only in the boot/runner half (never in a machine's `step`), and it is compiled out
//! of release builds with the `devtriggers` feature.

use std::sync::Arc;

/// Bare devtrig name — `plxnative-nuviofake` in the install's runtime directory.
const TRIGGER: &str = "nuviofake";

/// What the shim should serve, and whether this boot is synthetic at all.
pub(super) struct Selection {
    /// The backend handed to `plx_pmsshim::start`. `FakeBackend` when synthetic.
    pub backend: Arc<dyn plx_pmsshim::Backend>,
    /// The real addon backend, present ONLY when not synthetic. The account addon sink and
    /// Settings > Addons need it; under fake content they must be inert.
    pub addons: Option<Arc<plx_pmsshim::AddonBackend>>,
}

/// Is this boot synthetic? `false` at compile time without `devtriggers`.
pub(super) fn enabled() -> bool {
    #[cfg(feature = "devtriggers")]
    {
        plx_base::devtrig::flag(TRIGGER)
    }
    #[cfg(not(feature = "devtriggers"))]
    {
        let _ = TRIGGER;
        false
    }
}

/// Choose the shim backend. `synthetic` is passed in by the boot runner so this decision is a
/// pure, testable function of its two inputs rather than a second filesystem read.
pub(super) fn select(set: plx_stremio::AddonSet, synthetic: bool) -> Selection {
    if synthetic {
        super::log(
            "nuvio: SYNTHETIC debug backend — fixed catalog, no Nuvio account reads or writes",
        );
        return Selection {
            backend: Arc::new(plx_pmsshim::shim_fake::FakeBackend),
            addons: None,
        };
    }
    let addons = Arc::new(plx_pmsshim::AddonBackend::new(set));
    Selection {
        backend: addons.clone(),
        addons: Some(addons),
    }
}

/// Synthetic boots open NOTHING durable: no key table, watch state or account session from this
/// install, no torrent engine for a fake infoHash, and no account watch pull/push.
pub(super) fn config(mut cfg: plx_pmsshim::Config, synthetic: bool) -> plx_pmsshim::Config {
    if synthetic {
        cfg.synthetic = true;
        cfg.data_dir = None;
        cfg.torrent_server = None;
        cfg.p2p_enabled = false;
    }
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_synthetic_selection_serves_the_fixed_catalog_and_has_no_addon_backend() {
        let sel = select(plx_stremio::AddonSet::default(), true);
        assert!(sel.addons.is_none());
        let keys: Vec<String> = sel.backend.catalogs().into_iter().map(|c| c.key).collect();
        assert!(
            keys.iter().any(|k| k == "fake/movie/top"),
            "the fixed catalog must reach the shim"
        );
    }

    #[test]
    fn a_normal_selection_keeps_the_addon_backend() {
        let sel = select(plx_stremio::AddonSet::default(), false);
        assert!(sel.addons.is_some());
    }

    #[test]
    fn synthetic_config_opens_no_durable_state() {
        let cfg = config(
            plx_pmsshim::Config {
                data_dir: Some(std::path::PathBuf::from("/must/not/be/opened")),
                torrent_server: Some("http://127.0.0.1:11470".into()),
                p2p_enabled: true,
                ..Default::default()
            },
            true,
        );
        assert!(cfg.synthetic);
        assert!(cfg.data_dir.is_none());
        assert!(cfg.torrent_server.is_none());
        assert!(!cfg.p2p_enabled);
    }

    #[cfg(feature = "devtriggers")]
    #[test]
    fn the_trigger_is_read_from_the_install_runtime_root() {
        let _serial = plx_base::testlock::serial();
        plx_base::devtrig::with_private_triggers(|| {
            assert!(
                !enabled(),
                "an unarmed runtime root must not select fake content"
            );
            let path = plx_base::devtrig::path(TRIGGER);
            std::fs::write(&path, "").expect("arm trigger");
            assert!(
                enabled(),
                "the bare name must map to plxnative-nuviofake in the runtime root"
            );
            std::fs::remove_file(&path).expect("disarm trigger");
            assert!(!enabled());
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
            false,
        );
        assert!(!cfg.synthetic);
        assert_eq!(cfg.data_dir.as_deref(), Some(std::path::Path::new("keep")));
        assert!(cfg.p2p_enabled);
    }
}
