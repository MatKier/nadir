//! An orthographic globe view of the map pane — Earth as a disc centred on
//! the sub-satellite point, the ground track curving over the limb, the
//! night side a real shadowed crescent. Toggled with `b`, an *alternative*
//! framing of the same data `ui::map` draws, not a replacement for it: the
//! flat map keeps its follow-window zoom, place labels, aurora oval, launch
//! pad marker and Sun/Moon toggle. None of that is reproduced here — the
//! globe is deliberately just the core picture (coastline, ground track,
//! footprint, night shading, ground station, satellite), always centred on
//! the action, which is exactly the one thing a fixed follow-window zoom on
//! the flat map can't offer: the satellite is dead centre by construction,
//! not by pressing `f`.
//!
//! Same `Canvas`/braille-layer discipline as `ui::map` and `ui::skyplot` —
//! see `map::paint_scene`'s note on why a shared cell has to be resolved by
//! layer order rather than blended, and why a `Marker::Block` wash and a
//! `Marker::Braille` foreground can share one `Canvas` at all.
//!
//! The projection is the standard spherical orthographic one: [`project`]
//! turns a `(lat, lon)` into disc coordinates and `None` on the far
//! hemisphere (the "back-face" test, `cos c < 0`); [`unproject`] is its
//! inverse, used only to sample the night wash — walking disc *pixels*
//! backward to the `(lat, lon)` under them, the mirror image of how
//! `map::night_wash` walks canvas cells forward through `Grid::x_of`/`y_of`
//! on the flat map's linear projection. Neither direction needs the flat
//! map's antimeridian splitting (`geo::split_at_antimeridian`): a great
//! circle on a sphere has no seam, only a horizon.

use chrono::{DateTime, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::canvas::{Canvas, Circle, Context, Line as CanvasLine, Points};
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::geo::GeoPoint;
use crate::orbit::solar::{SunGeometry, CIVIL_TWILIGHT_DEG};
use crate::orbit::{SatState, Tracker};
use crate::ui::canvas::Grid;
use crate::ui::coastline::COASTLINE;
use crate::ui::skyplot::disc_bounds;
use crate::ui::track::{track_scene, TrackScene};
use crate::ui::{is_focused, panel_block, Theme};

/// Degrees of longitude the auto-rotation drifts per second while no
/// satellite has an element set yet — slow enough to read as patient motion
/// rather than a spin, the same register `FRAME_LIVE`'s "clock face" pacing
/// keeps everywhere else the display isn't actively warping.
const AUTO_ROTATE_DEG_PER_SEC: f64 = 0.5;

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sat: Option<&(Tracker, SatState)>,
    now: DateTime<Utc>,
    // How far into the acquisition sweep the tracked element set is — `1.0`
    // outside a sweep. See `ui::track` and `ui::ACQUIRE_REVEAL`.
    reveal: f32,
) {
    let block = panel_block(Panel::Map, "GLOBE", is_focused(app, Panel::Map) || app.map_fullscreen);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Centred on the sub-satellite point when there is one — which also
    // means the satellite itself always sits exactly at the disc's own
    // centre, `(0, 0)`, without a projection of its own (see `paint_scene`).
    // Otherwise a slow drift keyed off real wall time, so a fresh launch
    // with no element set yet still shows something moving rather than a
    // frozen disc.
    let centre = match sat {
        Some((_, s)) => GeoPoint::new(s.sub_point.lat_deg, s.sub_point.lon_deg, 0.0),
        None => {
            let lon = (app.uptime().as_secs_f64() * AUTO_ROTATE_DEG_PER_SEC).rem_euclid(360.0);
            GeoPoint::new(0.0, lon, 0.0)
        }
    };

    let station = app.config.ground_station();
    // Same derivation as the flat map's — see `map::draw`'s note — so the
    // two views can never show a different trail length or reveal progress
    // for the same clock state.
    let track = sat.map(|(tr, s)| track_scene(tr, s, now, app.clock.state(), reveal));

    let scene = Scene { has_sat: sat.is_some(), station, track };

    let (x_bounds, y_bounds) = disc_bounds(inner);
    let grid = Grid { inner, x: x_bounds, y: y_bounds };
    let wash = night_wash(&grid, &centre, now);
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds(x_bounds)
        .y_bounds(y_bounds)
        .paint(move |ctx| paint_scene(ctx, &grid, &centre, &scene, &wash));
    frame.render_widget(canvas, inner);
}

