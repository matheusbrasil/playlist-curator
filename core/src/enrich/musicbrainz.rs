//! MusicBrainz enrichment client.
//!
//! Implements the three-step cascade:
//!  1. ISRC lookup — deterministic, highest confidence.
//!  2. Spotify URL relationship — resolves the MB artist even when the track
//!     cannot be matched. Genre and origin are artist properties.
//!  3. Name-based search — scored fallback; used only when both of the above
//!     fail.

use crate::error::{CoreError, Result};
use crate::model::{EntityType, MbArtist, MbRecording, Source, TagSignal};
use crate::util::now_iso;
use serde_json::Value;
use url::form_urlencoded;

use super::fetch::Fetcher;
use super::ratelimit::Host;

const MB_BASE: &str = "https://musicbrainz.org/ws/2";
const SEARCH_SCORE_THRESHOLD: f64 = 0.50;

pub struct MusicBrainzClient {
    pub fetcher: Fetcher,
}

impl MusicBrainzClient {
    pub fn new(fetcher: Fetcher) -> Self {
        MusicBrainzClient { fetcher }
    }

    /// Look up a recording by ISRC.
    ///
    /// Returns the recording (with first_release_date taken from the earliest
    /// release in the response) and any genre/tag signals attached to it.
    /// Returns `None` if the ISRC is not in MusicBrainz.
    pub async fn resolve_isrc(
        &self,
        isrc: &str,
    ) -> Result<Option<(MbRecording, Vec<TagSignal>)>> {
        let url = format!(
            "{MB_BASE}/isrc/{isrc}?inc=artist-credits+genres+tags+releases&fmt=json"
        );
        let body = match self
            .fetcher
            .get(Host::MusicBrainz, Source::MusicBrainz, &url)
            .await
        {
            Ok(b) => b,
            Err(CoreError::Upstream { message, .. }) if message.contains("not found") => {
                return Ok(None)
            }
            Err(e) => return Err(e),
        };

        let v: Value = serde_json::from_str(&body)?;
        let recordings = match v["recordings"].as_array() {
            Some(arr) if !arr.is_empty() => arr,
            _ => return Ok(None),
        };

        let rec_json = &recordings[0];
        let mbid = rec_json["id"].as_str().unwrap_or_default().to_string();
        if mbid.is_empty() {
            return Ok(None);
        }

        // Pick the earliest first_release_date across all releases in the response.
        let first_release_date = earliest_release_date(rec_json);

        let recording = MbRecording {
            mbid: mbid.clone(),
            title: rec_json["title"].as_str().map(|s| s.to_string()),
            first_release_date,
            resolved_via: Some("isrc".into()),
            confidence: 1.0,
        };

        let now = now_iso();
        let mut signals = Vec::new();
        collect_mb_genres(rec_json, EntityType::MbRecording, &mbid, &now, &mut signals);
        collect_mb_tags(rec_json, EntityType::MbRecording, &mbid, &now, &mut signals);

        Ok(Some((recording, signals)))
    }

    /// Resolve a Spotify artist to their MusicBrainz entry via the URL relationship.
    ///
    /// This is the most valuable step: genre and origin are artist properties, and
    /// this path is deterministic (no fuzzy matching).
    pub async fn artist_by_spotify_url(
        &self,
        spotify_artist_id: &str,
    ) -> Result<Option<MbArtist>> {
        let spotify_url =
            format!("https://open.spotify.com/artist/{spotify_artist_id}");
        let encoded: String =
            form_urlencoded::byte_serialize(spotify_url.as_bytes()).collect();
        let url = format!(
            "{MB_BASE}/url?resource={encoded}&inc=artist-rels&target-type=artist&fmt=json"
        );

        let body = match self
            .fetcher
            .get(Host::MusicBrainz, Source::MusicBrainz, &url)
            .await
        {
            Ok(b) => b,
            Err(CoreError::Upstream { message, .. }) if message.contains("not found") => {
                return Ok(None)
            }
            Err(e) => return Err(e),
        };

        let v: Value = serde_json::from_str(&body)?;
        // Follow the first artist relation.
        let artist_mbid = v["relations"]
            .as_array()
            .and_then(|rels| {
                rels.iter().find_map(|rel| {
                    if rel["target-type"].as_str() == Some("artist") {
                        rel["artist"]["id"].as_str().map(|s| s.to_string())
                    } else {
                        None
                    }
                })
            });

        let mbid = match artist_mbid {
            Some(m) => m,
            None => return Ok(None),
        };

        self.fetch_artist_detail(&mbid).await.map(Some)
    }

