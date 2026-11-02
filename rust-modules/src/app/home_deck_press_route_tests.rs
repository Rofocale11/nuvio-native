//! **A Continue Watching card's press goes where the deck setting says** — a page under *Open
//! details*, a play under *Play*.
//!
//! The tier-C port mapped upstream's `activate_deck_card(from_deck, want_play)` onto this fork's
//! `activate_card(want_play)` and passed `from_deck` in that slot. Home had already resolved the
//! press through `deck_press()` and emitted `HomeReq::Detail`, so a Detail press on a Continue
//! Watching card arrived at the opener with "play" and started (or, with nothing to play, silently
//! lost) a stream the user never asked for. The card was found and the request drained, which is
//! why it read as a dead tile.
//!
//! Key-driven on a real `Dispatcher<AppHost>` (`bridge_test_support::World`), through the
//! production opener: the pump's `perform_home_detail` calls `content::activate_home_item` itself.

use super::*;
use super::test_support::World;
use plx_media::route::DeckPress;
use plx_screens::registry::ContentArg;

/// Put the deck setting back when the test ends, even on a panic. Created after the world, so the
/// serial lock `restore_deck_press` requires is held and is released only after the restore.
struct DeckPressGuard(DeckPress);
impl Drop for DeckPressGuard {
    fn drop(&mut self) { plx_media::route::restore_deck_press(self.0); }
}

#[test]
fn a_continue_watching_card_opens_its_detail_page_under_open_details() {
    let mut w = World::home_with_disc("cw-detail-press", [7; 12]);
    let _deck = DeckPressGuard(plx_media::route::deck_press());
    plx_media::route::restore_deck_press(DeckPress::Details);
    w.bridge.stores.hubs.seed_deck_home_for_test(2);
    w.frames(30);
    w.press(Key::Down);
    w.frames(30);
    let focused = w.focus().expect("the deck's first card takes focus");
    assert!(w.bridge.with_home(&w.d, |home, cx, _| {
        home.focused_item::<AppHost>(Some(focused), cx).map(|item| item.rk == "1")
    }).flatten().unwrap_or(false), "premise: the focused card is the deck's first episode, got {focused:?}");

    w.press(Key::Ok);
    w.frames(60);

    assert!(
        matches!(w.d.top_arg(), Some(AppArg::Content(ContentArg::Detail { rk, .. })) if rk == "1"),
        "OK on a Continue Watching card under Open details opens that episode's page"
    );
}
