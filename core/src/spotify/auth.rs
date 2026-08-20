//! Spotify OAuth 2.0 Authorization Code flow with PKCE.
//!
//! Spotify treats a desktop app as a *public* client: there is no client secret
//! to keep, so PKCE is mandatory. HTTP redirect URIs are accepted only on a
//! literal loopback address — `http://127.0.0.1:14523/callback` works,
//! `http://localhost:14523/callback` is rejected outright.
//!
//! Tokens never reach the webview. They live in the OS credential vault (or a
//! 0600 file where no vault exists) and are attached to requests inside Rust.

use crate::config::{Settings, OAUTH_PORT, OAUTH_REDIRECT_URI, USER_AGENT};
use crate::error::{CoreError, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

const AUTH_URL: &str = "https://accounts.spotify.com/authorize";
const TOKEN_URL: &str = "https://accounts.spotify.com/api/token";

/// Scopes required. Read scopes cover listing and reading the user's playlists;
/// modify scopes cover creating new ones. `user-read-private` is needed because
/// playlist creation requires the user id.
pub const SCOPES: &[&str] = &[
    "user-read-private",
    "playlist-read-private",
    "playlist-read-collaborative",
    "playlist-modify-private",
    "playlist-modify-public",
];

/// Refresh this many seconds before actual expiry, so a long request cannot
/// start with a valid token and finish with an expired one.
const EXPIRY_SKEW_SECS: i64 = 60;

// ------------------------------------------------------------------ PKCE

/// A PKCE verifier/challenge pair. The verifier is held in memory only for the
/// duration of one login.
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// Generate a fresh pair. RFC 7636 allows 43–128 characters from the
    /// unreserved set; 64 random alphanumerics sits comfortably inside that.
    pub fn generate() -> Self {
        use rand::distributions::Alphanumeric;
        use rand::Rng;
        let verifier: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(64)
            .map(char::from)
            .collect();
        let challenge = Self::challenge_for(&verifier);
        Pkce { verifier, challenge }
    }

    /// `BASE64URL-ENCODE(SHA256(ASCII(verifier)))`, without padding.
    pub fn challenge_for(verifier: &str) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(verifier.as_bytes());
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    }
}

/// Random opaque value echoed back by the authorization server; mismatch means
/// the callback did not originate from our request.
fn random_state() -> String {
    use rand::distributions::Alphanumeric;
    use rand::Rng;
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}

/// An in-flight login: the URL to open plus the secrets needed to complete it.
#[derive(Debug, Clone)]
pub struct PendingAuth {
    pub authorize_url: String,
    pub pkce: Pkce,
    pub state: String,
}

