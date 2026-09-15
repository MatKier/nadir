//! The mission-control boot splash, shown on launch for `config.ui.splash`
//! (`App::splash_active`) before `ui::draw` hands off to the dashboard.
//!
//! Two layers, painted in this order so the console block occludes whatever
//! sits behind it — which is what sells the satellite as *passing behind*
//! the readout rather than just sharing the screen with it:
//!
//!  1. A full-area `Canvas` — a procedural starfield, and an orbital transit
//!     (a satellite riding an arc across the frame with a fading trail
//!     behind it) — the same `Marker::Braille` / one-`layer`-per-feature
//!     discipline `ui::map` and `ui::skyplot` use.
//!  2. The console text itself: the wordmark (igniting left-to-right on
//!     power-up), version line, and the `label ....... value` rows that
//!     still reveal over the first third of the splash, unchanged from
//!     before this module existed.
//!
//! Everything here is a pure function of `(area, phase, progress)`, `phase`
//! being `App::uptime()` and `progress` being `phase / config.ui.splash` —
//! the same "no state riding along in the render loop" discipline
//! `ui::anim`'s module doc lays out for the aurora shimmer and the sky plot
//! twinkle, extended here to the transit and the ignition sweep. `boot_lines`
//! is the one exception with real data behind it, and keeps its existing
//! honesty rule: a field not yet resolved reads `…`, never a fake value.

use std::time::Duration;

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine, Points};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, AppData};
use crate::geo::GeoPoint;
use crate::orbit::Tracker;
use crate::ui::anim::{hash01, lerp, noise};
use crate::ui::{centered, Theme};

/// A small block-letter "nadir" mark for the boot splash's opening lines —
/// a static presentational table under `ui/`, the `stars.rs` / `places.rs`
/// idiom. Purely decorative, unlike [`boot_lines`] below: it carries no data,
/// so it needs no honesty rule of its own.
const NADIR_WORDMARK: [&str; 3] = [
    "     █▄ █  ▄▀▄  █▀▄  █  █▀▄",
    "     █ ▀█  █▀█  █ █  █  █▀▄",
    "     ▀  ▀  ▀ ▀  ▀▀   ▀  ▀ ▀",
];

/// The release codename shown next to the version on the boot splash. Bump
/// it alongside the version in `Cargo.toml` on a release, the same way a
/// changelog heading would.
const RELEASE_NAME: &str = "First Light";

/// How wide the splash's dotted leader column is, `label` padded up to it —
/// wide enough for "ground station" (14 chars, the longest of
/// [`boot_lines`]'s labels) with a handful of dots still visible after it.
const BOOT_LEADER_WIDTH: usize = 20;

/// The splash's content, in reveal order, as plain `(label, value)` text —
/// no styling, no layout, so `boot_lines` is testable without a `Frame`.
/// Mirrors exactly what the dashboard itself is about to show a moment
/// later (`app.config.ground_station()`, `data.tle.get()`), so the splash
/// never promises something the real panels don't back up: a field not yet
/// resolved (the TLE may still be loading from cache or network at boot)
/// reads `…` rather than a fabricated value or a fake progress bar. `sat` is
/// `config.sat` — known from the moment the process starts, so it's shown
/// unconditionally rather than waiting on `tle`; `offline` is `config.offline`,
/// which likewise nadir already knows at boot without fetching anything.
fn boot_lines(
    station: Option<GeoPoint>,
    tle: Option<&Tracker>,
    sat: u64,
    offline: bool,
) -> Vec<(&'static str, String)> {
    let station_line = station
        .map(|s| {
            let lat_h = if s.lat_deg >= 0.0 { 'N' } else { 'S' };
            let lon_h = if s.lon_deg >= 0.0 { 'E' } else { 'W' };
            format!("{:.3}{lat_h} {:.3}{lon_h}", s.lat_deg.abs(), s.lon_deg.abs())
        })
        .unwrap_or_else(|| "not configured".to_string());
    let sat_name = tle.map(Tracker::name).unwrap_or("…");
    let epoch_line =
        tle.map(|t| t.epoch().format("%Y-%m-%d %H:%MZ").to_string()).unwrap_or_else(|| "…".to_string());
    let feeds_line = if offline {
        "offline — cache only".to_string()
    } else {
        "celestrak · swpc · launch library".to_string()
    };
    vec![
        ("ground station", station_line),
        ("element set", format!("{sat_name} · {sat}")),
        ("epoch", epoch_line),
        ("propagator", "SGP4/SDP4  local, no network".to_string()),
        ("frames", "TEME → ECEF → WGS84".to_string()),
        ("feeds", feeds_line),
    ]
}

