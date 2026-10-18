//! **plex.tv, answered by the Stremio account.** The app's plex.tv base points at the shim
//! (`plx_base::local_server`), so PlxNative's own QR sign-in screen and who's-watching picker become
//! the Stremio login with no new UI.

use crate::shim_http::{Request, Response};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

pub type AddonSink = Arc<dyn Fn(plx_stremio::addons::AddonSet) + Send + Sync>;

pub struct Account {
    file: Option<PathBuf>,
    session: Mutex<Option<plx_stremio::nuvio::Session>>,
    pins: Mutex<HashMap<i64, PendingPin>>,
    library: Arc<Mutex<LibraryContext>>,
    next_pin: AtomicI64,
    sink: Mutex<Option<AddonSink>>,
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
    code: String,
    qrcode: String,
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

    fn fresh_session(&self) -> Option<plx_stremio::nuvio::Session> {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn sign_out(&self) {
        let mut context = self.library.lock().unwrap_or_else(|e| e.into_inner());
        *self.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(file) = &self.file { save_session_file(file, None, None); }
        context.generation = context.generation.wrapping_add(1);
        context.profile = None;
        context.cached = None;
    }

    pub fn saved_library(&self, _refresh: bool) -> plx_stremio::model::Result<Vec<plx_stremio::model::MetaPreview>> {
        let session = self.fresh_session().ok_or(plx_stremio::model::Error::Status(401))?;
        let rows = plx_stremio::stremio::get_library(&session.access_token)?;
        
        let mut previews = Vec::new();
        for row in rows {
            // Skip removed items
            if row["removed"].as_bool() == Some(true) { continue; }
            
            previews.push(plx_stremio::model::MetaPreview {
                id: row["_id"].as_str().unwrap_or_default().to_string(),
                type_: row["type"].as_str().unwrap_or_default().to_string(),
                name: row["name"].as_str().unwrap_or_default().to_string(),
                poster: Some(row["poster"].as_str().unwrap_or_default().to_string()),
                ..Default::default()
            });
        }
        Ok(previews)
    }

    pub fn set_saved(&self, meta: &plx_stremio::model::Meta, saved: bool) -> plx_stremio::model::Result<()> {
        let session = self.fresh_session().ok_or(plx_stremio::model::Error::Status(401))?;
        plx_stremio::stremio::set_library_item(
            &session.access_token,
            &meta.id,
            &meta.name,
            &meta.type_,
            meta.poster.as_deref().unwrap_or(""),
            saved
        )?;
        Ok(())
    }

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
        match plx_stremio::stremio::create_link_code() {
            Ok(login) => {
                let id = self.next_pin.fetch_add(1, Ordering::Relaxed);
                let code = login.code.clone();
                let mut pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
                if pins.len() > 8 {
                    pins.clear();
                }
                pins.insert(id, PendingPin { code: code.clone(), qrcode: login.link.clone(), authorized: false });
                Response::json_status(201, &json!({"id": id, "code": code, "expiresIn": 600, "authToken": null, "qr": ""}))
            }
            Err(error) => {
                log_failure("start", &error);
                Response::json_status(503, &json!({"error": "Stremio sign-in is unavailable"}))
            },
        }
    }

    fn qr(&self, code: &str) -> Response {
        let url = self.pins.lock().unwrap_or_else(|e| e.into_inner())
            .values()
            .find(|p| p.code == code)
            .map(|p| p.qrcode.clone());
            
        match url.and_then(|u| qr_png(&u)) {
            Some(png) => Response::bytes(200, "image/png", png),
            None => Response::not_found(),
        }
    }

    fn poll_pin(&self, id: i64) -> Response {
        let pin_code = {
            let pins = self.pins.lock().unwrap_or_else(|e| e.into_inner());
            match pins.get(&id) {
                Some(p) if p.authorized => return Response::json(&json!({"id": id, "authToken": self.token})),
                Some(p) => p.code.clone(),
                None => return Response::json_status(404, &json!({"error": "unknown pin"})),
            }
        };
        
        match plx_stremio::stremio::poll_link_code(&pin_code) {
            Ok(Some(auth_key)) => {
                let session = plx_stremio::nuvio::Session {
                    access_token: auth_key.clone(),
                    refresh_token: String::new(),
                    user_id: "stremio_user".into(),
                    expires_at_unix: 9999999999,
                };
                self.save(Some(&session));
                *self.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
                
                if let Some(p) = self.pins.lock().unwrap_or_else(|e| e.into_inner()).get_mut(&id) {
                    p.authorized = true;
                }
                
                self.load_profile(None); 
                
                Response::json(&json!({"id": id, "code": pin_code, "authToken": self.token}))
            }
            Ok(None) => Response::json(&json!({"id": id, "code": pin_code, "authToken": null})),
            Err(error) => {
                log_failure("poll", &error);
                Response::json_status(503, &json!({"error": "Stremio sign-in could not finish"}))
            },
        }
    }

    pub fn load_profile(&self, profile: Option<&str>) {
        let Some(session) = self.fresh_session() else { return };
        let Some(sink) = self.sink.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return };
        let profile = profile.map(str::to_string);
        let pid = profile.clone().unwrap_or_else(|| "1".into());
        
        let mut context = self.library.lock().unwrap_or_else(|e| e.into_inner());
        context.profile = Some(pid.clone());
        context.cached = None;
        
        if let Some(file) = &self.file { save_session_file(file, Some(&session), Some(&pid)); }
        
        match plx_stremio::stremio::get_addons(&session.access_token) {
            Ok(set) => {
                plx_stremio::nuvio::log_account_event(&format!("stremio: Fetched {} addons", set.addons.len()));
                sink(set);
            }
            Err(e) => {
                plx_stremio::nuvio::log_account_event(&format!("stremio: Failed to fetch addons -> {:?}", e));
            }
        }
    }

    fn user(&self) -> Response {
        Response::json(&json!({
            "id": 1, "uuid": "stremio", "title": "Stremio Native", "email": "",
            "thumb": "", "authToken": self.token, "subscription": {"active": true, "features": []}
        }))
    }

    fn resources(&self) -> Response {
        let origin = self.origin.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let uri = if origin.is_empty() { "http://127.0.0.1:32420".to_string() } else { origin };
        
        Response::json(&json!([{
            "name": "Stremio Native",
            "product": "Plex Media Server",
            // FIX: Use the real machine ID that the server actually broadcasts
            "clientIdentifier": crate::MACHINE_ID, 
            "provides": "server",
            "accessToken": self.token,
            "publicAddress": uri.clone(),
            "httpsRequired": false,
            "owned": true,
            "home": false,
            "synced": false,
            "connections": [{
                "protocol": "http",
                "address": "127.0.0.1",
                "port": 32420,
                "uri": uri
            }]
        }]))
    }

    fn home_users(&self) -> Response {
        Response::json(&json!([
            {"id": 1, "uuid": "stremio", "title": "Stremio Native", "thumb": "", "admin": true, "guest": false, "restricted": false}
        ]))
    }

    fn switch(&self, _uuid: &str) -> Response {
        self.load_profile(Some("1"));
        Response::json(&json!({
            "id": 1, "uuid": "stremio", "title": "Stremio Native", "authToken": self.token
        }))
    }
}

pub fn qr_png(text: &str) -> Option<Vec<u8>> {
    let qr = qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium).ok()?;
    let (scale, border) = (8i32, 4i32);
    let side = (qr.size() + border * 2) * scale;
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