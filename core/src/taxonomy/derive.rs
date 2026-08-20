//! Derive `artist_origin` and `track_era` from stored MusicBrainz data.
//!
//! Both functions are pure on their inputs — the only I/O is in the batch
//! wrappers that read from and write to the store.
//!
//! ## Origin precedence
//!
//! `begin_area` (city of formation) outranks the ISO country code because it
//! answers "where is this act from" correctly even when the band has since
//! relocated. A Brazilian act that moved to Lisbon stays Brazilian.
//!
//! ## Era precedence
//!
//! `mb_recording.first_release_date` outranks `track.spotify_release_date`
//! because Spotify dates reissues and remasters, not original releases.

use crate::error::Result;
use crate::model::{ArtistOrigin, EraSource, MbArtist, MbRecording, OriginSource, Track, TrackEra};
use crate::store::Store;

/// Derive the best available origin for a single artist.
///
/// Returns `None` when none of the MusicBrainz fields carry geographic data.
pub fn derive_origin(artist_spotify_id: &str, mb_artist: &MbArtist) -> Option<ArtistOrigin> {
    // Priority 1: begin_area — where the act formed.
    if let Some(city) = mb_artist.begin_area.as_deref().filter(|s| !s.is_empty()) {
        return Some(ArtistOrigin {
            artist_spotify_id: artist_spotify_id.to_owned(),
            country_code: mb_artist.country.clone(),
            country_label: None,
            city: Some(city.to_owned()),
            source: OriginSource::MbBeginArea,
            confidence: 0.95,
        });
    }

    // Priority 2: explicit country code.
    if let Some(country) = mb_artist.country.as_deref().filter(|s| !s.is_empty()) {
        return Some(ArtistOrigin {
            artist_spotify_id: artist_spotify_id.to_owned(),
            country_code: Some(country.to_owned()),
            country_label: None,
            city: None,
            source: OriginSource::MbCountry,
            confidence: 0.85,
        });
    }

    // Priority 3: area (less precise than country code, still useful).
    if let Some(area) = mb_artist.area.as_deref().filter(|s| !s.is_empty()) {
        return Some(ArtistOrigin {
            artist_spotify_id: artist_spotify_id.to_owned(),
            country_code: None,
            country_label: Some(area.to_owned()),
            city: None,
            source: OriginSource::MbArea,
            confidence: 0.7,
        });
    }

    None
}

/// Derive `artist_origin` for every artist in a playlist that has MB data.
///
/// Skips artists with no MB link or no geographic data; never overwrites a
/// `user_override` (that is enforced by the SQL upsert in `repo.rs`).
/// Returns the number of origins upserted.
pub fn derive_origins_for_playlist(store: &Store, playlist_id: &str) -> Result<usize> {
    let artists = store.playlist_artists(playlist_id)?;
    let mut count = 0;

    for artist in &artists {
        let Some(mbid) = store.get_artist_mbid(&artist.spotify_id)? else {
            continue;
        };
        let Some(mb) = store.get_mb_artist(&mbid)? else {
            continue;
        };
        if let Some(origin) = derive_origin(&artist.spotify_id, &mb) {
            store.upsert_artist_origin(&origin)?;
            count += 1;
        }
    }

    Ok(count)
}

/// Extract the year from a date string of the form `YYYY`, `YYYY-MM`, or
/// `YYYY-MM-DD`. Returns `None` for anything that does not start with four
/// ASCII digits.
fn parse_year(date: &str) -> Option<i32> {
    if date.len() < 4 {
        return None;
    }
    date[..4].parse::<i32>().ok()
}

/// Derive the era for a single track.
///
/// Prefers the MB recording's `first_release_date` over Spotify's
/// `spotify_release_date` so that a 1972 track on a 2015 remaster is placed
/// in the 1970s, not the 2010s.
pub fn derive_era(
    track_spotify_id: &str,
    mb_recording: Option<&MbRecording>,
    track: &Track,
) -> Option<TrackEra> {
    // Priority 1: MB first-release date.
    if let Some(year) = mb_recording
        .and_then(|r| r.first_release_date.as_deref())
        .and_then(parse_year)
    {
        return Some(TrackEra {
            track_spotify_id: track_spotify_id.to_owned(),
            year: Some(year),
            decade: Some((year / 10) * 10),
            source: EraSource::MbFirstRelease,
        });
    }

    // Priority 2: Spotify release date.
    if let Some(year) = track.spotify_release_date.as_deref().and_then(parse_year) {
        return Some(TrackEra {
            track_spotify_id: track_spotify_id.to_owned(),
            year: Some(year),
            decade: Some((year / 10) * 10),
            source: EraSource::SpotifyReleaseDate,
        });
    }

    None
}

