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
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;

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
    let client_id_configured =
        settings.spotify_client_id.as_deref().is_some_and(|s| !s.is_empty());
    let data_dir = &state.app.paths.data_dir;
    let token_store =
        pc_core::spotify::auth::TokenStore::detect(data_dir).name().to_owned();

    let http = build_http()?;
    let session_result =
        pc_core::spotify::auth::Session::from_settings(&settings, data_dir, http.clone());

    let (connected, user, premium_warning) = match session_result {
        Ok(session) if session.is_connected().await => {
            let client = pc_core::spotify::client::SpotifyClient::new(http, Arc::new(session));
            match client.me().await {
                Ok(u) => {
                    let premium = u.product.as_deref() != Some("premium");
                    (
                        true,
                        Some(SpotifyUser {
                            id: u.id,
                            display_name: u.display_name,
                            product: u.product,
                        }),
                        premium,
                    )
                }
                Err(_) => (false, None, false),
            }
        }
        _ => (false, None, false),
    };

    Ok(ConnectionStatus {
        connected,
        client_id_configured,
        user,
        premium_warning,
        token_store,
    })
}

#[tauri::command]
pub async fn spotify_login(
    state: State<'_, AppState>,
    app_handle: AppHandle,
) -> Cmd<SpotifyUser> {
    let settings = state.app.settings();
    let client_id = settings
        .require_client_id()
        .map_err(CommandError::from)?
        .to_owned();
    let http = build_http()?;

    // Build the authorize URL and open the system browser.
    let pending = pc_core::spotify::auth::begin(&client_id);
    app_handle
        .opener()
        .open_url(&pending.authorize_url, None::<&str>)
        .map_err(|e| CommandError { kind: "shell".into(), message: e.to_string() })?;

    // tiny_http is synchronous, so the wait runs on a blocking thread.
    let expected_state = pending.state.clone();
    let verifier = pending.pkce.verifier.clone();
    let code = tokio::task::spawn_blocking(move || {
        pc_core::spotify::auth::wait_for_callback(
            &expected_state,
            std::time::Duration::from_secs(300),
        )
    })
    .await
    .map_err(|e| CommandError { kind: "internal".into(), message: e.to_string() })?
    .map_err(CommandError::from)?;

    let tokens =
        pc_core::spotify::auth::exchange_code(&http, &client_id, &code, &verifier)
            .await
            .map_err(CommandError::from)?;

    let data_dir = &state.app.paths.data_dir;
    let session = pc_core::spotify::auth::Session::new(
        pc_core::spotify::auth::TokenStore::detect(data_dir),
        client_id,
        http.clone(),
    )
    .map_err(CommandError::from)?;
    session.set_tokens(tokens).await.map_err(CommandError::from)?;

    let client = pc_core::spotify::client::SpotifyClient::new(http, Arc::new(session));
    let user = client.me().await.map_err(CommandError::from)?;
    Ok(SpotifyUser { id: user.id, display_name: user.display_name, product: user.product })
}

