// The library page header (redesign PR4): geometry, controls per kind, the focus table and the
// "focus never switches the library" rule. Included from `navigation_tests` for its `Fixture`.
use super::*;
use super::header::pack_names;
use crate::library::tests::{test_catalogs, TEST_CATALOGS};
use plx_machine::machine::{InputEvent, InputKind, Measure, Source};

fn key_press(key: Key) -> ScreenEvent<TestHost> {
    ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key { key, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false },
    })
}

/// A settled grid page (no hub shelves) of `libraries` libraries with the first row focused.
fn grid_page(libraries: usize) -> (Fixture, LibraryScreen, FocusEngine<u32>) {
    grid_page_with_catalogs(libraries, test_catalogs(), Some(TEST_CATALOGS[0].0))
}

/// A library page whose section answered `list` (with `current` in force) BEFORE it was first
/// synced — what a section that advertises exactly that looks like on arrival. Setting the answer
/// after the first sync is a different question: the control then holds for the library already on
/// the row, so it never drops (see `refresh_header`).
fn grid_page_with_catalogs(libraries: usize, list: Vec<plx_data::browse::CatalogEntry>, current: Option<&str>)
    -> (Fixture, LibraryScreen, FocusEngine<u32>) {
    let mut fixture = Fixture::new(libraries, 36, 0);
    fixture.listing = fixture.listing.clone().with_catalogs(list, current);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    page.sync(&fixture.cx(&engine));
    page.relayout(None);
    let first = page.key(page.pair.detail.elem_at(0).unwrap());
    engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    (fixture, page, engine)
}

fn at(engine: &FocusEngine<u32>) -> u32 { engine.current(OWNER).unwrap().elem }

/// Fork Rofocale11: the "…" library-options pill is gone. It is never a stop, never a row item,
/// never a data sink, and no library name is pushed with `usize::MAX` as a section index.
#[test]
fn the_library_options_pill_is_never_on_the_row() {
    let _guard = plx_base::testlock::serial();
    for count in [1, 2, 12] {
        let (_, page, _) = grid_page(count);
        assert!(!page.header_elems().iter().any(|elem| page.probe_header_stops()
            .iter().any(|(name, e)| *name == "more" && e == elem)));
        assert!(page.libraries.iter().all(|(_, section)| *section != usize::MAX));
        let row = page.header.row().unwrap();
        assert!(!row.page.spec().left.iter().any(|item| item.glyph == Some(plx_ui::icons::Icon::More)
            || item.a11y == Some("libraries")), "{count} libraries: an options mark is drawn");
    }
}

#[test]
fn a_non_favorite_gets_one_selected_temporary_pill_after_the_favorites() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new(4, 36, 0);
    fixture.sections[2].row.pinned = false;
    fixture.sections[3].row.pinned = false;
    fixture.publish(0, 36);
    let mut page = fixture.screen();
    let engine = FocusEngine::new();
    let favorite_rects: Vec<_> = page.libraries.iter()
        .map(|(elem, index)| (*index, page.header_rect(*elem, At::Drawn).unwrap())).collect();
    for current in [2, 3] {
        fixture.publish(current, 36);
        page.sync(&fixture.cx(&engine));
        let names: Vec<_> = page.libraries.iter().map(|(_, index)| *index).collect();
        assert_eq!(names, [0, 1, current], "one selected visit pill follows the fixed favorites");
        for (index, before) in &favorite_rects {
            let elem = page.libraries.iter().find(|(_, section)| section == index).unwrap().0;
            assert_eq!(page.header_rect(elem, At::Drawn).unwrap(), *before);
        }
        let items = &page.header.row().unwrap().page.spec().left;
        assert_eq!(items.iter().filter(|item| item.selected_plate).count(), 1);
        // Two favorites, then the temporary visit: no options stop after them any more.
        assert!(items[2].selected_plate);
        assert_eq!(page.layout.row_y(0, 0.0), layout::LIBRARY_GRID_TOP, "no extra identity row");
    }
    fixture.publish(1, 36);
    page.sync(&fixture.cx(&engine));
    assert_eq!(page.libraries.len(), 2);
}

#[test]
fn the_library_pill_shows_the_owner_only_if_the_full_credit_fits() {
    let measure = plx_base::fontcov::advances::ShippedMeasure;
    let title = "LDN Films";
    let owner = "by a friend";
    let full = toolbar::joined_name(title, owner);
    let width = |s| measure.width_str(s, plx_ui::theme::size::BODY, false);
    assert_eq!(header::fit_name(title, owner, width(&full), &measure), full);
    assert_eq!(header::fit_name(title, owner, width("LDN Films b…"), &measure), title);
    let short = header::fit_name(title, owner, width("LDN Fi…"), &measure);
    assert!(short.ends_with('…') && title.starts_with(short.trim_end_matches('…')));
}

#[test]
fn long_favorite_names_leave_room_for_the_catalogs_control_in_every_language() {
    let _guard = plx_base::testlock::serial();
    let measure = plx_base::fontcov::advances::ShippedMeasure;
    for language in plx_os::i18n::SHIPPED {
        let _language = plx_os::i18n::language_on_this_thread_for_test(language);
        let mut fixture = Fixture::new(3, 36, 0);
        for section in &mut fixture.sections {
            section.row.title = format!("{} — {}", section.row.title, "International cinema and restored classics ".repeat(5));
        }
        fixture.sections[2].row.pinned = false;
        fixture.publish(2, 36);
        let engine = FocusEngine::new();
        let cx = Cx { measure: &measure, ..fixture.cx(&engine) };
        let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
        page.sync(&cx);
        let row = page.header.row().unwrap();
        // Fork Rofocale11: with the options stop gone each name gets a wider budget, so a full
        // title may now fit; what must hold in every language is that every drawn name fits the
        // box it was placed in (a too-long one is elided by `fit_name`).
        let name_rects: Vec<_> = row.layout.items.iter()
            .filter(|rect| matches!(rect.slot, plx_ui::page_header::Slot::Left(i)
                if i < page.libraries.len()))
            .map(|rect| rect.text.w).collect();
        let names: Vec<_> = row.page.spec().left.iter().filter(|item|
            item.glyph.is_none() && !item.fold_glyph && !item.place).collect();
        assert!(!names.is_empty(), "{}: a frequent library remains directly reachable", language.tag());
        assert_eq!(names.len(), name_rects.len(), "{}: every drawn name has a box", language.tag());
        assert!(names.iter().zip(&name_rects).all(|(item, width)|
            measure.width_str(&item.label, plx_ui::theme::size::BODY, false) <= *width + 1.0),
            "{}: a drawn name does not fit its box", language.tag());
        let mut stops: Vec<_> = page.header_elems().into_iter().map(|elem| page.header_rect(elem, At::Drawn).unwrap()).collect();
        stops.sort_by(|a, b| a.x.total_cmp(&b.x));
        assert!(stops.windows(2).all(|pair| pair[0].x + pair[0].w <= pair[1].x), "{}: controls overlap", language.tag());
        assert!(page.toolbar_elems().contains(&CATALOGS), "{}: the catalogs control fits", language.tag());
        assert_eq!(names.len(), 3, "{}: both favorites and the active visit fit", language.tag());
        assert!(names[2].selected_plate);
    }
}

#[test]
fn the_temporary_library_pill_keeps_the_single_row_grid_geometry() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new(3, 36, 0);
    fixture.sections[2].row.pinned = false;
    fixture.publish(2, 36);
    let page = fixture.screen();
    assert_eq!(page.layout.row_y(0, 0.0), layout::LIBRARY_GRID_TOP);
    let selected = page.header.row().unwrap().page.spec().left.iter().filter(|item| item.selected_plate).count();
    assert_eq!(selected, 1);
}

#[test]
fn the_header_row_stands_at_148_and_the_grid_starts_at_244() {
    let _guard = plx_base::testlock::serial();
    let (fixture, page, engine) = grid_page(2);
    let cx = fixture.cx(&engine);
    let control = <LibraryScreen as Focusable<TestHost>>::place(&page, &SORTFILTER, &cx, At::Drawn).unwrap().rect;
    assert_eq!(control.y + control.h * 0.5, layout::HEADER_Y + plx_ui::page_header::HEIGHT * 0.5);
    assert_eq!(layout::HEADER_Y, 148.0);
    assert_eq!(layout::LIBRARY_GRID_TOP, 244.0);
    assert_eq!(page.layout.row_y(0, 0.0), layout::LIBRARY_GRID_TOP, "the first poster row's top sits 96 under the header row's top");
    let tile = <LibraryScreen as Focusable<TestHost>>::place(&page, &page.pair.detail.elem_at(0).unwrap(), &cx, At::Drawn).unwrap().rect;
    assert!(tile.y <= layout::LIBRARY_GRID_TOP && tile.y + tile.h > layout::LIBRARY_GRID_TOP, "the first tile stands on that row");
}

