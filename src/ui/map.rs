//! The world-map panel: coastlines, night shading, ground track, footprint,
//! ground station and the satellite itself.

use chrono::{DateTime, Duration, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Circle, Context, Line as CanvasLine, Map, MapResolution, Points};
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::geo::GeoPoint;
use crate::orbit::solar::{is_sunlit, terminator_polyline};
use crate::orbit::{SatState, Tracker};
use crate::ui::panels::truncate;
use crate::ui::{is_focused, panel_block, PadMarker, Theme};

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sat: Option<&(Tracker, SatState)>,
    now: DateTime<Utc>,
    pad: Option<PadMarker>,
) {
    let block = panel_block(Panel::Map, "MAP", is_focused(app, Panel::Map) || app.map_fullscreen);
    // The block's inner cell grid, measured before the block is moved into
    // the Canvas — needed to budget marker labels against the same grid
    // ratatui itself maps coordinates onto (see `Canvas::render`).
    let inner = block.inner(area);

    // View bounds: whole world, or a 2x zoom centred on the satellite.
    let (x_bounds, y_bounds) = match (app.follow, sat) {
        (true, Some((_, s))) => {
            let cx = s.sub_point.lon_deg.clamp(-90.0, 90.0);
            let cy = s.sub_point.lat_deg.clamp(-45.0, 45.0);
            ([cx - 90.0, cx + 90.0], [cy - 45.0, cy + 45.0])
        }
        _ => ([-180.0, 180.0], [-90.0, 90.0]),
    };
    let grid = Grid { inner, x: x_bounds, y: y_bounds };

    let station = app.config.ground_station();
    let track_future = sat.map(|(tr, _)| {
        tr.ground_track(now, Duration::zero(), Duration::minutes(65), Duration::seconds(20))
    });
    let track_past = sat.map(|(tr, _)| {
        tr.ground_track(now, Duration::minutes(35), Duration::zero(), Duration::seconds(20))
    });

    // Night-side stipple, sampled on a coarse mesh (cheap, and reads clearly).
    let mut night = Vec::new();
    let mut lon = x_bounds[0];
    while lon <= x_bounds[1] {
        let mut lat = y_bounds[0];
        while lat <= y_bounds[1] {
            let p = GeoPoint::new(lat.clamp(-90.0, 90.0), lon, 0.0);
            if !is_sunlit(&p, now) {
                night.push((lon, lat));
            }
            lat += 4.0;
        }
        lon += 4.0;
    }

    let terminator: Vec<(f64, f64)> = terminator_polyline(now, 240)
        .into_iter()
        .map(|p| (p.lon_deg, p.lat_deg))
        .collect();

    let canvas = Canvas::default()
        .block(block)
        .marker(Marker::Braille)
        .x_bounds(x_bounds)
        .y_bounds(y_bounds)
        .paint(move |ctx| {
            ctx.draw(&Map {
                resolution: MapResolution::High,
                color: Theme::COAST,
            });

            ctx.draw(&Points {
                coords: &night,
                color: Theme::NIGHT,
            });
            ctx.layer();

            ctx.draw(&Points {
                coords: &terminator,
                color: Theme::CAUTION,
            });

            if let Some(segments) = &track_past {
                draw_polyline(ctx, segments, Theme::TRACK_PAST);
            }
            if let Some(segments) = &track_future {
                draw_polyline(ctx, segments, Theme::TRACK_FUTURE);
            }
            ctx.layer();

            if let Some((_, s)) = sat {
                let radius_deg = (s.footprint_km / 111.32).min(160.0);
                ctx.draw(&Circle {
                    x: s.sub_point.lon_deg,
                    y: s.sub_point.lat_deg,
                    radius: radius_deg,
                    color: Theme::TRACK_FUTURE,
                });
            }

            if let Some(g) = station {
                ctx.print(
                    g.lon_deg,
                    g.lat_deg,
                    Span::styled("▲", Style::new().fg(Theme::STATION).bold()),
                );
            }

            // The highlighted launch's pad, drawn before the satellite so
            // the satellite marker wins if the two ever coincide. The label
            // leads with the provider — a short, stable field that survives
            // truncation on a narrow map, unlike the often much longer
            // vehicle/mission name — except when the feed didn't know it.
            if let Some(p) = &pad {
                let label = if p.provider.is_empty() || p.provider == "—" {
                    p.vehicle.to_string()
                } else {
                    format!("{} · {}", p.provider, p.vehicle)
                };
                let detail = format!("{} · {}", p.site, fmt_coords(p.lat, p.lon));
                print_marker(
                    ctx,
                    &grid,
                    p.lon,
                    p.lat,
                    &MarkerLabel { glyph: "◉", color: Theme::PAD, name: &label, detail: Some(&detail) },
                );
            }

            match sat {
                Some((tr, s)) => {
                    print_marker(
                        ctx,
                        &grid,
                        s.sub_point.lon_deg,
                        s.sub_point.lat_deg,
                        &MarkerLabel { glyph: "◆", color: Theme::SAT, name: tr.name(), detail: None },
                    );
                }
                None => {
                    ctx.print(
                        x_bounds[0] + (x_bounds[1] - x_bounds[0]) * 0.30,
                        0.0,
                        Span::styled(
                            "acquiring element set…",
                            Style::new().fg(Theme::LABEL),
                        ),
                    );
                }
            }
        });

    frame.render_widget(canvas, area);
}

