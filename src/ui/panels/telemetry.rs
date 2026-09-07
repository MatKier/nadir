//! The TELEMETRY panel: live orbital state for the tracked satellite.

use chrono::{DateTime, Duration, Utc};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, Panel};
use crate::geo::look_angles;
use crate::orbit::{Confidence, SatState, Tracker};
use crate::ui::panels::fmt::{dim, kv, label};
use crate::ui::{is_focused, panel_block, Theme};

/// Width of the value column every telemetry row right-aligns into, so the
/// numbers end in the same column and the units line up beside them.
const VALUE_W: usize = 8;

/// Rows `draw` always emits, once an element set is available: ALT, SPD, POS,
/// FOOT, ORB, APSIS, REV, SUN, TLE, ACC.
const ALWAYS_BODY_ROWS: u16 = 10;

/// Height `draw` needs, including its two border rows. `ui::draw` sizes the
/// panel from this rather than a constant because one row is conditional —
/// RANGE needs a ground station — so a fixed height either wastes a row on
/// most satellites or clips it once a ground station is configured. Keeping
/// the count here means it moves with the row list instead of drifting out
/// of sync with a constant over in `ui::mod`.
pub fn height(app: &App) -> u16 {
    2 + body_rows(app.config.ground_station().is_some())
}

fn body_rows(has_ground_station: bool) -> u16 {
    ALWAYS_BODY_ROWS + u16::from(has_ground_station)
}

