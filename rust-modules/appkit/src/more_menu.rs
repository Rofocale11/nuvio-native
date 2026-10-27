//! The player transport's **overflow menu** — the popover behind the third control disc (`…`), on
//! the same animated [`TableView`] as the subtitle/audio and profile menus. It only REPORTS the
//! chosen [`Action`]; `app.rs` performs it, exactly as the profile menu did before it became
//! an owned surface (`crate::screens::account_menu`, restructure phase 10).
//!
//! # Why an overflow menu exists at all
//!
//! **Stats for nerds**, the diagnostics overlay (`crate::app::diagnostics`), needs a home a stranger can
//! find, because it is how this app gets bug reports off televisions nobody here owns — every other
//! diagnostic surface in the codebase (the `/tmp/plxnative-*` triggers, the remote FIFO, the
//! capture stream) is compiled out of RELEASE builds by the `devtriggers` feature, which is what a
//! user installs. "Press `…`, turn Stats for nerds on, photograph the screen" is a sentence that
//! fits in a GitHub reply and needs no ssh, no root and no rebuild.
//!
//! It held that one row for a while, and a menu with one row is not a mistake either: the
//! alternative — hanging the toggle off a hidden key chord — is undiscoverable by exactly the
//! people who would report the bug, and the alternative to THAT is a fourth disc for a control most
//! users touch once. Overflow is what a `…` means.
//!
//! # A root with a Source row, and one page per addon under it
//!
//! **Source leads.** On the ROOT it is one headerless drill-in row ([`MoreRow::OpenSource`]) whose
//! value is the item's playable-stream count; RIGHT or OK pushes the **Source page**. **Options**
//! trails it on the root: the diagnostics switch is an overflow affordance, something a viewer
//! reaches for once, to photograph a bug, not a control anyone returns to.
//!
//! **The Source page is one row per addon that answered for the item** ([`MoreRow::OpenAddon`]),
//! each stating how many streams it offered, and each opening its OWN page ([`MorePage::Addon`]) of
//! just that addon's streams. That is the sub-menu: a viewer who trusts one source picks it once
//! instead of walking a flat list of a hundred and fifty rows.
//!
//! The addon named on a row is the shim's own `addon_name` (the `Media[]` field), carried through
//! the ranked list the route publishes — see `crate::source_list::addon_groups`, which is where the
//! grouping rule lives. Groups appear in RANKED order (the addon holding the best stream leads),
//! and the streams inside a page stay in ranked order too.
//!
//! **There is no playback-quality ladder here.** Under the Stremio shim the quality of a playback is
//! a property of the STREAM picked — the shim resolves a concrete file and the route plays it — so
//! a list of bitrate ceilings beside the stream list would be a second, contradictory answer to the
//! same question. The ladder lives only in Settings > Playback, where it is a policy for ordinary
//! Plex servers. (It was drawn on this page until 2026-10-26, above the addon rows.)
//!
//! **Options** holds switches. Its row carries [`Row::toggle`], so it states itself as the WORD
//! `On`/`Off` at the row's trailing edge. It is a STATE, not a destination: a chevron would promise
//! a page behind the row and there is none.
//!
//! `docs/parity-gaps.md`'s standing decision is that this app has **no full-screen menu sheets**.
//! The streams drill in INSIDE the popover instead, on the same [`plx_ui::page_stack::PageStack`]
//! the Subtitles tab of [`crate::track_menu`] uses: BACK, LEFT and a click on the title band mean
//! "up one page" (an addon page goes back to the Source page, the Source page to the root), and
//! only BACK on the root dismisses. When a page stops fitting, the [`TableView`] scrolls, which is
//! what it is for.
//!
//! A stream closes the menu on commit (opening a page does not), so the read-out is never what
//! confirms the press: the overlay appearing behind the dismissed panel — and, once the new source
//! opens, a different picture — is.
//!
//! # What a picked STREAM does
//!
//! It is not a routing policy at all: the shim owns the item's stream list, so the pick is
//! `/nuvio/select?index=` (through `route::set_stream_index`) followed by a fresh resolve of the
//! same item — `app::playback::apply_more_action` performs both. The index is the shim's own ranked
//! index, which is why the pages carry `Action::SetStream(index)` and not a URL.
#![allow(non_upper_case_globals)]
use plx_ui::frame::Budget;
use plx_ui::geom::IndexElem;
use plx_machine::machine::{Cx, EntryId, FocusKey, GroupId, Host, Measure};
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Part, Placed, Step};
use plx_ui::form::{Activation, Form, FormId, FormSection, FormTable, RowKey, RowKind};
use plx_ui::page_stack::{
    self, popover_group_of, popover_groups, popover_neighbour, popover_place,
    popover_register_stops, popover_seat, PageStack, PopoverPanel,
};
use plx_ui::panel_motion::PanelMotion;
use plx_ui::table::Row;
use plx_ui::{theme, Rect};

// What the highlighted row does on OK.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    /// flip `crate::app::diagnostics`'s overlay on/off
    ToggleStats,
    /// select a specific Addon stream by index
    SetStream(usize), // <--- Just the index, no String!
    /// **Lab builds only** — snapshot and upload the diagnostic ring (`crate::lab`). Here as well
    /// as in `account_menu` because the account menu is unreachable during playback, and playback
    /// is what a Cloud Test Lab session is usually reproducing.
    SendDiagnostics,
}

/// **A drill-in page of the More menu** — the form's `Dest`. The root is the empty page stack, not
/// a value of this type (`docs/player-submenus.md`).
///
/// The Source page is TWO levels, the way a viewer thinks about a stream: first the addons that
/// answered, then the streams one of them returned. The second level is addressed by the addon's
/// POSITION in the list the page beneath was built from — the names themselves are user and addon
/// data, and a page stack that carried one would have to be re-keyed every time the list moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MorePage {
    /// The addons that answered, each a drill-in ([`MoreRow::OpenAddon`]).
    Source,
    /// One addon's streams, best first ([`MoreRow::Act`] of a [`Action::SetStream`]).
    Addon(usize),
}

impl MorePage {
    /// The title band's text; an addon page is titled by the addon itself.
    fn title(self, streams: &[(usize, String, String)]) -> String {
        match self {
            Self::Source => plx_platform::i18n::msg::widgets_menu_source().to_string(),
            Self::Addon(i) => match crate::source_list::addon_groups(streams).get(i) {
                Some((name, _)) if !name.is_empty() => name.clone(),
                Some(_) => crate::source_list::untagged_source().to_string(),
                None => String::new(),
            },
        }
    }

    /// A stable small number for the replay canon. Never reordered: recordings hash it.
    fn code(self) -> u32 {
        match self {
            Self::Source => 1,
            Self::Addon(i) => 2 + i as u32,
        }
    }
}

/// A row's identity on either page: the Source drill-in on the root, an addon on the Source page,
/// or a row that commits an [`Action`] (a stream on an addon page, an Options switch on the root).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoreRow {
    /// The root's Source row ([`MorePage::Source`]): reads out the stream count.
    OpenSource,
    /// A drill-in to one addon's streams, by its position in [`stream_groups`].
    OpenAddon(usize),
    Act(Action),
}

/// What OK on the highlighted row did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoreOk {
    /// Perform this action; the container dismisses the panel (a stream pick and the Options rows
    /// close it).
    Action(Action),
    /// A Nav row opened a page: the panel stays, with its rows replaced.
    Navigated,
}

type MoreForm = Form<MoreRow, Action, MorePage>;
type MoreTable = FormTable<MoreRow, Action, MorePage>;

/// The menu's whole state, owned by the container that mounts this panel — the modal PHASE and the
/// appear spring belong to `ui::containers::modal::ModalStack` now, not to this struct; `draw`
/// takes the appear fraction as a parameter instead of stepping its own `Popover`.
/// The panel's width is the shared menu rule (`TableView::menu_panel_width`): it hugs the widest
/// row, capped at `MENU_MAX_W`, and every row must fit the cap in every language
/// (`every_row_fits_the_panel_in_every_language`).

