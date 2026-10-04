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

use crate::error::Result;
use rusqlite::Connection;

/// Apply the complete migration chain: base `MIGRATIONS`, the FTS5
/// migration, every incremental batch (M003–M040) in number order, and the
/// runtime table guards. Idempotent — safe to call on every boot.
///
/// Every boot path (Tauri shell, standalone HTTP example, tests) MUST use
/// this function; nothing else may call the individual `apply_m0XX`
/// functions.
///
/// # Errors
///
/// Returns [`crate::error::Error::Sqlite`] if any statement fails.
pub fn apply_all(conn: &mut Connection) -> Result<()> {
    crate::db::ensure_migrations_table(conn)?;
    crate::migrations::run_all(conn)?;

    // FTS5 tables and triggers (reference migration 004_fts).
    crate::search::apply_fts_migration(conn)?;

    // Incremental batches, in number order (reference migrations
    // 003–016 land here in port form).
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
    crate::inbox::apply_m028(conn)?;
    crate::sync_schema::apply_m029(conn)?;
    crate::conversation_ops::apply_m030(conn)?;
    crate::outreach::apply_m031(conn)?;
    crate::ticket_states::apply_m032(conn)?;
    crate::ai_attributes::apply_m033(conn)?;
    crate::reports::apply_m034(conn)?;
    crate::intelligence_features::apply_m035(conn)?;
    crate::customer_events::apply_m036(conn)?;
    crate::maintenance::apply_m037(conn)?;
    crate::connectors::apply_m038(conn)?;
    crate::mirror_tables::apply_m039(conn)?;
    crate::db_breadth::apply_m040(conn)?;

    // Runtime table guards: tables the reference creates inside its
    // migrations but the port creates lazily on first use. Ensuring them at
    // boot keeps every runtime read (webhooks, saved views, OAuth state,
    // job queue) from depending on call order.
    crate::webhook::ensure_webhook_events_table(conn)?;
    crate::saved_views::ensure_inbox_views_table(conn)?;
    crate::oauth_state::ensure_oauth_states_table(conn)?;
    crate::jobs::ensure_jobs_table(conn)?;
    crate::interaction_current::ensure_client_current_signals_table(conn)?;
    // AI pipeline stores (ai_drafts/ai_sources/customer_memory reference
    // shape) + the Copilot session/message tables + the tool-read tables.
    // The reference creates these in migrations 003/013; the port creates
    // them lazily on first AI call — ensuring them at boot keeps runtime
    // reads (AI center, jobs, copilot session lists) from depending on
    // call order.
    crate::ai_pipeline::ensure_pipeline_schema(conn)?;
    crate::copilot::ensure_copilot_schema(conn)?;

    // Record the reference-equivalent migration set so `migrations_applied`
    // and the .sosync schema guard compare like with like (the reference's
    // migrator writes these rows as it applies 001–016).
    crate::encrypted_sync::ensure_schema_migrations_record(conn)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
