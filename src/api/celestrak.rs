//! Celestrak GP element sets. The one feed nadir genuinely depends on — but
//! only every few hours, and the result is cached to disk so `--offline` and a
//! flaky network both keep working.
//!
//! The same endpoint also backs [`search_by_name`], a catalogue name search
//! used to resolve a name the user types into a satellite to track. A bare
//! NORAD id needs no lookup — it's unambiguous by construction, so callers
//! use it directly and only find out here whether it's real when its
//! element set is fetched.

use anyhow::{Context, Result};

use crate::api::get_text;

const BASE: &str = "https://celestrak.org/NORAD/elements/gp.php";

/// One object found by a catalogue lookup: enough to show it and to start
/// tracking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SatMatch {
    pub norad_id: u64,
    pub name: String,
}

/// Fetch the current GP element set for `norad_id` as raw JSON text.
pub async fn fetch_gp_json(client: &reqwest::Client, norad_id: u64) -> Result<String> {
    let url = format!("{BASE}?CATNR={norad_id}&FORMAT=json");
    let body = get_text(client, &url).await?;
    checked_body(&body)?;
    Ok(body)
}

/// Objects whose catalogue name contains `name` (Celestrak's own `NAME=`
/// search: substring, case-insensitive). An empty result means no such
/// object, not a failure. Ranked so a name typed as an exact match — the
/// case that matters most, e.g. "HST" — sorts first, then a prefix match,
/// then everything else alphabetically; capped at 20 results so a broad
/// query (e.g. "STARLINK") doesn't flood the picker.
pub async fn search_by_name(client: &reqwest::Client, name: &str) -> Result<Vec<SatMatch>> {
    let url = format!("{BASE}?NAME={}&FORMAT=json", urlencode(name));
    let body = fetch_tolerating_no_match(client, &url).await?;
    let mut matches = parse_matches(&body)?;
    rank_by_name(&mut matches, name);
    matches.truncate(20);
    Ok(matches)
}

/// Celestrak answers a query that matches nothing with a plain-text line
/// rather than an HTTP error or an empty JSON array, so the shape has to be
/// checked before it's trusted as element data.
fn checked_body(body: &str) -> Result<()> {
    let trimmed = body.trim_start();
    if !trimmed.starts_with('[') && !trimmed.starts_with('{') {
        anyhow::bail!("Celestrak did not return element data: {}", body.trim());
    }
    Ok(())
}

/// Like [`get_text`], but tolerant of how Celestrak actually answers a
/// search that matches nothing: not always the 200 + plain-text line
/// `checked_body` expects — confirmed live, a `NAME=` query with zero
/// matches comes back as a **404** carrying that exact plain-text body.
/// `get_text`'s `error_for_status` would turn that into an opaque "bad
/// status" error before the body was ever read, misreporting a perfectly
/// normal "no such satellite" as a network failure. The tolerance is
/// matched against Celestrak's literal known text, not just "isn't
/// JSON" — a genuine outage (a proxy error page, a 500) still surfaces as
/// a real error rather than a silently empty result.
async fn fetch_tolerating_no_match(client: &reqwest::Client, url: &str) -> Result<String> {
    let resp = crate::api::send(client, url).await?;
    let status = resp.status();
    let body = resp.text().await.with_context(|| format!("reading body from {url}"))?;
    if status.is_success() || body.trim().eq_ignore_ascii_case("No GP data found") {
        Ok(body)
    } else {
        anyhow::bail!("bad status from {url}: {status}")
    }
}

/// Just the two fields a lookup needs out of a GP JSON entry — deliberately
/// not the full `sgp4::Elements` (which `fetch_gp_json`'s caller parses
/// instead), so a lookup doesn't fail were Celestrak ever to omit an
/// orbital-element field this code has no use for anyway.
#[derive(serde::Deserialize)]
struct GpEntry {
    #[serde(rename = "OBJECT_NAME")]
    object_name: Option<String>,
    #[serde(rename = "NORAD_CAT_ID")]
    norad_id: u64,
}

