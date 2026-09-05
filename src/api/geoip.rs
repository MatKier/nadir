//! Coarse IP geolocation, used once on first run when no ground station is
//! configured. Two providers are tried in order; either may be blocked or rate
//! limited, so a failure here is never fatal.

use anyhow::{anyhow, Result};
use serde::Deserialize;

use crate::api::get_json;

/// A guessed location, city-level at best.
#[derive(Debug, Clone)]
pub struct GuessedLocation {
    pub lat: f64,
    pub lon: f64,
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct IpApiCo {
    latitude: Option<f64>,
    longitude: Option<f64>,
    city: Option<String>,
    country_name: Option<String>,
    #[serde(default)]
    error: bool,
}

#[derive(Debug, Deserialize)]
struct IpApiCom {
    status: String,
    lat: Option<f64>,
    lon: Option<f64>,
    city: Option<String>,
    country: Option<String>,
}

/// Try to guess the caller's location from their IP address.
pub async fn guess(client: &reqwest::Client) -> Result<GuessedLocation> {
    if let Ok(g) = from_ipapi_co(client).await {
        return Ok(g);
    }
    from_ipapi_com(client).await
}

async fn from_ipapi_co(client: &reqwest::Client) -> Result<GuessedLocation> {
    let r: IpApiCo = get_json(client, "https://ipapi.co/json/").await?;
    if r.error {
        return Err(anyhow!("ipapi.co reported an error"));
    }
    match (r.latitude, r.longitude) {
        (Some(lat), Some(lon)) => Ok(GuessedLocation {
            lat,
            lon,
            label: label(r.city, r.country_name),
        }),
        _ => Err(anyhow!("ipapi.co returned no coordinates")),
    }
}

async fn from_ipapi_com(client: &reqwest::Client) -> Result<GuessedLocation> {
    let url = "http://ip-api.com/json/?fields=status,message,city,country,lat,lon";
    let r: IpApiCom = get_json(client, url).await?;
    if r.status != "success" {
        return Err(anyhow!("ip-api.com status: {}", r.status));
    }
    match (r.lat, r.lon) {
        (Some(lat), Some(lon)) => Ok(GuessedLocation {
            lat,
            lon,
            label: label(r.city, r.country),
        }),
        _ => Err(anyhow!("ip-api.com returned no coordinates")),
    }
}

fn label(city: Option<String>, country: Option<String>) -> String {
    match (city, country) {
        (Some(c), Some(k)) => format!("{c}, {k}"),
        (Some(c), None) => c,
        (None, Some(k)) => k,
        (None, None) => "unknown location".to_string(),
    }
}