pub struct MoreMenuState {
    /// The rows AND the action each one commits, declared together ([`root_form`],
    /// [`source_form`]) — the row SET varies (the streams arrive asynchronously, so the addon
    /// list grows), so a row is found by its identity ([`MoreRow`]'s [`FormId`] key), never by a
    /// position. One table serves the page that is showing. The table is main-thread-only, like
    /// every other panel's.
    form: MoreTable,
    /// The row set the form was last built from (what [`rows_for`] answered), so a live change to
    /// it is noticed and the panel rebuilt and RESIZED ([`Self::refresh`]) rather than left
    /// showing a list that has moved.
    rows: Vec<Action>,
    /// The pages pushed above the root, outermost first (empty = the root): each remembers the row
    /// that opened it and the scroll it was left at ([`page_stack`]).
    pages: PageStack<MorePage, MoreRow>,
    /// The card's resize and the page slide ([`plx_ui::panel_motion`]), the same one the track
    /// menu uses: a row set that changes height animates the top edge, bottom and right stay on the
    /// anchor, and a push or pop slides the two pages.
    motion: PanelMotion,
    /// The ranked stream list the pages were last built from, `(index, label, addon)`. Held so a
    /// pushed page is built from the SAME list its opener was, and so a live change to it can be
    /// noticed ([`Self::refresh_to`]) — the global the route publishes is read once per rebuild.
    streams: Vec<(usize, String, String)>,
    /// The route's revision of that list as of the last rebuild, so the tick's poll is one atomic
    /// load rather than a copy of every label on every frame.
    stream_rev: u32,
}

/// The hand-assigned focus key of each row — none of these is a discriminant, a position or a
/// hash, so reordering the menu moves no key.
impl FormId for Action {
    fn key(&self) -> RowKey {
        RowKey(match self {
            Action::None => 0,
            Action::ToggleStats => 1,
            Action::SendDiagnostics => 2,
            Action::SetStream(i) => 100 + *i as u32,
        })
    }
}

impl FormId for MoreRow {
    fn key(&self) -> RowKey {
        match self {
            // free in `Action`'s key space (0..=2)
            MoreRow::OpenSource => RowKey(3),
            // 0x1000 up: one band per addon, clear of every `SetStream` (100 + index). An addon
            // page's rows are `Act` rows and never collide with a drill-in, so the only keys that
            // must stay apart are these and the actions of the page BENEATH, which they do.
            MoreRow::OpenAddon(i) => RowKey(0x1000 + *i as u32),
            MoreRow::Act(a) => a.key(),
        }
    }
}

/// Is the Source row offered? An empty list has nothing to open onto — a row leading to an empty
/// page would read as a menu that failed to load.
fn source_offered(streams: &[(usize, String, String)]) -> bool {
    !streams.is_empty()
}

/// One addon's streams, as `(ranked index, label)` — what [`MorePage::Addon`]'s page lists.
fn streams_of(streams: &[(usize, String, String)], indices: &[usize]) -> Vec<(usize, String)> {
    indices
        .iter()
        .filter_map(|i| streams.iter().find(|(index, _, _)| index == i))
        .map(|(index, label, _)| (*index, label.clone()))
        .collect()
}

/// The ROOT as a pure form: the Source drill-in (headerless, like the Subtitles root's Style —
/// it reads out how many streams there are), then Options.
fn root_form(rows: &[Action], streams: &[(usize, String, String)]) -> MoreForm {
    let mut options = FormSection::new(plx_platform::i18n::msg::widgets_menu_options());

    // the streams are reached through the Source row, so they are not an option
    for a in rows.iter().copied().filter(|a| !matches!(a, Action::SetStream(_))) {
        options = options.item(MoreRow::Act(a), RowKind::Button, a, row_for(a));
    }

    let source = FormSection::new("").item_if(
        source_offered(streams),
        MoreRow::OpenSource,
        RowKind::Nav(MorePage::Source),
        Action::None,
        Row::new(plx_platform::i18n::msg::widgets_menu_source()).value(streams.len().to_string()),
    );
    Form::new().section(source).section(options)
}

/// **The Source PAGE**: one drill-in row per addon that answered for the item, stating how many
/// streams it offered. The second level is that addon's own page ([`addon_form`]).
///
/// Nothing about playback quality is drawn here: under the shim a stream IS a quality, so a rung
/// list beside the stream list would be a second answer to the same question.
fn source_form(streams: &[(usize, String, String)]) -> MoreForm {
    let addons = crate::source_list::addon_groups(streams)
        .into_iter()
        .enumerate()
        .fold(FormSection::new(""), |sec, (i, (name, indices))| {
            let name = if name.is_empty() { crate::source_list::untagged_source().to_string() } else { name };
            let row = Row::new(name).server_label().server_value().value(indices.len().to_string());
            sec.item(MoreRow::OpenAddon(i), RowKind::Nav(MorePage::Addon(i)), Action::None, row)
        });
    let mut form = Form::new().section(addons);
    if streams.is_empty() {
        form = form.section(FormSection::new("").note(plx_platform::i18n::msg::browse_streams_loading()));
    }
    form
}

/// One addon's page: its streams, best first. Nothing is checked — the shim's selected index is
/// not read back here — and every row commits its own [`Action::SetStream`].
fn addon_form(group: usize, streams: &[(usize, String, String)]) -> MoreForm {
    let indices = crate::source_list::addon_groups(streams)
        .get(group)
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let sec = streams_of(streams, &indices)
        .into_iter()
        .fold(FormSection::new(""), |sec, (index, label)| {
            let a = Action::SetStream(index);
            sec.item(MoreRow::Act(a), RowKind::Choice, a, Row::new(label).server_label())
        });
    Form::new().section(sec)
}

impl MoreMenuState {
    /// `on_open`: open straight on the Source page (the failure read-out's OK), no slide.
    fn open_focused(on_open: bool) -> Self {
        let rows = rows_for();
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        form.table.compact = true; // a short action list — BODY labels, like the profile menu
        form.table.min_panel_w = theme::layout::PLAYER_MENU_MIN_W;
        let streams = plx_media::route::available_streams();
        form.set_or_open(root_form(&rows, &streams), None);
        let mut st = MoreMenuState {
            form,
            rows,
            pages: PageStack::new(),
            motion: PanelMotion::new(),
            streams,
            stream_rev: plx_media::route::streams_rev(),
        };
        // An on_open entry (the failure screen's OK) opens straight on the Source page, no slide:
        // the viewer is fixing a bad decision, not browsing. Nothing is focused in particular —
        // there is no current rung to land on, and the first addon is the sensible start.
        if on_open && source_offered(&st.streams) {
            st.push(MorePage::Source);
            st.motion.cancel_slide();
        }
        st
    }

    /// The page showing, `None` at the root.
    #[cfg(any(test, feature = "test-support"))]
    pub fn page(&self) -> Option<MorePage> {
        self.pages.top()
    }

