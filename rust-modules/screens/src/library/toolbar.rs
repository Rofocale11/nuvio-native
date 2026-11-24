//! The header row's elements: the page's controls and which library the row names. The row's
//! declaration is `header.rs`; its geometry, paint and motion are `plx_ui::page_header`.
use super::*;

/// The header's controls. Their ids sit beside the other page controls (`SORT`, `FILTER`, ...).
// Fork Rofocale11: the fabricated "Collections · N" control is gone (the shim has no collections
// count); its slot on the left of the row is the section's addon-catalog selector.
pub(super) const CATALOGS: u32 = 8;
pub(super) const RECOMMENDED: u32 = 9;
pub(super) const SORTFILTER: u32 = 10;
/// The Collections page's crumb root ("Films"): OK returns to the page below.
pub(super) const CRUMB_ROOT: u32 = 11;

impl LibraryScreen {
    /// The controls on the LEFT of the row, left to right: the Catalogs selector (which names the
    /// catalog in force) and Recommended. They stand over the first poster rather than beside the
    /// Sort & Filter control at the right edge. A section that advertised no catalogs has nothing
    /// to pick and gets no selector.
    // Fork Rofocale11: the left/right control split replaces the upstream Collections/Recommended/
    // Sort row; the top Home/Movies/TV Shows tabs, not a library-options pill, are the switcher.
    pub(super) fn left_controls(&self) -> &'static [u32] {
        if self.listing == Listing::Collections { return &[]; }
        if self.catalogs { &[CATALOGS, RECOMMENDED] } else { &[RECOMMENDED] }
    }

    /// The controls on the RIGHT of the row (the Sort & Filter menu).
    pub(super) fn right_controls(&self) -> &'static [u32] {
        if self.listing == Listing::Collections {
            return if self.collections_sort { &[SORTFILTER] } else { &[] };
        }
        &[SORTFILTER]
    }

    /// Every focusable control on the row, in reading order: the left cluster then the right one.
    /// The Collections page keeps its own crumb/sort row.
    pub(super) fn toolbar_elems(&self) -> &'static [u32] {
        if self.listing == Listing::Collections { return self.right_controls(); }
        if self.catalogs { &[CATALOGS, RECOMMENDED, SORTFILTER] }
        else { &[RECOMMENDED, SORTFILTER] }
    }

    pub(super) fn view_section<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<usize> {
        let directory = H::directory(cx);
        self.pending.section().filter(|target| Some(target.epoch) == directory.epoch())
            .map(|target| target.index).or_else(|| match self.wanted_kind {
                Some(kind) => directory.preferred(kind), None => directory.current(),
            })
            .filter(|index| *index < directory.sections().len())
    }

    /// A library's name on the header row in its two pieces, borrowed from the directory: its title
    /// and its owner's handle when someone else's (empty when it is not). Everything
    /// [`Self::library_label`] reads, so a cache keyed on the pair cannot go stale.
    pub(super) fn library_name<'a, H: LibraryLike>(&self, section: usize, cx: &Cx<'a, H>) -> (&'a str, &'a str) {
        let directory = H::directory(cx);
        let Some(section) = directory.sections().get(section) else { return ("", "") };
        let owner = section.sid.and_then(|sid| directory.sources().iter().find(|(id, _)| *id == sid))
            .map(|(_, source)| source.handle.as_str()).unwrap_or("");
        (section.row.title.as_str(), owner)
    }

    /// A library's name on the header row: its title, and its owner's handle when someone else's.
    pub(super) fn library_label<H: LibraryLike>(&self, section: usize, cx: &Cx<'_, H>) -> String {
        let (title, owner) = self.library_name(section, cx);
        joined_name(title, owner)
    }
}

/// The two pieces of [`LibraryScreen::library_name`] as one name: the title, then the owner's handle
/// when there is one. The one place the join is spelled; the capsule builds it only when a piece changes.
pub(super) fn joined_name(title: &str, owner: &str) -> String {
    if owner.is_empty() { title.to_owned() } else { format!("{title} {owner}") }
}
