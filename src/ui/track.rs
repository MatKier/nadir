//! The ground track, footprint and acquisition/warp-trail geometry both
//! `ui::map` and `ui::globe` draw, computed once so the two views can't
//! disagree on how long a trail is or how far an acquisition sweep has
//! reached — the same reason `ui::canvas` exists for shared grid geometry.

use chrono::{DateTime, Duration, Utc};

use crate::geo::{footprint_ring, GeoPoint};
use crate::orbit::{SatState, Tracker};
use crate::simclock::ClockState;

/// The past window's length, in minutes, when the clock isn't warping fast
/// enough to stretch it — the trail's shipped length before this feature
/// existed, and the floor [`trail_windows`] never drops it below in the
/// direction the satellite is actually moving forward through.
const BASE_PAST_MIN: f64 = 35.0;
/// The future window's length, in minutes, under the same rule — the
/// look-ahead trail's shipped length. Named separately from
/// [`BASE_PAST_MIN`] because the two have never been equal: the future
/// window shows more because it's a plan, not a memory.
const BASE_FUTURE_MIN: f64 = 65.0;

/// How many seconds of *your* time the trail's length is measured against —
/// a camera shutter, not a tuned constant. At `rate`×, `rate * SHUTTER_SECS`
/// simulated seconds pass in one shutter's worth of wall time, and that is
/// the trail's span whenever it exceeds the window's own base length. So the
/// trail is literally "how far the satellite moved while you watched the
/// last `SHUTTER_SECS` go by" — motion blur whose length means something,
/// not a slider tuned until it looked right.
const SHUTTER_SECS: f64 = 12.0;

/// Ceiling on the shutter above, expressed in revolutions of whichever orbit
/// is being drawn rather than a fixed number of minutes — so a GEO satellite
/// (period ~1436 min) and the ISS (period ~93 min) both get "a few times
/// around", not one getting a trail a hundred times longer than the other at
/// the same warp rate. See `a_geostationary_period_caps_the_trail_far_later_than_a_low_orbit_one`.
const TRAIL_MAX_REVS: f64 = 4.0;

/// Target point budget for whichever window is doing the stretching. Bounds
/// the cost of the extra propagation a stretched trail asks for: `Tracker::
/// ground_track` runs one SGP4 call per point, and at `FRAME_WARP` (40ms)
/// that cost is paid every frame. Letting the step grow with the span keeps
/// the total roughly flat rather than letting a 1800× trail cost ten times
/// what a 1× one does. The trade is coarser sampling at the very top of the
/// ladder — at 1800× the step is ~54s, a ~3.9° chord of the ground track,
/// visibly straightening the track's tightest turns (near the poles) at
/// whole-world zoom. That is judged an acceptable trade for flat CPU at the
/// one speed nobody is reading the track's fine shape at; see the plan's
/// verification notes if it ever needs revisiting.
const TRAIL_POINTS: f64 = 400.0;

/// Floor under the step so a short, un-stretched trail keeps exactly its
/// shipped density — the step this feature must never make *finer* than.
const MIN_STEP_SECS: f64 = 20.0;

/// The three track-shaped things both map views draw, derived once so
/// `map::draw` and `globe::draw` can't compute them differently.
pub(in crate::ui) struct TrackScene {
    /// Positions from the stretched/revealed `before` window up to `now`,
    /// chronologically ascending (oldest first) — the shape
    /// `Tracker::ground_track` and `draw_fading_polyline` both expect.
    pub past: Vec<Vec<GeoPoint>>,
    /// Positions from `now` out to the stretched/revealed `after` window,
    /// also chronologically ascending.
    pub future: Vec<Vec<GeoPoint>>,
    pub footprint: Vec<Vec<GeoPoint>>,
    /// True while the clock is warping backwards. The comet-tail fade lives
    /// on whichever window is actually growing — `past` running forward,
    /// `future` running in reverse (see [`trail_windows`]) — and this is
    /// what tells `map::paint_scene` which one that is, since both windows
    /// are always populated and neither field says so on its own.
    pub reversed: bool,
}

