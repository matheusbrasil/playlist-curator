-- Playlist Curator — initial schema.
--
-- Design notes that matter:
--  * tag_signal stores raw, never-overwritten source data. Reprocessing the
--    taxonomy must never require refetching the network.
--  * api_cache is keyed by URL hash with long TTLs, so a second run of the
--    same playlist does zero network I/O.
--  * user_override always wins over any derived value.

-- ---------------------------------------------------------------- Spotify identity

CREATE TABLE IF NOT EXISTS track (
    spotify_id            TEXT PRIMARY KEY,
    name                  TEXT NOT NULL,
    isrc                  TEXT,
    duration_ms           INTEGER,
    spotify_album_id      TEXT,
    spotify_release_date  TEXT,
    is_local              INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_track_isrc ON track(isrc) WHERE isrc IS NOT NULL;

CREATE TABLE IF NOT EXISTS artist (
    spotify_id  TEXT PRIMARY KEY,
    name        TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS track_artist (
    track_spotify_id   TEXT NOT NULL REFERENCES track(spotify_id) ON DELETE CASCADE,
    artist_spotify_id  TEXT NOT NULL REFERENCES artist(spotify_id) ON DELETE CASCADE,
    position           INTEGER NOT NULL,
    PRIMARY KEY (track_spotify_id, artist_spotify_id)
);
CREATE INDEX IF NOT EXISTS idx_track_artist_artist ON track_artist(artist_spotify_id);

CREATE TABLE IF NOT EXISTS playlist (
    spotify_id   TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    owner        TEXT,
    snapshot_id  TEXT,
    track_count  INTEGER,
    synced_at    TEXT
);

CREATE TABLE IF NOT EXISTS playlist_track (
    playlist_id       TEXT NOT NULL REFERENCES playlist(spotify_id) ON DELETE CASCADE,
    track_spotify_id  TEXT NOT NULL REFERENCES track(spotify_id) ON DELETE CASCADE,
    position          INTEGER NOT NULL,
    added_at          TEXT,
    PRIMARY KEY (playlist_id, track_spotify_id, position)
);
CREATE INDEX IF NOT EXISTS idx_playlist_track_track ON playlist_track(track_spotify_id);

-- ---------------------------------------------------------------- Bridge to the real world

CREATE TABLE IF NOT EXISTS mb_recording (
    mbid                TEXT PRIMARY KEY,
    title               TEXT,
    first_release_date  TEXT,
    resolved_via        TEXT,     -- isrc | search | url-rel
    confidence          REAL NOT NULL DEFAULT 0.0
);

CREATE TABLE IF NOT EXISTS mb_artist (
    mbid          TEXT PRIMARY KEY,
    name          TEXT,
    sort_name     TEXT,
    type          TEXT,
    country       TEXT,
    area          TEXT,
    begin_area    TEXT,
    begin_date    TEXT,
    end_date      TEXT,
    wikidata_qid  TEXT
);

CREATE TABLE IF NOT EXISTS track_mb (
    track_spotify_id  TEXT PRIMARY KEY REFERENCES track(spotify_id) ON DELETE CASCADE,
    recording_mbid    TEXT NOT NULL REFERENCES mb_recording(mbid),
    confidence        REAL NOT NULL DEFAULT 0.0
);

CREATE TABLE IF NOT EXISTS artist_mb (
    artist_spotify_id  TEXT PRIMARY KEY REFERENCES artist(spotify_id) ON DELETE CASCADE,
    artist_mbid        TEXT NOT NULL REFERENCES mb_artist(mbid),
    confidence         REAL NOT NULL DEFAULT 0.0,
    resolved_via       TEXT
);

-- ---------------------------------------------------------------- Raw signals (auditable)

-- entity_type: mb_artist | mb_recording | release | spotify_artist
-- source:      musicbrainz | lastfm | discogs | spotify
CREATE TABLE IF NOT EXISTS tag_signal (
    entity_type  TEXT NOT NULL,
    entity_id    TEXT NOT NULL,
    source       TEXT NOT NULL,
    raw_tag      TEXT NOT NULL,
    weight       REAL NOT NULL,
    kind         TEXT,           -- genre | tag | style
    fetched_at   TEXT NOT NULL,
    PRIMARY KEY (entity_type, entity_id, source, raw_tag)
);
CREATE INDEX IF NOT EXISTS idx_tag_signal_entity ON tag_signal(entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_tag_signal_raw ON tag_signal(raw_tag);

-- ---------------------------------------------------------------- Derived knowledge

CREATE TABLE IF NOT EXISTS genre_canonical (
    slug         TEXT PRIMARY KEY,
    label        TEXT NOT NULL,
    parent_slug  TEXT REFERENCES genre_canonical(slug)
);
CREATE INDEX IF NOT EXISTS idx_genre_canonical_parent ON genre_canonical(parent_slug);

-- origin: dict | llm | user
CREATE TABLE IF NOT EXISTS genre_alias (
    raw_tag         TEXT PRIMARY KEY,
    canonical_slug  TEXT REFERENCES genre_canonical(slug),
    origin          TEXT NOT NULL,
    created_at      TEXT
);

CREATE TABLE IF NOT EXISTS track_genre (
    track_spotify_id  TEXT NOT NULL REFERENCES track(spotify_id) ON DELETE CASCADE,
    canonical_slug    TEXT NOT NULL REFERENCES genre_canonical(slug),
    score             REAL NOT NULL,
    derived_at        TEXT,
    PRIMARY KEY (track_spotify_id, canonical_slug)
);
CREATE INDEX IF NOT EXISTS idx_track_genre_slug ON track_genre(canonical_slug);

CREATE TABLE IF NOT EXISTS artist_origin (
    artist_spotify_id  TEXT PRIMARY KEY REFERENCES artist(spotify_id) ON DELETE CASCADE,
    country_code       TEXT,
    country_label      TEXT,
    city               TEXT,
    source             TEXT,      -- mb_begin_area | mb_country | mb_area | wikidata
    confidence         REAL NOT NULL DEFAULT 0.0
);
CREATE INDEX IF NOT EXISTS idx_artist_origin_country ON artist_origin(country_code);

CREATE TABLE IF NOT EXISTS track_era (
    track_spotify_id  TEXT PRIMARY KEY REFERENCES track(spotify_id) ON DELETE CASCADE,
    year              INTEGER,
    decade            INTEGER,
    source            TEXT       -- mb_first_release | spotify_release_date
);
CREATE INDEX IF NOT EXISTS idx_track_era_decade ON track_era(decade);

-- ---------------------------------------------------------------- Operational

CREATE TABLE IF NOT EXISTS api_cache (
    url_hash    TEXT PRIMARY KEY,
    url         TEXT,
    source      TEXT NOT NULL,
    body        TEXT,
    status      INTEGER,
    fetched_at  TEXT NOT NULL,
    expires_at  TEXT
);
CREATE INDEX IF NOT EXISTS idx_api_cache_expires ON api_cache(expires_at);

CREATE TABLE IF NOT EXISTS job_run (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    kind         TEXT NOT NULL,
    playlist_id  TEXT,
    started_at   TEXT NOT NULL,
    finished_at  TEXT,
    stats_json   TEXT
);

CREATE TABLE IF NOT EXISTS created_playlist (
    spotify_id   TEXT PRIMARY KEY,
    name         TEXT,
    recipe_json  TEXT NOT NULL,
    created_at   TEXT NOT NULL
);

-- Always wins over derived data. field: genre | country | year | needs_review
CREATE TABLE IF NOT EXISTS user_override (
    entity_type  TEXT NOT NULL,   -- track | artist
    entity_id    TEXT NOT NULL,
    field        TEXT NOT NULL,
    value        TEXT,
    created_at   TEXT NOT NULL,
    PRIMARY KEY (entity_type, entity_id, field)
);

-- Tracks/artists whose match fell below the confidence threshold. The app is
-- honest about what it does not know instead of guessing.
CREATE TABLE IF NOT EXISTS needs_review (
    entity_type  TEXT NOT NULL,
    entity_id    TEXT NOT NULL,
    reason       TEXT NOT NULL,
    detail       TEXT,
    created_at   TEXT NOT NULL,
    resolved_at  TEXT,
    PRIMARY KEY (entity_type, entity_id, reason)
);
