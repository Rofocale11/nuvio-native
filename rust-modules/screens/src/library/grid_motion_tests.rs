// The All grid's focus motion sampled over frames, in the delivery order the dispatcher
// guarantees (`ui::fixture::a_key_moves_focus_and_announces_it_before_the_frames_tick`): the
// key's `FocusMoved`, then the frame's `Tick`.
use super::*;

fn frame(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, ms: u32) {
    deliver(page, engine, fixture, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
}

fn settle(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, from: u32) {
    for i in 0..120 { frame(page, engine, fixture, from + i * 16); }
}

fn strictly_between(v: f32, a: f32, b: f32) -> bool { v > a.min(b) + 1.0e-3 && v < a.max(b) - 1.0e-3 }

/// Owner report items 4, 5 and 7: the tile losing focus returns to rest over frames, and the
/// rows open and close their caption band over frames — sideways, down, and out of the grid.
#[test]
fn an_all_grid_tile_and_its_rows_animate_back_when_focus_leaves() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let start = page.key(page.pair.detail.elem_at(12).unwrap());
    engine.set(OWNER, start, Some(page.pair.groups_config().detail), By::Restore);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from: None, to: start, by: By::Restore });
    settle(&mut page, &mut engine, &fixture, 0);
    let rest = page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 13).w;
    let full = page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 12).w;
    assert!(full > rest + 1.0, "the settled focused tile is lifted");

    direction(&mut page, &mut engine, &fixture, Dir::Right);
    frame(&mut page, &mut engine, &fixture, 2000);
    let leaving = page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 12).w;
    assert!(strictly_between(leaving, rest, full), "one frame after RIGHT the old tile is mid-return: {leaving}");
    settle(&mut page, &mut engine, &fixture, 2016);
    assert_eq!(page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 12).w, rest);

    let below = page.layout.row_y(4, page.scroll.pos);
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    frame(&mut page, &mut engine, &fixture, 5000);
    assert!(strictly_between(page.layout.row_expansion(2), 0.0, 1.0), "row 2 closes over frames");
    assert!(strictly_between(page.layout.row_expansion(3), 0.0, 1.0), "row 3 opens over frames");
    settle(&mut page, &mut engine, &fixture, 5016);
    assert!(page.layout.row_y(4, page.scroll.pos) != below, "the settled layout moved row 4");

    for _ in 0..4 { direction(&mut page, &mut engine, &fixture, Dir::Up); }
    assert!(engine.current(OWNER).is_some_and(|key| page.pair.detail.index_of(key.elem).is_none()),
        "focus left the grid for its heading");
    frame(&mut page, &mut engine, &fixture, 9000);
    assert!(strictly_between(page.layout.row_expansion(3), 0.0, 1.0), "leaving the grid closes its band over frames");
}

fn numbered(rks: std::ops::Range<usize>) -> plx_data::stores::browse::ListingSnapshot {
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let total = rks.len() as i64;
    plx_data::browse::view::ListingSnapshot::fixture(sid, rks.map(|i|
        Some(plx_data::pms::PmsMovie { sid, rk: format!("{i}"), title: format!("s{i:04x}"), ..Default::default() })).collect(),
        vec![("A".into(), total)])
}

/// A page on `before` with the item numbered `focus` seated and settled.
fn seated(before: std::ops::Range<usize>, focus: usize) -> (Fixture, LibraryScreen, FocusEngine<u32>, u32) {
    let mut fixture = Fixture::new();
    fixture.listing = numbered(before.clone());
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let elem = page.pair.detail.elem_at(focus - before.start).unwrap();
    let key = page.key(elem);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from: None, to: key, by: By::Restore });
    settle(&mut page, &mut engine, &fixture, 0);
    (fixture, page, engine, elem)
}

/// The listing lands `after` (a requery-free refill: same section, same query) and one frame ticks.
fn land(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &mut Fixture, after: std::ops::Range<usize>, ms: u32) {
    fixture.listing = numbered(after);
    deliver(page, engine, fixture, ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
    frame(page, engine, fixture, ms);
}

fn drawn_y(page: &LibraryScreen, engine: &FocusEngine<u32>, fixture: &Fixture, elem: u32) -> f32 {
    page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), page.pair.detail.index_of(elem).unwrap()).y
}

/// A content landing that moves the focused element to another row leaves its tile where it was
/// on screen, whichever way the row moved: a row inserted above pushes it down and a row removed
/// above pulls it up, and the caption band the new row opens does not skew the shift.
#[test]
fn a_landing_above_the_focused_tile_leaves_it_where_it_was_on_screen() {
    let _guard = plx_base::testlock::serial();
    for (before, after, what) in [(6..60, 0..60, "a row inserted above"), (0..60, 6..60, "a row removed above")] {
        let (mut fixture, mut page, mut engine, elem) = seated(before, 20);
        let y = drawn_y(&page, &engine, &fixture, elem);
        land(&mut page, &mut engine, &mut fixture, after, 4000);
        assert!((drawn_y(&page, &engine, &fixture, elem) - y).abs() < 0.5,
            "{what}: the tile moved on screen: {y} -> {}", drawn_y(&page, &engine, &fixture, elem));
        settle(&mut page, &mut engine, &fixture, 4016);
        assert!((drawn_y(&page, &engine, &fixture, elem) - y).abs() < 0.5, "{what}: and stayed put once settled");
    }
}

/// The shift never carries the page outside its document: a tile near the head of a barely scrolled
/// page pulled up by a removed row clamps at the head, and one at the foot of a scrolled page
/// pushed down by an inserted row while the tail goes away clamps at the foot.
#[test]
fn a_landing_shift_never_scrolls_the_page_outside_its_document() {
    let _guard = plx_base::testlock::serial();
    let within = |page: &LibraryScreen, what: &str| {
        let max = page.target_layout.max_scroll();
        assert!((0.0..=max).contains(&page.scroll.pos), "{what}: scroll {} outside 0..={max}", page.scroll.pos);
        assert!((0.0..=max).contains(&page.scroll_target), "{what}: target {} outside 0..={max}", page.scroll_target);
    };
    let (mut fixture, mut page, mut engine, _) = seated(0..60, 12);
    page.scroll.jump(50.0);
    page.scroll_target = 50.0;
    land(&mut page, &mut engine, &mut fixture, 6..60, 4000);
    within(&page, "head");

    let (mut fixture, mut page, mut engine, _) = seated(6..60, 50);
    let max = page.target_layout.max_scroll();
    page.scroll.jump(max);
    page.scroll_target = max;
    land(&mut page, &mut engine, &mut fixture, 0..52, 4000);
    within(&page, "foot");
}

/// A requery that empties and refills while the page is covered leaves the grid's pop on a cell
/// of the OLD content. Uncovering must read the refill as new content (the reset jump alone
/// places the page), not as the same element landing at another index and shifting the page
/// by rows on top of the jump.
#[test]
fn a_requery_refilled_under_a_cover_is_not_read_as_a_landing_on_uncover() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, mut engine, elem) = seated(30..90, 50);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::Cover);
    fixture.listing = numbered(0..0).with_query(99);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
    fixture.listing = numbered(0..90).with_query(99);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
    assert!(page.pair.detail.index_of(elem) != Some(20), "the same element sits at a new index");
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::Uncover);
    let jumped = page.scroll.pos;
    frame(&mut page, &mut engine, &fixture, 6000);
    assert!((page.scroll.pos - jumped).abs() < 0.5, "uncovering moved the page by a landing's rows: {jumped} -> {}", page.scroll.pos);
}