/// Parse a GP JSON response into [`SatMatch`]es, treating Celestrak's
/// "nothing found" plain-text response as an empty list rather than an error.
fn parse_matches(body: &str) -> Result<Vec<SatMatch>> {
    if checked_body(body).is_err() {
        return Ok(Vec::new());
    }
    let entries: Vec<GpEntry> =
        serde_json::from_str(body).map_err(|e| anyhow::anyhow!("parsing Celestrak GP JSON: {e}"))?;
    Ok(entries
        .into_iter()
        .map(|e| SatMatch {
            norad_id: e.norad_id,
            name: e.object_name.unwrap_or_else(|| format!("NORAD {}", e.norad_id)),
        })
        .collect())
}

/// Sort `matches` so a name that equals `query` (case-insensitively) sorts
/// first, then one that starts with it, then the rest — alphabetically
/// within each group.
fn rank_by_name(matches: &mut [SatMatch], query: &str) {
    let q = query.trim().to_ascii_lowercase();
    let rank = |m: &SatMatch| {
        let name = m.name.to_ascii_lowercase();
        if name == q {
            0
        } else if name.starts_with(&q) {
            1
        } else {
            2
        }
    };
    matches.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name)));
}

/// Minimal percent-encoding for a `NAME=` query value — just the characters
/// that would otherwise break the query string (space, and reserved `&`/`=`).
/// Catalogue names are plain ASCII, so nothing fancier is needed.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(entries: &[(&str, u64)]) -> String {
        let objs: Vec<String> = entries
            .iter()
            .map(|(name, id)| format!(r#"{{"OBJECT_NAME":"{name}","NORAD_CAT_ID":{id}}}"#))
            .collect();
        format!("[{}]", objs.join(","))
    }

    #[test]
    fn parse_matches_reads_name_and_id() {
        let b = body(&[("HST", 20580)]);
        let m = parse_matches(&b).unwrap();
        assert_eq!(m, vec![SatMatch { norad_id: 20580, name: "HST".to_string() }]);
    }

    #[test]
    fn parse_matches_treats_a_no_data_response_as_empty_not_an_error() {
        let m = parse_matches("No GP data found").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn rank_by_name_puts_an_exact_match_before_a_longer_substring_match() {
        let mut m = vec![
            SatMatch { norad_id: 55098, name: "HUBBLE 6".to_string() },
            SatMatch { norad_id: 20580, name: "HST".to_string() },
        ];
        rank_by_name(&mut m, "HST");
        assert_eq!(m[0].name, "HST");
    }

    #[test]
    fn rank_by_name_is_case_insensitive() {
        let mut m = vec![
            SatMatch { norad_id: 55098, name: "HUBBLE 6".to_string() },
            SatMatch { norad_id: 20580, name: "HST".to_string() },
        ];
        rank_by_name(&mut m, "hst");
        assert_eq!(m[0].name, "HST");
    }

    #[test]
    fn rank_by_name_orders_prefix_matches_before_the_rest() {
        let mut m = vec![
            SatMatch { norad_id: 1, name: "ZARYA MODULE".to_string() },
            SatMatch { norad_id: 2, name: "ISS DEB".to_string() },
            SatMatch { norad_id: 3, name: "ISS (ZARYA)".to_string() },
        ];
        rank_by_name(&mut m, "ISS");
        assert_eq!(m[0].name, "ISS (ZARYA)");
        assert_eq!(m[1].name, "ISS DEB");
        assert_eq!(m[2].name, "ZARYA MODULE");
    }

    /// Celestrak answers a `NAME=` search that matches nothing with a 404,
    /// not the 200 the rest of this API gets — confirmed against the live
    /// endpoint. That has to come back as an empty match list, not an error,
    /// or the popup would show a scary network failure for a perfectly
    /// ordinary "no such satellite".
    #[test]
    #[ignore = "hits the live network"]
    fn search_by_name_treats_a_live_404_no_match_as_empty_not_an_error() {
        let client = crate::api::client().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let results = rt
            .block_on(search_by_name(&client, "zzzznotasatellite"))
            .expect("a 404 no-match response must not surface as an error");
        assert!(results.is_empty());
    }

    #[test]
    #[ignore = "hits the live network"]
    fn search_by_name_finds_hst_by_its_official_catalogue_name() {
        let client = crate::api::client().unwrap();
        let rt = tokio::runtime::Runtime::new().unwrap();
        let results = rt.block_on(search_by_name(&client, "HST")).expect("search should succeed");
        assert!(results.iter().any(|m| m.norad_id == 20580 && m.name == "HST"));
    }
}
