//! Turning raw tags into a usable genre.
//!
//! The problem this module exists to solve: upstream folksonomies contain
//! genres that do not exist, plus a large amount of non-genre noise
//! (`favorites`, `seen live`, `beautiful`, `10s`). The answer is a *closed*
//! canonical vocabulary plus deterministic normalisation — not a model guessing.
//!
//! Order of operations:
//!  1. Deterministic normalisation (case, accents, punctuation, known synonyms).
//!  2. Lookup against the canonical vocabulary and the learned alias table.
//!  3. Weighted aggregation across sources into a 0–1 score per track.
//!  4. Anything still unresolved goes to a queue. Only there does the LLM get an
//!     opinion, and its answer is written to `genre_alias` so each tag is decided
//!     once in the lifetime of the app.

pub mod aliases;
pub mod derive;
pub mod genres;
pub mod normalize;

// Re-exported once those modules land:
// pub use aliases::Taxonomy;
// pub use derive::{derive_era, derive_origin};
// pub use normalize::{normalize_tag, NON_GENRE_TAGS};
