//! **The PMS shim.** An in-process HTTP server on 127.0.0.1 that answers the subset of the Plex
//! Media Server REST dialect the PlxNative front end requests (`tests/mock_pms.py` is the field
//! spec; `docs/pms-api.md` the protocol), backed by Stremio addons. The front end registers it as
//! its one server and never learns it is not Plex — see `docs/nuvio/ARCHITECTURE.md`.
//!
//! Mapping:
//! - sections `1` Movies and `2` TV Shows list the first movie/series catalog;
//! - Home hubs are Continue Watching plus one hub per home catalog (`/hubs/nuvio/{i}`, paged);
//! - a Stremio title, a season and an episode each get a stable numeric ratingKey ([`store::Keys`]);
//! - a movie's or episode's `Media[]` is its playable streams, best first ([`media`]); a Part key
//!   redirects to the stream's real URL;
//! - the transcoder decision always answers direct play; watch state is kept locally.
//!
//! Side API for the front end's own Nuvio screens (not PMS): `/nuvio/streams?ratingKey=` lists
//! the classified streams, `/nuvio/select?ratingKey=&index=` moves one to `Media[0]`.

pub mod shim_account;
pub mod shim_backend;
pub mod shim_fake;
pub mod shim_http;
pub mod shim_media;
pub mod shim_pms;
pub mod shim_probe;
pub mod shim_store;
pub mod shim_subs;
pub mod shim_core;

pub use shim_backend::{AddonBackend, Backend, CatalogInfo};
pub use shim_core::{start, start_on, Config, Handle, MACHINE_ID};
