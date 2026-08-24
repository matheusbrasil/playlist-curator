//! Scoring playlist candidates.
//!
//! Facet enumeration produces far more candidates than anyone wants to look at —
//! most of them tiny, redundant, or held together by weak data. This module
//! decides which are worth proposing. Every component is 0..=1 so the weights
//! stay comparable and the breakdown can be shown in the UI.

use super::query::ScoredTrack;
use serde::{Deserialize, Serialize};

/// A playlist below this is too thin to be interesting.
pub const MIN_USEFUL_TRACKS: usize = 15;
/// Above this it stops being a curated selection.
pub const MAX_USEFUL_TRACKS: usize = 120;

/// Relative importance of each component. Coherence leads: a playlist whose
/// tracks only weakly belong to its genre is worse than a smaller one that
/// clearly does.
const W_SIZE: f64 = 0.25;
const W_COHERENCE: f64 = 0.30;
const W_SPECIFICITY: f64 = 0.20;
const W_CONFIDENCE: f64 = 0.15;
const W_REDUNDANCY: f64 = 0.10;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CandidateScore {
    pub total: f64,
    /// How close the track count is to a useful range.
    pub size: f64,
    /// Mean genre score of the member tracks.
    pub coherence: f64,
    /// Bonus for cross-facet queries: "soul BR 70s" beats "soul".
    pub specificity: f64,
    /// Penalty for overlapping an already-accepted candidate.
    pub redundancy: f64,
    /// Penalty for resting on data flagged for review.
    pub confidence: f64,
}

/// Score one candidate.
///
/// `specificity_axes` is how many facets the filter constrains (1–3), and
/// `overlap` is the largest fraction of these tracks already covered by an
/// accepted candidate.
pub fn score_candidate(
    tracks: &[ScoredTrack],
    specificity_axes: usize,
    overlap: f64,
) -> CandidateScore {
    let size = size_score(tracks.len());
    let coherence = coherence_score(tracks);
    let specificity = specificity_score(specificity_axes);
    let confidence = confidence_score(tracks);
    // Stored as a penalty magnitude so the UI can display "70% redundant".
    let redundancy = overlap.clamp(0.0, 1.0);

    let total = (W_SIZE * size
        + W_COHERENCE * coherence
        + W_SPECIFICITY * specificity
        + W_CONFIDENCE * confidence
        + W_REDUNDANCY * (1.0 - redundancy))
        .clamp(0.0, 1.0);

    CandidateScore {
        total,
        size,
        coherence,
        specificity,
        redundancy,
        confidence,
    }
}

/// 1.0 inside the useful range, tapering off outside it.
///
/// Below the minimum the penalty is steep — a 3-track playlist is not a
/// playlist. Above the maximum it is gentle, because an over-full playlist is
/// merely unfocused rather than useless, and `max_tracks` can trim it.
fn size_score(n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    if n < MIN_USEFUL_TRACKS {
        // Quadratic so tiny playlists are punished hard.
        let ratio = n as f64 / MIN_USEFUL_TRACKS as f64;
        return ratio * ratio;
    }
    if n <= MAX_USEFUL_TRACKS {
        return 1.0;
    }
    let excess = (n - MAX_USEFUL_TRACKS) as f64 / MAX_USEFUL_TRACKS as f64;
    (1.0 - 0.5 * excess).max(0.3)
}

/// Mean genre score across members: how strongly these tracks really are what
/// the playlist claims.
fn coherence_score(tracks: &[ScoredTrack]) -> f64 {
    if tracks.is_empty() {
        return 0.0;
    }
    let sum: f64 = tracks.iter().map(|t| t.reason.genre_score).sum();
    (sum / tracks.len() as f64).clamp(0.0, 1.0)
}

/// One constrained axis is bland, three is a genuinely interesting cross-section.
fn specificity_score(axes: usize) -> f64 {
    match axes {
        0 => 0.0,
        1 => 0.4,
        2 => 0.75,
        _ => 1.0,
    }
}

/// Fraction of members whose metadata is *not* flagged for review.
fn confidence_score(tracks: &[ScoredTrack]) -> f64 {
    if tracks.is_empty() {
        return 0.0;
    }
    let flagged = tracks.iter().filter(|t| t.reason.needs_review).count();
    1.0 - (flagged as f64 / tracks.len() as f64)
}

