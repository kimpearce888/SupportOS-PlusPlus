//! The canonical boot migration chain — the port of the reference's
//! `applyMigrations(db)` (`src/server/database/migrations/index.ts`), which
//! runs EVERY migration from one place so no boot path can drift.
//!
//! The reference applies its 16 migrations through a single entry point that
//! the server, the Tauri shell and the tests all share. The port's batches
//! (`run_all` + `apply_m0XX` + the lazy-table guards) previously lived in
//! hand-copied chains that diverged: the Tauri shell's chain lacked the FTS
//! migration, M029 (sync mirror reshape), M034, M039 (chunk tables) and the
//! table guards, so a fresh database crashed at M036 ("no such table:
//! organizations") and the semantic-search chunk tables were never created.
//! One chain, used everywhere, is the fix — and the reference's shape.
//!
//! DB-01: the boot batches are now **versioned** (`_migrations` rows 3..N)
//! and each batch commits inside **one transaction together with the row
//! that records it** — the port's history table finally tells the truth
//! about what was applied and when. The batch bodies stay idempotent
//! (`IF NOT EXISTS` + guarded ALTERs), so an existing database created
//! before versioning adopts the new versions with the same no-op re-run
//! it always did on boot; nothing is fabricated.

use crate::error::Result;
use rusqlite::Connection;

/// One versioned boot step (DB-01). `apply` runs inside a single
/// transaction owned by [`apply_all`], together with the `_migrations`
/// row that records this version.
struct BootStep {
    version: u32,
    label: &'static str,
    apply: fn(&Connection) -> Result<()>,
}

/// M007 created a reduced automation shape; the reference rule columns
/// complete it. One version, one transaction.
fn apply_m007_full(conn: &Connection) -> Result<()> {
    crate::automation::apply_m007(conn)?;
    crate::automation::ensure_main_rule_columns(conn)
}

/// The runtime table guards: tables the reference creates inside its
/// migrations but the port creates lazily on first use. Ensuring them in
/// the boot chain keeps every runtime read (webhooks, saved views, OAuth
/// state, job queue) from depending on call order. The individual ensure
/// functions stay callable from the routes — they are idempotent.
fn apply_runtime_guards(conn: &Connection) -> Result<()> {
    crate::webhook::ensure_webhook_events_table(conn)?;
    crate::saved_views::ensure_inbox_views_table(conn)?;
    crate::oauth_state::ensure_oauth_states_table(conn)?;
    crate::jobs::ensure_jobs_table(conn)?;
    crate::interaction_current::ensure_client_current_signals_table(conn)?;
    // AI-19 (C7): the interaction evidence table the interactions routes
    // read — ensured so the routes never depend on a prior refresh.
    crate::interaction_current::ensure_interaction_evidence_table(conn)?;
    // AI-16: the interaction-intelligence layer (migration 005 shapes —
    // observations/baselines/preferences/human-overrides/outcomes + the
    // recommendation column).
    crate::interaction_engine::ensure_schema(conn)?;
    // The SLA schema owns `conversations.deleted_at` (reference migration
    // 003). Ensuring it in the chain makes the column invariant instead of
    // first-SLA-report-dependent.
    crate::sla::ensure_sla_schema(conn)?;
    // AI pipeline stores (ai_drafts/ai_sources/customer_memory reference
    // shape) + the Copilot session/message tables + the tool-read tables
    // (reference migrations 003/013).
    crate::ai_pipeline::ensure_pipeline_schema(conn)?;
    crate::copilot::ensure_copilot_schema(conn)?;
    // Quality-domain tables (reference migration 015 shapes).
    crate::quality::ensure_quality_tables(conn)
}

