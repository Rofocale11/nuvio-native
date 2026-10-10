//! Run the shim as a standalone server, for the simulator (or a TV on the LAN with `--host 0.0.0.0`).
//!
//!     cargo run -p plx_pmsshim --bin pmsshim-serve -- --port 32400 [--fake] [--host 127.0.0.1]
//!         [--data DIR] [--addons FILE] [--torrent-server http://host:11470]
//!
//! Without `--fake` it serves the addons saved in `--addons` (plx_stremio's JSON), or the defaults.

use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let val = |k: &str| args.iter().position(|a| a == k).and_then(|i| args.get(i + 1)).cloned();
    let port: u16 = val("--port").and_then(|v| v.parse().ok()).unwrap_or(32400);
    let host = val("--host").unwrap_or_else(|| "127.0.0.1".into());
    let mut cfg = plx_pmsshim::Config { data_dir: val("--data").map(Into::into), torrent_server: val("--torrent-server"), ..Default::default() };
    if let Some(t) = val("--token") {
        cfg.token = t;
    }
    let _ = plx_net::net::global_init();
    let backend: Arc<dyn plx_pmsshim::Backend> = if args.iter().any(|a| a == "--fake") {
        Arc::new(plx_pmsshim::shim_fake::FakeBackend)
    } else {
        let set = val("--addons").and_then(|p| plx_stremio::addons::AddonSet::load(std::path::Path::new(&p))).unwrap_or_else(plx_stremio::addons::AddonSet::defaults);
        eprintln!("pmsshim: {} addon(s): {}", set.addons.len(), set.addons.iter().map(|a| a.manifest.name.as_str()).collect::<Vec<_>>().join(", "));
        Arc::new(plx_pmsshim::AddonBackend::new(set))
    };
    let h = plx_pmsshim::start_on(backend, cfg, &host, port).unwrap_or_else(|e| {
        eprintln!("pmsshim: bind {host}:{port}: {e}");
        std::process::exit(1)
    });
    eprintln!("pmsshim: serving PMS dialect on http://{host}:{}", h.port);
    loop {
        std::thread::park();
    }
}
