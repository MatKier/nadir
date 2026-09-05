//! Persistent configuration: a small TOML file that command-line flags override.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::geo::GeoPoint;

/// The ISS (ZARYA), NORAD catalogue number.
pub const DEFAULT_SAT: u64 = 25544;

/// A satellite once tracked: its catalogue number and last known name, kept
/// around so it can be picked from the TRACKED panel instead of searched for
/// again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackedSat {
    pub norad_id: u64,
    pub name: String,
}

/// User configuration, read from and written back to `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Ground-station latitude in degrees, north positive.
    pub lat: Option<f64>,
    /// Ground-station longitude in degrees, east positive.
    pub lon: Option<f64>,
    /// A human-readable name for the ground station (e.g. "Munich, Bavaria,
    /// Germany"), from `--location` or an IP geolocation guess. `None` for a
    /// hand-typed `--lat`/`--lon` station, which has no name to show.
    #[serde(default)]
    pub location_name: Option<String>,
    /// Ground-station altitude in metres above the ellipsoid.
    #[serde(default)]
    pub alt_m: f64,
    /// NORAD catalogue number of the satellite to track.
    #[serde(default = "default_sat")]
    pub sat: u64,
    /// Every satellite ever tracked, most recently (re)tracked first. Shown
    /// in the TRACKED panel so one can be picked again without searching.
    #[serde(default)]
    pub tracked: Vec<TrackedSat>,
    /// Whether nadir may use IP geolocation when no location is set.
    #[serde(default = "default_true")]
    pub allow_geoip: bool,
    /// Whether to run without any network access.
    #[serde(default)]
    pub offline: bool,

    /// Path this config was loaded from; not serialised.
    #[serde(skip)]
    pub path: PathBuf,
    /// True when the file did not exist and defaults are in use.
    #[serde(skip)]
    pub is_new: bool,
    /// A free-text place name from `--location`, awaiting geocoding at
    /// startup. Never persisted — only the coordinates it resolves to are.
    #[serde(skip)]
    pub location_query: Option<String>,
    /// A `--sat` value that wasn't a bare NORAD id, awaiting a catalogue
    /// search at startup once a network client exists. Never persisted.
    #[serde(skip)]
    pub sat_query: Option<String>,
}

fn default_sat() -> u64 {
    DEFAULT_SAT
}
fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            lat: None,
            lon: None,
            location_name: None,
            alt_m: 0.0,
            sat: DEFAULT_SAT,
            tracked: Vec::new(),
            allow_geoip: true,
            offline: false,
            path: PathBuf::new(),
            is_new: true,
            location_query: None,
            sat_query: None,
        }
    }
}

impl Config {
    /// The default config path, `~/.config/nadir/config.toml` on Linux.
    pub fn default_path() -> Result<PathBuf> {
        let dirs = ProjectDirs::from("", "", "nadir")
            .context("could not determine a config directory for this platform")?;
        Ok(dirs.config_dir().join("config.toml"))
    }

