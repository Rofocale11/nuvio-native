//! The PMS routes. Every shape here is one `plx_plex::plex::models` deserialises; fields it does not
//! read are left out rather than invented.

use crate::shim_backend::{Backend, CatalogInfo};
use crate::shim_http::{pct_encode, Request, Response};
use crate::shim_media::{self, Classified, SubRef, PART_STRIDE};
use crate::shim_store::{now_secs, Ident, Keys, Watch, WatchState};
use crate::shim_core::{Config, MACHINE_ID};
use plx_stremio::model::{Meta, MetaPreview, Stream, Video};
// Fork Rofocale11: TMDB is this fork's metadata provider; upstream's shim only knew Plex. The shim
// enriches the PMS rows it builds, so it needs the flat TMDB response shapes and the cached client.
use plx_nuvio_store::NuvioCache;
use plx_stremio::addons_tmdb::{
    TmdbCast, TmdbClient, TmdbCollection, TmdbCollectionRef, TmdbCredit, TmdbImages,
    TmdbMovie, TmdbMovieDetails, TmdbPerson, TmdbShow, TmdbShowDetails, TmdbVideo, TmdbVideos,
};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const META_TTL: Duration = Duration::from_secs(6 * 3600);
const CATALOG_TTL: Duration = Duration::from_secs(15 * 60);
/// How long `/hubs` waits for catalogs that are not cached yet; late ones land in the cache for the
/// next fetch rather than holding Home up.
const HUBS_BUDGET: Duration = Duration::from_secs(4);
/// Catalog fetches in flight at once (the TV's network and CPU are shared with the UI).
const CATALOG_PARALLEL: usize = 8;
const STREAM_TTL: Duration = Duration::from_secs(30 * 60);
// Fork Rofocale11: account watch progress. `/:timeline` reports every few seconds, so one item is
// mirrored at most once a minute while playing; a pause or stop goes at once — that is the position
// a person returns to.
const WATCH_PUSH_INTERVAL: Duration = Duration::from_secs(60);
const NUVIO_SYNTHETIC_EPISODE: &str = "__nuvio_episode__:";
/// Rows a catalog listing reports beyond what it has fetched while the catalog is not exhausted,
/// so a grid keeps asking for the next page.
const MORE_HINT: i64 = 50;
/// How long a DETAIL request waits for streams. The page must not hang on addons; a play waits
/// the full deadline at `/decision` / the Part instead.
const DETAIL_WAIT: Duration = Duration::from_millis(800);
/// The Part index that means "whichever stream leads when this is played".
const BEST_PART: usize = PART_STRIDE as usize - 1;
const DEFAULT_EPISODE_MS: i64 = 45 * 60_000;
const DEFAULT_MOVIE_MS: i64 = 110 * 60_000;
const SECTION_MOVIES: i64 = 1;
const SECTION_SHOWS: i64 = 2;
// Fork Rofocale11: TMDB-only titles and people have no `Ident` to store in the key table, so their
// numeric identity is encoded arithmetically. A billion-wide gap keeps a real movie key (10_000+N)
// and every synthetic space from ever aliasing, and each decoder is bounded by the NEXT base so a
// person key can never decode as a show.
const TMDB_FALLBACK_KEY_BASE: i64 = 1_000_000_000;
const TMDB_SHOW_KEY_BASE: i64 = 2_000_000_000;
const TMDB_PERSON_KEY_BASE: i64 = 3_000_000_000;
const TMDB_COLLECTION_KEY_BASE: i64 = 4_000_000_000;
const TRAILER_KEY_BASE: i64 = 5_000_000_000;
/// A trailer clip's key sits a billion above every title space, so it never decodes as a title.
const TRAILER_KEY_END: i64 = 10_000_000_000;
const PERSON_GUID_PREFIX: &str = "nuvio-person-";
/// The parsed TMDB enrichment kept in the one durable store. Cast, trailers and recommendations
/// are stable for a title, so a 30-day TTL keeps repeat opens local (movies and series alike).
const ENRICH_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// An IMDb↔TMDB mapping never changes; cache both the hit and the miss so a poster never re-fetches.
const IMDB_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Movie metadata does not change once released; a series' `videos` IS its episode list, so its
/// cache stays at the short six-hour TTL upstream already uses.
const META_TTL_MOVIES: Duration = Duration::from_secs(30 * 24 * 3600);

struct CatalogCache {
    at: Instant,
    rows: Vec<MetaPreview>,
    exhausted: bool,
}

#[derive(Default)]
struct SlotState {
    /// Arrival order — a Part id names a stream by its position HERE, which never changes while the
    /// slot lives, however the ranking moves as later addons answer.
    streams: Vec<Stream>,
    done: bool,
    /// A stream (arrival index) the user picked; it leads `Media[]`.
    selected: Option<usize>,
    subs: Vec<SubRef>,
    /// Fork Rofocale11: the ranked list the detail/metadata path already built for this item. The
    /// Streams panel used to re-rank with an 800 ms window and show "No playable streams found"
    /// while Play Version showed the very same stream from the cached `Media[]`. One slot, one list.
    ranked: Vec<(usize, shim_media::Classified)>,
}

struct StreamSlot {
    started: Instant,
    state: Mutex<SlotState>,
    cv: Condvar,
}

pub struct Shim {
    backend: Arc<dyn Backend>,
    cfg: Config,
    keys: Keys,
    watch: WatchState,
    metas: Mutex<HashMap<(String, String), (Instant, Arc<Meta>)>>,
    catalogs: Mutex<HashMap<(String, Option<String>), Arc<Mutex<CatalogCache>>>>,
    catalog_list: Mutex<Option<(Instant, Vec<CatalogInfo>)>>,
    streams: Mutex<HashMap<i64, Arc<StreamSlot>>>,
    sub_urls: Mutex<HashMap<i64, String>>,
    poster_fallbacks: Mutex<HashMap<String, String>>,
    next_sub: AtomicI64,
    permits: (Mutex<usize>, Condvar),
    /// Real stream metadata from the app's FFmpeg (installed at boot; absent on the host bin).
    pub probes: crate::shim_probe::ProbeCache,
    /// Torrent markers resolved on a streaming server (`/create`), by marker + ratingKey.
    resolved: Mutex<HashMap<(i64, String), String>>,
    torrent_server: std::sync::RwLock<Option<String>>,
    /// Audio codecs this TV decodes (`devcaps`); empty = unknown, rank on video alone.
    audio_codecs: std::sync::RwLock<Vec<String>>,
    /// Set by [`Shim::bind_self`]: background work started from a `&self` method.
    me: std::sync::OnceLock<std::sync::Weak<Shim>>,
    /// plex.tv's account surface, answered by the Nuvio account (`account.rs`).
    pub account: crate::shim_account::Account,
    // Fork Rofocale11: franchises a Detail page has fetched, keyed by TMDB collection id. The
    // collection shelf's heading opens a page the shim answers from this map, no second TMDB call.
    collections: Mutex<HashMap<u64, TmdbCollection>>,
    /// TMDB rows behind synthetic related keys, so their cards keep resolving after a restart.
    tmdb_fallbacks: Mutex<HashMap<i64, TmdbFallback>>,
    /// Trailer clip rows built from TMDB `videos`, served by `/library/metadata/{clip}`.
    trailers: Mutex<HashMap<i64, Value>>,
    // Fork Rofocale11: throttle state for account mirrors, and the count a client polls to learn
    // that a boot/profile-switch account pull changed Continue Watching.
    watch_pushes: Mutex<HashMap<i64, Instant>>,
    content_gen: AtomicU64,
}

/// One local item's identity as Nuvio's account stores it (`content_type` is `movie`/`series`).
struct AccountIdentity {
    content_id: String,
    content_type: &'static str,
    video_id: String,
    season: Option<i64>,
    episode: Option<i64>,
}

fn container(fields: Value) -> Value {
    let mut mc = Map::new();
    mc.insert("size".into(), json!(0));
    mc.insert("allowSync".into(), json!(false));
    mc.insert("identifier".into(), json!("com.plexapp.plugins.library"));
    if let Value::Object(f) = fields {
        for (k, v) in f {
            mc.insert(k, v);
        }
    }
    for k in ["Metadata", "Directory", "Hub"] {
        if let Some(n) = mc.get(k).and_then(Value::as_array).map(Vec::len) {
            mc.insert("size".into(), json!(n));
        }
    }
    json!({ "MediaContainer": Value::Object(mc) })
}

fn ok_empty() -> Response {
    Response::json(&container(json!({})))
}

/// A small stable id for a tag (genre/person) name: PMS tag ids are integers.
fn tag_id(s: &str) -> i64 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x01000193);
    }
    i64::from(h & 0x7fff_ffff)
}

fn tags(v: &[String], role: bool) -> Value {
    Value::Array(
        v.iter()
            .filter(|s| !s.is_empty())
            .take(20)
            .map(|s| if role { json!({"tag": s, "id": tag_id(s), "role": ""}) } else { json!({"tag": s, "id": tag_id(s)}) })
            .collect(),
    )
}

fn year_of(s: Option<&str>) -> Option<i64> {
    let s = s?;
    let digits: String = s.chars().skip_while(|c| !c.is_ascii_digit()).take(4).collect();
    (digits.len() == 4).then(|| digits.parse().ok()).flatten()
}

// Fork Rofocale11: synthetic identity helpers. The key table only holds real catalog items; TMDB
// recommendations, franchise films, people and trailers are numbered from these disjoint bases and
// decoded on the way back in. Each decoder stops at the next base, and callers check highest-first.

/// A synthetic related-movie ratingKey's TMDB id (1B..2B).
fn synthetic_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_FALLBACK_KEY_BASE..TMDB_SHOW_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_FALLBACK_KEY_BASE) as u64)
}

/// A synthetic related-show ratingKey's TMDB id (2B..3B).
fn synthetic_show_tmdb_id(rk: i64) -> Option<u64> {
    (TMDB_SHOW_KEY_BASE..TMDB_PERSON_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_SHOW_KEY_BASE) as u64)
}

/// A Cast & Crew headshot's synthetic `personId` → TMDB person id (3B..4B).
fn synthetic_person_id(rk: i64) -> Option<u64> {
    (TMDB_PERSON_KEY_BASE..TMDB_COLLECTION_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_PERSON_KEY_BASE) as u64)
}

/// A synthetic collection ratingKey's TMDB collection id (4B..5B).
fn synthetic_collection_id(rk: i64) -> Option<u64> {
    (TMDB_COLLECTION_KEY_BASE..TRAILER_KEY_BASE)
        .contains(&rk)
        .then_some((rk - TMDB_COLLECTION_KEY_BASE) as u64)
}

/// The parent ratingKey behind a trailer clip key (5B..10B).
fn synthetic_trailer_parent(rk: i64) -> Option<i64> {
    (TRAILER_KEY_BASE..TRAILER_KEY_END).contains(&rk).then_some(rk - TRAILER_KEY_BASE)
}

/// The Stremio id addons know a synthetic title by (`tmdb:603`), if it is synthetic. Highest base
/// first so no space can be decoded by another.
fn translate_to_stremio_id(rk: i64) -> Option<String> {
    synthetic_show_tmdb_id(rk)
        .or_else(|| synthetic_tmdb_id(rk))
        .map(|id| format!("tmdb:{id}"))
}

/// A synthetic key's `Ident`. Synthetic keys are never in `keys.tsv`, so every route that needs one
/// (playback, metadata, photo, person media) falls back to this.
fn synthetic_ident(rk: i64) -> Option<Ident> {
    let id = translate_to_stremio_id(rk)?;
    let type_ = if synthetic_show_tmdb_id(rk).is_some() { "series" } else { "movie" };
    Some(Ident::Title { type_: type_.into(), id })
}

/// The two identities a Role[] row carries: numeric `personId` opens `/media`, the guid opens the
/// global biography and filmography. Neither stands in for the other (docs/nuvio/ID-MANAGEMENT.md).
fn person_guid(person_id: u64) -> String {
    format!("{PERSON_GUID_PREFIX}{person_id}")
}

fn person_id_of_guid(guid: &str) -> Option<u64> {
    guid.strip_prefix(PERSON_GUID_PREFIX)?.parse().ok().filter(|id| *id > 0)
}

/// The TMDB number in `tmdb:603`, or `None` for an id addons already understand.
fn tmdb_content_number(id: &str) -> Option<u64> {
    id.strip_prefix("tmdb:")?.parse().ok().filter(|n| *n > 0)
}

/// `tt` followed by digits — the id form addons, TMDB's `/find` and BetterPosters share.
fn is_imdb_id(id: &str) -> bool {
    id.len() > 2 && id.starts_with("tt") && id[2..].bytes().all(|b| b.is_ascii_digit())
}

/// Split an episode's `:S:E` tail off its content id (`tmdb:1396:1:2` → `tmdb:1396`, `:1:2`).
fn split_content_id(id: &str) -> (&str, &str) {
    let base = plx_stremio::addons::content_level_id(id);
    if base.is_empty() || base == id {
        return (id, "");
    }
    match id.strip_prefix(base.as_str()) {
        Some(suffix) => (&id[..base.len()], suffix),
        None => (id, ""),
    }
}

// Fork Rofocale11: account watch identity helpers.
/// One push per item per minute while playing; a paused or stopped report is the position a
/// person actually returns to and is the last report of a session, so it goes straight through.
fn watch_push_due(last: Option<Instant>, settled: bool, now: Instant) -> bool {
    settled || last.is_none_or(|at| now.duration_since(at) >= WATCH_PUSH_INTERVAL)
}

/// The season and episode an addon id ends in (`tt0944947:1:2`). `None` when there is no tail.
fn episode_numbers(video_id: &str) -> (Option<i64>, Option<i64>) {
    let mut parts = video_id.rsplit(':');
    let episode = parts.next().and_then(|s| s.parse::<i64>().ok());
    let season = parts.next().and_then(|s| s.parse::<i64>().ok());
    match (season, episode) {
        (Some(s), Some(e)) if s >= 0 && e > 0 => (Some(s), Some(e)),
        _ => (None, None),
    }
}

/// The series an episode id belongs to; an id with no `:S:E` tail names the content id instead.
fn episode_series_id<'a>(video_id: &'a str, content_id: &'a str) -> &'a str {
    video_id.rsplit_once(':')
        .and_then(|(rest, _)| rest.rsplit_once(':'))
        .map(|(series, _)| series)
        .unwrap_or(content_id)
}

/// The TMDB id behind an addon id, used only when TMDB names no IMDb id for it. A `tmdb:` id needs
/// no lookup; an IMDb id with no TMDB key (or a title neither service knows) does not sync.
fn account_tmdb_number(type_: &str, id: &str) -> Option<u64> {
    let base = if is_series(type_) { split_content_id(id).0 } else { id };
    if let Some(n) = tmdb_content_number(base) { return Some(n); }
    let client = TmdbClient::new();
    if !client.configured() { return None; }
    let found = if is_series(type_) { client.series_id(id) } else { client.movie_id(id) };
    found.ok().flatten().filter(|n| *n > 0)
}

/// `"142 min"`, `"2h 22min"`, `"45"` → ms.
fn runtime_ms(s: Option<&str>) -> Option<i64> {
    let s = s?.to_lowercase();
    let mut total = 0i64;
    let mut num = String::new();
    let mut any = false;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        if !num.is_empty() {
            let n: i64 = num.parse().unwrap_or(0);
            num.clear();
            if c == 'h' {
                total += n * 60;
            } else {
                total += n;
            }
            any = true;
        }
    }
    if !num.is_empty() {
        total += num.parse::<i64>().unwrap_or(0);
        any = true;
    }
    (any && total > 0).then_some(total * 60_000)
}

fn date_of(s: Option<&str>) -> Option<String> {
    let s = s?;
    (s.len() >= 10 && s.as_bytes()[4] == b'-').then(|| s[..10].to_string())
}

fn rating_of(s: Option<&str>) -> Option<f64> {
    s?.trim().parse::<f64>().ok().filter(|r| *r > 0.0)
}

fn is_series(type_: &str) -> bool {
    type_ == "series"
}

// Fork Rofocale11: TMDB → Plex field mapping. All pure so the shapes can be tested offline.

/// A TMDB image path as a sized CDN URL. Only paths TMDB actually returned are accepted.
fn tmdb_image(path: Option<&str>) -> Option<String> {
    path.filter(|path| path.starts_with('/'))
        .map(|path| format!("https://image.tmdb.org/t/p/w300{path}"))
}

fn tmdb_backdrop(path: Option<&str>) -> Option<String> {
    path.filter(|path| path.starts_with('/'))
        .map(|path| format!("https://image.tmdb.org/t/p/w1280{path}"))
}

/// TMDB `images.logos` → the row's `clearLogo`: English first, else the first with a file. `None`
/// means the row carries NO clearLogo at all, never a poster substitute (docs/nuvio/TMDB.md).
fn tmdb_logo(images: Option<&TmdbImages>) -> Option<String> {
    let logos = &images?.logos;
    let pick = logos
        .iter()
        .find(|logo| logo.iso_639_1.as_deref() == Some("en") && logo.file_path.is_some())
        .or_else(|| logos.iter().find(|logo| logo.file_path.is_some()))?;
    pick.file_path.as_deref().map(|path| format!("https://image.tmdb.org/t/p/w500{path}"))
}

/// Fork Rofocale11: the app reads a title's logo from `Metadata.Image[type=clearLogo]`, not from a
/// bare `clearLogo` field (`plx_plex::plex::models::Metadata::clear_logo`). Publish BOTH: the Image
/// gives the hero a stamped server path, and `photo` resolves that path back to the TMDB URL.
fn set_logo(row: &mut Value, rk: i64, url: &str) {
    row["clearLogo"] = json!(url);
    row["Image"] = json!([{"type": "clearLogo", "url": format!("/library/metadata/{rk}/clearLogo/{}", now_secs())}]);
}

/// One TMDB cast member as a Plex Role. `id` is the synthetic personId a headshot opens; `tagKey`
/// is the separate global guid the biography/filmography routes answer under.
fn tmdb_role(cast: &TmdbCast) -> Option<Value> {
    if cast.name.trim().is_empty() {
        return None;
    }
    let mut role = json!({"tag": cast.name, "role": cast.character});
    if let Some(thumb) = tmdb_image(cast.profile_path.as_deref()) {
        role["thumb"] = json!(thumb);
    }
    if cast.id > 0 && cast.id < (i64::MAX - TMDB_PERSON_KEY_BASE) as u64 {
        role["id"] = json!(TMDB_PERSON_KEY_BASE + cast.id as i64);
        role["tagKey"] = json!(person_guid(cast.id));
    }
    Some(role)
}

fn set_roles(row: &mut Value, cast: &[TmdbCast]) {
    let roles: Vec<Value> = cast.iter().filter_map(tmdb_role).take(10).collect();
    if !roles.is_empty() {
        row["Role"] = Value::Array(roles);
    }
}

/// TMDB `videos` → the `Extras` rows the detail page draws. Only YouTube carries a thumbnail we
/// can build without another request; dedupe by key so a trailer listed twice appears once.
fn extras_from_videos(videos: Option<&TmdbVideos>, parent: i64) -> Value {
    let mut seen: HashSet<String> = HashSet::new();
    let rows: Vec<Value> = videos
        .into_iter()
        .flat_map(|v| v.results.iter())
        .filter(|v| v.site.eq_ignore_ascii_case("youtube") && !v.key.is_empty())
        .filter(|v| seen.insert(v.key.clone()))
        .enumerate()
        .map(|(index, v): (usize, &TmdbVideo)| {
            let clip = TRAILER_KEY_BASE + parent + index as i64;
            json!({
                "ratingKey": clip.to_string(), "key": format!("/library/metadata/{clip}"),
                "type": "clip", "subtype": "trailer", "extraType": 1,
                "title": v.name, "thumb": format!("https://img.youtube.com/vi/{}/hqdefault.jpg", v.key),
            })
        })
        .collect();
    json!({"size": rows.len(), "Metadata": rows})
}

