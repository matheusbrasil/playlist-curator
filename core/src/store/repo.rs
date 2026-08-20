//! Typed queries. All SQL in the crate lives here so the shape of the database
//! stays reviewable in one place.

use crate::error::Result;
use crate::model::*;
use crate::store::Store;
use crate::util::now_iso;
use rusqlite::{params, Connection, OptionalExtension};

/// Artist credits for a track, in credit order, on a caller-supplied
/// connection. Free function so callers already holding a connection cannot
/// accidentally request a second one from the pool.
fn track_artists_with(conn: &Connection, track_id: &str) -> Result<Vec<Artist>> {
    let mut stmt = conn.prepare(
        "SELECT a.spotify_id, a.name
         FROM track_artist ta
         JOIN artist a ON a.spotify_id = ta.artist_spotify_id
         WHERE ta.track_spotify_id = ?1
         ORDER BY ta.position",
    )?;
    let rows = stmt.query_map(params![track_id], |r| {
        Ok(Artist {
            spotify_id: r.get(0)?,
            name: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

impl Store {
    // -------------------------------------------------------------- Spotify identity

    /// Upsert a track. `COALESCE` on the incoming value means a later import
    /// that lacks a field (e.g. ISRC absent from one API surface) does not wipe
    /// a value an earlier one supplied.
    pub fn upsert_track(&self, t: &Track) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO track (spotify_id, name, isrc, duration_ms,
                                spotify_album_id, spotify_release_date, is_local)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(spotify_id) DO UPDATE SET
                name                 = excluded.name,
                isrc                 = COALESCE(excluded.isrc, track.isrc),
                duration_ms          = COALESCE(excluded.duration_ms, track.duration_ms),
                spotify_album_id     = COALESCE(excluded.spotify_album_id, track.spotify_album_id),
                spotify_release_date = COALESCE(excluded.spotify_release_date, track.spotify_release_date),
                is_local             = excluded.is_local",
            params![
                t.spotify_id,
                t.name,
                t.isrc,
                t.duration_ms,
                t.spotify_album_id,
                t.spotify_release_date,
                t.is_local as i64,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_artist(&self, a: &Artist) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO artist (spotify_id, name) VALUES (?1, ?2)
             ON CONFLICT(spotify_id) DO UPDATE SET name = excluded.name",
            params![a.spotify_id, a.name],
        )?;
        Ok(())
    }

    pub fn link_track_artist(
        &self,
        track_id: &str,
        artist_id: &str,
        position: i64,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO track_artist (track_spotify_id, artist_spotify_id, position)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(track_spotify_id, artist_spotify_id)
             DO UPDATE SET position = excluded.position",
            params![track_id, artist_id, position],
        )?;
        Ok(())
    }

    pub fn upsert_playlist(&self, p: &Playlist) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO playlist (spotify_id, name, owner, snapshot_id, track_count, synced_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(spotify_id) DO UPDATE SET
                name        = excluded.name,
                owner       = excluded.owner,
                snapshot_id = excluded.snapshot_id,
                track_count = excluded.track_count,
                synced_at   = COALESCE(excluded.synced_at, playlist.synced_at)",
            params![
                p.spotify_id,
                p.name,
                p.owner,
                p.snapshot_id,
                p.track_count,
                p.synced_at
            ],
        )?;
        Ok(())
    }

    pub fn list_playlists(&self) -> Result<Vec<Playlist>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT spotify_id, name, owner, snapshot_id, track_count, synced_at
             FROM playlist ORDER BY name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Playlist {
                spotify_id: r.get(0)?,
                name: r.get(1)?,
                owner: r.get(2)?,
                snapshot_id: r.get(3)?,
                track_count: r.get(4)?,
                synced_at: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_playlist(&self, playlist_id: &str) -> Result<Option<Playlist>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT spotify_id, name, owner, snapshot_id, track_count, synced_at
                 FROM playlist WHERE spotify_id = ?1",
                params![playlist_id],
                |r| {
                    Ok(Playlist {
                        spotify_id: r.get(0)?,
                        name: r.get(1)?,
                        owner: r.get(2)?,
                        snapshot_id: r.get(3)?,
                        track_count: r.get(4)?,
                        synced_at: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    /// Replace the membership of a playlist wholesale. Positions shift whenever
    /// the user reorders upstream, so incremental patching would be wrong.
    pub fn replace_playlist_tracks(
        &self,
        playlist_id: &str,
        entries: &[(String, i64, Option<String>)],
    ) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM playlist_track WHERE playlist_id = ?1",
            params![playlist_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO playlist_track
                   (playlist_id, track_spotify_id, position, added_at)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            for (track_id, position, added_at) in entries {
                stmt.execute(params![playlist_id, track_id, position, added_at])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mark_playlist_synced(&self, playlist_id: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE playlist SET synced_at = ?2 WHERE spotify_id = ?1",
            params![playlist_id, now_iso()],
        )?;
        Ok(())
    }

    pub fn playlist_tracks(&self, playlist_id: &str) -> Result<Vec<PlaylistTrack>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT t.spotify_id, t.name, t.isrc, t.duration_ms, t.spotify_album_id,
                    t.spotify_release_date, t.is_local, pt.position, pt.added_at
             FROM playlist_track pt
             JOIN track t ON t.spotify_id = pt.track_spotify_id
             WHERE pt.playlist_id = ?1
             ORDER BY pt.position",
        )?;
        let rows: Vec<(Track, i64, Option<String>)> = stmt
            .query_map(params![playlist_id], |r| {
                Ok((
                    Track {
                        spotify_id: r.get(0)?,
                        name: r.get(1)?,
                        isrc: r.get(2)?,
                        duration_ms: r.get(3)?,
                        spotify_album_id: r.get(4)?,
                        spotify_release_date: r.get(5)?,
                        is_local: r.get::<_, i64>(6)? != 0,
                    },
                    r.get(7)?,
                    r.get(8)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;

        // Reuse the same connection for the artist lookups. Acquiring a second
        // one from the pool while still holding this one risks deadlocking when
        // the pool is saturated.
        let mut out = Vec::with_capacity(rows.len());
        for (track, position, added_at) in rows {
            let artists = track_artists_with(&conn, &track.spotify_id)?;
            out.push(PlaylistTrack {
                track,
                artists,
                position,
                added_at,
            });
        }
        Ok(out)
    }

    pub fn track_artists(&self, track_id: &str) -> Result<Vec<Artist>> {
        let conn = self.conn()?;
        track_artists_with(&conn, track_id)
    }

    /// Distinct artists appearing in a playlist. Enrichment is per-artist, and
    /// a 500-track playlist typically has ~200 artists — a 2.5x saving on the
    /// slowest (1 req/s) part of the pipeline.
    pub fn playlist_artists(&self, playlist_id: &str) -> Result<Vec<Artist>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT a.spotify_id, a.name
             FROM playlist_track pt
             JOIN track_artist ta ON ta.track_spotify_id = pt.track_spotify_id
             JOIN artist a ON a.spotify_id = ta.artist_spotify_id
             WHERE pt.playlist_id = ?1
             ORDER BY a.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map(params![playlist_id], |r| {
            Ok(Artist {
                spotify_id: r.get(0)?,
                name: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// How many tracks in the playlist carry an ISRC. Phase-2 sanity check: the
    /// ISRC is the deterministic key into MusicBrainz, so a low ratio here
    /// changes which match strategies the pipeline must lean on.
    pub fn isrc_coverage(&self, playlist_id: &str) -> Result<(i64, i64)> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*),
                    COUNT(CASE WHEN t.isrc IS NOT NULL AND t.isrc <> '' THEN 1 END)
             FROM playlist_track pt
             JOIN track t ON t.spotify_id = pt.track_spotify_id
             WHERE pt.playlist_id = ?1",
            params![playlist_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    // -------------------------------------------------------------- MusicBrainz bridge

    pub fn upsert_mb_recording(&self, rec: &MbRecording) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO mb_recording (mbid, title, first_release_date, resolved_via, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(mbid) DO UPDATE SET
                title              = COALESCE(excluded.title, mb_recording.title),
                first_release_date = COALESCE(excluded.first_release_date, mb_recording.first_release_date),
                resolved_via       = excluded.resolved_via,
                confidence         = MAX(excluded.confidence, mb_recording.confidence)",
            params![
                rec.mbid,
                rec.title,
                rec.first_release_date,
                rec.resolved_via,
                rec.confidence
            ],
        )?;
        Ok(())
    }

    pub fn upsert_mb_artist(&self, a: &MbArtist) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO mb_artist (mbid, name, sort_name, type, country, area,
                                    begin_area, begin_date, end_date, wikidata_qid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(mbid) DO UPDATE SET
                name         = COALESCE(excluded.name, mb_artist.name),
                sort_name    = COALESCE(excluded.sort_name, mb_artist.sort_name),
                type         = COALESCE(excluded.type, mb_artist.type),
                country      = COALESCE(excluded.country, mb_artist.country),
                area         = COALESCE(excluded.area, mb_artist.area),
                begin_area   = COALESCE(excluded.begin_area, mb_artist.begin_area),
                begin_date   = COALESCE(excluded.begin_date, mb_artist.begin_date),
                end_date     = COALESCE(excluded.end_date, mb_artist.end_date),
                wikidata_qid = COALESCE(excluded.wikidata_qid, mb_artist.wikidata_qid)",
            params![
                a.mbid,
                a.name,
                a.sort_name,
                a.artist_type,
                a.country,
                a.area,
                a.begin_area,
                a.begin_date,
                a.end_date,
                a.wikidata_qid
            ],
        )?;
        Ok(())
    }

    pub fn get_mb_artist(&self, mbid: &str) -> Result<Option<MbArtist>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT mbid, name, sort_name, type, country, area, begin_area,
                        begin_date, end_date, wikidata_qid
                 FROM mb_artist WHERE mbid = ?1",
                params![mbid],
                |r| {
                    Ok(MbArtist {
                        mbid: r.get(0)?,
                        name: r.get(1)?,
                        sort_name: r.get(2)?,
                        artist_type: r.get(3)?,
                        country: r.get(4)?,
                        area: r.get(5)?,
                        begin_area: r.get(6)?,
                        begin_date: r.get(7)?,
                        end_date: r.get(8)?,
                        wikidata_qid: r.get(9)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn get_mb_recording(&self, mbid: &str) -> Result<Option<MbRecording>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT mbid, title, first_release_date, resolved_via, confidence
                 FROM mb_recording WHERE mbid = ?1",
                params![mbid],
                |r| {
                    Ok(MbRecording {
                        mbid: r.get(0)?,
                        title: r.get(1)?,
                        first_release_date: r.get(2)?,
                        resolved_via: r.get(3)?,
                        confidence: r.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn link_track_mb(&self, track_id: &str, recording_mbid: &str, confidence: f64) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO track_mb (track_spotify_id, recording_mbid, confidence)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(track_spotify_id) DO UPDATE SET
                recording_mbid = excluded.recording_mbid,
                confidence     = excluded.confidence
             WHERE excluded.confidence >= track_mb.confidence",
            params![track_id, recording_mbid, confidence],
        )?;
        Ok(())
    }

    pub fn link_artist_mb(
        &self,
        artist_id: &str,
        artist_mbid: &str,
        confidence: f64,
        resolved_via: &str,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO artist_mb (artist_spotify_id, artist_mbid, confidence, resolved_via)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(artist_spotify_id) DO UPDATE SET
                artist_mbid  = excluded.artist_mbid,
                confidence   = excluded.confidence,
                resolved_via = excluded.resolved_via
             WHERE excluded.confidence >= artist_mb.confidence",
            params![artist_id, artist_mbid, confidence, resolved_via],
        )?;
        Ok(())
    }

    pub fn get_track_mbid(&self, track_id: &str) -> Result<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT recording_mbid FROM track_mb WHERE track_spotify_id = ?1",
                params![track_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn get_artist_mbid(&self, artist_id: &str) -> Result<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT artist_mbid FROM artist_mb WHERE artist_spotify_id = ?1",
                params![artist_id],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Fraction of a playlist's tracks resolved to either a recording MBID or
    /// (via any of their artists) an artist MBID. This is the phase-3 acceptance
    /// metric.
    pub fn mb_resolution_coverage(&self, playlist_id: &str) -> Result<(i64, i64)> {
        let conn = self.conn()?;
        Ok(conn.query_row(
            "SELECT COUNT(*),
                    COUNT(CASE WHEN tm.recording_mbid IS NOT NULL
                                 OR EXISTS (
                                     SELECT 1 FROM track_artist ta
                                     JOIN artist_mb am ON am.artist_spotify_id = ta.artist_spotify_id
                                     WHERE ta.track_spotify_id = pt.track_spotify_id
                                 )
                          THEN 1 END)
             FROM playlist_track pt
             LEFT JOIN track_mb tm ON tm.track_spotify_id = pt.track_spotify_id
             WHERE pt.playlist_id = ?1",
            params![playlist_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    // -------------------------------------------------------------- Raw signals

    /// Record a raw tag observation. Keeps the highest weight seen for a given
    /// (entity, source, tag) rather than the most recent, so re-running with a
    /// partial response cannot weaken existing evidence.
    pub fn insert_tag_signal(&self, sig: &TagSignal) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO tag_signal (entity_type, entity_id, source, raw_tag, weight, kind, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(entity_type, entity_id, source, raw_tag) DO UPDATE SET
                weight     = MAX(excluded.weight, tag_signal.weight),
                kind       = COALESCE(excluded.kind, tag_signal.kind),
                fetched_at = excluded.fetched_at",
            params![
                sig.entity_type.as_str(),
                sig.entity_id,
                sig.source.as_str(),
                sig.raw_tag,
                sig.weight,
                sig.kind.map(|k| k.as_str()),
                now_iso(),
            ],
        )?;
        Ok(())
    }

    pub fn insert_tag_signals(&self, sigs: &[TagSignal]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO tag_signal (entity_type, entity_id, source, raw_tag, weight, kind, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(entity_type, entity_id, source, raw_tag) DO UPDATE SET
                    weight     = MAX(excluded.weight, tag_signal.weight),
                    kind       = COALESCE(excluded.kind, tag_signal.kind),
                    fetched_at = excluded.fetched_at",
            )?;
            let ts = now_iso();
            for sig in sigs {
                stmt.execute(params![
                    sig.entity_type.as_str(),
                    sig.entity_id,
                    sig.source.as_str(),
                    sig.raw_tag,
                    sig.weight,
                    sig.kind.map(|k| k.as_str()),
                    ts,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn tag_signals_for(
        &self,
        entity_type: EntityType,
        entity_id: &str,
    ) -> Result<Vec<TagSignal>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT entity_type, entity_id, source, raw_tag, weight, kind, fetched_at
             FROM tag_signal WHERE entity_type = ?1 AND entity_id = ?2",
        )?;
        let rows = stmt.query_map(params![entity_type.as_str(), entity_id], |r| {
            let et: String = r.get(0)?;
            let src: String = r.get(2)?;
            let kind: Option<String> = r.get(5)?;
            Ok(TagSignal {
                entity_type: EntityType::parse(&et).unwrap_or(EntityType::MbArtist),
                entity_id: r.get(1)?,
                source: Source::parse(&src).unwrap_or(Source::MusicBrainz),
                raw_tag: r.get(3)?,
                weight: r.get(4)?,
                kind: kind.as_deref().and_then(TagKind::parse),
                fetched_at: r.get(6).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every distinct raw tag with no entry in `genre_alias` — the queue that
    /// the LLM (or the user) resolves once, permanently.
    pub fn unresolved_raw_tags(&self) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT DISTINCT ts.raw_tag
             FROM tag_signal ts
             LEFT JOIN genre_alias ga ON ga.raw_tag = ts.raw_tag
             WHERE ga.raw_tag IS NULL
             ORDER BY ts.raw_tag",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -------------------------------------------------------------- Taxonomy

    pub fn upsert_canonical_genres(&self, genres: &[CanonicalGenre]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        {
            // Parents may appear after children in the input, so the
            // self-referencing FK is deferred for the batch.
            tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            let mut stmt = tx.prepare(
                "INSERT INTO genre_canonical (slug, label, parent_slug)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(slug) DO UPDATE SET
                    label       = excluded.label,
                    parent_slug = excluded.parent_slug",
            )?;
            for g in genres {
                stmt.execute(params![g.slug, g.label, g.parent_slug])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn canonical_genre(&self, slug: &str) -> Result<Option<CanonicalGenre>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT slug, label, parent_slug FROM genre_canonical WHERE slug = ?1",
                params![slug],
                |r| {
                    Ok(CanonicalGenre {
                        slug: r.get(0)?,
                        label: r.get(1)?,
                        parent_slug: r.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn all_canonical_genres(&self) -> Result<Vec<CanonicalGenre>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT slug, label, parent_slug FROM genre_canonical ORDER BY slug")?;
        let rows = stmt.query_map([], |r| {
            Ok(CanonicalGenre {
                slug: r.get(0)?,
                label: r.get(1)?,
                parent_slug: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record a raw-tag -> canonical mapping. A `user` origin outranks `llm`,
    /// which outranks `dict`; a lower-precedence origin never overwrites a
    /// higher one.
    pub fn upsert_genre_alias(
        &self,
        raw_tag: &str,
        canonical_slug: Option<&str>,
        origin: &str,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO genre_alias (raw_tag, canonical_slug, origin, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(raw_tag) DO UPDATE SET
                canonical_slug = excluded.canonical_slug,
                origin         = excluded.origin,
                created_at     = excluded.created_at
             WHERE (CASE excluded.origin WHEN 'user' THEN 3 WHEN 'llm' THEN 2 ELSE 1 END)
                >= (CASE genre_alias.origin WHEN 'user' THEN 3 WHEN 'llm' THEN 2 ELSE 1 END)",
            params![raw_tag, canonical_slug, origin, now_iso()],
        )?;
        Ok(())
    }

    pub fn genre_alias(&self, raw_tag: &str) -> Result<Option<Option<String>>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT canonical_slug FROM genre_alias WHERE raw_tag = ?1",
                params![raw_tag],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?)
    }

    pub fn replace_track_genres(&self, track_id: &str, genres: &[TrackGenre]) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM track_genre WHERE track_spotify_id = ?1",
            params![track_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO track_genre (track_spotify_id, canonical_slug, score, derived_at)
                 VALUES (?1, ?2, ?3, ?4)",
            )?;
            let ts = now_iso();
            for g in genres {
                stmt.execute(params![track_id, g.canonical_slug, g.score, ts])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn track_genres(&self, track_id: &str) -> Result<Vec<TrackGenre>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT canonical_slug, score FROM track_genre
             WHERE track_spotify_id = ?1 ORDER BY score DESC",
        )?;
        let rows = stmt.query_map(params![track_id], |r| {
            Ok(TrackGenre {
                canonical_slug: r.get(0)?,
                score: r.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -------------------------------------------------------------- Origin & era

    pub fn upsert_artist_origin(&self, o: &ArtistOrigin) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO artist_origin (artist_spotify_id, country_code, country_label,
                                        city, source, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(artist_spotify_id) DO UPDATE SET
                country_code  = excluded.country_code,
                country_label = excluded.country_label,
                city          = excluded.city,
                source        = excluded.source,
                confidence    = excluded.confidence
             WHERE excluded.confidence >= artist_origin.confidence",
            params![
                o.artist_spotify_id,
                o.country_code,
                o.country_label,
                o.city,
                o.source.as_str(),
                o.confidence
            ],
        )?;
        Ok(())
    }

    pub fn artist_origin(&self, artist_id: &str) -> Result<Option<ArtistOrigin>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT artist_spotify_id, country_code, country_label, city, source, confidence
                 FROM artist_origin WHERE artist_spotify_id = ?1",
                params![artist_id],
                |r| {
                    let src: String = r.get(4)?;
                    Ok(ArtistOrigin {
                        artist_spotify_id: r.get(0)?,
                        country_code: r.get(1)?,
                        country_label: r.get(2)?,
                        city: r.get(3)?,
                        source: OriginSource::parse(&src).unwrap_or(OriginSource::MbCountry),
                        confidence: r.get(5)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn upsert_track_era(&self, e: &TrackEra) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO track_era (track_spotify_id, year, decade, source)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(track_spotify_id) DO UPDATE SET
                year   = excluded.year,
                decade = excluded.decade,
                source = excluded.source",
            params![e.track_spotify_id, e.year, e.decade, e.source.as_str()],
        )?;
        Ok(())
    }

    pub fn track_era(&self, track_id: &str) -> Result<Option<TrackEra>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT track_spotify_id, year, decade, source
                 FROM track_era WHERE track_spotify_id = ?1",
                params![track_id],
                |r| {
                    let src: String = r.get(3)?;
                    Ok(TrackEra {
                        track_spotify_id: r.get(0)?,
                        year: r.get(1)?,
                        decade: r.get(2)?,
                        source: EraSource::parse(&src).unwrap_or(EraSource::SpotifyReleaseDate),
                    })
                },
            )
            .optional()?)
    }

    // -------------------------------------------------------------- Overrides & review

    pub fn set_override(
        &self,
        entity_type: &str,
        entity_id: &str,
        field: &str,
        value: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO user_override (entity_type, entity_id, field, value, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(entity_type, entity_id, field) DO UPDATE SET
                value      = excluded.value,
                created_at = excluded.created_at",
            params![entity_type, entity_id, field, value, now_iso()],
        )?;
        Ok(())
    }

    pub fn get_override(
        &self,
        entity_type: &str,
        entity_id: &str,
        field: &str,
    ) -> Result<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT value FROM user_override
                 WHERE entity_type = ?1 AND entity_id = ?2 AND field = ?3",
                params![entity_type, entity_id, field],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn clear_override(&self, entity_type: &str, entity_id: &str, field: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "DELETE FROM user_override
             WHERE entity_type = ?1 AND entity_id = ?2 AND field = ?3",
            params![entity_type, entity_id, field],
        )?;
        Ok(())
    }

    pub fn flag_needs_review(
        &self,
        entity_type: &str,
        entity_id: &str,
        reason: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO needs_review (entity_type, entity_id, reason, detail, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(entity_type, entity_id, reason) DO UPDATE SET
                detail = excluded.detail",
            params![entity_type, entity_id, reason, detail, now_iso()],
        )?;
        Ok(())
    }

    pub fn resolve_review(&self, entity_type: &str, entity_id: &str, reason: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE needs_review SET resolved_at = ?4
             WHERE entity_type = ?1 AND entity_id = ?2 AND reason = ?3",
            params![entity_type, entity_id, reason, now_iso()],
        )?;
        Ok(())
    }

    pub fn open_reviews(&self) -> Result<Vec<ReviewItem>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT entity_type, entity_id, reason, detail, created_at
             FROM needs_review WHERE resolved_at IS NULL ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ReviewItem {
                entity_type: r.get(0)?,
                entity_id: r.get(1)?,
                reason: r.get(2)?,
                detail: r.get(3)?,
                created_at: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // -------------------------------------------------------------- API cache

    /// Returns a cached body if present and unexpired. Expiry is compared as a
    /// string, which is valid because all timestamps are RFC3339 UTC.
    pub fn cache_get(&self, url: &str) -> Result<Option<String>> {
        let conn = self.conn()?;
        Ok(conn
            .query_row(
                "SELECT body FROM api_cache
                 WHERE url_hash = ?1 AND (expires_at IS NULL OR expires_at > ?2)",
                params![crate::util::sha256_hex(url), now_iso()],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    pub fn cache_put(
        &self,
        url: &str,
        source: &str,
        body: &str,
        status: u16,
        ttl_secs: i64,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO api_cache (url_hash, url, source, body, status, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(url_hash) DO UPDATE SET
                body       = excluded.body,
                status     = excluded.status,
                fetched_at = excluded.fetched_at,
                expires_at = excluded.expires_at",
            params![
                crate::util::sha256_hex(url),
                url,
                source,
                body,
                status,
                now_iso(),
                crate::util::iso_in(ttl_secs),
            ],
        )?;
        Ok(())
    }

    pub fn cache_purge_expired(&self) -> Result<usize> {
        let conn = self.conn()?;
        Ok(conn.execute(
            "DELETE FROM api_cache WHERE expires_at IS NOT NULL AND expires_at <= ?1",
            params![now_iso()],
        )?)
    }

    pub fn cache_clear(&self) -> Result<usize> {
        let conn = self.conn()?;
        Ok(conn.execute("DELETE FROM api_cache", [])?)
    }

    // -------------------------------------------------------------- Jobs

    pub fn job_start(&self, kind: &str, playlist_id: Option<&str>) -> Result<i64> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO job_run (kind, playlist_id, started_at) VALUES (?1, ?2, ?3)",
            params![kind, playlist_id, now_iso()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn job_finish(&self, id: i64, stats_json: &str) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "UPDATE job_run SET finished_at = ?2, stats_json = ?3 WHERE id = ?1",
            params![id, now_iso(), stats_json],
        )?;
        Ok(())
    }

    pub fn record_created_playlist(
        &self,
        spotify_id: &str,
        name: &str,
        recipe_json: &str,
    ) -> Result<()> {
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO created_playlist (spotify_id, name, recipe_json, created_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(spotify_id) DO UPDATE SET
                name        = excluded.name,
                recipe_json = excluded.recipe_json",
            params![spotify_id, name, recipe_json, now_iso()],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn track(id: &str, isrc: Option<&str>) -> Track {
        Track {
            spotify_id: id.into(),
            name: format!("Track {id}"),
            isrc: isrc.map(String::from),
            duration_ms: Some(180_000),
            spotify_album_id: Some("alb1".into()),
            spotify_release_date: Some("2015-01-01".into()),
            is_local: false,
        }
    }

    #[test]
    fn upsert_track_preserves_isrc_when_reimported_without_one() {
        let s = store();
        s.upsert_track(&track("t1", Some("BRXXX1234567"))).unwrap();
        // A later import from an API surface that omits ISRC must not erase it.
        s.upsert_track(&track("t1", None)).unwrap();

        let conn = s.conn().unwrap();
        let isrc: Option<String> = conn
            .query_row("SELECT isrc FROM track WHERE spotify_id='t1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(isrc.as_deref(), Some("BRXXX1234567"));
    }

    #[test]
    fn playlist_roundtrip_with_positions_and_artists() {
        let s = store();
        s.upsert_playlist(&Playlist {
            spotify_id: "p1".into(),
            name: "Test".into(),
            owner: Some("me".into()),
            snapshot_id: Some("snap".into()),
            track_count: Some(2),
            synced_at: None,
        })
        .unwrap();
        s.upsert_artist(&Artist { spotify_id: "a1".into(), name: "Tim Maia".into() }).unwrap();
        s.upsert_track(&track("t1", Some("BR1"))).unwrap();
        s.upsert_track(&track("t2", None)).unwrap();
        s.link_track_artist("t1", "a1", 0).unwrap();

        s.replace_playlist_tracks(
            "p1",
            &[
                ("t2".into(), 0, Some("2020-01-01T00:00:00Z".into())),
                ("t1".into(), 1, None),
            ],
        )
        .unwrap();

        let tracks = s.playlist_tracks("p1").unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].track.spotify_id, "t2");
        assert_eq!(tracks[1].track.spotify_id, "t1");
        assert_eq!(tracks[1].artists[0].name, "Tim Maia");

        let (total, with_isrc) = s.isrc_coverage("p1").unwrap();
        assert_eq!((total, with_isrc), (2, 1));
    }

    #[test]
    fn replacing_playlist_tracks_drops_removed_entries() {
        let s = store();
        s.upsert_playlist(&Playlist {
            spotify_id: "p1".into(), name: "T".into(), owner: None,
            snapshot_id: None, track_count: None, synced_at: None,
        }).unwrap();
        s.upsert_track(&track("t1", None)).unwrap();
        s.upsert_track(&track("t2", None)).unwrap();

        s.replace_playlist_tracks("p1", &[("t1".into(), 0, None), ("t2".into(), 1, None)]).unwrap();
        s.replace_playlist_tracks("p1", &[("t2".into(), 0, None)]).unwrap();

        let tracks = s.playlist_tracks("p1").unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].track.spotify_id, "t2");
        assert_eq!(tracks[0].position, 0);
    }

    #[test]
    fn tag_signal_keeps_strongest_weight() {
        let s = store();
        s.upsert_mb_artist(&MbArtist { mbid: "mb1".into(), ..Default::default() }).unwrap();
        let mut sig = TagSignal {
            entity_type: EntityType::MbArtist,
            entity_id: "mb1".into(),
            source: Source::Lastfm,
            raw_tag: "soul".into(),
            weight: 0.9,
            kind: Some(TagKind::Tag),
            fetched_at: String::new(),
        };
        s.insert_tag_signal(&sig).unwrap();
        sig.weight = 0.3;
        s.insert_tag_signal(&sig).unwrap();

        let got = s.tag_signals_for(EntityType::MbArtist, "mb1").unwrap();
        assert_eq!(got.len(), 1);
        assert!((got[0].weight - 0.9).abs() < 1e-9);
    }

    #[test]
    fn user_alias_outranks_dict_but_not_the_reverse() {
        let s = store();
        s.upsert_canonical_genres(&[
            CanonicalGenre { slug: "soul".into(), label: "Soul".into(), parent_slug: None },
            CanonicalGenre { slug: "funk".into(), label: "Funk".into(), parent_slug: None },
        ]).unwrap();

        s.upsert_genre_alias("souI", Some("soul"), "dict").unwrap();
        s.upsert_genre_alias("souI", Some("funk"), "user").unwrap();
        assert_eq!(s.genre_alias("souI").unwrap(), Some(Some("funk".into())));

        // A dict pass re-running later must not clobber the user's decision.
        s.upsert_genre_alias("souI", Some("soul"), "dict").unwrap();
        assert_eq!(s.genre_alias("souI").unwrap(), Some(Some("funk".into())));
    }

    #[test]
    fn alias_can_record_a_deliberate_non_genre() {
        let s = store();
        // "seen live" is not a genre; recording NULL stops it being re-queued.
        s.upsert_genre_alias("seen live", None, "dict").unwrap();
        assert_eq!(s.genre_alias("seen live").unwrap(), Some(None));
        // Distinguishable from "never seen this tag".
        assert_eq!(s.genre_alias("never-seen").unwrap(), None);
    }

    #[test]
    fn unresolved_tags_exclude_already_aliased_ones() {
        let s = store();
        s.upsert_mb_artist(&MbArtist { mbid: "mb1".into(), ..Default::default() }).unwrap();
        for tag in ["soul", "seen live", "mpb"] {
            s.insert_tag_signal(&TagSignal {
                entity_type: EntityType::MbArtist,
                entity_id: "mb1".into(),
                source: Source::Lastfm,
                raw_tag: tag.into(),
                weight: 0.5,
                kind: Some(TagKind::Tag),
                fetched_at: String::new(),
            }).unwrap();
        }
        s.upsert_canonical_genres(&[CanonicalGenre {
            slug: "soul".into(), label: "Soul".into(), parent_slug: None,
        }]).unwrap();
        s.upsert_genre_alias("soul", Some("soul"), "dict").unwrap();
        s.upsert_genre_alias("seen live", None, "dict").unwrap();

        assert_eq!(s.unresolved_raw_tags().unwrap(), vec!["mpb".to_string()]);
    }

    #[test]
    fn cache_respects_expiry() {
        let s = store();
        s.cache_put("https://x/1", "musicbrainz", "{\"a\":1}", 200, 3600).unwrap();
        assert_eq!(s.cache_get("https://x/1").unwrap().as_deref(), Some("{\"a\":1}"));

        // Already expired.
        s.cache_put("https://x/2", "musicbrainz", "{}", 200, -10).unwrap();
        assert_eq!(s.cache_get("https://x/2").unwrap(), None);

        assert_eq!(s.cache_purge_expired().unwrap(), 1);
        assert_eq!(s.cache_get("https://x/1").unwrap().as_deref(), Some("{\"a\":1}"));
    }

    #[test]
    fn higher_confidence_match_wins_and_lower_is_ignored() {
        let s = store();
        s.upsert_artist(&Artist { spotify_id: "a1".into(), name: "X".into() }).unwrap();
        s.upsert_mb_artist(&MbArtist { mbid: "good".into(), ..Default::default() }).unwrap();
        s.upsert_mb_artist(&MbArtist { mbid: "weak".into(), ..Default::default() }).unwrap();

        s.link_artist_mb("a1", "good", 1.0, "url-rel").unwrap();
        s.link_artist_mb("a1", "weak", 0.4, "search").unwrap();
        assert_eq!(s.get_artist_mbid("a1").unwrap().as_deref(), Some("good"));
    }

    #[test]
    fn override_roundtrip() {
        let s = store();
        s.upsert_track(&track("t1", None)).unwrap();
        assert_eq!(s.get_override("track", "t1", "genre").unwrap(), None);
        s.set_override("track", "t1", "genre", Some("samba")).unwrap();
        assert_eq!(s.get_override("track", "t1", "genre").unwrap().as_deref(), Some("samba"));
        s.set_override("track", "t1", "genre", Some("samba-rock")).unwrap();
        assert_eq!(s.get_override("track", "t1", "genre").unwrap().as_deref(), Some("samba-rock"));
        s.clear_override("track", "t1", "genre").unwrap();
        assert_eq!(s.get_override("track", "t1", "genre").unwrap(), None);
    }

    #[test]
    fn review_queue_tracks_open_items_only() {
        let s = store();
        s.upsert_track(&track("t1", None)).unwrap();
        s.flag_needs_review("track", "t1", "low_confidence_match", Some("score 0.31")).unwrap();
        assert_eq!(s.open_reviews().unwrap().len(), 1);
        s.resolve_review("track", "t1", "low_confidence_match").unwrap();
        assert_eq!(s.open_reviews().unwrap().len(), 0);
    }
}
