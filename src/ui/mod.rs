//! Rendering. `draw` is the single entry point called once per frame.

mod help;
mod map;
mod panels;
mod places;

/// The tightest follow-mode zoom index, re-exported so `App::zoom_in` can
/// saturate against it without `mod map` being made public.
pub(crate) use map::MAX_ZOOM;

use chrono::Utc;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Panel, SearchState};
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

    let now = Utc::now();
    let data = match app.data.read() {
        Ok(d) => d,
        Err(_) => return,
    };
    let sat_state = data
        .tle
        .get()
        .and_then(|tr| tr.state_at(now).ok().map(|s| (tr.clone(), s)));
    let pad = selected_launch_pad(app, &data);

    if app.map_fullscreen {
        let [title, body, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        title_bar(frame, title, app, &data);
        map::draw(frame, body, app, sat_state.as_ref(), now, pad);
        status_bar(frame, status, app, &data);
    } else {
        let [title, main, bottom, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(10),
            Constraint::Length(8),
            Constraint::Length(1),
        ])
        .areas(area);

        let [map_area, right] =
            Layout::horizontal([Constraint::Min(44), Constraint::Length(40)]).areas(main);

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
        map::draw(frame, map_area, app, sat_state.as_ref(), now, pad);
        panels::tracked::draw(frame, tracked, app);
        panels::telemetry::draw(frame, telem, app, sat_state.as_ref(), now);
        panels::passes::draw(frame, passes, app, now);
        panels::weather::draw(frame, weather, app, &data);
        panels::launches::draw(frame, launches, app, &data, now);
        status_bar(frame, status, app, &data);
    }

    if let Some(picker) = &app.sat_input {
        sat_input_popup(frame, area, picker, &data.search);
    }

    drop(data);
    if app.show_help {
        help::draw(frame, area, app);
    }
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

