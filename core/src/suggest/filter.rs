//! `PlaylistFilter` — the one description of "which tracks belong in a playlist".
//!
//! Both entry points converge here: automatic facet enumeration builds these
//! structurally, and the natural-language parser produces one as validated JSON.
//! Nothing downstream needs to know which produced it.

use crate::error::{CoreError, Result};
use crate::store::Store;
use serde::{Deserialize, Serialize};

/// How to interpret the `genres` list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GenreMode {
    /// Track matches if it has any listed genre exactly.
    #[default]
    Any,
    /// Track matches if it has any listed genre *or any descendant* of one.
    /// This is what makes "samba" also collect samba-rock, samba-jazz and pagode
    /// instead of yielding forty playlists of three tracks.
    AnyWithChildren,
    /// Track must carry every listed genre.
    All,
}

/// A complete, executable description of a derived playlist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct PlaylistFilter {
    /// Canonical genre slugs. Always validated against `genre_canonical` before
    /// execution, so a hallucinated genre cannot reach the query.
    pub genres: Vec<String>,
    pub genre_mode: GenreMode,
    /// ISO 3166-1 alpha-2 country codes of artist origin.
    pub countries: Vec<String>,
    /// Inclusive year range.
    pub year_range: Option<(i32, i32)>,
    /// Reject the result outright below this many tracks.
    pub min_tracks: Option<usize>,
    /// Cap the result, keeping the highest-scoring tracks.
    pub max_tracks: Option<usize>,
    /// Minimum genre score a track must have to count as a member.
    pub min_genre_score: Option<f64>,
    /// Restrict to tracks of one source playlist. `None` searches everything
    /// imported.
    pub source_playlist_id: Option<String>,
    /// Exclude tracks whose metadata is flagged for review.
    pub exclude_needs_review: bool,
}

impl PlaylistFilter {
    /// Reject a filter that cannot produce a sensible query.
    ///
    /// Genre slugs are checked against the canonical vocabulary; this is the gate
    /// that stops an LLM inventing a genre and the app silently returning an
    /// empty or nonsensical playlist.
    pub fn validate(&self, store: &Store) -> Result<()> {
        if self.genres.is_empty() && self.countries.is_empty() && self.year_range.is_none() {
            return Err(CoreError::InvalidFilter(
                "a filter needs at least one of: genre, country, year range".into(),
            ));
        }

        for slug in &self.genres {
            if store.canonical_genre(slug)?.is_none() {
                return Err(CoreError::InvalidFilter(format!(
                    "'{slug}' is not a genre in the canonical vocabulary"
                )));
            }
        }

        for code in &self.countries {
            if code.len() != 2 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(CoreError::InvalidFilter(format!(
                    "'{code}' is not an ISO 3166-1 alpha-2 country code"
                )));
            }
        }

        if let Some((from, to)) = self.year_range {
            if from > to {
                return Err(CoreError::InvalidFilter(format!(
                    "year range {from}–{to} is inverted"
                )));
            }
            if !(1860..=2100).contains(&from) || !(1860..=2100).contains(&to) {
                return Err(CoreError::InvalidFilter(format!(
                    "year range {from}–{to} is outside the plausible range for recorded music"
                )));
            }
        }

        if let (Some(min), Some(max)) = (self.min_tracks, self.max_tracks) {
            if min > max {
                return Err(CoreError::InvalidFilter(format!(
                    "min_tracks {min} exceeds max_tracks {max}"
                )));
            }
        }

        if let Some(score) = self.min_genre_score {
            if !(0.0..=1.0).contains(&score) {
                return Err(CoreError::InvalidFilter(format!(
                    "min_genre_score {score} is outside 0.0–1.0"
                )));
            }
        }

