//! SatNOGS DB transmitter records: the frequencies a satellite actually
//! downlinks on. Keyless and public.
//!
//! This is deliberately *not* a feed. Celestrak publishes orbits, not radios,
//! and a satellite's downlink is static for years, so there is no `Feed<T>`,
//! no `Notifiers` entry, no status chip, no refresh interval and no cache
//! key: the lookup runs once when a satellite is added to the tracked list,
//! the chosen transmitter is written into `config.toml`, and nothing queries
//! again unless the user asks. From that point `config.toml` — hand-editable —
//! is the source of truth, which is what you want the day SatNOGS is wrong,
//! stale, or has never heard of the object.
//!
//! Two shapes to know about, because every other JSON source in this tree
//! differs: the endpoint answers with a **bare array**, no pagination
//! envelope; and an object with no transmitters (a real, catalogued satellite
//! with no amateur payload — the common case) comes back as `200` with `[]`,
//! not a 404. An empty result is [`Ok`], not an error.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::Transmitter;

const URL: &str = "https://db.satnogs.org/api/transmitters/";

/// Only the fields a downlink readout needs. `baud`, `service`, `type`,
/// `status` and `uuid` all come back on the wire and none of them changes what
/// the DOPP row prints or what the picker needs to tell two transmitters
/// apart — so, per the same rule `celestrak.rs` follows, they are not
/// deserialised and an upstream schema change to them cannot break the lookup.
#[derive(Debug, Deserialize)]
struct Record {
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    alive: bool,
    /// Hertz. Null on uplink-only records — which is why this is an `Option`
    /// and why those records are dropped rather than defaulted to zero.
    #[serde(default)]
    downlink_low: Option<u64>,
    #[serde(default)]
    mode: Option<String>,
}

/// Every live transmitter with a downlink frequency for `norad_id`, ordered by
/// frequency. An empty list — SatNOGS has no record of this object, or none of
/// its transmitters are both alive and receivable — is `Ok(vec![])`, mirroring
/// `celestrak::search_by_name`'s "empty means no match, not a failure".
pub async fn transmitters(client: &reqwest::Client, norad_id: u64) -> Result<Vec<Transmitter>> {
    let id = norad_id.to_string();
    // `alive=true` is a server-side convenience that shrinks the payload; the
    // client-side filter below is the actual invariant, so both are applied.
    let records: Vec<Record> = client
        .get(URL)
        .query(&[
            ("satellite__norad_cat_id", id.as_str()),
            ("alive", "true"),
            ("format", "json"),
        ])
        .send()
        .await
        .with_context(|| format!("requesting SatNOGS transmitters for NORAD {norad_id}"))?
        .error_for_status()
        .with_context(|| format!("bad status from SatNOGS for NORAD {norad_id}"))?
        .json()
        .await
        .with_context(|| format!("decoding SatNOGS transmitters for NORAD {norad_id}"))?;

    let mut out: Vec<Transmitter> = records
        .into_iter()
        .filter(|r| r.alive)
        .filter_map(|r| {
            Some(Transmitter {
                // An uplink-only record has no downlink to shift — drop it
                // rather than let it reach the DOPP row with nothing to tune.
                downlink_hz: r.downlink_low?,
                mode: r.mode.unwrap_or_default(),
                description: r.description.unwrap_or_default(),
            })
        })
        .collect();
    // Ascending by frequency so the picker reads predictably. Two transmitters
    // can share a frequency (the ISS has two on 145.800) and both are kept —
    // they are genuinely different services.
    out.sort_by_key(|t| t.downlink_hz);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "hits the live network"]
    fn the_iss_has_at_least_one_alive_vhf_downlink_on_satnogs() {
        let client = crate::api::client().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let found = rt.block_on(transmitters(&client, 25544)).unwrap();
        assert!(!found.is_empty(), "the ISS always has transmitters on file");
        assert!(found.iter().all(|t| t.downlink_hz > 0), "every entry carries a frequency");
        assert!(
            found.iter().any(|t| t.downlink_hz == 145_800_000),
            "the 145.800 MHz downlink is a fixture of the catalogue"
        );
    }

    /// Sentinel-2A: a real, catalogued satellite with no SatNOGS transmitter
    /// record — the empty answer the picker has to show gracefully, and not as
    /// a network error.
    #[test]
    #[ignore = "hits the live network"]
    fn a_satellite_with_no_amateur_radio_returns_an_empty_list_not_an_error() {
        let client = crate::api::client().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let found = rt
            .block_on(transmitters(&client, 40697))
            .expect("an empty transmitter list must not surface as an error");
        assert!(found.is_empty(), "expected no transmitters, got {found:?}");
    }
}
