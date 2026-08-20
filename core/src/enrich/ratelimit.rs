//! Per-host rate limiting.
//!
//! Each upstream gets its own limiter, because their limits are unrelated and a
//! shared bucket would throttle fast sources down to the slowest one.
//!
//! MusicBrainz's 1 req/s is not negotiable — exceeding it gets an IP blocked, and
//! it is also a condition of use alongside the identifying User-Agent.

use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

type Limiter = RateLimiter<NotKeyed, InMemoryState, DefaultClock>;

/// Which upstream a request is going to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Host {
    MusicBrainz,
    Lastfm,
    Discogs,
    Wikidata,
}

impl Host {
    /// Requests per second allowed. These are deliberately at or below the
    /// documented ceilings: being throttled costs far more time than going slow.
    pub fn per_second(self) -> u32 {
        match self {
            // Hard published limit. Do not raise.
            Host::MusicBrainz => 1,
            // Documented as ~5/s for non-commercial keys; stay under it.
            Host::Lastfm => 4,
            // 60 requests/minute for authenticated clients.
            Host::Discogs => 1,
            // No published limit; be a good citizen on a free public endpoint.
            Host::Wikidata => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Host::MusicBrainz => "musicbrainz",
            Host::Lastfm => "lastfm",
            Host::Discogs => "discogs",
            Host::Wikidata => "wikidata",
        }
    }
}

/// Holds one limiter per host. Cloneable and cheap to share across tasks.
#[derive(Clone)]
pub struct RateLimiters {
    limiters: Arc<HashMap<Host, Arc<Limiter>>>,
}

impl Default for RateLimiters {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiters {
    pub fn new() -> Self {
        let mut limiters = HashMap::new();
        for host in [Host::MusicBrainz, Host::Lastfm, Host::Discogs, Host::Wikidata] {
            limiters.insert(host, Arc::new(Self::limiter_for(host.per_second())));
        }
        RateLimiters {
            limiters: Arc::new(limiters),
        }
    }

    /// Build a limiter with no burst allowance beyond the per-second rate.
    ///
    /// `Quota::per_second(n)` would permit an initial burst of `n`; for
    /// MusicBrainz at n=1 that is exactly one request, which is what we want, and
    /// the replenish interval enforces the spacing thereafter.
    fn limiter_for(per_second: u32) -> Limiter {
        let quota = Quota::per_second(
            NonZeroU32::new(per_second.max(1)).expect("per_second is at least 1"),
        );
        RateLimiter::direct(quota)
    }

    /// Wait until the next request to `host` is permitted.
    pub async fn acquire(&self, host: Host) {
        if let Some(limiter) = self.limiters.get(&host) {
            limiter.until_ready().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn musicbrainz_limit_is_one_per_second() {
        // Guards against a well-meaning change that would get the user's IP
        // blocked by MusicBrainz.
        assert_eq!(Host::MusicBrainz.per_second(), 1);
    }

    #[tokio::test]
    async fn ten_musicbrainz_calls_take_at_least_nine_seconds() {
        // 1 req/s with a burst of 1: the first is immediate, the remaining nine
        // are spaced a second apart.
        let limiters = RateLimiters::new();
        let start = Instant::now();
        for _ in 0..10 {
            limiters.acquire(Host::MusicBrainz).await;
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_secs_f64() >= 8.9,
            "10 MusicBrainz acquisitions took {elapsed:?}, expected >= ~9s"
        );
    }

    #[tokio::test]
    async fn hosts_are_limited_independently() {
        // A slow MusicBrainz queue must not stall Last.fm requests.
        let limiters = RateLimiters::new();
        limiters.acquire(Host::MusicBrainz).await;

        let start = Instant::now();
        limiters.acquire(Host::Lastfm).await;
        assert!(
            start.elapsed().as_millis() < 500,
            "Last.fm was blocked by the MusicBrainz limiter"
        );
    }

    #[tokio::test]
    async fn limiter_is_shared_across_clones() {
        // Cloning must not hand out a fresh allowance, or concurrent tasks would
        // each get their own 1 req/s and collectively exceed the limit.
        let a = RateLimiters::new();
        let b = a.clone();
        a.acquire(Host::MusicBrainz).await;

        let start = Instant::now();
        b.acquire(Host::MusicBrainz).await;
        assert!(
            start.elapsed().as_millis() >= 900,
            "clone bypassed the shared limiter"
        );
    }
}