    /// The highlighted row's identity, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn sel_id(&self) -> Option<MoreRow> {
        self.form.selected_id().copied()
    }

    /// The form of a pushed page, from the panel's own read-outs.
    fn page_form(&self, page: MorePage) -> MoreForm {
        match page {
            MorePage::Source => source_form(&self.streams),
            MorePage::Addon(i) => addon_form(i, &self.streams),
        }
    }

    /// The explicit initial focus of a pushed page: none for either — the root's opener is restored
    /// by a pop and a fresh push starts on the page's opening row.
    fn page_initial(&self, _page: MorePage) -> Option<MoreRow> {
        None
    }

    /// Open `page` above the current one: remember the opener and its scroll, install the page's
    /// rows (the pill snaps, the scroll returns to the top, focus lands on [`Self::page_initial`])
    /// and its title band, and start the slide.
    fn push(&mut self, page: MorePage) {
        let Some(return_id) = self.form.selected_id().copied() else { return };
        self.pages.push(page, return_id, self.form.table.scroll_pos());
        let leaving = page_stack::leave_page(&mut self.form);
        let form = self.page_form(page);
        self.form.open(form, self.page_initial(page).as_ref());
        // after the sections: the band moves every row, so the pill is re-jumped onto the focused one
        self.form.table.set_title(Some(page.title(&self.streams)));
        self.motion.begin_slide(leaving, 1.0);
    }

    /// Pop the top page: the page beneath comes back exactly as it was left — the opener focused by
    /// id, the scroll reinstated ([`FormTable::restore`]). `false` at the root (nothing popped).
    pub fn pop(&mut self) -> bool {
        let Some(saved) = self.pages.pop() else { return false };
        let leaving = page_stack::leave_page(&mut self.form);
        self.motion.begin_slide(leaving, -1.0);
        // the page BENEATH the one that just left: the root, or the Source page an addon came from
        let (form, title) = match self.pages.top() {
            Some(page) => (self.page_form(page), Some(page.title(&self.streams))),
            None => (root_form(&self.rows, &self.streams), None),
        };
        // Land on a row we can NAME: the opener when it is still on offer (a re-rank can drop it),
        // else the page's first focusable row — never the table's own opening slot, which may be
        // an inert note, and never whatever the engine happens to still hold.
        let landing = if form.contains(&saved.return_id) {
            Some(saved.return_id)
        } else {
            form.slots().into_iter().flatten().next().map(|b| b.id)
        };
        self.form.restore(form, landing.as_ref(), saved.scroll);
        self.form.table.set_title(title);
        true
    }

    /// **RIGHT**: on a Nav row it enters — the same as OK; anywhere else nothing happens (this
    /// popover has no tabs to switch).
    pub fn on_right(&mut self) {
        let sel = self.form.table.sel.max(0) as usize;
        if matches!(self.form.binding_at(sel).map(|b| &b.kind), Some(RowKind::Nav(_))) {
            if let Some(Activation::Push(dest)) = self.form.activate(sel) {
                self.push(dest);
            }
        }
    }

    /// **The replay canon**: the page path (each pushed page and the row that opened it) and the
    /// selected row's [`RowKey`] — not its index, which two pages share.
    pub fn canon(&self, c: &mut plx_machine::machine::Canon) {
        self.pages.canon(c, MorePage::code, |r| r.key().0);
        c.u32(self.form.key_at(self.form.table.sel.max(0) as usize).map_or(u32::MAX, |k| k.0));
    }

    /// What the `moreosc` trigger needs to choose its next key: how many pages are pushed.
    pub fn osc_depth(&self) -> usize {
        self.pages.len()
    }

    /// **Follow a live change**: rebuild the page that is showing when the ranked stream list no
    /// longer answers what it was built from — which is what makes the Source pages fill in as the
    /// addons reply. On the root the form is rebuilt keeping the focused row by identity; on a
    /// pushed page it is refreshed IN PLACE (focus kept by id), and a page the change emptied pops
    /// back one level instead of leaving the viewer on a blank sheet. The panel's height follows,
    /// and the card animates to it ([`Self::update`]). Returns whether it rebuilt.
    pub fn refresh(&mut self) -> bool {
        let rev = plx_media::route::streams_rev();
        // `rows_for` is a function of the stream list and nothing else, so when the list has not
        // moved there is nothing the page could draw differently — which is what keeps the tick's
        // poll down to one atomic load, instead of copying every stream label each frame.
        if rev == self.stream_rev {
            return false;
        }
        self.stream_rev = rev;
        let streams = plx_media::route::available_streams();
        self.refresh_to(rows_for(), streams)
    }

    /// [`Self::refresh`] against explicit answers (what a test or a probe supplies in place of the
    /// live route).
    fn refresh_to(&mut self, rows: Vec<Action>, streams: Vec<(usize, String, String)>) -> bool {
        if rows == self.rows && streams == self.streams {
            return false;
        }
        self.rows = rows;
        self.streams = streams;
        let addons = crate::source_list::addon_groups(&self.streams);
        if let Some(first) = self.pages.first().copied() {
            // An addon page is addressed by the addon's POSITION in the list the Source page was
            // built from. A re-rank that drops that addon leaves nothing to show and the position
            // naming somebody else, so the page goes back one level instead of quietly listing a
            // different source under the same title.
            if matches!(self.pages.top(), Some(MorePage::Addon(i)) if i >= addons.len()) {
                let saved = self.pages.pop();
                let form = self.page_form(MorePage::Source);
                self.form.restore(form, saved.as_ref().map(|s| &s.return_id), saved.map_or(0.0, |s| s.scroll));
                self.form.table.set_title(Some(MorePage::Source.title(&self.streams)));
                self.motion.cancel_slide();
                return true;
            }
            if source_offered(&self.streams) {
                let page = self.pages.top().unwrap_or(MorePage::Source);
                let form = self.page_form(page);
                let selected = self.form.selected_id().copied();
                self.form.refresh_with(form, selected.as_ref(), None);
                // the addon list may have re-ordered: the band names the page as it is NOW
                self.form.table.set_title(Some(page.title(&self.streams)));
            } else {
                // nothing left to pick from: back to the root, restoring the opener and scroll the
                // stack saved at its first push
                self.pages.clear();
                let form = root_form(&self.rows, &self.streams);
                self.form.restore(form, Some(&first.return_id), first.scroll);
                self.form.table.set_title(None);
                self.motion.cancel_slide();
            }
        } else {
            let keep = self.form.selected_id().copied();
            self.form.set_or_open(root_form(&self.rows, &self.streams), keep.as_ref());
        }
        true
    }

    pub fn new() -> Self {
        Self::open_focused(false)
    }

    /// The same menu, opened ON the Source page.
    ///
    /// The terminal playback screen has no transport discs, so OK must reach the stream list
    /// directly — a viewer arriving here is fixing a bad decision, not browsing the list. It is
    /// still the SAME menu, form and action map as the ordinary `…` menu; only the page it opens
    /// on differs (BACK pops to the root, as from any pushed page).
    pub fn new_source() -> Self {
        Self::open_focused(true)
    }

    /// Every row's focus key, in drawn order (for a test that walks the rows).
    #[cfg(any(test, feature = "test-support"))]
    pub fn keys(&self) -> Vec<u32> {
        (0..self.form.table.n_rows() as usize).filter_map(|i| self.form.key_at(i).map(|k| k.0)).collect()
    }

    /// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the
    /// key ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes
    /// nothing else, so without this the fingerprint records the panel opening and closing and
    /// nothing between.
    pub fn sel(&self) -> i32 {
        self.form.table.sel
    }

    /// **Write back the engine's own focus cursor** (restructure phase 12): the Column group
    /// [`MoreMenuPart`] answers is the source of geometry, but the ENGINE owns the current element
    /// (§7.3 step 5) — the owner's `step` is the only place that mutates in response to a
    /// `FocusMoved`, and this is `screens::player::overlay::PlayerOverlayScreen::step`'s write.
    /// Both a D-pad move AND a pointer hover reach here now — hover parks focus THROUGH the engine
    /// (§7.5), replacing this menu's own `pointer_focus`.
    /// Returns whether a row with that key is on the page showing.
    pub fn focus_key(&mut self, elem: u32) -> bool {
        let Some(i) = self.form.index_of_key(RowKey(elem)) else { return false };
        self.form.table.sel = i as i32;
        true
    }

    /// OK on the highlighted row: an action to perform (the container dismisses the panel
    /// afterward), or a Nav row that opened its page.
    pub fn on_ok(&mut self) -> MoreOk {
        match self.form.selected_id().and_then(|id| self.form.index_of(id)).and_then(|i| self.form.activate(i)) {
            Some(Activation::Action(act)) => MoreOk::Action(act),
            Some(Activation::Push(page)) => {
                self.push(page);
                MoreOk::Navigated
            }
            None => MoreOk::Action(Action::None),
        }
    }

    /// Bottom-right, above the control row — anchored to the `…` disc that opened it, the way the
    /// track menu is anchored to the pair beside it. Shares the track menu's right margin
    /// (`player_hud::CTRL_RIGHT`, the discs' own edge) and its bottom edge, so opening one after the
    /// other does not make the panel hop. This is the NATURAL (layout) rect, cached against the
    /// table's `layout_rev`; [`Self::shown_rect`] is what is on screen while the card resizes.
    fn panel_rect(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        self.motion.natural(self.form.table.layout_rev(), || {
            let pw = self.form.table.menu_panel_width(measure);
            let px = crate::player_hud::CTRL_RIGHT - pw;
            let bottom = theme::layout::PLAYER_MENU_BOTTOM; // ~28px above the discs, as track_menu
            let ph = self.panel_h();
            Rect::new(px, bottom - ph, pw, ph)
        })
    }

    /// The panel's height alone. The ceiling was 320 while this menu held one row, and it was
    /// invisible then. With the Quality ladder beside it `measured_height()` can reach 600 when
    /// Auto is enabled — the tallest page is the Quality page: its title band plus seven
    /// rungs, AND the table's own top/bottom padding — so a 320 cap put four of nine rows on screen and silently scrolled the rest,
    /// which is a picker whose options you cannot see.
    ///
    /// The cap is a FRACTION of the room the panel has rather than a subtraction from it: the panel
    /// is anchored at `bottom` and grows upward, so `bottom` IS the space, and 0.86 of it leaves a
    /// clear margin at the top of the frame while comfortably clearing 600. Reaching for a
    /// `bottom - <margin>` literal is what put the first version of this line 4px UNDER the content
    /// — the margin was derived from the 560 of content and forgot the 40 of padding, so the last
    /// rung was clipped until you scrolled: the same symptom, one row deep instead of five. Past
    /// the cap it scrolls, which is what `TableView` is for.
    fn panel_h(&self) -> f32 {
        let bottom = theme::layout::PLAYER_MENU_BOTTOM;
        self.form.table.measured_height().clamp(120.0, bottom * 0.86)
    }

    /// The card as drawn this frame: top and left on their springs toward [`Self::panel_rect`].
    fn shown_rect(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        self.motion.shown(self.panel_rect(measure))
    }

    /// Is the card still resizing? The player overlay holds the pointer while it is
    /// (`Screen::pointer_held`).
    pub fn transitioning(&self) -> bool {
        self.motion.transitioning()
    }

    /// The opening page's strings (the root, or the Quality page for a quality entry), queued on
    /// the frame the panel mounts — see `TrackMenuState::warm_open`, which this mirrors.
    pub fn warm_open(&self, measure: &dyn plx_machine::machine::Measure) {
        self.motion.warm_open(&self.form.table, || self.panel_rect(measure), measure);
    }

    pub fn update(&mut self, dt: f32, measure: &dyn plx_machine::machine::Measure) {
        self.refresh();
        // A page can be installed before its rows exist (and the engine's key can name none of
        // them): the cursor must not be left on nothing while there is a row to focus.
        self.seat_valid_focus();
        // `update` subtracts its own top/bottom padding now — pass the panel's raw height.
        let natural = self.panel_rect(measure);
        self.form.table.update(dt, natural.h);
        self.motion.step(dt, natural);
        self.motion.prewarm_text(natural, &self.form.table, measure);
    }

    /// The index of the first focusable row on the page showing; `None` when the page has
    /// nothing to focus (an empty Source page's waiting note is inert).
    fn first_focusable(&self) -> Option<usize> {
        (0..self.form.table.n_rows() as usize).find(|&i| self.form.key_at(i).is_some())
    }

    /// **Keep the cursor on a row, every frame.** The form is installed (a page push, a live
    /// rebuild, a restore) before the rows it will show have all arrived, and an installed cursor
    /// can sit on an inert slot or past the end: nothing is highlighted, OK commits nothing
    /// (`on_ok` reads the selected id) and the reader has to nudge a direction to make the menu
    /// answer. Re-seat on the first focusable row until the cursor is valid; a valid cursor is
    /// left exactly where it is, so this never fights a move the engine made.
    fn seat_valid_focus(&mut self) {
        if self.form.selected_id().is_some() {
            return;
        }
        if let Some(i) = self.first_focusable() {
            self.form.table.sel = i as i32;
        }
    }

    pub fn draw(&mut self, appear: f32, measure: &dyn plx_machine::machine::Measure) {
        // rises INTO place from below, toward the disc that opened it. The dim under it is the
        // container's (`PlayerOverlayScreen::scrim`, `theme::underlay::DIM_SHEET`), painted at the
        // end of the player's page pass — not here.
        let p = plx_ui::Painter::root()
            .alpha(appear)
            .translate(0.0, 16.0 * (1.0 - appear));
        self.motion.draw(p, self.panel_rect(measure), 24.0, &self.form.table, measure);
    }
}

