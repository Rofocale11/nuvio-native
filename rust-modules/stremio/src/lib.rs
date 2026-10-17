//! NuvioNative backend: the Stremio addon protocol, the Nuvio account import, and stream
//! classification. This crate knows nothing about Plex or the UI; the PMS shim (`plx_pmsshim`)
//! adapts it to the PMS dialect the PlxNative front end already speaks.
//!
//! CONTRACT (fixed by the lead; the shim codes against it): the types in [`model`] and the
//! functions/methods re-exported below. Extend freely; do not rename or remove without telling the
//! lead.
//!
//! LOGGING RULE: configured addon URLs and stream URLs carry credentials (debrid keys, AIOStreams
//! config blobs). Log them only through [`redact_url`] (host only); `Debug` on [`Addon`] and
//! [`Stream`] already does.

pub mod model;
pub use model::*;

pub mod addons;
pub use addons::AddonSet;

pub mod classify;
pub use classify::{classify_stream, StreamInfo};

pub mod nuvio;

/// The HTTP door. Named `addon_http` because the module-layer gate reads every workspace crate as
/// one module tree, where `http` is already `plx_plex`'s; `plx_stremio::http` stays the public path.
pub mod addon_http;
pub use addon_http as http;

pub mod torrent;
pub use torrent::playable_url;

pub mod import;

mod lenient;
