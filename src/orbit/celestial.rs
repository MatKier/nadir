//! The Moon (and star geometry): a sibling of [`crate::orbit::solar`] for
//! everything that module doesn't cover — the sublunar point, the Moon's
//! phase, and turning a star's fixed right ascension/declination into a
//! local azimuth/elevation.
//!
//! Low-precision series throughout, in the same spirit `orbit::solar`'s own
//! doc comment states: good to a few arc-minutes in longitude/latitude and a
//! few hundred km in distance — far tighter than a terminal map or a polar
//! sky plot can show, and all that pass prediction's `visible` flag or a
//! "how full is the Moon tonight" readout needs. No I/O, same as every other
//! module under `orbit/`.

use chrono::{DateTime, Utc};

use crate::geo::{look_angles, radial_unit, wrap_longitude, GeoPoint, LookAngles};
use crate::orbit::propagate::gmst_rad;
use crate::orbit::solar::{days_since_j2000, obliquity_rad, sun_ecliptic_longitude_deg};

/// Mean Earth–Sun distance, km. The Sun's actual distance varies by under 2%
/// over the year, which moves its look angle by well under an arc-second —
/// invisible at terminal resolution — so the mean value stands in for the
/// real one everywhere the Sun is placed as a point rather than a direction.
const AU_KM: f64 = 149_597_870.7;

/// The Moon's synodic month (new moon to new moon), days — turns the phase
/// angle into an age for [`MoonPhase::age_days`].
const SYNODIC_MONTH_DAYS: f64 = 29.530_588_853;

/// The Moon's geocentric ecliptic longitude, latitude (both degrees) and
/// distance (km) at `time`, from Meeus' abridged lunar theory (*Astronomical
/// Algorithms*, ch. 47): only the handful of largest periodic terms, each
/// named for the fundamental argument it corrects — `D` mean elongation from
/// the Sun, `M`/`M'` the Sun's/Moon's mean anomaly, `F` the Moon's argument
/// of latitude. Dropping the smaller terms costs a few arc-minutes in
/// longitude/latitude and a few hundred km in distance, which is the
/// tolerance this whole module is built to.
fn lunar_ecliptic(time: DateTime<Utc>) -> (f64, f64, f64) {
    let t = days_since_j2000(time) / 36_525.0;

    let l = (218.316_447_7 + 481_267.881_23 * t).rem_euclid(360.0);
    let d = (297.850_192_1 + 445_267.111_403_4 * t).rem_euclid(360.0).to_radians();
    let m = (357.529_109_2 + 35_999.050_290_9 * t).rem_euclid(360.0).to_radians();
    let mp = (134.963_396_4 + 477_198.867_505_5 * t).rem_euclid(360.0).to_radians();
    let f = (93.272_095_0 + 483_202.017_523_3 * t).rem_euclid(360.0).to_radians();

    let lon_deg = l
        + 6.289 * mp.sin()
        - 1.274 * (mp - 2.0 * d).sin()
        + 0.658 * (2.0 * d).sin()
        - 0.186 * m.sin()
        - 0.059 * (2.0 * mp - 2.0 * d).sin()
        - 0.057 * (mp - 2.0 * d + m).sin()
        + 0.053 * (mp + 2.0 * d).sin();

    let lat_deg = 5.128 * f.sin() + 0.281 * (mp + f).sin() - 0.278 * (f - mp).sin();

    let dist_km = 385_000.56
        - 20_905.355 * mp.cos()
        - 3_699.111 * (2.0 * d - mp).cos()
        - 2_955.968 * (2.0 * d).cos()
        - 569.925 * (2.0 * mp).cos();

    (lon_deg.rem_euclid(360.0), lat_deg, dist_km)
}

/// The sublunar point (Moon in the zenith) at `time` — [`crate::orbit::solar::subsolar_point`]'s
/// twin, built the same way: an ecliptic longitude/latitude rotated into
/// equatorial declination/right ascension through the obliquity, then the
/// right ascension turned into a geographic longitude by subtracting
/// Greenwich sidereal time. Declination and ecliptic latitude aren't the same
/// angle in general — unlike the Sun, the Moon's orbit is inclined ~5° to the
/// ecliptic, so the full rotation (not the Sun's latitude-free shortcut) is
/// needed here.
pub fn sublunar_point(time: DateTime<Utc>) -> GeoPoint {
    let (lon_deg, lat_deg, _) = lunar_ecliptic(time);
    let (lon, lat) = (lon_deg.to_radians(), lat_deg.to_radians());
    let obliquity = obliquity_rad(time);

    let declination = (lat.sin() * obliquity.cos() + lat.cos() * obliquity.sin() * lon.sin()).asin();
    let right_ascension =
        (lon.sin() * obliquity.cos() - lat.tan() * obliquity.sin()).atan2(lon.cos());

    let geo_lon = wrap_longitude((right_ascension - gmst_rad(time)).to_degrees());
    GeoPoint { lat_deg: declination.to_degrees(), lon_deg: geo_lon, alt_km: 0.0 }
}

