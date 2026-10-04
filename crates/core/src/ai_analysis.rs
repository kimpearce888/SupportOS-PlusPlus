//! Legacy M010 AI-attributes migration — boot-order compatibility shim.
//!
//! The analysis + attribute layer now lives in `crate::ai_pipeline`
//! (AiPipeline, reference pipeline.ts) and `crate::ai_attributes` (M033
//! reference shape: versioned snapshots via `superseded_at`, closed 14-key
//! catalog, deterministic + AI layers). This module keeps only `apply_m010`:
//! it creates the legacy `ai_attributes` shape on DBs that have not yet
//! reached M033, and is a deliberate no-op once the M033 shape is in place
//! (so re-running the boot sequence never recreates the legacy index on the
//! rebuilt table).

use rusqlite::Connection;

use crate::error::Result;

/// The M010 migration: creates the legacy `ai_attributes` table.
pub const M010_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS ai_attributes (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        conversation_id INTEGER NOT NULL,
        attribute_key   TEXT NOT NULL,
        value           TEXT NOT NULL,
        evidence_excerpt TEXT NOT NULL,
        thread_ref      TEXT NOT NULL,
        confidence      REAL NOT NULL DEFAULT 0.0,
        created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
    );
    CREATE INDEX IF NOT EXISTS idx_ai_attributes_conv
        ON ai_attributes (conversation_id, attribute_key);

    UPDATE app_state SET schema_version = 10 WHERE id = 1;
"#;

/// Apply M010 migration. Idempotent. Superseded by M033 (`ai_attributes`
/// reference shape): once the table carries the `attribute` column, this is a
/// deliberate no-op so re-running the boot sequence never recreates the
/// legacy index on the rebuilt table.
pub fn apply_m010(conn: &Connection) -> Result<()> {
    // M033 shape already present — nothing to do (forward-only).
    {
        let mut stmt = conn.prepare("PRAGMA table_info(ai_attributes)")?;
        let has_new_shape = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|c| c.ok())
            .any(|c| c == "attribute");
        if has_new_shape {
            return Ok(());
        }
    }
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ai_attributes (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            conversation_id INTEGER NOT NULL,
            attribute_key   TEXT NOT NULL,
            value           TEXT NOT NULL,
            evidence_excerpt TEXT NOT NULL,
            thread_ref      TEXT NOT NULL,
            confidence      REAL NOT NULL DEFAULT 0.0,
            created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
        );
        CREATE INDEX IF NOT EXISTS idx_ai_attributes_conv
            ON ai_attributes (conversation_id, attribute_key);",
    )?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 10 WHERE id = 1", []);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn fresh_db() -> Connection {
        let f = tempfile::NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
        crate::embeddings::apply_m008(&conn).unwrap();
        apply_m010(&conn).unwrap();
        crate::ai_attributes::apply_m033(&conn).unwrap();
        conn
    }

    #[test]
    fn m010_creates_ai_attributes_table() {
        let conn = fresh_db();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ai_attributes", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn m033_replaces_the_m010_index_with_the_reference_indexes() {
        let conn = fresh_db();
        for idx in [
            "idx_ai_attributes_current",
            "idx_ai_attributes_value",
            "idx_ai_attributes_run",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = ?1",
                    params![idx],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "missing {idx}");
        }
    }

    #[test]
    fn m010_is_idempotent() {
        let conn = fresh_db();
        // After M033 the M010 apply is a deliberate no-op — re-running the
        // boot sequence must not fail on the rebuilt table.
        apply_m010(&conn).unwrap();
        apply_m010(&conn).unwrap();
    }
}
