//! **The Watchlist's I/O seam, answered by the shim's My List** (tier C's data boundary).
//!
//! Upstream's Watchlist page is built over the plex.tv Watchlist
//! (`discover.provider.plex.tv/library/sections/watchlist/all`). This fork has no plex.tv
//! credential: its list is the Nuvio account's saved library, and the in-process PMS shim is where
//! that boundary lives. This implementation is the whole translation, in the shim's terms:
//!
//! * **Identity.** A My List title's portable catalog id is the shim's numeric `ratingKey`
//!   (`docs/nuvio/ID-MANAGEMENT.md`), encoded as `NVIO` + the key in the same 12 bytes a discover
//!   id uses. Home, Detail and the Watchlist all derive the same value, so one membership answer
//!   serves all three.
//! * **Rows.** `page` reads the account's saved library through the shim and states each title as a
//!   `WatchItem`. The page's row state is "matched on the shim": `find` answers every title as held
//!   by it, so a tile is playable and opens the shim-backed Detail rather than a discover catalog.
//! * **Writes.** `write` maps the 12-byte id back to the key and calls the shim's own save/remove,
//!   the same path `/library/metadata/{rk}` takes.
//!
//! Nothing here reaches plex.tv. A signed-out account has no list, exactly as the page's
//! "unavailable" read-out says.
use plx_data::watchlist::ledger::Intent;
use plx_data::watchlist::model::Reach;
use plx_data::watchlist::{LiveIo, WatchlistAuth, WatchlistIo};
use plx_plex::plex::session::UserRef;
use plx_plex::plex::watchlist::{WatchItem, WatchKind, WatchPage, WatchlistError};
use plx_plex::plex::{GuidKind, GuidLookupError, GuidMatch, ServerId};

use super::nuvio;

/// The shim, when this boot has one. Host tests construct stores with no shim; they then behave as
/// a signed-out account (the read-out, not a panic).
fn shim() -> Option<std::sync::Arc<plx_pmsshim::shim_pms::Shim>> {
    nuvio::handle().map(|h| h.shim.clone())
}

fn shim_server() -> Option<ServerId> {
    plx_plex::plex::id_of_machine(plx_pmsshim::MACHINE_ID)
}

fn kind_of(entry: &plx_pmsshim::shim_pms::MyListEntry) -> WatchKind {
    if entry.type_ == "series" { WatchKind::Show } else { WatchKind::Movie }
}

fn guid_of(hex: &[u8; 12], kind: WatchKind) -> String {
    let tail = plx_data::watchlist::encode_hex(hex);
    match kind {
        WatchKind::Show => format!("plex://show/{tail}"),
        _ => format!("plex://movie/{tail}"),
    }
}

fn item_of(entry: &plx_pmsshim::shim_pms::MyListEntry) -> WatchItem {
    let hex = plx_plex::plex::hex_of_rating_key(entry.rating_key);
    let kind = kind_of(entry);
    WatchItem {
        hex: plx_data::watchlist::encode_hex(&hex),
        guid: guid_of(&hex, kind),
        kind: Some(kind),
        title: Some(entry.title.clone()),
        title_sort: Some(entry.title.clone()),
        year: entry.year,
        added_at: None,
        originally_available: entry.release.clone(),
        rating: entry.rating,
        summary: None,
        thumb: Some(entry.thumb.clone()),
    }
}

fn error_of(e: plx_stremio::model::Error) -> WatchlistError {
    match e {
        plx_stremio::model::Error::Status(401) => WatchlistError::Unauthorized,
        plx_stremio::model::Error::Status(404) => WatchlistError::NotFound,
        plx_stremio::model::Error::Status(code) => WatchlistError::Other(code),
        _ => WatchlistError::Transport,
    }
}

/// The production seam for this fork: `LiveIo`'s storage and `find` are the shim's, not a PMS
/// roster's.
pub(crate) struct MyListIo;

impl Reach for MyListIo {
    fn reachable(&self, server: ServerId) -> bool {
        shim_server() == Some(server) && shim().is_some()
    }
    fn roster(&self) -> Vec<ServerId> {
        shim_server().into_iter().collect()
    }
    fn copy_rank(&self, server: ServerId) -> u8 {
        if shim_server() == Some(server) { 0 } else { 2 }
    }
}

impl WatchlistIo for MyListIo {
    fn capture_auth(&self) -> Option<WatchlistAuth> {
        let shim = shim()?;
        if !shim.account.signed_in() {
            return None;
        }
        // The account is the identity. The credential fields are `LiveIo`'s; this seam never
        // builds a request from them.
        Some(WatchlistAuth {
            client_id: "nuvio".into(),
            credential: String::new(),
            user: UserRef::default(),
            generation: 0,
        })
    }

