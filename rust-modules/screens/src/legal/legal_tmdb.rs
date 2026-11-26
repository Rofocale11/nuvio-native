//! the TMDB attribution mark on the About page.
//!
//! TMDB's attribution terms require its logo in the app's About/Credits area, below the app's own
//! mark in prominence, together with the exact notice carried in `settings.about.body`. The SVG is
//! fork-owned and is drawn through the SHARED display-only picture slot (`Header::with_picture`),
//! so it adds no focus stop, no hit stop and no screen-local drawing primitive. Only `legal.rs`
//! opts in; this module owns the asset and its paint. Recorded in `docs/nuvio/MERGE-AUDIT-HISTORY.md`.

use plx_machine::machine::Measure;
use plx_ui::{
    follow_copy::PictureSlot,
    icons::{draw_svg, Svg},
    theme, Painter, Rect,
};

const TMDB_MARK: &str = include_str!("../../../../assets/nuvio/tmdb_logo.svg");

/// The official TMDB wordmark's viewBox is 190.24 × 81.52; keep its aspect ratio in the slot.
const ASPECT: f32 = 190.24 / 81.52;

/// the mark, tinted with a theme token so it stays a role, never a raw colour.
pub(crate) struct TmdbMark;

pub(crate) const MARK: TmdbMark = TmdbMark;

impl PictureSlot for TmdbMark {
    fn draw(&self, p: Painter, slot: Rect, _measure: &dyn Measure) {
        let w = slot.w.min(slot.h * ASPECT);
        let h = w / ASPECT;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let rect = Rect::new(slot.x + (slot.w - w) * 0.5, slot.y + (slot.h - h) * 0.5, w, h);
        draw_svg(p, Svg::new(TMDB_MARK), rect, theme::TEXT_SECONDARY);
    }
}