fn related_hub_json(kind: &str, rows: Vec<Value>) -> Value {
    json!({"title": "More Like This", "type": kind, "hubIdentifier": format!("{kind}.related"),
           "size": rows.len(), "Metadata": rows})
}

/// A crew department as a filmography tab. The three familiar ones use Plex's own names; TMDB's
/// catch-all "Crew" bucket becomes "Other".
fn person_department_group(department: &str) -> Option<(String, String)> {
    let name = department.trim();
    if name.is_empty() {
        return None;
    }
    match name.to_ascii_lowercase().as_str() {
        "directing" => return Some(("director".into(), "Director".into())),
        "writing" => return Some(("writer".into(), "Writer".into())),
        "production" => return Some(("producer".into(), "Producer".into())),
        "crew" => return Some(("other".into(), "Other".into())),
        _ => {}
    }
    let slug: String = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>()
        .join("-");
    (!slug.is_empty()).then(|| (slug, name.to_string()))
}

/// One filmography credit. The join key is exactly the tail of a shim row's guid
/// (`nuvio://movie/tmdb:<id>`), so the person page can mark credits the app holds.
fn person_credit_item(credit: &TmdbCredit, role: &str) -> Option<Value> {
    if credit.id == 0 {
        return None;
    }
    let (kind, title) = match credit.media_type.as_str() {
        "movie" => ("movie", credit.name.trim()),
        "tv" => ("show", credit.name.trim()),
        _ => return None,
    };
    if title.is_empty() {
        return None;
    }
    Some(json!({
        "role": role,
        "Metadata": {
            "type": kind, "title": title,
            "thumb": tmdb_image(credit.profile_path.as_deref()).unwrap_or_default(),
            "ratingKey": format!("tmdb:{}", credit.id),
        }
    }))
}

fn person_credit_group(kind: &str, title: &str, mut rows: Vec<Value>) -> Value {
    for (order, row) in rows.iter_mut().enumerate() {
        row["order"] = json!(order as i64);
    }
    json!({"type": kind, "title": title, "size": rows.len(), "Credit": rows})
}

/// A stored row's `art` is a real landscape backdrop only when it is not the addon's metahub
/// background (the fallback) and not empty.
fn usable_landscape(art: Option<&str>) -> bool {
    art.is_some_and(|url| !url.is_empty() && !url.contains("images.metahub.space/"))
}

/// The parsed TMDB answer persisted for a title. Related titles stay as TMDB returned them (not as
/// rendered rows) so a cached shelf can be rebuilt with fresh fallback keys after a restart.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Enrichment {
    tmdb_id: u64,
    roles: Option<Value>,
    extras: Option<Value>,
    related: Option<RelatedSource>,
    collection_ref: Option<TmdbCollectionRef>,
    collection: Option<TmdbCollection>,
    art: Option<String>,
    clear_logo: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum RelatedSource {
    Movies(Vec<TmdbMovie>),
    Shows(Vec<TmdbShow>),
}

impl Enrichment {
    /// Apply the cached fields to a freshly built row. Called on every later open, with no TMDB.
    fn apply(&self, rk: i64, row: &mut Value) {
        if let Some(roles) = &self.roles { row["Role"] = roles.clone(); }
        if let Some(extras) = &self.extras { row["Extras"] = extras.clone(); }
        if let Some(art) = &self.art { row["art"] = json!(art); }
        if let Some(logo) = &self.clear_logo { set_logo(row, rk, logo); }
    }
}

/// A TMDB title remembered under its synthetic ratingKey, so a related row stays clickable after a
/// restart without a fresh details request.
#[derive(Clone, Debug)]
enum TmdbFallback {
    Movie(TmdbMovie),
    Show(TmdbShow),
}

fn season_title(n: u32) -> String {
    if n == 0 {
        "Specials".into()
    } else {
        format!("Season {n}")
    }
}

/// Episodes in a stable (season, episode) order; season-less videos fold into season 1.
fn episodes_of(meta: &Meta) -> Vec<(u32, u32, &Video)> {
    let mut v: Vec<(u32, u32, &Video)> =
        meta.videos.iter().enumerate().map(|(i, x)| (x.season.unwrap_or(1), x.episode.unwrap_or(i as u32 + 1), x)).collect();
    // Specials (season 0) after the regular seasons, as Stremio lists them.
    v.sort_by_key(|(s, e, _)| (if *s == 0 { u32::MAX } else { *s }, *e));
    v
}

fn seasons_of(meta: &Meta) -> Vec<u32> {
    let mut s: Vec<u32> = episodes_of(meta).iter().map(|(s, _, _)| *s).collect();
    s.dedup();
    s
}

fn page_params(req: &Request) -> (usize, Option<usize>) {
    let start = req.q_or_header("X-Plex-Container-Start").and_then(|v| v.parse().ok()).unwrap_or(0);
    let size = req.q_or_header("X-Plex-Container-Size").and_then(|v| v.parse().ok()).or_else(|| req.q("count").and_then(|v| v.parse().ok()));
    (start, size)
}

/// `.../library/metadata/123` or `123` → 123.
fn trailing_key(s: &str) -> Option<i64> {
    s.trim_end_matches('/').rsplit('/').next()?.parse().ok()
}

impl Shim {
    pub fn new(backend: Arc<dyn Backend>, cfg: Config) -> Shim {
        let dir = cfg.data_dir.clone();
        let cfg_ts = cfg.torrent_server.clone();
        let cfg_token = cfg.token.clone();
        Shim {
            backend,
            keys: Keys::open(dir.as_deref()),
            watch: WatchState::open(dir.as_deref()),
            cfg,
            metas: Mutex::new(HashMap::new()),
            catalogs: Mutex::new(HashMap::new()),
            catalog_list: Mutex::new(None),
            streams: Mutex::new(HashMap::new()),
            sub_urls: Mutex::new(HashMap::new()),
            poster_fallbacks: Mutex::new(HashMap::new()),
            next_sub: AtomicI64::new(900_000_000),
            permits: (Mutex::new(CATALOG_PARALLEL), Condvar::new()),
            probes: crate::shim_probe::ProbeCache::default(),
            resolved: Mutex::new(HashMap::new()),
            torrent_server: std::sync::RwLock::new(cfg_ts),
            audio_codecs: std::sync::RwLock::new(Vec::new()),
            me: std::sync::OnceLock::new(),
            account: crate::shim_account::Account::new(dir.as_deref(), &cfg_token),
            collections: Mutex::new(HashMap::new()),
            tmdb_fallbacks: Mutex::new(HashMap::new()),
            trailers: Mutex::new(HashMap::new()),
            watch_pushes: Mutex::new(HashMap::new()),
            content_gen: AtomicU64::new(0),
        }
    }

    pub fn bind_self(self: &Arc<Self>) {
        let _ = self.me.set(Arc::downgrade(self));
    }

    // ---- Fork Rofocale11: account watch progress -------------------------------------------

    /// The identity Nuvio keys a resume point by. Nuvio names a title by its **IMDb id**, so a
    /// local `tmdb:` id is resolved through TMDB; the fallback keeps a title TMDB cannot name.
    /// A whole series or a season has no single position — its episodes do.
    fn nuvio_watch_identity(&self, rk: i64) -> Option<AccountIdentity> {
        let ident = self.keys.ident(rk).or_else(|| synthetic_ident(rk))?;
        match ident {
            Ident::Title { type_, .. } if is_series(&type_) => None,
            Ident::Title { type_, id } => {
                let base = split_content_id(&id).0;
                let content_id = match self.imdb_base(&type_, base) {
                    Some(imdb) => imdb,
                    None => format!("tmdb:{}", account_tmdb_number(&type_, base)?),
                };
                Some(AccountIdentity { video_id: content_id.clone(), content_id,
                    content_type: "movie", season: None, episode: None })
            }
            Ident::Episode { series, video } => {
                let content_id = self.imdb_base("series", &series)
                    .or_else(|| account_tmdb_number("series", &series).map(|n| format!("tmdb:{n}")))?;
                let (video_base, suffix) = split_content_id(&video);
                let video_id = self.imdb_base("series", video_base)
                    .map(|base| format!("{base}{suffix}"))
                    .unwrap_or_else(|| video.clone());
                let mut numbers = episode_numbers(&video_id);
                if numbers.0.is_none() {
                    // An addon that ids an episode without a `:S:E` tail still has the numbers in
                    // its meta. Without them every episode would overwrite the same account row.
                    if let Some((_, meta)) = self.series_of(&series) {
                        if let Some((s, e, _)) = episodes_of(&meta).into_iter().find(|(_, _, v)| v.id == video) {
                            numbers = (Some(i64::from(s)), Some(i64::from(e)));
                        }
                    }
                }
                let (season, episode) = numbers;
                Some(AccountIdentity { content_id, content_type: "series", video_id, season, episode })
            }
            Ident::Season { .. } => None,
        }
    }

    /// Mirror one item's resume point to the account. A failed push changes nothing locally: the
    /// local write already happened, and losing the mirror is not losing playback.
    fn push_watch_progress(&self, rk: i64, offset_ms: i64, duration_ms: i64, state: &str) {
        if !self.account.signed_in() { return; }
        let settled = matches!(state, "paused" | "stopped");
        {
            let mut pushes = self.watch_pushes.lock().unwrap_or_else(|e| e.into_inner());
            if !watch_push_due(pushes.get(&rk).copied(), settled, Instant::now()) { return; }
            if pushes.len() >= 512 { pushes.clear(); }
            pushes.insert(rk, Instant::now());
        }
        let Some(identity) = self.nuvio_watch_identity(rk) else { return; };
        self.watch.bind_account_key(&identity.content_id, &identity.video_id, rk);
        let entry = plx_stremio::nuvio::WatchProgress {
            content_id: identity.content_id, content_type: identity.content_type.into(),
            video_id: identity.video_id, season: identity.season, episode: identity.episode,
            position: offset_ms.max(0), duration: duration_ms, last_watched: now_secs() * 1000,
        };
        if let Err(error) = self.account.push_watch_progress(&[entry]) {
            let status = match error { plx_stremio::model::Error::Status(s) => s, _ => 0 };
            plx_stremio::nuvio::log_account_event(&format!("nuvio account: progress push failed status={status}"));
        }
    }

    /// Pull the account's resume points and merge them into local state, off the caller's thread.
    pub fn pull_account_watch(&self) {
        let Some(me) = self.me.get().and_then(|w| w.upgrade()) else { return; };
        let _ = std::thread::Builder::new().name("pmsshim-watch".into()).spawn(move || {
            let Ok(entries) = me.account.pull_watch_progress() else { return; };
            let mut merged = 0;
            for e in entries {
                // Cheapest rejection first: a row snapshot is a meta fetch, so never pay for an
                // entry the local state already beats.
                if e.position <= 0 { continue; }
                let stamp = e.last_watched / 1000;
                let Some(rk) = me.account_watch_key(&e) else { continue; };
                if me.watch.get(rk).last_viewed_at > stamp { continue; }
                if me.watch.merge_remote(rk, e.position, e.duration, stamp, me.row(rk)) {
                    me.watch.bind_account_key(&e.content_id, &e.video_id, rk);
                    merged += 1;
                }
            }
            me.account_watch_merged(merged);
        });
    }

    /// A real merge changes what Continue Watching should show. The app polls this count and
    /// re-reads Home once per observed value; a no-op pull says nothing, so a steady boot is quiet.
    fn account_watch_merged(&self, merged: usize) {
        if merged == 0 { return; }
        self.bump_content();
        plx_stremio::nuvio::log_account_event(&format!("nuvio account: merged {merged} resume point(s)"));
    }

    /// The local ratingKey an account row belongs to: what this device pushed under that identity,
    /// else the key its identity names. Does not bind on a miss — a pull that loses to the local
    /// state must not grow the map.
    fn account_watch_key(&self, e: &plx_stremio::nuvio::WatchProgress) -> Option<i64> {
        if let Some(key) = self.watch.account_key(&e.content_id, &e.video_id) { return Some(key); }
        if e.content_type != "series" {
            let id = self.imdb_base("movie", &e.content_id).unwrap_or_else(|| e.content_id.clone());
            return Some(self.keys.key_of(&Ident::Title { type_: "movie".into(), id }));
        }
        let content_id = self.imdb_base("series", &e.content_id).unwrap_or_else(|| e.content_id.clone());
        // Nuvio's clients name a synthetic episode `__nuvio_episode__:S:E` when playback did not
        // carry the addon's episode id. Place it through the series meta's season/episode: interning
        // that string would name an episode no addon can answer.
        let synthetic = e.video_id.is_empty()
            || e.video_id.starts_with(NUVIO_SYNTHETIC_EPISODE)
            || e.video_id == e.content_id;
        if synthetic {
            let (season, episode) = e.season.zip(e.episode)?;
            let (_, meta) = self.series_of(&content_id)?;
            let (_, _, video) = episodes_of(&meta)
                .into_iter().find(|(s, n, _)| i64::from(*s) == season && i64::from(*n) == episode)?;
            return Some(self.episode_key(&content_id, &video.id));
        }
        let raw_series = episode_series_id(&e.video_id, &e.content_id);
        let series = self.imdb_base("series", raw_series).unwrap_or_else(|| raw_series.to_string());
        let (video_base, suffix) = split_content_id(&e.video_id);
        let video = match self.imdb_base("series", video_base) {
            Some(base) => format!("{base}{suffix}"),
            None => e.video_id.clone(),
        };
        Some(self.keys.key_of(&Ident::Episode { series, video }))
    }

    /// The count a client polls to learn that account content (a merged resume point) arrived.
    /// Monotonic, and bumped only for real changes.
    pub fn content_generation(&self) -> u64 {
        self.content_gen.load(Ordering::Relaxed)
    }

    fn bump_content(&self) {
        self.content_gen.fetch_add(1, Ordering::Relaxed);
    }