/// Everything [`draw`] gathers once so [`paint_scene`] takes no ground
/// station or tracker of its own — the same role `map::Scene` plays for the
/// flat map, trimmed to what the globe actually draws.
struct Scene {
    has_sat: bool,
    station: Option<GeoPoint>,
    /// See `map::Scene::track` — the same shared derivation, `None` exactly
    /// when `has_sat` is false.
    track: Option<TrackScene>,
}

/// The night wash's sampled cells, bucketed the same way `map::Wash` is —
/// see that type's note — minus the aurora tiers, which are out of scope for
/// the globe (see this module's own doc comment).
#[derive(Default)]
struct Wash {
    night: Vec<(f64, f64)>,
    twilight: Vec<(f64, f64)>,
}

/// Orthographic projection of `target` onto the unit disc centred on
/// `centre`. `None` on the far hemisphere (`cos c < 0`, the standard
/// back-face test) — the caller drops the point rather than draw it, which
/// is what turns a flat scatter of coastline points into a globe with a
/// limb instead of a point cloud smeared across the whole disc.
fn project(centre: &GeoPoint, target: &GeoPoint) -> Option<(f64, f64)> {
    let lat0 = centre.lat_deg.to_radians();
    let lat = target.lat_deg.to_radians();
    let dlon = (target.lon_deg - centre.lon_deg).to_radians();

    let cos_c = lat0.sin() * lat.sin() + lat0.cos() * lat.cos() * dlon.cos();
    if cos_c < 0.0 {
        return None;
    }
    let x = lat.cos() * dlon.sin();
    let y = lat0.cos() * lat.sin() - lat0.sin() * lat.cos() * dlon.cos();
    Some((x, y))
}

/// The inverse of [`project`]: the `(lat, lon)` a disc point `(x, y)` shows,
/// or `None` when it falls outside the unit circle — off the globe
/// altogether, the "space" around the disc that a rectangular canvas always
/// has some of. Used only to sample the night wash; every foreground feature
/// goes through `project` in the forward direction instead; see this
/// module's doc comment.
fn unproject(centre: &GeoPoint, x: f64, y: f64) -> Option<GeoPoint> {
    let rho = (x * x + y * y).sqrt();
    if rho > 1.0 {
        return None;
    }
    let lat0 = centre.lat_deg.to_radians();
    let lon0 = centre.lon_deg.to_radians();
    if rho < 1e-12 {
        return Some(GeoPoint::new(centre.lat_deg, centre.lon_deg, 0.0));
    }
    let c = rho.asin();
    let (sin_c, cos_c) = c.sin_cos();
    let lat = (cos_c * lat0.sin() + (y * sin_c * lat0.cos()) / rho).asin();
    let lon = lon0 + (x * sin_c).atan2(rho * lat0.cos() * cos_c - y * lat0.sin() * sin_c);
    Some(GeoPoint::new(lat.to_degrees(), lon.to_degrees(), 0.0))
}

/// Night and civil-twilight cells of the visible hemisphere, one sample per
/// canvas cell — the globe's analogue of `map::night_wash`, sampling by
/// walking disc pixels backward through [`unproject`] rather than forward
/// through a linear lon/lat mapping, since an orthographic disc has no such
/// mapping to walk forward through. A cell whose centre unprojects to
/// nothing (off the globe, `rho > 1`) contributes no sample — the
/// surrounding "space" is left to the terminal's own background, the same
/// way the disc's corners are on `ui::skyplot`.
fn night_wash(grid: &Grid, centre: &GeoPoint, now: DateTime<Utc>) -> Wash {
    let sun = SunGeometry::at(now);
    let mut wash = Wash::default();
    for row in 0..grid.rows() {
        let y = grid.y_of(row);
        for col in 0..grid.cols() {
            let x = grid.x_of(col);
            let Some(p) = unproject(centre, x, y) else { continue };
            let elevation = sun.elevation_deg(p.lat_deg, p.lon_deg);
            if elevation <= CIVIL_TWILIGHT_DEG {
                wash.night.push((x, y));
            } else if elevation <= 0.0 {
                wash.twilight.push((x, y));
            }
        }
    }
    wash
}

