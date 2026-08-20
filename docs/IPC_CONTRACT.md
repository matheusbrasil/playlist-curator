# IPC contract

The frontend is a shell. It renders and calls commands; it holds no business
logic, no SQL, and never sees an OAuth token. Every command below is a Tauri
`invoke` target implemented in `src-tauri/src/commands.rs` as a thin wrapper over
`pc-core`.

Names are `snake_case` on the Rust side and called with the same string from
`invoke()`. Arguments are passed as a single object with `camelCase` keys (Tauri
converts to `snake_case` parameters).

## Errors

Every command rejects with `{ kind: string, message: string }`. `kind` is stable
and machine-readable; branch on it, show `message` to the user.

| `kind` | Meaning / what the UI should do |
|---|---|
| `not_authenticated` | Send the user to the Connect screen. |
| `quota_exceeded` | Spotify's per-developer-account quota is spent. Retrying will not help — say so and stop. |
| `spotify_api` | Generic upstream failure; offer retry. |
| `config` | Something is unset (e.g. Client ID). Link to Settings. |
| `credential` | OS credential vault unavailable. |
| `invalid_filter` | The query could not be turned into a valid filter (e.g. unknown genre). Show `message` verbatim. |
| `upstream` | MusicBrainz/Last.fm/Discogs/Wikidata failure. Non-fatal; enrichment continues. |
| `db`, `pool`, `http`, `json`, `io`, `oauth`, `other` | Unexpected. Show `message`. |

## Connect

```ts
connection_status() -> {
  connected: boolean,
  clientIdConfigured: boolean,
  user: { id: string, displayName: string | null, product: string | null } | null,
  // Development Mode requires the app owner to hold Premium.
  premiumWarning: boolean,
  tokenStore: "keyring" | "file",
}

// Opens the system browser, runs PKCE against the 127.0.0.1:14523 loopback,
// and resolves once tokens are stored. Long-running: may take as long as the
// user takes to approve.
spotify_login() -> { id: string, displayName: string | null, product: string | null }

spotify_logout() -> void
```

## Settings

```ts
get_settings() -> Settings
save_settings({ settings: Settings }) -> void

type Settings = {
  spotifyClientId: string | null,
  lastfmApiKey: string | null,
  discogsToken: string | null,
  llm: {
    provider: "disabled" | "ollama" | "anthropic",
    ollamaUrl: string,
    ollamaModel: string,
    anthropicModel: string,
    anthropicApiKey: string | null,
  },
  cache: {
    musicbrainzTtlDays: number,
    lastfmTtlDays: number,
    discogsTtlDays: number,
    wikidataTtlDays: number,
  },
  weights: {
    musicbrainzGenre: number, discogs: number, musicbrainzTag: number,
    lastfmArtist: number, lastfmTrack: number, spotifyArtist: number,
  },
  dryRun: boolean,          // defaults true; the UI must show this prominently
  reviewThreshold: number,  // 0..1
}

clear_cache() -> { rowsDeleted: number }
export_database({ destPath: string }) -> void
```

## Playlists

```ts
// Cached list; instant. Returns [] before the first sync.
list_playlists() -> Playlist[]

// Hits Spotify and refreshes the cached list.
sync_playlists() -> Playlist[]

type Playlist = {
  spotifyId: string,
  name: string,
  owner: string | null,
  snapshotId: string | null,
  trackCount: number | null,
  // null = never imported. The UI shows "not analysed yet".
  syncedAt: string | null,
}

// Imports the playlist's items into the local store.
import_playlist({ playlistId: string }) -> ImportStats

type ImportStats = {
  itemsSeen: number,
  tracksImported: number,
  withIsrc: number,          // show as a coverage badge; low = weaker matching
  artistsImported: number,
  skippedLocal: number,
  skippedEpisodes: number,
  skippedUnresolvable: number,
}
```

## Analysis

