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

pub mod backend;
pub mod fake;
pub mod http;
pub mod media;
pub mod pms;
pub mod probe;
pub mod store;
pub mod subs;

pub use backend::{AddonBackend, Backend, CatalogInfo};

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
            stream_deadline: Duration::from_secs(8),
            stream_grace: Duration::from_millis(2500),
            token: "nuvionative-local".into(),
        }
    }
}

pub struct Handle {
    pub port: u16,
    pub token: String,
    pub shim: Arc<pms::Shim>,
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
    let shim = Arc::new(pms::Shim::new(backend, cfg));
    let s2 = shim.clone();
    let handler: Arc<http::Handler> = Arc::new(move |req: &http::Request| s2.handle(req));
    std::thread::Builder::new().name("pmsshim".into()).spawn(move || http::serve(listener, handler))?;
    Ok(Handle { port, token, shim })
}