/// Jaccard similarity between two candidates: shared tracks over their union.
///
/// Used to suppress near-duplicates — once "Brazilian soul" is accepted,
/// "Brazilian funk-soul" over the same tracks adds nothing.
///
/// Symmetry is the whole point. Measuring instead the fraction of the candidate
/// already covered would make *every* subset look fully redundant, so a broad
/// "Brazil · 1970s" would suppress the far more interesting "Soul · Brazil ·
/// 1970s" nested inside it. Jaccard scores that pair at 12/20 = 0.6 and keeps
/// both, while still scoring two identical selections at 1.0.
pub fn jaccard_similarity(a: &[ScoredTrack], b: &[ScoredTrack]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let a_ids: std::collections::HashSet<&str> = a.iter().map(|t| t.spotify_id.as_str()).collect();
    let b_ids: std::collections::HashSet<&str> = b.iter().map(|t| t.spotify_id.as_str()).collect();
    let intersection = a_ids.intersection(&b_ids).count();
    let union = a_ids.union(&b_ids).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f64 / union as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suggest::query::TrackReason;

    fn tracks(n: usize, genre_score: f64, flagged: usize) -> Vec<ScoredTrack> {
        (0..n)
            .map(|i| ScoredTrack {
                spotify_id: format!("t{i}"),
                name: format!("Track {i}"),
                artists: vec!["A".into()],
                reason: TrackReason {
                    genre: Some("soul".into()),
                    genre_score,
                    genre_source: Some("musicbrainz".into()),
                    country_code: Some("BR".into()),
                    year: Some(1972),
                    era_source: Some("mb_first_release".into()),
                    needs_review: i < flagged,
                },
            })
            .collect()
    }

    #[test]
    fn tiny_playlists_are_punished_hard() {
        let three = score_candidate(&tracks(3, 0.9, 0), 3, 0.0);
        let twenty = score_candidate(&tracks(20, 0.9, 0), 3, 0.0);
        assert!(three.size < 0.1, "size {}", three.size);
        assert_eq!(twenty.size, 1.0);
        assert!(twenty.total > three.total);
    }

    #[test]
    fn size_score_plateaus_across_the_useful_range() {
        assert_eq!(size_score(MIN_USEFUL_TRACKS), 1.0);
        assert_eq!(size_score(60), 1.0);
        assert_eq!(size_score(MAX_USEFUL_TRACKS), 1.0);
        assert_eq!(size_score(0), 0.0);
    }

    #[test]
    fn oversized_playlists_are_penalised_gently_and_bounded() {
        let big = size_score(300);
        assert!(big < 1.0);
        // An over-full playlist is unfocused, not useless.
        assert!(big >= 0.3, "size {big}");
        assert!(size_score(100_000) >= 0.3);
    }

    #[test]
    fn coherence_tracks_the_mean_genre_score() {
        let strong = score_candidate(&tracks(20, 0.9, 0), 2, 0.0);
        let weak = score_candidate(&tracks(20, 0.2, 0), 2, 0.0);
        assert!((strong.coherence - 0.9).abs() < 1e-9);
        assert!((weak.coherence - 0.2).abs() < 1e-9);
        assert!(strong.total > weak.total);
    }

    #[test]
    fn cross_facet_candidates_outrank_single_facet_ones() {
        // "Soul BR 70s" should be proposed ahead of plain "soul".
        let broad = score_candidate(&tracks(30, 0.8, 0), 1, 0.0);
        let narrow = score_candidate(&tracks(30, 0.8, 0), 3, 0.0);
        assert!(narrow.specificity > broad.specificity);
        assert!(narrow.total > broad.total);
    }

    #[test]
    fn review_flags_reduce_confidence() {
        let clean = score_candidate(&tracks(20, 0.8, 0), 2, 0.0);
        let half = score_candidate(&tracks(20, 0.8, 10), 2, 0.0);
        assert_eq!(clean.confidence, 1.0);
        assert!((half.confidence - 0.5).abs() < 1e-9);
        assert!(clean.total > half.total);
    }

    #[test]
    fn redundancy_lowers_the_total() {
        let fresh = score_candidate(&tracks(20, 0.8, 0), 2, 0.0);
        let dupe = score_candidate(&tracks(20, 0.8, 0), 2, 1.0);
        assert!(fresh.total > dupe.total);
        assert_eq!(dupe.redundancy, 1.0);
    }

    #[test]
    fn empty_candidate_scores_zero_everywhere() {
        let s = score_candidate(&[], 3, 0.0);
        assert_eq!(s.size, 0.0);
        assert_eq!(s.coherence, 0.0);
        assert_eq!(s.confidence, 0.0);
    }

    #[test]
    fn every_component_and_total_stay_within_unit_range() {
        for n in [0usize, 1, 15, 120, 500] {
            for score in [0.0, 0.5, 1.0] {
                let s = score_candidate(&tracks(n, score, n / 2), 3, 0.5);
                for (name, v) in [
                    ("total", s.total), ("size", s.size), ("coherence", s.coherence),
                    ("specificity", s.specificity), ("redundancy", s.redundancy),
                    ("confidence", s.confidence),
                ] {
                    assert!((0.0..=1.0).contains(&v), "{name} = {v} for n={n}");
                }
            }
        }
    }

    #[test]
    fn identical_selections_are_fully_similar() {
        let a = tracks(10, 0.8, 0);
        assert_eq!(jaccard_similarity(&a, &a), 1.0);
        assert_eq!(jaccard_similarity(&a, &[]), 0.0);
        assert_eq!(jaccard_similarity(&[], &a), 0.0);
    }

    #[test]
    fn similarity_is_symmetric_so_a_subset_is_not_fully_redundant() {
        // The case that matters: a specific 12-track cross-section nested inside
        // a broad 20-track one must not be suppressed by it.
        let broad = tracks(20, 0.8, 0);
        let specific: Vec<_> = broad.iter().take(12).cloned().collect();

        let s = jaccard_similarity(&specific, &broad);
        assert!((s - 12.0 / 20.0).abs() < 1e-9, "similarity {s}");
        assert_eq!(s, jaccard_similarity(&broad, &specific), "must be symmetric");
        assert!(s < 0.85, "a meaningful subset would be wrongly suppressed");
    }

    #[test]
    fn disjoint_selections_are_not_similar() {
        let a = tracks(10, 0.8, 0);
        let b: Vec<_> = a
            .iter()
            .map(|t| ScoredTrack { spotify_id: format!("other-{}", t.spotify_id), ..t.clone() })
            .collect();
        assert_eq!(jaccard_similarity(&a, &b), 0.0);
    }
}
