//! The world-map panel: coastlines, night shading, ground track, footprint,
//! ground station and the satellite itself.

use chrono::{DateTime, Duration, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::{Line, Span};
use ratatui::widgets::canvas::{Canvas, Context, Line as CanvasLine, Map, MapResolution, Points};
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::geo::{footprint_ring, GeoPoint};
use crate::orbit::solar::{terminator_polyline, SunGeometry, CIVIL_TWILIGHT_DEG};
use crate::orbit::{SatState, Tracker};
use crate::ui::panels::truncate;
use crate::ui::places::{Place, PLACES};
use crate::ui::{is_focused, panel_block, PadMarker, Theme};

/// Longitude half-spans of the follow window, widest first — each level halves
/// the window, so the scale doubles: ×2, ×4, ×8, ×16 against the whole world.
/// Latitude gets half the longitude half-span, which is the 2:1 aspect the
/// whole-world view (360°×180°) already has, so zooming changes the scale of
/// the projection and never its shape.
pub(crate) const ZOOM_HALF_SPANS: [f64; 4] = [90.0, 45.0, 22.5, 11.25];
pub(crate) const MAX_ZOOM: usize = ZOOM_HALF_SPANS.len() - 1;

/// Most place labels drawn on one pane — a backstop, not the main control.
/// The collision test does most of the thinning; this just stops a wide
/// fullscreen map, where many labels clear each other, from turning into a
/// solid sheet of names.
const MAX_PLACE_LABELS: usize = 24;

/// The map's coordinate bounds: the whole world when `centre` is `None`, or a
/// follow window of `ZOOM_HALF_SPANS[zoom]` longitude around `centre`.
///
/// The centre longitude is *not* clamped — the window is free to run past
/// ±180°, and the part that overhangs the map is drawn by a second canvas pane
/// wrapped onto the opposite edge (see `panes`). Latitude has no such wrap, so
/// the centre is clamped to keep the window on the map — but only by its own
/// half-span, so a tight zoom can still push the window flush against a pole,
/// which is exactly where a tight zoom is most wanted.
fn view_bounds(centre: Option<&GeoPoint>, zoom: usize) -> ([f64; 2], [f64; 2]) {
    let Some(c) = centre else {
        return ([-180.0, 180.0], [-90.0, 90.0]);
    };
    let hx = ZOOM_HALF_SPANS[zoom.min(MAX_ZOOM)];
    let hy = hx / 2.0;
    let cx = c.lon_deg;
    let cy = c.lat_deg.clamp(-(90.0 - hy), 90.0 - hy);
    ([cx - hx, cx + hx], [cy - hy, cy + hy])
}

/// The map panel's title: plain `MAP` with no follow centre, or `MAP ×N` while
/// following, where `N` is the scale against the whole-world view. Derived from
/// the same `centre` option as [`view_bounds`], so the two can never disagree.
fn zoom_title(centre: Option<&GeoPoint>, zoom: usize) -> String {
    match centre {
        None => "MAP".to_string(),
        Some(_) => {
            let scale = 2u32.pow(zoom.min(MAX_ZOOM) as u32 + 1);
            format!("MAP ×{scale}")
        }
    }
}

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sat: Option<&(Tracker, SatState)>,
    now: DateTime<Utc>,
    pad: Option<PadMarker>,
) {
    // The follow-window centre — `Some` only once we're following *and* an
    // element set has arrived, so the zoom indicator and the bounds can never
    // claim a magnification the map isn't actually showing.
    let centre = sat.filter(|_| app.follow).map(|(_, s)| &s.sub_point);
    let title = zoom_title(centre, app.zoom);
    let block =
        panel_block(Panel::Map, &title, is_focused(app, Panel::Map) || app.map_fullscreen);
    // The block's inner cell grid, measured before the block is rendered —
    // needed both to place the pane canvases inside it and to budget marker
    // labels against the same grid ratatui maps coordinates onto (see
    // `Canvas::render`). The border is drawn on its own rather than through
    // `Canvas::block`, because the map now needs more than one canvas.
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (x_bounds, y_bounds) = view_bounds(centre, app.zoom);

    let station = app.config.ground_station();
    let track_future = sat.map(|(tr, _)| {
        tr.ground_track(now, Duration::zero(), Duration::minutes(65), Duration::seconds(20))
    });
    let track_past = sat.map(|(tr, _)| {
        tr.ground_track(now, Duration::minutes(35), Duration::zero(), Duration::seconds(20))
    });
    // The visibility footprint as a great-circle ring, split at the ±180°
    // meridian like the tracks are — a spherical cap, so it bulges in
    // longitude towards the poles rather than staying a projected circle, and
    // its far half wraps onto the opposite map edge instead of being clipped.
    let footprint = sat.map(|(_, s)| footprint_ring(&s.sub_point, s.footprint_km, 180));

    let terminator: Vec<(f64, f64)> = terminator_polyline(now, 240)
        .into_iter()
        .map(|p| (p.lon_deg, p.lat_deg))
        .collect();

    let scene =
        Scene { sat, pad, station, track_past, track_future, footprint, terminator, places: app.places };

    // One canvas per pane. A single `Canvas` has one linear longitude→column
    // mapping and so cannot show the coastline in two disjoint screen
    // regions; when the follow window crosses ±180° `panes` cuts `inner` on a
    // whole-column boundary into a near half and a wrapped half that tile it
    // exactly, each drawn from the same `[-180, 180)` geometry in its own
    // real-longitude bounds. Away from the dateline (and whenever follow is
    // off) it returns a single full-width pane and this is exactly the old
    // single-canvas render.
    for pane in panes(inner, x_bounds) {
        let grid = Grid { inner: pane.rect, x: pane.x, y: y_bounds };
        let (night, twilight) = night_wash(&grid, now);
        let canvas = Canvas::default()
            .marker(Marker::Braille)
            .x_bounds(pane.x)
            .y_bounds(y_bounds)
            .paint(|ctx| paint_scene(ctx, &grid, &scene, &night, &twilight));
        frame.render_widget(canvas, pane.rect);
    }
}