fn draw_polyline(ctx: &mut Context<'_>, segments: &[Vec<GeoPoint>], color: Color) {
    for seg in segments {
        for w in seg.windows(2) {
            ctx.draw(&CanvasLine {
                x1: w[0].lon_deg,
                y1: w[0].lat_deg,
                x2: w[1].lon_deg,
                y2: w[1].lat_deg,
                color,
            });
        }
    }
}

/// The canvas's cell grid: its inner area plus the coordinate bounds mapped
/// onto it. Bundles what [`col_of`](Grid::col_of)/[`x_of`](Grid::x_of) and the
/// marker-label layout below all need, so they don't each thread `inner`,
/// `x_bounds` and `y_bounds` through by hand.
#[derive(Clone, Copy)]
struct Grid {
    inner: Rect,
    x: [f64; 2],
    y: [f64; 2],
}

impl Grid {
    fn cols(&self) -> u16 {
        self.inner.width
    }

    fn rows(&self) -> u16 {
        self.inner.height
    }

    /// The cell column an x-coordinate lands on, mirroring the mapping
    /// `Canvas::render` itself uses for labels — so a text budget computed
    /// from it matches what's actually drawn. Rounds rather than truncating
    /// (as ratatui's own cast does) so it round-trips cleanly with
    /// [`Grid::x_of`]; ratatui draws the label itself from the raw x/y, so
    /// this only has to be accurate enough for our own budget math, not
    /// bit-identical to its truncation. The tiny epsilon before rounding
    /// makes a coordinate sitting exactly on a half-column boundary resolve
    /// the same way every time, rather than being at the mercy of which way
    /// rounding error happens to fall that frame — exactly the case a
    /// satellite held dead-centre in follow mode hits.
    fn col_of(&self, x: f64) -> u16 {
        let cols = self.cols();
        let span = (self.x[1] - self.x[0]).abs();
        if cols < 2 || span <= 0.0 {
            return 0;
        }
        (((x - self.x[0]) * (cols - 1) as f64 / span + 1e-9).round() as u16).min(cols - 1)
    }

    /// The x-coordinate at the left edge of cell column `col` — the inverse
    /// of [`Grid::col_of`].
    fn x_of(&self, col: u16) -> f64 {
        let cols = self.cols();
        if cols < 2 {
            return self.x[0];
        }
        let span = (self.x[1] - self.x[0]).abs();
        self.x[0] + col as f64 * span / (cols - 1) as f64
    }

    /// Degrees of latitude spanned by one cell row.
    fn deg_per_row(&self) -> f64 {
        (self.y[1] - self.y[0]) / (self.rows() - 1) as f64
    }
}

/// A one-glyph marker plus a name label beside it, and an optional dimmer
/// detail line under that.
struct MarkerLabel<'a> {
    glyph: &'a str,
    color: Color,
    name: &'a str,
    detail: Option<&'a str>,
}

/// Draw `marker` at `(lon, lat)`. The label goes to whichever side of the
/// marker has more room in the canvas's cell grid, so it's a single
/// `ctx.print` call per line — nothing gets drawn over the glyph — and it
/// isn't clipped by an edge the way a label fixed to one side would be.
fn print_marker(ctx: &mut Context<'_>, grid: &Grid, lon: f64, lat: f64, marker: &MarkerLabel<'_>) {
    let MarkerLabel { glyph, color, name, detail } = *marker;
    let cols = grid.cols();
    let glyph_span = || Span::styled(glyph.to_string(), Style::new().fg(color).bold());

    if cols < 2 || name.is_empty() {
        ctx.print(lon, lat, glyph_span());
        return;
    }

    let marker_col = grid.col_of(lon);
    let room_right = cols.saturating_sub(marker_col + 1);
    let room_left = marker_col;
    let right_side = label_on_right(room_left, room_right, name.chars().count());

    let budget = (if right_side { room_right } else { room_left }).saturating_sub(1) as usize;
    let shown = truncate(name, budget);

    // Column the label text itself starts at — used again below to line the
    // detail row up under it rather than under the glyph.
    let text_col = if right_side {
        ctx.print(
            lon,
            lat,
            Line::from(vec![glyph_span(), Span::styled(format!(" {shown}"), Style::new().fg(color))]),
        );
        marker_col + 1
    } else {
        let start_col = marker_col.saturating_sub(1 + shown.chars().count() as u16);
        let x = grid.x_of(start_col);
        ctx.print(
            x,
            lat,
            Line::from(vec![Span::styled(format!("{shown} "), Style::new().fg(color)), glyph_span()]),
        );
        start_col
    };

    if let Some(detail) = detail {
        print_detail_line(ctx, grid, text_col, lat, detail);
    }
}

