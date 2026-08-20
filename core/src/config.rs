//! Credentials, filesystem paths and tunable settings.
//!
//! API keys live in a JSON settings file; OAuth tokens do *not* — those go to
//! the OS credential vault (see `spotify::auth`).

use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Loopback port for the OAuth callback. Spotify accepts HTTP redirect URIs only
/// on a literal loopback IP, so this must be registered as
/// `http://127.0.0.1:14523/callback` — `localhost` is rejected.
pub const OAUTH_PORT: u16 = 14523;
pub const OAUTH_REDIRECT_URI: &str = "http://127.0.0.1:14523/callback";

/// Identifies this app to MusicBrainz, which requires a contactable User-Agent.
pub const USER_AGENT: &str = concat!(
    "PlaylistCurator/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/local/playlist-curator )"
);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Spotify application Client ID. Public client — there is no secret, PKCE
    /// is mandatory.
    pub spotify_client_id: Option<String>,
    pub lastfm_api_key: Option<String>,
    pub discogs_token: Option<String>,

    pub llm: LlmSettings,
    pub cache: CacheSettings,
    pub weights: SourceWeights,

    /// When true, playlist creation only reports what it *would* do. Default on
    /// so the first runs cannot touch the user's account.
    pub dry_run: bool,

    /// Match score below which a result is queued for review instead of trusted.
    pub review_threshold: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            spotify_client_id: None,
            lastfm_api_key: None,
            discogs_token: None,
            llm: LlmSettings::default(),
            cache: CacheSettings::default(),
            weights: SourceWeights::default(),
            dry_run: true,
            review_threshold: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmSettings {
    pub provider: LlmProviderKind,
    /// Ollama base URL, OpenAI-compatible surface.
    pub ollama_url: String,
    pub ollama_model: String,
    pub anthropic_model: String,
    pub anthropic_api_key: Option<String>,
}

