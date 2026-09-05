//! `Source<T>` — a value plus how fresh and how trustworthy it is.
//!
//! Every network-backed field on the dashboard is a `Source`. The render loop
//! never blocks on a fetch; it draws whatever value is here and lets the health
//! badge tell the user how old it is.

use std::time::{Duration, Instant};

/// The freshness/trust state of a [`Source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Never fetched yet this session.
    Pending,
    /// Fetched successfully; the value is current.
    Live,
    /// A fetch failed or was skipped; the value is the last good one, this old.
    Stale(Duration),
    /// No value is available at all, with a short reason.
    Error(String),
}

/// A single piece of fetched data with provenance.
#[derive(Debug, Clone)]
pub struct Source<T> {
    value: Option<T>,
    updated_at: Option<Instant>,
    health: Health,
}

impl<T> Default for Source<T> {
    fn default() -> Self {
        Self {
            value: None,
            updated_at: None,
            health: Health::Pending,
        }
    }
}

impl<T> Source<T> {
    /// Record a fresh, successful value.
    pub fn set_live(&mut self, value: T) {
        self.value = Some(value);
        self.updated_at = Some(Instant::now());
        self.health = Health::Live;
    }

    /// Note that a refresh failed. Keep any previous value but mark it stale,
    /// or surface the error if there is nothing to fall back to.
    pub fn set_failed(&mut self, reason: impl Into<String>) {
        match self.updated_at {
            Some(t) => self.health = Health::Stale(t.elapsed()),
            None => self.health = Health::Error(reason.into()),
        }
    }

    /// Adopt a value recovered from disk cache, `age` old.
    pub fn set_from_cache(&mut self, value: T, age: Duration) {
        self.value = Some(value);
        self.updated_at = Some(Instant::now().checked_sub(age).unwrap_or_else(Instant::now));
        self.health = Health::Stale(age);
    }

    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Current health, with `Stale` ages advanced to the present moment.
    pub fn health(&self) -> Health {
        match (&self.health, self.updated_at) {
            (Health::Live, Some(t)) if t.elapsed() > Duration::from_secs(1) => {
                Health::Stale(t.elapsed())
            }
            (Health::Stale(_), Some(t)) => Health::Stale(t.elapsed()),
            (h, _) => h.clone(),
        }
    }

    /// The age of a stale value, or `None` when this source is not stale.
    pub fn stale_age(&self) -> Option<Duration> {
        match self.health() {
            Health::Stale(age) => Some(age),
            _ => None,
        }
    }

    /// A colour hint for the UI: 0 = good, 1 = aging, 2 = bad, with the `Stale`
    /// age thresholds derived from how often this source is actually refetched
    /// — green while at most two refreshes could have been missed, amber up to
    /// six. No single fixed threshold can serve every feed: 15 minutes means
    /// "two refreshes late" for the 5-minute space weather poll and "still
    /// early" for the 30-minute launch manifest, so a shared constant
    /// necessarily libels one of them. [`Source::health`] demotes `Live` to
    /// `Stale` after a second, so these thresholds — not the `Live` arm — are
    /// what actually colours a chip.
    pub fn severity_for(&self, every: Duration) -> u8 {
        self.severity_with(2 * every, 6 * every)
    }

    /// The `Stale` age thresholds for "still green" and "amber, not yet bad"
    /// given explicitly. [`Source::severity_for`] is the usual way in, deriving
    /// both from a feed's refresh interval; this is the primitive underneath.
    pub fn severity_with(&self, green_until: Duration, amber_until: Duration) -> u8 {
        match self.health() {
            Health::Live => 0,
            Health::Pending => 1,
            Health::Stale(age) if age < green_until => 0,
            Health::Stale(age) if age < amber_until => 1,
            Health::Stale(_) | Health::Error(_) => 2,
        }
    }
}

/// Render a duration as a compact age string: `4s`, `12m`, `3h`, `2d`.
pub fn fmt_age(d: Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_with_stale_below_green_until_is_good() {
        let mut src: Source<()> = Source::default();
        src.set_from_cache((), Duration::from_secs(3600));
        assert_eq!(src.severity_with(Duration::from_secs(24 * 3600), Duration::from_secs(72 * 3600)), 0);
    }

    #[test]
    fn severity_with_stale_between_thresholds_is_aging() {
        let mut src: Source<()> = Source::default();
        src.set_from_cache((), Duration::from_secs(30 * 3600));
        assert_eq!(src.severity_with(Duration::from_secs(24 * 3600), Duration::from_secs(72 * 3600)), 1);
    }

    #[test]
    fn severity_with_stale_past_amber_until_is_bad() {
        let mut src: Source<()> = Source::default();
        src.set_from_cache((), Duration::from_secs(100 * 3600));
        assert_eq!(src.severity_with(Duration::from_secs(24 * 3600), Duration::from_secs(72 * 3600)), 2);
    }

    #[test]
    fn severity_for_is_good_until_two_intervals_could_have_been_missed() {
        let every = Duration::from_secs(5 * 60);
        let mut fresh: Source<()> = Source::default();
        fresh.set_from_cache((), Duration::from_secs(9 * 60));
        assert_eq!(fresh.severity_for(every), 0);
        let mut late: Source<()> = Source::default();
        late.set_from_cache((), Duration::from_secs(11 * 60));
        assert_eq!(late.severity_for(every), 1);
    }

    #[test]
    fn severity_for_is_bad_past_six_intervals() {
        let mut src: Source<()> = Source::default();
        src.set_from_cache((), Duration::from_secs(31 * 60));
        assert_eq!(src.severity_for(Duration::from_secs(5 * 60)), 2);
    }

    /// The interval-derived rule must land on exactly the 24h/72h knees the
    /// `TLE` chip carried before this became a rule — the guarantee that the
    /// one chip already reporting feed health keeps behaving identically.
    #[test]
    fn severity_for_reproduces_the_element_sets_established_thresholds() {
        let tle_ttl = Duration::from_secs(12 * 3600);
        for &(age_h, want) in &[(1u64, 0u8), (23, 0), (25, 1), (71, 1), (73, 2)] {
            let mut src: Source<()> = Source::default();
            src.set_from_cache((), Duration::from_secs(age_h * 3600));
            assert_eq!(
                src.severity_for(tle_ttl),
                src.severity_with(Duration::from_secs(24 * 3600), Duration::from_secs(72 * 3600)),
                "age {age_h}h"
            );
            assert_eq!(src.severity_for(tle_ttl), want, "age {age_h}h");
        }
    }
}
