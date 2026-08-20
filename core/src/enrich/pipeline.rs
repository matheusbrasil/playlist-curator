//! Enrichment pipeline orchestration.
//!
//! `enrich_playlist` drives the three-step cascade for every track in a playlist:
//!
//!  1. ISRC → MusicBrainz recording.
//!  2. Spotify URL → MusicBrainz artist (even when recording lookup fails).
//!  3. Name search → low-confidence fallback; queues for review if below threshold.
//!
//! The pipeline is **resumable**: a track whose recording and all artists are
//! already resolved in the database is skipped. Killing the app mid-run and
//! restarting picks up where it left off.
//!
//! Progress is reported per track via `on_progress`, which the Tauri shell
//! connects to an event emitter so the UI can show a live progress bar.

use std::collections::HashSet;
use std::sync::Arc;

use serde::Serialize;

use crate::config::Settings;
use crate::error::Result;
use crate::model::{EntityType, Source};
use crate::store::Store;

use super::discogs::DiscogsClient;
use super::fetch::Fetcher;
use super::lastfm::LastfmClient;
use super::musicbrainz::MusicBrainzClient;
use super::ratelimit::{Host, RateLimiters};
use super::wikidata::WikidataClient;

/// Aggregate counts for one enrichment run.
#[derive(Debug, Default, Clone, Serialize)]
pub struct EnrichStats {
    pub tracks_processed: usize,
    pub recordings_resolved: usize,
    pub artists_resolved: usize,
    pub name_matched: usize,
    pub needs_review: usize,
    pub tag_signals_inserted: usize,
    pub cache_hits: u64,
    pub network_calls: u64,
}

/// Snapshot emitted after each track is processed.
#[derive(Debug, Clone, Serialize)]
pub struct EnrichProgress {
    pub playlist_id: String,
    pub current: usize,
    pub total: usize,
    pub track_name: String,
    pub stats: EnrichStats,
}

pub type ProgressFn = Arc<dyn Fn(EnrichProgress) + Send + Sync>;

