//! **plex.tv, answered by the Nuvio account.** The app's plex.tv base points at the shim
//! (`plx_base::local_server`), so PlxNative's own QR sign-in screen and who's-watching picker become
//! the Nuvio login with no new UI:
//!
//! | plex.tv call the app makes | answered from |
//! |---|---|
//! | `POST /api/v2/pins` | `nuvio::start_login` (the code is Nuvio's user code) |
//! | `GET /api/v2/pins/qr/{code}` | a PNG of Nuvio's `verification_uri_complete` |
//! | `GET /api/v2/pins/{id}` | `nuvio::poll_login`; on approval the session is kept and a local token returned |
//! | `GET /api/v2/user` | the signed-in Nuvio account (or the local "Nuvio" user), with its account preferences |
//! | `GET /api/v2/resources` | exactly this shim, as the one owned server |
//! | `GET /api/v2/home/users` | the ONE profile of the signed-in Nuvio account |
//! | `POST /api/v2/home/users/{uuid}/switch` | that profile's addons (`nuvio::addons`) → the addon sink |
//! | `GET`/`PUT /api/v2/user/profile` | account preferences from the one redb file, in plex.tv's shape |
//!
//! Without a Nuvio account (or a build without its backend configured) the app never reaches any
//! of this: it boots straight into the shim with the local addon set.

use crate::shim_http::{Request, Response};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

/// Receives a profile's addon set when the user picks it in the who's-watching picker. The app
/// persists it and swaps it into the running backend.
pub type AddonSink = Arc<dyn Fn(plx_stremio::addons::AddonSet) + Send + Sync>;
/// Fork Rofocale11: fires the account watch pull after a profile's addons are installed. The pull
/// builds each resume point's row through those addons, so this order is load-bearing.
pub type WatchSink = Arc<dyn Fn() + Send + Sync>;

const PROFILE_UUID_PREFIX: &str = "nuvio-profile-";

pub struct Account {
    file: Option<PathBuf>,
    session: Mutex<Option<plx_stremio::nuvio::Session>>,
    pins: Mutex<HashMap<i64, PendingPin>>,
    /// The selected profile and library cache share a lock: profile switches cannot race a write.
    library: Arc<Mutex<LibraryContext>>,
    next_pin: AtomicI64,
    sink: Mutex<Option<AddonSink>>,
    watch_sink: Mutex<Option<WatchSink>>,
    /// The token handed to the app in place of a plex.tv account token.
    token: String,
    origin: Mutex<String>,
}

#[derive(Default)]
struct LibraryContext {
    profile: Option<String>,
    generation: u64,
    cached: Option<(std::time::Instant, Vec<plx_stremio::model::MetaPreview>)>,
}

struct PendingPin {
    login: plx_stremio::nuvio::DeviceLogin,
    authorized: bool,
}

fn session_to_json(s: &plx_stremio::nuvio::Session) -> Value {
    json!({"access_token": s.access_token, "refresh_token": s.refresh_token, "user_id": s.user_id, "expires_at_unix": s.expires_at_unix})
}

fn session_from_json(v: &Value) -> Option<plx_stremio::nuvio::Session> {
    Some(plx_stremio::nuvio::Session {
        access_token: v["access_token"].as_str()?.to_string(),
        refresh_token: v["refresh_token"].as_str().unwrap_or("").to_string(),
        user_id: v["user_id"].as_str().unwrap_or("").to_string(),
        expires_at_unix: v["expires_at_unix"].as_u64().unwrap_or(0),
    })
}

// Fork Rofocale11: a real Plex account owns these rows and a real PMS echoes them on
// `/api/v2/user/profile`. Nuvio has neither, so the shim provides the same surface from the ONE
// redb file. Keeping plex.tv's camelCase keys on the wire means the app's unchanged account
// client (Settings → Account/Subtitles and playback's preference warmup) reads and writes them
// exactly as it does against `tests/mock_pms.py`.
const PREF_KEYS: &[&str] = &[
    "autoSelectAudio",
    "defaultAudioLanguage",
    "defaultSubtitleLanguage",
    "autoSelectSubtitle",
    "defaultSubtitleForced",
];

