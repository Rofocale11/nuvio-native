//! The Detail page's About footer.
//!
//! Two columns on one heading line: the synopsis card on the left, the Information column on the
//! right. Only the card is a control; Information remains readable content, not a dead focus stop.

use std::ffi::CString;

use plx_data::metadata::Detail;
use plx_machine::machine::{GroupId, Measure};
use plx_ui::text_lift::{draw_focused, TextLift, CENTRE};
use plx_ui::hstack::Item;
use plx_ui::text_view::TextView;
use plx_ui::vstack::VFlow;
use plx_ui::{theme, Painter, Rect};

pub const ABOUT_ELEM_RANGE_START: u32 = 4 * super::SECTION_BLOCK;
pub const ABOUT_ELEM_RANGE_END: u32 = 5 * super::SECTION_BLOCK;
pub const ABOUT_GROUP: GroupId = GroupId(5);
pub const CARD_ELEM: u32 = ABOUT_ELEM_RANGE_START;

const CARD_W: f32 = 640.0;
const CARD_Y: f32 = 50.0;
const CARD_PAD: f32 = 30.0;
/// The `ui::text_lift` corner radius of the About card (the mockup's 14).
const LIFT_RADIUS: f32 = 14.0;
/// The Information column's left edge: clear of the card's own right edge (`MARGIN_X + CARD_W` =
/// 736) by enough air that the two columns read as separate halves of the footer, and its rows
/// stand at the card's top edge, so the two columns share one heading line and one body line.
const INFO_X: f32 = 1000.0;
/// The Information column's text measure — from [`INFO_X`] to the right safe-area margin.
const INFO_W: f32 = 720.0;
/// The air under a label/value pair.
const PAIR_AIR: f32 = 22.0;
/// A pair's value sits this far under its label.
const PAIR_VALUE_DY: f32 = 34.0;
/// Air between the synopsis' last line and the MORE pinned on it — the same air the Person bio
/// and the Collection summary keep before theirs.
const MORE_GAP: f32 = theme::space::LG;
/// The About card's synopsis measure.
const SYNOPSIS_W: f32 = CARD_W - 2.0 * CARD_PAD;

/// The About card's synopsis: ONE builder for the height `card_rect` reserves and the block
/// `draw` paints, so the two cannot disagree about the wrap.
fn synopsis<'a>(summary: &'a str, measure: &'a dyn Measure) -> TextView<'a> {
    TextView::new(summary, theme::size::CAPTION, theme::TEXT_HEADING)
        .with_measure(measure)
        .leading(30.0)
        .max_lines(5)
        .fade_for_more(MORE_GAP)
        .mark_always()
}

pub fn locate(key: u32) -> Option<usize> {
    (key == CARD_ELEM).then_some(0)
}

pub struct Rows {
    dirty: bool,
    identity: Option<(plx_plex::plex::ServerId, String)>,
    info: Vec<(&'static str, String)>,
}

impl Rows {
    pub fn new() -> Self {
        Self {
            dirty: true,
            identity: None,
            info: Vec::new(),
        }
    }

