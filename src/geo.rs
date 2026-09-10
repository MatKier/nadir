//! Shared geodetic types and WGS-84 conversions.
//!
//! Everything here is pure arithmetic — no I/O, no globals — so it can be unit
//! tested without a network or a terminal.

/// WGS-84 semi-major axis (equatorial radius), in kilometres.
pub const WGS84_A_KM: f64 = 6378.137;
/// WGS-84 flattening.
pub const WGS84_F: f64 = 1.0 / 298.257_223_563;
/// WGS-84 first eccentricity squared.
pub const WGS84_E2: f64 = WGS84_F * (2.0 - WGS84_F);
/// Earth's rotation rate, in radians per second (IERS mean value). The angular
/// speed the ECEF frame turns at relative to an inertial one — needed to take a
/// TEME velocity into a ground-relative ECEF velocity, see
/// [`teme_velocity_to_ecef`].
pub const EARTH_ROTATION_RAD_S: f64 = 7.292_115e-5;

/// A point on or above the Earth's surface, in geodetic coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    /// Latitude in degrees, north positive, in `[-90, 90]`.
    pub lat_deg: f64,
    /// Longitude in degrees, east positive, in `[-180, 180]`.
    pub lon_deg: f64,
    /// Altitude in kilometres above the WGS-84 ellipsoid.
    pub alt_km: f64,
}

impl GeoPoint {
    pub fn new(lat_deg: f64, lon_deg: f64, alt_km: f64) -> Self {
        Self {
            lat_deg,
            lon_deg: wrap_longitude(lon_deg),
            alt_km,
        }
    }

    /// Convert to an Earth-centred, Earth-fixed (ECEF) position in kilometres.
    pub fn to_ecef_km(&self) -> [f64; 3] {
        let lat = self.lat_deg.to_radians();
        let lon = self.lon_deg.to_radians();
        let (sin_lat, cos_lat) = lat.sin_cos();
        let (sin_lon, cos_lon) = lon.sin_cos();

        // Radius of curvature in the prime vertical.
        let n = WGS84_A_KM / (1.0 - WGS84_E2 * sin_lat * sin_lat).sqrt();

        [
            (n + self.alt_km) * cos_lat * cos_lon,
            (n + self.alt_km) * cos_lat * sin_lon,
            (n * (1.0 - WGS84_E2) + self.alt_km) * sin_lat,
        ]
    }
}

/// Convert an ECEF position (kilometres) to geodetic latitude/longitude/altitude
/// using Bowring's closed-form solution. Accurate to well under a millimetre for
/// any altitude a satellite will see.
pub fn ecef_to_geodetic(ecef_km: [f64; 3]) -> GeoPoint {
    let [x, y, z] = ecef_km;
    let a = WGS84_A_KM;
    let e2 = WGS84_E2;
    let b = a * (1.0 - WGS84_F);
    let ep2 = (a * a - b * b) / (b * b);

    let p = (x * x + y * y).sqrt();
    let lon = y.atan2(x);

    // Bowring's auxiliary angle gives a latitude seed good to ~1e-8 rad; a few
    // Newton refinements then take the round-trip to full double precision.
    let theta = (z * a).atan2(p * b);
    let (sin_t, cos_t) = theta.sin_cos();
    let mut lat = (z + ep2 * b * sin_t.powi(3)).atan2(p - e2 * a * cos_t.powi(3));

    let height_at = |lat: f64| -> (f64, f64) {
        let (sin_lat, cos_lat) = lat.sin_cos();
        let n = a / (1.0 - e2 * sin_lat * sin_lat).sqrt();
        let alt = if cos_lat.abs() > 1e-10 {
            p / cos_lat - n
        } else {
            z.abs() - b
        };
        (n, alt)
    };

    let mut alt = 0.0;
    for _ in 0..3 {
        let (n, h) = height_at(lat);
        alt = h;
        lat = z.atan2(p * (1.0 - e2 * n / (n + alt)));
    }

    GeoPoint {
        lat_deg: lat.to_degrees(),
        lon_deg: wrap_longitude(lon.to_degrees()),
        alt_km: alt,
    }
}

