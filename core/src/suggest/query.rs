//! Executing a [`PlaylistFilter`] against the local cache.
//!
//! This is the only place a filter is turned into rows, whichever entry point
//! produced it — automatic facet enumeration, the dropdowns, or the natural
//! language parser. Everything happens in SQL over already-cached data, so
//! running a query costs nothing and touches no network.

use super::filter::{GenreMode, PlaylistFilter};
use crate::error::Result;
use crate::store::Store;
use rusqlite::types::Value as SqlValue;
use serde::{Deserialize, Serialize};

/// Why one track ended up in a proposal. Shown in the review table so the user
/// can judge the suggestion instead of trusting it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TrackReason {
    /// The listed genre this track matched, which for `AnyWithChildren` may be a
    /// descendant of the requested one.
    pub genre: Option<String>,
    pub genre_score: f64,
    pub genre_source: Option<String>,
    pub country_code: Option<String>,
    pub year: Option<i32>,
    pub era_source: Option<String>,
    pub needs_review: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScoredTrack {
    pub spotify_id: String,
    pub name: String,
    pub artists: Vec<String>,
    pub reason: TrackReason,
}

/// A reviewable proposal: what would be created, and from what.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionCard {
    pub id: String,
    pub proposed_name: String,
    pub description: String,
    pub filter: PlaylistFilter,
    pub track_count: usize,
    pub score: super::score::CandidateScore,
    pub tracks: Vec<ScoredTrack>,
}

