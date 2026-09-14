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

use chrono::{DateTime, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::text::Span;
use ratatui::widgets::canvas::{Canvas, Circle, Context, Line as CanvasLine, Points};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::geo::{look_angles, GeoPoint, LookAngles};
use crate::orbit::{
    moon_look_angles, moon_phase, sample_pass, sky_sample, star_look_angles, sun_look_angles, Pass,
    SatState, SkySample, Tracker,
};
use crate::ui::anim::{hash01, lerp, noise};
use crate::ui::canvas::{Grid, DOTS_X, DOTS_Y};
use crate::ui::panels::{compass, local_date, local_hm, local_hms, split_footer};
use crate::ui::stars::STARS;
use crate::ui::{fitting_hint, panel_block, Highlight, Theme, PANEL_CHROME};

/// The brightest stars this plot will ever put a *name* beside — beyond this
/// many the disc is small enough that more labels would just overlap. Every
/// star in [`STARS`] still gets its bare mark; this only caps how many also
/// earn a name, in the brightest-first order the table is kept sorted in.
const MAX_STAR_LABELS: usize = 6;

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

    // Sun, Moon and the star field, all placed at the pass's own culmination
    // rather than the render instant: this panel is normally about a pass
    // still ahead, so "what's behind the arc" means the sky at the time of
    // the pass, not the sky right now. `sky::LookAngles` for a body below the
    // horizon (`elevation_deg < 0`) is deliberately not drawn — `project`
    // clamps anything past the horizon onto the rim, which would misplace it
    // rather than just omit it.
    let sun = sun_look_angles(&station, pass.peak);
    let moon = moon_look_angles(&station, pass.peak);
    let moon_glyph = moon_phase(pass.peak).glyph();

    // A one-row footer under the disc, but only when the pane can spare it —
    // a short map needs every row for the disc to stay round. `split_footer`
    // wants the block already drawn (above) so it splits the *inner* rect, the
    // same shape NEXT PASSES uses for its accuracy footer.
    let (disc_area, footer_area) = split_footer(inner, inner.height >= 8);

    let (x_bounds, y_bounds) = disc_bounds(disc_area);
    let grid = Grid { inner: disc_area, x: x_bounds, y: y_bounds };
    let sky = Sky { pass_time: pass.peak, station: &station, sun, moon, moon_glyph };
    // Wall time since the session started, not `app.sim_now()` — the
    // twinkle and any meteor are scenery riding along on the real clock, the
    // same choice `App::uptime` documents for every other idle-screen
    // effect: `sim_now()` pauses, reverses and jumps, and a twinkle that did
    // too would read as broken rather than paused.
    let phase = app.uptime().as_secs_f32();
    let canvas = Canvas::default()
        .marker(Marker::Braille)
        .x_bounds(x_bounds)
        .y_bounds(y_bounds)
        .paint(|ctx| paint(ctx, &grid, pass, &samples, peak, live, &sky, phase));
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

/// The non-satellite sky at the pass's own culmination — the Sun, the Moon
/// and (via [`STARS`]) the star field, gathered once in [`draw`] so `paint`
/// takes no ground station or time of its own. `pass_time` and `station` are
/// what [`draw_stars`] samples [`STARS`] against; `sun`/`moon` are already
/// resolved because [`sun_look_angles`]/[`moon_look_angles`] are one-shot
/// calls, not a per-star loop.
struct Sky<'a> {
    pass_time: DateTime<Utc>,
    station: &'a GeoPoint,
    sun: LookAngles,
    moon: LookAngles,
    moon_glyph: &'static str,
}