/// Rotate a TEME (true-equator, mean-equinox) vector into ECEF by spinning it
/// about the Z axis through the Greenwich mean sidereal time `gmst_rad`.
///
/// This is the single rotation SGP4 output needs before it can be turned into a
/// ground position; getting the sidereal angle right is the classic tracker bug.
pub fn teme_to_ecef(teme_km: [f64; 3], gmst_rad: f64) -> [f64; 3] {
    let (sin_g, cos_g) = gmst_rad.sin_cos();
    let [x, y, z] = teme_km;
    [x * cos_g + y * sin_g, -x * sin_g + y * cos_g, z]
}

/// Take a TEME *velocity* into ECEF. This is deliberately not just
/// [`teme_to_ecef`] applied to a velocity: for a position the bare Z rotation
/// is the whole story, but a velocity also has to shed the motion the rotating
/// frame itself introduces.
///
/// Rotating `vel_teme_kms` by GMST gives the satellite's *inertial* velocity
/// expressed on Earth-fixed axes. An observer bolted to the ground is not
/// inertial — the frame turns under them at ω⊕ — so what they actually see is
/// that vector minus `ω⊕ × r`, with `r` the satellite's ECEF position. With
/// ω⊕ along +Z the cross product is `[-ω·y, ω·x, 0]`, so the correction adds
/// `+ω·y` to x and `-ω·x` to y. Skipping it, or flipping its sign, leaves a
/// spurious ~0.46 km/s (a point on the equator's rotation speed) baked into
/// every range rate — the classic bug in hand-rolled tracker code.
pub fn teme_velocity_to_ecef(
    vel_teme_kms: [f64; 3],
    ecef_km: [f64; 3],
    gmst_rad: f64,
) -> [f64; 3] {
    let [vx, vy, vz] = teme_to_ecef(vel_teme_kms, gmst_rad);
    let w = EARTH_ROTATION_RAD_S;
    [vx + w * ecef_km[1], vy - w * ecef_km[0], vz]
}

/// Look angles from an observer to a target, both given in ECEF kilometres.
#[derive(Debug, Clone, Copy)]
pub struct LookAngles {
    /// Azimuth in degrees clockwise from true north, in `[0, 360)`.
    pub azimuth_deg: f64,
    /// Elevation above the local horizon in degrees; negative means below.
    pub elevation_deg: f64,
    /// Slant range in kilometres.
    pub range_km: f64,
}

/// Compute azimuth/elevation/range of `target_ecef` as seen from `observer`.
pub fn look_angles(observer: &GeoPoint, target_ecef_km: [f64; 3]) -> LookAngles {
    let obs_ecef = observer.to_ecef_km();
    let d = [
        target_ecef_km[0] - obs_ecef[0],
        target_ecef_km[1] - obs_ecef[1],
        target_ecef_km[2] - obs_ecef[2],
    ];

    let lat = observer.lat_deg.to_radians();
    let lon = observer.lon_deg.to_radians();
    let (sin_lat, cos_lat) = lat.sin_cos();
    let (sin_lon, cos_lon) = lon.sin_cos();

    // ECEF -> local east/north/up.
    let east = -sin_lon * d[0] + cos_lon * d[1];
    let north = -sin_lat * cos_lon * d[0] - sin_lat * sin_lon * d[1] + cos_lat * d[2];
    let up = cos_lat * cos_lon * d[0] + cos_lat * sin_lon * d[1] + sin_lat * d[2];

    let range = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let elevation = (up / range).clamp(-1.0, 1.0).asin().to_degrees();
    let mut azimuth = east.atan2(north).to_degrees();
    if azimuth < 0.0 {
        azimuth += 360.0;
    }

    LookAngles {
        azimuth_deg: azimuth,
        elevation_deg: elevation,
        range_km: range,
    }
}

