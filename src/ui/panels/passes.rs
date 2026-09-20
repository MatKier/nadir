//! The NEXT PASSES panel: overhead pass predictions for the configured
//! ground station.

use chrono::{DateTime, Duration, FixedOffset, Local, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, ListItem, Paragraph, Wrap};
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::orbit::{Confidence, Pass, SatState, Tracker};
use crate::simclock::ClockState;
use crate::ui::anim::{lerp, pulse};
use crate::ui::hit::{render_list, RowSpan};
use crate::ui::panels::fmt::{compass, dim, row_highlight, split_footer, station_label, DATE_FMT};
use crate::ui::{is_focused, panel_block, panel_block_styled, Theme, PANEL_CHROME};

/// How long one AOS border pulse takes, in seconds — slow enough to read as
/// a breathing highlight rather than an alarm; the row it accompanies
/// already carries the actual urgency in text.
const AOS_PULSE_PERIOD_SECS: f32 = 2.0;

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sat: Option<&(Tracker, SatState)>,
    now: DateTime<Utc>,
) -> Vec<RowSpan> {
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
        return Vec::new();
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
    let lead = app.aos_lead();
    // In progress wins over merely imminent — a pass already overhead is
    // always the more urgent of the two to reflect in the border, though in
    // practice at most one of them is ever `Some` at once (the two windows
    // meet exactly at `aos`; see `orbit::pass_imminent`'s doc comment).
    let alert = crate::orbit::pass_in_progress(passes, now)
        .or_else(|| crate::orbit::pass_imminent(passes, now, lead));
    let block = match alert {
        Some(p) => aos_block(
            &title,
            focused,
            p.visible,
            app.clock.state(),
            app.uptime().as_secs_f32(),
        ),
        None => panel_block(Panel::Passes, &title, focused),
    };

    if passes.is_empty() {
        frame.render_widget(
            Paragraph::new(dim("  no passes peaking above 10° in the next 96 h"))
                .block(block)
                .wrap(Wrap { trim: true }),
            area,
        );
        return Vec::new();
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
        pass_rows(passes, now, lead, budget).into_iter().map(ListItem::new).collect();

    let selected = focused.then(|| app.list_pos.min(passes.len().saturating_sub(1)));
    // The block was drawn on its own above, so the list gets the inner rect and
    // the scrollbar the outer `area` (its right border, pulsing with the AOS
    // tint, which the bar reads off it).
    let rows = render_list(frame, area, list_area, list_area, items, selected, |list| {
        list.highlight_style(row_highlight()).highlight_symbol("▶ ")
    });

    if let (Some((text, color)), Some(fa)) = (footer, footer_area) {
        frame.render_widget(Paragraph::new(Span::styled(text, Style::new().fg(color))), fa);
    }
    rows
}

/// The panel block while a pass is under way: its border tinted toward
/// `Theme::SAT` for a naked-eye pass or `Theme::NOMINAL` for any other, and —
/// only while `clock` reads real time — breathing between that colour and
/// the ordinary focus/unfocus one. Takes `clock` and `phase` as plain values
/// rather than `&App`, the same choice `ui::aurora_visible` makes and for the
/// same reason: it keeps this testable without constructing an `App`, whose
/// fields are private outside `app`'s own test module.
///
/// The pulse is gated on `ClockState::Live` for the same reason the aurora
/// oval is (`ui::aurora_visible`'s note): a warp or a pause leaves the clock
/// as static on screen as ever, and a pulse riding real wall time would
/// either race far ahead of a slow warp or read as broken during one, rather
/// than the "starting right now" cue it's meant to be. The border still gets
/// the flat, un-pulsed colour in that case — a pass really is under way, and
/// that stays worth marking even when it isn't worth animating.
fn aos_block<'a>(
    title: &'a str,
    focused: bool,
    naked_eye: bool,
    clock: ClockState,
    phase: f32,
) -> Block<'a> {
    let (target, _) = urgency(naked_eye);
    let border = if clock == ClockState::Live {
        let base = if focused { Theme::FRAME_FOCUS } else { Theme::FRAME };
        lerp(base, target, pulse(phase, AOS_PULSE_PERIOD_SECS))
    } else {
        target
    };
    let title_color = if focused { Theme::FRAME_FOCUS } else { Theme::LABEL };
    panel_block_styled(Panel::Passes, title, border, title_color)
}

