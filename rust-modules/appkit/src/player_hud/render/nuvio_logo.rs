//! Fork Rofocale11: the now-playing clear logo in the player HUD.
//!
//! Upstream's HUD package format has no logo slot: its only image cell binds `info.still`, and
//! `title.main` is a text cell. Under the Nuvio shim the addon's own `logo` is published as the
//! row's `clearLogo`, and `/photo/…/clearLogo` resolves it from the show's (or movie's) rating key.
//! This module uses the SAME shared artwork path the Home and Detail heroes use — `ui::tex` for the
//! texture and `hero_logo::{fit, place}` for sizing and placement — and paints into the title cell
//! the compiled HUD already measured. The HUD format, its bindings, its goldens and plain Plex's
//! typed title are all left alone; the text stays the fallback whenever no logo has resolved.
//!
//! The logo belongs to the SHOW for an episode, never to the episode leaf's still. `NowPlaying`
//! already carries that key as `detail_rk`, so no new playing-model field or artwork worker is
//! needed, and nothing here reads a global or starts IO.

use plx_ui::hero_logo::{self, LogoRung};
use plx_ui::label::HAlign;
use plx_ui::{Painter, Rect};

/// Match the footprint of the typed title this replaces, not a card logo and not a billboard.
/// `The Odyssey` at the 54px HUD rung measured about 400-430 px wide on the 1920 canvas; the
/// Compact rung gave ~246 px (too small) and a 3x area gave ~740 px (far too big). ~1.55x the
/// Compact linear size lands a 5:1 wordmark near 390x78, which reads as a title. These are the
/// fork's own roles, not new `MAX_*` caps.
const PLAYER_LOGO_AREA: f32 = 30_000.0;
const PLAYER_LOGO_BAND_H: f32 = 80.0;
const PLAYER_LOGO_W: f32 = 520.0;
const PLAYER_LOGO_H: f32 = 110.0;

fn player_rung() -> LogoRung {
    LogoRung::Custom {
        area: PLAYER_LOGO_AREA,
        band_h: PLAYER_LOGO_BAND_H,
        h_max: PLAYER_LOGO_H,
        w_max: PLAYER_LOGO_W,
        fallback_sz: 0,
        descent_share: 0.0,
    }
}

/// A resolved logo texture and its true pixel size.
pub(super) struct Logo {
    tex: u32,
    w: f32,
    h: f32,
}

/// The artwork key for the playing item: the detail rating key (show for an episode, movie
/// otherwise) on the playing item's server. `logo_path` turns a bare rk into the shim's
/// `/library/metadata/{rk}/clearLogo` proxy, which is why an absolute addon URL is never used.
fn logo_key<'a>(f: &'a super::Frame<'_>) -> Option<(u16, &'a str)> {
    let np = f.meta.now_playing()?;
    let rk = detail_rk(Some(np))?;
    let sid = plx_media::route::item_sid(plx_media::route::cur_sid(f.ps)).raw();
    Some((sid, rk))
}

/// The key the shim's clearLogo proxy resolves: the show for an episode, the movie otherwise.
/// `NowPlaying.detail_rk` already carries that, so the episode leaf's own rk/thumb is never used.
fn detail_rk(np: Option<&plx_data::metadata::NowPlaying>) -> Option<&str> {
    let rk = np?.detail_rk.as_str();
    (!rk.is_empty()).then_some(rk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_data::metadata::NowPlaying;

    fn np(detail_rk: &str) -> NowPlaying {
        NowPlaying {
            is_episode: true,
            is_real_episode: true,
            is_movie: false,
            title: "The Show".into(),
            ep_title: "The Night Crossing".into(),
            season: 2,
            index: 4,
            summary: String::new(),
            year: 2026,
            dur_ms: 0,
            rating: String::new(),
            thumb: "episode-still".into(),
            detail_rk: detail_rk.into(),
        }
    }

    #[test]
    fn the_logo_key_is_the_show_not_the_episode_leaf() {
        assert_eq!(detail_rk(Some(&np("77"))), Some("77"));
        assert_eq!(detail_rk(Some(&np(""))), None);
        assert_eq!(detail_rk(None), None);
    }

    #[test]
    fn the_player_mark_matches_the_typed_titles_footprint() {
        // A 5:1 wordmark: Compact ~246x49 (too small), 3x ~741x148 (too big); the typed 54px
        // title was ~400-430 wide, so the player rung lands ~387x77.
        let (cw, ch) = hero_logo::fit(LogoRung::Compact, 500.0, 100.0, 1600.0);
        let (pw, ph) = hero_logo::fit(player_rung(), 500.0, 100.0, 1600.0);
        assert!((pw / cw - 1.55).abs() < 0.15, "{cw} -> {pw}");
        assert!((ph / ch - 1.55).abs() < 0.15, "{ch} -> {ph}");
        assert!((350.0..=440.0).contains(&pw), "typed-title width: {pw}");
    }
}

/// `None` while the artwork is pending or absent, or the play has no shim row to resolve. The
/// caller keeps the typed title in every one of those cases — the text is the fallback, exactly as
/// it is on a hero whose clearLogo has not arrived.
pub(super) fn resolve(f: &super::Frame<'_>) -> Option<Logo> {
    let (sid, key) = logo_key(f)?;
    let (tex, w, h) = plx_ui::tex::logo_src(sid, key)?;
    (tex != 0 && w > 0.0 && h > 0.0).then_some(Logo { tex, w, h })
}

impl Logo {
    fn sized(&self, col_w: f32) -> (f32, f32) {
        hero_logo::fit(player_rung(), self.w, self.h, col_w.max(1.0))
    }

    /// How tall the mark will paint in this column. The caller lifts the episode kicker clear of
    /// it; a big logo must never cover the `S2 · E4` line.
    pub(super) fn painted_h(&self, col_w: f32) -> f32 {
        self.sized(col_w).1
    }

    /// Bottom-anchor the mark in the cell the compiled title already reserved, with the shared
    /// area/aspect rule so a wide wordmark and a square emblem get equal presence.
    pub(super) fn paint(self, q: Painter, band: Rect, size: i32) {
        // The compiled cell's y is where `Painter::text` paints the line; its own height is a
        // layout box, not the ink. Keep the bottom on the typed line's descender line so a taller
        // mark grows UP into the picture, never down over the playbar.
        let band = Rect::new(band.x, band.y, band.w, size.max(1) as f32);
        let (w, h) = self.sized(band.w);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        q.tex(self.tex, hero_logo::place(band, w, h, HAlign::Left), 0.0, plx_ui::theme::TEXT_PRIMARY);
    }
}
