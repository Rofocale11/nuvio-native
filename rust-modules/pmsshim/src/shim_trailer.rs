//! YouTube trailer resolution via the Node children (`js-service/trailer-decipher.js`
//! and `js-service/trailer-proxy.js`).
//!
//! The shim owns the HTTP dialect; this module owns only the decipher seam:
//! `youtubeId` → a candidate DASH pair (or the muxed MP4 fallback). It never
//! probes and never serves a `Media`/`Part` — `shim_pms` probes the candidate
//! with `ProbeCache` and only then builds the clip row. Resolution is deferred
//! to the player's first Part request (`shim_pms::trailer_shelf_rows` answers
//! the detail shelf without spawning Node; the first tile is warmed in the
//! background), and a `login_required` answer —
//! YouTube's anonymous age gate, which no client identity can pass — marks the
//! id blocked so the shim walks the title's next trailer (`PLAYBACK.md`). The
//! trailer autoplay (`DWELL_S` → `PreviewStart`) and one-shot full trailer
//! therefore keep the preview's direct-play-only contract (`plan.rs`,
//! `preview.rs`): no transcode, no timeline, `TRAILER_CONTEXT` still omits
//! `continuous`.
//!
//! The DASH pair's bytes come straight from the signed googlevideo URLs; there
//! is no proxy hop and no UA override. What those URLs refuse is the request
//! shape, not the identity: a GET with no `Range` header or an open-ended
//! `bytes=N-` answers `403`, a bounded window answers `206`. `curlio` states
//! the window (`GOOGLEVIDEO_WINDOW`) and continues it, so the manifest needs no
//! rewriting. [`TrailerProxy`] and `js-service/trailer-proxy.js` stay in the
//! tree, unused for track bytes.
//!
//! Design notes (docs/nuvio/ARCHITECTURE.md trailer rule, Node `v16.20.2` floor):
//! - Dependency-injected as `TrailerResolver` so tests use a fake and never
//!   spawn Node. Production resolves through `NodeTrailerResolver`, which
//!   spawns system `/usr/bin/node` (fallback `node` on `PATH`) on the
//!   shipped script, one spawn per resolve, 10 s wall-clock timeout.
//! - `TrailerCache` holds at most 256 entries: positive answers 30 min
//!   (signed URLs expire in hours, like `ProbeCache`'s 30-min host+path TTL),
//!   negative answers 60 s (a blip must not freeze a trailer for half an
//!   hour). `resolve_cached` wraps the resolver with `Flight` single-flight,
//!   the same loop `meta()` uses for addon fetches.
//! - A failed spawn, a non-zero exit, a timeout, unparseable stdout, or
//!   `{"ok":false}` is `None`, never an error string with a URL body: the
//!   caller degrades to thumbnail-only. The URL itself is never logged.

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A deciphered candidate. The shim re-validates it with `ProbeCache`
/// (`container == "mp4"`, `h264` + decodable `aac`) before serving; a
/// candidate that fails the probe is discarded, never served as a guess.
/// Muxed candidates carry `url`; DASH candidates carry `dash`.
#[derive(Clone, Debug, PartialEq)]
pub struct TrailerResolved {
    pub url: String,
    pub itag: i64,
    pub height: i64,
    pub is_dash: bool,
    pub dash: Option<DashInfo>,
    /// The 360p muxed fallback, present alongside a DASH pair. The shim
    /// serves the muxed row when the DASH video-track probe refuses (e.g. a
    /// 403 on the video-only URL), so the shelf never empties.
    pub muxed_url: Option<String>,
    pub muxed_itag: i64,
    pub muxed_height: i64,
}

/// A DASH adaptive pair (video-only + audio-only) the decipher child returns
/// when a ≥720p plain-url pair exists. The shim synthesizes a static MPD from
/// it; probe validates the video track, audio is trusted from the AAC pick.
#[derive(Clone, Debug, PartialEq)]
pub struct DashInfo {
    pub video_url: String,
    pub audio_url: String,
    pub video_itag: i64,
    pub audio_itag: i64,
    pub width: i64,
    pub height: i64,
    pub fps: i64,
    pub video_mime: String,
    pub audio_mime: String,
    pub video_bitrate: i64,
    pub audio_bitrate: i64,
    pub video_init: (i64, i64),
    pub video_index: (i64, i64),
    pub audio_init: (i64, i64),
    pub audio_index: (i64, i64),
    pub duration_ms: i64,
}

/// The decipher seam. `None` = no playable trailer (thumbnail-only).
pub trait TrailerResolver: Send + Sync {
    fn resolve(&self, youtube_id: &str) -> Option<TrailerResolved>;

