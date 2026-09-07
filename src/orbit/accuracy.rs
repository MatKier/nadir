//! A modelled position-accuracy estimate for an SGP4 element set.
//!
//! A TLE carries no covariance — SGP4 returns a position, not an error bar —
//! so any accuracy figure is a model, not a readout. This one is built from
//! the two things the element set *does* say about its own quality:
//!
//!  * **B\*** (`sgp4::Elements::drag_term`), the fitted drag coefficient. It is
//!    a lumped parameter that soaks up everything the atmosphere model gets
//!    wrong, and it routinely shifts by tens of percent between consecutive
//!    fits of the same object. Re-propagating with B\* nudged by
//!    [`BSTAR_REL_UNCERTAINTY`] and measuring how far the position moves gives
//!    a genuinely satellite-specific along-track error — a decaying ISS reads
//!    far worse than a stable high sun-sync bird at the same element age, with
//!    no lookup table deciding that.
//!  * the **orbital regime**, which sets a floor. A deep-space element set
//!    often carries `B* = 0`, so the drag perturbation is identically zero and,
//!    left to itself, the model would claim perfect knowledge of a GEO. GEO
//!    error is real; it is solar radiation pressure and stationkeeping
//!    manoeuvres, neither of which SGP4 models at all. Hence the floor is a
//!    `max`, not an added term.
//!
//! None of this touches the network or the wall clock — it is pure math over an
//! `sgp4::Constants`, like the rest of `orbit/`.

use crate::orbit::propagate::OrbitClass;

/// How far B\* is scaled to probe drag sensitivity: +10%, a deliberately
/// conservative read of how much a fitted B\* typically moves between
/// consecutive element sets for the same object. `Tracker` builds one extra
/// propagator at this perturbation and `Tracker::accuracy_at` differences the
/// two positions.
pub const BSTAR_REL_UNCERTAINTY: f64 = 0.10;

/// `|Δt|` from epoch, in days, past which the mean elements have stopped
/// describing the orbit well enough for a modelled error to mean anything.
/// SGP4 will still return a position out here — it is just wrong by an amount
/// nothing local can bound — so past this the estimate declines to quote a
/// number. This is a statement about the fit, not about how large σ has grown,
/// which is why it is keyed off time from epoch rather than off `total_km`.
const VALIDITY_CEILING_DAYS: f64 = 30.0;
/// Within this many days of epoch the estimate is trustworthy.
const NOMINAL_CEILING_DAYS: f64 = 3.0;
/// Out to here the number is still a fair guide; beyond it, an
/// order-of-magnitude figure at best.
const DEGRADED_CEILING_DAYS: f64 = 14.0;

/// Per-regime `(σ₀, k)`: the roughly constant radial/cross-track error in km,
/// and the linear along-track growth rate in km per day of `|Δt|` from epoch.
/// This pair is the floor the measured B\*-sensitivity term is `max`'d
/// against, not an alternative to it.
///
/// The values are the community rules of thumb for TLE accuracy (CelesTrak /
/// Kelso, and Vallado's revisiting-SGP4 error studies): about a kilometre at
/// epoch for LEO, a couple of km/day of along-track drift as the drag fit
/// ages. MEO and above get a *larger* growth rate than LEO despite having no
/// drag to speak of, because their dominant error source is an un-modelled
/// stationkeeping burn — rarer than drag, but a step change when it happens,
/// so a wider linear band is the honest summary.
fn regime_error_model(class: OrbitClass) -> (f64, f64) {
    match class {
        OrbitClass::Leo => (1.0, 1.5),
        OrbitClass::Heo => (2.0, 2.0),
        OrbitClass::Meo => (2.0, 3.0),
        OrbitClass::Geo | OrbitClass::Gso | OrbitClass::High => (3.0, 3.0),
    }
}

