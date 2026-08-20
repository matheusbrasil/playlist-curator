//! Metadata enrichment: turning Spotify track identities into musical facts.
//!
//! # The match cascade
//!
//! For each track, in order, stopping at the first success:
//!
//! 1. **Recording via ISRC** — deterministic, high confidence.
//! 2. **Artist via Spotify URL relationship** — resolves the *artist* even when
//!    the track cannot be matched. The most valuable step in the pipeline,
//!    because genre and origin are properties of the artist, not the recording.
//! 3. **Name search** — scored fallback; below the threshold it is queued for
//!    review rather than guessed at.
//!
//! Nothing here infers facts. Every genre, country and year is traceable to a
//! row in `tag_signal` or an `mb_*` table.

pub mod discogs;
pub mod fetch;
pub mod lastfm;
pub mod musicbrainz;
pub mod ratelimit;
pub mod wikidata;

pub mod pipeline;

pub use fetch::Fetcher;
pub use ratelimit::{Host, RateLimiters};
// Re-exported once `pipeline` lands:
// pub use pipeline::{enrich_playlist, EnrichProgress, EnrichStats};