/// **The Catalogs control is on the row only when the section advertised catalogs**, and no
/// "Collections" label is drawn anywhere: the fabricated count control was deleted with the probe.
#[test]
fn the_catalogs_control_is_on_the_row_only_when_the_section_advertises_catalogs() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, engine) = grid_page_with_catalogs(1, Vec::new(), None);
    assert!(!page.catalogs, "premise: the section advertised none");
    assert_eq!(page.toolbar_elems(), [RECOMMENDED, SORTFILTER]);
    let drawn = |page: &LibraryScreen| {
        let cx = fixture.cx(&engine);
        let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        plx_ui::draw_census::capture_runs(|| page.draw_header(&frame))
    };
    // Fork Rofocale11: the fabricated Collections count control is gone; the ordinary
    // Collections word still exists for the Collections page's crumb, but not on this row.
    assert!(!drawn(&page).iter().any(|run| run.text == plx_os::i18n::msg::browse_kind_collections()));
    assert!(!page.header.row().unwrap().page.spec().left.iter().any(|item|
        item.label == plx_os::i18n::msg::browse_kind_collections()));
    // The control joins the row when this same library's catalogs land.
    fixture.listing = fixture.listing.clone().with_catalogs(test_catalogs(), Some(TEST_CATALOGS[0].0));
    page.sync(&fixture.cx(&engine));
    assert!(page.catalogs);
    assert_eq!(page.toolbar_elems(), [CATALOGS, RECOMMENDED, SORTFILTER]);
    let control = page.header.row().unwrap().page.spec().left.iter()
        .find(|item| item.label == plx_os::i18n::msg::library_header_catalogs()).cloned().unwrap();
    assert_eq!(control.value.as_deref(), Some(TEST_CATALOGS[0].1), "the active catalog is named");
}

/// **One rule spaces each cluster of the row** whatever the catalog in force: adjacent items in
/// the LEFT cluster (names, then Catalogs and Recommended) and in the RIGHT cluster (read-out,
/// divider, sort) stand one `GAP` apart. Fork Rofocale11: the two clusters are deliberately far
/// apart — the left one sits over the first poster, the right one at the text edge — so the
/// cross-cluster gap is large and is NOT the row's gap rule.
#[test]
fn every_gap_within_a_cluster_is_one_gap_whatever_the_catalog_is() {
    use plx_ui::page_header::{Slot, CLUSTER_GAP, GAP};
    let _guard = plx_base::testlock::serial();
    for (index, title) in TEST_CATALOGS.iter().enumerate() {
        let (_, page, _) = grid_page_with_catalogs(2, test_catalogs(), Some(title.0));
        let row = page.header.row().unwrap();
        let left: Vec<_> = row.layout.items.iter()
            .filter(|rect| matches!(rect.slot, Slot::Left(_))).collect();
        for pair in left.windows(2) {
            let gap = pair[1].text.x - (pair[0].text.x + pair[0].text.w);
            assert!((gap - GAP).abs() < 0.02, "{}: left {:?} -> {:?} is {gap} apart, not {GAP}",
                title.1, pair[0].slot, pair[1].slot);
        }
        let right: Vec<_> = row.layout.items.iter()
            .filter(|rect| matches!(rect.slot, Slot::Right(_))).collect();
        // The divider is a visual hairline in its own right, not a text stop: measure the two
        // text-bearing items (read-out if any, then the sort control) as the cluster's spacing.
        let text_right: Vec<_> = right.iter().filter(|rect| match rect.slot {
            Slot::Right(i) => !row.page.spec().right[i].divider && rect.text.w > 0.0,
            _ => false,
        }).collect();
        for pair in text_right.windows(2) {
            let gap = pair[1].text.x - (pair[0].text.x + pair[0].text.w);
            assert!(gap >= GAP - 0.02, "{}: right {:?} -> {:?} is {gap} apart, less than {GAP}",
                title.1, pair[0].slot, pair[1].slot);
        }
        let between = right.first().unwrap().text.x - (left.last().unwrap().text.x + left.last().unwrap().text.w);
        assert!(between >= CLUSTER_GAP, "{}: the clusters are separated by the board's free space: {between}", title.1);
        assert_eq!(page.toolbar_elems().len(), 3, "catalog {index} keeps the same controls");
    }
}

/// **The row is declared**: Catalogs (a fold glyph) and Recommended (a place) stand on the left;
/// the divider is no stop and the sort control is its icon + criterion + down chevron whose name
/// ("Sort & Filter") is carried and not drawn. No Collections stop survives.
#[test]
fn the_row_declares_catalogs_recommended_a_divider_and_a_value_only_sort_control() {
    use plx_ui::page_header::Slot;
    let _guard = plx_base::testlock::serial();
    let (_, page, _) = grid_page(2);
    let row = page.header.row().unwrap();
    let left = &row.page.spec().left;
    let catalogs = left.iter().find(|item| item.label == plx_os::i18n::msg::library_header_catalogs()).unwrap();
    assert!(catalogs.fold_glyph && !catalogs.place, "Catalogs opens a menu, not a page");
    assert_eq!(catalogs.value.as_deref(), Some(TEST_CATALOGS[0].1));
    assert_eq!(catalogs.a11y, Some("catalogs"));
    let right = &row.page.spec().right;
    assert_eq!(right.iter().map(|it| (it.place, it.divider)).collect::<Vec<_>>(),
        [(false, true), (false, false)], "divider, sort");
    let divider = row.layout.items.iter().find(|r| r.slot == Slot::Right(0)).unwrap();
    assert!(!divider.focusable && !right[0].focusable, "the divider is no stop");
    let sort = &right[1];
    assert_eq!(sort.label, plx_os::i18n::msg::library_header_sort_filter(), "its name stays 'Sort & Filter'");
    assert!(sort.value_only && sort.fold_glyph && sort.fold_quiet && sort.glyph == Some(plx_ui::icons::Icon::Sort));
    let elems: Vec<u32> = row.stops.iter().map(|(elem, _)| *elem).collect();
    assert_eq!(&elems[elems.len() - 3..], [CATALOGS, RECOMMENDED, SORTFILTER]);
    assert_eq!(&elems[elems.len() - 3..], page.toolbar_elems());
    assert!(!elems.contains(&toolbar::CRUMB_ROOT));
}

/// **No reserved slot for the criterion, in any shipped language**: the longest criterion every
/// language has neither truncates nor moves the library names, which are packed against the room
/// the WIDEST criterion leaves, so a short value ("Year") beside them leaves no hole and a long one
/// ("Last episode added" in Russian) does not push them.
#[test]
fn a_criterion_in_every_language_takes_its_own_width_and_never_moves_the_names() {
    use plx_ui::page_header::{Slot, TEXT_X1};
    let _guard = plx_base::testlock::serial();
    let (fixture, page, engine) = grid_page(2);
    let cx = fixture.cx(&engine);
    let criteria = ["Year", "Date added", "Последняя добавленная серия", "Zuletzt hinzugefügte Folge", "Aggiunti di recente al catalogo"];
    let (right, mut widest) = page.header_right(&cx);
    // The page's own widest case holds the longest of the LISTING's sorts; these criteria are the
    // longest a language has, so the widest case is rebuilt around the longest of them, as it is
    // for a listing that declares them.
    let width = |text: &str| plx_ui::page_header::cluster_width(&[plx_ui::page_header::HeaderItem::sort_control("", text)], cx.measure);
    let longest = criteria.iter().copied().max_by(|a, b| width(a).total_cmp(&width(b))).unwrap();
    let last_widest = widest.len() - 1;
    widest[last_widest] = plx_ui::page_header::HeaderItem::sort_control(widest[last_widest].label.clone(), longest);
    let names_budget = |items: &[plx_ui::page_header::HeaderItem]| {
        plx_ui::page_header::PageHeader::new(
            plx_ui::page_header::PageHeaderSpec { right: items.to_vec(), ..Default::default() }, &widest, cx.measure)
            .layout(plx_ui::consts::SCR_W, cx.measure)
    };
    let base = names_budget(&right);
    for criterion in criteria {
        let mut items = right.clone();
        let last = items.len() - 1;
        items[last] = plx_ui::page_header::HeaderItem::sort_control(items[last].label.clone(), criterion);
        let placed = names_budget(&items);
        assert_eq!(placed.left_budget, base.left_budget, "{criterion}: the names' room does not move");
        let sort = placed.items.iter().find(|r| r.slot == Slot::Right(last)).unwrap().text;
        assert!((sort.x + sort.w - TEXT_X1).abs() < 0.01, "{criterion}: the control ends at the text edge");
        let width = plx_ui::page_header::cluster_width(&[items[last].clone()], cx.measure);
        assert!((sort.w - width).abs() < 0.01, "{criterion}: the control is as wide as what it draws, with no slot");
    }
}

#[test]
fn favorite_names_are_shown_even_for_one_library() {
    let _guard = plx_base::testlock::serial();
    assert_eq!(grid_page(1).1.libraries.len(), 1, "one favorite");
    assert_eq!(grid_page(2).1.libraries.len(), 2);
}

#[test]
fn the_favorite_prefix_never_reorders_when_an_overflow_library_is_picked() {
    let widths = [200.0, 180.0, 220.0, 160.0, 240.0];
    assert_eq!(pack_names(&widths, 4000.0, 8), [0, 1, 2, 3, 4], "everything fits: all names, in order");
    let gap = plx_ui::page_header::GAP;
    let tight = 200.0 + gap + 180.0 + gap + 220.0 + 1.0;
    let cut = pack_names(&widths, tight, 8);
    assert_eq!(cut, [0, 1, 2], "a hidden pick never replaces a shortcut");
    assert_eq!(cut.len(), 3);
    assert!(pack_names(&widths, 10.0, 8).is_empty(), "a tiny budget fits no name");
    assert!(pack_names(&[], 100.0, 8).is_empty());
}

#[test]
fn focus_on_a_library_name_never_switches_the_library() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(3);
    let other = page.libraries.iter().find(|(_, index)| Some(*index) != page.view_section(&fixture.cx(&engine))).unwrap().0;
    let from = engine.current(OWNER);
    engine.set(OWNER, page.key(other), Some(LIBRARY_GROUP), By::Dir);
    let to = page.key(other);
    let apps = fixture.step(&mut page, &mut engine, ScreenEvent::FocusMoved { from, to, by: By::Dir });
    assert!(page.pending.section().is_none(), "landing on a name requests nothing");
    assert!(!page.switched);
    assert!(apps.iter().all(|fx| !matches!(fx, AppFx::Library(LibraryReq::Menu { .. }))));
    fixture.step(&mut page, &mut engine, ScreenEvent::Activate(other));
    assert!(page.pending.section().is_some(), "OK switches");
    assert!(page.switched);
}