    /// Fetch and return the full artist record for an MB artist MBID.
    pub async fn fetch_artist_detail(&self, artist_mbid: &str) -> Result<MbArtist> {
        let url = format!(
            "{MB_BASE}/artist/{artist_mbid}?inc=genres+tags+url-rels&fmt=json"
        );
        let body = self
            .fetcher
            .get(Host::MusicBrainz, Source::MusicBrainz, &url)
            .await?;
        let v: Value = serde_json::from_str(&body)?;
        Ok(mb_artist_from_json(artist_mbid, &v))
    }

    /// Collect the genre and tag signals for an MB artist.
    ///
    /// Uses the cached artist detail when available (fetch.rs caches every GET).
    pub async fn artist_tags(&self, artist_mbid: &str) -> Result<Vec<TagSignal>> {
        let url = format!(
            "{MB_BASE}/artist/{artist_mbid}?inc=genres+tags+url-rels&fmt=json"
        );
        let body = self
            .fetcher
            .get(Host::MusicBrainz, Source::MusicBrainz, &url)
            .await?;
        let v: Value = serde_json::from_str(&body)?;

        let now = now_iso();
        let mut signals = Vec::new();
        collect_mb_genres(&v, EntityType::MbArtist, artist_mbid, &now, &mut signals);
        collect_mb_tags(&v, EntityType::MbArtist, artist_mbid, &now, &mut signals);
        Ok(signals)
    }