fn paint(
    ctx: &mut Context<'_>,
    grid: &Grid,
    pass: &Pass,
    samples: &[SkySample],
    peak: Option<SkySample>,
    live: Option<LookAngles>,
    sky: &Sky<'_>,
    phase: f32,
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

    // The star field, a passing meteor and the Sun/Moon, drawn before the
    // arc so nothing this panel is actually about — the pass — ever sits
    // under reference sky.
    draw_stars(ctx, grid, sky.pass_time, sky.station, phase);
    if let Some(m) = meteor_at(phase) {
        ctx.draw(&CanvasLine {
            x1: m.from.0,
            y1: m.from.1,
            x2: m.to.0,
            y2: m.to.1,
            // Bright at spawn, fading to the same dim slate the reference
            // sky already uses, rather than to black — so it dies into the
            // scenery instead of vanishing against it.
            color: lerp(Theme::VALUE, Theme::PLACE, m.age),
        });
        ctx.layer();
    }
    if sky.sun.elevation_deg >= 0.0 {
        let (x, y) = project(sky.sun.azimuth_deg, sky.sun.elevation_deg);
        grid.print_dot(ctx, x, y, Span::styled("☉", Style::new().fg(Theme::CAUTION)));
    }
    if sky.moon.elevation_deg >= 0.0 {
        let (x, y) = project(sky.moon.azimuth_deg, sky.moon.elevation_deg);
        grid.print_dot(ctx, x, y, Span::styled(sky.moon_glyph, Style::new().fg(Theme::ACCENT)));
    }

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

/// How many discrete twinkle brightness levels the star field draws in.
/// One `Points` call per tier — the same bucketing `map::night_wash` uses
/// for its aurora tiers, and for the same reason: a braille cell carries one
/// fg colour, so two stars landing in the same cell at genuinely different
/// continuous brightnesses would just fuse into whichever drew last. A
/// handful of discrete tiers, each its own layer-free `Points` draw, is
/// enough steps for a scintillation to read as motion without paying for a
/// draw call per star.
const TWINKLE_TIERS: usize = 4;

/// Which of [`TWINKLE_TIERS`] brightness levels `star_index`'s dot sits at,
/// at this `phase`. Scintillation amplitude scales with `mag`: Sirius
/// (magnitude ~-1.5, the brightest star on the table) barely dims, while the
/// faintest pointer stars (~2.4) swing across the whole range — matching
/// what actually happens looking up, since a fainter star's light is more
/// affected by atmospheric turbulence relative to its own brightness.
/// `star_index` seeds a per-star noise lane so neighbouring stars twinkle
/// out of step with each other rather than in lockstep.
fn twinkle_tier(star_index: usize, mag: f64, phase: f32) -> usize {
    const FAINTEST_MAG: f32 = 2.4;
    const BRIGHTEST_MAG: f32 = -1.5;
    let amp = ((mag as f32 - BRIGHTEST_MAG) / (FAINTEST_MAG - BRIGHTEST_MAG)).clamp(0.0, 1.0);
    let base = 1.0 - amp * 0.7; // even at minimum, a bright star stays high
    let flicker = noise(star_index as u32, phase * 1.3) * amp;
    let t = (base + flicker * 0.7).clamp(0.0, 1.0);
    ((t * (TWINKLE_TIERS - 1) as f32).round() as usize).min(TWINKLE_TIERS - 1)
}

/// The star field: every [`STARS`] entry above the local horizon at `time`
/// gets a bare dot, twinkling between [`Theme::PLACE`] and [`Theme::VALUE`]
/// (see [`twinkle_tier`]) in one shared set of braille layers, so — like the
/// coastline and the ground track on the map — it sits under whatever draws
/// after it, and the arc always wins where the two coincide. The brightest
/// [`MAX_STAR_LABELS`] of those visible also get their name, printed as a
/// label so it stays legible over the arc the way every other mark on this
/// plot does — labels are drawn after every layer regardless of call order
/// (see `print_marker`'s note in `ui::map`), so a name here can only ever sit
/// on top, never under. `STARS` is kept sorted brightest first, so walking it
/// in order already visits stars in the order worth labelling.
fn draw_stars(ctx: &mut Context<'_>, grid: &Grid, time: DateTime<Utc>, station: &GeoPoint, phase: f32) {
    let visible: Vec<(usize, &'static crate::ui::stars::Star, f64, f64)> = STARS
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let la = star_look_angles(s.ra_deg, s.dec_deg, station, time);
            (la.elevation_deg >= 0.0).then_some((i, s, la.azimuth_deg, la.elevation_deg))
        })
        .collect();

    let mut tiers: [Vec<(f64, f64)>; TWINKLE_TIERS] = std::array::from_fn(|_| Vec::new());
    for &(i, star, az, el) in &visible {
        tiers[twinkle_tier(i, star.mag, phase)].push(project(az, el));
    }
    let mut any_drawn = false;
    for (tier, points) in tiers.iter().enumerate() {
        if points.is_empty() {
            continue;
        }
        let t = tier as f32 / (TWINKLE_TIERS - 1) as f32;
        ctx.draw(&Points { coords: points, color: lerp(Theme::PLACE, Theme::VALUE, t) });
        any_drawn = true;
    }
    if any_drawn {
        ctx.layer();
    }

    // A lighter stand-in for the map's full label-collision machinery
    // (`ui::map::Claimed`): a handful of names on one small disc, not dozens
    // on a scrolling world map, so a flat "too close to an already-placed
    // label" check is enough — skipped stars keep their bare dot from the
    // layer above, just no name.
    let mut claimed: Vec<(u16, u16)> = Vec::new();
    for (_, star, az, el) in &visible {
        if claimed.len() >= MAX_STAR_LABELS {
            break;
        }
        let (x, y) = project(*az, *el);
        let (col, row) = (grid.dot_col_of(x), grid.dot_row_of(y));
        if claimed.iter().any(|&(c, r)| c.abs_diff(col) < 4 && r.abs_diff(row) < 2) {
            continue;
        }
        claimed.push((col, row));
        grid.print_dot(
            ctx,
            x,
            y,
            Span::styled(format!("· {}", star.name), Style::new().fg(Theme::PLACE)),
        );
    }
}

