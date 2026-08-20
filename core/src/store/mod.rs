//! SQLite storage: connection pool, WAL, versioned migrations.
//!
//! The database is private to Rust. Nothing here is exposed to the frontend as
//! SQL — the UI only ever sees typed values returned from commands.

pub mod repo;

use crate::error::Result;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use std::path::Path;

pub type Pool = r2d2::Pool<SqliteConnectionManager>;
pub type PooledConn = r2d2::PooledConnection<SqliteConnectionManager>;

/// Migrations are embedded at compile time and applied in order. `user_version`
/// records how many have run, so reopening an existing database is a no-op.
const MIGRATIONS: &[(&str, &str)] = &[(
    "001_initial",
    include_str!("migrations/001_initial.sql"),
)];

#[derive(Clone)]
pub struct Store {
    pool: Pool,
}

impl Store {
    /// Open (creating if needed) the database at `path` and run migrations.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let manager = SqliteConnectionManager::file(path).with_init(|c| {
            configure_connection(c)?;
            Ok(())
        });
        Self::from_manager(manager)
    }

    /// In-memory store for tests. Uses a shared cache so every pooled
    /// connection sees the same database.
    pub fn open_in_memory() -> Result<Self> {
        // `max_size(1)` keeps all access on one connection, which is what makes
        // an in-memory DB coherent across the pool. It doubles as a deadlock
        // canary: any repo method that acquires a second connection while
        // holding one will hang here instead of failing rarely in production.
        let manager = SqliteConnectionManager::memory().with_init(|c| {
            configure_connection(c)?;
            Ok(())
        });
        let pool = r2d2::Pool::builder().max_size(1).build(manager)?;
        let store = Store { pool };
        store.migrate()?;
        Ok(store)
    }

    fn from_manager(manager: SqliteConnectionManager) -> Result<Self> {
        let pool = r2d2::Pool::builder().max_size(8).build(manager)?;
        let store = Store { pool };
        store.migrate()?;
        Ok(store)
    }

    pub fn conn(&self) -> Result<PooledConn> {
        Ok(self.pool.get()?)
    }

    /// Apply any migrations whose index is beyond the recorded `user_version`.
    fn migrate(&self) -> Result<()> {
        let mut conn = self.conn()?;
        let current: i64 =
            conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let current = current as usize;

        if current > MIGRATIONS.len() {
            return Err(crate::error::CoreError::Config(format!(
                "database schema version {current} is newer than this build knows ({})",
                MIGRATIONS.len()
            )));
        }

        for (idx, (name, sql)) in MIGRATIONS.iter().enumerate().skip(current) {
            tracing::info!(migration = name, "applying migration");
            let tx = conn.transaction()?;
            tx.execute_batch(sql)?;
            // PRAGMA does not accept bound parameters.
            tx.pragma_update(None, "user_version", (idx + 1) as i64)?;
            tx.commit()?;
        }
        Ok(())
    }
}

/// Pragmas applied to every connection in the pool.
fn configure_connection(c: &Connection) -> rusqlite::Result<()> {
    // WAL survives crashes better and lets the enrichment writer run while the
    // UI reads. `busy_timeout` avoids spurious SQLITE_BUSY under that overlap.
    c.pragma_update(None, "journal_mode", "WAL")?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    c.pragma_update(None, "foreign_keys", true)?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_create_expected_tables() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.conn().unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap();
        let tables: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();

        for expected in [
            "track", "artist", "track_artist", "playlist", "playlist_track",
            "mb_recording", "mb_artist", "track_mb", "artist_mb", "tag_signal",
            "genre_canonical", "genre_alias", "track_genre", "artist_origin",
            "track_era", "api_cache", "job_run", "created_playlist",
            "user_override", "needs_review",
        ] {
            assert!(
                tables.iter().any(|t| t == expected),
                "missing table {expected}; got {tables:?}"
            );
        }
    }

    #[test]
    fn migrate_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("pc-test-{}", std::process::id()));
        let db = dir.join("test.db");
        let _ = std::fs::remove_file(&db);

        let s1 = Store::open(&db).unwrap();
        let v1: i64 = s1.conn().unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        drop(s1);

        // Reopening must not re-run migrations or fail on existing objects.
        let s2 = Store::open(&db).unwrap();
        let v2: i64 = s2.conn().unwrap().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v1, v2);
        assert_eq!(v1 as usize, MIGRATIONS.len());

        drop(s2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
