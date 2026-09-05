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
}