/// Build the authorize URL for `client_id`.
pub fn begin(client_id: &str) -> PendingAuth {
    let pkce = Pkce::generate();
    let state = random_state();
    let mut url = url::Url::parse(AUTH_URL).expect("static AUTH_URL parses");
    url.query_pairs_mut()
        .append_pair("client_id", client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", OAUTH_REDIRECT_URI)
        .append_pair("code_challenge_method", "S256")
        .append_pair("code_challenge", &pkce.challenge)
        .append_pair("state", &state)
        .append_pair("scope", &SCOPES.join(" "));
    PendingAuth {
        authorize_url: url.to_string(),
        pkce,
        state,
    }
}

// ------------------------------------------------------------------ Loopback callback

/// Parse the `code`/`state`/`error` triple out of a callback request target such
/// as `/callback?code=abc&state=xyz`.
pub fn parse_callback_query(request_url: &str) -> Result<CallbackParams> {
    // `request_url` is a path+query, so give it a base to resolve against.
    let base = url::Url::parse("http://127.0.0.1/").expect("static base parses");
    let parsed = base
        .join(request_url)
        .map_err(|e| CoreError::Oauth(format!("unparsable callback url: {e}")))?;

    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in parsed.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    Ok(CallbackParams { code, state, error })
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// Block on the loopback listener until Spotify redirects the browser back, then
/// return the authorization code.
///
/// Runs on a blocking thread (`tiny_http` is synchronous). `timeout` bounds the
/// wait so a user who abandons the browser tab does not leak the thread.
pub fn wait_for_callback(expected_state: &str, timeout: Duration) -> Result<String> {
    wait_for_callback_on(OAUTH_PORT, expected_state, timeout)
}

/// As [`wait_for_callback`], on an explicit port. Production must use
/// [`OAUTH_PORT`] because that is what the redirect URI registers; the parameter
/// exists so tests can bind non-conflicting ports.
pub fn wait_for_callback_on(port: u16, expected_state: &str, timeout: Duration) -> Result<String> {
    let server = tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| {
        CoreError::Oauth(format!(
            "cannot bind loopback listener on 127.0.0.1:{port}: {e}"
        ))
    })?;
    let bound_port = server
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .unwrap_or(port);
    tracing::debug!(port = bound_port, "waiting for OAuth callback");
    let deadline = std::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(CoreError::Oauth("timed out waiting for the browser callback".into()));
        }
        let Some(request) = server.recv_timeout(remaining)? else {
            continue;
        };

        // The browser also asks for /favicon.ico; ignore anything but the
        // registered callback path.
        let target = request.url().to_string();
        if !target.starts_with("/callback") {
            let _ = request.respond(tiny_http::Response::empty(404));
            continue;
        }

        let params = parse_callback_query(&target)?;
        let outcome = match (&params.error, &params.code, &params.state) {
            (Some(err), _, _) => Err(CoreError::Oauth(format!("Spotify denied the request: {err}"))),
            (_, _, state) if state.as_deref() != Some(expected_state) => Err(CoreError::Oauth(
                "state mismatch — the callback did not come from this login attempt".into(),
            )),
            (_, Some(code), _) => Ok(code.clone()),
            _ => Err(CoreError::Oauth("callback carried neither code nor error".into())),
        };

        let body = match &outcome {
            Ok(_) => "<html><body><h2>Connected.</h2><p>You can close this tab and return to Playlist Curator.</p></body></html>",
            Err(_) => "<html><body><h2>Login failed.</h2><p>Return to Playlist Curator for details.</p></body></html>",
        };
        let mut response = tiny_http::Response::from_string(body);
        if let Ok(header) = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]) {
            response.add_header(header);
        }
        let _ = request.respond(response);
        return outcome;
    }
}

// ------------------------------------------------------------------ Tokens

#[derive(Debug, Clone, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default)]
    scope: Option<String>,
}

/// Persisted credential set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Absolute expiry as a unix timestamp, so a restart does not reset the clock.
    pub expires_at: i64,
    #[serde(default)]
    pub scope: Option<String>,
}

impl Tokens {
    fn from_response(resp: TokenResponse, previous_refresh: Option<String>) -> Self {
        let expires_in = resp.expires_in.unwrap_or(3600);
        Tokens {
            access_token: resp.access_token,
            // A refresh grant may omit the refresh token, meaning "keep using
            // the one you have".
            refresh_token: resp.refresh_token.or(previous_refresh),
            expires_at: unix_now() + expires_in,
            scope: resp.scope,
        }
    }

    pub fn is_expired(&self) -> bool {
        unix_now() + EXPIRY_SKEW_SECS >= self.expires_at
    }
}

fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    http: &reqwest::Client,
    client_id: &str,
    code: &str,
    verifier: &str,
) -> Result<Tokens> {
    let form = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", OAUTH_REDIRECT_URI),
        ("client_id", client_id),
        ("code_verifier", verifier),
    ];
    let resp = post_token(http, &form).await?;
    Ok(Tokens::from_response(resp, None))
}

/// Trade a refresh token for a fresh access token.
pub async fn refresh(
    http: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<Tokens> {
    let form = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", client_id),
    ];
    let resp = post_token(http, &form).await?;
    Ok(Tokens::from_response(resp, Some(refresh_token.to_string())))
}

async fn post_token(http: &reqwest::Client, form: &[(&str, &str)]) -> Result<TokenResponse> {
    let resp = http
        .post(TOKEN_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .form(form)
        .send()
        .await?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        return Err(CoreError::Oauth(format!(
            "token endpoint returned {}: {body}",
            status.as_u16()
        )));
    }
    Ok(serde_json::from_str(&body)?)
}

// ------------------------------------------------------------------ Credential storage

const KEYRING_SERVICE: &str = "playlist-curator";
const KEYRING_USER: &str = "spotify-tokens";

/// Where tokens are kept. The OS vault is preferred; the file fallback exists
/// because headless Linux boxes (WSL, containers) often have no
/// secret-service/dbus, and failing to log in there would be worse than a
/// 0600 file.
#[derive(Debug, Clone)]
pub enum TokenStore {
    #[cfg(feature = "keyring-store")]
    Keyring,
    File(PathBuf),
}

