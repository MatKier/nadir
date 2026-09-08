//! The sky plot: a polar azimuth/elevation chart of the pass highlighted in
//! NEXT PASSES, drawn into the map pane while that panel holds focus.
//!
//! Zenith at the centre, the horizon at the rim, north up and azimuth
//! increasing clockwise — a compass rose you read by facing the direction a
//! mark sits from centre. Every angle it needs is local SGP4
//! ([`crate::orbit::sample_pass`]); like [`crate::ui::map`] it is a `Canvas`
//! of braille strokes and follows that module's one-`layer`-per-feature
//! discipline (see the long note in `map::paint_scene` for why braille
//! features must not share a layer, and why `ctx.layer()` is only called after
//! something actually drew).

use chrono::Local;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::canvas::{Canvas, Circle, Context, Line as CanvasLine};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::orbit::{sample_pass, sky_sample, Pass, SkySample, Tracker};
use crate::ui::panels::compass;
use crate::ui::{fitting_hint, panel_block, Theme};

/// Time samples across the arc. ~5 s apart on a ten-minute pass — fine enough
/// that the sunlit→eclipsed transition lands within a few seconds and the
/// stroke reads smooth. Around 120 SGP4 propagations, tens of microseconds, so
/// it is recomputed every frame with no cache (cf. the multi-day pass scan at
/// ~7 ms in `app::PASS_REBUILD_FLOOR`'s note).
const ARC_SAMPLES: usize = 120;

