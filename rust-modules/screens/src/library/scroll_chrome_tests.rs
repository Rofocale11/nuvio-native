//! The Library's use of the shared scroll chrome (`plx_ui::scroll_chrome`) and of the shared rest
//! rule (`poster_grid::rest_scroll`, through its layout), and what a tucked page declares.
// Fork Rofocale11: the scrolled capsule must also name the addon catalog and the active filter —
// facts this fork has and upstream Plex does not — so the four items are pinned here.
use plx_ui::scroll_capsule;

/// A focused row below the first settles with its poster top where the first row's is at rest
/// (the grid top, y 244): focus reads on one line at the top of a library and deep in it. At the
/// end of the list the document cannot scroll further, so the last row may sit lower.
#[test]
fn a_focused_row_settles_at_the_first_rows_resting_y() {
    use super::layout::{Layout, LIBRARY_GRID_TOP};
    let lay = Layout::new(true, 400);
    assert_eq!(lay.row_y(0, lay.row_reveal(0)), LIBRARY_GRID_TOP);
    assert_eq!(LIBRARY_GRID_TOP, 244.0);
    for row in [1, 2, 5, 40, 200] {
        let focused = lay.with_grid_focus(Some(row));
        assert_eq!(focused.row_y(row, focused.row_reveal(row)), LIBRARY_GRID_TOP, "row {row} rests at 244");
    }
    let last = lay.rows - 1;
    let focused = lay.with_grid_focus(Some(last));
    let y = focused.row_y(last, focused.row_reveal(last));
    assert!(y >= LIBRARY_GRID_TOP, "the last row never rests above the line: {y}");
    assert_eq!(focused.row_reveal(last), focused.max_scroll(), "the end clamps, it does not scroll past it");
    assert!(y > LIBRARY_GRID_TOP, "the last row of a long list sits lower: {y}");
}

#[test]
fn a_settled_row_is_clear_of_the_capsule() {
    assert!(super::layout::LIBRARY_GRID_TOP >= scroll_capsule::Y + scroll_capsule::H);
}

/// **The scrolled capsule says where the rows came from**: the library, the catalog in force, the
/// count, the sort and the active filter. The sort was already there; a named catalog and a filter
/// are now part of what the reader is looking at, so they belong on the same capsule.
#[test]
fn the_scroll_capsule_names_the_library_catalog_sort_and_filter() {
    use plx_ui::screen::Screen;
    let _g = plx_base::testlock::serial();
    let mut fixture = super::tests::Fixture::new();
    fixture.listing = fixture.listing.clone()
        .with_catalogs(super::tests::test_catalogs(), Some(super::tests::TEST_CATALOGS[2].0))
        .with_unwatched(true);
    let mut page = fixture.screen();
    let pitch = page.layout.grid_pitch();
    page.scroll.jump(pitch);
    page.scroll_target = pitch;
    page.handoff.step(0.0, pitch, pitch, pitch, page.capsule_wanted());
    let runs = plx_ui::draw_census::capture_runs(|| {
        let cx = fixture.cx(None);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        page.draw(&mut frame);
    });
    assert!(runs.iter().any(|run| run.text == "Cinema · Netflix Movies"),
        "the library and the catalog in force: {:?}", runs.iter().map(|r| &r.text).collect::<Vec<_>>());
    assert!(runs.iter().any(|run| run.text.ends_with(" · Unwatched")),
        "the sort and the filter share the criterion run: {:?}", runs.iter().map(|r| &r.text).collect::<Vec<_>>());
}

/// The leading run is assembled in ONE place: the library's own name, a shared owner and the
/// catalog in force, in that order, with the capsule's dot between them.
#[test]
fn the_capsule_name_joins_library_owner_and_catalog_in_one_place() {
    assert_eq!(super::draw::capsule_words("Films", "", "", ""), "Films");
    assert_eq!(super::draw::capsule_words("Films", "", "", "Netflix Movies"), "Films · Netflix Movies");
    assert_eq!(super::draw::capsule_words("Films", "admin", "Cinema server", "Netflix Movies"),
        "Films admin · Cinema server · Netflix Movies");
}

/// The paints one descriptive draw of `page` declares (`frame::backdrop::discover`: primitives are
/// recorded, GL is never reached), as `popover_glass::tests::declared_all` records a menu's.
fn declared_by_a_draw(fixture: &super::tests::Fixture, page: &mut crate::library::LibraryScreen)
    -> Vec<std::rc::Rc<plx_ui::frame::backdrop::Paint>>
{
    use plx_ui::screen::Screen;
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = plx_ui::frame::backdrop::discover(sources.clone());
        let cx = fixture.cx(None);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        page.draw(&mut frame);
    }
    let paints = sources.borrow().paints.clone();
    paints
}

/// **The top fade is drawn**: a tucked page lays the 72 px ground gradient over the top edge, after
/// the grid and under the capsule, and a resting page draws none. The unit test of `draw_top_fade`
/// alone could not see a page that never called it, or called it under the grid.
#[test]
fn a_tucked_page_draws_the_72px_top_fade_over_the_grid_and_under_the_capsule_and_a_resting_page_none() {
    let _g = plx_base::testlock::serial();
    let fixture = super::tests::Fixture::new();
    let mut page = fixture.screen();
    // the fade is abutting full-width strips (the eased ramp, `scroll_capsule::fade_strips`)
    let is_fade = |p: &std::rc::Rc<plx_ui::frame::backdrop::Paint>| {
        let r = p.placed();
        p.kind() == "rect" && r[0] == 0.0 && r[2] == super::SCR_W && r[3] == scroll_capsule::FADE_STEP && r[1] < scroll_capsule::FADE_H
    };

    let at_rest = declared_by_a_draw(&fixture, &mut page);
    assert!(!at_rest.iter().any(is_fade), "no fade at the top of the page");

    let pitch = page.layout.grid_pitch();
    page.scroll.jump(pitch);
    page.scroll_target = pitch;
    page.handoff.step(0.0, pitch, pitch, pitch, page.capsule_wanted()); // a jump lands on the decided state
    let tucked = declared_by_a_draw(&fixture, &mut page);
    let at = tucked.iter().position(is_fade).expect("a tucked page draws the top fade");
    let strips: Vec<[f32; 4]> = tucked.iter().filter(|p| is_fade(p)).map(|p| p.placed()).collect();
    assert_eq!(strips.len(), (scroll_capsule::FADE_H / scroll_capsule::FADE_STEP) as usize, "exactly one fade: its strips down to FADE_H");
    assert_eq!(strips[0][1], 0.0, "from the very top edge");
    assert!(strips.windows(2).all(|w| w[1][1] == w[0][1] + w[0][3]), "abutting");
    assert_eq!(strips.last().map(|r| r[1] + r[3]), Some(scroll_capsule::FADE_H));
    assert!(tucked.iter().any(|p| p.kind() == "glass"), "a singleton keeps its library identity in the capsule");
    assert!(tucked[..at].iter().any(|p| p.placed()[1] > 0.0 && p.placed()[3] > 0.0),
        "and over the grid: the page's own paints come before it");
}