/// The versioned boot-batch chain (DB-01). Versions continue the SQL-string
/// [`crate::migrations::MIGRATIONS`] sequence (1–2); order here is the
/// application order — forward-only, never reorder or renumber.
const BOOT_STEPS: &[BootStep] = &[
    BootStep {
        version: 3,
        label: "fts5_reference_004",
        apply: crate::search::apply_fts_migration,
    },
    BootStep {
        version: 4,
        label: "m003_activity",
        apply: crate::activity::apply_m003,
    },
    BootStep {
        version: 5,
        label: "m004_ticket_states",
        apply: crate::ticket_states::apply_m004,
    },
    BootStep {
        version: 6,
        label: "m005_notifications",
        apply: crate::notifications::apply_m005,
    },
    BootStep {
        version: 7,
        label: "m006_side_threads",
        apply: crate::side_threads::apply_m006,
    },
    BootStep {
        version: 8,
        label: "m007_automation_rules",
        apply: apply_m007_full,
    },
    BootStep {
        version: 9,
        label: "m008_embeddings",
        apply: crate::embeddings::apply_m008,
    },
    BootStep {
        version: 10,
        label: "m009_ai_center",
        apply: crate::ai_center::apply_m009,
    },
    BootStep {
        version: 11,
        label: "m010_ai_analysis",
        apply: crate::ai_analysis::apply_m010,
    },
    BootStep {
        version: 12,
        label: "m011_to_m013_ai_features",
        apply: crate::ai_features::apply_m011_to_m013,
    },
    BootStep {
        version: 13,
        label: "m014_intelligence",
        apply: crate::intelligence::apply_m014,
    },
    BootStep {
        version: 14,
        label: "m015_to_m019_intelligence_features",
        apply: crate::intelligence_features::apply_m015_to_m019,
    },
    BootStep {
        version: 15,
        label: "m020_to_m022_reports",
        apply: crate::reports::apply_m020_to_m022,
    },
    BootStep {
        version: 16,
        label: "m023_to_m025_outreach",
        apply: crate::outreach::apply_m023_to_m025,
    },
    BootStep {
        version: 17,
        label: "m026_to_m027_data_tools",
        apply: crate::data_tools::apply_m026_to_m027,
    },
    BootStep {
        version: 18,
        label: "m028_inbox",
        apply: crate::inbox::apply_m028,
    },
    BootStep {
        version: 19,
        label: "m029_sync_schema",
        apply: crate::sync_schema::apply_m029,
    },
    BootStep {
        version: 20,
        label: "m030_conversation_ops",
        apply: crate::conversation_ops::apply_m030,
    },
    BootStep {
        version: 21,
        label: "m031_outreach",
        apply: crate::outreach::apply_m031,
    },
    BootStep {
        version: 22,
        label: "m032_ticket_states",
        apply: crate::ticket_states::apply_m032,
    },
    BootStep {
        version: 23,
        label: "m033_ai_attributes",
        apply: crate::ai_attributes::apply_m033,
    },
    BootStep {
        version: 24,
        label: "m034_reports",
        apply: crate::reports::apply_m034,
    },
    BootStep {
        version: 25,
        label: "m035_incidents",
        apply: crate::intelligence_features::apply_m035,
    },
    BootStep {
        version: 26,
        label: "m036_customer_events",
        apply: crate::customer_events::apply_m036,
    },
    BootStep {
        version: 27,
        label: "m037_maintenance",
        apply: crate::maintenance::apply_m037,
    },
    BootStep {
        version: 28,
        label: "m038_connectors",
        apply: crate::connectors::apply_m038,
    },
    BootStep {
        version: 29,
        label: "m039_mirror_tables",
        apply: crate::mirror_tables::apply_m039,
    },
    BootStep {
        version: 30,
        label: "m040_db_breadth",
        apply: crate::db_breadth::apply_m040,
    },
    BootStep {
        version: 31,
        label: "m041_incident_workspace",
        apply: crate::intelligence_features::apply_m041,
    },
    BootStep {
        version: 32,
        label: "runtime_table_guards",
        apply: apply_runtime_guards,
    },
    BootStep {
        version: 33,
        label: "m042_sync_cursors_reference",
        apply: crate::sync_schema::apply_m042,
    },
    BootStep {
        version: 34,
        label: "m043_hs_rate_limit",
        apply: crate::helpscout_real::apply_m043,
    },
    BootStep {
        version: 35,
        label: "m044_mirror_schema_completion",
        apply: crate::mirror_parity::apply_m044,
    },
    BootStep {
        version: 36,
        label: "m045_threads_actor_model",
        apply: crate::mirror_parity::apply_m045,
    },
];

