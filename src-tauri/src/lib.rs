//! Tauri application shell — the thin layer between the webview and pc-core.
//!
//! All business logic lives in pc-core. This crate only:
//!  * opens the database and constructs `App` at startup,
//!  * registers Tauri commands that delegate to pc-core, and
//!  * emits progress events from async pipelines.
//!
//! No OAuth token, SQL query or genre computation originates here.

mod commands;

use pc_core::{App, Paths};
use std::sync::Arc;
use tauri::Manager;

/// Shared application state injected into every command handler.
pub struct AppState {
    pub app: Arc<App>,
}

pub fn run() {
    pc_core::init_tracing();

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|tauri_app| {
            let paths = Paths::resolve()?;
            let core_app = App::open(paths).map_err(|e| Box::new(e) as Box<dyn std::error::Error>)?;
            tauri_app.manage(AppState {
                app: Arc::new(core_app),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // Connect
            commands::connection_status,
            commands::spotify_login,
            commands::spotify_logout,
            // Settings
            commands::get_settings,
            commands::save_settings,
            commands::clear_cache,
            commands::export_database,
            // Playlists
            commands::list_playlists,
            commands::sync_playlists,
            commands::import_playlist,
            // Analysis
            commands::enrich_playlist,
            commands::enrich_counts,
            commands::derive_playlist,
            commands::analysis_summary,
            commands::analysis_tracks,
            commands::list_reviews,
            commands::retry_enrich_track,
            // Suggestions
            commands::suggest_playlists,
            commands::suggest_from_query,
            commands::suggest_from_filter,
            commands::list_genres,
            commands::list_countries,
            commands::list_decades,
            // Create
            commands::create_playlist,
            commands::list_created_playlists,
            // Overrides
            commands::set_override,
            commands::clear_override,
            commands::resolve_review,
            // LLM
            commands::llm_status,
            commands::normalize_orphan_tags,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