/// **The Engine-shaped view of this popover** (restructure phase 12): one `Column` focus group
/// over the rows of the page showing (the root's Quality drill-in plus Options, or the Quality
/// page's rungs), built fresh by
/// `screens::player::overlay::PlayerOverlayScreen` each frame from a `&MoreMenuState` — the same
/// borrowed-view shape `ui::table_screen::TablePart`/`ui::geom::Table` use for the other panels
/// that are already a bare `TableView` in a frame, so this popover answers the same
/// [`Focusable`]/[`Part`] query protocol they do. UP and DOWN `Stop` at the ends; LEFT and RIGHT
/// are `EdgeRule::Screen`, re-delivered to `PlayerOverlayScreen::edge_key`, which pops a pushed
/// page on LEFT and enters a Nav row (Quality) on RIGHT, and otherwise swallows the key so focus
/// never leaves the modal surface.
///
/// **`state` is a SHARED reference** — every [`Focusable`] method here is a pure read (`&self`),
/// and the owning screen's own `Focusable` impl only ever has `&self` too (§7.1: "the engine never
/// mutates a screen"), so a mutable field would make this type unconstructable from there. The
/// actual paint (`MoreMenuState::draw`) stays a direct call on the owned `Panel` from
/// `PlayerOverlayScreen::draw`'s `&mut self`; [`Part::draw`] below only registers stops.
pub struct MoreMenuPart<'a> {
    pub state: &'a MoreMenuState,
    pub entry: EntryId,
    pub group: GroupId,
}

impl PopoverPanel for MoreMenuState {
    type Id = MoreRow;
    type Act = Action;
    type Dest = MorePage;
    fn form(&self) -> &FormTable<Self::Id, Self::Act, Self::Dest> {
        &self.form
    }
    fn motion(&self) -> &PanelMotion {
        &self.motion
    }
    fn panel_rect(&self, measure: &dyn Measure) -> Rect {
        MoreMenuState::panel_rect(self, measure)
    }
    fn shown_rect(&self, measure: &dyn Measure) -> Rect {
        MoreMenuState::shown_rect(self, measure)
    }
}

impl<H: Host> Focusable<H> for MoreMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        popover_groups(self.state, self.group, cx.measure, out);
    }
    fn group_of(&self, key: &H::Elem, _cx: &Cx<'_, H>) -> Option<GroupId> {
        popover_group_of(self.state, key, self.group)
    }
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, _cx: &Cx<'_, H>) -> Step<H::Elem> {
        popover_neighbour(self.state, self.entry, key, dir)
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        popover_place(self.state, key, cx.measure)
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        // A live key stays where it is. One that names no row of the page now showing — the opener
        // after a push, the rung after a pop, a row of a shorter set — settles on the panel's OWN
        // cursor (where the push/pop/refresh already decided focus belongs), else the opening row.
        let kept = want
            .elem
            .index()
            .filter(|k| self.state.form.index_of_key(RowKey(*k)).is_some());
        let key = kept
            .or_else(|| self.state.form.selected_key().map(|k| k.0))
            .or_else(|| self.state.form.opening_key().map(|k| k.0));
        FocusKey { entry: self.entry, elem: H::Elem::of_index(key.unwrap_or(0)) }
    }
    fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        popover_seat(self.state, self.entry)
    }
}

