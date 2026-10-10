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
    #[serde(rename = "type")]
    pub type_: ContentType,
    pub id: String,
    pub name: Option<String>,
    pub extra: Vec<ExtraDecl>,
    /// Legacy form of `extra[].isRequired`.
    pub extra_required: Vec<String>,
    pub extra_supported: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ExtraDecl {
    pub name: String,
    pub is_required: bool,
    pub options: Vec<String>,
}

/// An installed addon: where it lives plus its manifest.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Addon {
    /// Base URL, WITHOUT the trailing `/manifest.json` (that is how Stremio and Nuvio store it).
    pub transport_url: String,
    pub manifest: Manifest,
    /// User disabled it without uninstalling.
    pub disabled: bool,
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
    pub description: Option<String>,
    pub release_info: Option<String>,
    pub imdb_rating: Option<String>,
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
    pub description: Option<String>,
    pub release_info: Option<String>,
    pub released: Option<String>,
    pub imdb_rating: Option<String>,
    pub runtime: Option<String>,
    pub genres: Vec<String>,
    pub cast: Vec<String>,
    pub director: Vec<String>,
    pub writer: Vec<String>,
    pub country: Option<String>,
    pub videos: Vec<Video>,
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
    pub season: Option<u32>,
    #[serde(alias = "number")]
    pub episode: Option<u32>,
    pub released: Option<String>,
    pub thumbnail: Option<String>,
    #[serde(alias = "description")]
    pub overview: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Stream {
    pub url: Option<String>,
    pub info_hash: Option<String>,
    pub file_idx: Option<u32>,
    pub file_must_include: Option<serde_json::Value>,
    pub yt_id: Option<String>,
    pub external_url: Option<String>,
    /// Usually the addon/provider label plus quality, e.g. `"Torrentio\n4k DV"`.
    pub name: Option<String>,
    /// Legacy; newer addons use `description`. Torrentio puts size/seeders/file name here.
    pub title: Option<String>,
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

pub type Result<T> = std::result::Result<T, Error>;
