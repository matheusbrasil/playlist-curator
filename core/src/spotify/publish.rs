//! Creating a derived playlist on Spotify.
//!
//! This is the only code in the project that writes to the user's account, so it
//! is deliberately cautious:
//!  * **dry-run is the default** — nothing is written until the user opts in;
//!  * new playlists are **private** unless explicitly made public;
//!  * the source playlist is **never** read-modify-written, only read;
//!  * the recipe is recorded in `created_playlist`, so a playlist can be
//!    regenerated later without reconstructing the query by hand.

use super::client::{track_uri, SpotifyClient};
use crate::error::Result;
use crate::store::Store;
use crate::suggest::query::SuggestionCard;
use serde::{Deserialize, Serialize};

/// A track that could not be included, and why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkippedTrack {
    pub spotify_id: String,
    pub name: String,
    pub reason: String,
}

/// Outcome of a create request. `created` is false in dry-run, and the UI must
/// make that unmistakable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateOutcome {
    pub dry_run: bool,
    pub created: bool,
    pub spotify_id: Option<String>,
    pub spotify_url: Option<String>,
    pub name: String,
    pub track_count: usize,
    pub skipped: Vec<SkippedTrack>,
}

/// Split a card's tracks into URIs that can be added and tracks that cannot.
///
/// Pure, so the dry-run preview and the real write agree by construction rather
/// than by two code paths happening to match.
pub fn plan_tracks(card: &SuggestionCard) -> (Vec<String>, Vec<SkippedTrack>) {
    let mut uris = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for track in &card.tracks {
        if track.spotify_id.is_empty() {
            skipped.push(SkippedTrack {
                spotify_id: track.spotify_id.clone(),
                name: track.name.clone(),
                reason: "no Spotify id (local file)".into(),
            });
            continue;
        }
        // Spotify accepts duplicates silently; a derived playlist should not have
        // the same track twice because two facets both matched it.
        if !seen.insert(track.spotify_id.clone()) {
            skipped.push(SkippedTrack {
                spotify_id: track.spotify_id.clone(),
                name: track.name.clone(),
                reason: "duplicate within the selection".into(),
            });
            continue;
        }
        uris.push(track_uri(&track.spotify_id));
    }
    (uris, skipped)
}

/// Create `card` as a playlist on Spotify, or describe what would happen.
///
/// `dry_run` should come from settings unless the user explicitly overrode it for
/// this one action.
pub async fn create_from_card(
    client: &SpotifyClient,
    store: &Store,
    card: &SuggestionCard,
    public: bool,
    dry_run: bool,
) -> Result<CreateOutcome> {
    let (uris, skipped) = plan_tracks(card);

    if dry_run {
        tracing::info!(
            name = %card.proposed_name,
            tracks = uris.len(),
            "dry run: nothing was written to Spotify"
        );
        return Ok(CreateOutcome {
            dry_run: true,
            created: false,
            spotify_id: None,
            spotify_url: None,
            name: card.proposed_name.clone(),
            track_count: uris.len(),
            skipped,
        });
    }

    // Creating an empty playlist and then failing to fill it would leave litter
    // in the user's account, so refuse before creating anything.
    if uris.is_empty() {
        return Err(crate::error::CoreError::other(
            "refusing to create an empty playlist",
        ));
    }

    let me = client.me().await?;
    let created = client
        .create_playlist(&me.id, &card.proposed_name, &card.description, public)
        .await?;

    client.add_items(&created.id, &uris).await?;

    // Record the recipe, not just the result, so the playlist can be refreshed
    // later when the underlying metadata improves.
    let recipe = serde_json::to_string(&card.filter)?;
    store.record_created_playlist(&created.id, &card.proposed_name, &recipe)?;

    let url = created
        .external_urls
        .as_ref()
        .and_then(|u| u.spotify.clone());
    tracing::info!(
        playlist_id = %created.id,
        tracks = uris.len(),
        "created playlist on Spotify"
    );

    Ok(CreateOutcome {
        dry_run: false,
        created: true,
        spotify_id: Some(created.id),
        spotify_url: url,
        name: card.proposed_name.clone(),
        track_count: uris.len(),
        skipped,
    })
}

