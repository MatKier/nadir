//! SGP4 propagation and the derived ground position.

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};

use crate::geo::{dot, ecef_to_geodetic, norm, split_at_antimeridian, teme_to_ecef, GeoPoint};
use crate::orbit::accuracy::{self, Accuracy};
use crate::orbit::solar::{subsolar_point, sun_ecef_unit};

/// Everything nadir needs about the satellite at one instant.
#[derive(Debug, Clone)]
pub struct SatState {
    /// The instant this state was propagated to. Kept for callers that log or
    /// extrapolate; the panels read "now" directly.
    #[allow(dead_code)]
    pub time: DateTime<Utc>,
    /// Sub-satellite point (geodetic latitude/longitude and orbital altitude).
    pub sub_point: GeoPoint,
    /// ECEF position in kilometres.
    pub ecef_km: [f64; 3],
    /// Speed relative to the rotating Earth is *not* what we report; this is the
    /// inertial speed magnitude from SGP4, in km/s.
    pub speed_kms: f64,
    /// Radius of the visibility footprint circle on the ground, in kilometres.
    pub footprint_km: f64,
    /// True when the satellite is in sunlight (not in Earth's shadow).
    pub sunlit: bool,
    /// Approximate revolution number since launch.
    pub revolution: u64,
}

/// Broad orbital regime, as classified from the element set alone — no
/// propagation needed, since it only depends on the orbit's fixed shape.
/// This is altitude/eccentricity only; which plane the orbit sits in
/// (equatorial, polar, sun-synchronous, …) is an orthogonal property, see
/// `OrbitPlane` — a regime is never lost just because a satellite is also
/// polar or sun-synchronous; both are reported side by side, see
/// `OrbitShape::label`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrbitClass {
    Leo,
    Meo,
    Geo,
    Gso,
    Heo,
    /// Near-circular and above the geostationary belt — a graveyard orbit, a
    /// high-altitude science orbit, or similar. Distinct from `Heo`, which is
    /// about eccentricity, not altitude; see `classify`'s doc comment.
    High,
}

impl OrbitClass {
    pub fn label(self) -> &'static str {
        match self {
            OrbitClass::Leo => "LEO",
            OrbitClass::Meo => "MEO",
            OrbitClass::Geo => "GEO",
            OrbitClass::Gso => "GSO",
            OrbitClass::Heo => "HEO",
            OrbitClass::High => "HIGH",
        }
    }
}

/// Which plane the orbit sits in, independent of `OrbitClass`'s altitude/
/// eccentricity regime — a polar MEO and a polar LEO are both `Polar`, just as
/// a sun-synchronous orbit is always a LEO (the J2 precession rate needed to
/// track the Sun only exists that close to Earth) but is worth calling out on
/// its own: folding it into the regime would mean losing the altitude/
/// eccentricity information a plain `OrbitClass` still carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrbitPlane {
    /// Nothing special about the inclination — the common case, so it earns
    /// no suffix in `OrbitShape::label`.
    Inclined,
    Polar,
    SunSync,
}

impl OrbitPlane {
    /// Celestrak SATCAT-style suffix (`LEO-P`, `LEO-S`, …), or `None` for the
    /// unremarkable case so `OrbitShape::label` can skip the dash entirely.
    pub fn suffix(self) -> Option<&'static str> {
        match self {
            OrbitPlane::Inclined => None,
            OrbitPlane::Polar => Some("P"),
            OrbitPlane::SunSync => Some("S"),
        }
    }
}

/// The time-invariant shape of the orbit: what the element set says about the
/// orbit itself, as opposed to `SatState`, which is where the satellite is at
/// one instant.
#[derive(Debug, Clone, Copy)]
pub struct OrbitShape {
    pub class: OrbitClass,
    pub plane: OrbitPlane,
    pub period_min: f64,
    pub inclination_deg: f64,
    pub eccentricity: f64,
    /// Altitude above the WGS-84 equatorial radius, in km — not the same
    /// reference surface as `SatState::sub_point`'s geodetic altitude above
    /// the local ellipsoid, so a current `ALT` outside `[perigee_km,
    /// apogee_km]` isn't necessarily a bug: away from the equator, WGS-84's
    /// flattening alone accounts for up to ~20 km of the gap.
    pub perigee_km: f64,
    /// Altitude above the WGS-84 equatorial radius, in km — see `perigee_km`.
    pub apogee_km: f64,
}

