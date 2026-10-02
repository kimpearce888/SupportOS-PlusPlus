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

/// Open a connection and run all pending application migrations.
///
/// Convenience wrapper for the common boot path: open + ensure_migrations_table +
/// migrations::run_all. Idempotent — safe to call on every boot.
pub fn open_with_migrations(path: &Path) -> Result<Connection> {
    let mut conn = open(path)?;
    ensure_migrations_table(&conn)?;
    crate::migrations::run_all(&mut conn)?;
    // Record the reference-equivalent migration set (the port implements
    // reference migrations 001..016 via its boot-time batches; the record
    // makes `migrations_applied` and the .sosync schema guard compare like
    // with like). Idempotent.
    let _ = crate::encrypted_sync::ensure_schema_migrations_record(&conn);
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

    #[test]
    fn open_with_migrations_creates_application_settings_and_secrets() {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let conn = open_with_migrations(&f).unwrap();
        // application_settings table exists.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM application_settings", [], |r| {
                r.get(0)
            })
            .unwrap();
        // secrets table exists.
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM secrets", [], |r| r.get(0))
            .unwrap();
        // app_state single-row bootstrap exists with first_run_done = 0.
        let first_run: i64 = conn
            .query_row(
                "SELECT first_run_done FROM app_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(first_run, 0);
        // Latest version equals the highest version in MIGRATIONS.
        assert_eq!(
            latest_version(&conn).unwrap(),
            crate::migrations::latest_version()
        );
    }

    #[test]
    fn open_with_migrations_is_idempotent_on_reopen() {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        // First open applies all migrations.
        let conn1 = open_with_migrations(&f).unwrap();
        let v1 = latest_version(&conn1).unwrap();
        drop(conn1);
        // Second open is a no-op for migrations.
        let conn2 = open_with_migrations(&f).unwrap();
        let v2 = latest_version(&conn2).unwrap();
        assert_eq!(v1, v2);
    }
}
