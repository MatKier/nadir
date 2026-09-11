//! Persistent configuration: a small TOML file that command-line flags override.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::cache::Cache;
use crate::geo::GeoPoint;

/// The ISS (ZARYA), NORAD catalogue number.
pub const DEFAULT_SAT: u64 = 25544;

/// How often each timer-driven feed refetches, as written under `[intervals]`
/// in `config.toml`. Every field is a lower bound on how much upstream traffic
/// nadir generates, so the shipped defaults double as the floors enforced by
/// [`Intervals::clamped`] — a config file can lengthen an interval freely but
/// only shorten it so far.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Intervals {
    /// How long a cached element set is trusted before it's worth refetching.
    /// Celestrak re-issues a given object's GP data only a few times a day and
    /// asks clients not to poll faster than roughly an orbital period; the
    /// 1-hour floor honours that and also keeps the TTL clear of the
    /// `TLE_RETRY_CAP` (30m) failure backoff, so a retry streak still resolves
    /// well before a healthy refresh would have come round.
    #[serde(default = "default_tle_interval", with = "interval_str")]
    pub tle: Duration,
    /// Space-weather poll. NOAA SWPC's K-index and solar-wind products refresh
    /// about once a minute, which is the floor.
    #[serde(default = "default_weather_interval", with = "interval_str")]
    pub weather: Duration,
    /// Aurora-nowcast poll. SWPC regenerates the OVATION grid every ~5 minutes,
    /// so a shorter interval only refetches the same picture.
    #[serde(default = "default_aurora_interval", with = "interval_str")]
    pub aurora: Duration,
    /// Launch-manifest poll. `api::launches::rate_guard` budgets 8 requests an
    /// hour against Launch Library's anonymous tier; the 10-minute floor is
    /// 6/hour, leaving room for a manual `r` refresh or two without spending
    /// the budget.
    #[serde(default = "default_launches_interval", with = "interval_str")]
    pub launches: Duration,
}

fn default_tle_interval() -> Duration {
    Duration::from_secs(12 * 3600)
}
fn default_weather_interval() -> Duration {
    Duration::from_secs(5 * 60)
}
fn default_aurora_interval() -> Duration {
    Duration::from_secs(15 * 60)
}
fn default_launches_interval() -> Duration {
    Duration::from_secs(30 * 60)
}

impl Default for Intervals {
    fn default() -> Self {
        Self {
            tle: default_tle_interval(),
            weather: default_weather_interval(),
            aurora: default_aurora_interval(),
            launches: default_launches_interval(),
        }
    }
}

impl Intervals {
    /// The smallest each interval may be set to, and why. Kept next to the
    /// defaults on purpose: a default that ever dropped below its own floor
    /// would make every stock config log an adjustment on startup.
    const TLE_FLOOR: Duration = Duration::from_secs(3600);
    const WEATHER_FLOOR: Duration = Duration::from_secs(60);
    const AURORA_FLOOR: Duration = Duration::from_secs(5 * 60);
    const LAUNCHES_FLOOR: Duration = Duration::from_secs(10 * 60);

    /// The effective intervals — each value raised to its floor if the config
    /// set it shorter — plus a human-readable line for every value that was
    /// raised, to be surfaced in the activity log. Network etiquette is not
    /// negotiable from a config file; a too-eager value is honoured up to the
    /// floor and no further.
    pub fn clamped(self) -> (Self, Vec<String>) {
        let mut notes = Vec::new();
        let mut clamp = |value: Duration, floor: Duration, feed: &str, why: &str| {
            if value < floor {
                notes.push(format!(
                    "{feed} interval {} raised to the {} minimum ({why})",
                    fmt_interval(value),
                    fmt_interval(floor),
                ));
                floor
            } else {
                value
            }
        };
        let clamped = Self {
            tle: clamp(self.tle, Self::TLE_FLOOR, "TLE", "Celestrak polling etiquette"),
            weather: clamp(self.weather, Self::WEATHER_FLOOR, "space weather", "upstream update rate"),
            aurora: clamp(self.aurora, Self::AURORA_FLOOR, "aurora", "upstream update rate"),
            launches: clamp(
                self.launches,
                Self::LAUNCHES_FLOOR,
                "launches",
                "Launch Library rate budget",
            ),
        };
        (clamped, notes)
    }
}

