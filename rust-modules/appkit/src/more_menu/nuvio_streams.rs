//! Fork Rofocale11: the Nuvio **Streams** drill-in inside the player's `…` popover.
//!
//! This is a child module of [`super`] so the merge surface in the upstream menu stays a handful
//! of marked routing branches: everything that is Nuvio-specific — the ranked stream list, the
//! two pages, the shim's addon grouping and the layout of the rows — lives here.
//!
//! The data arrives as an addressed answer, never from a process global: the owning screen emits
//! `NuvioReq::Streams` and hands the returned `StreamList` to [`StreamMenu::land`]. A pick is an
//! [`super::Action::NuvioStream`] reported through the ordinary `MoreOk::Action` path; the app's
//! `apply_more_action` performs the shim `select` and re-resolves the playing item.
//!
//! There is deliberately no playback-quality ladder on these pages. Under the shim a stream IS
//! the quality — the shim resolves a concrete file — so a list of bitrate ceilings beside the
//! stream list would be a second, contradictory answer to the same question. The ladder remains
//! the upstream menu's row, hidden only while this picker has streams to show.
#![allow(non_upper_case_globals)]
use super::{Action, MorePage, MoreRow};
use crate::source_list;
use plx_ui::form::{Form, FormSection, RowKind};
use plx_ui::table::Row;

/// The form built by this module, on the same row/action/page types as the upstream menu.
pub type StreamForm = Form<MoreRow, Action, MorePage>;

/// The stream state carried by the upstream `MoreMenuState`, owned and stepped entirely here.
#[derive(Clone, Debug, Default)]
pub struct StreamMenu {
    /// The last ranked answer from the shim, in shim rank order.
    pub(crate) choices: Vec<plx_plex::plex::nuvio::StreamChoice>,
    /// The request id of the in-flight (or last) stream fetch. Zero before the first ask.
    pub(crate) req: u32,
    pub(crate) pending: bool,
    pub(crate) attempts: u32,
    /// The shim's own `complete` flag: an addon fan-out answers in batches, so a partial list is
    /// polled until the shim says no more are coming.
    pub(crate) complete: bool,
    /// Bumped whenever `choices` changes, so the upstream form notices a live re-rank.
    pub(crate) rev: u32,
}

impl StreamMenu {
    /// Is there a picker to open? An empty list adds no row — a drill-in onto an empty page would
    /// read as a menu that failed to load.
    pub(crate) fn offered(&self) -> bool {
        !self.choices.is_empty()
    }

    /// The first fetch, and every poll after a partial answer, are emitted from the screen's
    /// `step` (a machine never starts work in a constructor). A refused fetch is answered with
    /// `None`, which ends the polling.
    pub(crate) fn should_request(&self) -> bool {
        !self.pending && !self.complete
    }

    /// Arm one fetch: return its correlation id and the delay the worker should apply. The first
    /// ask is immediate; later asks (polls after a partial answer) give the shim's addon fan-out
    /// time to answer. The worker, not the screen, sleeps.
    pub(crate) fn arm(&mut self) -> (u32, u32) {
        self.attempts += 1;
        self.pending = true;
        self.req = self.req.wrapping_add(1).max(1);
        let delay = if self.attempts > 1 { 500 } else { 0 };
        (self.req, delay)
    }

    /// Land an addressed answer. Returns whether the visible list changed (and so the form must
    /// be rebuilt). A `None` answer is the shim's refusal — end the polling, keep any list there
    /// is, and leave the picker available.
    pub(crate) fn land(&mut self, list: Option<plx_plex::plex::nuvio::StreamList>) -> bool {
        self.pending = false;
        let Some(list) = list else {
            self.complete = true;
            return false;
        };
        self.complete = list.complete;
        if self.choices == list.streams {
            return false;
        }
        self.choices = list.streams;
        self.rev = self.rev.wrapping_add(1);
        true
    }
}

/// The upstream menu's fork-owned seam. A child module may touch its parent's private fields, so
/// all the stream bookkeeping stays here and the upstream file only routes.
impl super::MoreMenuState {
    /// Should the owning screen ask the shim for streams? The screen emits the effect from `step`;
    /// no menu ever starts a worker or reads a registry.
    pub fn nuvio_should_request(&self) -> bool {
        self.nuvio.should_request()
    }

    /// The request id this panel is waiting on (zero before the first ask), for the screen's
    /// addressed-answer fence.
    pub fn nuvio_request_id(&self) -> u32 {
        self.nuvio.req
    }

    /// Arm one fetch and return `(sid, rk, delay, request id)` for the screen to put on an effect.
    pub fn nuvio_arm_request(&mut self, sid: plx_plex::plex::ServerId, rk: &str) -> (plx_plex::plex::ServerId, String, u32, u32) {
        let (req, delay) = self.nuvio.arm();
        (sid, rk.to_string(), delay, req)
    }

