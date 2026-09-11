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

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::canvas::{Canvas, Circle, Context, Line as CanvasLine};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::geo::{look_angles, GeoPoint, LookAngles};
use crate::orbit::{sample_pass, sky_sample, Pass, SatState, SkySample, Tracker};
use crate::ui::canvas::{Grid, DOTS_X, DOTS_Y};
use crate::ui::panels::{compass, local_date, local_hm, local_hms, split_footer};
use crate::ui::{fitting_hint, panel_block, Highlight, Theme, PANEL_CHROME};

/// Time samples across the arc. ~5 s apart on a ten-minute pass — fine enough
/// that the sunlit→eclipsed transition lands within a few seconds and the
/// stroke reads smooth. Around 120 SGP4 propagations, tens of microseconds, so
/// it is recomputed every frame with no cache (cf. the multi-day pass scan at
/// ~7 ms in `app::PASS_REBUILD_FLOOR`'s note).
const ARC_SAMPLES: usize = 120;

/// Draw the plot for the highlighted pass into `area`, sampling the arc with
/// `tracker` for the station in `app.config`. `state` is this frame's
/// already-propagated satellite state, which places the live position marker
/// when the pass happens to be under way.
pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    highlight: &Highlight<'_>,
    tracker: &Tracker,
    state: &SatState,
) {
    let pass = highlight.pass;
    // The caller only reaches here with a highlighted pass, and a pass list is
    // only ever non-empty when a ground station is set — but read it here
    // rather than assume it, so a future caller can't make this panic.
    let Some(station) = app.config.ground_station() else {
        return;
    };

    let title = plot_title(highlight, area.width);
    // Still the map's pane, so it keeps the map's panel key in the header;
    // only the contents change. Never drawn focused — the highlight that
    // drives it lives in NEXT PASSES, which is what actually holds focus.
    let block = panel_block(Panel::Map, &title, false);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let samples = sample_pass(tracker, &station, pass, ARC_SAMPLES);
    let peak = sky_sample(tracker, &station, pass.peak);
    let live = live_look(pass, &station, state);
    let footer = plot_footer(pass);

    // A one-row footer under the disc, but only when the pane can spare it —
    // a short map needs every row for the disc to stay round. `split_footer`
    // wants the block already drawn (above) so it splits the *inner* rect, the
    // same shape NEXT PASSES uses for its accuracy footer.
    let (disc_area, footer_area) = split_footer(inner, inner.height >= 8);

    let (x_bounds, y_bounds) = disc_bounds(disc_area);
    let grid = Grid { inner: disc_area, x: x_bounds, y: y_bounds };
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds(x_bounds)
        .y_bounds(y_bounds)
        .paint(|ctx| paint(ctx, &grid, pass, &samples, peak, live));
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

/// Where the satellite is *right now*, or `None` when the simulated clock is
/// not inside this pass — which is the usual case, since the highlighted pass
/// is normally still ahead.
///
/// `state.time` is the instant `state` was propagated to, so the window test
/// and the look angles cannot be taken from different moments however the
/// caller got hold of it. It is also the very state TELEMETRY reads, so this
/// marker and the `RANGE` row can never disagree — and it costs no propagation
/// of its own, `ui::draw` having already done the one for this frame.
fn live_look(pass: &Pass, station: &GeoPoint, state: &SatState) -> Option<LookAngles> {
    if state.time < pass.aos || state.time > pass.los {
        return None;
    }
    Some(look_angles(station, state.ecef_km))
}

/// Coordinate bounds for a disc canvas that renders as a true circle in
/// `area`. A braille cell is [`DOTS_X`]×[`DOTS_Y`] dots, so `area` is
/// `DOTS_X·w × DOTS_Y·h` dots; the axis wider in dots gets the proportionally
/// larger coordinate span, so one dot is the same coordinate step on both axes
/// and the rim is a circle rather than an ellipse. This is the polar analogue
/// of the map's fixed 2:1 lon:lat half-span (see `map::view_bounds`). Both
/// spans come out ≥ 2, so the radius-1 disc always fits with room to spare on
/// the wider axis.
fn disc_bounds(area: Rect) -> ([f64; 2], [f64; 2]) {
    let w = area.width.max(1) as f64 * f64::from(DOTS_X);
    let h = area.height.max(1) as f64 * f64::from(DOTS_Y);
    if w >= h {
        let ratio = w / h;
        ([-ratio, ratio], [-1.0, 1.0])
    } else {
        let ratio = h / w;
        ([-1.0, 1.0], [-ratio, ratio])
    }
}

fn paint(
    ctx: &mut Context<'_>,
    grid: &Grid,
    pass: &Pass,
    samples: &[SkySample],
    peak: Option<SkySample>,
    live: Option<LookAngles>,
) {
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
    // boundary. `ctx.layer()` only where a span actually drew — an empty layer
    // is a no-op that still allocates a full-grid buffer (the rule
    // `map::paint_scene` spells out), and a fully sunlit daytime pass leaves
    // the eclipsed span empty every frame.
    if draw_arc(ctx, samples, false, Theme::TRACK_PAST) {
        ctx.layer();
    }
    if draw_arc(ctx, samples, true, Theme::TRACK_FUTURE) {
        ctx.layer();
    }

    // Compass letters, pulled a few degrees inside the rim so they clear
    // ratatui's label filter (which drops, rather than clamps, an anchor on
    // the bound).
    for (letter, az) in [("N", 0.0), ("E", 90.0), ("S", 180.0), ("W", 270.0)] {
        let (x, y) = project(az, 4.0);
        grid.print_dot(ctx, x, y, Span::styled(letter, Style::new().fg(Theme::LABEL)));
    }

    // AOS and LOS on the rim, culmination where the arc peaks. The AOS/LOS
    // azimuths come straight from the `Pass` summary so the marks agree with
    // the row's `SW→NE` bearings to the degree. Culmination is a *hollow* `◇`:
    // the filled `◆` means "the satellite is here, now" everywhere else in
    // nadir (the map's sub-satellite point, and the live marker below), so the
    // predicted peak must not borrow it.
    let (ax, ay) = project(pass.aos_azimuth_deg, 0.0);
    grid.print_dot(ctx, ax, ay, Span::styled("▲", Style::new().fg(Theme::LABEL)));
    let (lx, ly) = project(pass.los_azimuth_deg, 0.0);
    grid.print_dot(ctx, lx, ly, Span::styled("▼", Style::new().fg(Theme::LABEL)));
    if let Some(p) = peak {
        let (px, py) = project(p.azimuth_deg, p.elevation_deg);
        grid.print_dot(
            ctx,
            px,
            py,
            Span::styled(
                format!("◇ {:.0}°", pass.peak_elevation_deg),
                Style::new().fg(Theme::SAT),
            ),
        );
    }

    // The satellite itself, printed *last* so it wins any cell it shares with
    // the marks above — canvas labels are drawn after every layer, in
    // insertion order, and a later one overwrites an earlier one in the same
    // cell. Bare glyph, no figure beside it: TELEMETRY's `RANGE` row already
    // reads out live elevation and slant range from this same state, and a
    // label here would collide with the peak's exactly when the satellite is
    // nearest culmination — which is when it is most worth watching.
    if let Some(l) = live {
        let (x, y) = project(l.azimuth_deg, l.elevation_deg);
        grid.print_dot(ctx, x, y, Span::styled("◆", Style::new().fg(Theme::SAT).bold()));
    }
}

/// Stroke the run of samples whose `sunlit` matches `want`, as braille line
/// segments between consecutive same-state points. A state change or a
/// propagation gap (a dropped sample) breaks the stroke, so a sunlit span and
/// an eclipsed span never join with a segment of the wrong colour — leaving a
/// one-sample (~5 s) gap at each shadow crossing, which reads as the boundary
/// rather than an error.
///
/// Returns whether any segment was drawn, so the caller only spends a
/// `ctx.layer()` on a span that put something on the canvas.
fn draw_arc(ctx: &mut Context<'_>, samples: &[SkySample], want: bool, color: Color) -> bool {
    let mut drew = false;
    for w in samples.windows(2) {
        if w[0].sunlit != want || w[1].sunlit != want {
            continue;
        }
        let (x1, y1) = project(w[0].azimuth_deg, w[0].elevation_deg);
        let (x2, y2) = project(w[1].azimuth_deg, w[1].elevation_deg);
        ctx.draw(&CanvasLine { x1, y1, x2, y2, color });
        drew = true;
    }
    drew
}

/// `SKY · pass 2 of 7`, plus ` · ★` when the pass is naked-eye and the header
/// has room for it. Budgeted against `width - PANEL_CHROME`, the columns the
/// block's border and key eat — not the raw pane width — so the star is
/// dropped a little before the title would actually be clipped rather than a
/// little after.
fn plot_title(highlight: &Highlight<'_>, width: u16) -> String {
    let base = format!("SKY · pass {} of {}", highlight.index + 1, highlight.total);
    let with_star = format!("{base} · ★");
    let budget = (width as usize).saturating_sub(PANEL_CHROME);
    if highlight.pass.visible && with_star.chars().count() <= budget {
        with_star
    } else {
        base
    }
}

/// Footer tiers for [`fitting_hint`], widest first: the pass's local date,
/// then AOS/culmination/LOS in local time with compass bearings and the total
/// duration, trimmed step by step down to just the peak elevation and length.
/// The date sits only on the top tier — every rung below it already dropped
/// the seconds, so putting the date underneath would break the ladder's rule
/// that each rung says strictly less than the one above it.
fn plot_footer(pass: &Pass) -> Vec<String> {
    let dur = pass.duration();
    let (mins, secs) = (dur.num_minutes(), dur.num_seconds() % 60);
    let a = compass(pass.aos_azimuth_deg);
    let l = compass(pass.los_azimuth_deg);
    let full = format!(
        "AOS {} {a} {:.0}°  ·  max {:.1}° {}  ·  LOS {} {l}  ·  {mins}m{secs:02}s",
        local_hms(pass.aos),
        pass.aos_azimuth_deg,
        pass.peak_elevation_deg,
        local_hm(pass.peak),
        local_hms(pass.los),
    );
    vec![
        format!("{}  ·  {full}", local_date(pass.aos)),
        full,
        format!(
            "AOS {} {a}  ·  max {:.0}° {}  ·  LOS {} {l}  ·  {mins}m",
            local_hm(pass.aos),
            pass.peak_elevation_deg,
            local_hm(pass.peak),
            local_hm(pass.los),
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
                let x_per_dot = (x1 - x0) / (f64::from(w) * f64::from(DOTS_X));
                let y_per_dot = (y1 - y0) / (f64::from(h) * f64::from(DOTS_Y));
                assert!(
                    (x_per_dot - y_per_dot).abs() < 1e-9,
                    "{w}x{h}: {x_per_dot} vs {y_per_dot} per dot",
                );
                assert!(x1 >= 1.0 && y1 >= 1.0, "{w}x{h}: the unit disc must fit");
            }
        }
    }

    /// The shared ISS-over-Munich fixture ([`crate::orbit::test_passes`]), as
    /// the `(tracker, station, first pass)` triple these tests want.
    fn iss_pass_over_munich() -> (crate::orbit::Tracker, GeoPoint, Pass) {
        let pass = crate::orbit::test_passes()
            .into_iter()
            .next()
            .expect("at least one ISS pass in 48 h");
        (crate::orbit::test_tracker(), crate::orbit::test_station(), pass)
    }

    #[test]
    fn the_live_marker_is_absent_unless_the_clock_is_inside_the_pass() {
        let (tr, munich, pass) = iss_pass_over_munich();
        let at = |t| tr.state_at(t).expect("the fixture propagates");

        assert!(
            live_look(&pass, &munich, &at(pass.aos - chrono::Duration::minutes(1))).is_none(),
            "nothing to mark before the satellite has risen",
        );
        assert!(
            live_look(&pass, &munich, &at(pass.los + chrono::Duration::minutes(1))).is_none(),
            "nor after it has set",
        );
        assert!(
            live_look(&pass, &munich, &at(pass.peak)).is_some(),
            "but the satellite is somewhere on the arc mid-pass",
        );
    }

    #[test]
    fn the_live_marker_sits_on_the_arc_at_culmination() {
        let (tr, munich, pass) = iss_pass_over_munich();
        let live = live_look(&pass, &munich, &tr.state_at(pass.peak).unwrap())
            .expect("culmination is inside the pass");
        assert!(
            (live.elevation_deg - pass.peak_elevation_deg).abs() < 0.1,
            "marker at {:.2}° vs the pass's peak {:.2}°",
            live.elevation_deg,
            pass.peak_elevation_deg,
        );
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
        let grid = Grid { inner: rect, x, y };
        let render = |live: Option<LookAngles>| {
            let mut buf = Buffer::empty(rect);
            Canvas::default()
                .marker(Marker::Braille)
                .x_bounds(x)
                .y_bounds(y)
                .paint(|ctx| paint(ctx, &grid, &pass, &samples, None, live))
                .render(rect, &mut buf);
            buf
        };

        let buf = render(None);
        let any_fg = |b: &Buffer, c: Color| b.content.iter().any(|cell| cell.fg == c);
        assert!(any_fg(&buf, Theme::TRACK_FUTURE), "the sunlit span should be drawn bright");
        assert!(any_fg(&buf, Theme::TRACK_PAST), "the eclipsed span should be drawn dim");
        assert!(any_fg(&buf, Theme::PLACE), "the elevation rings should be drawn");
    }

    /// `◆` is matched on the symbol rather than the colour, because the
    /// culmination `◇` shares `Theme::SAT` with it.
    #[test]
    fn the_live_marker_is_drawn_only_when_there_is_a_live_position() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;

        let now = chrono::Utc::now();
        let pass = Pass {
            aos: now,
            los: now + chrono::Duration::minutes(6),
            peak: now + chrono::Duration::minutes(3),
            peak_elevation_deg: 80.0,
            aos_azimuth_deg: 90.0,
            los_azimuth_deg: 180.0,
            visible: true,
        };
        let rect = Rect::new(0, 0, 40, 20);
        let (x, y) = disc_bounds(rect);
        let grid = Grid { inner: rect, x, y };
        let render = |live: Option<LookAngles>| {
            let mut buf = Buffer::empty(rect);
            Canvas::default()
                .marker(Marker::Braille)
                .x_bounds(x)
                .y_bounds(y)
                .paint(|ctx| paint(ctx, &grid, &pass, &[], None, live))
                .render(rect, &mut buf);
            buf
        };
        let has_marker = |b: &Buffer| b.content.iter().any(|c| c.symbol() == "◆");

        assert!(!has_marker(&render(None)), "no marker when the pass is not under way");
        let live = LookAngles { azimuth_deg: 135.0, elevation_deg: 40.0, range_km: 600.0 };
        assert!(has_marker(&render(Some(live))), "and one when it is");
    }

    #[test]
    fn the_footer_names_the_date_on_its_widest_tier_only() {
        let (_, _, pass) = iss_pass_over_munich();
        let tiers = plot_footer(&pass);
        assert_eq!(tiers[0], format!("{}  ·  {}", local_date(pass.aos), tiers[1]));
        // The rest of the ladder is exactly what it was before the date
        // existed — the date is added, nothing below it changes.
        assert!(!tiers[1].contains(&local_date(pass.aos)), "the date belongs on tier 0 only");
    }

    /// The invariant `fitting_hint` relies on: each tier says strictly less
    /// than the one above it, so trimming to a narrower one never widens the
    /// footer back out.
    #[test]
    fn the_footer_tiers_get_strictly_shorter() {
        let (_, _, pass) = iss_pass_over_munich();
        let tiers = plot_footer(&pass);
        for pair in tiers.windows(2) {
            assert!(
                pair[1].chars().count() < pair[0].chars().count(),
                "{:?} did not shrink from {:?}",
                pair[1],
                pair[0],
            );
        }
    }
}