/// The date each row leads with, widest first: the full local date, then a
/// year-less `Thu 09-11`, then the bare weekday the rows have always shown.
const DATE_FORMS: [&str; 3] = [DATE_FMT, "%a %m-%d", "%a"];

/// One NEXT PASSES row: day, AOS–LOS in local time, duration, peak elevation
/// and the AOS→peak→LOS compass azimuths — the bearing you'd actually point
/// at through the whole pass, not just where it rises and sets. `date_fmt`
/// picks the rung of [`DATE_FORMS`] the leading date is spelled out at;
/// `lead` and `budget` matter only when `p` is close enough to be urgent —
/// see [`aos_row`] and [`aos_imminent_row`].
fn pass_row(p: &Pass, now: DateTime<Utc>, date_fmt: &str, lead: Duration, budget: usize) -> Line<'static> {
    if p.aos <= now && now < p.los {
        return aos_row(p, now, budget);
    }
    if p.aos - lead <= now && now < p.aos {
        return aos_imminent_row(p, now, budget);
    }
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
                "{date} {}–{} {:>2}m {:>2.0}° {}",
                aos.format("%H:%M"),
                los.format("%H:%M"),
                p.duration().num_minutes(),
                p.peak_elevation_deg,
                bearing(p),
            ),
            style,
        ),
    ])
}

/// The AOS→LOS compass bearing for `p`, widened to `AOS→peak→LOS` when the
/// culmination doesn't already read the same 16-point compass name as AOS or
/// LOS — a pass whose culmination doesn't meaningfully add a new direction
/// (a short, low one, typically) stays a plain `SSW→ENE` instead of a
/// redundant `SSW→ENE→ENE`. Once it names a third point, showing it is
/// exactly the case that's worth the extra width: the pass swings wide of a
/// straight line between where it rises and sets. Shared by [`pass_row`]'s
/// ordinary time-range row and [`alert_row`]'s metadata tail, so a pass's
/// bearing reads the same whichever row happens to show it.
fn bearing(p: &Pass) -> String {
    let (aos_c, peak_c, los_c) =
        (compass(p.aos_azimuth_deg), compass(p.peak_azimuth_deg), compass(p.los_azimuth_deg));
    if peak_c == aos_c || peak_c == los_c {
        format!("{aos_c}→{los_c}")
    } else {
        format!("{aos_c}→{peak_c}→{los_c}")
    }
}

/// The colour and star marker for a pass close enough to matter right now —
/// imminent or in progress. Shared by [`aos_row`], [`aos_imminent_row`] and
/// [`aos_block`] so the row text, its leading glyph and the panel border
/// around it always agree on how urgent a given pass is.
fn urgency(naked_eye: bool) -> (Color, &'static str) {
    if naked_eye { (Theme::SAT, "★") } else { (Theme::NOMINAL, " ") }
}

/// The shared body of [`aos_row`] and [`aos_imminent_row`]: the [`urgency`]
/// star and colour, the caller's `head` (the specific countdown text), then
/// the widest metadata tail — duration, peak elevation and [`bearing`] — that
/// still fits `budget` columns. Same shortest-fit ladder idiom [`pass_rows`]
/// uses for the date column and `telemetry::range_row` uses for its own
/// rows, but resolved per row and around a fixed head rather than a fixed
/// date: the head is what makes this row worth showing in the first place
/// and must never itself be dropped, so only the tail narrows, down to
/// nothing. As with every ladder here, the narrowest rung is what's shown
/// once nothing wider fits; below that the `List` clips, exactly as it
/// always has.
fn alert_row(p: &Pass, head: &str, color: Color, star: &str, budget: usize) -> Line<'static> {
    let tails = [
        format!("  {:>2}m {:>2.0}° {}", p.duration().num_minutes(), p.peak_elevation_deg, bearing(p)),
        format!("  {:>2}m {:>2.0}°", p.duration().num_minutes(), p.peak_elevation_deg),
        String::new(),
    ];
    let mut line = Line::default();
    for tail in &tails {
        line = Line::from(vec![
            Span::styled(format!("{star} "), Style::new().fg(Theme::SAT)),
            Span::styled(format!("{head}{tail}"), Style::new().fg(color).add_modifier(Modifier::BOLD)),
        ]);
        if line.width() <= budget {
            break;
        }
    }
    line
}

