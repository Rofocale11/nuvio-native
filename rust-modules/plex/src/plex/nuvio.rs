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
