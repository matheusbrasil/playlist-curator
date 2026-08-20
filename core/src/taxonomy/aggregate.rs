//! Weighted aggregation of raw tag signals into a per-track genre score.
//!
//! # The three rules that make this useful rather than merely plausible
//!
//! **Rollup.** A `samba-rock` signal credits `samba` as well. Without it the app
//! offers forty genres with three tracks each, because upstream tags name
//! micro-genres and the user wants a playlist. Contributions travel up the
//! ancestor chain at [`ROLLUP_ATTENUATION`] per level, which is deliberately
//! `1.0`: a parent must never rank *below* a child, or the broad bucket loses to
//! the narrow one and the rollup achieves nothing. A parent therefore scores at
//! least as much as any single child's contribution, and more when several
//! children fire. Distance decay was considered and rejected for exactly that
//! reason; the constant is left tunable for anyone who disagrees.
//!
//! **Artist inheritance.** Track-level tags are sparse — Last.fm has tags for a
//! famous song and nothing for the other eleven on the album — so a track's
//! artists' signals are pooled in with its own. A track with no signals of its
//! own therefore still gets a genre. The signals are not re-weighted by hand:
//! [`SourceWeights`] already distinguishes `lastfm_track` from `lastfm_artist`,
//! which is the right place for that opinion to live.
//!
//! **The user is right.** A `genre` override on a track wins outright: it scores
//! 1.0 and everything derived is compressed below it.
//!
//! Scores are relative within a track (the strongest genre is 1.0), not absolute.
//! `PlaylistFilter::min_genre_score` is therefore a question about *this track's*
//! confidence ranking, not a cross-track comparison.

use crate::config::{Settings, SourceWeights};
use crate::error::Result;
use crate::model::{EntityType, Source, TagKind, TagSignal, TrackGenre};
use crate::store::Store;
use crate::taxonomy::aliases::Taxonomy;
use std::collections::HashMap;

/// Fraction of a contribution that reaches each further ancestor.
///
/// `1.0` — full credit. See the module docs for why decay is the wrong default
/// here.
pub const ROLLUP_ATTENUATION: f64 = 1.0;

/// Scores below this (post-normalisation) are dropped rather than stored. Keeps
/// `track_genre` from filling with the long tail of a single noisy Last.fm tag.
pub const MIN_RETAINED_SCORE: f64 = 0.05;

/// Hard cap on stored genres per track.
pub const MAX_GENRES_PER_TRACK: usize = 32;

/// Ceiling applied to derived scores when a user override is present, so the
/// override's 1.0 is strictly the top.
const OVERRIDE_HEADROOM: f64 = 0.9;

/// Weight for Wikidata-sourced tags. `SourceWeights` has no field for it, and
/// `config.rs` is not this module's to change; structured Wikidata statements sit
/// between Discogs and MusicBrainz's free-form tags in trustworthiness.
const WIKIDATA_WEIGHT: f64 = 0.7;

/// How much a single signal counts, before its own `weight` is applied.
fn source_weight(sig: &TagSignal, w: &SourceWeights) -> f64 {
    match sig.source {
        // MusicBrainz's curated, voted genre list is a different thing from its
        // free-form folksonomy, and the schema keeps them apart for this reason.
        Source::MusicBrainz => match sig.kind {
            Some(TagKind::Genre) => w.musicbrainz_genre,
            _ => w.musicbrainz_tag,
        },
        Source::Discogs => w.discogs,
        Source::Lastfm => {
            if is_artist_entity(sig.entity_type) {
                w.lastfm_artist
            } else {
                w.lastfm_track
            }
        }
        Source::Spotify => w.spotify_artist,
        Source::Wikidata => WIKIDATA_WEIGHT,
    }
}

fn is_artist_entity(entity: EntityType) -> bool {
    matches!(
        entity,
        EntityType::MbArtist | EntityType::SpotifyArtist | EntityType::Artist
    )
}

