//! Spotify Web API client: authenticated requests, pagination, retry/backoff.
//!
//! Only the endpoints that survived the 2024–2026 restrictions are used. Notably
//! absent, because they are gone for new apps: `audio-features`, `recommendations`,
//! `related-artists`, and the batch `GET /tracks` / `GET /albums` endpoints.

use super::auth::Session;
use super::models::*;
use crate::config::APP_USER_AGENT;
use crate::error::{CoreError, Result};
use std::time::Duration;

const API_BASE: &str = "https://api.spotify.com/v1";

/// Playlist items come back 100 at a time — the documented maximum.
const PAGE_LIMIT: usize = 100;

/// Spotify caps additions at 100 URIs per request.
pub const ADD_ITEMS_BATCH: usize = 100;

/// How many times to retry a transient failure before giving up.
const MAX_RETRIES: u32 = 4;

pub struct SpotifyClient {
    http: reqwest::Client,
    session: std::sync::Arc<Session>,
}

impl SpotifyClient {
    pub fn new(http: reqwest::Client, session: std::sync::Arc<Session>) -> Self {
        SpotifyClient { http, session }
    }

    /// Build the shared HTTP client. One instance keeps the connection pool warm
    /// across every host the app talks to.
    pub fn build_http() -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .user_agent(APP_USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()?)
    }

    // ------------------------------------------------------------ Reads

    pub async fn me(&self) -> Result<SpotifyUser> {
        self.get_json(&format!("{API_BASE}/me")).await
    }

    /// Every playlist the user can see, following pagination to the end.
    pub async fn my_playlists(&self) -> Result<Vec<SimplePlaylist>> {
        self.get_all_pages(&format!("{API_BASE}/me/playlists?limit=50"))
            .await
    }

    pub async fn playlist(&self, playlist_id: &str) -> Result<SimplePlaylist> {
        self.get_json(&format!("{API_BASE}/playlists/{playlist_id}"))
            .await
    }

    /// All items of a playlist.
    ///
    /// Uses `/items` rather than the pre-2026 `/tracks` path, and requests the
    /// `external_ids` field explicitly so the ISRC is included — without it the
    /// whole MusicBrainz match cascade loses its deterministic key.
    pub async fn playlist_items(&self, playlist_id: &str) -> Result<Vec<PlaylistItem>> {
        let fields = "items(added_at,is_local,track(id,name,duration_ms,type,is_local,\
                      external_ids(isrc),album(id,name,release_date,release_date_precision),\
                      artists(id,name))),next,total";
        let url = format!(
            "{API_BASE}/playlists/{playlist_id}/items?limit={PAGE_LIMIT}&fields={}",
            urlencode(fields)
        );
        self.get_all_pages(&url).await
    }

    /// Fetch artists one at a time. The batch `GET /artists?ids=` endpoint was
    /// among those removed, so this is unavoidably N requests.
    pub async fn artist(&self, artist_id: &str) -> Result<FullArtist> {
        self.get_json(&format!("{API_BASE}/artists/{artist_id}"))
            .await
    }

    // ------------------------------------------------------------ Writes

    /// Create an empty playlist owned by `user_id`. Private by default: this app
    /// should never publish anything the user did not ask it to.
    pub async fn create_playlist(
        &self,
        user_id: &str,
        name: &str,
        description: &str,
        public: bool,
    ) -> Result<CreatedPlaylist> {
        let body = serde_json::json!({
            "name": name,
            "description": description,
            "public": public,
        });
        self.post_json(&format!("{API_BASE}/users/{user_id}/playlists"), &body)
            .await
    }

    /// Append tracks in batches of at most 100.
    pub async fn add_items(&self, playlist_id: &str, uris: &[String]) -> Result<()> {
        for chunk in uris.chunks(ADD_ITEMS_BATCH) {
            let body = serde_json::json!({ "uris": chunk });
            let _: serde_json::Value = self
                .post_json(&format!("{API_BASE}/playlists/{playlist_id}/items"), &body)
                .await?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ Plumbing

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T> {
        let body = self.request_with_retry(reqwest::Method::GET, url, None).await?;
        Ok(serde_json::from_str(&body)?)
    }

    async fn post_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let text = self
            .request_with_retry(reqwest::Method::POST, url, Some(body.clone()))
            .await?;
        // Some write endpoints answer 201 with an empty body.
        if text.trim().is_empty() {
            return Ok(serde_json::from_str("null")?);
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Walk a paginated collection, following `next` until it is null.
    async fn get_all_pages<T: serde::de::DeserializeOwned>(&self, first_url: &str) -> Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(first_url.to_string());
        // `next` is server-supplied; bound the walk so a malformed response
        // cannot spin forever.
        let mut pages = 0;
        while let Some(url) = next {
            if pages > 1_000 {
                return Err(CoreError::other("pagination exceeded 1000 pages; aborting"));
            }
            pages += 1;
            let page: Page<T> = self.get_json(&url).await?;
            out.extend(page.items);
            next = page.next;
        }
        Ok(out)
    }

    /// Issue a request, refreshing the token as needed and honouring 429.
    async fn request_with_retry(
        &self,
        method: reqwest::Method,
        url: &str,
        body: Option<serde_json::Value>,
    ) -> Result<String> {
        let mut attempt = 0;
        loop {
            let token = self.session.access_token().await?;
            let mut req = self
                .http
                .request(method.clone(), url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json");
            if let Some(ref b) = body {
                req = req.json(b);
            }

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) if attempt < MAX_RETRIES && (e.is_timeout() || e.is_connect()) => {
                    attempt += 1;
                    let delay = backoff_delay(attempt);
                    tracing::warn!(attempt, ?delay, error = %e, "transport error; retrying");
                    tokio::time::sleep(delay).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };

            let status = resp.status();
            if status.is_success() {
                return Ok(resp.text().await?);
            }

            // Spotify tells us exactly how long to wait; obey it rather than
            // guessing, and treat a missing header as one second.
            let retry_after = resp
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok());
            let text = resp.text().await.unwrap_or_default();

            if status.as_u16() == 429 {
                // A per-developer-account quota exhaustion is not transient:
                // retrying just burns the remaining allowance.
                if is_quota_exceeded(&text) {
                    tracing::error!("Spotify reports QUOTA_EXCEEDED for this developer account");
                    return Err(CoreError::QuotaExceeded);
                }
                if attempt < MAX_RETRIES {
                    attempt += 1;
                    let delay = Duration::from_secs(retry_after.unwrap_or(1).min(120));
                    tracing::warn!(attempt, ?delay, "rate limited; waiting as instructed");
                    tokio::time::sleep(delay).await;
                    continue;
                }
            }

            if status.as_u16() == 401 {
                // The token was rejected despite looking fresh. One retry after a
                // forced refresh distinguishes a clock skew from a revoked grant.
                if attempt < 1 {
                    attempt += 1;
                    tracing::warn!("got 401; forcing a token refresh and retrying once");
                    self.session.force_expire().await;
                    continue;
                }
                return Err(CoreError::NotAuthenticated);
            }

            if status.is_server_error() && attempt < MAX_RETRIES {
                attempt += 1;
                let delay = backoff_delay(attempt);
                tracing::warn!(attempt, status = status.as_u16(), ?delay, "server error; retrying");
                tokio::time::sleep(delay).await;
                continue;
            }

            return Err(CoreError::SpotifyApi {
                status: status.as_u16(),
                body: truncate(&text, 500),
            });
        }
    }
}