/// Apply the pending boot steps, each in one transaction with its
/// `_migrations` row (DB-01).
fn apply_boot_steps(conn: &mut Connection, steps: &[BootStep]) -> Result<()> {
    let current = crate::db::latest_version(conn)?;
    for step in steps {
        if step.version <= current {
            continue;
        }
        tracing::info!(
            version = step.version,
            label = step.label,
            "applying migration"
        );
        let tx = conn.transaction()?;
        (step.apply)(&tx).map_err(|e| crate::error::Error::Migration {
            version: step.version,
            message: e.to_string(),
        })?;
        tx.execute(
            "INSERT INTO _migrations (version, label) VALUES (?1, ?2)",
            rusqlite::params![i64::from(step.version), step.label],
        )?;
        tx.commit()?;
    }
    Ok(())
}

/// Apply the complete migration chain: base `MIGRATIONS` (versions 1–2),
/// then every boot batch as versioned, transactional steps (versions 3..N).
/// Idempotent — safe to call on every boot.
///
/// Every boot path (Tauri shell, standalone HTTP example, tests) MUST use
/// this function; nothing else may call the individual `apply_m0XX`
/// functions.
///
/// # Errors
///
/// Returns [`crate::error::Error::Migration`] wrapping the failure if a
/// step fails — the step's transaction (including its history row) is
/// rolled back; earlier steps stay applied.
pub fn apply_all(conn: &mut Connection) -> Result<()> {
    crate::db::ensure_migrations_table(conn)?;
    crate::migrations::run_all(conn)?;
    apply_boot_steps(conn, BOOT_STEPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_steps_continue_the_sql_migration_sequence_contiguously() {
        let first = BOOT_STEPS
            .first()
            .expect("the boot chain must not be empty")
            .version;
        assert_eq!(
            first,
            crate::migrations::latest_version() + 1,
            "boot steps continue right after the SQL-string migrations"
        );
        for w in BOOT_STEPS.windows(2) {
            assert_eq!(
                w[1].version,
                w[0].version + 1,
                "versions must be contiguous"
            );
        }
    }

    #[test]
    fn boot_step_labels_are_unique() {
        let mut labels: Vec<&str> = BOOT_STEPS.iter().map(|s| s.label).collect();
        labels.sort_unstable();
        let n = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), n, "duplicate boot-step labels");
    }

    /// DB-01: a step that fails mid-way leaves NOTHING behind — neither its
    /// DDL nor its `_migrations` row — while earlier steps stay applied.
    #[test]
    fn a_failing_boot_step_rolls_back_completely() {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::db::ensure_migrations_table(&conn).unwrap();

        fn good_step(conn: &Connection) -> Result<()> {
            conn.execute_batch("CREATE TABLE IF NOT EXISTS boot_ok (id INTEGER PRIMARY KEY)")?;
            Ok(())
        }
        fn bad_step(conn: &Connection) -> Result<()> {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS boot_partial (id INTEGER PRIMARY KEY);
                 INSERT INTO table_that_does_not_exist VALUES (1);",
            )?;
            Ok(())
        }
        let steps = [
            BootStep {
                version: 3,
                label: "good",
                apply: good_step,
            },
            BootStep {
                version: 4,
                label: "bad",
                apply: bad_step,
            },
        ];
        let err = apply_boot_steps(&mut conn, &steps);
        assert!(err.is_err(), "the bad step must fail");
        assert!(matches!(
            err.unwrap_err(),
            crate::error::Error::Migration { version: 4, .. }
        ));
        // version 3 applied + recorded; version 4 neither.
        let versions: Vec<i64> = conn
            .prepare("SELECT version FROM _migrations ORDER BY version")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|v| v.ok())
            .collect();
        assert_eq!(versions, vec![3], "only the good step is recorded");
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .filter_map(|v| v.ok())
            .collect();
        assert!(tables.contains(&"boot_ok".into()));
        assert!(
            !tables.contains(&"boot_partial".into()),
            "the failed step's DDL must roll back (DB-01)"
        );
    }

    #[test]
    fn apply_all_creates_every_runtime_table() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_all(&mut conn).unwrap();

        // The tables previous boot chains forgot (the divergence this
        // module exists to prevent).
        for t in [
            // FTS (reference 004)
            "conversations_fts",
            // sync mirror (M029)
            "organizations",
            // report snapshots + business hours (M034)
            "report_snapshots",
            "mailbox_business_hours",
            // chunk tables + mirrors (M039)
            "knowledge_sources",
            "knowledge_documents",
            "knowledge_chunks",
            "docs_chunks",
            "conversation_chunks",
            "customer_emails",
            "support_cases",
            "ai_extracted_facts",
            "ai_sources",
            // runtime guards
            "webhook_events",
            "inbox_views",
            "client_current_signals",
            "interaction_evidence",
            "oauth_states",
            "jobs",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {t} must exist after apply_all");
        }

        // `conversations.deleted_at` is invariant after boot (the SLA
        // guard): mirror queries may rely on it from the first request.
        let conv_cols: Vec<String> = conn
            .prepare("PRAGMA table_info(conversations)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .collect();
        assert!(conv_cols.contains(&"deleted_at".to_string()));
        assert!(conv_cols.contains(&"customer_waiting_since".to_string()));

        // The reference-shaped sync_runs (M029 reshape): `state`, not
        // `status`.
        let cols: Vec<String> = conn
            .prepare("PRAGMA table_info(sync_runs)")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .filter_map(|c| c.ok())
            .collect();
        assert!(cols.contains(&"state".to_string()));
        assert!(!cols.contains(&"status".to_string()));
    }

    /// DB-01: after boot the history table records every applied version —
    /// the real chain, nothing fabricated.
    #[test]
    fn apply_all_records_the_real_version_history() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_all(&mut conn).unwrap();
        let (maxv, count): (i64, i64) = conn
            .query_row("SELECT MAX(version), COUNT(*) FROM _migrations", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        let expected_max = BOOT_STEPS.last().unwrap().version;
        assert_eq!(maxv, i64::from(expected_max));
        // 2 SQL-string migrations + every boot step, exactly once.
        assert_eq!(
            count,
            2 + BOOT_STEPS.len() as i64,
            "every applied version is recorded once"
        );
        // Re-boot: nothing new applied, nothing re-recorded.
        apply_all(&mut conn).unwrap();
        let count2: i64 = conn
            .query_row("SELECT COUNT(*) FROM _migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count2, count, "re-boot records nothing new");
    }

    /// DB-01: an existing pre-versioning database (versions 1–2 recorded,
    /// full schema present) adopts the new versions with the same no-op
    /// re-run it always did on boot — no fabricated history, no drift.
    #[test]
    fn existing_databases_adopt_the_new_versions_in_place() {
        // Simulate a pre-DB-01 database: full schema, versions 1–2 only.
        let mut conn = Connection::open_in_memory().unwrap();
        apply_all(&mut conn).unwrap();
        conn.execute_batch("DELETE FROM _migrations WHERE version >= 3")
            .unwrap();

        // Boot again: the batches re-run as no-ops and record 3..N.
        apply_all(&mut conn).unwrap();
        let (maxv, count): (i64, i64) = conn
            .query_row("SELECT MAX(version), COUNT(*) FROM _migrations", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(maxv, i64::from(BOOT_STEPS.last().unwrap().version));
        assert_eq!(count, 2 + BOOT_STEPS.len() as i64);
        // The schema is still complete (no crash, no loss).
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(tables >= 126, "expected >= 126 tables, got {tables}");
    }

    #[test]
    fn apply_all_is_idempotent() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_all(&mut conn).unwrap();
        apply_all(&mut conn).unwrap();
    }

    #[test]
    fn apply_all_gives_reference_table_count() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_all(&mut conn).unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // The reference's 126-table schema (124 migration tables + 2
        // runtime) plus the port's FTS5 shadow tables.
        assert!(n >= 126, "expected >= 126 tables, got {n}");
    }
}