/// Everything both map views need to draw the track, footprint and the
/// acquisition/warp-trail effects, gathered in one call. `reveal` is
/// `1.0` outside an acquisition sweep — see `ui::draw`'s note on
/// `ACQUIRE_REVEAL` — and scales both windows and the footprint radius down
/// together, so the whole scene grows outward from the sub-satellite point
/// rather than the track and the footprint arriving on different schedules.
pub(in crate::ui) fn track_scene(
    tr: &Tracker,
    state: &SatState,
    now: DateTime<Utc>,
    clock: ClockState,
    reveal: f32,
) -> TrackScene {
    let period_min = tr.orbit_shape().period_min;
    let (before, after, step) = trail_windows(clock, period_min, reveal);
    let reversed = matches!(clock, ClockState::Warp(rate) if rate < 0);
    let reveal = f64::from(reveal.clamp(0.0, 1.0));
    TrackScene {
        past: tr.ground_track(now, before, Duration::zero(), step),
        future: tr.ground_track(now, Duration::zero(), after, step),
        footprint: footprint_ring(&state.sub_point, state.footprint_km * reveal, 180),
        reversed,
    }
}

/// The `(before, after, step)` a ground track should be sampled over at this
/// clock state — the pure decision [`track_scene`] hands to
/// `Tracker::ground_track`, split out so it can be asserted without a
/// propagator.
///
/// Only the window in the direction the satellite is actually travelling
/// stretches: forward, that's `before` (the comet tail behind it); in
/// reverse, the satellite's direction of travel is towards *decreasing* sim
/// time, so the positions it visually just occupied are the ones ahead of
/// `now` — `after` stretches instead, and `before` stays at its own base.
/// Neither window's *base* length ever changes: only how far past that base
/// a fast enough warp pushes it.
pub(in crate::ui) fn trail_windows(
    clock: ClockState,
    period_min: f64,
    reveal: f32,
) -> (Duration, Duration, Duration) {
    let rate = match clock {
        ClockState::Warp(rate) => rate,
        // Live, Drifted and Paused all move the satellite at whatever the
        // real clock does (or not at all) — none of them is a warp, so all
        // three get the same un-stretched trail a plain 1× view always has.
        _ => 1,
    };
    let (before_min, after_min) = if rate < 0 {
        (BASE_PAST_MIN, stretched_min(rate, BASE_FUTURE_MIN, period_min))
    } else {
        (stretched_min(rate, BASE_PAST_MIN, period_min), BASE_FUTURE_MIN)
    };

    // Sized off whichever window is actually longest this frame — usually
    // the stretched one, but at low warp that's still the un-stretched 65
    // minute future default, which is exactly today's 20s step.
    let step_secs = (before_min.max(after_min) * 60.0 / TRAIL_POINTS).max(MIN_STEP_SECS);

    let reveal = f64::from(reveal.clamp(0.0, 1.0));
    (minutes(before_min * reveal), minutes(after_min * reveal), seconds(step_secs))
}

/// How long a trailing window should span, in minutes: the shutter distance
/// swept in the last [`SHUTTER_SECS`] of wall time, floored at `base_min` (so
/// a slow warp changes nothing) and capped at [`TRAIL_MAX_REVS`] revolutions
/// of `period_min` (so a fast enough warp stops growing the trail rather than
/// wrapping it around the planet indefinitely).
fn stretched_min(rate: i64, base_min: f64, period_min: f64) -> f64 {
    let shutter_min = rate.unsigned_abs() as f64 * SHUTTER_SECS / 60.0;
    // `.max(base_min)` guards a cap that would otherwise sit below the base
    // for an implausibly short period — not a case any real orbit reaches,
    // but a cap parameter should never be able to make a window shorter than
    // its own resting length.
    let cap_min = (TRAIL_MAX_REVS * period_min).max(base_min);
    shutter_min.max(base_min).min(cap_min)
}

fn minutes(m: f64) -> Duration {
    Duration::milliseconds((m * 60_000.0).round() as i64)
}

