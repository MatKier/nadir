//! Launch Library 2 (The Space Devs) — upcoming orbital launches.
//!
//! The anonymous tier allows only ~15 requests per hour, so a [`RateGuard`]
//! wraps every call and the caller serves cache when the budget is spent.
//! The production host also throttles per-IP more aggressively than that
//! budget alone would suggest (shared across every anonymous caller, not just
//! this app), so a `429` falls back once to `lldev.thespacedevs.com` — The
//! Space Devs' public test mirror, same shape, served from their cache.

use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::api::RateGuard;

const PRIMARY_URL: &str =
    "https://ll.thespacedevs.com/2.2.0/launch/upcoming/?limit=8&mode=normal&hide_recent_previous=true";
const MIRROR_URL: &str =
    "https://lldev.thespacedevs.com/2.2.0/launch/upcoming/?limit=8&mode=normal&hide_recent_previous=true";

/// A conservative budget: comfortably under the ~15/hour anonymous limit even
/// with retries and a manual refresh or two.
pub fn rate_guard() -> RateGuard {
    RateGuard::per_hour(8)
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Named {
    #[serde(default)]
    name: Option<String>,
}

// `mode=normal` returns the full launch detail shape: the provider is a
// nested `launch_service_provider` object, and `pad` is itself an object
// carrying the pad's own name plus its coordinates (as strings) and a
// nested `location` object for the site name. We need `mode=normal` (over
// the leaner `mode=list`) specifically for `pad.latitude`/`pad.longitude`,
// which `mode=list` doesn't send at all.
#[derive(Debug, Clone, Deserialize, Default)]
struct LaunchRaw {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    net: Option<DateTime<Utc>>,
    #[serde(default)]
    status: Option<Named>,
    #[serde(default)]
    launch_service_provider: Option<Named>,
    #[serde(default)]
    pad: Option<PadRaw>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PadRaw {
    #[serde(default)]
    name: Option<String>,
    /// Sent as a JSON string, not a number.
    #[serde(default)]
    latitude: Option<String>,
    /// Sent as a JSON string, not a number.
    #[serde(default)]
    longitude: Option<String>,
    #[serde(default)]
    location: Option<Named>,
}

#[derive(Debug, Clone, Deserialize)]
struct Page {
    results: Vec<LaunchRaw>,
}

/// One upcoming launch, flattened to what the panel needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launch {
    pub name: String,
    pub provider: String,
    /// Pad and site, e.g. "SLC-4E, Vandenberg SFB, CA, USA" — shown on the
    /// map next to the pad marker, and on the highlighted launch's second row.
    pub pad: String,
    pub status: String,
    /// "No Earlier Than" time, if known.
    pub net: Option<DateTime<Utc>>,
    /// Pad coordinates, when the feed knows them — used to mark the pad on
    /// the map. `serde(default)` keeps a cache written before this field
    /// existed loadable.
    #[serde(default)]
    pub pad_lat: Option<f64>,
    #[serde(default)]
    pub pad_lon: Option<f64>,
}

impl Launch {
    /// Time until launch from `now`; negative once the NET has passed.
    pub fn t_minus(&self, now: DateTime<Utc>) -> Option<chrono::Duration> {
        self.net.map(|t| t - now)
    }
}

/// Which host a launch list actually came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Origin {
    /// The production API, live.
    Primary,
    /// The public test mirror, used when the production host is throttling us.
    Mirror,
}

/// A fetched (or cached) launch list plus where it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Launches {
    pub list: Vec<Launch>,
    pub origin: Origin,
}

/// The outcome of one fetch attempt.
pub enum Fetched {
    Ok(Launches),
    /// The production host is rate-limiting us and the mirror didn't help
    /// either; retry after roughly this long.
    Throttled(Duration),
    /// Our own client-side rate budget is spent; keep showing cached data.
    BudgetSpent,
}

/// The feed sends coordinates as strings, and omits them for pads it
/// doesn't have fixed; either way a launch without usable coordinates just
/// doesn't get a map marker.
fn coord(raw: Option<String>, limit: f64) -> Option<f64> {
    raw.and_then(|s| s.parse::<f64>().ok())
        .filter(|v| v.is_finite() && v.abs() <= limit)
}

