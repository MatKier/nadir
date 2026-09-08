//! Overhead-pass prediction for a fixed ground station.
//!
//! Coarse-scan elevation, bracket each rise above the horizon, then refine the
//! acquisition/loss instants by bisection and the culmination by golden-section
//! search. Everything is local SGP4 — no network.

use chrono::{DateTime, Duration, Utc};

use crate::geo::{look_angles, GeoPoint};
use crate::orbit::propagate::Tracker;
use crate::orbit::solar::solar_elevation_deg;

/// Minimum peak elevation for a pass to be worth listing.
const MIN_PEAK_ELEVATION_DEG: f64 = 10.0;
/// Observer counts as "in the dark" for a visible pass below this Sun elevation.
const OBSERVER_DARK_BELOW_DEG: f64 = -6.0;

/// One predicted pass of the satellite over the ground station.
#[derive(Debug, Clone)]
pub struct Pass {
    /// Acquisition of signal: satellite rises through 0° elevation.
    pub aos: DateTime<Utc>,
    /// Loss of signal: satellite sets through 0° elevation.
    pub los: DateTime<Utc>,
    /// Instant of highest elevation — the culmination the sky plot marks.
    pub peak: DateTime<Utc>,
    /// Highest elevation reached, in degrees.
    pub peak_elevation_deg: f64,
    /// Azimuth at AOS and at LOS, in degrees.
    pub aos_azimuth_deg: f64,
    pub los_azimuth_deg: f64,
    /// True when the satellite is sunlit at culmination while the observer is in
    /// darkness — i.e. the pass should actually be visible to the naked eye.
    pub visible: bool,
}

impl Pass {
    pub fn duration(&self) -> Duration {
        self.los - self.aos
    }
}

/// One instant along a pass as seen from the ground station: where to point,
/// and whether the satellite is catching sunlight there.
#[derive(Debug, Clone, Copy)]
pub struct SkySample {
    /// The instant this sample was propagated to. Kept for callers that mark a
    /// live position on the arc; the sky plot itself reads only the angles.
    #[allow(dead_code)]
    pub at: DateTime<Utc>,
    /// Azimuth in degrees clockwise from true north, in `[0, 360)`.
    pub azimuth_deg: f64,
    /// Elevation above the horizon in degrees; negative means below it.
    pub elevation_deg: f64,
    /// Whether the satellite is sunlit at this instant. `Pass::visible` only
    /// samples illumination at culmination, so this is the field that lets a
    /// plot show the moment a naked-eye pass crosses into Earth's shadow.
    pub sunlit: bool,
}

/// Look angles and illumination at a single instant, or `None` when the
/// propagator refuses that time (out of range, or SGP4 divergence). Callers
/// drop the gap rather than draw a false point — the same policy
/// [`Tracker::ground_track`] uses for a failed step.
pub fn sky_sample(
    tracker: &Tracker,
    station: &GeoPoint,
    t: DateTime<Utc>,
) -> Option<SkySample> {
    let state = tracker.state_at(t).ok()?;
    let look = look_angles(station, state.ecef_km);
    Some(SkySample {
        at: t,
        azimuth_deg: look.azimuth_deg,
        elevation_deg: look.elevation_deg,
        sunlit: state.sunlit,
    })
}

/// Sample the arc of `pass` at `steps` equal time intervals, both endpoints
/// included — so up to `steps + 1` points, fewer when a step fails to
/// propagate and is dropped (leaving a gap in the arc, never a spurious
/// point). Cheap enough — a hundred-odd SGP4 calls — to run per frame with no
/// cache; contrast the full multi-day pass scan in [`predict_passes`].
pub fn sample_pass(
    tracker: &Tracker,
    station: &GeoPoint,
    pass: &Pass,
    steps: usize,
) -> Vec<SkySample> {
    let steps = steps.max(1) as i64;
    let span_ms = (pass.los - pass.aos).num_milliseconds();
    (0..=steps)
        .filter_map(|i| {
            let offset = Duration::milliseconds(span_ms * i / steps);
            sky_sample(tracker, station, pass.aos + offset)
        })
        .collect()
}

