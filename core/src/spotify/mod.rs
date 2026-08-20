//! Spotify integration: authentication, API client, and playlist import.
//!
//! Spotify's role here is deliberately narrow. It supplies track *identity*
//! (above all the ISRC) and accepts newly created playlists. It is not used as a
//! source of musical knowledge, because the endpoints that offered that
//! (`audio-features`, `recommendations`, `related-artists`) were withdrawn from
//! new apps in November 2024.

pub mod auth;
pub mod client;
pub mod import;
pub mod models;
pub mod publish;

pub use auth::{Session, TokenStore, Tokens};
pub use client::SpotifyClient;
pub use import::{import_playlist, ImportStats};
pub use publish::{create_from_card, CreateOutcome};
