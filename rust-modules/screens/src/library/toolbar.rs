//! The header row's elements: the page's controls and which library the row names. The row's
//! declaration is `header.rs`; its geometry, paint and motion are `plx_ui::page_header`.
use super::*;

/// The header's controls. Their ids sit beside the other page controls (`SORT`, `FILTER`, ...).
pub(super) const CATALOGS: u32 = 8;
pub(super) const RECOMMENDED: u32 = 9;
pub(super) const SORTFILTER: u32 = 10;
/// The Collections page's crumb root ("Films"): OK returns to the page below.
pub(super) const CRUMB_ROOT: u32 = 11;

impl LibraryScreen {
    /// The controls on the row, left to right. The Catalogs control is on it only when the section
    /// advertised catalogs (`LibraryScreen::catalogs`): a library with none has nothing to pick.
    pub(super) fn toolbar_elems(&self) -> &'static [u32] {
        // The Collections page's row: the crumb (its own element, `CRUMB_ROOT`) and, only when the
        // server declares two sorts, the one Sort control. No Catalogs, Recommended or menu.
        if self.listing == Listing::Collections { return if self.collections_sort { &[SORTFILTER] } else { &[] }; }
        if self.catalogs { &[CATALOGS, RECOMMENDED, SORTFILTER] } else { &[RECOMMENDED, SORTFILTER] }
    }


    pub(super) fn view_section<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> Option<usize> {
        let directory = H::directory(cx);
        self.pending.section().filter(|target| Some(target.epoch) == directory.epoch())
            .map(|target| target.index).or_else(|| match self.wanted_kind {
                Some(kind) => directory.preferred(kind), None => directory.current(),
            })
            .filter(|index| *index < directory.sections().len())
    }

    /// A library's name on the header row: its title, and its owner's handle when someone else's.
    pub(super) fn library_label<H: LibraryLike>(&self, section: usize, cx: &Cx<'_, H>) -> String {
        let directory = H::directory(cx);
        let Some(section) = directory.sections().get(section) else { return String::new() };
        let owner = section.sid.and_then(|sid| directory.sources().iter().find(|(id, _)| *id == sid))
            .map(|(_, source)| source.handle.as_str()).unwrap_or("");
        if owner.is_empty() { section.row.title.clone() } else { format!("{} {owner}", section.row.title) }
    }
}