#[test]
fn up_lands_on_the_selected_library_then_on_the_last_control_used() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let selected = page.libraries.iter().find(|(_, index)| Some(*index) == page.view_section(&fixture.cx(&engine))).unwrap().0;
    fixture.direction(&mut page, &mut engine, Dir::Up);
    assert_eq!(at(&engine), selected, "the first Up lands on the library on view");
    fixture.direction(&mut page, &mut engine, Dir::Right);
    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(at(&engine), CATALOGS, "Right walks the second name, then the left controls");
    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(at(&engine), RECOMMENDED);
    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(at(&engine), SORTFILTER, "the right cluster is the sort control alone");
    fixture.direction(&mut page, &mut engine, Dir::Down);
    assert!(region_of_elem(at(&engine)).is_none() || !page.is_header_elem(at(&engine)), "Down leaves the header for the grid");
    fixture.direction(&mut page, &mut engine, Dir::Up);
    assert_eq!(at(&engine), SORTFILTER, "after that, Up returns to the last control used");
}

#[test]
fn back_walks_grid_to_header_to_nav_to_home_without_changing_the_library() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let library = page.view_section(&fixture.cx(&engine));
    // Deep in the grid: to the top of the page, still on the grid.
    page.scroll_target = 900.0;
    page.scroll.jump(900.0);
    fixture.step(&mut page, &mut engine, key_press(Key::Back));
    assert_eq!(page.scroll_target, 0.0, "deep grid goes to the top of the page");
    // Top of the page: to the header.
    fixture.step(&mut page, &mut engine, key_press(Key::Back));
    assert!(page.is_header_elem(at(&engine)), "the top of the page goes to the header");
    // Header: to the nav.
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&key_press(Key::Back), &fixture.cx(&engine),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|e| matches!(&e.fx,
        Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(STRIP) }))))),
        "the header goes to the nav");
    assert!(page.pending.section().is_none() && !page.switched, "Back never changes the library");
    assert_eq!(page.view_section(&fixture.cx(&engine)), library);
}

#[test]
fn back_from_the_nav_leaves_for_home() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    // The nav holds focus: a strip key, which carries THIS page's entry (an empty engine, where
    // nothing of the page holds focus, is a different case and passed on a fiction).
    let pill = FocusKey { entry: page.entry, elem: plx_ui::widgets::TAB_KEY_MOVIES };
    engine.set(OWNER, pill, Some(STRIP), By::Dir);
    assert!(pill.elem >= plx_ui::dispatch::STRIP_BASE && engine.current(OWNER) == Some(pill));
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&key_press(Key::Back), &fixture.cx(&engine),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Library(LibraryReq::BackToHome { .. })))),
        "BACK on a nav pill leaves for Home; it does not seat the header again");
    assert!(!out.iter().any(|e| matches!(&e.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(_))))));
}

/// The right cluster in its widest case leaves the names at least 40% of the row in every shipped
/// language (measured with the device's whole-pixel advances).
#[test]
fn the_header_cluster_leaves_the_names_room_in_every_shipped_language() {
    use plx_os::i18n::{language_on_this_thread_for_test, SHIPPED};
    let _guard = plx_base::testlock::serial();
    let (fixture, page, engine) = grid_page(1);
    let measure = plx_base::fontcov::advances::ShippedMeasure;
    let mut cx = fixture.cx(&engine);
    cx.measure = &measure;
    let mut out = Vec::new();
    for language in SHIPPED {
        let _lang = language_on_this_thread_for_test(language);
        let (_, widest) = page.header_right(&cx);
        let width = plx_ui::page_header::cluster_width(&widest, &measure);
        let row = plx_ui::page_header::TEXT_X1 - plx_ui::page_header::TEXT_X0;
        if width > row * 0.6 {
            out.push(format!("{}: right cluster {width:.0}px of a {row:.0}px row", language.tag()));
        }
    }
    assert!(out.is_empty(), "{}", out.join("\n"));
}

/// **The widest catalog title reserves a stable budget** before the library names are packed, so
/// a catalog title much longer than the one in force cannot repack the names. The live value is
/// the one in force (by design the neighbouring left control stands by what it draws, so it can
/// shift with a shorter title); the widest is reserved only for the names' budget.
#[test]
fn the_widest_catalog_title_reserves_a_stable_budget() {
    let _guard = plx_base::testlock::serial();
    let long = "Streaming Catalogs — the very longest catalog the addon could advertise";
    for (active, title) in [(TEST_CATALOGS[0].0, TEST_CATALOGS[0].1), (long, long)] {
        let mut catalogs = test_catalogs();
        catalogs.push(plx_data::browse::CatalogEntry { key: long.into(), title: long.into(), active: false });
        let (fixture, page, engine) = grid_page_with_catalogs(3, catalogs, Some(active));
        let (left, widest) = page.header_left(&fixture.cx(&engine));
        assert_eq!(left.len(), 2, "Catalogs then Recommended");
        assert_eq!(widest[0].value.as_deref(), Some(long), "the widest title is reserved");
        assert_eq!(left[0].value.as_deref(), Some(title), "the active title is drawn");
    }
}

/// **The names are packed against a budget that does not depend on which catalog is in force**:
/// the titles the section advertises reserve the room once, and a pick that changes the drawn title
/// (or a sort the page reloads under it) leaves the names and their fold exactly where they were.
#[test]
fn the_names_do_not_repack_when_the_catalog_title_changes() {
    let _guard = plx_base::testlock::serial();
    let packed = |list, current| {
        let (_, page, _) = grid_page_with_catalogs(12, list, current);
        let row = page.header.row().unwrap();
        (row.layout.left_budget, page.libraries.clone(), page.folded.clone())
    };
    let base = packed(test_catalogs(), Some(TEST_CATALOGS[0].0));
    assert!(!base.2.is_empty(), "premise: twelve libraries fold");
    for (what, index) in [("the middle", 1), ("the longest", 2)] {
        let after = packed(test_catalogs(), Some(TEST_CATALOGS[index].0));
        assert_eq!(after.0, base.0, "{what}: the room the names are packed into moved");
        assert_eq!(after.1, base.1, "{what}: the names that fit changed");
        assert_eq!(after.2, base.2, "{what}: the fold changed");
    }
    let none = packed(Vec::new(), None);
    assert!(none.1.len() >= base.1.len(), "a section with no catalogs gets no control to reserve for");
}

/// **A named catalog survives a sort**: the grid's catalog read is the one the menu checked, not
/// whatever the first page of the re-sorted listing happens to be.
#[test]
fn a_named_catalog_survives_a_sort() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, engine) = grid_page_with_catalogs(1, test_catalogs(), Some(TEST_CATALOGS[2].0));
    assert_eq!(page.current_catalog(&fixture.cx(&engine)).as_deref(), Some(TEST_CATALOGS[2].1));
    fixture.listing = fixture.listing.clone().with_sort_key("addedAt").with_query(2)
        .with_catalogs(test_catalogs(), Some(TEST_CATALOGS[2].0));
    page.sync(&fixture.cx(&engine));
    assert_eq!(page.current_catalog(&fixture.cx(&engine)).as_deref(), Some(TEST_CATALOGS[2].1),
        "the sort reorders inside the named catalog; it does not re-point it");
}

/// The fold the left group makes must not depend on which header stop holds focus. Fork Rofocale11:
/// there is no options stop to stay visible; the NAMES stay put instead.
#[test]
fn the_fold_stays_wherever_focus_is_in_the_row() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(12);
    let before = page.libraries.clone();
    fixture.direction(&mut page, &mut engine, Dir::Up);
    for _ in 0..16 {
        fixture.direction(&mut page, &mut engine, Dir::Right);
        page.sync(&fixture.cx(&engine));
        assert_eq!(page.libraries, before, "focus on {} changed the fold", at(&engine));
    }
}

#[test]
fn a_one_library_row_stands_at_y_158_and_a_multi_library_row_at_148() {
    let _guard = plx_base::testlock::serial();
    for (libraries, top) in [(1, 148.0), (2, 148.0)] {
        let (fixture, page, engine) = grid_page(libraries);
        let cx = fixture.cx(&engine);
        let control = <LibraryScreen as Focusable<TestHost>>::place(&page, &SORTFILTER, &cx, At::Drawn).unwrap().rect;
        assert_eq!(control.y, top, "{libraries} libraries");
    }
}

#[test]
fn the_fold_holds_exactly_the_libraries_that_did_not_fit() {
    let _guard = plx_base::testlock::serial();
    let (fixture, page, engine) = grid_page(12);
    let shown: Vec<u32> = page.libraries.iter().map(|(_, index)| *index as u32).collect();
    let current = page.view_section(&fixture.cx(&engine)).unwrap() as u32;
    assert!(!page.folded.is_empty());
    assert_eq!(page.folded.len() + shown.len(), 12, "every library is either a name or folded");
    assert!(page.folded.iter().all(|index| !shown.contains(index)), "no visible name is repeated in the fold");
    assert!(!page.folded.contains(&current), "nor the library on view");
}

/// **A control a menu has lifted wears no pill on the page** (the opener lift draws it, lit); every
/// other focused control still does.
#[test]
fn a_control_its_menu_has_lifted_draws_no_focus_pill() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let key = page.key(SORTFILTER);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    // The pill is the row's motion: it fades in under the focused control over a few frames.
    for frame in 1..=60 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
    let pill = plx_ui::widgets::focus_pill(page.header.row().unwrap().plate(SORTFILTER).unwrap());
    let drawn = |page: &LibraryScreen, lifted: Option<FocusKey<u32>>| {
        let cx = fixture.cx(&engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        frame.lifted = lifted;
        plx_ui::draw_census::capture(|| page.draw_header(&frame)).iter()
            .filter(|(_, rect)| (rect.w - pill.w).abs() < 0.5 && (rect.h - pill.h).abs() < 0.5).count()
    };
    assert_eq!(drawn(&page, None), 1, "a focused control wears the pill");
    assert_eq!(drawn(&page, Some(key)), 0, "its menu lifted it: no plate under the scrim");
    page.scroll.jump(0.0);
}

