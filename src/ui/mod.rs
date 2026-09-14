//! Rendering. `draw` is the single entry point called once per frame.

mod anim;
mod canvas;
mod coastline;
mod globe;
mod help;
mod map;
mod panels;
mod places;
mod skyplot;
mod stars;

/// The tightest follow-mode zoom index, re-exported so `App::zoom_in` can
/// saturate against it without `mod map` being made public.
pub(crate) use map::MAX_ZOOM;

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, AppData, Panel, SearchState, TransmitterState};
use crate::geo::GeoPoint;
use crate::orbit::{Pass, Tracker};
use crate::simclock::ClockState;
use crate::source::Health;

/// Palette — a calm mission-control console: cyan structure, green nominal,
/// amber caution, red alert, on the terminal's own background.
pub struct Theme;
impl Theme {
    pub const FRAME: Color = Color::Rgb(70, 90, 100);
    pub const FRAME_FOCUS: Color = Color::Rgb(120, 220, 235);
    pub const LABEL: Color = Color::Rgb(130, 150, 160);
    pub const VALUE: Color = Color::Rgb(220, 230, 235);
    pub const NOMINAL: Color = Color::Rgb(120, 230, 150);
    pub const CAUTION: Color = Color::Rgb(240, 200, 120);
    pub const ALERT: Color = Color::Rgb(240, 120, 120);
    pub const ACCENT: Color = Color::Rgb(120, 200, 240);
    pub const SAT: Color = Color::Rgb(255, 240, 150);
    // A bright/dim pair carrying two related "then vs now" meanings: on the map
    // the ground track ahead of the satellite (FUTURE) vs behind it (PAST); on
    // the sky plot the stretch of a pass where the satellite is sunlit
    // (FUTURE) vs in the Earth's shadow (PAST). Both readings want the same
    // thing — one segment prominent, its counterpart receding — so they share
    // the pair rather than spend two more of the palette's lanes.
    pub const TRACK_FUTURE: Color = Color::Rgb(120, 200, 240);
    pub const TRACK_PAST: Color = Color::Rgb(70, 100, 120);
    // The visibility footprint. It shared TRACK_FUTURE's blue back when it was
    // an unmistakable circle and its shape did the distinguishing; now that it
    // is a real spherical cap — a long projected lens that at GEO sweeps most
    // of the map — a matching hue reads as more track. Violet is the one lane
    // the map has left: PAD's pink is the nearest neighbour but only ever a
    // single glyph, never a line this length.
    pub const FOOTPRINT: Color = Color::Rgb(175, 155, 235);
    // Now painted as a solid cell background rather than sparse foreground
    // dots (see ui::map), so both need to sit well below COAST's brightness
    // or the coastline stops reading as land against them.
    pub const NIGHT: Color = Color::Rgb(22, 26, 42);
    pub const TWILIGHT: Color = Color::Rgb(38, 44, 66);
    // The `a` aurora overlay's three brightening tiers — a background-wash
    // colour, not a foreground glyph hue, so this doesn't spend one of the
    // map's scarce braille lanes (see FOOTPRINT's note): it rides the same
    // `Marker::Block` layer NIGHT/TWILIGHT already paint on, and a `Block`
    // cell is a solid fill a later draw call simply overwrites, so the
    // brightest tier that touches a cell is always what shows. Green because
    // that's the OVATION nowcast's own colour convention and the aurora's
    // most commonly seen hue.
    pub const AURORA_LOW: Color = Color::Rgb(30, 65, 48);
    pub const AURORA_MED: Color = Color::Rgb(45, 120, 78);
    pub const AURORA_HIGH: Color = Color::Rgb(80, 210, 130);
    pub const COAST: Color = Color::Rgb(80, 110, 120);
    pub const STATION: Color = Color::Rgb(120, 230, 150);
    pub const PAD: Color = Color::Rgb(235, 150, 215);
    // The optional `p` place layer — cities and ground stations. Reference
    // scenery, not data, so it gets no hue of its own (the map's lanes are
    // spent — see FOOTPRINT): a neutral slate a notch below LABEL, bright
    // enough to read over COAST and the night wash, dim enough that the live
    // markers still sit clearly on top.
    pub const PLACE: Color = Color::Rgb(105, 120, 130);
}