    /// True when this id was refused in a way no retry can fix (YouTube's
    /// anonymous `LOGIN_REQUIRED` age gate). The shim uses it to decide whether
    /// asking TMDB for the title's other trailers is worth a network hop; a
    /// plain transient failure must not trigger one.
    fn blocked(&self, _youtube_id: &str) -> bool {
        false
    }

    /// The tallest rendition this host decodes — the device's own ceiling — or
    /// `0` when unknown. The autoplay quality scan stops as soon as a candidate
    /// resolves at this height, so a 1080p-only set does not pay a second Node
    /// spawn looking for a 4K track it cannot decode. `0` (the default, and
    /// every test fake) makes the scan settle on the first DASH candidate.
    fn max_height(&self) -> i64 {
        0
    }
}

/// Always `None`. The default in tests and on hosts without Node/script.
pub struct NullTrailerResolver;

impl TrailerResolver for NullTrailerResolver {
    fn resolve(&self, _youtube_id: &str) -> Option<TrailerResolved> {
        None
    }
}

/// Spawn-per-resolve Node child: system `/usr/bin/node` + the shipped
/// `js-service/trailer-decipher.js`. Short-lived (exits after one line);
/// nothing is kept warm, so idle RAM stays the Node baseline only while a
/// resolve is in flight — the OOM guard behind the TV RSS gate.
/// How long an id YouTube refused anonymously (`LOGIN_REQUIRED`, the
/// age-restricted red-band case) is skipped. Six hours is long enough that a
/// browsing session never re-asks, short enough that a friendlier copy (or a
/// changed policy) is picked up the same evening.
const BLOCK_TTL: Duration = Duration::from_secs(6 * 60 * 60);

pub struct NodeTrailerResolver {
    node: String,
    script: PathBuf,
    timeout: Duration,
    /// Ids YouTube will not serve to any anonymous client. Re-asking costs a
    /// Node spawn and answers the same `LOGIN_REQUIRED`, so the shim remembers
    /// and walks straight to the title's next trailer candidate.
    blocked: Mutex<HashMap<String, Instant>>,
    /// What THIS SoC decodes, handed to the child as `PLX_TRAILER_CODECS` /
    /// `PLX_TRAILER_MAXH`: the resolver must not pick a 2160p VP9 track on a
    /// set whose codec table has no VP9 row. Order is preference — `h264`
    /// first, which is the cheapest decode and the best-tested feed path.
    codecs: String,
    max_height: u32,
}

impl NodeTrailerResolver {
    pub fn new(node: String, script: PathBuf) -> NodeTrailerResolver {
        NodeTrailerResolver {
            node,
            script,
            timeout: Duration::from_secs(10),
            blocked: Mutex::new(HashMap::new()),
            codecs: "h264".to_string(),
            max_height: 1080,
        }
    }

    /// Limit the child to what this television's codec table (`devcaps`) can
    /// decode. h264 is always allowed (every webOS SoC has it); VP9/AV1 only
    /// when the device's own table lists the row.
    pub fn with_device_limits(mut self) -> NodeTrailerResolver {
        let caps = plx_platform::devcaps::caps();
        let mut codecs: Vec<&str> = vec!["h264"];
        let mut max_height = caps.h264_row.1;
        if caps.vp9 {
            codecs.push("vp9");
            max_height = max_height.max(caps.vp9_row.1);
        }
        if caps.av1 {
            codecs.push("av1");
            max_height = max_height.max(caps.av1_row.1);
        }
        // YouTube's own ceiling; a 8K row in a device table is not a reason to
        // ask for more than the service publishes.
        if max_height == 0 {
            max_height = 1080;
        }
        self.codecs = codecs.join(",");
        self.max_height = max_height.min(2160).max(1080);
        plx_base::eventlog::log(&format!(
            "trailer: child quality ceiling {}p, codecs {} (device table)",
            self.max_height, self.codecs
        ));
        self
    }

    /// Production paths: absolute `/usr/bin/node` first (the jail's `PATH`
    /// may not carry Node even though the binary exists), `node` fallback;
    /// script discovered beside the running binary (`js-service/…` under
    /// `app_dir`), else relative `js-service/trailer-decipher.js`.
    pub fn default_paths() -> NodeTrailerResolver {
        let node = if std::path::Path::new("/usr/bin/node").is_file() {
            "/usr/bin/node".to_string()
        } else {
            "node".to_string()
        };
        let script = std::env::current_exe()
            .ok()
            .and_then(|exe| {
                exe.parent()
                    .map(|p| p.join("js-service/trailer-decipher.js"))
            })
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("js-service/trailer-decipher.js"));
        NodeTrailerResolver::new(node, script).with_device_limits()
    }
}