pub fn draw(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    sat: Option<&(Tracker, SatState)>,
    has_elements: bool,
    now: DateTime<Utc>,
) {
    let block = panel_block(Panel::Telemetry, "TELEMETRY", is_focused(app, Panel::Telemetry));
    let mut rows: Vec<Line> = Vec::new();

    match sat {
        // `sat` is `None` either because no element set has arrived yet, or
        // because one has but the clock has been scrubbed past where SGP4 will
        // propagate it — different situations that must not read the same.
        None if has_elements => {
            rows.push(dim("  can't propagate the element set to this time"));
            rows.push(dim("  press 0 to snap back to now"));
        }
        None => rows.push(dim("  waiting for the element set…")),
        Some((tr, s)) => {
            rows.push(kv("ALT", format!("{:>VALUE_W$.1} km", s.sub_point.alt_km)));
            rows.push(kv(
                "SPD",
                format!(
                    "{:>VALUE_W$.0} km/h  {:.2} km/s",
                    s.speed_kms * 3600.0,
                    s.speed_kms
                ),
            ));
            rows.push(kv(
                "POS",
                format!("{:>VALUE_W$.2}°  {:>7.2}°", s.sub_point.lat_deg, s.sub_point.lon_deg),
            ));
            rows.push(kv("FOOT", format!("{:>VALUE_W$.0} km radius", s.footprint_km)));

            let orb = tr.orbit_shape();
            rows.push(Line::from(vec![
                label("ORB"),
                Span::styled(
                    format!("{:>VALUE_W$}", orb.label()),
                    Style::new().fg(Theme::VALUE),
                ),
                Span::styled(
                    format!("  {:.1} min · {:.1}°", orb.period_min, orb.inclination_deg),
                    Style::new().fg(Theme::LABEL),
                ),
            ]));

            rows.push(kv("APSIS", format!("{:>VALUE_W$.0} × {:.0} km", orb.perigee_km, orb.apogee_km)));

            rows.push(kv("REV", format!("{:>VALUE_W$}", s.revolution)));

            let (sun_word, sun_color) = if s.sunlit {
                ("sunlit", Theme::CAUTION)
            } else {
                ("eclipsed", Theme::ACCENT)
            };
            rows.push(Line::from(vec![
                label("SUN"),
                Span::styled(format!("{sun_word:>VALUE_W$}"), Style::new().fg(sun_color)),
                match next_sun_transition(tr, now) {
                    Some((to_lit, t)) => Span::styled(
                        format!(
                            "  {} in {}",
                            if to_lit { "sunrise" } else { "sunset" },
                            fmt_hms((t - now).max(Duration::zero()))
                        ),
                        Style::new().fg(Theme::LABEL),
                    ),
                    None => Span::raw(""),
                },
            ]));

            // Live look angle from the ground station, when one is configured.
            if let Some(g) = app.config.ground_station() {
                let la = look_angles(&g, s.ecef_km);
                let (word, wc) = if la.elevation_deg >= 0.0 {
                    (format!("el {:+.0}°  in view", la.elevation_deg), Theme::NOMINAL)
                } else {
                    (format!("el {:+.0}°  below horizon", la.elevation_deg), Theme::LABEL)
                };
                rows.push(Line::from(vec![
                    label("RANGE"),
                    Span::styled(format!("{:>VALUE_W$.0} km  ", la.range_km), Style::new().fg(Theme::VALUE)),
                    Span::styled(word, Style::new().fg(wc)),
                ]));
            }

            // `element_age` is signed — a backward scrub puts `now` before the
            // epoch — so the colour keys off the magnitude, and a negative age
            // reads "ahead" rather than a bare "-3d old".
            let age = tr.element_age(now);
            let age_color = if age.abs() > Duration::hours(72) {
                Theme::ALERT
            } else if age.abs() > Duration::hours(36) {
                Theme::CAUTION
            } else {
                Theme::LABEL
            };
            let (age_num, age_rel) = if age < Duration::zero() {
                (fmt_dur_coarse(-age), "ahead")
            } else {
                (fmt_dur_coarse(age), "old")
            };
            rows.push(Line::from(vec![
                label("TLE"),
                Span::styled(
                    format!("{age_num:>VALUE_W$} {age_rel}"),
                    Style::new().fg(age_color),
                ),
            ]));

            // What that age costs you: a modelled position error, from the
            // element set's own drag term and its orbital regime (see
            // `orbit::accuracy`). The along-track part doubles as a timing
            // error on the ground track. Past the model's validity horizon
            // SGP4 still answers but nothing here can bound how wrong it is, so
            // don't imply a figure — mirrors the "can't propagate" branch
            // above, for the softer failure.
            let acc = tr.accuracy_at(now, s.speed_kms);
            let acc_color = match acc.confidence {
                Confidence::Nominal => Theme::LABEL,
                Confidence::Degraded => Theme::CAUTION,
                Confidence::Unreliable | Confidence::Unmodelled => Theme::ALERT,
            };
            let acc_row = match acc.confidence {
                Confidence::Unmodelled => vec![
                    label("ACC"),
                    Span::styled(format!("{:>VALUE_W$}", "beyond"), Style::new().fg(acc_color)),
                    Span::styled("  model validity", Style::new().fg(Theme::LABEL)),
                ],
                _ => vec![
                    label("ACC"),
                    Span::styled(
                        format!("{:>VALUE_W$}", format!("±{:.0} km", acc.total_km)),
                        Style::new().fg(acc_color),
                    ),
                    Span::styled(
                        format!("  ±{:.1} s along-track", acc.timing_s),
                        Style::new().fg(Theme::LABEL),
                    ),
                ],
            };
            rows.push(Line::from(acc_row));
        }
    }

    frame.render_widget(Paragraph::new(rows).block(block), area);
}