impl TokenStore {
    /// Prefer the OS vault, probing it once with a real round-trip because
    /// construction succeeds even when no backend is reachable.
    pub fn detect(data_dir: &std::path::Path) -> Self {
        #[cfg(feature = "keyring-store")]
        {
            if keyring_probe().is_ok() {
                return TokenStore::Keyring;
            }
            tracing::warn!(
                "no usable OS credential vault; falling back to a 0600 token file"
            );
        }
        TokenStore::File(data_dir.join("tokens.json"))
    }

    pub fn save(&self, tokens: &Tokens) -> Result<()> {
        let json = serde_json::to_string(tokens)?;
        match self {
            #[cfg(feature = "keyring-store")]
            TokenStore::Keyring => {
                let entry = keyring_entry()?;
                entry
                    .set_password(&json)
                    .map_err(|e| CoreError::Credential(e.to_string()))
            }
            TokenStore::File(path) => write_private_file(path, &json),
        }
    }

    pub fn load(&self) -> Result<Option<Tokens>> {
        let raw = match self {
            #[cfg(feature = "keyring-store")]
            TokenStore::Keyring => match keyring_entry()?.get_password() {
                Ok(s) => Some(s),
                Err(keyring::Error::NoEntry) => None,
                Err(e) => return Err(CoreError::Credential(e.to_string())),
            },
            TokenStore::File(path) => match std::fs::read_to_string(path) {
                Ok(s) => Some(s),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            },
        };
        match raw {
            // A corrupt blob should force a fresh login, not a hard failure.
            Some(s) => Ok(serde_json::from_str(&s).ok()),
            None => Ok(None),
        }
    }

    pub fn clear(&self) -> Result<()> {
        match self {
            #[cfg(feature = "keyring-store")]
            TokenStore::Keyring => match keyring_entry()?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(CoreError::Credential(e.to_string())),
            },
            TokenStore::File(path) => match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            },
        }
    }
}

#[cfg(feature = "keyring-store")]
fn keyring_entry() -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|e| CoreError::Credential(e.to_string()))
}

/// Check the vault actually works. `Entry::new` can succeed on a machine with no
/// running secret service, so only a read proves usability.
#[cfg(feature = "keyring-store")]
fn keyring_probe() -> Result<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, "probe")
        .map_err(|e| CoreError::Credential(e.to_string()))?;
    match entry.get_password() {
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(CoreError::Credential(e.to_string())),
    }
}

/// Write owner-read/write-only, and create the file that way from the start so
/// the secret is never briefly world-readable.
fn write_private_file(path: &std::path::Path, contents: &str) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    Ok(())
}

// ------------------------------------------------------------------ Session

/// Holds the current tokens and refreshes them on demand.
pub struct Session {
    store: TokenStore,
    client_id: String,
    http: reqwest::Client,
    tokens: tokio::sync::Mutex<Option<Tokens>>,
}

impl Session {
    pub fn new(store: TokenStore, client_id: String, http: reqwest::Client) -> Result<Self> {
        let tokens = store.load()?;
        Ok(Session {
            store,
            client_id,
            http,
            tokens: tokio::sync::Mutex::new(tokens),
        })
    }

    pub fn from_settings(
        settings: &Settings,
        data_dir: &std::path::Path,
        http: reqwest::Client,
    ) -> Result<Self> {
        let client_id = settings.require_client_id()?.to_string();
        Session::new(TokenStore::detect(data_dir), client_id, http)
    }

    pub async fn is_connected(&self) -> bool {
        self.tokens.lock().await.is_some()
    }

    /// A valid bearer token, refreshing first if the current one is expired or
    /// about to be. The lock is held across the refresh so concurrent callers
    /// cannot both spend the refresh token.
    pub async fn access_token(&self) -> Result<String> {
        let mut guard = self.tokens.lock().await;
        let current = guard.as_ref().ok_or(CoreError::NotAuthenticated)?;
        if !current.is_expired() {
            return Ok(current.access_token.clone());
        }
        let refresh_token = current
            .refresh_token
            .clone()
            .ok_or_else(|| CoreError::Oauth("access token expired and no refresh token is stored".into()))?;

        tracing::info!("access token expired; refreshing");
        let next = refresh(&self.http, &self.client_id, &refresh_token).await?;
        self.store.save(&next)?;
        let token = next.access_token.clone();
        *guard = Some(next);
        Ok(token)
    }

