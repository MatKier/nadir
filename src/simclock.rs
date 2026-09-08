//! `SimClock` — the displayed clock, detached from wall time.
//!
//! Everything the dashboard draws about the satellite (position, ground track,
//! footprint, terminator, night wash, look angles, the sunlit/eclipsed
//! countdown and pass prediction) is a pure function of one instant. `SimClock`
//! is that instant: normally it is wall time, but it can be paused, run at a
//! multiple of real speed in either direction, stepped by a fixed amount, or
//! jumped to an arbitrary point.
//!
//! Feed freshness, the status chips, session uptime, cache ages and the launch
//! rate-limiter are all properties of the *real* clock and are deliberately not
//! routed through here — see `AppData` and `Source`.

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, NaiveDate, NaiveDateTime, NaiveTime, Utc};

/// Signed speed multipliers, slowest-in-reverse to fastest-forward. `,`/`.`
/// walk this as one continuum that passes through ±1× — there is no 0× entry;
/// a full stop is `paused`, which is orthogonal. Stepping "slower" from +1×
/// lands on −1× (reverse at real speed) and then keeps going into fast reverse,
/// mirroring how `App::zoom_out` steps off the widest follow level straight out
/// to the whole world.
const RATE_LADDER: [i64; 16] =
    [-1800, -900, -300, -60, -10, -5, -2, -1, 1, 2, 5, 10, 60, 300, 900, 1800];

/// Index of the `1` in [`RATE_LADDER`] — the live, real-time rate.
const LIVE_INDEX: usize = 8;

/// How the clock is moving right now, for the title-bar marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockState {
    /// Locked to wall time: zero offset, 1× forward, not paused.
    Live,
    /// Running at 1× forward but displaced from wall time by an earlier jump or
    /// step — the display is self-consistent but no longer "now".
    Drifted,
    /// Frozen at one instant.
    Paused,
    /// Running at `.0`× wall speed; the sign is the direction (negative is
    /// reverse). Never ±1 with a zero offset — that is [`ClockState::Live`].
    Warp(i64),
}

/// The displayed clock. `now()` is `wall_now + offset`; `offset` only moves
/// when the clock is scrubbed, so an untouched clock reads exactly live with no
/// drift to round off.
#[derive(Debug, Clone)]
pub struct SimClock {
    /// `sim_now - wall_now`. Exactly zero whenever the clock has never been
    /// scrubbed, because [`SimClock::advance_by`] adds `dt * (rate - 1)` and
    /// that term is exactly zero at rate 1×.
    offset: ChronoDuration,
    /// Index into [`RATE_LADDER`]. Survives a pause, the way `App::zoom`
    /// survives an `f` toggle.
    rate_index: usize,
    paused: bool,
    /// Wall instant `advance` last folded into `offset`.
    last: Instant,
}

impl Default for SimClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SimClock {
    pub fn new() -> Self {
        Self {
            offset: ChronoDuration::zero(),
            rate_index: LIVE_INDEX,
            paused: false,
            last: Instant::now(),
        }
    }

    /// The signed speed multiplier currently selected (ignores `paused`).
    fn rate(&self) -> i64 {
        RATE_LADDER[self.rate_index]
    }

    /// The simulated instant, given the current wall time. Split out from
    /// [`SimClock::now`] so tests can pin the wall clock.
    pub fn now_from(&self, wall: DateTime<Utc>) -> DateTime<Utc> {
        wall + self.offset
    }

    /// The simulated instant now.
    pub fn now(&self) -> DateTime<Utc> {
        self.now_from(Utc::now())
    }

    /// Fold the wall time elapsed since the last call into `offset`.
    pub fn advance(&mut self) {
        let dt = self.last.elapsed();
        self.last = Instant::now();
        self.advance_by(dt);
    }

    /// Advance the simulated clock for `dt` of elapsed wall time. Pure, so the
    /// warp arithmetic can be tested without sleeping.
    ///
    /// Paused: `offset` shrinks by exactly `dt`, so `now()` stays put.
    /// Running: `offset` moves by `dt * (rate - 1)` — zero at 1×, `dt * 59` at
    /// 60×, `-dt * 2` at −1×.
    pub fn advance_by(&mut self, dt: Duration) {
        let dt = ChronoDuration::from_std(dt).unwrap_or_else(|_| ChronoDuration::zero());
        if self.paused {
            self.offset -= dt;
        } else {
            let factor = (self.rate() - 1) as i32;
            self.offset += dt * factor;
        }
    }

