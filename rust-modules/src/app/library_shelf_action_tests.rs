//! Library grid physical-hold engine item capture.
//!
//! The Library is grid-only since `6b05b404`: `LibraryScreen::sync` clears the hub shelves and
//! never emits `PublishShelves`, so there is no continue-watching shelf left to carry the deck
//! flag. The physical hold is still the grid's card press: it must capture the engine's item,
//! open the bridge's rest opener over the pressed tile, and ask `ItemMenu` exactly once with
//! `from_deck` false (the flag is structurally false for a grid tile).

use super::*;
use plx_ui::screen::Focusable;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame};

#[test]
fn shelf_physical_hold_captures_engine_item_deck_flag_and_bridge_rest_opener() {
    let _guard = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("library-shelf-hold");
    session.watching("u-library-shelf-hold");
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            plx_plex::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    // Two grid rows, the same two cards the old shelf fixture pressed: column 1 of rows 0 and 1.
    for row in 0..2 {
        plx_plex::plex::reset_servers_for_test();
        let own =
            plx_plex::plex::register_for_test("shelf-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared =
            plx_plex::plex::register_for_test("shelf-shared", "127.0.0.1", 10, "synthetic", "fixture");
        plx_plex::plex::set_current(own);
        let mut d = Dispatcher::<AppHost>::new();
        let mut rig = Bridge::for_test(|| 0);
        rig.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
        rig.refresh_browse_directory();
        rig.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
        rig.stores.browse.borrow_mut().seed_items_for_test(12);
        for i in 0..80 {
            frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        }
        let position = |d: &Dispatcher<AppHost>| {
            d.top_screen()
                .unwrap()
                .as_any()
                .unwrap()
                .downcast_ref::<plx_screens::library::LibraryScreen>()
                .unwrap()
                .probe_viewport(d.focus())
        };
        // Seat the page's own command on the card this iteration presses.
        Bridge::library_command(
            &mut d,
            plx_screens::registry::LibraryCmd::FocusGrid { row, col: 1 },
        );
        for i in 80..90 {
            frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        }
        assert_eq!(
            (position(&d).0, position(&d).1, position(&d).2),
            ("grid", row as i32, 1),
            "the fixture must seat the pressed grid card"
        );
        let i = 90;
        let key = d.focus().unwrap();
        let (item, _) = rig.library_selection(&d, key.entry, Some(key)).unwrap();
        // Column 1 of row `row` is the second card of that row in `seed_items_for_test`'s
        // 1-based rating keys.
        assert_eq!(item.rk, (row * 6 + 2).to_string(), "not the intended grid card");
        let key_input = |edge, n| {
            vec![InputEvent {
                at: tick(n),
                source: Source::Script,
                kind: InputKind::Key {
                    key: Key::Ok,
                    sym: 0,
                    wcode: 0,
                    edge,
                    at_edge: false,
                },
            }]
        };
        rig.take_library_reqs();
        frame(
            &mut d,
            &mut rig,
            AppArg::Library,
            tick(i),
            key_input(Edge::Down, i),
        );
        assert_eq!(d.input.arm.as_ref().map(|arm| arm.key), Some(key));
        assert!(d.input.press.is_live());
        let mut requests = Vec::new();
        let mut saw_scaled_press = false;
        let rect = |r: plx_ui::Rect| [r.x, r.y, r.w, r.h];
        for n in i + 1..i + 80 {
            frame(&mut d, &mut rig, AppArg::Library, tick(n), vec![]);
            assert_eq!(d.focus(), Some(key));
            let page = d
                .nav
                .entry(key.entry)
                .unwrap()
                .inst
                .as_ref()
                .unwrap()
                .screen
                .as_any()
                .unwrap()
                .downcast_ref::<plx_screens::library::LibraryScreen>()
                .unwrap();
            let parts = CxParts {
                tick: tick(n),
                press: plx_machine::machine::PressRead {
                    scale: d.input.press.scale(),
                    is_long: d.input.press.was_long(),
                    held_ms: None,
                    owner: d.input.dip_owner,
                },
                focus: d.input.engine.read(InputOwner::Entry(key.entry)),
                owner: InputOwner::Entry(key.entry),
            };
            let cx = parts.cx::<AppHost>(
                rig.views_with(plx_media::route::idle_session_for_test()),
                &rig.measure,
            );
            let placed = page.place(&key.elem, &cx, At::Drawn).unwrap();
            let (selected, opener) = rig.library_selection(&d, key.entry, Some(key)).unwrap();
            assert_eq!(
                (selected.sid, selected.rk.as_str()),
                (item.sid, item.rk.as_str())
            );
            assert_eq!(rect(opener.rect.unwrap()), rect(placed.rest_rect));
            if d.input.press.is_live() && d.input.press.scale() < 0.99 {
                saw_scaled_press = true;
                assert!(
                    placed.rect.w < placed.rest_rect.w,
                    "the bridge opener must not inherit the pressed card's shrink"
                );
            }
            requests.extend(rig.take_library_reqs().into_iter().filter(|(_, req, _)| {
                matches!(
                    req,
                    LibraryReq::ItemMenu { .. }
                        | LibraryReq::Play { .. }
                        | LibraryReq::Detail { .. }
                )
            }));
        }
        assert!(
            saw_scaled_press,
            "exercise a real armed press, not a default scale fixture"
        );
        assert_eq!(
            requests.len(),
            1,
            "hold produces one menu, never play/detail"
        );
        assert_eq!(
            requests[0].1,
            LibraryReq::ItemMenu {
                sid: item.sid,
                rk: item.rk.clone(),
                from_deck: false
            }
        );
        assert_eq!(requests[0].2.focus, Some(key));
        let (selected, opener) = rig
            .library_selection(&d, key.entry, requests[0].2.focus)
            .unwrap();
        assert_eq!((selected.sid, selected.rk), (item.sid, item.rk));
        assert!(opener.rect.is_some());
        frame(
            &mut d,
            &mut rig,
            AppArg::Library,
            tick(i + 80),
            key_input(Edge::Up, i + 80),
        );
        for n in i + 81..i + 120 {
            frame(&mut d, &mut rig, AppArg::Library, tick(n), vec![]);
        }
        assert!(
            !rig.take_library_reqs().iter().any(|(_, req, _)| matches!(
                req,
                LibraryReq::ItemMenu { .. } | LibraryReq::Play { .. } | LibraryReq::Detail { .. }
            )),
            "release after handled hold must not activate again"
        );
    }
}
