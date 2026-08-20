//! Tauri command handlers — the IPC frontier.
//!
//! Every function here is a `#[tauri::command]`. Each one:
//!  1. receives typed arguments from the webview,
//!  2. reads `AppState` to get the core `App`,
//!  3. delegates to a `pc_core` function, and
//!  4. returns a serialisable result or a `CommandError`.
//!
//! No logic lives here. If you find yourself writing a `for` loop or a
//! conditional, the logic belongs in `pc_core` instead.

use crate::AppState;
use pc_core::error::CoreError;
use pc_core::{suggest::PlaylistFilter, Settings};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{AppHandle, State};

// ── Error serialisation ───────────────────────────────────────────────────────

/// The wire form of every command error. `kind` is the stable discriminant the
/// UI switches on; `message` is human-readable.
#[derive(Debug, Serialize)]
pub struct CommandError {
    kind: String,
    message: String,
}

impl From<CoreError> for CommandError {
    fn from(e: CoreError) -> Self {
        CommandError {
            kind: e.kind().to_owned(),
            message: e.to_string(),
        }
    }
}

type Cmd<T> = Result<T, CommandError>;

fn err(e: impl Into<CoreError>) -> CommandError {
    CommandError::from(e.into())
}

// ── Connection ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpotifyUser {
    pub id: String,
    pub display_name: Option<String>,
    pub product: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionStatus {
    pub connected: bool,
    pub client_id_configured: bool,
    pub user: Option<SpotifyUser>,
    pub premium_warning: bool,
    pub token_store: String,
}

#[tauri::command]
pub async fn connection_status(state: State<'_, AppState>) -> Cmd<ConnectionStatus> {
    let settings = state.app.settings();
    let client_id_configured = settings.spotify_client_id.as_deref().is_some_and(|s| !s.is_empty());

    let http = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(|e| CommandError { kind: "http".into(), message: e.to_string() })?;

    let session = pc_core::spotify::auth::Session::load(&state.app.paths, &settings);

    let (connected, user, premium_warning, token_store) = match session {
        Ok(sess) => {
            let client = pc_core::spotify::client::SpotifyClient::new(http, sess);
            match client.current_user().await {
                Ok(u) => {
                    let premium = u.product.as_deref() != Some("premium");
                    let store = if cfg!(target_os = "linux") || cfg!(target_os = "macos") || cfg!(target_os = "windows") {
                        "keyring"
                    } else {
                        "file"
                    };
                    (true, Some(SpotifyUser { id: u.id, display_name: u.display_name, product: u.product }), premium, store.to_owned())
                }
                Err(_) => (false, None, false, "file".to_owned()),
            }
        }
        Err(_) => (false, None, false, "file".to_owned()),
    };

    Ok(ConnectionStatus { connected, client_id_configured, user, premium_warning, token_store })
}

#[tauri::command]
pub async fn spotify_login(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Cmd<SpotifyUser> {
    let settings = state.app.settings();
    let client_id = settings
        .require_client_id()
        .map_err(|e| CommandError::from(e))?
        .to_owned();

    let http = build_http()?;

    // Open the system browser at the Spotify auth URL and block until
    // the loopback receives the callback.
    let session = pc_core::spotify::auth::run_pkce_flow(
        &client_id,
        &state.app.paths,
        &settings,
        |url| {
            // Open in default browser via the shell plugin.
            let _ = tauri_plugin_shell::ShellExt::shell_open(&app_handle, url, None);
        },
    )
    .await
    .map_err(CommandError::from)?;

    let client = pc_core::spotify::client::SpotifyClient::new(http, session);
    let user = client.current_user().await.map_err(CommandError::from)?;
    Ok(SpotifyUser { id: user.id, display_name: user.display_name, product: user.product })
}

#[tauri::command]
pub async fn spotify_logout(state: State<'_, AppState>) -> Cmd<()> {
    pc_core::spotify::auth::clear_tokens(&state.app.paths)
        .map_err(CommandError::from)
}

// ── Settings ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Cmd<Settings> {
    Ok(state.app.settings())
}

