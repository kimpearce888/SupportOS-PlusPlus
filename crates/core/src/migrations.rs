//! Application migrations — forward-only, versioned, applied in order.
//!
//! Per spec A12: "Database changes only through forward migrations; keep
//! configuration and constants in one place."
//!
//! The migrations themselves are static SQL strings stored in this file as
//! the single source of truth for the schema (D-015). Each migration is
//! wrapped in a transaction by [`crate::db::apply_migration`].
//!
//! Adding a new migration:
//!   1. Add a new entry to `MIGRATIONS` with the next version number.
//!   2. The migration SQL must be idempotent-safe within a transaction
//!      (prefer `CREATE TABLE IF NOT EXISTS`, `CREATE INDEX IF NOT EXISTS`).
//!   3. Test by running `migrations::tests::apply_in_order_on_fresh_db`.

/// One forward-only migration.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// Monotonic version. Versions must be contiguous starting at 1.
    pub version: u32,
    /// Human-readable label (shown in `_migrations` table and logs).
    pub label: &'static str,
    /// The SQL to apply, wrapped in a transaction by [`crate::db::apply_migration`].
    pub sql: &'static str,
}

/// All application migrations in version order.
///
/// Order matters: each migration is applied exactly once, in order, on every
/// database that hasn't seen it yet. Never reorder, never delete, never edit
/// a shipped migration — add a new one at the end with the next version.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        label: "initial_schema",
        sql: r#"
            -- Application settings: typed key/value store, NOT for secrets.
            CREATE TABLE IF NOT EXISTS application_settings (
                key    TEXT PRIMARY KEY,
                value  TEXT NOT NULL
            );

            -- Encrypted secrets: BLOB value is the ciphertext. The plaintext
            -- never enters this table; encryption happens in the secrets
            -- module before any INSERT. Reads must always go through the
            -- redaction helpers in crate::settings.
            CREATE TABLE IF NOT EXISTS secrets (
                key    TEXT PRIMARY KEY,
                value  BLOB NOT NULL
            );

            -- Single-row bootstrap marker. The UI uses this to detect first-run.
            -- Stored as a single INTEGER (0/1) so it is cheap to read at boot.
            CREATE TABLE IF NOT EXISTS app_state (
                id              INTEGER PRIMARY KEY CHECK (id = 1),
                first_run_done  INTEGER NOT NULL DEFAULT 0,
                schema_version  INTEGER NOT NULL DEFAULT 1
            );
            INSERT OR IGNORE INTO app_state (id, first_run_done, schema_version)
            VALUES (1, 0, 1);
        "#,
    },
    Migration {
        version: 2,
        label: "sync_tables",
        sql: r#"
            -- Sync cursors: one row per resource type.
            CREATE TABLE IF NOT EXISTS sync_cursors (
                resource       TEXT PRIMARY KEY,
                last_page       INTEGER NOT NULL DEFAULT 0,
                last_seen_at    TEXT,
                cursor_token    TEXT
            );

            -- Sync checkpoints: every successful page fetch is recorded.
            CREATE TABLE IF NOT EXISTS sync_checkpoints (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                resource        TEXT NOT NULL,
                page            INTEGER NOT NULL,
                cursor_token    TEXT,
                recorded_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );
            CREATE INDEX IF NOT EXISTS idx_sync_checkpoints_resource
                ON sync_checkpoints (resource, page);

            -- Sync runs: one row per sync attempt.
            CREATE TABLE IF NOT EXISTS sync_runs (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                started_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                completed_at    TEXT,
                status          TEXT NOT NULL DEFAULT 'running',
                error           TEXT,
                resources_synced INTEGER NOT NULL DEFAULT 0
            );

            -- OAuth tokens: Help Scout access + refresh tokens.
            CREATE TABLE IF NOT EXISTS oauth_tokens (
                id              INTEGER PRIMARY KEY CHECK (id = 1),
                access_token    TEXT NOT NULL,
                refresh_token   TEXT,
                expires_at      TEXT,
                token_type      TEXT NOT NULL DEFAULT 'bearer',
                scope           TEXT,
                obtained_at     TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );

            -- Conversations: the main ticket table.
            CREATE TABLE IF NOT EXISTS conversations (
                id              INTEGER PRIMARY KEY,
                remote_id       INTEGER NOT NULL UNIQUE,
                number          INTEGER NOT NULL,
                subject         TEXT,
                preview         TEXT,
                status          TEXT NOT NULL DEFAULT 'active',
                mailbox_id      INTEGER NOT NULL,
                assignee_id     INTEGER,
                customer_id     INTEGER NOT NULL,
                priority        TEXT,
                created_at      TEXT,
                updated_at      TEXT,
                closed_at       TEXT,
                local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );
            CREATE INDEX IF NOT EXISTS idx_conversations_mailbox
                ON conversations (mailbox_id, status);
            CREATE INDEX IF NOT EXISTS idx_conversations_assignee
                ON conversations (assignee_id, status);
            CREATE INDEX IF NOT EXISTS idx_conversations_customer
                ON conversations (customer_id);

            -- Customers: mirrors Help Scout's customer object.
            CREATE TABLE IF NOT EXISTS customers (
                id              INTEGER PRIMARY KEY,
                remote_id       INTEGER NOT NULL UNIQUE,
                first_name      TEXT,
                last_name       TEXT,
                email           TEXT,
                organization    TEXT,
                job_title       TEXT,
                phone           TEXT,
                created_at      TEXT,
                updated_at      TEXT,
                local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );
            CREATE INDEX IF NOT EXISTS idx_customers_email
                ON customers (email);

            -- Mailboxes: mirrors Help Scout's mailbox object.
            CREATE TABLE IF NOT EXISTS mailboxes (
                id              INTEGER PRIMARY KEY,
                remote_id       INTEGER NOT NULL UNIQUE,
                name            TEXT NOT NULL,
                slug            TEXT,
                email           TEXT,
                created_at      TEXT,
                updated_at      TEXT,
                local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );

            -- Users: mirrors Help Scout's user object (agents + system users).
            CREATE TABLE IF NOT EXISTS users (
                id              INTEGER PRIMARY KEY,
                remote_id       INTEGER NOT NULL UNIQUE,
                first_name      TEXT,
                last_name       TEXT,
                email           TEXT,
                role            TEXT,
                user_type       TEXT NOT NULL DEFAULT 'user',
                timezone        TEXT,
                photo_url       TEXT,
                initials        TEXT,
                mention         TEXT,
                job_title       TEXT,
                phone           TEXT,
                created_at      TEXT,
                updated_at      TEXT,
                local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );

            -- Teams: mirrors Help Scout's team object.
            CREATE TABLE IF NOT EXISTS teams (
                id              INTEGER PRIMARY KEY,
                remote_id       INTEGER NOT NULL UNIQUE,
                name            TEXT NOT NULL,
                local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );

            -- Tags: mirrors Help Scout's tag object.
            CREATE TABLE IF NOT EXISTS tags (
                id              INTEGER PRIMARY KEY,
                remote_id       INTEGER NOT NULL UNIQUE,
                name            TEXT NOT NULL,
                slug            TEXT,
                color           TEXT,
                ticket_count    INTEGER,
                created_at      TEXT,
                updated_at      TEXT,
                local_created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
            );

            UPDATE app_state SET schema_version = 2 WHERE id = 1;
        "#,
    },
];

