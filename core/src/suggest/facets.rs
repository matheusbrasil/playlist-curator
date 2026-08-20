//! Automatic candidate enumeration.
//!
//! Walks the facets actually present in a playlist — genres, origin countries,
//! decades — and their combinations, executes each as a filter, scores the
//! result, and returns the handful worth showing.
//!
//! Enumeration is driven by what the data contains, not by a fixed list, so a
//! playlist of Brazilian music proposes Brazilian cross-sections and never offers
//! an empty "Japanese metal of the 1950s".

use super::filter::{GenreMode, PlaylistFilter};
use super::query::{execute, ScoredTrack, SuggestionCard};
use super::score::{jaccard_similarity, score_candidate, MIN_USEFUL_TRACKS};
use crate::error::Result;
use crate::store::Store;
use rusqlite::params;

/// How many proposals to return. Enough to browse, few enough to review.
const MAX_SUGGESTIONS: usize = 20;

/// A candidate is dropped when this much of it is already covered by an accepted
/// one. Deliberately permissive: some overlap between "Soul" and "Soul · Brazil"
/// is expected and useful.
const REDUNDANCY_CUTOFF: f64 = 0.85;

/// One facet value observed in a playlist, with how many tracks carry it.
#[derive(Debug, Clone, PartialEq)]
pub struct FacetValue {
    pub key: String,
    pub label: String,
    pub count: i64,
}

