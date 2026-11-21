use super::{
    apply_user_agent, available, curl_easy_init, curl_easy_perform, curl_easy_setopt_long, curl_easy_setopt_ptr,
    curl_version_num, origin, refuse_name, require_option, require_trust_store, threaded_tls_ready,
    tls_failure_reason, Easy, Tls, CURL, CURLOPT_CAINFO, CURLOPT_CONNECTTIMEOUT, CURLOPT_NOPROGRESS, CURLOPT_NOSIGNAL,
    CURLOPT_PROGRESSFUNCTION, CURLOPT_PROTOCOLS, CURLOPT_SSL_VERIFYHOST, CURLOPT_SSL_VERIFYPEER, CURLOPT_URL,
    CURLOPT_XFERINFODATA, CURLOPT_XFERINFOFUNCTION, CURLPROTO_HTTP, CURLPROTO_HTTPS, CURL_XFERINFO_MIN_VERSION,
};
use std::ffi::CString;
use std::os::raw::{c_int, c_long, c_void};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

plx_base::dynlib! {
    raw: ["libcurl.so.4", "libcurl.so.5", "libcurl.4.dylib"] {
    fn curl_easy_send(handle: *mut CURL, buffer: *const c_void, length: usize, sent: *mut usize) -> c_int;
    fn curl_easy_recv(handle: *mut CURL, buffer: *mut c_void, length: usize, received: *mut usize) -> c_int;
    fn curl_easy_getinfo_socket = "curl_easy_getinfo"(handle: *mut CURL, info: c_int, ..., out: *mut c_int) -> c_int;
}}