/// Parse a refresh-interval string: a whole number then a unit — `s`, `m`, `h`
/// or `d`, with the long forms (`sec`, `min`, `hr`, `day`, and their plurals)
/// also accepted. No sign, and a bare number is refused: an interval is always
/// positive and its unit has to be explicit.
///
/// This shares its unit vocabulary with the clock-scrub parser
/// (`simclock::parse_relative`) so `[intervals]` and the in-app `t` prompt read
/// alike — but deliberately not its implementation: that one is signed, yields
/// a `chrono::Duration`, and carries a 100-year cap and clock-scrub error prose
/// that have no place here.
fn parse_interval(s: &str) -> Result<Duration> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (digits, unit) = s.split_at(split);
    if digits.is_empty() {
        bail!("a refresh interval starts with a number — e.g. \"12h\", \"5m\"");
    }
    let unit = unit.trim();
    if unit.is_empty() {
        bail!("a refresh interval needs a unit — e.g. \"12h\", \"5m\"");
    }
    let secs_per: u64 = match unit {
        "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" => 3600,
        "d" | "day" | "days" => 86_400,
        other => bail!("unknown unit \"{other}\" — use s, m, h or d"),
    };
    let n: u64 = digits.parse().context("that interval is not a whole number")?;
    if n == 0 {
        bail!("a refresh interval must be greater than zero");
    }
    n.checked_mul(secs_per).map(Duration::from_secs).context("that interval is too large")
}

/// Render an interval back to the shortest exact `s` / `m` / `h` string
/// [`parse_interval`] would read the same. Deliberately stops at hours: a 12h
/// TLE interval's amber threshold is 72h, which should print as `72h`, not
/// `3d`. Distinct from `source::fmt_age`, which truncates to a leading unit and
/// so cannot round-trip.
// clippy nudges toward `u64::is_multiple_of` here, but that is a 1.87 API and
// this crate's newest std touch is deliberately 1.82 (`Option::is_none_or`);
// plain modulo keeps the headroom under the 1.88 MSRV floor.
#[allow(clippy::manual_is_multiple_of)]
pub fn fmt_interval(d: Duration) -> String {
    let s = d.as_secs();
    if s % 3600 == 0 {
        format!("{}h", s / 3600)
    } else if s % 60 == 0 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// `#[serde(with = …)]` adapter: intervals live in the struct as `Duration` but
/// on disk as the `"12h"` strings [`parse_interval`] and [`fmt_interval`]
/// handle.
mod interval_str {
    use super::{fmt_interval, parse_interval};
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S: Serializer>(d: &Duration, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&fmt_interval(*d))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Duration, D::Error> {
        let s = String::deserialize(de)?;
        parse_interval(&s).map_err(serde::de::Error::custom)
    }
}

/// One downlink a satellite is known to transmit on, seeded from a one-shot
/// SatNOGS DB lookup when the satellite is first tracked. See
/// [`Config::downlinks`] for where and why this is stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transmitter {
    /// Downlink centre frequency in hertz. Integral because that is how SatNOGS
    /// publishes it and because a float would round-trip through TOML with an
    /// exponent nobody wants to hand-edit.
    pub downlink_hz: u64,
    /// Modulation as SatNOGS names it ("FM", "BPSK", "AFSK", …). May be empty
    /// when the record does not carry one.
    #[serde(default)]
    pub mode: String,
    /// SatNOGS's own human label, e.g. "Mode V/V FM (crew R2+3)". May be empty.
    #[serde(default)]
    pub description: String,
}