impl TrailerResolver for NodeTrailerResolver {
    fn resolve(&self, youtube_id: &str) -> Option<TrailerResolved> {
        if youtube_id.is_empty() || self.is_blocked(youtube_id) {
            return None;
        }
        // Pace youtubei: one Node spawn at a time, >=300ms between starts.
        // Hold the gate across run_node so browsing bursts cannot run N
        // concurrent decipher POST pairs and throttle the TV IP into
        // `formats=0`. The gap is kept while holding the lock.
        let mut gate = decipher_gate().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = *gate {
            let elapsed = last.elapsed();
            let gap = Duration::from_millis(300);
            if elapsed < gap {
                std::thread::sleep(gap - elapsed);
            }
        }
        *gate = Some(Instant::now());
        // `_gate` holds the lock until this function returns (drop at scope end),
        // serializing all decipher starts process-wide.
        let _gate = gate;
        let (stdout, stderr, status) =
            match run_node(
                &self.node,
                &self.script,
                youtube_id,
                self.timeout,
                &self.codecs,
                self.max_height,
            ) {
                Ok(out) => out,
                Err(reason) => {
                    plx_base::eventlog::log(&format!(
                        "trailer: node child failed ({youtube_id}) — {reason}"
                    ));
                    return None;
                }
            };
        if let Some(resolved) = parse_resolved(stdout.as_str()) {
            plx_base::eventlog::log(&format!(
                "trailer: node resolved (youtube={youtube_id} itag={} height={})",
                resolved.itag, resolved.height
            ));
            return Some(resolved);
        }
        let err = extract_error(stdout.as_str()).unwrap_or_else(|| {
            format!(
                "<no parseable json, {len} bytes, status={status}>",
                len = stdout.len()
            )
        });
        if !stderr.trim().is_empty() {
            plx_base::eventlog::log(&format!(
                "trailer: node returned non-ok (youtube={youtube_id}): {err} stderr={e}",
                e = log_excerpt(&stderr)
            ));
        } else {
            plx_base::eventlog::log(&format!(
                "trailer: node returned non-ok (youtube={youtube_id}): {err}"
            ));
        }
        if err.starts_with("login_required") {
            // Every anonymous client (VISIONOS, IOS, ANDROID, TV, WEB) answers
            // "Sign in to confirm your age" for a red-band video; no client
            // identity fixes that, so mark the id and let the caller try the
            // title's next trailer instead of spawning Node for it again.
            self.block(youtube_id);
            plx_base::eventlog::log(&format!(
                "trailer: youtube id is not anonymously playable (youtube={youtube_id}) \
                 — skipping it for 6h"
            ));
        }
        None
    }

    fn blocked(&self, youtube_id: &str) -> bool {
        self.blocked_id(youtube_id)
    }

    fn max_height(&self) -> i64 {
        self.max_height as i64
    }
}

impl NodeTrailerResolver {
    fn blocked_id(&self, youtube_id: &str) -> bool {
        self.is_blocked(youtube_id)
    }

    fn is_blocked(&self, youtube_id: &str) -> bool {
        let mut map = self.blocked.lock().unwrap_or_else(|e| e.into_inner());
        match map.get(youtube_id).copied() {
            Some(at) if at.elapsed() < BLOCK_TTL => true,
            Some(_) => {
                map.remove(youtube_id);
                false
            }
            None => false,
        }
    }

    fn block(&self, youtube_id: &str) {
        let mut map = self.blocked.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, at| at.elapsed() < BLOCK_TTL);
        map.insert(youtube_id.to_string(), Instant::now());
    }
}

