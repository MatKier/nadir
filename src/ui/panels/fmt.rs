//! Formatting and layout helpers shared by every panel in this module: the
//! label/value styling telemetry and weather both use, the row-selection
//! style every scrollable list shares, and the provenance footer weather and
//! launches both hang off `title_bottom`.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;

use crate::ui::Theme;

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
/// `"N"`, `"NNE"`, `"ENE"`, … Shared by the NEXT PASSES rows and the sky
/// plot's AOS/LOS bearings.
pub(in crate::ui) fn compass(deg: f64) -> &'static str {
    const P: [&str; 16] = [
        "N", "NNE", "NE", "ENE", "E", "ESE", "SE", "SSE", "S", "SSW", "SW", "WSW", "W", "WNW", "NW",
        "NNW",
    ];
    P[(((deg % 360.0) / 22.5).round() as usize) % 16]
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
}