/// Find the migration with the given version, if any.
#[must_use]
pub fn find(version: u32) -> Option<Migration> {
    MIGRATIONS.iter().find(|m| m.version == version).copied()
}

/// The highest migration version known to this binary.
#[must_use]
pub fn latest_version() -> u32 {
    MIGRATIONS.iter().map(|m| m.version).max().unwrap_or(0)
}

/// Apply every pending migration to the connection.
///
/// Idempotent: migrations already recorded in `_migrations` are skipped.
/// Each migration is wrapped in its own transaction (atomic per migration).
pub fn run_all(conn: &mut rusqlite::Connection) -> crate::error::Result<()> {
    crate::db::ensure_migrations_table(conn)?;
    let current = crate::db::latest_version(conn)?;
    let target = latest_version();
    if current >= target {
        tracing::debug!(current, target, "migrations up to date");
        return Ok(());
    }
    for m in MIGRATIONS.iter().filter(|m| m.version > current) {
        tracing::info!(version = m.version, label = m.label, "applying migration");
        crate::db::apply_migration(conn, m.version, m.label, m.sql)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ensure_migrations_table, latest_version as db_latest};
    use tempfile::NamedTempFile;

    fn fresh_db() -> rusqlite::Connection {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        ensure_migrations_table(&conn).unwrap();
        run_all(&mut conn).unwrap();
        conn
    }

    #[test]
    fn versions_are_contiguous_starting_at_1() {
        let versions: Vec<u32> = MIGRATIONS.iter().map(|m| m.version).collect();
        assert!(!versions.is_empty(), "must have at least one migration");
        assert_eq!(versions[0], 1, "first migration must be version 1");
        for w in versions.windows(2) {
            assert_eq!(w[1], w[0] + 1, "migration versions must be contiguous");
        }
    }

    #[test]
    fn apply_in_order_on_fresh_db() {
        let conn = fresh_db();
        // After run_all, the latest applied version equals our latest_version().
        assert_eq!(db_latest(&conn).unwrap(), latest_version());

        // The single-row app_state bootstrap exists with first_run_done = 0.
        let (first_run, schema): (i64, i64) = conn
            .query_row(
                "SELECT first_run_done, schema_version FROM app_state WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(first_run, 0);
        assert_eq!(schema, 2, "schema_version should be 2 after M001 + M002");

        // application_settings round-trips.
        conn.execute(
            "INSERT INTO application_settings (key, value) VALUES ('theme', 'dark')",
            [],
        )
        .unwrap();
        let v: String = conn
            .query_row(
                "SELECT value FROM application_settings WHERE key = 'theme'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(v, "dark");

        // secrets accepts a BLOB.
        conn.execute(
            "INSERT INTO secrets (key, value) VALUES ('test_key', X'deadbeef')",
            [],
        )
        .unwrap();
        let _ = conn; // hold the connection until the test ends
    }

    #[test]
    fn run_all_is_idempotent() {
        let mut conn = fresh_db();
        let v1 = db_latest(&conn).unwrap();
        // Re-running must be a no-op and not error.
        run_all(&mut conn).unwrap();
        let v2 = db_latest(&conn).unwrap();
        assert_eq!(v1, v2);
    }

    #[test]
    fn find_returns_known_migration() {
        assert_eq!(find(1).map(|m| m.label), Some("initial_schema"));
        assert!(find(999).is_none());
    }

    #[test]
    fn latest_version_at_least_one() {
        assert!(latest_version() >= 1);
    }
}