const CURLOPT_CONNECT_ONLY: c_int = 141;
const CURLOPT_HTTP_VERSION: c_int = 84;
const CURL_HTTP_VERSION_1_1: c_long = 2;
const CURLINFO_ACTIVESOCKET: c_int = 0x50_0000 + 44;
const CURLE_OK: c_int = 0;
const CURLE_COULDNT_RESOLVE_HOST: c_int = 6;
const CURLE_ABORTED_BY_CALLBACK: c_int = 42;
const CURLE_AGAIN: c_int = 81;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    Unavailable,
    SerialTls,
    Address,
    Refused(&'static str),
    Connect { rc: c_int, why: Option<String> },
    NoSocket,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Io {
    Moved(usize),
    Again,
    Closed,
    Failed(c_int),
}

pub struct Socket {
    easy: Easy,
    fd: c_int,
}

// Fork Rofocale11: upstream polls the caller's cancel flag through libcurl's progress/XFERINFO
// callback, but libcurl 8.x on Linux never invokes that callback while a CONNECT_ONLY TLS
// handshake is stalled on a peer that says nothing — the dial only fails at
// CURLOPT_CONNECTTIMEOUT (~10 s) with rc 28, so `ws::run` returned Refused instead of Cancelled
// (a_cancel_during_the_tls_handshake_ends_the_dial, every run). Run the blocking perform on a
// short-lived worker and poll the flag in the caller instead. Everything the worker touches is
// owned by it (the easy handle, both CStrings, and an Arc-backed abort flag the progress callback
// may still read), so a caller that leaves on Cancelled cannot dangle any of it. The fork's roots
// mode and diagnostic redaction live in net.rs and are deliberately untouched here.
const CANCEL_POLL: Duration = Duration::from_millis(100);

/// The easy handle is set up entirely before the worker starts and is touched by one thread at a
/// time: the worker during `curl_easy_perform`, the caller only after the outcome is sent back.
struct SendEasy(Easy);
unsafe impl Send for SendEasy {}

type Outcome = (c_int, c_int, bool, SendEasy);

struct Dial {
    easy: SendEasy,
    // Kept alive for the whole perform: libcurl retains the URL and CAINFO pointers.
    url: CString,
    bundle: Option<CString>,
    stop: Arc<AtomicBool>,
    watching: bool,
    tx: Sender<Outcome>,
}

fn perform(dial: Dial) {
    let Dial { easy: SendEasy(easy), url: _url, bundle: _bundle, stop: _stop, watching, tx } = dial;
    let rc = unsafe { curl_easy_perform(easy.0) };
    if watching {
        unsafe { curl_easy_setopt_long(easy.0, CURLOPT_NOPROGRESS, 1) };
    }
    let mut fd: c_int = -1;
    let socket = rc == CURLE_OK
        && unsafe { curl_easy_getinfo_socket(easy.0, CURLINFO_ACTIVESOCKET, &mut fd) } == CURLE_OK
        && fd >= 0;
    let _ = tx.send((rc, fd, socket, SendEasy(easy)));
}

pub fn open(
    _off: &plx_base::task::OffFrame,
    authority: &str,
    secure: Option<Tls<'_>>,
    connect_timeout_s: c_long,
    cancel: &AtomicBool,
) -> Result<Socket, OpenError> {
    if !available() || !bound() {
        return Err(OpenError::Unavailable);
    }
    if !threaded_tls_ready() {
        return Err(OpenError::SerialTls);
    }
    let tls = secure.is_some();
    let address = format!("{}://{authority}/", if tls { "https" } else { "http" });
    if refuse_name(origin::url_host(&address), connect_timeout_s) {
        return Err(OpenError::Connect { rc: CURLE_COULDNT_RESOLVE_HOST, why: None });
    }
    let url = CString::new(address).map_err(|_| OpenError::Address)?;
    let bundle = match &secure {
        Some(Tls::CaBundle(path)) => Some(CString::new(*path).map_err(|_| OpenError::Address)?),
        Some(Tls::Pinned(_)) => return Err(OpenError::Refused("CURLOPT_PINNEDPUBLICKEY")),
        Some(Tls::Ca) | None => None,
    };
    unsafe {
        let handle = curl_easy_init();
        if handle.is_null() {
            return Err(OpenError::Unavailable);
        }
        let easy = Easy(handle);
        let protocols = if tls { CURLPROTO_HTTPS } else { CURLPROTO_HTTP };
        let require = |rc: c_int, option: &'static str| require_option(rc, option).map_err(|_| OpenError::Refused(option));
        require(curl_easy_setopt_long(easy.0, CURLOPT_PROTOCOLS, protocols), "CURLOPT_PROTOCOLS")?;
        if tls {
            require(curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYPEER, 1), "CURLOPT_SSL_VERIFYPEER")?;
            require(curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYHOST, 2), "CURLOPT_SSL_VERIFYHOST")?;
        }
        if let Some(bundle) = &bundle {
            require_trust_store(curl_easy_setopt_ptr(easy.0, CURLOPT_CAINFO, bundle.as_ptr() as *const c_void))
                .map_err(|_| OpenError::Refused("CURLOPT_CAINFO"))?;
        }
        require(curl_easy_setopt_long(easy.0, CURLOPT_HTTP_VERSION, CURL_HTTP_VERSION_1_1), "CURLOPT_HTTP_VERSION")?;
        require(curl_easy_setopt_long(easy.0, CURLOPT_CONNECT_ONLY, 1), "CURLOPT_CONNECT_ONLY")?;
        curl_easy_setopt_ptr(easy.0, CURLOPT_URL, url.as_ptr() as *const c_void);
        curl_easy_setopt_long(easy.0, CURLOPT_NOSIGNAL, 1);
        curl_easy_setopt_long(easy.0, CURLOPT_CONNECTTIMEOUT, connect_timeout_s);
        apply_user_agent(easy.0);
        // The callback (when this libcurl does call it) watches our own Arc, not the caller's
        // flag: the worker may still be dialling after the caller has returned on Cancelled.
        let stop = Arc::new(AtomicBool::new(false));
        let watching = watch_cancel(easy.0, stop.as_ref());
        let (tx, rx) = mpsc::channel::<Outcome>();
        let (hand_tx, hand_rx) = mpsc::channel::<Dial>();
        let worker = plx_base::task::spawn("ws-connect (cancel-polled dial)", move || {
            if let Ok(dial) = hand_rx.recv() {
                perform(dial);
            }
        });
        let dial = Dial { easy: SendEasy(easy), url, bundle, stop: Arc::clone(&stop), watching, tx };
        let (rc, fd, socket, SendEasy(easy)) = if worker.is_some() {
            let _ = hand_tx.send(dial);
            let mut outcome = None;
            while outcome.is_none() {
                match rx.recv_timeout(CANCEL_POLL) {
                    Ok(got) => outcome = Some(got),
                    Err(RecvTimeoutError::Timeout) => {
                        if cancel.load(Ordering::Acquire) {
                            stop.store(true, Ordering::Release);
                            return Err(OpenError::Cancelled);
                        }
                    }
                    Err(RecvTimeoutError::Disconnected) => return Err(OpenError::Unavailable),
                }
            }
            outcome.unwrap()
        } else {
            // A refused thread must not take the transport down: keep the upstream inline dial.
            perform(dial);
            rx.recv().map_err(|_| OpenError::Unavailable)?
        };
        if rc == CURLE_ABORTED_BY_CALLBACK && stop.load(Ordering::Acquire) {
            return Err(OpenError::Cancelled);
        }
        if rc != CURLE_OK {
            return Err(OpenError::Connect { rc, why: tls_failure_reason(easy.0, rc) });
        }
        if !socket {
            return Err(OpenError::NoSocket);
        }
        Ok(Socket { easy, fd })
    }
}

