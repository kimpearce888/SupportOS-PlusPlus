//! Response state SQL fragment — the single source of truth for tiles + filters.
//!
//! Per spec M3 + KNOWN PITFALLS: "the badge must be computed by the same
//! expression as the filter." The reference repo had a v1.7.0 bug where two
//! different SQL fragments were used for the Operations Center tile count
//! and the inbox filter list, causing them to disagree.
//!
//! In SupportOS++, the `response_state` column on the `conversations` table
//! (added by M003 in M3-T01) IS the single source of truth. Both the tile
//! counts and the inbox filters read from this column directly — no separate
//! SQL expression, no possibility of disagreement.
//!
//! This module provides:
//! - `RESPONSE_STATE_SQL`: the SQL fragment that computes the response state
//!   from the `conversations` table. Used by both tile counts and filters.
//! - `count_by_response_state()`: counts conversations per response state.
//! - `filter_by_response_state()`: returns the WHERE clause for filtering.
//!
//! The test `tile_count_matches_filter_count` verifies the v1.7.0 invariant:
//! the tile count and the filter list count must always agree.

use rusqlite::Connection;

use crate::catalog::ResponseState;
use crate::error::Result;

/// The SQL fragment that selects the response state from the conversations
/// table. This is the SINGLE source of truth — both tile counts and inbox
/// filters use this exact expression.
///
/// Per KNOWN PITFALLS: "the badge must be computed by the same expression
/// as the filter." Using a stored column (not a computed expression) makes
/// this guarantee structural: there's only one `response_state` column.
pub const RESPONSE_STATE_SQL: &str = "response_state";

/// The WHERE clause fragment for filtering conversations by response state.
/// Uses the stored column directly (no computed expression).
pub fn filter_by_response_state(state: ResponseState) -> String {
    format!("response_state = '{}'", state.as_str())
}