    /// The Stremio-compatible streaming server torrents play through, if any.
    pub fn torrent_server(&self) -> Option<String> {
        self.torrent_server.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The audio codecs this TV decodes: a stream whose label names another ranks after the
    /// playable ones of the same resolution.
    pub fn set_audio_codecs(&self, codecs: Vec<String>) {
        *self.audio_codecs.write().unwrap_or_else(|e| e.into_inner()) = codecs;
    }

    pub fn set_torrent_server(&self, server: Option<String>) {
        *self.torrent_server.write().unwrap_or_else(|e| e.into_inner()) = server;
        self.streams.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// Drop every cache that depends on the addon set (after Settings changed it).
    pub fn addons_changed(&self) {
        self.metas.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.poster_fallbacks.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.catalogs.lock().unwrap_or_else(|e| e.into_inner()).clear();
        *self.catalog_list.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.streams.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    // ---- Fork Rofocale11: durable cache doors -----------------------------------------------

    /// The (type, id) key shared by the enrichment and IMDb tables. Keeping the two kinds apart by
    /// their type prefix stops a movie's answer from being read for a same-numbered show.
    fn cache_key(type_: &str, id: &str) -> String {
        format!("{type_}:{id}")
    }

    fn enrich_get(&self, type_: &str, id: &str) -> Option<Enrichment> {
        let bytes = plx_nuvio_store::get()?.cache_get(NuvioCache::Enrich, &Self::cache_key(type_, id))?;
        serde_json::from_slice::<Enrichment>(&bytes).ok()
    }

    fn enrich_put(&self, type_: &str, id: &str, value: &Enrichment) {
        if let (Some(store), Ok(bytes)) = (plx_nuvio_store::get(), serde_json::to_vec(value)) {
            let _ = store.cache_put(NuvioCache::Enrich, &Self::cache_key(type_, id), ENRICH_TTL, &bytes);
        }
    }

    /// The IMDb id behind a content id, resolved through TMDB only for a `tmdb:<n>` id and cached
    /// positive AND negative (an empty value is a stored miss) so a poster never re-fetches it.
    fn imdb_base(&self, type_: &str, base: &str) -> Option<String> {
        if is_imdb_id(base) {
            return Some(base.to_string());
        }
        let tmdb_id = tmdb_content_number(base)?;
        let key = Self::cache_key(type_, base);
        if let Some(store) = plx_nuvio_store::get() {
            if let Some(bytes) = store.cache_get(NuvioCache::Imdb, &key) {
                return (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned());
            }
        }
        let client = TmdbClient::new();
        let imdb = if client.configured() {
            let found = if is_series(type_) { client.tv_external_ids(tmdb_id) } else { client.movie_external_ids(tmdb_id) };
            found.ok().flatten().filter(|id| is_imdb_id(id))
        } else {
            None
        };
        if let Some(store) = plx_nuvio_store::get() {
            let _ = store.cache_put(NuvioCache::Imdb, &key, IMDB_TTL, imdb.as_deref().unwrap_or("").as_bytes());
        }
        imdb
    }

    /// The addon id an item's streams/subtitles are asked for: a `tmdb:<n>` catalog id is upgraded
    /// to the IMDb id TMDB names. An episode keeps its `:S:E` tail; a miss falls back to the original
    /// id, since an empty Source Picker is worse than one an addon may still resolve.
    fn addon_id_for(&self, type_: &str, id: &str) -> String {
        let (base, suffix) = split_content_id(id);
        match self.imdb_base(type_, base) {
            Some(imdb) => format!("{imdb}{suffix}"),
            None => id.to_string(),
        }
    }

    /// Resolve a real item's TMDB id: Cinemeta's `moviedb_id` first so a Cinemeta title never pays
    /// a `/find` round trip, then the id itself, then its IMDb id.
    fn resolve_tmdb_id(&self, type_: &str, id: &str, imdb_id: Option<&str>, moviedb_id: Option<u64>) -> Option<u64> {
        if let Some(tmdb) = moviedb_id.filter(|id| *id > 0) {
            return Some(tmdb);
        }
        let client = TmdbClient::new();
        if !client.configured() {
            return tmdb_content_number(id);
        }
        let found = if is_series(type_) { client.series_id(id) } else { client.movie_id(id) };
        if let Ok(Some(tmdb)) = found {
            return Some(tmdb);
        }
        if let Some(imdb) = imdb_id.filter(|imdb| *imdb != id) {
            let found = if is_series(type_) { client.series_id(imdb) } else { client.movie_id(imdb) };
            return found.ok().flatten();
        }
        None
    }

    /// The backdrop URL for a title, using the same id resolution as enrichment. Series and movies
    /// both carry `backdrop_path` on their one details answer.
    fn tmdb_backdrop_for(&self, type_: &str, id: &str, meta: Option<&Meta>) -> Option<String> {
        let moviedb = meta.and_then(|m| m.moviedb_id);
        let imdb = meta.and_then(|m| m.imdb_id.clone());
        let tmdb_id = moviedb
            .filter(|id| *id > 0)
            .or_else(|| tmdb_content_number(id))
            .or_else(|| self.resolve_tmdb_id(type_, id, imdb.as_deref(), None))?;
        let client = TmdbClient::new();
        if !client.configured() { return None; }
        let backdrop = if is_series(type_) {
            client.tv_details(tmdb_id).ok().and_then(|d| d.backdrop_path)
        } else {
            client.movie_details(tmdb_id).ok().and_then(|d| d.backdrop_path)
        };
        tmdb_backdrop(backdrop.as_deref())
    }

    // ---- data access ------------------------------------------------------------------------

    fn catalog_list(&self) -> Vec<CatalogInfo> {
        let mut g = self.catalog_list.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = g.as_ref() {
            if at.elapsed() < CATALOG_TTL {
                return v.clone();
            }
        }
        let v = self.backend.catalogs();
        *g = Some((Instant::now(), v.clone()));
        v
    }

    fn first_catalog(&self, section: i64) -> Option<CatalogInfo> {
        self.catalog_list().into_iter().find(|c| if section == SECTION_SHOWS { is_series(&c.type_) } else { c.type_ == "movie" })
    }

    /// Rows `[start, start+size)` of a catalog and the total to report.
    fn catalog_range(&self, key: &str, genre: Option<&str>, start: usize, size: usize) -> (Vec<MetaPreview>, i64) {
        let entry = {
            let mut g = self.catalogs.lock().unwrap_or_else(|e| e.into_inner());
            let k = (key.to_string(), genre.map(str::to_string));
            let fresh = g.get(&k).is_some_and(|c| c.lock().unwrap_or_else(|e| e.into_inner()).at.elapsed() < CATALOG_TTL);
            if !fresh {
                g.insert(k.clone(), Arc::new(Mutex::new(CatalogCache { at: Instant::now(), rows: Vec::new(), exhausted: false })));
            }
            g[&k].clone()
        };
        // One fetcher per catalog at a time: the lock is held across the page fetch on purpose.
        let mut c = entry.lock().unwrap_or_else(|e| e.into_inner());
        let mut guard = 0;
        while c.rows.len() < start + size && !c.exhausted && guard < 20 {
            guard += 1;
            match self.backend.catalog_page(key, c.rows.len() as u32, genre) {
                Ok(page) => {
                    let before = c.rows.len();
                    for p in page {
                        if !c.rows.iter().any(|r| r.id == p.id) {
                            c.rows.push(p);
                        }
                    }
                    if c.rows.len() == before {
                        c.exhausted = true;
                    }
                }
                Err(()) => break,
            }
        }
        let rows = c.rows.iter().skip(start).take(size).cloned().collect();
        let total = c.rows.len() as i64 + if c.exhausted { 0 } else { MORE_HINT };
        (rows, total)
    }

    fn meta(&self, type_: &str, id: &str) -> Option<Arc<Meta>> {
        let k = (type_.to_string(), id.to_string());
        // Fork Rofocale11: a series' `videos` is its episode list and must refresh as episodes air,
        // while a released movie's metadata is stable for a month.
        let ttl = if is_series(type_) { META_TTL } else { META_TTL_MOVIES };
        if let Some((at, m)) = self.metas.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
            if at.elapsed() < ttl {
                return Some(m.clone());
            }
        }
        let m = Arc::new(self.backend.meta(type_, id)?);
        let mut g = self.metas.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() > 400 {
            g.clear();
        }
        g.insert(k, (Instant::now(), m.clone()));
        Some(m)
    }

    /// The (type, id) an item's streams are asked for, and its runtime.
    fn stream_target(&self, ident: &Ident) -> Option<(String, String)> {
        match ident {
            Ident::Title { type_, id } if !is_series(type_) => Some((type_.clone(), id.clone())),
            Ident::Episode { video, .. } => Some(("series".into(), video.clone())),
            _ => None,
        }
    }

    fn slot(&self, rk: i64, ident: &Ident) -> Option<Arc<StreamSlot>> {
        let (type_, id) = self.stream_target(ident)?;
        // Fork Rofocale11: addons never see a synthetic number. The one function that starts BOTH
        // fetches translates it to `tmdb:<id>` (and a real `tmdb:` catalog id to its IMDb id), so
        // streams and subtitles always ask for the same identity (docs/nuvio/ID-MANAGEMENT.md).
        let addon_id = translate_to_stremio_id(rk).unwrap_or(id);
        let translated_id = self.addon_id_for(&type_, &addon_id);
        let mut g = self.streams.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.get(&rk) {
            if s.started.elapsed() < STREAM_TTL {
                return Some(s.clone());
            }
        }
        if g.len() > 200 {
            g.retain(|_, s| s.started.elapsed() < STREAM_TTL);
        }
        let slot = Arc::new(StreamSlot { started: Instant::now(), state: Mutex::new(SlotState::default()), cv: Condvar::new() });
        g.insert(rk, slot.clone());
        drop(g);
        let (backend, s2, deadline) = (self.backend.clone(), slot.clone(), self.cfg.stream_deadline);
        let subs_backend = self.backend.clone();
        let (st, sid) = (type_.clone(), translated_id.clone());
        // Subtitles ride beside the streams, never in front of them.
        let slot_subs = slot.clone();
        std::thread::Builder::new()
            .name("pmsshim-streams".into())
            .spawn(move || {
                let mut on_batch = |b: &[Stream]| {
                    let mut st = s2.state.lock().unwrap_or_else(|e| e.into_inner());
                    let room = (PART_STRIDE as usize - 1).saturating_sub(st.streams.len());
                    st.streams.extend(b.iter().take(room).cloned());
                    s2.cv.notify_all();
                };
                let all = backend.streams(&type_, &translated_id, deadline, &mut on_batch);
                let mut st = s2.state.lock().unwrap_or_else(|e| e.into_inner());
                // A backend that only returns at the end (no batches) still lands here.
                if st.streams.is_empty() && !all.is_empty() {
                    st.streams = all.into_iter().take(PART_STRIDE as usize - 1).collect();
                }
                st.done = true;
                s2.cv.notify_all();
            })
            .ok()?;
        let _ = std::thread::Builder::new().name("pmsshim-subs".into()).spawn(move || {
            let subs = subs_backend.subtitles(&st, &sid);
            let mut g = slot_subs.state.lock().unwrap_or_else(|e| e.into_inner());
            // Ids are assigned by the shim on first read (see `ranked`), so only stash here.
            g.subs = subs.into_iter().take(40).map(|s| SubRef { id: 0, lang: format!("{}\u{0}{}", s.lang, s.url) }).collect();
        });
        Some(slot)
    }

    /// The ranked playable streams of an item (waiting per the deadline/grace rules) and the
    /// subtitles known so far. Each entry carries its arrival index.
    fn ranked(&self, rk: i64, ident: &Ident, runtime_secs: Option<u32>, max_wait: Duration) -> (Vec<(usize, Classified)>, Vec<SubRef>, bool) {
        let Some(slot) = self.slot(rk, ident) else { return (Vec::new(), Vec::new(), true) };
        let deadline = self.cfg.stream_deadline.min(slot.started.elapsed() + max_wait);
        let mut st = slot.state.lock().unwrap_or_else(|e| e.into_inner());
        let ts_owned = self.torrent_server();
        let ts = ts_owned.as_deref();
        let mut first_playable: Option<Instant> = None;
        loop {
            if st.done {
                break;
            }
            let elapsed = slot.started.elapsed();
            if elapsed >= deadline {
                break;
            }
            if first_playable.is_none() && st.streams.iter().any(|s| shim_media::playable_url(s, ts).is_some()) {
                first_playable = Some(Instant::now());
            }
            let mut wait = deadline - elapsed;
            if let Some(t) = first_playable {
                let g = self.cfg.stream_grace.saturating_sub(t.elapsed());
                if g.is_zero() {
                    break;
                }
                wait = wait.min(g);
            }
            st = slot.cv.wait_timeout(st, wait.min(Duration::from_millis(250))).unwrap_or_else(|e| e.into_inner()).0;
        }
        // Assign subtitle ids once, registering their URLs for `/library/streams/{id}`.
        for s in st.subs.iter_mut().filter(|s| s.id == 0) {
            let id = self.next_sub.fetch_add(1, Ordering::Relaxed);
            if let Some((lang, url)) = s.lang.split_once('\u{0}') {
                self.sub_urls.lock().unwrap_or_else(|e| e.into_inner()).insert(id, url.to_string());
                s.lang = lang.to_string();
            }
            s.id = id;
        }
        let subs: Vec<SubRef> = st.subs.iter().map(|s| SubRef { id: s.id, lang: s.lang.clone() }).collect();
        let indexed: Vec<Stream> = st.streams.clone();
        let selected = st.selected;
        let done = st.done;
        drop(st);
        let mut out: Vec<(usize, Classified)> = Vec::new();
        let audio = self.audio_codecs.read().unwrap_or_else(|e| e.into_inner()).clone();
        for c in shim_media::rank(&indexed, runtime_secs, ts, &audio) {
            // `rank` clones; recover the arrival index by identity of the URL + name.
            let i = indexed.iter().position(|s| shim_media::playable_url(s, ts).as_deref() == Some(c.url.as_str()) && s.name == c.stream.name).unwrap_or(0);
            out.push((i, c));
        }
        if let Some(sel) = selected {
            if let Some(p) = out.iter().position(|(i, _)| *i == sel) {
                let x = out.remove(p);
                out.insert(0, x);
            }
        }
        (out, subs, done)
    }

    // ---- rows -------------------------------------------------------------------------------

    fn watch_fields(&self, row: &mut Value, rk: i64) {
        let w: Watch = self.watch.get(rk);
        if w.offset_ms > 0 {
            row["viewOffset"] = json!(w.offset_ms);
        }
        if w.view_count > 0 {
            row["viewCount"] = json!(w.view_count);
        }
        if w.last_viewed_at > 0 {
            row["lastViewedAt"] = json!(w.last_viewed_at);
        }
    }

    fn preview_row(&self, p: &MetaPreview) -> Value {
        self.preview_row_imdb(p, None)
    }

    /// `imdb` is the full Meta's IMDb id when the caller has one; a preview alone has only its addon
    /// id, which `poster_url` resolves itself.
    fn preview_row_imdb(&self, p: &MetaPreview, imdb: Option<&str>) -> Value {
        let poster = self.poster_url(&p.type_, &p.id, imdb, p.poster.as_deref());
        let mut row = self.preview_row_raw(p);
        row["thumb"] = json!(poster);
        row
    }

    /// The same row without resolving a poster. Fork Rofocale11: a TMDB-only related title carries
    /// no IMDb id, so resolving one here would cost one HTTP request per card on the detail page's
    /// critical path. The related builder already has the TMDB poster to draw.
    fn preview_row_raw(&self, p: &MetaPreview) -> Value {
        let rk = self.keys.key_of(&Ident::Title { type_: p.type_.clone(), id: p.id.clone() });
        let series = is_series(&p.type_);
        let section = if series { SECTION_SHOWS } else { SECTION_MOVIES };
        let mut row = json!({
            "ratingKey": rk.to_string(), "key": format!("/library/metadata/{rk}"),
            "guid": format!("nuvio://{}/{}", p.type_, p.id),
            "type": if series { "show" } else { "movie" },
            "title": p.name, "titleSort": p.name,
            "librarySectionID": section, "librarySectionTitle": if series { "TV Shows" } else { "Movies" },
            "librarySectionKey": format!("/library/sections/{section}"),
            "summary": p.description.clone().unwrap_or_default(),
            "thumb": p.poster.clone().unwrap_or_default(),
            "art": p.background.clone().unwrap_or_default(),
            "addedAt": 0, "updatedAt": 0,
            "Genre": tags(&p.genres, false),
        });
        if let Some(y) = year_of(p.release_info.as_deref()) {
            row["year"] = json!(y);
        }
        if let Some(r) = rating_of(p.imdb_rating.as_deref()) {
            row["audienceRating"] = json!(r);
            row["audienceRatingImage"] = json!("imdb://image.rating");
        }
        if let Some(score) = tomato_score(p) {
            let image = if score >= 60.0 { "rottentomatoes://image.rating.ripe" } else { "rottentomatoes://image.rating.rotten" };
            row["rating"] = json!(score / 10.0);
            row["ratingImage"] = json!(image);
            let mut ratings = vec![json!({"type": "critic", "value": score / 10.0, "image": image})];
            if let Some(imdb) = rating_of(p.imdb_rating.as_deref()) {
                ratings.push(json!({"type": "audience", "value": imdb, "image": "imdb://image.rating"}));
            }
            row["Rating"] = json!(ratings);
        }
        if !series {
            self.watch_fields(&mut row, rk);
        }
        row
    }

    fn meta_row(&self, rk: i64, m: &Meta) -> Value {
        let p = MetaPreview {
            id: m.id.clone(),
            type_: m.type_.clone(),
            name: m.name.clone(),
            poster: m.poster.clone(),
            background: m.background.clone(),
            logo: m.logo.clone(),
            description: m.description.clone(),
            release_info: m.release_info.clone(),
            imdb_rating: m.imdb_rating.clone(),
            ratings: m.ratings.clone(), mdb_list_ratings: m.mdb_list_ratings.clone(),
            rotten_tomatoes_rating: m.rotten_tomatoes_rating.clone(),
            genres: m.genres.clone(),
        };
        let mut row = self.preview_row_imdb(&p, m.imdb_id.as_deref());
        row["ratingKey"] = json!(rk.to_string());
        row["key"] = json!(format!("/library/metadata/{rk}"));
        if let Some(d) = date_of(m.released.as_deref()) {
            row["originallyAvailableAt"] = json!(d);
        }
        if row.get("year").is_none() {
            if let Some(y) = year_of(m.released.as_deref()) {
                row["year"] = json!(y);
            }
        }
        row["Role"] = tags(&m.cast, true);
        row["Director"] = tags(&m.director, false);
        row["Writer"] = tags(&m.writer, false);
        if let Some(c) = &m.country {
            row["Country"] = tags(&[c.clone()], false);
        }
        let series = is_series(&m.type_);
        row["duration"] = json!(runtime_ms(m.runtime.as_deref()).unwrap_or(if series { DEFAULT_EPISODE_MS } else { DEFAULT_MOVIE_MS }));
        if series {
            let eps = episodes_of(m);
            let viewed = eps.iter().filter(|(_, _, v)| self.watch.get(self.episode_key(&m.id, &v.id)).view_count > 0).count();
            row["childCount"] = json!(seasons_of(m).len());
            row["leafCount"] = json!(eps.len());
            row["viewedLeafCount"] = json!(viewed);
        }
        row
    }

    fn episode_key(&self, series: &str, video: &str) -> i64 {
        self.keys.key_of(&Ident::Episode { series: series.into(), video: video.into() })
    }

    /// The URL to open for a ranked stream: itself, or — for a torrent marker — what the streaming
    /// server answers `/create` with (season/episode let it pick the file in a season pack).
    fn open_url(&self, rk: i64, stream: &Stream, url: &str) -> Option<String> {
        if !url.starts_with(shim_media::TORRENT_SCHEME) {
            return Some(url.to_string());
        }
        let key = (rk, url.to_string());
        if let Some(u) = self.resolved.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
            return Some(u.clone());
        }
        let server = self.torrent_server()?;
        let server = server.as_str();
        let (season, episode) = match self.keys.ident(rk) {
            Some(Ident::Episode { video, .. }) => {
                let mut it = video.rsplit(':').filter_map(|x| x.parse::<u32>().ok());
                let e = it.next();
                (it.next(), e)
            }
            _ => (None, None),
        };
        let u = plx_stremio::torrent::resolve(server, stream, season, episode).ok()?;
        self.resolved.lock().unwrap_or_else(|e| e.into_inner()).insert(key, u.clone());
        Some(u)
    }

    fn season_row(&self, show_rk: i64, m: &Meta, n: u32) -> Value {
        let rk = self.keys.key_of(&Ident::Season { series: m.id.clone(), n });
        let eps: Vec<_> = episodes_of(m).into_iter().filter(|(s, _, _)| *s == n).collect();
        let viewed = eps.iter().filter(|(_, _, v)| self.watch.get(self.episode_key(&m.id, &v.id)).view_count > 0).count();
        json!({
            "ratingKey": rk.to_string(), "key": format!("/library/metadata/{rk}"),
            "guid": format!("nuvio://season/{}/{n}", m.id),
            "type": "season", "title": season_title(n), "titleSort": season_title(n), "index": n,
            "parentRatingKey": show_rk.to_string(), "parentKey": format!("/library/metadata/{show_rk}"),
            "parentTitle": m.name, "parentThumb": m.poster.clone().unwrap_or_default(),
            "thumb": m.poster.clone().unwrap_or_default(), "art": m.background.clone().unwrap_or_default(),
            "librarySectionID": SECTION_SHOWS, "librarySectionTitle": "TV Shows",
            "librarySectionKey": format!("/library/sections/{SECTION_SHOWS}"),
            "summary": m.description.clone().unwrap_or_default(),
            "leafCount": eps.len(), "viewedLeafCount": viewed,
            "addedAt": 0, "updatedAt": 0,
        })
    }

    fn episode_row(&self, show_rk: i64, m: &Meta, season: u32, ep: u32, v: &Video) -> Value {
        let rk = self.episode_key(&m.id, &v.id);
        let season_rk = self.keys.key_of(&Ident::Season { series: m.id.clone(), n: season });
        let title = v.title.clone().or_else(|| v.name.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| format!("Episode {ep}"));
        let mut row = json!({
            "ratingKey": rk.to_string(), "key": format!("/library/metadata/{rk}"),
            "guid": format!("nuvio://episode/{}", v.id),
            "type": "episode", "title": title, "titleSort": title, "index": ep, "parentIndex": season,
            "parentRatingKey": season_rk.to_string(), "parentKey": format!("/library/metadata/{season_rk}"),
            "parentTitle": season_title(season),
            "grandparentRatingKey": show_rk.to_string(), "grandparentKey": format!("/library/metadata/{show_rk}"),
            "grandparentTitle": m.name, "grandparentThumb": m.poster.clone().unwrap_or_default(),
            "grandparentArt": m.background.clone().unwrap_or_default(),
            "thumb": v.thumbnail.clone().or_else(|| m.background.clone()).unwrap_or_default(),
            "art": m.background.clone().unwrap_or_default(),
            "summary": v.overview.clone().unwrap_or_default(),
            "librarySectionID": SECTION_SHOWS, "librarySectionTitle": "TV Shows",
            "librarySectionKey": format!("/library/sections/{SECTION_SHOWS}"),
            "duration": runtime_ms(m.runtime.as_deref()).unwrap_or(DEFAULT_EPISODE_MS),
            "addedAt": 0, "updatedAt": 0,
        });
        if let Some(d) = date_of(v.released.as_deref()) {
            row["year"] = json!(year_of(Some(&d)));
            row["originallyAvailableAt"] = json!(d);
        }
        self.watch_fields(&mut row, rk);
        row
    }

    /// The series meta behind a season/episode ident, with the show's ratingKey.
    fn series_of(&self, series: &str) -> Option<(i64, Arc<Meta>)> {
        let m = self.meta("series", series)?;
        let rk = self.keys.key_of(&Ident::Title { type_: "series".into(), id: series.into() });
        Some((rk, m))
    }

    /// A list row (no Media) for any ratingKey; `None` when its source is gone.
    fn row(&self, rk: i64) -> Option<Value> {
        // Fork Rofocale11: synthetic keys have no key-table row, so they resolve through the TMDB
        // row builder instead. Collections and trailer clips have their own remembered rows.
        if synthetic_trailer_parent(rk).is_some() {
            return self.trailers.lock().unwrap_or_else(|e| e.into_inner()).get(&rk).cloned();
        }
        if let Some(id) = synthetic_collection_id(rk) {
            return self.tmdb_collection(id).map(|c| self.collection_row(&c));
        }
        let real = self.keys.ident(rk);
        let ident = real.clone().or_else(|| synthetic_ident(rk))?;
        match ident {
            Ident::Title { type_, id } => {
                if real.is_some() {
                    let m = self.meta(&type_, &id)?;
                    Some(self.meta_row(rk, &m))
                } else {
                    self.tmdb_synthetic_row(rk)
                }
            }
            Ident::Season { series, n } => {
                let (show, m) = self.series_of(&series)?;
                Some(self.season_row(show, &m, n))
            }
            Ident::Episode { series, video } => {
                let (show, m) = self.series_of(&series)?;
                let (s, e, v) = episodes_of(&m).into_iter().find(|(_, _, v)| v.id == video)?;
                Some(self.episode_row(show, &m, s, e, v))
            }
        }
    }

    /// The full item: the row plus, for a playable leaf, its `Media[]`. Media whose stream was
    /// probed already carry the real fields; `probe_index` probes that Media now (the decision
    /// path) and returns the verdict beside the row.
    fn item_probed(&self, rk: i64, probe_index: Option<usize>) -> Option<(Value, Option<Result<crate::shim_probe::ProbeInfo, String>>)> {
        // Fork Rofocale11: a synthetic related key has no key-table row; its Ident and its row both
        // come from the TMDB row builder.
        let ident = self.keys.ident(rk).or_else(|| synthetic_ident(rk))?;
        let mut row = self.row(rk)?;
        let mut verdict = None;
        if self.stream_target(&ident).is_some() {
            let dur = row["duration"].as_i64().unwrap_or(0);
            // Fork Rofocale11: a detail read must not hold the page behind addons that are still
            // answering. It paints from the "Finding streams…" placeholder while the slot keeps
            // fetching; only the play path waits the full deadline (docs/nuvio/DETAIL-LATENCY.md).
            let wait = if probe_index.is_some() { self.cfg.stream_deadline } else { Duration::ZERO };
            let (ranked, subs, done) = self.ranked(rk, &ident, u32::try_from(dur / 1000).ok(), wait);
            // Fork Rofocale11: publish the same ranked list the detail page just used to the Streams
            // panel's slot. Both menus must show one source; previously Streams re-ranked with a short
            // window and could answer empty while this `Media[]` held the very same stream.
            if !ranked.is_empty() {
                if let Some(slot) = self.streams.lock().unwrap_or_else(|e| e.into_inner()).get(&rk).cloned() {
                    slot.state.lock().unwrap_or_else(|e| e.into_inner()).ranked = ranked.clone();
                }
            }
            let mut media = Vec::with_capacity(ranked.len().max(1));
            // Pre-probe the leading stream once the list is complete, off this request: the play that
            // usually follows a detail page then finds real fields (and a warmed source) waiting.
            if probe_index.is_none() && done && self.probes.has_prober() {
                if let Some((_, c)) = ranked.first() {
                    let url = self.resolved.lock().unwrap_or_else(|e| e.into_inner()).get(&(rk, c.url.clone())).cloned().unwrap_or_else(|| c.url.clone());
                    if !url.starts_with(shim_media::TORRENT_SCHEME) && self.probes.cached(&url).is_none() {
                        if let Some(me) = self.me.get().and_then(std::sync::Weak::upgrade) {
                            let _ = std::thread::Builder::new().name("pmsshim-preprobe".into()).spawn(move || {
                                let _ = me.probes.probe(&url);
                            });
                        }
                    }
                }
            }
            for (n, (i, c)) in ranked.iter().enumerate() {
                let mut m = shim_media::media_json(rk, *i, c, dur, &subs);
                let probed = if probe_index == Some(n) {
                    let r = match self.open_url(rk, &c.stream, &c.url) {
                        Some(u) => self.probes.probe(&u),
                        None => Some(Err("the streaming server could not open this torrent".into())),
                    };
                    verdict = r.clone();
                    r
                } else {
                    self.resolved.lock().unwrap_or_else(|e| e.into_inner()).get(&(rk, c.url.clone())).cloned()
                        .or_else(|| Some(c.url.clone()))
                        .and_then(|u| self.probes.cached(&u))
                };
                if let Some(Ok(p)) = &probed {
                    crate::shim_probe::apply(&mut m, p);
                }
                media.push(m);
            }
            if media.is_empty() && !done {
                // Addons still answering: a Part that plays whichever stream leads at play time, so
                // the page is never held up and its Play button always has something to start.
                let pending = shim_media::Classified { stream: Stream::default(), info: plx_stremio::classify::StreamInfo { label: "Finding streams…".into(), ..Default::default() }, url: String::new() };
                media.push(shim_media::media_json(rk, BEST_PART, &pending, dur, &subs));
            }
            if let Some(d) = media.first().and_then(|m| m["duration"].as_i64()).filter(|d| *d > 0) {
                row["duration"] = json!(d);
            }
            row["Media"] = Value::Array(media);
        }
        Some((row, verdict))
    }

    fn item(&self, rk: i64) -> Option<Value> {
        self.item_probed(rk, None).map(|(row, _)| row)
    }

    /// The row an artwork request needs. Fork Rofocale11: `row()` is addon meta only, so it has no
    /// TMDB clearLogo; applying the persisted enrichment here gives `photo` the same logo the
    /// metadata route published, without probing streams for an image fetch.
    fn art_row(&self, rk: i64) -> Option<Value> {
        let mut row = self.row(rk)?;
        if let Some(Ident::Title { type_, id }) = self.keys.ident(rk) {
            if let Some(cached) = self.enrich_get(&type_, &id) {
                cached.apply(rk, &mut row);
            }
        }
        Some(row)
    }

    fn decision(&self, req: &Request) -> Response {
        let rk = req.q("path").and_then(trailing_key).unwrap_or(-1);
        let index: usize = req.q("mediaIndex").and_then(|v| v.parse().ok()).unwrap_or(0);
        let refuse = |why: String| {
            Response::json(&container(json!({"generalDecisionCode": 2000, "generalDecisionText": "Direct play is not available.",
                "transcodeDecisionCode": 4005, "transcodeDecisionText": why, "mdeDecisionCode": 2000, "Metadata": []})))
        };
        let Some((mut row, verdict)) = self.item_probed(rk, Some(index)) else {
            return refuse("This title is no longer available from your addons.".into());
        };
        if row["Media"].as_array().is_some_and(Vec::is_empty) {
            return refuse("No playable stream was found for this title.".into());
        }
        match verdict {
            Some(Err(e)) => return refuse(format!("This stream could not be opened ({e}). Pick another stream.")),
            Some(Ok(p)) => {
                if let Some(why) = crate::shim_probe::refusal(&p) {
                    return refuse(why);
                }
            }
            None if self.probes.has_prober() => return refuse("Streams are still being discovered. Try again or choose another stream.".into()),
            None => {}
        }
        // Return the chosen Media first: the native route consumes this response as the
        // authoritative codec/track description, including an explicit non-leading choice.
        if let Some(ms) = row["Media"].as_array_mut() {
            if index >= ms.len() { return refuse("This stream is no longer available. Choose another stream.".into()); }
            ms.swap(0, index);
        }
        if let Some(ms) = row["Media"].as_array_mut() {
            for m in ms.iter_mut() {
                if let Some(parts) = m["Part"].as_array_mut() {
                    for part in parts {
                        part["decision"] = json!("directplay");
                        if let Some(ss) = part["Stream"].as_array_mut() {
                            for s in ss {
                                s["decision"] = json!("copy");
                            }
                        }
                    }
                }
            }
        }
        Response::json(&container(json!({"generalDecisionCode": 1000, "generalDecisionText": "Direct play OK", "mdeDecisionCode": 1000, "Metadata": [row]})))
    }

    // ---- hubs ------------------------------------------------------------------------------

    /// BetterPosters is only served for IMDb ids, so the key passed to the backend is the RESOLVED
    /// id: the full Meta's `imdb_id` when present, else TMDB's external_ids for `tmdb:<n>` (cached
    /// positive and negative). An id with no mapping keeps the addon's own artwork.
    fn poster_url(&self, type_: &str, id: &str, imdb: Option<&str>, original: Option<&str>) -> String {
        let resolved = imdb.filter(|id| is_imdb_id(id)).map(str::to_string)
            .or_else(|| is_imdb_id(id).then(|| id.to_string()))
            .or_else(|| self.imdb_base(type_, id));
        let poster = self.backend.poster(resolved.as_deref().unwrap_or(id), original).unwrap_or_default();
        if let Some(original) = original.filter(|u| *u != poster) {
            let mut fallbacks = self.poster_fallbacks.lock().unwrap_or_else(|e| e.into_inner());
            if fallbacks.len() >= 4096 { fallbacks.clear(); }
            fallbacks.insert(poster.clone(), original.to_string());
        }
        poster
    }

    fn continue_rows(&self, section: Option<i64>) -> Vec<Value> {
        self.watch
            .in_progress()
            .into_iter()
            .filter_map(|(rk, w, mut row)| {
                if section.is_some_and(|s| row["librarySectionID"].as_i64() != Some(s)) {
                    return None;
                }
                row["viewOffset"] = json!(w.offset_ms);
                row["lastViewedAt"] = json!(w.last_viewed_at);
                if w.duration_ms > 0 {
                    row["duration"] = json!(w.duration_ms);
                }
                if let Some(ident) = self.keys.ident(rk).or_else(|| synthetic_ident(rk)) {
                    let (type_, id) = match &ident {
                        Ident::Title { type_, id } => (type_.clone(), id.clone()),
                        Ident::Season { series, .. } | Ident::Episode { series, .. } => ("series".into(), series.clone()),
                    };
                    // Fork Rofocale11: the thumb stays the BetterPosters poster; the landscape art
                    // comes from TMDB when it answers, so Continue Watching never shows a portrait
                    // crop. A 404 on the backdrop degrades to that poster.
                    row["thumb"] = json!(self.poster_url(&type_, &id, None, row["thumb"].as_str()));
                    if !usable_landscape(row["art"].as_str()) {
                        let meta = self.meta(&type_, &id);
                        if let Some(backdrop) = self.tmdb_backdrop_for(&type_, &id, meta.as_deref()) {
                            row["art"] = json!(backdrop);
                            if let Some(poster) = row["thumb"].as_str() {
                                let mut fallbacks = self.poster_fallbacks.lock().unwrap_or_else(|e| e.into_inner());
                                if fallbacks.len() >= 4096 { fallbacks.clear(); }
                                fallbacks.insert(backdrop, poster.to_string());
                            }
                        }
                    }
                }
                Some(row)
            })
            .collect()
    }

    fn continue_hub(&self, count: usize, section: Option<i64>) -> Value {
        let rows: Vec<Value> = self.continue_rows(section).into_iter().take(count).collect();
        json!({"title": "Continue Watching", "type": "mixed", "hubIdentifier": "home.continue",
               "key": "/hubs/continueWatching", "size": rows.len(), "more": false, "Metadata": rows})
    }

    fn saved_hub(&self, count: usize) -> Option<Value> {
        let items = self.account.saved_library(false).ok()?;
        let rows: Vec<_> = items.iter().take(count).map(|p| { let mut row = self.preview_row(p); row["librarySectionTitle"] = json!(""); row }).collect();
        if rows.is_empty() { return None; }
        Some(json!({"title":"My List","type":"mixed","hubIdentifier":"movie.recentlyadded.nuvio-list",
            "key":"/hubs/nuvio/list","size":rows.len(),"totalSize":items.len(),"more":items.len()>rows.len(),"Metadata":rows}))
    }

    fn saved_item(&self, req: &Request) -> Response {
        let Some(Ident::Title { type_, id }) = req.q("ratingKey").and_then(|v| v.parse().ok()).and_then(|rk| self.keys.ident(rk)) else { return Response::json_status(400,&json!({"error":"invalid title"})); };
        if !self.account.signed_in() { return Response::json(&json!({"signedIn":false,"saved":false})); }
        let result = (|| {
            if req.method == "POST" {
                let saved = match req.q("saved") { Some("1") => true, Some("0") => false, _ => return Err(plx_stremio::model::Error::Status(400)) };
                let meta = self.meta(&type_, &id).ok_or(plx_stremio::model::Error::Status(404))?;
                self.account.set_saved(&meta, saved)?;
            }
            let items = self.account.saved_library(req.method != "POST")?;
            Ok(items.iter().any(|p| p.type_ == type_ && p.id == id))
        })();
        match result {
            Ok(saved) => Response::json(&json!({"signedIn":true,"saved":saved})),
            Err(plx_stremio::model::Error::Status(400)) => Response::json_status(400,&json!({"error":"invalid request"})),
            Err(_) => Response::json_status(503,&json!({"error":"My List could not sync. Try again."})),
        }
    }

    /// `pageable`: Home's hubs use the per-library "recently added" identifier family, the only
    /// one `plx_data::pms` pages past the first window, with an empty `librarySectionTitle` on the
    /// rows so the app keeps the catalog's own title (`hub_title::localized_hub_title` falls back to
    /// it). A section's own hubs must not: there that family is retitled "Recently Added".
    fn catalog_hub(&self, i: usize, c: &CatalogInfo, count: usize, pageable: bool) -> Value {
        let (rows, total) = self.catalog_range(&c.key, None, 0, count);
        let series = is_series(&c.type_);
        let rows: Vec<Value> = rows
            .iter()
            .map(|p| {
                let mut r = self.preview_row(p);
                if pageable {
                    r["librarySectionTitle"] = json!("");
                }
                r
            })
            .collect();
        let id = if pageable { format!("{}.recentlyadded.nuvio-{i}", if series { "show" } else { "movie" }) } else { format!("nuvio.catalog.{i}") };
        json!({"title": c.title, "type": if series { "show" } else { "movie" },
               "hubIdentifier": id, "key": format!("/hubs/nuvio/{i}"),
               "more": total > rows.len() as i64, "size": rows.len(), "totalSize": total, "Metadata": rows})
    }

    fn acquire(&self) {
        let (m, cv) = &self.permits;
        let mut n = m.lock().unwrap_or_else(|e| e.into_inner());
        while *n == 0 {
            n = cv.wait(n).unwrap_or_else(|e| e.into_inner());
        }
        *n -= 1;
    }

    fn release(&self) {
        let (m, cv) = &self.permits;
        *m.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        cv.notify_one();
    }

    /// Catalog hubs in catalog order, fetched with bounded parallelism. Whatever has not answered
    /// within `budget` is left out of THIS answer and keeps fetching into the cache.
    fn catalog_hubs(self: &Arc<Self>, filter: impl Fn(&CatalogInfo) -> bool, count: usize, pageable: bool, budget: Duration) -> Vec<Value> {
        let cats: Vec<(usize, CatalogInfo)> = self.catalog_list().into_iter().enumerate().filter(|(_, c)| filter(c)).collect();
        let (tx, rx) = std::sync::mpsc::channel::<(usize, Value)>();
        for (pos, (i, c)) in cats.iter().cloned().enumerate() {
            let (me, tx) = (self.clone(), tx.clone());
            let _ = std::thread::Builder::new().name("pmsshim-catalog".into()).spawn(move || {
                me.acquire();
                let hub = me.catalog_hub(i, &c, count, pageable);
                me.release();
                let _ = tx.send((pos, hub));
            });
        }
        drop(tx);
        let deadline = Instant::now() + budget;
        let mut got: Vec<Option<Value>> = vec![None; cats.len()];
        let mut n = 0;
        while n < cats.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok((pos, hub)) => {
                    got[pos] = Some(hub);
                    n += 1;
                }
                Err(_) => break,
            }
        }
        got.into_iter().flatten().filter(|h| h["size"].as_u64() != Some(0)).collect()
    }

    /// Fill the catalog list and every catalog's first page in the background (boot, or after the
    /// addon set changed), so the first `/hubs` is mostly cache hits.
    pub fn warm(self: &Arc<Self>) {
        let me = self.clone();
        let _ = std::thread::Builder::new().name("pmsshim-warm".into()).spawn(move || {
            let _ = me.account.saved_library(false);
            let _ = me.catalog_hubs(|_| true, 12, true, Duration::from_secs(60));
        });
    }

    // ---- Fork Rofocale11: TMDB enrichment, hubs and special routes --------------------------

    /// One details answer per detail open: cast, trailers, the related shelf, the title logo and
    /// the landscape backdrop. The parsed result is persisted so later opens are local. metahub art
    /// stays on the row only when TMDB has nothing to say.
    fn enrich_row(&self, rk: i64, row: &mut Value) {
        let Some(Ident::Title { type_, id }) = self.keys.ident(rk) else { return };
        if let Some(cached) = self.enrich_get(&type_, &id) {
            // An entry without the related shelf is incomplete (an older build): refresh it.
            if cached.related.is_some() {
                cached.apply(rk, row);
                if let Some(parts) = &cached.collection { self.remember_collection(parts); }
                return;
            }
        }
        let meta = self.meta(&type_, &id);
        let client = TmdbClient::new();
        if !client.configured() { return; }
        let tmdb_id = self.resolve_tmdb_id(&type_, &id, meta.as_ref().and_then(|m| m.imdb_id.as_deref()), meta.as_ref().and_then(|m| m.moviedb_id));
        let Some(tmdb_id) = tmdb_id else { return; };
        let series = is_series(&type_);
        let mut learned = Enrichment { tmdb_id, ..Default::default() };
        let (credits, related, videos, backdrop, logo, collection_ref) = if series {
            match client.tv_details(tmdb_id) {
                Ok(d) => {
                    let TmdbShowDetails { credits, recommendations, videos, backdrop_path, images, .. } = d;
                    (credits, RelatedSource::Shows(recommendations.results), videos, backdrop_path, tmdb_logo(images.as_ref()), None)
                }
                Err(_) => return,
            }
        } else {
            match client.movie_details(tmdb_id) {
                Ok(d) => {
                    let TmdbMovieDetails { credits, recommendations, videos, backdrop_path, images, belongs_to_collection, .. } = d;
                    (credits, RelatedSource::Movies(recommendations.results), videos, backdrop_path, tmdb_logo(images.as_ref()), belongs_to_collection)
                }
                Err(_) => return,
            }
        };
        set_roles(row, &credits.cast);
        learned.roles = row.get("Role").cloned();
        let extras = extras_from_videos(videos.as_ref(), rk);
        if extras["size"].as_u64().unwrap_or(0) > 0 {
            if let Some(rows) = extras["Metadata"].as_array() {
                for clip in rows { self.remember_trailer(clip.clone()); }
            }
            row["Extras"] = extras.clone();
            learned.extras = Some(extras);
        }
        if let Some(backdrop) = tmdb_backdrop(backdrop.as_deref()) {
            row["art"] = json!(backdrop);
            learned.art = Some(backdrop);
        }
        if let Some(logo) = logo {
            set_logo(row, rk, &logo);
            learned.clear_logo = Some(logo);
        }
        learned.related = Some(related);
        learned.collection_ref = collection_ref;
        self.enrich_put(&type_, &id, &learned);
    }

    fn remember_trailer(&self, clip: Value) {
        if let (Some(rk), Some(map)) = (clip["ratingKey"].as_str().and_then(|s| s.parse().ok()), self.trailers.lock().ok()) {
            let mut map = map;
            if map.len() >= 256 { map.clear(); }
            map.insert(rk, clip);
        }
    }

    /// A title's TMDB id for `/related`: a synthetic key already names it; a real key resolves
    /// through Cinemeta's moviedb_id, then the id itself, then its IMDb id.
    fn related_tmdb_id(&self, rk: i64, series: bool) -> Option<u64> {
        if series {
            if let Some(id) = synthetic_show_tmdb_id(rk) { return Some(id); }
        } else if let Some(id) = synthetic_tmdb_id(rk) {
            return Some(id);
        }
        let type_ = if series { "series" } else { "movie" };
        match self.keys.ident(rk) {
            Some(Ident::Title { id, .. }) => {
                let meta = self.meta(type_, &id);
                self.resolve_tmdb_id(type_, &id, meta.as_ref().and_then(|m| m.imdb_id.as_deref()), meta.as_ref().and_then(|m| m.moviedb_id))
            }
            _ => None,
        }
    }

    fn rows_from_related(&self, source: &RelatedSource) -> (&'static str, Vec<Value>) {
        match source {
            RelatedSource::Movies(movies) => ("movie", movies.iter()
                .filter(|m| m.id > 0 && m.id < (i64::MAX - TMDB_FALLBACK_KEY_BASE) as u64)
                .take(10).map(|m| self.tmdb_recommendation_row(m)).collect()),
            RelatedSource::Shows(shows) => ("show", shows.iter()
                .filter(|s| s.id > 0 && s.id < (i64::MAX - TMDB_SHOW_KEY_BASE) as u64)
                .take(10).map(|s| self.tmdb_show_recommendation_row(s)).collect()),
        }
    }

    /// `/library/metadata/{rk}/related`. A cached title answers from its persisted enrichment; a
    /// synthetic key (never enriched) takes one TMDB details call. The franchise shelf is a second
    /// `collection.related.*` hub, not folded into More Like This.
    fn related_hub(&self, rk: i64) -> Response {
        let real = self.keys.ident(rk);
        let series = synthetic_show_tmdb_id(rk).is_some()
            || matches!(&real, Some(Ident::Title { type_, .. }) if is_series(type_));
        let movie = synthetic_tmdb_id(rk).is_some()
            || matches!(&real, Some(Ident::Title { type_, .. }) if type_ == "movie");
        if !series && !movie {
            return Response::json(&container(json!({"Hub": [related_hub_json("movie", Vec::new())]})));
        }
        let kind = if series { "show" } else { "movie" };
        if let Some(Ident::Title { type_, id }) = &real {
            if let Some(cached) = self.enrich_get(type_, id) {
                if let Some(source) = &cached.related {
                    let (kind, rows) = self.rows_from_related(source);
                    let mut hubs = vec![related_hub_json(kind, rows)];
                    let collection = cached.collection.clone().or_else(|| {
                        cached.collection_ref.as_ref().and_then(|c| self.tmdb_collection(c.id))
                    });
                    if let Some(parts) = collection {
                        self.remember_collection(&parts);
                        if let Some(hub) = self.collection_hub_json(&parts, cached.tmdb_id, &parts.name) { hubs.push(hub); }
                    }
                    return Response::json(&container(json!({"Hub": hubs})));
                }
            }
        }
        let client = TmdbClient::new();
        let empty = || Response::json(&container(json!({"Hub": [related_hub_json(kind, Vec::new())]})));
        if !client.configured() { return empty(); }
        let Some(tmdb_id) = self.related_tmdb_id(rk, series) else { return empty(); };
        let (source, collection_hub) = if series {
            match client.tv_details(tmdb_id) {
                Ok(d) => (RelatedSource::Shows(d.recommendations.results), None),
                Err(_) => return empty(),
            }
        } else {
            match client.movie_details(tmdb_id) {
                Ok(d) => {
                    let hub = self.collection_hub(rk, &d, &client);
                    (RelatedSource::Movies(d.recommendations.results), hub)
                }
                Err(_) => return empty(),
            }
        };
        let (kind, rows) = self.rows_from_related(&source);
        let mut hubs = vec![related_hub_json(kind, rows)];
        hubs.extend(collection_hub);
        Response::json(&container(json!({"Hub": hubs})))
    }

    /// `/library/metadata/{rk}/extras` — the same clip rows the detail answer carries in Extras.
    fn extras_route(&self, rk: i64) -> Response {
        if let Some(real) = self.keys.ident(rk) {
            if let Ident::Title { type_, id } = real {
                if let Some(extras) = self.enrich_get(&type_, &id).and_then(|e| e.extras) {
                    return Response::json(&container(extras));
                }
            }
        }
        if let Some(row) = self.tmdb_synthetic_row(rk) {
            if let Some(extras) = row.get("Extras").cloned() {
                return Response::json(&container(extras));
            }
        }
        Response::json(&container(json!({"Metadata": []})))
    }

    fn remember_collection(&self, collection: &TmdbCollection) {
        let mut map = self.collections.lock().unwrap_or_else(|e| e.into_inner());
        if map.len() >= 256 { map.clear(); }
        map.insert(collection.id, collection.clone());
    }

    fn remember_collection_for(&self, type_: &str, id: &str, collection: &TmdbCollection) {
        self.remember_collection(collection);
        if let Some(mut learned) = self.enrich_get(type_, id) {
            learned.collection = Some(collection.clone());
            self.enrich_put(type_, id, &learned);
        }
    }

    fn tmdb_collection(&self, id: u64) -> Option<TmdbCollection> {
        if let Some(collection) = self.collections.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned() {
            return Some(collection);
        }
        let client = TmdbClient::new();
        if !client.configured() { return None; }
        let collection = client.collection_details(id).ok()?;
        self.remember_collection(&collection);
        Some(collection)
    }

    fn collection_hub(&self, rk: i64, details: &TmdbMovieDetails, client: &TmdbClient) -> Option<Value> {
        let collection = details.belongs_to_collection.as_ref().filter(|c| c.id > 0)?;
        let parts = client.collection_details(collection.id).ok()?;
        if let Some(Ident::Title { type_, id }) = self.keys.ident(rk) {
            self.remember_collection_for(&type_, &id, &parts);
        } else {
            self.remember_collection(&parts);
        }
        self.collection_hub_json(&parts, details.id, &collection.name)
    }

    fn collection_hub_json(&self, parts: &TmdbCollection, own_id: u64, fallback_title: &str) -> Option<Value> {
        let rows: Vec<Value> = parts.parts.iter()
            .filter(|m| m.id > 0 && m.id != own_id && m.id < (i64::MAX - TMDB_FALLBACK_KEY_BASE) as u64)
            .map(|m| self.tmdb_recommendation_row(m)).collect();
        if rows.is_empty() { return None; }
        let title = if parts.name.trim().is_empty() { fallback_title } else { &parts.name };
        Some(json!({
            "title": title, "type": "movie",
            "hubIdentifier": format!("collection.related.{SECTION_MOVIES}.1"),
            "key": format!("/library/sections/{SECTION_MOVIES}/all?type=1&tagId={}", parts.id),
            "size": rows.len(), "totalSize": parts.parts.len(), "more": false, "Metadata": rows,
        }))
    }

    fn collection_row(&self, collection: &TmdbCollection) -> Value {
        let rk = TMDB_COLLECTION_KEY_BASE + collection.id as i64;
        let poster = tmdb_image(collection.poster_path.as_deref())
            .or_else(|| collection.parts.iter().find_map(|p| tmdb_image(p.poster_path.as_deref())))
            .unwrap_or_default();
        json!({
            "ratingKey": rk.to_string(), "key": format!("/library/collections/{rk}/children"),
            "guid": format!("nuvio://collection/{}", collection.id),
            "type": "collection", "title": collection.name, "titleSort": collection.name,
            "index": collection.id,
            "librarySectionID": SECTION_MOVIES, "librarySectionTitle": "Movies",
            "librarySectionKey": format!("/library/sections/{SECTION_MOVIES}"),
            "summary": collection.overview.clone().unwrap_or_default(),
            "thumb": poster, "childCount": collection.parts.len(), "collectionSort": 0,
            "addedAt": 0, "updatedAt": 0,
        })
    }

    fn collection_children(&self, collection: &TmdbCollection, start: usize, size: usize) -> Value {
        let rows: Vec<Value> = collection.parts.iter()
            .filter(|m| m.id > 0 && m.id < (i64::MAX - TMDB_FALLBACK_KEY_BASE) as u64)
            .skip(start).take(size).map(|m| self.tmdb_recommendation_row(m)).collect();
        let mut out = container(json!({"Metadata": rows}));
        out["MediaContainer"]["totalSize"] = json!(collection.parts.len());
        out
    }

    /// `/library/sections/{s}/collections` — the tagId resolution the collection page starts from.
    /// The shim has no library scan: only franchises this process already served are listed, ordered
    /// by id so a page's `start` means the same rows twice.
    fn section_collections(&self, req: &Request, section: i64) -> Response {
        let (start, size) = page_params(req);
        if section != SECTION_MOVIES { return Response::json(&container(json!({"Metadata": [], "totalSize": 0}))); }
        let mut all: Vec<TmdbCollection> = self.collections.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        all.sort_by_key(|c| c.id);
        let total = all.len();
        let rows: Vec<Value> = all.into_iter().skip(start).take(size.unwrap_or(50)).map(|c| self.collection_row(&c)).collect();
        Response::json(&container(json!({"Metadata": rows, "totalSize": total, "offset": start})))
    }

    fn tmdb_recommendation_row(&self, movie: &TmdbMovie) -> Value {
        let rk = TMDB_FALLBACK_KEY_BASE + movie.id as i64;
        if let Some(map) = self.tmdb_fallbacks.lock().ok() {
            let mut map = map;
            if map.len() >= 512 { map.clear(); }
            map.insert(rk, TmdbFallback::Movie(movie.clone()));
        }
        self.tmdb_fallback_row(movie, rk)
    }

    fn tmdb_fallback_row(&self, movie: &TmdbMovie, rk: i64) -> Value {
        let poster = tmdb_image(movie.poster_path.as_deref());
        let preview = MetaPreview {
            id: format!("tmdb:{}", movie.id), type_: "movie".into(), name: movie.title.clone(),
            poster: poster.clone(), background: tmdb_backdrop(movie.backdrop_path.as_deref()),
            release_info: movie.release_date.clone(), ..MetaPreview::default()
        };
        let mut row = self.preview_row_raw(&preview);
        row["ratingKey"] = json!(rk.to_string());
        row["key"] = json!(format!("/library/metadata/{rk}"));
        if let Some(poster) = poster { row["thumb"] = json!(poster); }
        if let Some(backdrop) = tmdb_backdrop(movie.backdrop_path.as_deref()) { row["art"] = json!(backdrop); }
        row["duration"] = json!(movie.runtime.filter(|n| *n > 0).map(|n| i64::from(n) * 60_000).unwrap_or(DEFAULT_MOVIE_MS));
        row
    }

    fn tmdb_show_recommendation_row(&self, show: &TmdbShow) -> Value {
        let rk = TMDB_SHOW_KEY_BASE + show.id as i64;
        if let Some(map) = self.tmdb_fallbacks.lock().ok() {
            let mut map = map;
            if map.len() >= 512 { map.clear(); }
            map.insert(rk, TmdbFallback::Show(show.clone()));
        }
        self.tmdb_show_fallback_row(show, rk)
    }

    fn tmdb_show_fallback_row(&self, show: &TmdbShow, rk: i64) -> Value {
        let poster = tmdb_image(show.poster_path.as_deref());
        let preview = MetaPreview {
            id: format!("tmdb:{}", show.id), type_: "series".into(), name: show.name.clone(),
            poster: poster.clone(), background: tmdb_backdrop(show.backdrop_path.as_deref()),
            release_info: show.first_air_date.clone(), ..MetaPreview::default()
        };
        let mut row = self.preview_row_raw(&preview);
        row["ratingKey"] = json!(rk.to_string());
        row["key"] = json!(format!("/library/metadata/{rk}"));
        if let Some(poster) = poster { row["thumb"] = json!(poster); }
        if let Some(backdrop) = tmdb_backdrop(show.backdrop_path.as_deref()) { row["art"] = json!(backdrop); }
        row["duration"] = json!(DEFAULT_EPISODE_MS);
        row
    }

    fn tmdb_details_row(&self, details: &TmdbMovieDetails, rk: i64) -> Value {
        let backdrop = tmdb_backdrop(details.backdrop_path.as_deref());
        let preview = MetaPreview {
            id: format!("tmdb:{}", details.id), type_: "movie".into(), name: details.title.clone(),
            background: backdrop.clone(), release_info: details.release_date.clone(), ..MetaPreview::default()
        };
        let mut row = self.preview_row(&preview);
        row["ratingKey"] = json!(rk.to_string());
        row["key"] = json!(format!("/library/metadata/{rk}"));
        set_roles(&mut row, &details.credits.cast);
        row["duration"] = json!(details.runtime.filter(|n| *n > 0).map(|n| i64::from(n) * 60_000).unwrap_or(DEFAULT_MOVIE_MS));
        if let Some(date) = date_of(details.release_date.as_deref()) { row["originallyAvailableAt"] = json!(date); }
        if let Some(backdrop) = backdrop { row["art"] = json!(backdrop); }
        if let Some(logo) = tmdb_logo(details.images.as_ref()) { set_logo(&mut row, rk, &logo); }
        let extras = extras_from_videos(details.videos.as_ref(), rk);
        if extras["size"].as_u64().unwrap_or(0) > 0 {
            if let Some(rows) = extras["Metadata"].as_array() { for clip in rows { self.remember_trailer(clip.clone()); } }
            row["Extras"] = extras;
        }
        row
    }

    fn tmdb_show_details_row(&self, details: &TmdbShowDetails, rk: i64) -> Value {
        let backdrop = tmdb_backdrop(details.backdrop_path.as_deref());
        let preview = MetaPreview {
            id: format!("tmdb:{}", details.id), type_: "series".into(), name: details.name.clone(),
            background: backdrop.clone(), release_info: details.first_air_date.clone(), ..MetaPreview::default()
        };
        let mut row = self.preview_row(&preview);
        row["ratingKey"] = json!(rk.to_string());
        row["key"] = json!(format!("/library/metadata/{rk}"));
        set_roles(&mut row, &details.credits.cast);
        row["duration"] = json!(DEFAULT_EPISODE_MS);
        if let Some(date) = date_of(details.first_air_date.as_deref()) { row["originallyAvailableAt"] = json!(date); }
        if let Some(backdrop) = backdrop { row["art"] = json!(backdrop); }
        if let Some(logo) = tmdb_logo(details.images.as_ref()) { set_logo(&mut row, rk, &logo); }
        let extras = extras_from_videos(details.videos.as_ref(), rk);
        if extras["size"].as_u64().unwrap_or(0) > 0 {
            if let Some(rows) = extras["Metadata"].as_array() { for clip in rows { self.remember_trailer(clip.clone()); } }
            row["Extras"] = extras;
        }
        row
    }

    /// Build a synthetic key's row from the remembered TMDB title, or from one cached details call
    /// after a restart. This is also the `photo` fallback: a synthetic key has no key-table row, so
    /// its thumb/art/logo must come from here.
    fn tmdb_synthetic_row(&self, rk: i64) -> Option<Value> {
        if let Some(parent) = synthetic_trailer_parent(rk) {
            let _ = parent;
            return self.trailers.lock().unwrap_or_else(|e| e.into_inner()).get(&rk).cloned();
        }
        if let Some(id) = synthetic_collection_id(rk) {
            return self.tmdb_collection(id).map(|c| self.collection_row(&c));
        }
        let remembered = self.tmdb_fallbacks.lock().unwrap_or_else(|e| e.into_inner()).get(&rk).cloned();
        let is_show = synthetic_show_tmdb_id(rk).is_some();
        if !is_show && synthetic_tmdb_id(rk).is_none() { return None; }
        if is_show {
            if let Some(TmdbFallback::Show(show)) = &remembered { return Some(self.tmdb_show_fallback_row(show, rk)); }
        } else if let Some(TmdbFallback::Movie(movie)) = &remembered {
            return Some(self.tmdb_fallback_row(movie, rk));
        }
        let client = TmdbClient::new();
        if !client.configured() { return None; }
        if is_show {
            let d = client.tv_details(synthetic_show_tmdb_id(rk)?).ok()?;
            let show = TmdbShow {
                id: d.id, name: d.name.clone(), first_air_date: d.first_air_date.clone(),
                backdrop_path: d.backdrop_path.clone(), poster_path: None,
            };
            return Some(self.tmdb_details_show_row_and_remember(&d, show, rk));
        }
        let tmdb_id = synthetic_tmdb_id(rk)?;
        let d = client.movie_details(tmdb_id).ok()?;
        let movie = TmdbMovie {
            id: d.id, title: d.title.clone(), release_date: d.release_date.clone(),
            runtime: d.runtime, backdrop_path: d.backdrop_path.clone(), poster_path: None,
            belongs_to_collection: d.belongs_to_collection.clone(),
        };
        Some(self.tmdb_details_movie_row_and_remember(&d, movie, rk))
    }

    fn tmdb_details_show_row_and_remember(&self, details: &TmdbShowDetails, show: TmdbShow, rk: i64) -> Value {
        if let Some(map) = self.tmdb_fallbacks.lock().ok() {
            let mut map = map;
            if map.len() >= 512 { map.clear(); }
            map.insert(rk, TmdbFallback::Show(show));
        }
        self.tmdb_show_details_row(details, rk)
    }

    fn tmdb_details_movie_row_and_remember(&self, details: &TmdbMovieDetails, movie: TmdbMovie, rk: i64) -> Value {
        if let Some(map) = self.tmdb_fallbacks.lock().ok() {
            let mut map = map;
            if map.len() >= 512 { map.clear(); }
            map.insert(rk, TmdbFallback::Movie(movie));
        }
        self.tmdb_details_row(details, rk)
    }

    // ---- person pages -----------------------------------------------------------------------

    fn person_profile_page(&self, guid: &str) -> Response {
        let empty = || Response::json(&container(json!({"Metadata": []})));
        let Some(person_id) = person_id_of_guid(guid) else { return empty(); };
        let client = TmdbClient::new();
        if !client.configured() { return empty(); }
        let Ok(details) = client.person_details(person_id) else { return empty(); };
        let career = client.person_career(person_id).unwrap_or_default();
        Response::json(&container(json!({
            "Metadata": [self.person_profile_row(&details, &career.cast, &career.crew)]
        })))
    }

    fn person_career_page(&self, guid: &str) -> Response {
        let empty = || Response::json(&container(json!({"CreditGroup": []})));
        let Some(person_id) = person_id_of_guid(guid) else { return empty(); };
        let client = TmdbClient::new();
        if !client.configured() { return empty(); }
        match client.person_career(person_id) {
            Ok(credits) => Response::json(&container(json!({"CreditGroup": self.person_credit_groups(&credits.cast, &credits.crew)}))),
            Err(_) => empty(),
        }
    }

    /// `/library/people/{personId}/media` — the person's two shelves. Rows come from TMDB alone: one
    /// request per credit through addon meta would be N HTTP calls for a prolific actor.
    fn person_media_page(&self, raw: &str) -> Response {
        let person_id = person_id_of_guid(raw)
            .or_else(|| raw.parse::<i64>().ok().and_then(synthetic_person_id));
        let empty = || Response::json(&container(json!({"Metadata": []})));
        let Some(person_id) = person_id else { return empty(); };
        let client = TmdbClient::new();
        if !client.configured() { return empty(); }
        let Ok(credits) = client.person_career(person_id) else { return empty(); };
        let all: Vec<TmdbCredit> = credits.cast.into_iter().chain(credits.crew).collect();
        Response::json(&container(json!({"Metadata": self.person_credit_rows(person_id, &all)})))
    }

    fn person_profile_row(&self, details: &TmdbPerson, cast: &[TmdbCredit], crew: &[TmdbCredit]) -> Value {
        let mut departments: Vec<(usize, String, String)> = Vec::new();
        let acted = cast.iter().filter(|c| person_credit_item(c, &c.character).is_some()).count();
        if acted > 0 { departments.push((acted, "actor".into(), "Actor".into())); }
        let mut by_kind: HashMap<String, (usize, String, String)> = HashMap::new();
        for credit in crew {
            let Some((kind, title)) = person_department_group(&credit.department) else { continue };
            if kind == "other" || person_credit_item(credit, &credit.job).is_none() { continue; }
            let entry = by_kind.entry(kind.clone()).or_insert((0, kind, title));
            entry.0 += 1;
        }
        departments.extend(by_kind.into_values());
        departments.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        let credit_types: Vec<Value> = departments.into_iter()
            .map(|(_, kind, title)| json!({"type": kind, "title": title})).collect();
        json!({
            "title": details.name, "summary": details.biography,
            "bornAt": details.birthday.clone().unwrap_or_default(),
            "diedAt": details.deathday.clone().unwrap_or_default(),
            "birthPlace": details.place_of_birth.clone().unwrap_or_default(),
            "knownFor": details.known_for_department.clone().unwrap_or_default(),
            "thumb": tmdb_image(details.profile_path.as_deref()).unwrap_or_default(),
            "CreditType": credit_types,
        })
    }

    fn person_credit_groups(&self, cast: &[TmdbCredit], crew: &[TmdbCredit]) -> Vec<Value> {
        let mut groups = Vec::new();
        let acted: Vec<Value> = cast.iter().filter_map(|c| person_credit_item(c, &c.character)).collect();
        if !acted.is_empty() { groups.push(person_credit_group("actor", "Actor", acted)); }
        let mut by_department: Vec<(String, String, Vec<Value>)> = Vec::new();
        for credit in crew {
            let Some((kind, title)) = person_department_group(&credit.department) else { continue };
            let Some(row) = person_credit_item(credit, &credit.job) else { continue };
            match by_department.iter_mut().find(|(known, _, _)| *known == kind) {
                Some((_, _, rows)) => rows.push(row),
                None => by_department.push((kind, title, vec![row])),
            }
        }
        by_department.sort_by(|a, b| b.2.len().cmp(&a.2.len()).then_with(|| a.1.cmp(&b.1)));
        for (kind, title, rows) in by_department { groups.push(person_credit_group(&kind, &title, rows)); }
        groups
    }

    fn person_credit_rows(&self, person_id: u64, credits: &[TmdbCredit]) -> Vec<Value> {
        let key = TMDB_PERSON_KEY_BASE + person_id as i64;
        let mut seen = HashSet::new();
        credits.iter()
            .filter(|credit| credit.id > 0 && credit.id < (i64::MAX - TMDB_PERSON_KEY_BASE) as u64)
            .filter(|credit| seen.insert(credit.id))
            .filter_map(|credit| {
                let base = if credit.media_type == "tv" { TMDB_SHOW_KEY_BASE } else { TMDB_FALLBACK_KEY_BASE };
                let rk = base + credit.id as i64;
                let mut row = if credit.media_type == "tv" {
                    self.tmdb_show_fallback_row(&TmdbShow {
                        id: credit.id, name: credit.name.clone(), poster_path: credit.profile_path.clone(),
                        ..Default::default()
                    }, rk)
                } else {
                    self.tmdb_fallback_row(&TmdbMovie {
                        id: credit.id, title: credit.name.clone(), poster_path: credit.profile_path.clone(),
                        ..Default::default()
                    }, rk)
                };
                if !credit.character.trim().is_empty() {
                    row["Role"] = json!([{"id": key, "tag": "", "role": credit.character}]);
                }
                Some(row)
            })
            .collect()
    }

    // ---- routing ---------------------------------------------------------------------------

    pub fn handle(self: &Arc<Self>, req: &Request) -> Response {
        // Paths only: a query can carry the token, and part redirects carry credentialed URLs.
        static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *TRACE.get_or_init(|| std::env::var_os("PMSSHIM_TRACE").is_some()) {
            let t = Instant::now();
            let r = self.route(req);
            eprintln!("pmsshim: {} {} -> {} ({} ms)", req.method, req.path, r.status, t.elapsed().as_millis());
            return r;
        }
        self.route(req)
    }

    fn route(self: &Arc<Self>, req: &Request) -> Response {
        let segs: Vec<&str> = req.path.split('/').filter(|s| !s.is_empty()).collect();
        let p = req.path.as_str();
        match (req.method.as_str(), segs.as_slice()) {
            (_, []) | (_, ["identity"]) => Response::json(&container(json!({
                "machineIdentifier": MACHINE_ID, "friendlyName": "Nuvio", "version": "1.41.0.0000-nuvio",
                "platform": "webOS", "myPlex": false, "myPlexSubscription": true,
            }))),
            // plex.tv: the app's plex.tv base points here; sign-in maps onto the Nuvio account.
            (_, ["api", "v2", rest @ ..]) => self.account.handle(req, rest),
            (_, ["api", ..]) => Response::json_status(404, &json!({"error": "no plex.tv here"})),

            (_, ["library", "sections"]) => Response::json(&container(json!({"Directory": [
                {"key": "1", "type": "movie", "title": "Movies", "agent": "tv.plex.agents.movie", "scanner": "Plex Movie", "language": "en-US", "refreshing": false, "allowSync": false},
                {"key": "2", "type": "show", "title": "TV Shows", "agent": "tv.plex.agents.series", "scanner": "Plex TV Series", "language": "en-US", "refreshing": false, "allowSync": false},
            ]}))),
            (_, ["library", "sections", k, rest @ ..]) => self.section(req, k.parse().unwrap_or(0), rest),
            (_, ["library", "metadata", rk]) => {
                // Fork Rofocale11: a single key is a Detail read — enrich it once from TMDB (or its
                // persisted cache) so cast, logo, landscape art, trailers and the shelf ride in the
                // same answer. A CSV listing stays cheap and un-enriched.
                let keys: Vec<i64> = rk.split(',').filter_map(|x| x.parse().ok()).collect();
                let single = keys.len() == 1;
                let mut rows = Vec::new();
                for k in keys {
                    if let Some(mut row) = self.item(k) {
                        if single { self.enrich_row(k, &mut row); }
                        rows.push(row);
                    }
                }
                if rows.is_empty() {
                    return Response::json_status(404, &container(json!({"Metadata": []})));
                }
                Response::json(&container(json!({"Metadata": rows})))
            }
            (_, ["library", "metadata", rk, "children"]) => Response::json(&container(json!({"Metadata": self.children(rk.parse().unwrap_or(0))}))),
            (_, ["library", "metadata", rk, "allLeaves"]) => Response::json(&container(json!({"Metadata": self.leaves(rk.parse().unwrap_or(0))}))),
            (_, ["library", "metadata", rk, "related"]) => self.related_hub(rk.parse().unwrap_or(-1)),
            (_, ["library", "metadata", rk, "extras"]) => self.extras_route(rk.parse().unwrap_or(-1)),
            (_, ["library", "metadata", _, ..]) => ok_empty(),
            ("PUT", ["library", "parts", ..]) => ok_empty(),
            (_, ["library", "parts", pid, ..]) => self.part(pid.parse().unwrap_or(-1)),
            (_, ["library", "streams", sid]) => self.subtitle(sid.split('.').next().unwrap_or("").parse().unwrap_or(-1)),
            // Fork Rofocale11: the person page's global reads and its shelves, plus the collection
            // page the franchise shelf's heading opens. An unknown guid answers an EMPTY container.
            (_, ["library", "people", guid]) => self.person_profile_page(guid),
            (_, ["library", "people", guid, "credits"]) => self.person_career_page(guid),
            (_, ["library", "people", person, "media"]) => self.person_media_page(person),
            (_, ["library", "collections", rk, "children"]) => {
                let (start, size) = page_params(req);
                let key = rk.parse::<i64>().ok().or_else(|| trailing_key(rk)).unwrap_or(-1);
                let rows = synthetic_collection_id(key)
                    .and_then(|id| self.tmdb_collection(id))
                    .map(|c| self.collection_children(&c, start, size.unwrap_or(50)))
                    .unwrap_or_else(|| container(json!({"Metadata": [], "totalSize": 0})));
                Response::json(&rows)
            }
            (_, ["library", "collections", ..]) => Response::json(&container(json!({"Metadata": []}))),

            (_, ["hubs"]) | (_, ["hubs", "promoted"]) => {
                let count = req.q("count").and_then(|v| v.parse().ok()).unwrap_or(12usize).clamp(1, 50);
                let mut hubs = vec![self.continue_hub(count, None)];
                if req.q("excludeContinueWatching") == Some("1") {
                    hubs.clear();
                }
                if let Some(list) = self.saved_hub(count) { hubs.push(list); }
                hubs.extend(self.catalog_hubs(|_| true, count, true, HUBS_BUDGET));
                Response::json(&container(json!({"Hub": hubs})))
            }
            (_, ["hubs", "continueWatching"]) => {
                let count = req.q("count").and_then(|v| v.parse().ok()).unwrap_or(12usize);
                Response::json(&container(json!({"Hub": [self.continue_hub(count, None)]})))
            }
            (_, ["hubs", "home", "recentlyAdded"]) => {
                let section = if req.q("type") == Some("2") || req.q("type") == Some("4") { SECTION_SHOWS } else { SECTION_MOVIES };
                let (start, size) = page_params(req);
                self.catalog_listing(self.first_catalog(section), None, start, size.unwrap_or(12))
            }
            (_, ["hubs", "sections", k]) => {
                let section: i64 = k.parse().unwrap_or(0);
                let count = req.q("count").and_then(|v| v.parse().ok()).unwrap_or(12usize).clamp(1, 50);
                let mut hubs = vec![self.continue_hub(count, Some(section))];
                hubs.extend(self.catalog_hubs(|c| (section == SECTION_SHOWS) == is_series(&c.type_), count, false, HUBS_BUDGET));
                Response::json(&container(json!({"Hub": hubs})))
            }
            (_, ["hubs", "nuvio", "list"]) => {
                let (start, size) = page_params(req);
                match self.account.saved_library(true) {
                    Ok(items) => {
                        let rows: Vec<_> = items.iter().skip(start).take(size.unwrap_or(50)).map(|p| self.preview_row(p)).collect();
                        Response::json(&container(json!({"Metadata":rows,"totalSize":items.len(),"offset":start})))
                    }
                    Err(_) => Response::json_status(503,&container(json!({"error":"My List is unavailable"}))),
                }
            }
            ("GET" | "POST", ["nuvio", "library"]) => self.saved_item(req),
            (_, ["hubs", "nuvio", i]) => {
                let (start, size) = page_params(req);
                let cat = i.parse::<usize>().ok().and_then(|i| self.catalog_list().into_iter().nth(i));
                self.catalog_listing(cat, None, start, size.unwrap_or(50))
            }
            (_, ["hubs", "search"]) => self.search(req.q("query").unwrap_or(""), req.q("limit").and_then(|v| v.parse().ok()).unwrap_or(10)),

            (_, ["photo", ":", "transcode"]) => self.photo(req),

            (_, [":", "timeline"]) | (_, [":", "progress"]) => {
                let rk = req.q("ratingKey").and_then(|v| v.parse().ok()).or_else(|| req.q("key").and_then(trailing_key));
                let time: Option<i64> = req.q("time").and_then(|v| v.parse().ok());
                if let (Some(rk), Some(t)) = (rk, time) {
                    let dur = req.q("duration").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let state = req.q("state").unwrap_or("playing").to_string();
                    if t > 0 && state != "stopped" || state == "stopped" && t > 0 {
                        let me = self.clone();
                        // The row snapshot may need a meta fetch: never on the reporting request.
                        std::thread::spawn(move || {
                            let row = me.row(rk);
                            me.watch.progress(rk, t, dur, row);
                            // Fork Rofocale11: mirror to the account after the local write.
                            me.push_watch_progress(rk, t, dur, &state);
                        });
                    }
                }
                ok_empty()
            }
            (_, [":", "scrobble"]) | (_, [":", "unscrobble"]) => {
                if let Some(rk) = req.q("key").and_then(trailing_key).or_else(|| req.q("ratingKey").and_then(|v| v.parse().ok())) {
                    self.watch.scrobble(rk, p == "/:/scrobble");
                    if let Some(Ident::Title { type_, id }) = self.keys.ident(rk).filter(|i| matches!(i, Ident::Title { type_, .. } if is_series(type_))) {
                        // Marking a show watched marks every episode.
                        if let Some(m) = self.meta(&type_, &id) {
                            for (_, _, v) in episodes_of(&m) {
                                self.watch.scrobble(self.episode_key(&id, &v.id), p == "/:/scrobble");
                            }
                        }
                    }
                    if let Some(Ident::Season { series, n }) = self.keys.ident(rk) {
                        if let Some((_, m)) = self.series_of(&series) {
                            for (_, _, v) in episodes_of(&m).into_iter().filter(|(s, _, _)| *s == n) {
                                self.watch.scrobble(self.episode_key(&series, &v.id), p == "/:/scrobble");
                            }
                        }
                    }
                }
                ok_empty()
            }
            (_, ["actions", "removeFromContinueWatching"]) => {
                if let Some(rk) = req.q("ratingKey").and_then(|v| v.parse().ok()) {
                    self.watch.clear_progress(rk);
                    // Fork Rofocale11: a cleared resume point must not come back on the next pull.
                    // Nuvio's RPC pair has no delete verb, so overwrite the account row with zero —
                    // `merge_remote` ignores zero on the way back.
                    self.push_watch_progress(rk, 0, 0, "stopped");
                }
                ok_empty()
            }
            (_, ["playQueues"]) => self.play_queue(req),
            (_, ["playQueues", ..]) => ok_empty(),
            (_, ["video", ":", "transcode", "universal", "decision"]) => self.decision(req),
            (_, ["video", ":", "transcode", "universal", "start", ..]) | (_, ["video", ":", "transcode", "universal", "start.m3u8"]) => {
                Response::bytes(404, "text/plain", b"no transcoder: streams play direct".to_vec())
            }
            (_, ["video", ..]) | (_, ["status", ..]) => ok_empty(),

            (_, ["nuvio", "streams"]) => {
                let rk = req.q("ratingKey").and_then(|v| v.parse().ok()).unwrap_or(-1);
                // Fork Rofocale11: a synthetic related key is never in the key table, but its picker
                // must still work. With no slot to ask (e.g. a related SHOW with no episode chosen)
                // answer `complete: true` and an empty list — `false` re-polls forever.
                let Some(ident) = self.keys.ident(rk).or_else(|| synthetic_ident(rk)) else {
                    return Response::json(&json!({"ratingKey": rk, "complete": true, "streams": []}));
                };
                // Fork Rofocale11: serve the very list the detail/metadata path ranked for `Media[]`
                // instead of a second 800 ms re-rank that could answer empty while Play Version
                // showed the same stream. Fall back to a live re-rank only before that list exists.
                let slot = self.streams.lock().unwrap_or_else(|e| e.into_inner()).get(&rk).cloned();
                let cached = slot.as_ref().and_then(|s| {
                    let g = s.state.lock().unwrap_or_else(|e| e.into_inner());
                    if !g.done || g.ranked.is_empty() { return None; }
                    let mut ranked = g.ranked.clone();
                    // `/nuvio/select` can land after the list was cached; the chosen stream still
                    // leads, exactly as `ranked()` orders it for `Media[]`.
                    if let Some(sel) = g.selected {
                        if let Some(p) = ranked.iter().position(|(i, _)| *i == sel) {
                            let chosen = ranked.remove(p);
                            ranked.insert(0, chosen);
                        }
                    }
                    Some(ranked)
                });
                let (ranked, done) = if let Some(r) = cached {
                    (r, true)
                } else {
                    // Fork Rofocale11: the streams MENU still uses DETAIL_WAIT on purpose — unlike
                    // the detail read, the picker is asking for streams and may show a short
                    // progress window. Only the detail read's wait was made ZERO above.
                    let (r, _, d) = self.ranked(rk, &ident, None, DETAIL_WAIT);
                    if !r.is_empty() {
                        if let Some(s) = slot {
                            s.state.lock().unwrap_or_else(|e| e.into_inner()).ranked = r.clone();
                        }
                    }
                    (r, d)
                };
                // The Streams menu must show what the file IS, not the addon's label guess — the
                // same fields `item_probed` applies to `Media[]` (docs/nuvio/PLAYBACK.md).
                let streams: Vec<Value> = ranked.iter().map(|(i, c)| {
                    let mut summary = shim_media::summary_json(*i, c);
                    let url = self.resolved.lock().unwrap_or_else(|e| e.into_inner()).get(&(rk, c.url.clone())).cloned().unwrap_or_else(|| c.url.clone());
                    if let Some(Ok(p)) = self.probes.cached(&url) {
                        crate::shim_probe::apply_summary(&mut summary, &p);
                    }
                    summary
                }).collect();
                Response::json(&json!({"ratingKey": rk, "complete": done, "streams": streams}))
            }
            (_, ["nuvio", "select"]) => {
                let rk = req.q("ratingKey").and_then(|v| v.parse().ok()).unwrap_or(-1);
                let idx: Option<usize> = req.q("index").and_then(|v| v.parse().ok());
                match (self.streams.lock().unwrap_or_else(|e| e.into_inner()).get(&rk), idx) {
                    (Some(slot), Some(i)) => {
                        let mut state = slot.state.lock().unwrap_or_else(|e| e.into_inner());
                        if i >= state.streams.len() { return Response::json_status(404, &json!({"ok": false})); }
                        state.selected = Some(i);
                        Response::json(&json!({"ok": true}))
                    }
                    _ => Response::json_status(404, &json!({"ok": false})),
                }
            }
            _ => {
                eprintln!("pmsshim: unhandled {} {}", req.method, req.path);
                ok_empty()
            }
        }
    }

    fn catalog_listing(&self, cat: Option<CatalogInfo>, genre: Option<&str>, start: usize, size: usize) -> Response {
        let Some(cat) = cat else { return Response::json(&container(json!({"Metadata": [], "totalSize": 0, "offset": start}))) };
        let (rows, total) = self.catalog_range(&cat.key, genre, start, size.min(200));
        let rows: Vec<Value> = rows.iter().map(|p| self.preview_row(p)).collect();
        Response::json(&container(json!({"Metadata": rows, "totalSize": total, "offset": start})))
    }

    fn section(&self, req: &Request, k: i64, rest: &[&str]) -> Response {
        if k != SECTION_MOVIES && k != SECTION_SHOWS {
            return Response::json_status(404, &container(json!({})));
        }
        let cat = self.first_catalog(k);
        match rest {
            ["all"] | ["recentlyAdded"] | ["newest"] => {
                let (start, size) = page_params(req);
                let genre = req.q("genre").filter(|g| !g.is_empty());
                let mut resp = self.catalog_listing(cat, genre, start, size.unwrap_or(60));
                if req.q("includeMeta") == Some("1") {
                    if let Ok(mut v) = serde_json::from_slice::<Value>(&resp.body) {
                        let kind = if k == SECTION_SHOWS { "show" } else { "movie" };
                        v["MediaContainer"]["Meta"] = json!({"Type": [{"key": format!("/library/sections/{k}/all?type={}", if k == SECTION_SHOWS { 2 } else { 1 }),
                            "type": kind, "title": kind, "active": true,
                            "Sort": [{"key": "titleSort", "defaultDirection": "asc", "title": "Popular"}]}]});
                        resp = Response::json(&v);
                    }
                }
                resp
            }
            ["genre"] => {
                let genres = cat.map(|c| c.genres).unwrap_or_default();
                let dirs: Vec<Value> = genres
                    .iter()
                    .map(|g| json!({"key": g, "title": g, "fastKey": format!("/library/sections/{k}/all?genre={}", pct_encode(g))}))
                    .collect();
                Response::json(&container(json!({"Directory": dirs})))
            }
            // Fork Rofocale11: the collection page resolves a shelf heading's tagId here. The shim
            // has no library scan, so only franchises already served are listed.
            ["collections"] => self.section_collections(req, k),
            ["firstCharacter"] => Response::json(&container(json!({"Directory": [], "Metadata": []}))),
            _ => Response::json(&container(json!({"Directory": []}))),
        }
    }

    fn children(&self, rk: i64) -> Vec<Value> {
        match self.keys.ident(rk) {
            Some(Ident::Title { type_, id }) if is_series(&type_) => {
                let Some(m) = self.meta(&type_, &id) else { return Vec::new() };
                seasons_of(&m).into_iter().map(|n| self.season_row(rk, &m, n)).collect()
            }
            Some(Ident::Season { series, n }) => {
                let Some((show, m)) = self.series_of(&series) else { return Vec::new() };
                episodes_of(&m).into_iter().filter(|(s, _, _)| *s == n).map(|(s, e, v)| self.episode_row(show, &m, s, e, v)).collect()
            }
            _ => Vec::new(),
        }
    }

    fn leaves(&self, rk: i64) -> Vec<Value> {
        match self.keys.ident(rk) {
            Some(Ident::Title { type_, id }) if is_series(&type_) => {
                let Some(m) = self.meta(&type_, &id) else { return Vec::new() };
                episodes_of(&m).into_iter().map(|(s, e, v)| self.episode_row(rk, &m, s, e, v)).collect()
            }
            Some(Ident::Season { .. }) => self.children(rk),
            _ => Vec::new(),
        }
    }

    fn search(self: &Arc<Self>, query: &str, limit: usize) -> Response {
        let q = query.trim();
        let mut movies: Vec<Value> = Vec::new();
        let mut shows: Vec<Value> = Vec::new();
        if !q.is_empty() {
            let mut seen = std::collections::HashSet::new();
            for (_, rows) in self.backend.search(q) {
                for p in rows {
                    if !seen.insert((p.type_.clone(), p.id.clone())) {
                        continue;
                    }
                    let row = self.preview_row(&p);
                    if is_series(&p.type_) { shows.push(row) } else { movies.push(row) }
                }
            }
        }
        let limit = limit.max(1);
        movies.truncate(limit);
        shows.truncate(limit);
        let hub = |kind: &str, rows: Vec<Value>| json!({"title": kind, "type": kind, "hubIdentifier": kind, "size": rows.len(), "Metadata": rows});
        Response::json(&container(json!({"Hub": [hub("movie", movies), hub("show", shows),
            {"title": "episode", "type": "episode", "hubIdentifier": "episode", "size": 0, "Metadata": []}]})))
    }

    fn play_queue(&self, req: &Request) -> Response {
        let rk = req.q("uri").and_then(trailing_key).or_else(|| req.q("key").and_then(trailing_key)).unwrap_or(-1);
        // The play is about to start: wait for the streams and probe the lead, so the queue's row
        // carries the real fields rather than the detail page's early guesses.
        let Some((first, _)) = self.item_probed(rk, Some(0)) else { return Response::json(&container(json!({"Metadata": [], "playQueueID": 1}))) };
        let mut rows = vec![first];
        if req.q("continuous") == Some("1") {
            if let Some(Ident::Episode { series, video }) = self.keys.ident(rk) {
                if let Some((show, m)) = self.series_of(&series) {
                    let eps = episodes_of(&m);
                    if let Some(at) = eps.iter().position(|(_, _, v)| v.id == video) {
                        rows.extend(eps[at + 1..].iter().filter(|(s, _, _)| *s != 0).take(20).map(|(s, e, v)| self.episode_row(show, &m, *s, *e, v)));
                    }
                }
            }
        }
        let rows: Vec<Value> = rows
            .into_iter()
            .enumerate()
            .map(|(n, mut r)| {
                r["playQueueItemID"] = json!(n + 1);
                r
            })
            .collect();
        let total = rows.len();
        Response::json(&container(json!({"Metadata": rows, "playQueueID": 1, "playQueueSelectedItemID": 1,
            "playQueueSelectedItemOffset": 0, "playQueueTotalCount": total, "playQueueVersion": 1})))
    }

    fn part(&self, pid: i64) -> Response {
        if pid < 0 {
            return Response::not_found();
        }
        let (rk, idx) = (pid / PART_STRIDE, (pid % PART_STRIDE) as usize);
        let Some(ident) = self.keys.ident(rk).or_else(|| synthetic_ident(rk)) else { return Response::not_found() };
        // Make sure the slot exists (a resume after a restart lands here first) and has waited.
        let (ranked, _, _) = self.ranked(rk, &ident, None, self.cfg.stream_deadline);
        let stream = if idx == BEST_PART {
            ranked.first().map(|(_, c)| c.stream.clone())
        } else {
            let slot = self.streams.lock().unwrap_or_else(|e| e.into_inner()).get(&rk).cloned();
            slot.and_then(|s| s.state.lock().unwrap_or_else(|e| e.into_inner()).streams.get(idx).cloned())
        };
        let url = stream.as_ref().and_then(|s| {
            let u = shim_media::playable_url(s, self.torrent_server().as_deref())?;
            self.open_url(rk, s, &u)
        });
        match url {
            Some(url) => Response::redirect(&url),
            None => Response::bytes(502, "text/plain", b"stream no longer available".to_vec()),
        }
    }

    fn subtitle(&self, sid: i64) -> Response {
        let Some(url) = self.sub_urls.lock().unwrap_or_else(|e| e.into_inner()).get(&sid).cloned() else { return Response::not_found() };
        match plx_net::net::request(&url, &[], "GET", None, plx_net::net::API, true, Some(4 << 20), None) {
            Some(r) if r.ok() => Response::bytes(200, "text/plain; charset=utf-8", crate::shim_subs::to_utf8_text(r.body)),
            _ => Response::bytes(502, "text/plain", b"subtitle fetch failed".to_vec()),
        }
    }

    fn photo(&self, req: &Request) -> Response {
        let Some(source) = req.q("url") else { return Response::not_found(); };
        // PMS artwork callers may send an absolute CDN URL or the item's local art path.
        let local = source.split('?').next().unwrap_or(source);
        let pieces: Vec<_> = local.trim_matches('/').split('/').collect();
        let resolved = match pieces.as_slice() {
            // Fork Rofocale11: `row()` resolves a synthetic key through the TMDB row builder, so a
            // Related title's logo/poster/backdrop is proxied even though it has no key-table row.
            ["library", "metadata", rk, kind @ ("thumb" | "art" | "clearLogo")] =>
                rk.parse().ok().and_then(|rk| self.art_row(rk)).and_then(|row|
                    row[*kind].as_str().filter(|s| !s.is_empty()).map(str::to_string)),
            // Fork Rofocale11: PMS stamps clearLogo paths with a version digit; the app passes that
            // segment through `/photo/:/transcode`, so accept it here instead of serving a 404.
            ["library", "metadata", rk, kind @ ("thumb" | "art" | "clearLogo"), _stamp] =>
                rk.parse().ok().and_then(|rk| self.art_row(rk)).and_then(|row|
                    row[*kind].as_str().filter(|s| !s.is_empty()).map(str::to_string)),
            _ => None,
        };
        let url = resolved.as_deref().unwrap_or(source);
        if !(url.starts_with("http://") || url.starts_with("https://")) { return Response::not_found(); }
        let width: u32 = req.q("width").and_then(|v| v.parse().ok()).unwrap_or(0);
        let height: u32 = req.q("height").and_then(|v| v.parse().ok()).unwrap_or(0);
        let sized = sized_image_url(url, width.max(height.saturating_mul(2) / 3));
        let fallback = self.poster_fallbacks.lock().unwrap_or_else(|e| e.into_inner()).get(url).cloned();
        let mut urls = vec![sized];
        if let Some(original) = fallback { urls.push(sized_image_url(&original, width.max(height.saturating_mul(2) / 3))); }
        let mut status = 502;
        for url in urls {
            if let Some(r) = plx_net::net::request(&url, &[], "GET", None, plx_net::net::API, true, Some(12 << 20), None) {
                if r.ok() && !r.body.is_empty() { return Response::bytes(200, sniff_image(&r.body), r.body); }
                status = r.status.max(400);
            }
        }
        Response::bytes(status, "text/plain", Vec::new())
    }
}