impl Socket {
    pub fn fd(&self) -> c_int {
        self.fd
    }

    pub fn send(&mut self, bytes: &[u8]) -> Io {
        let mut sent = 0usize;
        let rc = unsafe { curl_easy_send(self.easy.0, bytes.as_ptr() as *const c_void, bytes.len(), &mut sent) };
        match rc {
            CURLE_OK if sent > 0 => Io::Moved(sent),
            CURLE_OK | CURLE_AGAIN => Io::Again,
            rc => Io::Failed(rc),
        }
    }

    pub fn recv(&mut self, into: &mut [u8]) -> Io {
        let mut received = 0usize;
        let rc = unsafe { curl_easy_recv(self.easy.0, into.as_mut_ptr() as *mut c_void, into.len(), &mut received) };
        match rc {
            CURLE_OK if received == 0 => Io::Closed,
            CURLE_OK => Io::Moved(received),
            CURLE_AGAIN => Io::Again,
            rc => Io::Failed(rc),
        }
    }
}

fn bound() -> bool {
    static BOUND: OnceLock<bool> = OnceLock::new();
    *BOUND.get_or_init(|| {
        let ok = raw::load(None).ok();
        if !ok {
            plx_base::eventlog::log("net: this libcurl lacks curl_easy_send/curl_easy_recv; no WebSocket");
        }
        ok
    })
}

unsafe fn watch_cancel(easy: *mut CURL, cancel: &AtomicBool) -> bool {
    let data = (cancel as *const AtomicBool) as *const c_void;
    let version = curl_version_num();
    if curl_easy_setopt_long(easy, CURLOPT_NOPROGRESS, 0) != CURLE_OK {
        return false;
    }
    let xferinfo = (version == 0 || version >= CURL_XFERINFO_MIN_VERSION)
        && curl_easy_setopt_ptr(easy, CURLOPT_XFERINFOFUNCTION, cancel_xferinfo_cb as *const c_void) == CURLE_OK;
    let installed = (xferinfo
        || curl_easy_setopt_ptr(easy, CURLOPT_PROGRESSFUNCTION, cancel_progress_cb as *const c_void) == CURLE_OK)
        && curl_easy_setopt_ptr(easy, CURLOPT_XFERINFODATA, data) == CURLE_OK;
    if !installed {
        curl_easy_setopt_long(easy, CURLOPT_NOPROGRESS, 1);
    }
    installed
}

extern "C" fn cancel_xferinfo_cb(clientp: *mut c_void, _dltotal: i64, _dlnow: i64, _ultotal: i64, _ulnow: i64) -> c_int {
    cancelled(clientp)
}

extern "C" fn cancel_progress_cb(clientp: *mut c_void, _dltotal: f64, _dlnow: f64, _ultotal: f64, _ulnow: f64) -> c_int {
    cancelled(clientp)
}

fn cancelled(clientp: *mut c_void) -> c_int {
    if clientp.is_null() {
        return 1;
    }
    c_int::from(unsafe { &*(clientp as *const AtomicBool) }.load(Ordering::Acquire))
}