#[tauri::command]
pub async fn spotify_logout(state: State<'_, AppState>) -> Cmd<()> {
    pc_core::spotify::auth::TokenStore::detect(&state.app.paths.data_dir)
        .clear()
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
pub async fn export_database(state: State<'_, AppState>, dest_path: String) -> Cmd<()> {
    state.app.store.export_to(&dest_path).map_err(CommandError::from)
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
    let session = load_session(&state, &settings, http.clone())?;
    let client = pc_core::spotify::client::SpotifyClient::new(http, Arc::new(session));

    let playlists = client.my_playlists().await.map_err(CommandError::from)?;
    for p in &playlists {
        let playlist = pc_core::model::Playlist {
            spotify_id: p.id.clone(),
            name: p.name.clone(),
            owner: p.owner.as_ref().map(|o| o.id.clone()),
            snapshot_id: p.snapshot_id.clone(),
            track_count: p.tracks.as_ref().and_then(|t| t.total),
            synced_at: None,
        };
        state.app.store.upsert_playlist(&playlist).map_err(CommandError::from)?;
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
    let session = load_session(&state, &settings, http.clone())?;
    let client = pc_core::spotify::client::SpotifyClient::new(http, Arc::new(session));

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
    limit: Option<usize>,
    only_unresolved: Option<bool>,
) -> Cmd<pc_core::enrich::pipeline::EnrichStats> {
    let settings = state.app.settings();
    let store = state.app.store.clone();

    let on_progress = {
        let handle = app_handle.clone();
        Arc::new(move |progress: pc_core::enrich::pipeline::EnrichProgress| {
            let _ = handle.emit("enrich://progress", &progress);
        })
    };

    pc_core::enrich::pipeline::enrich_playlist(store, settings, &playlist_id, limit, only_unresolved, on_progress)
        .await
        .map_err(CommandError::from)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrichCounts {
    pub total: i64,
    pub unresolved: i64,
}

#[tauri::command]
pub async fn enrich_counts(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<EnrichCounts> {
    let (total, unresolved) = state.app.store.enrich_counts(&playlist_id).map_err(CommandError::from)?;
    Ok(EnrichCounts { total, unresolved })
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
    let tracks_with_genre =
        aggregate::derive_playlist_genres(&state.app.store, &settings, &playlist_id)
            .map_err(CommandError::from)?;

    Ok(DeriveResult { tracks_with_genre, origins_resolved: origins, eras_resolved: eras })
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

    let (total_tracks, with_isrc) =
        store.isrc_coverage(&playlist_id).map_err(CommandError::from)?;
    let (_, mb_resolved) =
        store.mb_resolution_coverage(&playlist_id).map_err(CommandError::from)?;

    let isrc_coverage =
        if total_tracks > 0 { with_isrc as f64 / total_tracks as f64 } else { 0.0 };
    let mb_coverage =
        if total_tracks > 0 { mb_resolved as f64 / total_tracks as f64 } else { 0.0 };

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
    use serde_json::json;

    let store = &state.app.store;
    let pts = store.playlist_tracks(&playlist_id).map_err(CommandError::from)?;
    // Load reviews once outside the loop.
    let reviews = store.open_reviews().map_err(CommandError::from)?;
    let mut result = Vec::with_capacity(pts.len());

    for pt in pts {
        let track = &pt.track;
        let genres = store.track_genres(&track.spotify_id).map_err(CommandError::from)?;
        let first_artist_id = pt.artists.first().map(|a| a.spotify_id.as_str()).unwrap_or("");
        let origin = store.artist_origin(first_artist_id).map_err(CommandError::from)?;
        let era = store.track_era(&track.spotify_id).map_err(CommandError::from)?;
        let needs_review = reviews.iter().any(|r| r.entity_id == track.spotify_id);

        result.push(json!({
            "spotifyId": track.spotify_id,
            "name": track.name,
            "isrc": track.isrc,
            "artists": pt.artists.iter().map(|a| json!({
                "spotifyId": a.spotify_id,
                "name": a.name,
            })).collect::<Vec<_>>(),
            "genres": genres.iter().map(|g| json!({
                "slug": g.canonical_slug,
                "score": g.score,
            })).collect::<Vec<_>>(),
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

/// Re-run enrichment for a single track, then re-derive genres/origins/eras
/// for every playlist that contains it.
#[tauri::command]
pub async fn retry_enrich_track(
    state: State<'_, AppState>,
    track_id: String,
    reason: String,
) -> Cmd<pc_core::enrich::pipeline::EnrichStats> {
    let settings = state.app.settings();
    let store = state.app.store.clone();

    let stats = pc_core::enrich::pipeline::enrich_track(
        store.clone(),
        settings,
        &track_id,
        &reason,
    )
    .await
    .map_err(CommandError::from)?;

    let playlist_ids = store.playlists_for_track(&track_id).map_err(CommandError::from)?;
    for playlist_id in &playlist_ids {
        use pc_core::taxonomy::{aggregate, derive};
        let settings = state.app.settings();
        let _ = derive::derive_origins_for_playlist(&store, playlist_id);
        let _ = derive::derive_eras_for_playlist(&store, playlist_id);
        let _ = aggregate::derive_playlist_genres(&store, &settings, playlist_id);
    }

    Ok(stats)
}

// ── Suggestions ───────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn suggest_playlists(
    state: State<'_, AppState>,
    playlist_id: String,
) -> Cmd<Vec<pc_core::suggest::SuggestionCard>> {
    pc_core::suggest::suggest(&state.app.store, &playlist_id).map_err(CommandError::from)
}

#[tauri::command]
pub async fn suggest_from_query(
    state: State<'_, AppState>,
    playlist_id: String,
    query: String,
) -> Cmd<pc_core::suggest::SuggestionCard> {
    use pc_core::llm::Llm;
    use pc_core::suggest::{facets, nl, score_candidate};

    let settings = state.app.settings();
    let store = &state.app.store;

    // Deterministic parser first; fall back to LLM if it can't parse.
    let filter = match nl::parse(store, &query, Some(&playlist_id)) {
        Ok(f) => f,
        Err(parse_err) => {
            let llm = Llm::from_settings(&settings, build_http_client());
            if llm.is_enabled() {
                let all_genres = store.all_canonical_genres().map_err(CommandError::from)?;
                let vocab: Vec<String> = all_genres.iter().map(|g| g.slug.clone()).collect();
                let schema = pc_core::llm::prompts::nl_filter_schema();
                let prompt_str =
                    pc_core::llm::prompts::nl_to_filter_prompt(&query, &vocab, &[]);
                let raw = llm
                    .complete_json(
                        &schema,
                        pc_core::llm::prompts::SYSTEM_NL_TO_FILTER,
                        &prompt_str,
                    )
                    .await
                    .map_err(CommandError::from)?;
                let mut f: PlaylistFilter = serde_json::from_value(raw)
                    .map_err(|e| CommandError { kind: "parse".into(), message: e.to_string() })?;
                f.source_playlist_id = Some(playlist_id.clone());
                f.validate(store).map_err(CommandError::from)?;
                f
            } else {
                return Err(CommandError::from(parse_err));
            }
        }
    };

    let tracks = pc_core::suggest::execute(store, &filter).map_err(CommandError::from)?;
    let score = score_candidate(&tracks, filter.specificity(), 0.0);
    let (proposed_name, description) =
        facets::name_for(store, &filter, tracks.len()).map_err(CommandError::from)?;

    Ok(pc_core::suggest::SuggestionCard {
        id: card_id(&filter),
        proposed_name,
        description,
        track_count: tracks.len(),
        score,
        tracks,
        filter,
    })
}

#[tauri::command]
pub async fn suggest_from_filter(
    state: State<'_, AppState>,
    filter: PlaylistFilter,
) -> Cmd<pc_core::suggest::SuggestionCard> {
    use pc_core::suggest::{facets, score_candidate};

    let store = &state.app.store;
    let tracks = pc_core::suggest::execute(store, &filter).map_err(CommandError::from)?;
    let score = score_candidate(&tracks, filter.specificity(), 0.0);
    let (proposed_name, description) =
        facets::name_for(store, &filter, tracks.len()).map_err(CommandError::from)?;

    Ok(pc_core::suggest::SuggestionCard {
        id: card_id(&filter),
        proposed_name,
        description,
        track_count: tracks.len(),
        score,
        tracks,
        filter,
    })
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
    state
        .app
        .store
        .all_canonical_genres()
        .map_err(CommandError::from)
        .map(|gs| {
            gs.into_iter()
                .map(|g| GenreEntry { slug: g.slug, label: g.label, parent_slug: g.parent_slug })
                .collect()
        })
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
    dry_run: bool,
) -> Cmd<CreateResult> {
    let settings = state.app.settings();

    let http = build_http()?;
    let session = load_session(&state, &settings, http.clone())?;
    let client = pc_core::spotify::client::SpotifyClient::new(http, Arc::new(session));

    let result = pc_core::spotify::publish::create_from_card(
        &client,
        &state.app.store,
        &card,
        public,
        dry_run,
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
pub async fn list_created_playlists(state: State<'_, AppState>) -> Cmd<Vec<serde_json::Value>> {
    use serde_json::json;
    pc_core::spotify::publish::list_created(&state.app.store)
        .map_err(CommandError::from)
        .map(|records| {
            records
                .into_iter()
                .map(|r| {
                    json!({
                        "spotifyId": r.spotify_id,
                        "name": r.name,
                        "createdAt": r.created_at,
                        "recipe": r.recipe,
                    })
                })
                .collect()
        })
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
        .set_override(&entity_type, &entity_id, &field, Some(&value))
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
        (
            llm.is_enabled(),
            if llm.is_enabled() { "Configured".into() } else { "No provider configured".into() },
        )
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
    use pc_core::llm::{prompts, Llm};

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
    let prompt_str = prompts::normalise_tags_prompt(&batch, &vocab);
    let result = llm
        .complete_json(&schema, prompts::SYSTEM_NORMALISE_TAGS, &prompt_str)
        .await
        .map_err(CommandError::from)?;

    let mut resolved = 0usize;
    let mut unresolved = 0usize;

    for tag in &batch {
        let canonical = result[tag].as_str().map(str::to_owned);
        state
            .app
            .store
            .upsert_genre_alias(tag, canonical.as_deref(), "llm")
            .map_err(CommandError::from)?;
        if canonical.is_some() {
            resolved += 1;
        } else {
            unresolved += 1;
        }
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
    reqwest::Client::builder().use_rustls_tls().build().unwrap_or_default()
}

fn load_session(
    state: &State<'_, AppState>,
    settings: &Settings,
    http: reqwest::Client,
) -> Result<pc_core::spotify::auth::Session, CommandError> {
    pc_core::spotify::auth::Session::from_settings(settings, &state.app.paths.data_dir, http)
        .map_err(CommandError::from)
}

/// A stable ID for a user-constructed card derived from the filter's key fields.
fn card_id(filter: &PlaylistFilter) -> String {
    let mut s = filter.genres.join(",");
    s.push('|');
    s.push_str(&filter.countries.join(","));
    if let Some((from, to)) = filter.year_range {
        use std::fmt::Write;
        let _ = write!(s, "|{from}-{to}");
    }
    s
}