/// Everything the map draws, gathered once in [`draw`] so it can be handed to
/// one paint closure per [`Pane`] without recomputing the orbital pipeline for
/// each half of a split view.
struct Scene<'a> {
    sat: Option<&'a (Tracker, SatState)>,
    pad: Option<PadMarker<'a>>,
    station: Option<GeoPoint>,
    track_past: Option<Vec<Vec<GeoPoint>>>,
    track_future: Option<Vec<Vec<GeoPoint>>>,
    footprint: Option<Vec<Vec<GeoPoint>>>,
    terminator: Vec<(f64, f64)>,
    /// Whether the `p` layer of prominent-place labels is on.
    places: bool,
}

/// Paint one map pane: the night wash, coastline, terminator, tracks,
/// footprint and markers, in the same layer order the single canvas used
/// before the view could be split. `night`/`twilight` are this pane's own
/// wash cells; every other field is shared across panes via `scene`.
fn paint_scene(
    ctx: &mut Context<'_>,
    grid: &Grid,
    scene: &Scene<'_>,
    night: &[(f64, f64)],
    twilight: &[(f64, f64)],
) {
    // The night wash is a *background*, not a foreground overlay: it has to go
    // on a `Block` grid, whose cells carry a bg colour (ratatui's
    // `PatternGrid`, what `Braille` uses, only ever sets fg — see
    // `CharGrid::apply_color_to_bg` upstream). Painted first and switched away
    // from before the coastline, so the Braille coastline dots drawn next land
    // on top of it rather than being recoloured by it the way a same-layer
    // stipple would.
    ctx.marker(Marker::Block);
    ctx.draw(&Points { coords: twilight, color: Theme::TWILIGHT });
    ctx.draw(&Points { coords: night, color: Theme::NIGHT });

    ctx.marker(Marker::Braille);
    ctx.draw(&Map { resolution: MapResolution::High, color: Theme::COAST });

    ctx.draw(&Points { coords: &scene.terminator, color: Theme::CAUTION });

    // Footprint before the tracks, same layer: a GEO ring crosses the ground
    // track several times, and the last write to a cell wins, so drawing it
    // first lets the track stay continuous through every crossing rather than
    // being punched through by the ring.
    if let Some(segments) = &scene.footprint {
        draw_polyline(ctx, segments, Theme::FOOTPRINT);
    }
    if let Some(segments) = &scene.track_past {
        draw_polyline(ctx, segments, Theme::TRACK_PAST);
    }
    if let Some(segments) = &scene.track_future {
        draw_polyline(ctx, segments, Theme::TRACK_FUTURE);
    }
    ctx.layer();

    // The place layer sits under the three live markers — it's reference
    // scenery, not data — but is laid out *against* them (they're seeded
    // into its collision set) so a city name never ends up under the
    // satellite even though the satellite is drawn last.
    if scene.places {
        draw_places(ctx, grid, scene);
    }

    if let Some(g) = scene.station {
        if grid.contains(g.lon_deg) {
            ctx.print(
                g.lon_deg,
                g.lat_deg,
                Span::styled("▲", Style::new().fg(Theme::STATION).bold()),
            );
        }
    }

    // The highlighted launch's pad, drawn before the satellite so the
    // satellite marker wins if the two ever coincide. The label leads with
    // the provider — a short, stable field that survives truncation on a
    // narrow map, unlike the often much longer vehicle/mission name — except
    // when the feed didn't know it.
    if let Some(p) = &scene.pad {
        let (label, detail) = pad_label(p);
        print_marker(
            ctx,
            grid,
            p.lon,
            p.lat,
            &MarkerLabel { glyph: "◉", color: Theme::PAD, name: &label, detail: Some(&detail) },
        );
    }

    match scene.sat {
        Some((tr, s)) => {
            print_marker(
                ctx,
                grid,
                s.sub_point.lon_deg,
                s.sub_point.lat_deg,
                &MarkerLabel { glyph: "◆", color: Theme::SAT, name: tr.name(), detail: None },
            );
        }
        None => {
            // `sat` is `None` only when follow mode is off, so this is always
            // the single whole-world pane — one placeholder, anchored 30% in.
            ctx.print(
                grid.x[0] + (grid.x[1] - grid.x[0]) * 0.30,
                0.0,
                Span::styled("acquiring element set…", Style::new().fg(Theme::LABEL)),
            );
        }
    }
}

/// The pad marker's two label lines: `"provider · vehicle"` (or the vehicle
/// alone when the feed gave no provider) and `"site · coords"`. Pulled out
/// of [`paint_scene`] so [`draw_places`] can reserve the exact footprint the
/// marker will draw, rather than a guess at it.
fn pad_label(p: &PadMarker<'_>) -> (String, String) {
    let name = if p.provider.is_empty() || p.provider == "—" {
        p.vehicle.to_string()
    } else {
        format!("{} · {}", p.provider, p.vehicle)
    };
    (name, format!("{} · {}", p.site, fmt_coords(p.lat, p.lon)))
}

/// The prominent-places layer: dim `·` cities and `+` ground stations with
/// their names, `PLACES` walked in tier order and each label drawn only
/// where it clears every label already placed — the satellite, station and
/// pad seeded first, then each other. That collision test *is* the density
/// control: on a small whole-world map only a scattered few land, and more
/// fill in the wider the map gets or the tighter the follow window zooms,
/// with nothing keyed to a zoom level (see [`MAX_PLACE_LABELS`] for the one
/// backstop).
fn draw_places(ctx: &mut Context<'_>, grid: &Grid, scene: &Scene<'_>) {
    if grid.cols() < 2 {
        return;
    }

    // Seed the collision set with the three markers drawn on top of this
    // layer, so a place label never lands under one. They're laid out a
    // second time when actually drawn — three items, far cheaper than
    // reordering the marker block so this layer could see already-drawn
    // cells.
    let mut seed = Claimed::default();
    if let Some((tr, s)) = scene.sat {
        let m = MarkerLabel { glyph: "◆", color: Theme::SAT, name: tr.name(), detail: None };
        seed.claim(&marker_cells(grid, s.sub_point.lon_deg, s.sub_point.lat_deg, &m));
    }
    if let Some(g) = scene.station {
        let m = MarkerLabel { glyph: "▲", color: Theme::STATION, name: "", detail: None };
        seed.claim(&marker_cells(grid, g.lon_deg, g.lat_deg, &m));
    }
    if let Some(p) = &scene.pad {
        let (label, detail) = pad_label(p);
        let m = MarkerLabel { glyph: "◉", color: Theme::PAD, name: &label, detail: Some(&detail) };
        seed.claim(&marker_cells(grid, p.lon, p.lat, &m));
    }

    for place in selected_places(grid, seed) {
        let m = MarkerLabel {
            glyph: place.glyph(),
            color: Theme::PLACE,
            name: place.name,
            detail: None,
        };
        print_marker(ctx, grid, place.lon, place.lat, &m);
    }
}

