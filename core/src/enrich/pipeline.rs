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
use super::ratelimit::RateLimiters;
use super::wikidata::WikidataClient;

/// Aggregate counts for one enrichment run.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
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
    limit: Option<usize>,
    only_unresolved: Option<bool>,
    on_progress: ProgressFn,
) -> Result<EnrichStats> {
    let user_agent = settings.user_agent();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_default();

    let limiters = RateLimiters::new();
    let fetcher = Fetcher::new(
        http.clone(),
        store.clone(),
        limiters,
        settings.cache.clone(),
        user_agent,
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
    // Tracks the MBIDs of artists processed in this run to avoid re-fetching MB/Wikidata.
    let mut seen_artist_mbids: HashSet<String> = HashSet::new();
    // Tracks Spotify artist IDs processed this run to avoid re-fetching Last.fm/Discogs.
    let mut seen_spotify_artists: HashSet<String> = HashSet::new();

    tracing::info!(
        tracks = total,
        limit = ?limit,
        lastfm_active = lastfm.is_some(),
        discogs_active = discogs.is_some(),
        "starting enrichment run"
    );

    let mut newly_enriched: usize = 0;
    for (idx, pt) in playlist_tracks.iter().enumerate() {
        if limit.map_or(false, |n| newly_enriched >= n) {
            tracing::info!(newly_enriched, limit = ?limit, "batch limit reached; stopping");
            break;
        }
        let track = &pt.track;
        let track_id = &pt.track.spotify_id;
        let track_name = track.name.clone();

        // --- Resumability check ---
        // A track is fully done only when its recording is resolved AND every
        // artist has already had their tags fetched in this run. We don't skip
        // on "artists have MBIDs" alone because a previous run may have stored
        // the MBID without completing tag fetching (e.g. due to a batch limit).
        let already_resolved = store.get_track_mbid(track_id)?.is_some();
        let track_artists = store.track_artists(track_id)?;
        let all_artists_seen_this_run = !track_artists.is_empty()
            && track_artists.iter().all(|a| {
                store.get_artist_mbid(&a.spotify_id)
                    .ok()
                    .flatten()
                    .map(|mbid| seen_artist_mbids.contains(&mbid))
                    .unwrap_or(false)
            });

        if already_resolved && all_artists_seen_this_run {
            // Truly done — recording resolved and tags fetched earlier this run.
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

        // If only_unresolved=true, skip tracks that already have both genres and origin.
        if only_unresolved.unwrap_or(false) && store.track_is_fully_resolved(track_id)? {
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

        // --- Step 1: Resolve recording (ISRC → name search fallback) ---
        //
        // Both paths return artist-credits from the MB response. These credits
        // are used in Step 2 as a fallback when the URL-rel lookup fails — the
        // recording response already carries the artist MBID, so we don't need
        // a separate URL-rel for artists like RPM/Marina Lima/Barão Vermelho
        // whose Spotify URL is not yet registered in MusicBrainz.
        let mut recording_credits: Vec<(String, String)> = Vec::new();
        let mut recording_resolved = already_resolved;

        if !already_resolved {
            // 1a: ISRC lookup (deterministic, highest confidence).
            if let Some(isrc) = &track.isrc {
                match mb.resolve_isrc(isrc).await {
                    Ok(Some((recording, signals, credits))) => {
                        store.upsert_mb_recording(&recording)?;
                        store.link_track_mb(track_id, &recording.mbid, recording.confidence)?;
                        let n = signals.len();
                        store.insert_tag_signals(&signals)?;
                        stats.recordings_resolved += 1;
                        stats.tag_signals_inserted += n;
                        recording_resolved = true;
                        recording_credits = credits;
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!(track_id, isrc, error = %e, "ISRC lookup failed"),
                }
            }

            // 1b: Name-based search (scored fallback when ISRC fails).
            if !recording_resolved {
                let primary_artist = track_artists.first().map(|a| a.name.as_str()).unwrap_or("");
                match mb.search_recording(&track.name, primary_artist).await {
                    Ok(Some((recording, score, credits))) => {
                        if score >= settings.review_threshold {
                            store.upsert_mb_recording(&recording)?;
                            store.link_track_mb(track_id, &recording.mbid, score)?;
                            stats.name_matched += 1;
                            // Fetch recording-level tags — the ISRC path does this too
                            if let Ok(rec_signals) = mb.recording_tags(&recording.mbid).await {
                                stats.tag_signals_inserted += rec_signals.len();
                                let _ = store.insert_tag_signals(&rec_signals);
                            }
                            recording_credits = credits;
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
                        store.flag_needs_review("track", track_id, "no_mb_match", None)?;
                        stats.needs_review += 1;
                    }
                    Err(e) => {
                        tracing::warn!(track_id, error = %e, "name search failed");
                        store.flag_needs_review("track", track_id, "search_error", None)?;
                        stats.needs_review += 1;
                    }
                }
            }
        }

        // If already resolved in a prior run, recording_credits is empty.
        // Re-run the ISRC lookup — it hits api_cache at zero network cost.
        if recording_credits.is_empty() {
            if let Some(isrc) = &track.isrc {
                if let Ok(Some((_, _, credits))) = mb.resolve_isrc(isrc).await {
                    recording_credits = credits;
                }
            }
        }

        // --- Step 2: Artist MBID resolution + tag fetching ---
        //
        // For each Spotify artist on the track, resolve their MB artist MBID via:
        //   1. Already in artist_mb (from a prior run)
        //   2. Spotify URL relationship lookup (authoritative when available)
        //   3. Name match against recording artist-credits (key fallback for
        //      artists without a Spotify URL in MusicBrainz)
        //
        // Then fetch tags (MB genres/tags, Last.fm, Discogs, Wikidata) for any
        // artist not yet processed in this run.
        for artist in &track_artists {
            // 1. Resolve MBID → Option<String> (never `continue` on failure so
            //    Last.fm/Discogs still run even without a MusicBrainz match).
            let artist_mbid: Option<String> = if let Some(mbid) = store.get_artist_mbid(&artist.spotify_id)? {
                Some(mbid)
            } else {
                // Try URL-rel first (most specific, authoritative link).
                match mb.artist_by_spotify_url(&artist.spotify_id).await {
                    Ok(Some(mb_artist)) => {
                        let mbid = mb_artist.mbid.clone();
                        store.upsert_mb_artist(&mb_artist)?;
                        store.link_artist_mb(
                            &artist.spotify_id,
                            &mbid,
                            0.95,
                            "url_relationship",
                        )?;
                        stats.artists_resolved += 1;
                        Some(mbid)
                    }
                    Ok(None) | Err(_) => {
                        // Match by name against the artist-credits in the recording response.
                        let name_lower = artist.name.to_lowercase();
                        if let Some((credit_mbid, _)) = recording_credits
                            .iter()
                            .find(|(_, n)| n.to_lowercase() == name_lower)
                        {
                            let mbid = credit_mbid.clone();
                            match mb.fetch_artist_detail(&mbid).await {
                                Ok(mb_artist) => {
                                    store.upsert_mb_artist(&mb_artist)?;
                                    store.link_artist_mb(
                                        &artist.spotify_id,
                                        &mbid,
                                        0.85,
                                        "recording_credit",
                                    )?;
                                    stats.artists_resolved += 1;
                                    Some(mbid)
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        artist_id = %artist.spotify_id,
                                        %mbid,
                                        error = %e,
                                        "artist detail fetch failed for recording credit"
                                    );
                                    None
                                }
                            }
                        } else {
                            // Last resort: search MB by artist name.
                            match mb.search_artist_by_name(&artist.name).await {
                                Ok(Some(mb_artist)) => {
                                    let mbid = mb_artist.mbid.clone();
                                    store.upsert_mb_artist(&mb_artist)?;
                                    store.link_artist_mb(
                                        &artist.spotify_id,
                                        &mbid,
                                        0.75,
                                        "name_search",
                                    )?;
                                    stats.artists_resolved += 1;
                                    Some(mbid)
                                }
                                Ok(None) => {
                                    tracing::debug!(
                                        artist = %artist.name,
                                        "no MB artist found via any resolution method"
                                    );
                                    None
                                }
                                Err(e) => {
                                    tracing::debug!(
                                        artist = %artist.name,
                                        error = %e,
                                        "artist name search failed"
                                    );
                                    None
                                }
                            }
                        }
                    }
                }
            };

            // MBID-gated: MB artist tags + Wikidata (deduplicated by MBID).
            if let Some(ref mbid) = artist_mbid {
                if seen_artist_mbids.insert(mbid.clone()) {
                    match mb.artist_tags(mbid).await {
                        Ok(signals) => {
                            stats.tag_signals_inserted += signals.len();
                            let _ = store.insert_tag_signals(&signals);
                        }
                        Err(e) => tracing::warn!(mbid, error = %e, "MB artist tags failed"),
                    }

                    if let Ok(Some(mb_a)) = store.get_mb_artist(mbid) {
                        if let Some(ref qid) = mb_a.wikidata_qid {
                            match wikidata.country_of_origin(qid).await {
                                Ok(Some(code)) => {
                                    let sig = crate::model::TagSignal {
                                        entity_type: EntityType::MbArtist,
                                        entity_id: mbid.clone(),
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

            // Name-gated: Last.fm + Discogs — fire even when MBID is unresolved
            // (deduplicated by Spotify artist ID).
            if seen_spotify_artists.insert(artist.spotify_id.clone()) {
                if let Some(ref lfm) = lastfm {
                    match lfm.artist_top_tags(&artist.name, &artist.spotify_id).await {
                        Ok(signals) => {
                            stats.tag_signals_inserted += signals.len();
                            let _ = store.insert_tag_signals(&signals);
                        }
                        Err(e) => tracing::debug!(error = %e, "Last.fm artist tags failed"),
                    }
                }

                if let Some(ref dg) = discogs {
                    match dg.artist_tags(&artist.name).await {
                        Ok(signals) => {
                            stats.tag_signals_inserted += signals.len();
                            let _ = store.insert_tag_signals(&signals);
                        }
                        Err(e) => tracing::debug!(error = %e, "Discogs artist tags failed"),
                    }
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
        newly_enriched += 1;

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
        assert!(json.contains("\"recordingsResolved\":3"));
    }
}