/// How many of `total` rows (`boot_lines`' length) should be visible after
/// `uptime` of a `splash`-long boot screen. Rows reveal across the *first
/// third* only, reaching `total` at `splash / 3` and holding there for the
/// remaining two-thirds — the quiet "any key to continue" beat, now filled
/// by the starfield and the transit rather than a dead hold. At least one
/// row shows from the very first frame, so a splash that gets skipped almost
/// immediately still showed something rather than a blank flash.
fn splash_reveal_count(uptime: Duration, splash: Duration, total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    let third_secs = (splash.as_secs_f32() / 3.0).max(f32::EPSILON);
    let frac = (uptime.as_secs_f32() / third_secs).clamp(0.0, 1.0);
    ((frac * total as f32).ceil() as usize).clamp(1, total)
}

/// How much of the splash the wordmark's left-to-right power-up sweep takes
/// — well inside the third `splash_reveal_count` spends filling the rows in,
/// so the mark is already lit by the time there's a console readout under it
/// worth reading.
const IGNITION_FRACTION: f32 = 0.2;

/// How many columns wide the ignition sweep's lit/unlit edge blends over —
/// soft enough to read as a moving glow rather than a hard wipe.
const IGNITION_EDGE_COLS: f32 = 3.0;

/// Ignition brightness (`1.0` fully lit, `0.0` still dark) of wordmark
/// column `col` of `cols` total at `progress` through the whole splash. A
/// sweep position advances from just left of the first column to the last
/// column over `IGNITION_FRACTION` of `progress`, and a column's brightness
/// is how far *behind* that sweep position it sits, clamped and blended over
/// [`IGNITION_EDGE_COLS`] — which makes it a strictly non-increasing function
/// of `col` for any fixed `progress`: once the sweep has passed a column, no
/// column further right can be brighter than it, so the lit region is always
/// a single unbroken run from the left edge.
fn ignition_brightness(col: usize, cols: usize, progress: f32) -> f32 {
    let sweep_t = (progress / IGNITION_FRACTION).clamp(0.0, 1.0);
    let last_col = cols.saturating_sub(1) as f32;
    let sweep_col = sweep_t * (last_col + IGNITION_EDGE_COLS) - IGNITION_EDGE_COLS;
    (1.0 - (col as f32 - sweep_col) / IGNITION_EDGE_COLS).clamp(0.0, 1.0)
}

