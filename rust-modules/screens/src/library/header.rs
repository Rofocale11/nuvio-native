//! The library page header (redesign spec, "Header"): the library names, then the Catalogs selector
//! (naming the catalog in force) and Recommended on the LEFT, over the first poster; the divider and
//! the sort control (icon, the chosen criterion, a down chevron, with "Sort & Filter" as its unshown
//! name — [`HeaderItem::sort_control`], which the Collections page's Sort and the Watchlist's wear
//! too) on the RIGHT; on the Collections page, the crumb and its Sort.
//!
//! Fork Rofocale11: the fabricated "Collections · N" control is gone, and the library-options ("…")
//! pill was withdrawn entirely — the top Home/Movies/TV Shows tabs are the switcher. The Catalogs
//! control takes the old left slot: it names the addon catalog in force, and its menu lists this
//! tab's catalogs with the one in force checked.
//!
//! This file DECLARES the row: which names, which controls, which element each focusable item is.
//! The row is built here, once per sync, as a ui [`HeaderRow`]; its layout, paint, motion and focus
//! group are the ui's (`plx_ui::page_header`), the same ones the Collections, Recommended and
//! Watchlist pages wear. The names are packed against the room the right cluster leaves
//! (`PageHeader::layout`'s `left_budget`, sized from the cluster's widest case) less the room the
//! left controls need, so the names never shift when a value changes — the sort criterion, the
//! filter read-out or the catalog title included. The widest catalog title reserves that room, so a
//! long title moves nothing else. The controls stand by what they draw, one gap apart. The row's
//! MOTION (`header_capsules`, `header_pop`) lives on the screen, not in the row: a library switch
//! replaces the whole row, and the pill must travel across it.
use super::*;
use plx_machine::machine::Measure;
use plx_ui::hstack::{self, Align, HStack};
use plx_ui::page_header::{self, HeaderFocus, HeaderItem, HeaderRow, PageHeader, PageHeaderSpec, Slot};
use plx_ui::theme;

/// A shared library's owner credit is atomic. An oversized credit gives its room back to the
/// title, which uses the ordinary single-line fit.
pub(super) fn fit_name(title: &str, owner: &str, room: f32, measure: &dyn Measure) -> String {
    let size = theme::size::BODY;
    let full = super::toolbar::joined_name(title, owner);
    if measure.width_str(&full, size, false) <= room {
        return full;
    }
    measure.fit_line(title, room, size, false).to_string_lossy().into_owned()
}

/// The width the names `picks` stand in, one [`page_header::GAP`] apart: the ui's own
/// [`HStack::width`]. `PageHeader::layout` places the names with the same stack and the same gap, so
/// what is packed here and where the names stand share the gap rule (not one construction: this
/// takes measured widths, the layout takes spec items).
fn names_w(widths: &[f32], picks: &[usize]) -> f32 {
    let names: Vec<hstack::Item> = picks.iter().map(|&i| hstack::Item::new(widths[i])).collect();
    HStack::new(&names, page_header::GAP, Align::Leading(0.0)).width()
}

/// Which of `n` names show when the row cannot hold them all (`budget` is text edge to text edge).
/// Keep a stable prefix of favorites: choosing another library never reorders these shortcuts.
/// Fork Rofocale11: the library-options stop is gone, so nothing is reserved after the names.
pub(super) fn pack_names(widths: &[f32], budget: f32, cap: usize) -> Vec<usize> {
    let mut picks = Vec::new();
    for i in 0..widths.len().min(cap) {
        let mut next = picks.clone();
        next.push(i);
        if names_w(widths, &next) > budget { break; }
        picks = next;
    }
    picks
}

/// What the read-out's filtered count was taken under: the library listing (the key `unfiltered`
/// carries) and the filter terms in force. A count is only ever shown for the key it was counted under.
#[derive(Clone, PartialEq)]
pub(super) struct FilterKey {
    library: (LibrarySectionIdentity, u32, plx_data::browse::LibraryType),
    genre: Option<String>,
    unwatched: bool,
}