/// Genres present in the playlist, rolled up to the level worth proposing.
///
/// Both the specific genre and its parent are returned when the parent has more
/// tracks, because "samba" and "samba-rock" can each be a good playlist and the
/// scorer decides which survives.
pub fn genre_facets(store: &Store, playlist_id: &str) -> Result<Vec<FacetValue>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "WITH RECURSIVE
         member AS (
             SELECT tg.track_spotify_id AS tid, tg.canonical_slug AS slug
             FROM track_genre tg
             JOIN playlist_track pt ON pt.track_spotify_id = tg.track_spotify_id
             WHERE pt.playlist_id = ?1
         ),
         -- Walk each observed genre up to its roots so a parent accumulates the
         -- track counts of all its descendants.
         rolled(tid, slug) AS (
             SELECT tid, slug FROM member
             UNION
             SELECT r.tid, gc.parent_slug
             FROM rolled r
             JOIN genre_canonical gc ON gc.slug = r.slug
             WHERE gc.parent_slug IS NOT NULL
         )
         SELECT r.slug, COALESCE(gc.label, r.slug), COUNT(DISTINCT r.tid) AS n
         FROM rolled r
         LEFT JOIN genre_canonical gc ON gc.slug = r.slug
         GROUP BY r.slug
         HAVING n > 0
         ORDER BY n DESC, r.slug",
    )?;
    let rows = stmt.query_map(params![playlist_id], |r| {
        Ok(FacetValue { key: r.get(0)?, label: r.get(1)?, count: r.get(2)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn country_facets(store: &Store, playlist_id: &str) -> Result<Vec<FacetValue>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT UPPER(ao.country_code),
                COALESCE(MAX(ao.country_label), UPPER(ao.country_code)),
                COUNT(DISTINCT pt.track_spotify_id) AS n
         FROM playlist_track pt
         JOIN track_artist ta ON ta.track_spotify_id = pt.track_spotify_id
         JOIN artist_origin ao ON ao.artist_spotify_id = ta.artist_spotify_id
         WHERE pt.playlist_id = ?1 AND ao.country_code IS NOT NULL
         GROUP BY UPPER(ao.country_code)
         ORDER BY n DESC",
    )?;
    let rows = stmt.query_map(params![playlist_id], |r| {
        Ok(FacetValue { key: r.get(0)?, label: r.get(1)?, count: r.get(2)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn decade_facets(store: &Store, playlist_id: &str) -> Result<Vec<FacetValue>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT te.decade, COUNT(*) AS n
         FROM playlist_track pt
         JOIN track_era te ON te.track_spotify_id = pt.track_spotify_id
         WHERE pt.playlist_id = ?1 AND te.decade IS NOT NULL
         GROUP BY te.decade
         ORDER BY n DESC",
    )?;
    let rows = stmt.query_map(params![playlist_id], |r| {
        let decade: i32 = r.get(0)?;
        Ok(FacetValue {
            key: decade.to_string(),
            label: format!("{decade}s"),
            count: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Enumerate, execute, score and rank candidate playlists for `playlist_id`.
pub fn suggest(store: &Store, playlist_id: &str) -> Result<Vec<SuggestionCard>> {
    let genres = genre_facets(store, playlist_id)?;
    let countries = country_facets(store, playlist_id)?;
    let decades = decade_facets(store, playlist_id)?;

    let candidates = enumerate_filters(playlist_id, &genres, &countries, &decades);

    // Execute everything first, then rank. Redundancy can only be judged against
    // candidates already chosen, so selection has to be a second pass.
    let mut executed: Vec<(PlaylistFilter, Vec<ScoredTrack>)> = Vec::new();
    for filter in candidates {
        let tracks = match execute(store, &filter) {
            Ok(t) => t,
            // A filter can become invalid if the vocabulary lacks a slug the
            // facet query produced; skip rather than abort the whole run.
            Err(e) => {
                tracing::debug!(?filter, error = %e, "skipping candidate");
                continue;
            }
        };
        if tracks.len() >= MIN_USEFUL_TRACKS.min(5) {
            executed.push((filter, tracks));
        }
    }

    // Provisionally rank by score in isolation, so the greedy pass considers the
    // strongest candidates first and suppresses weaker near-duplicates.
    //
    // Ties are common and must not be broken arbitrarily: a genre and its child
    // often cover exactly the same tracks, and only one will survive the
    // redundancy pass. Prefer the shallower genre — "Soul · Brazil · 1970s" is a
    // better proposal than "Funk Soul · Brazil · 1970s" over the same tracks, and
    // rolling up is the reason the hierarchy exists. The id is the final
    // tie-break so the output is stable between runs.
    let depths = genre_depths(store)?;
    executed.sort_by(|a, b| {
        let sa = score_candidate(&a.1, a.0.specificity(), 0.0).total;
        let sb = score_candidate(&b.1, b.0.specificity(), 0.0).total;
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| filter_depth(&a.0, &depths).cmp(&filter_depth(&b.0, &depths)))
            .then_with(|| filter_id(&a.0).cmp(&filter_id(&b.0)))
    });

    let mut accepted: Vec<SuggestionCard> = Vec::new();
    let mut accepted_tracks: Vec<Vec<ScoredTrack>> = Vec::new();

    for (filter, tracks) in executed {
        let overlap = accepted_tracks
            .iter()
            .map(|prev| jaccard_similarity(&tracks, prev))
            .fold(0.0_f64, f64::max);
        if overlap >= REDUNDANCY_CUTOFF {
            continue;
        }

        let score = score_candidate(&tracks, filter.specificity(), overlap);
        let (proposed_name, description) = name_for(store, &filter, tracks.len())?;

        accepted.push(SuggestionCard {
            id: filter_id(&filter),
            proposed_name,
            description,
            track_count: tracks.len(),
            score,
            tracks: tracks.clone(),
            filter,
        });
        accepted_tracks.push(tracks);

        if accepted.len() >= MAX_SUGGESTIONS {
            break;
        }
    }

    // Re-sort on the final scores, which include the redundancy penalty.
    accepted.sort_by(|a, b| {
        b.score
            .total
            .partial_cmp(&a.score.total)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(accepted)
}

/// Build the candidate filter set: each facet alone, then the pairs and triples
/// that make an interesting cross-section.
fn enumerate_filters(
    playlist_id: &str,
    genres: &[FacetValue],
    countries: &[FacetValue],
    decades: &[FacetValue],
) -> Vec<PlaylistFilter> {
    // Cap each axis: the combinatorial product is what would otherwise produce
    // thousands of queries, nearly all of them empty.
    const TOP_GENRES: usize = 12;
    const TOP_COUNTRIES: usize = 8;
    const TOP_DECADES: usize = 8;

    let g = &genres[..genres.len().min(TOP_GENRES)];
    let c = &countries[..countries.len().min(TOP_COUNTRIES)];
    let d = &decades[..decades.len().min(TOP_DECADES)];

    let base = || PlaylistFilter {
        genre_mode: GenreMode::AnyWithChildren,
        source_playlist_id: Some(playlist_id.to_string()),
        ..Default::default()
    };
    let mut out = Vec::new();

    for genre in g {
        out.push(PlaylistFilter { genres: vec![genre.key.clone()], ..base() });
    }
    for country in c {
        out.push(PlaylistFilter { countries: vec![country.key.clone()], ..base() });
    }
    for decade in d {
        if let Some(range) = decade_range(&decade.key) {
            out.push(PlaylistFilter { year_range: Some(range), ..base() });
        }
    }
    for genre in g {
        for country in c {
            out.push(PlaylistFilter {
                genres: vec![genre.key.clone()],
                countries: vec![country.key.clone()],
                ..base()
            });
        }
    }
    for genre in g {
        for decade in d {
            if let Some(range) = decade_range(&decade.key) {
                out.push(PlaylistFilter {
                    genres: vec![genre.key.clone()],
                    year_range: Some(range),
                    ..base()
                });
            }
        }
    }
    for country in c {
        for decade in d {
            if let Some(range) = decade_range(&decade.key) {
                out.push(PlaylistFilter {
                    countries: vec![country.key.clone()],
                    year_range: Some(range),
                    ..base()
                });
            }
        }
    }
    // The most specific shape, and the one the user actually asked for:
    // "Brazilian soul of the 1970s".
    for genre in g {
        for country in c {
            for decade in d {
                if let Some(range) = decade_range(&decade.key) {
                    out.push(PlaylistFilter {
                        genres: vec![genre.key.clone()],
                        countries: vec![country.key.clone()],
                        year_range: Some(range),
                        ..base()
                    });
                }
            }
        }
    }
    out
}

/// Depth of every canonical genre: 0 for a root, 1 for its children, and so on.
///
/// Walks the parent chain with a visited set, so a malformed vocabulary
/// containing a cycle yields a finite depth instead of hanging.
fn genre_depths(store: &Store) -> Result<std::collections::HashMap<String, usize>> {
    let all = store.all_canonical_genres()?;
    let parents: std::collections::HashMap<&str, Option<&str>> = all
        .iter()
        .map(|g| (g.slug.as_str(), g.parent_slug.as_deref()))
        .collect();

    let mut depths = std::collections::HashMap::new();
    for genre in &all {
        let mut depth = 0usize;
        let mut cursor = genre.slug.as_str();
        let mut seen = std::collections::HashSet::new();
        while let Some(Some(parent)) = parents.get(cursor) {
            if !seen.insert(cursor) {
                tracing::warn!(slug = cursor, "cycle in genre hierarchy; truncating depth");
                break;
            }
            depth += 1;
            cursor = parent;
        }
        depths.insert(genre.slug.clone(), depth);
    }
    Ok(depths)
}

/// Depth of the deepest genre a filter names; 0 when it names none.
fn filter_depth(
    filter: &PlaylistFilter,
    depths: &std::collections::HashMap<String, usize>,
) -> usize {
    filter
        .genres
        .iter()
        .map(|slug| depths.get(slug).copied().unwrap_or(0))
        .max()
        .unwrap_or(0)
}

fn decade_range(key: &str) -> Option<(i32, i32)> {
    let decade: i32 = key.parse().ok()?;
    Some((decade, decade + 9))
}

/// A deterministic name and description.
///
/// Plain and unambiguous by design; the optional LLM pass rewrites these into
/// something prettier, and that is purely cosmetic.
pub fn name_for(store: &Store, filter: &PlaylistFilter, track_count: usize) -> Result<(String, String)> {
    let mut parts: Vec<String> = Vec::new();

    for slug in &filter.genres {
        let label = store
            .canonical_genre(slug)?
            .map(|g| g.label)
            .unwrap_or_else(|| slug.clone());
        parts.push(label);
    }
    for code in filter.normalized_countries() {
        parts.push(country_label(store, &code)?);
    }
    if let Some(decade) = filter.single_decade() {
        parts.push(format!("{decade}s"));
    } else if let Some((from, to)) = filter.year_range {
        parts.push(format!("{from}–{to}"));
    }

    let name = if parts.is_empty() {
        "Selection".to_string()
    } else {
        parts.join(" · ")
    };
    let description = format!("{track_count} tracks · derived by Playlist Curator from local metadata");
    Ok((name, description))
}

/// Human-readable country name as recorded by the enrichment layer, falling back
/// to the code when nothing better is known.
fn country_label(store: &Store, code: &str) -> Result<String> {
    let conn = store.conn()?;
    let label: Option<String> = conn
        .query_row(
            "SELECT country_label FROM artist_origin
             WHERE UPPER(country_code) = ?1 AND country_label IS NOT NULL
             LIMIT 1",
            params![code],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    Ok(label.unwrap_or_else(|| code.to_string()))
}

/// Stable identifier for a candidate, so the UI can key on it across refreshes.
fn filter_id(filter: &PlaylistFilter) -> String {
    let mut key = String::new();
    key.push_str(&filter.genres.join("+"));
    key.push('|');
    key.push_str(&filter.normalized_countries().join("+"));
    key.push('|');
    if let Some((from, to)) = filter.year_range {
        key.push_str(&format!("{from}-{to}"));
    }
    crate::util::sha256_hex(&key)[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    /// 40 tracks: 20 Brazilian soul/samba from the 70s, 15 British rock from the
    /// 90s, 5 US hip-hop from the 2010s.
    fn seeded_store() -> Store {
        let s = Store::open_in_memory().unwrap();
        s.upsert_canonical_genres(&[
            CanonicalGenre { slug: "soul".into(), label: "Soul".into(), parent_slug: None },
            CanonicalGenre { slug: "funk-soul".into(), label: "Funk Soul".into(), parent_slug: Some("soul".into()) },
            CanonicalGenre { slug: "samba".into(), label: "Samba".into(), parent_slug: None },
            CanonicalGenre { slug: "samba-rock".into(), label: "Samba Rock".into(), parent_slug: Some("samba".into()) },
            CanonicalGenre { slug: "rock".into(), label: "Rock".into(), parent_slug: None },
            CanonicalGenre { slug: "britpop".into(), label: "Britpop".into(), parent_slug: Some("rock".into()) },
            CanonicalGenre { slug: "hip-hop".into(), label: "Hip-Hop".into(), parent_slug: None },
        ]).unwrap();
        s.upsert_playlist(&Playlist {
            spotify_id: "p1".into(), name: "Big Mix".into(), owner: Some("me".into()),
            snapshot_id: None, track_count: Some(40), synced_at: None,
        }).unwrap();

        let groups: &[(&str, &str, &str, &str, i32, usize)] = &[
            ("br-soul",  "funk-soul",  "BR", "Brazil",         1972, 12),
            ("br-samba", "samba-rock", "BR", "Brazil",         1975, 8),
            ("gb-rock",  "britpop",    "GB", "United Kingdom", 1995, 15),
            ("us-hh",    "hip-hop",    "US", "United States",  2015, 5),
        ];

        let mut entries = Vec::new();
        let mut pos = 0i64;
        for (prefix, genre, country, country_label, year, n) in groups {
            let artist_id = format!("art-{prefix}");
            s.upsert_artist(&Artist { spotify_id: artist_id.clone(), name: prefix.to_string() }).unwrap();
            s.upsert_artist_origin(&ArtistOrigin {
                artist_spotify_id: artist_id.clone(),
                country_code: Some((*country).into()),
                country_label: Some((*country_label).into()),
                city: None,
                source: OriginSource::MbBeginArea, confidence: 1.0,
            }).unwrap();

            for i in 0..*n {
                let tid = format!("{prefix}-{i}");
                s.upsert_track(&Track {
                    spotify_id: tid.clone(), name: format!("{prefix} {i}"),
                    isrc: Some(format!("I{prefix}{i}")), duration_ms: Some(200_000),
                    spotify_album_id: None,
                    // A reissue date for everything, to prove era comes from track_era.
                    spotify_release_date: Some("2015-01-01".into()),
                    is_local: false,
                }).unwrap();
                s.link_track_artist(&tid, &artist_id, 0).unwrap();
                s.replace_track_genres(&tid, &[TrackGenre {
                    canonical_slug: (*genre).into(), score: 0.85,
                }]).unwrap();
                s.upsert_track_era(&TrackEra {
                    track_spotify_id: tid.clone(),
                    year: Some(*year), decade: Some(crate::util::decade_of(*year)),
                    source: EraSource::MbFirstRelease,
                }).unwrap();
                entries.push((tid, pos, None));
                pos += 1;
            }
        }
        s.replace_playlist_tracks("p1", &entries).unwrap();
        s
    }

    #[test]
    fn genre_facets_roll_child_counts_into_the_parent() {
        let s = seeded_store();
        let facets = genre_facets(&s, "p1").unwrap();
        let by_key: std::collections::HashMap<_, _> =
            facets.iter().map(|f| (f.key.as_str(), f.count)).collect();

        // funk-soul has 12 tracks, and soul must inherit all of them.
        assert_eq!(by_key["funk-soul"], 12);
        assert_eq!(by_key["soul"], 12);
        assert_eq!(by_key["samba-rock"], 8);
        assert_eq!(by_key["samba"], 8);
        assert_eq!(by_key["britpop"], 15);
        assert_eq!(by_key["rock"], 15);
    }

    #[test]
    fn country_and_decade_facets_reflect_the_data() {
        let s = seeded_store();

        let countries = country_facets(&s, "p1").unwrap();
        let by_country: std::collections::HashMap<_, _> =
            countries.iter().map(|f| (f.key.as_str(), f.count)).collect();
        assert_eq!(by_country["BR"], 20);
        assert_eq!(by_country["GB"], 15);
        assert_eq!(by_country["US"], 5);
        // The label recorded by enrichment is preferred over the bare code.
        assert_eq!(
            countries.iter().find(|f| f.key == "BR").unwrap().label,
            "Brazil"
        );

        let decades = decade_facets(&s, "p1").unwrap();
        let by_decade: std::collections::HashMap<_, _> =
            decades.iter().map(|f| (f.key.as_str(), f.count)).collect();
        // Every track has a 2015 Spotify date, so these counts prove the facets
        // read track_era rather than the reissue date.
        assert_eq!(by_decade["1970"], 20);
        assert_eq!(by_decade["1990"], 15);
        assert_eq!(by_decade["2010"], 5);
        assert!(!by_decade.contains_key("2000"));
    }

    #[test]
    fn empty_playlist_yields_no_facets_and_no_suggestions() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_playlist(&Playlist {
            spotify_id: "empty".into(), name: "Empty".into(), owner: None,
            snapshot_id: None, track_count: Some(0), synced_at: None,
        }).unwrap();

        assert!(genre_facets(&s, "empty").unwrap().is_empty());
        assert!(country_facets(&s, "empty").unwrap().is_empty());
        assert!(decade_facets(&s, "empty").unwrap().is_empty());
        assert!(suggest(&s, "empty").unwrap().is_empty());
    }

    #[test]
    fn suggests_the_cross_section_the_user_actually_wants() {
        let s = seeded_store();
        let cards = suggest(&s, "p1").unwrap();
        assert!(!cards.is_empty());

        // "Soul · Brazil · 1970s" must be among the proposals.
        let card = cards
            .iter()
            .find(|c| {
                c.filter.genres == vec!["soul".to_string()]
                    && c.filter.normalized_countries() == vec!["BR".to_string()]
                    && c.filter.year_range == Some((1970, 1979))
            })
            .unwrap_or_else(|| {
                panic!(
                    "expected a Brazilian soul 1970s candidate; got: {:?}",
                    cards.iter().map(|c| &c.proposed_name).collect::<Vec<_>>()
                )
            });
        assert_eq!(card.track_count, 12);
        // The rolled-up parent genre is preferred over its child `funk-soul`,
        // which here covers exactly the same tracks.
        assert_eq!(card.proposed_name, "Soul · Brazil · 1970s");
        assert!(card.score.total > 0.0);
        // Every member must genuinely be from the 70s.
        assert!(card.tracks.iter().all(|t| t.reason.year == Some(1972)));
    }

    #[test]
    fn genre_depths_are_computed_from_the_hierarchy() {
        let s = seeded_store();
        let depths = genre_depths(&s).unwrap();
        assert_eq!(depths["soul"], 0);
        assert_eq!(depths["funk-soul"], 1);
        assert_eq!(depths["samba"], 0);
        assert_eq!(depths["samba-rock"], 1);
    }

    #[test]
    fn suggestion_order_is_stable_across_runs() {
        let s = seeded_store();
        let first: Vec<String> = suggest(&s, "p1").unwrap().into_iter().map(|c| c.id).collect();
        let second: Vec<String> = suggest(&s, "p1").unwrap().into_iter().map(|c| c.id).collect();
        assert_eq!(first, second);
    }

    #[test]
    fn suggestions_are_ranked_and_capped() {
        let s = seeded_store();
        let cards = suggest(&s, "p1").unwrap();

        assert!(cards.len() <= MAX_SUGGESTIONS);
        for pair in cards.windows(2) {
            assert!(
                pair[0].score.total >= pair[1].score.total,
                "cards are not in descending score order"
            );
        }
    }

    #[test]
    fn near_duplicate_candidates_are_suppressed() {
        let s = seeded_store();
        let cards = suggest(&s, "p1").unwrap();

        // "soul" and "funk-soul" cover exactly the same 12 tracks here, so only
        // one of them should survive the redundancy pass.
        let soul_like = cards
            .iter()
            .filter(|c| {
                c.filter.countries.is_empty()
                    && c.filter.year_range.is_none()
                    && (c.filter.genres == vec!["soul".to_string()]
                        || c.filter.genres == vec!["funk-soul".to_string()])
            })
            .count();
        assert!(soul_like <= 1, "both soul and funk-soul were proposed");
    }

    #[test]
    fn candidate_ids_are_stable_and_distinguish_filters() {
        let a = PlaylistFilter {
            genres: vec!["soul".into()], countries: vec!["BR".into()],
            year_range: Some((1970, 1979)), ..Default::default()
        };
        let b = PlaylistFilter { countries: vec!["br".into()], ..a.clone() };
        let c = PlaylistFilter { year_range: Some((1980, 1989)), ..a.clone() };

        assert_eq!(filter_id(&a), filter_id(&a));
        // Case differences in a country code are not a different playlist.
        assert_eq!(filter_id(&a), filter_id(&b));
        assert_ne!(filter_id(&a), filter_id(&c));
    }

    #[test]
    fn names_read_sensibly_for_each_shape() {
        let s = seeded_store();
        let genre_only = PlaylistFilter { genres: vec!["samba".into()], ..Default::default() };
        assert_eq!(name_for(&s, &genre_only, 20).unwrap().0, "Samba");

        let full = PlaylistFilter {
            genres: vec!["soul".into()], countries: vec!["BR".into()],
            year_range: Some((1970, 1979)), ..Default::default()
        };
        assert_eq!(name_for(&s, &full, 12).unwrap().0, "Soul · Brazil · 1970s");

        // A non-decade range is spelled out rather than mislabelled.
        let odd = PlaylistFilter { year_range: Some((1968, 1974)), ..Default::default() };
        assert_eq!(name_for(&s, &odd, 5).unwrap().0, "1968–1974");
    }

    #[test]
    fn enumeration_stays_bounded_with_many_facets() {
        // Guards the combinatorial blow-up: the axis caps must hold.
        let many: Vec<FacetValue> = (0..50)
            .map(|i| FacetValue { key: format!("g{i}"), label: format!("G{i}"), count: 10 })
            .collect();
        let countries: Vec<FacetValue> = (0..20)
            .map(|i| FacetValue { key: format!("C{i}"), label: format!("C{i}"), count: 10 })
            .collect();
        let decades: Vec<FacetValue> = (0..20)
            .map(|i| FacetValue { key: (1900 + i * 10).to_string(), label: "x".into(), count: 10 })
            .collect();

        let filters = enumerate_filters("p1", &many, &countries, &decades);
        // 12 + 8 + 8 + 96 + 96 + 64 + 768 = 1052 at the caps.
        assert!(filters.len() < 1200, "enumerated {} filters", filters.len());
        assert!(filters.iter().all(|f| f.source_playlist_id.as_deref() == Some("p1")));
    }
}