fn title_bar(frame: &mut Frame, area: Rect, app: &App, data: &crate::app::AppData) {
    let coords = match app.config.ground_station() {
        Some(g) => format!("{:.3},{:.3}", g.lat_deg, g.lon_deg),
        None => "no ground station".to_string(),
    };
    let up = format!("  up {}  ", crate::source::fmt_age(app.uptime()));
    let clock = Utc::now().format("%Y-%m-%d %H:%M:%SZ").to_string();

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
    let right_fixed_len = coords.chars().count() + up.chars().count() + clock.chars().count() + 1;

    // The COSPAR id is the first thing to go when the bar is tight: it is the
    // least-used of the three identifiers, and — same overlapping-paragraph
    // reason GAP exists — a left half that outgrows its share is silently
    // overwritten by the right-aligned one instead of wrapping, so it has to
    // be measured against the fixed right side before it's added rather than
    // trimmed after the fact.
    let sat = match data.tle.get() {
        Some(t) => {
            let base = format!("{} · NORAD {}", t.name(), t.norad_id());
            match t.international_designator() {
                Some(id)
                    if PREFIX.chars().count() + base.chars().count() + " · ".chars().count()
                        + id.chars().count()
                        + right_fixed_len
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

    let left = Line::from(vec![
        Span::styled(" nadir ", Style::new().fg(Color::Black).bg(Theme::ACCENT).bold()),
        Span::raw(" "),
        Span::styled(sat, Style::new().fg(Theme::VALUE).add_modifier(Modifier::BOLD)),
    ]);
    let right = Line::from(vec![
        Span::styled(loc, Style::new().fg(Theme::LABEL)),
        Span::styled(up, Style::new().fg(Theme::LABEL)),
        Span::styled(clock, Style::new().fg(Theme::ACCENT)),
        Span::raw(" "),
    ]);
    frame.render_widget(Paragraph::new(left), area);
    frame.render_widget(Paragraph::new(right).alignment(Alignment::Right), area);
}

fn status_bar(frame: &mut Frame, area: Rect, app: &App, data: &crate::app::AppData) {
    let mut spans = vec![Span::raw(" ")];
    let mut width: u16 = 1;
    for (name, sev, text) in [
        // Each chip's thresholds come from its own feed's interval, so a feed
        // refetching exactly on schedule reads green whether that schedule is
        // five minutes or twelve hours — and amber genuinely means "late".
        chip("TLE", &data.tle, crate::app::TLE_TTL),
        chip("SWX", &data.weather, crate::app::WEATHER_INTERVAL),
        chip("AUR", &data.aurora, crate::app::AURORA_INTERVAL),
        chip("LCH", &data.launches, crate::app::LAUNCHES_INTERVAL),
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
    } else if app.show_help {
        vec!["j/k scroll · ? close".to_string(), "? close".to_string()]
    } else {
        key_hints(app.focus)
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
/// The bottom bar is the only place a panel's own keys are advertised, so they
/// are the last thing dropped as the terminal narrows: everything shed before
/// them is either visible elsewhere (the `1`–`6` digits are in every panel
/// header) or reachable from `?`, which is why `? help` is what survives to
/// the very end.
fn key_hints(focus: Panel) -> Vec<String> {
    let scrollable = matches!(focus, Panel::Tracked | Panel::Passes | Panel::Launches);
    let panel_keys: &[&str] = match focus {
        Panel::Tracked => &["Enter track", "d remove"],
        _ => &[],
    };

    let mut tiers: Vec<String> = [
        &[
            "1-6 focus", "Tab", "m map", "f follow", "p places", "+/- zoom", "s sat", "r refresh",
            "? help", "q quit",
        ][..],
        &["1-6 focus", "m map", "f follow", "r refresh", "? help", "q quit"][..],
        &["r refresh", "? help", "q quit"][..],
        &["? help"][..],
    ]
    .into_iter()
    .map(|globals| {
        let mut parts: Vec<&str> = Vec::new();
        if scrollable {
            parts.push("j/k scroll");
        }
        parts.extend_from_slice(panel_keys);
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
fn fitting_hint(tiers: &[String], width: u16) -> Option<&str> {
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

/// A bordered block whose frame brightens when the panel holds focus. The
/// title leads with `panel`'s own focus key (`1`–`6`), so it's obvious at a
/// glance which key jumps to which panel.
pub fn panel_block(panel: Panel, title: &str, focused: bool) -> Block<'_> {
    let border = if focused { Theme::FRAME_FOCUS } else { Theme::FRAME };
    let title_color = if focused { Theme::FRAME_FOCUS } else { Theme::LABEL };
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

    frame.render_widget(
        Paragraph::new(lines).block(block).wrap(Wrap { trim: true }),
        popup,
    );
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
    fn tracked_hints_name_enter_and_d_at_every_width_that_shows_a_hint() {
        let tiers = key_hints(Panel::Tracked);
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
    fn every_hint_tier_fits_the_width_it_was_chosen_for() {
        for focus in Panel::ALL {
            let tiers = key_hints(focus);
            for width in 0..=200u16 {
                let Some(keys) = fitting_hint(&tiers, width) else { continue };
                assert!(
                    keys.chars().count() < width as usize,
                    "{focus:?} at width {width} chose a {}-column hint: {keys}",
                    keys.chars().count(),
                );
            }
        }
    }

    /// Byte length would put the widest tier ten columns over its true size —
    /// every ` · ` separator is a two-byte character one column wide.
    #[test]
    fn hint_tiers_are_measured_in_columns_not_bytes() {
        let widest = key_hints(Panel::Tracked).remove(0);
        let columns = widest.chars().count();
        // Exactly wide enough for the hint and the trailing space, and no
        // wider — a byte-length check would reject this and show nothing.
        let just_fits = columns as u16 + 2;
        assert!(widest.len() + 1 >= just_fits as usize, "no multi-byte chars left to catch");
        assert_eq!(fitting_hint(std::slice::from_ref(&widest), just_fits), Some(&widest[..]));
    }

    #[test]
    fn hint_tiers_get_shorter_and_always_offer_help() {
        for focus in Panel::ALL {
            let tiers = key_hints(focus);
            for pair in tiers.windows(2) {
                assert!(
                    pair[0].chars().count() > pair[1].chars().count(),
                    "{focus:?}: {:?} is not wider than {:?}",
                    pair[0],
                    pair[1],
                );
            }
            assert!(tiers.iter().all(|t| t.contains("? help")), "{focus:?}");
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