/// Earth–Moon distance at `time`, km.
pub fn lunar_distance_km(time: DateTime<Utc>) -> f64 {
    lunar_ecliptic(time).2
}

/// The Moon's Earth-centred ECEF position at `time` — [`sublunar_point`]'s
/// direction pushed out to [`lunar_distance_km`]. Real range, not a
/// direction-only placeholder, so [`geo::look_angles`](crate::geo::look_angles)
/// against it recovers genuine topocentric azimuth/elevation, parallax
/// included — worth doing here because the Moon is close enough (~60 Earth
/// radii) for parallax to shift its look angle by up to about a degree,
/// unlike the Sun.
fn moon_ecef_km(time: DateTime<Utc>) -> [f64; 3] {
    let p = sublunar_point(time);
    let dist = lunar_distance_km(time);
    let u = radial_unit(p.lat_deg, p.lon_deg);
    [u[0] * dist, u[1] * dist, u[2] * dist]
}

/// Topocentric azimuth/elevation/range of the Moon from `observer` at `time`.
/// `range_km` is the real Earth–Moon distance, not the (meaningless for the
/// Moon) slant range a satellite's `RANGE` row shows.
pub fn moon_look_angles(observer: &GeoPoint, time: DateTime<Utc>) -> LookAngles {
    look_angles(observer, moon_ecef_km(time))
}

/// Topocentric azimuth/elevation of the Sun from `observer` at `time`, via
/// the same "real ECEF position, then `look_angles`" route as
/// [`moon_look_angles`]. At [`AU_KM`] the parallax `look_angles` corrects for
/// is itself only ~9 arc-seconds (vs. up to a degree for the much closer
/// Moon) — invisible here either way — but going through the same path as
/// the Moon rather than a separate direction-only formula means one code
/// path handles both.
pub fn sun_look_angles(observer: &GeoPoint, time: DateTime<Utc>) -> LookAngles {
    use crate::orbit::solar::subsolar_point;
    let s = subsolar_point(time);
    let u = radial_unit(s.lat_deg, s.lon_deg);
    look_angles(observer, [u[0] * AU_KM, u[1] * AU_KM, u[2] * AU_KM])
}

/// Azimuth/elevation of a fixed star from `observer` at `time`, given its
/// right ascension and declination (J2000, degrees) — precession is ignored,
/// which drifts a star's true position by a few arc-minutes per decade, well
/// under this module's own accuracy floor and invisible on a terminal disc.
///
/// A star has no meaningful distance to place it at, so rather than build an
/// ECEF position the way [`moon_look_angles`]/[`sun_look_angles`] do, its
/// right ascension is turned into the same Greenwich-relative "geographic
/// longitude" [`sublunar_point`] uses, and the resulting direction is placed
/// a large fixed distance (`FAR_KM` below) out from the observer's own
/// position — so the observer-to-target vector `look_angles` builds is
/// exactly that direction regardless of where the observer stands, which is
/// what "a star's alt-az" actually means: no parallax, unlike the Moon.
pub fn star_look_angles(ra_deg: f64, dec_deg: f64, observer: &GeoPoint, time: DateTime<Utc>) -> LookAngles {
    /// Far enough that `observer.to_ecef_km()` (a few thousand km) is
    /// negligible against it, so `look_angles` recovers the star's direction
    /// rather than any real range.
    const FAR_KM: f64 = 1e12;
    let geo_lon = wrap_longitude(ra_deg - gmst_rad(time).to_degrees());
    let dir = radial_unit(dec_deg, geo_lon);
    let obs = observer.to_ecef_km();
    look_angles(
        observer,
        [obs[0] + dir[0] * FAR_KM, obs[1] + dir[1] * FAR_KM, obs[2] + dir[2] * FAR_KM],
    )
}

