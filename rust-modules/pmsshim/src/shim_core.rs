//! The shim's configuration, handle and entry points (re-exported at the crate root).

use crate::shim_backend::Backend;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The PMS machine identifier the shim advertises; stable so the app's per-server caches survive.
pub const MACHINE_ID: &str = "nuvionative-pmsshim-0001";

#[derive(Clone, Debug)]
pub struct Config {
    /// Where the key table and watch state live; `None` keeps everything in memory (tests).
    pub data_dir: Option<PathBuf>,
    /// A Stremio-compatible streaming server (`http://host:11470`) for `infoHash` streams.
    pub torrent_server: Option<String>,
    /// May peer-to-peer (torrent/magnet) streams be offered at all. Off by default: a torrent the
    /// client has no engine for is a pick that cannot start, so it is not listed. The app's
    /// Settings > Playback switch sets this at boot (`nuvio::restore_p2p`).
    pub p2p_enabled: bool,
    /// How long a detail request waits for addons' streams at most.
    pub stream_deadline: Duration,
    /// Once one playable stream has arrived, how much longer to wait for the rest.
    pub stream_grace: Duration,
    /// The token the app sends; checked on nothing (loopback only), carried for the boot wiring.
    pub token: String,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            data_dir: None,
            torrent_server: None,
            p2p_enabled: false,
            stream_deadline: Duration::from_secs(60),
            stream_grace: Duration::from_millis(2500),
            token: "nuvionative-local".into(),
        }
    }
}

pub struct Handle {
    pub port: u16,
    pub token: String,
    pub shim: Arc<crate::shim_pms::Shim>,
}

impl Handle {
    /// `http://127.0.0.1:{port}` — what the app registers as its server and as plex.tv.
    pub fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Bind `127.0.0.1:{port}` (0 = any free port) and serve on a background thread.
pub fn start(backend: Arc<dyn Backend>, cfg: Config, port: u16) -> std::io::Result<Handle> {
    start_on(backend, cfg, "127.0.0.1", port)
}

/// [`start`] on an explicit interface (the host bin's `--host`, e.g. to serve a TV on the LAN).
pub fn start_on(backend: Arc<dyn Backend>, cfg: Config, host: &str, port: u16) -> std::io::Result<Handle> {
    if let Some(d) = &cfg.data_dir {
        std::fs::create_dir_all(d)?;
    }
    let listener = TcpListener::bind((host, port))?;
    let port = listener.local_addr()?.port();
    let token = cfg.token.clone();
    let shim = Arc::new(crate::shim_pms::Shim::new(backend, cfg));
    shim.bind_self();
    shim.account.set_origin(&format!("http://127.0.0.1:{port}"));
    // Continue Watching is account-backed: merge the account's resume points at boot, and again
    // whenever the profile changes (a profile's history is its own).
    let s3 = shim.clone();
    shim.account.set_watch_sink(Arc::new(move || s3.pull_account_watch()));
    shim.pull_account_watch();
    let s2 = shim.clone();
    let handler: Arc<crate::shim_http::Handler> = Arc::new(move |req: &crate::shim_http::Request| s2.handle(req));
    std::thread::Builder::new().name("pmsshim".into()).spawn(move || crate::shim_http::serve(listener, handler))?;
    Ok(Handle { port, token, shim })
}
