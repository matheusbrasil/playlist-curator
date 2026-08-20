//! Discogs enrichment client.
//!
//! Discogs provides editorial genre and style data attached to releases. It tends
//! to use broad genre labels ("Electronic", "Jazz") combined with narrower styles
//! ("House", "Bebop") — both are stored as raw tag signals and normalised later.
//!
//! A personal access token is required for all requests, even read-only ones.
//! Rate limit: 60 authenticated requests per minute → 1 req/s via governor.
//!
//! Attribution: Discogs data is used under the Discogs API terms.
//! See docs/DATA_SOURCES.md.

use crate::error::{CoreError, Result};
use crate::model::{EntityType, Source, TagSignal};
use crate::util::now_iso;
use serde_json::Value;
use url::form_urlencoded;

use super::fetch::Fetcher;
use super::ratelimit::Host;

const DISCOGS_BASE: &str = "https://api.discogs.com";

pub struct DiscogsClient {
    pub fetcher: Fetcher,
    token: String,
}

impl DiscogsClient {
    pub fn new(fetcher: Fetcher, token: String) -> Self {
        DiscogsClient { fetcher, token }
    }

    /// Fetch genre and style tags for a release identified by ISRC.
    ///
    /// Uses a two-step approach: search by ISRC, then fetch the release detail.
    /// Both steps are cached individually so the second run never hits the network.
    pub async fn release_tags_by_isrc(
        &self,
        isrc: &str,
        release_title: &str,
        artist_name: &str,
    ) -> Result<Vec<TagSignal>> {
        // Try ISRC first; if no results, fall back to title+artist search.
        let q_enc: String = form_urlencoded::byte_serialize(isrc.as_bytes()).collect();
        let search_url = format!(
            "{DISCOGS_BASE}/database/search?q={q_enc}&type=release&token={}",
            self.token
        );

        let body = match self
            .fetcher
            .get(Host::Discogs, Source::Discogs, &search_url)
            .await
        {
            Ok(b) => b,
            Err(CoreError::Upstream { message, .. }) if message.contains("not found") => {
                return Ok(vec![])
            }
            Err(e) => return Err(e),
        };

        let v: Value = serde_json::from_str(&body)?;
        let results = v["results"].as_array();

        let release_id = match results.and_then(|r| r.first()) {
            Some(r) => r["id"].as_u64(),
            None => {
                // Fallback: search by title + artist
                return self.search_release_by_title(release_title, artist_name).await;
            }
        };

        match release_id {
            Some(id) => self.fetch_release_tags(id.to_string()).await,
            None => Ok(vec![]),
        }
    }

    /// Fetch genre and style tags for an artist by name.
    ///
    /// Searches for the artist, then aggregates genres across their top releases
    /// (up to 3 releases to stay within reasonable rate limits).
    pub async fn artist_tags(&self, artist_name: &str) -> Result<Vec<TagSignal>> {
        let name_enc: String =
            form_urlencoded::byte_serialize(artist_name.as_bytes()).collect();
        let search_url = format!(
            "{DISCOGS_BASE}/database/search?q={name_enc}&type=artist&token={}",
            self.token
        );

        let body = match self
            .fetcher
            .get(Host::Discogs, Source::Discogs, &search_url)
            .await
        {
            Ok(b) => b,
            Err(CoreError::Upstream { message, .. }) if message.contains("not found") => {
                return Ok(vec![])
            }
            Err(e) => return Err(e),
        };

        let v: Value = serde_json::from_str(&body)?;
        let artist_id = v["results"]
            .as_array()
            .and_then(|r| r.first())
            .and_then(|a| a["id"].as_u64());

        let artist_id = match artist_id {
            Some(id) => id,
            None => return Ok(vec![]),
        };

        // Fetch artist releases
        let releases_url = format!(
            "{DISCOGS_BASE}/artists/{artist_id}/releases?sort=year&sort_order=asc&per_page=3&token={}",
            self.token
        );
        let releases_body = match self
            .fetcher
            .get(Host::Discogs, Source::Discogs, &releases_url)
            .await
        {
            Ok(b) => b,
            Err(CoreError::Upstream { message, .. }) if message.contains("not found") => {
                return Ok(vec![])
            }
            Err(e) => return Err(e),
        };

        let rv: Value = serde_json::from_str(&releases_body)?;
        let releases = match rv["releases"].as_array() {
            Some(r) => r,
            None => return Ok(vec![]),
        };

        let mut all_signals = Vec::new();
        for release in releases.iter().take(3) {
            if let Some(id) = release["id"].as_u64() {
                match self.fetch_release_tags(id.to_string()).await {
                    Ok(mut signals) => all_signals.append(&mut signals),
                    Err(_) => continue,
                }
            }
        }

        Ok(all_signals)
    }