/// **Every header opener is painted lit by the lift while its menu owns it** (design review S12: one
/// rule; the glass then consumes it at the morph's `opener_alpha`). Sort &
/// Filter and `+N` are both lifted out of the page's pass (no capsule, no label: the page draws
/// none of the control) and the opener lift (`Screen::redraw_focused`) paints each as the focused
/// control, the accent pill at the focused extent and every run of the face in the focused ink,
/// above the scrim. Fork Rofocale11: Sort & Filter and Catalogs are both header menus; the old
/// options-pill opener no longer exists.
#[test]
fn every_header_opener_is_lit_by_the_lift_while_its_menu_is_open() {
    use plx_ui::screen::Screen;
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(12);
    for elem in [SORTFILTER, CATALOGS] {
        let key = page.key(elem);
        engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
        for frame in 1..=60 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
        let plate = page.header.row().unwrap().plate(elem).expect("the opener is a stop of the row");
        let pill = plx_ui::widgets::focus_pill(plate);
        let words = |rect: plx_ui::Rect| rect.cx() > plate.x && rect.cx() < plate.x + plate.w && rect.cy() > plate.y && rect.cy() < plate.y + plate.h;

        // the page's pass, with the opener lifted out of it, draws none of the control
        let cx = fixture.cx(&engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        frame.lifted = Some(key);
        let left = plx_ui::draw_census::capture_runs(|| page.draw_header(&frame));
        assert!(!left.iter().any(|run| words(run.at)), "elem {elem}: the page left words on the opener: {left:?}");

        // the lift paints it: one accent pill, the face in the focused ink
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        let mut pills = Vec::new();
        let runs = plx_ui::draw_census::capture_runs(|| {
            pills = plx_ui::draw_census::capture(|| Screen::<TestHost>::redraw_focused(&page, &mut frame, Some(key)));
        });
        let rects: Vec<_> = pills.iter().filter(|(tag, _)| *tag == 2).map(|(_, r)| *r).collect();
        assert!(rects.len() == 1 && (rects[0].x - pill.x).abs() < 0.5 && (rects[0].w - pill.w).abs() < 0.5 && (rects[0].h - pill.h).abs() < 0.5,
            "elem {elem}: one accent pill at {pill:?}, got {rects:?}");
        assert!(!runs.is_empty() && runs.iter().all(|run| words(run.at)
            && run.ink.iter().zip(plx_ui::ACCENT_INK).all(|(a, b)| (a - b).abs() < 0.01)),
            "elem {elem}: the face is on the pill, in the focused ink: {runs:?}");
    }
}

/// **A sort picked from the menu is seen through the lift** (the press path: the menu's pick pushes
/// the `LibraryEdit` and dismisses itself on the same frame, so the opener lift is the only thing
/// that draws the sort control until the menu is gone). The existing swap tests draw the page's
/// pass with `lifted = None`, which is how the lift painted the NEW criterion whole from its first
/// frame while the places slid underneath and then popped when it stood down. Driven through real
/// ticks and the lift (`Screen::redraw_focused`): the old criterion starts the swap and falls as the
/// new one rises, and the settled lift draws the new one alone.
#[test]
fn a_picked_sort_swaps_its_criterion_through_the_opener_lift() {
    use plx_os::i18n::msg;
    use plx_ui::screen::Screen;
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, mut engine) = grid_page(2);
    let key = page.key(SORTFILTER);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    let mut ms = 0;
    for _ in 0..60 { ms += 16; fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 })); }
    let (old, new) = (msg::sort_facts_title(), msg::sort_facts_date_added());
    // What the pick publishes: the same section under a new query, sorted by Date added.
    fixture.listing = fixture.listing.clone().with_sort_key("addedAt").with_query(2);
    // `(opacity of the old criterion, of the new one)` as the lift draws them now.
    let lifted = |page: &LibraryScreen, engine: &FocusEngine<u32>| {
        let cx = fixture.cx(engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        let runs = plx_ui::draw_census::capture_runs(|| Screen::<TestHost>::redraw_focused(page, &mut frame, Some(key)));
        let opacity = |text: &str| runs.iter().find(|run| run.text == text).map_or(0.0, |run| run.ink[3]);
        (opacity(old), opacity(new))
    };
    let mut last: Option<(f32, f32)> = None;
    for frame in 0..240 {
        ms += 16;
        fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
        if frame == 0 { assert!(page.header_swap.is_sliding(), "premise: the pick began a criterion swap"); }
        let (out, arriving) = lifted(&page, &engine);
        assert!((out + arriving - 1.0).abs() < 1e-3, "frame {frame}: the lift draws the criteria at {out} + {arriving}, which is not one");
        if frame == 0 { assert!(out > 0.9 && arriving < 0.1, "frame {frame}: the lift starts on the OLD criterion: {out}, {arriving}"); }
        if let Some((was_out, was_arriving)) = last {
            assert!(out <= was_out + 1e-3 && arriving >= was_arriving - 1e-3, "frame {frame}: the cross-fade runs one way");
        }
        last = Some((out, arriving));
        if !page.header_swap.is_active() { break; }
    }
    assert!(!page.header_swap.is_active(), "the swap landed");
    assert_eq!(lifted(&page, &engine), (0.0, 1.0), "the settled lift draws the new criterion alone");
}

/// The sorts the read-out tests' listing offers: the first is the one in force.
const READOUT_SORTS: [(&str, &str); 2] = [("titleSort", "Title"), ("addedAt", "Date added")];

/// A settled two-library page of 36 films whose listing the Unwatched filter then narrows to 12
/// (the unfiltered total is learnt first, as the real page learns it): the row carries its
/// "12 of 36" read-out. Returns the next tick's time too.
fn filtered_page() -> (Fixture, LibraryScreen, FocusEngine<u32>, u32) {
    let (mut fixture, mut page, mut engine) = grid_page(2);
    fixture.listing = fixture.listing.clone().with_sorts(&READOUT_SORTS, 0);
    let mut ms = 0;
    let mut settle = |fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, frames: u32| {
        for _ in 0..frames { ms += 16; fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 })); }
    };
    settle(&fixture, &mut page, &mut engine, 30);
    assert!(page.header.row().is_some_and(|row| row.readout().is_none()), "premise: unfiltered, no read-out");
    fixture.listing = fixture.listing.clone().with_unwatched(true).with_query(2).with_total(12);
    settle(&fixture, &mut page, &mut engine, 120);
    assert!(page.header.row().is_some_and(|row| row.readout().is_some()), "premise: the filter puts the read-out on the row");
    (fixture, page, engine, ms)
}

/// What the header row draws now.
fn drawn_header(fixture: &Fixture, page: &LibraryScreen, engine: &FocusEngine<u32>) -> Vec<plx_ui::draw_census::Run> {
    let cx = fixture.cx(engine);
    let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
    plx_ui::draw_census::capture_runs(|| page.draw_header(&frame))
}

/// A query edit as the menu delivers it, to the listing the page shows.
fn pick(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, edit: plx_data::stores::browse::QueryEdit) {
    let id = fixture.listing.view().id().unwrap();
    let target = SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
    fixture.step(page, engine, ScreenEvent::App(AppMsg::LibraryEdit { target, edit }));
}

fn commits(effects: &[AppFx]) -> bool {
    effects.iter().any(|fx| matches!(fx, AppFx::Store(_, StoreCmd::Browse(BrowseCmd::Addressed { work: LibraryWork::Commit { .. }, .. }))))
}

