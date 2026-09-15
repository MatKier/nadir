//! Pure, stateless helpers for the handful of places the dashboard now moves
//! on its own — the star twinkle, the aurora shimmer, the AOS pulse. Nothing
//! here holds a clock or a frame counter: every function takes a phase (see
//! [`crate::app::App::uptime`]) and returns the same answer for the same
//! phase every time, which is what makes each one a one-line test rather than
//! something that has to be watched on screen to believe.
//!
//! Deliberately *not* `crate::orbit::sim_now()`-driven: that clock pauses,
//! reverses and jumps, and a shimmer or a twinkle that did the same would
//! read as broken rather than paused. `App::uptime()` is wall time since the
//! session started, immune to all of that, so it's the phase source for
//! everything in this module.

use std::time::Duration;

use ratatui::style::Color;

/// A 0..1 triangle wave with the given period in seconds — up for the first
/// half, back down for the second, so it has no discontinuity at the wrap.
/// `period <= 0.0` is treated as always at the trough (`0.0`) rather than
/// dividing by zero.
pub(in crate::ui) fn pulse(phase: f32, period: f32) -> f32 {
    if period <= 0.0 {
        return 0.0;
    }
    let t = (phase.rem_euclid(period)) / period; // 0..1 across one period
    if t < 0.5 {
        t * 2.0
    } else {
        2.0 - t * 2.0
    }
}

/// Deterministic value noise in `0..1` from an integer seed and a time phase.
/// Not a PRNG stream — there is no state to advance, so the same `(seed,
/// phase)` pair always returns the same value, which is what lets a caller
/// like the star twinkle or the aurora shimmer reconstruct "what was drawn
/// last frame" without keeping anything around. Smoothly interpolated between
/// integer phase steps so it reads as a drift rather than a strobe.
pub(in crate::ui) fn noise(seed: u32, phase: f32) -> f32 {
    let p = phase.max(0.0);
    let i0 = p.floor() as u32;
    let i1 = i0.wrapping_add(1);
    let frac = p.fract();
    // Smoothstep, not linear, so consecutive integer steps don't kink.
    let t = frac * frac * (3.0 - 2.0 * frac);
    hash01(seed, i0) * (1.0 - t) + hash01(seed, i1) * t
}