impl OrbitShape {
    /// The regime and plane combined into one label, e.g. `"LEO-P"`,
    /// `"LEO-S"`, or plain `"LEO"` when the plane is unremarkable.
    pub fn label(&self) -> String {
        match self.plane.suffix() {
            Some(suffix) => format!("{}-{suffix}", self.class.label()),
            None => self.class.label().to_string(),
        }
    }
}

/// Radius of the geostationary belt, and the width of the band around it this
/// classifier treats as "geosynchronous" (catalogue geostationary/synchronous
/// satellites drift and get re-boosted, so an exact period match is too
/// strict). Sourced from the sidereal day, not a rounded "24h".
const GEO_PERIOD_MIN: f64 = 1436.068;
const GEO_PERIOD_TOLERANCE_MIN: f64 = 10.0;

/// Altitude marking the top of low Earth orbit, and separately the altitude
/// of the geostationary belt itself — the boundary above which a near-
/// circular orbit stops being "medium" and becomes `High`. Using the belt
/// altitude as the ceiling, rather than a round number well above it, means a
/// graveyard orbit a few hundred km above GEO reads as `High`, not `Meo`. No
/// extra tolerance is added here: the `±10 min` period test above already claims
/// everything within roughly ±195 km of the belt (`da/a = ⅔ dP/P` at
/// `a ≈ 42,164 km`) as GEO/GSO before this constant is ever consulted, so
/// whatever reaches it is already unambiguously above the belt.
const LEO_APOGEE_CEILING_KM: f64 = 2_000.0;
const GEO_ALTITUDE_KM: f64 = 35_786.0;

/// Inclinations within this many degrees of 90° are treated as polar — the
/// same ±10° band Celestrak's SATCAT uses for its `-P` suffix.
const POLAR_INCLINATION_BAND_DEG: f64 = 10.0;

/// The rate a satellite's ascending node must precess at to stay fixed
/// relative to the mean Sun: +360° per tropical year, i.e. Earth's mean
/// orbital angular rate about the Sun. An orbit whose *actual* nodal
/// precession (from J2 oblateness — see `orbit_shape`) matches this is sun-
/// synchronous. This needs no altitude gate of its own: J2 precession falls
/// off steeply with semi-major axis, so nothing above LEO ever gets close to
/// this rate regardless of inclination — a MEO or GEO simply can't match it.
const SUN_SYNC_PRECESSION_DEG_PER_DAY: f64 = 360.0 / 365.2422;
const SUN_SYNC_PRECESSION_TOLERANCE_DEG_PER_DAY: f64 = 0.05;

/// Classify an orbit's regime from its shape. Order matters:
///
/// An apogee below `LEO_APOGEE_CEILING_KM` is LEO outright, regardless of
/// eccentricity — nothing eccentric can reach a sidereal period from down
/// there anyway. Above that, eccentricity is checked *before* period: a
/// Molniya (e ≈ 0.7, ~718 min period) and a GPS satellite (e ≈ 0, ~718 min
/// period) share a period but are nothing alike, and a "Tundra" orbit has a
/// sidereal period yet is nobody's idea of geosynchronous — it's a highly
/// eccentric orbit that happens to repeat once a day. So anything eccentric or
/// with a low perigee is called HEO before the period test ever runs, and only
/// a near-circular, low-inclination sidereal orbit earns GEO. Past that,
/// `High` and `Meo` are split by altitude alone — both are already known to be
/// near-circular by this point, since the eccentric case returned above.
fn classify(
    period_min: f64,
    inclination_deg: f64,
    eccentricity: f64,
    perigee_km: f64,
    apogee_km: f64,
) -> OrbitClass {
    if apogee_km < LEO_APOGEE_CEILING_KM {
        return OrbitClass::Leo;
    }
    if eccentricity >= 0.1 || perigee_km < LEO_APOGEE_CEILING_KM {
        return OrbitClass::Heo;
    }
    if (period_min - GEO_PERIOD_MIN).abs() <= GEO_PERIOD_TOLERANCE_MIN {
        return if eccentricity < 0.01 && inclination_deg < 6.0 {
            OrbitClass::Geo
        } else {
            OrbitClass::Gso
        };
    }
    if apogee_km <= GEO_ALTITUDE_KM {
        OrbitClass::Meo
    } else {
        OrbitClass::High
    }
}

