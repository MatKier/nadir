//! Live cross-check against the real internet: nadir's own local propagation
//! must agree with an independent feed (WhereTheISS.at) to within a few
//! kilometres, and its eclipse test must agree with that feed's `visibility`
//! flag. This exercises the actual pipeline in `nadir::orbit`, not a copy.
//!
//! Ignored by default — run explicitly with `cargo test -- --ignored` when
//! online.

use chrono::Utc;
use nadir::geo::great_circle_km;
use nadir::orbit::Tracker;

#[derive(serde::Deserialize)]
struct IssNow {
    latitude: f64,
    longitude: f64,
    altitude: f64,
    visibility: String,
}

#[tokio::test]
#[ignore = "hits the live network"]
async fn local_propagation_agrees_with_wheretheiss_at() {
    let client = reqwest::Client::builder()
        .user_agent("nadir-test/0.1")
        .build()
        .expect("client");

    let gp_json = client
        .get("https://celestrak.org/NORAD/elements/gp.php?CATNR=25544&FORMAT=json")
        .send()
        .await
        .expect("celestrak request")
        .text()
        .await
        .expect("celestrak body");
    let tracker = Tracker::from_gp_json(&gp_json).expect("celestrak GP JSON parses");

    let theirs: IssNow = client
        .get("https://api.wheretheiss.at/v1/satellites/25544")
        .send()
        .await
        .expect("wheretheiss.at request")
        .json()
        .await
        .expect("wheretheiss.at body");

    let now = Utc::now();
    let ours = tracker.state_at(now).expect("propagates for the current time");
    let theirs_point = nadir::geo::GeoPoint::new(theirs.latitude, theirs.longitude, theirs.altitude);

    let distance_km = great_circle_km(&ours.sub_point, &theirs_point);
    assert!(
        distance_km < 25.0,
        "nadir disagrees with WhereTheISS.at by {distance_km:.1} km \
         (ours: {:.3},{:.3},{:.0}km; theirs: {:.3},{:.3},{:.0}km)",
        ours.sub_point.lat_deg,
        ours.sub_point.lon_deg,
        ours.sub_point.alt_km,
        theirs.latitude,
        theirs.longitude,
        theirs.altitude,
    );

    let their_sunlit = theirs.visibility != "eclipsed";
    assert_eq!(
        ours.sunlit, their_sunlit,
        "eclipse state disagrees with WhereTheISS.at's visibility field"
    );

    // 7.66 km/s is the textbook ISS orbital speed; this also pins the sgp4
    // crate's velocity units (km/s, not km/min) against physical reality.
    assert!(
        (7.0..8.2).contains(&ours.speed_kms),
        "propagated speed {:.3} km/s is not a plausible ISS velocity",
        ours.speed_kms
    );
}
