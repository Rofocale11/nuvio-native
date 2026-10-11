//! Stremio addon protocol types (https://github.com/Stremio/stremio-addon-sdk/tree/master/docs/api).
//! Field names follow the wire format; unknown fields are ignored.

use serde::{Deserialize, Serialize};

/// `"movie"`, `"series"`, `"channel"`, `"tv"`, or anything an addon declares.
pub type ContentType = String;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub description: Option<String>,
    pub logo: Option<String>,
    /// Either plain names (`"stream"`) or objects; normalised by [`Manifest::resources`].
    #[serde(rename = "resources")]
    pub raw_resources: Vec<serde_json::Value>,
    pub types: Vec<ContentType>,
    pub id_prefixes: Option<Vec<String>>,
    pub catalogs: Vec<CatalogDecl>,
    pub behavior_hints: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceDecl {
    pub name: String,
    pub types: Option<Vec<ContentType>>,
    pub id_prefixes: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CatalogDecl {
    /// `apiType` is the field name in Nuvio's cached (already-mapped) manifests.
    #[serde(rename = "type", alias = "apiType")]
    pub type_: ContentType,
    pub id: String,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub name: Option<String>,
    #[serde(deserialize_with = "crate::lenient::extras")]
    pub extra: Vec<ExtraDecl>,
    /// Legacy form of `extra[].isRequired`.
    pub extra_required: Vec<String>,
    pub extra_supported: Vec<String>,
    /// The addon's `showInHome`: when ANY catalog of an addon sets it explicitly, Nuvio puts on
    /// Home only the ones set to true.
    #[serde(deserialize_with = "crate::lenient::opt_bool")]
    pub show_in_home: Option<bool>,
    /// Page size for `skip` paging, when the addon declares one.
    #[serde(deserialize_with = "crate::lenient::opt_u32")]
    pub page_size: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtraDecl {
    pub name: String,
    pub is_required: bool,
    pub options: Vec<String>,
}

/// An installed addon: where it lives plus its manifest.
/// `Debug` prints the transport URL as host only: configured addon URLs (AIOStreams, Torrentio
/// with debrid) embed credentials, and logs must never carry them.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Addon {
    /// Base URL, WITHOUT the trailing `/manifest.json` (that is how Stremio and Nuvio store it).
    pub transport_url: String,
    pub manifest: Manifest,
    /// User disabled it without uninstalling.
    pub disabled: bool,
    /// The user's display-name override (Nuvio's `name` column), when set.
    pub display_name: Option<String>,
}

impl std::fmt::Debug for Addon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Addon")
            .field("transport_url", &redact_url(&self.transport_url))
            .field("id", &self.manifest.id)
            .field("name", &self.manifest.name)
            .field("disabled", &self.disabled)
            .field("display_name", &self.display_name)
            .finish()
    }
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stream")
            .field("url", &self.url.as_deref().map(redact_url))
            .field("info_hash", &self.info_hash)
            .field("file_idx", &self.file_idx)
            .field("external_url", &self.external_url.as_deref().map(redact_url))
            .field("name", &self.name)
            .field("title", &self.title)
            .field("description", &self.description)
            .field("behavior_hints", &self.behavior_hints)
            .field("subtitles", &self.subtitles.len())
            .field("addon_name", &self.addon_name)
            .finish()
    }
}

/// `scheme://host` of a URL, for logs: never the path or query, which carry addon configuration
/// and credentials.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s, r),
        None => return "<url>".to_string(),
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or(authority);
    format!("{scheme}://{host}")
}