/// Run `filter` and return the matching tracks, best match first.
///
/// Validation happens here rather than at the call sites, so no path — including
/// one fed by an LLM — can execute an unvalidated filter.
pub fn execute(store: &Store, filter: &PlaylistFilter) -> Result<Vec<ScoredTrack>> {
    filter.validate(store)?;

    let conn = store.conn()?;
    let mut params: Vec<SqlValue> = Vec::new();
    let mut ctes: Vec<String> = Vec::new();
    let mut wheres: Vec<String> = Vec::new();

    // ------------------------------------------------------------ genre
    if !filter.genres.is_empty() {
        let placeholders = bind_list(&mut params, filter.genres.iter().cloned());

        // `UNION` (not `UNION ALL`) makes the recursion terminate even if the
        // vocabulary somehow contains a parent cycle.
        let tree = match filter.genre_mode {
            GenreMode::AnyWithChildren => format!(
                "RECURSIVE wanted_genre(slug) AS (
                     SELECT slug FROM genre_canonical WHERE slug IN ({placeholders})
                     UNION
                     SELECT gc.slug FROM genre_canonical gc
                     JOIN wanted_genre wg ON gc.parent_slug = wg.slug
                 )"
            ),
            _ => format!("wanted_genre(slug) AS (SELECT value FROM (SELECT NULL AS value) WHERE 0 UNION ALL SELECT slug FROM genre_canonical WHERE slug IN ({placeholders}))"),
        };
        ctes.push(tree);

        let min_score = filter.min_genre_score.unwrap_or(0.0);
        params.push(SqlValue::Real(min_score));
        let score_param = params.len();

        match filter.genre_mode {
            GenreMode::All => {
                // Every requested genre must be present. Counting distinct
                // matches against the requested count is how "all" is enforced
                // without one join per genre.
                params.push(SqlValue::Integer(filter.genres.len() as i64));
                let count_param = params.len();
                wheres.push(format!(
                    "(SELECT COUNT(DISTINCT tg.canonical_slug) FROM track_genre tg
                      WHERE tg.track_spotify_id = t.spotify_id
                        AND tg.canonical_slug IN (SELECT slug FROM wanted_genre)
                        AND tg.score >= ?{score_param}) = ?{count_param}"
                ));
            }
            _ => {
                wheres.push(format!(
                    "EXISTS (SELECT 1 FROM track_genre tg
                             WHERE tg.track_spotify_id = t.spotify_id
                               AND tg.canonical_slug IN (SELECT slug FROM wanted_genre)
                               AND tg.score >= ?{score_param})"
                ));
            }
        }
    }

    // ------------------------------------------------------------ origin
    if !filter.countries.is_empty() {
        // A track matches when *any* credited artist has that origin. Requiring
        // the primary artist would drop collaborations that plainly belong.
        let placeholders = bind_list(&mut params, filter.normalized_countries());
        wheres.push(format!(
            "EXISTS (SELECT 1 FROM track_artist ta
                     JOIN artist_origin ao ON ao.artist_spotify_id = ta.artist_spotify_id
                     WHERE ta.track_spotify_id = t.spotify_id
                       AND UPPER(ao.country_code) IN ({placeholders}))"
        ));
    }

    // ------------------------------------------------------------ era
    if let Some((from, to)) = filter.year_range {
        params.push(SqlValue::Integer(from as i64));
        let from_param = params.len();
        params.push(SqlValue::Integer(to as i64));
        let to_param = params.len();
        // Requires a resolved era: a track of unknown year must not be silently
        // swept into a decade playlist.
        wheres.push(format!(
            "EXISTS (SELECT 1 FROM track_era te
                     WHERE te.track_spotify_id = t.spotify_id
                       AND te.year IS NOT NULL
                       AND te.year BETWEEN ?{from_param} AND ?{to_param})"
        ));
    }

    // ------------------------------------------------------------ scope
    let from_clause = if let Some(ref playlist_id) = filter.source_playlist_id {
        params.push(SqlValue::Text(playlist_id.clone()));
        let p = params.len();
        format!(
            "FROM track t
             JOIN playlist_track pt ON pt.track_spotify_id = t.spotify_id AND pt.playlist_id = ?{p}"
        )
    } else {
        "FROM track t".to_string()
    };

    if filter.exclude_needs_review {
        wheres.push(
            "NOT EXISTS (SELECT 1 FROM needs_review nr
                         WHERE nr.entity_id = t.spotify_id
                           AND nr.entity_type = 'track'
                           AND nr.resolved_at IS NULL)"
                .to_string(),
        );
    }

    // Local files cannot be added to a Spotify playlist — they have no URI.
    wheres.push("t.is_local = 0".to_string());

    // ------------------------------------------------------------ assemble
    let with_clause = if ctes.is_empty() {
        String::new()
    } else {
        format!("WITH {}", ctes.join(", "))
    };
    let where_clause = if wheres.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", wheres.join(" AND "))
    };

    // The representative genre reported per track is its best-scoring match
    // among the requested ones, falling back to its top genre overall.
    let genre_pick = if filter.genres.is_empty() {
        "(SELECT tg.canonical_slug FROM track_genre tg
           WHERE tg.track_spotify_id = t.spotify_id
           ORDER BY tg.score DESC LIMIT 1)"
    } else {
        "(SELECT tg.canonical_slug FROM track_genre tg
           WHERE tg.track_spotify_id = t.spotify_id
             AND tg.canonical_slug IN (SELECT slug FROM wanted_genre)
           ORDER BY tg.score DESC LIMIT 1)"
    };
    let score_pick = if filter.genres.is_empty() {
        "(SELECT MAX(tg.score) FROM track_genre tg WHERE tg.track_spotify_id = t.spotify_id)"
    } else {
        "(SELECT MAX(tg.score) FROM track_genre tg
           WHERE tg.track_spotify_id = t.spotify_id
             AND tg.canonical_slug IN (SELECT slug FROM wanted_genre))"
    };

    let sql = format!(
        "{with_clause}
         SELECT t.spotify_id,
                t.name,
                {genre_pick} AS match_genre,
                COALESCE({score_pick}, 0.0) AS match_score,
                (SELECT ao.country_code FROM track_artist ta
                   JOIN artist_origin ao ON ao.artist_spotify_id = ta.artist_spotify_id
                   WHERE ta.track_spotify_id = t.spotify_id
                   ORDER BY ao.confidence DESC, ta.position LIMIT 1) AS country_code,
                (SELECT te.year FROM track_era te WHERE te.track_spotify_id = t.spotify_id) AS year,
                (SELECT te.source FROM track_era te WHERE te.track_spotify_id = t.spotify_id) AS era_source,
                EXISTS (SELECT 1 FROM needs_review nr
                        WHERE nr.entity_id = t.spotify_id AND nr.entity_type = 'track'
                          AND nr.resolved_at IS NULL) AS needs_review
         {from_clause}
         {where_clause}
         ORDER BY match_score DESC, t.name COLLATE NOCASE"
    );

    type Row = (String, String, Option<String>, f64, Option<String>, Option<i32>, Option<String>, bool);
    // Scoped so the connection returns to the pool before the per-track lookups
    // below ask for one. Holding it across those calls would deadlock.
    let rows: Vec<Row> = {
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get::<_, i64>(7)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<Row>>>()?;
        rows
    };
    drop(conn);

    // Genre provenance comes from the strongest raw signal behind the derived
    // genre, so the UI can say "MusicBrainz" rather than just asserting a genre.
    let mut out = Vec::with_capacity(rows.len());
    for (spotify_id, name, genre, genre_score, country_code, year, era_source, needs_review) in rows
    {
        let artists = store
            .track_artists(&spotify_id)?
            .into_iter()
            .map(|a| a.name)
            .collect();
        let genre_source = match genre.as_deref() {
            Some(slug) => strongest_source_for(store, &spotify_id, slug)?,
            None => None,
        };
        out.push(ScoredTrack {
            spotify_id,
            name,
            artists,
            reason: TrackReason {
                genre,
                genre_score,
                genre_source,
                country_code,
                year,
                era_source,
                needs_review,
            },
        });
    }

    // Truncation happens after ordering, so a capped playlist keeps its best
    // tracks rather than an arbitrary slice.
    if let Some(max) = filter.max_tracks {
        out.truncate(max);
    }
    Ok(out)
}