impl Default for LlmSettings {
    fn default() -> Self {
        LlmSettings {
            provider: LlmProviderKind::Disabled,
            ollama_url: "http://127.0.0.1:11434".into(),
            ollama_model: "qwen3:8b".into(),
            anthropic_model: "claude-opus-5".into(),
            anthropic_api_key: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmProviderKind {
    Disabled,
    Ollama,
    Anthropic,
}

impl Default for LlmProviderKind {
    fn default() -> Self {
        LlmProviderKind::Disabled
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CacheSettings {
    /// MusicBrainz data is stable; cache it for a long time.
    pub musicbrainz_ttl_days: i64,
    pub lastfm_ttl_days: i64,
    pub discogs_ttl_days: i64,
    pub wikidata_ttl_days: i64,
}

impl Default for CacheSettings {
    fn default() -> Self {
        CacheSettings {
            musicbrainz_ttl_days: 90,
            lastfm_ttl_days: 30,
            discogs_ttl_days: 30,
            wikidata_ttl_days: 90,
        }
    }
}

/// How much each source's tags count toward a genre score. Exposed in Settings
/// because reasonable people disagree, and reprocessing is free (raw signals are
/// kept in `tag_signal`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceWeights {
    /// Curated, voted vocabulary.
    pub musicbrainz_genre: f64,
    /// Editorial, strong.
    pub discogs: f64,
    /// Free-form, noisy.
    pub musicbrainz_tag: f64,
    pub lastfm_artist: f64,
    pub lastfm_track: f64,
    /// Last resort: Spotify invents genre names.
    pub spotify_artist: f64,
}

impl Default for SourceWeights {
    fn default() -> Self {
        SourceWeights {
            musicbrainz_genre: 1.0,
            discogs: 0.8,
            musicbrainz_tag: 0.6,
            lastfm_artist: 0.5,
            lastfm_track: 0.5,
            spotify_artist: 0.2,
        }
    }
}

impl CacheSettings {
    pub fn ttl_secs(&self, source: crate::model::Source) -> i64 {
        use crate::model::Source::*;
        let days = match source {
            MusicBrainz => self.musicbrainz_ttl_days,
            Lastfm => self.lastfm_ttl_days,
            Discogs => self.discogs_ttl_days,
            Wikidata => self.wikidata_ttl_days,
            Spotify => 1,
        };
        days * 86_400
    }
}

/// Where the app keeps its database and settings.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
}

impl Paths {
    /// Resolve the per-user data directory, honouring `PLAYLIST_CURATOR_DATA_DIR`
    /// so tests and portable installs can redirect it.
    pub fn resolve() -> Result<Self> {
        if let Ok(dir) = std::env::var("PLAYLIST_CURATOR_DATA_DIR") {
            return Ok(Paths { data_dir: PathBuf::from(dir) });
        }
        let base = if cfg!(windows) {
            std::env::var("APPDATA").map(PathBuf::from).ok()
        } else if cfg!(target_os = "macos") {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join("Library/Application Support"))
                .ok()
        } else {
            std::env::var("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                .ok()
        };
        let base = base.ok_or_else(|| {
            CoreError::Config("cannot determine a home/data directory".into())
        })?;
        Ok(Paths { data_dir: base.join("playlist-curator") })
    }

    pub fn with_data_dir(dir: impl Into<PathBuf>) -> Self {
        Paths { data_dir: dir.into() }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("curator.db")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.data_dir.join("settings.json")
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        Ok(())
    }
}

impl Settings {
    /// Load settings, falling back to defaults when the file is absent.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write-then-rename so a crash mid-write cannot truncate the settings.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn require_client_id(&self) -> Result<&str> {
        self.spotify_client_id.as_deref().filter(|s| !s.is_empty()).ok_or_else(|| {
            CoreError::Config("Spotify Client ID is not configured".into())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_settings_file_yields_defaults() {
        let s = Settings::load("/nonexistent/path/settings.json").unwrap();
        assert!(s.dry_run, "dry-run must default to on");
        assert_eq!(s.llm.provider, LlmProviderKind::Disabled);
        assert_eq!(s.cache.musicbrainz_ttl_days, 90);
    }

    #[test]
    fn settings_roundtrip_and_partial_json_keeps_defaults() {
        let dir = std::env::temp_dir().join(format!("pc-cfg-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        s.spotify_client_id = Some("abc123".into());
        s.dry_run = false;
        s.save(&path).unwrap();

        let back = Settings::load(&path).unwrap();
        assert_eq!(back.spotify_client_id.as_deref(), Some("abc123"));
        assert!(!back.dry_run);

        // A file written by an older version must still load.
        std::fs::write(&path, r#"{"spotify_client_id":"xyz"}"#).unwrap();
        let partial = Settings::load(&path).unwrap();
        assert_eq!(partial.spotify_client_id.as_deref(), Some("xyz"));
        assert_eq!(partial.weights.musicbrainz_genre, 1.0);
        assert!(partial.dry_run);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn redirect_uri_is_a_literal_loopback_ip() {
        // Spotify rejects `localhost` for HTTP redirects; only 127.0.0.1 works.
        assert!(OAUTH_REDIRECT_URI.starts_with("http://127.0.0.1:"));
        assert!(!OAUTH_REDIRECT_URI.contains("localhost"));
        assert!(OAUTH_REDIRECT_URI.contains(&OAUTH_PORT.to_string()));
    }

    #[test]
    fn user_agent_is_contactable() {
        // MusicBrainz blocks generic agents; it must carry a URL or email.
        assert!(USER_AGENT.contains("PlaylistCurator/"));
        assert!(USER_AGENT.contains("http"));
    }

    #[test]
    fn ttl_maps_per_source() {
        let c = CacheSettings::default();
        assert_eq!(c.ttl_secs(crate::model::Source::MusicBrainz), 90 * 86_400);
        assert_eq!(c.ttl_secs(crate::model::Source::Lastfm), 30 * 86_400);
    }
}