/// One wordmark row, each character lit [`Theme::SAT`] bold, still dark
/// [`Theme::FRAME`], or blended between the two right at the sweep's edge —
/// see [`ignition_brightness`].
fn ignite_wordmark_row(row: &str, progress: f32) -> Line<'static> {
    let cols = row.chars().count();
    let spans = row
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let t = ignition_brightness(i, cols, progress);
            let style = if t >= 1.0 {
                Style::new().fg(Theme::SAT).bold()
            } else if t <= 0.0 {
                Style::new().fg(Theme::FRAME)
            } else {
                Style::new().fg(lerp(Theme::FRAME, Theme::SAT, t))
            };
            Span::styled(ch.to_string(), style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// The boot splash's text: the wordmark (lit by [`ignite_wordmark_row`] at
/// `progress`) and version line, always shown, then one line per `lines`
/// entry — a real row (label, dotted leader, value) for the first `revealed`
/// of them, a blank placeholder for the rest — and the closing footer. Every
/// call for a given `lines` returns the same number of lines regardless of
/// `revealed`: only whether a given row's *content* has filled in yet
/// changes, never how many lines there are or how long any revealed row's
/// text is. That's what keeps the splash's box a fixed size as rows reveal
/// (see [`draw`]) — nothing after the reveal, most visibly the "any key to
/// continue" footer, shifts as it fills in.
fn boot_text(lines: &[(&'static str, String)], revealed: usize, progress: f32) -> Vec<Line<'static>> {
    let mut text: Vec<Line> = NADIR_WORDMARK.iter().map(|row| ignite_wordmark_row(row, progress)).collect();
    text.push(Line::from(Span::styled(
        format!("           v{} · \"{RELEASE_NAME}\"", env!("CARGO_PKG_VERSION")),
        Style::new().fg(Theme::LABEL),
    )));
    text.push(Line::from(""));
    for (i, (label, value)) in lines.iter().enumerate() {
        if i < revealed {
            let dots = ".".repeat(BOOT_LEADER_WIDTH.saturating_sub(label.chars().count()));
            text.push(Line::from(vec![
                Span::styled(format!("  {label} "), Style::new().fg(Theme::LABEL)),
                Span::styled(dots, Style::new().fg(Theme::FRAME)),
                Span::styled(format!(" {value}"), Style::new().fg(Theme::VALUE)),
            ]));
        } else {
            text.push(Line::from(""));
        }
    }
    text.push(Line::from(""));
    text.push(Line::from(Span::styled("  any key to continue", Style::new().fg(Theme::LABEL))));
    text
}

/// How many procedural background stars the splash scatters, scaled to the
/// terminal area — one star per roughly 50 cells, clamped so neither a
/// crowded 80×24 minimum nor a huge fullscreen terminal looks wrong.
fn star_count(area: Rect) -> usize {
    ((u32::from(area.width) * u32::from(area.height)) / 50).clamp(40, 260) as usize
}

/// How many discrete twinkle tiers the starfield draws in — the same
/// bucketing `skyplot::TWINKLE_TIERS` uses and for the same reason: a
/// braille cell carries one foreground colour, so this has to be a handful
/// of `Points` layers rather than one draw call per star.
const STARFIELD_TIERS: usize = 3;

/// Noise/hash lanes for the starfield — position, twinkle and fade-in each
/// get their own so one star's roll on one axis can't accidentally repeat
/// another's roll on a different axis.
const STAR_POS_SEED: u32 = 0x5741;
const STAR_TWINKLE_SEED: u32 = 0x5742;
const STAR_FADE_SEED: u32 = 0x5743;

/// How much of the splash's total duration the starfield spends fading stars
/// in, one at a time, rather than snapping on at frame one — the same third
/// the row reveal gets, so the whole backdrop arrives together.
const STAR_FADE_IN_FRACTION: f32 = 1.0 / 3.0;

/// Every background star visible this frame: its twinkle tier and cell
/// position, with any star that would land on or beside the console block
/// already dropped — a `Paragraph` doesn't pad a short line out to the box's
/// full width, so a star left in that gap would show through in the blank
/// space after a label's value rather than staying hidden behind the panel.
/// Positions come from [`hash01`] on a per-star index, so the field is fixed
/// for a given `area` rather than reshuffling every frame; only which tier a
/// star twinkles at (via [`noise`]) and whether it has faded in yet move with
/// `phase`/`progress`.
fn starfield(area: Rect, box_rect: Rect, phase: f32, progress: f32) -> Vec<(usize, u16, u16)> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let margin = box_margin(box_rect, area);
    let n = star_count(area);
    (0..n)
        .filter_map(|i| {
            let idx = i as u32;
            let col = (hash01(STAR_POS_SEED, idx) * f32::from(area.width)) as u16;
            let row = (hash01(STAR_POS_SEED.wrapping_add(1), idx) * f32::from(area.height)) as u16;
            let col = col.min(area.width - 1);
            let row = row.min(area.height - 1);
            if in_rect(col, row, margin) {
                return None;
            }
            if progress < hash01(STAR_FADE_SEED, idx) * STAR_FADE_IN_FRACTION {
                return None;
            }
            let t = noise(idx, phase * 0.6 + hash01(STAR_TWINKLE_SEED, idx) * 10.0);
            let tier = ((t * (STARFIELD_TIERS - 1) as f32).round() as usize).min(STARFIELD_TIERS - 1);
            Some((tier, col, row))
        })
        .collect()
}

/// `box_rect` grown by one cell in every direction (clamped to `area`), so
/// the starfield rings the console block rather than touching its edge.
fn box_margin(box_rect: Rect, area: Rect) -> Rect {
    let x = box_rect.x.saturating_sub(1).max(area.x);
    let y = box_rect.y.saturating_sub(1).max(area.y);
    let x2 = (box_rect.x + box_rect.width + 1).min(area.x + area.width);
    let y2 = (box_rect.y + box_rect.height + 1).min(area.y + area.height);
    Rect { x, y, width: x2.saturating_sub(x), height: y2.saturating_sub(y) }
}

fn in_rect(col: u16, row: u16, rect: Rect) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// How far the transit's background arc extends past each visible edge, as
/// a fraction of the frame width — enough that the satellite is already
/// mid-arc, not sitting exactly on the frame boundary, the instant `progress`
/// reaches 0 or 1. The overhang itself never has to be clipped by hand: it's
/// outside the canvas's own `x_bounds`, which `Canvas` clips on its own.
const TRANSIT_OVERHANG: f32 = 0.15;

/// Samples across the background arc — enough that the dome reads as a
/// curve rather than a handful of straight segments at any terminal width.
const TRANSIT_SAMPLES: usize = 48;

/// The transit arc's row (0 = top of `area`) at horizontal fraction `t`
/// (0 at the left edge, 1 at the right; outside `0..=1` is the overhang).
/// A dome shape — `shoulder` at the two visible edges, rising to `apex` at
/// the midpoint — with `shoulder` pinned at least one row above `box_top`
/// and `apex` at least one row above that, so the arc clears the console
/// block for any box tall enough to matter and never dips into it.
fn transit_row(t: f32, box_top: f32) -> f32 {
    let shoulder = (box_top - 1.0).max(1.0);
    let apex = (shoulder * 0.35).max(1.0).min(shoulder - 1.0).max(0.0);
    let dome = (1.0 - (2.0 * t - 1.0).powi(2)).max(0.0);
    shoulder - (shoulder - apex) * dome
}

/// The transit point at horizontal fraction `t`, as `(t, canvas x, canvas y)`
/// — canvas `y` increases upward, so a screen row `r` in `0..area.height`
/// becomes `area.height - r`.
fn point_at(area: Rect, box_top: f32, t: f32) -> (f32, f64, f64) {
    let row = transit_row(t, box_top);
    let x = f64::from(t * f32::from(area.width));
    let y = f64::from(f32::from(area.height) - row);
    (t, x, y)
}

/// The transit's whole visible path: the sampled background arc from just
/// past the left edge to just past the right, with the satellite's own exact
/// position at `progress` spliced in — so the flown and still-ahead halves
/// [`draw`] slices out of this meet precisely where the satellite is, rather
/// than at whichever fixed sample happens to be closest.
fn transit_track(area: Rect, box_rect: Rect, progress: f32) -> Vec<(f32, f64, f64)> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let box_top = f32::from(box_rect.y);
    let mut track: Vec<(f32, f64, f64)> = (0..TRANSIT_SAMPLES)
        .map(|i| {
            let t = -TRANSIT_OVERHANG
                + (i as f32 / (TRANSIT_SAMPLES - 1) as f32) * (1.0 + 2.0 * TRANSIT_OVERHANG);
            point_at(area, box_top, t)
        })
        .collect();
    let progress = progress.clamp(0.0, 1.0);
    let here = point_at(area, box_top, progress);
    let insert_at = track.partition_point(|(t, _, _)| *t < progress);
    track.insert(insert_at, here);
    track
}

/// A flat-colour polyline through consecutive `points` — the arc's
/// not-yet-flown remainder, dim and uniform since nothing has happened there
/// yet.
fn draw_track(ctx: &mut Context<'_>, points: &[(f32, f64, f64)], color: Color) {
    for w in points.windows(2) {
        ctx.draw(&CanvasLine { x1: w[0].1, y1: w[0].2, x2: w[1].1, y2: w[1].2, color });
    }
}

/// Like [`draw_track`], but fading from `old` at the earliest point to `new`
/// at the most recent — a comet tail behind the satellite, the same shape
/// `ui::map`'s `draw_fading_polyline` draws behind the ground track, just
/// over plain `(t, x, y)` points instead of a `GeoPoint` track.
fn draw_fading_track(ctx: &mut Context<'_>, points: &[(f32, f64, f64)], old: Color, new: Color) {
    let total_lines = points.len().saturating_sub(1);
    if total_lines == 0 {
        return;
    }
    for (i, w) in points.windows(2).enumerate() {
        // `total_lines - 1` guards the single-line case the same way
        // `draw_fading_polyline` does: with nothing to interpolate across,
        // the one line keeps `old`.
        let t = if total_lines > 1 { i as f32 / (total_lines - 1) as f32 } else { 0.0 };
        ctx.draw(&CanvasLine { x1: w[0].1, y1: w[0].2, x2: w[1].1, y2: w[1].2, color: lerp(old, new, t) });
    }
}

/// Paint the Canvas backdrop: starfield, then the transit's not-yet-flown
/// path, then its fading trail, then the satellite itself — last, so it wins
/// any cell it shares with the path beneath it, the same reasoning
/// `skyplot::paint` gives for drawing its own live marker last.
fn paint(ctx: &mut Context<'_>, area: Rect, box_rect: Rect, phase: f32, progress: f32) {
    let stars = starfield(area, box_rect, phase, progress);
    let mut any_star = false;
    for tier in 0..STARFIELD_TIERS {
        let points: Vec<(f64, f64)> = stars
            .iter()
            .filter(|(t, _, _)| *t == tier)
            .map(|(_, col, row)| (f64::from(*col) + 0.5, f64::from(area.height) - f64::from(*row) - 0.5))
            .collect();
        if points.is_empty() {
            continue;
        }
        let t = tier as f32 / (STARFIELD_TIERS - 1) as f32;
        ctx.draw(&Points { coords: &points, color: lerp(Theme::PLACE, Theme::VALUE, t) });
        any_star = true;
    }
    if any_star {
        ctx.layer();
    }

    let track = transit_track(area, box_rect, progress);
    let idx = track.partition_point(|(t, _, _)| *t < progress.clamp(0.0, 1.0));
    let ahead = &track[idx..];
    let flown = &track[..=idx.min(track.len().saturating_sub(1))];

    if ahead.len() > 1 {
        draw_track(ctx, ahead, Theme::FRAME);
        ctx.layer();
    }
    if flown.len() > 1 {
        draw_fading_track(ctx, flown, Theme::TRACK_PAST, Theme::SAT);
        ctx.layer();
    }
    if let Some(&(_, x, y)) = flown.last() {
        ctx.print(x, y, Span::styled("◆", Style::new().fg(Theme::SAT).bold()));
    }
}

/// Render the boot splash into `area`: the Canvas backdrop over the whole
/// frame, then the console text on top of it — `Paragraph` only ever writes
/// the cells its glyphs occupy, so the backdrop shows through every blank
/// one, and [`starfield`]'s box-margin exclusion is what keeps that from
/// reading as stars poking through *inside* the panel rather than around it.
/// The box is sized from the *fully revealed, fully lit* text
/// (`boot_text(&lines, lines.len(), 1.0)`), not from what's actually drawn
/// this frame — so it's always its final size, even on the very first frame,
/// and never resizes or re-centres as the rows fill in or the wordmark
/// ignites.
pub fn draw(frame: &mut Frame, area: Rect, app: &App, data: &AppData) {
    let lines = boot_lines(app.config.ground_station(), data.tle.get(), app.config.sat, app.config.offline);
    let revealed = splash_reveal_count(app.uptime(), app.config.ui.splash, lines.len());
    let splash = app.config.ui.splash;
    let uptime = app.uptime();
    let progress =
        if splash.is_zero() { 1.0 } else { (uptime.as_secs_f32() / splash.as_secs_f32()).clamp(0.0, 1.0) };

    let full = boot_text(&lines, lines.len(), 1.0);
    let height = full.len() as u16;
    let width = full.iter().map(Line::width).max().unwrap_or(0) as u16;
    let box_rect = centered(area, width, height);

    // Wall time since the session started, not `app.sim_now()` — scenery
    // riding along on the real clock, the same choice `App::uptime`
    // documents for every other idle-screen effect.
    let phase = uptime.as_secs_f32();
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds([0.0, f64::from(area.width)])
        .y_bounds([0.0, f64::from(area.height)])
        .paint(move |ctx| paint(ctx, area, box_rect, phase, progress));
    frame.render_widget(canvas, area);

    let text = boot_text(&lines, revealed, progress);
    frame.render_widget(Paragraph::new(text), box_rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geo::GeoPoint;

    #[test]
    fn boot_lines_reads_pending_data_honestly_rather_than_faking_it() {
        let lines = boot_lines(None, None, 25544, false);
        let value = |label: &str| lines.iter().find(|(k, _)| *k == label).map(|(_, v)| v.as_str());
        assert_eq!(value("ground station"), Some("not configured"));
        // The NORAD id is known from `config.sat` alone, so it's shown even
        // with no element set fetched yet — only the *name* half is pending.
        assert_eq!(value("element set"), Some("… · 25544"));
        assert_eq!(value("epoch"), Some("…"));
    }

    #[test]
    fn boot_lines_names_the_ground_station_with_hemisphere_letters() {
        let station = GeoPoint::new(48.137, 11.575, 0.0);
        let lines = boot_lines(Some(station), None, 25544, false);
        let (_, value) = lines.iter().find(|(k, _)| *k == "ground station").unwrap();
        assert_eq!(value, "48.137N 11.575E");
    }

    #[test]
    fn boot_lines_names_the_southern_and_western_hemispheres_too() {
        let station = GeoPoint::new(-33.865, -70.9, 0.0);
        let lines = boot_lines(Some(station), None, 25544, false);
        let (_, value) = lines.iter().find(|(k, _)| *k == "ground station").unwrap();
        assert_eq!(value, "33.865S 70.900W");
    }

    /// The bug the visual check caught: `BOOT_LEADER_WIDTH` was sized against
    /// the wrong label ("element set", not the actually-longest "ground
    /// station"), leaving only a single dot in the leader. Pins every real
    /// label to a handful of dots of headroom so a future label change can't
    /// quietly repeat that.
    #[test]
    fn the_boot_splash_leader_width_fits_every_real_label_with_dots_to_spare() {
        for (label, _) in boot_lines(None, None, 25544, false) {
            let spare = BOOT_LEADER_WIDTH.saturating_sub(label.chars().count());
            assert!(spare >= 4, "label {label:?} leaves only {spare} dots in the leader");
        }
    }

    #[test]
    fn boot_lines_names_the_tracker_once_the_element_set_has_arrived() {
        let tr = crate::orbit::test_tracker();
        let lines = boot_lines(None, Some(&tr), 25544, false);
        let value = |label: &str| lines.iter().find(|(k, _)| *k == label).map(|(_, v)| v.as_str());
        assert_eq!(value("element set"), Some(format!("{} · 25544", tr.name()).as_str()));
        assert_ne!(value("epoch"), Some("…"));
    }

    #[test]
    fn boot_lines_names_the_coordinate_pipeline_and_the_feeds_it_will_poll() {
        let lines = boot_lines(None, None, 25544, false);
        let value = |label: &str| lines.iter().find(|(k, _)| *k == label).map(|(_, v)| v.as_str());
        assert_eq!(value("frames"), Some("TEME → ECEF → WGS84"));
        assert_eq!(value("feeds"), Some("celestrak · swpc · launch library"));
    }

    /// `--offline` (`config.offline`) is known at boot without fetching
    /// anything, so the splash says up front that nothing will be fetched
    /// rather than listing feeds that will never be polled this session.
    #[test]
    fn boot_lines_names_the_feeds_row_offline_when_the_config_is_offline() {
        let lines = boot_lines(None, None, 25544, true);
        let value = |label: &str| lines.iter().find(|(k, _)| *k == label).map(|(_, v)| v.as_str());
        assert_eq!(value("feeds"), Some("offline — cache only"));
    }

    /// The row count [`draw`] builds — wordmark, version, a blank, every
    /// `boot_lines` row, then a blank and the footer — must still fit
    /// comfortably under the 80×24 minimum terminal size.
    #[test]
    fn the_boot_splash_height_fits_the_minimum_terminal_size() {
        let rows = NADIR_WORDMARK.len() + 2 + boot_lines(None, None, 25544, false).len() + 2;
        assert!(rows <= 24, "boot splash is {rows} rows tall, taller than the 24-row minimum");
    }

    #[test]
    fn splash_reveal_count_starts_at_one_and_reaches_every_row_by_a_third_of_the_splash_duration() {
        let splash = Duration::from_secs(6);
        assert_eq!(splash_reveal_count(Duration::ZERO, splash, 6), 1);
        assert_eq!(splash_reveal_count(splash / 3, splash, 6), 6);
    }

    /// The reveal must not creep past the one-third point — the remaining
    /// two-thirds are meant to be a static hold for the rows, even though the
    /// backdrop keeps animating underneath them.
    #[test]
    fn splash_reveal_count_holds_at_every_row_through_the_remaining_two_thirds() {
        let splash = Duration::from_secs(6);
        assert_eq!(splash_reveal_count(splash / 3 + Duration::from_millis(1), splash, 6), 6);
        assert_eq!(splash_reveal_count(splash, splash, 6), 6);
        // Even a key press that lands right on the boundary, or a frame that
        // ticks a little past `splash` before `splash_active` catches up,
        // must not panic or overshoot `total`.
        assert_eq!(splash_reveal_count(splash * 2, splash, 6), 6);
    }

    #[test]
    fn splash_reveal_count_climbs_partway_through_the_first_third() {
        let splash = Duration::from_secs(6);
        let count = splash_reveal_count(Duration::from_secs(1), splash, 6);
        assert!((1..6).contains(&count), "expected a partial reveal, got {count}/6");
    }

    #[test]
    fn splash_reveal_count_is_zero_for_an_empty_line_list() {
        assert_eq!(splash_reveal_count(Duration::ZERO, Duration::from_secs(4), 0), 0);
    }

    /// The whole point: `boot_text` must return the same number of lines
    /// whether a row has revealed yet or not, so the box [`draw`] sizes from
    /// it never resizes as rows fill in — only line *content* changes, never
    /// line *count*.
    #[test]
    fn boot_text_has_the_same_line_count_at_every_reveal_step() {
        let lines = boot_lines(None, None, 25544, false);
        let full_len = boot_text(&lines, lines.len(), 1.0).len();
        for revealed in 0..=lines.len() {
            assert_eq!(boot_text(&lines, revealed, 1.0).len(), full_len, "revealed={revealed}");
        }
    }

    /// An unrevealed row is a blank placeholder, not its label text — so a
    /// row's content only appears once `revealed` reaches it.
    #[test]
    fn boot_text_shows_only_the_revealed_rows_content() {
        let lines = boot_lines(None, None, 25544, false);
        let text = boot_text(&lines, 1, 1.0);
        let rendered: String = text.iter().flat_map(|l| l.spans.iter()).map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains("ground station"), "the first row should show: {rendered}");
        assert!(!rendered.contains("element set"), "the second row should still be blank: {rendered}");
    }

    #[test]
    fn the_wordmark_ignition_lights_columns_left_to_right_without_gaps() {
        for progress in [0.0, 0.05, 0.1, 0.15, 0.2, 0.5, 1.0] {
            for row in NADIR_WORDMARK {
                let cols = row.chars().count();
                let mut last = f32::INFINITY;
                for col in 0..cols {
                    let t = ignition_brightness(col, cols, progress);
                    assert!(
                        t <= last + 1e-6,
                        "column {col} brighter than the column before it at progress {progress}"
                    );
                    last = t;
                }
            }
        }
    }

    #[test]
    fn the_wordmark_is_fully_lit_once_its_ignition_sweep_has_finished() {
        for row in NADIR_WORDMARK {
            let line = ignite_wordmark_row(row, 1.0);
            for span in &line.spans {
                assert_eq!(span.style.fg, Some(Theme::SAT), "expected every column lit at progress 1.0");
            }
        }
    }

    #[test]
    fn the_wordmark_starts_fully_dark() {
        for row in NADIR_WORDMARK {
            let line = ignite_wordmark_row(row, 0.0);
            for span in &line.spans {
                assert_eq!(span.style.fg, Some(Theme::FRAME));
            }
        }
    }

    #[test]
    fn the_starfield_never_places_a_star_behind_the_console_block() {
        let area = Rect::new(0, 0, 80, 24);
        let box_rect = Rect::new(20, 5, 40, 13);
        for phase in [0.0, 1.3, 5.0] {
            for progress in [0.0, 0.5, 1.0] {
                for (_, col, row) in starfield(area, box_rect, phase, progress) {
                    assert!(!in_rect(col, row, box_rect), "star landed inside the box at ({col},{row})");
                }
            }
        }
    }

    #[test]
    fn the_starfield_is_deterministic_for_the_same_area_and_phase() {
        let area = Rect::new(0, 0, 80, 24);
        let box_rect = Rect::new(20, 5, 40, 13);
        let a = starfield(area, box_rect, 3.0, 1.0);
        let b = starfield(area, box_rect, 3.0, 1.0);
        assert_eq!(a, b);
    }

    #[test]
    fn the_starfield_stays_inside_the_frame_at_the_minimum_terminal_size() {
        let area = Rect::new(0, 0, 80, 24);
        let box_rect = Rect::new(20, 5, 40, 13);
        for (_, col, row) in starfield(area, box_rect, 2.0, 1.0) {
            assert!(col < area.width && row < area.height, "star out of bounds at ({col},{row})");
        }
    }

    #[test]
    fn the_satellite_crosses_the_frame_exactly_once_across_the_splash_duration() {
        let area = Rect::new(0, 0, 80, 24);
        let box_top = 5.0_f32;
        let (_, x0, _) = point_at(area, box_top, 0.0);
        let (_, x1, _) = point_at(area, box_top, 1.0);
        assert_eq!(x0, 0.0, "should start exactly at the left edge");
        assert_eq!(x1, f64::from(area.width), "should finish exactly at the right edge");

        let mut last_x = f64::NEG_INFINITY;
        for i in 0..=20 {
            let progress = i as f32 / 20.0;
            let (t, x, _) = point_at(area, box_top, progress);
            assert_eq!(t, progress);
            assert!(x >= last_x, "x should never move backwards as progress advances");
            last_x = x;
        }
    }

    #[test]
    fn the_transit_arc_clears_the_console_block_at_the_minimum_terminal_size() {
        let area = Rect::new(0, 0, 80, 24);
        let lines = boot_lines(None, None, 25544, false);
        let full = boot_text(&lines, lines.len(), 1.0);
        let height = full.len() as u16;
        let width = full.iter().map(Line::width).max().unwrap_or(0) as u16;
        let box_rect = centered(area, width, height);

        for progress in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
            for (t, _, y) in transit_track(area, box_rect, progress) {
                let row = f64::from(area.height) - y;
                assert!(
                    row < f64::from(box_rect.y),
                    "arc dipped into the box at progress {progress}, t {t}: row {row} >= {}",
                    box_rect.y
                );
            }
        }
    }

    /// Renders `draw_fading_track` alone into a fresh buffer and returns the
    /// fg colour of every non-blank cell it painted, in left-to-right,
    /// top-to-bottom scan order — the same technique `ui::map`'s own
    /// `render_fading_track` test helper uses for `draw_fading_polyline`.
    fn render_fading_track(points: &[(f32, f64, f64)], old: Color, new: Color) -> Vec<Color> {
        use ratatui::widgets::Widget;
        let rect = Rect::new(0, 0, 60, 20);
        let mut buf = ratatui::buffer::Buffer::empty(rect);
        Canvas::default()
            .marker(Marker::Braille)
            .x_bounds([0.0, 60.0])
            .y_bounds([0.0, 20.0])
            .paint(|ctx| draw_fading_track(ctx, points, old, new))
            .render(rect, &mut buf);
        buf.content.iter().filter(|c| c.symbol() != " ").map(|c| c.fg).collect()
    }

    #[test]
    fn the_trail_fades_from_the_satellite_colour_towards_the_receding_track_colour() {
        let points: Vec<(f32, f64, f64)> = (0..=12).map(|i| (i as f32, f64::from(i) * 5.0, 10.0)).collect();
        let old = Theme::TRACK_PAST;
        let new = Theme::SAT;
        let colors = render_fading_track(&points, old, new);
        assert!(colors.len() >= 2, "expected the trail to paint more than one cell");
        assert_eq!(
            *colors.last().unwrap(),
            new,
            "the segment nearest the satellite should land on the satellite's own colour"
        );
        assert_eq!(
            *colors.first().unwrap(),
            old,
            "the oldest segment should stay at the receding track colour"
        );
    }
}