fn sniff_image(b: &[u8]) -> &'static str {
    if b.starts_with(&[0x89, b'P', b'N', b'G']) {
        "image/png"
    } else if b.len() > 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    }
}

/// Ask the image CDNs addons use for a size near what the app draws, instead of the original
/// (a 2000x3000 TMDB poster costs the TV a lot to decode for a 250px card).
pub fn sized_image_url(url: &str, width: u32) -> String {
    if let Some(i) = url.find("image.tmdb.org/t/p/") {
        let head = &url[..i + "image.tmdb.org/t/p/".len()];
        let rest = &url[head.len()..];
        if let Some((_, tail)) = rest.split_once('/') {
            let size = match width {
                0 => "w780",
                w if w <= 185 => "w185",
                w if w <= 342 => "w342",
                w if w <= 500 => "w500",
                w if w <= 780 => "w780",
                w if w <= 1280 => "w1280",
                _ => "original",
            };
            return format!("{head}{size}/{tail}");
        }
    }
    if url.contains("images.metahub.space/") {
        for kind in ["poster", "background", "logo"] {
            for from in ["small", "medium", "large"] {
                let pat = format!("/{kind}/{from}/");
                if url.contains(&pat) {
                    let to = match (kind, width) {
                        ("poster", w) if w > 0 && w <= 400 => "medium",
                        ("poster", _) => "large",
                        ("background", w) if w > 0 && w <= 1280 => "medium",
                        ("background", _) => "large",
                        _ => "medium",
                    };
                    return url.replacen(&pat, &format!("/{kind}/{to}/"), 1);
                }
            }
        }
    }
    url.to_string()
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<Shim>();
    let _ = now_secs;
}