/// Derive `track_era` for every track in a playlist.
///
/// Returns the number of eras upserted.
pub fn derive_eras_for_playlist(store: &Store, playlist_id: &str) -> Result<usize> {
    let playlist_tracks = store.playlist_tracks(playlist_id)?;
    let mut count = 0;

    for pt in &playlist_tracks {
        let mb_recording = store
            .get_track_mbid(&pt.track.spotify_id)?
            .and_then(|mbid| store.get_mb_recording(&mbid).ok().flatten());

        if let Some(era) = derive_era(&pt.track.spotify_id, mb_recording.as_ref(), &pt.track) {
            store.upsert_track_era(&era)?;
            count += 1;
        }
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::MbArtist;

    fn mb_artist() -> MbArtist {
        MbArtist {
            mbid: "fake-mbid".into(),
            name: Some("Test Artist".into()),
            sort_name: None,
            artist_type: None,
            country: None,
            area: None,
            begin_area: None,
            begin_date: None,
            end_date: None,
            wikidata_qid: None,
        }
    }

    fn track(release_date: Option<&str>) -> Track {
        Track {
            spotify_id: "tid".into(),
            name: "Test Track".into(),
            isrc: None,
            duration_ms: None,
            spotify_album_id: None,
            spotify_release_date: release_date.map(str::to_owned),
            is_local: false,
        }
    }

    #[test]
    fn begin_area_outranks_country() {
        let mb = MbArtist {
            country: Some("PT".into()),
            begin_area: Some("Rio de Janeiro".into()),
            ..mb_artist()
        };
        let origin = derive_origin("sp1", &mb).unwrap();
        assert_eq!(origin.source, OriginSource::MbBeginArea);
        assert_eq!(origin.city.as_deref(), Some("Rio de Janeiro"));
        assert_eq!(origin.country_code.as_deref(), Some("PT"));
        assert!((origin.confidence - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn country_used_when_no_begin_area() {
        let mb = MbArtist {
            country: Some("BR".into()),
            ..mb_artist()
        };
        let origin = derive_origin("sp2", &mb).unwrap();
        assert_eq!(origin.source, OriginSource::MbCountry);
        assert_eq!(origin.country_code.as_deref(), Some("BR"));
        assert!(origin.city.is_none());
    }

    #[test]
    fn area_used_as_last_resort() {
        let mb = MbArtist {
            area: Some("São Paulo".into()),
            ..mb_artist()
        };
        let origin = derive_origin("sp3", &mb).unwrap();
        assert_eq!(origin.source, OriginSource::MbArea);
        assert_eq!(origin.country_label.as_deref(), Some("São Paulo"));
    }

    #[test]
    fn none_when_no_geo_data() {
        assert!(derive_origin("sp4", &mb_artist()).is_none());
    }

    #[test]
    fn mb_release_date_beats_spotify() {
        let mb = MbRecording {
            mbid: "r1".into(),
            title: None,
            first_release_date: Some("1972-03".into()),
            resolved_via: None,
            confidence: 1.0,
        };
        let era = derive_era("tid", Some(&mb), &track(Some("2015-06-12"))).unwrap();
        assert_eq!(era.year, Some(1972));
        assert_eq!(era.decade, Some(1970));
        assert_eq!(era.source, EraSource::MbFirstRelease);
    }

    #[test]
    fn falls_back_to_spotify_when_no_mb() {
        let era = derive_era("tid", None, &track(Some("1985"))).unwrap();
        assert_eq!(era.year, Some(1985));
        assert_eq!(era.decade, Some(1980));
        assert_eq!(era.source, EraSource::SpotifyReleaseDate);
    }

    #[test]
    fn none_when_no_dates_at_all() {
        assert!(derive_era("tid", None, &track(None)).is_none());
    }

    #[test]
    fn parse_year_handles_all_formats() {
        assert_eq!(parse_year("1972"), Some(1972));
        assert_eq!(parse_year("1972-03"), Some(1972));
        assert_eq!(parse_year("1972-03-14"), Some(1972));
        assert_eq!(parse_year("197"), None);
        assert_eq!(parse_year(""), None);
        assert_eq!(parse_year("abcd"), None);
    }
}
