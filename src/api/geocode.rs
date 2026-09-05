//! Open-Meteo's geocoding API: resolve a free-text place name (city, region,
//! landmark) to coordinates. Keyless, generously rate-limited, and separate
//! from the IP-based first-run guess in [`crate::api::geoip`] — this one
//! answers an explicit place the user names with `--location`.

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;

const URL: &str = "https://geocoding-api.open-meteo.com/v1/search";

#[derive(Debug, Deserialize, Default)]
struct Response {
    #[serde(default)]
    results: Vec<Candidate>,
}

#[derive(Debug, Deserialize)]
struct Candidate {
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    admin1: Option<String>,
    #[serde(default)]
    country: Option<String>,
}

/// A resolved place: coordinates plus a human-readable label for confirmation.
#[derive(Debug, Clone)]
pub struct Place {
    pub lat: f64,
    pub lon: f64,
    pub label: String,
}

/// Resolve a free-text place name to coordinates, taking Open-Meteo's
/// top-ranked match. An ambiguous query (e.g. a common city name shared by
/// several countries) silently takes the most prominent match; make the
/// query more specific (add a country, e.g. "Springfield, Illinois") if that
/// isn't the one you meant.
pub async fn resolve(client: &reqwest::Client, query: &str) -> Result<Place> {
    let resp: Response = client
        .get(URL)
        .query(&[("name", query), ("count", "1"), ("language", "en"), ("format", "json")])
        .send()
        .await
        .with_context(|| format!("requesting a location match for '{query}'"))?
        .error_for_status()
        .with_context(|| format!("bad status resolving '{query}'"))?
        .json()
        .await
        .with_context(|| format!("decoding location results for '{query}'"))?;

    let top = resp
        .results
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no place found matching '{query}'"))?;

    let label = [Some(top.name), top.admin1, top.country]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");

    Ok(Place {
        lat: top.latitude,
        lon: top.longitude,
        label,
    })
}
