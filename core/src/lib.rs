//! Playlist Curator — core library.
//!
//! Reorganises a Spotify playlist into derived playlists by genre, geographic
//! origin and era.
//!
//! # Why the data flows the way it does
//!
//! Spotify's Web API lost `audio-features`, `recommendations` and
//! `related-artists` for new apps in November 2024, and Development Mode has
//! tightened repeatedly since. So Spotify is used for exactly two things: it is
//! the *identity source* for tracks (crucially the ISRC in `external_ids`) and
//! the *write target* for new playlists.
//!
//! All actual musical knowledge — genre, origin, era — comes from MusicBrainz,
//! Last.fm, Discogs and Wikidata, materialised into a local SQLite cache that
//! becomes a durable personal knowledge base. If Spotify's API tightens further,
//! the analysis still works; only the write step is lost.
//!
//! This crate has no dependency on `tauri`, so the entire pipeline is testable
//! with `cargo test` and needs no webview or GUI libraries.

pub mod config;
pub mod error;
pub mod model;
pub mod store;
pub mod util;

pub mod enrich;
pub mod llm;
pub mod spotify;
pub mod suggest;
pub mod taxonomy;

pub use config::{Paths, Settings};
pub use error::{CoreError, Result};
pub use store::Store;

/// Initialise tracing once, honouring `RUST_LOG` and defaulting to something
/// readable rather than silent.
pub fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("pc_core=info,warn"));
    // Ignore the error when a subscriber is already installed; the Tauri shell
    // and the test harness may both call this.
    let _ = fmt().with_env_filter(filter).with_target(true).try_init();
}

/// Everything the application needs, assembled once at startup.
pub struct App {
    pub store: Store,
    pub paths: Paths,
    pub settings: std::sync::RwLock<Settings>,
}

impl App {
    pub fn open(paths: Paths) -> Result<Self> {
        paths.ensure()?;
        let settings = Settings::load(paths.settings_path())?;
        let store = Store::open(paths.db_path())?;
        Ok(App {
            store,
            paths,
            settings: std::sync::RwLock::new(settings),
        })
    }

    /// Snapshot of current settings. Cloned rather than borrowed so callers
    /// never hold the lock across an await point.
    pub fn settings(&self) -> Settings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    pub fn update_settings(&self, next: Settings) -> Result<()> {
        next.save(self.paths.settings_path())?;
        *self.settings.write().expect("settings lock poisoned") = next;
        Ok(())
    }
}