/// Draw a whole frame. Takes `app` mutably only so the help overlay can
/// clamp its own scroll offset against the content it just laid out.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    if area.width < 80 || area.height < 24 {
        let msg = Paragraph::new(format!(
            "nadir needs at least 80x24 — this terminal is {}x{}.\nResize, or press q to quit.",
            area.width, area.height
        ))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        frame.render_widget(msg, centered(area, 60, 4));
        return;
    }

    // The simulated clock drives everything about the satellite; `wall_now` is
    // kept separate for the few things that must not scrub (the launch
    // countdown).
    let now = app.sim_now();
    let wall_now = Utc::now();
    let data = match app.data.read() {
        Ok(d) => d,
        Err(_) => return,
    };

    // The mission-control boot splash — after the min-size guard above (it
    // has no use showing a splash a terminal can't even fit), before
    // everything else, so a session that's about to skip it pays for none of
    // the panel layout below. `App::handle_key` swallows the keypress that
    // dismisses it before this ever sees a chance to run again — see its own
    // note on why the check lives there and not here.
    if app.splash_active() {
        draw_boot_splash(frame, area, app, &data);
        return;
    }

    let sat_state = data
        .tle
        .get()
        .and_then(|tr| tr.state_at(now).ok().map(|s| (tr.clone(), s)));
    let pad = selected_launch_pad(app, &data);
    // `None` unless the `a` key is on *and* the feed has ever returned a
    // grid — a failed or still-pending fetch just means no oval this frame,
    // never a panic or a blank grid drawn as though it were real data.
    let aurora = data.aurora.get().filter(|_| aurora_visible(app.aurora_overlay, app.clock.state()));

    if app.map_fullscreen {
        let [title, body, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        title_bar(frame, title, app, &data);
        // Fullscreen mode never swaps in the sky plot, even with a pass
        // highlighted in Passes — `m` is "always the map, just bigger", not a
        // second way to reach the plot. `b`'s globe toggle isn't that kind of
        // override, so it still applies here.
        if app.globe {
            globe::draw(frame, body, app, sat_state.as_ref(), now);
        } else {
            map::draw(frame, body, app, sat_state.as_ref(), now, pad, aurora);
        }
        status_bar(frame, status, app, &data);
    } else {
        let [title, main, bottom, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(8),
            Constraint::Length(1),
        ])
        .areas(area);

        let [map_area, right] = Layout::horizontal([
            Constraint::Min(MAP_FLOOR),
            Constraint::Length(right_width(main.width)),
        ])
        .areas(main);

        // TRACKED only needs to be as tall as it's useful: two border rows
        // plus up to five entries. Telemetry's height depends on whether its
        // one conditional row applies (RANGE needs a ground station) — see
        // `panels::telemetry::height`. `right_column_heights` works out how
        // the two share `right.height` when they don't both fit; NEXT PASSES
        // gives up rows first via its own `Min(0)` below, same as it already
        // does at the 80x24 minimum.
        let telem_h = panels::telemetry::height(app);
        let rows = (app.config.tracked.len() as u16).clamp(1, 5);
        let (tracked_h, telem_h) = right_column_heights(right.height, rows + 2, telem_h);
        let [tracked, telem, passes] = Layout::vertical([
            Constraint::Length(tracked_h),
            Constraint::Length(telem_h),
            Constraint::Min(0),
        ])
        .areas(right);
        let [weather, launches] =
            Layout::horizontal([Constraint::Length(40), Constraint::Min(0)]).areas(bottom);

        title_bar(frame, title, app, &data);
        // While NEXT PASSES holds focus the map pane shows a sky plot of the
        // highlighted pass instead of the world map — the same "a highlight
        // over here draws something over there" idiom the pad marker uses,
        // and it needs the element set the plot samples from. Any other focus,
        // or no element set yet, falls through to the map.
        match (selected_pass(app, now), sat_state.as_ref()) {
            (Some(highlight), Some((tracker, state))) => {
                skyplot::draw(frame, map_area, app, &highlight, tracker, state);
            }
            // The globe toggle (`b`) is the map pane's other alternate view —
            // yields to the sky plot above (a pass actually highlighted is
            // the more specific, more urgent thing to show) but otherwise
            // takes the pane over from the ordinary flat map.
            _ if app.globe => globe::draw(frame, map_area, app, sat_state.as_ref(), now),
            _ => map::draw(frame, map_area, app, sat_state.as_ref(), now, pad, aurora),
        }
        panels::tracked::draw(frame, tracked, app);
        panels::telemetry::draw(frame, telem, app, sat_state.as_ref(), data.tle.get().is_some(), now);
        panels::passes::draw(frame, passes, app, sat_state.as_ref(), now);
        panels::weather::draw(frame, weather, app, &data, now);
        panels::launches::draw(frame, launches, app, &data, wall_now);
        status_bar(frame, status, app, &data);
    }

    if let Some(picker) = &app.sat_input {
        sat_input_popup(frame, area, picker, &data.search);
    }
    if let Some(picker) = &app.tx_input {
        transmitter_popup(
            frame,
            area,
            picker,
            &data.tx_lookup,
            app.config.sat,
            app.config.active_transmitter(app.config.sat),
        );
    }
    if let Some(input) = &app.time_input {
        time_input_popup(frame, area, input);
    }

    drop(data);
    if app.show_help {
        help::draw(frame, area, app);
    }
}

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
/// remaining two-thirds — the quiet "any key to continue" beat, with nothing
/// left to animate. At least one row shows from the very first frame, so a
/// splash that gets skipped almost immediately still showed something rather
/// than a blank flash.
fn splash_reveal_count(uptime: std::time::Duration, splash: std::time::Duration, total: usize) -> usize {
    if total == 0 {
        return 0;
    }
    let third_secs = (splash.as_secs_f32() / 3.0).max(f32::EPSILON);
    let frac = (uptime.as_secs_f32() / third_secs).clamp(0.0, 1.0);
    ((frac * total as f32).ceil() as usize).clamp(1, total)
}

