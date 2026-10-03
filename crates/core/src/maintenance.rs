//! Maintenance tick machinery (reference `WorkerManager.maintenance` +
//! `issueRepo.computeTrends` + `graphService.refreshProducts` + retention).
//!
//! Runs every 6h from the worker loop: issue-cluster trends, the
//! deterministic products registry (INSERT OR IGNORE — a rebuild can only
//! ever ADD names, never overwrite), the notification-sweep piggyback,
//! interval-honored backups (pruned to the newest 20), and the local
//! data-retention window (Settings > Data).

use rusqlite::Connection;

use crate::error::Result;

// ─── M037 schema ───────────────────────────────────────────────────────────

/// Products registry + product provenance columns. Idempotent.
pub fn apply_m037(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS products (
            id           INTEGER PRIMARY KEY AUTOINCREMENT,
            name         TEXT NOT NULL UNIQUE COLLATE NOCASE,
            description  TEXT,
            pinned       INTEGER NOT NULL DEFAULT 0,
            source       TEXT NOT NULL DEFAULT 'derived'
                CHECK (source IN ('derived','human')),
            first_seen_at TEXT,
            last_seen_at TEXT,
            created_at   TEXT NOT NULL DEFAULT (datetime('now')),
            provenance   TEXT NOT NULL DEFAULT 'deterministic_local'
        );",
    )?;
    // Product provenance columns the legacy tables lacked (PRAGMA-guarded).
    add_column_if_missing(conn, "incidents", "product", "TEXT")?;
    add_column_if_missing(conn, "known_issues", "product", "TEXT")?;
    add_column_if_missing(conn, "issue_clusters", "product", "TEXT")?;
    let _ = conn.execute("UPDATE app_state SET schema_version = 37 WHERE id = 1", []);
    Ok(())
}

fn add_column_if_missing(conn: &Connection, table: &str, column: &str, decl: &str) -> Result<()> {
    let exists: bool = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(|r| r.ok())
        .any(|c| c == column);
    if !exists {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    }
    Ok(())
}

// ─── Issue-cluster trends (reference issueRepo.computeTrends) ──────────────