/// Draw the plot for `pass` (row `index` of `total` in NEXT PASSES) into
/// `area`, sampling the arc with `tracker` for the station in `app.config`.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    pass: &Pass,
    index: usize,
    total: usize,
    tracker: &Tracker,
) {
    // The caller only reaches here with a highlighted pass, and a pass list is
    // only ever non-empty when a ground station is set — but read it here
    // rather than assume it, so a future caller can't make this panic.
    let Some(station) = app.config.ground_station() else {
        return;
    };

    let title = plot_title(pass, index, total, area.width);
    // Still the map's pane, so it keeps the map's panel key in the header;
    // only the contents change. Never drawn focused — the highlight that
    // drives it lives in NEXT PASSES, which is what actually holds focus.
    let block = panel_block(Panel::Map, &title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let samples = sample_pass(tracker, &station, pass, ARC_SAMPLES);
    let peak = sky_sample(tracker, &station, pass.peak);
    let footer = plot_footer(pass);

    // A one-row footer under the disc, but only when the pane can spare it —
    // a short map needs every row for the disc to stay round. The block is
    // drawn on its own above so its *inner* rect can be split here, the same
    // reason NEXT PASSES draws its own block before laying out its accuracy
    // footer.
    let (disc_area, footer_area) = if inner.height >= 8 {
        let [d, f] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
        (d, Some(f))
    } else {
        (inner, None)
    };

    let (x_bounds, y_bounds) = disc_bounds(disc_area);
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds(x_bounds)
        .y_bounds(y_bounds)
        .paint(|ctx| paint(ctx, pass, &samples, peak));
    frame.render_widget(canvas, disc_area);

    if let Some(fa) = footer_area {
        if let Some(text) = fitting_hint(&footer, fa.width) {
            frame.render_widget(
                Paragraph::new(Span::styled(format!(" {text}"), Style::new().fg(Theme::LABEL))),
                fa,
            );
        }
    }
}

/// Project a look angle onto the unit disc: zenith (elevation 90°) at the
/// origin, the horizon (0°) at radius 1, north up, azimuth increasing
/// clockwise — the mapping a compass rose wants. Because both axes are angles
/// about the centre, a straight segment between two samples in this space
/// stays sane even when their azimuths differ by nearly 180° across a
/// near-zenith pass: both points are then near the origin and so is the
/// segment, which is where the true track goes. That is why the arc needs no
/// azimuth-unwrapping, unlike the map's lon/lat polylines.
fn project(azimuth_deg: f64, elevation_deg: f64) -> (f64, f64) {
    let r = ((90.0 - elevation_deg) / 90.0).clamp(0.0, 1.0);
    let (sin_a, cos_a) = azimuth_deg.to_radians().sin_cos();
    (r * sin_a, r * cos_a)
}

/// Coordinate bounds for a disc canvas that renders as a true circle in
/// `area`. A braille cell is 2 dots wide by 4 tall, so `area` is `2w × 4h`
/// dots; the axis wider in dots gets the proportionally larger coordinate
/// span, so one dot is the same coordinate step on both axes and the rim is a
/// circle rather than an ellipse. This is the polar analogue of the map's
/// fixed 2:1 lon:lat half-span (see `map::view_bounds`). Both spans come out
/// ≥ 2, so the radius-1 disc always fits with room to spare on the wider axis.
fn disc_bounds(area: Rect) -> ([f64; 2], [f64; 2]) {
    let w = area.width.max(1) as f64 * 2.0;
    let h = area.height.max(1) as f64 * 4.0;
    if w >= h {
        let ratio = w / h;
        ([-ratio, ratio], [-1.0, 1.0])
    } else {
        let ratio = h / w;
        ([-1.0, 1.0], [-ratio, ratio])
    }
}

fn paint(ctx: &mut Context<'_>, pass: &Pass, samples: &[SkySample], peak: Option<SkySample>) {
    // Elevation rings — the 0° horizon rim, then 30° and 60°. Reference
    // scenery, so PLACE, the same neutral slate the map's place layer uses.
    // A `Circle` in canvas units is round on screen because `disc_bounds` has
    // already corrected for the braille cell aspect. Rings are left
    // unlabelled: the only elevation figure worth the space is the peak's,
    // and its marker carries that.
    for elev in [0.0_f64, 30.0, 60.0] {
        ctx.draw(&Circle {
            x: 0.0,
            y: 0.0,
            radius: (90.0 - elev) / 90.0,
            color: Theme::PLACE,
        });
    }
    ctx.layer();

    // The arc in two passes so the two colours never fuse in a shared braille
    // cell: eclipsed spans (dim) first, then sunlit (bright) on its own
    // layer, so the bright colour wins any cell they meet in at the shadow
    // boundary.
    draw_arc(ctx, samples, false, Theme::TRACK_PAST);
    ctx.layer();
    draw_arc(ctx, samples, true, Theme::TRACK_FUTURE);
    ctx.layer();

    // Compass letters, pulled a few degrees inside the rim so they clear
    // ratatui's label filter (which drops, rather than clamps, an anchor on
    // the bound).
    for (letter, az) in [("N", 0.0), ("E", 90.0), ("S", 180.0), ("W", 270.0)] {
        let (x, y) = project(az, 4.0);
        ctx.print(x, y, Span::styled(letter, Style::new().fg(Theme::LABEL)));
    }

    // AOS and LOS on the rim, culmination where the arc peaks. The AOS/LOS
    // azimuths come straight from the `Pass` summary so the marks agree with
    // the row's `SW→NE` bearings to the degree.
    let (ax, ay) = project(pass.aos_azimuth_deg, 0.0);
    ctx.print(ax, ay, Span::styled("▲", Style::new().fg(Theme::LABEL)));
    let (lx, ly) = project(pass.los_azimuth_deg, 0.0);
    ctx.print(lx, ly, Span::styled("▼", Style::new().fg(Theme::LABEL)));
    if let Some(p) = peak {
        let (px, py) = project(p.azimuth_deg, p.elevation_deg);
        ctx.print(
            px,
            py,
            Span::styled(
                format!("◆ {:.0}°", pass.peak_elevation_deg),
                Style::new().fg(Theme::SAT),
            ),
        );
    }
}

/// Stroke the run of samples whose `sunlit` matches `want`, as braille line
/// segments between consecutive same-state points. A state change or a
/// propagation gap (a dropped sample) breaks the stroke, so a sunlit span and
/// an eclipsed span never join with a segment of the wrong colour — leaving a
/// one-sample (~5 s) gap at each shadow crossing, which reads as the boundary
/// rather than an error.
fn draw_arc(ctx: &mut Context<'_>, samples: &[SkySample], want: bool, color: Color) {
    for w in samples.windows(2) {
        if w[0].sunlit != want || w[1].sunlit != want {
            continue;
        }
        let (x1, y1) = project(w[0].azimuth_deg, w[0].elevation_deg);
        let (x2, y2) = project(w[1].azimuth_deg, w[1].elevation_deg);
        ctx.draw(&CanvasLine { x1, y1, x2, y2, color });
    }
}

/// `SKY · pass 2 of 7`, plus ` · ★` when the pass is naked-eye and the header
/// has room for it.
fn plot_title(pass: &Pass, index: usize, total: usize, width: u16) -> String {
    let base = format!("SKY · pass {} of {}", index + 1, total);
    if pass.visible && base.chars().count() + 4 <= width as usize {
        format!("{base} · ★")
    } else {
        base
    }
}

/// Footer tiers for [`fitting_hint`], widest first: AOS/culmination/LOS in
/// local time with compass bearings and the total duration, trimmed step by
/// step down to just the peak elevation and length.
fn plot_footer(pass: &Pass) -> Vec<String> {
    let hms = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&Local).format("%H:%M:%S").to_string();
    let hm = |t: chrono::DateTime<chrono::Utc>| t.with_timezone(&Local).format("%H:%M").to_string();
    let dur = pass.duration();
    let (mins, secs) = (dur.num_minutes(), dur.num_seconds() % 60);
    let a = compass(pass.aos_azimuth_deg);
    let l = compass(pass.los_azimuth_deg);
    vec![
        format!(
            "AOS {} {a} {:.0}°  ·  max {:.1}° {}  ·  LOS {} {l}  ·  {mins}m{secs:02}s",
            hms(pass.aos),
            pass.aos_azimuth_deg,
            pass.peak_elevation_deg,
            hm(pass.peak),
            hms(pass.los),
        ),
        format!(
            "AOS {} {a}  ·  max {:.0}° {}  ·  LOS {} {l}  ·  {mins}m",
            hm(pass.aos),
            pass.peak_elevation_deg,
            hm(pass.peak),
            hm(pass.los),
        ),
        format!("{a}→{l}  ·  max {:.0}°  ·  {mins}m", pass.peak_elevation_deg),
        format!("max {:.0}°  ·  {mins}m", pass.peak_elevation_deg),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_zenith_projects_to_the_centre_and_the_horizon_to_the_rim() {
        let (x, y) = project(123.0, 90.0);
        assert!(x.hypot(y) < 1e-9, "the zenith is the origin");
        let (x, y) = project(123.0, 0.0);
        assert!((x.hypot(y) - 1.0).abs() < 1e-9, "the horizon is radius 1");
        let (x, y) = project(45.0, 45.0);
        assert!((x.hypot(y) - 0.5).abs() < 1e-9, "radius is linear in elevation");
    }

    #[test]
    fn due_north_projects_up_and_azimuth_runs_clockwise() {
        let (nx, ny) = project(0.0, 0.0);
        assert!(nx.abs() < 1e-9 && ny > 0.99, "north is +y (up)");
        let (ex, ey) = project(90.0, 0.0);
        assert!(ex > 0.99 && ey.abs() < 1e-9, "east is +x (right) — clockwise from north");
        let (sx, sy) = project(180.0, 0.0);
        assert!(sx.abs() < 1e-9 && sy < -0.99, "south is -y (down)");
        let (wx, wy) = project(270.0, 0.0);
        assert!(wx < -0.99 && wy.abs() < 1e-9, "west is -x (left)");
    }

    #[test]
    fn the_horizon_rim_stays_round_at_every_pane_size() {
        for w in 10..100u16 {
            for h in 6..50u16 {
                let ([x0, x1], [y0, y1]) = disc_bounds(Rect::new(0, 0, w, h));
                // Coordinate span per screen dot, each axis — a round rim
                // needs the two equal.
                let x_per_dot = (x1 - x0) / (f64::from(w) * 2.0);
                let y_per_dot = (y1 - y0) / (f64::from(h) * 4.0);
                assert!(
                    (x_per_dot - y_per_dot).abs() < 1e-9,
                    "{w}x{h}: {x_per_dot} vs {y_per_dot} per dot",
                );
                assert!(x1 >= 1.0 && y1 >= 1.0, "{w}x{h}: the unit disc must fit");
            }
        }
    }

    #[test]
    fn the_arc_draws_its_sunlit_and_eclipsed_spans_in_different_colours() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;

        // A synthetic arc: a symmetric rise to the zenith, sunlit for the
        // first half and eclipsed for the second.
        let now = chrono::Utc::now();
        let samples: Vec<SkySample> = (0..=40)
            .map(|i| {
                let f = f64::from(i) / 40.0;
                SkySample {
                    at: now,
                    azimuth_deg: 90.0 + f * 90.0,
                    elevation_deg: 90.0 * (1.0 - (2.0 * f - 1.0).abs()),
                    sunlit: i < 20,
                }
            })
            .collect();
        let pass = Pass {
            aos: now,
            los: now + chrono::Duration::minutes(6),
            peak: now + chrono::Duration::minutes(3),
            peak_elevation_deg: 90.0,
            aos_azimuth_deg: 90.0,
            los_azimuth_deg: 180.0,
            visible: true,
        };

        let rect = Rect::new(0, 0, 40, 20);
        let (x, y) = disc_bounds(rect);
        let mut buf = Buffer::empty(rect);
        Canvas::default()
            .marker(Marker::Braille)
            .x_bounds(x)
            .y_bounds(y)
            .paint(|ctx| paint(ctx, &pass, &samples, None))
            .render(rect, &mut buf);

        let any_fg = |c: Color| buf.content.iter().any(|cell| cell.fg == c);
        assert!(any_fg(Theme::TRACK_FUTURE), "the sunlit span should be drawn bright");
        assert!(any_fg(Theme::TRACK_PAST), "the eclipsed span should be drawn dim");
        assert!(any_fg(Theme::PLACE), "the elevation rings should be drawn");
    }
}