/// The boot splash's text: the wordmark and version line, always shown, then
/// one line per `lines` entry — a real row (label, dotted leader, value) for
/// the first `revealed` of them, a blank placeholder for the rest — and the
/// closing footer. Every call for a given `lines` returns the same number of
/// lines regardless of `revealed`: only whether a given row's *content* has
/// filled in yet changes, never how many lines there are or how long any
/// revealed row's text is. That's what keeps the splash's box a fixed size
/// as rows reveal (see `draw_boot_splash`) — nothing after the reveal, most
/// visibly the "any key to continue" footer, shifts as it fills in.
fn boot_text(lines: &[(&'static str, String)], revealed: usize) -> Vec<Line<'static>> {
    let mut text: Vec<Line> = NADIR_WORDMARK
        .iter()
        .map(|row| Line::from(Span::styled(*row, Style::new().fg(Theme::SAT).bold())))
        .collect();
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

/// Render the boot splash: `boot_text`'s rows, with `splash_reveal_count`
/// rows of `boot_lines` filled in and the rest still blank placeholders. The
/// box is sized from the *fully revealed* text (`boot_text(&lines,
/// lines.len())`), not from what's actually drawn this frame — so it's
/// always its final size, even on the very first frame, and never resizes or
/// re-centres as rows fill in over the first third of `config.ui.splash`.
fn draw_boot_splash(frame: &mut Frame, area: Rect, app: &App, data: &AppData) {
    let lines = boot_lines(app.config.ground_station(), data.tle.get(), app.config.sat, app.config.offline);
    let revealed = splash_reveal_count(app.uptime(), app.config.ui.splash, lines.len());

    let full = boot_text(&lines, lines.len());
    let height = full.len() as u16;
    let width = full.iter().map(Line::width).max().unwrap_or(0) as u16;

    let text = boot_text(&lines, revealed);
    frame.render_widget(Paragraph::new(text), centered(area, width, height));
}

/// Split the right column's `total` rows between TRACKED and TELEMETRY,
/// leaving whatever's left to NEXT PASSES via its own `Min(0)` constraint —
/// so NEXT PASSES gives up rows first. Below that, TRACKED shrinks to its
/// three-row floor (two borders, one entry) before TELEMETRY loses any rows
/// of its own: a thin TRACKED is still useful, a TELEMETRY panel that starts
/// dropping rows mid-list is not, so it keeps its full ask as long as
/// possible. Past TRACKED's floor, TELEMETRY is capped to what remains —
/// without this cap the two `Length` constraints could together ask for more
/// than `total`, and it would be the layout solver, not this function, that
/// silently decided which telemetry rows to drop.
fn right_column_heights(total: u16, tracked_rows: u16, telem_h: u16) -> (u16, u16) {
    const TRACKED_FLOOR: u16 = 3;
    let tracked_h = tracked_rows.min(total.saturating_sub(telem_h).max(TRACKED_FLOOR)).min(total);
    let telem_h = telem_h.min(total.saturating_sub(tracked_h));
    (tracked_h, telem_h)
}

/// Columns the map keeps before the right column is allowed to grow: the
/// `Constraint::Min` floor of the horizontal split, and the width the map's own
/// contents were tuned against.
const MAP_FLOOR: u16 = 44;

/// Width of the right-hand column for a `total`-column main band. It floats
/// between two bounds: 40 — the width it was fixed at before, so the TELEMETRY
/// rows degrade to exactly their old wording at the 80-column minimum — and 54,
/// whose 52 inner columns are the widest any telemetry row asks for (the full
/// DOPP line, `f MHz  rx f MHz  ±k kHz  MODE`). Wider than that is just blank
/// space in the panel, so the map keeps every column past 98.
///
/// At 80..=83 columns the ask (40) and the map's `Min(44)` floor still overlap;
/// ratatui 0.30 ranks `Min` above `Length`, so the column is squeezed to 36
/// there — identical to the old fixed `Length(40)` at that size.
fn right_width(total: u16) -> u16 {
    total.saturating_sub(MAP_FLOOR).clamp(40, 54)
}

/// Whether the map paints the aurora oval this frame. The `a` toggle is only
/// half of it: the OVATION nowcast is a real-world snapshot on its own
/// schedule, not a function of `now`, so it stops describing the map the
/// moment the displayed instant isn't real time. That is every state but
/// `Live` — a warp's continuous drift, but equally a `←`/`[` step, a `g`
/// jump, `n`/`N`, or a pause, each of which leaves the clock reading an
/// instant the nowcast was never about.
///
/// `pub(crate)`, not private: `App::is_animating` needs the same rule to
/// decide whether the aurora shimmer (`ui::anim`) is actually running this
/// frame, and duplicating it there would risk the two drifting apart.
pub(crate) fn aurora_visible(overlay: bool, clock: ClockState) -> bool {
    overlay && clock == ClockState::Live
}

/// The title-bar transport marker for the simulated clock, and whether the
/// clock is locked to wall time (which colours it and the clock green rather
/// than amber). Single-width geometric glyphs only, from the same family as the
/// map's `◆ ◇ ◉ ★` — an emoji would render two columns wide and the title bar
/// measures everything with `chars().count()`.
fn clock_marker(clock: &crate::simclock::SimClock) -> (String, bool) {
    match clock.state() {
        ClockState::Live => ("▸ ".to_string(), true),
        ClockState::Drifted => ("▸ ".to_string(), false),
        ClockState::Paused => ("‖ ".to_string(), false),
        ClockState::Warp(-1) => ("◂ ".to_string(), false),
        ClockState::Warp(r) if r <= -2 => (format!("◂◂{}x ", -r), false),
        ClockState::Warp(r) => (format!("▸▸{r}x "), false),
    }
}

/// The clock's offset from wall time, compact and signed: `Δ+2h14m`, `Δ-45m`,
/// `Δ+3d`, `Δ+40d`. Two most-significant units at most, the finer one dropped
/// when it is zero. Empty string for a sub-second offset, so the caller can
/// treat "nothing worth showing" and "no room to show it" the same way.
fn fmt_clock_offset(d: ChronoDuration) -> String {
    let secs = d.num_seconds().abs();
    if secs == 0 {
        return String::new();
    }
    let sign = if d < ChronoDuration::zero() { '-' } else { '+' };
    let (days, hours, mins, s) =
        (secs / 86_400, secs / 3_600 % 24, secs / 60 % 60, secs % 60);
    let body = if days > 0 {
        if hours > 0 { format!("{days}d{hours}h") } else { format!("{days}d") }
    } else if hours > 0 {
        if mins > 0 { format!("{hours}h{mins}m") } else { format!("{hours}h") }
    } else if mins > 0 {
        if s > 0 { format!("{mins}m{s}s") } else { format!("{mins}m") }
    } else {
        format!("{s}s")
    };
    format!("Δ{sign}{body}")
}

fn title_bar(frame: &mut Frame, area: Rect, app: &App, data: &crate::app::AppData) {
    let coords_full = match app.config.ground_station() {
        Some(g) => format!("{:.3},{:.3}", g.lat_deg, g.lon_deg),
        None => "no ground station".to_string(),
    };
    let coords_short = match app.config.ground_station() {
        Some(g) => format!("{:.1},{:.1}", g.lat_deg, g.lon_deg),
        None => "no ground station".to_string(),
    };
    let up_full = format!("  up {}  ", crate::source::fmt_age(app.uptime()));

    // The clock shows the *simulated* instant, not wall time. `marker` is its
    // transport state (`▸` live, `‖` paused, `▸▸60x` / `◂◂5x` warp); `live`
    // greens both marker and clock when the two coincide, and only then.
    let clock = app.sim_now().format("%Y-%m-%d %H:%M:%SZ").to_string();
    let (marker, live) = clock_marker(&app.clock);
    let clock_color = if live { Theme::NOMINAL } else { Theme::CAUTION };
    let marker_w = marker.chars().count();

    // Budget for the ground-station name (e.g. "Munich, Bavaria, Germany"):
    // whatever's left of the title bar after the satellite label on the
    // left and the fixed right-hand parts, less the " · " separator and a
    // minimum gap. The two halves are full-width overlapping paragraphs
    // (not a Layout split), so filling the width exactly would run the
    // satellite label straight into the location with no space between —
    // GAP keeps a visible seam between them. station_label (shared with
    // the NEXT PASSES title) shrinks the name to fit, or omits it below a
    // budget too small to say anything useful.
    const GAP: usize = 2;
    const PREFIX: &str = " nadir  ";
    let right_full =
        coords_full.chars().count() + up_full.chars().count() + marker_w + clock.chars().count() + 1;

    // The COSPAR id is the first thing to go when the bar is tight: it is the
    // least-used of the three identifiers, and — same overlapping-paragraph
    // reason GAP exists — a left half that outgrows its share is silently
    // overwritten by the right-aligned one instead of wrapping, so it has to
    // be measured against the fixed right side before it's added rather than
    // trimmed after the fact. Measured against the *un-shed* right side: the
    // uptime field and coordinate precision below give way before the id does.
    let sat = match data.tle.get() {
        Some(t) => {
            let base = format!("{} · NORAD {}", t.name(), t.norad_id());
            match t.international_designator() {
                Some(id)
                    if PREFIX.chars().count() + base.chars().count() + " · ".chars().count()
                        + id.chars().count()
                        + right_full
                        + GAP
                        <= area.width as usize =>
                {
                    format!("{} · {id} · NORAD {}", t.name(), t.norad_id())
                }
                _ => base,
            }
        }
        None => format!("NORAD {} · acquiring…", app.config.sat),
    };

    let left_len = PREFIX.chars().count() + sat.chars().count();

    // Once the id and the name have already gone, the scrub marker still has to
    // fit: drop the session-uptime field to a single space, then coarsen the
    // coordinates from 3 to 1 decimal, until the bare bar fits the width.
    let mut up = up_full;
    let mut coords = coords_full;
    if left_len + right_full + GAP > area.width as usize {
        let saved = up.chars().count() - 1;
        up = " ".to_string();
        if left_len + (right_full - saved) + GAP > area.width as usize {
            coords = coords_short;
        }
    }

    let right_fixed_len =
        coords.chars().count() + up.chars().count() + marker_w + clock.chars().count() + 1;
    let name_budget = (area.width as usize)
        .saturating_sub(left_len + right_fixed_len + GAP)
        .saturating_sub(3);
    let loc = app
        .config
        .location_name
        .as_deref()
        .and_then(|full| panels::station_label(full, name_budget))
        .map(|name| format!("{name} · {coords}"))
        .unwrap_or(coords);

    // The wall-clock offset, e.g. `Δ+2h14m`. Shown only while the clock is
    // scrubbed, and only if the bar still has room once everything else has
    // claimed its width — it just restates the marker, so it is the first
    // thing to drop and is never counted into `right_fixed_len` above.
    let delta = if live { String::new() } else { fmt_clock_offset(app.clock.offset()) };
    let delta_field = if !delta.is_empty()
        && left_len + right_fixed_len + delta.chars().count() + 1 + GAP <= area.width as usize
    {
        format!("{delta} ")
    } else {
        String::new()
    };

    let left = Line::from(vec![
        Span::styled(" nadir ", Style::new().fg(Color::Black).bg(Theme::ACCENT).bold()),
        Span::raw(" "),
        Span::styled(sat, Style::new().fg(Theme::VALUE).add_modifier(Modifier::BOLD)),
    ]);
    let right = Line::from(vec![
        Span::styled(loc, Style::new().fg(Theme::LABEL)),
        Span::styled(up, Style::new().fg(Theme::LABEL)),
        Span::styled(delta_field, Style::new().fg(clock_color)),
        Span::styled(marker, Style::new().fg(clock_color)),
        Span::styled(clock, Style::new().fg(clock_color)),
        Span::raw(" "),
    ]);
    frame.render_widget(Paragraph::new(left), area);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

fn status_bar(frame: &mut Frame, area: Rect, app: &App, data: &crate::app::AppData) {
    let mut spans = vec![Span::raw(" ")];
    let mut width: u16 = 1;
    for (name, sev, text) in [
        // Each chip's thresholds come from its own feed's *configured*
        // interval, so a feed refetching exactly on schedule reads green
        // whether that schedule is the shipped default or something the user
        // set under `[intervals]` — and amber genuinely means "late for the
        // cadence in effect", not late for a hardcoded one.
        chip("TLE", &data.tle, app.config.intervals.tle),
        chip("SWX", &data.weather, app.config.intervals.weather),
        chip("AUR", &data.aurora, app.config.intervals.aurora),
        chip("LCH", &data.launches, app.config.intervals.launches),
    ] {
        let color = match sev {
            0 => Theme::NOMINAL,
            1 => Theme::CAUTION,
            2 => Theme::ALERT,
            _ => Theme::LABEL,
        };
        let label = format!("{name} ");
        let value = format!("{text}  ");
        width += (label.chars().count() + value.chars().count()) as u16;
        spans.push(Span::styled(label, Style::new().fg(Theme::LABEL)));
        spans.push(Span::styled(value, Style::new().fg(color)));
    }
    let tiers = if app.sat_input.is_some() {
        vec![
            "type a name or NORAD id · Enter search/track · ↑↓ select · Esc cancel".to_string(),
            "Enter search/track · ↑↓ select · Esc cancel".to_string(),
            "Enter search · Esc cancel".to_string(),
        ]
    } else if app.tx_input.is_some() {
        vec![
            "↑↓ select · Enter use this downlink · Esc cancel".to_string(),
            "↑↓ select · Enter use · Esc cancel".to_string(),
            "Enter use · Esc cancel".to_string(),
        ]
    } else if app.time_input.is_some() {
        vec![
            "a time or offset (2026-09-08 04:30 · 04:30 · +90m) · Enter go · Esc cancel".to_string(),
            "e.g. +90m or 2026-09-08 04:30 · Enter go · Esc cancel".to_string(),
            "Enter go · Esc cancel".to_string(),
        ]
    } else if app.show_help {
        vec!["j/k scroll · ? close".to_string(), "? close".to_string()]
    } else {
        key_hints(app.focus, app.map_fullscreen)
    };

    // Give the chips their measured width first; the key hints get whatever
    // is left. Measured in characters, not bytes — every separator here is a
    // multi-byte `·` that occupies one column, so byte length would overstate
    // the hint by ten columns and hide it on terminals it actually fits.
    let [chip_area, hint_area] =
        Layout::horizontal([Constraint::Length(width), Constraint::Min(0)]).areas(area);
    frame.render_widget(Paragraph::new(Line::from(spans)), chip_area);
    if let Some(keys) = fitting_hint(&tiers, hint_area.width) {
        frame.render_widget(
            Paragraph::new(Span::styled(format!("{keys} "), Style::new().fg(Theme::LABEL)))
                .alignment(Alignment::Right),
            hint_area,
        );
    }
}

/// The key hints for the current focus, widest variant first.
///
/// The bottom bar advertises only what the *focused* panel can do plus the
/// handful of always-useful globals — the `1`–`6` digits and `Tab` are left to
/// the panel headers, which show every panel's number all the time. A panel's
/// own keys are the last thing dropped as the terminal narrows, because the bar
/// is the only place they appear; everything shed before them is either visible
/// elsewhere or reachable from `?`, which is why `? help` survives to the end.
/// A fullscreen map counts as map focus — whatever holds focus underneath, the
/// map is the only thing on screen.
fn key_hints(focus: Panel, map_fullscreen: bool) -> Vec<String> {
    // Two lengths of the focused panel's own keys: the wide list for the roomy
    // tiers, the narrow one for the last tier before a bare `? help`. Spelling
    // `j/k scroll` out per panel rather than deriving it from a `scrollable`
    // flag is what lets Tracked shed it at the narrow tier while keeping its
    // own two keys.
    let map_keys: (&[&str], &[&str]) = (
        &["m map", "f follow", "p places", "o sun/moon", "b globe", "+/- zoom"],
        &["m map", "f follow", "o sun/moon", "+/- zoom"],
    );
    let (wide, narrow): (&[&str], &[&str]) = if map_fullscreen {
        map_keys
    } else {
        match focus {
            Panel::Map => map_keys,
            Panel::Tracked => (
                &["j/k scroll", "Enter track", "d remove"],
                &["Enter track", "d remove"],
            ),
            Panel::Telemetry => (&["t/T downlink"], &["t/T downlink"]),
            Panel::Passes | Panel::Launches => (&["j/k scroll"], &["j/k scroll"]),
            Panel::Weather => (&["a aurora"], &["a aurora"]),
        }
    };

    let mut tiers: Vec<String> = [
        (wide, &["s sat", "r refresh", "space pause", ",/. warp", "g goto", "? help", "q quit"][..]),
        (wide, &["s sat", "r refresh", "? help", "q quit"][..]),
        (wide, &["r refresh", "? help", "q quit"][..]),
        (narrow, &["? help"][..]),
    ]
    .into_iter()
    .map(|(panel_keys, globals)| {
        let mut parts: Vec<&str> = panel_keys.to_vec();
        parts.extend_from_slice(globals);
        parts.join(" · ")
    })
    .collect();
    // Narrower than even the panel keys fit: the global `?` is worth more than
    // half a list of panel keys, since it documents all of them. A panel with
    // no keys of its own has already bottomed out there.
    if tiers.last().is_none_or(|last| last != "? help") {
        tiers.push("? help".to_string());
    }
    tiers
}

/// The first of `tiers` that fits in `width` columns, leaving room for the
/// trailing space the caller pads with. `None` when even the last one doesn't.
/// Shared with the sky plot's footer, which is the same widest-that-fits pick.
pub(in crate::ui) fn fitting_hint(tiers: &[String], width: u16) -> Option<&str> {
    tiers
        .iter()
        .find(|keys| width as usize > keys.chars().count() + 1)
        .map(String::as_str)
}

/// A status chip: label, colour severity, and text. The feed's refresh
/// interval is a parameter because a chip's age is only meaningful relative to
/// how often that feed is *supposed* to refresh (see
/// [`crate::source::Source::severity_for`]).
fn chip<T>(
    name: &'static str,
    src: &crate::source::Source<T>,
    every: std::time::Duration,
) -> (&'static str, u8, String) {
    (name, src.severity_for(every), chip_text(src))
}

fn chip_text<T>(src: &crate::source::Source<T>) -> String {
    match src.health() {
        Health::Pending => "wait".to_string(),
        Health::Live => "live".to_string(),
        Health::Stale(age) => crate::source::fmt_age(age),
        Health::Error(_) => "err".to_string(),
    }
}

/// Columns [`panel_block`] spends around the title text and can't give to it:
/// the leading `" {key} "` (3) and the trailing space after the title (1),
/// plus the two border columns. A panel that fits its own title to the pane
/// width — NEXT PASSES, the sky plot — budgets against `width - PANEL_CHROME`.
pub(in crate::ui) const PANEL_CHROME: usize = 6;

/// A bordered block whose frame brightens when the panel holds focus. The
/// title leads with `panel`'s own focus key (`1`–`6`), so it's obvious at a
/// glance which key jumps to which panel.
pub fn panel_block(panel: Panel, title: &str, focused: bool) -> Block<'_> {
    let border = if focused { Theme::FRAME_FOCUS } else { Theme::FRAME };
    let title_color = if focused { Theme::FRAME_FOCUS } else { Theme::LABEL };
    panel_block_styled(panel, title, border, title_color)
}

/// [`panel_block`] with the border and title colours given directly rather
/// than derived from focus — for the one case a panel's border carries
/// information beyond who holds keyboard focus: NEXT PASSES pulsing while a
/// pass is overhead (`ui::panels::passes`). `panel_block` is a thin wrapper
/// over this for every other call site, so both stay visually identical
/// outside that one case.
pub fn panel_block_styled(panel: Panel, title: &str, border: Color, title_color: Color) -> Block<'_> {
    Block::bordered().border_style(Style::new().fg(border)).title(Line::from(vec![
        Span::raw(" "),
        Span::styled(panel.key().to_string(), Style::new().fg(Theme::SAT).bold()),
        Span::styled(format!(" {title} "), Style::new().fg(title_color)),
    ]))
}

pub fn is_focused(app: &App, panel: Panel) -> bool {
    app.focus == panel
}

/// The pad of the highlighted launch, marked on the map with its provider,
/// vehicle name, site and coordinates.
pub(in crate::ui) struct PadMarker<'a> {
    pub vehicle: &'a str,
    /// `Launch::provider` — e.g. "SpaceX"; `"—"` when the feed didn't know.
    pub provider: &'a str,
    /// `Launch::pad` — e.g. "SLC-4E, Vandenberg SFB, CA, USA".
    pub site: &'a str,
    pub lat: f64,
    pub lon: f64,
}

/// The pad of the highlighted launch, or `None`. Only while Launches holds
/// focus — that is exactly when the row highlight is visible, so the map
/// marker and the highlighted row always appear and disappear together.
fn selected_launch_pad<'a>(
    app: &App,
    data: &'a crate::app::AppData,
) -> Option<PadMarker<'a>> {
    if app.focus != Panel::Launches {
        return None;
    }
    let list = &data.launches.get()?.list;
    // Mirrors the clamp panels::launches applies to the same list, so the
    // marker can never point at a different launch than the highlighted row.
    let l = list.get(app.list_pos.min(list.len().saturating_sub(1)))?;
    Some(PadMarker {
        vehicle: l.name.as_str(),
        provider: l.provider.as_str(),
        site: l.pad.as_str(),
        lat: l.pad_lat?,
        lon: l.pad_lon?,
    })
}