        Ok(())
    }

    /// Country codes upper-cased, as stored.
    pub fn normalized_countries(&self) -> Vec<String> {
        self.countries.iter().map(|c| c.to_uppercase()).collect()
    }

    /// How many independent axes this filter constrains. Used by the scorer:
    /// "soul + BR + 1970s" is a more interesting suggestion than "soul".
    pub fn specificity(&self) -> usize {
        usize::from(!self.genres.is_empty())
            + usize::from(!self.countries.is_empty())
            + usize::from(self.year_range.is_some())
    }

    /// Decade covered, when the range is exactly one decade.
    pub fn single_decade(&self) -> Option<i32> {
        let (from, to) = self.year_range?;
        (from % 10 == 0 && to == from + 9).then_some(from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CanonicalGenre;

    fn store_with_genres() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.upsert_canonical_genres(&[
            CanonicalGenre { slug: "soul".into(), label: "Soul".into(), parent_slug: None },
            CanonicalGenre { slug: "samba".into(), label: "Samba".into(), parent_slug: None },
        ])
        .unwrap();
        s
    }

    #[test]
    fn accepts_a_well_formed_filter() {
        let s = store_with_genres();
        let f = PlaylistFilter {
            genres: vec!["soul".into()],
            genre_mode: GenreMode::AnyWithChildren,
            countries: vec!["BR".into()],
            year_range: Some((1970, 1979)),
            min_tracks: Some(15),
            ..Default::default()
        };
        f.validate(&s).unwrap();
        assert_eq!(f.specificity(), 3);
        assert_eq!(f.single_decade(), Some(1970));
    }

    #[test]
    fn rejects_a_genre_outside_the_canonical_vocabulary() {
        // The guard against an LLM inventing a plausible-sounding genre.
        let s = store_with_genres();
        let f = PlaylistFilter {
            genres: vec!["brazilian-cosmic-soul".into()],
            ..Default::default()
        };
        let err = f.validate(&s).unwrap_err();
        assert!(
            matches!(err, CoreError::InvalidFilter(ref m) if m.contains("brazilian-cosmic-soul")),
            "{err}"
        );
    }

    #[test]
    fn rejects_an_empty_filter() {
        let s = store_with_genres();
        let err = PlaylistFilter::default().validate(&s).unwrap_err();
        assert!(matches!(err, CoreError::InvalidFilter(_)), "{err}");
    }

    #[test]
    fn rejects_malformed_country_codes() {
        let s = store_with_genres();
        for bad in ["BRA", "B", "Brazil", "1R"] {
            let f = PlaylistFilter { countries: vec![bad.into()], ..Default::default() };
            assert!(f.validate(&s).is_err(), "{bad} should be rejected");
        }
        let ok = PlaylistFilter { countries: vec!["br".into()], ..Default::default() };
        ok.validate(&s).unwrap();
        assert_eq!(ok.normalized_countries(), vec!["BR".to_string()]);
    }

    #[test]
    fn rejects_inverted_and_implausible_year_ranges() {
        let s = store_with_genres();
        let inverted = PlaylistFilter { year_range: Some((1979, 1970)), ..Default::default() };
        assert!(inverted.validate(&s).is_err());

        let ancient = PlaylistFilter { year_range: Some((1200, 1300)), ..Default::default() };
        assert!(ancient.validate(&s).is_err());
    }

    #[test]
    fn rejects_contradictory_size_bounds() {
        let s = store_with_genres();
        let f = PlaylistFilter {
            genres: vec!["soul".into()],
            min_tracks: Some(50),
            max_tracks: Some(20),
            ..Default::default()
        };
        assert!(f.validate(&s).is_err());
    }

    #[test]
    fn rejects_out_of_band_genre_score() {
        let s = store_with_genres();
        let f = PlaylistFilter {
            genres: vec!["soul".into()],
            min_genre_score: Some(1.5),
            ..Default::default()
        };
        assert!(f.validate(&s).is_err());
    }

    #[test]
    fn single_decade_only_matches_an_exact_decade() {
        let mut f = PlaylistFilter { year_range: Some((1970, 1979)), ..Default::default() };
        assert_eq!(f.single_decade(), Some(1970));

        f.year_range = Some((1972, 1978));
        assert_eq!(f.single_decade(), None);

        f.year_range = Some((1970, 1989));
        assert_eq!(f.single_decade(), None);
    }

    #[test]
    fn roundtrips_through_the_json_the_llm_produces() {
        // The LLM is prompted to emit camelCase to match the struct's serde format.
        let json = r#"{
            "genres": ["soul"],
            "genreMode": "any_with_children",
            "countries": ["BR"],
            "yearRange": [1970, 1979],
            "minTracks": 15
        }"#;
        let f: PlaylistFilter = serde_json::from_str(json).unwrap();
        assert_eq!(f.genres, vec!["soul".to_string()]);
        assert_eq!(f.genre_mode, GenreMode::AnyWithChildren);
        assert_eq!(f.year_range, Some((1970, 1979)));
        assert_eq!(f.min_tracks, Some(15));
        // Unspecified fields fall back to defaults rather than failing.
        assert_eq!(f.max_tracks, None);
        assert!(!f.exclude_needs_review);
    }
}