/// The Moon's illuminated fraction and waxing/waning sense at one instant.
#[derive(Debug, Clone, Copy)]
pub struct MoonPhase {
    /// Illuminated fraction of the disc, `0.0` new to `1.0` full.
    pub illuminated: f64,
    /// Waxing (growing, between new and full) vs waning (shrinking, between
    /// full and new).
    pub waxing: bool,
    /// Days since the last new moon, `0` to just under [`SYNODIC_MONTH_DAYS`].
    pub age_days: f64,
}

impl MoonPhase {
    /// One of the eight traditional phase names.
    pub fn name(&self) -> &'static str {
        NAMES[self.octant()]
    }

    /// A single geometric glyph standing in for the phase — the same "shape
    /// implies meaning" idiom the rest of the map's markers use, so the Moon
    /// marker draws its own phase rather than needing a legend.
    pub fn glyph(&self) -> &'static str {
        GLYPHS[self.octant()]
    }

    /// Which of the eight 45°-wide phase segments `self` falls in, indexed
    /// the way [`NAMES`]/[`GLYPHS`] are: 0 new, 2 first quarter, 4 full, 6
    /// last quarter, odd indices the crescent/gibbous segments between them.
    fn octant(&self) -> usize {
        let angle = if self.waxing {
            self.age_days / SYNODIC_MONTH_DAYS * 360.0
        } else {
            360.0 - self.age_days / SYNODIC_MONTH_DAYS * 360.0
        };
        ((angle / 45.0).round() as usize) % 8
    }
}

const NAMES: [&str; 8] = [
    "new moon",
    "waxing crescent",
    "first quarter",
    "waxing gibbous",
    "full moon",
    "waning gibbous",
    "last quarter",
    "waning crescent",
];

/// Circle-quadrant glyphs, single-width like every other marker on the map
/// (`ui::mod`'s glyph family note). Waxing and waning gibbous/crescent share a
/// glyph — there is no separate "mirrored" character in this Unicode block —
/// so the shape says "gibbous"/"crescent" and [`MoonPhase::name`] is what
/// carries waxing vs waning.
const GLYPHS: [&str; 8] = ["○", "◔", "◑", "◕", "●", "◕", "◑", "◔"];