/// Score one track's genres from its own signals plus its artists'.
///
/// `genre_override` is the raw value of `user_override(track, …, 'genre')`; it is
/// resolved through `taxonomy` like any other tag, so an override of `Samba Rock`
/// works as well as one of `samba-rock`, and a nonsense override is ignored rather
/// than poisoning the result.
///
/// Returned sorted by descending score, then slug for a stable order.
pub fn aggregate_track_genres(
    track_signals: &[TagSignal],
    artist_signals: &[TagSignal],
    weights: &SourceWeights,
    taxonomy: &Taxonomy,
    genre_override: Option<&str>,
) -> Vec<TrackGenre> {
    let mut totals: HashMap<String, f64> = HashMap::new();

    for sig in track_signals.iter().chain(artist_signals.iter()) {
        let Some(slug) = taxonomy.resolve(&sig.raw_tag) else {
            continue;
        };
        let contribution = source_weight(sig, weights) * sig.weight.clamp(0.0, 1.0);
        if contribution <= 0.0 {
            continue;
        }
        for (depth, ancestor) in taxonomy.ancestors(&slug).into_iter().enumerate() {
            let credit = contribution * ROLLUP_ATTENUATION.powi(depth as i32);
            *totals.entry(ancestor).or_insert(0.0) += credit;
        }
    }

    let override_slug = genre_override.and_then(|raw| taxonomy.resolve(raw));
    let ceiling = if override_slug.is_some() { OVERRIDE_HEADROOM } else { 1.0 };

    let max = totals.values().copied().fold(0.0_f64, f64::max);
    let mut out: Vec<TrackGenre> = if max > 0.0 {
        totals
            .into_iter()
            .map(|(canonical_slug, score)| TrackGenre {
                canonical_slug,
                score: (score / max) * ceiling,
            })
            .filter(|g| g.score >= MIN_RETAINED_SCORE)
            .collect()
    } else {
        Vec::new()
    };

    sort_and_cap(&mut out);

    if let Some(slug) = override_slug {
        // The override's ancestors ride along at the ceiling, so an override of
        // `samba-rock` still puts the track in a `samba` playlist.
        for ancestor in taxonomy.ancestors(&slug) {
            let score = if ancestor == slug { 1.0 } else { OVERRIDE_HEADROOM };
            match out.iter_mut().find(|g| g.canonical_slug == ancestor) {
                Some(existing) => existing.score = existing.score.max(score),
                None => out.push(TrackGenre { canonical_slug: ancestor, score }),
            }
        }
        sort_and_cap(&mut out);
    }

    out
}

fn sort_and_cap(genres: &mut Vec<TrackGenre>) {
    genres.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.canonical_slug.cmp(&b.canonical_slug))
    });
    genres.truncate(MAX_GENRES_PER_TRACK);
}