/// Classify which plane the orbit sits in, independent of altitude regime.
/// Sun-sync is checked first because it's the more specific claim — a real
/// sun-synchronous orbit (typically ~98°) also falls inside the polar band,
/// and "sun-synchronous" is the more useful thing to say about it.
fn classify_plane(inclination_deg: f64, nodal_precession_deg_day: f64) -> OrbitPlane {
    if (nodal_precession_deg_day - SUN_SYNC_PRECESSION_DEG_PER_DAY).abs()
        <= SUN_SYNC_PRECESSION_TOLERANCE_DEG_PER_DAY
    {
        OrbitPlane::SunSync
    } else if (inclination_deg - 90.0).abs() <= POLAR_INCLINATION_BAND_DEG {
        OrbitPlane::Polar
    } else {
        OrbitPlane::Inclined
    }
}

/// Derive the orbit's shape from an element set.
///
/// The TLE/GP mean motion is a *Kozai* mean motion, not the two-body value a
/// naive `a = (μ/n²)^⅓` would assume — using it directly would put the
/// semi-major axis (and so apogee/perigee) off by several km. So this redoes
/// the same un-Kozai'ing `sgp4::Constants::from_elements` performs internally,
/// via the crate's own public `Orbit::from_kozai_elements`, to get the
/// Brouwer mean motion the semi-major axis is actually defined against.
///
/// The *displayed* period is still derived from the raw Kozai mean motion
/// (`1440 / n`), not the Brouwer one — that's the period Celestrak's SATCAT
/// and every other catalogue quote, so matching it is the least surprising
/// choice even though it differs from the Brouwer-consistent value by a
/// fraction of a second per orbit.
fn orbit_shape(elements: &sgp4::Elements) -> std::result::Result<OrbitShape, sgp4::KozaiElementsError> {
    let orbit = sgp4::Orbit::from_kozai_elements(
        &sgp4::WGS84,
        elements.inclination.to_radians(),
        elements.right_ascension.to_radians(),
        elements.eccentricity,
        elements.argument_of_perigee.to_radians(),
        elements.mean_anomaly.to_radians(),
        elements.mean_motion * (std::f64::consts::PI / 720.0), // rev/day -> rad/min
    )?;
    // a = (kₑ / n₀″)^⅔, in earth radii; `orbit.mean_motion` is the Brouwer value.
    let semi_major_km = (sgp4::WGS84.ke / orbit.mean_motion).powf(2.0 / 3.0) * sgp4::WGS84.ae;
    let e = elements.eccentricity;
    let perigee_km = semi_major_km * (1.0 - e) - sgp4::WGS84.ae;
    let apogee_km = semi_major_km * (1.0 + e) - sgp4::WGS84.ae;
    let period_min = 1440.0 / elements.mean_motion;

    // J2 secular nodal precession: dΩ/dt = -3/2 J2 (aₑ/p)² n cos(i), with the
    // semi-latus rectum p = a(1-e²). Used only to detect sun-synchronicity —
    // see `SUN_SYNC_PRECESSION_DEG_PER_DAY`.
    let p_km = semi_major_km * (1.0 - e * e);
    let precession_rad_min = -1.5
        * sgp4::WGS84.j2
        * (sgp4::WGS84.ae / p_km).powi(2)
        * orbit.mean_motion
        * elements.inclination.to_radians().cos();
    let nodal_precession_deg_day = precession_rad_min.to_degrees() * 1440.0;

    let class = classify(period_min, elements.inclination, e, perigee_km, apogee_km);
    let plane = classify_plane(elements.inclination, nodal_precession_deg_day);
    Ok(OrbitShape {
        class,
        plane,
        period_min,
        inclination_deg: elements.inclination,
        eccentricity: e,
        perigee_km,
        apogee_km,
    })
}

