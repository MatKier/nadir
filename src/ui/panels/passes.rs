//! The NEXT PASSES panel: overhead pass predictions for the configured
//! ground station.

use chrono::{DateTime, Duration, FixedOffset, Local, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::orbit::{Confidence, Pass, SatState, Tracker};
use crate::ui::panels::fmt::{dim, row_highlight, station_label};
use crate::ui::{is_focused, panel_block, Theme};

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sat: Option<&(Tracker, SatState)>,
    now: DateTime<Utc>,
) {
    let focused = is_focused(app, Panel::Passes);

    if app.config.ground_station().is_none() {
        let block = panel_block(Panel::Passes, "NEXT PASSES", focused);
        let p = Paragraph::new(vec![
            dim("  No ground station set."),
            dim("  Pass --location \"your city\", or"),
            dim("  --lat/--lon, to see overhead passes."),
        ])
        .block(block)
        .wrap(Wrap { trim: true });
        frame.render_widget(p, area);
        return;
    }

    // Pass times below are rendered in the local zone, so the title names
    // both the station they're computed for and the UTC offset those local
    // times carry.
    let title = passes_title(app, area.width);
    let block = panel_block(Panel::Passes, &title, focused);

    if app.passes.is_empty() {
        frame.render_widget(
            Paragraph::new(dim("  no passes above 10° in the next 48 h"))
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .passes
        .iter()
        .map(|p| {
            let aos = p.aos.with_timezone(&Local);
            let los = p.los.with_timezone(&Local);
            let soon = p.aos - now < Duration::hours(1) && p.aos > now;
            let day = aos.format("%a").to_string();

            let mut style = Style::new().fg(Theme::VALUE);
            if soon {
                style = style.fg(Theme::SAT).add_modifier(Modifier::BOLD);
            }

            let star = if p.visible { "★" } else { " " };
            let line = Line::from(vec![
                Span::styled(format!("{star} "), Style::new().fg(Theme::SAT)),
                Span::styled(
                    format!(
                        "{day} {}–{} {:>2}m {:>2.0}° {}→{}",
                        aos.format("%H:%M"),
                        los.format("%H:%M"),
                        p.duration().num_minutes(),
                        p.peak_elevation_deg,
                        compass(p.aos_azimuth_deg),
                        compass(p.los_azimuth_deg),
                    ),
                    style,
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    // Reserve the bottom inner row for an accuracy footer, but only when the
    // pass times on screen carry a timing error worth stating — a fresh
    // element set is good to a fraction of a second and gets no footer at all.
    // Because a footer needs the block's *inner* area, the block is drawn on
    // its own here rather than handed to the `List`.
    let footer = pass_accuracy_footer(sat, &app.passes);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (list_area, footer_area) = match footer {
        Some(_) => {
            let [l, f] =
                Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
            (l, Some(f))
        }
        None => (inner, None),
    };

    let selected = focused.then(|| app.list_pos.min(app.passes.len().saturating_sub(1)));
    let list = List::new(items)
        .highlight_style(row_highlight())
        .highlight_symbol("▶ ");
    frame.render_stateful_widget(
        list,
        list_area,
        &mut ListState::default().with_selected(selected),
    );

    if let (Some((text, color)), Some(fa)) = (footer, footer_area) {
        frame.render_widget(Paragraph::new(Span::styled(text, Style::new().fg(color))), fa);
    }
}

/// A one-line accuracy note for the pass list, or `None` when the modelled
/// timing error is under a second — not worth a row. The figure is the
/// along-track timing error at the *last* pass on screen, so the single number
/// is an upper bound over every row above it rather than right for the first
/// and stale by the last. Same model as the TELEMETRY panel's `ACC` row.
fn pass_accuracy_footer(
    sat: Option<&(Tracker, SatState)>,
    passes: &[Pass],
) -> Option<(String, Color)> {
    let (tr, state) = sat?;
    let last = passes.last()?;
    let acc = tr.accuracy_at(last.aos, state.speed_kms);

    let color = match acc.confidence {
        Confidence::Nominal => Theme::LABEL,
        Confidence::Degraded => Theme::CAUTION,
        Confidence::Unreliable | Confidence::Unmodelled => Theme::ALERT,
    };
    let text = match acc.confidence {
        Confidence::Unmodelled => "  pass times beyond the model".to_string(),
        _ if acc.timing_s < 1.0 => return None,
        _ => format!("  AOS/LOS good to ±{:.0} s", acc.timing_s),
    };
    Some((text, color))
}

/// The NEXT PASSES panel title: the base title, plus the ground station's
/// name and its UTC offset when there's room for them. The right column is
/// only ~40 columns wide, so both are budgeted against what `panel_block`
/// actually leaves for the title rather than assumed to always fit — the
/// offset (short, and the whole point of naming it here) is kept over the
/// name when both can't fit, and the name shrinks to its first component
/// (e.g. "Munich" from "Munich, Bavaria, Germany") before it's dropped
/// entirely.
fn passes_title(app: &App, width: u16) -> String {
    const BASE: &str = "NEXT PASSES";
    // panel_block's fixed overhead around the title text: " {key} " (3
    // cols) + the trailing space after the title (1) + two border columns.
    const CHROME: usize = 6;
    let extras_budget = (width as usize).saturating_sub(CHROME + BASE.chars().count());

    let offset_part = format!(" · {}", utc_offset_label(Local::now().offset()));
    let show_offset = extras_budget >= offset_part.chars().count();
    let remaining = if show_offset {
        extras_budget - offset_part.chars().count()
    } else {
        extras_budget
    };

    let name_part = app
        .config
        .location_name
        .as_deref()
        .and_then(|full| station_label(full, remaining.saturating_sub(3)))
        .map(|n| format!(" · {n}"));

    let mut title = BASE.to_string();
    if let Some(n) = &name_part {
        title.push_str(n);
    }
    if show_offset {
        title.push_str(&offset_part);
    }
    title
}

/// `UTC+02:00` / `UTC-05:00` / `UTC+05:30` (minutes matter — India, Nepal),
/// or plain `UTC` at zero offset.
fn utc_offset_label(offset: &FixedOffset) -> String {
    let secs = offset.local_minus_utc();
    if secs == 0 {
        return "UTC".to_string();
    }
    let sign = if secs < 0 { '-' } else { '+' };
    let secs = secs.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
}

fn compass(deg: f64) -> &'static str {
    const P: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW",
        "NNW",
    ];
    P[(((deg % 360.0) / 22.5).round() as usize) % 16]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_offset_label_zero_is_plain_utc() {
        assert_eq!(utc_offset_label(&FixedOffset::east_opt(0).unwrap()), "UTC");
    }

    #[test]
    fn utc_offset_label_formats_east_and_west() {
        assert_eq!(
            utc_offset_label(&FixedOffset::east_opt(2 * 3600).unwrap()),
            "UTC+02:00"
        );
        assert_eq!(
            utc_offset_label(&FixedOffset::west_opt(5 * 3600).unwrap()),
            "UTC-05:00"
        );
    }

    #[test]
    fn utc_offset_label_keeps_the_minutes_of_a_fractional_offset() {
        assert_eq!(
            utc_offset_label(&FixedOffset::east_opt(5 * 3600 + 30 * 60).unwrap()),
            "UTC+05:30"
        );
    }
}
