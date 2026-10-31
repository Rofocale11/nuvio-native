//! The Tier 2 card-conformance harness for the Library page (`cards_conformance_tests.rs`): the
//! All grid (a `GridPart` over `cards::Grid`). The page no longer draws hub shelves above the grid,
//! so there is one card set. A child module of `library` so it reads the private rect and scale
//! helpers the screen's own tests read.
use super::*;
use plx_machine::machine::{FocusRead, Host, InputOwner, PressRead, Tick};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::{FixtureArg, FixtureMeasure};
use plx_ui::screen::By;

struct LibHost;
#[derive(Clone, Copy)]
struct Views<'a> {
    listing: plx_data::stores::browse::ListingView<'a>,
    directory: plx_data::stores::browse::DirectoryView<'a>,
    hubs: plx_data::stores::browse::HubsView<'a>,
}
impl Host for LibHost {
    type Arg = FixtureArg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = FixtureArg;
    type Memory = PageMemory;
}
impl LibraryLike for LibHost {
    fn listing<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::ListingView<'a> { cx.views.listing }
    fn directory<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::DirectoryView<'a> { cx.views.directory }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::HubsView<'a> { cx.views.hubs }
}

const ENTRY: EntryId = EntryId(81);
const INSTANCE: InstanceId = InstanceId(19);

pub(crate) struct Harness {
    n: usize,
    rks: Vec<String>,
    listing: plx_data::stores::browse::ListingSnapshot,
    directory: plx_data::stores::browse::DirectorySnapshot,
    hubs: plx_data::stores::browse::HubsSnapshot,
    screen: LibraryScreen,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
}

pub(crate) fn mount_grid(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new(n)) }

fn listing_of(rks: &[String]) -> plx_data::stores::browse::ListingSnapshot {
    let sid = plx_plex::plex::ServerId::from_raw(0);
    plx_data::browse::view::ListingSnapshot::fixture(sid,
        rks.iter().map(|rk| Some(plx_data::pms::PmsMovie { sid, rk: rk.clone(), title: rk.clone(), ..Default::default() })).collect(),
        vec![("A".into(), rks.len() as i64)])
}

impl Harness {
    fn new(n: usize) -> Self {
        let rks: Vec<String> = (0..n).map(|i| format!("m{i}")).collect();
        let sid = plx_plex::plex::ServerId::from_raw(0);
        let directory = plx_data::browse::view::DirectorySnapshot::fixture(1, 0, vec![
            plx_data::browse::view::SectionView { sid: Some(sid), key: 1, kind: SecKind::Movie,
                row: plx_data::browse::SrcRow { section: 0, pinned: true, current: true, ..Default::default() } }]);
        let listing = listing_of(&rks);
        let hubs = plx_data::stores::browse::HubsSnapshot::empty_for_test();
        let mut h = Self { n, rks, listing, directory, hubs,
            screen: LibraryScreen::new(ENTRY, INSTANCE, SecKind::Movie), focus: None, press: 1.0, ms: 0 };
        h.sync();
        h.screen.initial = false;
        h
    }