    /// Land an addressed answer and rebuild the form if the list moved. The request id fence is
    /// checked by the screen before it calls this, so a stale reply cannot repaint the menu.
    pub fn nuvio_land(&mut self, ps: &plx_media::route::PlaybackSession, list: Option<plx_plex::plex::nuvio::StreamList>) -> bool {
        if !self.nuvio.land(list) {
            return false;
        }
        let forced = self.forced;
        let rows = self.rows.clone();
        let current = self.current;
        let speed = self.speed;
        let speeds = self.speeds.clone();
        let rev = self.nuvio.rev;
        self.refresh_to(ps, forced, rows, current, speed, speeds, rev)
    }
}

/// The root's headerless Streams drill-in, in the same idiom as the upstream Quality row: the
/// word names the destination and the trailing value says how many streams are on offer.
pub(crate) fn root_section(menu: &StreamMenu) -> FormSection<MoreRow, Action, MorePage> {
    FormSection::new("").item_if(
        menu.offered(),
        MoreRow::OpenStreams,
        RowKind::Nav(MorePage::Streams),
        Action::None,
        Row::new(plx_os::i18n::msg::browse_detail_also_available()).value(menu.choices.len().to_string()),
    )
}

/// A pushed page's title band. The Streams page uses the same word as the root row; an addon page
/// is titled by the addon's own name (server data, exempt from the app's fit ledger).
pub(crate) fn page_title(page: MorePage, menu: &StreamMenu) -> String {
    match page {
        MorePage::Streams => plx_os::i18n::msg::browse_detail_also_available().to_string(),
        MorePage::Addon(i) => source_list::addon_pages(&menu.choices)
            .get(i)
            .map(|group| group.name.clone())
            .unwrap_or_default(),
        MorePage::Quality | MorePage::Speed => String::new(),
    }
}

/// **The Source page**: one drill-in per addon that answered, stating how many streams it offered.
/// The second level is that addon's own page. Groups stay in shim rank order — the addon holding
/// the best stream leads.
///
/// **The Addon page**: that addon's streams, best first, each committing its own shim index.
/// [`crate::source_list::stream_row_with`] is the same row the Detail picker draws, so the P2P
/// seeder count that decides whether a torrent plays is stated here too. Nothing is checked: the
/// shim's selected index is not read back into this menu, and a mark beside every row would claim
/// a state this picker does not own.
pub(crate) fn page_form(page: MorePage, menu: &StreamMenu) -> StreamForm {
    match page {
        MorePage::Streams => {
            let addons = source_list::addon_pages(&menu.choices)
                .into_iter()
                .enumerate()
                .fold(FormSection::new(""), |sec, (i, group)| {
                    let row = Row::new(&group.name)
                        .server_label()
                        .server_value()
                        .value(group.rows.len().to_string());
                    sec.item(
                        MoreRow::OpenAddon(i),
                        RowKind::Nav(MorePage::Addon(i)),
                        Action::None,
                        row,
                    )
                });
            let mut form = Form::new().section(addons);
            if menu.choices.is_empty() {
                // placeholder-exempt: an inline busy NOTE on a picker page, not a content
                // stand-in on a screen a storyboard reaches.
                form = form.section(FormSection::new("").note(plx_os::i18n::msg::browse_streams_loading()));
            }
            form
        }
        MorePage::Addon(i) => {
            let groups = source_list::addon_pages(&menu.choices);
            let Some(group) = groups.get(i) else { return Form::new() };
            let sec = group
                .rows
                .iter()
                .filter_map(|&pos| menu.choices.get(pos))
                .fold(FormSection::new(""), |sec, choice| {
                    let a = Action::NuvioStream(choice.index);
                    sec.item(MoreRow::Act(a), RowKind::Choice, a, source_list::stream_row_with(choice, false))
                });
            Form::new().section(sec)
        }
        MorePage::Quality | MorePage::Speed => Form::new(),
    }
}