/// Which places earn a label on `grid`, in draw order: `PLACES` walked in
/// tier order, each kept only when its label clears `seed` (the live
/// markers) and every place already kept, and no more than
/// [`MAX_PLACE_LABELS`] of them. Split from [`draw_places`] so the density
/// behaviour is testable without a canvas — a place skipped here is skipped
/// whole, dot included, since a bare unlabelled dot on a coastline map is
/// noise rather than a landmark.
fn selected_places(grid: &Grid, seed: Claimed) -> Vec<&'static Place> {
    let mut claimed = seed;
    let mut out = Vec::new();
    for place in PLACES {
        if out.len() >= MAX_PLACE_LABELS {
            break;
        }
        // Off this pane's window, or outside the visible latitude band.
        if !grid.contains(place.lon) || place.lat < grid.y[0] || place.lat > grid.y[1] {
            continue;
        }
        let m = MarkerLabel {
            glyph: place.glyph(),
            color: Theme::PLACE,
            name: place.name,
            detail: None,
        };
        let cells = marker_cells(grid, place.lon, place.lat, &m);
        if cells.is_empty() || !claimed.free(&cells) {
            continue;
        }
        claimed.claim(&cells);
        out.push(place);
    }
    out
}

/// One line of a label's footprint: a cell row and the inclusive column span
/// it covers. A label is a single row tall, so a full rectangle would be
/// overkill.
#[derive(Clone, Copy, Debug)]
struct Cells {
    row: u16,
    from: u16,
    to: u16,
}

impl Cells {
    /// Columns of clearance kept between two labels — enough that they read
    /// as separate even on adjacent rows.
    const PAD: u16 = 2;

    /// Whether two label lines are close enough to read as one: the same row
    /// or an adjacent one, and horizontally within [`Cells::PAD`] columns.
    /// The vertical slack is what makes the place layer thin itself on a
    /// whole-world map and fill in as it zooms, instead of packing every
    /// latitude band solid at any scale.
    fn touches(self, other: Cells) -> bool {
        self.row.abs_diff(other.row) <= 1
            && self.from <= other.to.saturating_add(Self::PAD)
            && other.from <= self.to.saturating_add(Self::PAD)
    }
}

/// The label cells already spoken for on a pane. A later label overlapping
/// one of them is dropped rather than drawn: the last write to a canvas cell
/// wins, so two labels sharing a row would overprint into mush.
#[derive(Default)]
struct Claimed(Vec<Cells>);

impl Claimed {
    fn free(&self, lines: &[Cells]) -> bool {
        !lines.iter().any(|c| self.0.iter().any(|had| had.touches(*c)))
    }

    fn claim(&mut self, lines: &[Cells]) {
        self.0.extend_from_slice(lines);
    }
}

/// One horizontal slice of the map panel: the cells it covers and the *real*
/// longitude range drawn in them. Away from the dateline there is a single
/// pane covering all of `inner`; a follow window that runs past ±180° is cut
/// into two that tile `inner` exactly and share one degrees-per-column.
struct Pane {
    rect: Rect,
    x: [f64; 2],
}

/// Split `inner` into map panes for a window spanning the longitudes `x`.
///
/// The cut is made on a whole-column boundary so the two panes line up
/// seamlessly, and each pane's bounds are the real longitudes of its own
/// columns — the wrapped pane's shifted by ∓360° — so every shape can still
/// be drawn straight from the `[-180, 180)` geometry with no re-projection
/// and ratatui's own clipping drops whatever falls outside a pane.
fn panes(inner: Rect, x: [f64; 2]) -> Vec<Pane> {
    let cols = inner.width;
    let span = x[1] - x[0];
    // A degenerate grid, or a window that stays on the map: a single pane,
    // byte-for-byte the pre-split single-canvas render.
    if cols < 2 || span <= 0.0 || (x[0] >= -180.0 && x[1] <= 180.0) {
        return vec![Pane { rect: inner, x }];
    }
    let d = span / (cols - 1) as f64;

    // A pane covering panel columns `a..b`: its rect is that column range, its
    // bounds are those columns' longitudes with `shift` (0 or ±360°) applied.
    // A one-column pane would otherwise get a zero-width span that
    // `Painter::get_point` rejects outright, so widen it to one column's worth.
    let pane = |a: u16, b: u16, shift: f64| {
        let lo = x[0] + a as f64 * d + shift;
        let hi = x[0] + (b - 1) as f64 * d + shift;
        Pane {
            rect: Rect::new(inner.x + a, inner.y, b - a, inner.height),
            x: if hi - lo > 0.0 { [lo, hi] } else { [lo - d / 2.0, lo + d / 2.0] },
        }
    };

    if x[1] > 180.0 {
        // Centre east of +90°: the leading columns up to +180° stay put, the
        // rest wrap onto the −180° edge.
        let cut = (((180.0 - x[0]) / d).floor() as u16 + 1).clamp(1, cols - 1);
        vec![pane(0, cut, 0.0), pane(cut, cols, -360.0)]
    } else {
        // Centre west of −90°: the leading columns below −180° wrap onto the
        // +180° edge, the rest stay put.
        let cut = (((-180.0 - x[0]) / d).ceil() as u16).clamp(1, cols - 1);
        vec![pane(0, cut, 360.0), pane(cut, cols, 0.0)]
    }
}

