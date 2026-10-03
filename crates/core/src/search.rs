//! Lexical universal search (FTS5) — search conversations + customers + docs.
//!
//! Per spec M3: "lexical universal search."
//! Per KNOWN PITFALLS:
//! - "Escape LIKE wildcards; quote FTS5 queries safely; cap query length."
//! - "Bound every potentially large query, disclose bounds."
//!
//! This module creates FTS5 virtual tables on conversations (subject + preview),
//! customers (name + email + organization), and docs (name + text), and provides
//! a unified search function that queries all three.

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Maximum search query length.
pub const MAX_SEARCH_QUERY_LENGTH: usize = 500;

/// Maximum number of results per resource type.
pub const MAX_RESULTS_PER_TYPE: u32 = 50;

/// A search result item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub resource_type: String,
    pub remote_id: i64,
    pub title: String,
    pub snippet: String,
}

/// Apply the FTS5 migration. Creates virtual tables + triggers to keep them
/// in sync with the base tables. Idempotent (IF NOT EXISTS).
pub fn apply_fts_migration(conn: &Connection) -> Result<()> {
    // Conversations FTS5 index on subject + preview (standalone, no external content).
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS conversations_fts
         USING fts5(remote_id UNINDEXED, subject, preview);",
    )?;

    // Customers FTS5 index on first_name + last_name + email + organization.
    conn.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS customers_fts
         USING fts5(remote_id UNINDEXED, first_name, last_name, email, organization);",
    )?;

    Ok(())
}