/// Run the enrichment cascade for every track in `playlist_id`.
///
/// Call this again on the same playlist to resume after an interruption; already-
/// resolved tracks are skipped cheaply.
pub async fn enrich_playlist(
    store: Store,
    settings: Settings,
    playlist_id: &str,
    on_progress: ProgressFn,
) -> Result<EnrichStats> {
    let http = reqwest::Client::builder()
        .user_agent(crate::config::USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let limiters = RateLimiters::new();
    let fetcher = Fetcher::new(
        http.clone(),
        store.clone(),
        limiters,
        settings.cache.clone(),
    );

    let mb = MusicBrainzClient::new(fetcher.clone());
    let lastfm = settings.lastfm_api_key.as_ref().map(|key| {
        LastfmClient::new(fetcher.clone(), key.clone())
    });
    let discogs = settings.discogs_token.as_ref().map(|tok| {
        DiscogsClient::new(fetcher.clone(), tok.clone())
    });
    let wikidata = WikidataClient::new(fetcher.clone());

    let job_id = store.job_start("enrich", Some(playlist_id))?;

    let playlist_tracks = store.playlist_tracks(playlist_id)?;
    let total = playlist_tracks.len();
    let mut stats = EnrichStats::default();
    // Tracks the MBIDs of artists processed in this run to avoid re-fetching.
    let mut seen_artist_mbids: HashSet<String> = HashSet::new();

    for (idx, pt) in playlist_tracks.iter().enumerate() {
        let track = &pt.track;
        let track_id = &pt.track.spotify_id;
        let track_name = track.name.clone();

        // --- Resumability check ---
        let already_resolved = store.get_track_mbid(track_id)?.is_some();
        let track_artists = store.track_artists(track_id)?;
        let all_artists_resolved = !track_artists.is_empty()
            && track_artists
                .iter()
                .all(|a| store.get_artist_mbid(&a.spotify_id).ok().flatten().is_some());

        if already_resolved && all_artists_resolved {
            stats.tracks_processed += 1;
            on_progress(EnrichProgress {
                playlist_id: playlist_id.to_string(),
                current: idx + 1,
                total,
                track_name: track_name.clone(),
                stats: stats.clone(),
            });
            continue;
        }

        // --- Step 1: ISRC → recording ---
        let mut recording_resolved = already_resolved;
        if !already_resolved {
            if let Some(isrc) = &track.isrc {
                match mb.resolve_isrc(isrc).await {
                    Ok(Some((recording, signals))) => {
                        store.upsert_mb_recording(&recording)?;
                        store.link_track_mb(track_id, &recording.mbid, recording.confidence)?;
                        let n = signals.len();
                        store.insert_tag_signals(&signals)?;
                        stats.recordings_resolved += 1;
                        stats.tag_signals_inserted += n;
                        recording_resolved = true;
                    }
                    Ok(None) => {} // Not in MB; continue to next steps.
                    Err(e) => {
                        tracing::warn!(track_id, isrc, error = %e, "ISRC lookup failed");
                    }
                }
            }
        }

        // --- Step 2: Artist via Spotify URL relationship ---
        for artist in &track_artists {
            if store.get_artist_mbid(&artist.spotify_id)?.is_some() {
                continue; // Already resolved.
            }

            match mb.artist_by_spotify_url(&artist.spotify_id).await {
                Ok(Some(mb_artist)) => {
                    let artist_mbid = mb_artist.mbid.clone();
                    store.upsert_mb_artist(&mb_artist)?;
                    store.link_artist_mb(
                        &artist.spotify_id,
                        &artist_mbid,
                        0.95,
                        "url_relationship",
                    )?;
                    stats.artists_resolved += 1;

                    if seen_artist_mbids.insert(artist_mbid.clone()) {
                        // Fetch artist tags only once per run.
                        match mb.artist_tags(&artist_mbid).await {
                            Ok(signals) => {
                                stats.tag_signals_inserted += signals.len();
                                let _ = store.insert_tag_signals(&signals);
                            }
                            Err(e) => tracing::warn!(artist_mbid, error = %e, "MB artist tags failed"),
                        }

                        // Last.fm artist tags.
                        if let Some(ref lfm) = lastfm {
                            match lfm.artist_top_tags(&artist.name, &artist.spotify_id).await {
                                Ok(signals) => {
                                    stats.tag_signals_inserted += signals.len();
                                    let _ = store.insert_tag_signals(&signals);
                                }
                                Err(e) => tracing::debug!(error = %e, "Last.fm artist tags failed"),
                            }
                        }

                        // Discogs artist tags.
                        if let Some(ref dg) = discogs {
                            match dg.artist_tags(&artist.name).await {
                                Ok(signals) => {
                                    stats.tag_signals_inserted += signals.len();
                                    let _ = store.insert_tag_signals(&signals);
                                }
                                Err(e) => tracing::debug!(error = %e, "Discogs artist tags failed"),
                            }
                        }

                        // Wikidata: supplement origin if MB artist has a QID.
                        if let Ok(Some(mb_a)) = store.get_mb_artist(&artist_mbid) {
                            if let Some(ref qid) = mb_a.wikidata_qid {
                                match wikidata.country_of_origin(qid).await {
                                    Ok(Some(code)) => {
                                        // Store as a Wikidata origin signal; derive.rs picks it up.
                                        let sig = crate::model::TagSignal {
                                            entity_type: EntityType::MbArtist,
                                            entity_id: artist_mbid.clone(),
                                            source: Source::Wikidata,
                                            raw_tag: format!("country:{code}"),
                                            weight: 0.8,
                                            fetched_at: crate::util::now_iso(),
                                            kind: None,
                                        };
                                        let _ = store.insert_tag_signal(&sig);
                                    }
                                    Ok(None) => {}
                                    Err(e) => tracing::debug!(error = %e, "Wikidata lookup failed"),
                                }
                            }
                        }
                    }
                }
                Ok(None) => {} // Not in MB via URL-rel.
                Err(e) => {
                    tracing::warn!(artist_id = %artist.spotify_id, error = %e, "URL-rel lookup failed");
                }
            }
        }

        // --- Step 3: Name-based fallback if still unresolved ---
        if !recording_resolved {
            let primary_artist = track_artists.first().map(|a| a.name.as_str()).unwrap_or("");
            match mb.search_recording(&track.name, primary_artist).await {
                Ok(Some((recording, score))) => {
                    if score >= settings.review_threshold {
                        store.upsert_mb_recording(&recording)?;
                        store.link_track_mb(track_id, &recording.mbid, score)?;
                        stats.name_matched += 1;
                    } else {
                        store.flag_needs_review(
                            "track",
                            track_id,
                            "low_confidence_match",
                            Some(&format!("best score {score:.2}")),
                        )?;
                        stats.needs_review += 1;
                    }
                }
                Ok(None) => {
                    store.flag_needs_review(
                        "track",
                        track_id,
                        "no_mb_match",
                        None,
                    )?;
                    stats.needs_review += 1;
                }
                Err(e) => {
                    tracing::warn!(track_id, error = %e, "name search failed");
                    store.flag_needs_review("track", track_id, "search_error", None)?;
                    stats.needs_review += 1;
                }
            }
        }

        // --- Step 4: Discogs release tags by ISRC ---
        if let (Some(ref dg), Some(isrc)) = (&discogs, &track.isrc) {
            let primary_artist = track_artists.first().map(|a| a.name.as_str()).unwrap_or("");
            match dg.release_tags_by_isrc(isrc, &track.name, primary_artist).await {
                Ok(signals) if !signals.is_empty() => {
                    stats.tag_signals_inserted += signals.len();
                    let _ = store.insert_tag_signals(&signals);
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(isrc, error = %e, "Discogs release tags failed"),
            }
        }

        // --- Step 5: Last.fm track tags ---
        if let Some(ref lfm) = lastfm {
            let primary_artist = track_artists.first().map(|a| a.name.as_str()).unwrap_or("");
            match lfm.track_top_tags(&track.name, primary_artist, track_id).await {
                Ok(signals) if !signals.is_empty() => {
                    stats.tag_signals_inserted += signals.len();
                    let _ = store.insert_tag_signals(&signals);
                }
                Ok(_) => {}
                Err(e) => tracing::debug!(error = %e, "Last.fm track tags failed"),
            }
        }

        stats.tracks_processed += 1;

        // Update cache/network counters from the fetcher.
        let (hits, network, _) = fetcher.counters.snapshot();
        stats.cache_hits = hits;
        stats.network_calls = network;

        on_progress(EnrichProgress {
            playlist_id: playlist_id.to_string(),
            current: idx + 1,
            total,
            track_name,
            stats: stats.clone(),
        });
    }

    // Final counter sync.
    let (hits, network, _) = fetcher.counters.snapshot();
    stats.cache_hits = hits;
    stats.network_calls = network;

    store.job_finish(
        job_id,
        &serde_json::to_string(&stats).unwrap_or_default(),
    )?;

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enrich_stats_default_is_zeros() {
        let stats = EnrichStats::default();
        assert_eq!(stats.tracks_processed, 0);
        assert_eq!(stats.recordings_resolved, 0);
        assert_eq!(stats.tag_signals_inserted, 0);
    }

    #[test]
    fn enrich_progress_serialises() {
        let progress = EnrichProgress {
            playlist_id: "pl1".into(),
            current: 5,
            total: 100,
            track_name: "Some Track".into(),
            stats: EnrichStats {
                tracks_processed: 5,
                recordings_resolved: 3,
                ..Default::default()
            },
        };
        let json = serde_json::to_string(&progress).unwrap();
        assert!(json.contains("\"current\":5"));
        assert!(json.contains("\"total\":100"));
        assert!(json.contains("\"recordings_resolved\":3"));
    }
}
