//! Domain types shared across modules and returned across the IPC boundary.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Track {
    pub spotify_id: String,
    pub name: String,
    pub isrc: Option<String>,
    pub duration_ms: Option<i64>,
    pub spotify_album_id: Option<String>,
    /// Spotify's release date, which for reissues is the *reissue* date. Never
    /// use this for era classification; see [`TrackEra`].
    pub spotify_release_date: Option<String>,
    pub is_local: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Artist {
    pub spotify_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Playlist {
    pub spotify_id: String,
    pub name: String,
    pub owner: Option<String>,
    pub snapshot_id: Option<String>,
    pub track_count: Option<i64>,
    pub synced_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaylistTrack {
    pub track: Track,
    pub artists: Vec<Artist>,
    pub position: i64,
    pub added_at: Option<String>,
}

// ------------------------------------------------------------------ MusicBrainz

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MbRecording {
    pub mbid: String,
    pub title: Option<String>,
    pub first_release_date: Option<String>,
    pub resolved_via: Option<String>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct MbArtist {
    pub mbid: String,
    pub name: Option<String>,
    pub sort_name: Option<String>,
    /// MusicBrainz artist type: Person, Group, Orchestra, ...
    pub artist_type: Option<String>,
    /// Current country (ISO 3166-1 alpha-2).
    pub country: Option<String>,
    pub area: Option<String>,
    /// Where the artist/band began — the city. This outranks `country` for
    /// answering "where is this act from".
    pub begin_area: Option<String>,
    pub begin_date: Option<String>,
    pub end_date: Option<String>,
    pub wikidata_qid: Option<String>,
}

// ------------------------------------------------------------------ Signals

/// Which entity a tag was observed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    MbArtist,
    MbRecording,
    Release,
    SpotifyArtist,
    Track,
    Artist,
}

impl EntityType {
    pub fn as_str(self) -> &'static str {
        match self {
            EntityType::MbArtist => "mb_artist",
            EntityType::MbRecording => "mb_recording",
            EntityType::Release => "release",
            EntityType::SpotifyArtist => "spotify_artist",
            EntityType::Track => "track",
            EntityType::Artist => "artist",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "mb_artist" => EntityType::MbArtist,
            "mb_recording" => EntityType::MbRecording,
            "release" => EntityType::Release,
            "spotify_artist" => EntityType::SpotifyArtist,
            "track" => EntityType::Track,
            "artist" => EntityType::Artist,
            _ => return None,
        })
    }
}

/// Where a signal came from. The base weight encodes how much we trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    MusicBrainz,
    Discogs,
    Lastfm,
    Spotify,
    Wikidata,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::MusicBrainz => "musicbrainz",
            Source::Discogs => "discogs",
            Source::Lastfm => "lastfm",
            Source::Spotify => "spotify",
            Source::Wikidata => "wikidata",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "musicbrainz" => Source::MusicBrainz,
            "discogs" => Source::Discogs,
            "lastfm" => Source::Lastfm,
            "spotify" => Source::Spotify,
            "wikidata" => Source::Wikidata,
            _ => return None,
        })
    }
}

/// What flavour of tag this is; MusicBrainz distinguishes curated `genres` from
/// free-form `tags`, and they are not equally trustworthy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TagKind {
    /// Curated, voted vocabulary.
    Genre,
    /// Free-form folksonomy.
    Tag,
    /// Discogs "style" — narrower than its "genre".
    Style,
}

impl TagKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TagKind::Genre => "genre",
            TagKind::Tag => "tag",
            TagKind::Style => "style",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "genre" => TagKind::Genre,
            "tag" => TagKind::Tag,
            "style" => TagKind::Style,
            _ => return None,
        })
    }
}

/// A raw, unmodified observation from an upstream source. Never overwritten, so
/// the taxonomy can be reprocessed without touching the network.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TagSignal {
    pub entity_type: EntityType,
    pub entity_id: String,
    pub source: Source,
    pub raw_tag: String,
    /// Source-relative strength, normalised to 0..=1 by each client.
    pub weight: f64,
    pub kind: Option<TagKind>,
    /// RFC3339 UTC timestamp of when this signal was fetched.
    pub fetched_at: String,
}

// ------------------------------------------------------------------ Derived

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CanonicalGenre {
    pub slug: String,
    pub label: String,
    pub parent_slug: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackGenre {
    pub canonical_slug: String,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtistOrigin {
    pub artist_spotify_id: String,
    pub country_code: Option<String>,
    pub country_label: Option<String>,
    pub city: Option<String>,
    pub source: OriginSource,
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginSource {
    /// City of formation from MusicBrainz — the strongest signal.
    MbBeginArea,
    MbCountry,
    MbArea,
    Wikidata,
    UserOverride,
}

impl OriginSource {
    pub fn as_str(self) -> &'static str {
        match self {
            OriginSource::MbBeginArea => "mb_begin_area",
            OriginSource::MbCountry => "mb_country",
            OriginSource::MbArea => "mb_area",
            OriginSource::Wikidata => "wikidata",
            OriginSource::UserOverride => "user_override",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "mb_begin_area" => OriginSource::MbBeginArea,
            "mb_country" => OriginSource::MbCountry,
            "mb_area" => OriginSource::MbArea,
            "wikidata" => OriginSource::Wikidata,
            "user_override" => OriginSource::UserOverride,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TrackEra {
    pub track_spotify_id: String,
    pub year: Option<i32>,
    pub decade: Option<i32>,
    pub source: EraSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EraSource {
    /// MusicBrainz first release date — correct for reissues and remasters.
    MbFirstRelease,
    /// Spotify's album release date. Wrong whenever the album is a reissue.
    SpotifyReleaseDate,
    UserOverride,
}

impl EraSource {
    pub fn as_str(self) -> &'static str {
        match self {
            EraSource::MbFirstRelease => "mb_first_release",
            EraSource::SpotifyReleaseDate => "spotify_release_date",
            EraSource::UserOverride => "user_override",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "mb_first_release" => EraSource::MbFirstRelease,
            "spotify_release_date" => EraSource::SpotifyReleaseDate,
            "user_override" => EraSource::UserOverride,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub entity_type: String,
    pub entity_id: String,
    pub reason: String,
    pub detail: Option<String>,
    pub created_at: String,
}