    fn cx(&self) -> Cx<'_, LibHost> {
        Cx { views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
            tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) }
    }

    fn sync(&mut self) {
        let cx: Cx<'_, LibHost> = Cx {
            views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
            tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
        self.screen.sync(&cx);
    }

    fn step(&mut self, ev: ScreenEvent<LibHost>) -> bool {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx: Cx<'_, LibHost> = Cx {
                views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
                tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
                press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let mut fx = Effects::new(&mut out, MachineId::Instance(INSTANCE), &mut present);
            Machine::<LibHost>::step(&mut self.screen, &ev, &cx, &mut fx);
        });
        moving || present.page_moving()
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: ENTRY, elem } }

    fn cell(&self, elem: u32) -> Option<(usize, usize)> {
        self.screen.pair.detail.index_of(elem).map(|i| (0, i))
    }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> { self.screen.pair.detail.elems.clone() }
    fn focused(&self) -> Option<u32> { self.focus.map(|k| k.elem) }
    fn focus(&mut self, elem: u32, by: By) {
        let from = self.focus;
        self.focus = Some(self.key(elem));
        self.step(ScreenEvent::FocusMoved { from, to: self.key(elem), by });
    }
    fn tick(&mut self, frames: u32) -> bool {
        let mut moved = false;
        for _ in 0..frames {
            self.ms += 16;
            moved |= self.step(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 }));
        }
        moved
    }
    fn cover(&mut self) { self.step(ScreenEvent::Cover); }
    fn uncover(&mut self) { self.step(ScreenEvent::Uncover); }
    fn set_press(&mut self, scale: f32) { self.press = scale; }
    fn neighbour(&self, elem: u32, dir: Dir) -> Nb {
        match Focusable::<LibHost>::neighbour(&self.screen, self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        Focusable::<LibHost>::place(&self.screen, &elem, &self.cx(), at)
    }
    /// What the draw paints and registers: the grid's rect for the card, the shelves' recorded stop.
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        let (_, col) = self.cell(elem)?;
        let mut cx = self.cx();
        cx.press = PressRead { scale: press, owner: cx.focus.current, ..Default::default() };
        Some(self.screen.pair.detail.rect_at(&cx, col))
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        self.cell(elem)?;
        self.screen.pair.detail.scale_of(&self.cx(), elem)
    }
    fn focus_scale(&self) -> f32 { RowStyle::HOME.focus_scale }
    fn canon(&self) -> u64 { LogicalState::hash(&self.screen) }
    fn identity(&self, elem: u32) -> String {
        let Some((_, col)) = self.cell(elem) else { return String::new() };
        self.listing.view().item(col).map(|m| m.rk.clone()).unwrap_or_default()
    }
    fn scroll(&self) -> Option<f32> {
        Some(self.screen.scroll.pos)
    }
    fn scroll_max(&self) -> Option<f32> { None }
    fn columns(&self) -> Option<usize> { Some(super::layout::COLS) }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let (_, at) = self.focus.and_then(|k| self.cell(k.elem)).ok_or("nothing focused")?;
        match l {
            Landing::Reorder => self.rks.swap(0, 2),
            Landing::InsertAbove => self.rks.insert(0, "landed".into()),
            Landing::RemoveFocused => { self.rks.remove(at); }
        }
        self.listing = listing_of(&self.rks);
        self.step(ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
        let want = self.focus.unwrap();
        let now = Focusable::<LibHost>::reconcile(&self.screen, want, &self.cx());
        if now != want { self.focus(now.elem, By::Reconcile); }
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let PageMemory::Library(mem) = Screen::<LibHost>::memory_at(&self.screen, self.focus) else {
            return Err("memory_at did not return PageMemory::Library");
        };
        let mut fresh = Harness::new(self.n);
        fresh.screen.restore(&mem);
        fresh.sync();
        fresh.screen.initial = false;
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<LibHost>::reconcile(&fresh.screen, want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}

/// The All grid's focus pop, ported from the `GridPart`-internal tests that read `rect_at` /
/// `tile_scale` / `treatment_scale` (deleted with the part's own pop): the same assertions, now
/// read through `Focusable::place` and `Grid::scale_of`, the values the draw and the stops share.
mod pop_tests {
    use super::*;
    use plx_ui::consts::{CARD_H, CARD_W};
    use crate::library::parts::OVERLAID;

    fn width(h: &Harness, elem: u32) -> f32 { h.place(elem, At::Drawn).unwrap().rect.w }

    #[test]
    fn episode_grid_geometry_and_page_window_share_four_column_rows() {
        let mut h = Harness::new(80);
        let layout = Layout::new(false, &[], 20, true).with_episodes(true);
        h.screen.pair.detail.set_geometry(layout, 0.0, layout, 0.0);
        let cx = h.cx();
        let (first, next_row) = (h.screen.pair.detail.rect_at(&cx, 0), h.screen.pair.detail.rect_at(&cx, 4));
        assert_eq!(next_row.x, first.x);
        assert!((next_row.y - first.y - layout.grid_pitch()).abs() < 0.001);
        assert_eq!((first.w, first.h), (layout.card_w(), layout.card_h()));
        let scroll = layout.row_reveal(12);
        h.screen.pair.detail.set_geometry(layout, scroll, layout, scroll);
        let window = h.screen.pair.detail.window();
        assert!(window.contains(&48), "row 12's first episode must be in its page window");
        assert!(window.len() < 32, "the window should request only nearby episode rows");
    }

    /// Owner report, 2026-09-09: "in the All section the poster just pops right away, not
    /// animated". A deliberate move grows the new tile from rest over frames and lets the old go.
    #[test]
    fn a_newly_focused_grid_tile_grows_over_frames_and_the_old_one_lets_go() {
        let mut h = Harness::new(12);
        let cards = h.cards();
        let full = CARD_W * RowStyle::HOME.focus_scale;
        h.focus(cards[3], By::Dir);
        h.tick(1);
        let first = width(&h, cards[3]);
        assert!(first > CARD_W + 0.1 && first < full - 0.1,
            "one frame in, the tile is between rest and full scale: {first} (rest {CARD_W}, full {full})");
        h.tick(120);
        assert!((width(&h, cards[3]) - full).abs() < 0.5, "…and settles at full scale");
        h.focus(cards[4], By::Dir);
        h.tick(1);
        let (old, new) = (width(&h, cards[3]), width(&h, cards[4]));
        assert!(old > CARD_W + 0.1 && old < full - 0.1, "the old tile lets go rather than snapping: {old}");
        assert!(new > CARD_W + 0.1 && new < full - 0.1, "the new tile starts growing from rest: {new}");
        h.tick(120);
        assert_eq!(width(&h, cards[3]), CARD_W, "a settled neighbour costs nothing");
    }

    /// The tile LOSING focus must draw its shrinking rect and its treatment (radius, shadow) at one
    /// scale: `scale_of` is the value `Grid::draw` hands both.
    #[test]
    fn an_unfocusing_grid_tile_draws_its_shrinking_rect_and_treatment_at_the_same_scale() {
        let mut h = Harness::new(12);
        let cards = h.cards();
        h.focus(cards[3], By::Restore);
        h.tick(60);
        h.focus(cards[4], By::Dir);
        h.tick(1);
        let scale = h.scale(cards[3]).unwrap();
        assert!(scale > 1.0 && scale < RowStyle::HOME.focus_scale,
            "mid-shrink, the outgoing tile's own scale sits strictly between rest and full: {scale}");
        assert!((width(&h, cards[3]) - CARD_W * scale).abs() < 0.001,
            "the rect's width is CARD_W times the exact same scale: rect.w={} scale={scale}", width(&h, cards[3]));
        assert!((1.0_f32 - scale).abs() > 0.01, "a fresh mid-shrink tile must not already read as rest");
        h.tick(120);
        assert_eq!(h.scale(cards[3]), Some(1.0), "…and once settled, the shared scale agrees it is at rest");
    }
    /// A cell focused without a deliberate move is a restore or a reconcile: FULL scale on its
    /// first frame, before and after the tick that adopts it.
    #[test]
    fn a_cell_focused_without_a_move_is_adopted_at_full_scale() {
        let mut h = Harness::new(12);
        let cards = h.cards();
        let full = CARD_W * RowStyle::HOME.focus_scale;
        h.focus(cards[5], By::Restore);
        assert_eq!(width(&h, cards[5]), full, "the frame BEFORE the tick already draws it whole");
        h.tick(1);
        assert_eq!(width(&h, cards[5]), full, "and the tick adopts it without a step of animation");
        h.tick(3);
        assert_eq!(width(&h, cards[5]), full, "…and it stays there");
    }

    /// The click-in animation: the press is in the rect AND in the scale the card renderer is
    /// handed, so the label never slides and the shadow lets go (Home's treatment).
    #[test]
    fn a_pressed_grid_tile_hands_the_card_renderer_the_scale_its_rect_was_built_from() {
        let _guard = plx_base::testlock::serial();
        let mut h = Harness::new(12);
        let cards = h.cards();
        h.focus(cards[3], By::Restore);
        h.tick(1);
        let pop = h.scale(cards[3]).unwrap();
        let handed = |h: &mut Harness, press: f32| {
            h.set_press(press);
            OVERLAID.with(|seen| seen.borrow_mut().clear());
            let cx: Cx<'_, LibHost> = Cx {
                views: Views { listing: h.listing.view(), directory: h.directory.view(), hubs: h.hubs.view() },
                tick: Tick { ms: h.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
                press: PressRead { scale: h.press, owner: h.focus, ..Default::default() },
                focus: FocusRead { current: h.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let _discovery = plx_ui::frame::backdrop::discover(std::rc::Rc::new(std::cell::RefCell::new(Default::default())));
            let mut frame = plx_ui::screen::DrawFrame::new(&cx, plx_ui::Painter::root());
            Part::<LibHost>::draw(&mut h.screen.pair.detail, &mut frame, Rect::FULL);
            OVERLAID.with(|seen| seen.borrow().iter().find(|(i, _)| *i == 3).map(|(_, s)| *s))
                .expect("the focused tile reaches the renderer")
        };
        let ring = |s: f32| (s - 1.0) / (RowStyle::HOME.focus_scale - 1.0);
        for press in [1.0_f32, 0.96, 0.918] {
            let s = handed(&mut h, press);
            let rect = h.drawn_rect(cards[3], press).unwrap();
            assert!((rect.w - CARD_W * s).abs() < 0.001,
                "the scale handed to the renderer is the one the rect was built from: rect.w={} s={s}", rect.w);
            assert!((rect.h / s - CARD_H).abs() < 0.01,
                "a press never moves the label: rect.h/s={} (rest {CARD_H})", rect.h / s);
            assert!(s < pop * 1.0001, "a press only ever shrinks the pop: {s} vs {pop}");
        }
        assert!(ring(handed(&mut h, 1.0)) > 0.99, "a resting focused tile wears the whole shadow and sheen");
        assert!(ring(handed(&mut h, 0.918)) < 0.1, "…and lets go of them under a full press");
    }
}