impl<H: Host> Part<H> for MoreMenuPart<'_>
where
    H::Elem: IndexElem,
{
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, H>) {}
    /// Registers every selectable row's stop and the title band's ([`popover_register_stops`]); the
    /// popover's own paint happens directly on the owned state from `PlayerOverlayScreen::draw`
    /// (struct doc above).
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, _rect: Rect) {
        popover_register_stops(self.state, self.entry, f);
    }
}

/// Every row the menu can offer, in order and ACROSS SECTIONS. A free function (rather than a
/// literal inside [`MoreMenuState::open_focused`]) so the index mapping [`MoreMenuState::on_ok`]
/// relies on is one testable value.
///
/// **The order here is the whole contract**, because [`TableView`]'s `sel` is a single flat index
/// over every row of every section: this list must be built in exactly the order
/// [`MoreMenuState::open_focused`] pushes rows, or a press commits its neighbour.
///
/// The streams lead — they are what this popover is for — and everything else is an Option. An
/// empty list adds nothing: the Source page says it is still looking (`browse_streams_loading`),
/// and the root then has no Source row at all rather than a row onto an empty page.
fn rows_for() -> Vec<Action> {
    let mut v: Vec<Action> = Vec::new();

    for (index, _, _) in plx_media::route::AVAILABLE_STREAMS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
    {
        v.push(Action::SetStream(*index));
    }

    v.push(Action::ToggleStats);
    if plx_platform::labcfg::menu_row_enabled() {
        v.push(Action::SendDiagnostics);
    }
    v
}

fn label(a: Action) -> std::borrow::Cow<'static, str> {
    match a {
        Action::ToggleStats => plx_platform::i18n::msg::widgets_menu_stats().into(),
        Action::SetStream(index) => {
            let streams = plx_media::route::AVAILABLE_STREAMS.lock().unwrap();
            streams.iter()
                .find(|(i, _, _)| *i == index)
                .map(|(_, label, _)| label.clone())
                .unwrap_or_else(|| format!("Stream #{index}"))
                .into()
        }
        Action::SendDiagnostics => plx_platform::i18n::msg::widgets_menu_diagnostics().into(),
        Action::None => "".into(),
    }
}

/// Where the Stats for nerds switch reads its state. The read-out's flag is owned by
/// `app::diagnostics`, whose caller names this menu's [`Action`]s, so the application registers a
/// reader at boot ([`install_stats_reader`], from `app::enter_application`) instead of this
/// module naming `app`. Unset it reads OFF, which is what the flag itself reads before anything has
/// toggled it.
static STATS_READER: std::sync::OnceLock<fn() -> bool> = std::sync::OnceLock::new();

/// Register the reader for the Stats for nerds switch. The first registration wins; the loop makes
/// exactly one, before any screen exists.
pub fn install_stats_reader(read: fn() -> bool) {
    let _ = STATS_READER.set(read);
}

fn stats_on() -> bool {
    STATS_READER.get().is_some_and(|read| read())
}

/// Whether the SWITCH a row names is currently on. It reaches the row as [`Row::toggle`] and so
/// draws as the WORD `On`/`Off` at the trailing edge. (Named `checked` until 2026-08-21, from the
/// row builder it does not call — a name that read as a promise of the leading mark an Options row
/// deliberately does not draw.)
fn is_on(a: Action) -> bool {
    match a {
        Action::ToggleStats => stats_on(),
        Action::SendDiagnostics | Action::None | Action::SetStream(_) => false,
    }
}

/// One row, drawn in the idiom its ACTION calls for. Free-standing (rather than inline in the form
/// builders) so the idioms are decided in one place: a switch gets the trailing word, a stream is a
/// plain choice row.
fn row_for(a: Action) -> Row {
    match a {
        Action::SetStream(_) => Row::new(label(a)),
        _ => Row::new(label(a)).toggle(is_on(a)),
    }
}

/// The panel at its TALLEST, for the overscan audit ([`plx_ui::consts::SAFE`]).
#[cfg(any(test, feature = "test-support"))]
pub fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let (pw, ph) = (plx_ui::table::MENU_MAX_W, 320.0f32);
    let bottom = theme::layout::PLAYER_MENU_BOTTOM;
    out.push((
        "… overflow menu panel",
        Rect::new(crate::player_hud::CTRL_RIGHT - pw, bottom - ph, pw, ph),
    ));
}

/// The index→action mapping, which is the only part of a popover that is testable off the main
/// thread: a real `MoreMenuState` owns `TableView`/its row list, and both are main-thread-only,
/// like every other panel's state.
#[cfg(test)]
mod tests {
    use super::*;

    type Streams = Vec<(usize, String, String)>;

    /// A ranked stream list with two addons answering for one item: Torrentio twice (the best
    /// stream and a third), Sootio once. The order is ranked, so this is also the order the pages
    /// must show.
    fn two_addons() -> Streams {
        vec![
            (3, "4k DV".to_string(), "Torrentio".to_string()),
            (5, "1080p".to_string(), "Sootio".to_string()),
            (7, "720p".to_string(), "Torrentio".to_string()),
        ]
    }

    /// The menu built over an explicit row list and stream list — no process-global state, so
    /// these tests do not need a serial lock or a route.
    fn state(rows: Vec<Action>, streams: &[(usize, String, String)]) -> MoreMenuState {
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        form.table.compact = true;
        form.set_or_open(root_form(&rows, streams), None);
        MoreMenuState {
            form,
            rows,
            pages: PageStack::new(),
            motion: PanelMotion::new(),
            streams: streams.to_vec(),
            stream_rev: 0,
        }
    }

    /// The root over `two_addons`.
    fn menu() -> MoreMenuState {
        state(rows_for(), &two_addons())
    }

    /// [`menu`] with the Source page pushed and its slide skipped.
    fn source_page() -> MoreMenuState {
        let mut st = menu();
        st.focus_key(MoreRow::OpenSource.key().0);
        assert_eq!(st.on_ok(), MoreOk::Navigated);
        st.motion.cancel_slide();
        st
    }

    /// The actions of the rows showing, in table order, read back by identity (a drill-in row
    /// commits nothing and is not one).
    fn order(st: &MoreMenuState) -> Vec<Action> {
        (0..st.form.table.n_rows() as usize)
            .filter_map(|i| match st.form.id_at(i) {
                Some(MoreRow::Act(a)) => Some(*a),
                _ => None,
            })
            .collect()
    }

    /// Every row identity showing, in table order.
    fn ids(st: &MoreMenuState) -> Vec<MoreRow> {
        (0..st.form.table.n_rows() as usize)
            .filter_map(|i| st.form.id_at(i).copied())
            .collect()
    }

    /// The text a row reads out at its trailing edge (a Nav row's current value).
    fn value_of(st: &mut MoreMenuState, id: MoreRow) -> Option<String> {
        let i = st.form.index_of(&id)?;
        st.form.table.row_mut(i as i32).and_then(|r| r.value.clone())
    }

    #[test]
    fn every_row_has_a_label() {
        for a in rows_for() {
            assert!(!label(a).is_empty(), "{a:?} would draw a blank row");
        }
    }

    /// **There is no playback-quality ladder in this popover.** Under the shim a stream IS a
    /// quality, so a rung list would be a second answer to the same question; the rung variant
    /// does not exist, and the only rows are the streams themselves and the Options switches.
    #[test]
    fn the_menu_offers_no_quality_rung() {
        let rows = rows_for();
        let st = menu();
        assert_eq!(
            order(&st),
            rows,
            "the root's Actions are exactly `rows_for`, and the variant that named a rung is gone"
        );
        // the ladder is still a real setting, reached from Settings > Playback
        assert!(!plx_media::route::QUALITY_LADDER.is_empty());
    }

    /// **The root leads with the Source drill-in and holds no streams of its own** — the whole
    /// point of the sub-menu. Its value is how many streams are on offer.
    #[test]
    fn the_root_leads_with_the_source_row_and_holds_no_streams() {
        let mut st = menu();
        let shown = ids(&st);
        assert_eq!(shown[0], MoreRow::OpenSource, "Source leads");
        assert!(
            !shown.iter().any(|i| matches!(i, MoreRow::Act(Action::SetStream(_)))),
            "a stream is reached through its addon, never listed on the root: {shown:?}"
        );
        assert!(shown.contains(&MoreRow::Act(Action::ToggleStats)));
        assert_eq!(
            value_of(&mut st, MoreRow::OpenSource),
            Some("3".into()),
            "the row reads out the stream count"
        );
        assert_eq!(
            st.form.table.title(),
            None,
            "the root carries no title band"
        );
    }