/// **A sort picked under a filter keeps the "12 of 36" read-out on the row and gliding with its
/// neighbours through the reload.** The pick fades the grid out, then commits: the store forgets
/// the listing (`Loading`, total -1) under the SAME filter terms, and the read-out cannot be read
/// off a listing that has no total. It used to leave the row there, ~4 frames into the criterion's
/// slide (it left through the swap's leaving face at the previous row's x) and fade back in when the
/// listing landed, beside neighbours that travelled on the slide's spring. Driven through the real
/// ticks, the real commit effect and a `Loading` listing, then the landing: on every frame the
/// read-out is drawn ONCE at its resting opacity and glides monotonically from its old x to the
/// new one. Fork Rofocale11: the divider is NOT a peer — it is laid out at once from the new
/// criterion's width, whereas the read-out text travels on the row's slide. The left controls no
/// longer share that slide either.
#[test]
fn the_filter_readout_stays_and_glides_while_a_picked_sort_reloads_the_listing() {
    use plx_data::stores::browse::QueryEdit;
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, mut engine, mut ms) = filtered_page();
    let readout = page.header.row().and_then(|row| row.readout()).unwrap().to_owned();
    let x_of = |runs: &[plx_ui::draw_census::Run], text: &str, frame: &str| {
        runs.iter().find(|run| run.text == text).unwrap_or_else(|| panic!("{frame}: {text} is drawn")).at.x
    };
    let rest = drawn_header(&fixture, &page, &engine);
    let from = x_of(&rest, &readout, "at rest");
    let ink = plx_ui::theme::COUNT_FILTERED_INK[3];

    pick(&fixture, &mut page, &mut engine, QueryEdit::Sort { key: "addedAt".into(), desc: false });
    let reloading = fixture.listing.clone().with_sorts(&READOUT_SORTS, 1).with_query(3).with_total(0).with_fetch(SecFetch::Loading, -1);
    let landed = fixture.listing.clone().with_sorts(&READOUT_SORTS, 1).with_query(3).with_total(12).with_fetch(SecFetch::Ready, 12);
    let (mut committed, mut positions) = (None, Vec::new());
    for frame in 0..240u32 {
        ms += 16;
        let effects = fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
        if committed.is_none() && commits(&effects) {
            committed = Some(frame);
            // `browse::requery`: the same section under the new sort, forgotten and loading.
            fixture.listing = reloading.clone();
        }
        if committed.is_some_and(|at| frame == at + 8) { fixture.listing = landed.clone(); }
        let runs = drawn_header(&fixture, &page, &engine);
        let said: Vec<_> = runs.iter().filter(|run| run.text == readout).collect();
        assert_eq!(said.len(), 1, "frame {frame} (committed at {committed:?}): the read-out is drawn once: {said:?}");
        assert!((said[0].ink[3] - ink).abs() < 1e-4, "frame {frame}: at its resting opacity, not fading: {}", said[0].ink[3]);
        let label = format!("frame {frame}");
        positions.push(x_of(&runs, &readout, &label));
        if committed.is_some_and(|at| frame > at + 8) && !page.header_swap.is_active() { break; }
    }
    assert!(committed.is_some(), "premise: the pick committed");
    assert!(!page.header_swap.is_active(), "the swap landed");
    let to = *positions.last().unwrap();
    assert!((to - from).abs() > 1.0, "premise: the new criterion moves the read-out ({from} to {to})");
    let first_progress = (positions[0] - from) / (to - from);
    assert!(first_progress >= -0.01 && first_progress < 0.1,
        "the read-out starts at the old x ({from}), not cut to the new one ({to}): {first_progress}");
    let last = *positions.last().unwrap();
    assert!(((last - from) / (to - from)) > 0.99, "the read-out arrived at the new criterion's x: {last}");
}

/// **A held read-out belongs to its library and its filter terms.** The count the read-out keeps
/// across a reload (above) is dropped the moment the reload is for other terms: with the filter
/// turned off the read-out is gone, and a filter turned on again does not bring the old count back
/// while its own listing loads; and it is gone for good when the listing lands unfiltered.
#[test]
fn a_held_readout_is_dropped_when_the_filter_terms_change() {
    use plx_data::stores::browse::QueryEdit;
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, mut engine, mut ms) = filtered_page();
    let readout = |page: &LibraryScreen| page.header.row().and_then(|row| row.readout()).map(str::to_owned);
    let tick = |fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: &mut u32, frames: u32| {
        for _ in 0..frames { *ms += 16; fixture.step(page, engine, ScreenEvent::Tick(Tick { ms: *ms, dt_us: 16_667 })); }
    };
    let held = readout(&page).expect("premise: 12 of 36");
    // The same terms reloading (a sort pick's requery) keep it.
    let base = fixture.listing.clone();
    fixture.listing = base.clone().with_query(3).with_total(0).with_fetch(SecFetch::Loading, -1);
    tick(&fixture, &mut page, &mut engine, &mut ms, 10);
    assert_eq!(readout(&page), Some(held.clone()), "the same terms, reloading: the read-out holds");
    // The filter is turned off: its requery is for other terms, and nothing is held.
    pick(&fixture, &mut page, &mut engine, QueryEdit::Unwatched(false));
    fixture.listing = base.clone().with_unwatched(false).with_query(4).with_total(0).with_fetch(SecFetch::Loading, -1);
    tick(&fixture, &mut page, &mut engine, &mut ms, 60);
    assert_eq!(readout(&page), None, "the filter is off: no read-out while the listing loads");
    // The filter is put back: ITS listing is loading, and the count of the one before is not shown.
    fixture.listing = base.clone().with_unwatched(true).with_query(5).with_total(0).with_fetch(SecFetch::Loading, -1);
    tick(&fixture, &mut page, &mut engine, &mut ms, 60);
    assert_eq!(readout(&page), None, "a filter turned on again shows no count until its own listing lands");
    // And it lands: the read-out is the new listing's own.
    fixture.listing = base.with_query(5).with_total(9).with_fetch(SecFetch::Ready, 9);
    tick(&fixture, &mut page, &mut engine, &mut ms, 60);
    let landed = readout(&page).expect("the landed listing carries its read-out");
    assert_ne!(landed, held, "and it counts the landed listing");
}

/// The kinds one descriptive draw of `page`, scrolled one grid pitch (the chrome fully tucked),
/// declares: a capsule is a `glass` paint (the nav pill's live material).
fn tucked_draw_kinds(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>) -> Vec<(String, [f32; 4])> {
    let pitch = page.layout.grid_pitch();
    draw_kinds_at(fixture, page, engine, pitch)
}

/// The kinds one descriptive draw of `page` declares with the page put at exactly `scroll` (a
/// jump: the scroll and its target are there, and the handoff lands on that state at once).
fn draw_kinds_at(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>, scroll: f32) -> Vec<(String, [f32; 4])> {
    page.scroll.jump(scroll);
    page.scroll_target = scroll;
    let worn = page.capsule_wanted();
    page.handoff.step(0.0, scroll, scroll, page.layout.grid_pitch(), worn);
    draw_kinds_now(fixture, page, engine)
}

/// The kinds one descriptive draw of `page` declares, the page left as it is.
fn draw_kinds_now(fixture: &Fixture, page: &mut LibraryScreen, engine: &FocusEngine<u32>) -> Vec<(String, [f32; 4])> {
    use plx_ui::screen::Screen;
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    {
        let _walk = plx_ui::frame::backdrop::discover(sources.clone());
        let cx = fixture.cx(engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        page.draw(&mut frame);
    }
    let paints = sources.borrow().paints.iter().map(|p| (p.kind().to_owned(), p.placed())).collect();
    paints
}

#[test]
fn a_scrolled_page_wears_the_capsule_only_when_its_header_names_libraries() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut one, engine) = grid_page(1);
    let kinds = tucked_draw_kinds(&fixture, &mut one, &engine);
    assert!(kinds.iter().any(|(k, r)| k == "rect" && r[0] == 0.0 && r[1] == 0.0 && r[3] == plx_ui::scroll_capsule::FADE_STEP), "the top fade stays");
    assert!(kinds.iter().any(|(k, _)| k == "glass"), "a singleton keeps its library identity in the capsule");
    assert_eq!(kinds.iter().filter(|(k, r)| k == "rect" && r[0] == 0.0 && r[3] <= plx_ui::scroll_capsule::FADE_STEP && r[1] < 200.0)
        .map(|(_, r)| r[1] + r[3]).fold(0.0f32, f32::max), plx_ui::scroll_capsule::Y + plx_ui::scroll_capsule::H + plx_ui::scroll_capsule::FADE_BELOW_CAPSULE, "the fade reaches through the capsule");
    let (fixture, mut two, engine) = grid_page(2);
    let kinds = tucked_draw_kinds(&fixture, &mut two, &engine);
    assert!(kinds.iter().any(|(k, r)| k == "glass" && r[1] == plx_ui::scroll_capsule::Y), "two libraries: the capsule, as live glass");
    // the ground fade reaches through the capsule's band while the capsule shows, 12 px past the
    // capsule's own measured bottom, and only 72 deep on the one-library page that wears none
    let depth = |kinds: &[(String, [f32; 4])]| {
        kinds.iter().filter(|(k, r)| k == "rect" && r[0] == 0.0 && r[2] == SCR_W && r[3] <= plx_ui::scroll_capsule::FADE_STEP && r[1] < 200.0)
            .map(|(_, r)| r[1] + r[3]).fold(0.0f32, f32::max)
    };
    assert_eq!(depth(&kinds), plx_ui::scroll_capsule::Y + plx_ui::scroll_capsule::H + plx_ui::scroll_capsule::FADE_BELOW_CAPSULE);
    assert!(kinds.iter().all(|(k, _)| k != "glass_frozen"));
}

/// **The scrolled capsule carries the sort the header's control shows** ("Films 1,284 · Date
/// viewed"): the library, the count, a dot, then the listing's CURRENT sort label, the one string
/// the header's sort control wears (`sort_value`, a queued edit included), drawn once each; and the
/// capsule's rect, which the top fade reaches through, is as wide as those four pieces.
#[test]
fn the_scrolled_capsule_carries_the_sort_the_header_shows() {
    use plx_ui::screen::Screen;
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, engine) = grid_page(2);
    let pitch = page.layout.grid_pitch();
    page.scroll.jump(pitch);
    page.scroll_target = pitch;
    let worn = page.capsule_wanted();
    page.handoff.step(0.0, pitch, pitch, pitch, worn);
    let cx = fixture.cx(&engine);
    let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    let runs = {
        let _walk = plx_ui::frame::backdrop::discover(sources.clone());
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        plx_ui::draw_census::capture_runs(|| page.draw(&mut frame))
    };
    let (top, bottom) = (plx_ui::scroll_capsule::Y, plx_ui::scroll_capsule::Y + plx_ui::scroll_capsule::H);
    let capsule: Vec<&plx_ui::draw_census::Run> = runs.iter().filter(|r| r.at.y >= top && r.at.y + r.at.h <= bottom + 1.0).collect();
    let texts: Vec<&str> = capsule.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts.len(), 4, "the library, the count, the dot and the criterion, each once: {texts:?}");
    let sort = page.sort_value(&cx);
    assert_eq!(texts[2], "\u{b7}");
    assert_eq!(texts[3], sort, "the criterion is the listing's current sort label");
    let control = page.header.row().and_then(|row| row.page.spec().right.last().and_then(|item| item.value.clone()));
    assert_eq!(control.as_deref(), Some(sort), "and the header's control shows the same string");
    let last = capsule[3];
    assert!(last.at.x + last.at.w + plx_ui::scroll_capsule::PAD_R <= SCR_W, "the capsule stays on the screen");
}

