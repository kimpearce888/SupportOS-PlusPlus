//! M029 — sync/jobs schema parity with the reference.
//!
//! The reference (supportos) exposes these tables through the sync-status and
//! queue HTTP APIs, so the port's storage must carry the same columns
//! (`sync_runs`, `sync_checkpoints`, `jobs`, `outbound_jobs`,
//! `outbound_attempts`, `audit_log`, `application_errors`) plus the reference
//! resource tables the coordinator mirrors into (`accounts`, `folders`,
//! `inbox_fields`, `inbox_field_options`, property definitions,
//! `organizations`, `saved_replies`, `workflows`, `user_statuses`,
//! `webhook_configs`, `docs_collections`, `docs_categories`, `docs`).
//!
//! Everything is idempotent: legacy port tables are renamed, copied into the
//! reference-shaped replacement, then dropped — forward-only, no history
//! rewrite.

use rusqlite::Connection;

use crate::error::Result;

/// Apply the M029 migration batch (idempotent).
///
/// DB-01: the versioned boot-step wrapper in `bootstrap::apply_all` owns
/// the transaction (batch + `_migrations` row commit together), so this
/// body only applies the idempotent DDL. Callers that apply it standalone
/// get the same end state — every statement is `IF NOT EXISTS`-guarded.
pub fn apply_m029(conn: &Connection) -> Result<()> {
    apply_inner(conn)?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 29 WHERE id = 1", []);
    Ok(())
}