/// `sgp4::Constants` from `elements` with B\* scaled by
/// `1 + accuracy::BSTAR_REL_UNCERTAINTY`. `None` when that perturbed element
/// set won't initialise — see [`Tracker::drag_constants`]. Scaling a zero B\*
/// leaves it zero, which is fine: the difference against the nominal
/// propagation is then flat zero and `orbit::accuracy` falls through to its
/// regime floor.
fn perturbed_drag_constants(elements: &sgp4::Elements) -> Option<sgp4::Constants> {
    let mut perturbed = elements.clone();
    perturbed.drag_term *= 1.0 + accuracy::BSTAR_REL_UNCERTAINTY;
    sgp4::Constants::from_elements(&perturbed).ok()
}

/// A ready-to-use propagator for one satellite, built from a TLE / GP element set.
#[derive(Clone)]
pub struct Tracker {
    name: String,
    norad_id: u64,
    elements: sgp4::Elements,
    constants: sgp4::Constants,
    /// A second propagator from the same elements with B\* scaled by
    /// `1 + accuracy::BSTAR_REL_UNCERTAINTY`, used only by
    /// [`Tracker::accuracy_at`] to measure how sensitive this orbit is to its
    /// fitted drag term. `None` when that perturbed element set won't
    /// initialise — the accuracy model then leans on its regime floor alone
    /// rather than the whole `Tracker` failing over an estimate.
    drag_constants: Option<sgp4::Constants>,
    shape: OrbitShape,
}

impl std::fmt::Debug for Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracker")
            .field("name", &self.name)
            .field("norad_id", &self.norad_id)
            .field("epoch", &self.epoch())
            .finish()
    }
}

impl Tracker {
    /// Build from a single already-parsed element set.
    pub fn from_elements(elements: sgp4::Elements) -> Result<Self> {
        let constants = sgp4::Constants::from_elements(&elements)
            .map_err(|e| anyhow!("SGP4 rejected these elements: {e}"))?;
        let shape = orbit_shape(&elements)
            .map_err(|e| anyhow!("deriving orbit shape from these elements: {e}"))?;
        // Built once, here, so the accuracy estimate costs one extra
        // `propagate` per frame rather than a fresh SGP4 init. A near-zero B\*
        // nudged the wrong way, or any other reason the perturbed set is
        // rejected, just means no drag-sensitivity term — not a dead `Tracker`.
        let drag_constants = perturbed_drag_constants(&elements);
        Ok(Self {
            name: elements
                .object_name
                .clone()
                .unwrap_or_else(|| format!("NORAD {}", elements.norad_id)),
            norad_id: elements.norad_id,
            elements,
            constants,
            drag_constants,
            shape,
        })
    }