    /// **The Source page is one row per addon, in ranked order**, with the addon's own stream
    /// count as its value — and each of those opens a page of that addon's streams alone.
    #[test]
    fn the_source_page_lists_one_row_per_addon_and_each_page_its_own_streams() {
        let streams = two_addons();
        assert_eq!(
            crate::source_list::addon_groups(&streams),
            vec![
                ("Torrentio".to_string(), vec![3, 7]),
                ("Sootio".to_string(), vec![5]),
            ]
        );

        let mut st = source_page();
        assert_eq!(st.page(), Some(MorePage::Source));
        assert_eq!(
            ids(&st),
            vec![MoreRow::OpenAddon(0), MoreRow::OpenAddon(1)],
            "one drill-in per addon, nothing else"
        );
        assert_eq!(
            st.form.table.title(),
            Some(plx_platform::i18n::msg::widgets_menu_source()),
            "the band names the page"
        );
        assert_eq!(value_of(&mut st, MoreRow::OpenAddon(0)), Some("2".into()));
        assert_eq!(value_of(&mut st, MoreRow::OpenAddon(1)), Some("1".into()));

        // into the second addon: its page holds ITS streams and nothing else
        st.focus_key(MoreRow::OpenAddon(1).key().0);
        assert_eq!(st.on_ok(), MoreOk::Navigated);
        st.motion.cancel_slide();
        assert_eq!(st.page(), Some(MorePage::Addon(1)));
        assert_eq!(st.form.table.title(), Some("Sootio"), "the band names the addon");
        assert_eq!(
            order(&st),
            vec![Action::SetStream(5)],
            "only Sootio's stream, and it commits its own ranked index"
        );
        assert_eq!(st.on_ok(), MoreOk::Action(Action::SetStream(5)));
    }

    /// An empty list draws no Source row at all — a row onto an empty page would read as a menu
    /// that failed to load — and the Source page says it is still looking.
    #[test]
    fn nothing_to_pick_draws_no_source_row_and_the_page_says_so() {
        let st = state(rows_for(), &[]);
        assert!(
            st.form.index_of(&MoreRow::OpenSource).is_none(),
            "no streams, no page to open"
        );
        let page = source_form(&[]);
        assert!(
            page.slots().iter().all(Option::is_none),
            "the waiting caption is an inert note, never a focusable row"
        );

        // the background fetch lands under the open root: the row appears
        let mut st = st;
        assert!(st.refresh_to(rows_for(), two_addons()));
        assert!(st.form.index_of(&MoreRow::OpenSource).is_some());
    }

    /// **BACK walks one page at a time** — an addon page to the Source page, the Source page to
    /// the root — restoring the opener and the band at each step. At the root it does nothing.
    #[test]
    fn back_walks_one_page_at_a_time_from_an_addon() {
        let mut st = source_page();
        st.focus_key(MoreRow::OpenAddon(1).key().0);
        assert_eq!(st.on_ok(), MoreOk::Navigated);
        st.motion.cancel_slide();
        assert_eq!(st.page(), Some(MorePage::Addon(1)));
        assert_eq!(st.on_ok(), MoreOk::Action(Action::SetStream(5)));

        assert!(st.pop());
        assert_eq!(st.page(), Some(MorePage::Source));
        assert_eq!(
            st.form.table.title(),
            Some(plx_platform::i18n::msg::widgets_menu_source())
        );
        assert_eq!(
            st.form.selected_id(),
            Some(&MoreRow::OpenAddon(1)),
            "the opener comes back focused"
        );
        assert!(st.pop());
        assert_eq!(st.page(), None);
        assert_eq!(st.form.table.title(), None);
        assert_eq!(st.form.selected_id(), Some(&MoreRow::OpenSource));
        assert!(!st.pop(), "the root has nothing to pop");
    }

    /// **A cursor that names no row does not survive a frame.** The engine can leave a key behind
    /// the page now showing, and a page can be installed before its rows land; either way the
    /// cursor sits on nothing — no pill, and OK reads no id, so the menu looks dead until a
    /// direction nudges it. The tick re-seats it on the first focusable row.
    #[test]
    fn an_invalid_cursor_is_reseated_on_a_row_by_the_tick() {
        let _serial = plx_base::testlock::serial();
        const DT: f32 = 1.0 / 60.0;
        let m = plx_base::fontcov::advances::ShippedMeasure;
        let mut st = menu();
        st.stream_rev = plx_media::route::streams_rev(); // the tick's poll is not under test
        st.form.table.sel = st.form.table.n_rows(); // one past the last row
        assert_eq!(st.form.selected_id(), None, "the cursor names no row");
        assert_eq!(st.on_ok(), MoreOk::Action(Action::None), "so OK commits nothing");
        st.update(DT, &m);
        assert_eq!(
            st.form.selected_id(),
            Some(&MoreRow::OpenSource),
            "the tick puts the cursor back on the first row"
        );
        assert_eq!(st.on_ok(), MoreOk::Navigated, "and OK answers again");
    }

    /// **A pop lands on a row the returned page still has.** A re-rank can drop the addon that
    /// opened the page, so the opener is looked up on the page being returned to; when it is
    /// gone the cursor takes that page's first focusable row, never the old page's position.
    #[test]
    fn a_pop_lands_on_a_row_of_the_page_it_returns_to() {
        let mut st = source_page();
        st.focus_key(MoreRow::OpenAddon(1).key().0);
        assert_eq!(st.on_ok(), MoreOk::Navigated);
        st.motion.cancel_slide();
        // by BACK, the second addon has answered no more: its row, the opener, is gone
        st.streams = vec![(3, "4k DV".to_string(), "Torrentio".to_string())];

        assert!(st.pop());
        assert_eq!(st.page(), Some(MorePage::Source));
        assert_eq!(
            st.form.selected_id(),
            Some(&MoreRow::OpenAddon(0)),
            "the returned page's first row, not the vanished opener"
        );
        assert_eq!(st.on_ok(), MoreOk::Navigated, "and it commits on OK");
    }

    /// RIGHT enters a Nav row and does nothing anywhere else (this popover has no tabs).
    #[test]
    fn right_enters_a_nav_row_and_nothing_else() {
        let mut st = menu();
        st.focus_key(Action::ToggleStats.key().0);
        st.on_right();
        assert_eq!(st.page(), None, "RIGHT on an Options row stays put");
        st.focus_key(MoreRow::OpenSource.key().0);
        st.on_right();
        assert_eq!(st.page(), Some(MorePage::Source), "RIGHT on the Source row enters it");
    }

    /// Pressing each row commits ITS action, addressed by identity — never by a position.
    #[test]
    fn a_focused_row_commits_its_own_action() {
        let rows = rows_for();
        let mut st = state(rows.clone(), &two_addons());
        for a in rows.iter().copied().filter(|a| !matches!(a, Action::SetStream(_))) {
            st.focus_key(a.key().0);
            assert_eq!(st.on_ok(), MoreOk::Action(a));
        }
        let mut st = source_page();
        st.focus_key(MoreRow::OpenAddon(0).key().0);
        assert_eq!(st.on_ok(), MoreOk::Navigated);
        st.motion.cancel_slide();
        for a in [Action::SetStream(3), Action::SetStream(7)] {
            st.focus_key(a.key().0);
            // `app::playback::apply_more_action` is the one place a `SetStream` is performed
            assert_eq!(st.on_ok(), MoreOk::Action(a), "{a:?}");
        }
    }

