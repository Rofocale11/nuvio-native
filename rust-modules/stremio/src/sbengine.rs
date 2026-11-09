//! Nuvio Engine (libsbengine) FFI — the in-process C torrent engine.
//!
//! The engine exposes a single exported symbol, `sbe_get()`, returning a table of
//! function pointers (`sbe_api` in `vendor/engine-sources/core/sbengine.h`). This
//! module resolves that one symbol through the repo's `dynlib!` seam and dereferences
//! the table, rather than declaring a dozen top-level `sbe_*` entries one per call.
//! `dynlib!` refuses the table for it: every name added can empty the whole table on
//! a firmware where only a subset exists, and the engine's own version gate
//! (`api() == SBE_API_VERSION`) is the real compatibility decision.
//!
//! # Firmware posture
//!
//! `libsbengine.so` links libstdc++/libgcc AND OpenSSL 1.1.1 statically: the TV's
//! libstdc++ lacks three symbols the NDK's emits, and the firmware's `libssl` is
//! `libssl.so.1.0.x` (webOS 1.2–6.4), `libssl.so.1.1` (7.4/8.3) or `libssl.so.3`
//! (9.2 and later) depending on release. With both linked in, `tools/fwcompat.py`
//! grades one `.so` OK on every release in its inventory (1.2 to 11.2, current sets
//! included). Where `dlopen` still fails (a firmware we have never graded, a missing
//! file) load reports `NoLibrary` and torrent playback stays on the node child.
//!
//! # Build
//!
//! `libsbengine.so` is built by `ci/build-sbengine.sh` (Nuvio
//! Engine 0.1.5 = `99a1860`, libtorrent 2.0.12, Boost 1.86 headers,
//! OpenSSL 1.1.1w from `ci/build-openssl.sh`, NDK `arm-webos-linux-gnueabi-g++` 12.2.0) and staged by `make ipk`/`make deploy`.
//! The `.so` sits beside the binary in `paths::app_dir()`; the candidate list is
//! deliberately one name, because a SONAME search would find the wrong library on
//! webOS 11 (which ships its own FFmpeg 6) — the absolute app-dir path makes the
//! choice structural (`dynlib::Handle::open_in`).
//!
//! # Lifecycle and threading
//!
//! The engine owns threads (session I/O, event loop) and its `start`/`stop` are
//! safe to call from Rust but hold the engine's internal mutex and may block for
//! session teardown. Call `start_engine` / `stop_engine` only from boot/teardown
//! code, never from a per-frame poll or the shim's resolve path. Torrents are
//! opened and polled with `open` / `files` / `stream` from any thread; `error` and
//! `stats` are likewise mutex-guarded in the C++ shim.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::ptr::null_mut;
use std::sync::OnceLock;

/// The engine's own compatibility gate (`SBE_API_VERSION` in sbengine.h). 4 appends the playing
/// stream's counters (`scheduled_pieces`, `active_demands`, `delivered`) to `sbe_stats`, which is
/// what lets the transfer log tell "downloading for the player" from "downloading for itself".
pub const API_VERSION: c_int = 4;

/// Bytes of payload kept around the playhead (the streaming window).
///
/// **This is RAM on the television, not storage.** The window lives under
/// `runtime_dir()/serverp2p/cache`, and the runtime root is a RAM-backed tmpfs on the set
/// (`docs/ui-restructure-spec-v4.md` §5.3, settled on the device 2026-09-06). It is read with
/// `pread`, so it lands in shmem and never appears in the process's `VmRSS` — which is exactly
/// why it was easy to forget it while sizing: a set at `MemAvailable` ~80 MB pays for it as
/// surely as for a heap allocation. 32 MiB is ~10 s of lookahead at the ~24 Mbit/s a real
/// torrent measured on this set.
pub const DEFAULT_WINDOW_BYTES: u64 = 32 * 1024 * 1024;
/// Upload budget, bytes/second (soft, against sharing, not against playback).
pub const DEFAULT_UPLOAD_BPS: u64 = 2 * 1024 * 1024;
/// Verified pieces kept in RAM, swept by the engine when memory tightens.
///
/// A second copy of bytes the window file already holds, kept only to make a re-read fast. With
/// the window itself in RAM that is a duplicate, so it stays small: it saturates at its ceiling
/// in practice (`sbengine: … mem=` in the event log shows it), and 8 MiB is enough to cover the
/// re-reads around the playhead.
pub const DEFAULT_MEM_CACHE_BYTES: u64 = 8 * 1024 * 1024;
/// Segment size for FAT32 storage that cannot punch holes; 0 = per-torrent files.
pub const DEFAULT_SEGMENT_BYTES: u64 = 0;