    /// Parse Celestrak's JSON GP format (`FORMAT=json`) and track the first entry.
    pub fn from_gp_json(json: &str) -> Result<Self> {
        let list: Vec<sgp4::Elements> =
            serde_json::from_str(json).context("parsing Celestrak GP JSON")?;
        let first = list
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("Celestrak returned an empty element set"))?;
        Self::from_elements(first)
    }

    /// Parse a two/three-line element text block and track the first entry.
    /// Kept as a fallback for plain-text TLE sources.
    #[allow(dead_code)]
    pub fn from_3le_text(text: &str) -> Result<Self> {
        let list = sgp4::parse_3les(text).map_err(|e| anyhow!("parsing TLE text: {e}"))?;
        let first = list
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("TLE text contained no elements"))?;
        Self::from_elements(first)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn norad_id(&self) -> u64 {
        self.norad_id
    }

    /// The satellite's COSPAR international designator (e.g. `1998-067A`),
    /// when the element set carries one — absent for some catalogue entries.
    pub fn international_designator(&self) -> Option<&str> {
        self.elements.international_designator.as_deref()
    }

    /// The element-set epoch (UTC).
    pub fn epoch(&self) -> DateTime<Utc> {
        self.elements.datetime.and_utc()
    }

    /// Age of the element set relative to `now`. Signed: negative when `now` is
    /// before the epoch, which a backward time scrub can produce.
    pub fn element_age(&self, now: DateTime<Utc>) -> chrono::Duration {
        now - self.epoch()
    }

    /// A modelled position-accuracy estimate for `time` — see
    /// [`crate::orbit::accuracy`]. `speed_kms` is the value from the `SatState`
    /// the caller has already propagated; it only scales the along-track error
    /// into a timing error, so pass `0.0` when there is no state.
    ///
    /// Never fails. If the B\*-perturbed propagation can't be run — no
    /// perturbed propagator was built, or it diverges at this `time` — the
    /// estimate falls back to the regime floor rather than propagating the
    /// error up. An accuracy figure is not worth losing a frame over.
    pub fn accuracy_at(&self, time: DateTime<Utc>, speed_kms: f64) -> Accuracy {
        Accuracy::model(
            self.shape.class,
            time - self.epoch(),
            self.drag_divergence_km(time),
            speed_kms,
        )
    }

    /// Distance in km between the nominal position at `time` and the position
    /// SGP4 gives when B\* is perturbed by
    /// [`accuracy::BSTAR_REL_UNCERTAINTY`]. `None` when no perturbed
    /// propagator exists or either propagation fails at `time`.
    fn drag_divergence_km(&self, time: DateTime<Utc>) -> Option<f64> {
        let drag = self.drag_constants.as_ref()?;
        let minutes = self
            .elements
            .datetime_to_minutes_since_epoch(&time.naive_utc())
            .ok()?;
        let nominal = self.constants.propagate(minutes).ok()?;
        let perturbed = drag.propagate(minutes).ok()?;
        Some(norm([
            nominal.position[0] - perturbed.position[0],
            nominal.position[1] - perturbed.position[1],
            nominal.position[2] - perturbed.position[2],
        ]))
    }

    /// The orbit's time-invariant shape (period, inclination, apogee/perigee)
    /// and the broad regime it falls in. Computed once in `from_elements`, so
    /// this is a cheap copy, not a recomputation.
    pub fn orbit_shape(&self) -> OrbitShape {
        self.shape
    }

    /// Propagate to `time` and derive the full ground state.
    pub fn state_at(&self, time: DateTime<Utc>) -> Result<SatState> {
        let minutes = self
            .elements
            .datetime_to_minutes_since_epoch(&time.naive_utc())
            .map_err(|e| anyhow!("time is out of range for these elements: {e}"))?;
        let prediction = self
            .constants
            .propagate(minutes)
            .map_err(|e| anyhow!("SGP4 propagation failed: {e}"))?;

        let gmst = gmst_rad(time);
        let ecef = teme_to_ecef(prediction.position, gmst);
        let sub_point = ecef_to_geodetic(ecef);

        let speed_kms = norm(prediction.velocity);

        // Horizon geometry: half-angle subtended at Earth's centre by the circle
        // of ground points that can see the satellite.
        let re = crate::geo::WGS84_A_KM;
        let r = re + sub_point.alt_km.max(1.0);
        let central_angle = (re / r).acos();
        let footprint_km = re * central_angle;

        let sun_unit = sun_ecef_unit(subsolar_point(time));
        let sunlit = is_point_sunlit(ecef, sun_unit);

        let days = minutes.0 / 1440.0;
        let revolution =
            self.elements.revolution_number + (self.elements.mean_motion * days).floor().max(0.0) as u64;

        Ok(SatState {
            time,
            sub_point,
            ecef_km: ecef,
            speed_kms,
            footprint_km,
            sunlit,
            revolution,
        })
    }

    /// Sub-satellite points over `[now - before, now + after]`, split into
    /// polyline segments wherever the track crosses the ±180° meridian
    /// ([`split_at_antimeridian`]) so the map never draws a spurious streak
    /// across the world. A step whose propagation fails is skipped, leaving a
    /// time gap but never a false longitude jump.
    ///
    /// One case the split intentionally does *not* catch: a near-90°
    /// inclination track passing over a pole swings almost 180° of longitude in
    /// a single step (`cos i ≈ 0`, so the sub-point's longitude flips as it
    /// changes hemisphere). At exactly 90° that swing is ~179.9° — just under
    /// the split threshold — and whether any given step tips over 180° depends
    /// on where the sample grid lands relative to the pole, so the same orbit
    /// can show either a joined chord or a one-step gap at the pole from frame
    /// to frame. The chord is left in: unlike a dateline streak, it only spans
    /// the ~1° of latitude on either side of the pole that the satellite really
    /// does cross (SGP4 puts the worst step at lat 89.6°), so at whole-world
    /// zoom it lands within half a Braille sub-pixel of the true path, whereas
    /// splitting would open a real gap. See
    /// `ground_track_leaves_a_polar_crossing_unsplit_at_ninety_degrees_inclination`.
    ///
    /// A step whose propagation fails is skipped, leaving a time gap but never a
    /// false longitude jump.
    pub fn ground_track(
        &self,
        now: DateTime<Utc>,
        before: chrono::Duration,
        after: chrono::Duration,
        step: chrono::Duration,
    ) -> Vec<Vec<GeoPoint>> {
        let mut points = Vec::new();
        let mut t = now - before;
        let end = now + after;
        while t <= end {
            if let Ok(state) = self.state_at(t) {
                points.push(state.sub_point);
            }
            t += step;
        }
        split_at_antimeridian(points)
    }
}

