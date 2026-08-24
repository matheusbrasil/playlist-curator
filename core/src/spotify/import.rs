//! Import a Spotify playlist into the local store.
//!
//! This is the point where the app stops depending on Spotify being available.
//! Once a playlist is imported, every later phase reads from SQLite.

use super::client::SpotifyClient;
use super::models::{PlaylistItem, SimplePlaylist};
use crate::error::Result;
use crate::model::{Artist, Playlist, Track};
use crate::store::Store;
use serde::{Deserialize, Serialize};

/// What an import actually managed to record. `with_isrc` is the number that
/// matters: the ISRC is the deterministic join key into MusicBrainz, so a poor
/// ratio here means the enrichment cascade must lean on artist-level matching.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportStats {
    pub items_seen: usize,
    pub tracks_imported: usize,
    pub with_isrc: usize,
    pub artists_imported: usize,
    /// Local files: no Spotify id, so nothing can be resolved for them.
    pub skipped_local: usize,
    /// Podcast episodes mixed into the playlist.
    pub skipped_episodes: usize,
    /// Entries whose `track` came back null.
    pub skipped_unresolvable: usize,
}

impl ImportStats {
    /// Fraction of imported tracks carrying an ISRC, 0.0–1.0.
    pub fn isrc_ratio(&self) -> f64 {
        if self.tracks_imported == 0 {
            return 0.0;
        }
        self.with_isrc as f64 / self.tracks_imported as f64
    }
}

/// Fetch a playlist and its items, and persist them.
pub async fn import_playlist(
    client: &SpotifyClient,
    store: &Store,
    playlist_id: &str,
) -> Result<ImportStats> {
    let job = store.job_start("import_playlist", Some(playlist_id))?;

    let meta = client.playlist(playlist_id).await?;
    let items = client.playlist_items(playlist_id).await?;
    let stats = persist_playlist(store, playlist_id, &meta, &items)?;

    store.job_finish(job, &serde_json::to_string(&stats)?)?;

    if stats.tracks_imported > 0 && stats.with_isrc == 0 {
        // Worth shouting about: it means the API surface stopped returning
        // `external_ids`, and the pipeline must fall back to artist URL
        // relationships and name search.
        tracing::warn!(
            playlist_id,
            tracks = stats.tracks_imported,
            "no track carried an ISRC — MusicBrainz matching will rely on artist URL relationships"
        );
    } else {
        tracing::info!(
            playlist_id,
            tracks = stats.tracks_imported,
            isrc_ratio = format!("{:.0}%", stats.isrc_ratio() * 100.0),
            "playlist imported"
        );
    }
    Ok(stats)
}

/// The pure half of the import: given already-fetched API objects, write them to
/// the store. Separated from the network so it is testable without Spotify.
pub fn persist_playlist(
    store: &Store,
    playlist_id: &str,
    meta: &SimplePlaylist,
    items: &[PlaylistItem],
) -> Result<ImportStats> {
    let mut stats = ImportStats {
        items_seen: items.len(),
        ..Default::default()
    };

    store.upsert_playlist(&Playlist {
        spotify_id: playlist_id.to_string(),
        name: meta.name.clone(),
        owner: meta.owner.as_ref().map(|o| o.id.clone()),
        snapshot_id: meta.snapshot_id.clone(),
        track_count: meta.tracks.as_ref().and_then(|t| t.total),
        synced_at: None,
    })?;

    let mut entries: Vec<(String, i64, Option<String>)> = Vec::with_capacity(items.len());
    let mut seen_artists = std::collections::HashSet::new();
    // Position reflects the playlist order of the entries we actually keep, so
    // it stays contiguous even when episodes or local files are skipped.
    let mut position: i64 = 0;

    for item in items {
        let Some(track) = item.track.as_ref() else {
            stats.skipped_unresolvable += 1;
            continue;
        };
        if track.is_episode() {
            stats.skipped_episodes += 1;
            continue;
        }
        // Local files have no Spotify id and cannot be matched to anything, nor
        // added to a new playlist by URI.
        let is_local = item.is_local.unwrap_or(false) || track.is_local.unwrap_or(false);
        let Some(track_id) = track.id.as_deref().filter(|s| !s.is_empty()) else {
            stats.skipped_local += 1;
            continue;
        };

        let isrc = track.isrc().map(str::to_string);
        if isrc.is_some() {
            stats.with_isrc += 1;
        }

        store.upsert_track(&Track {
            spotify_id: track_id.to_string(),
            name: track.name.clone().unwrap_or_default(),
            isrc,
            duration_ms: track.duration_ms,
            spotify_album_id: track.album.as_ref().and_then(|a| a.id.clone()),
            spotify_release_date: track.album.as_ref().and_then(|a| a.release_date.clone()),
            is_local,
        })?;
        stats.tracks_imported += 1;

        for (idx, artist) in track.artists.iter().enumerate() {
            let Some(artist_id) = artist.id.as_deref().filter(|s| !s.is_empty()) else {
                continue;
            };
            store.upsert_artist(&Artist {
                spotify_id: artist_id.to_string(),
                name: artist.name.clone().unwrap_or_default(),
            })?;
            store.link_track_artist(track_id, artist_id, idx as i64)?;
            if seen_artists.insert(artist_id.to_string()) {
                stats.artists_imported += 1;
            }
        }

        entries.push((track_id.to_string(), position, item.added_at.clone()));
        position += 1;
    }

    store.replace_playlist_tracks(playlist_id, &entries)?;
    store.mark_playlist_synced(playlist_id)?;
    Ok(stats)
}