#[tauri::command]
pub async fn save_settings(state: State<'_, AppState>, settings: Settings) -> Cmd<()> {
    state.app.update_settings(settings).map_err(CommandError::from)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheClearResult {
    pub rows_deleted: usize,
}

#[tauri::command]
pub async fn clear_cache(state: State<'_, AppState>) -> Cmd<CacheClearResult> {
    let rows = state.app.store.cache_clear().map_err(CommandError::from)?;
    Ok(CacheClearResult { rows_deleted: rows })
}

#[tauri::command]
pub async fn export_database(
    state: State<'_, AppState>,
    dest_path: String,
) -> Cmd<()> {
    state
        .app
        .store
        .export_to(&dest_path)
        .map_err(CommandError::from)
}

// ── Playlists ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn list_playlists(state: State<'_, AppState>) -> Cmd<Vec<pc_core::model::Playlist>> {
    state.app.store.list_playlists().map_err(CommandError::from)
}

#[tauri::command]
pub async fn sync_playlists(state: State<'_, AppState>) -> Cmd<Vec<pc_core::model::Playlist>> {
    let settings = state.app.settings();
    let http = build_http()?;
    let session = load_session(&state, &settings)?;
    let client = pc_core::spotify::client::SpotifyClient::new(http, session);

    let playlists = client.current_user_playlists().await.map_err(CommandError::from)?;
    for p in &playlists {
        state.app.store.upsert_playlist(p).map_err(CommandError::from)?;
    }
    state.app.store.list_playlists().map_err(CommandError::from)
}