/// The highest-weighted source that contributed a tag mapping to `slug`, for the
/// track or any of its artists.
fn strongest_source_for(store: &Store, track_id: &str, slug: &str) -> Result<Option<String>> {
    let conn = store.conn()?;
    Ok(conn
        .query_row(
            "SELECT ts.source
             FROM tag_signal ts
             JOIN genre_alias ga ON ga.raw_tag = ts.raw_tag
             WHERE ga.canonical_slug = ?2
               AND (
                 (ts.entity_type = 'mb_recording' AND ts.entity_id = (
                     SELECT recording_mbid FROM track_mb WHERE track_spotify_id = ?1))
                 OR (ts.entity_type = 'mb_artist' AND ts.entity_id IN (
                     SELECT am.artist_mbid FROM track_artist ta
                     JOIN artist_mb am ON am.artist_spotify_id = ta.artist_spotify_id
                     WHERE ta.track_spotify_id = ?1))
                 OR (ts.entity_type = 'spotify_artist' AND ts.entity_id IN (
                     SELECT ta.artist_spotify_id FROM track_artist ta
                     WHERE ta.track_spotify_id = ?1))
               )
             ORDER BY ts.weight DESC
             LIMIT 1",
            rusqlite::params![track_id, slug],
            |r| r.get::<_, String>(0),
        )
        .ok())
}

