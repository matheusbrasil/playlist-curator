use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("connection pool error: {0}")]
    Pool(#[from] r2d2::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("not authenticated with Spotify")]
    NotAuthenticated,

    #[error("spotify api error {status}: {body}")]
    SpotifyApi { status: u16, body: String },

    /// Development Mode quota is counted per developer account; a 429 with this
    /// reason means the whole account is throttled, not just this request.
    #[error("spotify quota exceeded for this developer account")]
    QuotaExceeded,

    #[error("oauth error: {0}")]
    Oauth(String),

    #[error("credential store error: {0}")]
    Credential(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("upstream {source_name} error: {message}")]
    Upstream { source_name: String, message: String },

    #[error("invalid filter: {0}")]
    InvalidFilter(String),

    #[error("{0}")]
    Other(String),
}

impl CoreError {
    pub fn other(msg: impl Into<String>) -> Self {
        CoreError::Other(msg.into())
    }

    pub fn upstream(source_name: impl Into<String>, message: impl Into<String>) -> Self {
        CoreError::Upstream {
            source_name: source_name.into(),
            message: message.into(),
        }
    }

    /// Whether retrying the same call could plausibly succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            CoreError::Http(e) => e.is_timeout() || e.is_connect(),
            CoreError::SpotifyApi { status, .. } => *status >= 500 || *status == 429,
            CoreError::QuotaExceeded => false,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// Serializable shape for crossing the Tauri IPC boundary.
impl serde::Serialize for CoreError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("CoreError", 2)?;
        st.serialize_field("kind", self.kind())?;
        st.serialize_field("message", &self.to_string())?;
        st.end()
    }
}

impl CoreError {
    /// Stable machine-readable discriminant for the UI to branch on.
    pub fn kind(&self) -> &'static str {
        match self {
            CoreError::Db(_) => "db",
            CoreError::Pool(_) => "pool",
            CoreError::Http(_) => "http",
            CoreError::Json(_) => "json",
            CoreError::Io(_) => "io",
            CoreError::NotAuthenticated => "not_authenticated",
            CoreError::SpotifyApi { .. } => "spotify_api",
            CoreError::QuotaExceeded => "quota_exceeded",
            CoreError::Oauth(_) => "oauth",
            CoreError::Credential(_) => "credential",
            CoreError::Config(_) => "config",
            CoreError::Upstream { .. } => "upstream",
            CoreError::InvalidFilter(_) => "invalid_filter",
            CoreError::Other(_) => "other",
        }
    }
}
