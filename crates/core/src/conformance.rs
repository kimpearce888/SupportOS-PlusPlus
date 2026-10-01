//! Conformance and hardening — parity gate + crash-recovery tests (M11).
//!
//! Per spec M11: "Conformance and hardening: parity gate, reference-delta check,
//! black-box audit, performance and crash-recovery tests."
//! Per spec A3: "You may NOT declare the project complete or '100% parity'."
//! Per spec TESTING: "crash recovery, performance guards."

use rusqlite::Connection;

use crate::catalog::{
    ActivityField, AiAttributeKey, ConditionKind, CopilotTool, DateMode, GraphNodeKind,
    NotificationType, OperationsTileKey, ReportDimensionKey, ReportMetricKey,
};
use crate::error::Result;

// ─── M11-T01: Parity gate ─────────────────────────────────────────────────

/// The expected canonical counts from the reference repo's `inventory.json`.
/// Per spec A7: "Cross-check against the reference CHANGELOG counts."
/// These are the ground truth — if the catalog enum doesn't match, the test fails.
pub const EXPECTED_OPERATIONS_TILES: usize = 16;
pub const EXPECTED_NOTIFICATION_TYPES: usize = 15;
pub const EXPECTED_CONDITION_KINDS: usize = 22;
pub const EXPECTED_ACTIVITY_FIELDS: usize = 14;
pub const EXPECTED_DATE_MODES: usize = 15;
pub const EXPECTED_REPORT_METRICS: usize = 21;
pub const EXPECTED_REPORT_DIMENSIONS: usize = 14;
pub const EXPECTED_AI_ATTRIBUTE_KEYS: usize = 14;
pub const EXPECTED_GRAPH_NODE_KINDS: usize = 12;
pub const EXPECTED_COPILOT_TOOLS: usize = 22;

/// Verify that every catalog enum's `ALL` array has the correct count matching
/// the reference. Returns `Ok(())` if all match, `Err` with the mismatch.
pub fn verify_canonical_counts() -> Result<()> {
    assert_count(
        "OperationsTileKey",
        OperationsTileKey::ALL.len(),
        EXPECTED_OPERATIONS_TILES,
    )?;
    assert_count(
        "NotificationType",
        NotificationType::ALL.len(),
        EXPECTED_NOTIFICATION_TYPES,
    )?;
    assert_count(
        "ConditionKind",
        ConditionKind::ALL.len(),
        EXPECTED_CONDITION_KINDS,
    )?;
    assert_count(
        "ActivityField",
        ActivityField::ALL.len(),
        EXPECTED_ACTIVITY_FIELDS,
    )?;
    assert_count("DateMode", DateMode::ALL.len(), EXPECTED_DATE_MODES)?;
    assert_count(
        "ReportMetricKey",
        ReportMetricKey::ALL.len(),
        EXPECTED_REPORT_METRICS,
    )?;
    assert_count(
        "ReportDimensionKey",
        ReportDimensionKey::ALL.len(),
        EXPECTED_REPORT_DIMENSIONS,
    )?;
    assert_count(
        "AiAttributeKey",
        AiAttributeKey::ALL.len(),
        EXPECTED_AI_ATTRIBUTE_KEYS,
    )?;
    assert_count(
        "GraphNodeKind",
        GraphNodeKind::ALL.len(),
        EXPECTED_GRAPH_NODE_KINDS,
    )?;
    assert_count(
        "CopilotTool",
        CopilotTool::ALL.len(),
        EXPECTED_COPILOT_TOOLS,
    )?;
    Ok(())
}

fn assert_count(name: &str, actual: usize, expected: usize) -> Result<()> {
    if actual != expected {
        return Err(crate::error::Error::Config(format!(
            "parity gate: {name} has {actual} variants, expected {expected}"
        )));
    }
    Ok(())
}

/// Total count of all catalog enum variants. Used by the FINAL-PARITY-AUDIT.
#[must_use]
pub fn total_catalog_variants() -> usize {
    OperationsTileKey::ALL.len()
        + NotificationType::ALL.len()
        + ConditionKind::ALL.len()
        + ActivityField::ALL.len()
        + DateMode::ALL.len()
        + ReportMetricKey::ALL.len()
        + ReportDimensionKey::ALL.len()
        + AiAttributeKey::ALL.len()
        + GraphNodeKind::ALL.len()
        + CopilotTool::ALL.len()
}

// ─── M11-T02: Crash-recovery tests ────────────────────────────────────────