/// Range rate ṙ between `observer` and a target, in km/s: positive while the
/// range is opening (the target receding), negative while it is closing, and
/// passing through zero exactly at closest approach — the sign flip that marks
/// TCA on a pass.
///
/// `target_vel_ecef_kms` has to be the target's *ground-relative* velocity, the
/// output of [`teme_velocity_to_ecef`]; the observer is fixed in ECEF and adds
/// no velocity of its own, so ṙ is just that velocity projected onto the line
/// of sight — `d̂ · v`, with `d` the same observer→target vector [`look_angles`]
/// builds internally.
pub fn range_rate(
    observer: &GeoPoint,
    target_ecef_km: [f64; 3],
    target_vel_ecef_kms: [f64; 3],
) -> f64 {
    let obs = observer.to_ecef_km();
    let d = [
        target_ecef_km[0] - obs[0],
        target_ecef_km[1] - obs[1],
        target_ecef_km[2] - obs[2],
    ];
    let range = norm(d);
    if range == 0.0 {
        return 0.0;
    }
    dot(d, target_vel_ecef_kms) / range
}

/// The Doppler shift in hertz that a range rate of `range_rate_kms` (km/s) puts
/// on a carrier transmitted at `rest_hz`: `Δf = −f₀ · ṙ / c`. The leading minus
/// is why a *closing* target (negative ṙ) yields a *positive* shift — the
/// received frequency rises as the satellite approaches. First-order form only;
/// at orbital speeds the relativistic correction is parts in a billion.
pub fn doppler_shift_hz(rest_hz: f64, range_rate_kms: f64) -> f64 {
    /// Speed of light in km/s.
    const C_KMS: f64 = 299_792.458;
    -rest_hz * range_rate_kms / C_KMS
}

/// Dot product of two vectors.
pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Euclidean norm of a vector.
pub fn norm(v: [f64; 3]) -> f64 {
    dot(v, v).sqrt()
}

/// Unit vector from the Earth's centre towards (lat, lon) — the *geocentric*
/// radial direction, not the geodetic normal. Observer geometry that must
/// account for the ellipsoid belongs in [`look_angles`].
pub fn radial_unit(lat_deg: f64, lon_deg: f64) -> [f64; 3] {
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();
    let (sin_lat, cos_lat) = lat.sin_cos();
    let (sin_lon, cos_lon) = lon.sin_cos();
    [cos_lat * cos_lon, cos_lat * sin_lon, sin_lat]
}

/// Fold any longitude into the half-open interval `[-180, 180)`.
pub fn wrap_longitude(mut lon_deg: f64) -> f64 {
    while lon_deg >= 180.0 {
        lon_deg -= 360.0;
    }
    while lon_deg < -180.0 {
        lon_deg += 360.0;
    }
    lon_deg
}

/// Great-circle distance in kilometres between two surface points (haversine).
pub fn great_circle_km(a: &GeoPoint, b: &GeoPoint) -> f64 {
    let lat1 = a.lat_deg.to_radians();
    let lat2 = b.lat_deg.to_radians();
    let dlat = lat2 - lat1;
    let dlon = (b.lon_deg - a.lon_deg).to_radians();
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * WGS84_A_KM * h.sqrt().asin()
}

/// The point reached by leaving `from` on compass `bearing_rad` (clockwise from
/// north) and travelling `angular_dist_rad` radians of arc along that great
/// circle. Altitude is carried through unchanged.
///
/// [`crate::orbit::solar::terminator_polyline`] is the `angular_dist_rad = π/2`
/// special case of this, written out by hand there because at exactly a quarter
/// turn the `sin`/`cos` of the distance collapse to 1/0 and several terms drop.
pub fn destination_point(from: &GeoPoint, bearing_rad: f64, angular_dist_rad: f64) -> GeoPoint {
    let lat1 = from.lat_deg.to_radians();
    let lon1 = from.lon_deg.to_radians();
    let (sin_d, cos_d) = angular_dist_rad.sin_cos();
    let (sin_lat1, cos_lat1) = lat1.sin_cos();
    let (sin_brg, cos_brg) = bearing_rad.sin_cos();

    let sin_lat2 = (sin_lat1 * cos_d + cos_lat1 * sin_d * cos_brg).clamp(-1.0, 1.0);
    let lat2 = sin_lat2.asin();
    let lon2 = lon1 + (sin_brg * sin_d * cos_lat1).atan2(cos_d - sin_lat1 * sin_lat2);

    GeoPoint {
        lat_deg: lat2.to_degrees(),
        lon_deg: wrap_longitude(lon2.to_degrees()),
        alt_km: from.alt_km,
    }
}

