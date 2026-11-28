//! Fork Rofocale11: Settings ▸ Addons must not perform IO on the frame thread.
//!
//! The upstream `AddonControl` seam is synchronous (`installed()` -> `bool`, `set_enabled()` ->
//! `bool`). The old fork used it directly from the screen's key handler, which put a full
//! `AddonSet` clone, a `nuvio-session.json` write and — worst — five `nuvio.redb` table
//! clear+commit cycles (each an exclusive write transaction and an fsync) on the frame thread.
//! Profiles already solved this class of bug by moving the writes to a worker; addons had no
//! such wrapper.
//!
//! This module keeps a small, frame-safe cached view of `(url, name, enabled)`. Reads clone that
//! vector under a short mutex — no registry, no manifest clone, no IO. A toggle flips the cached
//! row optimistically so the screen's immediate `rebuild()` shows the press, then performs the
//! durable work (backend replace, account prefs write, cache invalidation, warm) on an
//! application worker. `refresh()` is called from the account sink when a profile replaces the
//! whole addon set off-frame.
//!
//! Recorded in `docs/nuvio/MERGE-AUDIT.md`.

use std::sync::{Arc, Mutex, OnceLock};

use plx_base::eventlog::log;

type Row = (String, String, bool);

fn view_of(set: &plx_stremio::AddonSet) -> Vec<Row> {
    set.addons
        .iter()
        .map(|a| {
            let name = a.display_name.clone().unwrap_or_else(|| a.manifest.name.clone());
            (a.transport_url.clone(), name, !a.disabled)
        })
        .collect()
}

static VIEW: OnceLock<Arc<Mutex<Vec<Row>>>> = OnceLock::new();

/// Refresh the frame-safe view after an off-frame replacement of the whole set (account sink).
pub(crate) fn refresh(set: &plx_stremio::AddonSet) {
    if let Some(view) = VIEW.get() {
        if let Ok(mut rows) = view.lock() {
            *rows = view_of(set);
        }
    }
}

/// Register the control. Called at boot with the durable backend already resolved; the initial
/// snapshot here is boot work, not frame work.
pub(crate) fn register(backend: Arc<plx_pmsshim::AddonBackend>, shim: Arc<plx_pmsshim::shim_pms::Shim>) {
    let view = Arc::new(Mutex::new(view_of(&backend.snapshot())));
    let _ = VIEW.set(Arc::clone(&view));
    plx_plex::plex::nuvio::register_addon_control(Arc::new(Ctl { backend, shim, view }));
}

struct Ctl {
    backend: Arc<plx_pmsshim::AddonBackend>,
    shim: Arc<plx_pmsshim::shim_pms::Shim>,
    view: Arc<Mutex<Vec<Row>>>,
}

impl plx_plex::plex::nuvio::AddonControl for Ctl {
    fn installed(&self) -> Vec<Row> {
        // Fork Rofocale11: a short lock over a small cached vector. The old implementation read the
        // shim's global RwLock and cloned every manifest here, on the frame thread.
        self.view.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_enabled(&self, key: String, enabled: bool) -> bool {
        // Optimistic flip: the screen rebuilds from `installed()` on this same key press, so the
        // toggle must read back as pressed without waiting for the worker.
        {
            let mut rows = self.view.lock().unwrap_or_else(|e| e.into_inner());
            let Some(row) = rows.iter_mut().find(|(url, _, _)| url == &key) else {
                return false;
            };
            row.2 = enabled;
        }

        let backend = Arc::clone(&self.backend);
        let shim = Arc::clone(&self.shim);
        let view = Arc::clone(&self.view);
        let k = key.clone();
        plx_base::eventlog::log("nuvio: addon toggle on worker");
        let queued = plx_base::task::spawn("nuvio-addon-toggle", move || {
            let mut set = backend.snapshot();
            let mut found = false;
            for a in set.addons.iter_mut() {
                if a.transport_url == k {
                    a.disabled = !enabled;
                    found = true;
                }
            }
            if !found {
                // Undo the optimistic flip: the key no longer names an installed addon.
                if let Ok(mut rows) = view.lock() {
                    if let Some(row) = rows.iter_mut().find(|(url, _, _)| url == &k) {
                        row.2 = !enabled;
                    }
                }
                log(&format!("nuvio: addon toggle refused — {k} is not installed"));
                return;
            }
            backend.replace(set);
            shim.account.set_addon_enabled(&k, enabled);
            shim.addons_changed();
            shim.warm();
        });
        if queued.is_none() {
            // The OS refused a worker; never leave the cached view claiming a change that did not
            // happen. The screen will rebuild from the restored value on the next poll.
            if let Ok(mut rows) = self.view.lock() {
                if let Some(row) = rows.iter_mut().find(|(url, _, _)| url == &key) {
                    row.2 = !enabled;
                }
            }
            log("nuvio: addon toggle refused — no worker thread available");
            return false;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_plex::plex::nuvio::AddonControl;

    /// The regression pin: the old inline `Ctl` read the shim's global RwLock and cloned every
    /// manifest in `installed()`, and performed five redb clear+commit cycles in `set_enabled()`.
    /// Both ran inside the screen's key handler. `FrameScope` makes `#[cfg(test)]`
    /// `assert_may_block` panic if a guarded IO call is restored on this path.
    #[test]
    fn addon_control_does_no_io_on_the_frame_thread() {
        let mut set = plx_stremio::AddonSet::default();
        set.addons.push(plx_stremio::addons::builtin_cinemeta());
        let key = set.addons[0].transport_url.clone();
        let backend = Arc::new(plx_pmsshim::AddonBackend::new(set));
        let shim = Arc::new(plx_pmsshim::shim_pms::Shim::new(
            Arc::clone(&backend) as Arc<dyn plx_pmsshim::Backend>,
            plx_pmsshim::Config::default(),
        ));
        let ctl = Ctl {
            backend: Arc::clone(&backend),
            shim,
            view: Arc::new(Mutex::new(view_of(&backend.snapshot()))),
        };

        let _frame = plx_base::task::FrameScope::enter();
        assert_eq!(ctl.installed().len(), 1);
        assert!(ctl.set_enabled(key, false));
        // The cached view flips before the worker returns, so the screen's immediate rebuild
        // still shows the press; durable state follows on the worker.
        assert!(!ctl.installed()[0].2);

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !backend.snapshot().addons[0].disabled {
            assert!(std::time::Instant::now() < deadline, "the toggle worker did not commit");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}
