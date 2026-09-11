//! Formatting and layout helpers shared by every panel in this module: the
//! label/value styling telemetry and weather both use, the row-selection
//! style every scrollable list shares, and the provenance footer weather and
//! launches both hang off `title_bottom`.

use chrono::{DateTime, Local, Utc};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;

use crate::ui::Theme;

/// `t` as a local wall-clock `HH:MM`. The panels show pass times in the
/// observer's own timezone; this is the one place the `Utc → Local → strftime`
/// dance is spelled out.
pub(in crate::ui) fn local_hm(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M").to_string()
}

/// [`local_hm`] to the second — `HH:MM:SS` — for the widest footer tier, where
/// a pass's few minutes are worth pinning down exactly.
pub(in crate::ui) fn local_hms(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%H:%M:%S").to_string()
}

/// The full local date form the NEXT PASSES rows and the sky plot's footer
/// share at their widest.
pub(in crate::ui) const DATE_FMT: &str = "%a %Y-%m-%d";

/// `t` as a local `Thu 2026-09-11` — local like [`local_hm`], and so with no
/// trailing `Z`: the zone is stated once in the panel title instead.
pub(in crate::ui) fn local_date(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format(DATE_FMT).to_string()
}

pub(super) fn label(text: &str) -> Span<'static> {
    Span::styled(format!("  {text:<6}"), Style::new().fg(Theme::LABEL))
}

pub(super) fn kv(key: &str, value: String) -> Line<'static> {
    Line::from(vec![label(key), Span::styled(value, Style::new().fg(Theme::VALUE))])
}

pub(super) fn dim(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), Style::new().fg(Theme::LABEL)))
}

pub(super) fn dim_span(text: &str) -> Span<'static> {
    Span::styled(text.to_string(), Style::new().fg(Theme::LABEL))
}

/// The row-selection style shared by every scrollable list, applied only
/// while its panel holds focus.
pub(in crate::ui) fn row_highlight() -> Style {
    Style::new()
        .bg(Theme::FRAME_FOCUS)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// A dim `title_bottom` label listing `bits`, joined with " · " — the pattern
/// LAUNCHES pioneered for surfacing feed provenance/age without spending a
/// body row on it. `bits` empty means nothing to say (e.g. not stale yet),
/// so the block is returned unchanged.
pub(super) fn footer(block: Block<'_>, bits: Vec<String>) -> Block<'_> {
    if bits.is_empty() {
        return block;
    }
    block.title_bottom(Line::from(Span::styled(
        format!(" {} ", bits.join(" · ")),
        Style::new().fg(Theme::LABEL),
    )))
}

/// The ground-station name to show in a title, sized to `budget` characters:
/// the full label when it fits, else just its first comma-separated
/// component (e.g. "Munich" from "Munich, Bavaria, Germany"), else that
/// component truncated. `None` when `budget` is too small to say anything
/// useful.
pub(in crate::ui) fn station_label(full: &str, budget: usize) -> Option<String> {
    if full.chars().count() <= budget {
        return Some(full.to_string());
    }
    let first = full.split(',').next().unwrap_or(full).trim();
    if first.chars().count() <= budget {
        return Some(first.to_string());
    }
    // `truncate` needs at least one character of budget besides its ellipsis.
    if budget < 2 {
        return None;
    }
    Some(truncate(first, budget))
}

/// The 16-point compass name nearest `deg` (degrees clockwise from north):
/// `"N"`, `"NNE"`, `"NE"`, … Shared by the NEXT PASSES rows and the sky
/// plot's AOS/LOS bearings. `deg` may be any real bearing — `rem_euclid` folds
/// it into `[0, 360)` first, so a negative one names a real point rather than
/// saturating to `"N"` the way a signed `%` and an `as usize` cast would.
pub(in crate::ui) fn compass(deg: f64) -> &'static str {
    const P: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW",
        "NNW",
    ];
    P[((deg.rem_euclid(360.0) / 22.5).round() as usize) % 16]
}

/// Split a panel's inner rect into a body and a one-row footer along its
/// bottom edge, or return the whole rect and `None` when `want_footer` is
/// false. The caller draws the block itself *before* calling this, so it's the
/// block's inner rect being split — a `Block` can't carry a body widget and a
/// separate footer line at the same time. Used by NEXT PASSES (footer only
/// when the pass times carry a stated error) and the sky plot (footer only
/// when the pane is tall enough to spare the row).
pub(in crate::ui) fn split_footer(inner: Rect, want_footer: bool) -> (Rect, Option<Rect>) {
    if !want_footer {
        return (inner, None);
    }
    let [body, footer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    (body, Some(footer))
}

pub(in crate::ui) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn station_label_fits_the_full_string_when_there_is_room() {
        assert_eq!(
            station_label("Munich, Bavaria, Germany", 24),
            Some("Munich, Bavaria, Germany".to_string())
        );
    }

    #[test]
    fn station_label_falls_back_to_the_first_component() {
        assert_eq!(station_label("Munich, Bavaria, Germany", 10), Some("Munich".to_string()));
    }

    #[test]
    fn station_label_truncates_the_first_component_when_still_too_long() {
        assert_eq!(station_label("Springfield, Illinois, USA", 6), Some("Sprin…".to_string()));
    }

    #[test]
    fn station_label_gives_up_below_a_two_char_budget() {
        assert_eq!(station_label("Munich, Bavaria, Germany", 1), None);
        assert_eq!(station_label("Munich, Bavaria, Germany", 0), None);
    }

    #[test]
    fn compass_names_the_sixteen_points_and_wraps_the_full_circle() {
        assert_eq!(compass(0.0), "N");
        assert_eq!(compass(90.0), "E");
        assert_eq!(compass(180.0), "S");
        assert_eq!(compass(270.0), "W");
        // Rounds to the nearest point: 22.5° is the N/NNE boundary, 23° is NNE.
        assert_eq!(compass(23.0), "NNE");
        // 360° folds back to N, not off the end of the table.
        assert_eq!(compass(360.0), "N");
        assert_eq!(compass(359.9), "N");
    }

    #[test]
    fn compass_folds_a_bearing_from_outside_zero_to_three_sixty() {
        // A negative bearing names a real point rather than saturating to "N".
        assert_eq!(compass(-90.0), "W");
        assert_eq!(compass(-30.0), compass(330.0));
        assert_eq!(compass(-30.0), "NNW");
        // And one past a full turn.
        assert_eq!(compass(450.0), "E");
    }
}