/// The pass highlighted in NEXT PASSES and where it sits in the list — what
/// the sky plot needs both to draw it and to title itself `pass 2 of 7`.
pub(crate) struct Highlight<'a> {
    pub pass: &'a Pass,
    /// Row index within [`App::upcoming_passes`], and that list's length.
    pub index: usize,
    pub total: usize,
}

/// The highlighted pass, or `None` unless NEXT PASSES holds focus — the row
/// highlight is only visible then, so the sky plot and the highlight appear
/// and vanish together (the same focus gate [`selected_launch_pad`] applies to
/// the pad marker). Indexes [`App::upcoming_passes`], the one definition of
/// what the panel shows, and applies the same `.min` clamp `panels::passes`
/// does, so the plot can never show a different pass than the highlighted row.
pub(crate) fn selected_pass(app: &App, now: DateTime<Utc>) -> Option<Highlight<'_>> {
    let passes = app.upcoming_passes(now);
    if app.focus != Panel::Passes || passes.is_empty() {
        return None;
    }
    let total = passes.len();
    let index = app.list_pos.min(total - 1);
    Some(Highlight { pass: &passes[index], index, total })
}

fn sat_input_popup(frame: &mut Frame, area: Rect, picker: &crate::app::SatPicker, search: &SearchState) {
    // Sized generously (and left fixed regardless of what's showing, so the
    // box doesn't resize under the user's fingers) to survive line-wrapping
    // in both a full page of results and a long error message.
    let popup = centered(area, 78, 13);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_style(Style::new().fg(Theme::FRAME_FOCUS))
        .title(" track satellite ");
    let mut lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  name or NORAD id: ", Style::new().fg(Theme::LABEL)),
            Span::styled(
                format!("{}▏", picker.query),
                Style::new().fg(Theme::VALUE).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
    ];

    match search {
        SearchState::Idle => lines.push(Line::from(Span::styled(
            "  Enter to search Celestrak's catalogue, or type a NORAD id",
            Style::new().fg(Theme::LABEL),
        ))),
        SearchState::Busy => {
            lines.push(Line::from(Span::styled("  searching…", Style::new().fg(Theme::LABEL))))
        }
        SearchState::Done { results, .. } if results.is_empty() => {
            lines.push(Line::from(Span::styled(
                format!("  no match for '{}'", picker.query),
                Style::new().fg(Theme::ALERT),
            )));
        }
        SearchState::Done { results, .. } => {
            // Rows above `lines` already used: a blank line, the input line,
            // another blank. A broad query (e.g. "STARLINK") can return up
            // to 20 results, more than the fixed popup height shows at
            // once — scroll the window to keep the selected row visible
            // rather than letting it run off the bottom unseen.
            let visible = (popup.height as usize).saturating_sub(2 + 3).max(1);
            let selected = picker.selected.min(results.len().saturating_sub(1));
            let start = selected
                .saturating_sub(visible.saturating_sub(1))
                .min(results.len().saturating_sub(visible));
            for (i, m) in results.iter().enumerate().skip(start).take(visible) {
                let is_selected = i == selected;
                let marker = if is_selected { "▶ " } else { "  " };
                let style = if is_selected { panels::row_highlight() } else { Style::new().fg(Theme::VALUE) };
                lines.push(Line::from(vec![
                    Span::styled(format!("{marker}{:<48}", panels::truncate(&m.name, 48)), style),
                    Span::styled(format!("NORAD {}", m.norad_id), style),
                ]));
            }
        }
        SearchState::Failed { msg, .. } => {
            lines.push(Line::from(Span::styled(format!("  {msg}"), Style::new().fg(Theme::ALERT))));
        }
    }

    // `trim: false`: the two leading columns on every result row are the
    // `▶ `/`  ` marker gutter, and a trimming wrapper strips leading
    // whitespace off *every* line, selected or not (ratatui-widgets'
    // `WordWrapper`), which is what made the name column jump sideways under
    // the selection. See the `HighlightSpacing` comment in
    // `ui/panels/passes.rs` for the same reasoning applied to `List`.
    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: false }),
        popup,
    );
}