#[cfg(test)]
mod playback_regressions {
    use super::*;

    #[test]
    fn an_inflight_stream_list_cannot_direct_play_a_placeholder() {
        let shim = Arc::new(Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default()));
        let rk = shim.keys.key_of(&Ident::Title { type_: "movie".into(), id: "tt1254207".into() });
        shim.streams.lock().unwrap().insert(rk, Arc::new(StreamSlot {
            started: Instant::now() - Duration::from_secs(90),
            state: Mutex::new(SlotState::default()), cv: Condvar::new(),
        }));
        shim.probes.set_prober(Arc::new(|_, _| panic!("no stream exists to probe")));
        let request = Request { method: "GET".into(), path: "/video/:/transcode/universal/decision".into(),
            query: HashMap::from([("path".into(), format!("/library/metadata/{rk}"))]),
            headers: HashMap::new(), body: Vec::new() };
        let response = shim.handle(&request);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["MediaContainer"]["generalDecisionCode"], 2000);
    }
    #[test]
    fn decision_carries_probed_hevc_dovi_tracks_and_refuses_missing_selection() {
        let shim=Arc::new(Shim::new(Arc::new(crate::shim_fake::FakeBackend),Config::default()));
        let rk=shim.keys.key_of(&Ident::Title {type_:"movie".into(),id:"tt1254207".into()});
        shim.streams.lock().unwrap().insert(rk,Arc::new(StreamSlot {
            started:Instant::now(),state:Mutex::new(SlotState {
                streams:vec![Stream {url:Some("https://example.invalid/media.mkv".into()),..Default::default()}],
                done:true,..Default::default()}),cv:Condvar::new()}));
        shim.probes.set_prober(Arc::new(|_,_| Ok(crate::shim_probe::ProbeInfo {
            container:"mkv".into(),duration_ms:90000,size:1000,video_decodable:true,
            video:Some(crate::shim_probe::Video {codec:"hevc".into(),width:3840,height:2160,fps:23.976,
                dovi:Some(crate::shim_probe::Dovi {profile:8,level:6,bl_compat:1,rpu:true,bl:true,..Default::default()}),
                ..Default::default()}),
            audio:vec![crate::shim_probe::Audio {codec:"eac3".into(),channels:6,decodable:true,..Default::default()}],
            subtitles:vec![crate::shim_probe::Sub {codec:"subrip".into(),language:"eng".into(),..Default::default()}],
        })));
        let mut request=Request {method:"GET".into(),path:"/video/:/transcode/universal/decision".into(),
            query:HashMap::from([("path".into(),format!("/library/metadata/{rk}"))]),headers:HashMap::new(),body:vec![]};
        let response=shim.handle(&request);
        let body:Value=serde_json::from_slice(&response.body).unwrap();
        let media=&body["MediaContainer"]["Metadata"][0]["Media"][0];
        assert_eq!(media["videoCodec"],"hevc");
        assert_eq!(media["audioCodec"],"eac3");
        assert_eq!(media["Part"][0]["Stream"][0]["DOVIProfile"],8);
        assert_eq!(media["Part"][0]["Stream"].as_array().unwrap().len(),3);
        request.query.insert("mediaIndex".into(),"99".into());
        let response=shim.handle(&request);
        let body:Value=serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["MediaContainer"]["generalDecisionCode"],2000);
    }

    #[test]
    fn photo_endpoint_passes_through_source_image_bytes() {
        use std::io::{Read,Write};
        plx_net::net::global_init();
        let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr=listener.local_addr().unwrap();
        let image=b"\x89PNG\r\n\x1a\nfixture";
        let worker=std::thread::spawn(move || {
            let (mut stream,_)=listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buffer=[0;4096]; let _=stream.read(&mut buffer).unwrap();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",image.len()).unwrap();
            stream.write_all(image).unwrap();
        });
        let shim=Shim::new(Arc::new(crate::shim_fake::FakeBackend),Config::default());
        let request=Request {method:"GET".into(),path:"/photo/:/transcode".into(),
            query:HashMap::from([("url".into(),format!("http://{addr}/poster.png")),
                ("height".into(),u32::MAX.to_string())]),headers:HashMap::new(),body:vec![]};
        let response=shim.photo(&request);
        assert_eq!(response.status,200);
        assert_eq!(response.content_type,"image/png");
        assert_eq!(response.body,image);
        worker.join().unwrap();
    }

    #[test]
    fn a_failed_poster_provider_falls_back_to_the_original_artwork() {
        use std::io::{Read,Write};
        plx_net::net::global_init();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let image = b"\x89PNG\r\n\x1a\noriginal";
        let worker = std::thread::spawn(move || {
            for status in [503,200] {
                let (mut stream,_) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut buffer = [0;4096]; let _ = stream.read(&mut buffer).unwrap();
                let body: &[u8] = if status == 200 { image } else { b"" };
                write!(stream,"HTTP/1.1 {status} Result\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                stream.write_all(body).unwrap();
            }
        });
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend),Config::default());
        let provider = format!("http://{addr}/provider");
        shim.poster_fallbacks.lock().unwrap().insert(provider.clone(),format!("http://{addr}/original"));
        let response = shim.photo(&Request { method:"GET".into(),path:"/photo/:/transcode".into(),query:HashMap::from([("url".into(),provider)]),headers:HashMap::new(),body:vec![] });
        assert_eq!(response.status,200);
        assert_eq!(response.body,image);
        worker.join().unwrap();
    }

}

