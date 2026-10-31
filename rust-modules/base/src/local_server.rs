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


/// Path to the shim's on-disk session (`nuvio-session.json`). Registered by `Account::new`, which
/// runs inside `Shim::new` **before** the shim binds its listener, so the first play can read
/// preferences without waiting for app wiring. Replaceable rather than `OnceLock`: a host run or a
/// test may start a second shim with its own data directory.
static SESSION_FILE: std::sync::RwLock<Option<std::path::PathBuf>> =
    std::sync::RwLock::new(None);

/// Install (or re-point) the session file. Returns whether this changed the registered path.
pub fn set_session_file(p: std::path::PathBuf) -> bool {
    let mut slot = SESSION_FILE.write().unwrap_or_else(|e| e.into_inner());
    let changed = slot.as_deref() != Some(p.as_path());
    *slot = Some(p);
    changed
}

pub fn session_file() -> Option<std::path::PathBuf> {
    SESSION_FILE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The slot used to be a `OnceLock`: the first shim's path stuck for the life of the process,
    /// so a host run or a test that started a second shim read the first one's preferences.
    #[test]
    fn the_session_file_can_be_repointed() {
        let _serial = crate::testlock::serial();
        let first = std::path::PathBuf::from("/tmp/nuvio-a/nuvio-session.json");
        let second = std::path::PathBuf::from("/tmp/nuvio-b/nuvio-session.json");
        assert!(set_session_file(first.clone()));
        assert_eq!(session_file(), Some(first.clone()));
        assert!(!set_session_file(first.clone()), "the same path is not a change");
        assert!(set_session_file(second.clone()));
        assert_eq!(session_file(), Some(second));
    }
}