/// Recompute `issue_clusters.trend` from 14/28-day conversation windows:
/// rising ≥ +30% and ≥3 recent, falling < −30%, new = no previous but some
/// recent, stable otherwise. Clusters first seen within 14 days start as
/// 'new' before the window math refines them.
pub fn compute_trends(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE issue_clusters SET trend = CASE
           WHEN julianday(first_seen_at) >= julianday('now', '-14 days') THEN 'new'
           ELSE 'stable'
         END",
        [],
    )?;
    let clusters: Vec<(i64, i64, i64)> = {
        let mut stmt = conn.prepare(
            "SELECT ic.id,
               (SELECT COUNT(*) FROM issue_cluster_members icm
                  JOIN conversations c ON c.id = icm.conversation_id
                 WHERE icm.cluster_id = ic.id
                   AND julianday(c.created_at) >= julianday('now', '-14 days')) AS recent,
               (SELECT COUNT(*) FROM issue_cluster_members icm
                  JOIN conversations c ON c.id = icm.conversation_id
                 WHERE icm.cluster_id = ic.id
                   AND julianday(c.created_at) >= julianday('now', '-28 days')
                   AND julianday(c.created_at) <  julianday('now', '-14 days')) AS previous
             FROM issue_clusters ic",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    let tx = conn.unchecked_transaction()?;
    for (id, recent, previous) in clusters {
        let trend = if recent as f64 > previous as f64 * 1.3 && recent >= 3 {
            "rising"
        } else if previous > 0 && (recent as f64) < previous as f64 * 0.7 {
            "falling"
        } else if previous == 0 && recent > 0 {
            "new"
        } else {
            "stable"
        };
        tx.execute("UPDATE issue_clusters SET trend = ?1 WHERE id = ?2", rusqlite::params![trend, id])?;
    }
    tx.commit()?;
    Ok(())
}

// ─── Products registry (reference graphService.refreshProducts) ────────────

/// Deterministic registry refresh: INSERT OR IGNORE over the observable
/// product mentions (incidents / known issues / issue clusters). A rebuild
/// can only ever ADD names, never overwrite. Returns how many were added.
pub fn refresh_products(conn: &Connection) -> Result<usize> {
    let before: i64 = conn.query_row("SELECT COUNT(*) FROM products", [], |r| r.get(0))?;
    conn.execute(
        "INSERT OR IGNORE INTO products (name, source, first_seen_at, last_seen_at, provenance)
         SELECT DISTINCT TRIM(p.product), 'derived', datetime('now'), datetime('now'), 'deterministic_local'
           FROM (
             SELECT product FROM incidents WHERE product IS NOT NULL AND TRIM(product) != '' AND LENGTH(TRIM(product)) <= 120
             UNION ALL
             SELECT product FROM known_issues WHERE product IS NOT NULL AND TRIM(product) != '' AND LENGTH(TRIM(product)) <= 120
             UNION ALL
             SELECT product FROM issue_clusters WHERE product IS NOT NULL AND TRIM(product) != '' AND LENGTH(TRIM(product)) <= 120
           ) AS p
          WHERE TRIM(p.product) != ''",
        [],
    )?;
    let after: i64 = conn.query_row("SELECT COUNT(*) FROM products", [], |r| r.get(0))?;
    Ok((after - before).max(0) as usize)
}

// ─── Data retention (reference WorkerManager.enforceRetention) ─────────────

/// Prune LOCAL operational data older than `retention_days` — webhook
/// events, application errors, audit log entries, AI run records and
/// notifications. Conversations/threads/customers are NOT pruned: they
/// mirror Help Scout and would simply re-sync; delete them in Help Scout
/// itself. 0/null (or a non-finite value) disables pruning. Returns the
/// total number of removed rows.
pub fn enforce_retention(conn: &Connection, retention_days: i64) -> Result<usize> {
    if retention_days <= 0 {
        return Ok(0);
    }
    // The reference compares ISO timestamps with julianday(); the port's
    // tables store either ISO ('T'-separated) or SQLite datetime('now')
    // (' '-separated) strings. julianday() parses BOTH formats, so the
    // cutoff is computed inside SQL as 'now' minus the window.
    let mut removed = 0usize;
    for (table, column) in [
        ("webhook_events", "received_at"),
        ("application_errors", "timestamp"),
        ("audit_log", "timestamp"),
        ("ai_runs", "created_at"),
    ] {
        let sql = format!(
            "DELETE FROM {table} WHERE julianday({column}) < julianday('now', '-{retention_days} days')"
        );
        // Table/column missing on older schemas — skip (reference behavior).
        if let Ok(n) = conn.execute(&sql, []) {
            removed += n;
        }
    }
    // v1.8.0: notifications are local operational data too — same window.
    if let Ok(n) = conn.execute(
        "DELETE FROM notifications WHERE read_at IS NOT NULL AND julianday(created_at) < julianday('now', '-?1 days')",
        [retention_days],
    ) {
        removed += n;
    }
    Ok(removed)
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
        crate::search::apply_fts_migration(&conn).unwrap();
        crate::activity::apply_m003(&conn).unwrap();
        crate::ticket_states::apply_m004(&conn).unwrap();
        crate::notifications::apply_m005(&conn).unwrap();
        crate::side_threads::apply_m006(&conn).unwrap();
        crate::automation::apply_m007(&conn).unwrap();
        crate::embeddings::apply_m008(&conn).unwrap();
        crate::ai_center::apply_m009(&conn).unwrap();
        crate::ai_analysis::apply_m010(&conn).unwrap();
        crate::ai_features::apply_m011_to_m013(&conn).unwrap();
        crate::intelligence::apply_m014(&conn).unwrap();
        crate::intelligence_features::apply_m015_to_m019(&conn).unwrap();
        crate::webhook::ensure_webhook_events_table(&conn).unwrap();
        crate::reports::apply_m020_to_m022(&conn).unwrap();
        crate::outreach::apply_m023_to_m025(&conn).unwrap();
        crate::data_tools::apply_m026_to_m027(&conn).unwrap();
        crate::inbox::apply_m028(&conn).unwrap();
        crate::sync_schema::apply_m029(&conn).unwrap();
        crate::conversation_ops::apply_m030(&conn).unwrap();
        crate::outreach::apply_m031(&conn).unwrap();
        crate::ticket_states::apply_m032(&conn).unwrap();
        crate::ai_attributes::apply_m033(&conn).unwrap();
        crate::reports::apply_m034(&conn).unwrap();
        crate::intelligence_features::apply_m035(&conn).unwrap();
        crate::customer_events::apply_m036(&conn).unwrap();
        crate::maintenance::apply_m037(&conn).unwrap();
        crate::connectors::apply_m038(&conn).unwrap();
        crate::mirror_tables::apply_m039(&conn).unwrap();
        conn
    }

    #[test]
    fn m037_is_idempotent() {
        let conn = fresh_db();
        apply_m037(&conn).unwrap();
    }

    #[test]
    fn products_registry_only_adds() {
        let mut conn = fresh_db();
        conn.execute(
            "INSERT INTO known_issues (name, product) VALUES ('KI', 'Widget')",
            [],
        )
        .unwrap();
        assert_eq!(refresh_products(&conn).unwrap(), 1);
        // Re-running adds nothing and never overwrites.
        assert_eq!(refresh_products(&conn).unwrap(), 0);
        conn.execute(
            "UPDATE known_issues SET product = 'Changed' WHERE name = 'KI'",
            [],
        )
        .unwrap();
        assert_eq!(refresh_products(&conn).unwrap(), 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM products", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn trends_recompute_rising_new_stable() {
        let mut conn = fresh_db();
        conn.execute(
            "INSERT INTO customers (remote_id, first_name) VALUES (1, 'A')",
            [],
        )
        .unwrap();
        // Cluster 1: 3 recent, 0 previous -> new (recent>0, previous==0).
        conn.execute(
            "INSERT INTO issue_clusters (name, first_seen_at) VALUES ('old cluster', '2025-01-01 00:00:00')",
            [],
        )
        .unwrap();
        for i in 1..=3i64 {
            conn.execute(
                "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id, created_at, updated_at)
                 VALUES (?1, ?1, 'active', 1, 1, datetime('now', '-1 day'), datetime('now', '-1 day'))",
                [i],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO issue_cluster_members (cluster_id, conversation_id) VALUES (1, ?1)",
                [i],
            )
            .unwrap();
        }
        compute_trends(&conn).unwrap();
        // recent=3 (>=3) with previous=0 crosses the rising bar first in
        // the reference switch order; only clusters without enough recent
        // volume stay 'new'.
        let trend: String = conn
            .query_row("SELECT trend FROM issue_clusters WHERE id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(trend, "rising");
        // first_seen within 14 days -> 'new' regardless (reference rule order).
        conn.execute(
            "INSERT INTO issue_clusters (name, first_seen_at) VALUES ('fresh cluster', datetime('now'))",
            [],
        )
        .unwrap();
        compute_trends(&conn).unwrap();
        // A fresh cluster with NO conversation volume stays 'stable' — the
        // reference's per-cluster switch has no 'new' branch for recent=0
        // (the blanket pre-update is always overridden by the loop).
        let trend2: String = conn
            .query_row("SELECT trend FROM issue_clusters WHERE id = 2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(trend2, "stable");
    }

    #[test]
    fn retention_disabled_and_prunes_old_rows() {
        let conn = fresh_db();
        assert_eq!(enforce_retention(&conn, 0).unwrap(), 0);
        conn.execute(
            "INSERT INTO webhook_events (event_hash, event_type, received_at, payload)
             VALUES ('h1', 'convo.created', datetime('now', '-90 days'), '{}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO webhook_events (event_hash, event_type, received_at, payload)
             VALUES ('h2', 'convo.created', datetime('now'), '{}')",
            [],
        )
        .unwrap();
        let removed = enforce_retention(&conn, 30).unwrap();
        assert_eq!(removed, 1);
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM webhook_events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