/// Preserve the source's percent scale, and reject missing/invalid scores.
fn tomato_score(meta: &MetaPreview) -> Option<f64> {
    fn percent(v: &Value) -> Option<f64> {
        let score = v.as_f64().or_else(|| v.as_str()?.trim().trim_end_matches('%').parse().ok())?;
        (score.is_finite() && (0.0..=100.0).contains(&score)).then_some(score)
    }
    meta.rotten_tomatoes_rating.as_deref().and_then(|v| percent(&json!(v)))
        .or_else(|| percent(&meta.mdb_list_ratings["tomatoes"]))
        .or_else(|| meta.ratings.as_array()?.iter().find_map(|r| {
            let source = r["source"].as_str()?.to_ascii_lowercase();
            matches!(source.as_str(), "tomatoes" | "rottentomatoes" | "rotten tomatoes")
                .then(|| percent(&r["value"])).flatten()
        }))
}

#[cfg(test)]
mod rating_tests {
    use super::*;
    #[test]
    fn metadata_provider_tomato_scores_keep_percent_units() {
        let m: MetaPreview = serde_json::from_value(json!({"ratings":[{"source":"tomatoes","value":94}]})).unwrap();
        assert_eq!(tomato_score(&m), Some(94.0));
        let m: MetaPreview = serde_json::from_value(json!({"mdbListRatings":{"tomatoes":"63%"}})).unwrap();
        assert_eq!(tomato_score(&m), Some(63.0));
        assert_eq!(tomato_score(&MetaPreview::default()), None);
        let m: MetaPreview = serde_json::from_value(json!({"rottenTomatoesRating":101})).unwrap();
        assert_eq!(tomato_score(&m), None);
    }
}