/// The Moon's phase at `time`: the phase angle is the difference between the
/// Moon's and the Sun's geocentric ecliptic longitude — `0°` at new moon (the
/// two coincide), `180°` at full (opposite) — and the illuminated fraction
/// follows from it as `(1 - cos(phase angle)) / 2`, the standard low-order
/// approximation that ignores the small further correction from the Earth–
/// Moon–Sun triangle's actual geometry (itself a fraction of a percent).
pub fn moon_phase(time: DateTime<Utc>) -> MoonPhase {
    let (moon_lon, _, _) = lunar_ecliptic(time);
    let sun_lon = sun_ecliptic_longitude_deg(time);
    let angle_deg = (moon_lon - sun_lon).rem_euclid(360.0);

    let illuminated = (1.0 - angle_deg.to_radians().cos()) / 2.0;
    let waxing = angle_deg < 180.0;
    let age_days = angle_deg / 360.0 * SYNODIC_MONTH_DAYS;
    MoonPhase { illuminated, waxing, age_days }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn sublunar_latitude_stays_within_the_moons_orbital_inclination() {
        // The Moon's orbit is inclined ~5.14° to the ecliptic and the
        // ecliptic itself is tilted ~23.44° to the equator, so its
        // declination — and so the sublunar latitude — never strays far
        // outside that combined band, at any date.
        let mut t = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        for _ in 0..400 {
            let p = sublunar_point(t);
            assert!(p.lat_deg.abs() <= 29.0, "{t}: sublunar lat {:.2}", p.lat_deg);
            t += chrono::Duration::hours(18);
        }
    }

    #[test]
    fn lunar_distance_stays_within_the_known_perigee_apogee_band() {
        // The Moon's distance ranges roughly 356,500–406,700 km over a full
        // anomalistic month; sweep a couple of months and check every sample
        // lands well inside that band (the abridged series can be off by a
        // few hundred km, so some margin is kept).
        let mut t = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        for _ in 0..120 {
            let d = lunar_distance_km(t);
            assert!((350_000.0..410_000.0).contains(&d), "{t}: distance {d:.0} km");
            t += chrono::Duration::hours(12);
        }
    }

    #[test]
    fn moon_phase_is_full_when_the_moon_is_opposite_the_sun() {
        // A real full moon (2026-01-03, ~10:04 UTC per public almanacs):
        // illumination should read near-total and the phase should be
        // right at the waxing/waning boundary — this cross-checks
        // `moon_phase` against `sun_ecliptic_longitude_deg` rather than a
        // hand-picked constant.
        let full = Utc.with_ymd_and_hms(2026, 1, 3, 10, 0, 0).unwrap();
        let phase = moon_phase(full);
        assert!(phase.illuminated > 0.99, "illuminated {:.3}", phase.illuminated);
        assert_eq!(phase.name(), "full moon");
    }

    #[test]
    fn moon_phase_is_new_when_the_moon_sits_at_the_suns_longitude() {
        // A real new moon (2026-01-18, ~19:52 UTC).
        let new = Utc.with_ymd_and_hms(2026, 1, 18, 20, 0, 0).unwrap();
        let phase = moon_phase(new);
        assert!(phase.illuminated < 0.01, "illuminated {:.3}", phase.illuminated);
        assert_eq!(phase.name(), "new moon");
    }

    #[test]
    fn moon_phase_advances_from_new_to_full_and_back_over_a_synodic_month() {
        let new = Utc.with_ymd_and_hms(2026, 1, 18, 20, 0, 0).unwrap();
        let week_later = new + chrono::Duration::days(7);
        let phase = moon_phase(week_later);
        assert!(phase.waxing, "a week after new moon should still be waxing");
        assert!(
            phase.illuminated > 0.1 && phase.illuminated < 0.9,
            "illuminated {:.3} should be mid-cycle",
            phase.illuminated
        );

        let three_weeks_later = new + chrono::Duration::days(21);
        assert!(!moon_phase(three_weeks_later).waxing, "three weeks in should be waning");
    }

    #[test]
    fn polaris_sits_at_the_observers_latitude_from_any_longitude() {
        // Polaris (RA 37.95°, Dec 89.26°) sits almost exactly over the north
        // celestial pole, so its elevation as seen from anywhere is close to
        // the observer's own latitude — the textbook fact that makes it a
        // navigational fixed point. True at any time of day, since Polaris
        // barely moves.
        let t = Utc.with_ymd_and_hms(2026, 6, 15, 3, 0, 0).unwrap();
        for lat in [10.0, 35.0, 51.6, 70.0] {
            for lon in [-120.0, 0.0, 60.0, 150.0] {
                let obs = GeoPoint::new(lat, lon, 0.0);
                let la = star_look_angles(37.95, 89.26, &obs, t);
                assert!(
                    (la.elevation_deg - lat).abs() < 1.0,
                    "lat {lat} lon {lon}: Polaris elevation {:.2}",
                    la.elevation_deg
                );
            }
        }
    }

    #[test]
    fn a_star_on_the_observers_meridian_and_equator_reads_due_south_at_the_zenith_complement() {
        // A star at declination 0 and right ascension equal to the local
        // sidereal time sits on the observer's meridian. From 30°N the
        // celestial equator crosses the meridian south of the zenith, at
        // elevation 90° - |latitude| = 60°, due south — this is really just
        // `star_look_angles` agreeing with plain spherical trigonometry for
        // the simplest case.
        let t = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).unwrap();
        let obs = GeoPoint::new(30.0, 20.0, 0.0);
        // Right ascension that puts the star on this observer's meridian
        // right now: ra = gmst + observer longitude (both in degrees).
        let ra = (gmst_rad(t).to_degrees() + obs.lon_deg).rem_euclid(360.0);
        let la = star_look_angles(ra, 0.0, &obs, t);
        assert!((la.elevation_deg - 60.0).abs() < 0.5, "elevation {:.2}", la.elevation_deg);
        // At 30°N the celestial equator crosses the meridian south of the
        // zenith, so a declination-0 star culminates due south.
        assert!((la.azimuth_deg - 180.0).abs() < 0.5, "expected due south, got az {:.2}", la.azimuth_deg);
    }

    #[test]
    fn moon_look_angles_agree_in_sign_with_sublunar_point_directly_overhead() {
        // Standing exactly at the sublunar point, the Moon must read at
        // (very close to) the zenith — the cross-check that ties
        // `moon_look_angles`'s ECEF-position route back to `sublunar_point`.
        let t = Utc.with_ymd_and_hms(2026, 5, 20, 6, 0, 0).unwrap();
        let sub = sublunar_point(t);
        let la = moon_look_angles(&sub, t);
        assert!(la.elevation_deg > 89.0, "elevation {:.3}", la.elevation_deg);
    }
}