    /// Reordering the rows moves no key: every row keeps the key (and so the action) it had.
    #[test]
    fn a_reordered_menu_resolves_every_key_to_the_same_action() {
        let mut rows = rows_for();
        rows.reverse();
        let st = state(rows.clone(), &two_addons());
        for a in rows {
            let i = st.form.index_of_key(a.key()).expect("every row keeps its key");
            assert_eq!(st.form.id_at(i), Some(&MoreRow::Act(a)));
            match st.form.activate(i) {
                Some(Activation::Action(got)) => assert_eq!(got, a),
                _ => panic!("{a:?} must commit its own action"),
            }
        }
    }

    /// A key that names no row commits nothing, never a neighbour's action.
    #[test]
    fn an_unknown_key_is_none_not_a_neighbour() {
        let mut st = menu();
        let before = st.sel();
        assert!(!st.focus_key(0xdead), "an unknown key reports it seated nothing");
        assert_eq!(st.sel(), before, "an unknown key moves nothing");
        assert!(st.focus_key(MoreRow::OpenSource.key().0), "the Source row is seatable");
    }

    /// Every row's key is distinct and below the band, and the title band's pointer key is none
    /// of them.
    #[test]
    fn every_key_is_distinct_and_below_the_band() {
        let mut all: Vec<MoreRow> = (0..4).map(MoreRow::OpenAddon).collect();
        all.extend([
            MoreRow::OpenSource,
            MoreRow::Act(Action::ToggleStats),
            MoreRow::Act(Action::SendDiagnostics),
            MoreRow::Act(Action::SetStream(3)),
            MoreRow::Act(Action::SetStream(7)),
        ]);
        let mut keys: Vec<u32> = all.iter().map(|a| a.key().0).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all.len());
        assert!(keys.iter().all(|k| *k < plx_ui::table_screen::BAND_BASE));
        assert!(page_stack::is_title_key(page_stack::TITLE_KEY));
        assert!(keys.iter().all(|k| !page_stack::is_title_key(*k)));
    }

    /// **The replay canon tells the root, the Source page and an addon page apart**, and one
    /// selected row from another: the same index on two pages must not hash alike.
    #[test]
    fn the_canon_tells_the_root_and_the_pages_apart() {
        use plx_machine::machine::Canon;
        let fp = |st: &MoreMenuState| {
            let mut c = Canon::new();
            st.canon(&mut c);
            c.finish()
        };
        let root = menu();
        let source = source_page();
        assert_ne!(fp(&root), fp(&source), "depth and opener are in the canon");
        assert_eq!(fp(&source), fp(&source_page()), "the same state hashes the same");
        let mut moved = source_page();
        moved.focus_key(MoreRow::OpenAddon(1).key().0);
        assert_ne!(fp(&source), fp(&moved), "the selected KEY is in the canon");

        let mut addon = source_page();
        addon.focus_key(MoreRow::OpenAddon(1).key().0);
        assert_eq!(addon.on_ok(), MoreOk::Navigated);
        assert_ne!(fp(&addon), fp(&moved), "the second level is its own page");
    }

    /// **A live change to the stream list refreshes the open page in place**: focus stays on the
    /// row the viewer was on, and the band follows a re-ordered addon list.
    #[test]
    fn a_live_stream_change_refreshes_the_open_page_in_place() {
        let mut st = source_page();
        st.focus_key(MoreRow::OpenAddon(1).key().0);
        assert_eq!(st.on_ok(), MoreOk::Navigated);
        st.motion.cancel_slide();
        assert_eq!(st.page(), Some(MorePage::Addon(1)));
        // Sootio answers once more: the list this page was built from has changed under it
        let mut longer = two_addons();
        longer.push((9, "720p".to_string(), "Sootio".to_string()));
        assert!(st.refresh_to(rows_for(), longer.clone()), "a longer list is a change");
        assert!(!st.refresh_to(rows_for(), longer.clone()), "and only once");
        assert_eq!(st.page(), Some(MorePage::Addon(1)), "still on the page");
        assert_eq!(st.form.table.title(), Some("Sootio"), "the band stays");
        assert_eq!(
            order(&st),
            vec![Action::SetStream(5), Action::SetStream(9)],
            "the new stream joined the page it belongs to, in ranked order"
        );
        assert_eq!(
            st.form.selected_id(),
            Some(&MoreRow::Act(Action::SetStream(5))),
            "focus keeps the stream it was on"
        );
    }

    /// **A page that the change emptied pops back one level** instead of leaving the viewer on a
    /// blank sheet — and the root loses its Source row when nothing is left to pick.
    #[test]
    fn a_vanished_addon_pops_and_an_empty_list_drops_the_source_row() {
        let mut st = source_page();
        st.focus_key(MoreRow::OpenAddon(1).key().0);
        assert_eq!(st.on_ok(), MoreOk::Navigated);
        st.motion.cancel_slide();

        // the second addon answers no more: the page's position now names nobody
        let narrower = vec![(3, "4k DV".to_string(), "Torrentio".to_string())];
        assert!(st.refresh_to(rows_for(), narrower.clone()));
        assert_eq!(st.page(), Some(MorePage::Source), "back to the addon list");
        assert!(!st.transitioning(), "a page that vanished does not slide away");
        assert_eq!(ids(&st), vec![MoreRow::OpenAddon(0)]);

        // everything goes away: back to the root, without a Source row
        assert!(st.refresh_to(rows_for(), Vec::new()));
        assert_eq!(st.page(), None);
        assert_eq!(st.form.table.title(), None);
        assert!(st.form.index_of(&MoreRow::OpenSource).is_none());
        assert_eq!(st.form.selected_id(), Some(&MoreRow::Act(Action::ToggleStats)));
    }

    /// **A row set that changes height animates the card** (`ui::panel_motion`): the panel opens at
    /// rest, the Source row arriving under it rebuilds and grows it, the top edge springs to the
    /// new layout while the bottom and right stay anchored, then it asks for no more frames.
    #[test]
    fn a_changed_row_set_resizes_the_card_with_a_spring_and_then_rests() {
        let _serial = plx_base::testlock::serial();
        const DT: f32 = 1.0 / 60.0;
        let m = plx_base::fontcov::advances::ShippedMeasure;
        let step = |st: &mut MoreMenuState| {
            plx_machine::idle::frame_begin(DT);
            st.update(DT, &m);
            plx_machine::idle::present_moving()
        };
        // opened with nothing to pick: Options only, a short panel
        let mut st = state(rows_for(), &[]);
        let short = st.panel_rect(&m);
        st.motion.step(DT, short); // the open: the first step places the card AT its layout
        assert!(!st.transitioning(), "a freshly opened panel is at rest");
        assert_eq!(st.shown_rect(&m), short);

        // the streams land: the Source row appears and the panel grows
        assert!(st.refresh_to(rows_for(), two_addons()), "the row set moved");
        let tall = st.panel_rect(&m);
        assert!(tall.h > short.h, "the Source row makes the panel taller: {} > {}", tall.h, short.h);
        assert_eq!(
            (tall.x + tall.w, tall.y + tall.h),
            (short.x + short.w, short.y + short.h),
            "bottom and right are the anchor"
        );
        assert!(st.transitioning());
        assert!(step(&mut st), "the resize is motion");
        let mid = st.shown_rect(&m);
        assert!(mid.h > short.h && mid.h < tall.h, "mid-resize the card is between the heights: {mid:?}");
        let mut n = 0;
        while step(&mut st) {
            n += 1;
            assert!(n < 240, "the resize never settles");
        }
        assert_eq!(st.shown_rect(&m), tall, "lands exactly on the new layout");
        assert!(!st.transitioning());
        assert!(!step(&mut st), "at rest: no frames requested");
    }

    /// **A push slides the page and a pop slides it back; both rest.** The panel reports motion
    /// each frame it animates and asks for none once settled — the same bar as the track menu.
    #[test]
    fn a_push_and_a_pop_slide_and_then_rest() {
        let _serial = plx_base::testlock::serial();
        const DT: f32 = 1.0 / 60.0;
        let m = plx_base::fontcov::advances::ShippedMeasure;
        let step = |st: &mut MoreMenuState| {
            plx_machine::idle::frame_begin(DT);
            st.update(DT, &m);
            plx_machine::idle::present_moving()
        };
        let mut st = menu();
        st.motion.step(DT, st.panel_rect(&m));
        assert!(!step(&mut st), "an opened menu is at rest");
        st.focus_key(MoreRow::OpenSource.key().0);
        assert_eq!(st.on_ok(), MoreOk::Navigated);
        assert!(st.motion.sliding());
        assert!(step(&mut st), "the slide is motion");
        assert!(st.motion.live_dx() > 0.0, "the arriving page comes in from the right");
        let mut n = 0;
        while step(&mut st) {
            n += 1;
            assert!(n < 240, "the push never settles");
        }
        assert!(!st.motion.sliding() && !st.transitioning());
        assert_eq!(st.motion.live_dx(), 0.0);
        assert!(!step(&mut st), "at rest: no frames requested");
        assert!(st.pop());
        assert!(st.motion.sliding());
        assert!(step(&mut st), "the pop is motion too");
        n = 0;
        while step(&mut st) {
            n += 1;
            assert!(n < 240, "the pop never settles");
        }
        assert!(!st.motion.sliding() && !st.transitioning());
        assert!(!step(&mut st));
    }
}