/// Push `values` as bound parameters and return the `?N, ?N+1, …` placeholder
/// list. Binding rather than interpolating keeps user- and LLM-supplied strings
/// out of the SQL text.
fn bind_list(params: &mut Vec<SqlValue>, values: impl IntoIterator<Item = String>) -> String {
    let mut placeholders = Vec::new();
    for v in values {
        params.push(SqlValue::Text(v));
        placeholders.push(format!("?{}", params.len()));
    }
    placeholders.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    /// A small world: Brazilian soul from the 70s, English rock from the 90s,
    /// and one US hip-hop track from 2015.
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
        ])
        .unwrap();

        s.upsert_playlist(&Playlist {
            spotify_id: "p1".into(), name: "Mix".into(), owner: Some("me".into()),
            snapshot_id: None, track_count: Some(4), synced_at: None,
        }).unwrap();

        let rows: &[(&str, &str, &str, &str, &str, f64, &str, i32)] = &[
            // track, name, artist, artist name, genre, score, country, year
            ("t1", "Azul da Cor do Mar", "a1", "Tim Maia",   "funk-soul",  0.9, "BR", 1972),
            ("t2", "Que Beleza",         "a2", "Tim Bernardes","samba-rock", 0.8, "BR", 1974),
            ("t3", "Bittersweet",        "a3", "The Verve",  "britpop",    0.85, "GB", 1997),
            ("t4", "Alright",            "a4", "Kendrick",   "hip-hop",    0.95, "US", 2015),
        ];

        let mut entries = Vec::new();
        for (i, (tid, tname, aid, aname, genre, score, country, year)) in rows.iter().enumerate() {
            s.upsert_artist(&Artist { spotify_id: (*aid).into(), name: (*aname).into() }).unwrap();
            s.upsert_track(&Track {
                spotify_id: (*tid).into(),
                name: (*tname).into(),
                isrc: Some(format!("ISRC{i}")),
                duration_ms: Some(200_000),
                spotify_album_id: Some("al".into()),
                // Deliberately a reissue date, to prove era comes from track_era.
                spotify_release_date: Some("2015-01-01".into()),
                is_local: false,
            }).unwrap();
            s.link_track_artist(tid, aid, 0).unwrap();
            s.replace_track_genres(tid, &[TrackGenre {
                canonical_slug: (*genre).into(), score: *score,
            }]).unwrap();
            s.upsert_artist_origin(&ArtistOrigin {
                artist_spotify_id: (*aid).into(),
                country_code: Some((*country).into()),
                country_label: None, city: None,
                source: OriginSource::MbBeginArea, confidence: 1.0,
            }).unwrap();
            s.upsert_track_era(&TrackEra {
                track_spotify_id: (*tid).into(),
                year: Some(*year), decade: Some(crate::util::decade_of(*year)),
                source: EraSource::MbFirstRelease,
            }).unwrap();
            entries.push(((*tid).to_string(), i as i64, None));
        }
        s.replace_playlist_tracks("p1", &entries).unwrap();
        s
    }

    fn ids(tracks: &[ScoredTrack]) -> Vec<&str> {
        tracks.iter().map(|t| t.spotify_id.as_str()).collect()
    }

    #[test]
    fn filters_by_exact_genre() {
        let s = seeded_store();
        let f = PlaylistFilter {
            genres: vec!["hip-hop".into()],
            genre_mode: GenreMode::Any,
            ..Default::default()
        };
        assert_eq!(ids(&execute(&s, &f).unwrap()), vec!["t4"]);
    }

    #[test]
    fn exact_mode_does_not_match_descendants() {
        // Asking for "soul" in Any mode must not pull in funk-soul.
        let s = seeded_store();
        let f = PlaylistFilter {
            genres: vec!["soul".into()],
            genre_mode: GenreMode::Any,
            ..Default::default()
        };
        assert!(execute(&s, &f).unwrap().is_empty());
    }

    #[test]
    fn rollup_mode_collects_descendants() {
        // This is what stops the app producing forty playlists of three tracks:
        // "soul" must gather funk-soul, "samba" must gather samba-rock.
        let s = seeded_store();
        let soul = PlaylistFilter {
            genres: vec!["soul".into()],
            genre_mode: GenreMode::AnyWithChildren,
            ..Default::default()
        };
        assert_eq!(ids(&execute(&s, &soul).unwrap()), vec!["t1"]);

        let samba = PlaylistFilter {
            genres: vec!["samba".into()],
            genre_mode: GenreMode::AnyWithChildren,
            ..Default::default()
        };
        assert_eq!(ids(&execute(&s, &samba).unwrap()), vec!["t2"]);
        // The reported genre is the descendant actually matched, not the request.
        assert_eq!(
            execute(&s, &samba).unwrap()[0].reason.genre.as_deref(),
            Some("samba-rock")
        );
    }

    #[test]
    fn combines_genre_country_and_era() {
        // The headline query: "Brazilian soul from the 70s".
        let s = seeded_store();
        let f = PlaylistFilter {
            genres: vec!["soul".into(), "samba".into()],
            genre_mode: GenreMode::AnyWithChildren,
            countries: vec!["BR".into()],
            year_range: Some((1970, 1979)),
            ..Default::default()
        };
        let got = execute(&s, &f).unwrap();
        assert_eq!(got.len(), 2);
        assert!(ids(&got).contains(&"t1"));
        assert!(ids(&got).contains(&"t2"));
    }

    #[test]
    fn era_uses_derived_year_not_the_reissue_date() {
        // Every seeded track has a 2015 Spotify release date; only t4 is really
        // from 2015. A 2010s filter must return t4 alone.
        let s = seeded_store();
        let f = PlaylistFilter {
            year_range: Some((2010, 2019)),
            ..Default::default()
        };
        assert_eq!(ids(&execute(&s, &f).unwrap()), vec!["t4"]);
    }

    #[test]
    fn lowercase_country_codes_are_accepted() {
        let s = seeded_store();
        let f = PlaylistFilter { countries: vec!["br".into()], ..Default::default() };
        assert_eq!(execute(&s, &f).unwrap().len(), 2);
    }

    #[test]
    fn all_mode_requires_every_genre() {
        let s = seeded_store();
        // t1 has funk-soul only, so demanding both must yield nothing.
        let both = PlaylistFilter {
            genres: vec!["funk-soul".into(), "hip-hop".into()],
            genre_mode: GenreMode::All,
            ..Default::default()
        };
        assert!(execute(&s, &both).unwrap().is_empty());

        // Now give t1 a second genre and it should qualify.
        s.replace_track_genres("t1", &[
            TrackGenre { canonical_slug: "funk-soul".into(), score: 0.9 },
            TrackGenre { canonical_slug: "hip-hop".into(), score: 0.4 },
        ]).unwrap();
        assert_eq!(ids(&execute(&s, &both).unwrap()), vec!["t1"]);
    }

    #[test]
    fn min_genre_score_excludes_weak_matches() {
        let s = seeded_store();
        s.replace_track_genres("t1", &[TrackGenre {
            canonical_slug: "funk-soul".into(), score: 0.2,
        }]).unwrap();

        let strict = PlaylistFilter {
            genres: vec!["soul".into()],
            genre_mode: GenreMode::AnyWithChildren,
            min_genre_score: Some(0.5),
            ..Default::default()
        };
        assert!(execute(&s, &strict).unwrap().is_empty());

        let lenient = PlaylistFilter { min_genre_score: Some(0.1), ..strict.clone() };
        assert_eq!(ids(&execute(&s, &lenient).unwrap()), vec!["t1"]);
    }

    #[test]
    fn tracks_of_unknown_year_are_never_swept_into_a_decade() {
        let s = seeded_store();
        s.upsert_track(&Track {
            spotify_id: "t5".into(), name: "Unknown Age".into(), isrc: None,
            duration_ms: None, spotify_album_id: None,
            spotify_release_date: Some("1975-01-01".into()), is_local: false,
        }).unwrap();
        s.replace_track_genres("t5", &[TrackGenre {
            canonical_slug: "samba".into(), score: 0.9,
        }]).unwrap();
        // No track_era row at all: the year is genuinely unknown.

        let f = PlaylistFilter { year_range: Some((1970, 1979)), ..Default::default() };
        assert!(!ids(&execute(&s, &f).unwrap()).contains(&"t5"));
    }

    #[test]
    fn excludes_needs_review_when_asked() {
        let s = seeded_store();
        s.flag_needs_review("track", "t1", "low_confidence_match", None).unwrap();

        let lenient = PlaylistFilter {
            genres: vec!["soul".into()],
            genre_mode: GenreMode::AnyWithChildren,
            ..Default::default()
        };
        assert_eq!(ids(&execute(&s, &lenient).unwrap()), vec!["t1"]);
        assert!(execute(&s, &lenient).unwrap()[0].reason.needs_review);

        let strict = PlaylistFilter { exclude_needs_review: true, ..lenient };
        assert!(execute(&s, &strict).unwrap().is_empty());
    }

    #[test]
    fn never_proposes_local_files() {
        // They have no Spotify URI, so they cannot be added to a new playlist.
        let s = seeded_store();
        s.upsert_track(&Track {
            spotify_id: "local1".into(), name: "My Rip".into(), isrc: None,
            duration_ms: None, spotify_album_id: None, spotify_release_date: None,
            is_local: true,
        }).unwrap();
        s.replace_track_genres("local1", &[TrackGenre {
            canonical_slug: "hip-hop".into(), score: 0.99,
        }]).unwrap();

        let f = PlaylistFilter { genres: vec!["hip-hop".into()], ..Default::default() };
        assert_eq!(ids(&execute(&s, &f).unwrap()), vec!["t4"]);
    }

    #[test]
    fn scopes_to_a_source_playlist() {
        let s = seeded_store();
        // A track in the library but not in p1.
        s.upsert_track(&Track {
            spotify_id: "outsider".into(), name: "Elsewhere".into(), isrc: None,
            duration_ms: None, spotify_album_id: None, spotify_release_date: None,
            is_local: false,
        }).unwrap();
        s.replace_track_genres("outsider", &[TrackGenre {
            canonical_slug: "hip-hop".into(), score: 0.99,
        }]).unwrap();

        let scoped = PlaylistFilter {
            genres: vec!["hip-hop".into()],
            source_playlist_id: Some("p1".into()),
            ..Default::default()
        };
        assert_eq!(ids(&execute(&s, &scoped).unwrap()), vec!["t4"]);

        let global = PlaylistFilter { source_playlist_id: None, ..scoped };
        assert_eq!(execute(&s, &global).unwrap().len(), 2);
    }

    #[test]
    fn orders_by_score_and_truncates_keeping_the_best() {
        let s = seeded_store();
        s.replace_track_genres("t1", &[TrackGenre { canonical_slug: "hip-hop".into(), score: 0.10 }]).unwrap();
        s.replace_track_genres("t2", &[TrackGenre { canonical_slug: "hip-hop".into(), score: 0.50 }]).unwrap();
        s.replace_track_genres("t3", &[TrackGenre { canonical_slug: "hip-hop".into(), score: 0.90 }]).unwrap();

        let f = PlaylistFilter {
            genres: vec!["hip-hop".into()],
            max_tracks: Some(2),
            ..Default::default()
        };
        let got = execute(&s, &f).unwrap();
        // t4 (0.95) and t3 (0.90) beat the rest.
        assert_eq!(ids(&got), vec!["t4", "t3"]);
    }

    #[test]
    fn reports_provenance_for_the_matched_genre() {
        let s = seeded_store();
        s.upsert_mb_artist(&MbArtist { mbid: "mb-a1".into(), ..Default::default() }).unwrap();
        s.link_artist_mb("a1", "mb-a1", 1.0, "url-rel").unwrap();
        s.upsert_genre_alias("funk soul", Some("funk-soul"), "dict").unwrap();
        s.insert_tag_signal(&TagSignal {
            entity_type: EntityType::MbArtist,
            entity_id: "mb-a1".into(),
            source: Source::MusicBrainz,
            raw_tag: "funk soul".into(),
            weight: 1.0,
            kind: Some(TagKind::Genre),
            fetched_at: String::new(),
        }).unwrap();

        let f = PlaylistFilter {
            genres: vec!["funk-soul".into()],
            ..Default::default()
        };
        let got = execute(&s, &f).unwrap();
        assert_eq!(got[0].reason.genre_source.as_deref(), Some("musicbrainz"));
        assert_eq!(got[0].reason.country_code.as_deref(), Some("BR"));
        assert_eq!(got[0].reason.year, Some(1972));
        assert_eq!(got[0].reason.era_source.as_deref(), Some("mb_first_release"));
    }

    #[test]
    fn refuses_to_execute_an_invalid_filter() {
        // No path may run an unvalidated filter, including an LLM-produced one.
        let s = seeded_store();
        let bogus = PlaylistFilter {
            genres: vec!["cosmic-brazilian-soul".into()],
            ..Default::default()
        };
        assert!(matches!(
            execute(&s, &bogus),
            Err(crate::error::CoreError::InvalidFilter(_))
        ));
    }

    #[test]
    fn artist_credited_on_a_collaboration_still_matches_its_country() {
        let s = seeded_store();
        // A GB artist guests on a BR track; the BR filter must still find it.
        s.upsert_artist(&Artist { spotify_id: "guest".into(), name: "Guest".into() }).unwrap();
        s.link_track_artist("t3", "guest", 1).unwrap();
        s.upsert_artist_origin(&ArtistOrigin {
            artist_spotify_id: "guest".into(),
            country_code: Some("BR".into()), country_label: None, city: None,
            source: OriginSource::MbCountry, confidence: 0.8,
        }).unwrap();

        let f = PlaylistFilter { countries: vec!["BR".into()], ..Default::default() };
        assert!(ids(&execute(&s, &f).unwrap()).contains(&"t3"));
    }
}
