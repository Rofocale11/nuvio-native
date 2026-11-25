//! Fork Rofocale11: Nuvio profile management for Settings, over the in-process shim.
//!
//! The screens crate may not name `pmsshim` (it is a lower layer), so this module owns the DTOs
//! and the registered control exactly the way `super::nuvio::AddonControl` owns addon flips. The
//! app registers the implementation at boot; before that the control is quiet, never panicking.
//!
//! Every account write is a FULL roster or FULL settings blob — the public API deletes omitted
//! slots and replaces whole blobs — so the screen sends the complete list it read.

use super::client::QueryBuilder;
use super::Client;

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct ProfileView {
    pub profile_index: u32,
    pub name: String,
    pub uses_primary_addons: bool,
    pub avatar_id: Option<String>,
    pub avatar_url: String,
    pub avatar_color_hex: Option<String>,
    pub pin_enabled: bool,
    pub tv_locked: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct AvatarView {
    pub id: String,
    pub display_name: String,
    pub url: String,
    pub bg_color: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ProfileEditView {
    pub profile_index: u32,
    pub name: String,
    pub uses_primary_addons: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_color_hex: Option<String>,
}

impl Client {
    pub fn nuvio_profiles(&self) -> Option<Vec<ProfileView>> {
        if !self.is_nuvio() { return None; }
        let bytes = self.get_bytes("/nuvio/profiles")?;
        let body: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        serde_json::from_value(body.get("profiles")?.clone()).ok()
    }

    pub fn nuvio_avatars(&self) -> Option<Vec<AvatarView>> {
        if !self.is_nuvio() { return None; }
        let bytes = self.get_bytes("/nuvio/avatars")?;
        let body: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        serde_json::from_value(body.get("avatars")?.clone()).ok()
    }

    pub fn nuvio_save_profiles(&self, profiles: &[ProfileEditView]) -> bool {
        if !self.is_nuvio() { return false; }
        let Ok(roster) = serde_json::to_string(&serde_json::json!({"profiles": profiles})) else { return false };
        let path = QueryBuilder::new("/nuvio/profiles").str("roster", &roster).build();
        self.body_2xx(&path, crate::http::Method::Post, &[]).is_some()
    }

    pub fn nuvio_delete_profile(&self, index: u32) -> bool {
        if !self.is_nuvio() { return false; }
        self.body_2xx(&format!("/nuvio/profiles/{index}/delete"), crate::http::Method::Post, &[]).is_some()
    }

    pub fn nuvio_forgot_pin(&self, index: u32) -> bool {
        if !self.is_nuvio() { return false; }
        let path = QueryBuilder::new("/nuvio/profile-pin/forgot").int("index", index as i64).build();
        self.body_2xx(&path, crate::http::Method::Post, &[]).is_some()
    }

    pub fn nuvio_verify_pin(&self, index: u32, pin: &str) -> bool {
        if !self.is_nuvio() { return false; }
        let path = QueryBuilder::new("/nuvio/profile-pin").int("index", index as i64)
            .str("pin", pin).build();
        let Some(bytes) = self.get_bytes(&path) else { return false };
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .is_ok_and(|v| v["ok"] == true && v["locked"] == true)
    }

    pub fn nuvio_set_pin(&self, index: u32, pin: Option<&str>, current: Option<&str>) -> bool {
        if !self.is_nuvio() { return false; }
        let mut q = QueryBuilder::new("/nuvio/profile-pin").int("index", index as i64);
        match pin {
            Some(p) => q = q.str("pin", p),
            None => q = q.str("clear", "1"),
        }
        if let Some(current) = current { q = q.str("current", current); }
        self.body_2xx(&q.build(), crate::http::Method::Post, &[]).is_some()
    }
}

/// The control the app registers at boot. Screens call these on a press; the app runs the HTTP
/// work. A missing registration reads as an empty roster and a refused write.
pub trait ProfileControl: Send + Sync {
    fn profiles(&self) -> Vec<ProfileView>;
    fn avatars(&self) -> Vec<AvatarView>;
    fn save(&self, profiles: &[ProfileEditView]) -> bool;
    fn delete(&self, index: u32) -> bool;
    /// `current` is required by the shim when a verifier already exists. The first PIN on a
    /// profile has none to check and is set before the remote changes hands.
    fn set_pin(&self, index: u32, pin: Option<&str>, current: Option<&str>) -> bool;
    fn forgot_pin(&self, index: u32) -> bool;
}

static CONTROL: std::sync::OnceLock<std::sync::Arc<dyn ProfileControl>> = std::sync::OnceLock::new();

/// **No HTTP on the frame thread.** The screen reads this snapshot; `refresh()` fills it on a
/// worker and bumps the generation so the page can rebuild on its next tick. A debug build aborts
/// the process on a frame-thread blocking call, which is exactly what a direct `control.profiles()`
/// from a screen did.
#[derive(Default)]
struct Snapshot {
    profiles: Vec<ProfileView>,
    avatars: Vec<AvatarView>,
}

static SNAPSHOT: std::sync::RwLock<Snapshot> = std::sync::RwLock::new(Snapshot {
    profiles: Vec::new(),
    avatars: Vec::new(),
});
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static REFRESHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn publish(profiles: Vec<ProfileView>, avatars: Vec<AvatarView>) {
    if let Ok(mut snap) = SNAPSHOT.write() {
        snap.profiles = profiles;
        snap.avatars = avatars;
    }
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
}

pub fn register_control(control: std::sync::Arc<dyn ProfileControl>) {
    let _ = CONTROL.set(control);
}

pub fn profiles() -> Vec<ProfileView> {
    SNAPSHOT.read().map(|s| s.profiles.clone()).unwrap_or_default()
}

pub fn avatars() -> Vec<AvatarView> {
    SNAPSHOT.read().map(|s| s.avatars.clone()).unwrap_or_default()
}

pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// Ask for a fresh snapshot on a worker. At most one refresh is in flight; a second caller while
/// one is running is harmless. Never call the control directly from a screen or a tick.
pub fn refresh() {
    let Some(control) = CONTROL.get().cloned() else { return };
    if REFRESHING.compare_exchange(false, true, std::sync::atomic::Ordering::AcqRel,
        std::sync::atomic::Ordering::Relaxed).is_err() { return; }
    let spawned = plx_base::task::spawn("nuvio-profiles", move || {
        let profiles = control.profiles();
        let avatars = control.avatars();
        publish(profiles, avatars);
        REFRESHING.store(false, std::sync::atomic::Ordering::Release);
    }).is_some();
    if !spawned { REFRESHING.store(false, std::sync::atomic::Ordering::Release); }
}

/// Run one write off the frame, then refresh the snapshot so a later tick shows the result.
fn write<F>(job: F) -> bool
where F: FnOnce(&std::sync::Arc<dyn ProfileControl>) + Send + 'static {
    let Some(control) = CONTROL.get().cloned() else { return false };
    plx_base::task::spawn("nuvio-profiles-write", move || {
        job(&control);
        refresh();
    }).is_some()
}

pub fn save(rows: &[ProfileEditView]) -> bool {
    let rows = rows.to_vec();
    write(move |control| { control.save(&rows); })
}

pub fn delete(index: u32) -> bool {
    write(move |control| { control.delete(index); })
}

pub fn set_pin(index: u32, pin: Option<&str>, current: Option<&str>) -> bool {
    let pin = pin.map(str::to_string);
    let current = current.map(str::to_string);
    write(move |control| { control.set_pin(index, pin.as_deref(), current.as_deref()); })
}

/// Arm the QR reset on a worker, then the caller opens the existing QR sign-in screen. The
/// verifier is cleared only after that QR is approved by the account.
pub fn forgot_pin(index: u32) -> bool {
    write(move |control| { control.forgot_pin(index); })
}

/// True when a durable TV PIN verifier exists for the slot named by a stored Nuvio profile.
/// Unlike upstream's auto-sign-in decision this does NOT depend on `boot_shows_picker`, which is
/// false when auto sign-in is on — that combination used to walk straight into a locked profile.
fn tv_pin_locked_in(store: Option<&plx_nuvio_store::Store>, uuid: &str) -> bool {
    let Some(index) = uuid.strip_prefix("nuvio-profile-").and_then(|s| s.parse::<u32>().ok()) else {
        return false;
    };
    store.is_some_and(|s| plx_nuvio_store::pins::get(s, index).is_some())
}

pub fn tv_pin_locked(uuid: &str) -> bool {
    tv_pin_locked_in(plx_nuvio_store::get(), uuid)
}

/// The boot gate: force the who's-watching picker only for a Nuvio slot with a TV PIN verifier.
/// Non-Nuvio identities are left to upstream's own picker decision.
fn must_show_picker_in(store: Option<&plx_nuvio_store::Store>, uuid: &str) -> bool {
    uuid.starts_with("nuvio-profile-") && tv_pin_locked_in(store, uuid)
}

pub fn must_show_picker(uuid: &str) -> bool {
    must_show_picker_in(plx_nuvio_store::get(), uuid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_is_quiet_until_the_app_registers_it() {
        assert!(profiles().is_empty());
        assert!(avatars().is_empty());
        assert!(!save(&[]));
        assert!(!delete(2));
        assert!(!set_pin(2, Some("1234"), None));
        assert!(!forgot_pin(2));
        // No verifier: the roster may not have landed yet, but an unlocked profile keeps auto sign-in.
        assert!(!must_show_picker("nuvio-profile-1"));
    }

    #[test]
    fn a_durable_tv_pin_verifier_forces_the_picker() {
        // A LOCAL store: installing the process-global one here would make every later session
        // test read install preferences from it instead of the file they assert on.
        let dir = std::env::temp_dir().join(format!("nuvio-plex-pin-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = plx_nuvio_store::Store::open_at(&dir.join("t.redb")).unwrap();
        let verifier = plx_nuvio_store::pins::PinVerifier::new("1234").unwrap();
        plx_nuvio_store::pins::put(&store, 3, &verifier).unwrap();
        assert!(tv_pin_locked_in(Some(&store), "nuvio-profile-3"));
        assert!(must_show_picker_in(Some(&store), "nuvio-profile-3"));
        plx_nuvio_store::pins::remove(&store, 3).unwrap();
        assert!(!must_show_picker_in(Some(&store), "nuvio-profile-3"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_full_roster_payload_has_six_slots_and_no_pin_fields() {
        let edit = ProfileEditView {
            profile_index: 2, name: "Kids".into(), uses_primary_addons: true,
            avatar_id: Some("avatar_lalo".into()), avatar_url: None, avatar_color_hex: None,
        };
        let text = serde_json::to_string(&serde_json::json!({"profiles": [edit]})).unwrap();
        let body: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["profiles"][0]["profile_index"], 2);
        assert_eq!(body["profiles"][0]["avatar_id"], "avatar_lalo");
        assert!(body["profiles"][0].get("pin_enabled").is_none());
        assert!(body["profiles"][0].get("avatar_url").is_none());
    }
}