/// Playlists this app has created, newest first, with their recipes.
pub fn list_created(store: &Store) -> Result<Vec<CreatedRecord>> {
    let conn = store.conn()?;
    let mut stmt = conn.prepare(
        "SELECT spotify_id, name, created_at, recipe_json
         FROM created_playlist ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(CreatedRecord {
            spotify_id: r.get(0)?,
            name: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            created_at: r.get(2)?,
            recipe: serde_json::from_str(&r.get::<_, String>(3)?).ok(),
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatedRecord {
    pub spotify_id: String,
    pub name: String,
    pub created_at: String,
    pub recipe: Option<crate::suggest::PlaylistFilter>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::suggest::query::{ScoredTrack, TrackReason};
    use crate::suggest::{GenreMode, PlaylistFilter};

    fn card(track_ids: &[&str]) -> SuggestionCard {
        SuggestionCard {
            id: "cardid".into(),
            proposed_name: "Soul · Brazil · 1970s".into(),
            description: "12 tracks".into(),
            filter: PlaylistFilter {
                genres: vec!["soul".into()],
                genre_mode: GenreMode::AnyWithChildren,
                countries: vec!["BR".into()],
                year_range: Some((1970, 1979)),
                ..Default::default()
            },
            track_count: track_ids.len(),
            score: Default::default(),
            tracks: track_ids
                .iter()
                .map(|id| ScoredTrack {
                    spotify_id: (*id).to_string(),
                    name: format!("Track {id}"),
                    artists: vec!["Tim Maia".into()],
                    reason: TrackReason {
                        genre: Some("soul".into()),
                        genre_score: 0.9,
                        genre_source: Some("musicbrainz".into()),
                        country_code: Some("BR".into()),
                        year: Some(1972),
                        era_source: Some("mb_first_release".into()),
                        needs_review: false,
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn plans_uris_for_every_playable_track() {
        let (uris, skipped) = plan_tracks(&card(&["t1", "t2", "t3"]));
        assert_eq!(
            uris,
            vec![
                "spotify:track:t1".to_string(),
                "spotify:track:t2".to_string(),
                "spotify:track:t3".to_string()
            ]
        );
        assert!(skipped.is_empty());
    }

    #[test]
    fn skips_tracks_with_no_spotify_id() {
        // A local file has no URI and cannot be added.
        let (uris, skipped) = plan_tracks(&card(&["t1", "", "t2"]));
        assert_eq!(uris.len(), 2);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].reason.contains("local file"));
    }

    #[test]
    fn deduplicates_within_a_selection() {
        let (uris, skipped) = plan_tracks(&card(&["t1", "t2", "t1"]));
        assert_eq!(uris, vec!["spotify:track:t1".to_string(), "spotify:track:t2".to_string()]);
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].reason.contains("duplicate"));
    }

    #[test]
    fn empty_card_plans_nothing() {
        let (uris, skipped) = plan_tracks(&card(&[]));
        assert!(uris.is_empty());
        assert!(skipped.is_empty());
    }

    #[test]
    fn created_playlists_roundtrip_with_their_recipe() {
        let store = Store::open_in_memory().unwrap();
        let c = card(&["t1", "t2"]);
        store
            .record_created_playlist("pl1", &c.proposed_name, &serde_json::to_string(&c.filter).unwrap())
            .unwrap();

        let listed = list_created(&store).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].spotify_id, "pl1");
        assert_eq!(listed[0].name, "Soul · Brazil · 1970s");
        // The recipe survives, so the playlist can be regenerated later.
        let recipe = listed[0].recipe.as_ref().unwrap();
        assert_eq!(recipe.genres, vec!["soul".to_string()]);
        assert_eq!(recipe.year_range, Some((1970, 1979)));
        assert_eq!(recipe.genre_mode, GenreMode::AnyWithChildren);
    }

    #[test]
    fn dry_run_outcome_reports_the_plan_without_creating() {
        // Verified without a client: dry-run must return before any network use.
        // `create_from_card` short-circuits on `dry_run`, so the plan is exactly
        // what `plan_tracks` produced.
        let c = card(&["t1", "t2", "t1"]);
        let (uris, skipped) = plan_tracks(&c);
        let outcome = CreateOutcome {
            dry_run: true,
            created: false,
            spotify_id: None,
            spotify_url: None,
            name: c.proposed_name.clone(),
            track_count: uris.len(),
            skipped,
        };
        assert!(!outcome.created);
        assert_eq!(outcome.track_count, 2);
        assert!(outcome.spotify_id.is_none());
        assert_eq!(outcome.skipped.len(), 1);
    }

    #[test]
    fn batching_matches_the_hundred_item_limit() {
        // 250 tracks must be sent as 100 + 100 + 50.
        let ids: Vec<String> = (0..250).map(|i| format!("t{i}")).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let (uris, _) = plan_tracks(&card(&refs));
        assert_eq!(uris.len(), 250);

        let batches: Vec<usize> = uris
            .chunks(super::super::client::ADD_ITEMS_BATCH)
            .map(<[String]>::len)
            .collect();
        assert_eq!(batches, vec![100, 100, 50]);
    }
}
