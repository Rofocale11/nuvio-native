//! Import the user's existing Nuvio configuration (addons, and whatever else is cheap) from their
//! Nuvio account, the same Supabase backend the Nuvio webOS app syncs to. CONTRACT: the shape
//! below; the backend agent fills it in from NuvioTVSmart's js/core/profile + js/data sources.

use crate::model::{Addon, Result};

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
}

pub fn start_login() -> Result<DeviceLogin> {
    todo!()
}

/// `Ok(None)` while still pending.
pub fn poll_login(login: &DeviceLogin) -> Result<Option<Session>> {
    let _ = login;
    todo!()
}

pub fn refresh(session: &Session) -> Result<Session> {
    let _ = session;
    todo!()
}

pub fn profiles(session: &Session) -> Result<Vec<Profile>> {
    let _ = session;
    todo!()
}

/// The profile's installed addons in the user's order, manifests included where Nuvio stores
/// them (else fetched).
pub fn addons(session: &Session, profile_id: &str) -> Result<Vec<Addon>> {
    let _ = (session, profile_id);
    todo!()
}
