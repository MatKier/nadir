//! Local orbital mechanics: SGP4 propagation, ground tracks, solar geometry and
//! pass prediction. None of this touches the network — a cached TLE is enough.

pub mod accuracy;
pub mod passes;
pub mod propagate;
pub mod solar;

pub use accuracy::{Accuracy, Confidence};
pub use passes::{predict_passes, Pass};
pub use propagate::{OrbitClass, OrbitPlane, OrbitShape, SatState, Tracker};

/// A real ISS element set fetched from Celestrak, shared by every test in this
/// crate that needs a `Tracker` — kept in one place instead of copy-pasted per
/// module.
#[cfg(test)]
pub(crate) const ISS_GP_JSON: &str = r#"[{"OBJECT_NAME":"ISS (ZARYA)","OBJECT_ID":"1998-067A",
"EPOCH":"2026-09-04T01:53:59.631360","MEAN_MOTION":15.48983228,"ECCENTRICITY":0.0005012,
"INCLINATION":51.6313,"RA_OF_ASC_NODE":269.6269,"ARG_OF_PERICENTER":105.0359,
"MEAN_ANOMALY":255.1184,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":25544,
"ELEMENT_SET_NO":999,"REV_AT_EPOCH":58398,"BSTAR":6.928243e-5,"MEAN_MOTION_DOT":3.366e-5,
"MEAN_MOTION_DDOT":0}]"#;

#[cfg(test)]
pub(crate) fn test_tracker() -> Tracker {
    Tracker::from_gp_json(ISS_GP_JSON).expect("fixture parses")
}

/// A real Sentinel-2A element set (NORAD 40697): a textbook sun-synchronous
/// orbit, used to test SSO detection end-to-end from real inclination/altitude
/// numbers rather than the hand-picked values `classify`'s own unit tests use.
#[cfg(test)]
pub(crate) const SENTINEL2A_GP_JSON: &str = r#"[{"OBJECT_NAME":"SENTINEL-2A","OBJECT_ID":"2015-028A",
"EPOCH":"2026-09-04T08:12:00.000000","MEAN_MOTION":14.30824323,"ECCENTRICITY":0.0001248,
"INCLINATION":98.5622,"RA_OF_ASC_NODE":100.4130,"ARG_OF_PERICENTER":90.0338,
"MEAN_ANOMALY":270.0966,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":40697,
"ELEMENT_SET_NO":999,"REV_AT_EPOCH":57500,"BSTAR":1.4126e-5,"MEAN_MOTION_DOT":2.3e-7,
"MEAN_MOTION_DDOT":0}]"#;

#[cfg(test)]
pub(crate) fn test_sso_tracker() -> Tracker {
    Tracker::from_gp_json(SENTINEL2A_GP_JSON).expect("fixture parses")
}

/// A *synthetic* element set — the ISS fixture with its inclination edited to
/// exactly 90° and a made-up catalogue number. Unlike its two real neighbours
/// above there is no cataloged satellite close enough to a true polar orbit to
/// exercise the near-pole ground-track case, where a step straddling the pole
/// jumps just under 180° of longitude and `split_at_antimeridian` deliberately
/// does not split it (see `Tracker::ground_track`).
#[cfg(test)]
pub(crate) const POLAR_GP_JSON: &str = r#"[{"OBJECT_NAME":"SYNTHETIC POLAR","OBJECT_ID":"0000-000A",
"EPOCH":"2026-09-04T01:53:59.631360","MEAN_MOTION":15.48983228,"ECCENTRICITY":0.0005012,
"INCLINATION":90.0,"RA_OF_ASC_NODE":269.6269,"ARG_OF_PERICENTER":105.0359,
"MEAN_ANOMALY":255.1184,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":99999,
"ELEMENT_SET_NO":999,"REV_AT_EPOCH":58398,"BSTAR":6.928243e-5,"MEAN_MOTION_DOT":3.366e-5,
"MEAN_MOTION_DDOT":0}]"#;

#[cfg(test)]
pub(crate) fn test_polar_tracker() -> Tracker {
    Tracker::from_gp_json(POLAR_GP_JSON).expect("fixture parses")
}

/// A *synthetic* deep-space element set: a near-circular geostationary orbit
/// with `BSTAR` forced to exactly zero, which real GEO element sets very often
/// carry. With no drag term the B\*-sensitivity probe in `orbit::accuracy`
/// measures nothing, so this fixture is what proves the per-regime error floor
/// still produces a growing estimate on its own. Like `POLAR_GP_JSON` it is
/// hand-built: a catalogued GEO's `BSTAR` is usually a small non-zero fit
/// artefact rather than a clean zero.
#[cfg(test)]
pub(crate) const GEO_ZERO_DRAG_GP_JSON: &str = r#"[{"OBJECT_NAME":"SYNTHETIC GEO","OBJECT_ID":"0000-000B",
"EPOCH":"2026-09-04T00:00:00.000000","MEAN_MOTION":1.00273790,"ECCENTRICITY":0.0001500,
"INCLINATION":0.0400,"RA_OF_ASC_NODE":95.0000,"ARG_OF_PERICENTER":270.0000,
"MEAN_ANOMALY":90.0000,"EPHEMERIS_TYPE":0,"CLASSIFICATION_TYPE":"U","NORAD_CAT_ID":99998,
"ELEMENT_SET_NO":999,"REV_AT_EPOCH":12000,"BSTAR":0,"MEAN_MOTION_DOT":0,"MEAN_MOTION_DDOT":0}]"#;

#[cfg(test)]
pub(crate) fn test_geo_zero_drag_tracker() -> Tracker {
    Tracker::from_gp_json(GEO_ZERO_DRAG_GP_JSON).expect("fixture parses")
}