    /// Shift the simulated clock by `delta` without touching the rate or the
    /// pause state — the ±1 min / ±1 h steps.
    pub fn jump(&mut self, delta: ChronoDuration) {
        self.offset += delta;
    }

    /// `sim_now - wall_now`: how far the displayed clock sits from real time.
    /// Exactly zero until the clock is scrubbed.
    pub fn offset(&self) -> ChronoDuration {
        self.offset
    }

    /// Land on `target` and pause there, at 1×, given the current wall time.
    /// Used by the go-to prompt and the next-pass jumps.
    pub fn goto_from(&mut self, target: DateTime<Utc>, wall: DateTime<Utc>) {
        self.offset = target - wall;
        self.rate_index = LIVE_INDEX;
        self.paused = true;
    }

    pub fn goto(&mut self, target: DateTime<Utc>) {
        self.goto_from(target, Utc::now());
    }

    /// Snap back to wall time: zero offset, 1× forward, running.
    pub fn reset(&mut self) {
        self.offset = ChronoDuration::zero();
        self.rate_index = LIVE_INDEX;
        self.paused = false;
        self.last = Instant::now();
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
    }

    /// Step one notch towards fast-forward.
    pub fn faster(&mut self) {
        self.rate_index = (self.rate_index + 1).min(RATE_LADDER.len() - 1);
    }

    /// Step one notch towards fast-reverse, passing through ±1× on the way.
    pub fn slower(&mut self) {
        self.rate_index = self.rate_index.saturating_sub(1);
    }

    pub fn is_live(&self) -> bool {
        self.state() == ClockState::Live
    }

    pub fn state(&self) -> ClockState {
        if self.paused {
            return ClockState::Paused;
        }
        match self.rate() {
            1 if self.offset.is_zero() => ClockState::Live,
            1 => ClockState::Drifted,
            r => ClockState::Warp(r),
        }
    }
}

/// Parse the go-to-time prompt. All times are UTC.
///
/// | Input | Meaning |
/// |---|---|
/// | `+90m` `-2h` `+3d` `+45s` | relative to `sim_now` |
/// | `04:30` `04:30:00` | the next time it is that clock time, at or after `sim_now` |
/// | `2026-09-08 04:30` / `2026-09-08T04:30[:SS][Z]` | that instant |
/// | `2026-09-08` | midnight UTC that day |
pub fn parse_goto(input: &str, sim_now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let s = input.trim();
    if s.is_empty() {
        bail!("enter a time — e.g. 2026-09-08 04:30, 04:30, or +90m");
    }

    // Relative offset.
    if s.starts_with('+') || s.starts_with('-') {
        let magnitude = parse_relative(&s[1..])
            .with_context(|| format!("\"{s}\" is not an offset like +90m, -2h or +3d"))?;
        let target = if s.starts_with('-') {
            sim_now.checked_sub_signed(magnitude)
        } else {
            sim_now.checked_add_signed(magnitude)
        };
        return target.context("that offset lands outside the representable range");
    }

    // Absolute date-time, with or without seconds, `T` or space, optional `Z`.
    let body = s.trim_end_matches(['Z', 'z']).trim();
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M"] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(body, fmt) {
            return Ok(dt.and_utc());
        }
    }

    // Bare date: midnight UTC.
    if let Ok(d) = NaiveDate::parse_from_str(body, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).expect("midnight is a valid time").and_utc());
    }

    // Bare clock time: the next occurrence at or after sim_now.
    for fmt in ["%H:%M:%S", "%H:%M"] {
        if let Ok(t) = NaiveTime::parse_from_str(body, fmt) {
            let mut when = sim_now.date_naive().and_time(t).and_utc();
            if when < sim_now {
                when += ChronoDuration::days(1);
            }
            return Ok(when);
        }
    }

    bail!("\"{input}\" is not a time — try 2026-09-08 04:30, 04:30, or +90m");
}