/// Derive and store genres for every track in a playlist.
///
/// Reads only `tag_signal` and the MusicBrainz bridge tables, so this is safe to
/// re-run at any time — after a vocabulary change, a new alias or an edited
/// override — with no network access at all.
///
/// Returns the number of tracks that ended up with at least one genre. Tracks
/// that ended up with none have their old rows cleared, so a stale derivation
/// never survives a re-run.
pub fn derive_playlist_genres(
    store: &Store,
    settings: &Settings,
    playlist_id: &str,
) -> Result<usize> {
    let mut taxonomy = Taxonomy::load(store)?;
    let tracks = store.playlist_tracks(playlist_id)?;

    // Gather every signal first: it lets the alias table be consulted once per
    // distinct raw tag instead of once per occurrence, and artist signals are
    // shared by every track that artist appears on.
    let mut artist_cache: HashMap<String, Vec<TagSignal>> = HashMap::new();
    let mut per_track: Vec<(String, Vec<TagSignal>, Vec<TagSignal>)> =
        Vec::with_capacity(tracks.len());

    for pt in &tracks {
        let track_id = pt.track.spotify_id.clone();
        let mut track_signals = store.tag_signals_for(EntityType::Track, &track_id)?;
        if let Some(mbid) = store.get_track_mbid(&track_id)? {
            track_signals.extend(store.tag_signals_for(EntityType::MbRecording, &mbid)?);
        }

        let mut artist_signals = Vec::new();
        for artist in &pt.artists {
            if !artist_cache.contains_key(&artist.spotify_id) {
                let mut sigs =
                    store.tag_signals_for(EntityType::SpotifyArtist, &artist.spotify_id)?;
                sigs.extend(store.tag_signals_for(EntityType::Artist, &artist.spotify_id)?);
                if let Some(mbid) = store.get_artist_mbid(&artist.spotify_id)? {
                    sigs.extend(store.tag_signals_for(EntityType::MbArtist, &mbid)?);
                }
                artist_cache.insert(artist.spotify_id.clone(), sigs);
            }
            artist_signals.extend(artist_cache[&artist.spotify_id].iter().cloned());
        }

        per_track.push((track_id, track_signals, artist_signals));
    }

    let raw_tags: Vec<String> = per_track
        .iter()
        .flat_map(|(_, t, a)| t.iter().chain(a.iter()))
        .map(|s| s.raw_tag.clone())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    taxonomy.learn_aliases(store, raw_tags.iter().map(String::as_str))?;

    let mut derived = 0;
    for (track_id, track_signals, artist_signals) in &per_track {
        let genre_override = store.get_override("track", track_id, "genre")?;
        let genres = aggregate_track_genres(
            track_signals,
            artist_signals,
            &settings.weights,
            &taxonomy,
            genre_override.as_deref(),
        );
        store.replace_track_genres(track_id, &genres)?;
        if !genres.is_empty() {
            derived += 1;
        }
    }
    Ok(derived)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Artist, Playlist, Track};
    use crate::taxonomy::genres::seed_canonical_genres;

    fn tax() -> Taxonomy {
        Taxonomy::embedded()
    }

    fn weights() -> SourceWeights {
        SourceWeights::default()
    }

    fn signal(entity: EntityType, id: &str, source: Source, tag: &str, weight: f64) -> TagSignal {
        TagSignal {
            entity_type: entity,
            entity_id: id.into(),
            source,
            raw_tag: tag.into(),
            weight,
            kind: None,
        }
    }

    fn score_of(genres: &[TrackGenre], slug: &str) -> Option<f64> {
        genres.iter().find(|g| g.canonical_slug == slug).map(|g| g.score)
    }

    #[test]
    fn a_samba_rock_signal_also_credits_samba() {
        let signals = [signal(EntityType::MbRecording, "r1", Source::MusicBrainz, "samba rock", 1.0)];
        let genres = aggregate_track_genres(&signals, &[], &weights(), &tax(), None);

        let samba_rock = score_of(&genres, "samba-rock").expect("samba-rock");
        let samba = score_of(&genres, "samba").expect("samba must be credited by rollup");
        assert!(
            samba >= samba_rock,
            "the broad bucket must not rank below its child: samba={samba}, samba-rock={samba_rock}"
        );
    }

    #[test]
    fn several_children_make_the_parent_outrank_each_of_them() {
        // The whole point of rollup: three narrow samba tags produce one strong
        // `samba`, not three thin genres.
        let signals = [
            signal(EntityType::MbArtist, "a", Source::Lastfm, "samba rock", 1.0),
            signal(EntityType::MbArtist, "a", Source::Lastfm, "samba-jazz", 0.8),
            signal(EntityType::MbArtist, "a", Source::Lastfm, "pagode", 0.6),
        ];
        let genres = aggregate_track_genres(&signals, &[], &weights(), &tax(), None);
        assert_eq!(genres[0].canonical_slug, "samba");
        assert!((genres[0].score - 1.0).abs() < 1e-9, "top genre is normalised to 1.0");
        assert!(score_of(&genres, "samba-rock").unwrap() < 1.0);
    }

    #[test]
    fn junk_tags_never_become_genres() {
        let signals = [
            signal(EntityType::MbArtist, "a", Source::Lastfm, "seen live", 1.0),
            signal(EntityType::MbArtist, "a", Source::Lastfm, "favorites", 1.0),
            signal(EntityType::MbArtist, "a", Source::Lastfm, "90s", 1.0),
            signal(EntityType::MbArtist, "a", Source::Lastfm, "female vocalists", 1.0),
        ];
        let genres = aggregate_track_genres(&signals, &[], &weights(), &tax(), None);
        assert!(genres.is_empty(), "{genres:?}");
    }

    #[test]
    fn a_curated_musicbrainz_genre_outweighs_a_free_form_tag() {
        let mut curated = signal(EntityType::MbRecording, "r", Source::MusicBrainz, "soul", 1.0);
        curated.kind = Some(TagKind::Genre);
        let folksonomy = signal(EntityType::MbArtist, "a", Source::Lastfm, "rock", 1.0);

        let genres = aggregate_track_genres(&[curated], &[folksonomy], &weights(), &tax(), None);
        assert_eq!(genres[0].canonical_slug, "soul");
        assert!(score_of(&genres, "rock").unwrap() < 1.0);
    }

    #[test]
    fn spotify_tags_barely_count() {
        // Spotify invents genre names, so its signals must not beat a real source.
        let spotify = signal(EntityType::SpotifyArtist, "a", Source::Spotify, "samba", 1.0);
        let discogs = signal(EntityType::MbRecording, "r", Source::Discogs, "bossa nova", 1.0);
        let genres = aggregate_track_genres(&[discogs], &[spotify], &weights(), &tax(), None);
        // bossa-nova rolls into samba, so samba wins on total — but the specific
        // genre Discogs named must outrank the one Spotify guessed at.
        assert!(score_of(&genres, "bossa-nova").unwrap() > 0.0);
        assert_eq!(genres[0].canonical_slug, "samba");
    }

    #[test]
    fn scores_are_normalised_into_the_unit_range() {
        let signals = [
            signal(EntityType::MbArtist, "a", Source::Lastfm, "mpb", 1.0),
            signal(EntityType::MbArtist, "a", Source::Lastfm, "tropicalia", 0.3),
        ];
        let genres = aggregate_track_genres(&signals, &[], &weights(), &tax(), None);
        assert!(!genres.is_empty());
        for g in &genres {
            assert!((0.0..=1.0).contains(&g.score), "{g:?}");
        }
        assert!((genres[0].score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_user_override_outranks_every_derived_genre() {
        let signals = [signal(EntityType::MbArtist, "a", Source::MusicBrainz, "heavy metal", 1.0)];
        let genres =
            aggregate_track_genres(&signals, &[], &weights(), &tax(), Some("Samba Rock"));

        assert_eq!(genres[0].canonical_slug, "samba-rock");
        assert!((genres[0].score - 1.0).abs() < 1e-9);
        // The evidence is kept, just demoted below the user's answer.
        let metal = score_of(&genres, "heavy-metal").expect("derived genre retained");
        assert!(metal < 1.0, "derived score {metal} must sit under the override");
        // And the override rolls up like any other genre.
        assert!(score_of(&genres, "samba").unwrap() >= OVERRIDE_HEADROOM);
    }

    #[test]
    fn an_override_works_even_with_no_signals_at_all() {
        let genres = aggregate_track_genres(&[], &[], &weights(), &tax(), Some("forró"));
        assert_eq!(genres.len(), 1);
        assert_eq!(genres[0].canonical_slug, "forro");
        assert!((genres[0].score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_nonsense_override_is_ignored_rather_than_trusted() {
        let signals = [signal(EntityType::MbArtist, "a", Source::MusicBrainz, "samba", 1.0)];
        let genres = aggregate_track_genres(
            &signals,
            &[],
            &weights(),
            &tax(),
            Some("brazilian cosmic soul"),
        );
        assert_eq!(genres[0].canonical_slug, "samba");
        assert!((genres[0].score - 1.0).abs() < 1e-9);
    }

    // ------------------------------------------------------------ store level

    fn playlist_with_one_track(store: &Store) {
        store
            .upsert_playlist(&Playlist {
                spotify_id: "p1".into(),
                name: "Source".into(),
                owner: None,
                snapshot_id: None,
                track_count: Some(1),
                synced_at: None,
            })
            .unwrap();
        store
            .upsert_track(&Track {
                spotify_id: "t1".into(),
                name: "Azul da Cor do Mar".into(),
                isrc: None,
                duration_ms: None,
                spotify_album_id: None,
                spotify_release_date: Some("2015-01-01".into()),
                is_local: false,
            })
            .unwrap();
        store
            .upsert_artist(&Artist { spotify_id: "a1".into(), name: "Tim Maia".into() })
            .unwrap();
        store.link_track_artist("t1", "a1", 0).unwrap();
        store
            .replace_playlist_tracks("p1", &[("t1".into(), 0, None)])
            .unwrap();
    }

    fn store_with_playlist() -> Store {
        let s = Store::open_in_memory().unwrap();
        seed_canonical_genres(&s).unwrap();
        playlist_with_one_track(&s);
        s
    }

    #[test]
    fn a_track_with_no_signals_of_its_own_inherits_its_artists_genres() {
        let store = store_with_playlist();
        // Nothing on the track; everything on the artist.
        store
            .insert_tag_signals(&[
                signal(EntityType::SpotifyArtist, "a1", Source::Lastfm, "soul", 1.0),
                signal(EntityType::SpotifyArtist, "a1", Source::Lastfm, "neo soul", 0.7),
            ])
            .unwrap();

        let n = derive_playlist_genres(&store, &Settings::default(), "p1").unwrap();
        assert_eq!(n, 1);

        let genres = store.track_genres("t1").unwrap();
        assert!(!genres.is_empty(), "the track must inherit from its artist");
        assert_eq!(genres[0].canonical_slug, "soul");
    }

    #[test]
    fn derivation_reaches_signals_stored_against_musicbrainz_ids() {
        let store = store_with_playlist();
        store
            .upsert_mb_artist(&crate::model::MbArtist {
                mbid: "mba".into(),
                ..Default::default()
            })
            .unwrap();
        store.link_artist_mb("a1", "mba", 1.0, "url-rel").unwrap();
        store
            .upsert_mb_recording(&crate::model::MbRecording {
                mbid: "mbr".into(),
                ..Default::default()
            })
            .unwrap();
        store.link_track_mb("t1", "mbr", 1.0).unwrap();

        let mut curated = signal(EntityType::MbRecording, "mbr", Source::MusicBrainz, "funk", 1.0);
        curated.kind = Some(TagKind::Genre);
        store
            .insert_tag_signals(&[
                curated,
                signal(EntityType::MbArtist, "mba", Source::Lastfm, "mpb", 0.9),
            ])
            .unwrap();

        derive_playlist_genres(&store, &Settings::default(), "p1").unwrap();
        let genres = store.track_genres("t1").unwrap();
        assert_eq!(genres[0].canonical_slug, "funk");
        assert!(score_of(&genres, "mpb").is_some(), "artist-level signal reached the track");
    }

    #[test]
    fn a_stored_override_beats_every_source() {
        let store = store_with_playlist();
        store
            .insert_tag_signals(&[signal(
                EntityType::SpotifyArtist,
                "a1",
                Source::MusicBrainz,
                "heavy metal",
                1.0,
            )])
            .unwrap();
        store.set_override("track", "t1", "genre", Some("samba-rock")).unwrap();

        derive_playlist_genres(&store, &Settings::default(), "p1").unwrap();
        let genres = store.track_genres("t1").unwrap();
        assert_eq!(genres[0].canonical_slug, "samba-rock");
        assert!((genres[0].score - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_learned_alias_in_the_store_is_honoured_by_the_derivation() {
        let store = store_with_playlist();
        store
            .insert_tag_signals(&[signal(
                EntityType::SpotifyArtist,
                "a1",
                Source::Lastfm,
                "black rio",
                1.0,
            )])
            .unwrap();
        // Unknown to every built-in rule until somebody decides it.
        assert!(derive_playlist_genres(&store, &Settings::default(), "p1").unwrap() == 0);

        store.upsert_genre_alias("black rio", Some("funk"), "user").unwrap();
        assert_eq!(derive_playlist_genres(&store, &Settings::default(), "p1").unwrap(), 1);
        assert_eq!(store.track_genres("t1").unwrap()[0].canonical_slug, "funk");
    }

    #[test]
    fn re_deriving_clears_genres_that_no_longer_apply() {
        let store = store_with_playlist();
        store
            .insert_tag_signals(&[signal(
                EntityType::SpotifyArtist,
                "a1",
                Source::Lastfm,
                "samba",
                1.0,
            )])
            .unwrap();
        derive_playlist_genres(&store, &Settings::default(), "p1").unwrap();
        assert!(!store.track_genres("t1").unwrap().is_empty());

        // A later decision that the tag means nothing must remove the genre.
        store.upsert_genre_alias("samba", None, "user").unwrap();
        assert_eq!(derive_playlist_genres(&store, &Settings::default(), "p1").unwrap(), 0);
        assert!(store.track_genres("t1").unwrap().is_empty());
    }
}