```ts
// Long-running. Progress arrives on the "enrich://progress" event, not as a
// return value. Resumable: calling it again continues where it stopped.
enrich_playlist({ playlistId: string }) -> EnrichStats

// Recomputes genres/origin/era from already-cached raw signals. No network.
// Cheap — safe to call after changing source weights or an override.
derive_playlist({ playlistId: string }) -> { tracksWithGenre: number, originsResolved: number, erasResolved: number }

analysis_summary({ playlistId: string }) -> {
  trackCount: number,
  isrcCoverage: number,        // 0..1
  mbCoverage: number,          // 0..1 — phase 3 target is >= 0.8
  genreDistribution: { slug: string, label: string, count: number }[],
  countryDistribution: { code: string, label: string, count: number }[],
  decadeDistribution: { decade: number, count: number }[],
  needsReviewCount: number,
}

analysis_tracks({ playlistId: string }) -> AnalysedTrack[]

type AnalysedTrack = {
  spotifyId: string,
  name: string,
  artists: { spotifyId: string, name: string }[],
  isrc: string | null,
  genres: { slug: string, score: number }[],
  // Where the metadata came from, so the user can judge it.
  origin: { countryCode: string | null, countryLabel: string | null, city: string | null, source: string, confidence: number } | null,
  era: { year: number | null, decade: number | null, source: string } | null,
  needsReview: boolean,
}

list_reviews() -> { entityType: string, entityId: string, reason: string, detail: string | null, createdAt: string }[]
```

### Events

```ts
// listen("enrich://progress", …)
type EnrichProgress =
  | { type: "started",  total: number }
  | { type: "track",    done: number, total: number, name: string }
  | { type: "artist",   done: number, total: number, name: string }
  | { type: "finished", stats: EnrichStats }

type EnrichStats = {
  tracksTotal: number, tracksMatched: number,
  artistsTotal: number, artistsMatched: number,
  signalsRecorded: number, needsReview: number,
  cacheHits: number, networkCalls: number,
}
```

## Suggestions

```ts
// Enumerate facets and score candidates. Pure local SQL; no network.
suggest_playlists({ playlistId: string }) -> SuggestionCard[]

// Free-text query. Uses the LLM if configured, else a deterministic parser.
suggest_from_query({ playlistId: string, query: string }) -> SuggestionCard

// Run an explicit filter built from the UI's dropdowns — the no-LLM path.
suggest_from_filter({ playlistId: string, filter: PlaylistFilter }) -> SuggestionCard

type SuggestionCard = {
  id: string,
  proposedName: string,
  description: string,
  filter: PlaylistFilter,
  trackCount: number,
  score: { total: number, size: number, coherence: number, specificity: number, redundancy: number, confidence: number },
  tracks: {
    spotifyId: string,
    name: string,
    artists: string[],
    // Why this track is in this playlist — shown in the review table.
    reason: { genre: string | null, genreScore: number, genreSource: string | null,
              countryCode: string | null, year: number | null, eraSource: string | null,
              needsReview: boolean },
  }[],
}

type PlaylistFilter = {
  genres: string[],
  genreMode: "any" | "any_with_children" | "all",
  countries: string[],
  yearRange: [number, number] | null,
  minTracks: number | null,
  maxTracks: number | null,
  minGenreScore: number | null,
  sourcePlaylistId: string | null,
  excludeNeedsReview: boolean,
}

// Vocabulary for the dropdowns, so the UI never invents a genre.
list_genres() -> { slug: string, label: string, parentSlug: string | null }[]
list_countries({ playlistId: string }) -> { code: string, label: string, count: number }[]
list_decades({ playlistId: string }) -> { decade: number, count: number }[]
```

## Creating playlists

```ts
// Honours settings.dryRun. When dry-run is on, nothing is written to Spotify and
// `created` is false — the UI must make that state obvious.
create_playlist({ card: SuggestionCard, public: boolean, dryRunOverride: boolean | null })
  -> {
    dryRun: boolean,
    created: boolean,
    spotifyId: string | null,
    spotifyUrl: string | null,
    name: string,
    trackCount: number,
    // Tracks that could not be added (local files have no URI).
    skipped: { spotifyId: string, name: string, reason: string }[],
  }

list_created_playlists() -> { spotifyId: string, name: string, createdAt: string, recipe: PlaylistFilter }[]
```

## Overrides

```ts
// Always wins over every derived value, permanently.
set_override({ entityType: "track" | "artist", entityId: string,
               field: "genre" | "country" | "year", value: string }) -> void
clear_override({ entityType: string, entityId: string, field: string }) -> void
resolve_review({ entityType: string, entityId: string, reason: string }) -> void
```

## LLM

```ts
llm_status() -> { provider: string, available: boolean, detail: string }
// Resolve queued unknown tags to canonical genres. Results are cached in
// `genre_alias` permanently, so each tag is decided once.
normalize_orphan_tags({ limit: number }) -> { resolved: number, unresolved: number }
```
