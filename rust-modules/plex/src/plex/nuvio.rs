//! Credential-free stream summaries and selection on the in-process Nuvio server.
use super::Client;
use super::client::QueryBuilder;

#[derive(Clone, Default, Debug, PartialEq, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StreamChoice {
    pub index: usize,
    pub label: String,
    pub addon: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub height: Option<u32>,
    pub hdr: String,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub size_bytes: Option<u64>,
    pub bitrate_kbps: Option<u64>,
    /// P2P seeders, read out of the addon's text (`👤 123`, `👤 1.2k`). The shim already sends
    /// it in `/nuvio/streams`; the picker needs it to rank a swarm by whether anybody seeds it.
    pub seeders: Option<u32>,
}

#[derive(Clone, Default, serde::Deserialize)]
pub struct StreamList {
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub streams: Vec<StreamChoice>,
}

impl Client {
    pub fn is_nuvio(&self) -> bool {
        self.machine_id() == "nuvionative-pmsshim-0001"
            || plx_base::local_server::get().is_some_and(|s| s.origin == self.origin().base())
    }

    pub fn nuvio_streams(&self, rk: &str) -> Option<StreamList> {
        if !self.is_nuvio() { return None; }
        let path = QueryBuilder::new("/nuvio/streams").str("ratingKey", rk).build();
        serde_json::from_slice(&self.get_bytes(&path)?).ok()
    }

    /// Fork Rofocale11: the shim answers `/library/metadata/{rk}/allLeaves` with every episode of
    /// a show in one hop, built from the already-cached series meta (`shim_pms.rs`'s `leaves()`).
    /// The detail data layer reads it instead of the serial season → season → episode chain so a
    /// show open costs one round trip rather than three. A real PMS has no such route: return None
    /// there so callers take the ordinary `/children` path unchanged.
    pub fn all_leaves(&self, rk: &str) -> Option<super::MediaContainer> {
        if !self.is_nuvio() { return None; }
        self.get_json(&format!("/library/metadata/{rk}/allLeaves"))
    }

    pub fn nuvio_select(&self, rk: &str, index: usize) -> bool {
        if !self.is_nuvio() { return false; }
        let path = QueryBuilder::new("/nuvio/select").str("ratingKey", rk).int("index", index as i64).build();
        self.get_bytes(&path).and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .is_some_and(|v| v["ok"] == true)
    }
}

/// Read the durable installation preference through the session cache.
pub fn rating_source() -> super::session::RatingSource { super::session::peek().rating_source() }
/// Called on the storage worker; publish only a verified DB8/file write.
pub fn set_rating_source(source: super::session::RatingSource) -> bool {
    super::session::update_with_outcome(|s| Some(s.with_rating_source(source)))
        .is_some_and(|write| matches!(write.classify(), super::session::async_persistence::CompletionOutcome::Durable(_)))
}


#[derive(Clone, Copy, Default, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryState { pub signed_in: bool, pub saved: bool }

impl Client {
    pub fn nuvio_library(&self, rk: &str, saved: Option<bool>) -> Option<LibraryState> {
        if !self.is_nuvio() { return None; }
        let mut path = QueryBuilder::new("/nuvio/library").str("ratingKey", rk);
        let method = if let Some(saved) = saved {
            path = path.int("saved", i64::from(saved));
            crate::http::Method::Post
        } else { crate::http::Method::Get };
        serde_json::from_slice(&self.body_2xx(&path.build(), method, &[])?).ok()
    }
}

// Fork Rofocale11: the old fork's addon-enable seam, ported verbatim. The screens layer may not
// name `pmsshim` (it is a lower layer and cannot depend on the app), so Settings asks for the
// installed addon list and flips one through this registered control instead. Registration stays
// at boot in the application; no store record is introduced — the shim's own account prefs and
// `AddonBackend` remain the one durable source.
use std::sync::{Arc, OnceLock};

/// Runtime control over the installed addon set, registered by the app at boot (the screens
/// layer may not name `pmsshim` directly). `installed` returns `(key, display name, enabled)`.
pub trait AddonControl: Send + Sync {
    fn installed(&self) -> Vec<(String, String, bool)>;
    fn set_enabled(&self, key: String, enabled: bool) -> bool;
}

static ADDON_CONTROL: OnceLock<Arc<dyn AddonControl>> = OnceLock::new();

pub fn register_addon_control(ctl: Arc<dyn AddonControl>) {
    let _ = ADDON_CONTROL.set(ctl);
}

pub fn installed_addons() -> Vec<(String, String, bool)> {
    ADDON_CONTROL.get().map(|c| c.installed()).unwrap_or_default()
}

pub fn set_addon_enabled(key: String, enabled: bool) -> bool {
    ADDON_CONTROL.get().map(|c| c.set_enabled(key, enabled)).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    /// Fork Rofocale11: before the app registers its boot control (the state of every unit-test
    /// process) a screen must see an empty addon list and a refused flip, never a panic. The
    /// once-set seam is deliberately quiet rather than fabricating a backend.
    #[test]
    fn addon_control_is_quiet_until_the_app_registers_it() {
        assert!(super::installed_addons().is_empty());
        assert!(!super::set_addon_enabled("https://addon.example/manifest.json".into(), true));
    }
}