/// Break a polyline into segments wherever two consecutive points jump more
/// than 180° of longitude — where the line crosses the ±180° meridian and
/// would otherwise be drawn as a spurious streak straight across the map.
/// Segments left with fewer than two points (nothing to draw a line through)
/// are dropped, so the result can be empty.
///
/// This is a *dateline* test, not a pole test. A step straddling a pole on a
/// near-90° orbit also flips close to 180° of longitude, but just under it, so
/// it stays inside one segment — deliberately; see [`crate::orbit::Tracker::ground_track`]
/// for why splitting there would be wrong.
pub fn split_at_antimeridian(points: Vec<GeoPoint>) -> Vec<Vec<GeoPoint>> {
    let mut segments: Vec<Vec<GeoPoint>> = vec![Vec::new()];
    let mut prev_lon: Option<f64> = None;
    for p in points {
        if let Some(prev) = prev_lon {
            if (p.lon_deg - prev).abs() > 180.0 {
                segments.push(Vec::new());
            }
        }
        prev_lon = Some(p.lon_deg);
        segments.last_mut().expect("segments always holds at least one Vec").push(p);
    }
    segments.retain(|s| s.len() > 1);
    segments
}

/// The visibility-footprint boundary as a drawable polyline: the circle of
/// surface points exactly `radius_km` of great-circle distance from `centre`,
/// sampled at `n` points around the ring (the sample count also closes it back
/// onto its start) and split at the antimeridian by [`split_at_antimeridian`].
///
/// `radius_km` is the same value `SatState::footprint_km` carries; the central
/// angle is recovered as `radius_km / WGS84_A_KM`, the exact inverse of how
/// that field is computed in `Tracker::state_at`, so the ring and the numeric
/// readout can never drift apart.
///
/// The boundary is a small circle on the sphere, *not* a circle in the map's
/// equirectangular projection — it bulges wider in longitude the further it
/// reaches from the equator. When the cap covers a pole its longitudes sweep
/// the full range and it draws as an open curve skirting the pole rather than a
/// closed loop, which is the correct outline of that region; a sample pair
/// straddling the pole can still leave a near-horizontal streak at extreme
/// latitude — the same limitation the ground track carries at the dateline.
pub fn footprint_ring(centre: &GeoPoint, radius_km: f64, n: usize) -> Vec<Vec<GeoPoint>> {
    let ground = GeoPoint { lat_deg: centre.lat_deg, lon_deg: centre.lon_deg, alt_km: 0.0 };
    let angular = radius_km / WGS84_A_KM;
    let ring = (0..=n)
        .map(|i| {
            let bearing = (i as f64 / n as f64) * std::f64::consts::TAU;
            destination_point(&ground, bearing, angular)
        })
        .collect();
    split_at_antimeridian(ring)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geodetic_ecef_round_trips_to_sub_metre() {
        let cases = [
            GeoPoint::new(0.0, 0.0, 0.0),
            GeoPoint::new(48.1372, 11.5755, 0.52), // Munich
            GeoPoint::new(-33.856, 151.215, 0.0),  // Sydney
            GeoPoint::new(51.6, -120.0, 420.0),    // an ISS-height sub-point
            GeoPoint::new(89.9, 45.0, 0.0),        // near-polar
        ];
        for p in cases {
            let back = ecef_to_geodetic(p.to_ecef_km());
            assert!((back.lat_deg - p.lat_deg).abs() < 1e-6, "lat {p:?} -> {back:?}");
            assert!((back.lon_deg - p.lon_deg).abs() < 1e-6, "lon {p:?} -> {back:?}");
            assert!((back.alt_km - p.alt_km).abs() < 1e-6, "alt {p:?} -> {back:?}");
        }
    }

    #[test]
    fn teme_rotation_by_zero_is_identity() {
        let v = [4000.0, -3000.0, 5000.0];
        assert_eq!(teme_to_ecef(v, 0.0), v);
    }

    #[test]
    fn longitude_wraps_into_range() {
        assert_eq!(wrap_longitude(190.0), -170.0);
        assert_eq!(wrap_longitude(-190.0), 170.0);
        assert_eq!(wrap_longitude(0.0), 0.0);
        assert!((wrap_longitude(540.0) - -180.0).abs() < 1e-9);
    }

    #[test]
    fn zenith_target_has_ninety_degree_elevation() {
        // A point 500 km straight up the *geodetic* normal (not the geocentric
        // radial, which differs from it away from the equator/poles) must read
        // back as zenith.
        let obs = GeoPoint::new(10.0, 20.0, 0.0);
        let lat = obs.lat_deg.to_radians();
        let lon = obs.lon_deg.to_radians();
        let up_hat = [
            lat.cos() * lon.cos(),
            lat.cos() * lon.sin(),
            lat.sin(),
        ];
        let base = obs.to_ecef_km();
        let target = [
            base[0] + 500.0 * up_hat[0],
            base[1] + 500.0 * up_hat[1],
            base[2] + 500.0 * up_hat[2],
        ];
        let la = look_angles(&obs, target);
        assert!((la.elevation_deg - 90.0).abs() < 1e-6, "{la:?}");
        assert!((la.range_km - 500.0).abs() < 1e-6, "{la:?}");
    }

    #[test]
    fn teme_velocity_to_ecef_cancels_the_velocity_of_a_point_fixed_to_the_ground() {
        // A point rigidly attached to the rotating Earth has, in the inertial
        // TEME frame, exactly the velocity ω⊕ × r. Its *ground-relative*
        // velocity is zero by definition, so the conversion has to return ~0 —
        // this is the assertion a wrong sign on the ω⊕ term fails.
        let w = EARTH_ROTATION_RAD_S;
        for &gmst in &[0.0, 1.3, 4.7] {
            let r_teme = [5000.0, -2500.0, 3200.0];
            let r_ecef = teme_to_ecef(r_teme, gmst);
            let v_teme = [-w * r_teme[1], w * r_teme[0], 0.0];
            let v_ecef = teme_velocity_to_ecef(v_teme, r_ecef, gmst);
            assert!(norm(v_ecef) < 1e-9, "gmst {gmst}: {v_ecef:?}");
        }
    }

    #[test]
    fn range_rate_of_a_target_climbing_the_local_vertical_equals_its_speed() {
        // Straight up the geodetic normal, moving outward along it: the whole
        // velocity is radial, so ṙ is the full speed and its sign is positive
        // (opening). Built the same way as `zenith_target_has_ninety_degree_elevation`.
        let obs = GeoPoint::new(10.0, 20.0, 0.0);
        let lat = obs.lat_deg.to_radians();
        let lon = obs.lon_deg.to_radians();
        let up_hat = [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()];
        let base = obs.to_ecef_km();
        let target = [
            base[0] + 500.0 * up_hat[0],
            base[1] + 500.0 * up_hat[1],
            base[2] + 500.0 * up_hat[2],
        ];
        let speed = 7.5;
        let vel = [speed * up_hat[0], speed * up_hat[1], speed * up_hat[2]];
        assert!((range_rate(&obs, target, vel) - speed).abs() < 1e-9);
    }

    #[test]
    fn range_rate_of_a_target_moving_tangentially_is_zero() {
        // Same zenith target, but the velocity is perpendicular to the line of
        // sight (due east in the local frame): nothing of it is radial, so ṙ
        // is zero.
        let obs = GeoPoint::new(10.0, 20.0, 0.0);
        let lat = obs.lat_deg.to_radians();
        let lon = obs.lon_deg.to_radians();
        let up_hat = [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()];
        let east_hat = [-lon.sin(), lon.cos(), 0.0];
        let base = obs.to_ecef_km();
        let target = [
            base[0] + 500.0 * up_hat[0],
            base[1] + 500.0 * up_hat[1],
            base[2] + 500.0 * up_hat[2],
        ];
        let vel = [7.5 * east_hat[0], 7.5 * east_hat[1], 7.5 * east_hat[2]];
        assert!(range_rate(&obs, target, vel).abs() < 1e-9);
    }

    #[test]
    fn a_closing_target_shifts_the_downlink_up() {
        // 145.800 MHz, satellite approaching at 3.81 km/s (negative ṙ): the
        // received carrier rises, by f₀·|ṙ|/c ≈ 1.85 kHz.
        let shift = doppler_shift_hz(145_800_000.0, -3.81);
        assert!(shift > 0.0, "closing target must shift up, got {shift}");
        assert!((shift - 1853.0).abs() < 5.0, "{shift} Hz");
        // And a receding target by the same rate is the mirror image.
        assert!((doppler_shift_hz(145_800_000.0, 3.81) + shift).abs() < 1e-6);
    }

    use std::f64::consts::{PI, TAU};

    /// Flatten a single-segment ring for the assertions that don't care about
    /// splitting; panics if the ring came back split so a test can't silently
    /// pass on half the points.
    fn one_segment(segments: Vec<Vec<GeoPoint>>) -> Vec<GeoPoint> {
        assert_eq!(segments.len(), 1, "expected a single unsplit segment");
        segments.into_iter().next().unwrap()
    }

    #[test]
    fn destination_point_travels_the_requested_great_circle_distance() {
        let from = GeoPoint::new(48.0, 11.0, 0.0);
        let dist_km = 3000.0;
        let angular = dist_km / WGS84_A_KM;
        for i in 0..12 {
            let bearing = i as f64 / 12.0 * TAU;
            let to = destination_point(&from, bearing, angular);
            assert!(
                (great_circle_km(&from, &to) - dist_km).abs() < 1e-3,
                "bearing {bearing}: {to:?}"
            );
        }
    }

    #[test]
    fn destination_point_at_ninety_degrees_matches_the_terminator_construction() {
        // `terminator_polyline` hand-rolls the quarter-turn case; the general
        // formula must agree with it point for point.
        let centre = GeoPoint::new(15.0, -60.0, 0.0);
        let lat_s = centre.lat_deg.to_radians();
        let lon_s = centre.lon_deg.to_radians();
        let (sin_lat_s, cos_lat_s) = lat_s.sin_cos();
        for i in 0..16 {
            let bearing = i as f64 / 16.0 * TAU;
            let lat = (cos_lat_s * bearing.cos()).asin();
            let lon = lon_s + (bearing.sin() * cos_lat_s).atan2(-sin_lat_s * lat.sin());
            let collapsed = GeoPoint {
                lat_deg: lat.to_degrees(),
                lon_deg: wrap_longitude(lon.to_degrees()),
                alt_km: 0.0,
            };
            let general = destination_point(&centre, bearing, PI / 2.0);
            assert!((general.lat_deg - collapsed.lat_deg).abs() < 1e-9, "{general:?} {collapsed:?}");
            assert!((general.lon_deg - collapsed.lon_deg).abs() < 1e-9, "{general:?} {collapsed:?}");
        }
    }

    #[test]
    fn footprint_ring_points_all_sit_one_footprint_radius_from_the_centre() {
        let centre = GeoPoint::new(50.0, 10.0, 420.0);
        let ground = GeoPoint::new(centre.lat_deg, centre.lon_deg, 0.0);
        let radius_km = 2200.0;
        for p in one_segment(footprint_ring(&centre, radius_km, 180)) {
            assert!(
                (great_circle_km(&ground, &p) - radius_km).abs() < 0.5,
                "{p:?} is {:.3} km from the centre",
                great_circle_km(&ground, &p)
            );
        }
    }

    #[test]
    fn footprint_ring_is_a_single_segment_away_from_the_dateline() {
        // ISS-sized footprint over Europe: nowhere near ±180°, so one segment.
        let segments = footprint_ring(&GeoPoint::new(50.0, 10.0, 420.0), 2200.0, 180);
        assert_eq!(segments.len(), 1);
    }

    #[test]
    fn footprint_ring_wraps_across_the_dateline() {
        // Centre just east of the antimeridian, footprint straddling it: the
        // ring has to break into at least two segments, and between them the
        // returned points must reach both the +180° and the −180° edge rather
        // than one half being clipped away.
        let segments = footprint_ring(&GeoPoint::new(0.0, 179.0, 420.0), 2200.0, 180);
        assert!(segments.len() >= 2, "expected a split ring, got {}", segments.len());
        let all: Vec<f64> = segments.iter().flatten().map(|p| p.lon_deg).collect();
        assert!(all.iter().any(|&l| l > 170.0), "no points near the +180° edge: {all:?}");
        assert!(all.iter().any(|&l| l < -170.0), "no points near the −180° edge: {all:?}");
    }

    #[test]
    fn footprint_ring_segments_never_jump_the_dateline() {
        // Mirrors `ground_track_splits_at_the_antimeridian`: within a segment no
        // adjacent pair may span ≥180° of longitude.
        let segments = footprint_ring(&GeoPoint::new(0.0, 179.0, 420.0), 2200.0, 180);
        for seg in &segments {
            for w in seg.windows(2) {
                assert!(
                    (w[0].lon_deg - w[1].lon_deg).abs() < 180.0,
                    "segment jumps the dateline: {:?} -> {:?}",
                    w[0],
                    w[1]
                );
            }
        }
    }

    #[test]
    fn footprint_ring_at_the_equator_spans_the_full_central_angle() {
        // A GEO-sized cap on the equator reaches ±(central angle) in both lat
        // and lon at its extremes — the one latitude where the projected shape
        // and a naive circle happen to agree.
        let radius_km = 9050.0;
        let central_deg = (radius_km / WGS84_A_KM).to_degrees();
        let pts = one_segment(footprint_ring(&GeoPoint::new(0.0, 0.0, 35_786.0), radius_km, 180));
        let max_lon = pts.iter().map(|p| p.lon_deg.abs()).fold(0.0_f64, f64::max);
        let max_lat = pts.iter().map(|p| p.lat_deg.abs()).fold(0.0_f64, f64::max);
        assert!((max_lon - central_deg).abs() < 1.0, "max lon {max_lon} vs {central_deg}");
        assert!((max_lat - central_deg).abs() < 1.0, "max lat {max_lat} vs {central_deg}");
    }

    #[test]
    fn footprint_ring_bulges_in_longitude_at_high_centre_latitude() {
        // The spherical cap is not a projected circle: centred at 70°N a
        // ~22° cap sweeps far more than ±22° of longitude at its own latitude,
        // which the old Euclidean `Circle` could never show.
        let radius_km = 2500.0;
        let central_deg = (radius_km / WGS84_A_KM).to_degrees();
        let pts = one_segment(footprint_ring(&GeoPoint::new(70.0, 0.0, 780.0), radius_km, 180));
        let max_lon = pts.iter().map(|p| p.lon_deg.abs()).fold(0.0_f64, f64::max);
        assert!(
            max_lon > central_deg * 1.5,
            "max lon {max_lon} did not exceed 1.5x the central angle {central_deg}"
        );
    }
}