fn apply_inner(conn: &Connection) -> Result<()> {
    // ------------------------------------------------------------------
    // 1. sync_runs -> reference shape
    // ------------------------------------------------------------------
    if !column_exists(conn, "sync_runs", "kind")? {
        conn.execute_batch(
            "ALTER TABLE sync_runs RENAME TO sync_runs_legacy_m029;
             CREATE TABLE sync_runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                state TEXT NOT NULL,
                started_at TEXT NOT NULL,
                finished_at TEXT,
                resources_done INTEGER DEFAULT 0,
                resources_total INTEGER DEFAULT 0,
                records_processed INTEGER DEFAULT 0,
                errors INTEGER DEFAULT 0,
                detail TEXT
            );
             CREATE INDEX IF NOT EXISTS idx_sync_runs_started ON sync_runs(started_at DESC);
             INSERT INTO sync_runs (id, kind, state, started_at, finished_at, records_processed, errors)
               SELECT id, 'manual', COALESCE(status, 'completed'), COALESCE(started_at, datetime('now')),
                      completed_at, resources_synced,
                      CASE WHEN error IS NOT NULL THEN 1 ELSE 0 END
               FROM sync_runs_legacy_m029;
             DROP TABLE sync_runs_legacy_m029;",
        )?;
    }

    // ------------------------------------------------------------------
    // 1b. application_settings needs the reference's updated_at column
    //     (the port created only key/value; every reference settings write
    //     stamps updated_at).
    // ------------------------------------------------------------------
    if table_exists(conn, "application_settings")?
        && !column_exists(conn, "application_settings", "updated_at")?
    {
        // SQLite cannot ADD COLUMN with a non-constant default
        // (datetime('now') is an expression) — add nullable, then backfill.
        conn.execute_batch(
            "ALTER TABLE application_settings ADD COLUMN updated_at TEXT;
             UPDATE application_settings SET updated_at = datetime('now') WHERE updated_at IS NULL;",
        )?;
    }

    // ------------------------------------------------------------------
    // 2. sync_checkpoints -> reference shape
    // ------------------------------------------------------------------
    if !column_exists(conn, "sync_checkpoints", "last_success_at")? {
        conn.execute_batch(
            "ALTER TABLE sync_checkpoints RENAME TO sync_checkpoints_legacy_m029;
             CREATE TABLE sync_checkpoints (
                resource TEXT PRIMARY KEY,
                last_success_at TEXT,
                remote_cursor TEXT,
                page_state TEXT,
                records_processed INTEGER DEFAULT 0,
                records_failed INTEGER DEFAULT 0,
                last_error TEXT,
                retry_count INTEGER DEFAULT 0,
                status TEXT DEFAULT 'idle'
            );
             INSERT INTO sync_checkpoints (resource, last_success_at, remote_cursor, records_processed, status)
               SELECT resource, recorded_at, cursor_token, 0, 'idle'
               FROM sync_checkpoints_legacy_m029;
             DROP TABLE sync_checkpoints_legacy_m029;",
        )?;
    }

    // ------------------------------------------------------------------
    // 3. jobs -> reference shape (queue/type/priority/status vocabulary)
    // ------------------------------------------------------------------
    if !table_exists(conn, "jobs")? {
        // Fresh database: create the reference-shaped table directly.
        conn.execute_batch(
            "CREATE TABLE jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                queue TEXT NOT NULL,
                type TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 2,
                status TEXT NOT NULL DEFAULT 'queued',
                payload TEXT,
                attempt INTEGER DEFAULT 0,
                max_attempts INTEGER DEFAULT 3,
                error TEXT,
                run_at TEXT,
                locked_by TEXT,
                locked_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status, priority, run_at);
            CREATE INDEX IF NOT EXISTS idx_jobs_queue ON jobs(queue, status);",
        )?;
    } else if !column_exists(conn, "jobs", "queue")? {
        conn.execute_batch(
            "ALTER TABLE jobs RENAME TO jobs_legacy_m029;
             CREATE TABLE jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                queue TEXT NOT NULL,
                type TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 2,
                status TEXT NOT NULL DEFAULT 'queued',
                payload TEXT,
                attempt INTEGER DEFAULT 0,
                max_attempts INTEGER DEFAULT 3,
                error TEXT,
                run_at TEXT,
                locked_by TEXT,
                locked_at TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                started_at TEXT,
                completed_at TEXT
            );
             CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status, priority, run_at);
             CREATE INDEX IF NOT EXISTS idx_jobs_queue ON jobs(queue, status);
             INSERT INTO jobs (id, queue, type, priority, status, payload, attempt, max_attempts,
                               error, run_at, created_at, started_at, completed_at)
               SELECT id, 'sync', kind, 2,
                      CASE state
                        WHEN 'pending' THEN 'queued'
                        WHEN 'queued' THEN 'queued'
                        WHEN 'claimed' THEN 'running'
                        WHEN 'running' THEN 'running'
                        WHEN 'done' THEN 'completed'
                        WHEN 'completed' THEN 'completed'
                        WHEN 'dead' THEN 'failed'
                        WHEN 'failed' THEN 'failed'
                        WHEN 'cancelled' THEN 'cancelled'
                        ELSE 'queued' END,
                      payload, attempts, max_attempts, last_error, available_at,
                      available_at, claimed_at, completed_at
               FROM jobs_legacy_m029;
             DROP TABLE jobs_legacy_m029;",
        )?;
    }

    // ------------------------------------------------------------------
    // 4. outbound write queue + audit + errors (reference migration 002)
    // ------------------------------------------------------------------
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS outbound_jobs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            kind TEXT NOT NULL,
            conversation_id INTEGER REFERENCES conversations(id) ON DELETE SET NULL,
            thread_id INTEGER REFERENCES conversation_threads(id) ON DELETE SET NULL,
            payload TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'queued',
            attempts INTEGER DEFAULT 0,
            max_attempts INTEGER DEFAULT 1,
            error TEXT,
            remote_result TEXT,
            requires_confirmation INTEGER DEFAULT 0,
            confirmed_at TEXT,
            idempotency_key TEXT UNIQUE,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE INDEX IF NOT EXISTS idx_outbound_status ON outbound_jobs(status);

        CREATE TABLE IF NOT EXISTS outbound_attempts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            outbound_job_id INTEGER NOT NULL REFERENCES outbound_jobs(id) ON DELETE CASCADE,
            attempt INTEGER NOT NULL,
            request_summary TEXT,
            status_code INTEGER,
            response_body TEXT,
            latency_ms INTEGER,
            at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            actor TEXT NOT NULL,
            action TEXT NOT NULL,
            conversation_id INTEGER,
            before_state TEXT,
            after_state TEXT,
            remote_operation TEXT,
            remote_result TEXT,
            ai_involvement INTEGER DEFAULT 0,
            job_id INTEGER,
            correlation_id TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_audit_conversation ON audit_log(conversation_id);
        CREATE INDEX IF NOT EXISTS idx_audit_time ON audit_log(timestamp DESC);

        CREATE TABLE IF NOT EXISTS application_errors (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp TEXT NOT NULL DEFAULT (datetime('now')),
            service TEXT,
            message TEXT,
            stack TEXT,
            context TEXT
        );",
    )?;

    // ------------------------------------------------------------------
    // 5a. conversation_threads needs a remote_id for mirror sync + stale
    //     cleanup (reference threads.remote_id semantics). A full UNIQUE
    //     index (not partial) so `ON CONFLICT(remote_id)` upserts work;
    //     NULL remote_ids remain unconstrained (local-only threads).
    if table_exists(conn, "conversation_threads")?
        && !column_exists(conn, "conversation_threads", "remote_id")?
    {
        conn.execute_batch(
            "ALTER TABLE conversation_threads ADD COLUMN remote_id INTEGER;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_conv_threads_remote
                 ON conversation_threads (remote_id);",
        )?;
    }

    // ------------------------------------------------------------------
    // 5c. issue_clusters needs the reference's `trend` column for the
    //     issue_spike operations tile ('rising' | 'stable' | 'declining').
    // ------------------------------------------------------------------
    if table_exists(conn, "issue_clusters")? && !column_exists(conn, "issue_clusters", "trend")? {
        conn.execute_batch(
            "ALTER TABLE issue_clusters ADD COLUMN trend TEXT NOT NULL DEFAULT 'stable';",
        )?;
    }

    // 5b. reference resource tables (migration 001 shapes)
    // ------------------------------------------------------------------
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS accounts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            plan TEXT,
            company_name TEXT,
            raw_json TEXT,
            raw_json_hash TEXT,
            last_seen_at TEXT,
            local_created_at TEXT NOT NULL DEFAULT (datetime('now')),
            local_updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );

        CREATE TABLE IF NOT EXISTS folders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            type TEXT,
            user_id INTEGER,
            total_count INTEGER DEFAULT 0,
            active_count INTEGER DEFAULT 0,
            raw_json TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_folders_mailbox ON folders(mailbox_id);

        CREATE TABLE IF NOT EXISTS inbox_fields (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            mailbox_id INTEGER NOT NULL REFERENCES mailboxes(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            type TEXT,
            system_type TEXT,
            required INTEGER DEFAULT 0,
            sort_order INTEGER DEFAULT 0,
            raw_json TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_inbox_fields_mailbox ON inbox_fields(mailbox_id);

        CREATE TABLE IF NOT EXISTS inbox_field_options (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            field_id INTEGER NOT NULL REFERENCES inbox_fields(id) ON DELETE CASCADE,
            label TEXT,
            sort_order INTEGER DEFAULT 0
        );

        CREATE TABLE IF NOT EXISTS customer_property_definitions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            name TEXT NOT NULL,
            slug TEXT,
            type TEXT,
            sort_order INTEGER DEFAULT 0,
            raw_json TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );

        CREATE TABLE IF NOT EXISTS organization_property_definitions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            name TEXT NOT NULL,
            slug TEXT,
            type TEXT,
            sort_order INTEGER DEFAULT 0,
            raw_json TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );

        CREATE TABLE IF NOT EXISTS organizations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE NOT NULL,
            name TEXT NOT NULL,
            domains TEXT,
            raw_json TEXT,
            raw_json_hash TEXT,
            remote_created_at TEXT,
            remote_updated_at TEXT,
            last_seen_at TEXT,
            last_synced_at TEXT,
            local_created_at TEXT NOT NULL DEFAULT (datetime('now')),
            local_updated_at TEXT NOT NULL DEFAULT (datetime('now')),
            deleted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_organizations_name ON organizations(name);

        CREATE TABLE IF NOT EXISTS saved_replies (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            mailbox_local_id INTEGER REFERENCES mailboxes(id),
            name TEXT NOT NULL,
            preview TEXT,
            text TEXT,
            chat_text TEXT,
            raw_json TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );

        CREATE TABLE IF NOT EXISTS workflows (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            mailbox_local_id INTEGER REFERENCES mailboxes(id),
            name TEXT NOT NULL,
            type TEXT,
            status TEXT,
            sort_order INTEGER,
            raw_json TEXT,
            remote_updated_at TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );

        CREATE TABLE IF NOT EXISTS user_statuses (
            user_local_id INTEGER PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
            email_status TEXT,
            email_updated_at TEXT,
            chat_status TEXT,
            mailbox_statuses TEXT,
            raw_json TEXT,
            last_synced_at TEXT
        );

        CREATE TABLE IF NOT EXISTS webhook_configs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            url TEXT,
            events TEXT,
            status TEXT,
            raw_json TEXT,
            last_synced_at TEXT
        );

        CREATE TABLE IF NOT EXISTS docs_collections (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            slug TEXT,
            name TEXT NOT NULL,
            description TEXT,
            raw_json TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );

        CREATE TABLE IF NOT EXISTS docs_categories (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            collection_local_id INTEGER NOT NULL REFERENCES docs_collections(id) ON DELETE CASCADE,
            slug TEXT,
            name TEXT NOT NULL,
            raw_json TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );

        CREATE TABLE IF NOT EXISTS docs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            collection_local_id INTEGER REFERENCES docs_collections(id) ON DELETE SET NULL,
            category_local_id INTEGER REFERENCES docs_categories(id) ON DELETE SET NULL,
            slug TEXT,
            number INTEGER,
            revision INTEGER,
            name TEXT NOT NULL,
            text TEXT,
            text_plain TEXT,
            status TEXT,
            visibility TEXT,
            views INTEGER DEFAULT 0,
            raw_json TEXT,
            remote_created_at TEXT,
            remote_updated_at TEXT,
            last_synced_at TEXT,
            deleted_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_docs_collection ON docs(collection_local_id);

        CREATE TABLE IF NOT EXISTS ratings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_id INTEGER UNIQUE,
            conversation_id INTEGER REFERENCES conversations(id) ON DELETE CASCADE,
            thread_local_id INTEGER REFERENCES conversation_threads(id),
            rating TEXT,
            comments TEXT,
            customer_local_id INTEGER REFERENCES customers(id),
            user_local_id INTEGER REFERENCES users(id),
            remote_created_at TEXT,
            raw_json TEXT,
            last_synced_at TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_ratings_conversation ON ratings(conversation_id);",
    )?;

    Ok(())
}