    pub fn update(&mut self, d: &Detail) {
        if !self.dirty
            && self
                .identity
                .as_ref()
                .is_some_and(|(sid, rk)| *sid == d.sid && rk == &d.rk)
        {
            return;
        }
        self.dirty = false;
        self.identity = Some((d.sid, d.rk.clone()));
        self.info.clear();
        let released = plx_ui::fmt::pretty_date(&d.aired, d.year);
        if !released.is_empty() {
            self.info.push((plx_platform::i18n::msg::browse_detail_released(), released));
        }
        // A show's or season's `duration` is the agent's nominal per-episode figure, no episode's
        // (#814): only a leaf has a runtime to print.
        if plx_data::metadata::has_runtime(&d.kind) && !d.is_show && d.dur_ms > 0 {
            self.info.push((
                plx_platform::i18n::msg::browse_detail_runtime(),
                plx_ui::fmt::duration(d.dur_ms),
            ));
        }
        self.info.push((
            plx_platform::i18n::msg::browse_detail_rated(),
            if d.rating.is_empty() {
                plx_platform::i18n::msg::browse_detail_unrated().into()
            } else {
                d.rating.clone()
            },
        ));
        if !d.countries.is_empty() {
            self.info
                .push((plx_platform::i18n::msg::browse_detail_origins(), d.countries.join(", ")));
        }
    }

    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn card_rect(&self, d: &Detail, top: f32, measure: &dyn Measure) -> Rect {
        let h = synopsis(&d.summary, measure).measure_h(SYNOPSIS_W).max(30.0);
        Rect::new(
            plx_ui::consts::MARGIN_X,
            top + CARD_Y,
            CARD_W,
            CARD_PAD + 100.0 + h + CARD_PAD,
        )
    }

    /// Where the footer's last ink stands for the page `top` — the card or the Information column,
    /// whichever runs lower. The page's scroll clamp reads this so the footer can come to rest at
    /// the bottom of the screen instead of being pinned under the compact title with the rest of
    /// the viewport left empty.
    pub fn footer_bottom(&self, d: &Detail, top: f32, measure: &dyn Measure) -> f32 {
        let card = self.card_rect(d, top, measure);
        let card_bottom = card.y + card.h;
        let mut flow = VFlow::new(top + CARD_Y, 0.0);
        for (_, value) in &self.info {
            flow.place(|_| Item::new(pair_h(value, measure)).after(PAIR_AIR));
        }
        card_bottom.max(flow.end())
    }

    pub fn draw(
        &self,
        p: Painter,
        d: &Detail,
        top: f32,
        card_lift: &TextLift,
        measure: &dyn plx_machine::machine::Measure,
    ) {
        let x = plx_ui::consts::MARGIN_X;
        p.text(
            plx_platform::i18n::msg::browse_detail_about_c().as_ptr(),
            x,
            top,
            theme::size::HEADLINE,
            theme::TEXT_PRIMARY,
            0,
            1,
        );
        // The Information heading shares the About heading's line; its rows then stand on the
        // card's top edge, so the two columns open on one line and read as one block.
        text_at(
            p,
            INFO_X,
            top,
            theme::size::HEADLINE,
            theme::TEXT_PRIMARY,
            1,
            plx_platform::i18n::msg::browse_detail_information(),
        );
        let card = self.card_rect(d, top, measure);
        // The card's content grows with its plate inside `draw_focused`'s closure only; the
        // Information column drawn after it uses the caller's own `p` and cannot inherit the lift.
        draw_focused(p, card, LIFT_RADIUS, card_lift, CENTRE, |p| {
            let ix = card.x + CARD_PAD;
            text_at(
                p,
                ix,
                card.y + CARD_PAD,
                theme::size::HEADLINE,
                theme::TEXT_PRIMARY,
                1,
                &plx_gfx::text::elide_by(&d.title, card.w - 2.0 * CARD_PAD, false, |t| {
                    measure.width_str(t, theme::size::HEADLINE, true)
                }),
            );
            if !d.genres.is_empty() {
                text_at(
                    p,
                    ix,
                    card.y + CARD_PAD + 44.0,
                    theme::size::CAPTION,
                    theme::TEXT_TERTIARY,
                    0,
                    &plx_gfx::text::elide_by(&d.genres.join(", "), card.w - 2.0 * CARD_PAD, false, |t| {
                        measure.width_str(t, theme::size::CAPTION, false)
                    }),
                );
            }
            synopsis(&d.summary, measure)
                .draw(p, Rect::new(ix, card.y + CARD_PAD + 100.0, SYNOPSIS_W, 0.0));
            p.text(
                plx_ui::text_view::more_mark().as_ptr(),
                card.x + card.w - CARD_PAD,
                card.y + card.h - CARD_PAD - theme::size::CAPTION as f32,
                theme::size::CAPTION,
                theme::TEXT_TERTIARY,
                2,
                1,
            );
        });

        self.draw_information(p, INFO_X, top + CARD_Y, measure);
    }

