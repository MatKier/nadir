//! The SPACE WEATHER panel: NOAA's K-index, solar wind, storm scales, the
//! OVATION aurora nowcast, and the Moon's phase and elevation.

use chrono::{DateTime, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, AppData, Panel};
use crate::orbit::{moon_look_angles, moon_phase};
use crate::ui::panels::fmt::{dim, dim_span, footer, label};
use crate::ui::{is_focused, panel_block, Theme};

pub fn draw(frame: &mut Frame, area: Rect, app: &App, data: &AppData, now: DateTime<Utc>) {
    let mut block = panel_block(Panel::Weather, "SPACE WEATHER", is_focused(app, Panel::Weather));
    let age = data.weather.stale_age();
    block = footer(
        block,
        age.map(|a| vec!["space weather".to_string(), format!("{} old", crate::source::fmt_age(a))])
            .unwrap_or_default(),
    );

    let Some(wx) = data.weather.get() else {
        frame.render_widget(
            Paragraph::new(dim("  contacting NOAA SWPC…")).block(block),
            area,
        );
        return;
    };

    let mut rows: Vec<Line> = Vec::new();

    let kp_vals: Vec<f64> = wx.kp.iter().rev().take(24).rev().map(|p| p.kp).collect();
    let latest_kp = wx.latest_kp().unwrap_or(0.0);
    let kp_color = kp_severity_color(latest_kp);
    rows.push(Line::from(vec![
        label("Kp"),
        Span::styled(format!("{latest_kp:>4.1}  "), Style::new().fg(kp_color).add_modifier(Modifier::BOLD)),
        Span::styled(sparkline(&kp_vals, 9.0), Style::new().fg(kp_color)),
    ]));

    let wind = wx
        .wind_speed_kms
        .map(|s| format!("{s:>4.0} km/s"))
        .unwrap_or_else(|| "   —".to_string());
    let bt = wx.bt_nt.map(|b| format!("{b:.1}")).unwrap_or_else(|| "—".to_string());
    let bz = wx.bz_nt.map(|b| format!("{b:+.1}")).unwrap_or_else(|| "—".to_string());
    let bz_color = match wx.bz_nt {
        Some(b) if b <= -10.0 => Theme::ALERT,
        Some(b) if b <= -5.0 => Theme::CAUTION,
        _ => Theme::VALUE,
    };
    rows.push(Line::from(vec![
        label("WIND"),
        Span::styled(format!("{wind}   "), Style::new().fg(Theme::VALUE)),
        Span::styled("Bt ", Style::new().fg(Theme::LABEL)),
        Span::styled(format!("{bt}  "), Style::new().fg(Theme::VALUE)),
        Span::styled("Bz ", Style::new().fg(Theme::LABEL)),
        Span::styled(format!("{bz} nT"), Style::new().fg(bz_color)),
    ]));

    rows.push(Line::from(vec![
        label("STORM"),
        scale_span("R", wx.scales.r.level()),
        Span::raw(" "),
        scale_span("S", wx.scales.s.level()),
        Span::raw(" "),
        scale_span("G", wx.scales.g.level()),
    ]));

    let aurora = match (app.config.ground_station(), data.aurora.get()) {
        (Some(g), Some(grid)) => grid
            .probability_at(g.lat_deg, g.lon_deg)
            .map(|p| (p, g.lat_deg)),
        _ => None,
    };
    match aurora {
        Some((prob, lat)) => {
            let c = if prob >= 25 { Theme::NOMINAL } else { Theme::LABEL };
            rows.push(Line::from(vec![
                label("AUR"),
                Span::styled(format!("{prob:>3}% "), Style::new().fg(c).add_modifier(Modifier::BOLD)),
                Span::styled(format!("overhead at {lat:.0}°"), Style::new().fg(Theme::LABEL)),
            ]));
        }
        None => rows.push(Line::from(vec![
            label("AUR"),
            dim_span("nowcast pending"),
        ])),
    }

    // The Moon is local math, not a feed — unlike every row above it, it
    // never depends on `wx` and so cannot go stale or fail. It sits under AUR
    // because both rows answer "what's the sky doing", and phase is what
    // decides whether a `★` naked-eye pass in NEXT PASSES is actually worth
    // walking outside for.
    let phase = moon_phase(now);
    let look = app.config.ground_station().map(|g| moon_look_angles(&g, now));
    rows.push(moon_row(phase, look));

    frame.render_widget(Paragraph::new(rows).block(block), area);
}

