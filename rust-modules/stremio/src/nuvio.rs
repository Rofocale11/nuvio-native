//! Import the user's existing Nuvio configuration (addons, and whatever else is cheap) from their
//! Nuvio account, the same Supabase backend the Nuvio webOS app syncs to. CONTRACT: the shape
//! below; the backend agent fills it in from NuvioTVSmart's js/core/profile + js/data sources.
//!
//! The protocol, as NuvioTVSmart 1.2.3 speaks it (`js/core/auth/{qrLoginService,authManager}.js`,
//! `js/core/profile/{librarySyncService,profileSyncService}.js`):
//!
//! 1. An **anonymous Supabase session** (`POST /auth/v1/signup`, falling back to
//!    `/auth/v1/token?grant_type=anonymous`) authorises the device-login RPCs.
//! 2. `POST /rest/v1/rpc/start_device_login_session {p_device_nonce, p_redirect_base_url,
//!    p_device_type:"tv", p_device_name}` → `[{device_code, user_code, verification_uri,
//!    verification_uri_complete, expires_at, poll_interval_seconds}]` (legacy servers:
//!    `start_tv_login_session`). The QR encodes `verification_uri_complete`.
//! 3. `POST /rest/v1/rpc/poll_tv_login_session {p_code, p_device_nonce}` → `[{status}]` with
//!    status `pending` | `approved` | `expired` | `used` | `cancelled`.
//! 4. On `approved`, `POST /functions/v1/tv-logins-exchange {code, device_nonce}` → the user's
//!    `access_token`/`refresh_token`.
//! 5. Profiles: `rpc/sync_pull_profiles {}` → rows `{profile_index, name, uses_primary_addons}`.
//! 6. Addons: the owner id is `rpc/get_sync_owner`; rows are
//!    `GET /rest/v1/addons?user_id=eq.<owner>&profile_id=eq.<n>&select=*&order=sort_order.asc`
//!    (`url`, `sort_order`, `enabled`, `name`), falling back to the legacy `tv_addons` table and
//!    then `rpc/sync_pull_addons {p_profile_id}`. A profile with `uses_primary_addons` reads
//!    profile 1's list. Rows carry URLs only; manifests are fetched here.
//!
//! Every request carries `apikey: <publishable key>`. The backend URL and publishable key are the
//! public values Nuvio's own bundle ships; this crate reads them at BUILD time from
//! `NUVIO_SUPABASE_URL` / `NUVIO_SUPABASE_ANON_KEY` (+ `NUVIO_SUPABASE_FALLBACK_URL`,
//! `DEVICE_LOGIN_WEB_BASE_URL`, `TV_LOGIN_WEB_BASE_URL`), which the Makefile exports from the
//! gitignored `rust-modules/stremio/nuvio.local.env`; [`configure`] overrides them at run time.
//! Account writes are the device-login handshake and explicit My List item mutations.

use std::sync::Mutex;
use std::time::Duration;

use serde_json::{json, Value};

use crate::addons::{self, AddonSet};
use crate::addon_http as http;
use crate::model::{Addon, Error, Manifest, Result};

/// A pending TV sign-in: show `qr_url` as a QR code and `user_code` as text.
#[derive(Clone, Debug)]
pub struct DeviceLogin {
    pub qr_url: String,
    pub user_code: String,
    pub poll_interval_secs: u32,
    /// Opaque; pass back to [`poll_login`].
    pub handle: String,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    pub user_id: String,
    pub expires_at_unix: u64,
}

#[derive(Clone, Debug)]
pub struct Profile {
    pub id: String,
    pub name: String,
    /// The profile shows the primary profile's addons (Nuvio's `uses_primary_addons`).
    pub uses_primary_addons: bool,
}

/// Where the Nuvio account backend lives.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub backend_url: String,
    pub fallback_url: String,
    pub publishable_key: String,
    pub device_login_base_url: String,
    pub tv_login_base_url: String,
}

static OVERRIDE: Mutex<Option<Config>> = Mutex::new(None);

/// Replace the build-time configuration (e.g. from a settings file).
pub fn configure(config: Config) {
    *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = Some(config);
}