/// **The scrolled capsule builds its strings when its text changes, not on every frame it is up**
/// (`CapsuleText::get_keyed`, keyed on the library's title and owner pieces, the catalog in force,
/// the total and the criterion): any number of frames on one library build them once, and a renamed
/// library or another section on view builds them once more, naming what is on view.
#[test]
fn the_capsule_builds_its_strings_only_when_the_library_on_view_or_its_name_changes() {
    use plx_ui::screen::Screen;
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, engine) = grid_page(2);
    let pitch = page.layout.grid_pitch();
    page.scroll.jump(pitch);
    page.scroll_target = pitch;
    let worn = page.capsule_wanted();
    page.handoff.step(0.0, pitch, pitch, pitch, worn);
    // The capsule's words as one draw says them, the library's name first.
    let words = |fixture: &Fixture, page: &mut LibraryScreen| -> Vec<String> {
        let cx = fixture.cx(&engine);
        let sources = std::rc::Rc::new(std::cell::RefCell::new(plx_ui::frame::backdrop::Sources::default()));
        sources.borrow_mut().begin(vec![]);
        let _walk = plx_ui::frame::backdrop::discover(sources.clone());
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
        let (top, bottom) = (plx_ui::scroll_capsule::Y, plx_ui::scroll_capsule::Y + plx_ui::scroll_capsule::H);
        plx_ui::draw_census::capture_runs(|| page.draw(&mut frame)).into_iter()
            .filter(|run| run.at.y >= top && run.at.y + run.at.h <= bottom + 1.0).map(|run| run.text).collect()
    };
    assert_eq!(page.capsule_text.builds(), 0, "premise: nothing drawn yet");
    assert_eq!(words(&fixture, &mut page).first().map(String::as_str), Some("Library 0 · Popular"), "premise: the capsule names the library and its catalog");
    assert_eq!(page.capsule_text.builds(), 1);
    for _ in 0..5 { assert_eq!(words(&fixture, &mut page).first().map(String::as_str), Some("Library 0 · Popular")); }
    assert_eq!(page.capsule_text.builds(), 1, "five more frames on the same library build nothing");
    // The library is renamed (the same section, another title): built once more.
    fixture.sections[0].row.title = "Renamed".into();
    fixture.publish(0, 36);
    for _ in 0..3 { assert_eq!(words(&fixture, &mut page).first().map(String::as_str), Some("Renamed · Popular")); }
    assert_eq!(page.capsule_text.builds(), 2, "a renamed library builds its strings once");
    // Another section is on view: built once more.
    fixture.publish(1, 36);
    for _ in 0..3 { assert_eq!(words(&fixture, &mut page).first().map(String::as_str), Some("Library 1 · Popular")); }
    assert_eq!(page.capsule_text.builds(), 3, "another section on view builds its strings once");
}

/// **Whatever a language calls its criterion, the capsule stays on the screen and clear of the
/// page's right margin**: the longest label every shipped language has for a sort, beside a long
/// library name and a five-figure count, measured the way the device sums advances. The criterion
/// is app-owned text, so it is never cut; only a library name past about 60 characters could push
/// the capsule off, and that name is the server's own text (it already grew the capsule unbounded).
#[test]
fn the_capsule_with_the_longest_criterion_in_every_language_ends_before_the_right_margin() {
    use plx_base::fontcov::advances::ShippedMeasure;
    let _guard = plx_base::testlock::serial();
    let measure = ShippedMeasure;
    let criteria = ["Date viewed", "Последняя добавленная серия", "Zuletzt hinzugefügte Folge", "Aggiunti di recente al catalogo"];
    for criterion in criteria {
        let (name, count, k) = plx_ui::scroll_chrome::capsule_parts("Documentaries and Concerts", 12_345, Some(criterion));
        let rect = plx_ui::scroll_capsule::rect(&name, &count, k.as_deref(), &measure);
        assert!(rect.x + rect.w <= SCR_W - plx_ui::consts::MARGIN_X, "{criterion}: the capsule ends at {}", rect.x + rect.w);
    }
}

/// **A two-library page declares no live glass while its chrome is still on screen, either way.**
/// The chrome band (the nav pill's live glass) and the capsule are two live glasses whose blur
/// footprints meet; sharing a frame makes the band capture inline in the visible pass, +1.4M GPU
/// cycles per frame and a quarter of the ramp's frames late on the television. So the page's
/// handoff (`scroll_chrome::Handoff`, stepped by the page's own `Tick`) sequences them: going down
/// the chrome leaves and only then does the capsule arrive; coming up the capsule leaves and only
/// then does the chrome return. Driven through real ticks and real draws: on every frame the page
/// declares the capsule only when the chrome it tells the dispatcher to draw is gone, and a page
/// that has fully tucked declares exactly one live glass, at the capsule's place.
#[test]
fn a_two_library_page_declares_no_live_glass_while_its_chrome_is_still_on_screen() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let pitch = page.layout.grid_pitch();
    let mut ms = 0;
    let mut run = |page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, target: f32| {
        page.scroll_target = target;
        let (mut chrome_frames, mut glass_frames) = (0, 0);
        for _ in 0..90 {
            ms += 16;
            fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
            let glass = draw_kinds_now(&fixture, page, engine).iter().filter(|(k, _)| k == "glass").count();
            let tuck = <LibraryScreen as plx_ui::screen::Screen<TestHost>>::chrome_tuck(page);
            if tuck < 1.0 {
                chrome_frames += 1;
                assert_eq!(glass, 0, "scroll {} -> {target}: the chrome is at tuck {tuck}, still on screen, so the capsule is not", page.scroll.pos);
            } else if glass > 0 {
                glass_frames += 1;
            }
        }
        (chrome_frames, glass_frames)
    };
    let (leaving, arriving) = run(&mut page, &mut engine, pitch);
    assert!(leaving >= 4 && arriving >= 4, "premise: Down has frames of each half ({leaving} with the chrome up, {arriving} with the capsule)");
    let kinds = draw_kinds_now(&fixture, &mut page, &engine);
    let capsules: Vec<_> = kinds.iter().filter(|(k, _)| k == "glass").collect();
    assert_eq!(capsules.len(), 1, "fully tucked: exactly one live glass");
    assert_eq!(capsules[0].1[1], plx_ui::scroll_capsule::Y, "and it is the capsule");
    let (returning, leaving) = run(&mut page, &mut engine, 0.0);
    assert!(leaving >= 4 && returning >= 4, "premise: Up has frames of each half ({leaving} with the capsule going, {returning} with the chrome coming back)");
    assert!(draw_kinds_now(&fixture, &mut page, &engine).iter().all(|(k, _)| k != "glass"), "back at the top: no capsule");
}

/// One frame of the page: its `Tick`, then the header drawn at that moment, as its rounded rects.
fn header_rrects_after_a_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32) -> Vec<plx_ui::Rect> {
    fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
    let cx = fixture.cx(engine);
    let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
    plx_ui::draw_census::capture(|| page.draw_header(&frame)).into_iter().filter(|(tag, _)| *tag == 2).map(|(_, rect)| rect).collect()
}

/// The focus pill (the one rounded rect taller than the 56 px plates, since it stands out by
/// `PILL_FOCUS_OUTSET`) after one frame, as its centre x; `None` when no pill is drawn.
fn pill_centre_after_a_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32) -> Option<f32> {
    let pills: Vec<f32> = header_rrects_after_a_frame(fixture, page, engine, ms).iter()
        .filter(|rect| rect.h > plx_ui::page_header::HEIGHT + 1.0 && rect.h < plx_ui::page_header::HEIGHT * 1.2)
        .map(|rect| rect.cx()).collect();
    assert!(pills.len() <= 1, "one focused control wears one pill: {pills:?}");
    pills.first().copied()
}

/// **The header's focus pill travels** (owner: "pills have no transitions, look raw"). Right from
/// one library name to the next slides the pill across the gap on the top bar's spring, one step
/// per frame and never backwards, and it comes to rest on the new name; before, it was redrawn on
/// the new plate in a single frame.
#[test]
fn the_header_focus_pill_travels_to_the_next_name_instead_of_jumping() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(3);
    let (first, second) = (page.libraries[0].0, page.libraries[1].0);
    let key = page.key(first);
    engine.set(OWNER, key, Some(LIBRARY_GROUP), By::Dir);
    fixture.step(&mut page, &mut engine, ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir });
    let mut ms = 0;
    let mut rest = None;
    for _ in 0..120 { ms += 16; rest = pill_centre_after_a_frame(&fixture, &mut page, &mut engine, ms); }
    let plates = page.header.row().unwrap();
    let (from, to) = (plates.plate(first).unwrap().cx(), plates.plate(second).unwrap().cx());
    assert!(to > from + 40.0, "two names with room between them");
    assert!((rest.expect("a focused name wears the pill") - from).abs() < 1.0, "settled on the first name");

    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(at(&engine), second);
    let mut path = Vec::new();
    for _ in 0..90 { ms += 16; path.push(pill_centre_after_a_frame(&fixture, &mut page, &mut engine, ms).expect("the pill stays drawn")); }
    assert!(path.windows(2).all(|w| w[1] >= w[0] - 0.01), "the pill never goes backwards: {path:?}");
    assert!(path.iter().any(|x| *x > from + 4.0 && *x < to - 4.0),
        "some frame draws the pill between the two names (it jumped): {path:?}");
    assert!((path.last().unwrap() - to).abs() < 1.0, "it comes to rest on the new name: {path:?}");
}