/// The `MOON` row: phase glyph, phase name and illuminated percentage, plus
/// elevation once `look` is given and the Moon is currently above the
/// horizon — below it, `el` would only invite the question of how far below,
/// which nothing here answers, so it's left off entirely rather than shown
/// as a confusing negative. `look` is `None` with no ground station
/// configured, the same as every other row that needs one.
fn moon_row(phase: crate::orbit::MoonPhase, look: Option<crate::geo::LookAngles>) -> Line<'static> {
    let mut spans = vec![
        label("MOON"),
        Span::styled(format!("{} ", phase.glyph()), Style::new().fg(Theme::ACCENT)),
        Span::styled(
            format!("{} {:.0}%", phase.name(), phase.illuminated * 100.0),
            Style::new().fg(Theme::VALUE),
        ),
    ];
    if let Some(la) = look {
        if la.elevation_deg >= 0.0 {
            spans.push(Span::styled(
                format!(" el {:.0}°", la.elevation_deg),
                Style::new().fg(Theme::NOMINAL),
            ));
        }
    }
    Line::from(spans)
}

fn kp_severity_color(kp: f64) -> Color {
    if kp >= 5.0 {
        Theme::ALERT
    } else if kp >= 4.0 {
        Theme::CAUTION
    } else {
        Theme::NOMINAL
    }
}

fn scale_span(letter: &str, level: u8) -> Span<'static> {
    let color = match level {
        0 => Theme::NOMINAL,
        1 | 2 => Theme::CAUTION,
        _ => Theme::ALERT,
    };
    Span::styled(format!("{letter}{level}"), Style::new().fg(color).add_modifier(Modifier::BOLD))
}

fn sparkline(values: &[f64], max: f64) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() {
        return String::new();
    }
    values
        .iter()
        .map(|&v| {
            let frac = (v / max).clamp(0.0, 1.0);
            BARS[((frac * (BARS.len() - 1) as f64).round() as usize).min(BARS.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orbit::MoonPhase;

    fn line_width(l: &Line) -> usize {
        l.spans.iter().map(|s| s.content.chars().count()).sum()
    }

    /// The weather column is a fixed `Length(40)` in `ui::mod`, never wider or
    /// narrower with the terminal — unlike TELEMETRY's floating right column,
    /// there's no shortest-fit ladder here, so this pins the one thing that
    /// keeps a fixed-width row honest: the worst case (longest phase name,
    /// full percentage, an elevation reading) must still fit the panel's
    /// fixed 38-column inner width.
    #[test]
    fn moon_row_fits_the_weather_panels_fixed_width_at_its_longest() {
        const INNER_WIDTH: usize = 38;
        let longest_name = MoonPhase { illuminated: 0.999, waxing: true, age_days: 3.7 };
        assert_eq!(longest_name.name(), "waxing crescent", "fixture assumption: the longest name");
        let look = crate::geo::LookAngles { azimuth_deg: 180.0, elevation_deg: 90.0, range_km: 0.0 };
        let w = line_width(&moon_row(longest_name, Some(look)));
        assert!(w <= INNER_WIDTH, "moon row is {w} columns, panel only has {INNER_WIDTH}");
    }

    #[test]
    fn moon_row_omits_elevation_below_the_horizon_and_with_no_station() {
        let phase = MoonPhase { illuminated: 0.5, waxing: true, age_days: 7.4 };
        let below = crate::geo::LookAngles { azimuth_deg: 0.0, elevation_deg: -5.0, range_km: 0.0 };
        assert!(!line_width_contains(&moon_row(phase, Some(below)), "el"));
        assert!(!line_width_contains(&moon_row(phase, None), "el"));

        let above = crate::geo::LookAngles { azimuth_deg: 0.0, elevation_deg: 5.0, range_km: 0.0 };
        assert!(line_width_contains(&moon_row(phase, Some(above)), "el"));
    }

    fn line_width_contains(l: &Line, needle: &str) -> bool {
        l.spans.iter().any(|s| s.content.contains(needle))
    }

    #[test]
    fn moon_row_always_names_a_ground_station_free_reading() {
        // The Moon's phase is local math with no ground station at all, so
        // the row must say something even when `look` is `None`.
        let phase = MoonPhase { illuminated: 0.02, waxing: false, age_days: 28.9 };
        let text: String = moon_row(phase, None).spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains(phase.name()));
    }

    #[test]
    fn moon_row_never_exceeds_the_panels_fixed_width_at_any_phase() {
        const INNER_WIDTH: usize = 38;
        for age in 0..30 {
            for waxing in [true, false] {
                let phase = MoonPhase { illuminated: 1.0, waxing, age_days: age as f64 };
                for look in [
                    None,
                    Some(crate::geo::LookAngles { azimuth_deg: 0.0, elevation_deg: 45.0, range_km: 0.0 }),
                ] {
                    let w = line_width(&moon_row(phase, look));
                    assert!(w <= INNER_WIDTH, "age {age} waxing {waxing}: {w} columns");
                }
            }
        }
    }
}
