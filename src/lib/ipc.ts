import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open as shellOpen } from "@tauri-apps/plugin-shell";

/**
 * Typed surface of `docs/IPC_CONTRACT.md`. This module is the only place that
 * touches `invoke`/`listen`; everything else in the UI imports from here.
 */

// ------------------------------------------------------------------ errors

/** Stable discriminants from `pc_core::CoreError::kind`. */
export const CORE_ERROR_KINDS = [
  "not_authenticated",
  "quota_exceeded",
  "spotify_api",
  "config",
  "credential",
  "invalid_filter",
  "upstream",
  "db",
  "pool",
  "http",
  "json",
  "io",
  "oauth",
  "other",
] as const;

export type KnownCoreErrorKind = (typeof CORE_ERROR_KINDS)[number];

/** `kind` is widened to `string`: the core may add discriminants. */
export type CoreError = { kind: string; message: string };

export class IpcError extends Error implements CoreError {
  readonly kind: string;

  constructor(err: CoreError) {
    super(err.message);
    this.name = "IpcError";
    this.kind = err.kind;
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

/** Normalise anything a rejected `invoke` can throw into a `CoreError`. */
export function toCoreError(value: unknown): CoreError {
  if (value instanceof IpcError) {
    return { kind: value.kind, message: value.message };
  }
  if (isRecord(value) && typeof value["kind"] === "string") {
    const message = value["message"];
    return {
      kind: value["kind"],
      message: typeof message === "string" && message.length > 0 ? message : value["kind"],
    };
  }
  if (value instanceof Error) {
    return { kind: "other", message: value.message };
  }
  if (typeof value === "string") {
    return { kind: "other", message: value };
  }
  return { kind: "other", message: "Unexpected failure: " + safeStringify(value) };
}

function safeStringify(value: unknown): string {
  try {
    return JSON.stringify(value) ?? String(value);
  } catch {
    return String(value);
  }
}

/** Retrying a spent developer-account quota can never succeed. */
export function isRetryable(err: CoreError): boolean {
  switch (err.kind) {
    case "quota_exceeded":
    case "not_authenticated":
    case "config":
    case "invalid_filter":
      return false;
    default:
      return true;
  }
}

async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return args === undefined
      ? await invoke<T>(command)
      : await invoke<T>(command, args);
  } catch (err) {
    throw new IpcError(toCoreError(err));
  }
}

// ------------------------------------------------------------------ connect

export type TokenStoreKind = "keyring" | "file";

export type SpotifyUser = {
  id: string;
  displayName: string | null;
  product: string | null;
};

export type ConnectionStatus = {
  connected: boolean;
  clientIdConfigured: boolean;
  user: SpotifyUser | null;
  /** Development Mode requires the app owner to hold Premium. */
  premiumWarning: boolean;
  tokenStore: TokenStoreKind;
};

export function connectionStatus(): Promise<ConnectionStatus> {
  return call<ConnectionStatus>("connection_status");
}

/** Opens the system browser and completes PKCE on 127.0.0.1:14523. Long-running. */
export function spotifyLogin(): Promise<SpotifyUser> {
  return call<SpotifyUser>("spotify_login");
}

export function spotifyLogout(): Promise<void> {
  return call<void>("spotify_logout");
}

// ------------------------------------------------------------------ settings

export type LlmProvider = "disabled" | "ollama" | "anthropic";

export type LlmSettings = {
  provider: LlmProvider;
  ollamaUrl: string;
  ollamaModel: string;
  anthropicModel: string;
  anthropicApiKey: string | null;
};

export type CacheSettings = {
  musicbrainzTtlDays: number;
  lastfmTtlDays: number;
  discogsTtlDays: number;
  wikidataTtlDays: number;
};

export type SourceWeights = {
  musicbrainzGenre: number;
  discogs: number;
  musicbrainzTag: number;
  lastfmArtist: number;
  lastfmTrack: number;
  spotifyArtist: number;
};

export type Settings = {
  spotifyClientId: string | null;
  lastfmApiKey: string | null;
  discogsToken: string | null;
  /** Sent in the MusicBrainz User-Agent header — must be a real email MB can use to contact you. */
  mbContactEmail: string | null;
  llm: LlmSettings;
  cache: CacheSettings;
  weights: SourceWeights;
  /** Defaults true. The UI must show this prominently. */
  dryRun: boolean;
  /** 0..1 */
  reviewThreshold: number;
};

export const SOURCE_WEIGHT_KEYS: readonly (keyof SourceWeights)[] = [
  "musicbrainzGenre",
  "discogs",
  "musicbrainzTag",
  "lastfmArtist",
  "lastfmTrack",
  "spotifyArtist",
];

export function getSettings(): Promise<Settings> {
  return call<Settings>("get_settings");
}

export function saveSettings(settings: Settings): Promise<void> {
  return call<void>("save_settings", { settings });
}