/// The same starting row the mock serves: language picking on, English audio and subtitles,
/// subtitles "when needed". A Nuvio account that never opened Settings still reads as a complete
/// profile rather than a Retry card.
fn default_prefs() -> Map<String, Value> {
    let mut prefs = Map::new();
    prefs.insert("autoSelectAudio".into(), json!(true));
    prefs.insert("defaultAudioLanguage".into(), json!("en"));
    prefs.insert("defaultSubtitleLanguage".into(), json!("en"));
    prefs.insert("autoSelectSubtitle".into(), json!(1));
    prefs.insert("defaultSubtitleForced".into(), json!(0));
    prefs.insert("defaultSubtitleAccessibility".into(), json!(0));
    prefs
}

/// Read the stored profile over the defaults. Only the keys the account client knows are adopted:
/// an older build's row could carry other shapes, and unknown fields must never leak onto the
/// account surface.
fn read_prefs(store: Option<&plx_nuvio_store::Store>) -> Map<String, Value> {
    let mut prefs = default_prefs();
    let bytes = store.and_then(|s| s.get(plx_nuvio_store::Table::AccountPrefs, "prefs"));
    if let Some(bytes) = bytes {
        if let Ok(Value::Object(stored)) = serde_json::from_slice::<Value>(&bytes) {
            for (key, value) in stored {
                if PREF_KEYS.contains(&key.as_str()) {
                    prefs.insert(key, value);
                }
            }
        }
    }
    prefs
}

fn put_prefs(
    store: Option<&plx_nuvio_store::Store>,
    prefs: &Map<String, Value>,
) -> Result<(), String> {
    let store = store.ok_or_else(|| "the Nuvio store is not installed".to_string())?;
    let bytes = serde_json::to_vec(&Value::Object(prefs.clone()))
        .map_err(|e| format!("cannot encode account preferences: {e}"))?;
    store.put(plx_nuvio_store::Table::AccountPrefs, "prefs", &bytes)
}

/// A PUT changes only the named keys and validates them the way the mock does: an empty language
/// is a real value ("Original"/"Off"), booleans accept 1/0 as well as true/false, and the two
/// numeric modes have the Plex ranges. A bad query leaves `prefs` untouched.
fn apply_profile_update(
    prefs: &mut Map<String, Value>,
    query: &HashMap<String, String>,
) -> Result<(), &'static str> {
    for (key, value) in query {
        let parsed = match key.as_str() {
            "defaultAudioLanguage" | "defaultSubtitleLanguage" => Value::String(value.clone()),
            "autoSelectAudio" => match value.as_str() {
                "true" | "1" => json!(true),
                "false" | "0" => json!(false),
                _ => return Err("invalid profile preference"),
            },
            "autoSelectSubtitle" | "defaultSubtitleForced" => {
                let limit = if key == "autoSelectSubtitle" { 2 } else { 3 };
                match value.parse::<i64>() {
                    Ok(n) if (0..=limit).contains(&n) => json!(n),
                    _ => return Err("invalid profile preference"),
                }
            }
            _ => return Err("unknown profile preference"),
        };
        prefs.insert(key.clone(), parsed);
    }
    Ok(())
}

