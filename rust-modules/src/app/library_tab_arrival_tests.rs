//! A Library page reached from a strip pill: where its first selection plays.
//!
//! The pill press mounts the page's fade at 0 and reveals it once its listing lands. The page's
//! arrival seat is its first poster — the route table declares `Arrival::Content`, so the header
//! is one Up away — but the seat is DORMANT while the page dissolves in: the grid holds it at
//! rest until the first opaque frame, so the lift plays where it can be seen rather than under
//! the fade.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame};

#[test]
fn a_pill_arrival_pops_its_head_tile_after_the_page_is_opaque_not_under_the_dissolve() {
    let _guard = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("library-tab-arrival");
    session.watching("u-library-tab-arrival");
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            plx_plex::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    plx_plex::plex::reset_servers_for_test();
    let own = plx_plex::plex::register_for_test("arrive-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared = plx_plex::plex::register_for_test("arrive-shared", "127.0.0.1", 10, "synthetic", "fixture");
    plx_plex::plex::set_current(own);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
    rig.refresh_browse_directory();
    rig.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
    rig.stores.browse.borrow_mut().seed_items_for_test(12);
    // What `nav_tab` queues for a Movies press, before the page exists.
    rig.enter_library(plx_data::browse::SecKind::Movie);
    fn page(d: &Dispatcher<AppHost>) -> &plx_screens::library::LibraryScreen {
        let entry = d.nav.top_page().unwrap();
        entry.inst.as_ref().unwrap().screen.as_any().unwrap()
            .downcast_ref::<plx_screens::library::LibraryScreen>().unwrap()
    }
    // The first selection of the page: `(frame, page alpha, the seated tile's drawn pop)`. `1.0`
    // is rest and `focus_scale` is fully lifted; a dormant (still dissolving) grid probes rest.
    let mut timeline = Vec::new();
    for i in 0..90 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        let (alpha, pop) = page(&d).probe_arrival(d.focus());
        timeline.push((i, alpha, pop));
    }
    assert!(!timeline.is_empty(), "the page never recorded an arrival");
    let first_dark = timeline.iter().position(|(_, alpha, _)| *alpha < 1.0).expect("the page never dissolved in");
    let opaque = timeline[first_dark..].iter().position(|(_, alpha, _)| *alpha >= 1.0)
        .map(|at| at + first_dark).expect("the page never became opaque");
    assert!(opaque > first_dark, "the fixture must include the dissolve: {timeline:?}");
    assert_eq!(page(&d).probe_viewport(d.focus()).0, "grid", "the arrival seat is the first poster");
    assert!(
        timeline[first_dark..opaque].iter().all(|(_, _, pop)| *pop <= 1.01),
        "the head tile grew while the page was still dissolving in: {timeline:?}"
    );
    let after = &timeline[opaque..];
    assert!(after[0].2 < 1.01, "the tile was already up when the page turned opaque: {:?}", after[0]);
    assert!(after.iter().any(|(_, _, pop)| *pop > 1.05),
        "the tile never grew after the page turned opaque: {after:?}");
    let ramp: Vec<f32> = after.iter().map(|(_, _, pop)| *pop).collect();
    assert!(ramp.windows(2).all(|w| w[1] >= w[0] - 1.0e-4), "the pop must grow monotonically: {ramp:?}");
}
