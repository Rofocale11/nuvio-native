//! **plex.tv, answered by the Nuvio account.** The app's plex.tv base points at the shim
//! (`plx_base::local_server`), so PlxNative's own QR sign-in screen and who's-watching picker become
//! the Nuvio login with no new UI:
//!
//! | plex.tv call the app makes | answered from |
//! |---|---|
//! | `POST /api/v2/pins` | `nuvio::start_login` (the code is Nuvio's user code) |
//! | `GET /api/v2/pins/qr/{code}` | a PNG of Nuvio's `verification_uri_complete` |
//! | `GET /api/v2/pins/{id}` | `nuvio::poll_login`; on approval the session is kept and a local token returned |
//! | `GET /api/v2/user` | the signed-in Nuvio account (or the local "Nuvio" user) |
//! | `GET /api/v2/resources` | exactly this shim, as the one owned server |
//! | `GET /api/v2/home/users` | `nuvio::profiles` |
//! | `POST /api/v2/home/users/{uuid}/switch` | that profile's addons (`nuvio::addons`) → the addon sink |
//!
//! Without a Nuvio account (or a build without its backend configured) the app never reaches any
//! of this: it boots straight into the shim with the local addon set.

use crate::shim_http::{Request, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

/// Receives a profile's addon set when the user picks it in the who's-watching picker. The app
/// persists it and swaps it into the running backend.
pub type AddonSink = Arc<dyn Fn(plx_stremio::addons::AddonSet) + Send + Sync>;

const PROFILE_UUID_PREFIX: &str = "nuvio-profile-";

pub struct Account {
    file: Option<PathBuf>,
    session: Mutex<Option<plx_stremio::nuvio::Session>>,
    pins: Mutex<HashMap<i64, PendingPin>>,
    /// The selected profile and library cache share a lock: profile switches cannot race a write.
    library: Arc<Mutex<LibraryContext>>,
    next_pin: AtomicI64,
    sink: Mutex<Option<AddonSink>>,
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

impl Account {
    pub fn new(dir: Option<&std::path::Path>, token: &str) -> Account {
        let file = dir.map(|d| d.join("nuvio-session.json"));
        let stored = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let session = stored.as_ref().and_then(session_from_json);
        let profile = stored.as_ref().and_then(|v| v["profile_id"].as_str()).filter(|s| s.parse::<u32>().is_ok_and(|n| n > 0)).map(str::to_string);
        Account {
            file,
            session: Mutex::new(session),
            pins: Mutex::new(HashMap::new()),
            library: Arc::new(Mutex::new(LibraryContext { profile, ..Default::default() })),
            next_pin: AtomicI64::new(1_000_001),
            sink: Mutex::new(None),
            token: token.to_string(),
            origin: Mutex::new(String::new()),
        }
    }

    pub fn set_sink(&self, sink: AddonSink) {
        *self.sink.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
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

    /// The plex.tv surface. `segs` are the path segments after `api/v2`.
    pub fn handle(&self, req: &Request, segs: &[&str]) -> Response {
        match (req.method.as_str(), segs) {
            ("POST", ["pins"]) => self.create_pin(),
            (_, ["pins", "qr", code]) => self.qr(code),
            (_, ["pins", id]) => self.poll_pin(id.parse().unwrap_or(-1)),
            (_, ["user"]) => self.user(),
            (_, ["user", "profile"]) => Response::json(&json!({"autoSelectAudio": true, "autoSelectSubtitle": 0, "defaultSubtitleLanguage": "en"})),
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

    fn user(&self) -> Response {
        let signed_in = self.signed_in();
        Response::json(&json!({
            "id": 1, "uuid": "nuvio-local", "username": "nuvio", "title": if signed_in { "Nuvio account" } else { "Nuvio" },
            "friendlyName": "Nuvio", "email": "", "thumb": "", "authToken": self.token,
            "profile": {"autoSelectAudio": true, "defaultSubtitleLanguage": "en", "autoSelectSubtitle": 0},
            "subscription": {"active": true, "status": "Active", "plan": "lifetime"},
        }))
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
        let profiles = self.fresh_session().and_then(|s| plx_stremio::nuvio::profiles(&s).ok()).unwrap_or_default();
        let users: Vec<Value> = if profiles.is_empty() {
            vec![json!({"id": 1, "uuid": "nuvio-local", "title": "Nuvio", "thumb": "", "admin": true, "restricted": false, "protected": false})]
        } else {
            profiles
                .iter()
                .enumerate()
                .map(|(i, p)| json!({"id": i + 1, "uuid": format!("{PROFILE_UUID_PREFIX}{}", p.id), "title": p.name, "thumb": "",
                                      "admin": i == 0, "restricted": false, "protected": false}))
                .collect()
        };
        Response::json(&json!({"users": users}))
    }

    fn switch(&self, uuid: &str) -> Response {
        let profile = uuid.strip_prefix(PROFILE_UUID_PREFIX).map(str::to_string);
        self.load_profile(profile.as_deref());
        Response::json(&json!({"id": 1, "uuid": uuid, "title": "Nuvio", "authToken": self.token}))
    }

    /// Fetch a profile's addons (the first profile when `None`) and hand them to the sink, off the
    /// request: manifests are fetched per addon.
    fn load_profile(&self, profile: Option<&str>) {
        let Some(session) = self.fresh_session() else { return };
        let Some(sink) = self.sink.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
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
            let mut context = library.lock().unwrap_or_else(|e| e.into_inner());
            if context.generation != generation { return; }
            context.profile = Some(pid.clone());
            if let Some(file) = file { save_session_file(&file, Some(&session), Some(&pid)); }
            if let Ok(addons) = plx_stremio::nuvio::addons(&session, &pid) {
                if !addons.is_empty() {
                    sink(plx_stremio::addons::AddonSet { addons });
                }
            }
        });
    }
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
    #[test]
    fn qr_is_a_png() {
        let png = super::qr_png("https://example.invalid/tv?code=ABCD").unwrap();
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }
}

fn log_failure(stage: &str, error: &plx_stremio::model::Error) {
    let category = match error { plx_stremio::model::Error::Network => "network", plx_stremio::model::Error::Status(_) => "http", plx_stremio::model::Error::Parse(_) => "response", plx_stremio::model::Error::NoAddon => "addon" };
    let status = match error { plx_stremio::model::Error::Status(s) => *s, _ => 0 };
    plx_stremio::nuvio::log_account_event(&format!("nuvio account: {stage} failed category={category} status={status}"));
}


fn save_session_file(file: &std::path::Path, session: Option<&plx_stremio::nuvio::Session>, profile: Option<&str>) {
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
