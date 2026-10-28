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
}

#[derive(Default, serde::Deserialize)]
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

/// Settings > Playback's P2P switch and engine URL, through the session cache: whether the Source
/// Picker may offer peer-to-peer streams, and the streaming server they play through (`None` probes
/// the default local ports). The shim is told both at boot and after every save.
pub fn p2p() -> (bool, Option<String>) {
    let session = super::session::peek();
    (session.p2p_enabled(), session.p2p_server_url().map(str::to_owned))
}
/// Called on the storage worker; publish only a verified DB8/file write.
pub fn set_p2p_enabled(enabled: bool) -> bool {
    super::session::update_with_outcome(|s| Some(s.with_p2p_enabled(enabled)))
        .is_some_and(|write| matches!(write.classify(), super::session::async_persistence::CompletionOutcome::Durable(_)))
}
/// Called on the storage worker; publish only a verified DB8/file write.
pub fn set_p2p_server(url: Option<String>) -> bool {
    super::session::update_with_outcome(|s| Some(s.with_p2p_server_url(url)))
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