    /// Name-based recording search — last resort after ISRC and URL-rel fail.
    ///
    /// Returns `(recording, confidence)` when the top hit scores ≥ 0.5, otherwise
    /// `None`. The score comes from the response itself; we do not add fuzzy logic
    /// on top of it.
    pub async fn search_recording(
        &self,
        track_name: &str,
        artist_name: &str,
    ) -> Result<Option<(MbRecording, f64)>> {
        let q = format!(
            "recording:\"{}\" AND artist:\"{}\"",
            track_name.replace('"', "\\\""),
            artist_name.replace('"', "\\\"")
        );
        let encoded: String = form_urlencoded::byte_serialize(q.as_bytes()).collect();
        let url = format!("{MB_BASE}/recording?query={encoded}&fmt=json&limit=1");

        let body = match self
            .fetcher
            .get(Host::MusicBrainz, Source::MusicBrainz, &url)
            .await
        {
            Ok(b) => b,
            Err(CoreError::Upstream { message, .. }) if message.contains("not found") => {
                return Ok(None)
            }
            Err(e) => return Err(e),
        };

        let v: Value = serde_json::from_str(&body)?;
        let recordings = match v["recordings"].as_array() {
            Some(arr) if !arr.is_empty() => arr,
            _ => return Ok(None),
        };

        let rec_json = &recordings[0];
        let score: f64 = rec_json["score"]
            .as_u64()
            .map(|s| s as f64 / 100.0)
            .unwrap_or(0.0);

        if score < SEARCH_SCORE_THRESHOLD {
            return Ok(None);
        }

        let mbid = match rec_json["id"].as_str() {
            Some(id) if !id.is_empty() => id.to_string(),
            _ => return Ok(None),
        };

        let first_release_date = earliest_release_date(rec_json);

        let recording = MbRecording {
            mbid,
            title: rec_json["title"].as_str().map(|s| s.to_string()),
            first_release_date,
            resolved_via: Some("name_search".into()),
            confidence: score,
        };

        Ok(Some((recording, score)))
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn earliest_release_date(rec_json: &Value) -> Option<String> {
    // `first-release-date` may be at top level (ISRC response) or in release array.
    if let Some(d) = rec_json["first-release-date"].as_str().filter(|s| !s.is_empty()) {
        return Some(d.to_string());
    }
    rec_json["releases"]
        .as_array()
        .and_then(|releases| {
            releases
                .iter()
                .filter_map(|r| r["date"].as_str().filter(|s| !s.is_empty()))
                .min()
                .map(|s| s.to_string())
        })
}

fn collect_mb_genres(
    v: &Value,
    entity_type: EntityType,
    entity_id: &str,
    now: &str,
    out: &mut Vec<TagSignal>,
) {
    if let Some(genres) = v["genres"].as_array() {
        for g in genres {
            let name = g["name"].as_str().unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            // MusicBrainz genres are voted; count normalised to weight 0..1.
            let count = g["count"].as_u64().unwrap_or(1) as f64;
            let weight = (count / 100.0).clamp(0.05, 1.0);
            out.push(TagSignal {
                entity_type,
                entity_id: entity_id.to_string(),
                source: Source::MusicBrainz,
                raw_tag: name.to_string(),
                weight,
                fetched_at: now.to_string(),
                kind: None,
            });
        }
    }
}

fn collect_mb_tags(
    v: &Value,
    entity_type: EntityType,
    entity_id: &str,
    now: &str,
    out: &mut Vec<TagSignal>,
) {
    if let Some(tags) = v["tags"].as_array() {
        for t in tags {
            let name = t["name"].as_str().unwrap_or_default();
            if name.is_empty() {
                continue;
            }
            let count = t["count"].as_u64().unwrap_or(1) as f64;
            // Tags are free-form and noisier than genres; base weight is lower.
            let weight = (count / 100.0 * 0.6).clamp(0.05, 0.6);
            out.push(TagSignal {
                entity_type,
                entity_id: entity_id.to_string(),
                source: Source::MusicBrainz,
                raw_tag: name.to_string(),
                weight,
                fetched_at: now.to_string(),
                kind: None,
            });
        }
    }
}

fn mb_artist_from_json(mbid: &str, v: &Value) -> MbArtist {
    // Wikidata QID is in url-rels.
    let wikidata_qid = v["relations"]
        .as_array()
        .and_then(|rels| {
            rels.iter().find_map(|rel| {
                if rel["type"].as_str() == Some("wikidata") {
                    rel["url"]["resource"]
                        .as_str()
                        .and_then(|u| u.rsplit('/').next())
                        .map(|q| q.to_string())
                } else {
                    None
                }
            })
        });

    MbArtist {
        mbid: mbid.to_string(),
        name: v["name"].as_str().map(|s| s.to_string()),
        sort_name: v["sort-name"].as_str().map(|s| s.to_string()),
        artist_type: v["type"].as_str().map(|s| s.to_string()),
        country: v["country"].as_str().map(|s| s.to_string()),
        area: v["area"]["name"].as_str().map(|s| s.to_string()),
        begin_area: v["begin-area"]["name"].as_str().map(|s| s.to_string()),
        begin_date: v["life-span"]["begin"].as_str().map(|s| s.to_string()),
        end_date: v["life-span"]["end"].as_str().map(|s| s.to_string()),
        wikidata_qid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_genres_from_isrc_response() {
        let v = json!({
            "id": "abc123",
            "title": "Test Track",
            "first-release-date": "1972-05-01",
            "genres": [{"name": "soul", "count": 80}, {"name": "funk", "count": 50}],
            "tags": []
        });
        let now = "2024-01-01T00:00:00Z";
        let mut signals = Vec::new();
        collect_mb_genres(&v, EntityType::MbRecording, "abc123", now, &mut signals);
        assert_eq!(signals.len(), 2);
        let soul = signals.iter().find(|s| s.raw_tag == "soul").unwrap();
        assert!((soul.weight - 0.8).abs() < 0.01);
    }

    #[test]
    fn picks_earliest_release_date() {
        let v = json!({
            "releases": [
                {"date": "2015-03-01"},
                {"date": "1972-05-01"},
                {"date": "1985-01-01"}
            ]
        });
        assert_eq!(earliest_release_date(&v), Some("1972-05-01".into()));
    }

    #[test]
    fn first_release_date_field_takes_priority() {
        let v = json!({
            "first-release-date": "1969-01-01",
            "releases": [{"date": "1970-01-01"}]
        });
        assert_eq!(earliest_release_date(&v), Some("1969-01-01".into()));
    }

    #[test]
    fn extracts_wikidata_qid_from_url_rels() {
        let v = json!({
            "relations": [
                {"type": "wikidata", "url": {"resource": "https://www.wikidata.org/wiki/Q12345"}},
                {"type": "discogs", "url": {"resource": "https://www.discogs.com/artist/1"}}
            ]
        });
        let artist = mb_artist_from_json("mbid1", &v);
        assert_eq!(artist.wikidata_qid, Some("Q12345".into()));
    }

    #[test]
    fn handles_missing_wikidata_qid() {
        let v = json!({ "relations": [] });
        let artist = mb_artist_from_json("mbid1", &v);
        assert!(artist.wikidata_qid.is_none());
    }
}