#[cfg(test)]
mod focus_tests {
    use super::*;
    use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};

    struct HostFixture;
    impl Host for HostFixture {
        type Arg = plx_ui::fixture::FixtureArg;
        type Fx = plx_ui::fixture::FixtureFx;
        type Msg = plx_ui::fixture::FixtureMsg;
        type Elem = u32;
        type Views<'a> = ();
        type Init = plx_ui::fixture::FixtureInit;
        type Memory = ();
    }

    fn with_cx<R>(entry: EntryId, test: impl FnOnce(&Cx<'_, HostFixture>) -> R) -> R {
        let measure = plx_ui::fixture::FixtureMeasure;
        test(&Cx {
            views: (),
            tick: Tick::default(),
            measure: &measure,
            focus: FocusRead::default(),
            press: PressRead::default(),
            owner: InputOwner::Entry(entry),
        })
    }

    /// A three-row menu mirroring a real addon page's shape — three streams, in ranked order —
    /// built without a route: no row here reads one.
    fn three_row_menu() -> MoreMenuState {
        let mut sec = FormSection::new("");
        let mut rows = Vec::new();
        for (a, label) in [
            (Action::SetStream(3), "4k DV"),
            (Action::SetStream(5), "1080p"),
            (Action::ToggleStats, "Stats for nerds"),
        ] {
            sec = sec.item(MoreRow::Act(a), RowKind::Button, a, Row::new(label));
            rows.push(a);
        }
        let mut form = FormTable::new(plx_ui::table_screen::BAND_BASE);
        form.table.compact = true;
        form.set(Form::new().section(sec), None);
        MoreMenuState {
            form,
            rows,
            pages: PageStack::new(),
            motion: PanelMotion::new(),
            streams: Vec::new(),
            stream_rev: 0,
        }
    }

    /// The focus element of the row at `index` — the keys are identities, not positions.
    fn elem(st: &MoreMenuState, index: usize) -> u32 {
        st.form.key_at(index).expect("a bound row").0
    }

    /// **UP/DOWN step by one row and clamp at both ends**, over `TableView::next_selectable`,
    /// exercised here through the real `Focusable` dispatch over `HostFixture`.
    #[test]
    fn up_down_step_by_one_and_clamp_at_both_ends() {
        let e = EntryId(4);
        let st = three_row_menu();
        let part = MoreMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let step = |i: u32, dir: Dir| {
                match <MoreMenuPart as Focusable<HostFixture>>::neighbour(
                    &part,
                    FocusKey { entry: e, elem: i },
                    dir,
                    cx,
                ) {
                    Step::Move(k) => Some(k.elem),
                    Step::Edge => None,
                }
            };
            let (r0, r1, r2) = (elem(&st, 0), elem(&st, 1), elem(&st, 2));
            assert_eq!(step(r0, Dir::Down), Some(r1));
            assert_eq!(step(r1, Dir::Down), Some(r2));
            assert_eq!(step(r2, Dir::Down), None, "the last row does not wrap");
            assert_eq!(step(r0, Dir::Up), None, "the first row does not wrap");
            assert_eq!(step(r1, Dir::Up), Some(r0));
            // LEFT/RIGHT are swallowed — this popover is ONE column.
            assert!(matches!(step(elem(&st, 1), Dir::Left), None));
        });
    }

    /// `place` reports exactly the row rect `TableView::row_frame` would paint at.
    #[test]
    fn place_matches_the_tables_own_row_frame() {
        let e = EntryId(4);
        let st = three_row_menu();
        let r = st.panel_rect(&plx_ui::fixture::FixtureMeasure);
        let want = st.form.table.row_frame(r, 1);
        let part = MoreMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let placed = <MoreMenuPart as Focusable<HostFixture>>::place(&part, &elem(&st, 1), cx, At::Drawn);
            assert_eq!(placed.map(|p| (p.rect.x, p.rect.y, p.rect.w, p.rect.h)), want.map(|r| (r.x, r.y, r.w, r.h)));
        });
    }

    /// A cursor whose key names no row (a shorter previous row set) settles onto the menu's
    /// OPENING row — a key has no neighbour to slide to — reached through `Focusable::reconcile`.
    #[test]
    fn a_stale_cursor_settles_onto_the_opening_row() {
        let e = EntryId(4);
        let st = three_row_menu();
        let part = MoreMenuPart { state: &st, entry: e, group: GroupId(0) };
        with_cx(e, |cx| {
            let got = <MoreMenuPart as Focusable<HostFixture>>::reconcile(
                &part,
                FocusKey { entry: e, elem: 0xdead },
                cx,
            );
            assert_eq!(got.elem, elem(&st, 0));
            let kept = <MoreMenuPart as Focusable<HostFixture>>::reconcile(
                &part,
                FocusKey { entry: e, elem: elem(&st, 2) },
                cx,
            );
            assert_eq!(kept.elem, elem(&st, 2), "a live key is left where it is");
        });
    }

    /// **Every row fits the panel, in every shipped language** — the root (with each possible
    /// stream count read out beside its Source row), the Source page, and an addon page with the
    /// title band. The panel hugs its widest row up to [`MENU_MAX_W`](plx_ui::table::MENU_MAX_W),
    /// and a row elides its label to what the value beside it leaves — Spanish *Estadísticas
    /// avanzadas* and Belarusian *Падрабязная статыстыка* both ended in `…` beside their *Off*.
    /// Measured with the device's whole-pixel advances.
    #[test]
    fn every_row_fits_the_panel_in_every_language() {
        use plx_platform::i18n::{language_on_this_thread_for_test, SHIPPED};
        let streams: Vec<(usize, String, String)> = vec![
            (3, "4k DV".to_string(), "Torrentio".to_string()),
            (5, "1080p".to_string(), "Sootio".to_string()),
        ];
        let mut out = Vec::new();
        let rows = rows_for();
        let mut check = |tag: &str, form: &MoreTable| {
            out.extend(form.table.menu_cap_failure(&plx_base::fontcov::advances::ShippedMeasure, tag));
            out.extend(form.table.app_fit_failures(plx_ui::table::MENU_MAX_W, tag));
            out.extend(form.table.app_fit_failures_hugged(tag));
        };
        for language in SHIPPED {
            let _guard = language_on_this_thread_for_test(language);
            let mut root = MoreMenuState::new();
            root.form.set(root_form(&rows, &streams), None);
            check(language.tag(), &root.form);
            let mut page = MoreMenuState::new();
            page.rows = rows.clone();
            page.streams = streams.clone();
            page.push(MorePage::Source);
            check(language.tag(), &page.form);
            let mut addon = MoreMenuState::new();
            addon.rows = rows.clone();
            addon.streams = streams.clone();
            addon.push(MorePage::Addon(0));
            check(language.tag(), &addon.form);
            // the root with nothing to pick (Options only)
            let empty = MoreMenuState::new();
            check(language.tag(), &empty.form);
        }
        plx_ui::table::assert_no_fit_failures(&out);
    }
}