/// Time of the next sunlit/eclipsed flip after `from`, refined to
/// sub-second precision. A coarse 30 s scan finds the window the flip falls
/// in — cheap, and good enough to bound it — then a dozen bisections of that
/// window narrow it to a few milliseconds, well under the one-second
/// resolution `fmt_hms` prints. Without the bisection the countdown visibly
/// jumped in 30 s steps instead of ticking down smoothly.
fn next_sun_transition(tr: &Tracker, from: DateTime<Utc>) -> Option<(bool, DateTime<Utc>)> {
    let mut lo = from;
    let prev = tr.state_at(lo).ok()?.sunlit;
    let mut hi = lo;
    let mut cur = prev;
    for _ in 0..220 {
        hi += Duration::seconds(30);
        cur = tr.state_at(hi).ok()?.sunlit;
        if cur != prev {
            break;
        }
        lo = hi;
    }
    if cur == prev {
        return None;
    }
    for _ in 0..12 {
        let mid = lo + (hi - lo) / 2;
        if tr.state_at(mid).ok()?.sunlit == prev {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some((cur, hi))
}

fn fmt_hms(d: Duration) -> String {
    let s = d.num_seconds().max(0);
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn fmt_dur_coarse(d: Duration) -> String {
    let m = d.num_minutes();
    if m < 90 {
        format!("{m}m")
    } else if m < 60 * 48 {
        format!("{}h", m / 60)
    } else {
        format!("{}d", m / (60 * 24))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the exact `{:>VALUE_W$...}` templates `draw()` uses for each
    /// row's leading number, in the order `draw` emits them — one entry per
    /// row it can produce, RANGE included, which is also what
    /// `body_row_count_matches_the_rows_draw_can_emit` counts against
    /// `body_rows`. `POS` and `TLE` were the rows off the shared column
    /// before this fix (widths of 7 and 8-with-an-extra-space respectively);
    /// the rest already used a bare `8`. Pinning all of them to `VALUE_W`
    /// here means the next row that copies a literal instead of the constant
    /// is a visible test failure, not a squint at a screenshot.
    fn row_templates() -> Vec<String> {
        vec![
            format!("{:>VALUE_W$.1}", 423.5_f64),      // ALT
            format!("{:>VALUE_W$.0}", 27565.0_f64),    // SPD
            format!("{:>VALUE_W$.2}", -8.53_f64),      // POS (first coord)
            format!("{:>VALUE_W$.0}", 2263.0_f64),     // FOOT
            format!("{:>VALUE_W$}", "LEO-P"),          // ORB
            format!("{:>VALUE_W$.0}", 408.0_f64),      // APSIS (first altitude)
            format!("{:>VALUE_W$}", 58409),            // REV
            format!("{:>VALUE_W$}", "sunlit"),         // SUN
            format!("{:>VALUE_W$.0}", 7352.0_f64),     // RANGE
            format!("{:>VALUE_W$}", "18h"),            // TLE
            format!("{:>VALUE_W$}", "±2 km"),          // ACC
        ]
    }

    #[test]
    fn telemetry_values_share_one_right_aligned_column() {
        for s in row_templates() {
            assert_eq!(s.chars().count(), VALUE_W, "{s:?} is not VALUE_W columns wide");
        }
    }

    /// `panels::telemetry::height` (and the `ui::mod` layout that calls it)
    /// budgets rows from `body_rows`, computed from the one conditional
    /// row's precondition rather than counted from what `draw` actually
    /// pushes — this pins the two to the same number so they can't drift
    /// apart silently.
    #[test]
    fn body_row_count_matches_the_rows_draw_can_emit() {
        assert_eq!(body_rows(true) as usize, row_templates().len());
    }

    #[test]
    fn next_sun_transition_is_stable_to_the_second_not_quantised_to_the_scan_step() {
        // Same ISS element set the propagate tests use.
        let tr = crate::orbit::test_tracker();
        let t0 = tr.epoch();

        let (_, at_t0) =
            next_sun_transition(&tr, t0).expect("a transition exists within the scan window");
        let (_, at_t0_plus_1s) = next_sun_transition(&tr, t0 + Duration::seconds(1))
            .expect("a transition still exists one second later");

        // Before the bisection, this instant was quantised to the 30 s scan
        // step, so the countdown visibly jumped rather than counting down.
        let drift_ms = (at_t0 - at_t0_plus_1s).num_milliseconds().abs();
        assert!(
            drift_ms < 2000,
            "the transition instant moved by {drift_ms} ms for a 1 s shift in `from`"
        );
    }
}
