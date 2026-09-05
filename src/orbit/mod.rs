//! Local orbital mechanics: SGP4 propagation, ground tracks, solar geometry and
//! pass prediction. None of this touches the network — a cached TLE is enough.

pub mod passes;
pub mod propagate;
pub mod solar;

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