/// M042 — reshape `sync_cursors` to the reference shape (DB-12).
///
/// The reference's `syncRepo.getCursor/setCursor` (syncRepo.ts:118-129)
/// ride a dedicated table created by its migration 002:
/// `sync_cursors (resource TEXT PRIMARY KEY, cursor TEXT, updated_at TEXT)`.
/// The port's migration 2 already created a table with that NAME but the
/// legacy port shape (`last_page` / `last_seen_at` / `cursor_token` —
/// never read or written by any code). This batch reshapes it to the
/// reference columns and adopts any stored token, so the DB-12
/// `get_cursor`/`set_cursor` helpers have the reference's exact SQL shape
/// to ride on. Idempotent: only runs when the legacy shape is detected.
pub fn apply_m042(conn: &Connection) -> Result<()> {
    if table_exists(conn, "sync_cursors")? && !column_exists(conn, "sync_cursors", "cursor")? {
        conn.execute_batch(
            "ALTER TABLE sync_cursors RENAME TO sync_cursors_legacy_m042;
             CREATE TABLE sync_cursors (
                resource TEXT PRIMARY KEY,
                cursor TEXT,
                updated_at TEXT
             );
             INSERT INTO sync_cursors (resource, cursor, updated_at)
               SELECT resource, cursor_token, COALESCE(last_seen_at, datetime('now'))
                 FROM sync_cursors_legacy_m042
                WHERE cursor_token IS NOT NULL AND cursor_token != '';
             DROP TABLE sync_cursors_legacy_m042;",
        )?;
    } else if !table_exists(conn, "sync_cursors")? {
        conn.execute_batch(
            "CREATE TABLE sync_cursors (
                resource TEXT PRIMARY KEY,
                cursor TEXT,
                updated_at TEXT
             );",
        )?;
    }
    Ok(())
}