/// Night and civil-twilight cells of the map, one sample per canvas cell.
///
/// Sampled on the `Grid`'s own cell centres rather than a fixed lon/lat mesh:
/// the wash is painted on a `Marker::Block` grid, whose resolution is exactly
/// `(cols, rows)` cells (unlike `Braille`'s 2x4-dots-per-cell), so a mesh at
/// any other spacing either leaves gaps between samples (coarser than a cell)
/// or repaints the same cell for nothing (finer). Sampling the grid itself
/// also means the follow-mode zoom — half the degrees per cell of the
/// whole-world view — gets exactly as fine a wash as the whole-world view
/// does, which a fixed-degree mesh could not offer both at once.
fn night_wash(grid: &Grid, now: DateTime<Utc>) -> (Vec<(f64, f64)>, Vec<(f64, f64)>) {
    let sun = SunGeometry::at(now);
    let mut night = Vec::new();
    let mut twilight = Vec::new();
    for row in 0..grid.rows() {
        // Clamp guards the globe only — `elevation_deg` needs a real latitude
        // — not the canvas bounds. `view_bounds` never returns a window that
        // runs past a pole, and `Grid::y_of`/`x_of` now hit their endpoints
        // exactly, so every sample is already inside the pane ratatui will
        // test it against.
        let lat = grid.y_of(row).clamp(-90.0, 90.0);
        for col in 0..grid.cols() {
            let lon = grid.x_of(col);
            let elevation = sun.elevation_deg(lat, lon);
            if elevation <= CIVIL_TWILIGHT_DEG {
                night.push((lon, lat));
            } else if elevation <= 0.0 {
                twilight.push((lon, lat));
            }
        }
    }
    (night, twilight)
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
    ///
    /// Written as a two-weight interpolation between the bounds rather than
    /// `x[0] + col * span / (cols - 1)` so that the last column lands on
    /// `x[1]` *bit-for-bit* (and column 0 on `x[0]`): with `t == 1.0` the
    /// first term is a clean `0.0 * x[0]` and the second an exact `1.0 *
    /// x[1]`. The stepped form reassociates as `x[0] + (cols - 1) *
    /// (span / (cols - 1))`, which lands a few ULPs either side of `x[1]`.
    /// That matters because [`night_wash`] feeds these coordinates straight
    /// to ratatui's `Painter::get_point`, which *rejects* rather than clamps
    /// a point outside the canvas bounds — so an edge sample a single ULP
    /// over drops the whole rightmost column of the night wash for that
    /// frame, and in follow mode the bounds shift every frame, so the
    /// dropped column flickers in and out. Same reasoning drives [`y_of`].
    ///
    /// [`y_of`]: Grid::y_of
    fn x_of(&self, col: u16) -> f64 {
        let cols = self.cols();
        if cols < 2 {
            return self.x[0];
        }
        let t = col as f64 / (cols - 1) as f64;
        self.x[0] * (1.0 - t) + self.x[1] * t
    }

    /// Degrees of latitude spanned by one cell row.
    fn deg_per_row(&self) -> f64 {
        (self.y[1] - self.y[0]) / (self.rows() - 1) as f64
    }

    /// The y-coordinate at the top edge of cell row `row` — the inverse of
    /// [`Grid::col_of`] along the other axis. Row 0 is the top of the canvas
    /// (`y[1]`), matching ratatui's own `Painter::get_point`, which maps `y`
    /// as `(top - y) * (rows - 1) / height`.
    ///
    /// Interpolated between the bounds for the exact-endpoints reason spelled
    /// out on [`x_of`](Grid::x_of): the last row must land on `y[0]`
    /// bit-for-bit or its wash samples fall through `Painter::get_point` and
    /// the bottom row of the night shading vanishes for the frame.
    fn y_of(&self, row: u16) -> f64 {
        let rows = self.rows();
        if rows < 2 {
            return self.y[1];
        }
        let t = row as f64 / (rows - 1) as f64;
        self.y[1] * (1.0 - t) + self.y[0] * t
    }

    /// The cell row a y-coordinate lands on — [`Grid::col_of`]'s twin on the
    /// latitude axis, with the same round-and-epsilon so it round-trips with
    /// [`Grid::y_of`]. Only used to reserve a label's row for collision
    /// checks, so approximate agreement with ratatui's own truncation is
    /// enough.
    fn row_of(&self, y: f64) -> u16 {
        let rows = self.rows();
        let span = (self.y[1] - self.y[0]).abs();
        if rows < 2 || span <= 0.0 {
            return 0;
        }
        (((self.y[1] - y) * (rows - 1) as f64 / span + 1e-9).round() as u16).min(rows - 1)
    }

    /// Whether longitude `x` falls within this grid's window. When the follow
    /// view is split across the dateline each pane's paint closure still sees
    /// every marker, so a marker is skipped unless the pane it belongs to is
    /// the one drawing it — without this the left-label branch of
    /// [`print_marker`] would draw at an in-bounds column and leave a stray
    /// label pinned to a pane edge.
    fn contains(&self, x: f64) -> bool {
        let (lo, hi) = (self.x[0].min(self.x[1]), self.x[0].max(self.x[1]));
        x >= lo && x <= hi
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
/// isn't clipped by an edge the way a label fixed to one side would be. The
/// placement is decided by [`label_layout`], which [`marker_cells`] reuses so
/// the place layer can reserve exactly the cells this draws into.
fn print_marker(ctx: &mut Context<'_>, grid: &Grid, lon: f64, lat: f64, marker: &MarkerLabel<'_>) {
    // `None` when the marker isn't in this pane's window — leave it to the
    // pane that does hold it. This also fixes a pre-existing glitch: a pad
    // far outside a narrow follow window used to have its label pinned to the
    // map edge, because `col_of` saturated to the last column and the label
    // then flipped left onto an in-bounds coordinate.
    let Some(layout) = label_layout(grid, lon, marker) else {
        return;
    };

    let MarkerLabel { glyph, color, detail, .. } = *marker;
    let glyph_span = || Span::styled(glyph.to_string(), Style::new().fg(color).bold());

    // No room for a name, or none given: just the glyph.
    if layout.shown.is_empty() {
        ctx.print(lon, lat, glyph_span());
        return;
    }

    let shown = layout.shown.as_str();
    if layout.right_side {
        ctx.print(
            lon,
            lat,
            Line::from(vec![glyph_span(), Span::styled(format!(" {shown}"), Style::new().fg(color))]),
        );
    } else {
        ctx.print(
            grid.x_of(layout.start_col),
            lat,
            Line::from(vec![Span::styled(format!("{shown} "), Style::new().fg(color)), glyph_span()]),
        );
    }

    if let Some(detail) = detail {
        print_detail_line(ctx, grid, layout.text_col, lat, detail);
    }
}

/// Where a marker's label lands: which side of the glyph, the text after
/// truncation (empty when there's no name to draw), and the cell columns it
/// occupies. Resolved once so [`print_marker`] and [`marker_cells`] can never
/// disagree about placement.
struct LabelLayout {
    right_side: bool,
    shown: String,
    /// Column of the glyph itself.
    marker_col: u16,
    /// Column the first drawn line starts at — the glyph or the text,
    /// whichever is leftmost.
    start_col: u16,
    /// Column the name, and any detail line, is aligned to.
    text_col: u16,
}

/// Compute [`LabelLayout`] for `marker` at longitude `lon`. `None` when the
/// marker is outside this pane's window.
fn label_layout(grid: &Grid, lon: f64, marker: &MarkerLabel<'_>) -> Option<LabelLayout> {
    if !grid.contains(lon) {
        return None;
    }
    let cols = grid.cols();
    let marker_col = grid.col_of(lon);

    if cols < 2 || marker.name.is_empty() {
        return Some(LabelLayout {
            right_side: true,
            shown: String::new(),
            marker_col,
            start_col: marker_col,
            text_col: marker_col,
        });
    }

    let room_right = cols.saturating_sub(marker_col + 1);
    let room_left = marker_col;
    let right_side = label_on_right(room_left, room_right, marker.name.chars().count());

    let budget = (if right_side { room_right } else { room_left }).saturating_sub(1) as usize;
    let shown = truncate(marker.name, budget);

    let (start_col, text_col) = if right_side {
        (marker_col, marker_col + 1)
    } else {
        let start = marker_col.saturating_sub(1 + shown.chars().count() as u16);
        (start, start)
    };

    Some(LabelLayout { right_side, shown, marker_col, start_col, text_col })
}

/// The cells [`print_marker`] draws `marker`'s label into on this grid: one
/// span for the glyph-and-name line, plus one for the detail line when there
/// is one. Empty when the marker isn't in this pane's window. [`draw_places`]
/// uses it to keep place labels off the markers and off each other.
fn marker_cells(grid: &Grid, lon: f64, lat: f64, marker: &MarkerLabel<'_>) -> Vec<Cells> {
    let Some(layout) = label_layout(grid, lon, marker) else {
        return Vec::new();
    };
    let last = grid.cols().saturating_sub(1);
    let row = grid.row_of(lat);

    if layout.shown.is_empty() {
        return vec![Cells { row, from: layout.marker_col, to: layout.marker_col }];
    }

    // Right side draws `glyph ' ' name` from `start_col` (== the glyph
    // column); left side draws `name ' ' glyph` from `start_col`. Either way
    // the run is `shown_len + 2` cells wide.
    let shown_len = layout.shown.chars().count() as u16;
    let mut out =
        vec![Cells { row, from: layout.start_col, to: (layout.start_col + shown_len + 1).min(last) }];

    if let Some(detail) = marker.detail {
        if let Some(cells) = detail_cells(grid, layout.text_col, lat, detail) {
            out.push(cells);
        }
    }
    out
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

/// The cells [`print_detail_line`] would occupy, or `None` when it would draw
/// nothing (a one-row grid, or the detail row falling below the visible
/// band). Mirrors that function's own row choice — one row below the marker,
/// or one above at the bottom edge.
fn detail_cells(grid: &Grid, text_col: u16, lat: f64, text: &str) -> Option<Cells> {
    if grid.rows() < 2 {
        return None;
    }
    let deg_per_row = grid.deg_per_row();
    let mut y = lat - deg_per_row;
    if y < grid.y[0] {
        y = lat + deg_per_row;
    }
    if y > grid.y[1] {
        return None;
    }
    let last = grid.cols().saturating_sub(1);
    let len = truncate(text, grid.cols().saturating_sub(text_col) as usize).chars().count() as u16;
    Some(Cells {
        row: grid.row_of(y),
        from: text_col,
        to: (text_col + len.saturating_sub(1)).min(last),
    })
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
    fn y_of_maps_row_zero_to_the_top_and_the_last_row_to_the_bottom() {
        let g = Grid { inner: Rect::new(0, 0, 1, 50), x: [-180.0, 180.0], y: [-90.0, 90.0] };
        assert_eq!(g.y_of(0), 90.0);
        assert_eq!(g.y_of(49), -90.0);
    }

    #[test]
    fn y_of_degenerate_grid_is_the_top_bound() {
        let one_row = Grid { inner: Rect::new(0, 0, 1, 1), x: [-180.0, 180.0], y: [-90.0, 90.0] };
        assert_eq!(one_row.y_of(0), 90.0);
        let no_rows = Grid { inner: Rect::new(0, 0, 1, 0), x: [-180.0, 180.0], y: [-90.0, 90.0] };
        assert_eq!(no_rows.y_of(0), 90.0);
    }

    #[test]
    fn night_wash_classifies_every_cell_at_most_once_and_covers_roughly_half_the_globe() {
        use chrono::TimeZone;
        let g = Grid { inner: Rect::new(0, 0, 72, 36), x: [-180.0, 180.0], y: [-90.0, 90.0] };
        let t = Utc.with_ymd_and_hms(2026, 9, 4, 6, 0, 0).unwrap();
        let (night, twilight) = night_wash(&g, t);

        let total = usize::from(g.cols()) * usize::from(g.rows());
        assert!(night.len() + twilight.len() <= total);

        // No cell is classified as both: the two sets are disjoint.
        let overlap = night.iter().filter(|p| twilight.contains(p)).count();
        assert_eq!(overlap, 0);

        // Full night plus twilight should sit somewhere around half the
        // sampled cells — loosely, since twilight and the terminator's
        // curvature both eat into the round number, but nowhere near "all"
        // or "none" the way a broken sign or a stuck constant would produce.
        let dark_fraction = (night.len() + twilight.len()) as f64 / total as f64;
        assert!((0.3..0.7).contains(&dark_fraction), "dark fraction {dark_fraction}");
    }

    /// Follow-window bounds that have bitten the wash, each reconstructed from
    /// a flickering frame. The first has a longitude span whose far edge the
    /// old stepped `x_of` overshot (a ×2 window centred at −83.39°); the
    /// second a latitude span whose bottom row the old `y_of` undershot (a ×2
    /// window whose centre latitude carried full mantissa entropy). The third
    /// is the plain whole-world grid — a regression guard on the common path,
    /// where the bounds are round and nothing missed even before the fix.
    fn edge_case_grids() -> [Grid; 3] {
        [
            Grid {
                inner: Rect::new(0, 0, 120, 90),
                x: [-173.38698115457174, 6.613018845428243],
                y: [-90.0, 90.0],
            },
            Grid {
                inner: Rect::new(0, 0, 150, 20),
                x: [-92.51915, 87.48085],
                y: [-16.265400000435616, 73.73459999956438],
            },
            Grid { inner: Rect::new(0, 0, 200, 90), x: [-180.0, 180.0], y: [-90.0, 90.0] },
        ]
    }

    #[test]
    fn x_of_and_y_of_hit_their_far_bounds_exactly() {
        // Bit-for-bit, not `< epsilon`: `Painter::get_point` *rejects* rather
        // than clamps a coordinate even one ULP past the bound, and
        // `Points::draw` then silently skips it — so a stepped edge sample
        // drops the whole outer row or column of the night wash for that
        // frame, and in follow mode the bounds move every frame so it
        // flickers. The interpolated forms land on the endpoints exactly.
        for g in edge_case_grids() {
            assert_eq!(g.x_of(0), g.x[0], "x_of(0), inner {:?}", g.inner);
            assert_eq!(g.x_of(g.cols() - 1), g.x[1], "x_of(last), inner {:?}", g.inner);
            assert_eq!(g.y_of(0), g.y[1], "y_of(0), inner {:?}", g.inner);
            assert_eq!(g.y_of(g.rows() - 1), g.y[0], "y_of(last), inner {:?}", g.inner);
        }
    }

    #[test]
    fn night_wash_keeps_every_sample_inside_the_canvas_bounds() {
        use chrono::TimeZone;
        // The invariant the flicker violates: every cell the wash emits must
        // pass the same test `Painter::get_point` applies before drawing it,
        // `x[0] <= lon <= x[1] && y[0] <= lat <= y[1]`. A sample outside it is
        // dropped by ratatui, unshaded, and the bug is back.
        let t = Utc.with_ymd_and_hms(2026, 9, 4, 6, 0, 0).unwrap();
        for g in edge_case_grids() {
            let (night, twilight) = night_wash(&g, t);
            for (lon, lat) in night.iter().chain(&twilight) {
                assert!(
                    *lon >= g.x[0] && *lon <= g.x[1] && *lat >= g.y[0] && *lat <= g.y[1],
                    "sample ({lon}, {lat}) outside bounds x {:?} y {:?}",
                    g.x,
                    g.y
                );
            }
        }
    }

    #[test]
    fn night_wash_shades_every_cell_of_a_fully_dark_follow_window() {
        use chrono::TimeZone;
        // A ×8 follow window parked over the antisolar point (~7°S 90°W at
        // this instant) is night in every corner, so the wash must emit one
        // night sample per cell and no gaps — the behavioural form of the bug
        // report, where the bottom row of exactly such a window went unshaded.
        let g = Grid {
            inner: Rect::new(0, 0, 80, 30),
            x: [-123.75, -78.75],
            y: [-41.25, -18.75],
        };
        let t = Utc.with_ymd_and_hms(2026, 9, 4, 6, 0, 0).unwrap();
        let (night, twilight) = night_wash(&g, t);

        let cells = usize::from(g.cols()) * usize::from(g.rows());
        assert_eq!(night.len(), cells, "every cell should be full night");
        assert!(twilight.is_empty(), "window is well past civil twilight");
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

    /// A map-panel rect `cols` wide, tall enough that the height is never the
    /// thing under test.
    fn inner_rect(cols: u16) -> Rect {
        Rect::new(0, 0, cols, 20)
    }

    /// A ground point at `(lat, lon)` for the `view_bounds` tests.
    fn gp(lat: f64, lon: f64) -> GeoPoint {
        GeoPoint::new(lat, lon, 0.0)
    }

    #[test]
    fn view_bounds_without_a_centre_is_the_whole_world() {
        for zoom in 0..=MAX_ZOOM + 2 {
            assert_eq!(view_bounds(None, zoom), ([-180.0, 180.0], [-90.0, 90.0]));
        }
    }

    #[test]
    fn view_bounds_halves_the_window_at_each_zoom_level() {
        let c = gp(0.0, 0.0);
        let lon_spans: Vec<f64> = (0..=MAX_ZOOM)
            .map(|z| {
                let (x, _) = view_bounds(Some(&c), z);
                x[1] - x[0]
            })
            .collect();
        assert_eq!(lon_spans, vec![180.0, 90.0, 45.0, 22.5]);

        // Every level keeps the whole-world 2:1 longitude:latitude aspect, so
        // zooming only rescales the projection, never reshapes it.
        for z in 0..=MAX_ZOOM {
            let (x, y) = view_bounds(Some(&c), z);
            let (lon, lat) = (x[1] - x[0], y[1] - y[0]);
            assert!((lon - 2.0 * lat).abs() < 1e-9, "level {z}: {lon} vs 2×{lat}");
        }
    }

    #[test]
    fn view_bounds_keeps_the_window_on_the_map_at_every_latitude() {
        for z in 0..=MAX_ZOOM {
            let full_height = view_bounds(Some(&gp(0.0, 0.0)), z).1;
            let height = full_height[1] - full_height[0];
            for step in -18..=18 {
                let lat = f64::from(step) * 5.0;
                let (_, y) = view_bounds(Some(&gp(lat, 0.0)), z);
                assert!(y[0] >= -90.0 - 1e-9 && y[1] <= 90.0 + 1e-9, "lat {lat} level {z}: {y:?}");
                assert!((y[1] - y[0] - height).abs() < 1e-9, "lat {lat} level {z} lost height");
            }
        }
    }

    #[test]
    fn view_bounds_lets_a_tighter_zoom_reach_closer_to_the_pole() {
        // A sub-point at 80°N: the tightest window (±5.625° of latitude) fits
        // around it untouched, while the widest (±45°) has to clamp its centre
        // well south of it.
        let (_, tight) = view_bounds(Some(&gp(80.0, 0.0)), MAX_ZOOM);
        let tight_centre = (tight[0] + tight[1]) / 2.0;
        assert!((tight_centre - 80.0).abs() < 1e-9, "tight centre {tight_centre} should be 80");
        assert!(tight[1] > 85.0, "tight window should reach past 85°N, got {}", tight[1]);

        let (_, wide) = view_bounds(Some(&gp(80.0, 0.0)), 0);
        let wide_centre = (wide[0] + wide[1]) / 2.0;
        assert!(wide_centre < 80.0 - 1e-9, "wide centre {wide_centre} should be clamped south");
        assert!((wide[1] - 90.0).abs() < 1e-9, "wide window should sit flush against the pole");
    }

    #[test]
    fn view_bounds_leaves_longitude_unclamped_across_the_dateline() {
        for (z, hx) in ZOOM_HALF_SPANS.iter().enumerate() {
            let (x, _) = view_bounds(Some(&gp(0.0, 175.0)), z);
            assert!((x[0] - (175.0 - hx)).abs() < 1e-9);
            assert!((x[1] - (175.0 + hx)).abs() < 1e-9);
            assert!(x[1] > 180.0, "level {z}: window should overhang +180°, got {}", x[1]);
        }
    }

    #[test]
    fn zoom_title_names_the_scale_against_the_whole_world() {
        assert_eq!(zoom_title(None, 0), "MAP");
        assert_eq!(zoom_title(None, MAX_ZOOM), "MAP");
        let c = gp(0.0, 0.0);
        assert_eq!(zoom_title(Some(&c), 0), "MAP ×2");
        assert_eq!(zoom_title(Some(&c), 1), "MAP ×4");
        assert_eq!(zoom_title(Some(&c), 2), "MAP ×8");
        assert_eq!(zoom_title(Some(&c), MAX_ZOOM), "MAP ×16");
    }

    #[test]
    fn grid_contains_rejects_a_longitude_outside_the_window() {
        let g = Grid { inner: Rect::new(0, 0, 50, 10), x: [85.0, 180.0], y: [-90.0, 90.0] };
        assert!(g.contains(85.0) && g.contains(90.0) && g.contains(180.0));
        assert!(!g.contains(80.0));
        assert!(!g.contains(-100.0));
    }

    #[test]
    fn panes_are_a_single_pane_when_the_window_stays_inside_the_map() {
        let whole = panes(inner_rect(100), [-180.0, 180.0]);
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].rect, inner_rect(100));
        assert_eq!(whole[0].x, [-180.0, 180.0]);

        // A follow window that doesn't reach the dateline is one pane too.
        let zoomed = panes(inner_rect(100), [-40.0, 140.0]);
        assert_eq!(zoomed.len(), 1);
        assert_eq!(zoomed[0].x, [-40.0, 140.0]);
    }

    #[test]
    fn panes_of_a_degenerate_grid_are_a_single_pane() {
        assert_eq!(panes(inner_rect(1), [85.0, 265.0]).len(), 1);
        assert_eq!(panes(Rect::new(0, 0, 0, 20), [85.0, 265.0]).len(), 1);
    }

    #[test]
    fn panes_split_at_the_dateline_and_tile_the_inner_width_exactly() {
        // Every centre whose ±90° window crosses a dateline edge, east or west.
        for centre in [175.0, -175.0, 130.0, -95.0] {
            let ps = panes(inner_rect(120), [centre - 90.0, centre + 90.0]);
            assert_eq!(ps.len(), 2, "centre {centre}");
            assert_eq!(ps[0].rect.x, 0);
            assert_eq!(ps[1].rect.x, ps[0].rect.width, "panes are contiguous");
            assert_eq!(ps[0].rect.width + ps[1].rect.width, 120, "panes fill the width");
            assert!(ps.iter().all(|p| p.rect.width >= 1 && p.rect.height == 20));
        }
    }

    #[test]
    fn panes_hold_a_uniform_degrees_per_column_across_the_seam() {
        let ps = panes(inner_rect(100), [85.0, 265.0]);
        let deg_per_col = |p: &Pane| (p.x[1] - p.x[0]) / (p.rect.width - 1) as f64;
        assert!((deg_per_col(&ps[0]) - deg_per_col(&ps[1])).abs() < 1e-9);
        assert!((deg_per_col(&ps[0]) - 180.0 / 99.0).abs() < 1e-9);
    }

    #[test]
    fn panes_cover_the_windows_longitudes_once_each() {
        // Centre 175°E: the near half reaches from 85°E to +180°, the wrapped
        // half from −180° to −95° (i.e. on to 265°E). Together they cover the
        // 180°-wide window with nothing drawn twice.
        let ps = panes(inner_rect(100), [85.0, 265.0]);
        let (near, wrapped) = (&ps[0], &ps[1]);
        assert!((near.x[0] - 85.0).abs() < 2.0 && near.x[1] <= 180.0 + 1e-9);
        assert!(wrapped.x[0] >= -180.0 - 1e-9 && (wrapped.x[1] - -95.0).abs() < 2.0);
    }

    #[test]
    fn the_satellite_lands_on_the_centre_column_at_every_longitude() {
        // The whole point of follow mode: sweep the sub-point across the full
        // range of longitudes and the satellite's panel column never drifts
        // more than a cell off centre — the invariant the old ±90° clamp
        // broke near the dateline.
        let cols = 101u16;
        let centre_col = i32::from((cols - 1) / 2);
        // ...and at every zoom level: parameterising the sweep proves the
        // two-pane dateline split still centres the marker however narrow the
        // follow window gets.
        for hx in ZOOM_HALF_SPANS {
            for step in 0..=72 {
                let lon = -180.0 + f64::from(step) * 5.0;
                let ps = panes(inner_rect(cols), [lon - hx, lon + hx]);
                let holder = ps
                    .iter()
                    .find(|p| Grid { inner: p.rect, x: p.x, y: [-90.0, 90.0] }.contains(lon))
                    .expect("some pane holds the sub-point");
                let g = Grid { inner: holder.rect, x: holder.x, y: [-90.0, 90.0] };
                let panel_col = i32::from(holder.rect.x + g.col_of(lon));
                assert!(
                    (panel_col - centre_col).abs() <= 1,
                    "half-span {hx}, lon {lon}: column {panel_col} vs centre {centre_col}"
                );
            }
        }
    }

    // ---- the `p` place layer ------------------------------------------------

    /// A grid with real rows, for the place-layer tests below (the `col_of`/
    /// `x_of` tests above only ever needed one row).
    fn map_grid(cols: u16, rows: u16, x: [f64; 2], y: [f64; 2]) -> Grid {
        Grid { inner: Rect::new(0, 0, cols, rows), x, y }
    }

    #[test]
    fn claimed_rejects_a_nearby_label_and_accepts_a_separated_one() {
        let mut c = Claimed::default();
        c.claim(&[Cells { row: 5, from: 10, to: 20 }]);

        // Same row, columns overlap outright.
        assert!(!c.free(&[Cells { row: 5, from: 18, to: 25 }]));
        // Same row, abutting or within the padding gap: reads as one run.
        assert!(!c.free(&[Cells { row: 5, from: 21, to: 30 }]));
        assert!(!c.free(&[Cells { row: 5, from: 22, to: 30 }]));
        // Same row, a clear gap of PAD columns: allowed.
        assert!(c.free(&[Cells { row: 5, from: 23, to: 30 }]));
        // Directly above or below and overlapping: still too close.
        assert!(!c.free(&[Cells { row: 6, from: 12, to: 18 }]));
        // Two rows clear: fine.
        assert!(c.free(&[Cells { row: 7, from: 12, to: 18 }]));
    }

    #[test]
    fn marker_cells_covers_exactly_the_columns_print_marker_would_write() {
        let g = map_grid(120, 40, [-180.0, 180.0], [-90.0, 90.0]);

        // Right-side label: `glyph ' ' name` printed from the marker's own
        // column, so the run is name-length + 2 wide starting there.
        let m = MarkerLabel { glyph: "·", color: Theme::PLACE, name: "Nairobi", detail: None };
        let (lat, lon) = (-1.29, 36.82);
        let layout = label_layout(&g, lon, &m).unwrap();
        assert!(layout.right_side, "a short name near mid-map goes right");
        let shown = layout.shown.chars().count() as u16;
        let cells = marker_cells(&g, lon, lat, &m);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].row, g.row_of(lat));
        assert_eq!(cells[0].from, g.col_of(lon));
        assert_eq!(cells[0].to, (g.col_of(lon) + shown + 1).min(g.cols() - 1));

        // Left-side label near the right edge: `name ' ' glyph` printed from
        // `start_col`, glyph landing on the marker column.
        let m = MarkerLabel { glyph: "·", color: Theme::PLACE, name: "Vladivostok", detail: None };
        let (lat, lon) = (43.12, 172.5);
        let layout = label_layout(&g, lon, &m).unwrap();
        assert!(!layout.right_side, "a long name at the right edge flips left");
        let shown = layout.shown.chars().count() as u16;
        let cells = marker_cells(&g, lon, lat, &m);
        assert_eq!(cells[0].from, layout.start_col);
        assert_eq!(cells[0].to, (layout.start_col + shown + 1).min(g.cols() - 1));
        // The glyph sits on (or just left of, after saturation) the marker col.
        assert!(cells[0].to >= layout.marker_col.saturating_sub(1));
    }

    #[test]
    fn marker_cells_puts_the_detail_line_on_an_adjacent_row() {
        let g = map_grid(120, 40, [-180.0, 180.0], [-90.0, 90.0]);
        let detail = "SLC-4E · 34.63°N 120.61°W";
        let m = MarkerLabel { glyph: "◉", color: Theme::PAD, name: "SpaceX · Falcon 9", detail: Some(detail) };

        // Mid-map: the detail row is the one below.
        let cells = marker_cells(&g, -30.0, 0.0, &m);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].row, cells[0].row + 1);

        // Hard against the bottom bound: it flips to the row above instead.
        let cells = marker_cells(&g, -30.0, -89.5, &m);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[1].row + 1, cells[0].row);
    }

    #[test]
    fn a_narrower_map_never_labels_more_places_than_a_wider_one() {
        // Fewer columns can only mean more collisions, so the count is
        // monotone in width — the density genuinely follows how much room
        // there is, with nothing keyed to a zoom level.
        let mut last = 0usize;
        for cols in [40u16, 50, 64, 80, 110, 150] {
            let g = map_grid(cols, cols / 3, [-180.0, 180.0], [-90.0, 90.0]);
            let n = selected_places(&g, Claimed::default()).len();
            assert!(n >= last, "width {cols} labelled {n}, narrower labelled {last}");
            assert!(n <= MAX_PLACE_LABELS, "width {cols} exceeded the cap with {n}");
            last = n;
        }
        assert!(last == MAX_PLACE_LABELS, "a wide whole-world map should fill to the cap");
    }

    #[test]
    fn a_window_over_open_ocean_labels_only_its_few_anchors() {
        // The central Pacific: almost nothing is there, so the layer draws
        // the two or three sparse-region anchors that are and no more — and
        // every one it picks is really inside the window.
        let g = map_grid(90, 28, [150.0, 240.0], [-30.0, 30.0]);
        let picked = selected_places(&g, Claimed::default());
        assert!((1..=6).contains(&picked.len()), "pacific window labelled {}", picked.len());
        for p in &picked {
            assert!(g.contains(p.lon), "{} isn't in the window", p.name);
        }
    }

    #[test]
    fn place_labels_never_land_on_the_satellite_or_pad_label() {
        // A Europe-ish zoom, with a satellite marker parked right over London
        // where the city labels are densest.
        let g = map_grid(120, 36, [-40.0, 40.0], [30.0, 70.0]);
        let sat = MarkerLabel { glyph: "◆", color: Theme::SAT, name: "ISS (ZARYA)", detail: None };
        let seed_cells = marker_cells(&g, -0.13, 51.5, &sat);
        assert!(!seed_cells.is_empty(), "the satellite marker should be on this grid");

        let mut seed = Claimed::default();
        seed.claim(&seed_cells);
        for p in selected_places(&g, seed) {
            let m = MarkerLabel { glyph: p.glyph(), color: Theme::PLACE, name: p.name, detail: None };
            for c in marker_cells(&g, p.lon, p.lat, &m) {
                for s in &seed_cells {
                    assert!(!c.touches(*s), "{}'s label overlaps the satellite label", p.name);
                }
            }
        }
    }

    #[test]
    fn no_place_is_labelled_outside_its_own_pane_across_the_dateline() {
        // A follow window centred at 175°E, split into a near and a wrapped
        // pane. Each pane selects independently; a place must land in exactly
        // one, and only in the pane whose window really holds its longitude.
        let cols = 120u16;
        let ps = panes(inner_rect(cols), [175.0 - 90.0, 175.0 + 90.0]);
        assert_eq!(ps.len(), 2, "this centre must split");

        let mut labelled: Vec<&str> = Vec::new();
        for pane in &ps {
            let g = Grid { inner: pane.rect, x: pane.x, y: [-60.0, 60.0] };
            for p in selected_places(&g, Claimed::default()) {
                assert!(g.contains(p.lon), "{} labelled by a pane that doesn't hold it", p.name);
                labelled.push(p.name);
            }
        }
        let unique: std::collections::HashSet<_> = labelled.iter().collect();
        assert_eq!(unique.len(), labelled.len(), "a place was labelled twice across the seam");
    }
}
