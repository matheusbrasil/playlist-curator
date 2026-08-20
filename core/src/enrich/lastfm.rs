//! Last.fm enrichment client.
//!
//! Fetches folksonomic genre tags from the Last.fm API. Tags are noisy but cover
//! artists and tracks that MusicBrainz may not have catalogued in detail.
//!
//! API rate limit: ~5 req/s for non-commercial keys. The governor in `ratelimit.rs`
//! is set to 4 req/s to stay safely under.
//!
//! Attribution: Last.fm data is used under the Last.fm API terms for non-commercial
//! personal use. See docs/DATA_SOURCES.md.

use crate::error::Result;
use crate::model::{EntityType, Source, TagSignal};
use crate::util::now_iso;
use serde_json::Value;
use url::form_urlencoded;

use super::fetch::Fetcher;
use super::ratelimit::Host;

const LASTFM_BASE: &str = "http://ws.audioscrobbler.com/2.0/";
/// Tags below this normalised weight are noise (e.g. a single person tagged it).
const MIN_WEIGHT: f64 = 0.05;

pub struct LastfmClient {
    pub fetcher: Fetcher,
    api_key: String,
}

impl LastfmClient {
    pub fn new(fetcher: Fetcher, api_key: String) -> Self {
        LastfmClient { fetcher, api_key }
    }

    /// Fetch the top genre tags for an artist.
    ///
    /// Tag count (0–100 in the API) is normalised to `weight = count / 100`.
    /// Tags below `MIN_WEIGHT` are dropped. `entity_id` is the Spotify artist ID
    /// so the signal joins via `tag_signal.entity_id`.
    pub async fn artist_top_tags(
        &self,
        artist_name: &str,
        artist_spotify_id: &str,
    ) -> Result<Vec<TagSignal>> {
        let name_enc: String =
            form_urlencoded::byte_serialize(artist_name.as_bytes()).collect();
        let url = format!(
            "{LASTFM_BASE}?method=artist.getTopTags&artist={name_enc}&api_key={}&format=json",
            self.api_key
        );

        let body = self
            .fetcher
            .get(Host::Lastfm, Source::Lastfm, &url)
            .await?;

        let v: Value = serde_json::from_str(&body)?;
        parse_tags(
            &v["toptags"]["tag"],
            EntityType::SpotifyArtist,
            artist_spotify_id,
        )
    }

    /// Fetch the top genre tags for a specific track.
    ///
    /// More specific than artist tags but also sparser; the weight formula is the
    /// same. `entity_id` is the Spotify track ID.
    pub async fn track_top_tags(
        &self,
        track_name: &str,
        artist_name: &str,
        track_spotify_id: &str,
    ) -> Result<Vec<TagSignal>> {
        let track_enc: String =
            form_urlencoded::byte_serialize(track_name.as_bytes()).collect();
        let artist_enc: String =
            form_urlencoded::byte_serialize(artist_name.as_bytes()).collect();
        let url = format!(
            "{LASTFM_BASE}?method=track.getTopTags&track={track_enc}&artist={artist_enc}&api_key={}&format=json",
            self.api_key
        );

        let body = self
            .fetcher
            .get(Host::Lastfm, Source::Lastfm, &url)
            .await?;

        let v: Value = serde_json::from_str(&body)?;
        parse_tags(
            &v["toptags"]["tag"],
            EntityType::Track,
            track_spotify_id,
        )
    }
}

fn parse_tags(
    tag_array: &Value,
    entity_type: EntityType,
    entity_id: &str,
) -> Result<Vec<TagSignal>> {
    let now = now_iso();
    let mut signals = Vec::new();

    let tags = match tag_array.as_array() {
        Some(arr) => arr,
        // Last.fm returns an empty object `{}` when there are no tags.
        None => return Ok(signals),
    };

    for tag in tags {
        let name = tag["name"].as_str().unwrap_or_default().trim();
        if name.is_empty() {
            continue;
        }
        // Last.fm count is 0-100; treat it as a percentage.
        let count = tag["count"]
            .as_u64()
            .or_else(|| tag["count"].as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0) as f64;
        let weight = (count / 100.0).clamp(0.0, 1.0);
        if weight < MIN_WEIGHT {
            continue;
        }
        signals.push(TagSignal {
            entity_type,
            entity_id: entity_id.to_string(),
            source: Source::Lastfm,
            raw_tag: name.to_string(),
            weight,
            fetched_at: now.clone(),
            kind: None,
        });
    }

    Ok(signals)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_artist_tags() {
        let tag_array = json!([
            {"name": "soul", "count": "100"},
            {"name": "funk", "count": "60"},
            {"name": "seen live", "count": "3"}  // below MIN_WEIGHT threshold
        ]);
        let signals =
            parse_tags(&tag_array, EntityType::SpotifyArtist, "spotify123").unwrap();
        // "seen live" weight = 0.03 < 0.05, so it's dropped.
        assert_eq!(signals.len(), 2);
        let soul = signals.iter().find(|s| s.raw_tag == "soul").unwrap();
        assert!((soul.weight - 1.0).abs() < 0.01);
        let funk = signals.iter().find(|s| s.raw_tag == "funk").unwrap();
        assert!((funk.weight - 0.6).abs() < 0.01);
    }

    #[test]
    fn handles_empty_toptags() {
        let signals =
            parse_tags(&json!({}), EntityType::SpotifyArtist, "spotify123").unwrap();
        assert!(signals.is_empty());
    }

    #[test]
    fn handles_empty_array() {
        let signals =
            parse_tags(&json!([]), EntityType::SpotifyArtist, "spotify123").unwrap();
        assert!(signals.is_empty());
    }

    #[test]
    fn uses_spotify_artist_entity_type() {
        let tag_array = json!([{"name": "bossa nova", "count": "80"}]);
        let signals =
            parse_tags(&tag_array, EntityType::SpotifyArtist, "sp_id").unwrap();
        assert_eq!(signals[0].entity_type, EntityType::SpotifyArtist);
        assert_eq!(signals[0].entity_id, "sp_id");
        assert_eq!(signals[0].source, Source::Lastfm);
    }
}