export function clearCache(): Promise<{ rowsDeleted: number }> {
  return call<{ rowsDeleted: number }>("clear_cache");
}

export function exportDatabase(destPath: string): Promise<void> {
  return call<void>("export_database", { destPath });
}

// ------------------------------------------------------------------ playlists

export type Playlist = {
  spotifyId: string;
  name: string;
  owner: string | null;
  snapshotId: string | null;
  trackCount: number | null;
  /** null = never imported. */
  syncedAt: string | null;
};

export type ImportStats = {
  itemsSeen: number;
  tracksImported: number;
  /** Low ISRC coverage means weaker MusicBrainz matching. */
  withIsrc: number;
  artistsImported: number;
  skippedLocal: number;
  skippedEpisodes: number;
  skippedUnresolvable: number;
};

export function listPlaylists(): Promise<Playlist[]> {
  return call<Playlist[]>("list_playlists");
}

export function syncPlaylists(): Promise<Playlist[]> {
  return call<Playlist[]>("sync_playlists");
}

export function importPlaylist(playlistId: string): Promise<ImportStats> {
  return call<ImportStats>("import_playlist", { playlistId });
}

// ------------------------------------------------------------------ analysis

export type EnrichStats = {
  tracksProcessed: number;
  recordingsResolved: number;
  artistsResolved: number;
  nameMatched: number;
  needsReview: number;
  tagSignalsInserted: number;
  cacheHits: number;
  networkCalls: number;
};

export type DeriveStats = {
  tracksWithGenre: number;
  originsResolved: number;
  erasResolved: number;
};

export type GenreCount = { slug: string; label: string; count: number };
export type CountryCount = { code: string; label: string; count: number };
export type DecadeCount = { decade: number; count: number };

export type AnalysisSummary = {
  trackCount: number;
  /** 0..1 */
  isrcCoverage: number;
  /** 0..1 */
  mbCoverage: number;
  genreDistribution: GenreCount[];
  countryDistribution: CountryCount[];
  decadeDistribution: DecadeCount[];
  needsReviewCount: number;
};

export type TrackArtist = { spotifyId: string; name: string };
export type TrackGenre = { slug: string; score: number };

export type TrackOrigin = {
  countryCode: string | null;
  countryLabel: string | null;
  city: string | null;
  source: string;
  confidence: number;
};

export type TrackEra = {
  year: number | null;
  decade: number | null;
  source: string;
};

export type AnalysedTrack = {
  spotifyId: string;
  name: string;
  artists: TrackArtist[];
  isrc: string | null;
  genres: TrackGenre[];
  origin: TrackOrigin | null;
  era: TrackEra | null;
  needsReview: boolean;
};

export type ReviewItem = {
  entityType: string;
  entityId: string;
  reason: string;
  detail: string | null;
  createdAt: string;
};

/** Long-running; progress arrives on `enrich://progress`. Resumable. */
export function enrichPlaylist(
  playlistId: string,
  limit?: number,
  onlyUnresolved?: boolean,
): Promise<EnrichStats> {
  return call<EnrichStats>("enrich_playlist", {
    playlistId,
    limit: limit ?? null,
    onlyUnresolved: onlyUnresolved ?? null,
  });
}

export function enrichCounts(
  playlistId: string,
): Promise<{ total: number; unresolved: number }> {
  return call<{ total: number; unresolved: number }>("enrich_counts", { playlistId });
}

/** Recomputes from cached signals. No network — cheap enough to call on change. */
export function derivePlaylist(playlistId: string): Promise<DeriveStats> {
  return call<DeriveStats>("derive_playlist", { playlistId });
}

export function analysisSummary(playlistId: string): Promise<AnalysisSummary> {
  return call<AnalysisSummary>("analysis_summary", { playlistId });
}

export function analysisTracks(playlistId: string): Promise<AnalysedTrack[]> {
  return call<AnalysedTrack[]>("analysis_tracks", { playlistId });
}

export function listReviews(): Promise<ReviewItem[]> {
  return call<ReviewItem[]>("list_reviews");
}

// ------------------------------------------------------------------ events

export type EnrichProgress = {
  playlistId: string;
  current: number;
  total: number;
  trackName: string;
  stats: EnrichStats;
};

export const ENRICH_PROGRESS_EVENT = "enrich://progress";

/** Resolves to the unlisten function; call it on unmount. */
export function listenEnrichProgress(
  cb: (progress: EnrichProgress) => void,
): Promise<UnlistenFn> {
  return listen<EnrichProgress>(ENRICH_PROGRESS_EVENT, (event) => cb(event.payload));
}

export type { UnlistenFn };

// ------------------------------------------------------------------ suggestions

export type GenreMode = "any" | "any_with_children" | "all";