    /// Mark the stored access token as expired so the next [`Session::access_token`]
    /// call refreshes. Used when the API returns 401 despite a token that looked
    /// fresh — a clock skew or an early server-side invalidation.
    pub async fn force_expire(&self) {
        if let Some(t) = self.tokens.lock().await.as_mut() {
            t.expires_at = 0;
        }
    }

    pub async fn set_tokens(&self, tokens: Tokens) -> Result<()> {
        self.store.save(&tokens)?;
        *self.tokens.lock().await = Some(tokens);
        Ok(())
    }

    pub async fn disconnect(&self) -> Result<()> {
        self.store.clear()?;
        *self.tokens.lock().await = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_example() {
        // The worked example from RFC 7636 Appendix B.
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            Pkce::challenge_for(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifier_is_within_rfc_length_bounds() {
        let p = Pkce::generate();
        assert!((43..=128).contains(&p.verifier.len()), "len {}", p.verifier.len());
        // Unreserved characters only.
        assert!(p.verifier.chars().all(|c| c.is_ascii_alphanumeric()));
        // Base64url without padding.
        assert!(!p.challenge.contains('='));
        assert!(!p.challenge.contains('+'));
        assert!(!p.challenge.contains('/'));
        assert_eq!(p.challenge, Pkce::challenge_for(&p.verifier));
    }

    #[test]
    fn generated_verifiers_differ_between_logins() {
        assert_ne!(Pkce::generate().verifier, Pkce::generate().verifier);
    }

    #[test]
    fn authorize_url_carries_pkce_and_loopback_redirect() {
        let pending = begin("my-client-id");
        let url = url::Url::parse(&pending.authorize_url).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().collect();

        assert_eq!(q["client_id"], "my-client-id");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], pending.pkce.challenge.as_str());
        assert_eq!(q["redirect_uri"], OAUTH_REDIRECT_URI);
        // Never send the verifier to the authorize endpoint.
        assert!(!pending.authorize_url.contains(&pending.pkce.verifier));
        // Scopes are space-delimited in one parameter.
        assert!(q["scope"].contains("playlist-modify-private"));
        assert!(q["scope"].contains("user-read-private"));
    }

    #[test]
    fn parses_successful_callback() {
        let p = parse_callback_query("/callback?code=AQD123&state=xyz").unwrap();
        assert_eq!(p.code.as_deref(), Some("AQD123"));
        assert_eq!(p.state.as_deref(), Some("xyz"));
        assert!(p.error.is_none());
    }

    #[test]
    fn parses_denied_callback() {
        let p = parse_callback_query("/callback?error=access_denied&state=xyz").unwrap();
        assert_eq!(p.error.as_deref(), Some("access_denied"));
        assert!(p.code.is_none());
    }

    #[test]
    fn percent_decodes_callback_values() {
        let p = parse_callback_query("/callback?code=a%2Bb%2Fc&state=s").unwrap();
        assert_eq!(p.code.as_deref(), Some("a+b/c"));
    }

    #[test]
    fn expiry_accounts_for_skew() {
        let mut t = Tokens {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at: unix_now() + 3600,
            scope: None,
        };
        assert!(!t.is_expired());

        // Inside the skew window: treated as expired so a long request cannot
        // start valid and finish invalid.
        t.expires_at = unix_now() + (EXPIRY_SKEW_SECS / 2);
        assert!(t.is_expired());

        t.expires_at = unix_now() - 1;
        assert!(t.is_expired());
    }

    #[test]
    fn refresh_response_without_refresh_token_keeps_the_old_one() {
        let resp: TokenResponse = serde_json::from_str(
            r#"{"access_token":"new-at","expires_in":3600,"scope":"playlist-read-private"}"#,
        ).unwrap();
        let t = Tokens::from_response(resp, Some("original-rt".into()));
        assert_eq!(t.access_token, "new-at");
        assert_eq!(t.refresh_token.as_deref(), Some("original-rt"));
    }

    #[test]
    fn rotated_refresh_token_replaces_the_old_one() {
        let resp: TokenResponse = serde_json::from_str(
            r#"{"access_token":"at","refresh_token":"rotated","expires_in":3600}"#,
        ).unwrap();
        let t = Tokens::from_response(resp, Some("original".into()));
        assert_eq!(t.refresh_token.as_deref(), Some("rotated"));
    }

    #[test]
    fn file_token_store_roundtrip_is_private_and_clearable() {
        let dir = std::env::temp_dir().join(format!("pc-tok-{}", std::process::id()));
        let store = TokenStore::File(dir.join("tokens.json"));
        assert_eq!(store.load().unwrap(), None);

        let tokens = Tokens {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at: 1_800_000_000,
            scope: Some("x".into()),
        };
        store.save(&tokens).unwrap();
        assert_eq!(store.load().unwrap(), Some(tokens));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("tokens.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "token file must not be group/world readable");
        }

        store.clear().unwrap();
        assert_eq!(store.load().unwrap(), None);
        // Clearing an absent credential is not an error.
        store.clear().unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_token_blob_reads_as_absent() {
        let dir = std::env::temp_dir().join(format!("pc-tok-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tokens.json");
        std::fs::write(&path, "not json at all").unwrap();
        // Forces a fresh login rather than a hard error the user cannot escape.
        assert_eq!(TokenStore::File(path).load().unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn session_without_tokens_reports_not_authenticated() {
        let dir = std::env::temp_dir().join(format!("pc-sess-{}", std::process::id()));
        let session = Session::new(
            TokenStore::File(dir.join("tokens.json")),
            "cid".into(),
            reqwest::Client::new(),
        )
        .unwrap();
        assert!(!session.is_connected().await);
        assert!(matches!(
            session.access_token().await,
            Err(CoreError::NotAuthenticated)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn session_returns_unexpired_token_without_network() {
        let dir = std::env::temp_dir().join(format!("pc-sess-ok-{}", std::process::id()));
        let session = Session::new(
            TokenStore::File(dir.join("tokens.json")),
            "cid".into(),
            reqwest::Client::new(),
        )
        .unwrap();
        session
            .set_tokens(Tokens {
                access_token: "live-token".into(),
                refresh_token: Some("rt".into()),
                expires_at: unix_now() + 3600,
                scope: None,
            })
            .await
            .unwrap();

        assert!(session.is_connected().await);
        assert_eq!(session.access_token().await.unwrap(), "live-token");

        session.disconnect().await.unwrap();
        assert!(!session.is_connected().await);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Drive the real loopback listener on `port`: bind, send `request_line` the
    /// way a browser would, and return whatever the handshake produced.
    fn drive_callback(port: u16, expected_state: &str, request_line: &str) -> Result<String> {
        let state = expected_state.to_string();
        let handle = std::thread::spawn(move || {
            wait_for_callback_on(port, &state, Duration::from_secs(10))
        });

        // Poll until the listener has bound, rather than sleeping a fixed time.
        let addr = format!("127.0.0.1:{port}");
        let mut stream = None;
        for _ in 0..100 {
            match std::net::TcpStream::connect(&addr) {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let mut stream = stream.expect("listener should accept a connection");

        use std::io::{Read, Write};
        write!(
            stream,
            "GET {request_line} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut sink = String::new();
        let _ = stream.read_to_string(&mut sink);

        handle.join().expect("callback thread should not panic")
    }

    #[test]
    fn loopback_callback_completes_the_handshake() {
        let code = drive_callback(
            14701,
            "expected-state",
            "/callback?code=THE_CODE&state=expected-state",
        )
        .unwrap();
        assert_eq!(code, "THE_CODE");
    }

    #[test]
    fn loopback_callback_rejects_state_mismatch() {
        // A callback with the wrong state must not yield a code, or an attacker
        // could inject their own authorization code into our session.
        let err = drive_callback(14702, "right-state", "/callback?code=C&state=wrong-state")
            .unwrap_err();
        assert!(
            matches!(err, CoreError::Oauth(ref m) if m.contains("state mismatch")),
            "{err}"
        );
    }

    #[test]
    fn loopback_callback_surfaces_user_denial() {
        let err = drive_callback(
            14703,
            "s",
            "/callback?error=access_denied&state=s",
        )
        .unwrap_err();
        assert!(
            matches!(err, CoreError::Oauth(ref m) if m.contains("access_denied")),
            "{err}"
        );
    }

    #[test]
    fn loopback_callback_times_out_when_user_abandons_the_browser() {
        // No request is ever sent; the thread must give up rather than leak.
        let err = wait_for_callback_on(14704, "s", Duration::from_millis(300)).unwrap_err();
        assert!(matches!(err, CoreError::Oauth(ref m) if m.contains("timed out")), "{err}");
    }
}