impl Account {
    pub fn new(dir: Option<&std::path::Path>, token: &str) -> Account {
        let file = dir.map(|d| d.join("nuvio-session.json"));
        // Fork Rofocale11: the Nuvio backend session lives in the one redb file. A legacy JSON
        // file is still read on first boot so an install can migrate without signing in again.
        let stored = match plx_nuvio_store::get().and_then(|s| s.get(plx_nuvio_store::Table::AccountSession, "session")) {
            Some(bytes) => serde_json::from_slice::<Value>(&bytes).ok(),
            None => file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok()),
        };
        let session = stored.as_ref().and_then(session_from_json);
        let profile = stored.as_ref().and_then(|v| v["profile_id"].as_str()).filter(|s| s.parse::<u32>().is_ok_and(|n| n > 0)).map(str::to_string);
        Account {
            file,
            session: Mutex::new(session),
            pins: Mutex::new(HashMap::new()),
            library: Arc::new(Mutex::new(LibraryContext { profile, ..Default::default() })),
            next_pin: AtomicI64::new(1_000_001),
            sink: Mutex::new(None),
            watch_sink: Mutex::new(None),
            token: token.to_string(),
            origin: Mutex::new(String::new()),
        }
    }

    pub fn set_sink(&self, sink: AddonSink) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    /// Fork Rofocale11: install the callback that merges this profile's account resume points.
    pub fn set_watch_sink(&self, sink: WatchSink) {
        *self.watch_sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    }

    pub fn set_origin(&self, origin: &str) {
        *self.origin.lock().unwrap_or_else(|e| e.into_inner()) = origin.to_string();
    }

    pub fn signed_in(&self) -> bool {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn save(&self, s: Option<&plx_stremio::nuvio::Session>) {
        let Some(f) = &self.file else { return };
        let profile = if s.is_some() { self.library.lock().unwrap_or_else(|e| e.into_inner()).profile.clone() } else { None };
        save_session_file(f, s, profile.as_deref());
    }

    /// The stored session, refreshed when it is near expiry.
    fn fresh_session(&self) -> Option<plx_stremio::nuvio::Session> {
        let cur = self.session.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        match plx_stremio::nuvio::fresh(&cur) {
            Ok(s) => {
                if s.access_token != cur.access_token {
                    self.save(Some(&s));
                    *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
                }
                Some(s)
            }
            Err(_) => Some(cur),
        }
    }

    pub fn sign_out(&self) {
        let mut context = self.library.lock().unwrap_or_else(|e| e.into_inner());
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(file) = &self.file { save_session_file(file, None, None); }
        context.generation = context.generation.wrapping_add(1);
        context.profile = None;
        context.cached = None;
    }

    /// Read the active account's saved library; failures remain failures, never an empty success.
    pub fn saved_library(&self, refresh: bool) -> plx_stremio::model::Result<Vec<plx_stremio::model::MetaPreview>> {
        let session = self.fresh_session().ok_or(plx_stremio::model::Error::Status(401))?;
        let mut context = self.library.lock().unwrap_or_else(|e| e.into_inner());
        if !self.session.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.user_id == session.user_id) {
            return Err(plx_stremio::model::Error::Status(401));
        }
        if !refresh {
            if let Some((at, rows)) = &context.cached {
                if at.elapsed() < std::time::Duration::from_secs(60) { return Ok(rows.clone()); }
            }
        }
        let pid = match &context.profile {
            Some(pid) => pid.clone(),
            None => plx_stremio::nuvio::profiles(&session)?.first().map(|p| p.id.clone()).unwrap_or_else(|| "1".into()),
        };
        let rows = plx_stremio::nuvio::saved_library(&session, &pid)?;
        context.profile = Some(pid);
        context.cached = Some((std::time::Instant::now(), rows.clone()));
        Ok(rows)
    }

    /// Explicit save/remove; confirm with a fresh pull before publishing the new state to the UI.
    pub fn set_saved(&self, meta: &plx_stremio::model::Meta, saved: bool) -> plx_stremio::model::Result<()> {
        let session = self.fresh_session().ok_or(plx_stremio::model::Error::Status(401))?;
        let mut context = self.library.lock().unwrap_or_else(|e| e.into_inner());
        if !self.session.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.user_id == session.user_id) {
            return Err(plx_stremio::model::Error::Status(401));
        }
        let pid = match &context.profile {
            Some(pid) => pid.clone(),
            None => plx_stremio::nuvio::profiles(&session)?.first().map(|p| p.id.clone()).unwrap_or_else(|| "1".into()),
        };
        context.cached = None;
        plx_stremio::nuvio::save_library_item(&session, &pid, meta, saved)?;
        let rows = plx_stremio::nuvio::saved_library(&session, &pid)?;
        if rows.iter().any(|r| r.id == meta.id && r.type_ == meta.type_) != saved {
            return Err(plx_stremio::model::Error::Parse("saved library confirmation failed".into()));
        }
        context.profile = Some(pid);
        context.cached = Some((std::time::Instant::now(), rows));
        plx_stremio::nuvio::log_account_event(if saved { "nuvio: My List add confirmed" } else { "nuvio: My List remove confirmed" });
        Ok(())
    }

    // Fork Rofocale11: account watch progress, on the same profile-scoped session as My List.
    /// Report resume points to the account. No `library` guard is held across this call: a push
    /// runs on the player's reporting thread while `load_profile` wants that lock.
    pub fn push_watch_progress(&self, entries: &[plx_stremio::nuvio::WatchProgress]) -> plx_stremio::model::Result<()> {
        let session = self.fresh_session().ok_or(plx_stremio::model::Error::Status(401))?;
        if !self.session.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.user_id == session.user_id) {
            return Err(plx_stremio::model::Error::Status(401));
        }
        let pid = self.profile_id(&session)?;
        plx_stremio::nuvio::push_watch_progress(&session, &pid, entries)
    }

    /// The account's resume points for the active profile.
    pub fn pull_watch_progress(&self) -> plx_stremio::model::Result<Vec<plx_stremio::nuvio::WatchProgress>> {
        let session = self.fresh_session().ok_or(plx_stremio::model::Error::Status(401))?;
        if !self.session.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|s| s.user_id == session.user_id) {
            return Err(plx_stremio::model::Error::Status(401));
        }
        let pid = self.profile_id(&session)?;
        plx_stremio::nuvio::pull_watch_progress(&session, &pid)
    }

    /// The active profile id, resolving the first profile when none was picked — the same fallback
    /// `saved_library` uses. The `library` lock is never held across the HTTP call that resolves it.
    fn profile_id(&self, session: &plx_stremio::nuvio::Session) -> plx_stremio::model::Result<String> {
        if let Some(pid) = self.library.lock().unwrap_or_else(|e| e.into_inner()).profile.clone() {
            return Ok(pid);
        }
        let pid = plx_stremio::nuvio::profiles(session)?.first().map(|p| p.id.clone()).unwrap_or_else(|| "1".into());
        self.library.lock().unwrap_or_else(|e| e.into_inner()).profile = Some(pid.clone());
        Ok(pid)
    }

    /// The plex.tv surface. `segs` are the path segments after `api/v2`.
    pub fn handle(&self, req: &Request, segs: &[&str]) -> Response {
        match (req.method.as_str(), segs) {
            ("POST", ["pins"]) => self.create_pin(),
            (_, ["pins", "qr", code]) => self.qr(code),
            (_, ["pins", id]) => self.poll_pin(id.parse().unwrap_or(-1)),
            (_, ["user"]) => self.user(),
            (_, ["user", "profile"]) => self.user_profile(req),
            (_, ["resources"]) => self.resources(),
            (_, ["home", "users"]) => self.home_users(),
            ("POST", ["home", "users", uuid, "switch"]) => self.switch(uuid),
            (_, ["ping"]) => Response::json(&json!({})),
            _ => Response::json_status(404, &json!({"error": "not available"})),
        }
    }

    fn create_pin(&self) -> Response {
        match plx_stremio::nuvio::start_login() {
            Ok(login) => {
                let id = self.next_pin.fetch_add(1, Ordering::Relaxed);
                let code = login.user_code.clone();
                let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
                if pins.len() > 8 {
                    pins.clear();
                }
                pins.insert(id, PendingPin { login, authorized: false });
                Response::json_status(201, &json!({"id": id, "code": code, "expiresIn": 600, "authToken": null, "qr": ""}))
            }
            // The login screen reads a failure as "can't reach the service" and offers Retry.
            Err(error) => {
                log_failure("start", &error);
                Response::json_status(503, &json!({"error": "Nuvio sign-in is unavailable"}))
            },
        }
    }

    fn qr(&self, code: &str) -> Response {
        let url = self.pins.lock().unwrap_or_else(|e| e.into_inner()).values().find(|p| p.login.user_code == code).map(|p| p.login.qr_url.clone());
        match url.and_then(|u| qr_png(&u)) {
            Some(png) => Response::bytes(200, "image/png", png),
            None => Response::not_found(),
        }
    }

    fn poll_pin(&self, id: i64) -> Response {
        let login = {
            let pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
            match pins.get(&id) {
                Some(p) if p.authorized => return Response::json(&json!({"id": id, "authToken": self.token})),
                Some(p) => p.login.clone(),
                None => return Response::json_status(404, &json!({"error": "unknown pin"})),
            }
        };
        match plx_stremio::nuvio::poll_login(&login) {
            Ok(Some(session)) => {
                self.save(Some(&session));
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
                if let Some(p) = self.pins.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) {
                    p.authorized = true;
                }
                // A single-profile account never reaches the picker: load the first profile now.
                self.load_profile(None);
                Response::json(&json!({"id": id, "code": login.user_code, "authToken": self.token}))
            }
            Ok(None) => Response::json(&json!({"id": id, "code": login.user_code, "authToken": null})),
            Err(error) => {
                log_failure("poll", &error);
                let status = match &error { plx_stremio::model::Error::Parse(s) if matches!(s.as_str(), "login expired" | "login cancelled" | "login used") => 404, _ => 503 };
                Response::json_status(status, &json!({"error": "Nuvio sign-in could not finish"}))
            },
        }
    }

    /// Fork Rofocale11: one Nuvio login is one Plex Home profile. Every account route must name
    /// that same identity — the mock proves the app only seats a profile and enables its Settings
    /// when `/user`, the Home roster and the switch reply agree on id and uuid. The uuid is a fixed
    /// account identity rather than a live Nuvio profile id, so an addon load still in flight at
    /// login cannot make these three answers disagree.
    fn profile_identity(&self) -> (i64, String, String) {
        let title = if self.signed_in() { "Nuvio account" } else { "Nuvio" };
        (1, format!("{PROFILE_UUID_PREFIX}1"), title.to_string())
    }

    fn user(&self) -> Response {
        let (id, uuid, title) = self.profile_identity();
        // Fork Rofocale11: `/user` carries the same preference row as `/user/profile`, just as the
        // mock does, so playback's account warmup and Settings never see two different answers.
        let profile = read_prefs(plx_nuvio_store::get());
        Response::json(&json!({
            "id": id, "uuid": uuid, "username": "nuvio", "title": title,
            "friendlyName": "Nuvio", "email": "", "thumb": "", "authToken": self.token,
            "profile": profile,
            "subscription": {"active": true, "status": "Active", "plan": "lifetime"},
        }))
    }

    /// The plex.tv account-preference surface, backed by the single redb file. PUT uses an empty
    /// body and query parameters, exactly as Plex and the mock do; the updated row is returned so
    /// the caller can confirm what was saved without a second GET.
    fn user_profile(&self, req: &Request) -> Response {
        match req.method.as_str() {
            "GET" | "HEAD" => Response::json(&Value::Object(read_prefs(plx_nuvio_store::get()))),
            "PUT" => {
                if !req.body.is_empty() {
                    return Response::json_status(400, &json!({"error": "profile PUT requires an empty body"}));
                }
                let mut prefs = read_prefs(plx_nuvio_store::get());
                if let Err(error) = apply_profile_update(&mut prefs, &req.query) {
                    return Response::json_status(400, &json!({"error": error}));
                }
                match put_prefs(plx_nuvio_store::get(), &prefs) {
                    Ok(()) => Response::json(&Value::Object(prefs)),
                    Err(_) => Response::json_status(503, &json!({"error": "preferences not saved"})),
                }
            }
            _ => Response::json_status(405, &json!({"error": "method not allowed"})),
        }
    }

    fn resources(&self) -> Response {
        let origin = self.origin.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let port = origin.rsplit(':').next().and_then(|p| p.parse::<u16>().ok()).unwrap_or(0);
        Response::json(&json!([{
            "name": "Nuvio", "clientIdentifier": crate::shim_core::MACHINE_ID, "provides": "server",
            "owned": true, "home": false, "ownerId": 1, "presence": true, "accessToken": self.token,
            "httpsRequired": false, "publicAddressMatches": true,
            "connections": [{"protocol": "http", "address": "127.0.0.1", "port": port, "uri": origin,
                             "local": true, "relay": false, "IPv6": false}],
        }]))
    }

    fn home_users(&self) -> Response {
        // Fork Rofocale11: one Nuvio login is one profile — the roster cannot grow a Plex Home the
        // account does not have. Its uuid and id are the ones `/user` and switch publish.
        let (id, uuid, title) = self.profile_identity();
        Response::json(&json!({"users": [json!({
            "id": id, "uuid": uuid, "title": title, "username": "nuvio", "thumb": "",
            "admin": true, "restricted": false, "protected": false,
        })]}))
    }

    fn switch(&self, uuid: &str) -> Response {
        // Fork Rofocale11: the picker is a formality for one profile. Accept the uuid the account
        // routes publish, load that account's addons, and answer with the SAME identity so the app
        // seats it instead of leaving Settings read-only.
        let _ = uuid;
        self.load_profile(None);
        let (id, uuid, title) = self.profile_identity();
        Response::json(&json!({"id": id, "uuid": uuid, "title": title, "authToken": self.token}))
    }

    /// Fetch a profile's addons (the first profile when `None`) and hand them to the sink, off the
    /// request: manifests are fetched per addon. Fork Rofocale11: the account watch pull fires only
    /// AFTER those addons are installed — its rows are built through the addon backend, and a row
    /// stored without a snapshot is dropped by Continue Watching until the next boot.
    fn load_profile(&self, profile: Option<&str>) {
        let Some(session) = self.fresh_session() else { return };
        let Some(sink) = self.sink.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        let watch_sink = self.watch_sink.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let profile = profile.map(str::to_string);
        let library = self.library.clone();
        let file = self.file.clone();
        // Mark unresolved immediately, so no request can retain the previous profile's library.
        let generation = {
            let mut context = library.lock().unwrap_or_else(|e| e.into_inner());
            context.generation = context.generation.wrapping_add(1);
            context.profile = profile.clone();
            context.cached = None;
            context.generation
        };
        let _ = std::thread::Builder::new().name("pmsshim-profile".into()).spawn(move || {
            let pid = profile.clone().or_else(|| plx_stremio::nuvio::profiles(&session).ok().and_then(|p| p.first().map(|p| p.id.clone()))).unwrap_or_else(|| "1".into());
            {
                let mut context = library.lock().unwrap_or_else(|e| e.into_inner());
                if context.generation != generation { return; }
                context.profile = Some(pid.clone());
            }
            if let Some(file) = file { save_session_file(&file, Some(&session), Some(&pid)); }
            // The guard is dropped: the pull resolves its profile through the same lock, and
            // holding it here deadlocks on the first profile switch.
            let addons = plx_stremio::nuvio::addons(&session, &pid);
            activate_loaded_profile(&library, generation, &sink, watch_sink.as_ref(), addons);
        });
    }
}

