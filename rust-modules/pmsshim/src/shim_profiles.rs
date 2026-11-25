//! Pure profile rules shared by the Plex-dialect roster and the fork-owned Nuvio management
//! routes. Nothing here performs IO: the shim fetches the roster and catalog, then asks these
//! functions to build the answer, so the shape is gradable on the host.
//!
//! The account's `pin_enabled` is READ-ONLY advisory state. It never gates entry: the public API
//! has no PIN verification call, so a single-profile account marked locked would otherwise have
//! no way at all to open its one profile. A tile is protected on this TV only when a TV-side
//! verifier exists in the one store.

use plx_stremio::nuvio::{Avatar, Profile, ProfileEdit};
use serde_json::{json, Map, Value};

pub const PROFILE_UUID_PREFIX: &str = "nuvio-profile-";

pub fn uuid(index: u32) -> String {
    format!("{PROFILE_UUID_PREFIX}{index}")
}

/// The Plex switch route carries a uuid; translate it to the integer Nuvio slot and refuse
/// anything outside the documented 1..=6 range.
/// The account's display name for a slot. The switch reply's `title` is what the app writes as
/// the seated profile's name, so this must be the profile row's `name`, never the product name.
pub fn profile_title(profiles: &[Profile], index: u32) -> Option<String> {
    profiles.iter().find(|p| p.index == index).map(|p| p.name.clone())
}

pub fn index_of(uuid: &str) -> Option<u32> {
    let text = uuid.strip_prefix(PROFILE_UUID_PREFIX)?;
    let index = text.parse::<u32>().ok()?;
    (1..=plx_stremio::nuvio::MAX_PROFILES).contains(&index).then_some(index)
}

/// A custom URL wins; otherwise resolve the catalog entry's storage path against the backend's
/// public storage bucket. A colour is a tint, not a picture, so it is never returned here.
pub fn avatar_url(base: &str, profile: &Profile, catalog: &[Avatar]) -> String {
    if let Some(url) = &profile.avatar_url {
        if !url.is_empty() {
            return url.clone();
        }
    }
    let Some(id) = &profile.avatar_id else { return String::new() };
    let Some(avatar) = catalog.iter().find(|a| a.id == *id && a.is_active) else {
        return String::new();
    };
    plx_stremio::nuvio::avatar_image_url(base, &avatar.storage_path)
}

/// `{"users": [...]}` — the envelope `plex::account::HomeUsers` parses. A bare array reads as an
/// unparseable 200 and silently seats the default profile instead of showing the picker.
pub fn home_users(
    profiles: &[Profile],
    catalog: &[Avatar],
    base: &str,
    token: &str,
    locked: &dyn Fn(u32) -> bool,
) -> Value {
    let users: Vec<Value> = profiles.iter().map(|p| {
        json!({
            "id": p.index,
            "uuid": uuid(p.index),
            "title": p.name,
            "thumb": avatar_url(base, p, catalog),
            "admin": p.index == 1,
            "guest": false,
            "restricted": false,
            "protected": locked(p.index),
            "authToken": token,
            "plex_tv_token": token,
        })
    }).collect();
    json!({ "users": users })
}

/// Parse the Settings screen's full-roster payload. The client never sends PIN fields: the API
/// keeps them and this client cannot set them.
pub fn profile_edits_from_json(v: &Value) -> Option<Vec<ProfileEdit>> {
    let rows = v.get("profiles")?.as_array()?;
    rows.iter().map(|r| {
        let index = r.get("profile_index")?.as_u64()?;
        let name = r.get("name")?.as_str()?.to_string();
        let text = |key: &str| r.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
        Some(ProfileEdit {
            index: u32::try_from(index).ok()?,
            name,
            uses_primary_addons: r.get("uses_primary_addons").and_then(Value::as_bool).unwrap_or(false),
            avatar_id: text("avatar_id"),
            avatar_url: text("avatar_url"),
            avatar_color_hex: text("avatar_color_hex"),
        })
    }).collect()
}