/// How far from the element-set epoch the displayed time has been scrubbed,
/// expressed as trust in the resulting position. Keyed off the *magnitude* of
/// the offset: a week before epoch is exactly as far from the fit as a week
/// after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Within [`NOMINAL_CEILING_DAYS`] of epoch.
    Nominal,
    /// Days to a fortnight out — the orbit has had time to wander from the fit.
    Degraded,
    /// Weeks out — quote the figure, but it is order-of-magnitude now.
    Unreliable,
    /// Past [`VALIDITY_CEILING_DAYS`] — the model declines to put a number on
    /// it.
    Unmodelled,
}

impl Confidence {
    fn from_offset(offset: chrono::Duration) -> Self {
        let days = offset.num_seconds().abs() as f64 / 86_400.0;
        if days <= NOMINAL_CEILING_DAYS {
            Confidence::Nominal
        } else if days <= DEGRADED_CEILING_DAYS {
            Confidence::Degraded
        } else if days <= VALIDITY_CEILING_DAYS {
            Confidence::Unreliable
        } else {
            Confidence::Unmodelled
        }
    }
}

/// A modelled position-error estimate for one instant. All distances are km,
/// all one-sigma-ish — this is a "the truth is probably within this" figure,
/// not a rigorous covariance.
#[derive(Debug, Clone, Copy)]
pub struct Accuracy {
    /// Modelled along-track (down-range) position error — the dominant
    /// component, and the one that grows with time from epoch.
    pub along_km: f64,
    /// [`along_km`](Self::along_km) and the constant radial/cross-track floor
    /// combined in quadrature: the figure to quote as the overall position
    /// error.
    pub total_km: f64,
    /// [`along_km`](Self::along_km) as a timing error — how far ahead of or
    /// behind schedule the satellite is along its own track. This is what
    /// turns into ± seconds on a predicted rise or set time.
    pub timing_s: f64,
    pub confidence: Confidence,
}