/// The `T` picker: choose which SatNOGS downlink drives the DOPP row. Shares
/// only `centered`, `Clear` and the focus-frame block with `sat_input_popup`;
/// the scroll-window arithmetic below is a deliberate second copy, because the
/// two lists differ in what their rows show and where they come from
/// (ui/mod.rs already keeps `time_input_popup` separate for the same reason).
fn transmitter_popup(
    frame: &mut Frame,
    area: Rect,
    picker: &crate::app::TxPicker,
    lookup: &TransmitterState,
    sat: u64,
    active: Option<&crate::config::Transmitter>,
) {
    let popup = centered(area, 78, 13);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_style(Style::new().fg(Theme::FRAME_FOCUS))
        .title(format!(" downlink frequency — NORAD {sat} "));
    let dim = |s: String| Line::from(Span::styled(s, Style::new().fg(Theme::LABEL)));
    let alert = |s: String| Line::from(Span::styled(s, Style::new().fg(Theme::ALERT)));
    let mut lines = vec![Line::from("")];

    match lookup {
        TransmitterState::Idle | TransmitterState::Busy { .. } => {
            lines.push(dim("  looking up transmitters…".to_string()));
        }
        TransmitterState::Failed { msg, .. } => lines.push(alert(format!("  {msg}"))),
        TransmitterState::Done { found, .. } if found.is_empty() => {
            // The common answer for a non-amateur payload, so it gets real
            // prose rather than a bare "no results".
            lines.push(alert(format!("  SatNOGS has no transmitter on file for NORAD {sat}")));
            lines.push(dim(
                "  add one by hand to transmitters-<norad>.json in the cache directory"
                    .to_string(),
            ));
        }
        TransmitterState::Done { found, .. } => {
            let visible = (popup.height as usize).saturating_sub(2 + 1).max(1);
            let selected = picker.selected.min(found.len().saturating_sub(1));
            let start = selected
                .saturating_sub(visible.saturating_sub(1))
                .min(found.len().saturating_sub(visible));
            for (i, t) in found.iter().enumerate().skip(start).take(visible) {
                let is_selected = i == selected;
                // `●` marks the one already driving the DOPP row — the same
                // pairing of `▶`/`●` the TRACKED panel uses for its list.
                let sel = if is_selected { "▶ " } else { "  " };
                let act = if active == Some(t) { "●" } else { " " };
                let style = if is_selected {
                    panels::row_highlight()
                } else {
                    Style::new().fg(Theme::VALUE)
                };
                let mode = if t.mode.is_empty() { "—" } else { t.mode.as_str() };
                lines.push(Line::from(Span::styled(
                    format!(
                        "{sel}{act} {:>9.3} MHz  {:<6} {}",
                        t.downlink_hz as f64 / 1e6,
                        mode,
                        panels::truncate(&t.description, 44),
                    ),
                    style,
                )));
            }
        }
    }

    // `trim: false` for the same reason as `sat_input_popup`: the leading
    // `{sel}{act} ` gutter is load-bearing whitespace, not padding, and a
    // trimming wrapper strips it off every unselected row.
    frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), popup);
}