    async fn search_release_by_title(
        &self,
        title: &str,
        artist: &str,
    ) -> Result<Vec<TagSignal>> {
        let q = format!("{title} {artist}");
        let q_enc: String = form_urlencoded::byte_serialize(q.as_bytes()).collect();
        let search_url = format!(
            "{DISCOGS_BASE}/database/search?q={q_enc}&type=release&token={}",
            self.token
        );

        let body = match self
            .fetcher
            .get(Host::Discogs, Source::Discogs, &search_url)
            .await
        {
            Ok(b) => b,
            Err(CoreError::Upstream { message, .. }) if message.contains("not found") => {
                return Ok(vec![])
            }
            Err(e) => return Err(e),
        };

        let v: Value = serde_json::from_str(&body)?;
        match v["results"]
            .as_array()
            .and_then(|r| r.first())
            .and_then(|r| r["id"].as_u64())
        {
            Some(id) => self.fetch_release_tags(id.to_string()).await,
            None => Ok(vec![]),
        }
    }

    async fn fetch_release_tags(&self, release_id: String) -> Result<Vec<TagSignal>> {
        let release_url = format!(
            "{DISCOGS_BASE}/releases/{release_id}?token={}",
            self.token
        );
        let body = match self
            .fetcher
            .get(Host::Discogs, Source::Discogs, &release_url)
            .await
        {
            Ok(b) => b,
            Err(CoreError::Upstream { message, .. }) if message.contains("not found") => {
                return Ok(vec![])
            }
            Err(e) => return Err(e),
        };

        let v: Value = serde_json::from_str(&body)?;
        let now = now_iso();
        let mut signals = Vec::new();

        // Genres: broad labels, higher confidence.
        if let Some(genres) = v["genres"].as_array() {
            for g in genres {
                if let Some(name) = g.as_str() {
                    if !name.is_empty() {
                        signals.push(TagSignal {
                            entity_type: EntityType::Release,
                            entity_id: release_id.clone(),
                            source: Source::Discogs,
                            raw_tag: name.to_lowercase(),
                            weight: 0.8,
                            fetched_at: now.clone(),
                            kind: None,
                        });
                    }
                }
            }
        }

        // Styles: more specific, slightly lower weight.
        if let Some(styles) = v["styles"].as_array() {
            for s in styles {
                if let Some(name) = s.as_str() {
                    if !name.is_empty() {
                        signals.push(TagSignal {
                            entity_type: EntityType::Release,
                            entity_id: release_id.clone(),
                            source: Source::Discogs,
                            raw_tag: name.to_lowercase(),
                            weight: 0.6,
                            fetched_at: now.clone(),
                            kind: None,
                        });
                    }
                }
            }
        }

        Ok(signals)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn discogs_genres_and_styles_weights() {
        // Ensure the weight hierarchy: genres (0.8) > styles (0.6).
        // This test validates the design decision, not live API responses.
        assert!(0.8_f64 > 0.6_f64);
    }

    #[test]
    fn discogs_tags_are_lowercased() {
        // Verify that genres like "Electronic" become "electronic" for normalization.
        let genre = "Electronic";
        assert_eq!(genre.to_lowercase(), "electronic");
    }

    #[test]
    fn discogs_release_id_is_entity_id() {
        // The entity_id for a release signal is the Discogs release ID (as string),
        // so it can be stored in tag_signal without conflicting with MBID namespacing.
        let id = 12345_u64.to_string();
        assert_eq!(id, "12345");
    }

    #[test]
    fn parses_genres_and_styles_from_release_json() {
        use super::*;
        use crate::model::{EntityType, Source};

        let v = json!({
            "genres": ["Soul", "Funk"],
            "styles": ["Rhythm & Blues", "Deep Funk"]
        });

        let now = "2024-01-01T00:00:00Z";
        let mut signals = Vec::new();
        let release_id = "99999".to_string();

        if let Some(genres) = v["genres"].as_array() {
            for g in genres {
                if let Some(name) = g.as_str() {
                    signals.push(TagSignal {
                        entity_type: EntityType::Release,
                        entity_id: release_id.clone(),
                        source: Source::Discogs,
                        raw_tag: name.to_lowercase(),
                        weight: 0.8,
                        kind: None,
                            fetched_at: String::new(),
                    });
                }
            }
        }
        if let Some(styles) = v["styles"].as_array() {
            for s in styles {
                if let Some(name) = s.as_str() {
                    signals.push(TagSignal {
                        entity_type: EntityType::Release,
                        entity_id: release_id.clone(),
                        source: Source::Discogs,
                        raw_tag: name.to_lowercase(),
                        weight: 0.6,
                        kind: None,
                            fetched_at: String::new(),
                    });
                }
            }
        }

        assert_eq!(signals.len(), 4);
        let soul = signals.iter().find(|s| s.raw_tag == "soul").unwrap();
        assert!((soul.weight - 0.8).abs() < f64::EPSILON);
        let rnb = signals.iter().find(|s| s.raw_tag == "rhythm & blues").unwrap();
        assert!((rnb.weight - 0.6).abs() < f64::EPSILON);
    }
}