fn seconds(s: f64) -> Duration {
    Duration::milliseconds((s * 1_000.0).round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISS_PERIOD_MIN: f64 = 92.7;

    #[test]
    fn trail_windows_are_unchanged_at_one_times_speed() {
        for state in [ClockState::Live, ClockState::Drifted, ClockState::Paused] {
            let (before, after, step) = trail_windows(state, ISS_PERIOD_MIN, 1.0);
            assert_eq!(before, Duration::minutes(35), "{state:?}");
            assert_eq!(after, Duration::minutes(65), "{state:?}");
            assert_eq!(step, Duration::seconds(20), "{state:?}");
        }
    }

    #[test]
    fn trail_windows_are_unchanged_below_the_shutter_threshold() {
        // 60× sweeps 12 simulated minutes per shutter — under both windows'
        // own base lengths, forward or in reverse — so nothing should move.
        let (before, after, step) = trail_windows(ClockState::Warp(60), ISS_PERIOD_MIN, 1.0);
        assert_eq!((before, after, step), (Duration::minutes(35), Duration::minutes(65), Duration::seconds(20)));

        let (before, after, step) = trail_windows(ClockState::Warp(-60), ISS_PERIOD_MIN, 1.0);
        assert_eq!((before, after, step), (Duration::minutes(35), Duration::minutes(65), Duration::seconds(20)));
    }

    #[test]
    fn a_faster_warp_stretches_the_trail_further() {
        let (at_60, _, _) = trail_windows(ClockState::Warp(60), ISS_PERIOD_MIN, 1.0);
        let (at_300, _, _) = trail_windows(ClockState::Warp(300), ISS_PERIOD_MIN, 1.0);
        let (at_900, _, _) = trail_windows(ClockState::Warp(900), ISS_PERIOD_MIN, 1.0);
        assert!(at_300 > at_60, "300x ({at_300}) should exceed 60x ({at_60})");
        assert!(at_900 > at_300, "900x ({at_900}) should exceed 300x ({at_300})");
    }

    #[test]
    fn the_trail_is_capped_at_a_few_revolutions_of_the_orbit_it_draws() {
        // A short enough period that 1800x's 360-simulated-minute shutter
        // actually overruns the cap (unlike the ISS, whose own period keeps
        // it just under at the top of the real warp ladder).
        let period = 60.0;
        let (before, _, _) = trail_windows(ClockState::Warp(1800), period, 1.0);
        assert_eq!(before, Duration::minutes((TRAIL_MAX_REVS * period) as i64));
    }

    #[test]
    fn a_geostationary_period_caps_the_trail_far_later_than_a_low_orbit_one() {
        let geo_period = 1436.1;
        // A rate far past anything on the real warp ladder, so both orbits'
        // caps are actually reached rather than merely approached.
        let extreme = ClockState::Warp(100_000);
        let (leo_before, _, _) = trail_windows(extreme, ISS_PERIOD_MIN, 1.0);
        let (geo_before, _, _) = trail_windows(extreme, geo_period, 1.0);
        assert_eq!(leo_before, Duration::milliseconds((TRAIL_MAX_REVS * ISS_PERIOD_MIN * 60_000.0) as i64));
        assert_eq!(geo_before, Duration::milliseconds((TRAIL_MAX_REVS * geo_period * 60_000.0) as i64));
        assert!(geo_before > leo_before * 10, "a GEO cap should dwarf a LEO one, got {geo_before} vs {leo_before}");
    }

    #[test]
    fn the_trail_step_keeps_its_point_count_bounded_as_the_window_grows() {
        let (before, _, step) = trail_windows(ClockState::Warp(1800), ISS_PERIOD_MIN, 1.0);
        let points = before.num_milliseconds() / step.num_milliseconds() + 1;
        assert!(points <= 401, "expected the point budget to hold, got {points}");
    }

    #[test]
    fn a_reverse_warp_stretches_the_leading_window_instead_of_the_trailing_one() {
        let (before, after, _) = trail_windows(ClockState::Warp(-900), ISS_PERIOD_MIN, 1.0);
        assert_eq!(before, Duration::minutes(35), "the past window should stay at its own base");
        assert!(after > Duration::minutes(65), "the future window should be the one stretching");
    }

    #[test]
    fn an_acquisition_reveal_shortens_both_windows_from_the_satellite_outward() {
        let (before, after, _) = trail_windows(ClockState::Live, ISS_PERIOD_MIN, 0.0);
        assert_eq!(before, Duration::zero());
        assert_eq!(after, Duration::zero());

        let (before, after, _) = trail_windows(ClockState::Live, ISS_PERIOD_MIN, 0.5);
        assert_eq!(before, Duration::milliseconds(35 * 60_000 / 2));
        assert_eq!(after, Duration::milliseconds(65 * 60_000 / 2));
    }

    #[test]
    fn a_complete_reveal_leaves_the_windows_exactly_where_an_unscrubbed_clock_has_them() {
        assert_eq!(
            trail_windows(ClockState::Live, ISS_PERIOD_MIN, 1.0),
            trail_windows(ClockState::Live, ISS_PERIOD_MIN, 1.0),
        );
        let (before, after, step) = trail_windows(ClockState::Live, ISS_PERIOD_MIN, 1.0);
        assert_eq!((before, after, step), (Duration::minutes(35), Duration::minutes(65), Duration::seconds(20)));
    }
}
