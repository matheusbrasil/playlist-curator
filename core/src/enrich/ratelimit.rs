//! Per-host rate limiting.
//!
//! Each upstream gets its own limiter, because their limits are unrelated and a
//! shared bucket would throttle fast sources down to the slowest one.
//!
//! MusicBrainz's 1 req/s is not negotiable — exceeding it gets an IP blocked, and
//! it is also a condition of use alongside the identifying User-Agent. We use
//! 1 req/2s (half the documented ceiling) so timing jitter, retries and the extra
//! recording-tags call never push us over the line.

use governor::clock::DefaultClock;
use governor::state::{InMemoryState, NotKeyed};
use governor::{Quota, RateLimiter};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

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
            limiters.insert(host, Arc::new(Self::limiter_for(host)));
        }
        RateLimiters {
            limiters: Arc::new(limiters),
        }
    }

    fn limiter_for(host: Host) -> Limiter {
        let quota = match host {
            // 1 req/2s — half of MB's published 1 req/s ceiling. The extra headroom
            // absorbs retries and the recording-tags second call per ISRC without
            // triggering a temporary IP ban.
            Host::MusicBrainz => Quota::with_period(Duration::from_millis(2_000))
                .expect("2 000 ms is a non-zero period"),
            // Last.fm: ~5/s documented; stay conservatively under.
            Host::Lastfm => Quota::per_second(NonZeroU32::new(4).unwrap()),
            // Discogs: 60/minute for authenticated clients.
            Host::Discogs => Quota::per_second(NonZeroU32::new(1).unwrap()),
            // Wikidata: no published limit; be a good citizen.
            Host::Wikidata => Quota::per_second(NonZeroU32::new(2).unwrap()),
        };
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
    fn musicbrainz_period_is_at_least_two_seconds() {
        // Guards against a well-meaning change that would collapse the period
        // back to 1 s and risk getting our IP blocked by MusicBrainz.
        // Three acquisitions: first is immediate, second and third each wait
        // one period, so total must be >= 2 x 2 s = 4 s.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let limiters = RateLimiters::new();
            let start = Instant::now();
            for _ in 0..3 {
                limiters.acquire(Host::MusicBrainz).await;
            }
            let elapsed = start.elapsed();
            assert!(
                elapsed.as_secs_f64() >= 3.8,
                "3 MusicBrainz acquisitions took {elapsed:?}, expected >= ~4 s (2 s period)"
            );
        });
    }

    #[tokio::test]
    async fn hosts_are_limited_independently() {
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
        // Cloning must not hand out a fresh allowance.
        let a = RateLimiters::new();
        let b = a.clone();
        a.acquire(Host::MusicBrainz).await;

        let start = Instant::now();
        b.acquire(Host::MusicBrainz).await;
        assert!(
            start.elapsed().as_millis() >= 1_800,
            "clone bypassed the shared limiter"
        );
    }
}