/// The dim second line of a marker's label: one cell row below it, or above
/// when a row below would fall outside the visible bounds.
fn print_detail_line(ctx: &mut Context<'_>, grid: &Grid, text_col: u16, lat: f64, text: &str) {
    if grid.rows() < 2 {
        return;
    }
    let deg_per_row = grid.deg_per_row();
    let mut y = lat - deg_per_row;
    if y < grid.y[0] {
        y = lat + deg_per_row;
    }
    if y > grid.y[1] {
        return;
    }

    let budget = grid.cols().saturating_sub(text_col) as usize;
    let x = grid.x_of(text_col);
    ctx.print(x, y, Span::styled(truncate(text, budget), Style::new().fg(Theme::LABEL)));
}

/// Which side of the marker its label goes on. Defaults to the right and
/// only flips left when the left offers materially more room: in follow mode
/// the satellite sits dead-centre on the map, where the two sides differ by
/// at most a column — deciding on `room_right >= room_left` there let plain
/// floating-point noise in [`Grid::col_of`] flip the side every frame.
fn label_on_right(room_left: u16, room_right: u16, name_len: usize) -> bool {
    const FLIP_MARGIN: u16 = 8;
    room_right >= name_len as u16 + 1 || room_left < room_right + FLIP_MARGIN
}

/// `40.96°N 100.30°E` — hemisphere letters rather than signed degrees, to
/// read naturally next to a pad's site name.
fn fmt_coords(lat: f64, lon: f64) -> String {
    let ns = if lat >= 0.0 { 'N' } else { 'S' };
    let ew = if lon >= 0.0 { 'E' } else { 'W' };
    format!("{:.2}°{ns} {:.2}°{ew}", lat.abs(), lon.abs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Grid` spanning `[-180, 180]` with `cols` columns, for the `col_of`/
    /// `x_of` tests below — their row count and position are irrelevant.
    fn grid(cols: u16) -> Grid {
        Grid { inner: Rect::new(0, 0, cols, 1), x: [-180.0, 180.0], y: [-90.0, 90.0] }
    }

    #[test]
    fn col_of_and_x_of_round_trip_at_bounds_and_centre() {
        let g = grid(100);
        for col in [0u16, 1, 50, 98, 99] {
            let x = g.x_of(col);
            assert_eq!(g.col_of(x), col, "col {col} -> x {x} -> col");
        }
    }

    #[test]
    fn col_of_clamps_to_last_column() {
        let g = grid(100);
        assert_eq!(g.col_of(180.0), 99);
        assert_eq!(g.col_of(-180.0), 0);
    }

    #[test]
    fn col_of_degenerate_grid_is_zero() {
        assert_eq!(grid(1).col_of(5.0), 0);
        assert_eq!(grid(0).col_of(5.0), 0);
    }

    #[test]
    fn label_on_right_stays_right_when_dead_centre() {
        // A marker held exactly centred (follow mode) with a name too long
        // to fit on either side outright, so the margin logic actually
        // decides: the two sides differ by at most one column, and it must
        // always resolve to the same side regardless of which way that one
        // column falls, or the label flips every frame on rounding noise.
        assert!(label_on_right(42, 43, 60));
        assert!(label_on_right(43, 42, 60));
        assert!(label_on_right(43, 43, 60));
    }

    #[test]
    fn label_on_right_flips_left_only_with_a_real_margin() {
        // Near the right edge, with far more room on the left: must flip.
        assert!(!label_on_right(70, 5, 60));
        // A small edge nudge alone (well under the margin) must not flip.
        assert!(label_on_right(50, 43, 60));
    }

    #[test]
    fn label_on_right_stays_right_when_the_name_fits_there() {
        // Near an edge, but the name is short enough to fit on the right
        // anyway — no reason to flip just because the left has more room.
        assert!(label_on_right(70, 12, 10));
    }

    #[test]
    fn fmt_coords_picks_hemisphere_letters() {
        assert_eq!(fmt_coords(40.958, 100.298), "40.96°N 100.30°E");
        assert_eq!(fmt_coords(-34.632, -120.611), "34.63°S 120.61°W");
        assert_eq!(fmt_coords(0.0, 0.0), "0.00°N 0.00°E");
    }
}
