//! Cached, rate-limited HTTP GET shared by every enrichment client.
//!
//! This is the single choke point that makes two guarantees hold:
//!  * no upstream is ever called faster than its published limit, and
//!  * a second run over the same playlist performs zero network I/O.

use super::ratelimit::{Host, RateLimiters};
use crate::config::{CacheSettings, USER_AGENT};
use crate::error::{CoreError, Result};
use crate::model::Source;
use crate::store::Store;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Counts cache hits vs. network calls, so the "second run does no network"
/// property is observable rather than assumed.
#[derive(Debug, Default)]
pub struct FetchCounters {
    pub cache_hits: AtomicU64,
    pub network_calls: AtomicU64,
    pub errors: AtomicU64,
}

impl FetchCounters {
    pub fn snapshot(&self) -> (u64, u64, u64) {
        (
            self.cache_hits.load(Ordering::Relaxed),
            self.network_calls.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
        )
    }
}

#[derive(Clone)]
pub struct Fetcher {
    http: reqwest::Client,
    store: Store,
    limiters: RateLimiters,
    cache: CacheSettings,
    pub counters: Arc<FetchCounters>,
}

impl Fetcher {
    pub fn new(
        http: reqwest::Client,
        store: Store,
        limiters: RateLimiters,
        cache: CacheSettings,
    ) -> Self {
        Fetcher {
            http,
            store,
            limiters,
            cache,
            counters: Arc::new(FetchCounters::default()),
        }
    }

    /// GET `url`, serving from `api_cache` when a fresh entry exists.
    ///
    /// `host` selects the rate limiter and `source` selects the cache TTL. The
    /// limiter is only entered on a cache miss — waiting a second to serve a
    /// cached row would make re-runs pointlessly slow.
    pub async fn get(&self, host: Host, source: Source, url: &str) -> Result<String> {
        if let Some(body) = self.store.cache_get(url)? {
            self.counters.cache_hits.fetch_add(1, Ordering::Relaxed);
            tracing::trace!(url, "cache hit");
            return Ok(body);
        }

        self.limiters.acquire(host).await;
        self.counters.network_calls.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(url, host = host.as_str(), "fetching");

        let body = self.get_uncached(host, url).await.inspect_err(|_| {
            self.counters.errors.fetch_add(1, Ordering::Relaxed);
        })?;

        self.store.cache_put(
            url,
            source.as_str(),
            &body,
            200,
            self.cache.ttl_secs(source),
        )?;
        Ok(body)
    }

    /// GET with retry, bypassing the cache. Retries on transport failures, 429
    /// and 5xx; anything else is returned as an error.
    async fn get_uncached(&self, host: Host, url: &str) -> Result<String> {
        const MAX_RETRIES: u32 = 3;
        let mut attempt = 0;

        loop {
            let resp = self
                .http
                .get(url)
                // MusicBrainz rejects requests without an identifying agent.
                .header(reqwest::header::USER_AGENT, USER_AGENT)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) if attempt < MAX_RETRIES && (e.is_timeout() || e.is_connect()) => {
                    attempt += 1;
                    let delay = Duration::from_secs(1u64 << attempt);
                    tracing::warn!(attempt, ?delay, error = %e, "transport error; retrying");
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            let status = resp.status();
            if status.is_success() {
                return Ok(resp.text().await?);
            }

            // A 404 is a definitive answer ("no such ISRC"), not a failure to
            // retry. Callers treat it as "not found" and move to the next
            // strategy in the cascade.
            if status.as_u16() == 404 {
                return Err(CoreError::upstream(host.as_str(), "not found"));
            }

            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());

            if (status.as_u16() == 429 || status.is_server_error()) && attempt < MAX_RETRIES {
                attempt += 1;
                // Back off hard on 429: we have already exceeded what this host
                // tolerates, so being timid is cheaper than being blocked.
                let delay = Duration::from_secs(retry_after.unwrap_or(1u64 << attempt).min(60));
                tracing::warn!(
                    attempt, status = status.as_u16(), ?delay,
                    host = host.as_str(), "throttled or server error; backing off"
                );
                tokio::time::sleep(delay).await;
                continue;
            }

            let body = resp.text().await.unwrap_or_default();
            return Err(CoreError::upstream(
                host.as_str(),
                format!("HTTP {}: {}", status.as_u16(), body.chars().take(200).collect::<String>()),
            ));
        }
    }

    /// True when `url` is already cached and fresh.
    pub fn is_cached(&self, url: &str) -> bool {
        self.store.cache_get(url).ok().flatten().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetcher(store: Store) -> Fetcher {
        Fetcher::new(
            reqwest::Client::new(),
            store,
            RateLimiters::new(),
            CacheSettings::default(),
        )
    }

    #[tokio::test]
    async fn serves_from_cache_without_touching_the_network() {
        let store = Store::open_in_memory().unwrap();
        // Pre-seed the cache the way a first run would have.
        store
            .cache_put(
                "https://musicbrainz.org/ws/2/isrc/BRRCA7200015",
                "musicbrainz",
                r#"{"recordings":[]}"#,
                200,
                86_400,
            )
            .unwrap();

        let f = fetcher(store);
        let body = f
            .get(
                Host::MusicBrainz,
                Source::MusicBrainz,
                "https://musicbrainz.org/ws/2/isrc/BRRCA7200015",
            )
            .await
            .unwrap();

        assert_eq!(body, r#"{"recordings":[]}"#);
        let (hits, network, _) = f.counters.snapshot();
        assert_eq!(hits, 1);
        assert_eq!(network, 0, "a cached URL must not hit the network");
    }

    #[tokio::test]
    async fn cache_hit_does_not_pay_the_rate_limit() {
        // Ten cached MusicBrainz reads must not take ten seconds.
        let store = Store::open_in_memory().unwrap();
        for i in 0..10 {
            store
                .cache_put(&format!("https://mb/{i}"), "musicbrainz", "{}", 200, 86_400)
                .unwrap();
        }
        let f = fetcher(store);

        let start = std::time::Instant::now();
        for i in 0..10 {
            f.get(Host::MusicBrainz, Source::MusicBrainz, &format!("https://mb/{i}"))
                .await
                .unwrap();
        }
        assert!(
            start.elapsed().as_secs() < 2,
            "cached reads were throttled: {:?}",
            start.elapsed()
        );
        assert_eq!(f.counters.snapshot().0, 10);
    }

    #[tokio::test]
    async fn expired_cache_entry_is_a_miss() {
        let store = Store::open_in_memory().unwrap();
        store
            .cache_put("https://mb/x", "musicbrainz", "{}", 200, -10)
            .unwrap();
        let f = fetcher(store);
        assert!(!f.is_cached("https://mb/x"));
    }
}