/// The first focusable row identity of a page — the opener of an addon page is restored to the
/// Source page on BACK, so a fresh push starts on the first real row, never on a note.
pub(crate) fn page_initial(page: MorePage, menu: &StreamMenu) -> Option<MoreRow> {
    page_form(page, menu)
        .slots()
        .into_iter()
        .flatten()
        .next()
        .map(|binding| binding.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::form::FormId;

    fn choice(index: usize, label: &str, addon: &str, seeders: Option<u32>) -> plx_plex::plex::nuvio::StreamChoice {
        plx_plex::plex::nuvio::StreamChoice {
            index,
            label: label.into(),
            addon: Some(addon.into()),
            seeders,
            ..Default::default()
        }
    }

    fn menu() -> StreamMenu {
        let mut menu = StreamMenu::default();
        assert!(menu.land(Some(plx_plex::plex::nuvio::StreamList {
            complete: true,
            streams: vec![
                choice(3, "4k DV", "Torrentio", Some(123)),
                choice(5, "1080p", "Sootio", None),
                choice(7, "720p", "Torrentio", None),
            ],
        })));
        menu
    }

    fn ids(form: &StreamForm) -> Vec<MoreRow> {
        form.slots().into_iter().flatten().map(|b| b.id).collect()
    }

    /// The root row names the destination and counts the streams; with no answer it is absent
    /// rather than a drill-in onto an empty page.
    #[test]
    fn the_root_opens_the_picker_once_streams_arrive() {
        let empty = StreamMenu::default();
        assert!(Form::new().section(root_section(&empty)).slots().is_empty(),
            "no Source row before the shim answers");
        let menu = menu();
        assert_eq!(Form::new().section(root_section(&menu)).slots().len(), 1);
    }

    /// The two levels: addons first, then one addon's ranked streams, grouped by the shim's own
    /// addon name. Groups are in rank order (Torrentio leads with stream 3).
    #[test]
    fn the_pages_group_by_addon_in_rank_order() {
        let menu = menu();
        let source = page_form(MorePage::Streams, &menu);
        let rows = ids(&source);
        assert_eq!(rows.len(), 2, "two addons answered");
        assert!(matches!(rows[0], MoreRow::OpenAddon(0)));
        let addon = page_form(MorePage::Addon(0), &menu);
        let acts: Vec<Action> = ids(&addon)
            .into_iter()
            .filter_map(|row| match row { MoreRow::Act(a) => Some(a), _ => None })
            .collect();
        assert_eq!(acts, vec![Action::NuvioStream(3), Action::NuvioStream(7)]);
        assert_eq!(page_title(MorePage::Addon(0), &menu), "Torrentio");
    }

    /// A P2P stream states its seeder count on the player's addon page — the one fact that decides
    /// whether that stream plays, in the same `▲ 123` form the Detail picker uses.
    #[test]
    fn the_addon_page_keeps_the_seeder_fact() {
        let menu = menu();
        let mut table = plx_ui::form::FormTable::new(plx_ui::table_screen::BAND_BASE);
        table.set(page_form(MorePage::Addon(0), &menu), None);
        let i = table.index_of(&MoreRow::Act(Action::NuvioStream(3))).unwrap() as i32;
        assert_eq!(table.table.row_mut(i).unwrap().detail.as_str(), "\u{25b2} 123");
        let i = table.index_of(&MoreRow::Act(Action::NuvioStream(7))).unwrap() as i32;
        assert!(!table.table.row_mut(i).unwrap().detail.contains('\u{25b2}'));
    }

    /// A partial answer is polled; a refusal ends the polling; a re-rank bumps the revision so the
    /// upstream form rebuilds instead of showing stale rows.
    #[test]
    fn the_list_is_polled_until_the_shim_is_done() {
        let mut menu = StreamMenu::default();
        assert!(menu.should_request());
        let (req, delay) = menu.arm();
        assert_eq!(req, 1);
        assert_eq!(delay, 0, "the first ask is immediate");
        assert!(menu.land(Some(plx_plex::plex::nuvio::StreamList {
            complete: false,
            streams: vec![choice(1, "720p", "Torrentio", None)],
        })));
        assert!(menu.should_request(), "a partial answer is followed, not shown as final");
        let (_, delay) = menu.arm();
        assert_eq!(delay, 500, "later asks are delayed on the worker");
        assert!(!menu.land(Some(plx_plex::plex::nuvio::StreamList {
            complete: true,
            streams: vec![choice(1, "720p", "Torrentio", None)],
        })), "an unchanged list does not rebuild the form");
        assert!(!menu.should_request());
        // A re-rank while the page is open moves the revision even when the count is the same.
        assert!(menu.land(Some(plx_plex::plex::nuvio::StreamList {
            complete: true,
            streams: vec![choice(2, "1080p", "Torrentio", None)],
        })));
        assert_eq!(menu.rev, 2);
    }

    /// Focus keys are identities, not positions: a re-rank moves no focus key, and an addon drill-in
    /// can never collide with a stream row on the page beneath it.
    #[test]
    fn stream_keys_are_stable_and_clear_of_the_drill_ins() {
        assert_ne!(MoreRow::OpenStreams.key(), MoreRow::OpenAddon(0).key());
        assert_ne!(Action::NuvioStream(3).key(), Action::NuvioStream(7).key());
        assert_ne!(MoreRow::OpenAddon(0).key(), Action::NuvioStream(0).key());
    }
}