/// Verify that all migrations are idempotent (re-running doesn't error).
/// Per spec TESTING: "crash recovery."
pub fn verify_migration_idempotency(conn: &Connection) -> Result<()> {
    // Re-apply every migration — should be a no-op.
    crate::activity::apply_m003(conn)?;
    crate::ticket_states::apply_m004(conn)?;
    crate::notifications::apply_m005(conn)?;
    crate::side_threads::apply_m006(conn)?;
    crate::automation::apply_m007(conn)?;
    crate::embeddings::apply_m008(conn)?;
    crate::ai_center::apply_m009(conn)?;
    crate::ai_analysis::apply_m010(conn)?;
    crate::ai_features::apply_m011_to_m013(conn)?;
    crate::intelligence::apply_m014(conn)?;
    crate::intelligence_features::apply_m015_to_m019(conn)?;
    crate::reports::apply_m020_to_m022(conn)?;
    crate::outreach::apply_m023_to_m025(conn)?;
    crate::data_tools::apply_m026_to_m027(conn)?;
    Ok(())
}

/// Verify that a DB reopens cleanly after a simulated crash (WAL recovery).
/// Per spec TESTING: "crash recovery."
pub fn verify_db_reopen(path: &std::path::Path) -> Result<()> {
    let conn = crate::db::open(path)?;
    // If the DB was closed mid-write, WAL recovery happens on open.
    // Verify we can query a known table.
    let _: i64 = conn.query_row("SELECT COUNT(*) FROM app_state WHERE id = 1", [], |r| {
        r.get(0)
    })?;
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
        let mut conn = crate::db::open(&f).unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();
        crate::migrations::run_all(&mut conn).unwrap();
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
        crate::reports::apply_m020_to_m022(&conn).unwrap();
        crate::outreach::apply_m023_to_m025(&conn).unwrap();
        crate::data_tools::apply_m026_to_m027(&conn).unwrap();
        conn
    }

    // ---- M11-T01: Parity gate -----------------------------------------------

    #[test]
    fn all_canonical_counts_match() {
        verify_canonical_counts().unwrap();
    }

    #[test]
    fn operations_tiles_count() {
        assert_eq!(OperationsTileKey::ALL.len(), 16);
    }

    #[test]
    fn notification_types_count() {
        assert_eq!(NotificationType::ALL.len(), 15);
    }

    #[test]
    fn condition_kinds_count() {
        assert_eq!(ConditionKind::ALL.len(), 22);
    }

    #[test]
    fn copilot_tools_count() {
        assert_eq!(CopilotTool::ALL.len(), 22);
    }

    #[test]
    fn ai_attribute_keys_count() {
        assert_eq!(AiAttributeKey::ALL.len(), 14);
    }

    #[test]
    fn graph_node_kinds_count() {
        assert_eq!(GraphNodeKind::ALL.len(), 12);
    }

    #[test]
    fn report_metrics_count() {
        assert_eq!(ReportMetricKey::ALL.len(), 21);
    }

    #[test]
    fn report_dimensions_count() {
        assert_eq!(ReportDimensionKey::ALL.len(), 14);
    }

    #[test]
    fn total_catalog_variants_is_correct() {
        // 16 + 15 + 22 + 14 + 15 + 21 + 14 + 14 + 12 + 22 = 165
        assert_eq!(total_catalog_variants(), 165);
    }

    // ---- M11-T02: Crash recovery --------------------------------------------

    #[test]
    fn all_migrations_are_idempotent() {
        let conn = fresh_db();
        verify_migration_idempotency(&conn).unwrap();
    }

    #[test]
    fn db_reopens_cleanly() {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        // Create + populate.
        {
            let mut conn = crate::db::open(&f).unwrap();
            crate::db::ensure_migrations_table(&conn).unwrap();
            crate::migrations::run_all(&mut conn).unwrap();
        }
        // Reopen — WAL recovery should be transparent.
        verify_db_reopen(&f).unwrap();
    }

    #[test]
    fn job_recovery_on_restart() {
        // Simulate: enqueue a job, "crash" (drop the connection), reopen, verify the job is still there.
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        {
            let mut conn = crate::db::open(&f).unwrap();
            crate::db::ensure_migrations_table(&conn).unwrap();
            crate::migrations::run_all(&mut conn).unwrap();
            crate::jobs::ensure_jobs_table(&conn).unwrap();
            crate::jobs::enqueue(&conn, "test_job", r#"{"data":"test"}"#).unwrap();
        }
        // Reopen.
        let conn = crate::db::open(&f).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE state = 'pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "job survives a restart");
    }

    #[test]
    fn notification_sweep_cursor_survives_restart() {
        let f = NamedTempFile::new()
            .unwrap()
            .into_temp_path()
            .keep()
            .unwrap();
        {
            let mut conn = crate::db::open(&f).unwrap();
            crate::db::ensure_migrations_table(&conn).unwrap();
            crate::migrations::run_all(&mut conn).unwrap();
            crate::activity::apply_m003(&conn).unwrap();
            crate::embeddings::apply_m008(&conn).unwrap();
            crate::settings::set_i64(&conn, "notifications.sweep.last_processed_event_id", 42)
                .unwrap();
        }
        // Reopen.
        let conn = crate::db::open(&f).unwrap();
        let cursor: i64 =
            crate::settings::get_i64(&conn, "notifications.sweep.last_processed_event_id", -1)
                .unwrap();
        assert_eq!(cursor, 42, "sweep cursor survives restart");
    }
}
