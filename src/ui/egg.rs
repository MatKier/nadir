//! The easter egg behind a click on the title bar's ` nadir ` badge: the badge
//! lights up and a small satellite is deployed from it, flies once round the
//! title row trailing a comet tail, wraps off the right edge and back in from
//! the left, and settles at the badge again.
//!
//! Confined to the title row on purpose. nadir's whole pitch is that the
//! display never misrepresents a live reading, and a two-second animation laid
//! over the map or a panel would be exactly that — so the sprite only ever
//! overwrites a few cells of the bar's own text for a fraction of a second, and
//! is self-evidently decoration.
//!
//! Like everything in `ui::anim`, the geometry is a pure function of a phase —
//! here the wall-clock time since the click, never `sim_now()`, because a
//! satellite that paused or reversed with the simulated clock would read as
//! broken rather than stopped. [`transit`] and [`badge_glow`] hold the math and
//! are tested without a terminal; [`draw`] is the thin, dumb part that writes
//! cells.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

use crate::ui::anim::lerp;
use crate::ui::hit::BADGE_W;
use crate::ui::Theme;

/// How long one flight lasts. Long enough to read as a lap, short enough that a
/// second click a moment later restarts it rather than queueing behind it.
pub(crate) const EGG_FLIGHT: Duration = Duration::from_millis(1600);

/// The leading fraction of the flight spent lighting the badge before the
/// satellite leaves it — the "deploy".
const IGNITE: f32 = 0.15;

/// How many `·` follow the satellite. Short: a comet tail, not a ground track.
const TRAIL_LEN: u32 = 5;

/// The badge's glow at flight progress `t` in `0..=1`: rises to full over the
/// ignition, then eases back to nothing by the time the lap is over, so the
/// chip is back to its resting colour exactly when the egg ends and never
/// snaps.
fn badge_glow(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < IGNITE {
        t / IGNITE
    } else {
        ((1.0 - t) / (1.0 - IGNITE)).clamp(0.0, 1.0)
    }
}

/// Where the satellite and its tail are at flight progress `t` in `0..=1`, on a
/// bar `width` columns wide: `(satellite column, [(tail column, age)])`, with
/// `age` in `0..1` from just-behind to furthest-behind.
///
/// During the ignition the satellite waits at the badge's right edge with no
/// tail. After it, it advances a full `width` columns — so it crosses the right
/// edge, wraps in from the left and finishes back where it started, one lap
/// exactly. A tail dot is only drawn once the satellite has travelled far
/// enough to have left it behind; otherwise the first frames would strew dots
/// across the badge the satellite hasn't yet flown over.
fn transit(width: u16, t: f32) -> (u16, Vec<(u16, f32)>) {
    let start = u32::from(BADGE_W);
    let width = u32::from(width);
    if width == 0 {
        return (0, Vec::new());
    }
    let progress = ((t.clamp(0.0, 1.0) - IGNITE) / (1.0 - IGNITE)).max(0.0);
    let travelled = (progress * width as f32).round() as u32;
    let sat = (start + travelled) % width;
    let tail = (1..=TRAIL_LEN.min(travelled))
        .map(|k| (((start + travelled - k) % width) as u16, k as f32 / (TRAIL_LEN + 1) as f32))
        .collect();
    (sat as u16, tail)
}

/// Draw the egg into the title row `area` of `buf`. A no-op once `elapsed`
/// has reached [`EGG_FLIGHT`] — gated on the flight itself, not on the click
/// having happened, because `App::egg` is never reset: clamping the phase
/// would otherwise park the satellite on the bar for the rest of the session.
pub(in crate::ui) fn draw(buf: &mut Buffer, area: Rect, elapsed: Duration) {
    if elapsed >= EGG_FLIGHT || area.is_empty() {
        return;
    }
    let t = elapsed.as_secs_f32() / EGG_FLIGHT.as_secs_f32();
    let y = area.y;

    // Light the chip: only its background moves, so the black label keeps its
    // contrast against every step from ACCENT to SAT.
    let glow = badge_glow(t);
    for x in area.x..area.x.saturating_add(BADGE_W).min(area.right()) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_bg(lerp(Theme::ACCENT, Theme::SAT, glow));
        }
    }

    let (sat, tail) = transit(area.width, t);
    for (col, age) in tail {
        put(buf, area, col, "·", lerp(Theme::SAT, Theme::TRACK_PAST, age), false);
    }
    put(buf, area, sat, "◆", Theme::SAT, true);
}