fn elevation_at(tracker: &Tracker, station: &GeoPoint, t: DateTime<Utc>) -> f64 {
    match tracker.state_at(t) {
        Ok(state) => look_angles(station, state.ecef_km).elevation_deg,
        Err(_) => -90.0,
    }
}

/// Predict passes with peak elevation above [`MIN_PEAK_ELEVATION_DEG`] that
/// begin within `horizon` of `from`.
pub fn predict_passes(
    tracker: &Tracker,
    station: &GeoPoint,
    from: DateTime<Utc>,
    horizon: Duration,
    max_results: usize,
) -> Vec<Pass> {
    let coarse = Duration::seconds(30);
    let end = from + horizon;

    let mut passes = Vec::new();
    let mut t = from;
    let mut prev_el = elevation_at(tracker, station, t);

    while t < end && passes.len() < max_results {
        let next = t + coarse;
        let el = elevation_at(tracker, station, next);

        // Rising edge through the horizon: a pass has begun.
        if prev_el < 0.0 && el >= 0.0 {
            let aos = bisect_crossing(tracker, station, t, next, true);

            // Walk forward to the setting edge.
            let mut a = next;
            let mut ea = el;
            let mut set_from = a;
            let mut set_to = a;
            while a < end {
                let b = a + coarse;
                let eb = elevation_at(tracker, station, b);
                if ea >= 0.0 && eb < 0.0 {
                    set_from = a;
                    set_to = b;
                    break;
                }
                a = b;
                ea = eb;
            }
            let los = if set_to > set_from {
                bisect_crossing(tracker, station, set_from, set_to, false)
            } else {
                a // pass runs past the horizon window; clamp
            };

            let (peak, peak_el) = golden_section_peak(tracker, station, aos, los);

            if peak_el >= MIN_PEAK_ELEVATION_DEG {
                let sat_sunlit = tracker
                    .state_at(peak)
                    .map(|s| s.sunlit)
                    .unwrap_or(false);
                let observer_dark = solar_elevation_deg(station, peak) < OBSERVER_DARK_BELOW_DEG;
                passes.push(Pass {
                    aos,
                    los,
                    peak,
                    peak_elevation_deg: peak_el,
                    aos_azimuth_deg: azimuth_at(tracker, station, aos),
                    los_azimuth_deg: azimuth_at(tracker, station, los),
                    visible: sat_sunlit && observer_dark,
                });
            }

            // Resume scanning just after this pass.
            t = los + coarse;
            prev_el = elevation_at(tracker, station, t);
            continue;
        }

        prev_el = el;
        t = next;
    }

    passes
}

fn azimuth_at(tracker: &Tracker, station: &GeoPoint, t: DateTime<Utc>) -> f64 {
    tracker
        .state_at(t)
        .map(|s| look_angles(station, s.ecef_km).azimuth_deg)
        .unwrap_or(0.0)
}