pub fn config() -> Config {
    if let Some(c) = OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return c;
    }
    Config {
        backend_url: option_env!("NUVIO_SUPABASE_URL").unwrap_or("").trim_end_matches('/').to_string(),
        fallback_url: option_env!("NUVIO_SUPABASE_FALLBACK_URL").unwrap_or("").trim_end_matches('/').to_string(),
        publishable_key: option_env!("NUVIO_SUPABASE_ANON_KEY").unwrap_or("").to_string(),
        device_login_base_url: option_env!("DEVICE_LOGIN_WEB_BASE_URL").unwrap_or("").to_string(),
        tv_login_base_url: option_env!("TV_LOGIN_WEB_BASE_URL").unwrap_or("").to_string(),
    }
}

/// Whether this build can talk to the Nuvio account backend at all.
pub fn available() -> bool {
    let c = config();
    !c.backend_url.is_empty() && !c.publishable_key.is_empty()
}

fn require_config() -> Result<Config> {
    let c = config();
    if c.backend_url.is_empty() || c.publishable_key.is_empty() {
        return Err(Error::Parse("Nuvio account backend is not configured in this build".into()));
    }
    Ok(c)
}

// --- transport ----------------------------------------------------------------------------------

/// One call against the backend, retried once on the fallback host when the primary is
/// unreachable or answers a gateway error (Nuvio's `fetchSupabaseAuth`).
fn call(c: &Config, verb: &str, path: &str, bearer: Option<&str>, body: Option<&Value>) -> Result<(u16, Value)> {
    let mut headers = vec![format!("apikey: {}", c.publishable_key)];
    headers.push(format!("Authorization: Bearer {}", bearer.unwrap_or(&c.publishable_key)));
    let bytes = body.map(|b| serde_json::to_vec(b).unwrap_or_default());
    let mut hosts = vec![c.backend_url.as_str()];
    if !c.fallback_url.is_empty() && !c.fallback_url.eq_ignore_ascii_case(&c.backend_url) {
        hosts.push(c.fallback_url.as_str());
    }
    let mut last = Err(Error::Network);
    for host in hosts {
        let url = format!("{host}{path}");
        match http::request_status(&url, verb, &headers, bytes.as_deref(), http::CONTROL) {
            Ok((status, raw)) => {
                if !(200..300).contains(&status) {
                    let endpoint = path.split('?').next().unwrap_or("/unknown");
                    plx_base::eventlog::log(&format!("nuvio account: {verb} {endpoint} status={status}"));
                }
                let retryable = matches!(status, 408 | 500 | 502 | 503 | 504 | 520..=526 | 530);
                let v = serde_json::from_slice(&raw).unwrap_or(Value::Null);
                last = Ok((status, v));
                if !retryable {
                    break;
                }
            }
            Err(e) => last = Err(e),
        }
    }
    last
}

fn ok(r: (u16, Value)) -> Result<Value> {
    if (200..300).contains(&r.0) {
        Ok(r.1)
    } else {
        Err(Error::Status(r.0))
    }
}

fn first_row(v: Value) -> Value {
    match v {
        Value::Array(mut a) if !a.is_empty() => a.swap_remove(0),
        other => other,
    }
}

fn str_field(v: &Value, keys: &[&str]) -> String {
    keys.iter().find_map(|k| v.get(*k).and_then(|x| match x {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })).unwrap_or_default()
}