impl Accuracy {
    /// Assemble an estimate from the regime, the signed offset from epoch, the
    /// measured B\*-sensitivity offset (`None` when the perturbed propagation
    /// couldn't be run), and the satellite's inertial speed for the timing
    /// conversion.
    ///
    /// `drag_offset_km` is a full 3-D position difference, but a B\*
    /// perturbation acts almost entirely down-range — drag changes the mean
    /// motion, so the satellite runs ahead of or behind where it should be —
    /// so charging the whole delta to the along-track term is fair and, if
    /// anything, slightly conservative.
    pub fn model(
        class: OrbitClass,
        offset_from_epoch: chrono::Duration,
        drag_offset_km: Option<f64>,
        speed_kms: f64,
    ) -> Self {
        let (sigma0_km, k_km_per_day) = regime_error_model(class);
        let days = offset_from_epoch.num_seconds().abs() as f64 / 86_400.0;
        let floor_km = k_km_per_day * days;
        let along_km = floor_km.max(drag_offset_km.unwrap_or(0.0));
        let total_km = (sigma0_km * sigma0_km + along_km * along_km).sqrt();
        // `speed_kms` is an SGP4 inertial speed — a handful of km/s for any
        // real orbit — but a caller with no state yet passes 0, and that must
        // come back 0, not a division by zero.
        let timing_s = if speed_kms > 0.0 { along_km / speed_kms } else { 0.0 };
        Self {
            along_km,
            total_km,
            timing_s,
            confidence: Confidence::from_offset(offset_from_epoch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orbit::{test_geo_zero_drag_tracker, test_sso_tracker, test_tracker};
    use chrono::Duration;

    /// A speed to feed the timing conversion in tests that don't care about it.
    const ISS_SPEED_KMS: f64 = 7.66;

    #[test]
    fn accuracy_at_epoch_is_about_a_kilometre_for_the_iss() {
        let tr = test_tracker();
        let acc = tr.accuracy_at(tr.epoch(), ISS_SPEED_KMS);

        // At epoch the drag perturbation and the linear floor are both zero, so
        // all that is left is the LEO radial/cross-track constant.
        assert!(
            (0.5..2.0).contains(&acc.total_km),
            "epoch estimate {:.2} km is not ~1 km",
            acc.total_km
        );
        assert!(acc.timing_s < 0.5, "epoch timing error {:.2} s should be ~0", acc.timing_s);
        assert_eq!(acc.confidence, Confidence::Nominal);
    }

    #[test]
    fn a_week_from_epoch_the_iss_estimate_is_tens_of_kilometres() {
        // The claim README and the `?` overlay have always made in prose,
        // pinned to code: "roughly a kilometre near the epoch, growing to tens
        // of kilometres after a week in low Earth orbit".
        let tr = test_tracker();
        let acc = tr.accuracy_at(tr.epoch() + Duration::days(7), ISS_SPEED_KMS);

        assert!(
            (8.0..120.0).contains(&acc.total_km),
            "a week out the estimate is {:.1} km, expected tens of km",
            acc.total_km
        );
        assert!(acc.timing_s > 1.0, "a week out the timing error should be seconds, not {:.2} s", acc.timing_s);
        assert_eq!(acc.confidence, Confidence::Degraded);
    }

    #[test]
    fn a_low_drag_sun_synchronous_orbit_degrades_more_slowly_than_the_iss() {
        // Sentinel-2A's B* (1.41e-5) is far below the ISS's (6.93e-5), and it
        // sits ~300 km higher where the air is thinner still. Near epoch both
        // ride the shared LEO floor; two weeks out the ISS's drag sensitivity
        // has lifted it well clear of that floor while the SSO is still on it.
        // Drop the B* probe for a flat per-regime table and the two match and
        // this fails.
        let dt = Duration::days(14);
        let iss = test_tracker();
        let sso = test_sso_tracker();
        let iss_acc = iss.accuracy_at(iss.epoch() + dt, ISS_SPEED_KMS);
        let sso_acc = sso.accuracy_at(sso.epoch() + dt, ISS_SPEED_KMS);

        assert!(
            sso_acc.along_km < iss_acc.along_km - 3.0,
            "SSO along-track {:.1} km should sit well below the ISS's {:.1} km",
            sso_acc.along_km,
            iss_acc.along_km
        );
    }

    #[test]
    fn a_zero_drag_element_set_still_reports_a_growing_error() {
        // A synthetic GEO with BSTAR exactly 0: the B* probe measures nothing,
        // so only the per-regime floor is left — and it must still grow with
        // time rather than sit at the epoch constant forever.
        let tr = test_geo_zero_drag_tracker();
        let near = tr.accuracy_at(tr.epoch() + Duration::days(1), 3.07);
        let far = tr.accuracy_at(tr.epoch() + Duration::days(10), 3.07);

        assert!(far.total_km > near.total_km + 10.0, "the floor did not grow: {near:?} -> {far:?}");
    }

    #[test]
    fn the_estimate_is_symmetric_about_the_epoch() {
        let tr = test_tracker();
        let before = tr.accuracy_at(tr.epoch() - Duration::days(3), ISS_SPEED_KMS);
        let after = tr.accuracy_at(tr.epoch() + Duration::days(3), ISS_SPEED_KMS);

        assert!(
            (before.total_km - after.total_km).abs() < 1.0,
            "±3 d should read alike: {:.2} km vs {:.2} km",
            before.total_km,
            after.total_km
        );
    }

    #[test]
    fn the_estimate_grows_monotonically_with_distance_from_epoch() {
        let tr = test_tracker();
        let mut last = 0.0;
        for day in 0..15 {
            let acc = tr.accuracy_at(tr.epoch() + Duration::days(day), ISS_SPEED_KMS);
            assert!(
                acc.total_km >= last - 1e-6,
                "estimate dropped at day {day}: {:.3} km after {last:.3} km",
                acc.total_km
            );
            last = acc.total_km;
        }
    }

    #[test]
    fn past_the_validity_ceiling_the_model_declines_to_quote_a_number() {
        let tr = test_tracker();
        let acc = tr.accuracy_at(tr.epoch() + Duration::days(40), ISS_SPEED_KMS);
        assert_eq!(acc.confidence, Confidence::Unmodelled);
    }
}
