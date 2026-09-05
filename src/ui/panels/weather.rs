//! The SPACE WEATHER panel: NOAA's K-index, solar wind, storm scales and the
//! OVATION aurora nowcast.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, AppData, Panel};
use crate::ui::panels::fmt::{dim, dim_span, footer, label};
use crate::ui::{is_focused, panel_block, Theme};

pub fn draw(frame: &mut Frame, area: Rect, app: &App, data: &AppData) {
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
    let bz = wx.bz_nt.map(|b| format!("{b:+.1}")).unwrap_or_else(|| "—".to_string());
    let bz_color = match wx.bz_nt {
        Some(b) if b <= -10.0 => Theme::ALERT,
        Some(b) if b <= -5.0 => Theme::CAUTION,
        _ => Theme::VALUE,
    };
    rows.push(Line::from(vec![
        label("WIND"),
        Span::styled(format!("{wind}   "), Style::new().fg(Theme::VALUE)),
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

    frame.render_widget(Paragraph::new(rows).block(block), area);
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