#[cfg(test)]
mod streams_menu_regression {
    use super::*;

    fn probe_ok() -> crate::shim_probe::ProbeInfo {
        crate::shim_probe::ProbeInfo {
            container: "mkv".into(), duration_ms: 90_000, size: 9_000_000, video_decodable: true,
            video: Some(crate::shim_probe::Video {
                codec: "hevc".into(), width: 3840, height: 2160, fps: 23.976,
                color_trc: 16, ..Default::default()
            }),
            audio: vec![crate::shim_probe::Audio { codec: "eac3".into(), channels: 6, ..Default::default() }],
            ..Default::default()
        }
    }

    /// The Streams menu used to answer "No playable streams found" while Play Version showed the
    /// very same stream from the detail metadata's `Media[]` (owner report on 45UR7500PSC).
    #[test]
    fn streams_menu_shows_the_same_probed_stream_as_play_version() {
        let shim = Arc::new(Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default()));
        shim.probes.set_prober(Arc::new(|_, _| Ok(probe_ok())));
        let rk = shim.keys.key_of(&Ident::Title { type_: "movie".into(), id: "tt1254207".into() });
        let req = |path: String, query: HashMap<String, String>| Request {
            method: "GET".into(), path, query, headers: HashMap::new(), body: Vec::new(),
        };

        // Detail page: `Media[]` (what Play Version reads) and the ranked slot are populated.
        let meta: Value = serde_json::from_slice(&shim.handle(&req(format!("/library/metadata/{rk}"), HashMap::new())).body).unwrap();
        let media = meta["MediaContainer"]["Metadata"][0]["Media"].as_array().unwrap().clone();

        // The decision path caches the probe under the resolved URL.
        let _ = shim.handle(&req("/video/:/transcode/universal/decision".into(), HashMap::from([
            ("path".into(), format!("/library/metadata/{rk}")), ("mediaIndex".into(), "0".into()),
        ])));

        let resp = shim.handle(&req("/nuvio/streams".into(), HashMap::from([("ratingKey".into(), rk.to_string())])));
        let list: Value = serde_json::from_slice(&resp.body).unwrap();
        let streams = list["streams"].as_array().unwrap();
        assert!(!streams.is_empty(), "Streams menu must not be empty when Play Version has a version: {list}");
        assert_eq!(streams.len(), media.len(), "both menus must show the same single source of truth");
        assert_eq!(streams[0]["videoCodec"], "hevc", "the probed codec, not the addon label guess");
        assert_eq!(streams[0]["height"], 2160);
        assert_eq!(streams[0]["hdr"], "HDR10");
        assert_eq!(streams[0]["audioCodec"], "eac3");
    }
}

// Fork Rofocale11: TMDB enrichment / synthetic identity regressions. All are host-only and need no
// API key, so the crate's focused gate exercises the shapes without a network.

#[cfg(test)]
mod nuvio_tmdb_tests {
    use super::*;
    use plx_stremio::addons_tmdb::TmdbLogo;

    #[test]
    fn synthetic_keys_decode_only_into_their_own_range() {
        // Related movies (1B..2B).
        assert_eq!(synthetic_tmdb_id(1_000_000_603), Some(603));
        assert_eq!(synthetic_tmdb_id(TMDB_FALLBACK_KEY_BASE), Some(0));
        assert_eq!(synthetic_tmdb_id(TMDB_FALLBACK_KEY_BASE - 1), None);
        assert_eq!(synthetic_tmdb_id(TMDB_SHOW_KEY_BASE), None);
        assert_eq!(synthetic_tmdb_id(3_000_000_638), None);
        // Related shows (2B..3B): a person key is NOT a show.
        assert_eq!(synthetic_show_tmdb_id(2_000_001_396), Some(1396));
        assert_eq!(synthetic_show_tmdb_id(TMDB_SHOW_KEY_BASE - 1), None);
        assert_eq!(synthetic_show_tmdb_id(TMDB_PERSON_KEY_BASE), None);
        assert_eq!(synthetic_show_tmdb_id(3_000_000_638), None);
        assert_eq!(synthetic_show_tmdb_id(1_000_000_603), None);
        // People (3B..4B): a collection is NOT a person.
        assert_eq!(synthetic_person_id(3_000_000_638), Some(638));
        assert_eq!(synthetic_person_id(TMDB_PERSON_KEY_BASE - 1), None);
        assert_eq!(synthetic_person_id(TMDB_COLLECTION_KEY_BASE), None);
        assert_eq!(synthetic_tmdb_id(3_000_000_638), None);
        // Collections (4B..5B): a trailer is NOT a collection.
        assert_eq!(synthetic_collection_id(4_000_000_234), Some(234));
        assert_eq!(synthetic_collection_id(TMDB_COLLECTION_KEY_BASE - 1), None);
        assert_eq!(synthetic_collection_id(TRAILER_KEY_BASE), None);
        assert_eq!(synthetic_person_id(4_000_000_234), None);
        // Trailers (5B..10B), bounded above so a large key cannot alias.
        assert_eq!(synthetic_trailer_parent(5_000_010_137), Some(10_137));
        assert_eq!(synthetic_trailer_parent(TRAILER_KEY_END - 1), Some(4_999_999_999));
        assert_eq!(synthetic_trailer_parent(TRAILER_KEY_END), None);
        assert_eq!(synthetic_collection_id(5_000_010_137), None);
    }