/// **Switching library keeps the row's motion** (owner: "pills have no transitions"; the screen
/// holds the capsules, never the built row). OK on the name the pill sits on switches the library:
/// the selection plate travels to the pill (it is never cut onto it), and the pill neither restarts
/// from the left, nor fades, nor moves; the row is rebuilt for the new library and its motion state is not.
#[test]
fn a_library_switch_keeps_the_rows_pill_and_travels_the_plate_to_the_picked_name() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(3);
    let current = page.view_section(&fixture.cx(&engine));
    let other = page.libraries.iter().find(|(_, index)| Some(*index) != current).unwrap().0;
    let (from, to) = (None, page.key(other));
    engine.set(OWNER, to, Some(LIBRARY_GROUP), By::Dir);
    fixture.step(&mut page, &mut engine, ScreenEvent::FocusMoved { from, to, by: By::Dir });
    let mut ms = 0;
    let mut rest = None;
    for _ in 0..90 { ms += 16; rest = pill_centre_after_a_frame(&fixture, &mut page, &mut engine, ms); }
    let rest = rest.expect("a focused name wears the pill");
    let plate = page.header.row().unwrap().plate(other).unwrap();
    assert!((rest - plate.cx()).abs() < 0.5, "settled under the name that will be picked");
    let selected_under = |rects: &[plx_ui::Rect]| rects.iter().any(|r| (r.h - plx_ui::page_header::HEIGHT).abs() < 0.01 && (r.cx() - plate.cx()).abs() < 1.0);
    let plate_cx = |rects: &[plx_ui::Rect]| rects.iter().find(|r| (r.h - plx_ui::page_header::HEIGHT).abs() < 0.01).map(|r| r.cx());
    let before_rects = header_rrects_after_a_frame(&fixture, &mut page, &mut engine, ms + 16);
    assert!(!selected_under(&before_rects), "premise: the old library wears the plate");
    let old_cx = plate_cx(&before_rects).expect("premise: the old library's plate is drawn");

    let motion = |page: &LibraryScreen| {
        let mut canon = plx_machine::machine::Canon::new();
        page.header_capsules.write_motion(&mut canon);
        page.header_pop.write_motion(&mut canon);
        canon.finish()
    };
    let before = motion(&page);
    fixture.step(&mut page, &mut engine, ScreenEvent::Activate(other));
    assert!(page.switched, "OK switched the library");
    assert_eq!(motion(&page), before, "the switch itself touches no motion state: the row is rebuilt, its capsules are not");
    for frame in 1..=30 {
        let rects = header_rrects_after_a_frame(&fixture, &mut page, &mut engine, ms + 16 * (frame + 1));
        let pill = rects.iter().filter(|r| r.h > plx_ui::page_header::HEIGHT + 1.0).map(|r| r.cx()).collect::<Vec<_>>();
        assert_eq!(pill.len(), 1, "frame {frame}: the pill stays drawn through the switch: {rects:?}");
        assert!((pill[0] - rest).abs() < 0.5, "frame {frame}: and stays where it was ({} vs {rest})", pill[0]);
        // Frame 5 is where `flush` hands the pick to the store and `pending` is consumed. The
        // fixture has no store behind it to move the directory's current library, so the page then
        // reads the old library again; while the switch is pending, the plate TRAVELS to the name
        // (design review S5: it is never cut onto it): on its way on the first frame, nearer
        // the name every frame, on it when it arrives.
        if frame <= 4 {
            let at = plate_cx(&rects).expect("the plate stays drawn through the switch");
            let left = (at - plate.cx()).abs();
            assert!(left < (old_cx - plate.cx()).abs() - 0.5, "frame {frame}: the plate has left the old library ({at} from {old_cx})");
            assert!(frame > 1 || left > 1.0, "the plate is on its way on the first frame, not cut onto the name ({at} vs {})", plate.cx());
        }
    }
}

#[test]
fn the_selection_arrives_on_a_new_visit_and_does_not_sweep_its_slot_on_return() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new(3, 36, 0);
    fixture.sections[2].row.pinned = false;
    fixture.publish(0, 36);
    let mut page = fixture.screen();
    page.initial = false;
    let first = page.libraries[0].0;
    let second = page.libraries[1].0;
    let mut engine = FocusEngine::new();
    engine.set(OWNER, page.key(second), Some(LIBRARY_GROUP), By::Restore);
    for frame in 1..=90 {
        header_rrects_after_a_frame(&fixture, &mut page, &mut engine, frame * 16);
    }
    let before = page.header.row().unwrap().plate(second).unwrap();

    fixture.step(&mut page, &mut engine, ScreenEvent::App(AppMsg::LibrarySelect(SectionAddress {
        epoch: fixture.epoch, sid: plx_plex::plex::ServerId::UNSET, section: 3,
    })));
    let visit = page.header.row().unwrap().selected_stop().unwrap();
    let visit_plate = page.header.row().unwrap().plate(visit).unwrap();
    assert_eq!(at(&engine), visit);
    let plate = |rects: &[Rect]| rects.iter().find(|r| (r.h - plx_ui::page_header::HEIGHT).abs() < 0.01).copied();
    let frame = header_rrects_after_a_frame(&fixture, &mut page, &mut engine, 91 * 16);
    assert_eq!(plate(&frame), Some(visit_plate), "the selection starts inside the inserted slot");
    let focus = frame.iter().find(|r| r.h > plx_ui::page_header::HEIGHT + 1.0).unwrap();
    assert!(before.cx() < focus.cx() && focus.cx() < visit_plate.cx(), "focus glides from the fixed favorite to the visit");

    for n in 92..=95 {
        header_rrects_after_a_frame(&fixture, &mut page, &mut engine, n * 16);
    }
    fixture.publish(2, 36);
    fixture.step(&mut page, &mut engine, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
    for n in 96..=180 {
        let frame = header_rrects_after_a_frame(&fixture, &mut page, &mut engine, n * 16);
        assert_eq!(plate(&frame), Some(visit_plate), "the listing landing does not move the selection");
    }
    assert_eq!(at(&engine), visit);
    fixture.direction(&mut page, &mut engine, Dir::Left);
    assert_eq!(at(&engine), second, "Left from the visit goes to the last favorite, with no options stop");
    fixture.direction(&mut page, &mut engine, Dir::Left);
    assert_eq!(at(&engine), first);
    fixture.step(&mut page, &mut engine, ScreenEvent::Activate(first));
    let favorite_plate = page.header.row().unwrap().plate(first).unwrap();
    assert!(page.header.row().unwrap().plate(visit).is_none());
    assert!(page.probe_header_stops().iter().all(|(name, _)| *name != "more"), "no options element is left on the row");
    let frame = header_rrects_after_a_frame(&fixture, &mut page, &mut engine, 181 * 16);
    assert_eq!(plate(&frame), Some(favorite_plate), "the removed slot leaves no traveling selection behind");
}

/// **While the Sort & Filter menu is up, the control's accent pill is not under its panel.** The
/// engine's focus is on the menu's own rows (another entry), so the row's motion HOLDS (a pill
/// that faded meanwhile would have to fade back in when the closing morph hands over), and the
/// page must still draw it out: the control is lifted, the menu is its only copy.
#[test]
fn the_held_focus_pill_is_not_drawn_under_an_open_menu_and_is_still_up_when_it_closes() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let key = page.key(SORTFILTER);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    for frame in 1..=60 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
    let pill_w = page.header.row().unwrap().plate(SORTFILTER).unwrap().w + 2.0 * plx_ui::widgets::PILL_FOCUS_OUTSET;
    let pills = |page: &LibraryScreen, engine: &FocusEngine<u32>, lifted: Option<FocusKey<u32>>| {
        let cx = fixture.cx(engine);
        let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
        frame.lifted = lifted;
        plx_ui::draw_census::capture(|| page.draw_header(&frame)).iter()
            .filter(|(tag, rect)| *tag == 2 && (rect.w - pill_w).abs() < 0.5).count()
    };
    assert_eq!(pills(&page, &engine, None), 1, "premise: the focused control wears the pill");
    let motion = |page: &LibraryScreen| {
        let mut canon = plx_machine::machine::Canon::new();
        page.header_capsules.write_motion(&mut canon);
        page.header_pop.write_motion(&mut canon);
        canon.finish()
    };
    let up = motion(&page);

    // The menu opens: its rows hold focus (another entry) and the page lifts the control.
    engine.set(OWNER, FocusKey { entry: EntryId(ENTRY.0 + 1), elem: 0 }, None, By::Dir);
    for frame in 61..=120 { fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 })); }
    assert_eq!(pills(&page, &engine, Some(key)), 0, "its menu lifted it: no pill under the panel");
    assert_eq!(motion(&page), up, "the row's motion held while a surface above had focus");

    // The menu closes: focus is back on the control, the page draws it, and the pill is still up.
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    assert_eq!(pills(&page, &engine, None), 1, "the first frame after the menu is the pill, not a fade-in");
}

/// **The header row is built once per key, not once per frame.** `sync` runs every frame; the row
/// (text measurement, packing) is rebuilt only when something it is made of changed: a library
/// switch builds exactly one, and the frames either side of it build none. The same rule holds the
/// Recommended page's and the Watchlist's rows (`plx_ui::page_header::RowCache`).
#[test]
fn the_header_row_is_built_once_per_key_not_once_per_frame() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, engine) = grid_page(2);
    let built = page.header.builds();
    assert!(built >= 1, "premise: the row is built");
    for _ in 0..60 { page.sync(&fixture.cx(&engine)); }
    assert_eq!(page.header.builds(), built, "sixty idle frames build no row");
    fixture.publish(1, 36);
    page.sync(&fixture.cx(&engine));
    assert_eq!(page.header.builds(), built + 1, "a library switch builds the row once");
    for _ in 0..60 { page.sync(&fixture.cx(&engine)); }
    assert_eq!(page.header.builds(), built + 1, "and the frames after it build none");
}

