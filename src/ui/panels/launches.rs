//! The LAUNCHES panel: upcoming orbital launches from Launch Library 2.

use chrono::{DateTime, Duration, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::api::launches::Origin;
use crate::app::{App, AppData, Panel};
use crate::source::Health;
use crate::ui::panels::fmt::{dim, footer, row_highlight, truncate};
use crate::ui::{is_focused, panel_block, Theme};

/// `wall_now`, not the simulated clock: a launch countdown tracks a real
/// scheduled event, so scrubbing the display time must not move it.
pub fn draw(frame: &mut Frame, area: Rect, app: &App, data: &AppData, wall_now: DateTime<Utc>) {
    let mut block = panel_block(Panel::Launches, "LAUNCHES", is_focused(app, Panel::Launches));
    // Note where the launch list actually came from, when it isn't a live
    // pull from the primary host — the mirror or the disk cache.
    if let Some(launches) = data.launches.get() {
        let mirrored = launches.origin == Origin::Mirror;
        let age = data.launches.stale_age();
        if mirrored || age.is_some() {
            let mut bits = vec!["launch manifest".to_string()];
            if mirrored {
                bits.push("mirror".to_string());
            }
            if let Some(a) = age {
                bits.push(format!("{} old", crate::source::fmt_age(a)));
            }
            block = footer(block, bits);
        }
    }
    let launch_area = block.inner(area);
    frame.render_widget(block, area);

    // A scrollable list when the Launches panel has focus, so a fetched
    // batch bigger than the panel's height is still all reachable.
    let launch_focused = is_focused(app, Panel::Launches);
    match data.launches.get().map(|l| &l.list) {
        Some(list) if !list.is_empty() => {
            // Every row's countdown text, and its color — built first so the
            // column can be sized to this batch's actual widest entry
            // instead of a fixed guess ("T-4d 19:18:45" is wide, "TBD" is
            // not), then reused per row below.
            let countdowns: Vec<(String, Color)> = list
                .iter()
                .map(|l| match l.t_minus(wall_now) {
                    Some(d) if d.num_seconds() >= 0 => (fmt_countdown(d), Theme::ACCENT),
                    Some(_) => ("in flight".to_string(), Theme::CAUTION),
                    None => ("TBD".to_string(), Theme::LABEL),
                })
                .collect();
            const SEP: usize = 2;
            let countdown_w = countdowns.iter().map(|(t, _)| t.chars().count()).max().unwrap_or(0);

            // Budget the row from the panel's actual width instead of fixed
            // literals, so the vehicle name — usually the longest, most
            // useful field — gets whatever room the terminal has instead of
            // being clipped on a narrow one and wasted on a wide one. The
            // provider shrinks first and is dropped entirely below a
            // threshold, since the name is worth more than a clipped
            // provider on a narrow panel.
            let rest = (launch_area.width as usize).saturating_sub(countdown_w + SEP);
            let name_nat = list.iter().map(|l| l.name.chars().count()).max().unwrap_or(0);
            let provider_nat = list.iter().map(|l| l.provider.chars().count()).max().unwrap_or(0);
            let (name_w, provider_w) = launch_columns(rest, name_nat, provider_nat);
            // The highlighted row expands to a full-width name plus a second,
            // dimmer line with provider and pad — it alone can afford that,
            // since only one row pays the cost.
            let full_name_w = (launch_area.width as usize).saturating_sub(countdown_w + SEP);

            let selected = launch_focused.then(|| app.list_pos.min(list.len().saturating_sub(1)));

            let items: Vec<ListItem> = list
                .iter()
                .zip(countdowns.iter())
                .enumerate()
                .map(|(i, (l, (t, tc)))| {
                    let name_color = launch_status_color(&l.status);
                    // Left-aligned: right-aligning shifted the field sideways
                    // whenever a row's width changed between rows (e.g.
                    // "T-19:.." vs "T-4d 19:..").
                    let countdown = Span::styled(format!("{t:<countdown_w$}  "), Style::new().fg(*tc));

                    if Some(i) == selected {
                        let line1 = Line::from(vec![
                            countdown,
                            Span::styled(truncate(&l.name, full_name_w), Style::new().fg(name_color)),
                        ]);
                        let detail = format!("{} · {}", l.provider, l.pad);
                        let indent = " ".repeat(countdown_w + SEP);
                        let line2 = Line::from(Span::styled(
                            format!("{indent}{}", truncate(&detail, full_name_w)),
                            Style::new().fg(Theme::LABEL),
                        ));
                        ListItem::new(vec![line1, line2])
                    } else {
                        let mut spans = vec![
                            countdown,
                            Span::styled(truncate(&l.name, name_w), Style::new().fg(name_color)),
                        ];
                        if provider_w > 0 {
                            spans.push(Span::styled(
                                format!("  {}", truncate(&l.provider, provider_w)),
                                Style::new().fg(Theme::LABEL),
                            ));
                        }
                        ListItem::new(Line::from(spans))
                    }
                })
                .collect();
            let widget = List::new(items).highlight_style(row_highlight());
            frame.render_stateful_widget(
                widget,
                launch_area,
                &mut ListState::default().with_selected(selected),
            );
        }
        Some(_) => {
            frame.render_widget(Paragraph::new(dim("  no upcoming launches listed")), launch_area);
        }
        None => {
            let line = match data.launches.health() {
                Health::Error(reason) if reason.contains("throttled") => {
                    dim("  launch feed throttled — retrying")
                }
                Health::Error(_) => dim("  launch feed unavailable"),
                _ => dim("  launch manifest pending…"),
            };
            frame.render_widget(Paragraph::new(line), launch_area);
        }
    }
}

/// A launch name carries how much the feed actually commits to the row:
/// confirmed launches read at full strength, an unfixed date recedes, and
/// a hold or a failure is flagged. LL2 sends either the abbreviation
/// ("Go", "TBD") or the full name ("Go for Launch", "To Be Determined"),
/// so both spellings are matched; an unrecognised status is treated as
/// unconfirmed rather than promoted to full strength.
fn launch_status_color(status: &str) -> Color {
    match status.trim().to_ascii_lowercase().as_str() {
        "go" | "go for launch" | "success" | "launch successful" | "in flight" => Theme::VALUE,
        "hold" | "on hold" => Theme::CAUTION,
        "failure" | "launch failure" | "partial failure" => Theme::ALERT,
        _ => Theme::LABEL,
    }
}

/// A launch countdown in the conventional form — no space after `T-`:
/// `T-4d 19:18:45` past 24h, `T-19:18:45` under it.
fn fmt_countdown(d: Duration) -> String {
    let s = d.num_seconds().max(0);
    let days = s / 86_400;
    let hh = (s % 86_400) / 3600;
    let mm = (s % 3600) / 60;
    let ss = s % 60;
    if days > 0 {
        format!("T-{days}d {hh:02}:{mm:02}:{ss:02}")
    } else {
        format!("T-{hh:02}:{mm:02}:{ss:02}")
    }
}

/// Width of the name and provider columns for a batch of launch rows, given
/// the room left after the countdown (`rest`) and what the batch's widest
/// name and provider actually need. Both are sized to the batch rather than
/// to a fixed fraction of the row, so nothing truncates while there is
/// space — the wider panel that losing CREW bought should show more text,
/// not more whitespace. The name is served first (it's what identifies a
/// launch), but a `PROVIDER_MIN`-wide column is reserved for the provider
/// before the name is allowed to grow into the whole row; below that
/// reserve a truncated provider would say nothing useful, so it's dropped
/// entirely instead of showing an ellipsis and two letters.
fn launch_columns(rest: usize, name_nat: usize, provider_nat: usize) -> (usize, usize) {
    const GAP: usize = 2;
    const PROVIDER_MIN: usize = 10;
    const NAME_MIN: usize = 20;

    if rest < NAME_MIN + GAP + PROVIDER_MIN {
        return (name_nat.min(rest), 0);
    }

    let name_w = name_nat.min(rest - GAP - PROVIDER_MIN);
    let provider_w = (rest - name_w - GAP).min(provider_nat);
    (name_w, provider_w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_countdown_under_a_day_has_no_day_field() {
        assert_eq!(fmt_countdown(Duration::seconds(19 * 3600 + 18 * 60 + 45)), "T-19:18:45");
    }

    #[test]
    fn fmt_countdown_past_a_day_breaks_out_days() {
        assert_eq!(
            fmt_countdown(Duration::seconds(4 * 86_400 + 19 * 3600 + 18 * 60 + 45)),
            "T-4d 19:18:45"
        );
    }

    #[test]
    fn fmt_countdown_exactly_a_day_is_one_day_zero_hours() {
        assert_eq!(fmt_countdown(Duration::seconds(86_400)), "T-1d 00:00:00");
    }

    #[test]
    fn fmt_countdown_zero_and_negative_clamp_to_zero() {
        assert_eq!(fmt_countdown(Duration::zero()), "T-00:00:00");
        assert_eq!(fmt_countdown(Duration::seconds(-30)), "T-00:00:00");
    }

    #[test]
    fn launch_status_color_matches_abbreviation_and_full_name() {
        assert_eq!(launch_status_color("Go"), Theme::VALUE);
        assert_eq!(launch_status_color("Go for Launch"), Theme::VALUE);
        assert_eq!(launch_status_color("TBD"), Theme::LABEL);
        assert_eq!(launch_status_color("To Be Determined"), Theme::LABEL);
    }

    #[test]
    fn launch_status_color_flags_hold_and_failure() {
        assert_eq!(launch_status_color("Partial Failure"), Theme::ALERT);
        assert_eq!(launch_status_color("On Hold"), Theme::CAUTION);
    }

    #[test]
    fn launch_status_color_unknown_status_falls_back_to_unconfirmed() {
        assert_eq!(launch_status_color("Some New Status"), Theme::LABEL);
    }

    #[test]
    fn launch_status_color_ignores_case_and_stray_whitespace() {
        assert_eq!(launch_status_color(" go for launch "), Theme::VALUE);
    }

    #[test]
    fn launch_columns_gives_both_columns_their_natural_width_when_everything_fits() {
        assert_eq!(launch_columns(100, 39, 22), (39, 22));
    }

    #[test]
    fn launch_columns_gives_the_provider_the_leftover_instead_of_a_fixed_cap() {
        // The live batch that motivated this change: at a 78-column inner
        // width with a 13-wide countdown, `rest` comes out to 63. The old
        // `(rest / 4).clamp(10, 16)` rule would have capped the provider at
        // 16 despite 22 columns being free and useful.
        assert_eq!(launch_columns(63, 39, 50), (39, 22));
    }

    #[test]
    fn launch_columns_drops_the_provider_and_hands_the_name_the_whole_row_when_too_narrow() {
        assert_eq!(launch_columns(25, 50, 50), (25, 0));
    }

    #[test]
    fn launch_columns_never_returns_a_provider_width_below_its_minimum() {
        for rest in 0..80 {
            let (_, provider_w) = launch_columns(rest, 50, 50);
            assert!(
                provider_w == 0 || provider_w >= 10,
                "rest={rest} gave a provider column {provider_w} wide — too narrow to say anything useful"
            );
        }
    }
}