/// The row for a pass currently under way, in place of `pass_row`'s ordinary
/// AOS–LOS time range. Counts down to whichever of culmination or LOS is
/// still ahead: before the peak, the best-signal moment is what's worth
/// anticipating; after it, how much longer the satellite stays up. `fmt_mmss`
/// already clamps a negative delta to zero, so the frame the clock crosses
/// `p.peak` is safe without a separate guard.
fn aos_row(p: &Pass, now: DateTime<Utc>, budget: usize) -> Line<'static> {
    let (color, star) = urgency(p.visible);
    let head = if now < p.peak {
        format!("▲ AOS  peak in {}", fmt_mmss(p.peak - now))
    } else {
        format!("▲ AOS  LOS in {}", fmt_mmss(p.los - now))
    };
    alert_row(p, &head, color, star, budget)
}

/// The row for a pass rising within `lead` (`App::aos_lead`) — the
/// counterpart to [`aos_row`] for the stretch just *before* AOS: `n`/`N`
/// land the clock exactly at the start of this window (see
/// `App::jump_to_next_pass`), so resuming the clock there starts this
/// countdown immediately rather than leaving the ordinary date/time row up
/// until the pass has already begun.
fn aos_imminent_row(p: &Pass, now: DateTime<Utc>, budget: usize) -> Line<'static> {
    let (color, star) = urgency(p.visible);
    let head = format!("▲ AOS in {}", fmt_mmss(p.aos - now));
    alert_row(p, &head, color, star, budget)
}

/// A plain `Nm SSs` countdown for [`aos_row`] and [`aos_imminent_row`] — a
/// pass runs at most a handful of minutes and `App::aos_lead` is well under
/// one (`config::Ui`'s ceiling is 10 minutes), so unlike
/// `launches::fmt_countdown` this never needs an hours or days field. Clamped
/// to zero rather than going negative: `now` can tick a frame past the target
/// between the check that selected this row and the moment this formats it.
fn fmt_mmss(d: Duration) -> String {
    let s = d.num_seconds().max(0);
    format!("{}m{:02}s", s / 60, s % 60)
}

