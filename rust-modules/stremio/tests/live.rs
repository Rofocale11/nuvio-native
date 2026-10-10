//! Live network tests (`cargo test -p plx_stremio --features live-tests -- --nocapture`).
//!
//! Public addons only by default. `NUVIO_IMPORT_FILE=<path>` additionally runs the user's own
//! Nuvio addon set from a `nuvio-localstorage` export, read-only (catalog, meta and stream-list
//! GETs; nothing is played or downloaded). Output prints addon HOSTS only, never configured URLs.
#![cfg(feature = "live-tests")]

use std::time::{Duration, Instant};

use plx_stremio::{classify_stream, redact_url, AddonSet};

const SHAWSHANK: &str = "tt0111161";
const GOT_S1E1: &str = "tt0944947:1:1";

#[test]
fn cinemeta_catalog_meta_and_search() {
    let set = AddonSet::defaults();
    let home = set.home_catalogs();
    let t = Instant::now();
    let items = set.catalog(&home[0], 0, None).expect("cinemeta top movies");
    println!("cinemeta {} -> {} items in {:?}", home[0].key(&set), items.len(), t.elapsed());
    assert!(items.len() >= 20);
    let page2 = set.catalog(&home[0], items.len() as u32, None).expect("page 2");
    assert!(!page2.is_empty());
    let genre = set.catalog(&home[0], 0, Some("Sci-Fi")).expect("genre");
    assert!(!genre.is_empty());

    let m = set.meta("movie", SHAWSHANK).expect("movie meta");
    assert_eq!(m.name, "The Shawshank Redemption");
    let s = set.meta("series", "tt0944947").expect("series meta");
    let s1: Vec<_> = s.videos.iter().filter(|v| v.season == Some(1)).collect();
    println!("GoT: {} videos, S1 has {}", s.videos.len(), s1.len());
    assert_eq!(s1.len(), 10);
    assert!(s1.iter().all(|v| v.episode.is_some()));

    let t = Instant::now();
    let found = set.search("matrix");
    println!("search 'matrix': {} catalogs in {:?}", found.len(), t.elapsed());
    assert!(found.iter().any(|(_, items)| items.iter().any(|i| i.id == "tt0133093")));

    let subs = set.subtitles("movie", SHAWSHANK);
    println!("opensubtitles: {} subtitles", subs.len());
}

#[test]
fn torrentio_public_streams() {
    let mut set = AddonSet::defaults();
    match set.install("https://torrentio.strem.fun/manifest.json") {
        Ok(i) => println!("installed {} ({})", set.addons[i].manifest.name, set.addons[i].manifest.id),
        Err(e) => panic!("torrentio manifest: {e}"),
    }
    for (ty, id) in [("movie", SHAWSHANK), ("series", GOT_S1E1)] {
        let t = Instant::now();
        let mut batches = 0;
        let streams = set.streams(ty, id, Duration::from_secs(30), &mut |_| batches += 1);
        println!("torrentio {ty} {id}: {} streams ({batches} batches) in {:?}", streams.len(), t.elapsed());
        assert!(!streams.is_empty());
        assert!(streams.iter().all(|s| s.info_hash.is_some() && s.addon_name == "Torrentio"));
        for s in streams.iter().take(4) {
            let i = classify_stream(s, Some(140 * 60));
            println!("  {:<60} | {:?}", i.label, s.name.as_deref().unwrap_or("").replace('\n', " / "));
        }
        let with_height = streams.iter().filter(|s| classify_stream(s, None).height.is_some()).count();
        assert!(with_height * 10 >= streams.len() * 8, "resolution parsed for {with_height}/{}", streams.len());
    }
}

#[test]
fn user_nuvio_import_read_only() {
    let Ok(path) = std::env::var("NUVIO_IMPORT_FILE") else {
        println!("NUVIO_IMPORT_FILE not set; skipping");
        return;
    };
    let mut set = AddonSet::from_import_file(std::path::Path::new(&path)).expect("import");
    for a in &set.addons {
        println!(
            "addon {:<28} id={:<32} disabled={} catalogs={} resources={:?}",
            redact_url(&a.transport_url),
            a.manifest.id,
            a.disabled,
            a.manifest.catalogs.len(),
            a.manifest.resources().iter().map(|r| r.name.clone()).collect::<Vec<_>>()
        );
    }
    if set.needs_manifests() {
        println!("filled {} missing manifests", set.refresh_missing_manifests(Duration::from_secs(15)));
    }
    let home = set.home_catalogs();
    println!("home shelves: {}", home.len());
    assert!(!home.is_empty());

    // First two shelves: catalog, then meta (source addon first) and streams for the first item.
    for cat in home.iter().take(2) {
        let t = Instant::now();
        let items = match set.catalog(cat, 0, None) {
            Ok(items) => items,
            Err(e) => {
                println!("catalog {} failed: {e}", cat.key(&set));
                continue;
            }
        };
        println!("catalog {} '{}' -> {} items in {:?}", cat.key(&set), cat.name(), items.len(), t.elapsed());
        let Some(first) = items.first() else { continue };
        let t = Instant::now();
        let meta = set.meta_preferring(Some(cat.addon), &first.type_, &first.id).expect("meta");
        println!("  meta {} {} '{}' imdb_id={:?} videos={} in {:?}", first.type_, first.id, meta.name, meta.imdb_id, meta.videos.len(), t.elapsed());
        let video_id = if first.type_ == "series" {
            meta.videos.iter().find(|v| v.season == Some(1) && v.episode == Some(1)).map(|v| v.id.clone()).unwrap_or(meta.id.clone())
        } else {
            meta.id.clone()
        };
        let sid = set.stream_id(&first.type_, &video_id, meta.imdb_id.as_deref());
        let t = Instant::now();
        let mut first_batch: Option<Duration> = None;
        let streams = set.streams(&first.type_, &sid, Duration::from_secs(30), &mut |_| {
            first_batch.get_or_insert(t.elapsed());
        });
        println!("  streams for {sid}: {} (first batch {:?}, all {:?})", streams.len(), first_batch, t.elapsed());
        for s in streams.iter().take(6) {
            let i = classify_stream(s, None);
            println!("    {:<64} direct={}", i.label, plx_stremio::torrent::direct_url(s).is_some());
            println!("      name={:?}", s.name.as_deref().unwrap_or("").replace('\n', " / "));
            println!("      desc={:?}", s.description.as_deref().or(s.title.as_deref()).unwrap_or("").replace('\n', " / "));
        }
    }
    let t = Instant::now();
    let found = set.search("dune");
    println!("search 'dune': {} catalogs, {} items in {:?}", found.len(), found.iter().map(|(_, v)| v.len()).sum::<usize>(), t.elapsed());
}