fn item(label: impl Into<String>) -> HeaderItem {
    HeaderItem { label: label.into(), focusable: true, ..Default::default() }
}

/// A control that opens a page (Collections, Recommended): the row draws a chevron after it. `name`
/// is its accessibility name (`header.collections`), a word and never its label, which is the
/// language's.
fn place(name: &'static str, label: impl Into<String>) -> HeaderItem {
    HeaderItem { place: true, a11y: Some(name), ..item(label) }
}

/// The hairline between the places and the sort control. Not a stop.
fn divider() -> HeaderItem {
    HeaderItem { divider: true, ..Default::default() }
}

impl LibraryScreen {
    /// Every focusable element of the header row in focus order: favorite shortcuts, options, the
    /// current visit when present, then the controls (on Collections, the crumb's root, then Sort).
    pub(super) fn header_elems(&self) -> Vec<u32> {
        self.header.row().map_or_else(Vec::new, |row| row.stops.iter().map(|(elem, _)| *elem).collect())
    }

    pub(super) fn is_header_elem(&self, elem: u32) -> bool {
        self.header_focus().is_some_and(|focus| focus.contains(elem))
    }

    /// The Sort & Filter control's value: the sort in force, a queued edit included. The scroll
    /// capsule carries the same string. Borrowed from the listing (or the catalogue), so the capsule,
    /// which asks every frame it is up, allocates nothing for it.
    pub(super) fn sort_value<'a, H: LibraryLike>(&self, cx: &Cx<'a, H>) -> &'a str {
        let listing = H::listing(cx);
        let queued = self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action);
        let sort = match queued {
            Some(GridAction::Sort { key, .. }) => listing.sorts().iter().find(|sort| &sort.key == key),
            _ => listing.sorts().get(listing.sort_index()),
        };
        sort.map_or(plx_os::i18n::msg::browse_library_title(), |sort| sort.label())
    }

    /// The active filter as words the scroll capsule can name beside the sort. A genre and
    /// Unwatched can both be on, and the locale's own joined wording is used then, so the capsule
    /// never invents a separator the reader has not seen in the filter menu.
    // Fork Rofocale11: the Catalogs dropdown is tab-scoped, and a chosen catalog can carry a genre
    // filter of its own; naming it on the capsule says what the rows on screen are filtered by.
    pub(super) fn filter_value<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<String> {
        if !self.filter_active(cx) { return None; }
        let listing = H::listing(cx);
        let genre = listing.genre().map(|genre| genre.title.as_str()).filter(|title| !title.is_empty());
        let unwatched = listing.unwatched();
        match (genre, unwatched) {
            (Some(genre), true) => Some(plx_os::i18n::msg::browse_library_genre_unwatched(genre)),
            (Some(genre), false) => Some(genre.to_owned()),
            (None, true) => Some(plx_os::i18n::msg::browse_library_unwatched().to_owned()),
            (None, false) => None,
        }
    }

    /// Is a genre or Unwatched filter in force (a queued edit included)? Only then does the row
    /// carry the "312 of 1,284" read-out.
    pub(super) fn filter_active<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> bool {
        let listing = H::listing(cx);
        let queued = self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action);
        let genre = match queued {
            Some(GridAction::Genre { id }) => id.is_some(),
            _ => listing.genre().is_some(),
        };
        let unwatched = match queued {
            Some(GridAction::Unwatched { desired }) => *desired,
            _ => listing.unwatched(),
        };
        genre || unwatched
    }

    /// What a filtered total belongs to: this listing's library and the filter terms it was asked
    /// with. `None` while the listing has no identity or the page no section.
    fn filter_key<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<FilterKey> {
        let listing = H::listing(cx);
        let (id, section) = (listing.id()?, self.section.clone()?);
        Some(FilterKey {
            library: (section, id.epoch, listing.library_type()),
            genre: listing.genre().map(|genre| genre.id.clone()),
            unwatched: listing.unwatched(),
        })
    }

    /// Remember the totals the read-out is made of: the data layer publishes only the filtered
    /// total, and the read-out needs the unfiltered one beside it.
    ///
    /// **The filtered total is held across a reload that keeps its terms.** A sort pick forgets the
    /// listing (`reset_listing`: loading, no total) under the same genre and Unwatched terms, and
    /// the count of what those terms match has not changed; so the read-out keeps saying it, and
    /// stays on the row to glide with its neighbours instead of leaving and coming back when the
    /// listing lands. The held count is dropped the first time the page sees any other library or
    /// other terms, and when the listing lands unfiltered, so it is never shown for a filter it
    /// was not counted under.
    fn note_totals<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let listing = H::listing(cx);
        let Some(key) = self.filter_key(cx) else { return };
        if listing.total() < 0 || listing.fetch() != SecFetch::Ready {
            if self.filtered.as_ref().is_some_and(|(held, _)| *held != key) { self.filtered = None; }
            return;
        }
        if key.genre.is_none() && !key.unwatched {
            self.unfiltered = Some((key.library, listing.total()));
            self.filtered = None;
        } else {
            if self.unfiltered.as_ref().is_some_and(|(seen, _)| *seen != key.library) { self.unfiltered = None; }
            self.filtered = Some((key, listing.total()));
        }
    }

    /// "312 of 1,284 films", once a filter is on and the unfiltered total is known. The noun is the
    /// listing's own (`LibraryType::count_label`, the words the Sources list's counts wear), so the
    /// read-out cannot be taken for a count of the Collections beside it.
    fn filtered_readout<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<String> {
        if !self.filter_active(cx) { return None; }
        let listing = H::listing(cx);
        let (_, total) = self.unfiltered.as_ref()?;
        // The listing's own count once it has one; while it reloads under the same terms, the count
        // held for them (see [`Self::note_totals`]).
        let matching = if listing.total() >= 0 && listing.fetch() == SecFetch::Ready {
            listing.total()
        } else {
            let (held, matching) = self.filtered.as_ref()?;
            if self.filter_key(cx).as_ref() != Some(held) { return None; }
            *matching
        };
        let locale = plx_os::i18n::current();
        let of_total = listing.library_type().count_label(self.kind, *total);
        Some(plx_os::i18n::msg::library_header_filtered_of(&locale.number(matching), &of_total))
    }

    /// The right cluster as it is now, and in its widest case (every value at its longest), which
    /// fixes the cluster's BUDGET so the room the library names may use does not change when a
    /// value does. Only the sort control stands here now (with the filtered read-out before it):
    /// Catalogs and Recommended moved to [`Self::header_left`], over the first poster.
    // Fork Rofocale11: no Collections count, no place pills on the right.
    pub(super) fn header_right<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> (Vec<HeaderItem>, Vec<HeaderItem>) {
        use plx_os::i18n::msg;
        let mut right = Vec::new();
        let mut widest = Vec::new();
        if let Some(readout) = self.filtered_readout(cx) {
            let item = HeaderItem { label: readout, readout: true, ..Default::default() };
            widest.push(item.clone());
            right.push(item);
        }
        // The divider and the sort control draw what the criterion says, with no slot for it: the
        // control ends at the row's edge and the criterion slides as its width changes. The widest
        // criterion only sizes the room the library names may use.
        right.push(divider());
        widest.push(divider());
        let sort = self.sort_value(cx);
        let longest = H::listing(cx).sorts().iter().map(|sort| sort.label()).chain([sort])
            .map(|label| (page_header::cluster_width(&[HeaderItem::sort_control("", label)], cx.measure), label))
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map_or("", |(_, label)| label).to_owned();
        right.push(HeaderItem::sort_control(msg::library_header_sort_filter(), sort));
        widest.push(HeaderItem::sort_control(msg::library_header_sort_filter(), longest));
        (right, widest)
    }

    /// The catalog the grid is listing — the one the menu checks — or the server's first, which is
    /// what the grid lists until a pick lands. `None` when the section advertised none.
    // Fork Rofocale11: the old fork's `current_catalog` rule (active key, then the flagged entry,
    // then the first) ported for the catalog selector.
    pub(super) fn current_catalog<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<String> {
        let listing = H::listing(cx);
        let catalogs = listing.catalogs();
        let active = listing.catalog()
            .and_then(|key| catalogs.iter().find(|catalog| catalog.key == key))
            .or_else(|| catalogs.iter().find(|catalog| catalog.active))
            .or_else(|| catalogs.first())?;
        Some(active.title.clone())
    }

    /// The controls that stand on the LEFT of the row: the Catalogs selector, drawing the catalog
    /// in force, then Recommended. Returned as (live, widest): the widest catalog title fixes the
    /// room they need so the library names beside them are packed against a stable budget.
    // Fork Rofocale11: the left cluster replaces the upstream Collections/Recommended places.
    pub(super) fn header_left<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> (Vec<HeaderItem>, Vec<HeaderItem>) {
        use plx_os::i18n::msg;
        let mut live = Vec::new();
        let mut widest = Vec::new();
        if self.listing == Listing::Collections { return (live, widest); }
        if self.catalogs {
            let current = self.current_catalog(cx).unwrap_or_default();
            let longest = H::listing(cx).catalogs().iter().map(|catalog| catalog.title.as_str())
                .max_by(|a, b| page_header::cluster_width(&[item(*a)], cx.measure)
                    .total_cmp(&page_header::cluster_width(&[item(*b)], cx.measure)))
                .unwrap_or_default().to_owned();
            live.push(HeaderItem { value: (!current.is_empty()).then_some(current), fold_glyph: true,
                a11y: Some("catalogs"), ..item(msg::library_header_catalogs()) });
            widest.push(HeaderItem { value: (!longest.is_empty()).then_some(longest), fold_glyph: true,
                a11y: Some("catalogs"), ..item(msg::library_header_catalogs()) });
        }
        let recommended = HeaderItem { selected_plate: self.listing == Listing::Recommended,
            ..place("recommended", msg::library_header_recommended()) };
        live.push(recommended.clone());
        widest.push(recommended);
        (live, widest)
    }

    /// A fingerprint of everything [`Self::refresh_header`] reads, cheap enough for every frame.
    fn header_key<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let listing = H::listing(cx);
        let directory = H::directory(cx);
        (std::ptr::from_ref(plx_os::i18n::current()) as usize).hash(&mut h);
        self.view_section(cx).hash(&mut h);
        // Fork Rofocale11: the catalogs control and the catalog it names are row inputs.
        self.catalogs.hash(&mut h);
        self.listing.hash(&mut h);
        self.listed.code().hash(&mut h);
        matches!(self.kind, SecKind::Movie).hash(&mut h);
        self.switched.hash(&mut h);
        for section in directory.sections() {
            (section.sid.map(|sid| sid.raw()), section.key, section.row.title.as_str(), section.row.pinned).hash(&mut h);
        }
        for (id, source) in directory.sources() { (id.raw(), source.handle.as_str()).hash(&mut h); }
        (listing.id().map(|id| (id.epoch, id.section, id.query)), listing.sort_index(), listing.sort_desc(),
            listing.unwatched(), listing.genre().map(|genre| genre.id.as_str()), listing.total(),
            listing.fetch() == SecFetch::Ready, listing.sorts().len(), self.unfiltered.as_ref().map(|(_, total)| *total),
            self.filtered.as_ref().map(|(_, total)| *total)).hash(&mut h);
        // Fork Rofocale11: the catalogs the section advertises and the one in force are what the
        // left control draws; a pick already queued changes the name before the listing lands.
        listing.catalogs().len().hash(&mut h);
        for catalog in listing.catalogs() {
            (catalog.key.as_str(), catalog.title.as_str(), catalog.active).hash(&mut h);
        }
        listing.catalog().hash(&mut h);
        match self.pending.grid().filter(|(target, _)| target.matches(listing)).map(|(_, action)| action) {
            Some(GridAction::Sort { key, desc }) => (1u8, key.as_str(), *desc).hash(&mut h),
            Some(GridAction::Unwatched { desired }) => (2u8, *desired).hash(&mut h),
            Some(GridAction::Genre { id }) => (3u8, id.as_deref()).hash(&mut h),
            Some(GridAction::LibraryType(kind)) => (4u8, kind.code()).hash(&mut h),
            Some(GridAction::Catalog(key)) => (5u8, key.as_deref()).hash(&mut h),
            None => 0u8.hash(&mut h),
        }
        h.finish()
    }

    /// Rebuild everything the header row is made of: whether it carries the Catalogs control, the
    /// names that fit (`self.libraries`), and the cached geometry.
    ///
    /// **The control is on the row when the published listing advertised catalogs for its
    /// section.** A section with none has nothing to pick and gets no control. The flag holds its
    /// last answer for the SAME library: the Browse store holds one listing per section and a
    /// pushed page (Collections, Recommended) re-points it, so a read of nothing for the library
    /// the row already names is not news — without the hold the control dropped and returned on
    /// Back. Across a switch it is another library's answer, so a control is only ever one this
    /// library gave.
    // Fork Rofocale11: the catalogs flag replaces the collections count and its hold rule.
    pub(super) fn refresh_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) {
        let of = self.view_section(cx).and_then(|index| H::directory(cx).sections().get(index))
            .map(|section| (section.sid, section.key));
        let read = !H::listing(cx).catalogs().is_empty();
        self.catalogs = read || (of == self.header_of && self.catalogs);
        self.note_totals(cx);
        // `sync` runs every frame; the row (text measurement, packing) is rebuilt only when one
        // of the things it is made of changed.
        let key = self.header_key(cx);
        if self.header.is_current(key) { return; }
        let row = if self.listing == Listing::Collections { self.collections_header(cx) } else { self.names_header(cx) };
        if let Some(old) = self.header.row() {
            // The same library's row changing fades what comes and goes; a row for ANOTHER library
            // is the new page's own, and shows nothing of the one left (see `HeaderSwap::begin_switch`).
            if of == self.header_of { self.header_swap.begin(old, &row); } else { self.header_swap.begin_switch(old, &row); }
            row.reflow_selection(old, &mut self.header_capsules);
        }
        self.header_of = of;
        self.header.install(key, row);
    }

    /// The row of the Library page itself: the library names that fit, then the left controls.
    fn names_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) -> HeaderRow {
        let (right, widest) = self.header_right(cx);
        let (left_controls, left_widest) = self.header_left(cx);
        let measure = cx.measure;
        // The right cluster fixes this page's left budget; the controls standing at the left edge
        // take their (widest) room out of it first, so the names packed below never run under them
        // and no name repacks when the active catalog's title changes.
        let probe = PageHeader::new(PageHeaderSpec { right: right.clone(), ..Default::default() }, &widest, measure)
            .layout(SCR_W, measure);
        let reserve = if left_widest.is_empty() { 0.0 }
            else { page_header::cluster_width(&left_widest, measure) + page_header::GAP };
        let budget = (probe.left_budget - reserve).max(0.0);

        self.libraries.clear();
        self.folded.clear();
        let current = self.view_section(cx);
        let mut left = Vec::new();
        if let Some(current) = current {
            let directory = H::directory(cx);
            let mut all: Vec<(u32, usize)> = Vec::new();
            for (index, section) in directory.favorite_sections_for(current).filter(|(_, section)| section.row.pinned) {
                let Some(sid) = section.sid else { continue };
                let elem = self.keys.register(
                    LibraryIdentity::Library(LibrarySectionIdentity { sid, key: section.key }),
                    LIBRARY_GROUP, all.len());
                all.push((elem, index));
            }
            // Reserve a current-library slot whenever the roster can require one. The same
            // reservation applies while a favorite is selected, so a visit never shifts or
            // displaces the favorite shortcuts. Shared measurement/elision bounds every label.
            let visit_slot = all.len() > 2 || directory.sections().iter().any(|section|
                section.kind == directory.sections()[current].kind && !section.row.pinned);
            let slots = if visit_slot { 3.0 } else { 2.0 };
            let name_budget = ((budget - slots * page_header::GAP) / slots).max(0.0);
            let label = |section, room| {
                let (title, owner) = self.library_name(section, cx);
                fit_name(title, owner, room, measure)
            };
            let labels: Vec<String> = all.iter().map(|(_, section)| label(*section, name_budget)).collect();
            let widths: Vec<f32> = labels.iter().map(|label|
                page_header::cluster_width(&[item(label.as_str())], measure)).collect();
            let reserved = if visit_slot { name_budget + page_header::GAP } else { 0.0 };
            let picks = pack_names(&widths, (budget - reserved).max(0.0), layout::MAX_LIBRARY_PILLS);
            let current_room = (budget - names_w(&widths, &picks) - page_header::GAP).max(0.0);
            let current_label = label(current, current_room);
            self.folded = all.iter().enumerate().filter(|(i, (_, section))|
                !picks.contains(i) && *section != current)
                .filter_map(|(_, (_, section))| u32::try_from(*section).ok()).collect();
            for &pick in &picks {
                left.push(HeaderItem { selected_plate: all[pick].1 == current, ..item(labels[pick].as_str()) });
                self.libraries.push(all[pick]);
            }
            if !self.libraries.iter().any(|(_, index)| *index == current) {
                if let Some(sid) = directory.sections()[current].sid {
                    let elem = self.keys.register(LibraryIdentity::Library(LibrarySectionIdentity {
                        sid, key: directory.sections()[current].key,
                    }), LIBRARY_GROUP, self.libraries.len());
                    left.push(HeaderItem { selected_plate: true, ..item(current_label) });
                    self.libraries.push((elem, current));
                }
            }
        }
        // The controls stand after any names, so on the usual one-library page they sit at the
        // left text edge, above the first poster. Fork Rofocale11: no library-options ("…") stop.
        left.extend(left_controls);

        let page = PageHeader::new(PageHeaderSpec { left, right, ..Default::default() }, &widest, measure);
        let layout = page.layout(SCR_W, measure);
        self.header_row(page, layout)
    }

    /// The built row: which element each focusable item of `layout` is. The names are
    /// `self.libraries`, then [`Self::left_controls`]; the right cluster is [`Self::right_controls`]
    /// (a read-out before them and the divider among them are no stops); the crumb's root is
    /// [`CRUMB_ROOT`] and its page name, the title, is none.
    fn header_row(&self, page: PageHeader, layout: page_header::HeaderLayout) -> HeaderRow {
        let names = self.libraries.len();
        let left_controls = self.left_controls();
        let right_controls = self.right_controls();
        let first_control = page.spec().right.iter().position(|it| it.focusable).unwrap_or(0);
        let stops = layout.items.iter().enumerate().filter(|(_, rect)| rect.focusable).filter_map(|(index, rect)| {
            let elem = match rect.slot {
                Slot::Left(i) => if i < names { self.libraries.get(i).map(|(elem, _)| *elem) }
                    else { left_controls.get(i - names).copied() },
                Slot::Overflow => None,
                Slot::Right(i) => i.checked_sub(first_control).and_then(|i| right_controls.get(i).copied()),
                Slot::CrumbRoot => Some(CRUMB_ROOT),
                Slot::CrumbPage => None,
            };
            elem.map(|elem| (elem, index))
        }).collect();
        HeaderRow::new(page, layout, stops)
    }

    /// The Collections page's row: the crumb (its root is a stop, its page name the title) in
    /// place of the library names, and Sort when the server declares more than one.
    fn collections_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>) -> HeaderRow {
        self.libraries.clear();
        // Only the collections' own declared sorts count: until the store has switched the section
        // to the collections listing, `sorts()` is still the films' list.
        let declared = if self.listed == plx_data::browse::LibraryType::Collections { H::listing(cx).sorts().len() } else { 0 };
        let library = self.view_section(cx).map(|index| self.library_label(index, cx)).unwrap_or_default();
        let spec = super::collections_page::header_spec(&library, self.kind, declared, self.sort_value(cx));
        self.collections_sort = super::collections_page::sort_visible(declared);
        let page = PageHeader::new(spec, &[], cx.measure);
        let layout = page.layout(SCR_W, cx.measure);
        self.header_row(page, layout)
    }

    /// The row as one focus group of this page, at its scroll: what the engine's `Focusable`
    /// questions about the header are answered from (`None` before the row is built).
    pub(super) fn header_focus(&self) -> Option<HeaderFocus<'_>> {
        self.header.row().map(|row| row.focus(self.entry, LIBRARY_GROUP, (self.scroll.pos, self.scroll_target), content_clip()))
    }

    /// Where the header control or name `elem` is, in screen space at the drawn or target scroll.
    pub(super) fn header_rect(&self, elem: u32, at: At) -> Option<Rect> {
        self.header_focus()?.place(elem, at).map(|placed| placed.rect)
    }

    /// One frame of the row's motion: the focus capsule travels to the focused control, the
    /// selection plate lands under it. Focus off the row (or a menu holding it) closes the pop.
    pub(super) fn step_header<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, dt: f32) {
        self.header_swap.step(dt);
        let Some(row) = self.header.row() else { return };
        row.step(self.entry, cx.focus.current, &mut self.header_capsules, &mut self.header_pop, dt);
    }

    /// Paint the row at the drawn scroll. The row scrolls away with the page: only its pill and
    /// the selected plate are chrome of its own.
    pub(super) fn draw_header<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>) {
        let Some(row) = self.header.row() else { return };
        let top = layout::HEADER_Y - self.scroll.pos;
        // The row rides the scroll under the top fade, and keeps out of the band the returning
        // chrome is drawn in while the scroll is still bringing it home (`Handoff::clearance`).
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha() * self.handoff.clearance(top));
        // The pill's outset (2 px) is far inside the cast's padding the band is grown by, so the band needs no pop.
        if !super::draw::document_band_visible(p, top, page_header::HEIGHT, 1.0) { return; }
        // A control a menu has lifted out of the page (`Screen::opener`) is not drawn here at all:
        // the opener lift is its only copy ([`Self::draw_opener`]), lit above the scrim. One rule
        // for every header opener (design review S12). The dispatcher decides when it stands down
        // (`lifted_opener`: a closing menu whose focus moved off the opener), so a lift is always
        // the control's whole menu-open stretch, during which the engine's focus is on the menu's
        // rows and the row's motion HOLDS.
        let lifted = f.lifted.filter(|key| key.entry == self.entry).map(|key| key.elem);
        row.draw_with(p, self.scroll.pos, &self.header_capsules, self.header_pop.scale_with(0, f.press), lifted, &self.header_swap);
    }

    /// **The opener lift for a header control** (`Screen::redraw_focused`): `elem`, the control a
    /// menu opened from, painted as the focused one above the menu's scrim, where the page pass
    /// left it out. Nothing is drawn for an element that is no stop of this row (a card).
    ///
    /// **It paints through the row's swap**, as the page's pass does: a sort picked from the menu
    /// rebuilds the row on the frame the menu is dismissed, and until the menu is gone this lift is
    /// the only thing that draws the sort control, so a lift that ignored the swap drew the new
    /// criterion whole from the first frame and popped when it stood down mid-slide.
    pub(super) fn draw_header_opener<H: LibraryLike>(&self, f: &DrawFrame<'_, '_, H>, elem: u32) {
        let Some(row) = self.header.row() else { return };
        let p = f.painter.alpha(f.page_alpha * self.page_fade.alpha() * self.handoff.clearance(layout::HEADER_Y - self.scroll.pos));
        row.draw_opener_with(p, self.scroll.pos, self.header_pop.scale_with(0, f.press), elem, &self.header_swap);
    }
}