/// Persist the user's playlist list so the Playlists screen works offline.
pub async fn sync_playlist_list(client: &SpotifyClient, store: &Store) -> Result<Vec<Playlist>> {
    let remote = client.my_playlists().await?;
    for p in &remote {
        store.upsert_playlist(&Playlist {
            spotify_id: p.id.clone(),
            name: p.name.clone(),
            owner: p.owner.as_ref().map(|o| o.id.clone()),
            snapshot_id: p.snapshot_id.clone(),
            track_count: p.tracks.as_ref().and_then(|t| t.total),
            // Listing a playlist is not importing it; leave `synced_at` alone so
            // the UI can still distinguish analysed from unanalysed.
            synced_at: None,
        })?;
    }
    store.list_playlists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn meta() -> SimplePlaylist {
        serde_json::from_str(
            r#"{"id":"p1","name":"Mixtape","owner":{"id":"me"},
                "snapshot_id":"snap1","tracks":{"total":3}}"#,
        )
        .unwrap()
    }

    fn items(json: &str) -> Vec<PlaylistItem> {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn imports_tracks_artists_and_positions() {
        let s = store();
        let its = items(
            r#"[
              {"added_at":"2021-01-01T00:00:00Z","track":{
                 "id":"t1","name":"Azul da Cor do Mar","duration_ms":224000,"type":"track",
                 "album":{"id":"al1","release_date":"2015-06-01"},
                 "artists":[{"id":"a1","name":"Tim Maia"}],
                 "external_ids":{"isrc":"BRRCA7200015"}}},
              {"added_at":"2021-01-02T00:00:00Z","track":{
                 "id":"t2","name":"Bittersweet Symphony","type":"track",
                 "album":{"id":"al2","release_date":"1997-09-29"},
                 "artists":[{"id":"a2","name":"The Verve"}],
                 "external_ids":{"isrc":"GBAAA9700123"}}}
            ]"#,
        );

        let stats = persist_playlist(&s, "p1", &meta(), &its).unwrap();
        assert_eq!(stats.tracks_imported, 2);
        assert_eq!(stats.with_isrc, 2);
        assert_eq!(stats.artists_imported, 2);
        assert!((stats.isrc_ratio() - 1.0).abs() < 1e-9);

        let tracks = s.playlist_tracks("p1").unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].track.name, "Azul da Cor do Mar");
        assert_eq!(tracks[0].track.isrc.as_deref(), Some("BRRCA7200015"));
        assert_eq!(tracks[0].position, 0);
        assert_eq!(tracks[1].position, 1);
        assert_eq!(tracks[1].artists[0].name, "The Verve");

        // The playlist is marked synced so the UI can show when it was analysed.
        assert!(s.get_playlist("p1").unwrap().unwrap().synced_at.is_some());
    }

    #[test]
    fn skips_local_files_episodes_and_null_tracks_without_gaps_in_position() {
        let s = store();
        let its = items(
            r#"[
              {"track":{"id":"t1","name":"Real","type":"track",
                        "artists":[{"id":"a1","name":"A"}],
                        "external_ids":{"isrc":"X1"}}},
              {"track":null},
              {"is_local":true,"track":{"id":null,"name":"My Rip","is_local":true}},
              {"track":{"id":"ep1","name":"Some Podcast","type":"episode"}},
              {"track":{"id":"t2","name":"Also Real","type":"track",
                        "artists":[{"id":"a1","name":"A"}]}}
            ]"#,
        );

        let stats = persist_playlist(&s, "p1", &meta(), &its).unwrap();
        assert_eq!(stats.items_seen, 5);
        assert_eq!(stats.tracks_imported, 2);
        assert_eq!(stats.skipped_unresolvable, 1);
        assert_eq!(stats.skipped_local, 1);
        assert_eq!(stats.skipped_episodes, 1);
        // One artist, credited on two tracks, counted once.
        assert_eq!(stats.artists_imported, 1);

        let tracks = s.playlist_tracks("p1").unwrap();
        assert_eq!(tracks.len(), 2);
        // Positions stay contiguous despite the three skipped entries.
        assert_eq!(tracks[0].position, 0);
        assert_eq!(tracks[1].position, 1);
    }

    #[test]
    fn reports_zero_isrc_coverage_when_external_ids_are_absent() {
        // The phase-2 risk: if the API surface stops returning `external_ids`,
        // the import must still succeed and say so loudly.
        let s = store();
        let its = items(
            r#"[{"track":{"id":"t1","name":"No ISRC","type":"track",
                          "artists":[{"id":"a1","name":"A"}]}}]"#,
        );
        let stats = persist_playlist(&s, "p1", &meta(), &its).unwrap();
        assert_eq!(stats.tracks_imported, 1);
        assert_eq!(stats.with_isrc, 0);
        assert_eq!(stats.isrc_ratio(), 0.0);

        let (total, with_isrc) = s.isrc_coverage("p1").unwrap();
        assert_eq!((total, with_isrc), (1, 0));
    }

    #[test]
    fn multiple_artist_credits_are_kept_in_order() {
        let s = store();
        let its = items(
            r#"[{"track":{"id":"t1","name":"Feat","type":"track",
                 "artists":[{"id":"a1","name":"Main"},
                            {"id":"a2","name":"Guest"},
                            {"id":"a3","name":"Producer"}]}}]"#,
        );
        persist_playlist(&s, "p1", &meta(), &its).unwrap();

        let artists = s.track_artists("t1").unwrap();
        assert_eq!(
            artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["Main", "Guest", "Producer"]
        );
    }

    #[test]
    fn reimport_reflects_removals_and_reordering() {
        let s = store();
        let first = items(
            r#"[{"track":{"id":"t1","name":"One","type":"track","artists":[{"id":"a1","name":"A"}],
                          "external_ids":{"isrc":"I1"}}},
                {"track":{"id":"t2","name":"Two","type":"track","artists":[{"id":"a1","name":"A"}]}}]"#,
        );
        persist_playlist(&s, "p1", &meta(), &first).unwrap();

        // User removed t1 and the order changed upstream.
        let second = items(
            r#"[{"track":{"id":"t2","name":"Two","type":"track","artists":[{"id":"a1","name":"A"}]}}]"#,
        );
        let stats = persist_playlist(&s, "p1", &meta(), &second).unwrap();
        assert_eq!(stats.tracks_imported, 1);

        let tracks = s.playlist_tracks("p1").unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track.spotify_id, "t2");

        // t1 still exists as a track (its enrichment is worth keeping), it is
        // just no longer a member of this playlist.
        let conn = s.conn().unwrap();
        let still_there: i64 = conn
            .query_row("SELECT COUNT(*) FROM track WHERE spotify_id='t1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(still_there, 1);
        // And the previously-recorded ISRC survived the re-import.
        let isrc: Option<String> = conn
            .query_row("SELECT isrc FROM track WHERE spotify_id='t1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(isrc.as_deref(), Some("I1"));
    }

    #[test]
    fn distinct_artists_are_deduplicated_for_enrichment() {
        let s = store();
        let its = items(
            r#"[{"track":{"id":"t1","type":"track","name":"1","artists":[{"id":"a1","name":"Tim Maia"}]}},
                {"track":{"id":"t2","type":"track","name":"2","artists":[{"id":"a1","name":"Tim Maia"}]}},
                {"track":{"id":"t3","type":"track","name":"3","artists":[{"id":"a2","name":"Jorge Ben"}]}}]"#,
        );
        persist_playlist(&s, "p1", &meta(), &its).unwrap();

        // Enrichment runs per artist, so 3 tracks cost only 2 artist lookups.
        let artists = s.playlist_artists("p1").unwrap();
        assert_eq!(artists.len(), 2);
    }

    #[test]
    fn empty_playlist_imports_cleanly() {
        let s = store();
        let stats = persist_playlist(&s, "p1", &meta(), &[]).unwrap();
        assert_eq!(stats, ImportStats { items_seen: 0, ..Default::default() });
        assert_eq!(stats.isrc_ratio(), 0.0);
        assert!(s.playlist_tracks("p1").unwrap().is_empty());
    }

    #[test]
    fn track_without_artist_ids_still_imports() {
        // Some very old catalogue entries carry a name but no artist id.
        let s = store();
        let its = items(
            r#"[{"track":{"id":"t1","name":"Orphan","type":"track",
                          "artists":[{"id":null,"name":"Unknown"}]}}]"#,
        );
        let stats = persist_playlist(&s, "p1", &meta(), &its).unwrap();
        assert_eq!(stats.tracks_imported, 1);
        assert_eq!(stats.artists_imported, 0);
        assert!(s.track_artists("t1").unwrap().is_empty());
    }
}