    fn is_current(&self, _: &WatchlistAuth) -> bool {
        shim().is_some_and(|s| s.account.signed_in())
    }

    fn page(&self, _: &WatchlistAuth, start: u32, size: u32, _: Option<&str>)
        -> Result<WatchPage, WatchlistError>
    {
        let shim = shim().ok_or(WatchlistError::Unauthorized)?;
        let entries = shim.my_list_entries(start == 0).map_err(error_of)?;
        let total = u32::try_from(entries.len()).unwrap_or(u32::MAX);
        let items: Vec<WatchItem> = entries
            .iter()
            .skip(start as usize)
            .take(size as usize)
            .map(item_of)
            .collect();
        Ok(WatchPage { start, total: Some(total), raw: u32::try_from(items.len()).unwrap_or(0), items, etag: None })
    }

    fn released(&self, _: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> {
        let shim = shim().ok_or(WatchlistError::Unauthorized)?;
        let entries = shim.my_list_entries(false).map_err(error_of)?;
        let today = <LiveIo as WatchlistIo>::today(&LiveIo);
        Ok(entries
            .iter()
            .filter(|e| match (&e.release, &today) {
                (None, _) => true,
                (Some(_), None) => true,
                (Some(release), Some(today)) => release.as_str() <= today.as_str(),
            })
            .map(|e| {
                let hex = plx_plex::plex::hex_of_rating_key(e.rating_key);
                let kind = kind_of(e);
                WatchItem { hex: plx_data::watchlist::encode_hex(&hex), guid: guid_of(&hex, kind), kind: Some(kind), ..WatchItem::default() }
            })
            .collect())
    }

    fn write(&self, _: &WatchlistAuth, hex: &str, intent: Intent) -> Result<(), WatchlistError> {
        let shim = shim().ok_or(WatchlistError::Unauthorized)?;
        let bytes = plx_data::watchlist::decode_hex(hex).ok_or(WatchlistError::BadRequest)?;
        let key = plx_plex::plex::rating_key_of_hex(&bytes).ok_or(WatchlistError::BadRequest)?;
        shim.set_my_list_saved(key, matches!(intent, Intent::Add)).map_err(error_of)
    }

    fn find(&self, server: ServerId, _: GuidKind, guids: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> {
        if shim_server() != Some(server) {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for guid in guids {
            let tail = guid.strip_prefix("plex://movie/").or_else(|| guid.strip_prefix("plex://show/"));
            let Some(hex) = tail.and_then(plx_data::watchlist::decode_hex) else { continue };
            let Some(key) = plx_plex::plex::rating_key_of_hex(&hex) else { continue };
            found.push(GuidMatch {
                guid: guid.clone(),
                rating_key: key.to_string(),
                library_section_id: None,
                blur: None,
                facts: Default::default(),
            });
        }
        Ok(found)
    }

    fn roster_gen(&self) -> u32 {
        plx_plex::plex::server_roster_gen()
    }

    fn facts_gen(&self) -> u32 {
        plx_plex::plex::server_facts_gen()
    }

    fn today(&self) -> Option<String> {
        <LiveIo as WatchlistIo>::today(&LiveIo)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shim_key_is_the_watchlist_identity() {
        let key = 10_069i64;
        let hex = plx_plex::plex::hex_of_rating_key(key);
        assert_eq!(plx_plex::plex::rating_key_of_hex(&hex), Some(key));
        assert_eq!(guid_of(&hex, WatchKind::Movie), "plex://movie/4e56494f0000000000002755");
        assert_eq!(guid_of(&hex, WatchKind::Show), "plex://show/4e56494f0000000000002755");
        let back = plx_data::watchlist::decode_hex(&guid_of(&hex, WatchKind::Movie)[13..]).expect("decodes");
        assert_eq!(back, hex);
    }

    #[test]
    fn an_empty_guid_or_foreign_hex_finds_nothing() {
        assert_eq!(plx_plex::plex::rating_key_of_hex(&[0u8; 12]), None);
        assert_eq!(plx_data::watchlist::decode_hex("not-a-catalog-id"), None);
    }

    #[test]
    fn kind_maps_series_to_show() {
        let entry = plx_pmsshim::shim_pms::MyListEntry {
            rating_key: 1, type_: "series".into(), id: "tt1".into(), title: "T".into(),
            year: None, release: None, rating: None, thumb: String::new(),
        };
        assert_eq!(kind_of(&entry), WatchKind::Show);
        assert_eq!(item_of(&entry).kind, Some(WatchKind::Show));
        assert_eq!(item_of(&entry).guid.starts_with("plex://show/"), true);
    }
}