/// Write one glyph at bar column `col`. Over the badge itself the glyph is
/// drawn black, since the satellite's own yellow would vanish against the
/// chip's light background.
fn put(buf: &mut Buffer, area: Rect, col: u16, glyph: &str, fg: Color, bold: bool) {
    let Some(cell) = buf.cell_mut((area.x + col, area.y)) else { return };
    let fg = if col < BADGE_W { Color::Black } else { fg };
    cell.set_symbol(glyph).set_fg(fg);
    if bold {
        cell.modifier.insert(Modifier::BOLD);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A title-bar-shaped buffer filled with a recognisable character, so a
    /// glyph the egg wrote (or left behind) is easy to tell from the bar.
    fn bar(width: u16) -> (Buffer, Rect) {
        let area = Rect::new(0, 0, width, 1);
        let mut buf = Buffer::empty(area);
        for x in 0..width {
            buf[(x, 0)].set_symbol("x");
        }
        (buf, area)
    }

    fn row(buf: &Buffer, width: u16) -> String {
        (0..width).map(|x| buf[(x, 0)].symbol().to_string()).collect()
    }

    #[test]
    fn the_egg_transit_wraps_exactly_once_and_returns_to_the_badge() {
        let width = 100;
        let mut wraps = 0;
        let mut last = transit(width, 0.0).0;
        for i in 1..=2000 {
            let (col, _) = transit(width, i as f32 / 2000.0);
            if col < last {
                wraps += 1;
            }
            last = col;
        }
        assert_eq!(wraps, 1, "one lap crosses the right edge exactly once");
        assert_eq!(last, BADGE_W, "and finishes back at the badge's edge");
    }

    #[test]
    fn the_satellite_waits_at_the_badge_with_no_tail_while_it_ignites() {
        let (col, tail) = transit(100, IGNITE * 0.5);
        assert_eq!(col, BADGE_W);
        assert!(tail.is_empty(), "nothing has been flown over yet");
    }

    #[test]
    fn the_egg_trail_ages_from_the_satellite_backwards() {
        let (sat, tail) = transit(100, 0.5);
        assert_eq!(tail.len(), TRAIL_LEN as usize);
        for (k, (col, age)) in tail.iter().enumerate() {
            assert_eq!((*col + k as u16 + 1) % 100, sat, "dot {k} sits {} behind", k + 1);
            if k > 0 {
                let nearer = tail[k - 1].1;
                assert!(*age > nearer, "each dot is older than the one nearer the satellite");
            }
            assert!((0.0..1.0).contains(age));
        }
    }

    #[test]
    fn the_egg_trail_wraps_round_the_right_edge_with_the_satellite() {
        // A moment after the wrap the satellite is near column 0 and its tail
        // is still back at the far right of the bar.
        let width = 100;
        let t = IGNITE + (1.0 - IGNITE) * (f32::from(width - BADGE_W) + 2.0) / f32::from(width);
        let (sat, tail) = transit(width, t);
        assert!(sat < 5, "just wrapped, got column {sat}");
        assert!(tail.iter().any(|(c, _)| *c > width - 5), "the tail trails off the right edge");
    }

    #[test]
    fn transit_on_a_zero_width_bar_draws_nothing_rather_than_dividing_by_zero() {
        assert_eq!(transit(0, 0.5), (0, Vec::new()));
    }

    #[test]
    fn the_badge_glow_peaks_when_the_satellite_launches_and_is_gone_by_the_end() {
        assert_eq!(badge_glow(0.0), 0.0);
        assert!((badge_glow(IGNITE) - 1.0).abs() < 1e-6);
        assert_eq!(badge_glow(1.0), 0.0);
        assert!(badge_glow(0.5) > 0.0 && badge_glow(0.5) < 1.0);
    }

    #[test]
    fn the_egg_leaves_the_title_row_untouched_once_its_flight_is_over() {
        let (mut buf, area) = bar(80);
        let before = buf.clone();
        draw(&mut buf, area, EGG_FLIGHT);
        assert_eq!(buf, before);
        draw(&mut buf, area, EGG_FLIGHT * 10);
        assert_eq!(buf, before, "and stays gone, so a stale click never parks a sprite");
    }

    #[test]
    fn the_egg_draws_the_satellite_and_a_tail_on_the_title_row_mid_flight() {
        let (mut buf, area) = bar(80);
        draw(&mut buf, area, EGG_FLIGHT / 2);
        let text = row(&buf, 80);
        assert_eq!(text.matches('◆').count(), 1, "exactly one satellite: {text}");
        assert_eq!(text.matches('·').count(), TRAIL_LEN as usize, "and its tail: {text}");
    }

    #[test]
    fn the_egg_lights_the_badge_and_leaves_the_cells_beside_it_alone() {
        let (mut buf, area) = bar(80);
        let resting = buf[(0, 0)].bg;
        draw(&mut buf, area, EGG_FLIGHT.mul_f32(IGNITE));
        assert_ne!(buf[(0, 0)].bg, resting, "the chip glows at peak ignition");
        assert_eq!(buf[(BADGE_W + 3, 0)].bg, resting, "only the chip's own cells change");
    }
}
