//! Solar geometry: where the Sun is overhead, where the day/night line falls,
//! and whether a given point is in sunlight.
//!
//! Low-precision formulae (NOAA / Astronomical Almanac "approximate" Sun), good
//! to a few arc-minutes — far tighter than a terminal map can show, and enough
//! to decide eclipse and twilight for pass prediction.

use chrono::{DateTime, Utc};

use crate::geo::{dot, radial_unit, wrap_longitude, GeoPoint};
use crate::orbit::propagate::gmst_rad;

/// Geocentric unit vector towards the Sun, in the ECEF frame.
pub fn sun_ecef_unit(subsolar: GeoPoint) -> [f64; 3] {
    radial_unit(subsolar.lat_deg, subsolar.lon_deg)
}

/// The subsolar point (Sun in the zenith) at `time`.
pub fn subsolar_point(time: DateTime<Utc>) -> GeoPoint {
    let jd = time.timestamp_millis() as f64 / 86_400_000.0 + 2_440_587.5;
    let n = jd - 2_451_545.0;

    let mean_long = (280.460 + 0.985_647_4 * n).rem_euclid(360.0);
    let mean_anom = (357.528 + 0.985_600_3 * n).rem_euclid(360.0).to_radians();

    let ecliptic_long =
        (mean_long + 1.915 * mean_anom.sin() + 0.020 * (2.0 * mean_anom).sin()).to_radians();
    let obliquity = (23.439 - 3.6e-7 * n).to_radians();

    let declination = (obliquity.sin() * ecliptic_long.sin()).asin();
    let right_ascension = (obliquity.cos() * ecliptic_long.sin()).atan2(ecliptic_long.cos());

    // Rotate from the inertial equinox frame into the Earth-fixed frame.
    let lon = wrap_longitude((right_ascension - gmst_rad(time)).to_degrees());

    GeoPoint {
        lat_deg: declination.to_degrees(),
        lon_deg: lon,
        alt_km: 0.0,
    }
}

/// The day/night terminator as a closed polyline of `n` surface points: the
/// great circle exactly 90° from the subsolar point.
pub fn terminator_polyline(time: DateTime<Utc>, n: usize) -> Vec<GeoPoint> {
    let s = subsolar_point(time);
    let lat_s = s.lat_deg.to_radians();
    let lon_s = s.lon_deg.to_radians();
    let (sin_lat_s, cos_lat_s) = lat_s.sin_cos();

    (0..n)
        .map(|i| {
            let bearing = (i as f64 / n as f64) * std::f64::consts::TAU;
            // Angular distance is exactly 90°, so the standard destination-point
            // formulae collapse to these.
            let lat = (cos_lat_s * bearing.cos()).asin();
            let lon = lon_s + (bearing.sin() * cos_lat_s).atan2(-sin_lat_s * lat.sin());
            GeoPoint {
                lat_deg: lat.to_degrees(),
                lon_deg: wrap_longitude(lon.to_degrees()),
                alt_km: 0.0,
            }
        })
        .collect()
}

/// Elevation of the Sun above the local horizon at `where_`, in degrees.
/// Negative during night; below −6° is (at least) civil darkness.
pub fn solar_elevation_deg(where_: &GeoPoint, time: DateTime<Utc>) -> f64 {
    let sun = sun_ecef_unit(subsolar_point(time));
    let up = radial_unit(where_.lat_deg, where_.lon_deg);
    dot(up, sun).clamp(-1.0, 1.0).asin().to_degrees()
}

/// Whether a surface point is in daylight (Sun above the geometric horizon).
pub fn is_sunlit(where_: &GeoPoint, time: DateTime<Utc>) -> bool {
    solar_elevation_deg(where_, time) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn subsolar_latitude_tracks_the_seasons() {
        // Northern summer solstice: Sun near the Tropic of Cancer.
        let jun = Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap();
        assert!((subsolar_point(jun).lat_deg - 23.4).abs() < 0.6);

        // Equinox: Sun near the equator.
        let sep = Utc.with_ymd_and_hms(2026, 9, 22, 12, 0, 0).unwrap();
        assert!(subsolar_point(sep).lat_deg.abs() < 1.5);
    }

    #[test]
    fn subsolar_longitude_is_near_local_noon() {
        // At 12:00 UTC the Sun is roughly over the prime meridian (±4° for EoT).
        let noon = Utc.with_ymd_and_hms(2026, 3, 15, 12, 0, 0).unwrap();
        assert!(subsolar_point(noon).lon_deg.abs() < 5.0);
        // Six hours later it has moved ~90° west.
        let later = Utc.with_ymd_and_hms(2026, 3, 15, 18, 0, 0).unwrap();
        assert!((subsolar_point(later).lon_deg + 90.0).abs() < 5.0);
    }

    #[test]
    fn terminator_points_are_all_at_solar_terminator() {
        let t = Utc.with_ymd_and_hms(2026, 9, 4, 6, 0, 0).unwrap();
        for p in terminator_polyline(t, 180) {
            // By construction the Sun sits on the horizon everywhere on the line.
            assert!(solar_elevation_deg(&p, t).abs() < 0.5, "{p:?}");
        }
    }

    #[test]
    fn noon_side_is_lit_and_midnight_side_is_dark() {
        let t = Utc.with_ymd_and_hms(2026, 6, 21, 12, 0, 0).unwrap();
        let s = subsolar_point(t);
        assert!(is_sunlit(&s, t));
        let antipode = GeoPoint::new(-s.lat_deg, s.lon_deg + 180.0, 0.0);
        assert!(!is_sunlit(&antipode, t));
    }
}