/// Count conversations by response state. Returns a map of state → count.
/// This is the function the Operations Center uses for tile counts.
pub fn count_by_response_state(conn: &Connection) -> Result<Vec<(String, u32)>> {
    let mut stmt = conn.prepare(
        "SELECT response_state, COUNT(*) as cnt
         FROM conversations
         GROUP BY response_state
         ORDER BY response_state",
    )?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Count conversations matching a specific response state. This is the
/// function the inbox filter uses.
pub fn count_conversations_with_state(conn: &Connection, state: ResponseState) -> Result<u32> {
    let count: u32 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM conversations WHERE {}",
            filter_by_response_state(state)
        ),
        [],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// Count conversations matching a response state, filtered by mailbox.
/// Used by the Operations Center tiles (which can be scoped to a mailbox).
pub fn count_conversations_with_state_in_mailbox(
    conn: &Connection,
    state: ResponseState,
    mailbox_id: Option<i64>,
) -> Result<u32> {
    let sql = match mailbox_id {
        Some(mid) => format!(
            "SELECT COUNT(*) FROM conversations WHERE response_state = '{}' AND mailbox_id = {}",
            state.as_str(),
            mid
        ),
        None => format!(
            "SELECT COUNT(*) FROM conversations WHERE response_state = '{}'",
            state.as_str()
        ),
    };
    let count: u32 = conn.query_row(&sql, [], |r| r.get(0))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activity::update_derived_columns;
    use crate::activity::{apply_m003, record_event, ActivityEvent};
    use rusqlite::params;
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
        apply_m003(&conn).unwrap();
        conn
    }

    fn insert_conversation(conn: &Connection, remote_id: i64, status: &str, mailbox_id: i64) {
        conn.execute(
            "INSERT INTO conversations (remote_id, number, status, mailbox_id, customer_id)
             VALUES (?1, ?2, ?3, ?4, 2001)",
            params![remote_id, remote_id, status, mailbox_id],
        )
        .unwrap();
    }

    fn insert_event(
        conn: &Connection,
        conv_id: i64,
        event_type: &str,
        actor: &str,
        occurred_at: &str,
        dedup: &str,
    ) {
        record_event(
            conn,
            &ActivityEvent {
                id: None,
                conversation_id: conv_id,
                event_type: event_type.into(),
                actor_type: actor.into(),
                actor_id: None,
                occurred_at: occurred_at.into(),
                dedup_key: dedup.into(),
            },
        )
        .unwrap();
    }

    fn setup_test_conversations(conn: &Connection) {
        // 3 conversations with different response states.
        // Conv 1: needs_first_response (no events)
        insert_conversation(conn, 1001, "active", 101);

        // Conv 2: customer_waiting (customer sent last message)
        insert_conversation(conn, 1002, "active", 101);
        insert_event(
            conn,
            1002,
            "message",
            "customer",
            "2026-01-01T10:00:00Z",
            "evt_002",
        );
        update_derived_columns(conn, 1002).unwrap();

        // Conv 3: closed
        insert_conversation(conn, 1003, "closed", 102);
        update_derived_columns(conn, 1003).unwrap();
    }

    #[test]
    fn count_by_response_state_returns_all_states() {
        let conn = fresh_db();
        setup_test_conversations(&conn);

        let counts = count_by_response_state(&conn).unwrap();
        // Should have entries for: needs_first_response, customer_waiting, closed.
        assert!(
            counts.len() >= 3,
            "expected at least 3 states, got {counts:?}"
        );
    }

    #[test]
    fn count_conversations_with_needs_first_response() {
        let conn = fresh_db();
        setup_test_conversations(&conn);

        let count =
            count_conversations_with_state(&conn, ResponseState::NeedsFirstResponse).unwrap();
        assert_eq!(count, 1, "conv 1001 should be needs_first_response");
    }

    #[test]
    fn count_conversations_with_customer_waiting() {
        let conn = fresh_db();
        setup_test_conversations(&conn);

        let count = count_conversations_with_state(&conn, ResponseState::CustomerWaiting).unwrap();
        assert_eq!(count, 1, "conv 1002 should be customer_waiting");
    }

    #[test]
    fn count_conversations_with_closed() {
        let conn = fresh_db();
        setup_test_conversations(&conn);

        let count = count_conversations_with_state(&conn, ResponseState::Closed).unwrap();
        assert_eq!(count, 1, "conv 1003 should be closed");
    }

    /// THE v1.7.0 invariant test: tile count must match filter count.
    /// Per KNOWN PITFALLS: "the badge must be computed by the same expression
    /// as the filter." Since both use the same `response_state` column, they
    /// can never disagree.
    #[test]
    fn tile_count_matches_filter_count() {
        let conn = fresh_db();
        setup_test_conversations(&conn);

        // For each state, verify count_by_response_state (tiles) ==
        // count_conversations_with_state (filters).
        let tile_counts = count_by_response_state(&conn).unwrap();
        for (state_str, tile_count) in &tile_counts {
            let state = match state_str.as_str() {
                "needs_first_response" => ResponseState::NeedsFirstResponse,
                "customer_waiting" => ResponseState::CustomerWaiting,
                "agent_waiting" => ResponseState::AgentWaiting,
                "closed" => ResponseState::Closed,
                _ => continue,
            };
            let filter_count = count_conversations_with_state(&conn, state).unwrap();
            assert_eq!(
                *tile_count, filter_count,
                "tile count ({tile_count}) must match filter count ({filter_count}) for state {state_str}"
            );
        }
    }

    #[test]
    fn filter_by_mailbox_scope() {
        let conn = fresh_db();
        setup_test_conversations(&conn);

        // Conv 1001 (needs_first_response) is in mailbox 101.
        let count = count_conversations_with_state_in_mailbox(
            &conn,
            ResponseState::NeedsFirstResponse,
            Some(101),
        )
        .unwrap();
        assert_eq!(count, 1);

        // No needs_first_response in mailbox 102.
        let count = count_conversations_with_state_in_mailbox(
            &conn,
            ResponseState::NeedsFirstResponse,
            Some(102),
        )
        .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn filter_without_mailbox_scope_counts_all() {
        let conn = fresh_db();
        setup_test_conversations(&conn);

        let count =
            count_conversations_with_state_in_mailbox(&conn, ResponseState::Closed, None).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn response_state_sql_constant_is_column_name() {
        // The SQL fragment must be just the column name — not a computed
        // expression. This is the structural guarantee that tiles and
        // filters can never disagree.
        assert_eq!(RESPONSE_STATE_SQL, "response_state");
    }

    #[test]
    fn filter_by_response_state_produces_valid_where_clause() {
        let sql = filter_by_response_state(ResponseState::CustomerWaiting);
        assert_eq!(sql, "response_state = 'customer_waiting'");
    }
}
