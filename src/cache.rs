//! A tiny on-disk cache under `~/.cache/nadir`.
//!
//! Each data source stores its last good raw payload under a stable key. On a
//! failed fetch (or in `--offline` mode) nadir falls back to whatever is here,
//! so the map and pass predictions keep working with no network. One entry
//! class is different: `transmitters-<norad>` (see
//! `config::Config::downlinks`) is the sole copy of a one-shot SatNOGS lookup,
//! possibly hand-corrected — nothing refetches it on a schedule, so losing it
//! costs a live query, not just a stale read.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use directories::ProjectDirs;

/// Handle to the cache directory. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// Open (and create) the cache directory for this platform.
    pub fn open() -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "nadir")
            .context("could not determine a cache directory for this platform")?;
        let dir = dirs.cache_dir().to_path_buf();
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating cache directory {}", dir.display()))?;
        Ok(Self { dir })
    }

    fn path_for(&self, key: &str) -> PathBuf {
        // Keys are short, code-defined slugs; keep only sane characters.
        let safe: String = key
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.dir.join(format!("{safe}.json"))
    }

    /// Store a raw payload under `key`, overwriting any previous value.
    pub fn put(&self, key: &str, bytes: &[u8]) -> Result<()> {
        let path = self.path_for(key);
        std::fs::write(&path, bytes)
            .with_context(|| format!("writing cache entry {}", path.display()))
    }

    /// Read the payload stored under `key`, if any.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        std::fs::read(self.path_for(key)).ok()
    }

    /// How long ago `key` was last written, if it exists.
    pub fn age(&self, key: &str) -> Option<Duration> {
        let meta = std::fs::metadata(self.path_for(key)).ok()?;
        let modified = meta.modified().ok()?;
        SystemTime::now().duration_since(modified).ok()
    }

    /// A cache rooted at an arbitrary directory, for tests that need to drive
    /// the real put/get path without touching the real `~/.cache/nadir`.
    /// `open()` remains the only production entry point.
    #[cfg(test)]
    pub(crate) fn in_dir(dir: PathBuf) -> Self {
        Self { dir }
    }
}
