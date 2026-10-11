//! **The in-process server the app talks to instead of Plex** (NuvioNative). Set once at boot by
//! the app's `nuvio` module after it starts the PMS shim on loopback; read by the three seams that
//! otherwise name an external server: the primary PMS origin, the token that authorises it, and
//! the plex.tv base the account calls go to. Unset (tests, a controlled replay boot) leaves every
//! seam exactly as upstream wrote it.

use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct LocalServer {
    /// `http://127.0.0.1:<port>` — always plain HTTP to a literal loopback address.
    pub origin: String,
    /// The token the shim was started with. Not a secret (loopback only); never logged anyway.
    pub token: String,
    /// Identity returned by this exact in-process server.
    pub machine_id: String,
}

static SLOT: OnceLock<LocalServer> = OnceLock::new();

/// Install the local server. First call wins; returns false if one was already installed.
pub fn set(origin: String, token: String, machine_id: String) -> bool {
    SLOT.set(LocalServer { origin, token, machine_id }).is_ok()
}

pub fn get() -> Option<&'static LocalServer> {
    SLOT.get()
}