    /// Load the config from `explicit` if given, otherwise the default path.
    /// A missing file is not an error: defaults are returned with `is_new` set.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(p) => p.to_path_buf(),
            None => Self::default_path()?,
        };

        if !path.exists() {
            return Ok(Config {
                path,
                ..Config::default()
            });
        }

        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        let mut cfg: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config file {}", path.display()))?;
        cfg.path = path;
        cfg.is_new = false;
        Ok(cfg)
    }

    /// Write the current config back to its path, creating parent directories.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating config directory {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialising config")?;
        std::fs::write(&self.path, text)
            .with_context(|| format!("writing config file {}", self.path.display()))?;
        Ok(())
    }

    /// Apply command-line overrides on top of the loaded values. `location`
    /// (from `--location`) and a non-numeric `sat` are resolved later, once a
    /// network client exists; a location wins over `lat`/`lon` (clap already
    /// refuses to accept both on the command line, but a config file could
    /// still carry lat/lon from a previous run).
    pub fn apply_overrides(
        &mut self,
        lat: Option<f64>,
        lon: Option<f64>,
        alt_m: Option<f64>,
        sat: Option<String>,
        location: Option<String>,
    ) {
        if let Some(v) = lat {
            self.lat = Some(v);
            // A hand-typed coordinate replaces whatever place a previous
            // --location or geoip guess named — the two would otherwise
            // disagree the moment one axis changes but not the other.
            self.location_name = None;
        }
        if let Some(v) = lon {
            self.lon = Some(v);
            self.location_name = None;
        }
        if let Some(v) = alt_m {
            self.alt_m = v;
        }
        if let Some(v) = sat {
            match v.trim().parse::<u64>() {
                Ok(id) => self.sat = id,
                Err(_) => self.sat_query = Some(v),
            }
        }
        if location.is_some() {
            self.location_query = location;
        }
    }

    /// Record a satellite as tracked, most recently (re)tracked first. An
    /// existing entry for the same object is updated in place and moved to
    /// the front, rather than duplicated.
    pub fn track(&mut self, norad_id: u64, name: impl Into<String>) {
        let name = name.into();
        self.tracked.retain(|t| t.norad_id != norad_id);
        self.tracked.insert(0, TrackedSat { norad_id, name });
    }

    /// Remove a satellite from the tracked list. Returns `false` if it wasn't
    /// there.
    pub fn untrack(&mut self, norad_id: u64) -> bool {
        let before = self.tracked.len();
        self.tracked.retain(|t| t.norad_id != norad_id);
        self.tracked.len() != before
    }

    /// The name last recorded for `norad_id`, if it's been tracked.
    pub fn tracked_name(&self, norad_id: u64) -> Option<&str> {
        self.tracked.iter().find(|t| t.norad_id == norad_id).map(|t| t.name.as_str())
    }

    /// The configured ground station, if a latitude and longitude are known.
    pub fn ground_station(&self) -> Option<GeoPoint> {
        match (self.lat, self.lon) {
            (Some(lat), Some(lon)) => Some(GeoPoint::new(lat, lon, self.alt_m / 1000.0)),
            _ => None,
        }
    }

    /// Record a location discovered by geolocation (or resolved from
    /// `--location`) and persist it, along with its human-readable name.
    pub fn set_location(&mut self, lat: f64, lon: f64, name: Option<String>) {
        self.lat = Some(lat);
        self.lon = Some(lon);
        self.location_name = name;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_adds_a_new_entry_at_the_front() {
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        c.track(20580, "HST");
        assert_eq!(c.tracked[0].norad_id, 20580);
        assert_eq!(c.tracked[1].norad_id, 25544);
    }

    #[test]
    fn track_moves_an_existing_entry_to_the_front_and_updates_its_name() {
        let mut c = Config::default();
        c.track(25544, "NORAD 25544");
        c.track(20580, "HST");
        c.track(25544, "ISS (ZARYA)");
        assert_eq!(c.tracked.len(), 2, "re-tracking must not duplicate the entry");
        assert_eq!(c.tracked[0], TrackedSat { norad_id: 25544, name: "ISS (ZARYA)".to_string() });
    }

    #[test]
    fn untrack_removes_an_entry_and_reports_whether_it_was_there() {
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        assert!(c.untrack(25544));
        assert!(c.tracked.is_empty());
        assert!(!c.untrack(25544));
    }

    #[test]
    fn tracked_name_looks_up_by_norad_id() {
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        assert_eq!(c.tracked_name(25544), Some("ISS (ZARYA)"));
        assert_eq!(c.tracked_name(1), None);
    }

    #[test]
    fn tracked_list_round_trips_through_toml() {
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        c.track(20580, "HST");
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.tracked, c.tracked);
    }

    #[test]
    fn a_config_file_without_a_tracked_list_still_loads() {
        // The format written before this field existed.
        let c: Config = toml::from_str("sat = 25544\n").unwrap();
        assert!(c.tracked.is_empty());
    }
}