/// One frame of the page, then every text run the header draws at that moment.
fn header_runs_after_a_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32) -> Vec<plx_ui::draw_census::Run> {
    fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
    let cx = fixture.cx(engine);
    let frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::recording());
    plx_ui::draw_census::capture_runs(|| page.draw_header(&frame))
}

/// What one frame of the header shows of the Catalogs control, and what the built row says of it.
struct Frame {
    /// `(x, alpha)` of every drawn "Catalogs" label.
    labels: Vec<(f32, f32)>,
    /// `(text, x, alpha)` of every drawn run naming a catalog.
    values: Vec<(String, f32, f32)>,
    /// The Catalogs item of the BUILT row: `None` = the row has no such control.
    built: Option<Option<String>>,
    /// Is the Catalogs control one of the controls the row's focus walks?
    focusable: bool,
}

fn watch_frame(fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, ms: u32) -> Frame {
    use plx_os::i18n::msg;
    let runs = header_runs_after_a_frame(fixture, page, engine, ms);
    let label = msg::library_header_catalogs();
    let values: Vec<&str> = TEST_CATALOGS.iter().map(|(_, title)| *title).collect();
    Frame {
        labels: runs.iter().filter(|run| run.text == label).map(|run| (run.at.x, run.ink[3])).collect(),
        values: runs.iter().filter(|run| values.contains(&run.text.as_str()))
            .map(|run| (run.text.clone(), run.at.x, run.ink[3])).collect(),
        built: page.header.row().and_then(|row| row.page.spec().left.iter()
            .find(|item| item.label == label).map(|item| item.value.clone())),
        focusable: page.toolbar_elems().contains(&CATALOGS),
    }
}

/// Settle `page` for ten frames, then publish a listing whose catalogs are `list` / `current`
/// and watch the row for 60 frames. Returns the settled frame and the ones after the publication.
fn catalog_and_watch(list: Vec<plx_data::browse::CatalogEntry>, current: Option<&str>) -> (Frame, Vec<Frame>) {
    let (mut fixture, mut page, mut engine) = grid_page_with_catalogs(2, Vec::new(), None);
    let mut ms = 0;
    let mut rest = None;
    for _ in 0..10 { ms += 16; rest = Some(watch_frame(&fixture, &mut page, &mut engine, ms)); }
    fixture.listing = fixture.listing.clone().with_catalogs(list, current);
    let mut frames = Vec::new();
    for _ in 1..=60 {
        ms += 16;
        frames.push(watch_frame(&fixture, &mut page, &mut engine, ms));
    }
    (rest.unwrap(), frames)
}

/// **The Catalogs control names the catalog in force**: the left item carries the active title
/// beside its label, and a queued or committed pick changes that value without repacking the
/// library names or moving the right cluster. Fork Rofocale11: the neighbouring Recommended pill
/// stands by what it draws, so a shorter catalog title DOES slide it left — that is the old
/// fork's drawn-width rule, not a regression (the reserved widest title protects the NAMES only).
#[test]
fn the_catalogs_control_names_the_catalog_in_force() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, mut engine) = grid_page_with_catalogs(2, test_catalogs(), Some(TEST_CATALOGS[2].0));
    let cx = fixture.cx(&engine);
    let row = page.header.row().unwrap();
    let left = &row.page.spec().left;
    let control = left.iter().find(|item| item.label == plx_os::i18n::msg::library_header_catalogs()).unwrap();
    assert_eq!(control.value.as_deref(), Some(TEST_CATALOGS[2].1));
    assert!(control.fold_glyph);
    assert_eq!(page.current_catalog(&cx).as_deref(), Some(TEST_CATALOGS[2].1));
    let packing = |page: &LibraryScreen| (page.header.row().unwrap().layout.left_budget,
        page.libraries.clone(), page.folded.clone());
    let before = packing(&page);
    let sort = page.header_rect(SORTFILTER, At::Drawn).unwrap();
    // The pick lands as a query edit through the same transaction lane as every other query.
    let id = fixture.listing.view().id().unwrap();
    let target = SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
    fixture.step(&mut page, &mut engine, ScreenEvent::App(AppMsg::LibraryEdit {
        target, edit: plx_data::stores::browse::QueryEdit::Catalog(Some(TEST_CATALOGS[1].0.into())),
    }));
    fixture.listing = fixture.listing.clone().with_catalogs(test_catalogs(), Some(TEST_CATALOGS[1].0));
    page.sync(&fixture.cx(&engine));
    let control = page.header.row().unwrap().page.spec().left.iter()
        .find(|item| item.label == plx_os::i18n::msg::library_header_catalogs()).cloned().unwrap();
    assert_eq!(control.value.as_deref(), Some(TEST_CATALOGS[1].1), "the picked catalog is named");
    assert_eq!(packing(&page), before, "the pick does not repack the names or their budget");
    assert_eq!(page.header_rect(SORTFILTER, At::Drawn), Some(sort), "the right cluster stays put");
}

/// **The control joins the row when a section's catalogs arrive**, whole (label, value and focus
/// stop together); a section that advertised none has nothing to pick and draws no bare label.
#[test]
fn the_control_joins_the_row_when_the_catalogs_land() {
    let _guard = plx_base::testlock::serial();
    let (rest, frames) = catalog_and_watch(test_catalogs(), Some(TEST_CATALOGS[0].0));
    assert_eq!(rest.built, None, "premise: no catalogs, no control");
    assert!(!rest.focusable);
    let first = frames.iter().find(|frame| frame.built.is_some()).expect("the catalogs landed");
    assert_eq!(first.built, Some(Some(TEST_CATALOGS[0].1.to_string())));
    assert!(first.focusable);
    assert!(frames.iter().skip_while(|frame| frame.built.is_none())
        .all(|frame| frame.labels.len() <= 1 && frame.values.len() <= 1), "one face at a time");
}

/// **A named catalog holds for the SAME library when the store is re-pointed** by a pushed page
/// that reads no catalogs (the store holds one listing per section). Another library never keeps
/// the one it left.
#[test]
fn a_named_catalog_holds_for_the_same_library_across_a_repoint() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, engine) = grid_page_with_catalogs(2, test_catalogs(), Some(TEST_CATALOGS[2].0));
    assert!(page.catalogs, "premise: the section advertised catalogs");
    fixture.listing = fixture.listing.clone().with_catalogs(Vec::new(), None);
    for _ in 0..30 {
        page.sync(&fixture.cx(&engine));
        assert!(page.catalogs, "a read of nothing for this library does not withdraw the control");
        assert_eq!(page.current_catalog(&fixture.cx(&engine)), None, "but there is no title to name");
    }
    // A switch to another library whose listing advertises none clears it.
    fixture.publish(1, 36);
    fixture.listing = fixture.listing.clone().with_catalogs(Vec::new(), None);
    page.sync(&fixture.cx(&engine));
    assert!(!page.catalogs, "a different library's answer is not held");
}

/// OK on the Catalogs control opens the Type menu, anchored on that control. The Collections page
/// entry point is gone: no header stop opens it any more.
#[test]
fn ok_on_the_control_opens_the_catalog_menu() {
    let _guard = plx_base::testlock::serial();
    let (fixture, mut page, mut engine) = grid_page(2);
    let key = page.key(CATALOGS);
    engine.set(OWNER, key, Some(LIBRARY_GROUP), By::Restore);
    let effects = fixture.step(&mut page, &mut engine, ScreenEvent::Activate(CATALOGS));
    assert!(effects.iter().any(|fx| matches!(fx,
        AppFx::Library(LibraryReq::Menu { kind, control, .. })
            if *kind == crate::registry::LibraryMenuKind::Type && *control == CATALOGS)),
        "the catalog menu opened");
    assert!(page.probe_header_stops().iter().all(|(name, _)| *name != "collections"));
}

/// A page of two libraries of different kinds, Movies (key 1) and TV Shows (key 2), settled on
/// section `current`, as the top navigation finds it.
fn two_kind_page(current: usize) -> (Fixture, LibraryScreen, FocusEngine<u32>) {
    let mut fixture = Fixture::new(2, 36, 0);
    fixture.sections[1].kind = SecKind::Show;
    fixture.publish(current, 36);
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), fixture.sections[current].kind);
    let mut engine = FocusEngine::new();
    page.sync(&fixture.cx(&engine));
    page.initial = false;
    page.sync(&fixture.cx(&engine));
    page.relayout(None);
    let first = page.key(page.pair.detail.elem_at(0).unwrap());
    engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    (fixture, page, engine)
}

/// **Entering a library whose section advertised no catalogs never builds the control** (Movies
/// to TV Shows from the top navigation): the flag is read from the published listing, not held
/// from the library left behind, and focus never walks to a control that is not there.
#[test]
fn entering_a_library_without_catalogs_never_builds_the_control() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, engine) = two_kind_page(0);
    assert!(page.catalogs, "premise: Movies advertises catalogs");
    fixture.publish(1, 36);
    fixture.listing = fixture.listing.clone().with_catalogs(Vec::new(), None);
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.command(LibraryCmd::Enter(SecKind::Show), &fixture.cx(&engine),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    for frame in 1..=40 {
        page.sync(&fixture.cx(&engine));
        assert!(!page.catalogs, "frame {frame}: TV Shows has no catalogs to pick");
        assert!(!page.toolbar_elems().contains(&CATALOGS), "frame {frame}: no catalogs stop");
        let row = page.header.row().unwrap();
        assert!(!row.page.spec().left.iter().any(|item|
            item.label == plx_os::i18n::msg::library_header_catalogs()), "frame {frame}: no control built");
    }
}