impl Addon {
    /// What the UI calls this addon: the override, else the manifest name, else the id.
    pub fn label(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(Some(self.manifest.name.as_str()).filter(|s| !s.trim().is_empty()))
            .unwrap_or(&self.manifest.id)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MetaPreview {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: ContentType,
    pub name: String,
    pub poster: Option<String>,
    pub background: Option<String>,
    pub logo: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub description: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub release_info: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub imdb_rating: Option<String>,
    /// Optional provider scores returned by metadata addons (MDBList's wire shapes).
    pub ratings: serde_json::Value,
    #[serde(rename = "mdbListRatings")]
    pub mdb_list_ratings: serde_json::Value,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub rotten_tomatoes_rating: Option<String>,
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub genres: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Meta {
    pub id: String,
    #[serde(rename = "type")]
    pub type_: ContentType,
    pub name: String,
    pub poster: Option<String>,
    pub background: Option<String>,
    pub logo: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub description: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub release_info: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub released: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub imdb_rating: Option<String>,
    /// Optional provider scores returned by metadata addons (MDBList's wire shapes).
    pub ratings: serde_json::Value,
    #[serde(rename = "mdbListRatings")]
    pub mdb_list_ratings: serde_json::Value,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub rotten_tomatoes_rating: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub runtime: Option<String>,
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub genres: Vec<String>,
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub cast: Vec<String>,
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub director: Vec<String>,
    #[serde(deserialize_with = "crate::lenient::string_vec")]
    pub writer: Vec<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub country: Option<String>,
    pub videos: Vec<Video>,
    /// IMDb id when the meta's own id is not one (`tmdb:…` catalogs carry it here).
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub imdb_id: Option<String>,
    pub trailers: Vec<serde_json::Value>,
    pub behavior_hints: serde_json::Value,
}

/// A series episode (or a channel video).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Video {
    /// For series, conventionally `tt1234567:1:2` (imdb:season:episode).
    pub id: String,
    /// Some addons use `name` instead of `title`.
    pub title: Option<String>,
    pub name: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_u32")]
    pub season: Option<u32>,
    #[serde(deserialize_with = "crate::lenient::opt_u32")]
    pub episode: Option<u32>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub released: Option<String>,
    pub thumbnail: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub overview: Option<String>,
}

/// `Debug` redacts `url`/`external_url` to host only (debrid/AIOStreams playback URLs embed
/// credentials).
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Stream {
    pub url: Option<String>,
    pub info_hash: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_u32")]
    pub file_idx: Option<u32>,
    pub file_must_include: Option<serde_json::Value>,
    pub yt_id: Option<String>,
    pub external_url: Option<String>,
    /// Usually the addon/provider label plus quality, e.g. `"Torrentio\n4k DV"`.
    pub name: Option<String>,
    /// Legacy; newer addons use `description`. Torrentio puts size/seeders/file name here.
    pub title: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_string")]
    pub description: Option<String>,
    pub sources: Vec<String>,
    pub subtitles: Vec<Subtitle>,
    pub behavior_hints: StreamBehaviorHints,
    /// Filled by this crate, not the wire: the addon that returned it.
    #[serde(skip_deserializing)]
    pub addon_name: String,
    #[serde(skip_deserializing)]
    pub addon_id: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StreamBehaviorHints {
    pub not_web_ready: bool,
    pub binge_group: Option<String>,
    pub filename: Option<String>,
    #[serde(deserialize_with = "crate::lenient::opt_u64")]
    pub video_size: Option<u64>,
    pub video_hash: Option<String>,
    /// `{ "request": {hdr: val}, "response": {...} }`
    pub proxy_headers: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Subtitle {
    pub id: Option<String>,
    pub url: String,
    pub lang: String,
}

#[derive(Debug)]
pub enum Error {
    /// Transport failure or timeout.
    Network,
    /// Non-2xx status.
    Status(u16),
    /// Body was not the expected JSON.
    Parse(String),
    /// No installed addon serves this request.
    NoAddon,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Network => write!(f, "network error"),
            Error::Status(s) => write!(f, "HTTP {s}"),
            Error::Parse(m) => write!(f, "parse error: {m}"),
            Error::NoAddon => write!(f, "no addon serves this request"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
