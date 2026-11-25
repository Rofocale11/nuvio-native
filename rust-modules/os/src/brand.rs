//! Fork Rofocale11: the product name the fork shows to its users.
//!
//! `plx_plex::plex::identity::PRODUCT` stays `PlxNative`: it is also the value of the `X-Plex-Product`
//! wire header, which is part of the PMS dialect the in-process shim speaks so the upstream player
//! keeps working. This constant is the SEPARATE, user-facing name, so rebranding the UI cannot
//! change a byte on the wire.

/// The name printed in diagnostics, support read-outs and the simulator's window title.
pub const UI_PRODUCT: &str = "Nuvio Native";
