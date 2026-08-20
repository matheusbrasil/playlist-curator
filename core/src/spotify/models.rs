//! Wire types for the Spotify Web API.
//!
//! Every field is optional or defaulted where Spotify might omit it. The 2026
//! Development Mode surface removed `popularity` and `available_markets`, and
//! playlist items moved from `/tracks` to `/items`; deserialisation is kept
//! permissive so a further field removal degrades rather than breaks the import.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SpotifyUser {
    pub id: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    /// `premium` or `free`. Development Mode requires the app owner to hold
    /// Premium, so this is worth surfacing in the UI.
    #[serde(default)]
    pub product: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Page<T> {
    #[serde(default = "Vec::new")]
    pub items: Vec<T>,
    /// Absolute URL of the next page, or null on the last page.
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub total: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimplePlaylist {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub owner: Option<PlaylistOwner>,
    #[serde(default)]
    pub snapshot_id: Option<String>,
    #[serde(default)]
    pub tracks: Option<TracksRef>,
    #[serde(default)]
    pub public: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistOwner {
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TracksRef {
    #[serde(default)]
    pub total: Option<i64>,
}

/// One entry in a playlist. `track` is null for items Spotify can no longer
/// resolve, and is an episode object when the playlist mixes in podcasts —
/// both are skipped by the importer.
#[derive(Debug, Clone, Deserialize)]
pub struct PlaylistItem {
    #[serde(default)]
    pub added_at: Option<String>,
    #[serde(default)]
    pub track: Option<FullTrack>,
    #[serde(default)]
    pub is_local: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FullTrack {
    /// Null for local files, which have no Spotify identity at all.
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<i64>,
    #[serde(default)]
    pub album: Option<SimpleAlbum>,
    #[serde(default)]
    pub artists: Vec<SimpleArtist>,
    /// Carries the ISRC — the single most valuable field for this app, since it
    /// is the deterministic join key into MusicBrainz.
    #[serde(default)]
    pub external_ids: Option<ExternalIds>,
    #[serde(default)]
    pub is_local: Option<bool>,
    /// `track` or `episode`.
    #[serde(default, rename = "type")]
    pub item_type: Option<String>,
}

impl FullTrack {
    pub fn isrc(&self) -> Option<&str> {
        self.external_ids
            .as_ref()
            .and_then(|e| e.isrc.as_deref())
            .filter(|s| !s.is_empty())
    }

    pub fn is_episode(&self) -> bool {
        self.item_type.as_deref() == Some("episode")
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalIds {
    #[serde(default)]
    pub isrc: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimpleAlbum {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// The date of *this* release. For a reissue it is the reissue date, which
    /// is why era classification uses MusicBrainz instead.
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub release_date_precision: Option<String>,
    #[serde(default)]
    pub album_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SimpleArtist {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// Full artist object. `genres` here is Spotify's own invented taxonomy — kept
/// only as a last-resort signal at weight 0.2.
#[derive(Debug, Clone, Deserialize)]
pub struct FullArtist {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatedPlaylist {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub external_urls: Option<ExternalUrls>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExternalUrls {
    #[serde(default)]
    pub spotify: Option<String>,
}

/// Spotify's error envelope: `{"error":{"status":429,"message":"...","reason":"QUOTA_EXCEEDED"}}`
#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorEnvelope {
    pub error: ApiError,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub message: Option<String>,
    /// `QUOTA_EXCEEDED` means the developer account's quota is spent, not that
    /// this one request was too fast — retrying will not help.
    #[serde(default)]
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playlist_item_with_isrc() {
        let json = r#"{
          "added_at": "2021-03-04T10:00:00Z",
          "is_local": false,
          "track": {
            "id": "4uLU6hMCjMI75M1A2tKUQC",
            "name": "Azul da Cor do Mar",
            "duration_ms": 224000,
            "type": "track",
            "album": { "id": "alb1", "name": "Tim Maia", "release_date": "2015-06-01" },
            "artists": [{ "id": "art1", "name": "Tim Maia" }],
            "external_ids": { "isrc": "BRRCA7200015" }
          }
        }"#;
        let item: PlaylistItem = serde_json::from_str(json).unwrap();
        let track = item.track.unwrap();
        assert_eq!(track.isrc(), Some("BRRCA7200015"));
        assert!(!track.is_episode());
        assert_eq!(track.artists[0].name.as_deref(), Some("Tim Maia"));
    }

    #[test]
    fn tolerates_item_with_null_track() {
        // Spotify returns a null track for entries it can no longer resolve.
        let item: PlaylistItem =
            serde_json::from_str(r#"{"added_at":null,"track":null}"#).unwrap();
        assert!(item.track.is_none());
    }

    #[test]
    fn tolerates_missing_isrc_and_missing_external_ids() {
        let t: FullTrack = serde_json::from_str(
            r#"{"id":"x","name":"y","external_ids":{}}"#,
        ).unwrap();
        assert_eq!(t.isrc(), None);

        let t2: FullTrack = serde_json::from_str(r#"{"id":"x","name":"y"}"#).unwrap();
        assert_eq!(t2.isrc(), None);

        // An empty-string ISRC is as useless as a missing one.
        let t3: FullTrack = serde_json::from_str(
            r#"{"id":"x","external_ids":{"isrc":""}}"#,
        ).unwrap();
        assert_eq!(t3.isrc(), None);
    }

    #[test]
    fn identifies_local_file_without_id() {
        let item: PlaylistItem = serde_json::from_str(
            r#"{"is_local":true,"track":{"id":null,"name":"My Rip","is_local":true}}"#,
        ).unwrap();
        let t = item.track.unwrap();
        assert!(t.id.is_none());
        assert_eq!(t.is_local, Some(true));
    }

    #[test]
    fn identifies_podcast_episode() {
        let t: FullTrack =
            serde_json::from_str(r#"{"id":"ep1","name":"Ep 1","type":"episode"}"#).unwrap();
        assert!(t.is_episode());
    }

    #[test]
    fn parses_page_without_optional_fields() {
        let p: Page<SimplePlaylist> = serde_json::from_str(r#"{"items":[]}"#).unwrap();
        assert!(p.items.is_empty());
        assert!(p.next.is_none());
    }

    #[test]
    fn detects_quota_exceeded_reason() {
        let env: ApiErrorEnvelope = serde_json::from_str(
            r#"{"error":{"status":429,"message":"rate limited","reason":"QUOTA_EXCEEDED"}}"#,
        ).unwrap();
        assert_eq!(env.error.reason.as_deref(), Some("QUOTA_EXCEEDED"));
    }
}