fn bool_field(v: &Value, keys: &[&str], default: bool) -> bool {
    for k in keys {
        match v.get(*k) {
            Some(Value::Bool(b)) => return *b,
            Some(Value::Number(n)) => return n.as_f64().unwrap_or(0.0) != 0.0,
            Some(Value::String(s)) => {
                let s = s.trim().to_ascii_lowercase();
                if s == "true" || s == "1" {
                    return true;
                }
                if s == "false" || s == "0" {
                    return false;
                }
            }
            _ => {}
        }
    }
    default
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The JWT payload's `sub` and `exp`.
fn jwt_claims(token: &str) -> (Option<String>, Option<u64>) {
    let Some(payload) = token.split('.').nth(1) else { return (None, None) };
    let mut b64: String = payload.chars().map(|c| match c { '-' => '+', '_' => '/', c => c }).collect();
    while !b64.len().is_multiple_of(4) {
        b64.push('=');
    }
    let Some(bytes) = plx_base::b64::decode(&b64) else { return (None, None) };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { return (None, None) };
    (v.get("sub").and_then(Value::as_str).map(str::to_string), v.get("exp").and_then(Value::as_u64))
}

fn session_from(v: &Value) -> Option<Session> {
    let src = if v.get("access_token").is_some() { v } else { v.get("session").unwrap_or(v) };
    let access = str_field(src, &["access_token", "accessToken"]);
    let refresh = str_field(src, &["refresh_token", "refreshToken"]);
    if access.is_empty() || refresh.is_empty() {
        return None;
    }
    let (sub, exp) = jwt_claims(&access);
    let user_id = src
        .get("user")
        .and_then(|u| u.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(sub)
        .unwrap_or_default();
    let expires = src
        .get("expires_at")
        .and_then(Value::as_u64)
        .or(exp)
        .or_else(|| src.get("expires_in").and_then(Value::as_u64).map(|s| now_unix() + s))
        .unwrap_or_else(|| now_unix() + 3600);
    Some(Session { access_token: access, refresh_token: refresh, user_id, expires_at_unix: expires })
}

fn nonce() -> String {
    use std::io::Read;
    let mut bytes = [0u8; 24];
    let filled = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_ok();
    if !filled {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let digest = plx_base::sha256::sha256(format!("{seed}-{}", std::process::id()).as_bytes());
        bytes.copy_from_slice(&digest[..24]);
    }
    plx_base::b64::encode(&bytes).replace('+', "-").replace('/', "_").trim_end_matches('=').to_string()
}

// --- device login -------------------------------------------------------------------------------

fn anonymous_session(c: &Config) -> Result<Session> {
    let signup = call(c, "POST", "/auth/v1/signup", None, Some(&json!({"data": {"tv_client": "webos-native"}})));
    if let Ok((s, v)) = &signup {
        if (200..300).contains(s) {
            if let Some(sess) = session_from(v) {
                return Ok(sess);
            }
        }
    }
    let v = ok(call(c, "POST", "/auth/v1/token?grant_type=anonymous", None, Some(&json!({})))?)?;
    session_from(&v).ok_or_else(|| Error::Parse("anonymous auth returned no session".into()))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Handle {
    code: String,
    nonce: String,
    anon_access: String,
    anon_refresh: String,
}

pub fn start_login() -> Result<DeviceLogin> {
    let c = require_config()?;
    let anon = anonymous_session(&c)?;
    let device_nonce = nonce();
    let redirect = if c.device_login_base_url.is_empty() { c.tv_login_base_url.clone() } else { c.device_login_base_url.clone() };
    let body = json!({
        "p_device_nonce": device_nonce,
        "p_redirect_base_url": redirect,
        "p_device_type": "tv",
        "p_device_name": "LG webOS TV (Nuvio Native)",
    });
    let (status, v) = call(&c, "POST", "/rest/v1/rpc/start_device_login_session", Some(&anon.access_token), Some(&body))?;
    let (code, user_code, qr_url, interval) = if (200..300).contains(&status) {
        let row = first_row(v);
        (
            str_field(&row, &["device_code"]),
            str_field(&row, &["user_code"]),
            str_field(&row, &["verification_uri_complete"]),
            row.get("poll_interval_seconds").and_then(Value::as_u64).unwrap_or(3),
        )
    } else {
        // Older backends only have the TV-specific RPC.
        let legacy_redirect = if c.tv_login_base_url.is_empty() { redirect.clone() } else { c.tv_login_base_url.clone() };
        let body = json!({"p_device_nonce": device_nonce, "p_redirect_base_url": legacy_redirect, "p_device_name": "LG webOS TV (Nuvio Native)"});
        let row = first_row(ok(call(&c, "POST", "/rest/v1/rpc/start_tv_login_session", Some(&anon.access_token), Some(&body))?)?);
        (
            str_field(&row, &["device_code", "deviceCode", "code"]),
            str_field(&row, &["user_code", "userCode", "code"]),
            str_field(&row, &["verification_uri_complete", "qr_content", "web_url"]),
            row.get("poll_interval_seconds").and_then(Value::as_u64).unwrap_or(3),
        )
    };
    if code.is_empty() || user_code.is_empty() || qr_url.is_empty() {
        return Err(Error::Parse("incomplete device login response".into()));
    }
    let handle = serde_json::to_string(&Handle {
        code,
        nonce: device_nonce,
        anon_access: anon.access_token,
        anon_refresh: anon.refresh_token,
    })
    .map_err(|e| Error::Parse(e.to_string()))?;
    plx_base::eventlog::log("nuvio account: QR session created");
    Ok(DeviceLogin { qr_url, user_code, poll_interval_secs: interval.clamp(1, 30) as u32, handle })
}

/// `Ok(None)` while still pending.
pub fn poll_login(login: &DeviceLogin) -> Result<Option<Session>> {
    let c = require_config()?;
    let h: Handle = serde_json::from_str(&login.handle).map_err(|e| Error::Parse(e.to_string()))?;
    let mut bearer = h.anon_access.clone();
    let body = json!({"p_code": h.code, "p_device_nonce": h.nonce});
    let mut res = call(&c, "POST", "/rest/v1/rpc/poll_tv_login_session", Some(&bearer), Some(&body))?;
    if res.0 == 401 {
        // The anonymous token lapsed; refresh it and retry once.
        let anon = Session { access_token: h.anon_access.clone(), refresh_token: h.anon_refresh.clone(), user_id: String::new(), expires_at_unix: 0 };
        bearer = refresh(&anon)?.access_token;
        res = call(&c, "POST", "/rest/v1/rpc/poll_tv_login_session", Some(&bearer), Some(&body))?;
    }
    let row = first_row(ok(res)?);
    match str_field(&row, &["status"]).as_str() {
        "approved" => { plx_base::eventlog::log("nuvio account: QR approved; exchanging session"); }
        "pending" | "" => return Ok(None),
        other => {
            let state = match other { "expired" => "expired", "used" => "used", "cancelled" => "cancelled", _ => "unknown" };
            plx_base::eventlog::log(&format!("nuvio account: QR state={state}"));
            return Err(Error::Parse(format!("login {state}")));
        },
    }
    let v = ok(call(&c, "POST", "/functions/v1/tv-logins-exchange", Some(&bearer), Some(&json!({"code": h.code, "device_nonce": h.nonce})))?)?;
    session_from(&v).map(Some).ok_or_else(|| Error::Parse("login exchange returned no session".into()))
}

pub fn refresh(session: &Session) -> Result<Session> {
    let c = require_config()?;
    let v = ok(call(&c, "POST", "/auth/v1/token?grant_type=refresh_token", None, Some(&json!({"refresh_token": session.refresh_token})))?)?;
    let mut s = session_from(&v).ok_or_else(|| Error::Parse("refresh returned no session".into()))?;
    if s.user_id.is_empty() {
        s.user_id = session.user_id.clone();
    }
    Ok(s)
}

/// `session` itself if it has more than a minute left, else a refreshed one.
pub fn fresh(session: &Session) -> Result<Session> {
    if session.expires_at_unix > now_unix() + 60 {
        Ok(session.clone())
    } else {
        refresh(session)
    }
}

// --- profiles and addons ------------------------------------------------------------------------

fn owner_id(c: &Config, s: &Session) -> Result<String> {
    let v = ok(call(c, "POST", "/rest/v1/rpc/get_sync_owner", Some(&s.access_token), Some(&json!({})))?)?;
    match v {
        Value::String(id) if !id.is_empty() => Ok(id),
        other => {
            let id = str_field(&first_row(other), &["get_sync_owner", "id", "owner_id", "user_id"]);
            if id.is_empty() {
                Ok(s.user_id.clone())
            } else {
                Ok(id)
            }
        }
    }
}

fn map_profile(row: &Value) -> Profile {
    let idx = str_field(row, &["profile_index", "profileIndex", "id"]);
    let idx = idx.parse::<u32>().ok().filter(|n| *n > 0).unwrap_or(1);
    let name = str_field(row, &["name"]);
    Profile {
        id: idx.to_string(),
        name: if name.is_empty() { format!("Profile {idx}") } else { name },
        uses_primary_addons: bool_field(row, &["uses_primary_addons", "usesPrimaryAddons"], false),
    }
}

pub fn profiles(session: &Session) -> Result<Vec<Profile>> {
    let c = require_config()?;
    let rows = match call(&c, "POST", "/rest/v1/rpc/sync_pull_profiles", Some(&session.access_token), Some(&json!({})))? {
        (s, v) if (200..300).contains(&s) => v,
        (404, _) => {
            let owner = owner_id(&c, session)?;
            let q = format!("/rest/v1/profiles?user_id=eq.{}&select=*&order=profile_index.asc", http::encode_component(&owner));
            ok(call(&c, "GET", &q, Some(&session.access_token), None)?)?
        }
        (s, _) => return Err(Error::Status(s)),
    };
    let mut out: Vec<Profile> = rows.as_array().map(|a| a.iter().map(map_profile).collect()).unwrap_or_default();
    out.sort_by_key(|p| p.id.parse::<u32>().unwrap_or(u32::MAX));
    out.dedup_by(|a, b| a.id == b.id);
    if out.is_empty() {
        out.push(Profile { id: "1".into(), name: "Profile 1".into(), uses_primary_addons: false });
    }
    Ok(out)
}

/// One addon row as Nuvio stores it.
#[derive(Clone, Debug, PartialEq)]
pub struct AddonRow {
    pub url: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub sort_order: i64,
}

/// Nuvio's `extractAddonEntries`: sort by `sort_order` (then source order), drop rows without a
/// URL, dedupe by canonical URL.
pub fn addon_rows(rows: &Value) -> Vec<AddonRow> {
    let mut out: Vec<(i64, usize, AddonRow)> = rows
        .as_array()
        .map(|a| {
            a.iter()
                .enumerate()
                .filter_map(|(i, r)| {
                    let url = str_field(r, &["url", "base_url"]);
                    if url.is_empty() {
                        return None;
                    }
                    let order = ["sort_order", "sortOrder", "position"]
                        .iter()
                        .find_map(|k| r.get(*k).and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))))
                        .unwrap_or(i as i64);
                    let name = str_field(r, &["display_name", "displayName", "custom_name", "customName", "alias", "name"]);
                    Some((order, i, AddonRow { url, display_name: (!name.is_empty()).then_some(name), enabled: bool_field(r, &["enabled"], true), sort_order: order }))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by_key(|(o, i, _)| (*o, *i));
    let mut seen = std::collections::HashSet::new();
    out.into_iter().map(|(_, _, r)| r).filter(|r| seen.insert(addons::url_key(&r.url))).collect()
}

fn fetch_rows(c: &Config, s: &Session, profile_index: u32) -> Result<Value> {
    let owner = owner_id(c, s)?;
    let owner_q = http::encode_component(&owner);
    let q = format!("/rest/v1/addons?user_id=eq.{owner_q}&profile_id=eq.{profile_index}&select=*&order=sort_order.asc");
    match call(c, "GET", &q, Some(&s.access_token), None)? {
        (st, v) if (200..300).contains(&st) => return Ok(v),
        (404, _) => {}
        (st, _) => return Err(Error::Status(st)),
    }
    let q = format!("/rest/v1/tv_addons?owner_id=eq.{owner_q}&select=*&order=position.asc");
    match call(c, "GET", &q, Some(&s.access_token), None)? {
        (st, v) if (200..300).contains(&st) => return Ok(v),
        (404, _) => {}
        (st, _) => return Err(Error::Status(st)),
    }
    ok(call(c, "POST", "/rest/v1/rpc/sync_pull_addons", Some(&s.access_token), Some(&json!({"p_profile_id": profile_index})))?)
}

/// Turn rows into addons, fetching manifests in parallel. A row whose manifest cannot be fetched
/// is kept (so the user's list survives a flaky addon) with an empty manifest named after it,
/// except the two Nuvio defaults, which fall back to their built-in manifests.
pub fn addons_from_rows(rows: &[AddonRow], deadline: Duration) -> Vec<Addon> {
    let mut set = AddonSet {
        addons: rows
            .iter()
            .map(|r| Addon {
                transport_url: addons::canonicalize_url(&r.url),
                manifest: Manifest::default(),
                disabled: !r.enabled,
                display_name: r.display_name.clone(),
            })
            .collect(),
    };
    set.refresh_manifests(deadline);
    for a in set.addons.iter_mut() {
        if !a.manifest.id.is_empty() {
            continue;
        }
        let key = addons::url_key(&a.transport_url);
        if key == addons::url_key(addons::CINEMETA_URL) {
            a.manifest = addons::builtin_cinemeta().manifest;
        } else if key == addons::url_key(addons::OPENSUBTITLES_URL) {
            a.manifest = addons::builtin_opensubtitles().manifest;
        } else {
            a.manifest.name = a.display_name.clone().unwrap_or_else(|| host_of(&a.transport_url));
        }
    }
    set.addons
}

fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or(url).to_string()
}

/// The profile's installed addons in the user's order, manifests included where Nuvio stores
/// them (else fetched).
pub fn addons(session: &Session, profile_id: &str) -> Result<Vec<Addon>> {
    let c = require_config()?;
    let s = fresh(session)?;
    let mut index = profile_id.trim().parse::<u32>().ok().filter(|n| *n > 0).unwrap_or(1);
    if index != 1 {
        if let Ok(ps) = profiles(&s) {
            if ps.iter().any(|p| p.id == index.to_string() && p.uses_primary_addons) {
                index = 1;
            }
        }
    }
    let rows = addon_rows(&fetch_rows(&c, &s, index)?);
    Ok(addons_from_rows(&rows, Duration::from_secs(15)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_sort_dedupe_and_map() {
        let v = json!([
            {"url": "https://torrentio.strem.fun/rd=KEY/manifest.json", "sort_order": 2, "enabled": true},
            {"url": "https://v3-cinemeta.strem.io", "sort_order": 0, "enabled": "true", "name": "Meta"},
            {"url": "https://cinemeta-v3.strem.io/manifest.json", "sort_order": 1},
            {"base_url": "https://aio.example/stremio/abc", "position": 3, "enabled": 0},
            {"name": "no url"}
        ]);
        let rows = addon_rows(&v);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].display_name.as_deref(), Some("Meta"));
        assert!(rows[1].url.contains("torrentio"));
        assert!(!rows[2].enabled);
    }

    #[test]
    fn sessions_from_supabase_shapes() {
        // header.payload.sig with payload {"sub":"u-1","exp":2000000000}
        let payload = plx_base::b64::encode(br#"{"sub":"u-1","exp":2000000000}"#).replace('=', "").replace('+', "-").replace('/', "_");
        let tok = format!("eyJhbGciOiJIUzI1NiJ9.{payload}.sig");
        let s = session_from(&json!({"access_token": tok, "refresh_token": "r"})).unwrap();
        assert_eq!(s.user_id, "u-1");
        assert_eq!(s.expires_at_unix, 2_000_000_000);
        let s = session_from(&json!({"session": {"access_token": "a.b.c", "refresh_token": "r", "expires_at": 5, "user": {"id": "x"}}})).unwrap();
        assert_eq!((s.user_id.as_str(), s.expires_at_unix), ("x", 5));
        assert!(session_from(&json!({"access_token": "a"})).is_none());
    }

    #[test]
    fn profiles_map() {
        let p = map_profile(&json!({"profile_index": 2, "name": "Kids", "uses_primary_addons": "true"}));
        assert_eq!((p.id.as_str(), p.name.as_str(), p.uses_primary_addons), ("2", "Kids", true));
        let p = map_profile(&json!({}));
        assert_eq!((p.id.as_str(), p.name.as_str()), ("1", "Profile 1"));
    }

    #[test]
    fn nonce_is_urlsafe() {
        let n = nonce();
        assert_eq!(n.len(), 32);
        assert!(n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(n, nonce());
    }

    #[test]
    fn unconfigured_build_says_so() {
        configure(Config::default());
        assert!(!available());
        assert!(matches!(start_login(), Err(Error::Parse(_))));
        *OVERRIDE.lock().unwrap() = None;
    }
}

/// Safe account diagnostics supplied by the shim: callers must pass only fixed categories/statuses.
pub fn log_account_event(message: &str) { plx_base::eventlog::log(message); }

/// Account-backed My List, paged with the same RPC as NuvioTVSmart.
/// These rows may include private addon fields; callers must never log the raw response.
pub fn saved_library(session: &Session, profile: &str) -> Result<Vec<crate::model::MetaPreview>> {
    let c = require_config()?;
    let pid = library_profile(profile)?;
    let mut items = Vec::new();
    for offset in (0..100_000).step_by(500) {
        let v = ok(call(&c, "POST", "/rest/v1/rpc/sync_pull_library", Some(&session.access_token),
            Some(&json!({"p_profile_id":pid,"p_limit":500,"p_offset":offset})))?)?;
        let rows = v.as_array().ok_or_else(|| Error::Parse("invalid saved library page".into()))?;
        items.extend(rows.iter().filter_map(library_preview));
        if rows.len() < 500 { return Ok(items); }
    }
    Err(Error::Parse("saved library pagination exceeded limit".into()))
}

fn library_profile(profile: &str) -> Result<u32> {
    profile.parse::<u32>().ok().filter(|n| *n > 0)
        .ok_or_else(|| Error::Parse("invalid library profile".into()))
}

fn library_preview(v: &Value) -> Option<crate::model::MetaPreview> {
    let id = str_field(v, &["content_id", "contentId"]);
    let type_ = str_field(v, &["content_type", "contentType"]);
    if id.is_empty() || !matches!(type_.as_str(), "movie" | "series") { return None; }
    Some(crate::model::MetaPreview {
        id, type_, name: str_field(v, &["name", "title"]),
        poster: v["poster"].as_str().map(str::to_string),
        background: v["background"].as_str().map(str::to_string),
        logo: v["logo"].as_str().map(str::to_string),
        description: v["description"].as_str().map(str::to_string),
        release_info: v["release_info"].as_str().map(str::to_string),
        imdb_rating: match &v["imdb_rating"] { Value::Number(n) => Some(n.to_string()), Value::String(s) => Some(s.clone()), _ => None },
        genres: v["genres"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default(),
        ..Default::default()
    })
}

/// Mutate ONE item, rather than replacing the whole library and overwriting another device's changes.
/// The item/delta RPC signatures are verified against the deployed Nuvio backend's OpenAPI schema.
pub fn save_library_item(session: &Session, profile: &str, meta: &crate::model::Meta, saved: bool) -> Result<()> {
    let c = require_config()?;
    let (path, payload) = library_write(profile, meta, saved)?;
    ok(call(&c, "POST", path, Some(&session.access_token), Some(&payload))?)?;
    Ok(())
}

fn library_write(profile: &str, meta: &crate::model::Meta, saved: bool) -> Result<(&'static str, Value)> {
    let pid = library_profile(profile)?;
    if !matches!(meta.type_.as_str(), "movie" | "series") || meta.id.is_empty() || meta.id.len() > 512 || meta.id.chars().any(char::is_control) {
        return Err(Error::Parse("invalid saved library item".into()));
    }
    let common = json!({"p_profile_id":pid,"p_origin_client_id":"nuvio-native"});
    let mut payload = common;
    if saved {
        payload["p_items"] = json!([{
            "content_id":meta.id,"content_type":meta.type_,"name":meta.name,
            "poster":meta.poster,"poster_shape":"POSTER","background":meta.background,"logo":meta.logo,
            "description":meta.description,"release_info":meta.release_info,
            "imdb_rating":meta.imdb_rating.as_deref().and_then(|s| s.parse::<f64>().ok()).filter(|n| n.is_finite()),
            "genres":meta.genres,"addon_base_url":null,"added_at":now_unix()*1000
        }]);
        Ok(("/rest/v1/rpc/sync_push_library_items", payload))
    } else {
        payload["p_keys"] = json!([{"content_id":meta.id,"content_type":meta.type_}]);
        Ok(("/rest/v1/rpc/sync_delete_library_items", payload))
    }
}

#[cfg(test)]
mod library_tests {
    use super::*;
    #[test]
    fn single_item_writes_are_profile_scoped_and_do_not_replace_other_titles() {
        let m = crate::model::Meta { id:"tt0133093".into(),type_:"movie".into(),name:"The Matrix".into(),..Default::default() };
        let (path, body) = library_write("2", &m, true).unwrap();
        assert_eq!(path,"/rest/v1/rpc/sync_push_library_items");
        assert_eq!(body["p_profile_id"],2);
        assert_eq!(body["p_items"].as_array().unwrap().len(),1);
        assert!(body["p_items"][0]["addon_base_url"].is_null());
        let (path, body) = library_write("2", &m, false).unwrap();
        assert_eq!(path,"/rest/v1/rpc/sync_delete_library_items");
        assert_eq!(body["p_keys"],json!([{"content_id":"tt0133093","content_type":"movie"}]));
        assert!(library_write("0",&m,true).is_err());
        assert!(library_write("1",&crate::model::Meta::default(),true).is_err());
    }
    #[test]
    fn library_rows_map_without_exposing_addon_configuration() {
        let p = library_preview(&json!({"content_id":"tt0133093","content_type":"movie","name":"The Matrix","imdb_rating":8.7,"genres":["Action"],"profile_id":2})).unwrap();
        assert_eq!(p.imdb_rating.as_deref(),Some("8.7"));
        assert_eq!(p.type_,"movie");
        assert!(library_preview(&json!({"name":"missing identity"})).is_none());
    }
}