/// Cylindrical shadow test: the satellite is lit unless it is behind the Earth
/// (negative projection on the Sun direction) *and* within one Earth radius of
/// the Sun–Earth line.
fn is_point_sunlit(ecef_km: [f64; 3], sun_unit: [f64; 3]) -> bool {
    let along = dot(ecef_km, sun_unit);
    if along >= 0.0 {
        return true;
    }
    let perp = [
        ecef_km[0] - along * sun_unit[0],
        ecef_km[1] - along * sun_unit[1],
        ecef_km[2] - along * sun_unit[2],
    ];
    norm(perp) > crate::geo::WGS84_A_KM
}

/// Greenwich mean sidereal time in radians, from UTC (treating UT1 ≈ UTC).
/// Uses the IAU 1982 polynomial; good to well under an arc-second for tracking.
pub fn gmst_rad(time: DateTime<Utc>) -> f64 {
    let jd = time.timestamp_millis() as f64 / 86_400_000.0 + 2_440_587.5;
    let d = jd - 2_451_545.0;
    let t = d / 36_525.0;
    let deg = 280.460_618_37 + 360.985_647_366_29 * d + 0.000_387_933 * t * t
        - t * t * t / 38_710_000.0;
    deg.to_radians().rem_euclid(std::f64::consts::TAU)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orbit::test_tracker as tracker;

    #[test]
    fn gp_json_deserializes_into_a_tracker() {
        let tr = tracker();
        assert_eq!(tr.norad_id(), 25544);
        assert!(tr.name().contains("ISS"));
    }

    #[test]
    fn iss_state_at_epoch_is_physically_sane() {
        let tr = tracker();
        let st = tr.state_at(tr.epoch()).expect("propagates at epoch");

        // ISS altitude sits in a well-known band.
        assert!(
            (300.0..=460.0).contains(&st.sub_point.alt_km),
            "altitude {:.1} km outside the ISS band",
            st.sub_point.alt_km
        );
        // Inclination 51.6° caps the reachable latitude.
        assert!(st.sub_point.lat_deg.abs() <= 53.0);
        // Inertial speed is ~7.66 km/s. This assertion also pins down the crate's
        // velocity units (km/s, not km/min).
        assert!(
            (7.4..=7.9).contains(&st.speed_kms),
            "speed {:.3} km/s is not an orbital velocity",
            st.speed_kms
        );
        // Footprint radius for a ~420 km orbit is a couple of thousand km.
        assert!((1500.0..=3000.0).contains(&st.footprint_km));
    }

    #[test]
    fn iss_element_set_is_a_low_earth_orbit() {
        let tr = tracker();
        let orb = tr.orbit_shape();

        assert_eq!(orb.class, OrbitClass::Leo);
        // The ISS precesses at roughly -5°/day and sits at 51.6° — nowhere
        // near polar or sun-synchronous, so the plane is unremarkable.
        assert_eq!(orb.plane, OrbitPlane::Inclined);
        // 1440 / MEAN_MOTION (15.48983228 rev/day) from the fixture.
        assert!((90.0..96.0).contains(&orb.period_min), "period {:.2} min", orb.period_min);
        assert!((orb.inclination_deg - 51.6313).abs() < 0.001);
        // Near-circular orbit: apogee and perigee both sit in the same band
        // `iss_state_at_epoch_is_physically_sane` already brackets altitude to.
        assert!((300.0..480.0).contains(&orb.perigee_km), "perigee {:.1} km", orb.perigee_km);
        assert!((300.0..480.0).contains(&orb.apogee_km), "apogee {:.1} km", orb.apogee_km);
    }

    #[test]
    fn classify_separates_a_molniya_from_a_gps_orbit_by_eccentricity() {
        // Both share a ~718 min (half-sidereal) period, so the period test
        // alone can't tell them apart — eccentricity has to be checked first.
        let gps = classify(717.97, 55.0, 0.001, 20_180.0, 20_220.0);
        assert_eq!(gps, OrbitClass::Meo);

        let molniya = classify(717.97, 63.4, 0.72, 600.0, 39_900.0);
        assert_eq!(molniya, OrbitClass::Heo);
    }

    #[test]
    fn classify_calls_an_inclined_geosynchronous_orbit_gso_not_geo() {
        let geo = classify(GEO_PERIOD_MIN, 0.05, 0.0002, 35_780.0, 35_790.0);
        assert_eq!(geo, OrbitClass::Geo);

        let gso = classify(GEO_PERIOD_MIN, 8.0, 0.0003, 35_770.0, 35_800.0);
        assert_eq!(gso, OrbitClass::Gso);

        // A Tundra orbit: sidereal period, but far too eccentric to call GSO.
        let tundra = classify(GEO_PERIOD_MIN, 63.4, 0.24, 25_000.0, 46_500.0);
        assert_eq!(tundra, OrbitClass::Heo);
    }

    #[test]
    fn a_near_circular_orbit_above_the_belt_is_high_not_heo() {
        // Circular and 15,000 km above GEO: nothing eccentric about it, so
        // falling through to HEO on altitude alone would be wrong — this is
        // what `High` exists to catch instead.
        let graveyard = classify(2870.0, 10.0, 0.0005, 50_700.0, 50_800.0);
        assert_eq!(graveyard, OrbitClass::High);
    }

    #[test]
    fn a_graveyard_orbit_just_above_geo_is_high_not_meo() {
        // A few hundred km above the belt — close enough that a ceiling set
        // well above GEO_ALTITUDE_KM would misclassify this as MEO instead.
        let graveyard = classify(1465.0, 5.0, 0.001, 36_050.0, 36_150.0);
        assert_eq!(graveyard, OrbitClass::High);
    }

    #[test]
    fn a_gps_orbit_stays_meo() {
        // Guards the MEO/High boundary from the other side: GPS's ~20,200 km
        // altitude is nowhere near the belt, so moving the ceiling down to
        // GEO_ALTITUDE_KM must not have pulled it in.
        let gps = classify(717.97, 55.0, 0.001, 20_180.0, 20_220.0);
        assert_eq!(gps, OrbitClass::Meo);
    }

    #[test]
    fn classify_plane_prefers_sun_synchronous_over_polar_for_a_98_degree_orbit() {
        // A real sun-synchronous inclination (~98°) also sits inside the
        // ±10° polar band — sun-sync must win because it's the more specific,
        // more useful claim.
        let sso = classify_plane(98.2, SUN_SYNC_PRECESSION_DEG_PER_DAY);
        assert_eq!(sso, OrbitPlane::SunSync);

        // The ISS precesses at roughly -5°/day — nowhere near the sun-
        // synchronous rate — and sits at 51.6°, well outside the polar band.
        let iss_like = classify_plane(51.6, -5.0);
        assert_eq!(iss_like, OrbitPlane::Inclined);
    }

    #[test]
    fn classify_plane_calls_a_90_degree_inclination_polar_even_off_sun_sync() {
        // Polar, but the precession rate rules out sun-synchronous — the
        // case a PO satellite at the "wrong" altitude for SSO must still hit.
        let polar = classify_plane(89.5, -2.0);
        assert_eq!(polar, OrbitPlane::Polar);
    }

    #[test]
    fn a_polar_leo_is_labelled_leo_p_not_just_po() {
        // The whole point of splitting plane from class: a bare "PO" would
        // hide the regime the satellite is also in.
        let shape = OrbitShape {
            class: OrbitClass::Leo,
            plane: OrbitPlane::Polar,
            period_min: 100.9,
            inclination_deg: 89.5,
            eccentricity: 0.001,
            perigee_km: 700.0,
            apogee_km: 720.0,
        };
        assert_eq!(shape.label(), "LEO-P");

        let sun_sync = OrbitShape { plane: OrbitPlane::SunSync, ..shape };
        assert_eq!(sun_sync.label(), "LEO-S");

        let plain = OrbitShape { plane: OrbitPlane::Inclined, ..shape };
        assert_eq!(plain.label(), "LEO");
    }

    #[test]
    fn sentinel_2a_element_set_is_detected_as_sun_synchronous() {
        // A real element set, unlike the hand-picked values
        // `classify_plane_prefers_sun_synchronous_over_polar_for_a_98_degree_orbit`
        // uses, so this is the test that would catch an error in the
        // precession formula itself rather than just in the branch logic.
        let tr = crate::orbit::test_sso_tracker();
        let orb = tr.orbit_shape();
        assert_eq!(orb.class, OrbitClass::Leo);
        assert_eq!(
            orb.plane,
            OrbitPlane::SunSync,
            "{:.1} km apogee, {:.2}° incl",
            orb.apogee_km,
            orb.inclination_deg
        );
    }

    #[test]
    fn ground_track_splits_at_the_antimeridian() {
        let tr = tracker();
        let segs = tr.ground_track(
            tr.epoch(),
            chrono::Duration::zero(),
            chrono::Duration::minutes(100),
            chrono::Duration::seconds(30),
        );
        // One ~93-minute orbit crosses the ±180° line, so we expect a split.
        assert!(segs.len() >= 2, "expected the track to be split, got {}", segs.len());
        for seg in &segs {
            for w in seg.windows(2) {
                assert!(
                    (w[0].lon_deg - w[1].lon_deg).abs() < 180.0,
                    "a segment still straddles the antimeridian"
                );
            }
        }
    }

    #[test]
    fn ground_track_leaves_a_polar_crossing_unsplit_at_ninety_degrees_inclination() {
        // A ~90° track crossing the pole flips almost exactly 180° in longitude
        // in one 20 s step, so `split_at_antimeridian`'s `> 180.0` test *just*
        // fails to fire and the pair is left inside one segment. Whether it
        // fires at all is phase-dependent, so sweep the start instant and take
        // the run whose pole-straddling step comes closest to — but under — the
        // threshold. The point of the test: the un-split chord is a faithful
        // draw, not a bug — it spans only the ~1° of latitude either side of
        // the pole that the satellite genuinely traverses, unlike a dateline
        // streak that would sweep longitudes it never visits.
        let tr = crate::orbit::test_polar_tracker();
        let mut worst: Option<(f64, f64)> = None; // (|Δlon|, |lat| at that step)
        for off_s in 0..600 {
            let start = tr.epoch() + chrono::Duration::seconds(off_s);
            let segs = tr.ground_track(
                start,
                chrono::Duration::zero(),
                chrono::Duration::minutes(65),
                chrono::Duration::seconds(20),
            );
            for seg in &segs {
                for w in seg.windows(2) {
                    let dlon = (w[0].lon_deg - w[1].lon_deg).abs();
                    // Inside a segment the splitter guarantees this.
                    assert!(dlon < 180.0, "splitter left a >180° jump inside a segment");
                    let lat = w[0].lat_deg.abs().max(w[1].lat_deg.abs());
                    if lat > 88.0 && worst.is_none_or(|(d, _)| dlon > d) {
                        worst = Some((dlon, lat));
                    }
                }
            }
        }

        let (dlon, lat) = worst.expect("a near-polar orbit must produce a pole-straddling step");
        // The chord really does span most of the map in longitude …
        assert!(dlon > 150.0, "expected a near-180° unsplit jump, got {dlon:.1}°");
        // … while covering barely a degree of latitude, which is why leaving it
        // un-split is correct: the true path is within ~1° of the pole here.
        assert!(lat > 89.0, "pole-straddling step sits at lat {lat:.2}°, not near the pole");
    }

    #[test]
    fn gmst_advances_by_earth_rotation_rate() {
        let t0 = tr_epoch();
        let t1 = t0 + chrono::Duration::hours(1);
        let mut delta = gmst_rad(t1) - gmst_rad(t0);
        if delta < 0.0 {
            delta += std::f64::consts::TAU;
        }
        // One sidereal hour of rotation ≈ 0.2625 rad.
        assert!((delta - 0.262_5).abs() < 0.001, "gmst delta {delta:.5} rad/hour");
    }

    fn tr_epoch() -> DateTime<Utc> {
        tracker().epoch()
    }
}