fn paint_scene(ctx: &mut Context<'_>, grid: &Grid, centre: &GeoPoint, scene: &Scene, wash: &Wash) {
    // The night/twilight wash, on the coarser `Marker::Block` grid — see
    // `map::paint_scene`'s note on why the wash needs `Block` rather than
    // `Braille`: a `Braille` cell can only ever set one foreground colour, so
    // it can't paint a cell *background* the way a solid `Block` fill can.
    ctx.marker(Marker::Block);
    ctx.draw(&Points { coords: &wash.twilight, color: Theme::TWILIGHT });
    ctx.draw(&Points { coords: &wash.night, color: Theme::NIGHT });
    ctx.marker(Marker::Braille);
    ctx.layer();

    // The limb: a plain outline circle, dim, so the edge of the Earth reads
    // even over open ocean where no coastline point falls near it.
    ctx.draw(&Circle { x: 0.0, y: 0.0, radius: 1.0, color: Theme::PLACE });
    ctx.layer();

    let coast: Vec<(f64, f64)> = COASTLINE
        .iter()
        .filter_map(|&(lon, lat)| project(centre, &GeoPoint::new(lat, lon, 0.0)))
        .collect();
    if !coast.is_empty() {
        ctx.draw(&Points { coords: &coast, color: Theme::COAST });
        ctx.layer();
    }

    if let Some(track) = &scene.track {
        if draw_globe_polyline(ctx, centre, &track.footprint, Theme::FOOTPRINT) {
            ctx.layer();
        }
        // The globe has no fade to spend on a comet tail (see `map::
        // paint_scene`'s gradient version), so the two windows get flat
        // colours instead — but which physical window is "trailing" (dim)
        // versus "leading" (bright, the direction of travel) still swaps in
        // reverse, the same as the flat map, so the two views agree on what
        // bright and dim mean even though only one of them can fade.
        let (trailing, leading) =
            if track.reversed { (&track.future, &track.past) } else { (&track.past, &track.future) };
        if draw_globe_polyline(ctx, centre, trailing, Theme::TRACK_PAST) {
            ctx.layer();
        }
        if draw_globe_polyline(ctx, centre, leading, Theme::TRACK_FUTURE) {
            ctx.layer();
        }
    }

    if let Some(station) = &scene.station {
        if let Some((x, y)) = project(centre, station) {
            grid.print_dot(ctx, x, y, Span::styled("▲", Style::new().fg(Theme::STATION).bold()));
        }
    }

    if scene.has_sat {
        // Always exactly the disc centre: `centre` *is* the sub-satellite
        // point whenever a satellite is tracked (see `draw`), so this needs
        // no projection of its own.
        ctx.print(0.0, 0.0, Span::styled("◆", Style::new().fg(Theme::SAT).bold()));
    } else {
        let col = (f64::from(grid.cols()) * 0.30) as u16;
        ctx.print(
            grid.x_print_of(col),
            grid.y_print_of(grid.row_of(0.0)),
            Span::styled("acquiring element set…", Style::new().fg(Theme::LABEL)),
        );
    }
}