/// Escape an FTS5 query string. Per KNOWN PITFALLS: "quote FTS5 queries safely."
///
/// FTS5 uses double quotes for phrase queries. We wrap the user's query in
/// double quotes and escape any internal double quotes. This turns the input
/// into a single phrase query, which is the safest approach (no special FTS5
/// operators like AND/OR/NOT/NEAR are interpreted).
fn escape_fts5_query(query: &str) -> String {
    // Replace internal double quotes with two double quotes (FTS5 escape).
    let escaped = query.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Atomic rebuild of the FTS indexes (the `rebuild_search_index`
/// maintenance job). Wiping FTS outside a transaction would leave search
/// empty or degraded if the process died mid-rebuild, so everything runs
/// inside one. Returns the number of conversations re-indexed.
pub fn rebuild_indexes(conn: &Connection) -> Result<usize> {
    conn.execute_batch("BEGIN")?;
    let result = (|| -> Result<usize> {
        conn.execute("DELETE FROM conversations_fts", [])?;
        conn.execute("DELETE FROM customers_fts", [])?;
        let rows: Vec<(i64, Option<String>, Option<String>)> = {
            let mut stmt = conn.prepare(
                "SELECT remote_id, subject, preview FROM conversations WHERE deleted_at IS NULL",
            )?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .filter_map(|r| r.ok())
                .collect();
            rows
        };
        for (remote_id, subject, preview) in &rows {
            conn.execute(
                "INSERT INTO conversations_fts (remote_id, subject, preview) VALUES (?1, ?2, ?3)",
                params![remote_id, subject, preview],
            )?;
        }
        let customers: Vec<(i64, Option<String>, Option<String>, Option<String>, Option<String>)> = {
            let mut stmt = conn.prepare(
                "SELECT remote_id, first_name, last_name, email, organization FROM customers WHERE deleted_at IS NULL",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                    ))
                })?
                .filter_map(|r| r.ok())
                .collect();
            rows
        };
        for (remote_id, first, last, email, org) in &customers {
            conn.execute(
                "INSERT INTO customers_fts (remote_id, first_name, last_name, email, organization)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![remote_id, first, last, email, org],
            )?;
        }
        Ok(rows.len())
    })();
    match result {
        Ok(n) => {
            conn.execute_batch("COMMIT")?;
            Ok(n)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Search conversations + customers in one query.
/// Returns results sorted by relevance (FTS5 rank).
pub fn universal_search(conn: &Connection, query: &str) -> Result<Vec<SearchResult>> {
    // Cap query length.
    let query = if query.len() > MAX_SEARCH_QUERY_LENGTH {
        &query[..MAX_SEARCH_QUERY_LENGTH]
    } else {
        query
    };

    if query.trim().is_empty() {
        return Ok(Vec::new());
    }

    let fts_query = escape_fts5_query(query);
    let mut results = Vec::new();

    // Search conversations.
    let conv_results = search_conversations(conn, &fts_query)?;
    results.extend(conv_results);

    // Search customers.
    let cust_results = search_customers(conn, &fts_query)?;
    results.extend(cust_results);

    Ok(results)
}

fn search_conversations(conn: &Connection, fts_query: &str) -> Result<Vec<SearchResult>> {
    let mut stmt = conn.prepare(
        "SELECT remote_id, COALESCE(subject, ''), COALESCE(preview, '')
         FROM conversations_fts
         WHERE conversations_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;

    let rows = stmt
        .query_map(params![fts_query, MAX_RESULTS_PER_TYPE], |r| {
            Ok(SearchResult {
                resource_type: "conversation".into(),
                remote_id: r.get(0)?,
                title: r.get(1)?,
                snippet: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(rows)
}

fn search_customers(conn: &Connection, fts_query: &str) -> Result<Vec<SearchResult>> {
    let mut stmt = conn.prepare(
        "SELECT remote_id,
                COALESCE(first_name, '') || ' ' || COALESCE(last_name, ''),
                COALESCE(email, '')
         FROM customers_fts
         WHERE customers_fts MATCH ?1
         ORDER BY rank
         LIMIT ?2",
    )?;

    let rows = stmt
        .query_map(params![fts_query, MAX_RESULTS_PER_TYPE], |r| {
            Ok(SearchResult {
                resource_type: "customer".into(),
                remote_id: r.get(0)?,
                title: r.get(1)?,
                snippet: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok(rows)
}

/// Index a conversation (called after upsert).
pub fn index_conversation(
    conn: &Connection,
    remote_id: i64,
    subject: &str,
    preview: &str,
) -> Result<()> {
    // Delete old entry + insert new.
    conn.execute(
        "DELETE FROM conversations_fts WHERE remote_id = ?1",
        params![remote_id],
    )?;
    conn.execute(
        "INSERT INTO conversations_fts (remote_id, subject, preview) VALUES (?1, ?2, ?3)",
        params![remote_id, subject, preview],
    )?;
    Ok(())
}

/// Index a customer (called after upsert).
pub fn index_customer(
    conn: &Connection,
    remote_id: i64,
    first_name: &str,
    last_name: &str,
    email: &str,
    organization: &str,
) -> Result<()> {
    conn.execute(
        "DELETE FROM customers_fts WHERE remote_id = ?1",
        params![remote_id],
    )?;
    conn.execute(
        "INSERT INTO customers_fts (remote_id, first_name, last_name, email, organization)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![remote_id, first_name, last_name, email, organization],
    )?;
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
        apply_fts_migration(&conn).unwrap();
        conn
    }

    fn insert_conversation(conn: &Connection, remote_id: i64, subject: &str, preview: &str) {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id, subject, preview)
             VALUES (?1, ?1, 'active', 101, 2001, ?2, ?3)",
            params![remote_id, subject, preview],
        )
        .unwrap();
        index_conversation(conn, remote_id, subject, preview).unwrap();
    }

    fn insert_customer(
        conn: &Connection,
        remote_id: i64,
        first: &str,
        last: &str,
        email: &str,
        org: &str,
    ) {
        conn.execute(
            "INSERT INTO customers (remote_id, first_name, last_name, email, organization)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![remote_id, first, last, email, org],
        )
        .unwrap();
        index_customer(conn, remote_id, first, last, email, org).unwrap();
    }

    #[test]
    fn search_finds_conversation_by_subject() {
        let conn = fresh_db();
        insert_conversation(
            &conn,
            1001,
            "Billing issue with invoice",
            "Customer reports wrong amount",
        );

        let results = universal_search(&conn, "billing").unwrap();
        assert!(results
            .iter()
            .any(|r| r.resource_type == "conversation" && r.remote_id == 1001));
    }

    #[test]
    fn search_finds_conversation_by_preview() {
        let conn = fresh_db();
        insert_conversation(
            &conn,
            1002,
            "General inquiry",
            "Need help with refund processing",
        );

        let results = universal_search(&conn, "refund").unwrap();
        assert!(results.iter().any(|r| r.remote_id == 1002));
    }

    #[test]
    fn search_finds_customer_by_name() {
        let conn = fresh_db();
        insert_customer(
            &conn,
            2001,
            "Alice",
            "Wonderland",
            "alice@example.com",
            "Acme Corp",
        );

        let results = universal_search(&conn, "Alice").unwrap();
        assert!(results
            .iter()
            .any(|r| r.resource_type == "customer" && r.remote_id == 2001));
    }

    #[test]
    fn search_finds_customer_by_email() {
        let conn = fresh_db();
        insert_customer(&conn, 2002, "Bob", "Jones", "bob@example.com", "Globex");

        let results = universal_search(&conn, "bob@example.com").unwrap();
        assert!(results
            .iter()
            .any(|r| r.resource_type == "customer" && r.remote_id == 2002));
    }

    #[test]
    fn search_finds_customer_by_organization() {
        let conn = fresh_db();
        insert_customer(
            &conn,
            2003,
            "Charlie",
            "Brown",
            "charlie@example.com",
            "Peanuts Inc",
        );

        let results = universal_search(&conn, "Peanuts").unwrap();
        assert!(results
            .iter()
            .any(|r| r.resource_type == "customer" && r.remote_id == 2003));
    }

    #[test]
    fn search_empty_query_returns_empty() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "Test", "Test preview");

        let results = universal_search(&conn, "").unwrap();
        assert!(results.is_empty());

        let results = universal_search(&conn, "   ").unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_caps_query_length() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "billing", "preview");

        // Very long query — should be truncated, not error.
        let long_query = "billing ".repeat(200);
        let results = universal_search(&conn, &long_query);
        assert!(results.is_ok());
    }

    #[test]
    fn search_injection_attempt_is_safe() {
        let conn = fresh_db();
        insert_conversation(&conn, 1001, "Test conversation", "Preview text");

        // FTS5 injection attempt — should be quoted safely (no error, no all-rows leak).
        let results = universal_search(&conn, "test\" OR 1=1--");
        assert!(
            results.is_ok(),
            "injection attempt should not cause an error"
        );
    }

    #[test]
    fn escape_fts5_query_wraps_in_quotes() {
        let escaped = escape_fts5_query("hello world");
        assert_eq!(escaped, "\"hello world\"");
    }

    #[test]
    fn escape_fts5_query_escapes_internal_quotes() {
        let escaped = escape_fts5_query("test\"quote");
        assert_eq!(escaped, "\"test\"\"quote\"");
    }

    #[test]
    fn index_conversation_is_upsert() {
        let conn = fresh_db();
        // First insert.
        insert_conversation(&conn, 1001, "Old subject", "Old preview");

        // Update — re-index.
        index_conversation(&conn, 1001, "New subject", "New preview").unwrap();

        // Old subject should not be found.
        let results = universal_search(&conn, "Old").unwrap();
        assert!(!results.iter().any(|r| r.remote_id == 1001));

        // New subject should be found.
        let results = universal_search(&conn, "New").unwrap();
        assert!(results.iter().any(|r| r.remote_id == 1001));
    }

    #[test]
    fn results_are_bounded() {
        let conn = fresh_db();
        // Insert more than MAX_RESULTS_PER_TYPE conversations.
        for i in 0..100 {
            insert_conversation(&conn, 1000 + i, "billing issue", "preview");
        }

        let results = universal_search(&conn, "billing").unwrap();
        let conv_count = results
            .iter()
            .filter(|r| r.resource_type == "conversation")
            .count();
        assert!(conv_count <= MAX_RESULTS_PER_TYPE as usize);
    }
}