/// One integer step of the hash `noise` interpolates between — exposed on its
/// own for callers that want a flat per-slot coin flip rather than a value
/// that drifts smoothly as the phase moves through a slot (`skyplot`'s meteor
/// spawn roll: a meteor must not fade in from nothing as the phase crosses
/// into its slot, so whether the slot spawns one at all has to be a step, not
/// an interpolation). A cheap integer mix (splitmix64-style), not a
/// cryptographic hash — it only has to look unpatterned across a few thousand
/// `(seed, step)` pairs, not resist analysis.
pub(in crate::ui) fn hash01(seed: u32, step: u32) -> f32 {
    let mut x = (seed as u64) ^ ((step as u64).wrapping_mul(0x9E3779B97F4A7C15));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58476D1CE4E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D049BB133111EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// Linear interpolation between two colours at `t` in `0..=1` (values outside
/// that range extrapolate rather than panic — callers that clamp their own
/// `t` don't pay for a second clamp here). Only `Color::Rgb` pairs blend;
/// anything else — a named ANSI colour, `Reset` — falls back to `b` at `t >=
/// 0.5` and `a` otherwise, since there's no channel to interpolate.
pub(in crate::ui) fn lerp(a: Color, b: Color, t: f32) -> Color {
    match (a, b) {
        (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) => Color::Rgb(
            lerp_channel(ar, br, t),
            lerp_channel(ag, bg, t),
            lerp_channel(ab, bb, t),
        ),
        _ => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

fn lerp_channel(a: u8, b: u8, t: f32) -> u8 {
    let v = a as f32 + (b as f32 - a as f32) * t;
    v.round().clamp(0.0, 255.0) as u8
}

/// Eased 0..1 progress through a reveal of length `span`, `elapsed` since it
/// started — the acquisition sweep's ground track and footprint bloom both
/// scale their geometry by this fraction (`ui::track::track_scene`). Square-
/// root eased so the early frames cover most of the distance: at
/// `FRAME_ANIM`'s 100 ms an 800 ms reveal is only eight frames, and a linear
/// ramp over eight steps reads as stepping rather than sweeping — easing the
/// front-loads the motion into more of those few frames. A zero-length span
/// reads as already complete rather than dividing by zero, the same guard
/// [`pulse`] makes for a non-positive period.
pub(in crate::ui) fn reveal(elapsed: Duration, span: Duration) -> f32 {
    if span.is_zero() {
        return 1.0;
    }
    let t = (elapsed.as_secs_f32() / span.as_secs_f32()).clamp(0.0, 1.0);
    t.sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_starts_and_ends_each_period_at_the_trough() {
        assert_eq!(pulse(0.0, 2.0), 0.0);
        assert_eq!(pulse(2.0, 2.0), 0.0); // wraps to the next period's start
        assert_eq!(pulse(4.0, 2.0), 0.0);
    }

    #[test]
    fn pulse_peaks_at_the_period_midpoint() {
        let v = pulse(1.0, 2.0);
        assert!((v - 1.0).abs() < 1e-6, "expected the midpoint at 1.0, got {v}");
    }

    #[test]
    fn pulse_stays_in_bounds_across_many_periods() {
        for i in 0..1000 {
            let v = pulse(i as f32 * 0.037, 2.3);
            assert!((0.0..=1.0).contains(&v), "pulse({}) = {v} out of range", i as f32 * 0.037);
        }
    }

    #[test]
    fn pulse_with_a_nonpositive_period_never_divides_by_zero() {
        assert_eq!(pulse(5.0, 0.0), 0.0);
        assert_eq!(pulse(5.0, -1.0), 0.0);
    }

    #[test]
    fn noise_is_deterministic_for_the_same_seed_and_phase() {
        assert_eq!(noise(7, 1.25), noise(7, 1.25));
        assert_eq!(noise(0, 0.0), noise(0, 0.0));
    }

    #[test]
    fn noise_differs_across_seeds_at_the_same_phase() {
        // Not guaranteed for every pair by construction, but true for enough
        // of a sample that a broken hash collapsing everything to one value
        // would fail this.
        let values: Vec<f32> = (0..16).map(|s| noise(s, 3.0)).collect();
        let distinct = values
            .iter()
            .enumerate()
            .filter(|(i, v)| values[..*i].iter().all(|o| (o - *v).abs() > 1e-6))
            .count();
        assert!(distinct > 8, "expected most of 16 seeds to differ, got {distinct} distinct");
    }

    #[test]
    fn noise_stays_in_bounds_across_a_long_phase_sweep() {
        for i in 0..2000 {
            let v = noise(42, i as f32 * 0.05);
            assert!((0.0..=1.0).contains(&v), "noise at step {i} = {v} out of range");
        }
    }

    #[test]
    fn noise_is_continuous_across_an_integer_phase_boundary() {
        // Smoothstep interpolation means a tiny phase step near an integer
        // boundary should not produce a large jump — the failure mode of a
        // naive "just index into a hash table" implementation.
        let just_before = noise(3, 4.999);
        let just_after = noise(3, 5.001);
        assert!(
            (just_before - just_after).abs() < 0.05,
            "expected continuity across the boundary, got {just_before} vs {just_after}"
        );
    }

    #[test]
    fn lerp_hits_both_endpoints_exactly() {
        let a = Color::Rgb(10, 20, 30);
        let b = Color::Rgb(200, 100, 50);
        assert_eq!(lerp(a, b, 0.0), a);
        assert_eq!(lerp(a, b, 1.0), b);
    }

    #[test]
    fn lerp_at_the_midpoint_averages_each_channel() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(100, 200, 50);
        assert_eq!(lerp(a, b, 0.5), Color::Rgb(50, 100, 25));
    }

    #[test]
    fn lerp_falls_back_to_an_endpoint_for_a_non_rgb_colour() {
        let a = Color::Reset;
        let b = Color::Rgb(10, 20, 30);
        assert_eq!(lerp(a, b, 0.9), b);
        assert_eq!(lerp(a, b, 0.1), a);
    }

    #[test]
    fn a_reveal_reaches_one_exactly_at_its_span() {
        let span = Duration::from_millis(800);
        assert_eq!(reveal(span, span), 1.0);
    }

    #[test]
    fn a_reveal_is_monotonic_across_its_span() {
        let span = Duration::from_millis(800);
        let mut last = 0.0;
        for ms in 0..=800u64 {
            let v = reveal(Duration::from_millis(ms), span);
            assert!(v >= last, "reveal dipped at {ms}ms: {v} < {last}");
            last = v;
        }
    }

    #[test]
    fn a_reveal_covers_most_of_its_distance_in_its_first_half() {
        // The whole point of the square-root ease over a linear one: at the
        // midpoint a linear ramp would read 0.5, but this should already be
        // well past it, since `FRAME_ANIM`'s 100ms steps leave only a
        // handful of frames to work with.
        let span = Duration::from_millis(800);
        let v = reveal(span / 2, span);
        assert!(v > 0.65, "expected the midpoint past 0.65, got {v}");
    }

    #[test]
    fn a_reveal_of_zero_span_is_complete_rather_than_dividing_by_zero() {
        assert_eq!(reveal(Duration::from_millis(100), Duration::ZERO), 1.0);
        assert_eq!(reveal(Duration::ZERO, Duration::ZERO), 1.0);
    }
}
