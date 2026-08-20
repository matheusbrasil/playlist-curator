//! Suggestion engine: from enriched tracks to reviewable playlist proposals.
//!
//! Two entry points, one executor. Automatic facet enumeration and free-text
//! queries both produce a [`filter::PlaylistFilter`], which is executed as plain
//! SQL over the local cache.

pub mod facets;
pub mod filter;
pub mod nl;
pub mod query;
pub mod score;

pub use facets::suggest;
pub use filter::{GenreMode, PlaylistFilter};
pub use query::{execute, ScoredTrack, SuggestionCard, TrackReason};
pub use score::{score_candidate, CandidateScore};