export type PlaylistFilter = {
  genres: string[];
  genreMode: GenreMode;
  countries: string[];
  yearRange: [number, number] | null;
  minTracks: number | null;
  maxTracks: number | null;
  minGenreScore: number | null;
  sourcePlaylistId: string | null;
  excludeNeedsReview: boolean;
};

export const EMPTY_FILTER: PlaylistFilter = {
  genres: [],
  genreMode: "any_with_children",
  countries: [],
  yearRange: null,
  minTracks: null,
  maxTracks: null,
  minGenreScore: null,
  sourcePlaylistId: null,
  excludeNeedsReview: true,
};

export type SuggestionScore = {
  total: number;
  size: number;
  coherence: number;
  specificity: number;
  redundancy: number;
  confidence: number;
};

/** Why a track is in a suggestion — shown in the card's review table. */
export type TrackReason = {
  genre: string | null;
  genreScore: number;
  genreSource: string | null;
  countryCode: string | null;
  year: number | null;
  eraSource: string | null;
  needsReview: boolean;
};

export type SuggestionTrack = {
  spotifyId: string;
  name: string;
  artists: string[];
  reason: TrackReason;
};

export type SuggestionCard = {
  id: string;
  proposedName: string;
  description: string;
  filter: PlaylistFilter;
  trackCount: number;
  score: SuggestionScore;
  tracks: SuggestionTrack[];
};

export type CanonicalGenre = {
  slug: string;
  label: string;
  parentSlug: string | null;
};

export function suggestPlaylists(playlistId: string): Promise<SuggestionCard[]> {
  return call<SuggestionCard[]>("suggest_playlists", { playlistId });
}

export function suggestFromQuery(playlistId: string, query: string): Promise<SuggestionCard> {
  return call<SuggestionCard>("suggest_from_query", { playlistId, query });
}

/** The no-LLM path: an explicit filter built from the dropdowns. */
export function suggestFromFilter(
  playlistId: string,
  filter: PlaylistFilter,
): Promise<SuggestionCard> {
  return call<SuggestionCard>("suggest_from_filter", { playlistId, filter });
}

export function listGenres(): Promise<CanonicalGenre[]> {
  return call<CanonicalGenre[]>("list_genres");
}

export function listCountries(playlistId: string): Promise<CountryCount[]> {
  return call<CountryCount[]>("list_countries", { playlistId });
}

export function listDecades(playlistId: string): Promise<DecadeCount[]> {
  return call<DecadeCount[]>("list_decades", { playlistId });
}

// ------------------------------------------------------------------ creating

export type SkippedTrack = { spotifyId: string; name: string; reason: string };

export type CreateResult = {
  dryRun: boolean;
  created: boolean;
  spotifyId: string | null;
  spotifyUrl: string | null;
  name: string;
  trackCount: number;
  /** Local files have no URI, so they can never be added. */
  skipped: SkippedTrack[];
};

export type CreatedPlaylist = {
  spotifyId: string;
  name: string;
  createdAt: string;
  recipe: PlaylistFilter;
};

export function createPlaylist(
  card: SuggestionCard,
  isPublic: boolean,
  dryRun: boolean,
): Promise<CreateResult> {
  return call<CreateResult>("create_playlist", {
    card,
    public: isPublic,
    dryRun,
  });
}

export function listCreatedPlaylists(): Promise<CreatedPlaylist[]> {
  return call<CreatedPlaylist[]>("list_created_playlists");
}

// ------------------------------------------------------------------ overrides

export type OverrideEntityType = "track" | "artist";
export type OverrideField = "genre" | "country" | "year";

/** Wins over every derived value, permanently. */
export function setOverride(args: {
  entityType: OverrideEntityType;
  entityId: string;
  field: OverrideField;
  value: string;
}): Promise<void> {
  return call<void>("set_override", { ...args });
}

export function clearOverride(args: {
  entityType: string;
  entityId: string;
  field: string;
}): Promise<void> {
  return call<void>("clear_override", { ...args });
}

export function resolveReview(args: {
  entityType: string;
  entityId: string;
  reason: string;
}): Promise<void> {
  return call<void>("resolve_review", { ...args });
}

// ------------------------------------------------------------------ llm

export type LlmStatus = { provider: string; available: boolean; detail: string };

export function llmStatus(): Promise<LlmStatus> {
  return call<LlmStatus>("llm_status");
}

/** Each tag is decided once; the answer is cached in `genre_alias` forever. */
export function normalizeOrphanTags(limit: number): Promise<{
  resolved: number;
  unresolved: number;
}> {
  return call<{ resolved: number; unresolved: number }>("normalize_orphan_tags", { limit });
}

// ------------------------------------------------------------------ shell

/** Open a URL in the system browser rather than inside the webview. */
export async function openExternal(url: string): Promise<void> {
  try {
    await shellOpen(url);
  } catch (err) {
    throw new IpcError(toCoreError(err));
  }
}
