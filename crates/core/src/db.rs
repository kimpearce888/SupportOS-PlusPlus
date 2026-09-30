//! Database connection + migrations runner.
//!
//! SQLite is bundled (rusqlite's `bundled` feature compiles SQLite from source with FTS5 + WAL).
//! Migrations are forward-only, versioned, and idempotent on re-run.

use std::path::Path;

use rusqlite::Connection;

use crate::error::{Error, Result};

/// Open a SQLite connection at `path` with WAL + FTS5 enabled.
///
/// - Creates the file (and parent dirs) if missing.
/// - Sets `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`.
/// - Sets a `busy_timeout` to avoid "database is locked" under contention.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

/// Run the migrations table bootstrap (migration 000).
///
/// Creates `_migrations` if it does not exist; idempotent.
pub fn ensure_migrations_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS _migrations (
            version    INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
            label      TEXT
        );",
    )?;
    Ok(())
}

/// The latest migration version applied. Returns `0` if the table is empty.
pub fn latest_version(conn: &Connection) -> Result<u32> {
    let v: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM _migrations",
        [],
        |r| r.get(0),
    )?;
    Ok(u32::try_from(v).unwrap_or(0))
}

/// Apply a single migration version, recording the row in `_migrations` in the same transaction.
pub fn apply_migration(conn: &mut Connection, version: u32, label: &str, sql: &str) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(sql).map_err(|e| Error::Migration {
        version,
        message: e.to_string(),
    })?;
    tx.execute(
        "INSERT INTO _migrations (version, label) VALUES (?1, ?2)",
        rusqlite::params![i64::from(version as i32), label],
    )?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn fresh_db() -> Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let conn = open(&f).unwrap();
        ensure_migrations_table(&conn).unwrap();
        conn
    }

    #[test]
    fn opens_with_wal() {
        let conn = fresh_db();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn migrations_table_idempotent() {
        let conn = fresh_db();
        ensure_migrations_table(&conn).unwrap(); // twice
        ensure_migrations_table(&conn).unwrap(); // thrice
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM _migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn apply_migration_records_version() {
        let mut conn = fresh_db();
        apply_migration(
            &mut conn,
            1,
            "test",
            "CREATE TABLE test_one (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        assert_eq!(latest_version(&conn).unwrap(), 1);

        apply_migration(
            &mut conn,
            2,
            "test_two",
            "CREATE TABLE test_two (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        assert_eq!(latest_version(&conn).unwrap(), 2);
    }

    #[test]
    fn migration_failure_is_rolled_back() {
        let mut conn = fresh_db();
        // First migration creates a table; second intentionally fails (syntax error).
        apply_migration(
            &mut conn,
            1,
            "good",
            "CREATE TABLE good (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        let bad = apply_migration(&mut conn, 2, "bad", "THIS IS NOT SQL;");
        assert!(bad.is_err(), "expected migration 2 to fail");
        // Latest version stays at 1, the bad migration is not recorded.
        assert_eq!(latest_version(&conn).unwrap(), 1);
    }
}