/// One shooting star in flight: the endpoints of its streak in disc
/// coordinates, and `age` in `0..1` from spawn to the end of its life — the
/// fade [`meteor_at`]'s caller draws it at.
struct Meteor {
    from: (f64, f64),
    to: (f64, f64),
    age: f32,
}

/// How often a meteor might appear: time is cut into slots this many seconds
/// long, and each slot independently rolls whether it spawns one.
const METEOR_SLOT_SECONDS: f32 = 8.0;
/// How long a spawned meteor's streak stays on screen, in seconds — well
/// inside its slot, so there's always a quiet gap before the next one could
/// appear.
const METEOR_LIFETIME_SECONDS: f32 = 0.6;
/// Chance a given slot spawns a meteor at all — roughly one every four slots,
/// so about one every half-minute on average, often enough to notice, rare
/// enough to still be a surprise.
const METEOR_SPAWN_CHANCE: f32 = 0.25;
/// A noise seed lane reserved for meteor rolls, chosen well clear of
/// `STARS`'s index range (under 40) so a meteor's spawn roll and a star's
/// twinkle roll never draw from the same hash lane by coincidence.
const METEOR_SEED: u32 = 0xFEED;

/// The meteor in flight at `phase`, if any. Time is bucketed into
/// [`METEOR_SLOT_SECONDS`]-long slots, each independently deciding — from a
/// hash of its own slot index, so the same slot always decides the same way —
/// whether it spawns a meteor living for the slot's first
/// [`METEOR_LIFETIME_SECONDS`]. Entirely a function of `phase`: nothing is
/// threaded through the render loop to remember "was a meteor already
/// flying", which is also what makes this trivial to test — the same phase
/// always answers the same way.
fn meteor_at(phase: f32) -> Option<Meteor> {
    if phase < 0.0 {
        return None;
    }
    let slot = (phase / METEOR_SLOT_SECONDS).floor();
    let slot_index = slot as u32;
    let t_in_slot = phase - slot * METEOR_SLOT_SECONDS;
    if t_in_slot >= METEOR_LIFETIME_SECONDS {
        return None;
    }
    // A step function, not `noise`'s smoothed interpolation: whether a slot
    // spawns a meteor at all must not drift as the phase crosses into it, or
    // a meteor could fade in from nothing right as its slot begins.
    if hash01(METEOR_SEED, slot_index) >= METEOR_SPAWN_CHANCE {
        return None;
    }
    let age = t_in_slot / METEOR_LIFETIME_SECONDS;

    // Start point and direction both drawn from the same slot index, so a
    // meteor's path is fixed for its whole life and different slots streak
    // across different parts of the disc rather than all repeating one path.
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, TAU};
    let start_angle = hash01(METEOR_SEED.wrapping_add(1), slot_index) * TAU;
    let travel_angle =
        start_angle + FRAC_PI_4 + hash01(METEOR_SEED.wrapping_add(2), slot_index) * FRAC_PI_2;
    let start_r = 0.15 + hash01(METEOR_SEED.wrapping_add(3), slot_index) * 0.5;
    const LENGTH: f32 = 0.25;
    let from = (f64::from(start_r * start_angle.cos()), f64::from(start_r * start_angle.sin()));
    let to = (
        f64::from(start_r * start_angle.cos() + LENGTH * travel_angle.cos()),
        f64::from(start_r * start_angle.sin() + LENGTH * travel_angle.sin()),
    );
    Some(Meteor { from, to, age })
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
            peak_azimuth_deg: 135.0,
            visible: true,
        };

        let rect = Rect::new(0, 0, 40, 20);
        let (x, y) = disc_bounds(rect);
        let grid = Grid { inner: rect, x, y };
        let station = crate::orbit::test_station();
        // Sun and Moon held below the horizon so they draw nothing here —
        // this test is about the arc, not the sky behind it.
        let sky = Sky {
            pass_time: now,
            station: &station,
            sun: LookAngles { azimuth_deg: 0.0, elevation_deg: -10.0, range_km: 0.0 },
            moon: LookAngles { azimuth_deg: 0.0, elevation_deg: -10.0, range_km: 0.0 },
            moon_glyph: "●",
        };
        let render = |live: Option<LookAngles>| {
            let mut buf = Buffer::empty(rect);
            Canvas::default()
                .marker(Marker::Braille)
                .x_bounds(x)
                .y_bounds(y)
                .paint(|ctx| paint(ctx, &grid, &pass, &samples, None, live, &sky, 0.0))
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
            peak_azimuth_deg: 135.0,
            visible: true,
        };
        let rect = Rect::new(0, 0, 40, 20);
        let (x, y) = disc_bounds(rect);
        let grid = Grid { inner: rect, x, y };
        let station = crate::orbit::test_station();
        let sky = Sky {
            pass_time: now,
            station: &station,
            sun: LookAngles { azimuth_deg: 0.0, elevation_deg: -10.0, range_km: 0.0 },
            moon: LookAngles { azimuth_deg: 0.0, elevation_deg: -10.0, range_km: 0.0 },
            moon_glyph: "●",
        };
        let render = |live: Option<LookAngles>| {
            let mut buf = Buffer::empty(rect);
            Canvas::default()
                .marker(Marker::Braille)
                .x_bounds(x)
                .y_bounds(y)
                .paint(|ctx| paint(ctx, &grid, &pass, &[], None, live, &sky, 0.0))
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

    /// A render harness for the Sun/Moon markers: a minimal pass and disc,
    /// with `sun`/`moon` set by the caller so each case controls exactly one
    /// body's elevation.
    fn render_sky(sky: &Sky<'_>) -> ratatui::buffer::Buffer {
        use ratatui::widgets::Widget;
        let pass = Pass {
            aos: sky.pass_time,
            los: sky.pass_time + chrono::Duration::minutes(6),
            peak: sky.pass_time + chrono::Duration::minutes(3),
            peak_elevation_deg: 45.0,
            aos_azimuth_deg: 90.0,
            los_azimuth_deg: 180.0,
            peak_azimuth_deg: 135.0,
            visible: true,
        };
        let rect = Rect::new(0, 0, 40, 20);
        let (x, y) = disc_bounds(rect);
        let grid = Grid { inner: rect, x, y };
        let mut buf = ratatui::buffer::Buffer::empty(rect);
        Canvas::default()
            .marker(Marker::Braille)
            .x_bounds(x)
            .y_bounds(y)
            .paint(|ctx| paint(ctx, &grid, &pass, &[], None, None, sky, 0.0))
            .render(rect, &mut buf);
        buf
    }

    /// `☉` only appears while the Sun is above the local horizon — below it,
    /// `project` would clamp it onto the rim as though it had just risen,
    /// which is exactly the misplacement this guards against.
    #[test]
    fn the_sun_marker_is_drawn_only_above_the_horizon() {
        let now = chrono::Utc::now();
        let station = crate::orbit::test_station();
        let has_sun = |b: &ratatui::buffer::Buffer| b.content.iter().any(|c| c.symbol() == "☉");

        let below = Sky {
            pass_time: now,
            station: &station,
            sun: LookAngles { azimuth_deg: 90.0, elevation_deg: -5.0, range_km: 0.0 },
            moon: LookAngles { azimuth_deg: 0.0, elevation_deg: -90.0, range_km: 0.0 },
            moon_glyph: "●",
        };
        assert!(!has_sun(&render_sky(&below)), "no Sun marker while it's below the horizon");

        let above = Sky { sun: LookAngles { elevation_deg: 20.0, ..below.sun }, ..below };
        assert!(has_sun(&render_sky(&above)), "a Sun marker once it's up");
    }

    /// The Moon marker draws its phase glyph, and (like the Sun) only above
    /// the horizon.
    #[test]
    fn the_moon_marker_draws_its_phase_glyph_only_above_the_horizon() {
        let now = chrono::Utc::now();
        let station = crate::orbit::test_station();
        let has_glyph =
            |b: &ratatui::buffer::Buffer, g: &str| b.content.iter().any(|c| c.symbol() == g);

        let below = Sky {
            pass_time: now,
            station: &station,
            sun: LookAngles { azimuth_deg: 0.0, elevation_deg: -90.0, range_km: 0.0 },
            moon: LookAngles { azimuth_deg: 200.0, elevation_deg: -3.0, range_km: 385_000.0 },
            moon_glyph: "◕",
        };
        assert!(!has_glyph(&render_sky(&below), "◕"), "no Moon marker while it's below the horizon");

        let above = Sky { moon: LookAngles { elevation_deg: 15.0, ..below.moon }, ..below };
        assert!(has_glyph(&render_sky(&above), "◕"), "the Moon marker once it's up, in its own phase glyph");
    }

    /// `draw_stars` has to survive a real pass (not a synthetic one), and put
    /// at least one dot on a disc that spans a whole hemisphere — some star
    /// out of forty-odd is above the horizon at any given instant.
    #[test]
    fn draw_stars_marks_at_least_one_star_above_the_horizon() {
        use ratatui::buffer::Buffer;
        use ratatui::widgets::Widget;

        let (_, station, pass) = iss_pass_over_munich();
        let rect = Rect::new(0, 0, 60, 30);
        let (x, y) = disc_bounds(rect);
        let grid = Grid { inner: rect, x, y };
        let mut buf = Buffer::empty(rect);
        Canvas::default()
            .marker(Marker::Braille)
            .x_bounds(x)
            .y_bounds(y)
            .paint(|ctx| draw_stars(ctx, &grid, pass.peak, &station, 0.0))
            .render(rect, &mut buf);

        // Theme::PLACE, not one of the twinkle tier colours: the visible
        // star's *name* label is drawn in it unconditionally, regardless of
        // which brightness tier its dot lands in at this phase — see
        // `draw_stars`'s labelling loop.
        assert!(
            buf.content.iter().any(|c| c.fg == Theme::PLACE),
            "expected at least one star dot above the horizon"
        );
    }

    #[test]
    fn twinkle_tier_always_lands_inside_its_own_range() {
        for star_index in 0..40u32 {
            for step in 0..200 {
                let tier = twinkle_tier(star_index as usize, 1.0, step as f32 * 0.37);
                assert!(tier < TWINKLE_TIERS, "tier {tier} out of range for star {star_index}");
            }
        }
    }

    #[test]
    fn twinkle_tier_is_deterministic_for_the_same_star_and_phase() {
        assert_eq!(twinkle_tier(5, 0.9, 12.3), twinkle_tier(5, 0.9, 12.3));
    }

    #[test]
    fn the_brightest_star_on_the_table_twinkles_less_than_the_faintest() {
        // Sirius (mag -1.46) should stay clustered near the bright tiers
        // across a long phase sweep; the faintest pointer star on the table
        // should visit a wider spread of tiers over the same sweep — the
        // amplitude-scales-with-magnitude rule `twinkle_tier` documents.
        let sirius_mag = STARS.iter().map(|s| s.mag).fold(f64::INFINITY, f64::min);
        let faintest_mag = STARS.iter().map(|s| s.mag).fold(f64::NEG_INFINITY, f64::max);

        let spread = |mag: f64| -> usize {
            let tiers: std::collections::HashSet<usize> =
                (0..500).map(|i| twinkle_tier(1, mag, i as f32 * 0.31)).collect();
            tiers.len()
        };
        assert!(
            spread(sirius_mag) <= spread(faintest_mag),
            "Sirius (mag {sirius_mag}) should not out-twinkle the faintest star (mag {faintest_mag})"
        );
    }

    #[test]
    fn meteor_at_is_deterministic_for_the_same_phase() {
        // Whatever the answer at a given phase is — spawned or not — it must
        // be the same answer every time, since nothing else tells the render
        // loop "a meteor was already flying".
        for i in 0..500 {
            let phase = i as f32 * 0.41;
            let a = meteor_at(phase).map(|m| (m.from, m.to, m.age));
            let b = meteor_at(phase).map(|m| (m.from, m.to, m.age));
            assert_eq!(a, b, "meteor_at({phase}) differed between two calls");
        }
    }

    #[test]
    fn meteor_at_neither_never_nor_always_fires_over_a_long_sweep() {
        let mut slots_with_meteor = 0usize;
        let total_slots = 300;
        for slot in 0..total_slots {
            // Sample the middle of each slot's lifetime window, where a
            // spawned meteor is definitely still alive.
            let phase = slot as f32 * METEOR_SLOT_SECONDS + METEOR_LIFETIME_SECONDS * 0.5;
            if meteor_at(phase).is_some() {
                slots_with_meteor += 1;
            }
        }
        assert!(slots_with_meteor > 0, "expected at least one meteor over {total_slots} slots");
        assert!(
            slots_with_meteor < total_slots,
            "expected at least one quiet slot over {total_slots} slots"
        );
    }

    #[test]
    fn meteor_at_is_silent_outside_its_slots_lifetime_window() {
        // Whatever slot 0 decides, the instant right at the end of its
        // lifetime window and beyond must be quiet — a meteor does not span
        // an entire slot, only its first `METEOR_LIFETIME_SECONDS`.
        assert!(meteor_at(METEOR_LIFETIME_SECONDS).is_none());
        assert!(meteor_at(METEOR_SLOT_SECONDS - 0.01).is_none());
    }

    #[test]
    fn meteor_at_never_panics_on_a_negative_phase() {
        assert!(meteor_at(-5.0).is_none());
    }
}