/// The `g` prompt: a small centred box that takes a time or an offset. Shares
/// `centered` and the focus-frame idiom with `sat_input_popup`, but it is one
/// input line and one hint line — nothing like the picker's result list — so it
/// is its own function rather than a reuse of that one.
fn time_input_popup(frame: &mut Frame, area: Rect, input: &crate::app::TimeInput) {
    let popup = centered(area, 46, 6);
    frame.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_style(Style::new().fg(Theme::FRAME_FOCUS))
        .title(" go to time ");
    let hint = match &input.error {
        Some(msg) => Line::from(Span::styled(format!("  {msg}"), Style::new().fg(Theme::ALERT))),
        None => Line::from(Span::styled(
            "  UTC · also +90m, -2h, +3d, or 04:30",
            Style::new().fg(Theme::LABEL),
        )),
    };
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("  time: ", Style::new().fg(Theme::LABEL)),
            Span::styled(
                format!("{}▏", input.buffer),
                Style::new().fg(Theme::VALUE).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        hint,
    ];
    // `trim: false` to match `sat_input_popup` and `transmitter_popup`: this
    // popup has no list to jump, but a trimming wrapper would still strip its
    // hand-indented "  " off the label and hint lines, so all three popups
    // agree on a two-column indent instead of disagreeing by two columns.
    frame.render_widget(Paragraph::new(lines).block(block).wrap(Wrap { trim: false }), popup);
}