/// A satellite once tracked: its catalogue number and last known name, kept
/// around so it can be picked from the TRACKED panel instead of searched for
/// again — plus which of its cached downlinks (if any) drives the DOPP row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackedSat {
    pub norad_id: u64,
    pub name: String,
    /// Index into `Config::downlinks[norad_id]` of the one currently driving
    /// the DOPP row — the user's choice, so unlike the list itself it lives
    /// here in config.toml. Clamped on read, so a hand-edited value past the
    /// end of the list (or a list since shortened by a re-lookup, or emptied
    /// by a cache wipe) is harmless.
    #[serde(default)]
    pub active_transmitter: usize,
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
    /// How often each timer-driven feed refetches. Missing entirely, or missing
    /// individual keys, on an older config file — each field falls back to its
    /// shipped default.
    #[serde(default)]
    pub intervals: Intervals,

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
    /// Downlink lists, keyed by NORAD id — the in-memory half of the
    /// `transmitters-<norad>.json` cache entries `load_downlinks` hydrates
    /// this from and `set_transmitters` writes through to. Kept separate from
    /// `TrackedSat` (rather than nested on it, as it was before this existed)
    /// because `TrackedSat` is exactly what serialises into config.toml:
    /// fetched data has to live off of it to stay out of the file.
    /// `TrackedSat::active_transmitter` — the index into a satellite's list
    /// here — is the user's choice, not fetched data, so it stays on
    /// `TrackedSat` and in the file. A cache wipe or miss just costs one more
    /// SatNOGS lookup on the next switch — never an error, never a blank
    /// config.
    #[serde(skip)]
    downlinks: HashMap<u64, Vec<Transmitter>>,
    /// Handle to the disk cache backing `downlinks`, attached by
    /// `load_downlinks` rather than opened here. `None` in `Config::default()`
    /// so unit tests never touch the real cache directory — the same
    /// convention that keeps an empty `path` out of the real config file.
    #[serde(skip)]
    cache: Option<Cache>,
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
            intervals: Intervals::default(),
            path: PathBuf::new(),
            is_new: true,
            location_query: None,
            sat_query: None,
            downlinks: HashMap::new(),
            cache: None,
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

    /// Attach the disk cache and hydrate `downlinks` from it for every
    /// currently tracked satellite. Called once by `app::run`, right after it
    /// opens the cache it owns for the whole session — not from `load`, so
    /// there is exactly one `Cache::open()` per run and one failure policy for
    /// it, rather than `load` silently swallowing a second, independent open.
    ///
    /// Eager rather than lazy: a missing entry is exactly what tells
    /// `switch_satellite` to fire a SatNOGS lookup, so the map has to already
    /// reflect the cache by the time the first satellite switch is
    /// considered, not fill in on demand. A satellite with no cache entry
    /// (never looked up, or the cache was wiped) is silently left absent —
    /// this is cache, not config, so a miss here is never an error.
    pub(crate) fn load_downlinks(&mut self, cache: &Cache) {
        self.cache = Some(cache.clone());
        for t in &self.tracked {
            let Some(bytes) = cache.get(&transmitters_cache_key(t.norad_id)) else { continue };
            let Ok(list) = serde_json::from_slice::<Vec<Transmitter>>(&bytes) else { continue };
            self.downlinks.insert(t.norad_id, list);
        }
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
    ///
    /// The existing entry is *lifted out and put back*, not rebuilt: everything
    /// it carries beyond the name — which transmitter is active — is
    /// per-satellite state a re-track must preserve. `App::sync_tracked_name`
    /// calls this from the render loop the moment an element set supplies the
    /// real catalogue name, so a fresh `TrackedSat { norad_id, name, .. }`
    /// literal here would silently wipe a hand-picked transmitter one frame
    /// after it landed. `downlinks` needs no such care — it's keyed by
    /// `norad_id`, not nested in the entry, so a re-track can't touch it.
    pub fn track(&mut self, norad_id: u64, name: impl Into<String>) {
        let name = name.into();
        let mut entry = match self.tracked.iter().position(|t| t.norad_id == norad_id) {
            Some(i) => self.tracked.remove(i),
            None => TrackedSat { norad_id, name: String::new(), active_transmitter: 0 },
        };
        entry.name = name;
        self.tracked.insert(0, entry);
    }

    /// The downlink frequencies on file for `norad_id`; empty if none have
    /// been looked up or cached, tracked or not.
    pub fn transmitters(&self, norad_id: u64) -> &[Transmitter] {
        self.downlinks.get(&norad_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The transmitter currently selected to drive the DOPP row, or `None` when
    /// none are on file. A stored index past the end of the list clamps to the
    /// last entry rather than blanking the row or panicking.
    pub fn active_transmitter(&self, norad_id: u64) -> Option<&Transmitter> {
        let t = self.tracked.iter().find(|t| t.norad_id == norad_id)?;
        let list = self.transmitters(norad_id);
        let i = t.active_transmitter.min(list.len().checked_sub(1)?);
        list.get(i)
    }

    /// Replace the transmitter list for `norad_id`, set which one is active
    /// (clamped to the new list), and write the list through to the disk
    /// cache. A no-op on the `active_transmitter` index if the satellite is
    /// not tracked — it always is when this is reached, since a lookup only
    /// runs for a tracked satellite — but the cache write and in-memory list
    /// happen regardless, since `downlinks` isn't keyed by tracked-ness.
    pub fn set_transmitters(&mut self, norad_id: u64, list: Vec<Transmitter>, active: usize) {
        if let Some(t) = self.tracked.iter_mut().find(|t| t.norad_id == norad_id) {
            t.active_transmitter = active.min(list.len().saturating_sub(1));
        }
        if let (Some(cache), Ok(bytes)) = (&self.cache, serde_json::to_vec(&list)) {
            let _ = cache.put(&transmitters_cache_key(norad_id), &bytes);
        }
        self.downlinks.insert(norad_id, list);
    }

    /// Advance the active transmitter to the next in the list, wrapping. A
    /// no-op with fewer than two on file.
    pub fn cycle_transmitter(&mut self, norad_id: u64) {
        let len = self.transmitters(norad_id).len();
        if len < 2 {
            return;
        }
        if let Some(t) = self.tracked.iter_mut().find(|t| t.norad_id == norad_id) {
            t.active_transmitter = (t.active_transmitter + 1) % len;
        }
    }

    /// Remove a satellite from the tracked list. Returns `false` if it wasn't
    /// there. `downlinks` drops its entry too — matching the pre-cache
    /// behaviour where untracking discarded the transmitter list outright —
    /// but the cache *file* is left on disk, exactly as a TLE's
    /// `tle-<norad>.json` outlives its satellite being untracked; nothing
    /// deletes it. `track` doesn't consult the cache, so re-tracking the same
    /// object this session starts with an empty list again (`looked_up`
    /// already marks it queried, so no automatic re-lookup fires either); the
    /// orphaned file only comes back into play if the satellite is tracked
    /// again by the time of a later `Config::load`, which does hydrate from
    /// whatever is still on disk.
    pub fn untrack(&mut self, norad_id: u64) -> bool {
        self.downlinks.remove(&norad_id);
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

/// The disk-cache key a satellite's downlink list is stored under, mirroring
/// the `tle-{norad}` scheme `Feed::tle` already uses for element sets.
fn transmitters_cache_key(norad_id: u64) -> String {
    format!("transmitters-{norad_id}")
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
        assert_eq!(c.tracked[0].norad_id, 25544);
        assert_eq!(c.tracked[0].name, "ISS (ZARYA)");
    }

    fn tx(hz: u64, mode: &str) -> Transmitter {
        Transmitter { downlink_hz: hz, mode: mode.to_string(), description: String::new() }
    }

    #[test]
    fn track_preserves_the_active_transmitter_index() {
        // The regression guard for the render-loop clobber: `sync_tracked_name`
        // calls `track()` every time the catalogue name changes, and it must
        // not throw away a hand-picked transmitter. `downlinks` needs no such
        // guard — it's keyed by NORAD id, not nested in the tracked entry, so
        // `track()` structurally cannot touch it.
        let mut c = Config::default();
        c.track(25544, "NORAD 25544");
        c.set_transmitters(25544, vec![tx(145_800_000, "FM"), tx(437_800_000, "FSK")], 1);
        c.track(25544, "ISS (ZARYA)");
        assert_eq!(c.active_transmitter(25544).map(|t| t.downlink_hz), Some(437_800_000));
        assert_eq!(c.tracked[0].name, "ISS (ZARYA)");
    }

    /// A cache rooted at a fresh temp directory, for tests that need to drive
    /// the real `Cache::put`/`get` path rather than the codec alone.
    /// `Config::default()`'s `cache: None` keeps every other test hermetic;
    /// this is the one place a test deliberately opts back into real (if
    /// scratch) disk I/O.
    fn temp_cache() -> (Cache, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("nadir-config-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        (Cache::in_dir(dir.clone()), dir)
    }

    #[test]
    fn set_transmitters_writes_through_and_a_fresh_load_reads_it_back() {
        let (cache, dir) = temp_cache();
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        c.load_downlinks(&cache); // attaches the cache so set_transmitters can write
        c.set_transmitters(25544, vec![tx(145_800_000, "FM"), tx(437_800_000, "FSK")], 1);

        // A fresh Config, sharing nothing but the tracked list and cache dir,
        // is what a real restart's `load_downlinks` sees.
        let mut reloaded = Config { tracked: c.tracked.clone(), ..Config::default() };
        reloaded.load_downlinks(&cache);
        assert_eq!(reloaded.transmitters(25544), c.transmitters(25544));
        assert_eq!(reloaded.active_transmitter(25544).map(|t| t.downlink_hz), Some(437_800_000));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_tracked_entry_without_a_transmitter_list_still_loads() {
        // The format written before this field existed.
        let c: Config =
            toml::from_str("[[tracked]]\nnorad_id = 25544\nname = \"ISS (ZARYA)\"\n").unwrap();
        assert_eq!(c.tracked[0].active_transmitter, 0);
    }

    #[test]
    fn a_config_file_with_legacy_nested_transmitters_still_loads() {
        // The format this feature originally shipped with, before the list
        // moved to the cache — a real file on disk right now looks like this.
        // Serde's default of ignoring unknown fields is what makes this a
        // silent no-op rather than a load failure; this test pins that so a
        // future `deny_unknown_fields` can't turn an old config into one.
        let c: Config = toml::from_str(
            "[[tracked]]\nnorad_id = 25544\nname = \"ISS (ZARYA)\"\nactive_transmitter = 0\n\
             \n[[tracked.transmitters]]\ndownlink_hz = 145800000\nmode = \"FM\"\n\
             description = \"Mode V/V FM\"\n",
        )
        .unwrap();
        assert_eq!(c.tracked[0].norad_id, 25544);
    }

    #[test]
    fn an_out_of_range_active_index_clamps_to_the_last_transmitter() {
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        c.set_transmitters(25544, vec![tx(145_800_000, "FM"), tx(437_800_000, "FSK")], 0);
        // Simulate a hand edit that points past the end.
        c.tracked[0].active_transmitter = 9;
        assert_eq!(c.active_transmitter(25544).map(|t| t.downlink_hz), Some(437_800_000));
    }

    #[test]
    fn active_transmitter_is_none_when_none_are_on_file() {
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        assert!(c.active_transmitter(25544).is_none());
    }

    #[test]
    fn cycle_transmitter_wraps_and_refuses_a_list_of_one() {
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        c.set_transmitters(25544, vec![tx(1, "A")], 0);
        c.cycle_transmitter(25544);
        assert_eq!(c.tracked[0].active_transmitter, 0, "one entry: nothing to cycle to");

        c.set_transmitters(25544, vec![tx(1, "A"), tx(2, "B"), tx(3, "C")], 0);
        c.cycle_transmitter(25544);
        assert_eq!(c.tracked[0].active_transmitter, 1);
        c.cycle_transmitter(25544);
        c.cycle_transmitter(25544);
        assert_eq!(c.tracked[0].active_transmitter, 0, "wraps back to the start");
    }

    #[test]
    fn untrack_drops_the_transmitters_with_the_entry() {
        let mut c = Config::default();
        c.track(25544, "ISS (ZARYA)");
        c.set_transmitters(25544, vec![tx(145_800_000, "FM")], 0);
        assert!(c.untrack(25544));
        assert!(c.transmitters(25544).is_empty(), "no orphaned transmitter rows");
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
        // The `[intervals]` table and the `[[tracked]]` array of tables share
        // one serialised document; this is also the guard against a field
        // ordering that would emit a scalar after a table.
        assert_eq!(back.intervals, c.intervals);
        // The regression guard for the bloat this whole split fixes: a
        // downlink list must never again end up nested in config.toml.
        assert!(!text.contains("transmitters"), "downlinks belong in the cache, not here: {text}");
    }

    #[test]
    fn a_config_file_without_a_tracked_list_still_loads() {
        // The format written before this field existed.
        let c: Config = toml::from_str("sat = 25544\n").unwrap();
        assert!(c.tracked.is_empty());
    }

    #[test]
    fn parse_interval_accepts_the_same_units_as_the_clock_prompt() {
        assert_eq!(parse_interval("12h").unwrap(), Duration::from_secs(12 * 3600));
        assert_eq!(parse_interval("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_interval("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_interval("2d").unwrap(), Duration::from_secs(2 * 86_400));
        // Long forms and surrounding whitespace, as `simclock::parse_relative`
        // also tolerates.
        assert_eq!(parse_interval(" 90 mins ").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse_interval("1 hr").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn parse_interval_rejects_a_sign_a_bare_number_and_zero() {
        assert!(parse_interval("-5m").is_err());
        assert!(parse_interval("12").is_err());
        assert!(parse_interval("0h").is_err());
        assert!(parse_interval("h").is_err());
        assert!(parse_interval("banana").is_err());
        assert!(parse_interval("5y").is_err());
    }

    #[test]
    fn fmt_interval_picks_the_largest_unit_that_divides_evenly() {
        assert_eq!(fmt_interval(Duration::from_secs(12 * 3600)), "12h");
        assert_eq!(fmt_interval(Duration::from_secs(90 * 60)), "90m");
        assert_eq!(fmt_interval(Duration::from_secs(45)), "45s");
        // Deliberately does not promote hours to days, so a 72h amber knee
        // reads as `72h` in the `?` overlay rather than `3d`.
        assert_eq!(fmt_interval(Duration::from_secs(72 * 3600)), "72h");
    }

    #[test]
    fn an_interval_round_trips_through_parse_and_fmt() {
        for s in ["1h", "12h", "5m", "90m", "30s", "10m"] {
            assert_eq!(fmt_interval(parse_interval(s).unwrap()), s);
        }
    }

    #[test]
    fn an_intervals_table_round_trips_through_toml() {
        let mut c = Config::default();
        c.intervals = Intervals {
            tle: Duration::from_secs(6 * 3600),
            weather: Duration::from_secs(120),
            aurora: Duration::from_secs(20 * 60),
            launches: Duration::from_secs(45 * 60),
        };
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.intervals, c.intervals);
    }

    #[test]
    fn a_config_file_without_an_intervals_table_gets_the_defaults() {
        let c: Config = toml::from_str("sat = 25544\n").unwrap();
        assert_eq!(c.intervals, Intervals::default());
    }

    #[test]
    fn a_partial_intervals_table_fills_the_rest_from_defaults() {
        let c: Config = toml::from_str("[intervals]\nweather = \"2m\"\n").unwrap();
        assert_eq!(c.intervals.weather, Duration::from_secs(120));
        assert_eq!(c.intervals.tle, Intervals::default().tle);
        assert_eq!(c.intervals.launches, Intervals::default().launches);
    }

    #[test]
    fn an_unparseable_interval_fails_the_load_rather_than_silently_defaulting() {
        let bad = toml::from_str::<Config>("[intervals]\ntle = \"banana\"\n").unwrap_err();
        assert!(bad.to_string().contains("refresh interval"), "surfaces the reason: {bad}");
        let bad_unit = toml::from_str::<Config>("[intervals]\ntle = \"5y\"\n").unwrap_err();
        assert!(bad_unit.to_string().contains("unknown unit"), "surfaces the reason: {bad_unit}");
    }

    #[test]
    fn clamped_raises_a_too_short_interval_to_its_floor_and_reports_it() {
        let eager = Intervals {
            tle: Duration::from_secs(60),
            weather: Duration::from_secs(1),
            aurora: Duration::from_secs(60),
            launches: Duration::from_secs(60),
        };
        let (out, notes) = eager.clamped();
        assert_eq!(out.tle, Intervals::TLE_FLOOR);
        assert_eq!(out.weather, Intervals::WEATHER_FLOOR);
        assert_eq!(out.aurora, Intervals::AURORA_FLOOR);
        assert_eq!(out.launches, Intervals::LAUNCHES_FLOOR);
        assert_eq!(notes.len(), 4, "one line per value raised");
        assert!(notes.iter().any(|n| n.contains("launches") && n.contains("rate budget")));
    }

    #[test]
    fn clamped_leaves_the_shipped_defaults_untouched() {
        let (out, notes) = Intervals::default().clamped();
        assert_eq!(out, Intervals::default());
        assert!(notes.is_empty(), "a stock config must never log an adjustment");
    }

    #[test]
    fn clamped_keeps_a_longer_than_default_interval_as_set() {
        let relaxed = Intervals { tle: Duration::from_secs(48 * 3600), ..Intervals::default() };
        let (out, notes) = relaxed.clamped();
        assert_eq!(out.tle, Duration::from_secs(48 * 3600));
        assert!(notes.is_empty());
    }
}
