//! NOAA Space Weather Prediction Center feeds: planetary K-index, solar wind,
//! the R/S/G storm scales and the OVATION aurora nowcast.

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::api::get_json;

const KP_URL: &str = "https://services.swpc.noaa.gov/products/noaa-planetary-k-index.json";
const WIND_SPEED_URL: &str =
    "https://services.swpc.noaa.gov/products/summary/solar-wind-speed.json";
const WIND_MAG_URL: &str =
    "https://services.swpc.noaa.gov/products/summary/solar-wind-mag-field.json";
const SCALES_URL: &str = "https://services.swpc.noaa.gov/products/noaa-scales.json";
const AURORA_URL: &str = "https://services.swpc.noaa.gov/json/ovation_aurora_latest.json";

/// A single 3-hour planetary K-index sample.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KpPoint {
    pub time_tag: String,
    #[serde(rename = "Kp")]
    pub kp: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct WindSpeed {
    proton_speed: f64,
}

#[derive(Debug, Clone, Deserialize)]
struct WindMag {
    bt: f64,
    bz_gsm: f64,
}

/// One storm-scale reading (`R`, `S` or `G`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ScaleValue {
    #[serde(rename = "Scale")]
    pub scale: Option<String>,
    #[serde(rename = "Text")]
    pub text: Option<String>,
}

impl ScaleValue {
    /// Numeric level 0–5, defaulting to 0 when absent.
    pub fn level(&self) -> u8 {
        self.scale
            .as_deref()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct ScaleDay {
    #[serde(rename = "R", default)]
    r: ScaleValue,
    #[serde(rename = "S", default)]
    s: ScaleValue,
    #[serde(rename = "G", default)]
    g: ScaleValue,
}

/// Current radio-blackout / radiation-storm / geomagnetic-storm scales.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StormScales {
    pub r: ScaleValue,
    pub s: ScaleValue,
    pub g: ScaleValue,
}

/// The compact space-weather picture nadir shows.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Indices {
    /// K-index history, oldest first, roughly the last day and a half.
    pub kp: Vec<KpPoint>,
    /// Solar wind bulk speed, km/s.
    pub wind_speed_kms: Option<f64>,
    /// Interplanetary field magnitude and north–south component, nT.
    pub bt_nt: Option<f64>,
    pub bz_nt: Option<f64>,
    pub scales: StormScales,
}

impl Indices {
    pub fn latest_kp(&self) -> Option<f64> {
        self.kp.last().map(|p| p.kp)
    }
}

/// Fetch K-index, solar wind and storm scales together.
pub async fn fetch_indices(client: &reqwest::Client) -> Result<Indices> {
    let kp: Vec<KpPoint> = get_json(client, KP_URL).await.context("K-index feed")?;

    let wind_speed = get_json::<Vec<WindSpeed>>(client, WIND_SPEED_URL)
        .await
        .ok()
        .and_then(|v| v.into_iter().next())
        .map(|w| w.proton_speed);

    let wind_mag = get_json::<Vec<WindMag>>(client, WIND_MAG_URL)
        .await
        .ok()
        .and_then(|v| v.into_iter().next());

    let scales_map: HashMap<String, ScaleDay> =
        get_json(client, SCALES_URL).await.context("storm-scales feed")?;
    let today = scales_map.get("0").cloned().unwrap_or_default();

    Ok(Indices {
        kp,
        wind_speed_kms: wind_speed,
        bt_nt: wind_mag.as_ref().map(|m| m.bt),
        bz_nt: wind_mag.as_ref().map(|m| m.bz_gsm),
        scales: StormScales {
            r: today.r,
            s: today.s,
            g: today.g,
        },
    })
}

#[derive(Debug, Clone, Deserialize)]
struct AuroraRaw {
    coordinates: Vec<[f64; 3]>,
}

/// The OVATION aurora nowcast as a lookup grid on a 1° mesh.
#[derive(Debug, Clone, Default)]
pub struct AuroraGrid {
    /// `prob[lon_index][lat_index]`, lon 0..360, lat 0..181 (−90..90).
    prob: Vec<Vec<u8>>,
}

impl AuroraGrid {
    /// Aurora probability (%) nearest the given ground point.
    pub fn probability_at(&self, lat_deg: f64, lon_deg: f64) -> Option<u8> {
        if self.prob.is_empty() {
            return None;
        }
        let lon_idx = (lon_deg.rem_euclid(360.0)).round() as usize % 360;
        let lat_idx = ((lat_deg + 90.0).round() as i64).clamp(0, 180) as usize;
        self.prob.get(lon_idx).and_then(|col| col.get(lat_idx)).copied()
    }
}

/// Fetch and index the OVATION aurora nowcast (~900 kB payload).
pub async fn fetch_aurora(client: &reqwest::Client) -> Result<AuroraGrid> {
    let raw: AuroraRaw = get_json(client, AURORA_URL).await.context("aurora feed")?;
    let mut prob = vec![vec![0u8; 181]; 360];
    for [lon, lat, val] in raw.coordinates {
        let lon_idx = (lon.rem_euclid(360.0)).round() as usize % 360;
        let lat_idx = ((lat + 90.0).round() as i64).clamp(0, 180) as usize;
        prob[lon_idx][lat_idx] = val.clamp(0.0, 100.0) as u8;
    }
    Ok(AuroraGrid { prob })
}
