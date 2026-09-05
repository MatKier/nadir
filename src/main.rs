//! nadir — a mission-control console for the terminal.
//!
//! Tracks a satellite live (locally, via SGP4), predicts passes over your ground
//! station, and pulls space weather and launch data from public APIs.

use anyhow::{Context, Result};
use clap::Parser;
use nadir::config::Config;

/// Command-line options. Anything given here overrides the on-disk config.
#[derive(Debug, Parser)]
#[command(name = "nadir", version, about = "Track a satellite overhead in your terminal")]
struct Cli {
    /// Ground-station latitude in degrees (north positive).
    #[arg(long, allow_hyphen_values = true, conflicts_with = "location")]
    lat: Option<f64>,

    /// Ground-station longitude in degrees (east positive).
    #[arg(long, allow_hyphen_values = true, conflicts_with = "location")]
    lon: Option<f64>,

    /// Ground-station altitude in metres above the ellipsoid.
    #[arg(long, allow_hyphen_values = true)]
    alt: Option<f64>,

    /// A place name to use as the ground station instead of --lat/--lon, e.g.
    /// "Munich" or "Springfield, Illinois". Resolved once at startup.
    #[arg(long)]
    location: Option<String>,

    /// Satellite to track: a NORAD catalogue number, or a name to search
    /// Celestrak's catalogue for (e.g. "ISS", "Hubble") — the top-ranked
    /// match is used; run the in-app search (`s`) to pick from several.
    /// Default: the ISS.
    #[arg(long)]
    sat: Option<String>,

    /// Use a specific config file instead of the default location
    /// (~/.config/nadir/config.toml on Linux).
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Run entirely from cache; make no network requests.
    #[arg(long)]
    offline: bool,

    /// Never attempt IP geolocation, even when no location is configured.
    #[arg(long)]
    no_geoip: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut config = Config::load(cli.config.as_deref()).context("loading configuration")?;
    config.apply_overrides(cli.lat, cli.lon, cli.alt, cli.sat, cli.location);
    config.offline = config.offline || cli.offline;
    config.allow_geoip = config.allow_geoip && !cli.no_geoip;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;

    runtime.block_on(nadir::app::run(config))
}