/// Install a profile's addons, then fire its watch pull. Both are guarded by the load generation:
/// a profile picked while this worker was fetching neither installs its set nor pulls its history.
fn activate_loaded_profile(library: &Mutex<LibraryContext>, generation: u64, sink: &AddonSink,
    watch_sink: Option<&WatchSink>, addons: plx_stremio::model::Result<Vec<plx_stremio::model::Addon>>) {
    if library.lock().unwrap_or_else(|e| e.into_inner()).generation != generation { return; }
    if let Ok(addons) = addons {
        if !addons.is_empty() { sink(plx_stremio::addons::AddonSet { addons }); }
    }
    if library.lock().unwrap_or_else(|e| e.into_inner()).generation != generation { return; }
    if let Some(watch) = watch_sink { watch(); }
}

/// A QR code PNG the way plex.tv serves one (RGBA, opaque modules on transparent, 8 px modules,
/// 4-module quiet zone).
pub fn qr_png(text: &str) -> Option<Vec<u8>> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let (scale, border) = (8i32, 4i32);
    let side = (qr.size() + border * 2) * scale;
    // plex.tv's shape: WHITE modules on a TRANSPARENT ground, which the login screen tints dark over
    // its own white card (an opaque image drew as a solid black square).
    let mut pixels = Vec::with_capacity((side * side * 4) as usize);
    for y in 0..side {
        for x in 0..side {
            let dark = qr.get_module(x / scale - border, y / scale - border);
            pixels.extend_from_slice(if dark { &[255, 255, 255, 255] } else { &[255, 255, 255, 0] });
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, side as u32, side as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().ok()?;
        w.write_image_data(&pixels).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn request(method: &str, query: &[(&str, &str)], body: &[u8]) -> Request {
        Request {
            method: method.to_string(),
            path: "/api/v2/user/profile".to_string(),
            query: query.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            headers: HashMap::new(),
            body: body.to_vec(),
        }
    }

    /// Fork Rofocale11: a profile's addons are installed before its watch pull. The pull builds
    /// each resume point's row through the addon backend, so reversing this order stores positions
    /// with no row snapshot and Continue Watching drops them until the next launch.
    #[test]
    fn a_profile_load_installs_addons_before_it_pulls_watch() {
        use std::sync::Mutex as StdMutex;
        let order = Arc::new(StdMutex::new(Vec::new()));
        let library = Mutex::new(LibraryContext { generation: 7, ..Default::default() });
        let sink_addons = Arc::clone(&order);
        let sink: AddonSink = Arc::new(move |_| sink_addons.lock().unwrap().push("addons"));
        let pull_order = Arc::clone(&order);
        let watch: WatchSink = Arc::new(move || pull_order.lock().unwrap().push("watch"));
        let addon = plx_stremio::model::Addon { transport_url: "https://example.invalid/addon".into(), ..Default::default() };
        activate_loaded_profile(&library, 7, &sink, Some(&watch), Ok(vec![addon]));
        assert_eq!(*order.lock().unwrap(), ["addons", "watch"]);
        // A stale load pulls nothing.
        order.lock().unwrap().clear();
        activate_loaded_profile(&library, 6, &sink, Some(&watch), Ok(Vec::new()));
        assert!(order.lock().unwrap().is_empty());
        // An empty addon answer still pulls the account history.
        activate_loaded_profile(&library, 7, &sink, Some(&watch), Ok(Vec::new()));
        assert_eq!(*order.lock().unwrap(), ["watch"]);
    }

    #[test]
    fn qr_is_a_png() {
        let png = super::qr_png("https://example.invalid/tv?code=ABCD").unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    /// The mock's own selftest (`tests/mock_pms.py`, account-preference block): `/user`, the Home
    /// roster and the switch reply all describe one profile, so the app seats it and reads its
    /// Settings. A uuid mismatch here is what left Nuvio's Settings rows greyed out.
    #[test]
    fn account_routes_publish_one_profile_identity() {
        let account = Account::new(None, "local-token");
        let user: Value = serde_json::from_slice(&account.user().body).unwrap();
        let roster: Value = serde_json::from_slice(&account.home_users().body).unwrap();
        let switched: Value = serde_json::from_slice(&account.switch(user["uuid"].as_str().unwrap()).body).unwrap();
        let only = &roster["users"][0];
        assert_eq!(roster["users"].as_array().unwrap().len(), 1);
        assert_eq!(user["uuid"], only["uuid"]);
        assert_eq!(user["id"], only["id"]);
        assert_eq!(user["uuid"], switched["uuid"]);
        assert_eq!(user["id"], switched["id"]);
        assert_eq!(user["title"], only["title"]);
        assert_eq!(user["profile"], json!(default_prefs()), "the account row is on /user too");
    }

    #[test]
    fn profile_get_and_put_follow_the_mock_contract() {
        // GET serves the defaults even with no installed store (the memory-only build).
        let account = Account::new(None, "local-token");
        // Through `handle`, the route the app's `GET /api/v2/user/profile` actually takes.
        let got = account.handle(&request("GET", &[], b""), &["user", "profile"]);
        assert_eq!(got.status, 200);
        assert_eq!(serde_json::from_slice::<Value>(&got.body).unwrap(), json!(default_prefs()));

        // A body on PUT is refused, as in the mock.
        assert_eq!(account.user_profile(&request("PUT", &[], b"body")).status, 400);
        assert_eq!(account.user_profile(&request("POST", &[], b"")).status, 405);

        // An update changes only the named keys; an empty language is a real value.
        let mut prefs = default_prefs();
        let query: HashMap<String, String> = [("defaultAudioLanguage", ""),
            ("autoSelectAudio", "true"), ("defaultSubtitleForced", "3")]
            .into_iter().map(|(k, v)| (k.into(), v.into())).collect();
        apply_profile_update(&mut prefs, &query).unwrap();
        assert_eq!(prefs["defaultAudioLanguage"], json!(""));
        assert_eq!(prefs["autoSelectAudio"], json!(true));
        assert_eq!(prefs["defaultSubtitleForced"], json!(3));
        assert_eq!(prefs["defaultSubtitleLanguage"], json!("en"), "unnamed keys survive");
        assert_eq!(prefs["autoSelectSubtitle"], json!(1));

        // Bad values and unknown keys are refused and leave the row alone.
        for bad in [("autoSelectSubtitle", "3"), ("defaultSubtitleForced", "9"),
            ("autoSelectAudio", "yes"), ("unknown", "1")] {
            let mut before = default_prefs();
            let q: HashMap<String, String> = [(bad.0.to_string(), bad.1.to_string())].into_iter().collect();
            assert!(apply_profile_update(&mut before, &q).is_err(), "{bad:?}");
            assert_eq!(before, default_prefs(), "a refused PUT cannot partially apply");
        }
    }

    /// The durable half: a PUT's row is what a later GET reads, including after a restart (a
    /// second `Store::open_at` over the same file). Unknown redb rows are never echoed.
    #[test]
    fn account_preferences_round_trip_through_redb() {
        let dir = std::env::temp_dir().join(format!("nuvio-account-prefs-test-{}-{}",
            std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let file = dir.join("nuvio.redb");
        let mut prefs = default_prefs();
        prefs.insert("defaultSubtitleLanguage".into(), json!("nl"));
        prefs.insert("bogus".into(), json!(1));
        {
            let store = plx_nuvio_store::Store::open_at(&file).unwrap();
            put_prefs(Some(&store), &prefs).unwrap();
        } // release redb's file lock before re-opening, as a process restart would
        let reopened = plx_nuvio_store::Store::open_at(&file).unwrap();
        let read = read_prefs(Some(&reopened));
        assert_eq!(read["defaultSubtitleLanguage"], json!("nl"));
        assert!(!read.contains_key("bogus"));
        assert_eq!(read["autoSelectSubtitle"], json!(1));
    }
}

fn log_failure(stage: &str, error: &plx_stremio::model::Error) {
    let category = match error { plx_stremio::model::Error::Network => "network", plx_stremio::model::Error::Status(_) => "http", plx_stremio::model::Error::Parse(_) => "response", plx_stremio::model::Error::NoAddon => "addon" };
    let status = match error { plx_stremio::model::Error::Status(s) => *s, _ => 0 };
    plx_stremio::nuvio::log_account_event(&format!("nuvio account: {stage} failed category={category} status={status}"));
}


fn save_session_file(file: &std::path::Path, session: Option<&plx_stremio::nuvio::Session>, profile: Option<&str>) {
    // Fork Rofocale11: one file, one record. The path is kept only as the in-memory-mode gate.
    if let Some(store) = plx_nuvio_store::get() {
        match session {
            Some(session) => {
                let mut value = session_to_json(session);
                value["profile_id"] = json!(profile);
                if let Ok(bytes) = serde_json::to_vec(&value) {
                    let _ = store.put(plx_nuvio_store::Table::AccountSession, "session", &bytes);
                }
            }
            None => { let _ = store.remove(plx_nuvio_store::Table::AccountSession, "session"); }
        }
        return;
    }
    match session {
        Some(session) => {
            let mut value = session_to_json(session);
            value["profile_id"] = json!(profile);
            let tmp = file.with_extension("json.tmp");
            if std::fs::write(&tmp, serde_json::to_vec(&value).unwrap_or_default()).is_ok() { let _ = std::fs::rename(&tmp,file); }
        }
        None => { let _ = std::fs::remove_file(file); }
    }
}