#[tauri::command]
pub async fn import_playlist(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<pc_core::spotify::import::ImportStats> {
    let settings = state.app.settings();
    let http = build_http()?;
    let session = load_session(&state, &settings)?;
    let client = pc_core::spotify::client::SpotifyClient::new(http, session);

    pc_core::spotify::import::import_playlist(&client, &state.app.store, &playlist_id)
        .await
        .map_err(CommandError::from)
}

// ── Analysis ──────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn enrich_playlist(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    playlist_id: String,
) -> Cmd<pc_core::enrich::pipeline::EnrichStats> {
    let settings = state.app.settings();
    let store = state.app.store.clone();

    let on_progress = {
        let handle = app_handle.clone();
        Arc::new(move |progress: pc_core::enrich::pipeline::EnrichProgress| {
            let _ = handle.emit("enrich://progress", &progress);
        })
    };

    pc_core::enrich::pipeline::enrich_playlist(store, settings, &playlist_id, on_progress)
        .await
        .map_err(CommandError::from)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeriveResult {
    pub tracks_with_genre: usize,
    pub origins_resolved: usize,
    pub eras_resolved: usize,
}

#[tauri::command]
pub async fn derive_playlist(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<DeriveResult> {
    use pc_core::taxonomy::{aggregate, derive};

    let origins = derive::derive_origins_for_playlist(&state.app.store, &playlist_id)
        .map_err(CommandError::from)?;
    let eras = derive::derive_eras_for_playlist(&state.app.store, &playlist_id)
        .map_err(CommandError::from)?;

    let settings = state.app.settings();
    let tracks_with_genre = aggregate::derive_genres_for_playlist(
        &state.app.store,
        &playlist_id,
        &settings,
    )
    .map_err(CommandError::from)?;

    Ok(DeriveResult {
        tracks_with_genre,
        origins_resolved: origins,
        eras_resolved: eras,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetEntry {
    pub slug: String,
    pub label: String,
    pub count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CountryEntry {
    pub code: String,
    pub label: String,
    pub count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecadeEntry {
    pub decade: i64,
    pub count: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisSummary {
    pub track_count: i64,
    pub isrc_coverage: f64,
    pub mb_coverage: f64,
    pub genre_distribution: Vec<FacetEntry>,
    pub country_distribution: Vec<CountryEntry>,
    pub decade_distribution: Vec<DecadeEntry>,
    pub needs_review_count: i64,
}

#[tauri::command]
pub async fn analysis_summary(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<AnalysisSummary> {
    let store = &state.app.store;

    let (total_tracks, with_isrc) = store.isrc_coverage(&playlist_id).map_err(CommandError::from)?;
    let (_, mb_resolved) = store.mb_resolution_coverage(&playlist_id).map_err(CommandError::from)?;

    let isrc_coverage = if total_tracks > 0 { with_isrc as f64 / total_tracks as f64 } else { 0.0 };
    let mb_coverage = if total_tracks > 0 { mb_resolved as f64 / total_tracks as f64 } else { 0.0 };

    let genre_dist = store.genre_distribution(&playlist_id).map_err(CommandError::from)?;
    let country_dist = store.country_distribution(&playlist_id).map_err(CommandError::from)?;
    let decade_dist = store.decade_distribution(&playlist_id).map_err(CommandError::from)?;
    let needs_review = store.open_reviews().map_err(CommandError::from)?.len() as i64;

    Ok(AnalysisSummary {
        track_count: total_tracks,
        isrc_coverage,
        mb_coverage,
        genre_distribution: genre_dist
            .into_iter()
            .map(|(slug, label, count)| FacetEntry { slug, label, count })
            .collect(),
        country_distribution: country_dist
            .into_iter()
            .map(|(code, label, count)| CountryEntry { code, label, count })
            .collect(),
        decade_distribution: decade_dist
            .into_iter()
            .map(|(decade, count)| DecadeEntry { decade, count })
            .collect(),
        needs_review_count: needs_review,
    })
}

#[tauri::command]
pub async fn analysis_tracks(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<Vec<serde_json::Value>> {
    use pc_core::model::OriginSource;
    use serde_json::json;

    let pts = state.app.store.playlist_tracks(&playlist_id).map_err(CommandError::from)?;
    let mut result = Vec::with_capacity(pts.len());

    for pt in pts {
        let track = &pt.track;
        let artists = state.app.store.track_artists(&track.spotify_id).map_err(CommandError::from)?;
        let genres = state.app.store.track_genres(&track.spotify_id).map_err(CommandError::from)?;
        let origin = state.app.store.artist_origin(&artists.first().map(|a| a.spotify_id.as_str()).unwrap_or("")).map_err(CommandError::from)?;
        let era = state.app.store.track_era(&track.spotify_id).map_err(CommandError::from)?;
        let reviews = state.app.store.open_reviews().map_err(CommandError::from)?;
        let needs_review = reviews.iter().any(|r| r.entity_id == track.spotify_id);

        result.push(json!({
            "spotifyId": track.spotify_id,
            "name": track.name,
            "isrc": track.isrc,
            "artists": artists.iter().map(|a| json!({ "spotifyId": a.spotify_id, "name": a.name })).collect::<Vec<_>>(),
            "genres": genres.iter().map(|g| json!({ "slug": g.canonical_slug, "score": g.score })).collect::<Vec<_>>(),
            "origin": origin.as_ref().map(|o| json!({
                "countryCode": o.country_code,
                "countryLabel": o.country_label,
                "city": o.city,
                "source": o.source.as_str(),
                "confidence": o.confidence,
            })),
            "era": era.as_ref().map(|e| json!({
                "year": e.year,
                "decade": e.decade,
                "source": e.source.as_str(),
            })),
            "needsReview": needs_review,
        }));
    }
    Ok(result)
}

#[tauri::command]
pub async fn list_reviews(state: State<'_, AppState>) -> Cmd<Vec<pc_core::model::ReviewItem>> {
    state.app.store.open_reviews().map_err(CommandError::from)
}

// ── Suggestions ───────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn suggest_playlists(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<Vec<pc_core::suggest::SuggestionCard>> {
    pc_core::suggest::suggest(&state.app.store, &playlist_id)
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn suggest_from_query(
    state: State<'_, AppState>,
    playlist_id: String,
    query: String,
) -> Cmd<pc_core::suggest::SuggestionCard> {
    use pc_core::llm::Llm;
    use pc_core::suggest::nl;

    let settings = state.app.settings();

    // Try deterministic parser first; if it produces a valid filter, use it.
    // If not and an LLM is configured, ask the LLM.
    let filter = nl::parse_query(&query, &state.app.store)
        .or_else(|_| {
            let llm = Llm::from_settings(&settings, build_http_client());
            if llm.is_enabled() {
                // Blocking bridge: parse_query_with_llm is async but we need sync here.
                // In practice commands run on the Tokio runtime so this is fine.
                futures::executor::block_on(nl::parse_query_with_llm(
                    &query,
                    &state.app.store,
                    &llm,
                ))
            } else {
                Err(CoreError::Config("no LLM configured and deterministic parser failed".into()))
            }
        })
        .map_err(CommandError::from)?;

    pc_core::suggest::execute(&state.app.store, &playlist_id, &filter)
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn suggest_from_filter(
    state: State<'_, AppState>,
    playlist_id: String,
    filter: PlaylistFilter,
) -> Cmd<pc_core::suggest::SuggestionCard> {
    pc_core::suggest::execute(&state.app.store, &playlist_id, &filter)
        .map_err(CommandError::from)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenreEntry {
    pub slug: String,
    pub label: String,
    pub parent_slug: Option<String>,
}

#[tauri::command]
pub async fn list_genres(state: State<'_, AppState>) -> Cmd<Vec<GenreEntry>> {
    let genres = state.app.store.all_canonical_genres().map_err(CommandError::from)?;
    Ok(genres
        .into_iter()
        .map(|g| GenreEntry { slug: g.slug, label: g.label, parent_slug: g.parent_slug })
        .collect())
}

#[tauri::command]
pub async fn list_countries(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<Vec<CountryEntry>> {
    state
        .app
        .store
        .country_distribution(&playlist_id)
        .map_err(CommandError::from)
        .map(|v| {
            v.into_iter()
                .map(|(code, label, count)| CountryEntry { code, label, count })
                .collect()
        })
}

#[tauri::command]
pub async fn list_decades(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<Vec<DecadeEntry>> {
    state
        .app
        .store
        .decade_distribution(&playlist_id)
        .map_err(CommandError::from)
        .map(|v| v.into_iter().map(|(decade, count)| DecadeEntry { decade, count }).collect())
}

// ── Create playlists ──────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateResult {
    pub dry_run: bool,
    pub created: bool,
    pub spotify_id: Option<String>,
    pub spotify_url: Option<String>,
    pub name: String,
    pub track_count: usize,
    pub skipped: Vec<SkippedTrack>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedTrack {
    pub spotify_id: String,
    pub name: String,
    pub reason: String,
}

#[tauri::command]
pub async fn create_playlist(
    state: State<'_, AppState>,
    card: pc_core::suggest::SuggestionCard,
    public: bool,
    dry_run_override: Option<bool>,
) -> Cmd<CreateResult> {
    let settings = state.app.settings();
    let http = build_http()?;

    let result = pc_core::spotify::publish::create_from_card(
        &state.app.store,
        &settings,
        http,
        &state.app.paths,
        &card,
        public,
        dry_run_override,
    )
    .await
    .map_err(CommandError::from)?;

    Ok(CreateResult {
        dry_run: result.dry_run,
        created: result.created,
        spotify_id: result.spotify_id,
        spotify_url: result.spotify_url,
        name: result.name,
        track_count: result.track_count,
        skipped: result
            .skipped
            .into_iter()
            .map(|s| SkippedTrack { spotify_id: s.spotify_id, name: s.name, reason: s.reason })
            .collect(),
    })
}

#[tauri::command]
pub async fn list_created_playlists(
    state: State<'_, AppState>,
) -> Cmd<Vec<serde_json::Value>> {
    state
        .app
        .store
        .list_created_playlists()
        .map_err(CommandError::from)
}

// ── Overrides ─────────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn set_override(
    state: State<'_, AppState>,
    entity_type: String,
    entity_id: String,
    field: String,
    value: String,
) -> Cmd<()> {
    state
        .app
        .store
        .set_override(&entity_type, &entity_id, &field, &value)
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn clear_override(
    state: State<'_, AppState>,
    entity_type: String,
    entity_id: String,
    field: String,
) -> Cmd<()> {
    state
        .app
        .store
        .clear_override(&entity_type, &entity_id, &field)
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn resolve_review(
    state: State<'_, AppState>,
    entity_type: String,
    entity_id: String,
    reason: String,
) -> Cmd<()> {
    state
        .app
        .store
        .resolve_review(&entity_type, &entity_id, &reason)
        .map_err(CommandError::from)
}

// ── LLM ───────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmStatus {
    pub provider: String,
    pub available: bool,
    pub detail: String,
}

#[tauri::command]
pub async fn llm_status(state: State<'_, AppState>) -> Cmd<LlmStatus> {
    use pc_core::llm::Llm;

    let settings = state.app.settings();
    let llm = Llm::from_settings(&settings, build_http_client());

    let (available, detail) = if let Llm::Ollama(ref ollama) = llm {
        match ollama.probe().await {
            Ok(true) => (true, "Model is installed and daemon is reachable".into()),
            Ok(false) => (false, "Daemon is reachable but the model is not installed".into()),
            Err(e) => (false, format!("Cannot reach Ollama daemon: {e}")),
        }
    } else {
        (llm.is_enabled(), if llm.is_enabled() { "Configured".into() } else { "No provider configured".into() })
    };

    Ok(LlmStatus { provider: llm.name().to_owned(), available, detail })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NormaliseResult {
    pub resolved: usize,
    pub unresolved: usize,
}

#[tauri::command]
pub async fn normalize_orphan_tags(
    state: State<'_, AppState>,
    limit: usize,
) -> Cmd<NormaliseResult> {
    use pc_core::llm::Llm;
    use pc_core::llm::prompts;
    use pc_core::taxonomy::aliases::Taxonomy;

    let settings = state.app.settings();
    let llm = Llm::from_settings(&settings, build_http_client());
    if !llm.is_enabled() {
        return Ok(NormaliseResult { resolved: 0, unresolved: 0 });
    }

    let orphans = state.app.store.unresolved_raw_tags().map_err(CommandError::from)?;
    let batch: Vec<String> = orphans.into_iter().take(limit).collect();
    if batch.is_empty() {
        return Ok(NormaliseResult { resolved: 0, unresolved: 0 });
    }

    let all_genres = state.app.store.all_canonical_genres().map_err(CommandError::from)?;
    let vocab: Vec<String> = all_genres.iter().map(|g| g.slug.clone()).collect();

    let schema = prompts::normalise_tags_schema(&batch);
    let prompt = prompts::normalise_tags_prompt(&batch, &vocab);
    let system = prompts::SYSTEM_NORMALISE_TAGS;

    let result = llm.complete_json(&schema, system, &prompt).await.map_err(CommandError::from)?;

    let mut resolved = 0usize;
    let mut unresolved = 0usize;

    for tag in &batch {
        let canonical = result[tag].as_str().map(str::to_owned);
        state
            .app
            .store
            .upsert_genre_alias(tag, canonical.as_deref(), "llm")
            .map_err(CommandError::from)?;
        if canonical.is_some() { resolved += 1; } else { unresolved += 1; }
    }

    Ok(NormaliseResult { resolved, unresolved })
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn build_http() -> Result<reqwest::Client, CommandError> {
    reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(|e| CommandError { kind: "http".into(), message: e.to_string() })
}

fn build_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .unwrap_or_default()
}

fn load_session(
    state: &State<'_, AppState>,
    settings: &Settings,
) -> Result<pc_core::spotify::auth::Session, CommandError> {
    pc_core::spotify::auth::Session::load(&state.app.paths, settings)
        .map_err(CommandError::from)
}