    #[test]
    fn translate_to_stremio_id_prefers_the_highest_base_and_leaves_real_keys_alone() {
        assert_eq!(translate_to_stremio_id(2_000_001_396).as_deref(), Some("tmdb:1396"));
        assert_eq!(translate_to_stremio_id(1_000_000_603).as_deref(), Some("tmdb:603"));
        assert_eq!(translate_to_stremio_id(3_000_000_638), None);
        assert_eq!(translate_to_stremio_id(10_100), None);
        assert_eq!(
            synthetic_ident(2_000_001_396),
            Some(Ident::Title { type_: "series".into(), id: "tmdb:1396".into() })
        );
        assert_eq!(
            synthetic_ident(1_000_000_603),
            Some(Ident::Title { type_: "movie".into(), id: "tmdb:603".into() })
        );
    }

    #[test]
    fn person_guid_round_trips_and_never_names_person_zero() {
        assert_eq!(person_guid(638), "nuvio-person-638");
        assert_eq!(person_id_of_guid("nuvio-person-638"), Some(638));
        assert_eq!(person_id_of_guid("nuvio-person-"), None);
        assert_eq!(person_id_of_guid("nuvio-person-0"), None);
        assert_eq!(person_id_of_guid("plex://person/abc"), None);
    }

    #[test]
    fn poster_url_resolves_imdb_ids_and_keeps_addon_art_otherwise() {
        let shim = Shim::new(
            Arc::new(crate::shim_backend::AddonBackend::new(plx_stremio::addons::AddonSet::defaults())),
            Config::default(),
        );
        // An IMDb id — either the item id or the full Meta's imdb_id — reaches BetterPosters.
        let better = shim.poster_url("movie", "tt1254207", None, Some("https://addon/poster.jpg"));
        assert!(better.starts_with("https://btttr.cc/poster/imdb/poster-default/tt1254207.jpg"));
        let better = shim.poster_url("movie", "tmdb:999999999", Some("tt0133093"), Some("https://addon/poster.jpg"));
        assert!(better.starts_with("https://btttr.cc/poster/imdb/poster-default/tt0133093.jpg"));
        // With no TMDB key and no mapping, a tmdb: id keeps the addon's own artwork.
        let addon = shim.poster_url("movie", "tmdb:999999998", None, Some("https://metahub/poster.jpg"));
        assert_eq!(addon, "https://metahub/poster.jpg");
    }

    #[test]
    fn landscape_art_only_accepts_a_non_metahub_image() {
        assert!(!usable_landscape(None));
        assert!(!usable_landscape(Some("")));
        assert!(!usable_landscape(Some("https://images.metahub.space/background/medium/x/img")));
        assert!(usable_landscape(Some("https://image.tmdb.org/t/p/w1280/abc.jpg")));
    }

    #[test]
    fn tmdb_logo_prefers_english_then_the_first_with_a_file() {
        let images = TmdbImages {
            logos: vec![
                TmdbLogo { file_path: None, iso_639_1: Some("en".into()) },
                TmdbLogo { file_path: Some("/de.svg".into()), iso_639_1: Some("de".into()) },
                TmdbLogo { file_path: Some("/en.svg".into()), iso_639_1: Some("en".into()) },
            ],
        };
        assert_eq!(tmdb_logo(Some(&images)).as_deref(), Some("https://image.tmdb.org/t/p/w500/en.svg"));
        assert_eq!(tmdb_logo(None), None);
        assert_eq!(tmdb_logo(Some(&TmdbImages { logos: vec![TmdbLogo::default()] })), None);
    }

    #[test]
    fn enrichment_apply_sets_roles_extras_art_and_logo_without_tmdb() {
        let mut e = Enrichment::default();
        e.roles = Some(json!([{"id": (TMDB_PERSON_KEY_BASE + 638), "tag": "Keanu"}]));
        e.extras = Some(json!({"Metadata": [{"ratingKey": "1"}]}));
        e.art = Some("https://image.tmdb.org/t/p/w1280/backdrop.jpg".into());
        e.clear_logo = Some("https://image.tmdb.org/t/p/w500/logo.png".into());
        let mut row = json!({"art": "https://images.metahub.space/background/medium/x/img"});
        e.apply(10_001, &mut row);
        assert_eq!(row["Role"][0]["tag"], "Keanu");
        assert_eq!(row["Extras"]["Metadata"][0]["ratingKey"], "1");
        assert_eq!(row["art"], "https://image.tmdb.org/t/p/w1280/backdrop.jpg");
        assert_eq!(row["clearLogo"], "https://image.tmdb.org/t/p/w500/logo.png");
        assert_eq!(row["Image"][0]["type"], "clearLogo");
        assert!(row["Image"][0]["url"].as_str().unwrap().starts_with("/library/metadata/10001/clearLogo/"));
    }

    #[test]
    fn enrichment_is_a_serialisable_cache_value() {
        // The store's `cache_put` takes opaque bytes; the parsed shape must survive that round trip.
        let mut e = Enrichment { tmdb_id: 603, ..Default::default() };
        e.roles = Some(json!([{"tag": "Neo"}]));
        e.related = Some(RelatedSource::Movies(vec![TmdbMovie { id: 604, title: "Reloaded".into(), ..Default::default() }]));
        let bytes = serde_json::to_vec(&e).unwrap();
        let read: Enrichment = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(read.tmdb_id, 603);
        assert_eq!(read.roles.unwrap()[0]["tag"], "Neo");
        assert!(matches!(read.related, Some(RelatedSource::Movies(v)) if v[0].id == 604));
    }

    #[test]
    fn tmdb_details_row_uses_flat_fields_and_synthetic_person_ids() {
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let details = TmdbMovieDetails {
            id: 603, title: "The Matrix".into(), release_date: Some("1999-03-31".into()),
            runtime: Some(136), backdrop_path: Some("/backdrop.jpg".into()),
            belongs_to_collection: None,
            credits: plx_stremio::addons_tmdb::TmdbCredits {
                cast: vec![TmdbCast { id: 638, name: "Keanu Reeves".into(), character: "Neo".into(), profile_path: None }],
                ..Default::default()
            },
            recommendations: plx_stremio::addons_tmdb::TmdbRecommendations::default(),
            images: Some(TmdbImages { logos: vec![TmdbLogo { file_path: Some("/logo.png".into()), iso_639_1: Some("en".into()) }] }),
            videos: Some(TmdbVideos { results: vec![TmdbVideo {
                id: "v1".into(), key: "dQw4w9WgXcQ".into(), name: "Trailer".into(),
                site: "YouTube".into(), type_: "Trailer".into(),
            }] }),
        };
        let movie = TmdbMovie {
            id: details.id, title: details.title.clone(), release_date: details.release_date.clone(),
            runtime: details.runtime, backdrop_path: details.backdrop_path.clone(),
            poster_path: None, belongs_to_collection: None,
        };
        let row = shim.tmdb_details_movie_row_and_remember(&details, movie, 1_000_000_603);
        assert_eq!(row["ratingKey"], "1000000603");
        assert_eq!(row["title"], "The Matrix");
        assert_eq!(row["originallyAvailableAt"], "1999-03-31");
        assert_eq!(row["duration"], 136 * 60_000);
        assert_eq!(row["art"], "https://image.tmdb.org/t/p/w1280/backdrop.jpg");
        assert_eq!(row["clearLogo"], "https://image.tmdb.org/t/p/w500/logo.png");
        assert_eq!(row["Role"][0]["id"], TMDB_PERSON_KEY_BASE + 638);
        assert_eq!(row["Role"][0]["tagKey"], "nuvio-person-638");
        assert_eq!(row["Extras"]["Metadata"][0]["title"], "Trailer");
        assert_eq!(shim.tmdb_synthetic_row(1_000_000_603).unwrap()["title"], "The Matrix");
    }

    #[test]
    fn extras_from_videos_dedupes_and_skips_non_youtube_clips() {
        let videos = TmdbVideos { results: vec![
            TmdbVideo { id: "a".into(), key: "abcdefgh".into(), name: "A".into(), site: "YouTube".into(), type_: "Trailer".into() },
            TmdbVideo { id: "b".into(), key: "abcdefgh".into(), name: "A again".into(), site: "YouTube".into(), type_: "Teaser".into() },
            TmdbVideo { id: "c".into(), key: "other".into(), name: "Vimeo".into(), site: "Vimeo".into(), type_: "Trailer".into() },
        ]};
        let extras = extras_from_videos(Some(&videos), 10_100);
        assert_eq!(extras["size"], 1);
        assert_eq!(extras["Metadata"][0]["title"], "A");
        assert_eq!(extras["Metadata"][0]["ratingKey"], (TRAILER_KEY_BASE + 10_100).to_string());
    }

    #[test]
    fn hubs_use_the_plex_collection_and_related_identifiers() {
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let hub = related_hub_json("movie", vec![json!({"ratingKey": "1"})]);
        assert_eq!(hub["hubIdentifier"], "movie.related");
        let collection = TmdbCollection {
            id: 234, name: "Dark Knight".into(), overview: None, poster_path: None, backdrop_path: None,
            parts: vec![
                TmdbMovie { id: 603, title: "Batman Begins".into(), ..Default::default() },
                TmdbMovie { id: 604, title: "The Dark Knight".into(), ..Default::default() },
            ],
        };
        let hub = shim.collection_hub_json(&collection, 603, "fallback").unwrap();
        assert_eq!(hub["hubIdentifier"], "collection.related.1.1");
        assert_eq!(hub["title"], "Dark Knight");
        assert_eq!(hub["Metadata"].as_array().unwrap().len(), 1);
        assert_eq!(hub["Metadata"][0]["ratingKey"], "1000000604");
        let row = shim.collection_row(&collection);
        assert_eq!(row["ratingKey"], (TMDB_COLLECTION_KEY_BASE + 234).to_string());
        assert_eq!(row["index"], 234);
    }

    #[test]
    fn person_routes_join_by_tmdb_id_and_caption_roles_by_synthetic_person_id() {
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let person = TmdbPerson {
            name: "Christopher Nolan".into(), biography: "Director".into(),
            birthday: Some("1970-07-30".into()), deathday: None,
            place_of_birth: Some("London".into()), known_for_department: Some("Directing".into()),
            profile_path: Some("/nolan.jpg".into()),
        };
        let cast = vec![TmdbCredit { id: 603, name: "The Matrix".into(), character: "Neo".into(), media_type: "movie".into(), ..Default::default() }];
        let crew = vec![
            TmdbCredit { id: 604, name: "The Dark Knight".into(), job: "Director".into(), department: "Directing".into(), media_type: "movie".into(), ..Default::default() },
            TmdbCredit { id: 605, name: "Unknown".into(), job: "stunts".into(), department: "Crew".into(), media_type: "movie".into(), ..Default::default() },
        ];
        let header = shim.person_profile_row(&person, &cast, &crew);
        assert_eq!(header["bornAt"], "1970-07-30");
        assert_eq!(header["CreditType"][0]["title"], "Actor");
        let groups = shim.person_credit_groups(&cast, &crew);
        assert_eq!(groups[0]["type"], "actor");
        assert!(groups.iter().any(|g| g["title"] == "Director"));
        assert!(groups.iter().any(|g| g["title"] == "Other"));
        assert_eq!(groups[0]["Credit"][0]["Metadata"]["ratingKey"], "tmdb:603");
        let rows = shim.person_credit_rows(638, &cast);
        assert_eq!(rows[0]["type"], "movie");
        assert_eq!(rows[0]["Role"][0]["id"], TMDB_PERSON_KEY_BASE + 638);
        assert_eq!(rows[0]["Role"][0]["role"], "Neo");
    }

    #[test]
    fn unknown_person_guids_answer_empty_containers_not_errors() {
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        for (response, key) in [
            (shim.person_profile_page(""), "Metadata"),
            (shim.person_career_page("plex://person/nope"), "CreditGroup"),
        ] {
            assert_eq!(response.status, 200);
            let body: Value = serde_json::from_slice(&response.body).unwrap();
            assert_eq!(body["MediaContainer"][key].as_array().unwrap().len(), 0);
        }
    }

    #[test]
    fn a_synthetic_movie_metadata_route_answers_from_the_remembered_tmdb_row() {
        let shim = Arc::new(Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default()));
        let rk = 1_000_000_603;
        shim.tmdb_recommendation_row(&TmdbMovie {
            id: 603, title: "The Matrix".into(), release_date: Some("1999-03-31".into()),
            ..Default::default()
        });
        let req = Request {
            method: "GET".into(), path: format!("/library/metadata/{rk}"),
            query: HashMap::new(), headers: HashMap::new(), body: Vec::new(),
        };
        let response = shim.handle(&req);
        assert_eq!(response.status, 200);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["MediaContainer"]["Metadata"][0]["title"], "The Matrix");
        assert_eq!(body["MediaContainer"]["Metadata"][0]["ratingKey"], rk.to_string());
    }

    #[test]
    fn nuvio_streams_for_a_synthetic_key_is_complete_with_no_slot() {
        let shim = Arc::new(Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default()));
        let req = Request {
            method: "GET".into(), path: "/nuvio/streams".into(),
            query: HashMap::from([("ratingKey".into(), "2_000_001_396".replace('_', ""))]),
            headers: HashMap::new(), body: Vec::new(),
        };
        let response = shim.handle(&req);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["complete"], true);
        assert_eq!(body["streams"].as_array().unwrap().len(), 0);
    }

    /// A backend whose addon never answers: a Detail read must not wait for it.
    struct SlowBackend;
    impl Backend for SlowBackend {
        fn catalogs(&self) -> Vec<CatalogInfo> { crate::shim_fake::FakeBackend.catalogs() }
        fn catalog_page(&self, key: &str, skip: u32, genre: Option<&str>) -> Result<Vec<MetaPreview>, ()> {
            crate::shim_fake::FakeBackend.catalog_page(key, skip, genre)
        }
        fn search(&self, query: &str) -> Vec<(CatalogInfo, Vec<MetaPreview>)> { crate::shim_fake::FakeBackend.search(query) }
        fn meta(&self, type_: &str, id: &str) -> Option<Meta> { crate::shim_fake::FakeBackend.meta(type_, id) }
        fn streams(&self, _type_: &str, _id: &str, _deadline: Duration, _on_batch: &mut dyn FnMut(&[Stream])) -> Vec<Stream> {
            std::thread::sleep(Duration::from_millis(600));
            Vec::new()
        }
        fn subtitles(&self, type_: &str, id: &str) -> Vec<plx_stremio::model::Subtitle> {
            crate::shim_fake::FakeBackend.subtitles(type_, id)
        }
    }

    #[test]
    fn detail_read_does_not_wait_for_a_slow_addon() {
        let shim = Arc::new(Shim::new(Arc::new(SlowBackend), Config::default()));
        let rk = shim.keys.key_of(&Ident::Title { type_: "movie".into(), id: "tt1254207".into() });
        let req = Request {
            method: "GET".into(), path: format!("/library/metadata/{rk}"),
            query: HashMap::new(), headers: HashMap::new(), body: Vec::new(),
        };
        let started = Instant::now();
        let response = shim.handle(&req);
        let elapsed = started.elapsed();
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        let media = &body["MediaContainer"]["Metadata"][0]["Media"];
        assert!(media[0]["title"].as_str().unwrap_or_default().contains("Finding streams"));
        assert!(elapsed < Duration::from_millis(300), "detail waited {elapsed:?} for streams");
    }
}

#[cfg(test)]
mod account_progress_tests {
    use super::*;

    #[test]
    fn a_playing_report_is_throttled_and_a_settled_one_is_not() {
        let now = Instant::now();
        assert!(watch_push_due(None, false, now));
        assert!(!watch_push_due(Some(now), false, now + Duration::from_secs(59)));
        assert!(watch_push_due(Some(now), false, now + WATCH_PUSH_INTERVAL));
        assert!(watch_push_due(Some(now), true, now));
    }

    #[test]
    fn episode_identity_helpers_name_the_series_and_the_season_episode() {
        assert_eq!(episode_series_id("tt0944947:1:2", "tmdb:1396"), "tt0944947");
        assert_eq!(episode_series_id("tmdb:1396:3:7", "tmdb:1396"), "tmdb:1396");
        assert_eq!(episode_series_id("tt0944947", "tmdb:1396"), "tmdb:1396");
        assert_eq!(episode_numbers("tt0944947:1:2"), (Some(1), Some(2)));
        assert_eq!(episode_numbers("tmdb:1396:0:5"), (Some(0), Some(5)));
        assert_eq!(episode_numbers("tt0944947"), (None, None));
        assert_eq!(episode_numbers("tt0944947:1"), (None, None));
        assert_eq!(account_tmdb_number("movie", "tmdb:550"), Some(550));
        assert_eq!(account_tmdb_number("series", "tmdb:1396:3:7"), Some(1396));
        assert_eq!(account_tmdb_number("movie", "tmdb:0"), None);
    }

    /// Nuvio names a title by IMDb id: one already named that way must be pushed unchanged, and an
    /// episode carries its season/episode so every episode is not collapsed into one account row.
    #[test]
    fn a_movie_and_an_episode_are_named_by_imdb() {
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let movie = shim.keys.key_of(&Ident::Title { type_: "movie".into(), id: "tt0133093".into() });
        let i = shim.nuvio_watch_identity(movie).expect("a movie is a resume point");
        assert_eq!((i.content_id.as_str(), i.content_type, i.video_id.as_str()),
            ("tt0133093", "movie", "tt0133093"));
        assert_eq!((i.season, i.episode), (None, None));

        let episode = shim.keys.key_of(&Ident::Episode {
            series: "tt0944947".into(), video: "tt0944947:1:2".into() });
        let i = shim.nuvio_watch_identity(episode).expect("an episode is a resume point");
        assert_eq!((i.content_id.as_str(), i.content_type, i.video_id.as_str()),
            ("tt0944947", "series", "tt0944947:1:2"));
        assert_eq!((i.season, i.episode), (Some(1), Some(2)));

        // A season and a whole series have no single position of their own.
        let season = shim.keys.key_of(&Ident::Season { series: "tt0944947".into(), n: 1 });
        assert!(shim.nuvio_watch_identity(season).is_none());
        let series = shim.keys.key_of(&Ident::Title { type_: "series".into(), id: "tt0944947".into() });
        assert!(shim.nuvio_watch_identity(series).is_none());
    }

    /// A row Nuvio's own clients pushed with a synthetic episode id lands on the addon's real
    /// episode, so Continue Watching can draw a row for it; an episode the series lacks is skipped.
    #[test]
    fn a_synthetic_episode_row_is_placed_through_season_and_episode() {
        use plx_stremio::nuvio::WatchProgress;
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let series = crate::shim_fake::FAKE_SERIES_ID;
        let entry = |s: i64, e: i64| WatchProgress {
            content_id: series.into(), content_type: "series".into(),
            video_id: format!("{NUVIO_SYNTHETIC_EPISODE}{s}:{e}"),
            season: Some(s), episode: Some(e), position: 1000, duration: 2000, last_watched: 3000,
        };
        let found = shim.account_watch_key(&entry(1, 2)).expect("season 1 episode 2 exists");
        assert_eq!(shim.keys.ident(found), Some(Ident::Episode {
            series: series.into(), video: format!("{series}:1:2") }));
        assert!(shim.account_watch_key(&entry(9, 9)).is_none());
    }

    #[test]
    fn a_real_merge_bumps_the_content_generation_and_a_noop_does_not() {
        let shim = Shim::new(Arc::new(crate::shim_fake::FakeBackend), Config::default());
        let before = shim.content_generation();
        shim.account_watch_merged(0);
        assert_eq!(shim.content_generation(), before);
        shim.account_watch_merged(2);
        assert_eq!(shim.content_generation(), before + 1);
    }
}
