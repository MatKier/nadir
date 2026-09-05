//! HTTP data sources. One module per upstream API.
//!
//! Everything shares a single [`reqwest::Client`] with a real User-Agent and a
//! tight timeout. `RateGuard` makes it structurally impossible to exceed a
//! feed's published request budget.

pub mod celestrak;
pub mod geocode;
pub mod geoip;
pub mod launches;
pub mod swpc;

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::sync::Mutex;

/// Sent to the browser as `User-Agent`; identifies the app and a contact.
const USER_AGENT: &str = concat!(
    "nadir/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/MatKier/nadir; terminal satellite tracker)",
);

/// Build the shared HTTP client.
pub fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(8))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .context("building the HTTP client")
}

/// Send a GET request to `url`, without checking the response status. Most
/// callers want [`get`] instead; this exists for the rare one (Celestrak's
/// catalogue search) that has to inspect a non-2xx body before deciding
/// whether the response was actually a failure.
async fn send(client: &reqwest::Client, url: &str) -> Result<reqwest::Response> {
    client.get(url).send().await.with_context(|| format!("requesting {url}"))
}

/// GET `url`, returning an error-checked response.
async fn get(client: &reqwest::Client, url: &str) -> Result<reqwest::Response> {
    send(client, url).await?.error_for_status().with_context(|| format!("bad status from {url}"))
}

/// GET `url` and deserialize the JSON body into `T`.
pub async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    get(client, url)
        .await?
        .json::<T>()
        .await
        .with_context(|| format!("decoding JSON from {url}"))
}

/// GET `url` and return the raw body text.
pub async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    get(client, url)
        .await?
        .text()
        .await
        .with_context(|| format!("reading body from {url}"))
}

/// A leaky-bucket limiter. `try_take` refuses rather than sleeps, so a caller
/// can serve cache instead of queueing behind a rate limit.
#[derive(Debug)]
pub struct RateGuard {
    inner: Mutex<Bucket>,
    capacity: f64,
    refill_per_sec: f64,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateGuard {
    /// Allow `max_per_hour` requests, refilling smoothly across the hour.
    pub fn per_hour(max_per_hour: u32) -> Self {
        let capacity = max_per_hour as f64;
        Self {
            inner: Mutex::new(Bucket {
                tokens: capacity,
                last: Instant::now(),
            }),
            capacity,
            refill_per_sec: capacity / 3600.0,
        }
    }

    /// Take one token if available. Returns `false` when the budget is spent.
    pub async fn try_take(&self) -> bool {
        let mut b = self.inner.lock().await;
        let now = Instant::now();
        let elapsed = now.duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}
