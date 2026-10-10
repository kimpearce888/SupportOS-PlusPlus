//! Performance guards (M3-T09).
//!
//! Per spec M3 + KNOWN PITFALLS:
//! - "Bound every potentially large query, disclose bounds, add indexes plus
//!   EXPLAIN QUERY PLAN and performance tests on a synthetic 2,000-conversation
//!   dataset."
//!
//! This module creates a synthetic dataset of 2,000 conversations and runs
//! performance tests on the key queries (response state counts, saved view
//! filters, FTS5 search). Each test measures elapsed time and verifies the
//! result is correct + bounded.

use std::time::Instant;

use rusqlite::{params, Connection};

use crate::error::Result;

/// The synthetic dataset size.
pub const DATASET_SIZE: u32 = 2000;

/// Maximum acceptable query time in milliseconds for a bounded query.
/// Per KNOWN PITFALLS: "disclose bounds."
pub const MAX_QUERY_MS: u128 = 500;

/// Populate the database with a synthetic dataset of 2,000 conversations.
/// Each conversation has a random status, mailbox, assignee, customer,
/// subject, and preview. Activity events are generated for half of them
/// so the response state distribution is realistic.
pub fn populate_synthetic_dataset(conn: &Connection) -> Result<()> {
    // Apply M003 + M004 + FTS migrations if not already applied.
    crate::activity::apply_m003(conn)?;
    crate::ticket_states::apply_m004(conn)?;
    crate::search::apply_fts_migration(conn)?;
    // DB-03 (M047): the reference column names on conversations. The slim
    // chains this dataset runs on (base migrations + the guards above)
    // can't run m047's converging copy, so reproduce the renamed result
    // directly — plain renames, no FK clauses; on a full (M047) chain the
    // renames are no-ops that simply fail and get swallowed.
    let _ = conn.execute(
        "ALTER TABLE conversations RENAME COLUMN mailbox_id TO mailbox_local_id",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE conversations RENAME COLUMN assignee_id TO assignee_local_id",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE conversations RENAME COLUMN customer_id TO customer_local_id",
        [],
    );

    // Insert conversations.
    for i in 1..=DATASET_SIZE as i64 {
        let status = match i % 3 {
            0 => "closed",
            1 => "active",
            _ => "pending",
        };
        let mailbox_id = 101 + (i % 2);
        let customer_id = 2001 + (i % 100);
        let subject = format!("Conversation #{i} - Support request");
        let preview = format!("Customer reports issue #{i} with product feature...");

        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_local_id, customer_local_id, subject, preview)
             VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6)",
            params![i, status, mailbox_id, customer_id, subject, preview],
        )?;

        // Index in FTS5.
        crate::search::index_conversation(conn, i, &subject, &preview)?;

        // Set response_state via the activity engine (for half the conversations).
        if i % 2 == 0 {
            // Add a customer message → customer_waiting.
            crate::activity::record_event(
                conn,
                &crate::activity::ActivityEvent {
                    id: None,
                    conversation_id: i,
                    event_type: "message".into(),
                    actor_type: "customer".into(),
                    actor_id: None,
                    occurred_at: format!("2026-01-{:02}T10:00:00Z", (i % 28) + 1),
                    dedup_key: format!("synth_evt_{i}"),
                },
            )?;
            crate::activity::update_derived_columns(conn, i)?;
        }
    }

    // Insert some customers for FTS5 search.
    for i in 1..=100 {
        let name = format!("Customer{i}");
        let email = format!("customer{i}@example.com");
        conn.execute(
            "INSERT INTO customers (remote_id, first_name, last_name, email, organization)
             VALUES (?1, ?2, 'Last', ?3, ?4)",
            params![
                2000 + i,
                name,
                email,
                if i <= 50 { "Acme Corp" } else { "Globex" }
            ],
        )?;
        crate::search::index_customer(
            conn,
            2000 + i,
            &name,
            "Last",
            &email,
            if i <= 50 { "Acme Corp" } else { "Globex" },
        )?;
    }

    Ok(())
}

/// Run a performance test on a query and return the elapsed time in milliseconds.
pub fn benchmark_query(conn: &Connection, label: &str, sql: &str) -> Result<(u128, usize)> {
    let start = Instant::now();
    let mut stmt = conn.prepare(sql)?;
    let count = stmt.query_map([], |r| r.get::<_, i64>(0))?.count();
    let elapsed = start.elapsed().as_millis();
    tracing::info!(label, elapsed_ms = elapsed, count, "benchmark query");
    Ok((elapsed, count))
}