/// One file of a torrent, laid out exactly like `sbe_file` in sbengine.h: a 512-byte
/// path followed by a 64-bit size. `repr(C)` and fields in declaration order — this
/// is the ABI, not a convenience struct.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EngineFile {
    pub path: [c_char; 512],
    pub size: u64,
}

impl Default for EngineFile {
    fn default() -> Self { Self { path: [0; 512], size: 0 } }
}

impl EngineFile {
    pub fn path_string(&self) -> String {
        let mut len = 0usize;
        while len < self.path.len() && self.path[len] != 0 {
            len += 1;
        }
        let bytes: Vec<u8> = self.path[..len].iter().map(|c| *c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// The statistics block the C++ shim copies out (`sbe_stats`). Same ABI discipline
/// as `EngineFile`: repr(C), declaration order, no padding assumptions beyond C.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineStats {
    pub peers: u32,
    pub seeds: u32,
    pub known: u32,
    pub connecting: u32,
    pub down_bps: u64,
    pub up_bps: u64,
    pub downloaded: u64,
    pub disk_used: u64,
    pub disk_capacity: u64,
    pub window: u64,
    pub resident: u64,
    pub freed: u64,
    pub refetched: u64,
    pub mem_cache_used: u64,
    pub window_unsupported: u32,
    /// Pieces the playing stream's current schedule asks libtorrent for (0 with no stream).
    pub scheduled: u32,
    /// The player's HTTP reads in flight for that stream.
    pub active_demands: u32,
    /// Bytes the engine has served to the player since the stream opened.
    pub delivered: u64,
    /// The torrent's piece length: the unit the window evicts and the schedule requests.
    pub piece_bytes: u64,
    /// File-relative offset of the newest player read (the live playhead the schedule follows).
    pub primary_demand_start: u64,
    /// A second live read of the same file, when one exists.
    pub secondary_demand_start: u64,
}

// The function-pointer table `sbe_get` returns. `#[repr(C)]` and the field order
// must mirror `sbe_api` in sbengine.h exactly; a mismatch here is silent UB.
#[repr(C)]
struct EngineApi {
    api: unsafe extern "C" fn() -> c_int,
    version: unsafe extern "C" fn() -> *const c_char,
    start: unsafe extern "C" fn(*const c_char, *const c_char, u64, u64, u64, u64) -> c_int,
    stop: unsafe extern "C" fn(),
    open: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int,
    files: unsafe extern "C" fn(*const c_char, *mut EngineFile, c_int) -> c_int,
    stream: unsafe extern "C" fn(*const c_char, u32, *mut c_char, c_int) -> c_int,
    error: unsafe extern "C" fn(*const c_char) -> *const c_char,
    stats: unsafe extern "C" fn(*mut EngineStats) -> c_int,
    limits: unsafe extern "C" fn(u64, u64),
    remove: unsafe extern "C" fn(*const c_char),
    remove_all: unsafe extern "C" fn(),
    window: unsafe extern "C" fn(u64),
    setting: unsafe extern "C" fn(*const c_char, c_int) -> c_int,
    relocate: unsafe extern "C" fn(*const c_char, u64) -> c_int,
    restored: unsafe extern "C" fn(*const c_char) -> c_int,
}

plx_base::dynlib! {
    /// ONE symbol. See this module's doc before adding a second: every name added
    /// here is a name that can empty the table on some firmware, and the cost of
    /// an empty table is that torrent playback goes offline rather than failing.
    sbengine: ["libsbengine.so", "libsbengine.3.dylib"] {
        fn sbe_get() -> *const EngineApi;
    }
}

// The engine handle has no `Drop` and is deliberately never released: the handle
// stays mapped for the life of the process, and `dlclose` on a library with live
// engine threads inside it would unmap code that is executing.
struct ApiPtr(*const EngineApi);
unsafe impl Sync for ApiPtr {}
unsafe impl Send for ApiPtr {}
static API: OnceLock<Option<ApiPtr>> = OnceLock::new();

fn api() -> Option<&'static EngineApi> {
    let slot = API.get_or_init(|| {
        let dir = plx_base::paths::app_dir().to_path_buf();
        match sbengine::load(Some(&dir)) {
            plx_base::dynlib::Loaded::Ok(soname) => {
                let p = unsafe { sbe_get() };
                if p.is_null() {
                    plx_base::eventlog::log(&format!(
                        "sbengine: sbe_get() returned null from {soname}"
                    ));
                    return None;
                }
                let a = unsafe { &*p };
                let got = unsafe { (a.api)() };
                if got != API_VERSION {
                    plx_base::eventlog::log(&format!(
                        "sbengine: API version {got}, this build wants {API_VERSION}"
                    ));
                    return None;
                }
                Some(ApiPtr(p))
            }
            plx_base::dynlib::Loaded::NoLibrary => {
                plx_base::eventlog::log(
                    "sbengine: libsbengine.so missing or not loadable (see fwcompat)",
                );
                None
            }
            plx_base::dynlib::Loaded::Incomplete(soname, n) => {
                plx_base::eventlog::log(&format!(
                    "sbengine: {soname} missing {n} symbols"
                ));
                None
            }
        }
    });
    slot.as_ref().map(|p| unsafe { &*p.0 })
}

/// Is the engine library present and the table verified? Latches the load once.
pub fn available() -> bool {
    api().is_some()
}

/// The engine's version string, e.g. `"0.1.5 / libtorrent 2.0.12"`. Empty when the
/// engine is not bound; callers that need a hard signal should check `available()`.
pub fn version() -> String {
    match api() {
        Some(a) => {
            let p = unsafe { (a.version)() };
            if p.is_null() {
                return String::new();
            }
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
        None => String::new(),
    }
}

/// Start the engine. `state_dir` holds the engine's session state; `cache_dir`
/// holds the streaming-window payload. Both must be real, non-empty, pre-created
/// directories: `nuvio_engine_create` returns `INVALID_ARGUMENT` for an empty or
/// null directory (`vendor/engine-sources/nuvio-engine/src/core/engine.cpp`), so
/// callers must not pass `""`.
pub fn start_engine(state_dir: &str, cache_dir: &str) -> bool {
    let Some(a) = api() else { return false };
    let c_state = CString::new(state_dir).unwrap_or_else(|_| CString::new("").unwrap());
    let c_cache = CString::new(cache_dir).unwrap_or_else(|_| CString::new("").unwrap());
    unsafe {
        (a.start)(
            c_state.as_ptr(),
            c_cache.as_ptr(),
            DEFAULT_WINDOW_BYTES,
            DEFAULT_UPLOAD_BPS,
            DEFAULT_MEM_CACHE_BYTES,
            DEFAULT_SEGMENT_BYTES,
        ) == 0
    }
}

/// Stop the engine and join its threads. Idempotent; also runs at process exit
/// inside the C++ shim (`StopAtExit`), so a skipped `stop` never aborts.
pub fn stop_engine() {
    if let Some(a) = api() {
        unsafe { (a.stop)() };
    }
}

/// Add a torrent once (0 == ok / already added). `magnet` is the magnet URI and
/// `ih` the 40-character lowercase info hash.
pub fn open(magnet: &str, ih: &str) -> bool {
    let Some(a) = api() else { return false };
    let c_magnet = CString::new(magnet).unwrap();
    let c_ih = CString::new(ih).unwrap();
    unsafe { (a.open)(c_magnet.as_ptr(), c_ih.as_ptr()) == 0 }
}

/// Open `ih` so the engine restores its kept swarm state (metadata, trackers,
/// saved peer addresses) and dials those peers, WITHOUT preparing a stream: no
/// file priorities, no reads, no payload. The play that follows finds the
/// handle already open and the swarm connected, so it behaves like a seek on a
/// live torrent instead of paying the connection ramp.
///
/// The info hash is enough — the `.resume` file carries everything else. No
/// resume file means the add is cold (metadata from DHT/trackers), which is
/// still the play's own work, just started earlier. Best effort: false when the
/// engine is not bound or the add was refused.
pub fn warm(ih: &str) -> bool {
    let Some(a) = api() else { return false };
    let magnet = format!("magnet:?xt=urn:btih:{ih}");
    let c_magnet = CString::new(magnet).unwrap();
    let c_ih = CString::new(ih).unwrap();
    unsafe { (a.open)(c_magnet.as_ptr(), c_ih.as_ptr()) == 0 }
}

/// The torrent's files once metadata is ready. Returns `None` if the engine is
/// not bound, `Some(Err(msg))` for a failed torrent, and `Some(Ok(files))`
/// (possibly empty while metadata is still in flight) otherwise.
pub fn files(ih: &str) -> Option<Result<Vec<EngineFile>, String>> {
    let a = api()?;
    let c_ih = CString::new(ih).unwrap();
    let mut buf = vec![EngineFile::default(); 256];
    let n = unsafe { (a.files)(c_ih.as_ptr(), buf.as_mut_ptr(), buf.len() as c_int) };
    match n {
        x if x >= 0 => {
            buf.truncate(x as usize);
            Some(Ok(buf))
        }
        -1 => Some(Ok(Vec::new())),
        _ => Some(Err(error(ih))),
    }
}

/// Ask for the stream URL of file `idx`. Returns 1 when `url` is filled, 0 while
/// preparing, -1 on failure. The C++ side `snprintf`s into `cap` bytes; on `1` the
/// vec holds the NUL-terminated URL — call [`url_trim`] (or read up to the first
/// `0`) to get the address.
pub fn stream(ih: &str, idx: u32, url: &mut Vec<u8>) -> Option<c_int> {
    let a = api()?;
    let c_ih = CString::new(ih).unwrap();
    url.resize(4096, 0);
    let rv = unsafe { (a.stream)(c_ih.as_ptr(), idx, url.as_mut_ptr() as *mut c_char, url.len() as c_int) };
    Some(rv)
}

/// Trim a `stream()` copy-out buffer at the first NUL.
pub fn url_trim(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Poll for a ready stream URL: `open` once, then `files` until metadata, then
/// pick the file, then `stream` until `1`, then a short prebuffer wait until the
/// engine has data to serve (so the probe does not outrun the torrent).
/// Bounded (default 30 s); empty-polling sleeps 100 ms. Returns the engine-served
/// loopback URL, or an error string.
///
/// File pick: `stream.file_idx` wins; else the largest file (the usual season-pack
/// heuristic — callers with `(season, episode)` should pre-pick via `torrent.rs`);
/// else the first file.
pub fn resolve_stream(magnet: &str, ih: &str, preferred_idx: Option<u32>, timeout_ms: u64) -> Result<String, String> {
    resume_foreground();
    if !open(magnet, ih) {
        return Err(error(ih));
    }
    resolve_open(ih, preferred_idx, timeout_ms)
}

/// The torrent the shim last resolved. The engine keeps every opened torrent alive, still
/// downloading and still holding its own window, until `remove`; without this a source change
/// left the old torrent competing with the new one for the same bandwidth and RAM.
static ACTIVE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The info hash of the torrent the engine is serving now, or `None` when nothing has been
/// resolved yet. A URL the engine returned names one of its streams, and `remember` deletes the
/// previous torrent when the source changes, which invalidates that URL (`/stream/<id>` then
/// answers 404 for the rest of the session). Callers that cache an engine URL must compare it
/// against this before reusing it.
pub fn active_hash() -> Option<String> {
    ACTIVE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Remember `ih` as the playing torrent and drop the previous one. Same hash is a no-op (a retry
/// of the current source must not tear its stream down).
fn remember(ih: &str) {
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    if active.as_deref() == Some(ih) {
        return;
    }
    if let Some(previous) = active.replace(ih.to_string()) {
        remove(&previous);
        plx_base::eventlog::log(&format!(
            "sbengine: released previous torrent {}",
            &previous[..previous.len().min(12)]
        ));
    }
}

/// Whether `ih` resolved from a kept resume file (metadata restored, peer
/// list remembered) instead of a cold fetch. A restored swarm is already
/// known, so the prebuffer gate must not fire for it — it only earns its
/// keep for a cold start. `None` when the engine is not bound, told apart
/// from "definitely cold" by the caller.
pub fn is_restored(ih: &str) -> Option<bool> {
    let a = api()?;
    let c_ih = CString::new(ih).unwrap();
    Some(unsafe { (a.restored)(c_ih.as_ptr()) } != 0)
}

/// Whether `ih` resolved from a kept resume file (see [`is_restored`]).
fn was_restored(ih: &str) -> bool {
    is_restored(ih).unwrap_or(false)
}

/// [`resolve_stream`] once the magnet is already open: wait for metadata (30 s bound by the
/// caller), pick the file, then wait for the engine's stream URL. Split out so a re-resolve of an
/// already-open torrent does not re-add it.
///
/// The URL is returned after a short prebuffer wait (see [`wait_for_prebuffer`]): the engine
/// prepares a stream URL as soon as priorities are set, before any piece has landed, and a URL
/// with zero bytes in it made the probe fail before data could arrive. The wait is only enough
/// for the header now — the engine's reads block until pieces are verified, so the player's own
/// buffering can start while the rest of the lookahead is still in flight.
fn resolve_open(ih: &str, preferred_idx: Option<u32>, timeout_ms: u64) -> Result<String, String> {
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_millis(timeout_ms);
    // Evict the previous torrent BEFORE taking the baseline. `EngineStats::downloaded`
    // is engine-wide, so while the old torrent is still live its bytes keep arriving
    // during the prebuffer wait and fill the delta with stale bytes: the gate
    // passed instantly, the URL was served with zero fresh pieces of the new torrent,
    // the probe failed (fps=14/3/2) and `playback retry` saved it. Removing first
    // stops the old swarm competing and makes the delta count only this torrent.
    // A same-hash retry must not tear its own stream down (remember() is a no-op
    // to itself) and must not wait for the gate again — it already has what it needs.
    let retry_same = active_hash().as_deref() == Some(ih);
    remember(ih);
    let baseline = stats().map(|s| s.downloaded).unwrap_or(0);
    let files = loop {
        match files(ih) {
            Some(Ok(f)) if !f.is_empty() => break f,
            Some(Ok(_)) => {}
            Some(Err(e)) => return Err(e),
            None => return Err(error(ih)),
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("sbengine: metadata timeout for {ih}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let metadata_wait = started.elapsed();
    let idx = match preferred_idx {
        Some(i) if (i as usize) < files.len() => i,
        _ => files
            .iter()
            .enumerate()
            .max_by_key(|(_, f)| f.size)
            .map(|(i, _)| i as u32)
            .unwrap_or(0),
    };
    let mut buf = vec![0u8; 4096];
    loop {
        match stream(ih, idx, &mut buf) {
            Some(1) => {
                let url = url_trim(&buf);
                let url_wait = started.elapsed();
                let restored = was_restored(ih);
                let prebuffer = if retry_same || restored {
                    PrebufferOutcome::Skipped
                } else {
                    wait_for_prebuffer(baseline, deadline)
                };
                if restored {
                    plx_base::eventlog::log(&format!(
                        "sbengine: {} resumed from kept swarm state (metadata + peers restored); prebuffer gate skipped",
                        &ih[..ih.len().min(12)]
                    ));
                }
                // One line per opened stream: without the split, a slow start on the TV cannot be
                // attributed to peer discovery, our prebuffer gate, or the caller's deadline.
                plx_base::eventlog::log(&format!(
                    "sbengine: {} metadata {}ms ({} files, index {}) -> url {}ms -> prebuffer {} -> resolve {}ms",
                    &ih[..ih.len().min(12)],
                    metadata_wait.as_millis(),
                    files.len(),
                    idx,
                    url_wait.as_millis(),
                    prebuffer.describe(),
                    started.elapsed().as_millis(),
                ));
                return Ok(url);
            }
            Some(0) => {}
            Some(_) => return Err(error(ih)),
            None => return Err(error(ih)),
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("sbengine: stream timeout for {ih}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The last error for a torrent (or the engine-wide last error).
pub fn error(ih: &str) -> String {
    match api() {
        Some(a) => {
            let c_ih = CString::new(ih).unwrap();
            let p = unsafe { (a.error)(c_ih.as_ptr()) };
            if p.is_null() {
                let p2 = unsafe { (a.error)(null_mut()) };
                if p2.is_null() {
                    return String::new();
                }
                return unsafe { CStr::from_ptr(p2) }.to_string_lossy().into_owned();
            }
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
        None => String::new(),
    }
}

/// How much the engine must have downloaded before a stream URL is handed to
/// the player: enough for the container header plus the first units the probe
/// reads. It was 8 MiB — a 3-5 s head start at 20-30 Mbit/s, sized when the
/// shim's decision was cut off at 15 s and a zero-byte URL made the probe fail
/// before any data landed. Neither holds now: the decision waits 120 s, and the
/// engine's HTTP reads block until the requested pieces are verified, so the
/// player's own buffering can begin far earlier. 1 MiB covers the header and a
/// keyframe; the engine's piece schedule (8 MiB critical, 32 MiB playback, up
/// to 256 MiB readahead) keeps fetching ahead of the playhead while the player
/// reads. The exact byte count is driven by [`EngineStats::downloaded`], which
/// the engine already reports.
///
/// The number is read as a DELTA from a baseline taken after the previous torrent
/// has been evicted (`remember`) — see [`wait_for_prebuffer`] — because
/// `EngineStats.downloaded` is engine-wide, not per-torrent. A second title opened
/// after the first already has dozens of MB counted, so gating on the absolute
/// value returned immediately with zero fresh bytes for the new torrent (the
/// `fb0cdf43dcf8` first-try failure). A same-hash retry skips the wait entirely.
const PREBUFFER_BYTES: u64 = 1024 * 1024;
/// How long the prebuffer wait may hold the URL before serving anyway. A slow
/// swarm must still play — the wait is advisory, never a gate on playback: if
/// no bytes have arrived inside this window the URL is returned anyway (the
/// player's own buffering and the `playback retry` path take over, exactly as
/// before this change). 10 s, inside the caller's 30 s resolve deadline.
const PREBUFFER_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// What [`wait_for_prebuffer`] observed, so the one startup line can say where
/// a slow resolve spent its time.
enum PrebufferOutcome {
    Reached { bytes: u64, elapsed: std::time::Duration },
    Capped { bytes: u64, elapsed: std::time::Duration },
    Skipped,
    Unavailable,
}

impl PrebufferOutcome {
    fn describe(&self) -> String {
        match self {
            Self::Reached { bytes, elapsed } => {
                format!("{}KiB in {}ms", bytes / 1024, elapsed.as_millis())
            }
            Self::Capped { bytes, elapsed } => {
                format!("cap after {}ms, only {}KiB", elapsed.as_millis(), bytes / 1024)
            }
            Self::Skipped => "skipped (same torrent)".to_string(),
            Self::Unavailable => "no stats".to_string(),
        }
    }
}

/// Wait until the engine has prebuffered [`PREBUFFER_BYTES`] MORE than it had
/// at `baseline` (the `downloaded` counter at resolve start), or the bounded
/// wait runs out. Runs on the shim's resolve thread, never the UI thread
/// (assert_may_block at the call site), and polls `stats()` every 100 ms —
/// the same cadence as the metadata/stream loops above. Returns immediately
/// when `stats()` is unavailable.
fn wait_for_prebuffer(baseline: u64, deadline: std::time::Instant) -> PrebufferOutcome {
    let started = std::time::Instant::now();
    let cap = deadline.min(started + PREBUFFER_MAX_WAIT);
    loop {
        let Some(s) = stats() else {
            return PrebufferOutcome::Unavailable;
        };
        let bytes = s.downloaded.saturating_sub(baseline);
        if bytes >= PREBUFFER_BYTES {
            return PrebufferOutcome::Reached {
                bytes,
                elapsed: started.elapsed(),
            };
        }
        if std::time::Instant::now() >= cap {
            return PrebufferOutcome::Capped {
                bytes,
                elapsed: started.elapsed(),
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// Copy the current transfer numbers out, or `None` when the engine is not bound.
pub fn stats() -> Option<EngineStats> {
    let a = api()?;
    let mut out = EngineStats::default();
    let rc = unsafe { (a.stats)(&mut out as *mut _) };
    (rc == 0).then_some(out)
}

/// Remove one torrent and its payload.
pub fn remove(ih: &str) {
    if let Some(a) = api() {
        let c_ih = CString::new(ih).unwrap();
        unsafe { (a.remove)(c_ih.as_ptr()) };
    }
}

/// Remove every torrent and its payload.
pub fn remove_all() {
    if let Some(a) = api() {
        unsafe { (a.remove_all)() };
    }
}

/// Set the download/upload ceilings (bytes/second); 0 == unlimited.
pub fn limits(down_bps: u64, up_bps: u64) {
    if let Some(a) = api() {
        unsafe { (a.limits)(down_bps, up_bps) };
    }
}

/// Stop the engine when the player leaves: remove every torrent (the window
/// payload goes with it) and forget the active hash.
///
/// A cap was the old answer, but a torrent left downloading at 64 KiB/s still
/// stuttered Home and the sources screen. Removal is cheap and the `.resume`
/// file survives, so the next play restores metadata and peers instead of a
/// cold fetch. Clearing [`active_hash`] is what makes that safe: the shim's
/// `cached_open_url` drops a cached engine URL when its torrent is no longer
/// active, so a replay resolves again rather than opening a dead
/// `/stream/<id>` (the 404 retry loop).
pub fn stop_background() {
    if available() {
        remove_all();
        *ACTIVE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        plx_base::eventlog::log("sbengine: engine stopped (player left)");
    }
}

/// Restore foreground (unlimited down, default up) at the head of
/// [`resolve_stream`] and from the app loop when the player page appears.
/// Leaving the player now removes the torrents (see [`stop_background`]), so
/// this is a defensive reset: a torrent opened from Home must never inherit a
/// limit set by anything else.
pub fn resume_foreground() {
    if available() {
        limits(0, DEFAULT_UPLOAD_BPS);
        plx_base::eventlog::log("sbengine: foreground unlimited (playback)");
    }
}

/// Set the streaming-window size (payload bytes kept around the playhead).
pub fn window(bytes: u64) {
    if let Some(a) = api() {
        unsafe { (a.window)(bytes) };
    }
}

/// Apply one libtorrent setting by name (int or bool); 0 == applied.
pub fn setting(name: &str, value: c_int) -> bool {
    match api() {
        Some(a) => {
            let c_name = CString::new(name).unwrap();
            unsafe { (a.setting)(c_name.as_ptr(), value) == 0 }
        }
        None => false,
    }
}

/// Point the payload at another folder (0 == queued). Streams keep their address.
pub fn relocate(cache_dir: &str, segment_bytes: u64) -> bool {
    match api() {
        Some(a) => {
            let c_dir = CString::new(cache_dir).unwrap();
            unsafe { (a.relocate)(c_dir.as_ptr(), segment_bytes) == 0 }
        }
        None => false,
    }
}

// Suppress the unused warning for `c_void` — the dynlib macro's `dynlib_wrapper`
// expansion needs `c_void` in scope; `sbengine` declares no fn needing it, so the
// import would otherwise warn. `EngineApi` covers the table.
#[allow(dead_code)]
fn _assert_c_void(_: *mut c_void) {}

#[cfg(test)]
mod budget_tests {
    use super::{DEFAULT_MEM_CACHE_BYTES, DEFAULT_WINDOW_BYTES};

    /// The engine's payload is one budget, not two independent knobs.
    ///
    /// Both terms are RAM on the television: the window lives on the RAM-backed tmpfs, and the
    /// cache is a second copy of bytes the window already holds. They were 40 + 32 MiB, sized as
    /// if the window were storage, and the set measured `MemAvailable` **78 MB** mid-film with
    /// them in place. Raising either half means re-measuring on the device first; this test fails
    /// so that cannot happen by editing one constant in passing.
    #[test]
    fn the_engine_payload_stays_inside_40_mib() {
        let total = DEFAULT_WINDOW_BYTES + DEFAULT_MEM_CACHE_BYTES;
        assert!(
            total <= 40 * 1024 * 1024,
            "engine payload is {} MiB; the set measured 78 MB MemAvailable mid-film",
            total / (1024 * 1024)
        );
    }

    #[test]
    fn the_window_can_still_hold_a_few_seconds_of_a_4k_remux() {
        // 25 Mbit/s is a 4K remux at the low end; 8 s of lookahead is the floor below which a
        // slow peer shows up as a stall rather than a moment of buffering.
        let bytes_per_second = 25_000_000 / 8;
        let seconds = DEFAULT_WINDOW_BYTES / bytes_per_second;
        assert!(seconds >= 8, "window holds only {seconds} s at 25 Mbit/s");
    }
}