/// The pass rows, every one on the widest form from [`DATE_FORMS`] that lets
/// *all* of them fit `budget` columns — the shortest-fit ladder the TELEMETRY
/// rows use (`telemetry::range_row` and friends), but resolved once over the
/// whole list rather than per row: rows that disagreed about how wide their
/// date is would not line up in a column. Measuring the real rows rather than
/// a worst case is deliberate — a list of short bearings (`N→SE`) earns the
/// year a column or two before one with `WNW→WNW` in it. As with every ladder
/// here, the last rung is what's shown once nothing fits; below that width
/// the `List` clips, exactly as it always has. `lead` is `App::aos_lead()` —
/// the width of the imminent-countdown window [`pass_row`] switches a row to.
fn pass_rows(passes: &[Pass], now: DateTime<Utc>, lead: Duration, budget: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for fmt in DATE_FORMS {
        rows = passes.iter().map(|p| pass_row(p, now, fmt, lead, budget)).collect();
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

    /// The lead used by every test below that doesn't itself vary it —
    /// `Ui::default().aos_lead`'s value, spelled out here rather than
    /// referencing `config` so these pure-function tests don't need a `Config`
    /// at all.
    const TEST_LEAD: Duration = Duration::seconds(30);
    /// A budget wide enough that no row in this module's fixtures — ordinary
    /// or alert, full metadata tail included — is ever forced to narrow, for
    /// tests that aren't themselves about the ladder.
    const WIDE_BUDGET: usize = 200;

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
            peak_azimuth_deg: (aos_az + los_az) / 2.0,
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
        let rows = pass_rows(&passes, now, TEST_LEAD, 60);
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

        let full = pass_row(&passes[0], now, DATE_FMT, TEST_LEAD, WIDE_BUDGET);
        let mid = pass_row(&passes[0], now, "%a %m-%d", TEST_LEAD, WIDE_BUDGET);
        let short = pass_row(&passes[0], now, "%a", TEST_LEAD, WIDE_BUDGET);

        let rows_mid = pass_rows(&passes, now, TEST_LEAD, full.width() - 1);
        assert_eq!(line_text(&rows_mid[0]), line_text(&mid));

        let rows_short = pass_rows(&passes, now, TEST_LEAD, mid.width() - 1);
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

        let floor =
            passes.iter().map(|p| pass_row(p, now, "%a", TEST_LEAD, WIDE_BUDGET).width()).max().unwrap();
        for budget in floor..=60 {
            for row in pass_rows(&passes, now, TEST_LEAD, budget) {
                assert!(row.width() <= budget, "budget {budget}: width {}", row.width());
            }
        }
    }

    /// A `Pass` with all three azimuths set independently, for the bearing
    /// tests below — `test_pass` above ties `peak` to the midpoint of
    /// `aos`/`los`, which can't exercise the "peak on a straight line"
    /// collapse on its own.
    fn test_pass_with_peak(aos_az: f64, peak_az: f64, los_az: f64) -> Pass {
        use chrono::TimeZone;
        let aos = Utc.with_ymd_and_hms(2026, 9, 11, 20, 14, 0).unwrap();
        Pass {
            aos,
            los: aos + Duration::minutes(6),
            peak: aos + Duration::minutes(3),
            peak_elevation_deg: 30.0,
            aos_azimuth_deg: aos_az,
            los_azimuth_deg: los_az,
            peak_azimuth_deg: peak_az,
            visible: false,
        }
    }

    /// A pass that swings well wide of a straight AOS→LOS line — a high
    /// overhead pass, typically — earns the three-point `AOS→peak→LOS`
    /// bearing.
    #[test]
    fn a_pass_that_swings_wide_of_a_straight_line_shows_its_peak_bearing() {
        let p = test_pass_with_peak(270.0, 0.0, 90.0); // W → N → E
        let row = pass_row(&p, p.aos - Duration::hours(1), "%a", TEST_LEAD, WIDE_BUDGET);
        // Only the bearing is under test here; date and clock time (in the
        // system's local zone, which the test can't pin down) are covered by
        // the other `pass_row`/`pass_rows` tests in this module.
        assert!(line_text(&row).ends_with(" 30° W→N→E"), "{}", line_text(&row));
    }

    /// When the culmination reads the same 16-point compass name as AOS (or
    /// LOS) already does, naming it a second time would be redundant — the
    /// row collapses to the plain two-point `AOS→LOS` it always showed before
    /// `peak_azimuth_deg` existed.
    #[test]
    fn a_pass_whose_peak_reads_the_same_compass_point_as_aos_keeps_the_two_point_bearing() {
        let p = test_pass_with_peak(0.0, 5.0, 90.0); // both N and 5° round to "N"
        let row = pass_row(&p, p.aos - Duration::hours(1), "%a", TEST_LEAD, WIDE_BUDGET);
        assert!(line_text(&row).ends_with(" 30° N→E"), "{}", line_text(&row));
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

        let narrow_full_w = pass_row(&narrow, now, DATE_FMT, TEST_LEAD, WIDE_BUDGET).width();
        let wide_full_w = pass_row(&wide, now, DATE_FMT, TEST_LEAD, WIDE_BUDGET).width();
        assert!(narrow_full_w < wide_full_w, "fixture assumption: azimuths differ in width");

        // Fits the narrow row's full date exactly, but not the wide row's.
        let budget = narrow_full_w;
        let rows = pass_rows(&[wide.clone(), narrow.clone()], now, TEST_LEAD, budget);

        // Both rows fall back to the same rung — the year-less mid form —
        // rather than the narrow row keeping its full date alone.
        assert_eq!(
            line_text(&rows[0]),
            line_text(&pass_row(&wide, now, "%a %m-%d", TEST_LEAD, WIDE_BUDGET))
        );
        assert_eq!(
            line_text(&rows[1]),
            line_text(&pass_row(&narrow, now, "%a %m-%d", TEST_LEAD, WIDE_BUDGET))
        );
    }

    /// Renders `aos_block` into a small buffer and returns the fg colour of
    /// its top-left corner cell — part of the border on any bordered block,
    /// so a stand-in for "what colour is the frame" without a getter on
    /// `Block` itself.
    fn aos_block_border_color(naked_eye: bool, clock: ClockState, phase: f32) -> Color {
        use ratatui::widgets::Widget;
        let rect = Rect::new(0, 0, 20, 5);
        let mut buf = ratatui::buffer::Buffer::empty(rect);
        aos_block("t", false, naked_eye, clock, phase).render(rect, &mut buf);
        buf.content[0].fg
    }

    #[test]
    fn aos_block_is_flat_naked_eye_colour_when_the_clock_is_not_live() {
        assert_eq!(aos_block_border_color(true, ClockState::Paused, 0.0), Theme::SAT);
        assert_eq!(aos_block_border_color(true, ClockState::Warp(5), 1.0), Theme::SAT);
        assert_eq!(aos_block_border_color(true, ClockState::Drifted, 2.0), Theme::SAT);
    }

    #[test]
    fn aos_block_is_flat_ordinary_colour_when_not_naked_eye_and_not_live() {
        assert_eq!(aos_block_border_color(false, ClockState::Paused, 0.0), Theme::NOMINAL);
    }

    #[test]
    fn aos_block_pulses_only_while_the_clock_is_live() {
        // Off the clock, the border must not move at all across phases.
        let still: Vec<Color> =
            (0..10).map(|i| aos_block_border_color(true, ClockState::Paused, i as f32)).collect();
        assert!(still.iter().all(|c| *c == Theme::SAT), "expected a flat colour while paused: {still:?}");

        // Live, sweeping a full pulse period should visit more than one
        // colour — the whole point of the pulse.
        let moving: Vec<Color> = (0..20)
            .map(|i| aos_block_border_color(true, ClockState::Live, i as f32 * AOS_PULSE_PERIOD_SECS / 20.0))
            .collect();
        let distinct = moving.iter().collect::<std::collections::HashSet<_>>().len();
        assert!(distinct > 1, "expected the border to vary while live: {moving:?}");
    }

    #[test]
    fn fmt_mmss_pads_seconds_to_two_digits() {
        assert_eq!(fmt_mmss(Duration::seconds(65)), "1m05s");
        assert_eq!(fmt_mmss(Duration::seconds(5)), "0m05s");
        assert_eq!(fmt_mmss(Duration::seconds(600)), "10m00s");
    }

    #[test]
    fn fmt_mmss_clamps_a_negative_duration_to_zero() {
        assert_eq!(fmt_mmss(Duration::seconds(-5)), "0m00s");
    }

    #[test]
    fn a_pass_under_way_gets_the_live_aos_row_instead_of_its_time_range() {
        let p = test_pass(chrono::Utc::now(), 0.0, 90.0);
        let midpoint = p.aos + (p.los - p.aos) / 2;
        let row = pass_row(&p, midpoint, DATE_FMT, TEST_LEAD, WIDE_BUDGET);
        let text = line_text(&row);
        assert!(text.contains("AOS"), "{text}");
        assert!(text.contains("LOS in"), "{text}");
        // The ordinary row's "AOS–LOS" clock-time range must not also be
        // there — this is the live row, not the scheduled one.
        assert!(!text.contains('–'), "{text}");
    }

    #[test]
    fn a_pass_not_yet_risen_keeps_its_ordinary_time_range_row() {
        let p = test_pass(chrono::Utc::now() + Duration::hours(2), 0.0, 90.0);
        let row = pass_row(&p, chrono::Utc::now(), DATE_FMT, TEST_LEAD, WIDE_BUDGET);
        assert!(!line_text(&row).contains("LOS in"), "{}", line_text(&row));
    }

    #[test]
    fn a_pass_already_set_keeps_its_ordinary_time_range_row() {
        let p = test_pass(chrono::Utc::now() - Duration::hours(2), 0.0, 90.0);
        // Just past its own LOS.
        let row = pass_row(&p, p.los + Duration::seconds(1), DATE_FMT, TEST_LEAD, WIDE_BUDGET);
        assert!(!line_text(&row).contains("LOS in"), "{}", line_text(&row));
    }

    #[test]
    fn the_aos_row_names_naked_eye_passes_with_a_star() {
        let mut visible = test_pass(chrono::Utc::now(), 0.0, 90.0);
        visible.visible = true;
        let midpoint = visible.aos + (visible.los - visible.aos) / 2;
        let row = aos_row(&visible, midpoint, WIDE_BUDGET);
        assert!(line_text(&row).starts_with("★ "), "{}", line_text(&row));

        let mut not_visible = visible.clone();
        not_visible.visible = false;
        let row = aos_row(&not_visible, midpoint, WIDE_BUDGET);
        assert!(line_text(&row).starts_with("  "), "{}", line_text(&row));
    }

    /// Before culmination, an under-way pass counts down to the peak — the
    /// best-signal moment, and the one worth anticipating mid-pass.
    #[test]
    fn an_under_way_pass_before_culmination_counts_down_to_peak() {
        let p = test_pass(chrono::Utc::now(), 0.0, 90.0);
        let just_after_aos = p.aos + Duration::seconds(1);
        assert!(just_after_aos < p.peak, "fixture assumption");
        let text = line_text(&aos_row(&p, just_after_aos, WIDE_BUDGET));
        assert!(text.contains("peak in"), "{text}");
        assert!(!text.contains("LOS in"), "{text}");
    }

    /// After culmination, it switches to counting down to LOS instead — the
    /// peak has already passed, so there's nothing left to anticipate but the
    /// satellite setting.
    #[test]
    fn an_under_way_pass_after_culmination_counts_down_to_los() {
        let p = test_pass(chrono::Utc::now(), 0.0, 90.0);
        let just_after_peak = p.peak + Duration::seconds(1);
        assert!(just_after_peak < p.los, "fixture assumption");
        let text = line_text(&aos_row(&p, just_after_peak, WIDE_BUDGET));
        assert!(text.contains("LOS in"), "{text}");
        assert!(!text.contains("peak in"), "{text}");
    }

    /// The handover between the two heads is exact and gap-free: the instant
    /// `now` reaches `p.peak` the row must already read "LOS in", not still
    /// "peak in 0m00s" for one extra frame.
    #[test]
    fn the_aos_row_switches_from_peak_to_los_exactly_at_culmination() {
        let p = test_pass(chrono::Utc::now(), 0.0, 90.0);
        let text = line_text(&aos_row(&p, p.peak, WIDE_BUDGET));
        assert!(text.contains("LOS in"), "{text}");
    }

    /// The metadata the user asked for: duration, peak elevation and bearing
    /// must all still be on the row once it switches into countdown mode —
    /// the whole point being that the row stays informative through the
    /// urgent stretch of a pass rather than losing detail right when it
    /// matters most.
    #[test]
    fn the_aos_row_carries_duration_peak_and_bearing() {
        let p = test_pass_with_peak(270.0, 0.0, 90.0); // W → N → E, 6m, 30°
        let text = line_text(&aos_row(&p, p.aos + Duration::seconds(1), WIDE_BUDGET));
        assert!(text.contains("6m"), "{text}");
        assert!(text.contains("30°"), "{text}");
        assert!(text.contains("W→N→E"), "{text}");
    }

    #[test]
    fn the_aos_imminent_row_carries_duration_peak_and_bearing() {
        let p = test_pass_with_peak(270.0, 0.0, 90.0);
        let now = p.aos - TEST_LEAD;
        let text = line_text(&aos_imminent_row(&p, now, WIDE_BUDGET));
        assert!(text.contains("6m"), "{text}");
        assert!(text.contains("30°"), "{text}");
        assert!(text.contains("W→N→E"), "{text}");
    }

    /// As the budget narrows, the metadata tail gives way one rung at a
    /// time — bearing first, then duration and peak too — but the countdown
    /// head itself, the reason the row exists, never does. Each threshold is
    /// taken from the previous rung's own rendered width rather than computed
    /// by hand, so this doesn't need to know how many columns `→` or `★`
    /// occupy — only that each successive rung is strictly narrower than the
    /// last.
    #[test]
    fn the_alert_rows_metadata_tail_narrows_before_its_countdown_head_ever_would() {
        let p = test_pass_with_peak(270.0, 0.0, 90.0);
        let now = p.aos - TEST_LEAD;

        let full = aos_imminent_row(&p, now, WIDE_BUDGET);
        let full_text = line_text(&full);
        assert!(full_text.contains("W→N→E"), "{full_text}");
        assert!(full_text.contains("6m"), "{full_text}");

        let no_bearing = aos_imminent_row(&p, now, full.width() - 1);
        let no_bearing_text = line_text(&no_bearing);
        assert!(no_bearing_text.contains("AOS in"), "{no_bearing_text}");
        assert!(!no_bearing_text.contains("W→N→E"), "{no_bearing_text}");
        assert!(no_bearing_text.contains("6m"), "{no_bearing_text}");
        assert!(no_bearing.width() < full.width());

        let head_only = aos_imminent_row(&p, now, no_bearing.width() - 1);
        let head_only_text = line_text(&head_only);
        assert!(head_only_text.contains("AOS in"), "{head_only_text}");
        assert!(!head_only_text.contains("6m"), "{head_only_text}");
        assert!(head_only.width() < no_bearing.width());

        // Even at a budget of zero the head still prints — the row can run
        // over its budget, exactly like the date ladder's narrowest rung; the
        // `List` clips it, this function never does.
        let starved_text = line_text(&aos_imminent_row(&p, now, 0));
        assert!(starved_text.contains("AOS in"), "{starved_text}");
    }

    /// The gap the user actually reported: pressing `n` lands the clock at
    /// `aos - lead` and there was nothing distinguishing that lead-in from an
    /// ordinary hours-away pass until AOS itself arrived — no countdown at
    /// all until the "LOS in" row appeared out of nowhere.
    #[test]
    fn a_pass_rising_within_aos_lead_gets_a_countdown_row_instead_of_its_time_range() {
        let p = test_pass(chrono::Utc::now() + TEST_LEAD, 0.0, 90.0);
        // Exactly where `n` (`App::jump_to_next_pass`) lands the clock.
        let now = p.aos - TEST_LEAD;
        let text = line_text(&pass_row(&p, now, DATE_FMT, TEST_LEAD, WIDE_BUDGET));
        assert!(text.contains("AOS in"), "{text}");
        assert!(!text.contains("LOS in"), "{text}");
        assert!(!text.contains('–'), "the ordinary time-range row must not also show: {text}");
    }

    #[test]
    fn the_aos_lead_countdown_counts_down_to_zero_right_as_the_pass_begins() {
        let p = test_pass(chrono::Utc::now() + TEST_LEAD, 0.0, 90.0);
        let just_before_aos = p.aos - Duration::seconds(1);
        let text = line_text(&pass_row(&p, just_before_aos, DATE_FMT, TEST_LEAD, WIDE_BUDGET));
        assert!(text.contains("AOS in 0m01s"), "{text}");
    }

    #[test]
    fn a_pass_further_out_than_aos_lead_keeps_its_ordinary_time_range_row() {
        let p = test_pass(chrono::Utc::now() + TEST_LEAD, 0.0, 90.0);
        // One second earlier than the lead window opens.
        let now = p.aos - TEST_LEAD - Duration::seconds(1);
        let text = line_text(&pass_row(&p, now, DATE_FMT, TEST_LEAD, WIDE_BUDGET));
        assert!(!text.contains("AOS in"), "{text}");
    }

    #[test]
    fn a_pass_honours_a_non_default_lead() {
        let p = test_pass(chrono::Utc::now() + Duration::minutes(3), 0.0, 90.0);
        let lead = Duration::minutes(3);
        let now = p.aos - lead;
        let text = line_text(&pass_row(&p, now, DATE_FMT, lead, WIDE_BUDGET));
        assert!(text.contains("AOS in"), "{text}");
    }

    #[test]
    fn the_aos_imminent_row_names_naked_eye_passes_with_a_star() {
        let mut visible = test_pass(chrono::Utc::now() + TEST_LEAD, 0.0, 90.0);
        visible.visible = true;
        let now = visible.aos - TEST_LEAD;
        assert!(line_text(&aos_imminent_row(&visible, now, WIDE_BUDGET)).starts_with("★ "));

        let mut not_visible = visible.clone();
        not_visible.visible = false;
        assert!(line_text(&aos_imminent_row(&not_visible, now, WIDE_BUDGET)).starts_with("  "));
    }
}