/// Project and draw each segment of a `geo::split_at_antimeridian`-shaped
/// polyline — the ground track and footprint both come in this shape, split
/// for the flat map's antimeridian seam, which the globe has no use for but
/// tolerates for free: each sub-`Vec` is drawn as its own contiguous run
/// regardless of why it was split. A line whose endpoints straddle the limb
/// (one visible, one not) is simply skipped rather than drawn to a
/// projection that doesn't exist — the correct outline is "the track
/// vanishes over the horizon", not a chord cutting across the disc.
///
/// Returns whether anything was drawn, so the caller only spends a
/// `ctx.layer()` on a feature that actually put something on the canvas.
fn draw_globe_polyline(
    ctx: &mut Context<'_>,
    centre: &GeoPoint,
    segments: &[Vec<GeoPoint>],
    color: Color,
) -> bool {
    let mut drew = false;
    for seg in segments {
        for w in seg.windows(2) {
            let (Some((x1, y1)), Some((x2, y2))) = (project(centre, &w[0]), project(centre, &w[1]))
            else {
                continue;
            };
            ctx.draw(&CanvasLine { x1, y1, x2, y2, color });
            drew = true;
        }
    }
    drew
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_centre_of_projection_lands_on_the_disc_centre() {
        let centre = GeoPoint::new(48.0, 11.0, 0.0);
        let (x, y) = project(&centre, &centre).expect("a point projects onto itself");
        assert!(x.abs() < 1e-9 && y.abs() < 1e-9, "expected (0, 0), got ({x}, {y})");
    }

    #[test]
    fn a_point_on_the_far_hemisphere_is_culled_and_its_antipode_is_not() {
        let centre = GeoPoint::new(0.0, 0.0, 0.0);
        let antipode = GeoPoint::new(0.0, 180.0, 0.0);
        assert!(project(&centre, &antipode).is_none(), "the antipode must be culled");
        assert!(project(&centre, &centre).is_some(), "the centre itself must not be");
    }

    #[test]
    fn a_point_exactly_ninety_degrees_away_lands_on_the_rim() {
        let centre = GeoPoint::new(0.0, 0.0, 0.0);
        let quarter_turn = GeoPoint::new(0.0, 90.0, 0.0);
        let (x, y) = project(&centre, &quarter_turn).expect("still on the near hemisphere");
        let r = (x * x + y * y).sqrt();
        assert!((r - 1.0).abs() < 1e-9, "expected radius 1, got {r}");
    }

    #[test]
    fn projection_never_leaves_the_unit_disc() {
        let centre = GeoPoint::new(23.0, -47.0, 0.0);
        for lat in (-90..=90).step_by(10) {
            for lon in (-180..180).step_by(10) {
                if let Some((x, y)) = project(&centre, &GeoPoint::new(lat as f64, lon as f64, 0.0)) {
                    let r = (x * x + y * y).sqrt();
                    assert!(r <= 1.0 + 1e-6, "lat {lat} lon {lon}: radius {r} > 1");
                }
            }
        }
    }

    #[test]
    fn unproject_recovers_the_centre_at_the_disc_origin() {
        let centre = GeoPoint::new(48.0, 11.0, 0.0);
        let p = unproject(&centre, 0.0, 0.0).expect("the origin is always on the globe");
        assert!((p.lat_deg - centre.lat_deg).abs() < 1e-9);
        assert!((p.lon_deg - centre.lon_deg).abs() < 1e-9);
    }

    #[test]
    fn unproject_is_none_outside_the_unit_disc() {
        let centre = GeoPoint::new(0.0, 0.0, 0.0);
        assert!(unproject(&centre, 1.5, 0.0).is_none());
        assert!(unproject(&centre, 0.8, 0.8).is_none()); // radius > 1
    }

    /// `project` then `unproject` should round-trip any point on the near
    /// hemisphere back to itself — the two are meant to be exact inverses of
    /// each other, not merely "close enough to look right".
    #[test]
    fn project_then_unproject_round_trips_a_near_hemisphere_point() {
        let centre = GeoPoint::new(-15.0, 60.0, 0.0);
        for lat in [-80.0, -30.0, 0.0, 45.0, 89.0] {
            for lon in [-170.0, -20.0, 0.0, 33.0, 179.0] {
                let target = GeoPoint::new(lat, lon, 0.0);
                let Some((x, y)) = project(&centre, &target) else { continue };
                let back = unproject(&centre, x, y).expect("what was just projected must unproject");
                // Near the rim a tiny numeric wobble in x/y can swing the
                // recovered longitude by more than it swings latitude — the
                // usual pole-like behaviour of any azimuthal projection —
                // so the tolerance is generous there and tight everywhere
                // else, rather than uniformly loose.
                let near_rim = (x * x + y * y).sqrt() > 0.999;
                let tol = if near_rim { 1.0 } else { 1e-6 };
                assert!(
                    (back.lat_deg - lat).abs() < tol,
                    "lat {lat} lon {lon}: recovered lat {}",
                    back.lat_deg
                );
            }
        }
    }

    #[test]
    fn no_coastline_point_ever_projects_outside_the_unit_disc() {
        let centre = GeoPoint::new(10.0, 20.0, 0.0);
        for &(lon, lat) in crate::ui::coastline::COASTLINE.iter() {
            if let Some((x, y)) = project(&centre, &GeoPoint::new(lat, lon, 0.0)) {
                let r = (x * x + y * y).sqrt();
                assert!(r <= 1.0 + 1e-6, "({lon}, {lat}) projected to radius {r}");
            }
        }
    }

    #[test]
    fn draw_globe_polyline_skips_a_segment_that_crosses_the_limb() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;

        let centre = GeoPoint::new(0.0, 0.0, 0.0);
        // One endpoint on the near side, one on the far side — must not
        // draw a chord straight across the disc for this pair.
        let segments = vec![vec![GeoPoint::new(0.0, 0.0, 0.0), GeoPoint::new(0.0, 170.0, 0.0)]];

        let rect = Rect::new(0, 0, 40, 20);
        let (x, y) = disc_bounds(rect);
        let mut buf = Buffer::empty(rect);
        Canvas::default()
            .marker(Marker::Braille)
            .x_bounds(x)
            .y_bounds(y)
            .paint(|ctx| {
                let drew = draw_globe_polyline(ctx, &centre, &segments, Theme::TRACK_FUTURE);
                assert!(!drew, "a limb-crossing pair should draw nothing");
            })
            .render(rect, &mut buf);
    }

    #[test]
    fn draw_globe_polyline_draws_a_fully_visible_segment() {
        let centre = GeoPoint::new(0.0, 0.0, 0.0);
        let segments = vec![vec![GeoPoint::new(0.0, -10.0, 0.0), GeoPoint::new(0.0, 10.0, 0.0)]];
        let rect = Rect::new(0, 0, 40, 20);
        let (x, y) = disc_bounds(rect);
        let canvas = Canvas::default().marker(Marker::Braille).x_bounds(x).y_bounds(y).paint(|ctx| {
            let drew = draw_globe_polyline(ctx, &centre, &segments, Theme::TRACK_FUTURE);
            assert!(drew, "a fully visible segment should draw");
        });
        use ratatui::widgets::Widget;
        let mut buf = ratatui::buffer::Buffer::empty(rect);
        canvas.render(rect, &mut buf);
    }

    #[test]
    fn night_wash_only_samples_the_visible_hemisphere() {
        use chrono::TimeZone;
        let centre = GeoPoint::new(0.0, 0.0, 0.0);
        let t = Utc.with_ymd_and_hms(2026, 9, 4, 12, 0, 0).unwrap();
        let rect = Rect::new(0, 0, 60, 30);
        let (x, y) = disc_bounds(rect);
        let grid = Grid { inner: rect, x, y };
        let wash = night_wash(&grid, &centre, t);
        for &(x, y) in wash.night.iter().chain(&wash.twilight) {
            assert!((x * x + y * y).sqrt() <= 1.0 + 1e-6, "sample ({x}, {y}) off the globe");
        }
        // Noon at (0°, 0°) puts the whole visible hemisphere in daylight —
        // vacuous unless it's also demonstrably possible to get *some*
        // night samples with a different instant.
        let midnight_side = Utc.with_ymd_and_hms(2026, 9, 4, 0, 0, 0).unwrap();
        let night_wash_result = night_wash(&grid, &centre, midnight_side);
        assert!(
            !night_wash_result.night.is_empty(),
            "expected some night cells at the opposite time of day"
        );
    }
}