/// A rectangle of the given size, centred inside `area`.
pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The narrowest terminal nadir will draw at is 80 columns, and the feed
    /// chips eat the left ~41 of the status bar — so this is roughly the worst
    /// case the hint has to survive.
    const NARROWEST_HINT_AREA: u16 = 80 - 41;

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

    /// The row count `draw_boot_splash` builds — wordmark, version, a blank,
    /// every `boot_lines` row, then a blank and the footer — must still fit
    /// comfortably under the 80×24 minimum terminal size even with the two
    /// rows this round added to `boot_lines`.
    #[test]
    fn the_boot_splash_height_fits_the_minimum_terminal_size() {
        let rows = NADIR_WORDMARK.len() + 2 + boot_lines(None, None, 25544, false).len() + 2;
        assert!(rows <= 24, "boot splash is {rows} rows tall, taller than the 24-row minimum");
    }

    #[test]
    fn splash_reveal_count_starts_at_one_and_reaches_every_row_by_a_third_of_the_splash_duration() {
        let splash = std::time::Duration::from_secs(6);
        assert_eq!(splash_reveal_count(std::time::Duration::ZERO, splash, 6), 1);
        assert_eq!(splash_reveal_count(splash / 3, splash, 6), 6);
    }

    /// The reveal must not creep past the one-third point — the remaining
    /// two-thirds are meant to be a static hold, not more of the animation.
    #[test]
    fn splash_reveal_count_holds_at_every_row_through_the_remaining_two_thirds() {
        let splash = std::time::Duration::from_secs(6);
        assert_eq!(splash_reveal_count(splash / 3 + std::time::Duration::from_millis(1), splash, 6), 6);
        assert_eq!(splash_reveal_count(splash, splash, 6), 6);
        // Even a key press that lands right on the boundary, or a frame that
        // ticks a little past `splash` before `splash_active` catches up,
        // must not panic or overshoot `total`.
        assert_eq!(splash_reveal_count(splash * 2, splash, 6), 6);
    }

    #[test]
    fn splash_reveal_count_climbs_partway_through_the_first_third() {
        let splash = std::time::Duration::from_secs(6);
        let count = splash_reveal_count(std::time::Duration::from_secs(1), splash, 6);
        assert!((1..6).contains(&count), "expected a partial reveal, got {count}/6");
    }

    #[test]
    fn splash_reveal_count_is_zero_for_an_empty_line_list() {
        assert_eq!(splash_reveal_count(std::time::Duration::ZERO, std::time::Duration::from_secs(4), 0), 0);
    }

    /// The whole point: `boot_text` must return the same number of lines
    /// whether a row has revealed yet or not, so the box `draw_boot_splash`
    /// sizes from it never resizes as rows fill in — only line *content*
    /// changes, never line *count*.
    #[test]
    fn boot_text_has_the_same_line_count_at_every_reveal_step() {
        let lines = boot_lines(None, None, 25544, false);
        let full_len = boot_text(&lines, lines.len()).len();
        for revealed in 0..=lines.len() {
            assert_eq!(boot_text(&lines, revealed).len(), full_len, "revealed={revealed}");
        }
    }

    /// An unrevealed row is a blank placeholder, not its label text — so a
    /// row's content only appears once `revealed` reaches it.
    #[test]
    fn boot_text_shows_only_the_revealed_rows_content() {
        let lines = boot_lines(None, None, 25544, false);
        let text = boot_text(&lines, 1);
        let rendered: String = text.iter().flat_map(|l| l.spans.iter()).map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains("ground station"), "the first row should show: {rendered}");
        assert!(!rendered.contains("element set"), "the second row should still be blank: {rendered}");
    }

    #[test]
    fn the_aurora_oval_is_drawn_only_when_the_clock_reads_real_time() {
        assert!(aurora_visible(true, ClockState::Live));
        // Every way of leaving real time hides it, not just a warp: a step or
        // a `g` jump leaves the clock Drifted and a pause freezes it, and the
        // nowcast describes none of those instants.
        for off in [ClockState::Drifted, ClockState::Paused, ClockState::Warp(2), ClockState::Warp(-1800)] {
            assert!(!aurora_visible(true, off), "{off:?}");
        }
        // …and the `a` toggle still wins on its own.
        assert!(!aurora_visible(false, ClockState::Live));
    }

    #[test]
    fn clock_offset_shows_the_two_most_significant_units_signed() {
        let d = ChronoDuration::seconds;
        assert_eq!(fmt_clock_offset(d(60)), "Δ+1m");
        assert_eq!(fmt_clock_offset(d(45)), "Δ+45s");
        assert_eq!(fmt_clock_offset(d(-90)), "Δ-1m30s");
        assert_eq!(fmt_clock_offset(d(-(2 * 3600 + 14 * 60 + 30))), "Δ-2h14m");
        assert_eq!(fmt_clock_offset(d(3 * 3600)), "Δ+3h");
        assert_eq!(fmt_clock_offset(d(3 * 86_400)), "Δ+3d");
        assert_eq!(fmt_clock_offset(d(3 * 86_400 + 5 * 3600)), "Δ+3d5h");
        assert_eq!(fmt_clock_offset(d(40 * 86_400)), "Δ+40d");
    }

    #[test]
    fn a_sub_second_clock_offset_renders_nothing() {
        assert_eq!(fmt_clock_offset(ChronoDuration::zero()), "");
        assert_eq!(fmt_clock_offset(ChronoDuration::milliseconds(400)), "");
    }

    #[test]
    fn tracked_hints_name_enter_and_d_at_every_width_that_shows_a_hint() {
        let tiers = key_hints(Panel::Tracked, false);
        for width in NARROWEST_HINT_AREA..=200 {
            let Some(keys) = fitting_hint(&tiers, width) else { continue };
            if keys == "? help" {
                continue;
            }
            assert!(keys.contains("Enter track"), "width {width}: {keys}");
            assert!(keys.contains("d remove"), "width {width}: {keys}");
        }
    }

    #[test]
    fn telemetry_hints_the_downlink_keys_at_every_width_that_shows_a_hint() {
        let tiers = key_hints(Panel::Telemetry, false);
        for width in NARROWEST_HINT_AREA..=200 {
            let Some(keys) = fitting_hint(&tiers, width) else { continue };
            if keys == "? help" {
                continue;
            }
            assert!(keys.contains("t/T downlink"), "width {width}: {keys}");
        }
    }

    #[test]
    fn map_keys_are_hinted_only_while_the_map_holds_focus() {
        for focus in Panel::ALL {
            let widest = &key_hints(focus, false)[0];
            assert_eq!(
                widest.contains("m map"),
                focus == Panel::Map,
                "{focus:?}: {widest}",
            );
        }
    }

    #[test]
    fn a_fullscreen_map_hints_the_map_keys_whatever_holds_focus() {
        // `m` toggles fullscreen from any panel, so once the map fills the
        // screen its keys are the relevant ones no matter what has focus.
        let widest = &key_hints(Panel::Launches, true)[0];
        assert!(widest.contains("m map"), "{widest}");
        assert!(widest.contains("+/- zoom"), "{widest}");
    }

    #[test]
    fn the_hint_bar_leaves_panel_digits_and_tab_to_the_panel_headers() {
        // The whole point of the focus-scoped bar: `1`–`6` and `Tab` are shown
        // permanently in the panel headers, so repeating them here only crowds
        // out the keys that are advertised nowhere else.
        for fullscreen in [false, true] {
            for focus in Panel::ALL {
                for tier in key_hints(focus, fullscreen) {
                    assert!(!tier.contains("1-6"), "{focus:?} fs={fullscreen}: {tier}");
                    assert!(!tier.contains("Tab"), "{focus:?} fs={fullscreen}: {tier}");
                }
            }
        }
    }

    #[test]
    fn every_hint_tier_fits_the_width_it_was_chosen_for() {
        for fullscreen in [false, true] {
            for focus in Panel::ALL {
                let tiers = key_hints(focus, fullscreen);
                for width in 0..=200u16 {
                    let Some(keys) = fitting_hint(&tiers, width) else { continue };
                    assert!(
                        keys.chars().count() < width as usize,
                        "{focus:?} fs={fullscreen} at width {width} chose a {}-column hint: {keys}",
                        keys.chars().count(),
                    );
                }
            }
        }
    }

    /// Byte length would put the widest tier ten columns over its true size —
    /// every ` · ` separator is a two-byte character one column wide.
    #[test]
    fn hint_tiers_are_measured_in_columns_not_bytes() {
        let widest = key_hints(Panel::Tracked, false).remove(0);
        let columns = widest.chars().count();
        // Exactly wide enough for the hint and the trailing space, and no
        // wider — a byte-length check would reject this and show nothing.
        let just_fits = columns as u16 + 2;
        assert!(widest.len() + 1 >= just_fits as usize, "no multi-byte chars left to catch");
        assert_eq!(fitting_hint(std::slice::from_ref(&widest), just_fits), Some(&widest[..]));
    }

    #[test]
    fn hint_tiers_get_shorter_and_always_offer_help() {
        for fullscreen in [false, true] {
            for focus in Panel::ALL {
                let tiers = key_hints(focus, fullscreen);
                for pair in tiers.windows(2) {
                    assert!(
                        pair[0].chars().count() > pair[1].chars().count(),
                        "{focus:?} fs={fullscreen}: {:?} is not wider than {:?}",
                        pair[0],
                        pair[1],
                    );
                }
                assert!(tiers.iter().all(|t| t.contains("? help")), "{focus:?} fs={fullscreen}");
            }
        }
    }

    #[test]
    fn the_right_column_widens_with_the_terminal_without_starving_the_map() {
        // Pinned points: the 80-column minimum and just above it stay at the
        // old fixed 40; the column reaches its 54 cap at 98 and holds there.
        assert_eq!(right_width(80), 40);
        assert_eq!(right_width(84), 40);
        assert_eq!(right_width(90), 46);
        assert_eq!(right_width(98), 54);
        assert_eq!(right_width(200), 54);

        for total in 80..=400u16 {
            let right = right_width(total);
            assert!((40..=54).contains(&right), "total {total}: right {right}");
            // Once the ask no longer collides with the map's `Min` floor
            // (total ≥ 84), the map keeps at least MAP_FLOOR columns.
            if total >= MAP_FLOOR + 40 {
                assert!(
                    total - right >= MAP_FLOOR,
                    "total {total}: right {right} leaves the map {}",
                    total - right,
                );
            }
        }
    }

    #[test]
    fn the_right_column_never_budgets_more_rows_than_it_has() {
        for total in 0..=60u16 {
            for tracked_rows in 3..=7u16 {
                for telem_h in 11..=12u16 {
                    let (tracked_h, telem_h) = right_column_heights(total, tracked_rows, telem_h);
                    assert!(
                        tracked_h + telem_h <= total,
                        "total {total}, tracked_rows {tracked_rows}, telem_h {telem_h}: \
                         got ({tracked_h}, {telem_h})",
                    );
                }
            }
        }
    }

    #[test]
    fn next_passes_gives_up_its_rows_before_telemetry_does() {
        // Roomy terminal: both panels get their full ask, and NEXT PASSES
        // takes what's left (checked by the caller's `Min(0)`, not here).
        assert_eq!(right_column_heights(40, 7, 12), (7, 12));

        // The 80x24 minimum: `main` is 14 rows tall in the right column
        // there. NEXT PASSES has already given up everything it has, so
        // TRACKED is at its three-row floor and TELEMETRY absorbs the rest
        // of the shortfall rather than the layout solver picking for it.
        let (tracked_h, telem_h) = right_column_heights(14, 7, 12);
        assert_eq!(tracked_h, 3);
        assert_eq!(telem_h, 11);
    }
}