/// Run all performance tests on the synthetic dataset.
/// Returns a summary of results.
pub fn run_performance_tests(conn: &Connection) -> Result<Vec<PerfResult>> {
    populate_synthetic_dataset(conn)?;
    let mut results = Vec::new();

    // Test 1: Count by response state (used by Operations Center tiles).
    let (ms, count) = benchmark_query(
        conn,
        "count_by_response_state",
        "SELECT COUNT(*) FROM conversations GROUP BY response_state",
    )?;
    results.push(PerfResult {
        label: "count_by_response_state".into(),
        elapsed_ms: ms,
        result_count: count,
        within_bound: ms <= MAX_QUERY_MS,
    });

    // Test 2: Filter by status (used by inbox filters).
    let (ms, count) = benchmark_query(
        conn,
        "filter_by_status_active",
        "SELECT COUNT(*) FROM conversations WHERE status = 'active'",
    )?;
    results.push(PerfResult {
        label: "filter_by_status_active".into(),
        elapsed_ms: ms,
        result_count: count,
        within_bound: ms <= MAX_QUERY_MS,
    });

    // Test 3: FTS5 search (used by universal search).
    let start = Instant::now();
    let search_results = crate::search::universal_search(conn, "Conversation")?;
    let elapsed = start.elapsed().as_millis();
    results.push(PerfResult {
        label: "universal_search_fts5".into(),
        elapsed_ms: elapsed,
        result_count: search_results.len(),
        within_bound: elapsed <= MAX_QUERY_MS,
    });

    Ok(results)
}

/// One performance test result.
#[derive(Debug, Clone)]
pub struct PerfResult {
    pub label: String,
    pub elapsed_ms: u128,
    pub result_count: usize,
    pub within_bound: bool,
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
        conn
    }

    #[test]
    fn synthetic_dataset_has_2000_conversations() {
        let conn = fresh_db();
        populate_synthetic_dataset(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 2000);
    }

    #[test]
    fn synthetic_dataset_has_customers() {
        let conn = fresh_db();
        populate_synthetic_dataset(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM customers", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 100);
    }

    #[test]
    fn count_by_response_state_is_fast() {
        let conn = fresh_db();
        populate_synthetic_dataset(&conn).unwrap();
        let (ms, count) = benchmark_query(
            &conn,
            "count_by_response_state",
            "SELECT COUNT(*) FROM conversations GROUP BY response_state",
        )
        .unwrap();
        assert!(count > 0, "should have at least one response state");
        // Per KNOWN PITFALLS: "disclose bounds."
        // On a fast machine this should be < 10ms; we allow up to MAX_QUERY_MS (500ms)
        // for CI runners that may be slower.
        assert!(
            ms <= MAX_QUERY_MS,
            "count_by_response_state took {ms}ms, max is {MAX_QUERY_MS}ms"
        );
    }

    #[test]
    fn fts5_search_on_2000_conversations_is_fast() {
        let conn = fresh_db();
        populate_synthetic_dataset(&conn).unwrap();
        let start = Instant::now();
        let results = crate::search::universal_search(&conn, "Conversation").unwrap();
        let elapsed = start.elapsed().as_millis();
        assert!(
            !results.is_empty(),
            "should find conversations matching 'Conversation'"
        );
        assert!(
            elapsed <= MAX_QUERY_MS,
            "FTS5 search took {elapsed}ms, max is {MAX_QUERY_MS}ms"
        );
    }

    #[test]
    fn filter_by_status_is_fast() {
        let conn = fresh_db();
        populate_synthetic_dataset(&conn).unwrap();
        let (ms, count) = benchmark_query(
            &conn,
            "filter_by_status_active",
            "SELECT COUNT(*) FROM conversations WHERE status = 'active'",
        )
        .unwrap();
        assert!(count > 0, "should have active conversations");
        assert!(
            ms <= MAX_QUERY_MS,
            "filter_by_status took {ms}ms, max is {MAX_QUERY_MS}ms"
        );
    }

    #[test]
    fn run_all_performance_tests_passes() {
        let conn = fresh_db();
        let results = run_performance_tests(&conn).unwrap();
        assert!(!results.is_empty(), "should have at least one result");
        for r in &results {
            assert!(
                r.within_bound,
                "query {} took {}ms (max {}ms)",
                r.label, r.elapsed_ms, MAX_QUERY_MS
            );
        }
    }
}