/// When a profile is pulled in, the account blob wins on keys it holds; local-only keys survive
/// so a write that failed to reach the account is not silently dropped.
pub fn merge_settings(remote: &Map<String, Value>, local: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = local.clone();
    for (key, value) in remote {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(index: u32, name: &str) -> Profile {
        Profile { id: index.to_string(), index, name: name.into(), ..Default::default() }
    }

    #[test]
    fn switch_uuids_round_trip_within_the_documented_range() {
        for n in 1..=6 {
            assert_eq!(index_of(&uuid(n)), Some(n));
        }
        assert_eq!(index_of("nuvio"), None);
        assert_eq!(index_of("nuvio-profile-0"), None);
        assert_eq!(index_of("nuvio-profile-7"), None);
        assert_eq!(index_of("nuvio-profile-x"), None);
        assert_eq!(index_of("other-profile-2"), None);
    }

    #[test]
    fn a_switch_names_the_profile_not_the_product() {
        let rows = vec![profile(1, "Main"), profile(2, "Kids")];
        assert_eq!(super::profile_title(&rows, 2).as_deref(), Some("Kids"));
        assert_eq!(super::profile_title(&rows, 9), None);
    }

    #[test]
    fn roster_maps_every_profile_and_uses_only_the_tv_verifier_for_the_lock() {
        let mut kids = profile(2, "Kids");
        kids.avatar_id = Some("avatar_lalo".into());
        kids.pin_enabled = true; // account flag is advisory only
        let catalog = vec![Avatar {
            id: "avatar_lalo".into(), display_name: "Bram".into(),
            storage_path: "animals/bram v1.png".into(), bg_color: None, is_active: true,
        }];
        let body = home_users(&[profile(1, "Main"), kids], &catalog, "https://api.nuvio.tv", "tok",
            &|n| n == 3);
        let users = body["users"].as_array().unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0]["uuid"], "nuvio-profile-1");
        assert_eq!(users[0]["id"], 1);
        assert_eq!(users[0]["admin"], true);
        assert_eq!(users[0]["protected"], false);
        assert_eq!(users[0]["thumb"], "");
        // the account's lock flag did not mark this tile
        assert_eq!(users[1]["protected"], false);
        assert_eq!(users[1]["thumb"],
            "https://api.nuvio.tv/storage/v1/object/public/avatars/animals/bram%20v1.png");
        assert_eq!(users[1]["title"], "Kids");
        // only a TV verifier produces a lock
        let locked = home_users(&[profile(3, "Parent")], &[], "", "tok", &|n| n == 3);
        assert_eq!(locked["users"][0]["protected"], true);
    }

    #[test]
    fn a_custom_avatar_url_beats_the_catalog() {
        let mut p = profile(1, "Main");
        p.avatar_url = Some("https://example.invalid/face.png".into());
        p.avatar_id = Some("avatar_lalo".into());
        assert_eq!(avatar_url("https://api.nuvio.tv", &p, &[]), "https://example.invalid/face.png");
    }

    #[test]
    fn a_management_payload_parses_only_full_profile_rows() {
        let payload = json!({"profiles": [
            {"profile_index": 1, "name": "Main", "uses_primary_addons": false,
             "avatar_id": "avatar_lalo", "pin_enabled": true},
            {"profile_index": 2, "name": "Kids", "uses_primary_addons": true}
        ]});
        let edits = profile_edits_from_json(&payload).unwrap();
        assert_eq!(edits.len(), 2);
        assert_eq!(edits[0].avatar_id.as_deref(), Some("avatar_lalo"));
        assert!(edits[1].uses_primary_addons);
        assert!(profile_edits_from_json(&json!({"profiles": [{"name": "no index"}]})).is_none());
        assert!(profile_edits_from_json(&json!({"profiles": "not an array"})).is_none());
    }

    #[test]
    fn remote_settings_win_without_dropping_local_only_keys() {
        let mut remote = Map::new();
        remote.insert("defaultSubtitleLanguage".into(), json!("fr"));
        let mut local = Map::new();
        local.insert("defaultSubtitleLanguage".into(), json!("en"));
        local.insert("addonEnabled".into(), json!({}));
        let merged = merge_settings(&remote, &local);
        assert_eq!(merged["defaultSubtitleLanguage"], "fr");
        assert_eq!(merged["addonEnabled"], json!({}));
    }
}