fn flatten(raw: LaunchRaw) -> Launch {
    let (pad_name, pad_loc, pad_lat, pad_lon) = match raw.pad {
        Some(p) => (
            p.name,
            p.location.and_then(|l| l.name),
            coord(p.latitude, 90.0),
            coord(p.longitude, 180.0),
        ),
        None => (None, None, None, None),
    };
    let pad = match (pad_name, pad_loc) {
        (Some(p), Some(loc)) => format!("{p}, {loc}"),
        (Some(p), None) => p,
        (None, Some(loc)) => loc,
        (None, None) => "—".to_string(),
    };
    Launch {
        name: raw.name.unwrap_or_else(|| "Unknown vehicle".to_string()),
        provider: raw
            .launch_service_provider
            .and_then(|p| p.name)
            .unwrap_or_else(|| "—".to_string()),
        pad,
        status: raw
            .status
            .and_then(|s| s.name)
            .unwrap_or_else(|| "TBD".to_string()),
        net: raw.net,
        pad_lat,
        pad_lon,
    }
}

/// Seconds to wait before retrying, from a `429`'s `Retry-After` header
/// (falls back to 5 minutes when absent or unparseable).
fn retry_after(resp: &reqwest::Response) -> Duration {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(300))
}

async fn fetch_one(client: &reqwest::Client, url: &str) -> Result<std::result::Result<Page, Duration>> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("requesting {url}"))?;
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(Err(retry_after(&resp)));
    }
    let page = resp
        .error_for_status()
        .with_context(|| format!("bad status from {url}"))?
        .json::<Page>()
        .await
        .with_context(|| format!("decoding JSON from {url}"))?;
    Ok(Ok(page))
}

/// Fetch upcoming launches, respecting `guard`. Falls back to the public
/// mirror on a `429` from the primary host, and reports how long to wait
/// before trying again when both are throttled.
pub async fn fetch(client: &reqwest::Client, guard: &RateGuard) -> Result<Fetched> {
    if !guard.try_take().await {
        return Ok(Fetched::BudgetSpent);
    }
    match fetch_one(client, PRIMARY_URL).await? {
        Ok(page) => Ok(Fetched::Ok(Launches {
            list: page.results.into_iter().map(flatten).collect(),
            origin: Origin::Primary,
        })),
        Err(retry) => match fetch_one(client, MIRROR_URL).await? {
            Ok(page) => Ok(Fetched::Ok(Launches {
                list: page.results.into_iter().map(flatten).collect(),
                origin: Origin::Mirror,
            })),
            Err(mirror_retry) => Ok(Fetched::Throttled(retry.min(mirror_retry))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed `mode=normal` launch, shaped like the live payload verified
    /// against `lldev.thespacedevs.com`.
    fn sample_raw() -> LaunchRaw {
        serde_json::from_str(
            r#"{
                "name": "Falcon 9 Block 5 | Starlink Group 15-24",
                "net": "2026-09-10T12:00:00Z",
                "status": {"name": "Go"},
                "launch_service_provider": {"name": "SpaceX"},
                "pad": {
                    "name": "SLC-4E",
                    "latitude": "34.632",
                    "longitude": "-120.611",
                    "location": {"name": "Vandenberg SFB, CA, USA"}
                }
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn flatten_reads_normal_mode_shape() {
        let l = flatten(sample_raw());
        assert_eq!(l.name, "Falcon 9 Block 5 | Starlink Group 15-24");
        assert_eq!(l.provider, "SpaceX");
        assert_eq!(l.status, "Go");
        assert_eq!(l.pad, "SLC-4E, Vandenberg SFB, CA, USA");
        assert_eq!(l.pad_lat, Some(34.632));
        assert_eq!(l.pad_lon, Some(-120.611));
    }

    #[test]
    fn flatten_tolerates_missing_pad() {
        let mut raw = sample_raw();
        raw.pad = None;
        let l = flatten(raw);
        assert_eq!(l.pad, "—");
        assert_eq!(l.pad_lat, None);
        assert_eq!(l.pad_lon, None);
    }

    #[test]
    fn coord_rejects_unparseable_and_out_of_range() {
        assert_eq!(coord(None, 90.0), None);
        assert_eq!(coord(Some("not a number".to_string()), 90.0), None);
        assert_eq!(coord(Some("owl".to_string()), 180.0), None);
        assert_eq!(coord(Some("200".to_string()), 90.0), None); // out of range for latitude
        assert_eq!(coord(Some("34.632".to_string()), 90.0), Some(34.632));
    }

    /// A `Launches` blob cached by a build predating `pad_lat`/`pad_lon` must
    /// still deserialize, or offline mode silently loses the launch list.
    #[test]
    fn launches_without_pad_coords_still_deserialize() {
        let json = r#"{
            "list": [{
                "name": "Old Cached Launch",
                "provider": "Some Provider",
                "pad": "Some Pad",
                "status": "TBD",
                "net": null
            }],
            "origin": "Primary"
        }"#;
        let launches: Launches = serde_json::from_str(json).unwrap();
        assert_eq!(launches.list.len(), 1);
        assert_eq!(launches.list[0].pad_lat, None);
        assert_eq!(launches.list[0].pad_lon, None);
    }
}
