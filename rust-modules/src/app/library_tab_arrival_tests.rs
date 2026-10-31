//! A Library page reached from a strip pill: where its first selection plays.
//!
//! The pill press mounts the page's fade at 0 and reveals it once its listing lands. The
//! grid-only Library (since `6b05b404`) no longer has hub shelves: its document head is the
//! Type/Sort/Filter control row, so that row is the pill arrival's seat and no poster is
//! focused while the page is dark. The first poster is reached with DOWN after the page is
//! opaque, and it lifts from rest there — the selection has to play out where it can be seen,
//! not under the dissolve.

use super::*;
use plx_ui::screen::Focusable;
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
    // The first selection of the page: `(frame, page alpha, region the focus is in)`.
    let mut arrival = Vec::new();
    for i in 0..20 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        let (alpha, _, _, _, _) = page(&d).probe_content();
        arrival.push((i, alpha, page(&d).probe_viewport(d.focus()).0));
    }
    assert!(!arrival.is_empty(), "the page never recorded an arrival");
    let first_dark = arrival.iter().position(|(_, alpha, _)| *alpha < 1.0).expect("the page never dissolved in");
    let opaque = arrival[first_dark..].iter().position(|(_, alpha, _)| *alpha >= 1.0)
        .map(|at| at + first_dark).expect("the page never became opaque");
    assert!(opaque > first_dark, "the fixture must include the dissolve: {arrival:?}");
    assert!(
        arrival[first_dark..opaque].iter().all(|(_, _, region)| *region != "grid"),
        "a poster took focus under the dissolve: {arrival:?}"
    );
    assert_eq!(
        arrival[opaque].2, "toolbar",
        "the grid-only Library's head seat is its Type/Sort/Filter row: {:?}",
        arrival[opaque]
    );
    // DOWN enters the grid after the page is opaque. The first card's drawn pop (normalised so
    // 1.0 is rest and `focus_scale` is fully lifted) must rise from rest, not be adopted whole.
    let scale = plx_ui::cards::RowStyle::HOME.focus_scale;
    assert_eq!(page(&d).probe_viewport(d.focus()).0, "toolbar", "premise: the head row is seated");
    frame(&mut d, &mut rig, AppArg::Library, tick(20), script_key(Key::Down, tick(20)));
    let mut lift = Vec::new();
    for i in 21..90 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        let page = page(&d);
        let Some(key) = d.focus().filter(|_| page.probe_viewport(d.focus()).0 == "grid") else { continue };
        let parts = CxParts {
            tick: tick(i),
            press: Default::default(),
            focus: d.input.engine.read(InputOwner::Entry(key.entry)),
            owner: InputOwner::Entry(key.entry),
        };
        let cx = parts.cx::<AppHost>(
            rig.views_with(plx_media::route::idle_session_for_test()),
            &rig.measure,
        );
        if let Some(placed) = page.place(&key.elem, &cx, At::Drawn) {
            lift.push(placed.rect.w / placed.rest_rect.w * scale);
        }
    }
    assert!(!lift.is_empty(), "the head tile never took focus");
    let full = *lift.last().unwrap();
    assert!(full > 1.02, "premise: the seated tile ends lifted: {full}");
    assert!(
        lift[0] < 1.0 + (full - 1.0) * 0.5,
        "the lift is adopted whole on the frame the grid is entered: {} of {full}: {lift:?}",
        lift[0]
    );
    assert!(lift.windows(2).all(|pair| pair[1] >= pair[0] - 1.0e-4), "the lift is not a monotone rise: {lift:?}");
}