/// Parse the magnitude of a relative offset: digits then a single unit letter
/// (`s`, `m`, `h`, `d`). The sign is handled by the caller. Capped at 100 years
/// — anything beyond is past where SGP4 gives an answer at all, and an
/// unchecked `TimeDelta` that far out panics.
fn parse_relative(s: &str) -> Result<ChronoDuration> {
    /// 100 years in days — a generous ceiling for a scrub, well inside the
    /// range `TimeDelta` and `DateTime` arithmetic stay total.
    const MAX_OFFSET_DAYS: i64 = 36_525;

    let s = s.trim();
    let split = s
        .find(|c: char| !c.is_ascii_digit())
        .context("expected digits then a unit — s, m, h or d")?;
    let (digits, unit) = s.split_at(split);
    let n: i64 = digits.parse().context("not a whole number")?;
    let magnitude = match unit.trim() {
        "s" | "sec" | "secs" => ChronoDuration::try_seconds(n),
        "m" | "min" | "mins" => ChronoDuration::try_minutes(n),
        "h" | "hr" | "hrs" => ChronoDuration::try_hours(n),
        "d" | "day" | "days" => ChronoDuration::try_days(n),
        other => bail!("unknown unit \"{other}\" — use s, m, h or d"),
    };
    magnitude
        .filter(|d| d.num_days().abs() <= MAX_OFFSET_DAYS)
        .context("that offset is too large — keep it under 100 years")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn wall() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 7, 12, 0, 0).unwrap()
    }

    #[test]
    fn a_clock_at_one_times_rate_stays_exactly_live() {
        let mut c = SimClock::new();
        for _ in 0..1000 {
            c.advance_by(Duration::from_millis(250));
        }
        assert!(c.is_live());
        assert_eq!(c.state(), ClockState::Live);
        assert_eq!(c.now_from(wall()), wall());
    }

    #[test]
    fn pausing_freezes_the_simulated_instant() {
        let mut c = SimClock::new();
        c.toggle_pause();
        let frozen = c.now_from(wall());
        // 40 ticks of 250 ms = 10 s of wall time passes.
        for _ in 0..40 {
            c.advance_by(Duration::from_millis(250));
        }
        assert_eq!(c.now_from(wall() + ChronoDuration::seconds(10)), frozen);
        assert_eq!(c.state(), ClockState::Paused);
    }

    #[test]
    fn warp_advances_the_offset_by_rate_minus_one_per_wall_second() {
        let mut c = SimClock::new();
        c.faster(); // 2x
        c.faster(); // 5x
        assert_eq!(c.rate(), 5);
        c.advance_by(Duration::from_secs(10));
        // 10 wall seconds at 5x = 50 sim seconds: 40s of accumulated offset on
        // top of the 10s of wall time that itself elapsed.
        assert_eq!(
            c.now_from(wall() + ChronoDuration::seconds(10)),
            wall() + ChronoDuration::seconds(50)
        );
        assert_eq!(c.state(), ClockState::Warp(5));
    }

    #[test]
    fn slowing_past_one_times_crosses_into_reverse() {
        let mut c = SimClock::new();
        assert_eq!(c.rate(), 1);
        c.slower();
        assert_eq!(c.rate(), -1);
        assert_eq!(c.state(), ClockState::Warp(-1));
        c.slower();
        assert_eq!(c.rate(), -2);
        c.advance_by(Duration::from_secs(10));
        // 10 wall seconds at -2x lands the clock 20s before where it started.
        assert_eq!(
            c.now_from(wall() + ChronoDuration::seconds(10)),
            wall() - ChronoDuration::seconds(20)
        );
    }

    #[test]
    fn live_index_points_at_the_one_times_entry() {
        assert_eq!(RATE_LADDER[LIVE_INDEX], 1);
    }

    /// The ladder is meant to read the same in both directions — every
    /// forward multiplier has its negative at the mirrored position.
    #[test]
    fn the_rate_ladder_is_symmetric_about_live() {
        for (i, &r) in RATE_LADDER.iter().enumerate() {
            assert_eq!(r, -RATE_LADDER[RATE_LADDER.len() - 1 - i], "index {i}");
        }
    }

    #[test]
    fn the_rate_ladder_saturates_at_both_ends() {
        let mut c = SimClock::new();
        for _ in 0..20 {
            c.faster();
        }
        assert_eq!(c.rate(), 1800);
        for _ in 0..40 {
            c.slower();
        }
        assert_eq!(c.rate(), -1800);
    }

    #[test]
    fn reset_returns_an_offset_clock_to_live() {
        let mut c = SimClock::new();
        c.faster();
        c.jump(ChronoDuration::hours(3));
        c.toggle_pause();
        assert!(!c.is_live());
        c.reset();
        assert!(c.is_live());
        assert_eq!(c.now_from(wall()), wall());
    }

    #[test]
    fn goto_lands_on_the_target_and_pauses_at_one_times() {
        let mut c = SimClock::new();
        c.faster();
        let target = wall() + ChronoDuration::days(3) + ChronoDuration::minutes(30);
        c.goto_from(target, wall());
        assert_eq!(c.now_from(wall()), target);
        assert_eq!(c.state(), ClockState::Paused);
        c.toggle_pause();
        assert_eq!(c.state(), ClockState::Drifted);
    }

    #[test]
    fn offset_is_zero_until_the_clock_is_scrubbed() {
        let mut c = SimClock::new();
        for _ in 0..1000 {
            c.advance_by(Duration::from_millis(250));
        }
        assert_eq!(c.offset(), ChronoDuration::zero());
    }

    #[test]
    fn a_paused_clock_reports_the_negative_offset_it_accumulates() {
        let mut c = SimClock::new();
        c.toggle_pause();
        // 40 ticks of 250 ms = 10 s of wall time the frozen clock falls behind.
        for _ in 0..40 {
            c.advance_by(Duration::from_millis(250));
        }
        assert_eq!(c.offset(), ChronoDuration::seconds(-10));
    }

    #[test]
    fn a_step_while_live_leaves_the_clock_drifted_not_live() {
        let mut c = SimClock::new();
        c.jump(ChronoDuration::minutes(1));
        assert_eq!(c.state(), ClockState::Drifted);
        assert_eq!(c.now_from(wall()), wall() + ChronoDuration::minutes(1));
    }

    #[test]
    fn parse_goto_accepts_a_relative_offset() {
        let now = wall();
        assert_eq!(parse_goto("+90m", now).unwrap(), now + ChronoDuration::minutes(90));
        assert_eq!(parse_goto("-2h", now).unwrap(), now - ChronoDuration::hours(2));
        assert_eq!(parse_goto("+3d", now).unwrap(), now + ChronoDuration::days(3));
        assert_eq!(parse_goto("+45s", now).unwrap(), now + ChronoDuration::seconds(45));
    }

    #[test]
    fn parse_goto_accepts_an_absolute_datetime() {
        let want = Utc.with_ymd_and_hms(2026, 9, 8, 4, 30, 0).unwrap();
        for input in ["2026-09-08 04:30", "2026-09-08T04:30", "2026-09-08T04:30:00Z"] {
            assert_eq!(parse_goto(input, wall()).unwrap(), want, "{input}");
        }
        let midnight = Utc.with_ymd_and_hms(2026, 9, 8, 0, 0, 0).unwrap();
        assert_eq!(parse_goto("2026-09-08", wall()).unwrap(), midnight);
    }

    #[test]
    fn parse_goto_reads_a_bare_time_as_the_next_occurrence() {
        // 12:00 exactly — sim_now itself does not count as "after".
        let now = wall();
        assert_eq!(
            parse_goto("14:30", now).unwrap(),
            Utc.with_ymd_and_hms(2026, 9, 7, 14, 30, 0).unwrap()
        );
        // A time already past today rolls to tomorrow.
        assert_eq!(
            parse_goto("06:00", now).unwrap(),
            Utc.with_ymd_and_hms(2026, 9, 8, 6, 0, 0).unwrap()
        );
    }

    #[test]
    fn parse_goto_rejects_nonsense() {
        for bad in ["", "later", "+", "+10y", "25:00", "2026-13-40", "banana"] {
            assert!(parse_goto(bad, wall()).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn parse_goto_refuses_an_absurd_offset_instead_of_panicking() {
        // `TimeDelta::days(1e15 as i64)` panics unchecked — the parser must
        // turn these into an error, not take the process down.
        for bad in ["+999999999999d", "+9999999999h", "-99999999999999d"] {
            assert!(parse_goto(bad, wall()).is_err(), "{bad:?} must be a clean error");
        }
    }
}