/// Exponential backoff: 1s, 2s, 4s, 8s, capped at 32s.
fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.clamp(1, 6) - 1;
    Duration::from_secs(1u64 << shift)
}

/// Whether a 429 body carries `reason: "QUOTA_EXCEEDED"`.
pub fn is_quota_exceeded(body: &str) -> bool {
    serde_json::from_str::<ApiErrorEnvelope>(body)
        .ok()
        .and_then(|e| e.error.reason)
        .is_some_and(|r| r.eq_ignore_ascii_case("QUOTA_EXCEEDED"))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    // Cut on a character boundary, not a byte index.
    let end = s
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= max)
        .last()
        .unwrap_or(0);
    format!("{}…", &s[..end])
}

fn urlencode(s: &str) -> String {
    // Percent-encode everything outside the unreserved set. `form_urlencoded`
    // would turn spaces into `+`, which Spotify's `fields` parser rejects.
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `spotify:track:<id>` URI for the playlist-add payload.
pub fn track_uri(track_id: &str) -> String {
    format!("spotify:track:{track_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_quota_exceeded_only_for_that_reason() {
        assert!(is_quota_exceeded(
            r#"{"error":{"status":429,"reason":"QUOTA_EXCEEDED","message":"x"}}"#
        ));
        // Ordinary rate limiting is retryable and must not be mistaken for it.
        assert!(!is_quota_exceeded(r#"{"error":{"status":429,"message":"slow down"}}"#));
        assert!(!is_quota_exceeded(
            r#"{"error":{"status":429,"reason":"RATE_LIMIT"}}"#
        ));
        assert!(!is_quota_exceeded("not json"));
        assert!(!is_quota_exceeded(""));
    }

    #[test]
    fn backoff_grows_exponentially_and_is_bounded() {
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(4), Duration::from_secs(8));
        // Never grows without limit.
        assert!(backoff_delay(50) <= Duration::from_secs(32));
    }

    #[test]
    fn urlencode_escapes_spaces_and_parens_not_plus() {
        // Spotify's `fields` parser needs %20, not '+'.
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("track(id,name)"), "track%28id%2Cname%29");
        assert_eq!(urlencode("aA0-_.~"), "aA0-_.~");
    }

    #[test]
    fn builds_track_uris() {
        assert_eq!(track_uri("4uLU6hMCjMI"), "spotify:track:4uLU6hMCjMI");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "a".repeat(600);
        assert!(truncate(&s, 500).len() <= 504);
        assert_eq!(truncate("short", 500), "short");
        // Multi-byte input must not panic mid-character.
        let unicode = "é".repeat(400);
        let _ = truncate(&unicode, 500);
    }

    #[test]
    fn batch_size_matches_spotify_limit() {
        assert_eq!(ADD_ITEMS_BATCH, 100);
    }
}