/// Bisect the horizon crossing bracketed by `lo` and `hi`, to ~1 s. `rising`
/// says which way the elevation passes through zero: at AOS the satellite is
/// below the horizon at `lo` and above it at `hi`, at LOS the other way round.
///
/// It has to be told, rather than inferring from the endpoints, because each
/// step keeps whichever half still contains the crossing — and that is the
/// *opposite* half in the two cases. Bisecting a set as though it were a rise
/// throws the crossing away on the first iteration and then walks to an end of
/// the bracket, which is exactly what used to leave LOS (and so every pass
/// duration, and the arc's end on the sky plot) wrong by up to the caller's
/// 30 s coarse step, in whichever direction the coarse grid happened to fall.
fn bisect_crossing(
    tracker: &Tracker,
    station: &GeoPoint,
    mut lo: DateTime<Utc>,
    mut hi: DateTime<Utc>,
    rising: bool,
) -> DateTime<Utc> {
    for _ in 0..24 {
        if (hi - lo) <= Duration::seconds(1) {
            break;
        }
        let mid = lo + (hi - lo) / 2;
        // On a rise, `mid` already above the horizon puts the crossing behind
        // it; on a set, above puts the crossing still ahead of it.
        if (elevation_at(tracker, station, mid) >= 0.0) == rising {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    lo + (hi - lo) / 2
}

/// Golden-section search for the elevation maximum within `[a, b]`.
fn golden_section_peak(
    tracker: &Tracker,
    station: &GeoPoint,
    a: DateTime<Utc>,
    b: DateTime<Utc>,
) -> (DateTime<Utc>, f64) {
    const INV_PHI: f64 = 0.618_033_988_749_895;
    let (mut lo, mut hi) = (a, b);
    let span = |lo: DateTime<Utc>, hi: DateTime<Utc>| hi - lo;

    let lerp = |lo: DateTime<Utc>, hi: DateTime<Utc>, frac: f64| {
        lo + Duration::milliseconds((span(lo, hi).num_milliseconds() as f64 * frac) as i64)
    };

    let mut c = lerp(lo, hi, 1.0 - INV_PHI);
    let mut d = lerp(lo, hi, INV_PHI);
    let mut fc = elevation_at(tracker, station, c);
    let mut fd = elevation_at(tracker, station, d);

    for _ in 0..40 {
        if span(lo, hi) <= Duration::seconds(1) {
            break;
        }
        if fc > fd {
            hi = d;
            d = c;
            fd = fc;
            c = lerp(lo, hi, 1.0 - INV_PHI);
            fc = elevation_at(tracker, station, c);
        } else {
            lo = c;
            c = d;
            fc = fd;
            d = lerp(lo, hi, INV_PHI);
            fd = elevation_at(tracker, station, d);
        }
    }

    let peak = lo + span(lo, hi) / 2;
    (peak, elevation_at(tracker, station, peak))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orbit::test_tracker;

    #[test]
    fn iss_produces_several_well_formed_passes_over_a_mid_latitude_site() {
        let tr = test_tracker();
        let munich = GeoPoint::new(48.137, 11.575, 0.52);
        let passes = predict_passes(&tr, &munich, tr.epoch(), Duration::hours(48), 20);

        assert!(
            (3..=25).contains(&passes.len()),
            "expected a handful of ISS passes in 48 h, got {}",
            passes.len()
        );
        for p in &passes {
            assert!(p.los > p.aos, "LOS must follow AOS");
            assert!(p.peak >= p.aos && p.peak <= p.los, "peak lies within the pass");
            assert!(
                p.peak_elevation_deg >= MIN_PEAK_ELEVATION_DEG
                    && p.peak_elevation_deg <= 90.0,
                "peak elevation {:.1} out of range",
                p.peak_elevation_deg
            );
            assert!(p.duration() <= Duration::minutes(12), "ISS passes are short");
            assert!((0.0..360.0).contains(&p.aos_azimuth_deg));
        }
    }

    /// The premise `App::refresh_passes`'s look-back rests on: a scan only
    /// records a pass where it sees the satellite *rise*, so a pass already
    /// under way at the scan start is invisible — and starting the scan before
    /// that rise is what brings it back.
    #[test]
    fn a_scan_starting_mid_pass_misses_it_but_one_starting_before_the_rise_does_not() {
        let tr = test_tracker();
        let munich = GeoPoint::new(48.137, 11.575, 0.52);
        let passes = predict_passes(&tr, &munich, tr.epoch(), Duration::hours(48), 20);
        let pass = passes.first().expect("at least one ISS pass in 48 h").clone();

        // Scanning from inside the pass: its rise is behind us, so it's gone.
        let midpoint = pass.aos + (pass.los - pass.aos) / 2;
        let from_mid = predict_passes(&tr, &munich, midpoint, Duration::hours(48), 20);
        assert!(
            from_mid.iter().all(|p| p.aos > midpoint),
            "a scan from mid-pass cannot see the pass it is inside",
        );

        // Backing the start up past the rise finds it again, unchanged.
        let lookback = Duration::minutes(tr.orbit_shape().period_min.round() as i64);
        let from_before = predict_passes(&tr, &munich, midpoint - lookback, Duration::hours(48), 20);
        let found = from_before
            .iter()
            .find(|p| p.aos <= midpoint && p.los > midpoint)
            .expect("one revolution of look-back catches the pass under way");
        assert!(
            (found.aos - pass.aos).abs() < Duration::seconds(2),
            "the same rise, bisected to the same instant",
        );
        assert!(
            (found.los - pass.los).abs() < Duration::seconds(2),
            "and the same set — a LOS that moved with the coarse grid's phase \
             would mean `bisect_crossing` is not tracking the setting edge",
        );
    }

    /// Both ends of a pass are horizon crossings, so a correctly bisected AOS
    /// and LOS both land *on* 0° elevation. LOS used to be bisected with the
    /// polarity of a rise, which walked it to an end of the 30 s coarse step
    /// instead — leaving it a degree or two off the horizon, and up to half a
    /// minute out in time, in whichever direction the grid happened to fall.
    #[test]
    fn both_ends_of_a_pass_are_bisected_onto_the_horizon() {
        let tr = test_tracker();
        let munich = GeoPoint::new(48.137, 11.575, 0.52);
        let passes = predict_passes(&tr, &munich, tr.epoch(), Duration::hours(48), 20);
        assert!(!passes.is_empty(), "the fixture must yield passes to check");

        for p in &passes {
            let aos_el = elevation_at(&tr, &munich, p.aos);
            let los_el = elevation_at(&tr, &munich, p.los);
            assert!(aos_el.abs() < 0.5, "AOS sits at {aos_el:.3}°, not on the horizon");
            assert!(los_el.abs() < 0.5, "LOS sits at {los_el:.3}°, not on the horizon");
        }
    }

    #[test]
    fn a_sampled_pass_starts_and_ends_near_the_horizon_and_peaks_between_them() {
        let tr = test_tracker();
        let munich = GeoPoint::new(48.137, 11.575, 0.52);
        let passes = predict_passes(&tr, &munich, tr.epoch(), Duration::hours(48), 20);
        let pass = passes.first().expect("at least one ISS pass in 48 h");

        let arc = sample_pass(&tr, &munich, pass, 120);
        assert!(arc.len() >= 100, "most of 121 steps should propagate, got {}", arc.len());

        // The endpoints are the AOS/LOS horizon crossings, so they sit within
        // a fraction of a degree of 0°; the interior rises to the pass's
        // recorded peak elevation.
        assert!(arc.first().unwrap().elevation_deg.abs() < 1.0);
        assert!(arc.last().unwrap().elevation_deg.abs() < 1.0);
        let apex = arc.iter().map(|s| s.elevation_deg).fold(f64::MIN, f64::max);
        assert!(
            (apex - pass.peak_elevation_deg).abs() < 1.5,
            "sampled apex {apex:.1}° should match the pass's {:.1}°",
            pass.peak_elevation_deg,
        );
    }

    #[test]
    fn sampled_azimuths_agree_with_the_pass_summary_at_aos_and_los() {
        let tr = test_tracker();
        let munich = GeoPoint::new(48.137, 11.575, 0.52);
        let passes = predict_passes(&tr, &munich, tr.epoch(), Duration::hours(48), 20);
        let pass = passes.first().expect("at least one ISS pass in 48 h");

        let arc = sample_pass(&tr, &munich, pass, 120);
        // Compare on the circle — a few degrees' tolerance for the ~1 s gap
        // between the bisected crossing and the first/last sample.
        let near = |a: f64, b: f64| ((a - b + 540.0).rem_euclid(360.0) - 180.0).abs() < 4.0;
        assert!(
            near(arc.first().unwrap().azimuth_deg, pass.aos_azimuth_deg),
            "AOS azimuth {:.1} vs sampled {:.1}",
            pass.aos_azimuth_deg,
            arc.first().unwrap().azimuth_deg,
        );
        assert!(
            near(arc.last().unwrap().azimuth_deg, pass.los_azimuth_deg),
            "LOS azimuth {:.1} vs sampled {:.1}",
            pass.los_azimuth_deg,
            arc.last().unwrap().azimuth_deg,
        );
    }
}