/// Whether `table.column` exists (SQLite pragma helper).
pub fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether `table` exists.
pub fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |r| r.get(0),
    )?;
    Ok(n > 0)
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
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        crate::inbox::apply_m028(&conn).unwrap();
        conn
    }

    #[test]
    fn m029_creates_reference_sync_runs() {
        let conn = fresh_db();
        crate::inbox::apply_m028(&conn).unwrap();
        apply_m029(&conn).unwrap();
        assert!(column_exists(&conn, "sync_runs", "kind").unwrap());
        assert!(column_exists(&conn, "sync_runs", "resources_total").unwrap());
        assert!(column_exists(&conn, "conversation_threads", "remote_id").unwrap());
        // Idempotent.
        apply_m029(&conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='sync_runs_legacy_m029'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn m029_upgrades_jobs_vocabulary() {
        let conn = fresh_db();
        // Simulate the legacy port jobs table (pre-M029 shape).
        conn.execute_batch(
            "DROP TABLE IF EXISTS jobs;
             CREATE TABLE jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind TEXT NOT NULL,
                payload TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'pending',
                attempts INTEGER NOT NULL DEFAULT 0,
                max_attempts INTEGER NOT NULL DEFAULT 5,
                available_at TEXT NOT NULL DEFAULT (datetime('now')),
                claimed_at TEXT,
                completed_at TEXT,
                last_error TEXT
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO jobs (kind, payload, state) VALUES ('demo.x', '{}', 'done')",
            [],
        )
        .unwrap();
        apply_m029(&conn).unwrap();
        let status: String = conn
            .query_row("SELECT status FROM jobs LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(status, "completed");
        let queue: String = conn
            .query_row("SELECT queue FROM jobs LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(queue, "sync");
    }

    #[test]
    fn m029_preserves_sync_run_history() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO sync_runs (status, started_at, resources_synced) VALUES ('completed', '2026-01-01T00:00:00Z', 5)",
            [],
        )
        .unwrap();
        apply_m029(&conn).unwrap();
        let (kind, state, rec): (String, String, i64) = conn
            .query_row(
                "SELECT kind, state, records_processed FROM sync_runs LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(kind, "manual");
        assert_eq!(state, "completed");
        assert_eq!(rec, 5);
    }

    #[test]
    fn m029_creates_outbound_and_audit_tables() {
        let conn = fresh_db();
        apply_m029(&conn).unwrap();
        for table in [
            "outbound_jobs",
            "outbound_attempts",
            "audit_log",
            "application_errors",
            "folders",
            "inbox_fields",
            "organizations",
            "saved_replies",
            "workflows",
            "user_statuses",
            "docs_collections",
            "docs_categories",
            "docs",
        ] {
            let n: i64 = conn
                .query_row(
                    &format!(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{table}'"
                    ),
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} must exist after M029");
        }
    }

    #[test]
    fn m029_bumps_schema_version() {
        let conn = fresh_db();
        apply_m029(&conn).unwrap();
        let v: i64 = conn
            .query_row(
                "SELECT COALESCE(schema_version, 0) FROM app_state WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(v >= 29);
    }
}
