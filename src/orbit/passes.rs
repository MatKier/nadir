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
    /// Instant of highest elevation. Exposed for callers; not shown in the
    /// compact pass row.
    #[allow(dead_code)]
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
            let aos = bisect_crossing(tracker, station, t, next);

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
                bisect_crossing(tracker, station, set_from, set_to)
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

/// Bisect the horizon crossing between `lo` (below) and `hi` (above), to ~1 s.
fn bisect_crossing(
    tracker: &Tracker,
    station: &GeoPoint,
    mut lo: DateTime<Utc>,
    mut hi: DateTime<Utc>,
) -> DateTime<Utc> {
    for _ in 0..24 {
        if (hi - lo) <= Duration::seconds(1) {
            break;
        }
        let mid = lo + (hi - lo) / 2;
        if elevation_at(tracker, station, mid) >= 0.0 {
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
}
