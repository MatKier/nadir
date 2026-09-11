//! The NEXT PASSES panel: overhead pass predictions for the configured
//! ground station.

use chrono::{DateTime, Duration, FixedOffset, Local, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::orbit::{Confidence, Pass, SatState, Tracker};
use crate::ui::panels::fmt::{compass, dim, row_highlight, split_footer, station_label, DATE_FMT};
use crate::ui::{is_focused, panel_block, Theme, PANEL_CHROME};

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

    // `app.passes` keeps a look-back of already-set passes so a pass under way
    // survives a rebuild; `upcoming_passes` is what belongs on screen, and
    // judging it against this frame's `now` is what drops a pass the moment it
    // sets rather than at the next rebuild.
    let passes = app.upcoming_passes(now);

    // Pass times below are rendered in the local zone, so the title names
    // both the station they're computed for and the UTC offset those local
    // times carry — taken from the passes actually listed (falling back to
    // `now` when there are none), not from wall-clock `Local::now()`: the
    // clock keys can detach the display from today, and a list that straddles
    // a daylight-saving change carries two offsets, not one.
    let title = passes_title(app, area.width, passes, now);
    let block = panel_block(Panel::Passes, &title, focused);

    if passes.is_empty() {
        frame.render_widget(
            Paragraph::new(dim("  no passes peaking above 10° in the next 96 h"))
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    // Reserve the bottom inner row for an accuracy footer, but only when the
    // pass times on screen carry a timing error worth stating — a fresh
    // element set is good to a fraction of a second and gets no footer at all.
    // Because a footer needs the block's *inner* area, the block is drawn on
    // its own here rather than handed to the `List`.
    let footer = pass_accuracy_footer(sat, passes);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (list_area, footer_area) = split_footer(inner, footer.is_some());

    // Less the two columns of the `▶ ` gutter, whether or not the panel is
    // focused: ratatui reserves them only while something is selected
    // (`HighlightSpacing::WhenSelected`, the default), and a rung that changed
    // as focus arrived would shift every row sideways under the cursor.
    let budget = (list_area.width as usize).saturating_sub(2);
    let items: Vec<ListItem> =
        pass_rows(passes, now, budget).into_iter().map(ListItem::new).collect();

    let selected = focused.then(|| app.list_pos.min(passes.len().saturating_sub(1)));
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

/// The date each row leads with, widest first: the full local date, then a
/// year-less `Thu 09-11`, then the bare weekday the rows have always shown.
const DATE_FORMS: [&str; 3] = [DATE_FMT, "%a %m-%d", "%a"];

/// One NEXT PASSES row: day, AOS–LOS in local time, duration, peak elevation
/// and the AOS→LOS compass azimuths. `date_fmt` picks the rung of
/// [`DATE_FORMS`] the leading date is spelled out at.
fn pass_row(p: &Pass, now: DateTime<Utc>, date_fmt: &str) -> Line<'static> {
    let aos = p.aos.with_timezone(&Local);
    let los = p.los.with_timezone(&Local);
    // "Within the hour" includes "happening right now": a pass under way is
    // the most urgent row on screen and must not read as the dimmest. No
    // `p.aos > now` guard is needed to keep an already-set pass out of this —
    // `upcoming_passes` has none to give.
    let soon = p.aos - now < Duration::hours(1);
    let date = aos.format(date_fmt).to_string();

    let mut style = Style::new().fg(Theme::VALUE);
    if soon {
        style = style.fg(Theme::SAT).add_modifier(Modifier::BOLD);
    }

    let star = if p.visible { "★" } else { " " };
    Line::from(vec![
        Span::styled(format!("{star} "), Style::new().fg(Theme::SAT)),
        Span::styled(
            format!(
                "{date} {}–{} {:>2}m {:>2.0}° {}→{}",
                aos.format("%H:%M"),
                los.format("%H:%M"),
                p.duration().num_minutes(),
                p.peak_elevation_deg,
                compass(p.aos_azimuth_deg),
                compass(p.los_azimuth_deg),
            ),
            style,
        ),
    ])
}

/// The pass rows, every one on the widest form from [`DATE_FORMS`] that lets
/// *all* of them fit `budget` columns — the shortest-fit ladder the TELEMETRY
/// rows use (`telemetry::range_row` and friends), but resolved once over the
/// whole list rather than per row: rows that disagreed about how wide their
/// date is would not line up in a column. Measuring the real rows rather than
/// a worst case is deliberate — a list of short bearings (`N→SE`) earns the
/// year a column or two before one with `WNW→WNW` in it. As with every ladder
/// here, the last rung is what's shown once nothing fits; below that width
/// the `List` clips, exactly as it always has.
fn pass_rows(passes: &[Pass], now: DateTime<Utc>, budget: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for fmt in DATE_FORMS {
        rows = passes.iter().map(|p| pass_row(p, now, fmt)).collect();
        if rows.iter().map(Line::width).max().unwrap_or(0) <= budget {
            break;
        }
    }
    rows
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
/// narrow — 40 columns at the 80-column minimum, up to 54 on a wide terminal
/// (`ui::right_width`) — so both are budgeted against what `panel_block`
/// actually leaves for the title rather than assumed to always fit — the
/// offset (short, and the whole point of naming it here) is kept over the
/// name when both can't fit, and the name shrinks to its first component
/// (e.g. "Munich" from "Munich, Bavaria, Germany") before it's dropped
/// entirely. `passes` and `now` are the same slice and instant the rows below
/// are drawn from, so a title claiming `UTC+02:00→+01:00` is always backed by
/// rows that actually carry both offsets.
fn passes_title(app: &App, width: u16, passes: &[Pass], now: DateTime<Utc>) -> String {
    const BASE: &str = "NEXT PASSES";
    let extras_budget = (width as usize).saturating_sub(PANEL_CHROME + BASE.chars().count());

    let offset_at = |t: DateTime<Utc>| *t.with_timezone(&Local).offset();
    let first = offset_at(passes.first().map_or(now, |p| p.aos));
    let last = offset_at(passes.last().map_or(now, |p| p.aos));
    let offset_part = format!(" · {}", offset_label(&first, &last));
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

/// The UTC offset the rows' local times carry, as the title states it: one
/// label when every pass on screen shares an offset, and both sides when a
/// daylight-saving change falls between them (`UTC+02:00→+01:00`, reading as
/// "changes to" — the same arrow the rows use for AOS→LOS bearings).
///
/// Only the first and last pass are consulted, not every pass in between. A
/// zone changes offset twice a year and this list spans at most 96 h, so a
/// change *between* two rows that share an offset would need two transitions
/// inside four days — no zone does that.
fn offset_label(first: &FixedOffset, last: &FixedOffset) -> String {
    if first == last {
        return utc_offset_label(first);
    }
    format!("{}→{}", utc_offset_label(first), offset_digits(last))
}

/// `UTC+02:00` / `UTC-05:00` / `UTC+05:30` (minutes matter — India, Nepal),
/// or plain `UTC` at zero offset.
fn utc_offset_label(offset: &FixedOffset) -> String {
    if offset.local_minus_utc() == 0 {
        return "UTC".to_string();
    }
    format!("UTC{}", offset_digits(offset))
}

/// `+02:00` / `-05:00` / `+05:30` — the offset without its `UTC` prefix, for
/// the far side of an [`offset_label`] transition where the prefix has
/// already been spelled out once. Unlike `utc_offset_label` this always
/// prints digits, even at zero (`UTC+01:00→+00:00`, the UK in autumn) — a bare
/// `→UTC` on the far side would read as a different kind of statement than
/// "the offset became zero".
fn offset_digits(offset: &FixedOffset) -> String {
    let secs = offset.local_minus_utc();
    let sign = if secs < 0 { '-' } else { '+' };
    let secs = secs.unsigned_abs();
    format!("{sign}{:02}:{:02}", secs / 3600, (secs % 3600) / 60)
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

    #[test]
    fn offset_label_states_one_offset_when_the_list_does_not_cross_a_change() {
        let cest = FixedOffset::east_opt(2 * 3600).unwrap();
        assert_eq!(offset_label(&cest, &cest), "UTC+02:00");
    }

    #[test]
    fn offset_label_names_both_sides_of_a_daylight_saving_change() {
        let cest = FixedOffset::east_opt(2 * 3600).unwrap();
        let cet = FixedOffset::east_opt(3600).unwrap();
        assert_eq!(offset_label(&cest, &cet), "UTC+02:00→+01:00");
        // Spring forward reverses the direction, not just the digits.
        assert_eq!(offset_label(&cet, &cest), "UTC+01:00→+02:00");
    }

    /// The far side of a transition prints `+00:00` in digits rather than
    /// falling back to the bare `UTC` `utc_offset_label` uses on its own —
    /// `UTC+01:00→UTC` would read as a different kind of statement than "the
    /// offset became zero".
    #[test]
    fn offset_label_spells_a_zero_far_side_in_digits() {
        let bst = FixedOffset::east_opt(3600).unwrap();
        let gmt = FixedOffset::east_opt(0).unwrap();
        assert_eq!(offset_label(&bst, &gmt), "UTC+01:00→+00:00");
        assert_eq!(offset_label(&gmt, &bst), "UTC→+01:00");
    }

    /// `passes_title` keeps the offset over the station name when both can't
    /// fit (its doc comment states the precedence); this pins the other half
    /// of that promise — that the *widest* offset chip, a full transition
    /// label, still fits inside the narrowest title the panel ever draws.
    /// Computed from `PANEL_CHROME` and the base title rather than hardcoded,
    /// so a future change to either can't silently push the chip off the end.
    #[test]
    fn the_widest_offset_label_still_fits_the_narrowest_panel_title() {
        // "UTC+02:00→+01:00": the longest a transition label gets — every
        // offset here already uses its full "+HH:MM" digits.
        let widest_offset = " · UTC+02:00→+01:00";
        // `ui::right_width` squeezed against the map's floor at the
        // 80-column terminal minimum — the narrowest the right column, and so
        // this title, ever gets (see `right_width`'s doc comment).
        const NARROWEST_RIGHT_COLUMN: usize = 36;
        let budget = NARROWEST_RIGHT_COLUMN - PANEL_CHROME - "NEXT PASSES".chars().count();
        assert!(
            widest_offset.chars().count() <= budget,
            "widest offset chip ({} cols) does not fit the narrowest title budget ({budget} cols)",
            widest_offset.chars().count(),
        );
    }

    fn line_text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// A `Pass` with the given AOS and AOS/LOS azimuths — the two fields that
    /// change how wide a row is, besides its date. Duration and peak elevation
    /// are fixed at values that print at their full padded width (`" 6m"`,
    /// `"30°"`) so they never mask a date-form change.
    fn test_pass(aos: DateTime<Utc>, aos_az: f64, los_az: f64) -> Pass {
        Pass {
            aos,
            los: aos + Duration::minutes(6),
            peak: aos + Duration::minutes(3),
            peak_elevation_deg: 30.0,
            aos_azimuth_deg: aos_az,
            los_azimuth_deg: los_az,
            visible: false,
        }
    }

    #[test]
    fn pass_rows_lead_with_the_full_date_at_the_widest_right_column() {
        use crate::ui::panels::fmt::local_date;
        use chrono::TimeZone;

        let aos1 = Utc.with_ymd_and_hms(2026, 9, 11, 20, 14, 0).unwrap();
        let aos2 = Utc.with_ymd_and_hms(2026, 9, 13, 19, 28, 0).unwrap();
        let passes = vec![test_pass(aos1, 292.5, 292.5), test_pass(aos2, 0.0, 90.0)];
        let now = aos1 - Duration::hours(2);

        // Wide enough for the widest row (worst-case "WNW→WNW" bearings) to
        // still spell out the full date.
        let rows = pass_rows(&passes, now, 60);
        for (row, p) in rows.iter().zip(&passes) {
            let text = line_text(row);
            assert!(text.contains(&local_date(p.aos)), "{text}");
        }
    }

    /// One budget short of what the full date needs drops to the year-less
    /// `%a %m-%d` rung; one short of *that* drops to the bare weekday — the
    /// exact row this panel showed before dates were added.
    #[test]
    fn pass_rows_drop_the_year_and_then_the_date_as_the_column_narrows() {
        use chrono::TimeZone;

        let aos = Utc.with_ymd_and_hms(2026, 9, 11, 20, 14, 0).unwrap();
        // Worst-case bearings, so this row is as wide as any row gets.
        let passes = vec![test_pass(aos, 292.5, 292.5)];
        let now = aos - Duration::hours(2);

        let full = pass_row(&passes[0], now, DATE_FMT);
        let mid = pass_row(&passes[0], now, "%a %m-%d");
        let short = pass_row(&passes[0], now, "%a");

        let rows_mid = pass_rows(&passes, now, full.width() - 1);
        assert_eq!(line_text(&rows_mid[0]), line_text(&mid));

        let rows_short = pass_rows(&passes, now, mid.width() - 1);
        assert_eq!(line_text(&rows_short[0]), line_text(&short));
    }

    /// Mirrors `range_row_never_exceeds_the_width_it_is_given` in
    /// `telemetry.rs`: whatever budget the panel is drawn at, no row may run
    /// past it. Below the bare-weekday floor a row can't shrink any further —
    /// the `List` clips it there exactly as it always has, which this test
    /// doesn't need to cover.
    #[test]
    fn pass_rows_never_exceed_the_width_they_are_given() {
        use chrono::TimeZone;

        let aos = Utc.with_ymd_and_hms(2026, 9, 11, 20, 14, 0).unwrap();
        let passes =
            vec![test_pass(aos, 292.5, 292.5), test_pass(aos + Duration::days(1), 0.0, 90.0)];
        let now = aos - Duration::hours(2);

        let floor = passes.iter().map(|p| pass_row(p, now, "%a").width()).max().unwrap();
        for budget in floor..=60 {
            for row in pass_rows(&passes, now, budget) {
                assert!(row.width() <= budget, "budget {budget}: width {}", row.width());
            }
        }
    }

    /// The ladder is resolved once over the whole list, not per row: a list
    /// with one row that could afford the full date and one that can't must
    /// still agree on a single rung, or the date column wouldn't line up.
    #[test]
    fn every_pass_row_shares_one_date_form_so_the_columns_line_up() {
        use chrono::TimeZone;

        let aos = Utc.with_ymd_and_hms(2026, 9, 11, 20, 14, 0).unwrap();
        let wide = test_pass(aos, 292.5, 292.5); // "WNW→WNW"
        let narrow = test_pass(aos + Duration::days(1), 0.0, 90.0); // "N→E"
        let now = aos - Duration::hours(2);

        let narrow_full_w = pass_row(&narrow, now, DATE_FMT).width();
        let wide_full_w = pass_row(&wide, now, DATE_FMT).width();
        assert!(narrow_full_w < wide_full_w, "fixture assumption: azimuths differ in width");

        // Fits the narrow row's full date exactly, but not the wide row's.
        let budget = narrow_full_w;
        let rows = pass_rows(&[wide.clone(), narrow.clone()], now, budget);

        // Both rows fall back to the same rung — the year-less mid form —
        // rather than the narrow row keeping its full date alone.
        assert_eq!(line_text(&rows[0]), line_text(&pass_row(&wide, now, "%a %m-%d")));
        assert_eq!(line_text(&rows[1]), line_text(&pass_row(&narrow, now, "%a %m-%d")));
    }
}