    /// The column's pairs, standing from `y` — the card's top edge — with each pair's air under it.
    fn draw_information(&self, p: Painter, x: f32, y: f32, measure: &dyn Measure) {
        let mut flow = VFlow::new(y, 0.0);
        for (label, value) in &self.info {
            flow.place(|yy| Item::new(draw_pair(p, x, yy, label, value, measure)).after(PAIR_AIR));
        }
    }
}

/// The height of a pair's label and value, the air under it excluded.
fn pair_h(value: &str, measure: &dyn Measure) -> f32 {
    PAIR_VALUE_DY
        + TextView::new(value, theme::size::LABEL, theme::TEXT_HEADING)
            .bold()
            .with_measure(measure)
            .leading(30.0)
            .max_lines(2)
            .measure_h(INFO_W)
            .max(30.0)
}

/// Draws a label/value pair at `y` and returns its height (the air excluded).
fn draw_pair(p: Painter, x: f32, y: f32, label: &str, value: &str, measure: &dyn Measure) -> f32 {
    text_at(
        p,
        x,
        y,
        theme::size::CAPTION,
        theme::TEXT_TERTIARY,
        0,
        label,
    );
    let h = TextView::new(value, theme::size::LABEL, theme::TEXT_HEADING)
        .bold()
        .with_measure(measure)
        .leading(30.0)
        .max_lines(2)
        .draw(p, Rect::new(x, y + PAIR_VALUE_DY, INFO_W, 0.0));
    PAIR_VALUE_DY + h.max(30.0)
}

fn text_at(p: Painter, x: f32, y: f32, size: i32, color: [f32; 4], bold: i32, text: &str) -> f32 {
    CString::new(text)
        .ok()
        .map(|s| p.text(s.as_ptr(), x, y, size, color, 0, bold))
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue 15: the About card's MORE is pinned on the synopsis' last line whether or not the
    /// text was truncated, so a synopsis that fits in exactly its five lines with a long last
    /// line runs under MORE unless that line fades too.
    #[test]
    fn an_untruncated_last_line_reaching_under_more_fades() {
        let measure = plx_ui::fixture::FixtureMeasure;
        let fits = |n: usize| {
            let text = "word ".repeat(n);
            (!synopsis(&text, &measure).truncates(SYNOPSIS_W)).then_some(text)
        };
        let longest = (1..2000).map_while(fits).last().expect("some synopsis fits");
        let view = synopsis(&longest, &measure);
        let mark = measure.width(plx_ui::text_view::more_mark(), theme::size::CAPTION, true);
        assert!(!view.truncates(SYNOPSIS_W));
        assert!(view.last_line_w(SYNOPSIS_W) > SYNOPSIS_W - (mark + MORE_GAP),
            "precondition: the last line ends inside MORE's rect and its air");
        assert!(view.last_line_fades(SYNOPSIS_W), "text running under MORE must fade");
        let short = synopsis("A short synopsis.", &measure);
        assert!(!short.last_line_fades(SYNOPSIS_W), "a line clear of MORE stays plain");
    }

    #[test]
    fn only_the_card_is_focusable() {
        assert_eq!(locate(CARD_ELEM), Some(0));
        for key in ABOUT_ELEM_RANGE_START..ABOUT_ELEM_RANGE_END {
            if key != CARD_ELEM {
                assert_eq!(locate(key), None, "elem {key} has no stop in this footer");
            }
        }
    }

    #[test]
    fn a_same_identity_metadata_landing_invalidates_cached_about_rows() {
        let mut rows = Rows::new();
        let first = Detail {
            sid: plx_plex::plex::ServerId::UNSET,
            rk: "movie".into(),
            rating: "PG".into(),
            dur_ms: 60_000,
            ..Default::default()
        };
        rows.update(&first);
        assert!(rows
            .info
            .iter()
            .any(|(label, value)| *label == "Rated" && value == "PG"));

        let second = Detail {
            rating: "R".into(),
            dur_ms: 120_000,
            ..first
        };
        rows.update(&second);
        assert!(
            rows.info
                .iter()
                .any(|(label, value)| *label == "Rated" && value == "PG"),
            "without a landing invalidation, repeated reads remain O(1)"
        );
        rows.invalidate();
        rows.update(&second);
        assert!(rows
            .info
            .iter()
            .any(|(label, value)| *label == "Rated" && value == "R"));
        assert!(rows
            .info
            .iter()
            .any(|(label, value)| *label == "Run Time" && value == "2m"));
    }

    fn movie() -> Detail {
        Detail {
            sid: plx_plex::plex::ServerId::UNSET,
            rk: "movie".into(),
            rating: "PG".into(),
            summary: "word ".repeat(40),
            ..Default::default()
        }
    }

    fn settled(lifted: bool) -> TextLift {
        let mut lift = TextLift::new();
        for _ in 0..240 {
            lift.step(lifted, 1.0 / 60.0);
        }
        lift
    }

    fn census(rows: &Rows, d: &Detail, card: &TextLift) -> Vec<(u64, Rect)> {
        let measure = plx_ui::fixture::FixtureMeasure;
        plx_ui::draw_census::capture(|| rows.draw(Painter::recording(), d, 0.0, card, &measure))
    }

    /// The Information heading at rest and under a lifted card: the two-column footer's right
    /// half is drawn after the card's `draw_focused` closure, so it must not inherit the scale.
    fn info_heading(rows: &Rows, d: &Detail, card: &TextLift) -> Option<Rect> {
        census(rows, d, card)
            .into_iter()
            .find(|(tag, r)| *tag == 100 && (r.x - INFO_X).abs() < 0.5 && r.y.abs() < 0.5)
            .map(|(_, r)| r)
    }

    /// A focused About card grows its own content and nothing else: the Information column, drawn
    /// after it, stays exactly where it does at rest.
    #[test]
    fn a_focused_about_card_does_not_scale_its_sibling() {
        let _g = plx_base::testlock::serial();
        let mut rows = Rows::new();
        let d = movie();
        rows.update(&d);
        let at_rest = info_heading(&rows, &d, &TextLift::new());
        assert!(at_rest.is_some(), "precondition: the Information heading is in the census");
        let lifted_card = settled(true);
        assert!(lifted_card.scale() > 1.0, "precondition: the card is lifted");
        assert_eq!(info_heading(&rows, &d, &lifted_card), at_rest,
            "the sibling column must not inherit the card's scale");
        assert_ne!(census(&rows, &d, &lifted_card), census(&rows, &d, &TextLift::new()),
            "…and the card's own content did move, so the assertion above is not vacuous");
    }

    /// Focus leaving the card leaves nothing behind once it settles: the draw is exactly the
    /// never-focused draw.
    #[test]
    fn leaving_a_lifted_card_leaves_no_plate_or_shadow_once_settled() {
        let _g = plx_base::testlock::serial();
        let mut rows = Rows::new();
        let d = movie();
        rows.update(&d);
        let at_rest = census(&rows, &d, &TextLift::new());
        let mut card = settled(true);
        assert_ne!(census(&rows, &d, &card), at_rest, "precondition: focus draws something");
        card = settled(false);
        assert_eq!(census(&rows, &d, &card), at_rest);
    }

    /// #814: a show's `duration` is the metadata agent's nominal per-episode figure, so About
    /// prints no Runtime for a show or a season, and still prints a movie's or episode's.
    #[test]
    fn a_show_or_season_has_no_runtime_row_and_a_leaf_keeps_its_own() {
        let runtime = plx_platform::i18n::msg::browse_detail_runtime();
        let has = |d: &Detail| {
            let mut rows = Rows::new();
            rows.update(d);
            rows.info.iter().any(|(label, _)| *label == runtime)
        };
        let episode = plx_data::metadata::Episode { dur_ms: 45 * 60_000, ..Default::default() };
        let show = Detail { kind: "show".into(), is_show: true, dur_ms: 6_000_000, ..Default::default() };
        let season = Detail {
            kind: "season".into(),
            dur_ms: 6_000_000,
            episodes: vec![episode].into(),
            ..Default::default()
        };
        assert!(!has(&show), "a show's nominal duration is no episode's");
        assert!(!has(&season));
        for kind in ["movie", "episode"] {
            let leaf = Detail { kind: kind.into(), dur_ms: 6_000_000, ..Default::default() };
            assert!(has(&leaf), "{kind}");
        }
    }

    /// The two headings share one line; the pairs then stand on the card's top edge, at the
    /// Information column's own `x`, each pair a body and [`PAIR_AIR`] under the last.
    #[test]
    fn the_two_columns_share_the_heading_line_and_the_pairs_keep_their_air() {
        let _g = plx_base::testlock::serial();
        let measure = plx_ui::fixture::FixtureMeasure;
        let d = Detail::default();
        let mut rows = Rows::new();
        rows.info = vec![("Rated", "PG".into()), ("Origins", "France".into())];
        let runs = plx_ui::draw_census::capture_runs(|| {
            rows.draw(Painter::recording(), &d, 0.0, &TextLift::new(), &measure)
        });
        let run = |text: &str| {
            runs.iter()
                .find(|r| r.text == text)
                .unwrap_or_else(|| panic!("{text} not drawn"))
        };
        let tol = 0.5;
        let about = run(plx_platform::i18n::msg::browse_detail_about_c()
            .to_str()
            .expect("the About heading is UTF-8"));
        let info = run(plx_platform::i18n::msg::browse_detail_information());
        assert!((info.at.y - about.at.y).abs() < tol, "the headings share one line");
        assert_eq!(
            (info.size, info.ink, info.bold),
            (about.size, about.ink, about.bold),
            "Information wears the About heading's own type — same size, ink and weight"
        );
        assert!((info.at.x - INFO_X).abs() < tol, "Information owns the right column");
        assert!((about.at.x - plx_ui::consts::MARGIN_X).abs() < tol);

        let rated = run("Rated");
        assert!((rated.at.y - CARD_Y).abs() < tol, "the first pair stands on the card's top edge");
        assert!((rated.at.x - INFO_X).abs() < tol);
        let body = PAIR_VALUE_DY
            + TextView::new("PG", theme::size::LABEL, theme::TEXT_HEADING)
                .bold()
                .with_measure(&measure)
                .leading(30.0)
                .max_lines(2)
                .measure_h(INFO_W)
                .max(30.0);
        assert!((run("Origins").at.y - rated.at.y - (body + PAIR_AIR)).abs() < tol);
    }

    /// The page's scroll clamp reads [`Rows::footer_bottom`]: it covers the card's plate and the
    /// pairs' last line, whichever runs lower, so neither column can be cut off.
    #[test]
    fn the_footer_bottom_is_the_lower_of_the_card_and_the_pairs() {
        let measure = plx_ui::fixture::FixtureMeasure;
        let mut rows = Rows::new();
        let d = Detail { summary: "word ".repeat(6), ..Default::default() };
        let card_bottom = CARD_Y + rows.card_rect(&d, 0.0, &measure).h;
        assert!(rows.footer_bottom(&d, 0.0, &measure) >= card_bottom - 0.5);

        rows.info = (0..8).map(|i| ("Label", format!("Value {i}"))).collect();
        assert!(rows.footer_bottom(&d, 0.0, &measure) > card_bottom,
            "eight pairs run lower than a six-word card, and the clamp must follow them");
    }
}