/// One line of JSON from the child, under a wall-clock cap. The returned
/// tuple is `(stdout, stderr, exit_status)`. `Err` carries a short,
/// URL-free reason for the failure (spawn failure, non-zero exit, timeout),
/// because every `None` used to be silent and a TV run could not tell a
/// missing binary from a 403 from YouTube. The URL itself is never logged.
fn run_node(
    node: &str,
    script: &PathBuf,
    youtube_id: &str,
    timeout: Duration,
    codecs: &str,
    max_height: u32,
) -> Result<(String, String, String), String> {
    let mut cmd = Command::new(node);
    cmd.env("PLX_TRAILER_CODECS", codecs)
        .env("PLX_TRAILER_MAXH", max_height.to_string())
        .arg(script)
        .arg(youtube_id)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("LD_PRELOAD");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Match boot.rs (start_p2p_service_inner): the expected ppid after
        // fork is THIS process's pid, not `getppid()` before spawn. Using
        // `getppid()` here makes the hook fire on every successful fork
        // and Rust reports it as EINVAL (os error 22) — the TV resolver
        // miss this fixes.
        let expected_ppid = std::process::id() as libc::pid_t;
        unsafe {
            cmd.pre_exec(move || {
                if libc::getppid() != expected_ppid {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "parent died",
                    ));
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("spawn {node} {script:?}: {e}"))?;
    // Log which binary/script we actually tried, so a `/usr/bin/node` vs
    // PATH fallback, or a wrong script path, is visible on the TV.
    plx_base::eventlog::log(&format!(
        "trailer: node spawning (youtube={youtube_id} node={node} script={})",
        script.display()
    ));
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().map_err(|e| format!("wait: {e}"))? {
            let mut out = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut out);
            }
            let mut err = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut err);
            }
            let status_s = status.to_string();
            if !status.success() {
                return Err(format!(
                    "exit {status_s} out={} stderr={}",
                    log_excerpt(&out),
                    log_excerpt(&err)
                ));
            }
            return Ok((out, err, status_s));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("timeout after {}s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// Truncate a child output excerpt for the event log: control runs collapse,
/// over-long text is cut. Never call this on a line containing a signed URL.
fn log_excerpt(s: &str) -> String {
    let one_line: String = s
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if one_line.len() > 300 {
        format!("{}…", &one_line[..300])
    } else {
        one_line
    }
}

/// The `"error":"…"` field the decipher child answers with on `ok:false`
/// (e.g. `http 403`, `no muxed mp4 (formats=0)`). `None` when stdout is not
/// that shape — the caller logs the byte count instead of the raw body,
/// because a success body carries a signed URL that must never be logged.
fn extract_error(stdout: &str) -> Option<String> {
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value
        .get("error")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

/// Parse the child's single JSON line. `kind":"dash"` → a DASH pair;
/// `kind":"muxed"` (or legacy no-kind) → a single URL. Only `ok:true` with
/// an `https://` URL is a candidate; everything else is `None`.
fn parse_resolved(stdout: &str) -> Option<TrailerResolved> {
    let line = stdout.lines().map(str::trim).find(|l| !l.is_empty())?;
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("muxed");
    if kind == "dash" {
        let video = value.get("video")?;
        let audio = value.get("audio")?;
        let vurl = video.get("url").and_then(|v| v.as_str())?;
        let aurl = audio.get("url").and_then(|v| v.as_str())?;
        if !(vurl.starts_with("https://")
            && vurl.contains('.')
            && aurl.starts_with("https://")
            && aurl.contains('.'))
        {
            return None;
        }
        let range = |o: &serde_json::Value, key: &str| -> (i64, i64) {
            (
                o.get(key)
                    .and_then(|r| r.get("start"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
                o.get(key)
                    .and_then(|r| r.get("end"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            )
        };
        let video_range = video.get("range")?;
        let audio_range = audio.get("range")?;
        let duration_ms = value
            .get("durationSec")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            * 1000;
        let height = video.get("height").and_then(|v| v.as_i64()).unwrap_or(0);
        let muxed = value.get("muxed");
        let muxed_url = muxed
            .and_then(|m| m.get("url"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let muxed_itag = muxed
            .and_then(|m| m.get("itag"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        let muxed_height = muxed
            .and_then(|m| m.get("height"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        return Some(TrailerResolved {
            url: vurl.to_string(),
            itag: video.get("itag").and_then(|v| v.as_i64()).unwrap_or(0),
            height,
            is_dash: true,
            dash: Some(DashInfo {
                video_url: vurl.to_string(),
                audio_url: aurl.to_string(),
                video_itag: video.get("itag").and_then(|v| v.as_i64()).unwrap_or(0),
                audio_itag: audio.get("itag").and_then(|v| v.as_i64()).unwrap_or(140),
                width: video.get("width").and_then(|v| v.as_i64()).unwrap_or(0),
                height,
                fps: video.get("fps").and_then(|v| v.as_i64()).unwrap_or(0),
                video_mime: video
                    .get("mime")
                    .and_then(|v| v.as_str())
                    .unwrap_or("video/mp4")
                    .to_string(),
                audio_mime: audio
                    .get("mime")
                    .and_then(|v| v.as_str())
                    .unwrap_or("audio/mp4")
                    .to_string(),
                video_bitrate: video.get("bitrate").and_then(|v| v.as_i64()).unwrap_or(0),
                audio_bitrate: audio.get("bitrate").and_then(|v| v.as_i64()).unwrap_or(0),
                video_init: range(video_range, "init"),
                video_index: range(video_range, "index"),
                audio_init: range(audio_range, "init"),
                audio_index: range(audio_range, "index"),
                duration_ms,
            }),
            muxed_url,
            muxed_itag,
            muxed_height,
        });
    }
    let url = value.get("url").and_then(|v| v.as_str())?;
    if !(url.starts_with("https://") && url.contains('.')) {
        return None;
    }
    let itag = value.get("itag").and_then(|v| v.as_i64()).unwrap_or(0);
    let height = value.get("height").and_then(|v| v.as_i64()).unwrap_or(0);
    Some(TrailerResolved {
        url: url.to_string(),
        itag,
        height,
        is_dash: false,
        dash: None,
        muxed_url: Some(url.to_string()),
        muxed_itag: itag,
        muxed_height: height,
    })
}

const POS_TTL: Duration = Duration::from_secs(30 * 60);

/// Process-wide decipher pacer: one Node spawn at a time with a 300 ms gap.
/// Browsing bursts trigger N concurrent youtubei POSTs (2 per title) and
/// YouTube throttles the TV IP into `formats=0`; serializing here keeps the
/// decipher reliable without changing the per-ID single-flight.
/// Production-only: the fake resolver in tests bypasses `NodeTrailerResolver`.
fn decipher_gate() -> &'static Mutex<Option<Instant>> {
    static GATE: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    GATE.get_or_init(|| Mutex::new(None))
}
const NEG_TTL: Duration = Duration::from_secs(60);
const MAX_ENTRIES: usize = 256;

/// `youtubeId` → resolved candidate, with the TTL split above. Interior
/// `Mutex` so `Shim` holds one instance without `RwLock` boilerplate.
#[derive(Default)]
pub struct TrailerCache {
    rows: Mutex<HashMap<String, (Instant, Option<TrailerResolved>)>>,
}

impl TrailerCache {
    pub fn new() -> TrailerCache {
        TrailerCache::default()
    }

    pub fn get(&self, youtube_id: &str) -> Option<Option<TrailerResolved>> {
        let g = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        let (at, answer) = g.get(youtube_id)?;
        let ttl = if answer.is_some() { POS_TTL } else { NEG_TTL };
        (at.elapsed() < ttl).then(|| answer.clone())
    }

    pub fn insert(&self, youtube_id: &str, answer: Option<TrailerResolved>) {
        let mut g = self.rows.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() >= MAX_ENTRIES && !g.contains_key(youtube_id) {
            g.clear();
        }
        g.insert(youtube_id.to_string(), (Instant::now(), answer));
    }
}

/// Cached, single-flight resolve: the `meta()` loop shape. Callers re-check
/// the cache after `Flight::claim` returns `None`; a failed fetch releases
/// the key and the waiter retries rather than reading the failure as final.
pub fn resolve_cached(
    resolver: &dyn TrailerResolver,
    cache: &TrailerCache,
    flight: &plx_base::flight::Flight,
    youtube_id: &str,
) -> Option<TrailerResolved> {
    loop {
        if let Some(answer) = cache.get(youtube_id) {
            return answer;
        }
        let gate = format!("trailer\0{youtube_id}");
        let Some(_turn) = flight.claim(&gate) else {
            continue;
        };
        let answer = resolver.resolve(youtube_id);
        cache.insert(youtube_id, answer.clone());
        return answer;
    }
}

/// One persistent Node proxy child (`js-service/trailer-proxy.js`): the DASH
/// pair's `http://127.0.0.1:<port>/track/<id>` BaseURLs. Spawned lazily on the
/// first DASH trailer and kept for the process lifetime (a trailer autoplay
/// re-registers the same pair on every detail/focus prefetch; a per-request or
/// per-track spawn would pay a Node cold start per seek). The child dies with
/// the shim (`PR_SET_PDEATHSIG` from a supervisor thread that lives as long as
/// the proxy — the `boot.rs` P2P shape) or on `TrailerProxy` drop.
///
/// When it cannot start (no Node, missing script, port never announced) every
/// method answers `None` and the caller falls back to the raw googlevideo
/// BaseURLs — degraded autoplay, never an empty shelf.
pub struct TrailerProxy {
    node: String,
    script: PathBuf,
    state: Mutex<ProxyState>,
    cache: Mutex<HashMap<String, String>>,
    next_id: AtomicU64,
    fixed_base: Option<String>,
}

enum ProxyState {
    NotStarted,
    Ready(ProxyInner),
    Failed,
}

struct ProxyInner {
    port: u16,
    pid: i32,
    supervisor: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ProxyInner {
    fn drop(&mut self) {
        // SIGTERM, not SIGKILL: the child has no shutdown hook, but SIGTERM
        // is enough for a backend that just streams. The supervisor is parked
        // in `child.wait()`; killing makes it exit so the join returns.
        #[cfg(unix)]
        unsafe {
            libc::kill(self.pid, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        {}
        if let Some(sup) = self.supervisor.take() {
            let _ = sup.join();
        }
    }
}

impl TrailerProxy {
    pub fn new(node: String, script: PathBuf) -> TrailerProxy {
        TrailerProxy {
            node,
            script,
            state: Mutex::new(ProxyState::NotStarted),
            cache: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            fixed_base: None,
        }
    }

    /// Production paths: the same discovery `NodeTrailerResolver` uses, for
    /// `trailer-proxy.js` beside the shipped decipher script.
    pub fn default_paths() -> TrailerProxy {
        let node = if std::path::Path::new("/usr/bin/node").is_file() {
            "/usr/bin/node".to_string()
        } else {
            "node".to_string()
        };
        let script = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.join("js-service/trailer-proxy.js")))
            .filter(|p| p.is_file())
            .unwrap_or_else(|| PathBuf::from("js-service/trailer-proxy.js"));
        TrailerProxy::new(node, script)
    }

    /// Tests: loopback URLs are synthesised without spawning anything, so a
    /// shim test can prove the manifest/probe carry proxy BaseURLs.
    #[cfg(test)]
    pub fn fixed_base(base: String) -> TrailerProxy {
        TrailerProxy {
            node: String::new(),
            script: PathBuf::new(),
            state: Mutex::new(ProxyState::Failed),
            cache: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            fixed_base: Some(base),
        }
    }

    /// The two proxy track URLs for a DASH pair, or `None` when the proxy is
    /// unavailable (caller then uses the raw googlevideo URLs).
    pub fn dash_urls(&self, d: &DashInfo) -> Option<(String, String)> {
        let video = self.track_url(&d.video_url)?;
        let audio = self.track_url(&d.audio_url)?;
        Some((video, audio))
    }

    fn track_url(&self, google_url: &str) -> Option<String> {
        {
            let g = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(p) = g.get(google_url) {
                return Some(p.clone());
            }
        }
        let proxy = if let Some(base) = &self.fixed_base {
            let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
            format!("{base}/track/{id}")
        } else {
            let port = self.ensure_port()?;
            let id = post_register(port, google_url, "ios")?;
            plx_base::eventlog::log(&format!("trailer-proxy: registered track id={id} ua=ios"));
            format!("http://127.0.0.1:{port}/track/{id}")
        };
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(google_url.to_string(), proxy.clone());
        Some(proxy)
    }

    /// The live port, spawning on first use. A spawn failure is sticky
    /// (`Failed`): retrying every trailer would re-log the same missing binary.
    fn ensure_port(&self) -> Option<u16> {
        let mut g = self.state.lock().unwrap_or_else(|e| e.into_inner());
        match &*g {
            ProxyState::Ready(inner) => return Some(inner.port),
            ProxyState::Failed => return None,
            ProxyState::NotStarted => {}
        }
        let (tx, rx) = mpsc::channel();
        let node = self.node.clone();
        let script = self.script.clone();
        let supervisor = std::thread::spawn(move || {
            let mut child = match spawn_proxy_child(&node, &script) {
                Ok(child) => child,
                Err(reason) => {
                    let _ = tx.send(Err(reason));
                    return;
                }
            };
            let pid = child.id() as i32;
            let Some(out) = child.stdout.take() else {
                let _ = tx.send(Err("no stdout pipe".to_string()));
                let _ = child.kill();
                let _ = child.wait();
                return;
            };
            let mut first_line = String::new();
            let mut reader = std::io::BufReader::new(out);
            let read = (&mut reader).take(4096).read_line(&mut first_line);
            match read.and_then(|_| {
                if first_line.is_empty() {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "child exited before port line",
                    ))
                } else {
                    Ok(())
                }
            }) {
                Ok(()) => match parse_port_line(&first_line) {
                    Ok((port, version)) => {
                        let _ = tx.send(Ok((port, pid, version)));
                        let _ = child.wait();
                    }
                    Err(reason) => {
                        let _ = tx.send(Err(reason));
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                },
                Err(e) => {
                    let _ = tx.send(Err(format!("read port line: {e}")));
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok((port, pid, version))) => {
                plx_base::eventlog::log(&format!(
                    "trailer-proxy: spawned node {version} pid={pid} port={port} (floor v16.20.2)"
                ));
                *g = ProxyState::Ready(ProxyInner {
                    port,
                    pid,
                    supervisor: Some(supervisor),
                });
                Some(port)
            }
            Ok(Err(reason)) => {
                plx_base::eventlog::log(&format!(
                    "trailer-proxy: unavailable ({reason}) — DASH falls back to direct googlevideo"
                ));
                // The supervisor already exited on Err; join so its pid is reaped.
                let _ = supervisor.join();
                *g = ProxyState::Failed;
                None
            }
            Err(_) => {
                plx_base::eventlog::log(
                    "trailer-proxy: unavailable (port line timeout) — DASH falls back to direct googlevideo",
                );
                // Do not join: the supervisor is still parked on the child's
                // stdout. Detached, it finishes when the child exits; the
                // pdeathsig takes the child with the process.
                std::mem::forget(supervisor);
                *g = ProxyState::Failed;
                None
            }
        }
    }
}

/// Spawn the proxy child. The supervisor thread reads its first stdout line
/// (the port announcement). Logs nothing itself — `ensure_port` logs the
/// verdict once.
fn spawn_proxy_child(node: &str, script: &PathBuf) -> Result<Child, String> {
    if !script.is_file() {
        return Err(format!("script missing {}", script.display()));
    }
    let mut cmd = Command::new(node);
    cmd.arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("LD_PRELOAD");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let expected_ppid = std::process::id() as libc::pid_t;
        unsafe {
            cmd.pre_exec(move || {
                if libc::getppid() != expected_ppid {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "parent died",
                    ));
                }
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn {node} {}: {e}", script.display()))?;
    plx_base::eventlog::log(&format!(
        "trailer-proxy: spawning (node={node} script={})",
        script.display()
    ));
    Ok(child)
}

/// `{"port":N,"node":"vX.Y.Z"}` → (port, version for the floor log).
fn parse_port_line(line: &str) -> Result<(u16, String), String> {
    let value: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|_| "bad port line".to_string())?;
    let port = value
        .get("port")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "bad port line".to_string())?;
    let port = u16::try_from(port).map_err(|_| "bad port line".to_string())?;
    if port == 0 {
        return Err("bad port line".to_string());
    }
    let version = value
        .get("node")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Ok((port, version))
}

/// `POST /register {"u":…,"ua":"ios"}` → track id. Plain `TcpStream` on
/// purpose: the whole request is loopback, so `plx_net`'s TLS ladder would be
/// dead weight. The product User-Agent is still sent, like every other
/// hand-built request. The registered URL is never logged.
fn post_register(port: u16, google_url: &str, ua: &str) -> Option<u64> {
    let body = serde_json::json!({"u": google_url, "ua": ua}).to_string();
    let request = format!(
        "POST /register HTTP/1.1\r\nHost: 127.0.0.1\r\n{}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        plx_net::net::user_agent_header_line(),
        body.len(),
        body
    );
    let addr: std::net::SocketAddr = ([127, 0, 0, 1], port).into();
    let mut stream = std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    let mut response: Vec<u8> = Vec::new();
    stream.take(64 * 1024).read_to_end(&mut response).ok()?;
    let text = String::from_utf8_lossy(&response);
    let (_head, body) = text.split_once("\r\n\r\n")?;
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return None;
    }
    value.get("id").and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved() -> TrailerResolved {
        TrailerResolved {
            url: "https://cdn.example/trailer.mp4?token=abc".into(),
            itag: 22,
            height: 720,
            is_dash: false,
            dash: None,
            muxed_url: Some("https://cdn.example/trailer.mp4?token=abc".into()),
            muxed_itag: 22,
            muxed_height: 720,
        }
    }

    #[test]
    fn cache_hits_positive_and_negative_within_their_ttl() {
        let cache = TrailerCache::new();
        cache.insert("a", Some(resolved()));
        cache.insert("b", None);
        assert_eq!(cache.get("a"), Some(Some(resolved())));
        assert_eq!(cache.get("b"), Some(None));
        assert_eq!(cache.get("missing"), None);
    }

    #[test]
    fn resolve_cached_shares_one_spawn() {
        struct SlowResolver {
            calls: std::sync::atomic::AtomicUsize,
        }
        impl TrailerResolver for SlowResolver {
            fn resolve(&self, _youtube_id: &str) -> Option<TrailerResolved> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(80));
                Some(resolved())
            }
        }
        let resolver = SlowResolver {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let cache = TrailerCache::new();
        let flight = plx_base::flight::Flight::default();
        std::thread::scope(|scope| {
            for _ in 0..2 {
                let (resolver, cache, flight) = (&resolver, &cache, &flight);
                scope.spawn(move || {
                    assert_eq!(
                        resolve_cached(resolver, cache, flight, "youtube123"),
                        Some(resolved())
                    );
                });
            }
        });
        assert_eq!(resolver.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// YouTube refuses age-restricted trailers to every anonymous client, so a
    /// `login_required` answer is permanent for the process (six hours) and
    /// must not cost a Node spawn on every later open.
    #[test]
    fn an_age_gated_id_is_remembered_as_blocked() {
        let resolver = NodeTrailerResolver::new(
            "node".into(),
            std::path::PathBuf::from("js-service/trailer-decipher.js"),
        );
        assert!(!resolver.blocked("BBBBBBBBBBB"));
        resolver.block("BBBBBBBBBBB");
        assert!(resolver.blocked("BBBBBBBBBBB"));
        assert!(!resolver.blocked("CCCCCCCCCCC"));
    }

    #[test]
    fn parse_accepts_only_ok_https_lines() {
        let want = resolved();
        assert_eq!(
            parse_resolved(
                r#"{"ok":true,"url":"https://cdn.example/trailer.mp4?token=abc","itag":22,"height":720}"#
            ),
            Some(want)
        );
        assert_eq!(parse_resolved(r#"{"ok":false,"error":"stub"}"#), None);
        assert_eq!(parse_resolved("not json"), None);
        assert_eq!(
            parse_resolved(r#"{"ok":true,"url":"/library/parts/1/0/file.mp4"}"#),
            None
        );
    }

    #[test]
    fn parse_dash_pair() {
        let text = r#"{"ok":true,"kind":"dash","video":{"url":"https://example.com/v?sig=abc&exp=1","itag":137,"width":1920,"height":1080,"fps":24,"mime":"video/mp4; codecs=\"avc1.640028\"","bitrate":4000000,"range":{"init":{"start":0,"end":740},"index":{"start":741,"end":2000},"contentLength":42834313}},"audio":{"url":"https://example.com/a?sig=def&exp=1","itag":140,"mime":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":128000,"range":{"init":{"start":0,"end":600},"index":{"start":601,"end":1500},"contentLength":1000000}},"durationSec":145,"muxed":{"url":"https://example.com/m?sig=xyz","itag":18,"height":360}}"#;
        let r = parse_resolved(text).expect("a dash pair parses");
        assert!(r.is_dash);
        assert_eq!(r.height, 1080);
        let d = r.dash.as_ref().unwrap();
        assert_eq!(d.video_itag, 137);
        assert_eq!(d.audio_itag, 140);
        assert_eq!(d.video_init, (0, 740));
        assert_eq!(d.video_index, (741, 2000));
        assert_eq!(d.duration_ms, 145_000);
        assert_eq!(
            r.muxed_url.as_deref(),
            Some("https://example.com/m?sig=xyz")
        );
        assert_eq!(r.muxed_height, 360);
    }

    #[test]
    fn extract_error_reads_short_reason_without_url() {
        assert_eq!(
            extract_error(r#"{"ok":false,"error":"http 403"}"#).as_deref(),
            Some("http 403")
        );
        assert_eq!(
            extract_error(r#"{"ok":false,"error":"no muxed mp4 (formats=0)"}"#).as_deref(),
            Some("no muxed mp4 (formats=0)")
        );
        // A success body must not leak its signed URL through the error path.
        assert_eq!(
            extract_error(r#"{"ok":true,"url":"https://cdn.example/trailer.mp4?token=abc"}"#),
            None
        );
        assert_eq!(extract_error("not json"), None);
    }

    #[test]
    fn log_excerpt_collapses_and_truncates() {
        assert_eq!(log_excerpt("  http   403\n"), "http 403");
        let long = "a".repeat(500);
        assert!(log_excerpt(&long).len() <= 310);
    }

    fn dash_info() -> DashInfo {
        DashInfo {
            video_url: "https://rr1.googlevideo.com/videoplayback?sig=a".into(),
            audio_url: "https://rr1.googlevideo.com/videoplayback?sig=c".into(),
            video_itag: 137,
            audio_itag: 140,
            width: 1920,
            height: 1080,
            fps: 24,
            video_mime: "video/mp4".into(),
            audio_mime: "audio/mp4".into(),
            video_bitrate: 5_000_000,
            audio_bitrate: 128_000,
            video_init: (0, 999),
            video_index: (1000, 2000),
            audio_init: (2001, 2999),
            audio_index: (3000, 4000),
            duration_ms: 145_000,
        }
    }

    #[test]
    fn proxy_fixed_base_synthesizes_stable_loopback_urls() {
        let proxy = TrailerProxy::fixed_base("http://127.0.0.1:18080".into());
        let (video, audio) = proxy.dash_urls(&dash_info()).expect("fixed proxy urls");
        assert!(
            video.starts_with("http://127.0.0.1:18080/track/"),
            "{video}"
        );
        assert!(
            audio.starts_with("http://127.0.0.1:18080/track/"),
            "{audio}"
        );
        assert_ne!(
            video, audio,
            "each track has its own id (ProbeCache key + Range)"
        );
        // The same signed URL reuses its id, so the probe cache key is stable.
        let (video2, audio2) = proxy.dash_urls(&dash_info()).expect("cached proxy urls");
        assert_eq!((video, audio), (video2, audio2));
    }

    #[test]
    fn parse_port_line_only_accepts_valid_ports() {
        assert_eq!(
            parse_port_line(r#"{"port":18765,"node":"v16.20.2"}"#).map(|(p, _)| p),
            Ok(18765)
        );
        assert!(parse_port_line(r#"{"port":0,"node":"v16.20.2"}"#).is_err());
        assert!(parse_port_line(r#"{"port":70000}"#).is_err());
        assert!(parse_port_line("not json").is_err());
    }
}
